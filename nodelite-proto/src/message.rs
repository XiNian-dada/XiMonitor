//! Agent 与 Server 之间通过 WebSocket 交换的消息定义。
//! 所有消息均为 JSON 文本帧,顶层使用 `type` 字段进行内部标记式枚举区分。

use serde::{Deserialize, Serialize};

use crate::model::{NodeIdentity, NodeSnapshot};

/// 当前 WebSocket 线协议版本。
///
/// 只要 `WireMessage` 的兼容性承诺被打破(删除字段、修改语义、移除变体),
/// 就必须递增该版本,让 server 在握手阶段拒绝不兼容 agent。
pub const WIRE_PROTOCOL_VERSION: u16 = 1;

fn current_protocol_version() -> u16 {
    WIRE_PROTOCOL_VERSION
}

/// 线协议消息枚举:WebSocket 通道上允许出现的所有消息类型。
///
/// 序列化时通过 `type` 字段区分子类型,例如 `{"type":"hello", ...}`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WireMessage {
    /// Agent 建立连接后发送的握手消息,携带身份与令牌。
    Hello(HelloMessage),
    /// Agent 周期性上报的监控快照。
    Metrics(MetricsMessage),
    /// Server 发往 Agent 的心跳探测,用于测量往返时延。
    Ping(PingMessage),
    /// Agent 对 Server `Ping` 的响应。
    Pong(PongMessage),
    /// Server 推送给 Agent 的告知性消息(认证成功、错误提示等)。
    ServerNotice(ServerNoticeMessage),
    /// Agent 请求刷新即将过期的 Token。
    RefreshTokenRequest(RefreshTokenRequestMessage),
    /// Server 响应 Token 刷新请求,返回新 Token 和过期时间。
    RefreshTokenResponse(RefreshTokenResponseMessage),
    /// Agent 批量上报自身运行日志,供服务端日志页排障使用。
    AgentLogs(AgentLogsMessage),
}

/// Agent 连接 Server 时发送的首个消息。
///
/// `token` 由 Server 的节点注册表分发,`identity` 由 Agent 在本地采集后填充。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HelloMessage {
    #[serde(default = "current_protocol_version")]
    pub protocol_version: u16,
    pub token: String,
    pub identity: NodeIdentity,
}

/// Agent 周期性上报的监控数据包装。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MetricsMessage {
    pub snapshot: NodeSnapshot,
}

/// Server 发往 Agent 的心跳请求,`nonce` 用于配对返回的 Pong。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PingMessage {
    pub nonce: u64,
}

/// Agent 回复的心跳响应,需要回传相同的 `nonce`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PongMessage {
    pub nonce: u64,
}

/// Server 推送的通知消息,Agent 用于日志输出与判定认证状态等。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerNoticeMessage {
    pub level: NoticeLevel,
    pub message: String,
}

/// Agent 请求刷新 Token(当 Token 即将过期时)。
///
/// `node_id` 字段由历史原因保留以兼容旧客户端,**服务端不再使用它**:刷新
/// 的目标节点完全由 WebSocket 会话的认证身份决定。未来一个协议大版本
/// 可以彻底移除该字段。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RefreshTokenRequestMessage {
    #[serde(default)]
    pub node_id: String,
}

/// Server 响应 Token 刷新请求。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RefreshTokenResponseMessage {
    pub new_token: String,
    pub expires_at: String, // ISO 8601 格式
}

/// Agent 运行时日志中的单条结构化事件。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentLogEntry {
    pub occurred_at: String, // ISO 8601 格式
    pub level: NoticeLevel,
    pub message: String,
}

/// Agent 批量上传的运行时日志。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentLogsMessage {
    pub entries: Vec<AgentLogEntry>,
}

/// 通知级别,与常见的日志等级对应。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NoticeLevel {
    Info,
    Warn,
    Error,
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::{
        AgentLogEntry, AgentLogsMessage, HelloMessage, NoticeLevel, ServerNoticeMessage,
        WIRE_PROTOCOL_VERSION, WireMessage,
    };
    use crate::model::{LoadAverage, MemoryUsage, NetworkCounters, NodeIdentity, NodeSnapshot};

    #[test]
    fn hello_without_protocol_version_defaults_to_current_version() {
        let payload = r#"{
            "token":"node-token",
            "identity":{
                "node_id":"node-1",
                "node_label":"Node 1",
                "hostname":"node-1",
                "os":"Linux",
                "kernel_version":"6.8",
                "cpu_model":"test cpu",
                "cpu_cores":2,
                "agent_version":"test",
                "tags":[]
            }
        }"#;

        let hello: HelloMessage = serde_json::from_str(payload).expect("valid legacy hello");
        assert_eq!(hello.protocol_version, WIRE_PROTOCOL_VERSION);
    }

    /// 验证所有 WireMessage 子类型都能完整序列化和反序列化。
    #[test]
    fn round_trips_wire_messages() {
        let identity = NodeIdentity {
            node_id: "hk-01".to_string(),
            node_label: "Hong Kong 01".to_string(),
            hostname: "hk-01".to_string(),
            os: "linux".to_string(),
            kernel_version: Some("6.6.1".to_string()),
            cpu_model: Some("AMD EPYC".to_string()),
            cpu_cores: 8,
            agent_version: "0.1.0".to_string(),
            boot_time: Some(Utc.with_ymd_and_hms(2026, 5, 7, 0, 0, 0).unwrap()),
            tags: vec!["apac".to_string()],
        };
        let hello = WireMessage::Hello(HelloMessage {
            protocol_version: WIRE_PROTOCOL_VERSION,
            token: "token".to_string(),
            identity: identity.clone(),
        });
        let snapshot = WireMessage::Metrics(super::MetricsMessage {
            snapshot: NodeSnapshot {
                collected_at: Utc.with_ymd_and_hms(2026, 5, 7, 1, 0, 0).unwrap(),
                cpu_usage_percent: 42.5,
                load: LoadAverage {
                    one: 0.3,
                    five: 0.4,
                    fifteen: 0.5,
                },
                memory: MemoryUsage {
                    total_bytes: 1024,
                    used_bytes: 512,
                    available_bytes: 256,
                    swap_total_bytes: 2048,
                    swap_used_bytes: 128,
                },
                uptime_secs: 3600,
                disks: Vec::new(),
                network: NetworkCounters {
                    total_rx_bytes: 100,
                    total_tx_bytes: 200,
                    rx_bytes_per_sec: Some(10.0),
                    tx_bytes_per_sec: Some(20.0),
                },
            },
        });
        let notice = WireMessage::ServerNotice(ServerNoticeMessage {
            level: NoticeLevel::Warn,
            message: "careful".to_string(),
        });
        let agent_logs = WireMessage::AgentLogs(AgentLogsMessage {
            entries: vec![AgentLogEntry {
                occurred_at: Utc
                    .with_ymd_and_hms(2026, 5, 7, 1, 2, 3)
                    .unwrap()
                    .to_rfc3339(),
                level: NoticeLevel::Info,
                message: "authenticated".to_string(),
            }],
        });

        for message in [hello, snapshot, notice, agent_logs] {
            let encoded = serde_json::to_string(&message).expect("encode");
            let decoded: WireMessage = serde_json::from_str(&encoded).expect("decode");
            assert_eq!(message, decoded);
        }
    }
}
