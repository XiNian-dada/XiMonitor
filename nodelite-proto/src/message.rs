//! Agent 与 Server 之间通过 WebSocket 交换的消息定义。
//! 所有消息均为 JSON 文本帧,顶层使用 `type` 字段进行内部标记式枚举区分。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::model::{NodeIdentity, NodeListItem, NodeSnapshot, OverviewData};

/// 当前 WebSocket 线协议版本。
///
/// 只要 `WireMessage` 的兼容性承诺被打破(删除字段、修改语义、移除变体),
/// 就必须递增该版本,让 server 在握手阶段拒绝不兼容 agent。
pub const WIRE_PROTOCOL_VERSION: u16 = 2;

/// Server 当前仍接受的最早线协议版本。
///
/// v1 Agent 会继续把 CPU 首帧表示为 `0`,v2 Agent 则可用 `null` 表示差分尚未就绪。
pub const MIN_SUPPORTED_WIRE_PROTOCOL_VERSION: u16 = 1;

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

/// Server → 浏览器 WebSocket(`/ws/browser`)通道上的消息。
///
/// 与 Agent 通道的 [`WireMessage`] 区分:浏览器通道是只读监控推送,客户端只发送
/// 应用层 [`BrowserMessage::Ping`](浏览器 `WebSocket` API 无法发送协议级 ping 帧)。
///
/// 除全量 `InitialState` 外都是**增量**:单节点变化只发该节点一行,而非整张列表。
/// 每条消息携带 `generated_at`,客户端据此做单调时间戳守卫,丢弃乱序/过期消息。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BrowserMessage {
    /// 连接建立(以及重连 / 服务端 lag 恢复)时下发的全量快照,客户端整体替换本地状态。
    InitialState {
        generated_at: DateTime<Utc>,
        overview: OverviewData,
        nodes: Vec<NodeListItem>,
    },
    /// 概览聚合数字更新(整体替换,体积很小)。
    OverviewUpdate {
        generated_at: DateTime<Utc>,
        overview: OverviewData,
    },
    /// 单节点增量:新增或更新一行,客户端按 `node_id` 合并进本地 Map。
    NodeUpsert {
        generated_at: DateTime<Utc>,
        node: NodeListItem,
    },
    /// 单节点移除(注销),客户端按 `node_id` 删除。
    NodeRemoved {
        generated_at: DateTime<Utc>,
        node_id: String,
    },
    /// 应用层心跳:客户端发送 `Ping`,服务端回 `Pong`。
    Ping,
    /// 服务端对客户端 `Ping` 的应答。
    Pong,
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

    #[test]
    fn metrics_cpu_usage_accepts_legacy_number_and_null() {
        let base_payload = r#"{
            "type":"metrics",
            "snapshot":{
                "collected_at":"2026-05-07T01:00:00Z",
                "load":{"one":0.3,"five":0.4,"fifteen":0.5},
                "memory":{
                    "total_bytes":1024,
                    "used_bytes":512,
                    "available_bytes":512,
                    "swap_total_bytes":0,
                    "swap_used_bytes":0
                },
                "uptime_secs":60,
                "disks":[],
                "network":{
                    "total_rx_bytes":100,
                    "total_tx_bytes":200,
                    "rx_bytes_per_sec":null,
                    "tx_bytes_per_sec":null
                }
            }
        }"#;
        let legacy_payload =
            base_payload.replace(r#""load""#, r#""cpu_usage_percent":42.5,"load""#);
        let null_payload = base_payload.replace(r#""load""#, r#""cpu_usage_percent":null,"load""#);

        let legacy: WireMessage =
            serde_json::from_str(&legacy_payload).expect("legacy numeric cpu should parse");
        let null: WireMessage =
            serde_json::from_str(&null_payload).expect("nullable cpu should parse");

        let WireMessage::Metrics(legacy) = legacy else {
            panic!("legacy payload should decode as metrics");
        };
        let WireMessage::Metrics(null) = null else {
            panic!("null payload should decode as metrics");
        };
        assert_eq!(legacy.snapshot.cpu_usage_percent, Some(42.5));
        assert_eq!(null.snapshot.cpu_usage_percent, None);
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
                cpu_usage_percent: Some(42.5),
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

    /// 验证所有 BrowserMessage 子类型(含增量与心跳)都能完整序列化和反序列化。
    #[test]
    fn round_trips_browser_messages() {
        use super::BrowserMessage;
        use crate::model::{
            NodeListIdentity, NodeListItem, NodeListLoadAverage, NodeListMemoryUsage,
            NodeListSnapshot, OverviewData,
        };

        let generated_at = Utc.with_ymd_and_hms(2026, 5, 31, 12, 0, 0).unwrap();
        let overview = OverviewData {
            generated_at,
            total_nodes: 3,
            online_nodes: 2,
            offline_nodes: 1,
            total_rx_bytes: 1000,
            total_tx_bytes: 2000,
            current_rx_bytes_per_sec: 12.5,
            current_tx_bytes_per_sec: 24.0,
            average_latency_ms: Some(7.5),
        };
        let node = NodeListItem {
            identity: NodeListIdentity {
                node_id: "hk-01".to_string(),
                node_label: "Hong Kong 01".to_string(),
                hostname: "hk-01".to_string(),
                tags: vec!["apac".to_string()],
            },
            snapshot: Some(NodeListSnapshot {
                cpu_usage_percent: Some(33.0),
                load: NodeListLoadAverage { one: 0.5 },
                memory: NodeListMemoryUsage {
                    total_bytes: 2048,
                    used_bytes: 1024,
                },
            }),
            latency_ms: Some(9),
            online: true,
        };

        let initial = BrowserMessage::InitialState {
            generated_at,
            overview: overview.clone(),
            nodes: vec![node.clone()],
        };
        let overview_update = BrowserMessage::OverviewUpdate {
            generated_at,
            overview,
        };
        let upsert = BrowserMessage::NodeUpsert { generated_at, node };
        let removed = BrowserMessage::NodeRemoved {
            generated_at,
            node_id: "hk-01".to_string(),
        };

        for message in [
            initial,
            overview_update,
            upsert,
            removed,
            BrowserMessage::Ping,
            BrowserMessage::Pong,
        ] {
            let encoded = serde_json::to_string(&message).expect("encode");
            let decoded: BrowserMessage = serde_json::from_str(&encoded).expect("decode");
            assert_eq!(message, decoded);
        }

        // 标记式枚举的线格式:单元变体只剩一个 `type` 字段。
        assert_eq!(
            serde_json::to_string(&BrowserMessage::Ping).expect("encode"),
            r#"{"type":"ping"}"#
        );
        // 客户端发来的 ping 文本帧必须能被服务端解析为 Ping。
        let parsed: BrowserMessage =
            serde_json::from_str(r#"{"type":"ping"}"#).expect("parse client ping");
        assert_eq!(parsed, BrowserMessage::Ping);
    }
}
