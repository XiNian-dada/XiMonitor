use std::net::SocketAddr;
use std::path::PathBuf;

use serde::Deserialize;

use super::defaults::{
    default_audit_db_path, default_audit_enabled, default_audit_log_failed_auth,
    default_audit_log_rate_limit, default_audit_log_successful_auth,
    default_audit_log_token_events, default_audit_retention_days, default_connect_timeout_secs,
    default_hello_timeout_secs, default_history_db_path, default_ignored_filesystems,
    default_insecure_transport_warn_interval_secs, default_max_incoming_message_bytes,
    default_max_message_bytes, default_max_outstanding_pings, default_max_sanitized_disks,
    default_max_sanitized_string_bytes, default_metric_anomaly_session_limit,
    default_node_registry_path, default_ping_interval_secs, default_refresh_interval_secs,
    default_report_interval_secs, default_snapshot_path, default_sqlite_busy_timeout_secs,
    default_stale_after_secs, default_trusted_proxies, default_ws_auth_block_secs,
    default_ws_auth_fail_max_attempts, default_ws_auth_fail_window_secs,
    default_ws_max_connections_per_ip, default_ws_max_total_connections,
};
use super::helpers::{
    normalize_tags, normalize_totp_secret, parse_trusted_proxies,
    uses_insecure_remote_public_base_url, validate_sha256, validate_totp_secret, validate_url,
};
use super::{AgentConfig, AuditConfig, ConfigError, ReadonlyAuthConfig, ServerConfig, WsConfig};
use crate::validation::{
    ValidationError, normalize_string_list, validate_identifier, validate_non_empty,
};

impl From<ValidationError> for ConfigError {
    fn from(error: ValidationError) -> Self {
        Self::new(error.to_string())
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawServerConfigFile {
    server: RawServerSection,
    #[serde(default)]
    auth: RawAuthSection,
    #[serde(default)]
    ws: RawWsSection,
    #[serde(default)]
    audit: RawAuditSection,
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
    #[serde(default = "default_trusted_proxies")]
    trusted_proxies: Vec<String>,
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
    #[serde(default = "default_hello_timeout_secs")]
    hello_timeout_secs: u64,
    #[serde(default = "default_max_outstanding_pings")]
    max_outstanding_pings: usize,
    #[serde(default = "default_insecure_transport_warn_interval_secs")]
    insecure_transport_warn_interval_secs: u64,
    #[serde(default = "default_max_sanitized_disks")]
    max_sanitized_disks: usize,
    #[serde(default = "default_max_sanitized_string_bytes")]
    max_sanitized_string_bytes: usize,
    #[serde(default = "default_metric_anomaly_session_limit")]
    metric_anomaly_session_limit: usize,
    #[serde(default = "default_sqlite_busy_timeout_secs")]
    sqlite_busy_timeout_secs: u64,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAuditSection {
    #[serde(default = "default_audit_enabled")]
    enabled: bool,
    #[serde(default = "default_audit_db_path")]
    db_path: PathBuf,
    #[serde(default = "default_audit_retention_days")]
    retention_days: u64,
    #[serde(default = "default_audit_log_successful_auth")]
    log_successful_auth: bool,
    #[serde(default = "default_audit_log_failed_auth")]
    log_failed_auth: bool,
    #[serde(default = "default_audit_log_token_events")]
    log_token_events: bool,
    #[serde(default = "default_audit_log_rate_limit")]
    log_rate_limit: bool,
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

impl Default for RawAuditSection {
    fn default() -> Self {
        Self {
            enabled: default_audit_enabled(),
            db_path: default_audit_db_path(),
            retention_days: default_audit_retention_days(),
            log_successful_auth: default_audit_log_successful_auth(),
            log_failed_auth: default_audit_log_failed_auth(),
            log_token_events: default_audit_log_token_events(),
            log_rate_limit: default_audit_log_rate_limit(),
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
pub(super) struct RawAgentConfigFile {
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
    #[serde(default = "default_connect_timeout_secs")]
    connect_timeout_secs: u64,
    #[serde(default = "default_max_incoming_message_bytes")]
    max_incoming_message_bytes: usize,
    #[serde(default = "default_insecure_transport_warn_interval_secs")]
    insecure_transport_warn_interval_secs: u64,
}

struct ValidatedInstall {
    agent_release_base_url: Option<String>,
    agent_release_sha256_x86_64: Option<String>,
    agent_release_sha256_aarch64: Option<String>,
}

impl RawServerConfigFile {
    /// 集中执行所有跨字段、跨小节的语义校验。
    pub(super) fn validate(self) -> Result<ServerConfig, ConfigError> {
        let listen = self.parse_listen()?;
        self.validate_public_base_url()?;
        let trusted_proxies = parse_trusted_proxies(self.server.trusted_proxies.clone())?;
        let install = self.validate_install()?;
        let readonly_auth = self.validate_auth(&listen)?;
        let audit = self.validate_audit()?;
        self.validate_server_limits()?;
        self.validate_ws_limits()?;
        self.validate_ui_limits()?;

        Ok(ServerConfig {
            listen,
            public_base_url: self.server.public_base_url,
            insecure_allow_http: self.server.insecure_allow_http,
            trusted_proxies,
            readonly_auth,
            ws: WsConfig {
                max_total_connections: self.ws.max_total_connections,
                max_connections_per_ip: self.ws.max_connections_per_ip,
                auth_fail_window_secs: self.ws.auth_fail_window_secs,
                auth_fail_max_attempts: self.ws.auth_fail_max_attempts,
                auth_block_secs: self.ws.auth_block_secs,
            },
            audit,
            node_registry_path: self.server.node_registry_path,
            history_db_path: self.server.history_db_path,
            snapshot_path: self.server.snapshot_path,
            stale_after_secs: self.server.stale_after_secs,
            ping_interval_secs: self.server.ping_interval_secs,
            max_message_bytes: self.server.max_message_bytes,
            refresh_interval_secs: self.ui.refresh_interval_secs,
            ignored_filesystems: normalize_string_list(self.filters.ignored_filesystems),
            agent_release_base_url: install.agent_release_base_url,
            agent_release_sha256_x86_64: install.agent_release_sha256_x86_64,
            agent_release_sha256_aarch64: install.agent_release_sha256_aarch64,
            hello_timeout_secs: self.server.hello_timeout_secs,
            max_outstanding_pings: self.server.max_outstanding_pings,
            insecure_transport_warn_interval_secs: self
                .server
                .insecure_transport_warn_interval_secs,
            max_sanitized_disks: self.server.max_sanitized_disks,
            max_sanitized_string_bytes: self.server.max_sanitized_string_bytes,
            metric_anomaly_session_limit: self.server.metric_anomaly_session_limit,
            sqlite_busy_timeout_secs: self.server.sqlite_busy_timeout_secs,
        })
    }

    fn parse_listen(&self) -> Result<SocketAddr, ConfigError> {
        self.server
            .listen
            .parse::<SocketAddr>()
            .map_err(|error| ConfigError::new(format!("invalid server.listen: {error}")))
    }

    fn validate_public_base_url(&self) -> Result<(), ConfigError> {
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
        Ok(())
    }

    fn validate_install(&self) -> Result<ValidatedInstall, ConfigError> {
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
            .as_deref()
            .map(str::trim)
            .map(str::to_string);
        let agent_release_sha256_aarch64 = self
            .install
            .agent_release_sha256_aarch64
            .as_deref()
            .map(str::trim)
            .map(str::to_string);
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

        Ok(ValidatedInstall {
            agent_release_base_url: self.install.agent_release_base_url.clone(),
            agent_release_sha256_x86_64,
            agent_release_sha256_aarch64,
        })
    }

    fn validate_audit(&self) -> Result<AuditConfig, ConfigError> {
        if self.audit.retention_days == 0 {
            return Err(ConfigError::new(
                "audit.retention_days must be greater than 0",
            ));
        }

        Ok(AuditConfig {
            enabled: self.audit.enabled,
            db_path: self.audit.db_path.clone(),
            retention_days: self.audit.retention_days,
            log_successful_auth: self.audit.log_successful_auth,
            log_failed_auth: self.audit.log_failed_auth,
            log_token_events: self.audit.log_token_events,
            log_rate_limit: self.audit.log_rate_limit,
        })
    }

    fn validate_auth(
        &self,
        listen: &SocketAddr,
    ) -> Result<Option<ReadonlyAuthConfig>, ConfigError> {
        let enable_2fa = self.auth.enable_2fa;
        let totp_secret = self
            .auth
            .totp_secret
            .as_deref()
            .map(normalize_totp_secret)
            .filter(|value| !value.is_empty());
        if enable_2fa && self.auth.username.is_none() {
            return Err(ConfigError::new(
                "auth.username and auth.password are required when auth.enable_2fa = true",
            ));
        }
        if enable_2fa
            && totp_secret
                .as_deref()
                .map(|secret| secret.is_empty())
                .unwrap_or(true)
        {
            return Err(ConfigError::new(
                "auth.totp_secret is required when auth.enable_2fa = true",
            ));
        }
        if enable_2fa {
            self.validate_https_for_two_factor()?;
            if let Some(secret) = totp_secret.as_deref() {
                validate_totp_secret("auth.totp_secret", secret)?;
            }
        }

        let readonly_auth = self.build_readonly_auth(enable_2fa, totp_secret)?;
        if readonly_auth.is_none() && !listen.ip().is_loopback() {
            return Err(ConfigError::new(
                "auth.username and auth.password are required when server.listen is not loopback",
            ));
        }
        Ok(readonly_auth)
    }

    fn validate_https_for_two_factor(&self) -> Result<(), ConfigError> {
        if !self.server.public_base_url.starts_with("https://") {
            return Err(ConfigError::new(
                "server.public_base_url must use https:// when auth.enable_2fa = true",
            ));
        }
        Ok(())
    }

    fn build_readonly_auth(
        &self,
        enable_2fa: bool,
        totp_secret: Option<String>,
    ) -> Result<Option<ReadonlyAuthConfig>, ConfigError> {
        match (
            self.auth
                .username
                .as_deref()
                .map(str::trim)
                .map(str::to_string),
            self.auth
                .password
                .as_deref()
                .map(str::trim)
                .map(str::to_string),
        ) {
            (Some(username), Some(password)) => {
                validate_non_empty("auth.username", &username)?;
                validate_non_empty("auth.password", &password)?;
                Ok(Some(ReadonlyAuthConfig {
                    username,
                    password,
                    enable_2fa,
                    totp_secret,
                }))
            }
            (None, None) => Ok(None),
            (Some(_), None) => Err(ConfigError::new(
                "auth.password must be set when auth.username is configured",
            )),
            (None, Some(_)) => Err(ConfigError::new(
                "auth.username must be set when auth.password is configured",
            )),
        }
    }

    fn validate_server_limits(&self) -> Result<(), ConfigError> {
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
        Ok(())
    }

    fn validate_ws_limits(&self) -> Result<(), ConfigError> {
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
        Ok(())
    }

    fn validate_ui_limits(&self) -> Result<(), ConfigError> {
        if self.ui.refresh_interval_secs < 1 {
            return Err(ConfigError::new(
                "ui.refresh_interval_secs must be at least 1 second",
            ));
        }
        Ok(())
    }
}

impl RawAgentConfigFile {
    /// 校验 Agent 配置,并把 `agent.tags` 等字段规范化(去空白、去重、排序)。
    pub(super) fn validate(self) -> Result<AgentConfig, ConfigError> {
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
            connect_timeout_secs: self.agent.connect_timeout_secs,
            max_incoming_message_bytes: self.agent.max_incoming_message_bytes,
            insecure_transport_warn_interval_secs: self.agent.insecure_transport_warn_interval_secs,
        })
    }
}
