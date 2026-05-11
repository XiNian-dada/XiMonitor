// 节点注册表:服务端唯一的"哪些节点被允许上报"的事实来源。
//
// 注册表是一份 JSON 文件,内容由 `RegistryFile` 结构序列化得到。
// 服务端进程与运维 CLI(`server install-agent` 等)都会读写这份文件,
// 因此对每次写入都采用 flock + 原子替换的策略。
//
// 字段语义:
// - `RegisteredNode`:被认证的 Agent 凭证(node_id + token)。
// - `InstallSession`:一次性的"安装令牌",拥有它可以拉取 Agent 配置。

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use getrandom::fill as fill_random;
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::sync::RwLock;
use url::Url;
use ximonitor_proto::NodeIdentity;

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

/// 已登记节点的持久化条目。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RegisteredNode {
    pub node_id: String,
    pub node_label: String,
    pub token: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
}

/// 安装会话:由 CLI 颁发的一次性令牌,Agent 用它拉取自己的配置。
///
/// `expires_at` 为绝对过期时间;每次写入注册表时会顺带清理已过期会话。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstallSession {
    pub token: String,
    pub node_id: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
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

    /// 校验 Agent 提交的 Hello 信息与 token,通过后返回"覆盖了注册表里权威字段"的身份。
    pub async fn authorize(&self, identity: &NodeIdentity, token: &str) -> Result<NodeIdentity> {
        validate_runtime_identity(identity)?;
        validate_non_empty("hello.token", token)?;
        let state = self.state.read().await;
        authorize_identity(&state.entries, identity, token)
    }

    /// 判断当前 token 是否仍是该节点的有效凭证。
    /// 用于在长连接运行中检测"管理员是否已轮换该节点 token"。
    pub async fn is_token_current(&self, node_id: &str, token: &str) -> bool {
        let state = self.state.read().await;
        is_token_current(&state.entries, node_id, token)
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

    /// 一次性消费安装令牌:成功时返回对应的 `RegisteredNode`,并把令牌从注册表移除。
    pub async fn consume_install_token(&self, token: &str) -> Result<Option<RegisteredNode>> {
        validate_non_empty("install token", token)?;

        let token = token.to_string();
        let (node, file) = mutate_registry_file(self.path.as_path(), move |file| {
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
            Ok((node, true))
        })
        .await?;
        self.replace_state_from_file(file).await?;
        Ok(node)
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
                file.nodes[index].tags = normalize_string_list(request.tags.clone());
            }
            if request.rotate_token {
                file.nodes[index].token = generate_token()?;
                rotated_token = true;
            }

            validate_registered_node(&file.nodes[index])?;
            let node = file.nodes[index].clone();
            let install_session = mint_install_session(file, &node.node_id, now)?;
            return Ok((
                IssueNodeResult {
                    node,
                    created: false,
                    rotated_token,
                    install_token: install_session.token,
                    install_token_expires_at: install_session.expires_at,
                },
                true,
            ));
        }

        let node = RegisteredNode {
            node_id: request.node_id.trim().to_string(),
            node_label: request
                .node_label
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(request.node_id.as_str())
                .to_string(),
            token: generate_token()?,
            tags: normalize_string_list(request.tags.clone()),
            created_at: now,
        };
        validate_registered_node(&node)?;

        file.nodes.push(node.clone());
        file.nodes
            .sort_by(|left, right| left.node_id.cmp(&right.node_id));
        let install_session = mint_install_session(file, &node.node_id, now)?;
        Ok((
            IssueNodeResult {
                node,
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
            "  XIMONITOR_AGENT_INSTALL_TOKEN={} sh -s -- \\",
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
        "  XIMONITOR_AGENT_MODE=upgrade sh -s -- \\".to_string(),
        "  --mode upgrade \\".to_string(),
        format!("  --base-url {}", shell_quote(agent_release_base_url)),
    ];

    Ok(lines.join("\n"))
}

/// 渲染单个节点的 `agent.toml` 文本,作为引导接口的响应体。
pub fn render_agent_config(public_base_url: &str, node: &RegisteredNode) -> Result<String> {
    let server_url = build_agent_server_url(public_base_url)?;
    let mut content = String::new();
    content.push_str("[agent]\n");
    content.push_str(&format!("node_id = \"{}\"\n", toml_escape(&node.node_id)));
    content.push_str(&format!(
        "node_label = \"{}\"\n",
        toml_escape(&node.node_label)
    ));
    content.push_str(&format!("server = \"{}\"\n", toml_escape(&server_url)));
    content.push_str(&format!("token = \"{}\"\n", toml_escape(&node.token)));
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
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let payload =
        serde_json::to_string_pretty(file).context("failed to serialize node registry")?;
    let tmp_path = temporary_registry_path(path);
    write_registry_payload(&tmp_path, &payload)
        .with_context(|| format!("failed to write {}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, path)
        .with_context(|| format!("failed to replace {}", path.display()))?;
    harden_registry_permissions(path)
        .with_context(|| format!("failed to set permissions on {}", path.display()))?;
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

fn temporary_registry_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("server.json");
    path.with_file_name(format!("{file_name}.tmp"))
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
    Ok(())
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
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
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
    validate_non_empty("node.token", &node.token)?;
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
) -> Result<InstallSession> {
    file.install_sessions
        .retain(|session| session.node_id != node_id);
    let session = InstallSession {
        token: generate_token()?,
        node_id: node_id.to_string(),
        created_at: now,
        expires_at: now + ChronoDuration::minutes(INSTALL_TOKEN_TTL_MINUTES),
    };
    file.install_sessions.push(session.clone());
    Ok(session)
}

fn authorize_identity(
    entries: &HashMap<String, RegisteredNode>,
    identity: &NodeIdentity,
    token: &str,
) -> Result<NodeIdentity> {
    if let Some(entry) = entries.get(identity.node_id.as_str()) {
        if !constant_time_eq(token, &entry.token) {
            bail!("invalid token for enrolled node {}", entry.node_id);
        }

        let mut identity = identity.clone();
        identity.node_id = entry.node_id.clone();
        identity.node_label = entry.node_label.clone();
        identity.tags = entry.tags.clone();
        return Ok(identity);
    }

    bail!("node {} is not enrolled", identity.node_id);
}

fn is_token_current(entries: &HashMap<String, RegisteredNode>, node_id: &str, token: &str) -> bool {
    if let Some(entry) = entries.get(node_id) {
        return constant_time_eq(token, &entry.token);
    }

    false
}

/// 常量时间字符串比较,用于 token 校验,避免基于响应耗时的旁路攻击。
fn constant_time_eq(left: &str, right: &str) -> bool {
    // 让 token 比较的耗时与"在哪个字节最早出现差异"无关。
    let left = left.as_bytes();
    let right = right.as_bytes();
    let mut diff = left.len() ^ right.len();
    let max_len = left.len().max(right.len());

    for index in 0..max_len {
        let left_byte = usize::from(*left.get(index).unwrap_or(&0));
        let right_byte = usize::from(*right.get(index).unwrap_or(&0));
        diff |= left_byte ^ right_byte;
    }

    diff == 0
}

struct RegistryFileLock {
    file: File,
    lock_path: PathBuf,
}

impl Drop for RegistryFileLock {
    fn drop(&mut self) {
        unlock_file(&self.file);
        let _ = harden_registry_permissions(&self.lock_path);
    }
}

fn validate_runtime_identity(identity: &NodeIdentity) -> Result<()> {
    validate_identifier("identity.node_id", &identity.node_id)?;
    validate_non_empty("identity.node_label", &identity.node_label)?;
    validate_non_empty("identity.agent_version", &identity.agent_version)?;
    validate_non_empty("identity.hostname", &identity.hostname)?;
    validate_non_empty("identity.os", &identity.os)?;
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

/// 生成 256-bit 的随机 token 并以十六进制字符串形式返回。
fn generate_token() -> Result<String> {
    let mut bytes = [0_u8; 32];
    fill_random(&mut bytes).context("failed to gather secure random bytes")?;
    Ok(hex_encode(&bytes))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
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

    use chrono::Utc;
    use tokio::runtime::Runtime;

    use super::{
        IssueNodeRequest, NodeRegistry, RegisteredNode, RegistryFile, build_agent_server_url,
        build_github_release_base_url, default_agent_release_base_url, issue_node,
        render_install_command,
    };
    use ximonitor_proto::NodeIdentity;

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
                std::env::temp_dir().join(format!("ximonitor-registry-auth-test-{unique}"));
            std::fs::create_dir_all(&temp_dir).expect("temp dir should exist");
            let path = temp_dir.join("server.json");
            let file = RegistryFile {
                nodes: vec![RegisteredNode {
                    node_id: "osaka-01".to_string(),
                    node_label: "Osaka 01".to_string(),
                    token: "secret".to_string(),
                    tags: vec!["edge".to_string()],
                    created_at: Utc::now(),
                }],
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
            assert_eq!(authorized.node_label, "Osaka 01");
            assert_eq!(authorized.tags, vec!["edge"]);

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
            let temp_dir = std::env::temp_dir().join(format!("ximonitor-registry-test-{unique}"));
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
            assert_eq!(parsed.install_sessions[0].token, issued.install_token);

            let command = render_install_command(
                "https://monitor.example.com",
                &issued.install_token,
                "https://github.com/XiNian-dada/XiMonitor/releases/latest/download",
            )
            .expect("install command should render");
            assert!(command.contains("--bootstrap-url"));
            assert!(command.contains("/install-agent.sh"));
            assert!(command.contains("XIMONITOR_AGENT_INSTALL_TOKEN="));
            assert!(command.contains(&issued.install_token));
            assert!(!command.contains(&issued.node.token));

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
                std::env::temp_dir().join(format!("ximonitor-registry-reload-test-{unique}"));
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
            let old_token = issued.node.token.clone();
            let registry = NodeRegistry::load(&path)
                .await
                .expect("registry should load");
            assert!(registry.is_token_current("hk-01", &old_token).await);

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
            assert!(!registry.is_token_current("hk-01", &old_token).await);
            assert!(
                registry
                    .is_token_current("hk-01", &rotated.node.token)
                    .await
            );

            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_dir(&temp_dir);
        });
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
                std::env::temp_dir().join(format!("ximonitor-install-token-test-{unique}"));
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
            assert_eq!(consumed.node_id, issued.node.node_id);
            assert_eq!(consumed.token, issued.node.token);
            assert!(
                registry
                    .consume_install_token(&issued.install_token)
                    .await
                    .expect("second install token lookup should succeed")
                    .is_none()
            );

            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_dir(&temp_dir);
        });
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
                std::env::temp_dir().join(format!("ximonitor-registry-legacy-test-{unique}"));
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
            assert!(error.to_string().contains("not enrolled"));

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
                std::env::temp_dir().join(format!("ximonitor-registry-mode-test-{unique}"));
            std::fs::create_dir_all(&temp_dir).expect("temp dir should exist");
            let path = temp_dir.join("server.json");

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

            let mode = std::fs::metadata(&path)
                .expect("metadata should exist")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);

            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_dir(&temp_dir);
        });
    }

    #[test]
    fn github_release_base_url_uses_latest_download_path() {
        let release_url =
            build_github_release_base_url("https://github.com/XiNian-dada/XiMonitor.git")
                .expect("release url should build");
        assert_eq!(
            release_url,
            "https://github.com/XiNian-dada/XiMonitor/releases/latest/download"
        );
    }

    #[test]
    fn default_release_base_url_points_at_github_latest_download() {
        let release_url =
            default_agent_release_base_url().expect("default release url should build");
        assert_eq!(
            release_url,
            "https://github.com/XiNian-dada/XiMonitor/releases/latest/download"
        );
    }
}
