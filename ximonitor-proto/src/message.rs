// Agent 与 Server 之间通过 WebSocket 交换的消息定义。
// 所有消息均为 JSON 文本帧,顶层使用 `type` 字段进行内部标记式枚举区分。

use serde::{Deserialize, Serialize};

use crate::model::{NodeIdentity, NodeSnapshot};

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
}

/// Agent 连接 Server 时发送的首个消息。
///
/// `token` 由 Server 的节点注册表分发,`identity` 由 Agent 在本地采集后填充。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HelloMessage {
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RefreshTokenRequestMessage {
    pub node_id: String,
}

/// Server 响应 Token 刷新请求。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RefreshTokenResponseMessage {
    pub new_token: String,
    pub expires_at: String, // ISO 8601 格式
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

    use super::{HelloMessage, NoticeLevel, ServerNoticeMessage, WireMessage};
    use crate::model::{LoadAverage, MemoryUsage, NetworkCounters, NodeIdentity, NodeSnapshot};

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

        for message in [hello, snapshot, notice] {
            let encoded = serde_json::to_string(&message).expect("encode");
            let decoded: WireMessage = serde_json::from_str(&encoded).expect("decode");
            assert_eq!(message, decoded);
        }
    }
}
