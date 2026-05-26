//! 节点注册表:服务端唯一的"哪些节点被允许上报"的事实来源。
//!
//! 注册表是一份 JSON 文件,内容由 `RegistryFile` 结构序列化得到。
//! 服务端进程与运维 CLI(`server install-agent` 等)都会读写这份文件,
//! 因此对每次写入都采用 flock + 原子替换的策略。
//!
//! 字段语义:
//! - [`RegisteredNode`]:被认证的 Agent 凭证(node_id + token)。
//! - [`InstallSession`]:一次性的"安装令牌",拥有它可以拉取 Agent 配置。

mod error;
mod render;
mod storage;
#[cfg(test)]
mod tests;
mod token;
mod validate;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::anyhow;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use nodelite_proto::{
    MAX_NODE_TAG_BYTES, MAX_NODE_TAGS, NodeIdentity, normalize_string_list, validate_identifier,
    validate_non_empty, validate_tag_list,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{RwLock, Semaphore};
use tracing::warn;

pub use self::error::{RegistryError, RegistryResult};
#[cfg(test)]
pub use self::render::{build_agent_server_url, build_github_release_base_url};
pub use self::render::{
    build_install_script_url, default_agent_release_base_url, render_agent_config,
    render_install_command, render_upgrade_command,
};
use self::storage::{
    load_registry_state, load_registry_state_with_fingerprint, mutate_registry_file,
    registry_file_fingerprint,
};
#[cfg(test)]
use self::storage::{
    registry_file_read_count, release_registry_lock_with, reset_registry_file_read_count,
};
#[cfg(test)]
use self::token::token_is_unexpired;
use self::token::{
    authorized_node_from_entry, constant_time_eq, generate_token, hash_token, mint_install_session,
    prune_expired_install_sessions, verify_token,
};
use self::validate::{validate_registered_node, validate_runtime_identity};

/// Agent Token 默认有效期:30 天。
const DEFAULT_TOKEN_VALIDITY_DAYS: i64 = 30;

/// Argon2id 参数:用 OWASP 2023 推荐的"低延迟服务器"档位,大约 ~12-25ms/verify。
/// memory=19 MiB, iterations=2, parallelism=1。
/// 落在 WS Hello 等"每会话一次"的路径上是可以接受的,但 #56 的设计要求 hot-path
/// 通过 `token_generation` 比较而非每次 verify。
const ARGON2_MEMORY_KIB: u32 = 19 * 1024;
const ARGON2_ITERATIONS: u32 = 2;
const ARGON2_PARALLELISM: u32 = 1;

/// 已登记节点的持久化条目。
///
/// Token 存储语义 (#56):
/// - 新条目 **不再** 在 `token` 字段保留明文; 只保留 `token_hash` 的 Argon2id PHC 字符串。
/// - `token_generation` 每次 token 轮换递增一次, 供 WS hot-path
///   `is_token_current` 做 O(1) 比较而不必每条消息都跑 Argon2 verify。
/// - `token` 字段保留是为了 **向后兼容**: 老版本写出的 registry.json 仍然
///   能被读取; `load_registry_state` 会在首次加载时把 `token` 哈希并清空,
///   随即把升级后的文件写回磁盘 —— 之后磁盘上不再出现明文。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RegisteredNode {
    pub node_id: String,
    pub node_label: String,
    /// Argon2id PHC 编码的 token 哈希。空串表示 "尚未迁移过的旧条目",
    /// 此时应当用 `token` 字段做最后一次明文比较并触发迁移。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub token_hash: String,
    /// 单调递增的 token 代次。每次 `refresh_token` / `issue_node` 轮换都 +1。
    /// WS 会话在认证时捕获这个值, 后续 hot-path 只比较代次,
    /// 避免每条消息都跑 Argon2 verify。
    #[serde(default)]
    pub token_generation: u64,
    /// Legacy: 旧版本的明文 token 字段。新版本启动后会一次性把它哈希到
    /// `token_hash` 并清空, 之后磁盘上不再出现。保留 #[serde(default)]
    /// 与 skip_serializing_if 是为了让升级与降级都能干净通过。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub token: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
    /// Token 过期时间。None 表示永不过期(向后兼容旧版本注册表)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_expires_at: Option<DateTime<Utc>>,
}

/// 一次成功的 token 验证 / 颁发结果:返回身份与 token 状态快照,
/// 供 WS 会话缓存后避开每帧 registry 读锁。
#[derive(Debug, Clone)]
pub struct AuthorizedNode {
    pub identity: NodeIdentity,
    pub generation: u64,
    pub token_expires_at: Option<DateTime<Utc>>,
    pub registry_revision: u64,
}

/// 轻量 token 状态快照,供 WebSocket 会话在 registry revision 变化时刷新本地缓存。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegistryTokenStatus {
    pub generation: u64,
    pub token_expires_at: Option<DateTime<Utc>>,
    pub registry_revision: u64,
}

/// `consume_install_token` 的成功返回值:Agent 拿到这个结构后即可写出本地配置。
#[derive(Debug, Clone)]
pub struct ConsumedInstall {
    pub node: RegisteredNode,
    /// 节点的明文 session token, 由 install_session 短暂持有, 返回后立即从注册表删除。
    pub node_session_token: String,
}

/// 安装会话:由 CLI 颁发的一次性令牌,Agent 用它拉取自己的配置。
///
/// `expires_at` 为绝对过期时间;每次写入注册表时会顺带清理已过期会话。
///
/// Token 存储 (#56):
/// - `node_session_token` 持有该 install_session 所属节点的**明文 session token**。
///   这是 Argon2id 化之后整个系统里唯一暂存明文的位置, 生存周期 <= 15 分钟
///   (`INSTALL_TOKEN_TTL_MINUTES`), 一旦 `consume_install_token` 被调用就连同
///   整条 session 一起从注册表删除。比起 #56 之前的 "永久保留 token 明文"
///   是一个明显的硬化。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstallSession {
    pub token: String,
    pub node_id: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    /// 节点的明文 session token, 仅在 install 流程消费之前短暂持有。
    /// 老版本的 install_session 不带这个字段,旧的 install_token 在升级后
    /// 自然过期(15min)、不可恢复 —— 运维需要重新颁发 install_token。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub node_session_token: String,
}

/// 注册表的运行期视图:进程内部以 HashMap 形式持有,便于鉴权 / 查询。
#[derive(Debug, Clone)]
pub struct NodeRegistry {
    path: Arc<PathBuf>,
    state: Arc<RwLock<RegistryState>>,
    reload_checkpoint: Arc<RwLock<RegistryReloadCheckpoint>>,
    registry_revision: Arc<AtomicU64>,
    token_verify_limit: usize,
    token_verify_limiter: Arc<Semaphore>,
    #[cfg(test)]
    token_verify_probe: Option<Arc<TokenVerifyProbe>>,
}

#[derive(Debug)]
struct RegistryReloadCheckpoint {
    fingerprint: Option<storage::RegistryFileFingerprint>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct RegistryState {
    entries: HashMap<String, RegisteredNode>,
    install_sessions: HashMap<String, InstallSession>,
}

/// `server install-agent` / `server issue-node` 等命令传给注册表的请求结构。
#[derive(Debug, Clone)]
pub struct IssueNodeRequest {
    pub node_id: String,
    pub node_label: Option<String>,
    pub tags: Vec<String>,
}

/// `IssueNodeRequest` 的结果集:同时返回节点凭证与一次性安装令牌。
#[derive(Debug, Clone)]
pub struct IssueNodeResult {
    pub node: RegisteredNode,
    pub node_session_token: String,
    pub created: bool,
    pub rotated_token: bool,
    pub install_token: String,
    pub install_token_expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
struct RegistryFile {
    #[serde(default)]
    version: u64,
    #[serde(default)]
    nodes: Vec<RegisteredNode>,
    #[serde(default)]
    install_sessions: Vec<InstallSession>,
}

/// 一次性安装令牌的有效期(分钟)。
const INSTALL_TOKEN_TTL_MINUTES: i64 = 15;
/// Argon2id 每次 verify 会短时占用约 19MiB 内存;限制到 2 可以把认证风暴的
/// CPU/内存峰值钉住,同时让正常重连只承担队列等待。
const TOKEN_VERIFY_MAX_PARALLELISM: usize = 2;
const TOKEN_VERIFY_WAIT_WARN_AFTER: Duration = Duration::from_millis(100);

impl NodeRegistry {
    /// 从磁盘加载注册表;文件不存在时返回空注册表(首次部署的合理状态)。
    pub async fn load(path: &Path) -> RegistryResult<Self> {
        let (state, fingerprint) = load_registry_state_with_fingerprint(path).await?;
        let token_verify_limit = default_token_verify_limit();

        Ok(Self {
            path: Arc::new(path.to_path_buf()),
            state: Arc::new(RwLock::new(state)),
            reload_checkpoint: Arc::new(RwLock::new(RegistryReloadCheckpoint {
                fingerprint: Some(fingerprint),
            })),
            registry_revision: Arc::new(AtomicU64::new(1)),
            token_verify_limit,
            token_verify_limiter: Arc::new(Semaphore::new(token_verify_limit)),
            #[cfg(test)]
            token_verify_probe: None,
        })
    }

    /// 校验 Agent 提交的 Hello 信息与 token,通过后返回"覆盖了注册表里权威字段"的身份
    /// 以及当时的 token 代次, 供 WS 会话后续 hot-path 比较使用。
    pub async fn authorize(
        &self,
        identity: &NodeIdentity,
        token: &str,
    ) -> RegistryResult<AuthorizedNode> {
        validate_runtime_identity(identity)?;
        validate_non_empty("hello.token", token).map_err(RegistryError::validation)?;

        for _ in 0..2 {
            let Some(entry) = self.registered_node(identity.node_id.as_str()).await else {
                return Err(RegistryError::Unauthorized);
            };

            let token_matched = self.token_matches_entry(token, &entry).await?;
            let Some((current_entry, registry_revision)) = self
                .registered_node_snapshot(identity.node_id.as_str())
                .await
            else {
                return Err(RegistryError::Unauthorized);
            };

            if !token_material_matches(&entry, &current_entry) {
                continue;
            }
            if !token_matched {
                return Err(RegistryError::Unauthorized);
            }
            return authorized_node_from_entry(identity, &current_entry, registry_revision);
        }

        warn!(
            node_id = %identity.node_id,
            "registry entry changed repeatedly during token verify; rejecting authorization"
        );
        Err(RegistryError::Unauthorized)
    }

    #[cfg(test)]
    pub async fn is_token_current(&self, node_id: &str, session_generation: u64) -> bool {
        self.token_status(node_id)
            .await
            .is_some_and(|status| status.generation == session_generation)
    }

    /// 返回节点当前 token 状态快照。WS 会话只在 registry revision 变化时调用它,
    /// 平常每帧只比较本地缓存与 atomic revision。
    pub async fn token_status(&self, node_id: &str) -> Option<RegistryTokenStatus> {
        let state = self.state.read().await;
        state
            .entries
            .get(node_id)
            .and_then(|node| token_status_for_node(node, self.registry_revision(), Utc::now()))
    }

    /// 当前注册表状态版本。任一注册表 reload / 写入导致的内存状态变化都会递增。
    pub fn registry_revision(&self) -> u64 {
        self.registry_revision.load(Ordering::Acquire)
    }

    /// 刷新节点的 Token:生成新明文 token, 哈希入库,代次 +1, 延长过期时间。
    /// 返回 (new_plaintext_token, expires_at, new_generation)。明文只在
    /// 进程内存里短暂存在,从这里被传递给 WS 端发送给 agent。
    pub async fn refresh_token(
        &self,
        node_id: &str,
    ) -> RegistryResult<(String, DateTime<Utc>, u64)> {
        let path = Arc::clone(&self.path);
        let node_id = node_id.to_string();
        let ((new_token, expires_at, generation), _) =
            mutate_registry_file(path.as_ref(), move |file| {
                let now = Utc::now();
                let Some(node) = file.nodes.iter_mut().find(|n| n.node_id == node_id) else {
                    return Err(RegistryError::NodeNotFound(node_id.clone()));
                };

                let new_token = generate_token()?;
                let expires_at = now + ChronoDuration::days(DEFAULT_TOKEN_VALIDITY_DAYS);
                node.token_hash = hash_token(&new_token).map_err(|error| {
                    RegistryError::internal("failed to hash refreshed token", error.into())
                })?;
                node.token_generation = node.token_generation.saturating_add(1);
                node.token_expires_at = Some(expires_at);
                // 升级路径残留的明文也在这里清空,确保从此刻起 disk 上彻底无明文。
                node.token.clear();

                Ok(((new_token, expires_at, node.token_generation), true))
            })
            .await?;

        // 刷新内存中的状态
        self.reload().await?;

        Ok((new_token, expires_at, generation))
    }

    /// 从磁盘重新加载注册表。返回 `Ok(true)` 表示发现了变化。
    pub async fn reload(&self) -> RegistryResult<bool> {
        let fingerprint = registry_file_fingerprint(self.path.as_path()).await?;
        self.reload_from_disk(fingerprint).await
    }

    pub(crate) async fn reload_if_file_changed(&self) -> RegistryResult<bool> {
        let fingerprint = registry_file_fingerprint(self.path.as_path()).await?;
        {
            let checkpoint = self.reload_checkpoint.read().await;
            if checkpoint.fingerprint == Some(fingerprint) {
                return Ok(false);
            }
        }

        self.reload_from_disk(fingerprint).await
    }

    async fn reload_from_disk(
        &self,
        fingerprint: storage::RegistryFileFingerprint,
    ) -> RegistryResult<bool> {
        let next_state = load_registry_state(self.path.as_path()).await?;
        let mut state = self.state.write().await;
        let changed = *state != next_state;
        if changed {
            self.bump_registry_revision();
            *state = next_state;
        }
        drop(state);

        let mut checkpoint = self.reload_checkpoint.write().await;
        checkpoint.fingerprint = Some(fingerprint);
        Ok(changed)
    }

    /// 已登记的节点数量。
    pub async fn count(&self) -> usize {
        let state = self.state.read().await;
        state.entries.len()
    }

    /// 返回注册表中的节点条目,但不会暴露 token 字符串。
    ///
    /// 设置页需要查看 token 到期时间与登记标签;这些信息来自注册表而不是
    /// 运行态快照。调用方负责只序列化安全字段,不要把 `token` 下发给浏览器。
    pub async fn list_registered_nodes(&self) -> Vec<RegisteredNode> {
        let state = self.state.read().await;
        let mut nodes: Vec<_> = state.entries.values().cloned().collect();
        nodes.sort_by(|left, right| {
            left.node_label
                .cmp(&right.node_label)
                .then_with(|| left.node_id.cmp(&right.node_id))
        });
        nodes
    }

    /// 返回当前注册表里的全部 node_id,用于跨模块做被动清理。
    pub async fn node_ids(&self) -> Vec<String> {
        let state = self.state.read().await;
        let mut node_ids: Vec<_> = state.entries.keys().cloned().collect();
        node_ids.sort();
        node_ids
    }

    /// 一次性消费安装令牌:成功时返回对应的 `RegisteredNode` **以及**该节点
    /// 当前明文 session token —— 后者由 install_session 在颁发时短暂持有,
    /// 这是 #56 之后整个系统里唯一返回明文的入口。一旦本函数返回,
    /// install_session(连同明文)即被从注册表删除。
    pub async fn consume_install_token(
        &self,
        token: &str,
    ) -> RegistryResult<Option<ConsumedInstall>> {
        validate_non_empty("install token", token).map_err(RegistryError::validation)?;

        let token = token.to_string();
        let (result, file) = mutate_registry_file(self.path.as_path(), move |file| {
            let pruned = prune_expired_install_sessions(file, Utc::now());
            let Some(index) = file
                .install_sessions
                .iter()
                .position(|session| constant_time_eq(&session.token, &token))
            else {
                return Ok((None, pruned));
            };

            let session = file.install_sessions.remove(index);
            let node = file
                .nodes
                .iter()
                .find(|node| node.node_id == session.node_id)
                .cloned();
            let result = node.map(|node| ConsumedInstall {
                node,
                node_session_token: session.node_session_token,
            });
            Ok((result, true))
        })
        .await?;
        self.replace_state_from_file(file).await?;
        Ok(result)
    }

    pub fn path(&self) -> &Path {
        self.path.as_path()
    }

    async fn registered_node(&self, node_id: &str) -> Option<RegisteredNode> {
        let state = self.state.read().await;
        state.entries.get(node_id).cloned()
    }

    async fn registered_node_snapshot(&self, node_id: &str) -> Option<(RegisteredNode, u64)> {
        let state = self.state.read().await;
        state
            .entries
            .get(node_id)
            .cloned()
            .map(|entry| (entry, self.registry_revision()))
    }

    async fn token_matches_entry(
        &self,
        input: &str,
        entry: &RegisteredNode,
    ) -> RegistryResult<bool> {
        if !entry.token_hash.is_empty() {
            self.verify_hashed_token(input, &entry.token_hash).await
        } else if !entry.token.is_empty() {
            Ok(constant_time_eq(input, &entry.token))
        } else {
            Ok(false)
        }
    }

    async fn verify_hashed_token(&self, input: &str, token_hash: &str) -> RegistryResult<bool> {
        let wait_started = Instant::now();
        let permit = Arc::clone(&self.token_verify_limiter)
            .acquire_owned()
            .await
            .map_err(|error| {
                RegistryError::internal("token verify limiter closed", anyhow!(error))
            })?;
        let waited = wait_started.elapsed();
        if waited >= TOKEN_VERIFY_WAIT_WARN_AFTER {
            warn!(
                wait_ms = waited.as_millis(),
                limit = self.token_verify_limit,
                "argon2 token verify waited for global concurrency limiter"
            );
        }

        let input = input.to_string();
        let token_hash = token_hash.to_string();
        #[cfg(test)]
        let probe = self.token_verify_probe.clone();

        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            #[cfg(test)]
            let _probe_guard = probe.as_ref().map(|probe| probe.enter());
            verify_token(&input, &token_hash)
        })
        .await
        .map_err(|error| RegistryError::internal("token verify task failed", anyhow!(error)))
    }

    async fn replace_state_from_file(&self, file: RegistryFile) -> RegistryResult<()> {
        let state = storage::load_registry_state_from_file(self.path.as_path(), file)?;
        let mut guard = self.state.write().await;
        self.bump_registry_revision();
        *guard = state;
        Ok(())
    }

    fn bump_registry_revision(&self) {
        self.registry_revision.fetch_add(1, Ordering::AcqRel);
    }

    #[cfg(test)]
    pub(crate) async fn hold_state_write_lock_for_test(
        &self,
        acquired: tokio::sync::oneshot::Sender<()>,
        release: tokio::sync::oneshot::Receiver<()>,
    ) {
        let _guard = self.state.write().await;
        let _ = acquired.send(());
        let _ = release.await;
    }

    #[cfg(test)]
    pub(super) fn with_token_verify_limit_for_tests(mut self, max_parallel: usize) -> Self {
        assert!(max_parallel > 0, "test token verify limit must be positive");
        self.token_verify_limit = max_parallel;
        self.token_verify_limiter = Arc::new(Semaphore::new(max_parallel));
        self
    }

    #[cfg(test)]
    pub(super) fn with_token_verify_probe_for_tests(
        mut self,
        probe: Arc<TokenVerifyProbe>,
    ) -> Self {
        self.token_verify_probe = Some(probe);
        self
    }
}

fn token_status_for_node(
    node: &RegisteredNode,
    registry_revision: u64,
    now: DateTime<Utc>,
) -> Option<RegistryTokenStatus> {
    if !token::token_is_unexpired(node, now) {
        return None;
    }

    Some(RegistryTokenStatus {
        generation: node.token_generation,
        token_expires_at: node.token_expires_at,
        registry_revision,
    })
}

fn default_token_verify_limit() -> usize {
    std::thread::available_parallelism()
        .map(|parallelism| parallelism.get().min(TOKEN_VERIFY_MAX_PARALLELISM))
        .unwrap_or(1)
        .max(1)
}

fn token_material_matches(left: &RegisteredNode, right: &RegisteredNode) -> bool {
    left.token_generation == right.token_generation
        && left.token_hash == right.token_hash
        && constant_time_eq(&left.token, &right.token)
}

#[cfg(test)]
#[derive(Debug)]
pub(super) struct TokenVerifyProbe {
    active: AtomicUsize,
    max_active: AtomicUsize,
    delay: Duration,
}

#[cfg(test)]
impl TokenVerifyProbe {
    pub(super) fn new(delay: Duration) -> Self {
        Self {
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
            delay,
        }
    }

    pub(super) fn max_active(&self) -> usize {
        self.max_active.load(Ordering::SeqCst)
    }

    fn enter(&self) -> TokenVerifyProbeGuard<'_> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.record_max_active(active);
        if !self.delay.is_zero() {
            std::thread::sleep(self.delay);
        }
        TokenVerifyProbeGuard { probe: self }
    }

    fn record_max_active(&self, active: usize) {
        let mut observed = self.max_active.load(Ordering::SeqCst);
        while active > observed {
            match self.max_active.compare_exchange(
                observed,
                active,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return,
                Err(next_observed) => observed = next_observed,
            }
        }
    }
}

#[cfg(test)]
struct TokenVerifyProbeGuard<'a> {
    probe: &'a TokenVerifyProbe,
}

#[cfg(test)]
impl Drop for TokenVerifyProbeGuard<'_> {
    fn drop(&mut self) {
        self.probe.active.fetch_sub(1, Ordering::SeqCst);
    }
}

/// 创建或更新一个节点:首次出现时插入新条目,已存在时按需轮换 token、覆盖标签等。
///
/// 同时为该节点签发一个一次性安装令牌。这是 CLI 命令的核心入口。
pub async fn issue_node(path: &Path, request: IssueNodeRequest) -> RegistryResult<IssueNodeResult> {
    validate_identifier("node_id", &request.node_id).map_err(RegistryError::validation)?;
    if let Some(node_label) = request.node_label.as_deref() {
        validate_non_empty("node_label", node_label).map_err(RegistryError::validation)?;
    }
    let normalized_tags = normalize_string_list(request.tags.clone());
    validate_tag_list("tags", &normalized_tags, MAX_NODE_TAGS, MAX_NODE_TAG_BYTES)
        .map_err(RegistryError::validation)?;

    let request = request.clone();
    let (result, _) = mutate_registry_file(path, move |file| {
        let now = Utc::now();
        prune_expired_install_sessions(file, now);
        let mut rotated_token = false;

        if let Some(index) = file
            .nodes
            .iter()
            .position(|node| node.node_id == request.node_id)
        {
            if let Some(node_label) = request.node_label.as_ref() {
                file.nodes[index].node_label = node_label.trim().to_string();
            }
            if !request.tags.is_empty() {
                file.nodes[index].tags = normalized_tags.clone();
            }
            // install_session 必须带上当时有效的明文 token 给本次 install 流程使用;
            // #56 改造之后 disk 上不再有明文,因此唯一能把明文传给 agent 的位置就是这里。
            // 每次 issue_node 都强制轮换 token,确保一次 install 流程对应一次 token 颁发。
            let session_plaintext = {
                let new_token = generate_token()?;
                file.nodes[index].token_hash = hash_token(&new_token).map_err(|error| {
                    RegistryError::internal("failed to hash token", error.into())
                })?;
                file.nodes[index].token_generation =
                    file.nodes[index].token_generation.saturating_add(1);
                file.nodes[index].token_expires_at =
                    Some(now + ChronoDuration::days(DEFAULT_TOKEN_VALIDITY_DAYS));
                file.nodes[index].token.clear();
                rotated_token = true;
                new_token
            };

            validate_registered_node(&file.nodes[index])?;
            let node = file.nodes[index].clone();
            let install_session =
                mint_install_session(file, &node.node_id, now, session_plaintext.clone())?;
            return Ok((
                IssueNodeResult {
                    node,
                    node_session_token: session_plaintext,
                    created: false,
                    rotated_token,
                    install_token: install_session.token,
                    install_token_expires_at: install_session.expires_at,
                },
                true,
            ));
        }

        let plaintext_token = generate_token()?;
        let token_hash = hash_token(&plaintext_token).map_err(|error| {
            RegistryError::internal("failed to hash issued token", error.into())
        })?;
        let node = RegisteredNode {
            node_id: request.node_id.trim().to_string(),
            node_label: request
                .node_label
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(request.node_id.as_str())
                .to_string(),
            token_hash,
            token_generation: 1,
            token: String::new(),
            tags: normalized_tags.clone(),
            created_at: now,
            token_expires_at: Some(now + ChronoDuration::days(DEFAULT_TOKEN_VALIDITY_DAYS)),
        };
        validate_registered_node(&node)?;

        file.nodes.push(node.clone());
        file.nodes
            .sort_by(|left, right| left.node_id.cmp(&right.node_id));
        let install_session =
            mint_install_session(file, &node.node_id, now, plaintext_token.clone())?;
        Ok((
            IssueNodeResult {
                node,
                node_session_token: plaintext_token,
                created: true,
                rotated_token,
                install_token: install_session.token,
                install_token_expires_at: install_session.expires_at,
            },
            true,
        ))
    })
    .await?;

    Ok(result)
}
