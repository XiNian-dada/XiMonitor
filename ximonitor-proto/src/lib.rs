// XiMonitor 协议公共库:定义 Agent 与 Server 共享的数据结构和通信约束。
//
// - `config`:解析并校验 Agent / Server 的 TOML 配置文件。
// - `message`:WebSocket 上传输的线协议(WireMessage)。
// - `model`:节点身份、监控快照、历史采样等数据模型。

pub mod config;
pub mod message;
pub mod model;

pub use config::{
    AgentConfig, ConfigError, DEFAULT_HISTORY_RETENTION_HOURS, DEFAULT_HISTORY_WRITE_INTERVAL_SECS,
    DEFAULT_MAX_MESSAGE_BYTES, DEFAULT_PING_INTERVAL_SECS, DEFAULT_REFRESH_INTERVAL_SECS,
    DEFAULT_REPORT_INTERVAL_SECS, DEFAULT_STALE_AFTER_SECS, ReadonlyAuthConfig, ServerConfig,
    WsConfig, parse_agent_config, parse_server_config,
};
pub use message::{
    HelloMessage, MetricsMessage, NoticeLevel, PingMessage, PongMessage, ServerNoticeMessage,
    WireMessage,
};
pub use model::{
    DiskUsage, HistoryPoint, LoadAverage, MemoryUsage, NetworkCounters, NodeIdentity, NodeSnapshot,
    NodeStatus, OverviewData, percentage,
};
