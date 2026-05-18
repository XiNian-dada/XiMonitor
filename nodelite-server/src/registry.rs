//! 节点注册表:服务端唯一的"哪些节点被允许上报"的事实来源。
//!
//! 注册表是一份 JSON 文件,内容由 `RegistryFile` 结构序列化得到。
//! 服务端进程与运维 CLI(`server install-agent` 等)都会读写这份文件,
//! 因此对每次写入都采用 flock + 原子替换的策略。
//!
//! 字段语义:
//! - [`RegisteredNode`]:被认证的 Agent 凭证(node_id + token)。
//! - [`InstallSession`]:一次性的"安装令牌",拥有它可以拉取 Agent 配置。

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use getrandom::fill as fill_random;
use nodelite_proto::{MAX_NODE_TAG_BYTES, MAX_NODE_TAGS, NodeIdentity};
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::sync::RwLock;
use url::Url;

use crate::auth::constant_time_compare_bytes;
use crate::encoding::hex_encode;
use crate::fs_security::{create_private_dir_all, ensure_directory_mode};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

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

/// 一次成功的 token 验证 / 颁发结果:同时返回身份和当时的代次,
/// 供 WS 会话捕获 generation 用于后续 hot-path 比较。
#[derive(Debug, Clone)]
pub struct AuthorizedNode {
    pub identity: NodeIdentity,
    pub generation: u64,
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
    pub rotate_token: bool,
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
    nodes: Vec<RegisteredNode>,
    #[serde(default)]
    install_sessions: Vec<InstallSession>,
}

/// 一次性安装令牌的有效期(分钟)。
const INSTALL_TOKEN_TTL_MINUTES: i64 = 15;

impl NodeRegistry {
    /// 从磁盘加载注册表;文件不存在时返回空注册表(首次部署的合理状态)。
    pub async fn load(path: &Path) -> Result<Self> {
        let state = load_registry_state(path).await?;

        Ok(Self {
            path: Arc::new(path.to_path_buf()),
            state: Arc::new(RwLock::new(state)),
        })
    }

    /// 校验 Agent 提交的 Hello 信息与 token,通过后返回"覆盖了注册表里权威字段"的身份
    /// 以及当时的 token 代次, 供 WS 会话后续 hot-path 比较使用。
    pub async fn authorize(&self, identity: &NodeIdentity, token: &str) -> Result<AuthorizedNode> {
        validate_runtime_identity(identity)?;
        validate_non_empty("hello.token", token)?;
        let state = self.state.read().await;
        authorize_identity(&state.entries, identity, token)
    }

    /// 判断当前 session 的 token **代次** 是否仍是该节点的最新代次。
    ///
    /// #56 之前这里接收 token 字符串做常量时间比较;现在为了避免每条 WS 消息
    /// 都跑 Argon2 verify(~20ms)的灾难性 CPU 占用, hot-path 改为只比较 generation。
    /// generation 由 [`authorize`] 在 hello 阶段返回, 每次 `refresh_token` /
    /// `issue_node --rotate-token` 都会让它 +1, 因此"管理员轮换了 token"会被
    /// 立即感知。
    pub async fn is_token_current(&self, node_id: &str, session_generation: u64) -> bool {
        let state = self.state.read().await;
        is_token_current(&state.entries, node_id, session_generation)
    }

    /// 查询节点 token 的过期时间。`None` 既可能表示节点不存在,也可能是旧注册表
    /// 里的永不过期 token;调用方通常只在节点已通过认证后使用它。
    pub async fn token_expires_at(&self, node_id: &str) -> Option<DateTime<Utc>> {
        let state = self.state.read().await;
        state
            .entries
            .get(node_id)
            .and_then(|node| node.token_expires_at)
    }

    /// 刷新节点的 Token:生成新明文 token, 哈希入库,代次 +1, 延长过期时间。
    /// 返回 (new_plaintext_token, expires_at, new_generation)。明文只在
    /// 进程内存里短暂存在,从这里被传递给 WS 端发送给 agent。
    pub async fn refresh_token(&self, node_id: &str) -> Result<(String, DateTime<Utc>, u64)> {
        let path = Arc::clone(&self.path);
        let node_id = node_id.to_string();
        let ((new_token, expires_at, generation), _) =
            mutate_registry_file(path.as_ref(), move |file| {
                let now = Utc::now();
                let Some(node) = file.nodes.iter_mut().find(|n| n.node_id == node_id) else {
                    bail!("node not found");
                };

                let new_token = generate_token()?;
                let expires_at = now + ChronoDuration::days(DEFAULT_TOKEN_VALIDITY_DAYS);
                node.token_hash =
                    hash_token(&new_token).context("failed to hash refreshed token")?;
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
    pub async fn reload(&self) -> Result<bool> {
        let next_state = load_registry_state(self.path.as_path()).await?;
        let mut state = self.state.write().await;
        if *state == next_state {
            return Ok(false);
        }

        *state = next_state;
        Ok(true)
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
    pub async fn consume_install_token(&self, token: &str) -> Result<Option<ConsumedInstall>> {
        validate_non_empty("install token", token)?;

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

    async fn replace_state_from_file(&self, file: RegistryFile) -> Result<()> {
        let state = registry_state_from_file(self.path.as_path(), file)?;
        let mut guard = self.state.write().await;
        *guard = state;
        Ok(())
    }
}

/// 创建或更新一个节点:首次出现时插入新条目,已存在时按需轮换 token、覆盖标签等。
///
/// 同时为该节点签发一个一次性安装令牌。这是 CLI 命令的核心入口。
pub async fn issue_node(path: &Path, request: IssueNodeRequest) -> Result<IssueNodeResult> {
    validate_identifier("node_id", &request.node_id)?;
    if let Some(node_label) = request.node_label.as_deref() {
        validate_non_empty("node_label", node_label)?;
    }
    let normalized_tags = normalize_string_list(request.tags.clone());
    validate_tag_list("tags", &normalized_tags)?;

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
            // 不论是否轮换 token, install_session 必须带上当时有效的明文 token
            // 给本次 install 流程使用; #56 改造之后 disk 上不再有明文,因此唯一
            // 能把明文传给 agent 的位置就是这里。
            let session_plaintext = if request.rotate_token {
                // 真的生成新明文并入库哈希。
                let new_token = generate_token()?;
                file.nodes[index].token_hash =
                    hash_token(&new_token).context("failed to hash rotated token")?;
                file.nodes[index].token_generation =
                    file.nodes[index].token_generation.saturating_add(1);
                file.nodes[index].token_expires_at =
                    Some(now + ChronoDuration::days(DEFAULT_TOKEN_VALIDITY_DAYS));
                file.nodes[index].token.clear();
                rotated_token = true;
                new_token
            } else {
                // 不轮换的情况:install_session 必须能给到一个**该节点目前持有的**
                // 明文。Disk 上只有哈希,无法逆推,所以这里强制轮换一次 ——
                // 这与历史行为有差异:历史上 `issue-node` 在节点已存在时不会
                // 强制换 token,但磁盘上存的就是明文,所以 install_session 可以
                // 直接 clone 它。改用哈希后,我们要么轮换、要么拒绝 install。
                // 选轮换:更安全,且 #56 验收里也写"一次 install 流程对应一次
                // token 颁发"是合理的语义。
                let new_token = generate_token()?;
                file.nodes[index].token_hash =
                    hash_token(&new_token).context("failed to hash re-issued token")?;
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
        let token_hash = hash_token(&plaintext_token).context("failed to hash issued token")?;
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

/// 从 `public_base_url` 推导 Agent 应连接的 WebSocket URL(http → ws,https → wss)。
pub fn build_agent_server_url(public_base_url: &str) -> Result<String> {
    let mut url = Url::parse(public_base_url)
        .with_context(|| "invalid server.public_base_url".to_string())?;
    let scheme = match url.scheme() {
        "http" => "ws",
        "https" => "wss",
        other => bail!("unsupported public_base_url scheme for agent install: {other}"),
    };
    url.set_scheme(scheme)
        .map_err(|_| anyhow!("failed to set websocket scheme"))?;
    url.set_path("/ws");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.into())
}

/// 拼装"安装脚本下载 URL"。
pub fn build_install_script_url(public_base_url: &str) -> Result<String> {
    let mut url = Url::parse(public_base_url)
        .with_context(|| "invalid server.public_base_url".to_string())?;
    url.set_path("/install/install-agent.sh");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.into())
}

/// 拼装"安装引导 URL":Agent 安装脚本会带上 Bearer 安装令牌请求这个地址换取自己的 agent.toml。
pub fn build_install_bootstrap_url(public_base_url: &str) -> Result<String> {
    let mut url = Url::parse(public_base_url)
        .with_context(|| "invalid server.public_base_url".to_string())?;
    url.set_path("/install/bootstrap");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.into())
}

/// 从 GitHub 仓库 URL 推导 `releases/latest/download` 形式的下载基地址。
/// 只支持 GitHub 仓库,避免误把任意 URL 当作发布源。
pub fn build_github_release_base_url(repository_url: &str) -> Result<String> {
    let url = Url::parse(repository_url).with_context(|| "invalid repository URL".to_string())?;
    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("repository URL must include a host"))?;
    if host != "github.com" {
        bail!("only GitHub repositories are supported for latest release installs");
    }

    let mut segments = url
        .path_segments()
        .ok_or_else(|| anyhow!("repository URL must include an owner and repo"))?;
    let owner = segments
        .next()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("repository URL must include an owner"))?
        .to_string();
    let repo = segments
        .next()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("repository URL must include a repo"))?
        .trim_end_matches(".git")
        .to_string();

    let mut release_url = url;
    release_url.set_path(&format!("{owner}/{repo}/releases/latest/download"));
    release_url.set_query(None);
    release_url.set_fragment(None);
    Ok(release_url.into())
}

/// 缺省下载源:由当前 crate 在编译期注入的仓库地址推导而来。
pub fn default_agent_release_base_url() -> Result<String> {
    build_github_release_base_url(env!("CARGO_PKG_REPOSITORY"))
}

/// 渲染"复制即可用"的安装命令文本。
///
/// 输出形如多行 `curl ... | sh ...`,把安装令牌通过环境变量传递,避免它出现在 `ps` 列表中。
pub fn render_install_command(
    public_base_url: &str,
    install_token: &str,
    agent_release_base_url: &str,
) -> Result<String> {
    let script_url = build_install_script_url(public_base_url)?;
    let bootstrap_url = build_install_bootstrap_url(public_base_url)?;
    let lines = [
        format!("curl -fsSL {} | \\", shell_quote(&script_url)),
        format!(
            "  NODELITE_AGENT_INSTALL_TOKEN={} sh -s -- \\",
            shell_quote(install_token)
        ),
        format!("  --bootstrap-url {} \\", shell_quote(&bootstrap_url)),
        format!("  --base-url {}", shell_quote(agent_release_base_url)),
    ];

    Ok(lines.join("\n"))
}

/// 渲染就地升级 Agent 的命令文本。与 `render_install_command` 类似,但不需要令牌。
pub fn render_upgrade_command(
    public_base_url: &str,
    agent_release_base_url: &str,
) -> Result<String> {
    let script_url = build_install_script_url(public_base_url)?;
    let lines = [
        format!("curl -fsSL {} | \\", shell_quote(&script_url)),
        "  NODELITE_AGENT_MODE=upgrade sh -s -- \\".to_string(),
        "  --mode upgrade \\".to_string(),
        format!("  --base-url {}", shell_quote(agent_release_base_url)),
    ];

    Ok(lines.join("\n"))
}

/// 渲染单个节点的 `agent.toml` 文本,作为引导接口的响应体。
pub fn render_agent_config(
    public_base_url: &str,
    node: &RegisteredNode,
    plaintext_token: &str,
) -> Result<String> {
    let server_url = build_agent_server_url(public_base_url)?;
    let mut content = String::new();
    content.push_str("[agent]\n");
    content.push_str(&format!("node_id = \"{}\"\n", toml_escape(&node.node_id)));
    content.push_str(&format!(
        "node_label = \"{}\"\n",
        toml_escape(&node.node_label)
    ));
    content.push_str(&format!("server = \"{}\"\n", toml_escape(&server_url)));
    content.push_str(&format!("token = \"{}\"\n", toml_escape(plaintext_token)));
    content.push_str("report_interval_secs = 5\n");
    if !node.tags.is_empty() {
        let tags = node
            .tags
            .iter()
            .map(|tag| format!("\"{}\"", toml_escape(tag)))
            .collect::<Vec<_>>()
            .join(", ");
        content.push_str(&format!("tags = [{tags}]\n"));
    }
    Ok(content)
}

async fn load_registry_file(path: &Path) -> Result<RegistryFile> {
    let content = match fs::read_to_string(path).await {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RegistryFile::default());
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read node registry {}", path.display()));
        }
    };

    let file: RegistryFile = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse node registry {}", path.display()))?;
    validate_registry_file(path, &file)?;
    Ok(file)
}

fn load_registry_file_sync(path: &Path) -> Result<RegistryFile> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RegistryFile::default());
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read node registry {}", path.display()));
        }
    };

    let file: RegistryFile = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse node registry {}", path.display()))?;
    validate_registry_file(path, &file)?;
    Ok(file)
}

async fn load_registry_state(path: &Path) -> Result<RegistryState> {
    let mut file = load_registry_file(path).await?;
    prune_expired_install_sessions(&mut file, Utc::now());

    // #56: 升级老版本的明文 token 到 Argon2id 哈希。一旦发现旧字段, 哈希后
    // 立即落盘, 之后磁盘上不再有任何节点的明文。这里用同步 file IO 是因为
    // mutate_registry_file 的语义已经覆盖了 flock + 原子替换, 我们直接复用。
    let migrated = migrate_legacy_tokens(&mut file)?;
    if migrated {
        let path_buf = path.to_path_buf();
        let file_clone = file.clone();
        tokio::task::spawn_blocking(move || save_registry_file_sync(&path_buf, &file_clone))
            .await
            .context("legacy token migration task failed")??;
    }

    registry_state_from_file(path, file)
}

fn registry_state_from_file(path: &Path, file: RegistryFile) -> Result<RegistryState> {
    let mut entries = HashMap::with_capacity(file.nodes.len());
    for node in file.nodes {
        if entries.insert(node.node_id.clone(), node).is_some() {
            bail!("duplicate node_id found in {}", path.display());
        }
    }
    let mut install_sessions = HashMap::with_capacity(file.install_sessions.len());
    for session in file.install_sessions {
        if install_sessions
            .insert(session.token.clone(), session)
            .is_some()
        {
            bail!("duplicate install token found in {}", path.display());
        }
    }

    Ok(RegistryState {
        entries,
        install_sessions,
    })
}

fn save_registry_file_sync(path: &Path, file: &RegistryFile) -> Result<()> {
    validate_registry_file(path, file)?;

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        create_private_dir_all(parent)?;
    }

    let payload =
        serde_json::to_string_pretty(file).context("failed to serialize node registry")?;
    let tmp_path = temporary_registry_path(path)?;
    write_registry_payload(&tmp_path, &payload)
        .with_context(|| format!("failed to write {}", tmp_path.display()))?;
    harden_registry_permissions(&tmp_path)
        .with_context(|| format!("failed to set permissions on {}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, path)
        .with_context(|| format!("failed to replace {}", path.display()))?;
    // rename 之后再 fsync 父目录,保证目录项变更也落盘,与 write_registry_payload 内部的
    // fsync 配合,使 crash 后要么看到旧文件、要么看到完整新文件,不会出现空文件。
    sync_parent_dir(path);
    verify_registry_permissions(path)
        .with_context(|| format!("insecure permissions after replacing {}", path.display()))?;
    Ok(())
}

/// 在 `spawn_blocking` 中以"读 → 改 → 写"的方式更新注册表文件,并由 flock 保护互斥。
async fn mutate_registry_file<T, F>(path: &Path, operation: F) -> Result<(T, RegistryFile)>
where
    T: Send + 'static,
    F: FnOnce(&mut RegistryFile) -> Result<(T, bool)> + Send + 'static,
{
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        // 注册表的修改可能来自运行中的 Server,也可能来自一次性 CLI 命令,
        // 所以在 read-modify-write 之前先拿到文件锁,保证串行化。
        let _lock = acquire_registry_lock(&path)?;
        let mut file = load_registry_file_sync(&path)?;
        let (value, should_persist) = operation(&mut file)?;
        if should_persist {
            save_registry_file_sync(&path, &file)?;
        }
        Ok((value, file))
    })
    .await
    .context("registry mutation task failed")?
}

fn temporary_registry_path(path: &Path) -> Result<PathBuf> {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("server.json");
    // 并发写时固定 tmp 名会互相覆盖;加随机后缀让每个写操作拿到独立临时文件。
    let mut suffix = [0u8; 8];
    getrandom::fill(&mut suffix).context("failed to generate registry temp-file suffix")?;
    let suffix_hex = suffix
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(path.with_file_name(format!("{file_name}.tmp.{suffix_hex}")))
}

fn write_registry_payload(path: &Path, payload: &str) -> Result<()> {
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);

    let mut file = options
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    file.write_all(payload.as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))?;
    // rename 前确保数据已经刷盘,避免主机崩溃后留下空的注册表文件 —— 注册表丢失
    // 等于所有 Agent 鉴权失败,后果比一次写入失败更严重。
    file.sync_all()
        .with_context(|| format!("failed to fsync {}", path.display()))?;
    Ok(())
}

/// rename 之后 fsync 父目录,使新目录项随之持久化。
/// 打不开父目录(权限等)时静默退出 —— 数据已经 fsync,目录项丢失只意味着回退到上一份注册表。
fn sync_parent_dir(path: &Path) {
    let Some(parent) = path.parent() else {
        return;
    };
    if parent.as_os_str().is_empty() {
        return;
    }
    let _ = std::fs::File::open(parent).and_then(|dir| dir.sync_all());
}

fn registry_lock_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("server.json");
    path.with_file_name(format!("{file_name}.lock"))
}

fn acquire_registry_lock(path: &Path) -> Result<RegistryFileLock> {
    let lock_path = registry_lock_path(path);
    if let Some(parent) = lock_path.parent()
        && !parent.as_os_str().is_empty()
    {
        create_private_dir_all(parent)?;
    }

    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);

    let file = options
        .open(&lock_path)
        .with_context(|| format!("failed to open {}", lock_path.display()))?;
    harden_registry_permissions(&lock_path)?;
    lock_file_exclusive(&file)
        .with_context(|| format!("failed to lock {}", lock_path.display()))?;
    Ok(RegistryFileLock { file, lock_path })
}

fn lock_file_exclusive(file: &File) -> Result<()> {
    #[cfg(unix)]
    {
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if result != 0 {
            return Err(std::io::Error::last_os_error()).context("flock failed");
        }
    }

    #[cfg(not(unix))]
    {
        let _ = file;
    }

    Ok(())
}

fn unlock_file(file: &File) {
    #[cfg(unix)]
    {
        let _ = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    }

    #[cfg(not(unix))]
    {
        let _ = file;
    }
}

fn harden_registry_permissions(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        ensure_directory_mode(parent, 0o700)?;
    }
    #[cfg(unix)]
    {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to chmod {}", path.display()))?;
    }

    #[cfg(not(unix))]
    {
        let _ = path;
    }

    Ok(())
}

fn verify_registry_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        let mode = std::fs::metadata(path)
            .with_context(|| format!("failed to stat {}", path.display()))?
            .permissions()
            .mode()
            & 0o777;
        if mode != 0o600 {
            bail!("{} must be mode 0600, got {mode:o}", path.display());
        }
    }

    #[cfg(not(unix))]
    {
        let _ = path;
    }

    Ok(())
}

fn validate_registry_file(path: &Path, file: &RegistryFile) -> Result<()> {
    let mut seen_nodes = HashMap::with_capacity(file.nodes.len());
    for node in &file.nodes {
        validate_registered_node(node)?;
        if seen_nodes.insert(node.node_id.as_str(), ()).is_some() {
            bail!("duplicate node_id {} in {}", node.node_id, path.display());
        }
    }
    let mut seen_install_tokens = HashMap::with_capacity(file.install_sessions.len());
    for session in &file.install_sessions {
        validate_install_session(session)?;
        if !seen_nodes.contains_key(session.node_id.as_str()) {
            bail!(
                "install token for unknown node_id {} in {}",
                session.node_id,
                path.display()
            );
        }
        if seen_install_tokens
            .insert(session.token.as_str(), ())
            .is_some()
        {
            bail!("duplicate install token in {}", path.display());
        }
    }
    Ok(())
}

fn validate_registered_node(node: &RegisteredNode) -> Result<()> {
    validate_identifier("node.node_id", &node.node_id)?;
    validate_non_empty("node.node_label", &node.node_label)?;
    // 注册表中 token 必须以哈希形式存在; 旧版本的明文 `token` 字段
    // 在 `migrate_legacy_tokens` 中已经被搬迁过来。
    if node.token_hash.is_empty() && node.token.is_empty() {
        bail!("node.token_hash is empty");
    }
    validate_tag_list("node.tags", &node.tags)?;
    Ok(())
}

fn validate_install_session(session: &InstallSession) -> Result<()> {
    validate_non_empty("install_session.token", &session.token)?;
    validate_identifier("install_session.node_id", &session.node_id)?;
    Ok(())
}

fn prune_expired_install_sessions(file: &mut RegistryFile, now: DateTime<Utc>) -> bool {
    let original_len = file.install_sessions.len();
    file.install_sessions
        .retain(|session| session.expires_at > now);
    original_len != file.install_sessions.len()
}

fn mint_install_session(
    file: &mut RegistryFile,
    node_id: &str,
    now: DateTime<Utc>,
    node_session_token: String,
) -> Result<InstallSession> {
    file.install_sessions
        .retain(|session| session.node_id != node_id);
    let session = InstallSession {
        token: generate_token()?,
        node_id: node_id.to_string(),
        created_at: now,
        expires_at: now + ChronoDuration::minutes(INSTALL_TOKEN_TTL_MINUTES),
        node_session_token,
    };
    file.install_sessions.push(session.clone());
    Ok(session)
}

fn authorize_identity(
    entries: &HashMap<String, RegisteredNode>,
    identity: &NodeIdentity,
    token: &str,
) -> Result<AuthorizedNode> {
    if let Some(entry) = entries.get(identity.node_id.as_str()) {
        if !token_matches_entry(token, entry) {
            bail!("unauthorized");
        }

        if !token_is_unexpired(entry, Utc::now()) {
            bail!("token expired");
        }

        let mut identity = identity.clone();
        identity.node_id = entry.node_id.clone();
        identity.node_label = entry.node_label.clone();
        identity.tags = entry.tags.clone();
        return Ok(AuthorizedNode {
            identity,
            generation: entry.token_generation,
        });
    }

    bail!("unauthorized");
}

fn is_token_current(
    entries: &HashMap<String, RegisteredNode>,
    node_id: &str,
    session_generation: u64,
) -> bool {
    if let Some(entry) = entries.get(node_id) {
        return entry.token_generation == session_generation
            && token_is_unexpired(entry, Utc::now());
    }

    false
}

/// 在两种 token 存储格式之间做兼容比较。
///
/// 新格式: `entry.token_hash` 是 Argon2id PHC 字符串, 用 `verify_token`。
/// 旧格式: `entry.token_hash` 为空, `entry.token` 是明文, 走 constant-time eq。
/// 后者应当只在首次加载未迁移的 registry.json 时出现 —— `migrate_legacy_tokens`
/// 会立即把它升级到新格式。
fn token_matches_entry(input: &str, entry: &RegisteredNode) -> bool {
    if !entry.token_hash.is_empty() {
        verify_token(input, &entry.token_hash)
    } else if !entry.token.is_empty() {
        constant_time_eq(input, &entry.token)
    } else {
        false
    }
}

fn token_is_unexpired(entry: &RegisteredNode, now: DateTime<Utc>) -> bool {
    entry
        .token_expires_at
        .is_none_or(|expires_at| now < expires_at)
}

/// 用统一参数构造 Argon2id 实例。OWASP 2023 服务器档位:
/// memory 19 MiB / iterations 2 / parallelism 1。
fn argon2_instance() -> Argon2<'static> {
    let params = Params::new(
        ARGON2_MEMORY_KIB,
        ARGON2_ITERATIONS,
        ARGON2_PARALLELISM,
        None,
    )
    .expect("argon2 parameters are constants picked from OWASP 2023 cheat sheet");
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
}

/// 把明文 token 哈希成 Argon2id PHC 字符串。返回的字符串自带 salt + params,
/// 可直接存入 registry.json 并在后续 verify 时无需额外参数。
fn hash_token(token: &str) -> Result<String> {
    let mut salt_bytes = [0u8; 16];
    fill_random(&mut salt_bytes).context("failed to generate token salt")?;
    let salt = SaltString::encode_b64(&salt_bytes)
        .map_err(|error| anyhow!("failed to encode token salt: {error}"))?;
    let hash = argon2_instance()
        .hash_password(token.as_bytes(), &salt)
        .map_err(|error| anyhow!("failed to hash token: {error}"))?;
    Ok(hash.to_string())
}

/// 用 PHC 字符串校验候选 token。失败 / 解析错误一律返回 false,
/// 永远不让密码学错误溢出成 panic。
fn verify_token(candidate: &str, phc: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(phc) else {
        return false;
    };
    argon2_instance()
        .verify_password(candidate.as_bytes(), &parsed)
        .is_ok()
}

/// 把还在用明文 `token` 字段的旧 registry 条目迁移到 `token_hash`。
///
/// 这一步在 [`load_registry_state`] 中完成,完成后调用方会把 file 写回磁盘,
/// 之后磁盘上不再保留明文。返回值表示是否触发了任何变更。
fn migrate_legacy_tokens(file: &mut RegistryFile) -> Result<bool> {
    let mut changed = false;
    for node in &mut file.nodes {
        if node.token_hash.is_empty() && !node.token.is_empty() {
            node.token_hash = hash_token(&node.token)
                .with_context(|| format!("hash legacy token for node {}", node.node_id))?;
            // 用 zero-overwrite 清掉明文。即便后续 file 没立即写盘,内存里的副本
            // 也尽量短地存在明文。
            node.token.clear();
            if node.token_generation == 0 {
                node.token_generation = 1;
            }
            changed = true;
        }
    }
    Ok(changed)
}

/// 常量时间字符串比较,仅在旧版本明文 token 兼容路径使用。
fn constant_time_eq(left: &str, right: &str) -> bool {
    constant_time_compare_bytes(left.as_bytes(), right.as_bytes())
}

struct RegistryFileLock {
    file: File,
    lock_path: PathBuf,
}

impl Drop for RegistryFileLock {
    fn drop(&mut self) {
        release_registry_lock_with(
            || unlock_file(&self.file),
            || {
                let _ = harden_registry_permissions(&self.lock_path);
            },
        );
    }
}

fn release_registry_lock_with<U, H>(unlock: U, harden: H)
where
    U: FnOnce(),
    H: FnOnce(),
{
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(unlock));
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(harden));
}

fn validate_runtime_identity(identity: &NodeIdentity) -> Result<()> {
    validate_identifier("identity.node_id", &identity.node_id)?;
    validate_non_empty("identity.node_label", &identity.node_label)?;
    validate_non_empty("identity.agent_version", &identity.agent_version)?;
    validate_non_empty("identity.hostname", &identity.hostname)?;
    validate_non_empty("identity.os", &identity.os)?;
    validate_tag_list("identity.tags", &identity.tags)?;
    Ok(())
}

fn validate_identifier(field: &str, value: &str) -> Result<()> {
    validate_non_empty(field, value)?;
    if value.len() > 128 {
        bail!("{field} must be <= 128 characters");
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        bail!("{field} must use only ASCII letters, numbers, '-', '_' or '.'");
    }
    Ok(())
}

fn validate_non_empty(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("{field} must not be empty");
    }
    Ok(())
}

fn normalize_string_list(values: Vec<String>) -> Vec<String> {
    let mut values: Vec<String> = values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect();
    values.sort();
    values.dedup();
    values
}

fn validate_tag_list(field: &str, values: &[String]) -> Result<()> {
    if values.len() > MAX_NODE_TAGS {
        bail!("{field} must contain at most {MAX_NODE_TAGS} tags");
    }
    for (index, value) in values.iter().enumerate() {
        if value.len() > MAX_NODE_TAG_BYTES {
            bail!("{field}[{index}] must be <= {MAX_NODE_TAG_BYTES} bytes");
        }
    }
    Ok(())
}

/// 生成 256-bit 的随机 token 并以十六进制字符串形式返回。
fn generate_token() -> Result<String> {
    let mut bytes = [0_u8; 32];
    fill_random(&mut bytes).context("failed to gather secure random bytes")?;
    Ok(hex_encode(&bytes))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn toml_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use chrono::{DateTime, Duration as ChronoDuration, TimeZone, Utc};
    use proptest::prelude::*;
    use tokio::runtime::Runtime;

    use super::{
        IssueNodeRequest, MAX_NODE_TAG_BYTES, NodeRegistry, RegisteredNode, RegistryFile,
        build_agent_server_url, build_github_release_base_url, default_agent_release_base_url,
        issue_node, render_install_command, validate_registered_node,
    };
    use nodelite_proto::NodeIdentity;

    fn legacy_node(
        node_id: &str,
        node_label: &str,
        token: &str,
        token_expires_at: Option<DateTime<Utc>>,
    ) -> RegisteredNode {
        RegisteredNode {
            node_id: node_id.to_string(),
            node_label: node_label.to_string(),
            token_hash: String::new(),
            token_generation: 0,
            token: token.to_string(),
            tags: Vec::new(),
            created_at: Utc::now(),
            token_expires_at,
        }
    }

    fn identity_for(node_id: &str) -> NodeIdentity {
        NodeIdentity {
            node_id: node_id.to_string(),
            node_label: node_id.to_string(),
            hostname: format!("{node_id}.internal"),
            os: "Ubuntu".to_string(),
            kernel_version: None,
            cpu_model: None,
            cpu_cores: 2,
            agent_version: "0.1.0".to_string(),
            boot_time: None,
            tags: Vec::new(),
        }
    }

    #[test]
    fn agent_server_url_uses_wss_for_https() {
        let url = build_agent_server_url("https://monitor.example.com").expect("url should build");
        assert_eq!(url, "wss://monitor.example.com/ws");
    }

    #[test]
    fn registry_authorizes_per_node_token_and_overrides_metadata() {
        let runtime = Runtime::new().expect("runtime should build");
        runtime.block_on(async {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should be monotonic enough")
                .as_nanos();
            let temp_dir =
                std::env::temp_dir().join(format!("nodelite-registry-auth-test-{unique}"));
            std::fs::create_dir_all(&temp_dir).expect("temp dir should exist");
            let path = temp_dir.join("server.json");
            let mut node = legacy_node("osaka-01", "Osaka 01", "secret", None);
            node.tags = vec!["edge".to_string()];
            let file = RegistryFile {
                nodes: vec![node],
                install_sessions: Vec::new(),
            };
            std::fs::write(&path, serde_json::to_string_pretty(&file).expect("json"))
                .expect("registry should be written");
            let registry = NodeRegistry::load(&path)
                .await
                .expect("registry should load");
            let identity = NodeIdentity {
                node_id: "osaka-01".to_string(),
                node_label: "Wrong".to_string(),
                hostname: "osaka-01.internal".to_string(),
                os: "Ubuntu".to_string(),
                kernel_version: None,
                cpu_model: None,
                cpu_cores: 2,
                agent_version: "0.1.0".to_string(),
                boot_time: None,
                tags: vec!["wrong".to_string()],
            };

            let authorized = registry
                .authorize(&identity, "secret")
                .await
                .expect("identity should authorize");
            assert_eq!(authorized.identity.node_label, "Osaka 01");
            assert_eq!(authorized.identity.tags, vec!["edge"]);
            assert_eq!(authorized.generation, 1);

            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_dir(&temp_dir);
        });
    }

    #[test]
    fn load_hashes_legacy_plaintext_tokens_and_persists_migration() {
        let runtime = Runtime::new().expect("runtime should build");
        runtime.block_on(async {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should be monotonic enough")
                .as_nanos();
            let temp_dir =
                std::env::temp_dir().join(format!("nodelite-registry-migration-test-{unique}"));
            std::fs::create_dir_all(&temp_dir).expect("temp dir should exist");
            let path = temp_dir.join("server.json");
            let file = RegistryFile {
                nodes: vec![legacy_node("legacy-01", "Legacy 01", "legacy-secret", None)],
                install_sessions: Vec::new(),
            };
            std::fs::write(&path, serde_json::to_string_pretty(&file).expect("json"))
                .expect("registry should be written");

            let registry = NodeRegistry::load(&path)
                .await
                .expect("registry should load");
            let authorized = registry
                .authorize(&identity_for("legacy-01"), "legacy-secret")
                .await
                .expect("legacy token should still authorize after migration");
            assert_eq!(authorized.generation, 1);

            let stored = std::fs::read_to_string(&path).expect("registry should be readable");
            assert!(!stored.contains("legacy-secret"));
            let parsed: RegistryFile =
                serde_json::from_str(&stored).expect("stored registry should parse");
            assert_eq!(parsed.nodes.len(), 1);
            assert!(parsed.nodes[0].token.is_empty());
            assert!(parsed.nodes[0].token_hash.starts_with("$argon2id$"));
            assert!(super::verify_token(
                "legacy-secret",
                &parsed.nodes[0].token_hash
            ));
            assert_eq!(parsed.nodes[0].token_generation, 1);

            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_dir(&temp_dir);
        });
    }

    #[test]
    fn issue_node_persists_registry_and_renders_install_command() {
        let runtime = Runtime::new().expect("runtime should build");
        runtime.block_on(async {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should be monotonic enough")
                .as_nanos();
            let temp_dir = std::env::temp_dir().join(format!("nodelite-registry-test-{unique}"));
            std::fs::create_dir_all(&temp_dir).expect("temp dir should exist");
            let path = temp_dir.join("server.json");
            let issued = issue_node(
                &path,
                IssueNodeRequest {
                    node_id: "hk-01".to_string(),
                    node_label: Some("Hong Kong 01".to_string()),
                    tags: vec!["edge".to_string(), "apac".to_string()],
                    rotate_token: false,
                },
            )
            .await
            .expect("node should be issued");
            assert!(issued.created);

            let stored = std::fs::read_to_string(&path).expect("registry should be stored");
            let parsed: RegistryFile =
                serde_json::from_str(&stored).expect("stored registry should parse");
            assert_eq!(parsed.nodes.len(), 1);
            assert_eq!(parsed.install_sessions.len(), 1);
            assert!(parsed.nodes[0].token.is_empty());
            assert!(parsed.nodes[0].token_hash.starts_with("$argon2id$"));
            assert_ne!(parsed.nodes[0].token_hash, issued.node_session_token);
            assert_eq!(parsed.nodes[0].token_generation, 1);
            assert_eq!(parsed.install_sessions[0].token, issued.install_token);
            assert_eq!(
                parsed.install_sessions[0].node_session_token,
                issued.node_session_token
            );

            let command = render_install_command(
                "https://monitor.example.com",
                &issued.install_token,
                "https://github.com/XiNian-dada/NodeLite/releases/latest/download",
            )
            .expect("install command should render");
            assert!(command.contains("--bootstrap-url"));
            assert!(command.contains("/install-agent.sh"));
            assert!(command.contains("NODELITE_AGENT_INSTALL_TOKEN="));
            assert!(command.contains(&issued.install_token));
            assert!(!command.contains(&issued.node_session_token));

            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_dir(&temp_dir);
        });
    }

    #[test]
    fn registry_reload_picks_up_rotated_tokens() {
        let runtime = Runtime::new().expect("runtime should build");
        runtime.block_on(async {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should be monotonic enough")
                .as_nanos();
            let temp_dir =
                std::env::temp_dir().join(format!("nodelite-registry-reload-test-{unique}"));
            std::fs::create_dir_all(&temp_dir).expect("temp dir should exist");
            let path = temp_dir.join("server.json");

            let issued = issue_node(
                &path,
                IssueNodeRequest {
                    node_id: "hk-01".to_string(),
                    node_label: Some("Hong Kong 01".to_string()),
                    tags: Vec::new(),
                    rotate_token: false,
                },
            )
            .await
            .expect("node should be issued");
            let old_token = issued.node_session_token.clone();
            let registry = NodeRegistry::load(&path)
                .await
                .expect("registry should load");
            let identity = identity_for("hk-01");
            let old_authorized = registry
                .authorize(&identity, &old_token)
                .await
                .expect("old token should authorize before rotation");
            assert!(
                registry
                    .is_token_current("hk-01", old_authorized.generation)
                    .await
            );

            let rotated = issue_node(
                &path,
                IssueNodeRequest {
                    node_id: "hk-01".to_string(),
                    node_label: Some("Hong Kong 01".to_string()),
                    tags: Vec::new(),
                    rotate_token: true,
                },
            )
            .await
            .expect("node token should rotate");
            assert!(registry.reload().await.expect("reload should succeed"));
            assert!(
                !registry
                    .is_token_current("hk-01", old_authorized.generation)
                    .await
            );
            assert!(
                registry.authorize(&identity, &old_token).await.is_err(),
                "old plaintext token should no longer authorize"
            );
            let rotated_authorized = registry
                .authorize(&identity, &rotated.node_session_token)
                .await
                .expect("rotated token should authorize");
            assert!(
                registry
                    .is_token_current("hk-01", rotated_authorized.generation)
                    .await
            );

            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_dir(&temp_dir);
        });
    }

    #[tokio::test]
    async fn issued_tokens_default_to_thirty_day_expiry() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!("nodelite-registry-expiry-test-{unique}"));
        std::fs::create_dir_all(&temp_dir).expect("temp dir");
        let path = temp_dir.join("server.json");

        let issued = issue_node(
            &path,
            IssueNodeRequest {
                node_id: "hk-01".to_string(),
                node_label: Some("Hong Kong 01".to_string()),
                tags: Vec::new(),
                rotate_token: false,
            },
        )
        .await
        .expect("node should be issued");

        let expires_at = issued
            .node
            .token_expires_at
            .expect("issued token should carry expiry");
        let remaining = expires_at - Utc::now();
        assert!(
            remaining >= ChronoDuration::days(29)
                && remaining <= ChronoDuration::days(30) + ChronoDuration::minutes(1),
            "expected about 30 days of remaining validity, got {remaining:?}",
        );

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&temp_dir);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]

        #[test]
        fn normalize_string_list_returns_sorted_trimmed_deduped_values(
            values in proptest::collection::vec(".*", 0..32),
        ) {
            let normalized = super::normalize_string_list(values.clone());

            for value in &normalized {
                prop_assert!(!value.is_empty());
                prop_assert_eq!(value.trim(), value);
            }

            let mut sorted = normalized.clone();
            sorted.sort();
            prop_assert_eq!(normalized.as_slice(), sorted.as_slice());

            let mut deduped = normalized.clone();
            deduped.dedup();
            prop_assert_eq!(normalized.as_slice(), deduped.as_slice());

            for value in &normalized {
                prop_assert!(values.iter().any(|original| original.trim() == value));
            }
        }

        #[test]
        fn generate_token_always_returns_lowercase_hex(_case in any::<u8>()) {
            let token = super::generate_token().expect("token generation should succeed");
            prop_assert_eq!(token.len(), 64);
            prop_assert!(
                token
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            );
        }
    }

    #[test]
    fn registry_lock_drop_cleanup_swallows_panics_and_runs_both_steps() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let cleanup_steps = Arc::new(AtomicUsize::new(0));
        let unlock_steps = Arc::clone(&cleanup_steps);
        let harden_steps = Arc::clone(&cleanup_steps);

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            super::release_registry_lock_with(
                move || {
                    unlock_steps.fetch_or(0b01, Ordering::SeqCst);
                    panic!("unlock panic");
                },
                move || {
                    harden_steps.fetch_or(0b10, Ordering::SeqCst);
                    panic!("harden panic");
                },
            );
        }));

        assert!(
            result.is_ok(),
            "cleanup helper should swallow internal panics"
        );
        assert_eq!(cleanup_steps.load(Ordering::SeqCst), 0b11);
    }

    #[test]
    fn install_tokens_are_one_time_use() {
        let runtime = Runtime::new().expect("runtime should build");
        runtime.block_on(async {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should be monotonic enough")
                .as_nanos();
            let temp_dir =
                std::env::temp_dir().join(format!("nodelite-install-token-test-{unique}"));
            std::fs::create_dir_all(&temp_dir).expect("temp dir should exist");
            let path = temp_dir.join("server.json");

            let issued = issue_node(
                &path,
                IssueNodeRequest {
                    node_id: "hk-01".to_string(),
                    node_label: Some("Hong Kong 01".to_string()),
                    tags: Vec::new(),
                    rotate_token: false,
                },
            )
            .await
            .expect("node should be issued");
            let registry = NodeRegistry::load(&path)
                .await
                .expect("registry should load");

            let consumed = registry
                .consume_install_token(&issued.install_token)
                .await
                .expect("install token should be consumable")
                .expect("install token should resolve to a node");
            assert_eq!(consumed.node.node_id, issued.node.node_id);
            assert_eq!(consumed.node_session_token, issued.node_session_token);
            assert!(
                registry
                    .consume_install_token(&issued.install_token)
                    .await
                    .expect("second install token lookup should succeed")
                    .is_none()
            );
            let stored = std::fs::read_to_string(&path).expect("registry should be readable");
            assert!(!stored.contains(&issued.node_session_token));

            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_dir(&temp_dir);
        });
    }

    #[test]
    fn expired_tokens_are_not_current_after_handshake() {
        let runtime = Runtime::new().expect("runtime should build");
        runtime.block_on(async {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should be monotonic enough")
                .as_nanos();
            let temp_dir =
                std::env::temp_dir().join(format!("nodelite-expired-token-test-{unique}"));
            std::fs::create_dir_all(&temp_dir).expect("temp dir should exist");
            let path = temp_dir.join("server.json");
            let file = RegistryFile {
                nodes: vec![legacy_node(
                    "expired-01",
                    "Expired 01",
                    "secret",
                    Some(Utc::now() - ChronoDuration::seconds(1)),
                )],
                install_sessions: Vec::new(),
            };
            std::fs::write(&path, serde_json::to_string_pretty(&file).expect("json"))
                .expect("registry should be written");
            let registry = NodeRegistry::load(&path)
                .await
                .expect("registry should load");

            let error = registry
                .authorize(&identity_for("expired-01"), "secret")
                .await
                .expect_err("expired token should not authorize");
            assert_eq!(error.to_string(), "token expired");
            assert!(!registry.is_token_current("expired-01", 1).await);

            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_dir(&temp_dir);
        });
    }

    #[test]
    fn token_is_expired_at_exact_expiry_moment() {
        let expires_at = Utc.with_ymd_and_hms(2026, 5, 18, 12, 0, 0).unwrap();
        let entry = RegisteredNode {
            node_id: "boundary-01".to_string(),
            node_label: "Boundary 01".to_string(),
            token_hash: "hash".to_string(),
            token_generation: 1,
            token: "secret".to_string(),
            tags: Vec::new(),
            created_at: expires_at - ChronoDuration::minutes(5),
            token_expires_at: Some(expires_at),
        };

        assert!(!super::token_is_unexpired(&entry, expires_at));
        assert!(super::token_is_unexpired(
            &entry,
            expires_at - ChronoDuration::nanoseconds(1),
        ));
    }

    #[test]
    fn unenrolled_nodes_are_rejected() {
        let runtime = Runtime::new().expect("runtime should build");
        runtime.block_on(async {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should be monotonic enough")
                .as_nanos();
            let temp_dir =
                std::env::temp_dir().join(format!("nodelite-registry-legacy-test-{unique}"));
            std::fs::create_dir_all(&temp_dir).expect("temp dir should exist");
            let path = temp_dir.join("server.json");
            std::fs::write(&path, "{\"nodes\":[]}").expect("empty registry should be written");

            let registry = NodeRegistry::load(&path)
                .await
                .expect("registry should load");
            let identity = NodeIdentity {
                node_id: "legacy-01".to_string(),
                node_label: "Legacy 01".to_string(),
                hostname: "legacy-01.internal".to_string(),
                os: "Ubuntu".to_string(),
                kernel_version: None,
                cpu_model: None,
                cpu_cores: 2,
                agent_version: "0.1.0".to_string(),
                boot_time: None,
                tags: Vec::new(),
            };

            let error = registry
                .authorize(&identity, "some-token")
                .await
                .expect_err("unenrolled node should be rejected");
            assert_eq!(error.to_string(), "unauthorized");

            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_dir(&temp_dir);
        });
    }

    #[test]
    fn wrong_tokens_use_the_same_auth_error() {
        let runtime = Runtime::new().expect("runtime should build");
        runtime.block_on(async {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should be monotonic enough")
                .as_nanos();
            let temp_dir =
                std::env::temp_dir().join(format!("nodelite-registry-auth-error-test-{unique}"));
            std::fs::create_dir_all(&temp_dir).expect("temp dir should exist");
            let path = temp_dir.join("server.json");
            let file = RegistryFile {
                nodes: vec![legacy_node("osaka-01", "Osaka 01", "secret", None)],
                install_sessions: Vec::new(),
            };
            std::fs::write(&path, serde_json::to_string_pretty(&file).expect("json"))
                .expect("registry should be written");
            let registry = NodeRegistry::load(&path)
                .await
                .expect("registry should load");
            let identity = NodeIdentity {
                node_id: "osaka-01".to_string(),
                node_label: "Osaka 01".to_string(),
                hostname: "osaka-01.internal".to_string(),
                os: "Ubuntu".to_string(),
                kernel_version: None,
                cpu_model: None,
                cpu_cores: 2,
                agent_version: "0.1.0".to_string(),
                boot_time: None,
                tags: Vec::new(),
            };

            let error = registry
                .authorize(&identity, "wrong-secret")
                .await
                .expect_err("wrong token should be rejected");
            assert_eq!(error.to_string(), "unauthorized");

            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_dir(&temp_dir);
        });
    }

    #[cfg(unix)]
    #[test]
    fn issued_registry_file_is_mode_600() {
        use std::os::unix::fs::PermissionsExt;

        let runtime = Runtime::new().expect("runtime should build");
        runtime.block_on(async {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should be monotonic enough")
                .as_nanos();
            let temp_dir =
                std::env::temp_dir().join(format!("nodelite-registry-mode-test-{unique}"));
            std::fs::create_dir_all(&temp_dir).expect("temp dir should exist");
            let config_dir = temp_dir.join("config");
            let path = config_dir.join("server.json");

            issue_node(
                &path,
                IssueNodeRequest {
                    node_id: "hk-01".to_string(),
                    node_label: Some("Hong Kong 01".to_string()),
                    tags: Vec::new(),
                    rotate_token: false,
                },
            )
            .await
            .expect("node should be issued");

            let dir_mode = std::fs::metadata(&config_dir)
                .expect("config dir should exist")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(dir_mode, 0o700);

            let mode = std::fs::metadata(&path)
                .expect("metadata should exist")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);

            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_dir(&config_dir);
            let _ = std::fs::remove_dir(&temp_dir);
        });
    }

    #[test]
    fn github_release_base_url_uses_latest_download_path() {
        let release_url =
            build_github_release_base_url("https://github.com/XiNian-dada/NodeLite.git")
                .expect("release url should build");
        assert_eq!(
            release_url,
            "https://github.com/XiNian-dada/NodeLite/releases/latest/download"
        );
    }

    #[test]
    fn default_release_base_url_points_at_github_latest_download() {
        let release_url =
            default_agent_release_base_url().expect("default release url should build");
        assert_eq!(
            release_url,
            "https://github.com/XiNian-dada/NodeLite/releases/latest/download"
        );
    }

    #[test]
    fn validate_registered_node_rejects_oversized_tags() {
        let mut node = RegisteredNode {
            node_id: "hk-01".to_string(),
            node_label: "Hong Kong 01".to_string(),
            token_hash: "hash".to_string(),
            token_generation: 1,
            token: "secret-token".to_string(),
            tags: vec!["edge".to_string()],
            created_at: Utc::now(),
            token_expires_at: None,
        };
        node.tags = vec!["x".repeat(MAX_NODE_TAG_BYTES + 1)];

        let error = validate_registered_node(&node).expect_err("oversized tag should fail");
        assert!(error.to_string().contains("node.tags[0]"));
    }

    #[tokio::test]
    async fn issue_node_rejects_excessive_tags() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let temp_dir =
            std::env::temp_dir().join(format!("nodelite-registry-tag-limit-test-{unique}"));
        std::fs::create_dir_all(&temp_dir).expect("temp dir");
        let path = temp_dir.join("server.json");

        let error = issue_node(
            &path,
            IssueNodeRequest {
                node_id: "hk-01".to_string(),
                node_label: Some("Hong Kong 01".to_string()),
                tags: (0..1000).map(|index| format!("tag-{index}")).collect(),
                rotate_token: false,
            },
        )
        .await
        .expect_err("too many tags should fail");

        assert!(error.to_string().contains("tags"));
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn concurrent_issue_node_preserves_all_nodes() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let temp_dir =
            std::env::temp_dir().join(format!("nodelite-registry-concurrent-test-{unique}"));
        std::fs::create_dir_all(&temp_dir).expect("temp dir");
        let path = temp_dir.join("server.json");

        // 并发 issue 10 个不同节点,验证 flock + 唯一 tmp 文件名能保证全部落盘。
        let mut handles = Vec::new();
        for i in 0..10 {
            let path = path.clone();
            let handle = tokio::spawn(async move {
                issue_node(
                    &path,
                    IssueNodeRequest {
                        node_id: format!("node-{i:02}"),
                        node_label: Some(format!("Node {i:02}")),
                        tags: Vec::new(),
                        rotate_token: false,
                    },
                )
                .await
                .expect("issue should succeed")
            });
            handles.push(handle);
        }

        let results = futures::future::join_all(handles).await;
        assert_eq!(results.len(), 10, "all tasks should complete");

        let registry = NodeRegistry::load(&path).await.expect("load");
        let node_ids: Vec<_> = registry
            .state
            .read()
            .await
            .entries
            .keys()
            .cloned()
            .collect();
        assert_eq!(
            node_ids.len(),
            10,
            "all 10 nodes should be present in registry"
        );

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&temp_dir);
    }
}
