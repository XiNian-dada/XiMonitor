//! 配置文件解析:Agent 与 Server 启动时读取的 TOML 配置。
//!
//! 设计要点:
//! 1. 暴露的 [`ServerConfig`]/[`AgentConfig`] 是经过校验的"干净"结构;
//!    原始反序列化用的 `RawXxx` 仅在本模块内可见,以便在 `validate()` 中统一兜底。
//! 2. 所有默认值通过常量 `DEFAULT_*` 暴露,既被本文件的 `default_*` 函数引用,
//!    也被外部代码(例如 history 模块)直接使用,确保各组件的默认值一致。
//! 3. 校验逻辑会拒绝看似合理但运行期会出问题的配置,例如:
//!    - 非回环地址上线却没有配置只读认证;
//!    - 安装基址只给了 base_url 而没给对应的校验和。

use std::net::SocketAddr;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use url::{Url, form_urlencoded};

use crate::netutil::uses_insecure_remote_url;

/// 节点超时阈值:超过该时长未收到任何报文即视为离线。
pub const DEFAULT_STALE_AFTER_SECS: u64 = 20;
/// Server 默认 ping 间隔(秒)。
pub const DEFAULT_PING_INTERVAL_SECS: u64 = 10;
/// WebSocket 单帧最大字节数,用于抑制恶意大包。
pub const DEFAULT_MAX_MESSAGE_BYTES: usize = 64 * 1024;
/// 前端默认刷新间隔(秒)。
pub const DEFAULT_REFRESH_INTERVAL_SECS: u64 = 5;
/// Agent 默认上报间隔(秒)。
pub const DEFAULT_REPORT_INTERVAL_SECS: u64 = 5;
/// 历史数据保留时长(小时),默认 14 天。
pub const DEFAULT_HISTORY_RETENTION_HOURS: u64 = 24 * 14;
/// 同一节点两次历史写入的最小间隔(秒),降低 SQLite 压力。
pub const DEFAULT_HISTORY_WRITE_INTERVAL_SECS: u64 = 30;
/// WebSocket 并发连接总数上限。
pub const DEFAULT_WS_MAX_TOTAL_CONNECTIONS: usize = 1024;
/// 单个 IP 允许的 WebSocket 并发连接数。
pub const DEFAULT_WS_MAX_CONNECTIONS_PER_IP: usize = 32;
/// 认证失败统计窗口(秒);超出该窗口的失败记录会被丢弃。
pub const DEFAULT_WS_AUTH_FAIL_WINDOW_SECS: u64 = 300;
/// 在统计窗口内允许的最大失败次数,达到后触发临时封禁。
pub const DEFAULT_WS_AUTH_FAIL_MAX_ATTEMPTS: usize = 12;
/// 触发封禁后的禁用时长(秒)。
pub const DEFAULT_WS_AUTH_BLOCK_SECS: u64 = 900;
/// 单个节点允许携带的最大标签数。
pub const MAX_NODE_TAGS: usize = 64;
/// 单个标签允许的最大字节数。
pub const MAX_NODE_TAG_BYTES: usize = 256;

/// 配置加载或校验过程中产生的错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    message: String,
}

impl ConfigError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ConfigError {}

/// Server 启动需要的全部配置。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerConfig {
    pub listen: SocketAddr,
    pub public_base_url: String,
    pub insecure_allow_http: bool,
    pub readonly_auth: Option<ReadonlyAuthConfig>,
    pub ws: WsConfig,
    pub node_registry_path: PathBuf,
    pub history_db_path: PathBuf,
    pub snapshot_path: PathBuf,
    pub stale_after_secs: u64,
    pub ping_interval_secs: u64,
    pub max_message_bytes: usize,
    pub refresh_interval_secs: u64,
    pub ignored_filesystems: Vec<String>,
    pub agent_release_base_url: Option<String>,
    pub agent_release_sha256_x86_64: Option<String>,
    pub agent_release_sha256_aarch64: Option<String>,
}

/// 前端只读访问所用的基本认证凭证。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReadonlyAuthConfig {
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub enable_2fa: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub totp_secret: Option<String>,
}

/// WebSocket 准入控制参数,用于限流与抗暴力破解。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WsConfig {
    pub max_total_connections: usize,
    pub max_connections_per_ip: usize,
    pub auth_fail_window_secs: u64,
    pub auth_fail_max_attempts: usize,
    pub auth_block_secs: u64,
}

/// Agent 启动需要的全部配置。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentConfig {
    pub node_id: String,
    pub node_label: String,
    pub server: String,
    pub token: String,
    pub report_interval_secs: u64,
    pub hostname_override: Option<String>,
    pub tags: Vec<String>,
}

/// 从 TOML 文本中解析并校验出 `ServerConfig`。
pub fn parse_server_config(input: &str) -> Result<ServerConfig, ConfigError> {
    let raw: RawServerConfigFile =
        toml::from_str(input).map_err(|error| ConfigError::new(error.to_string()))?;
    raw.validate()
}

/// 从 TOML 文本中解析并校验出 `AgentConfig`。
pub fn parse_agent_config(input: &str) -> Result<AgentConfig, ConfigError> {
    let raw: RawAgentConfigFile =
        toml::from_str(input).map_err(|error| ConfigError::new(error.to_string()))?;
    raw.validate()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawServerConfigFile {
    server: RawServerSection,
    #[serde(default)]
    auth: RawAuthSection,
    #[serde(default)]
    ws: RawWsSection,
    #[serde(default)]
    ui: RawUiSection,
    #[serde(default)]
    filters: RawFiltersSection,
    #[serde(default)]
    install: RawInstallSection,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawServerSection {
    listen: String,
    public_base_url: String,
    #[serde(default)]
    insecure_allow_http: bool,
    #[serde(default = "default_node_registry_path")]
    node_registry_path: PathBuf,
    #[serde(default = "default_history_db_path")]
    history_db_path: PathBuf,
    #[serde(default = "default_snapshot_path")]
    snapshot_path: PathBuf,
    #[serde(default = "default_stale_after_secs")]
    stale_after_secs: u64,
    #[serde(default = "default_ping_interval_secs")]
    ping_interval_secs: u64,
    #[serde(default = "default_max_message_bytes")]
    max_message_bytes: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawUiSection {
    #[serde(default = "default_refresh_interval_secs")]
    refresh_interval_secs: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWsSection {
    #[serde(default = "default_ws_max_total_connections")]
    max_total_connections: usize,
    #[serde(default = "default_ws_max_connections_per_ip")]
    max_connections_per_ip: usize,
    #[serde(default = "default_ws_auth_fail_window_secs")]
    auth_fail_window_secs: u64,
    #[serde(default = "default_ws_auth_fail_max_attempts")]
    auth_fail_max_attempts: usize,
    #[serde(default = "default_ws_auth_block_secs")]
    auth_block_secs: u64,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawAuthSection {
    username: Option<String>,
    password: Option<String>,
    #[serde(default)]
    enable_2fa: bool,
    totp_secret: Option<String>,
}

impl Default for RawUiSection {
    fn default() -> Self {
        Self {
            refresh_interval_secs: default_refresh_interval_secs(),
        }
    }
}

impl Default for RawWsSection {
    fn default() -> Self {
        Self {
            max_total_connections: default_ws_max_total_connections(),
            max_connections_per_ip: default_ws_max_connections_per_ip(),
            auth_fail_window_secs: default_ws_auth_fail_window_secs(),
            auth_fail_max_attempts: default_ws_auth_fail_max_attempts(),
            auth_block_secs: default_ws_auth_block_secs(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFiltersSection {
    #[serde(default = "default_ignored_filesystems")]
    ignored_filesystems: Vec<String>,
}

impl Default for RawFiltersSection {
    fn default() -> Self {
        Self {
            ignored_filesystems: default_ignored_filesystems(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawInstallSection {
    agent_release_base_url: Option<String>,
    agent_release_sha256_x86_64: Option<String>,
    agent_release_sha256_aarch64: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAgentConfigFile {
    agent: RawAgentSection,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAgentSection {
    node_id: String,
    node_label: String,
    server: String,
    token: String,
    #[serde(default = "default_report_interval_secs")]
    report_interval_secs: u64,
    hostname_override: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
}

impl RawServerConfigFile {
    /// 集中执行所有跨字段、跨小节的语义校验。
    fn validate(self) -> Result<ServerConfig, ConfigError> {
        let listen = self
            .server
            .listen
            .parse::<SocketAddr>()
            .map_err(|error| ConfigError::new(format!("invalid server.listen: {error}")))?;
        validate_url(
            "server.public_base_url",
            &self.server.public_base_url,
            &["http", "https"],
        )?;
        if uses_insecure_remote_public_base_url(&self.server.public_base_url)
            && !self.server.insecure_allow_http
        {
            return Err(ConfigError::new(
                "server.insecure_allow_http = true is required when server.public_base_url uses remote http://",
            ));
        }
        if let Some(agent_release_base_url) = self.install.agent_release_base_url.as_deref() {
            validate_url(
                "install.agent_release_base_url",
                agent_release_base_url,
                &["http", "https"],
            )?;
        }
        let agent_release_sha256_x86_64 = self
            .install
            .agent_release_sha256_x86_64
            .map(|value| value.trim().to_string());
        let agent_release_sha256_aarch64 = self
            .install
            .agent_release_sha256_aarch64
            .map(|value| value.trim().to_string());
        if let Some(sha256) = agent_release_sha256_x86_64.as_deref() {
            validate_sha256("install.agent_release_sha256_x86_64", sha256)?;
        }
        if let Some(sha256) = agent_release_sha256_aarch64.as_deref() {
            validate_sha256("install.agent_release_sha256_aarch64", sha256)?;
        }
        if self.install.agent_release_base_url.is_some()
            && (agent_release_sha256_x86_64.is_none() || agent_release_sha256_aarch64.is_none())
        {
            return Err(ConfigError::new(
                "install.agent_release_sha256_x86_64 and install.agent_release_sha256_aarch64 are required when install.agent_release_base_url is configured",
            ));
        }
        let enable_2fa = self.auth.enable_2fa;
        let totp_secret = self
            .auth
            .totp_secret
            .map(|value| normalize_totp_secret(&value))
            .filter(|value| !value.is_empty());
        if enable_2fa && self.auth.username.is_none() {
            return Err(ConfigError::new(
                "auth.username and auth.password are required when auth.enable_2fa = true",
            ));
        }
        if enable_2fa && totp_secret.as_deref().is_none_or(str::is_empty) {
            return Err(ConfigError::new(
                "auth.totp_secret is required when auth.enable_2fa = true",
            ));
        }
        if enable_2fa && let Some(secret) = totp_secret.as_deref() {
            validate_totp_secret("auth.totp_secret", secret)?;
        }
        // 没有 HTTPS,2FA 是个剧场:Cookie 与 TOTP code 都会在明文链路上传输,
        // 攻击者一次嗅探即可越过二次验证。所以 enable_2fa = true 时,
        // 必须使用 https:// 的 public_base_url —— 即便 listen 是回环地址,
        // 也得借此提醒部署人员前面必须有 TLS 终结。
        if enable_2fa && !self.server.public_base_url.starts_with("https://") {
            return Err(ConfigError::new(
                "server.public_base_url must use https:// when auth.enable_2fa = true",
            ));
        }

        // 用户名与密码必须成对出现:任意一个单独存在都视为配置错误。
        let readonly_auth = match (
            self.auth.username.map(|value| value.trim().to_string()),
            self.auth.password.map(|value| value.trim().to_string()),
        ) {
            (Some(username), Some(password)) => {
                validate_non_empty("auth.username", &username)?;
                validate_non_empty("auth.password", &password)?;
                Some(ReadonlyAuthConfig {
                    username,
                    password,
                    enable_2fa,
                    totp_secret,
                })
            }
            (None, None) => None,
            (Some(_), None) => {
                return Err(ConfigError::new(
                    "auth.password must be set when auth.username is configured",
                ));
            }
            (None, Some(_)) => {
                return Err(ConfigError::new(
                    "auth.username must be set when auth.password is configured",
                ));
            }
        };

        if self.server.stale_after_secs < 5 {
            return Err(ConfigError::new(
                "server.stale_after_secs must be at least 5 seconds",
            ));
        }
        if self.server.ping_interval_secs < 1 {
            return Err(ConfigError::new(
                "server.ping_interval_secs must be at least 1 second",
            ));
        }
        if self.server.max_message_bytes < 1024 {
            return Err(ConfigError::new(
                "server.max_message_bytes must be at least 1024 bytes",
            ));
        }
        if self.ws.max_total_connections < 1 {
            return Err(ConfigError::new(
                "ws.max_total_connections must be at least 1",
            ));
        }
        if self.ws.max_connections_per_ip < 1 {
            return Err(ConfigError::new(
                "ws.max_connections_per_ip must be at least 1",
            ));
        }
        if self.ws.max_connections_per_ip > self.ws.max_total_connections {
            return Err(ConfigError::new(
                "ws.max_connections_per_ip must be <= ws.max_total_connections",
            ));
        }
        if self.ws.auth_fail_window_secs < 1 {
            return Err(ConfigError::new(
                "ws.auth_fail_window_secs must be at least 1 second",
            ));
        }
        if self.ws.auth_fail_max_attempts < 1 {
            return Err(ConfigError::new(
                "ws.auth_fail_max_attempts must be at least 1",
            ));
        }
        if self.ws.auth_block_secs < 1 {
            return Err(ConfigError::new(
                "ws.auth_block_secs must be at least 1 second",
            ));
        }
        if self.ui.refresh_interval_secs < 1 {
            return Err(ConfigError::new(
                "ui.refresh_interval_secs must be at least 1 second",
            ));
        }
        // 监听非回环地址时必须配置只读认证,防止公网暴露。
        if readonly_auth.is_none() && !listen.ip().is_loopback() {
            return Err(ConfigError::new(
                "auth.username and auth.password are required when server.listen is not loopback",
            ));
        }

        Ok(ServerConfig {
            listen,
            public_base_url: self.server.public_base_url,
            insecure_allow_http: self.server.insecure_allow_http,
            readonly_auth,
            ws: WsConfig {
                max_total_connections: self.ws.max_total_connections,
                max_connections_per_ip: self.ws.max_connections_per_ip,
                auth_fail_window_secs: self.ws.auth_fail_window_secs,
                auth_fail_max_attempts: self.ws.auth_fail_max_attempts,
                auth_block_secs: self.ws.auth_block_secs,
            },
            node_registry_path: self.server.node_registry_path,
            history_db_path: self.server.history_db_path,
            snapshot_path: self.server.snapshot_path,
            stale_after_secs: self.server.stale_after_secs,
            ping_interval_secs: self.server.ping_interval_secs,
            max_message_bytes: self.server.max_message_bytes,
            refresh_interval_secs: self.ui.refresh_interval_secs,
            ignored_filesystems: normalize_string_list(self.filters.ignored_filesystems),
            agent_release_base_url: self.install.agent_release_base_url,
            agent_release_sha256_x86_64,
            agent_release_sha256_aarch64,
        })
    }
}

fn uses_insecure_remote_public_base_url(public_base_url: &str) -> bool {
    uses_insecure_remote_url(public_base_url, "http")
}

impl RawAgentConfigFile {
    /// 校验 Agent 配置,并把 `agent.tags` 等字段规范化(去空白、去重、排序)。
    fn validate(self) -> Result<AgentConfig, ConfigError> {
        validate_identifier("agent.node_id", &self.agent.node_id)?;
        validate_non_empty("agent.node_label", &self.agent.node_label)?;
        validate_url("agent.server", &self.agent.server, &["ws", "wss"])?;
        validate_non_empty("agent.token", &self.agent.token)?;

        if self.agent.report_interval_secs < 1 {
            return Err(ConfigError::new(
                "agent.report_interval_secs must be at least 1 second",
            ));
        }

        if let Some(hostname) = &self.agent.hostname_override {
            validate_non_empty("agent.hostname_override", hostname)?;
        }

        Ok(AgentConfig {
            node_id: self.agent.node_id.trim().to_string(),
            node_label: self.agent.node_label.trim().to_string(),
            server: self.agent.server,
            token: self.agent.token,
            report_interval_secs: self.agent.report_interval_secs,
            hostname_override: self
                .agent
                .hostname_override
                .map(|value| value.trim().to_string()),
            tags: normalize_tags("agent.tags", self.agent.tags)?,
        })
    }
}

fn validate_non_empty(field: &str, value: &str) -> Result<(), ConfigError> {
    if value.trim().is_empty() {
        return Err(ConfigError::new(format!("{field} must not be empty")));
    }
    Ok(())
}

/// 校验"标识符"风格的字段:非空、长度可控、仅含 ASCII 字母数字与 `-_.`。
fn validate_identifier(field: &str, value: &str) -> Result<(), ConfigError> {
    validate_non_empty(field, value)?;
    if value.len() > 128 {
        return Err(ConfigError::new(format!(
            "{field} must be <= 128 characters"
        )));
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err(ConfigError::new(format!(
            "{field} must use only ASCII letters, numbers, '-', '_' or '.'"
        )));
    }
    Ok(())
}

/// 校验 URL 字段:能被解析,并且采用了允许的协议方案。
fn validate_url(field: &str, value: &str, schemes: &[&str]) -> Result<(), ConfigError> {
    let parsed =
        Url::parse(value).map_err(|error| ConfigError::new(format!("invalid {field}: {error}")))?;
    if !schemes.iter().any(|scheme| *scheme == parsed.scheme()) {
        return Err(ConfigError::new(format!(
            "{field} must use one of these schemes: {}",
            schemes.join(", ")
        )));
    }
    Ok(())
}

/// 规范化字符串列表:trim 后去空、排序并去重,确保比较与持久化稳定。
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

fn normalize_tags(field: &str, values: Vec<String>) -> Result<Vec<String>, ConfigError> {
    let values = normalize_string_list(values);
    validate_tag_list(field, &values)?;
    Ok(values)
}

fn validate_tag_list(field: &str, values: &[String]) -> Result<(), ConfigError> {
    if values.len() > MAX_NODE_TAGS {
        return Err(ConfigError::new(format!(
            "{field} must contain at most {MAX_NODE_TAGS} tags"
        )));
    }
    for (index, value) in values.iter().enumerate() {
        if value.len() > MAX_NODE_TAG_BYTES {
            return Err(ConfigError::new(format!(
                "{field}[{index}] must be <= {MAX_NODE_TAG_BYTES} bytes"
            )));
        }
    }
    Ok(())
}

/// 校验 SHA-256 摘要:长度必须是 64 个十六进制字符。
fn validate_sha256(field: &str, value: &str) -> Result<(), ConfigError> {
    validate_non_empty(field, value)?;
    if value.len() != 64 || !value.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(ConfigError::new(format!(
            "{field} must be a 64-character hexadecimal SHA-256 digest"
        )));
    }
    Ok(())
}

/// 兼容常见的 TOTP secret 输入形式:
/// - 纯 RFC4648 Base32
/// - 带空格/连字符/小写的手工录入
/// - 直接粘贴 `otpauth://...?...secret=...`
/// - 只粘贴 `secret=...` 这样的查询片段
pub fn normalize_totp_secret(value: &str) -> String {
    let candidate =
        extract_totp_secret_candidate(value).unwrap_or_else(|| value.trim().to_string());
    candidate
        .chars()
        .filter(|ch| !ch.is_ascii_whitespace() && *ch != '-')
        .collect::<String>()
        .to_ascii_uppercase()
}

fn extract_totp_secret_candidate(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if let Ok(url) = Url::parse(trimmed)
        && url.scheme().eq_ignore_ascii_case("otpauth")
    {
        return extract_secret_query_value(url.query().unwrap_or_default());
    }

    if trimmed.contains("secret=") {
        return extract_secret_query_value(trimmed.trim_start_matches('?'));
    }

    None
}

fn extract_secret_query_value(query: &str) -> Option<String> {
    form_urlencoded::parse(query.as_bytes()).find_map(|(key, value)| {
        key.eq_ignore_ascii_case("secret")
            .then(|| value.into_owned())
            .filter(|value| !value.trim().is_empty())
    })
}

fn decode_totp_secret_bytes(value: &str) -> Option<Vec<u8>> {
    let normalized = normalize_totp_secret(value);
    base32::decode(base32::Alphabet::Rfc4648 { padding: false }, &normalized)
        .or_else(|| base32::decode(base32::Alphabet::Rfc4648 { padding: true }, &normalized))
}

fn validate_totp_secret(field: &str, value: &str) -> Result<(), ConfigError> {
    validate_non_empty(field, value)?;
    let decoded = decode_totp_secret_bytes(value);
    let Some(decoded) = decoded else {
        return Err(ConfigError::new(format!(
            "{field} must be a valid RFC4648 base32 TOTP secret"
        )));
    };
    if decoded.len() < 10 {
        return Err(ConfigError::new(format!(
            "{field} must decode to at least 10 bytes"
        )));
    }
    Ok(())
}

fn default_history_db_path() -> PathBuf {
    PathBuf::from("./data/history.sqlite3")
}

fn default_node_registry_path() -> PathBuf {
    PathBuf::from("./config/server.json")
}

fn default_snapshot_path() -> PathBuf {
    PathBuf::from("./data/snapshot.json")
}

fn default_stale_after_secs() -> u64 {
    DEFAULT_STALE_AFTER_SECS
}

fn default_ping_interval_secs() -> u64 {
    DEFAULT_PING_INTERVAL_SECS
}

fn default_max_message_bytes() -> usize {
    DEFAULT_MAX_MESSAGE_BYTES
}

fn default_refresh_interval_secs() -> u64 {
    DEFAULT_REFRESH_INTERVAL_SECS
}

fn default_ws_max_total_connections() -> usize {
    DEFAULT_WS_MAX_TOTAL_CONNECTIONS
}

fn default_ws_max_connections_per_ip() -> usize {
    DEFAULT_WS_MAX_CONNECTIONS_PER_IP
}

fn default_ws_auth_fail_window_secs() -> u64 {
    DEFAULT_WS_AUTH_FAIL_WINDOW_SECS
}

fn default_ws_auth_fail_max_attempts() -> usize {
    DEFAULT_WS_AUTH_FAIL_MAX_ATTEMPTS
}

fn default_ws_auth_block_secs() -> u64 {
    DEFAULT_WS_AUTH_BLOCK_SECS
}

fn default_report_interval_secs() -> u64 {
    DEFAULT_REPORT_INTERVAL_SECS
}

/// 默认忽略的文件系统类型;这些通常是虚拟挂载,不应在磁盘视图中出现。
fn default_ignored_filesystems() -> Vec<String> {
    vec![
        "devtmpfs".to_string(),
        "overlay".to_string(),
        "tmpfs".to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        DEFAULT_MAX_MESSAGE_BYTES, DEFAULT_WS_AUTH_BLOCK_SECS, DEFAULT_WS_AUTH_FAIL_MAX_ATTEMPTS,
        DEFAULT_WS_AUTH_FAIL_WINDOW_SECS, DEFAULT_WS_MAX_CONNECTIONS_PER_IP,
        DEFAULT_WS_MAX_TOTAL_CONNECTIONS, MAX_NODE_TAG_BYTES, parse_agent_config,
        parse_server_config,
    };

    #[test]
    fn parses_server_config_with_defaults() {
        let config = parse_server_config(
            r#"
            [server]
            listen = "127.0.0.1:8080"
            public_base_url = "http://127.0.0.1:8080"
            "#,
        )
        .expect("server config should parse");

        assert_eq!(config.listen.to_string(), "127.0.0.1:8080");
        assert!(!config.insecure_allow_http);
        assert_eq!(config.readonly_auth, None);
        assert_eq!(config.max_message_bytes, DEFAULT_MAX_MESSAGE_BYTES);
        assert_eq!(
            config.ws.max_total_connections,
            DEFAULT_WS_MAX_TOTAL_CONNECTIONS
        );
        assert_eq!(
            config.ws.max_connections_per_ip,
            DEFAULT_WS_MAX_CONNECTIONS_PER_IP
        );
        assert_eq!(
            config.ws.auth_fail_window_secs,
            DEFAULT_WS_AUTH_FAIL_WINDOW_SECS
        );
        assert_eq!(
            config.ws.auth_fail_max_attempts,
            DEFAULT_WS_AUTH_FAIL_MAX_ATTEMPTS
        );
        assert_eq!(config.ws.auth_block_secs, DEFAULT_WS_AUTH_BLOCK_SECS);
        assert_eq!(
            config.node_registry_path,
            PathBuf::from("./config/server.json")
        );
        assert_eq!(
            config.ignored_filesystems,
            vec!["devtmpfs", "overlay", "tmpfs"]
        );
    }

    #[test]
    fn rejects_invalid_server_listen_address() {
        let error = parse_server_config(
            r#"
            [server]
            listen = "oops"
            public_base_url = "http://127.0.0.1:8080"
            "#,
        )
        .expect_err("invalid config should fail");

        assert!(error.to_string().contains("server.listen"));
    }

    #[test]
    fn rejects_invalid_agent_server_scheme() {
        let error = parse_agent_config(
            r#"
            [agent]
            node_id = "hk-01"
            node_label = "Hong Kong 01"
            server = "http://127.0.0.1:8080/ws"
            token = "token"
            "#,
        )
        .expect_err("invalid agent config should fail");

        assert!(error.to_string().contains("agent.server"));
    }

    #[test]
    fn parses_agent_config() {
        let config = parse_agent_config(
            r#"
            [agent]
            node_id = "hk-01"
            node_label = "Hong Kong 01"
            server = "ws://127.0.0.1:8080/ws"
            token = "token"
            report_interval_secs = 7
            hostname_override = "hk-01.internal"
            tags = [" edge ", "apac"]
            "#,
        )
        .expect("agent config should parse");

        assert_eq!(config.node_id, "hk-01");
        assert_eq!(config.report_interval_secs, 7);
        assert_eq!(config.tags, vec!["apac", "edge"]);
    }

    #[test]
    fn rejects_agent_config_with_too_many_tags() {
        let tags = (0..1000)
            .map(|index| format!("\"tag-{index}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let input = format!(
            r#"
            [agent]
            node_id = "hk-01"
            node_label = "Hong Kong 01"
            server = "ws://127.0.0.1:8080/ws"
            token = "token"
            tags = [{tags}]
            "#
        );

        let error = parse_agent_config(&input).expect_err("too many tags should fail");
        assert!(error.to_string().contains("agent.tags"));
    }

    #[test]
    fn rejects_agent_config_with_oversized_tag() {
        let oversized = "x".repeat(MAX_NODE_TAG_BYTES + 1);
        let input = format!(
            r#"
            [agent]
            node_id = "hk-01"
            node_label = "Hong Kong 01"
            server = "ws://127.0.0.1:8080/ws"
            token = "token"
            tags = ["{oversized}"]
            "#
        );

        let error = parse_agent_config(&input).expect_err("oversized tag should fail");
        assert!(error.to_string().contains("agent.tags[0]"));
    }

    #[test]
    fn parses_server_config_with_install() {
        let config = parse_server_config(
            r#"
            [server]
            listen = "127.0.0.1:8080"
            public_base_url = "https://monitor.example.com"
            node_registry_path = "/etc/nodelite/server.json"

            [auth]
            username = "viewer"
            password = "secret"

            [install]
            agent_release_base_url = "https://downloads.example.com/nodelite/releases/latest/download"
            agent_release_sha256_x86_64 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
            agent_release_sha256_aarch64 = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
            "#,
        )
        .expect("server config should parse");

        assert_eq!(
            config
                .readonly_auth
                .as_ref()
                .map(|auth| auth.username.as_str()),
            Some("viewer")
        );
        assert_eq!(
            config.node_registry_path,
            PathBuf::from("/etc/nodelite/server.json")
        );
        assert_eq!(
            config.agent_release_base_url.as_deref(),
            Some("https://downloads.example.com/nodelite/releases/latest/download")
        );
        assert_eq!(
            config.agent_release_sha256_x86_64.as_deref(),
            Some("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
        );
        assert_eq!(
            config.agent_release_sha256_aarch64.as_deref(),
            Some("abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789")
        );
    }

    #[test]
    fn parses_server_config_with_totp_2fa() {
        let config = parse_server_config(
            r#"
            [server]
            listen = "127.0.0.1:8080"
            public_base_url = "https://monitor.example.com"

            [auth]
            username = "viewer"
            password = "secret123"
            enable_2fa = true
            totp_secret = "JBSWY3DPEHPK3PXP"
            "#,
        )
        .expect("2fa config should parse");

        let auth = config.readonly_auth.expect("auth should be configured");
        assert!(auth.enable_2fa);
        assert_eq!(auth.totp_secret.as_deref(), Some("JBSWY3DPEHPK3PXP"));
    }

    #[test]
    fn parses_server_config_with_otpauth_totp_secret() {
        let config = parse_server_config(
            r#"
            [server]
            listen = "127.0.0.1:8080"
            public_base_url = "https://monitor.example.com"

            [auth]
            username = "viewer"
            password = "secret123"
            enable_2fa = true
            totp_secret = "otpauth://totp/NodeLite:viewer%40example.com?secret=jbsw y3dp-ehpk3pxp&issuer=NodeLite"
            "#,
        )
        .expect("otpauth uri should parse");

        let auth = config.readonly_auth.expect("auth should be configured");
        assert_eq!(auth.totp_secret.as_deref(), Some("JBSWY3DPEHPK3PXP"));
    }

    #[test]
    fn parses_server_config_with_secret_query_totp_secret() {
        let config = parse_server_config(
            r#"
            [server]
            listen = "127.0.0.1:8080"
            public_base_url = "https://monitor.example.com"

            [auth]
            username = "viewer"
            password = "secret123"
            enable_2fa = true
            totp_secret = "secret=jbswy3dp ehpk3pxp&issuer=NodeLite"
            "#,
        )
        .expect("secret query string should parse");

        let auth = config.readonly_auth.expect("auth should be configured");
        assert_eq!(auth.totp_secret.as_deref(), Some("JBSWY3DPEHPK3PXP"));
    }

    #[test]
    fn ignores_empty_totp_secret_when_2fa_is_disabled() {
        let config = parse_server_config(
            r#"
            [server]
            listen = "127.0.0.1:8080"
            public_base_url = "https://monitor.example.com"

            [auth]
            username = "viewer"
            password = "secret123"
            enable_2fa = false
            totp_secret = ""
            "#,
        )
        .expect("disabled 2fa should ignore empty totp secret");

        let auth = config.readonly_auth.expect("auth should be configured");
        assert!(!auth.enable_2fa);
        assert_eq!(auth.totp_secret, None);
    }

    #[test]
    fn ignores_invalid_totp_secret_when_2fa_is_disabled() {
        let config = parse_server_config(
            r#"
            [server]
            listen = "127.0.0.1:8080"
            public_base_url = "https://monitor.example.com"

            [auth]
            username = "viewer"
            password = "secret123"
            enable_2fa = false
            totp_secret = "not-a-base32-secret"
            "#,
        )
        .expect("disabled 2fa should ignore invalid totp secret");

        let auth = config.readonly_auth.expect("auth should be configured");
        assert!(!auth.enable_2fa);
        assert_eq!(auth.totp_secret.as_deref(), Some("NOTABASE32SECRET"));
    }

    #[test]
    fn rejects_2fa_without_totp_secret() {
        let error = parse_server_config(
            r#"
            [server]
            listen = "127.0.0.1:8080"
            public_base_url = "https://monitor.example.com"

            [auth]
            username = "viewer"
            password = "secret123"
            enable_2fa = true
            "#,
        )
        .expect_err("2fa without totp secret should fail");

        assert!(error.to_string().contains("auth.totp_secret"));
    }

    #[test]
    fn rejects_2fa_with_plaintext_public_base_url() {
        let error = parse_server_config(
            r#"
            [server]
            listen = "127.0.0.1:8080"
            public_base_url = "http://monitor.example.com"
            insecure_allow_http = true

            [auth]
            username = "viewer"
            password = "secret123"
            enable_2fa = true
            totp_secret = "JBSWY3DPEHPK3PXP"
            "#,
        )
        .expect_err("2fa over plaintext http should be rejected");

        assert!(error.to_string().contains("public_base_url"));
        assert!(error.to_string().contains("https"));
    }

    #[test]
    fn rejects_public_listener_without_auth() {
        let error = parse_server_config(
            r#"
            [server]
            listen = "0.0.0.0:8080"
            public_base_url = "https://monitor.example.com"
            "#,
        )
        .expect_err("public listener without auth should fail");

        assert!(error.to_string().contains("auth.username"));
    }

    #[test]
    fn rejects_install_release_base_without_checksums() {
        let error = parse_server_config(
            r#"
            [server]
            listen = "127.0.0.1:8080"
            public_base_url = "https://monitor.example.com"

            [install]
            agent_release_base_url = "https://downloads.example.com/nodelite/releases/latest/download"
            "#,
        )
        .expect_err("release base without checksums should fail");

        assert!(error.to_string().contains("agent_release_sha256_x86_64"));
    }

    #[test]
    fn rejects_invalid_ws_limits() {
        let error = parse_server_config(
            r#"
            [server]
            listen = "127.0.0.1:8080"
            public_base_url = "http://127.0.0.1:8080"

            [ws]
            max_total_connections = 4
            max_connections_per_ip = 8
            "#,
        )
        .expect_err("invalid ws limits should fail");

        assert!(error.to_string().contains("ws.max_connections_per_ip"));
    }

    #[test]
    fn rejects_remote_http_without_explicit_opt_in() {
        let error = parse_server_config(
            r#"
            [server]
            listen = "0.0.0.0:8080"
            public_base_url = "http://monitor.example.com"

            [auth]
            username = "viewer"
            password = "secret"
            "#,
        )
        .expect_err("remote http without opt-in should fail");

        assert!(error.to_string().contains("server.insecure_allow_http"));
    }

    #[test]
    fn allows_remote_http_with_explicit_opt_in() {
        let config = parse_server_config(
            r#"
            [server]
            listen = "0.0.0.0:8080"
            public_base_url = "http://monitor.example.com"
            insecure_allow_http = true

            [auth]
            username = "viewer"
            password = "secret"
            "#,
        )
        .expect("remote http should parse with explicit opt-in");

        assert!(config.insecure_allow_http);
    }
}
