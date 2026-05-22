//! NodeLite 协议公共库:定义 Agent 与 Server 共享的数据结构和通信约束。
//!
//! - `config` — 解析并校验 Agent / Server 的 TOML 配置文件。
//! - `message` — WebSocket 上传输的线协议(WireMessage)。
//! - `model` — 节点身份、监控快照、历史采样等数据模型。

pub mod config;
pub mod message;
pub mod model;
pub mod netutil;
pub mod text;
pub mod validation;

pub use config::{
    AgentConfig, AuditConfig, ConfigError, DEFAULT_AUDIT_RETENTION_DAYS,
    DEFAULT_HISTORY_RETENTION_HOURS, DEFAULT_HISTORY_WRITE_INTERVAL_SECS,
    DEFAULT_MAX_MESSAGE_BYTES, DEFAULT_PING_INTERVAL_SECS, DEFAULT_REFRESH_INTERVAL_SECS,
    DEFAULT_REPORT_INTERVAL_SECS, DEFAULT_STALE_AFTER_SECS, MAX_NODE_TAG_BYTES, MAX_NODE_TAGS,
    ReadonlyAuthConfig, ServerConfig, WsConfig, normalize_totp_secret, parse_agent_config,
    parse_server_config,
};
pub use message::{
    AgentLogEntry, AgentLogsMessage, HelloMessage, MetricsMessage, NoticeLevel, PingMessage,
    PongMessage, RefreshTokenRequestMessage, RefreshTokenResponseMessage, ServerNoticeMessage,
    WIRE_PROTOCOL_VERSION, WireMessage,
};
pub use model::{
    DiskUsage, HistoryPoint, LoadAverage, MemoryUsage, NetworkCounters, NodeIdentity, NodeSnapshot,
    NodeStatus, OverviewData, percentage,
};
pub use netutil::{host_is_local, uses_insecure_remote_url};
pub use text::{truncate_string_to_byte_boundary, truncate_to_byte_boundary};
pub use validation::{
    ValidationError, normalize_string_list, validate_identifier, validate_non_empty,
    validate_tag_list,
};
