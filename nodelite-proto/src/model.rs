//! 监控数据模型:描述节点身份、单次采样以及历史聚合等核心结构。
//! 这些类型同时被 Agent(生产数据)和 Server(消费、存储与下发到前端)使用。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// 节点身份信息,在 Agent 启动并发送 `Hello` 时确定,后续不再变更。
///
/// `tags` 用于在前端进行分组或过滤,具体语义由部署方约定。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NodeIdentity {
    pub node_id: String,
    pub node_label: String,
    pub hostname: String,
    pub os: String,
    pub kernel_version: Option<String>,
    pub cpu_model: Option<String>,
    pub cpu_cores: u32,
    pub agent_version: String,
    pub boot_time: Option<DateTime<Utc>>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Linux 三档平均负载,与 `uptime` / `/proc/loadavg` 输出一致。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LoadAverage {
    pub one: f64,
    pub five: f64,
    pub fifteen: f64,
}

/// 内存使用情况,所有字段以字节为单位。
///
/// `available_bytes` 取自 `MemAvailable`(若不可用则用 `MemFree + Buffers + Cached` 近似)。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryUsage {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    pub swap_total_bytes: u64,
    pub swap_used_bytes: u64,
}

/// 单个挂载点的磁盘使用情况。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DiskUsage {
    pub device: String,
    pub mount_point: String,
    pub fs_type: String,
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub used_bytes: u64,
    pub used_percent: f64,
}

/// 全节点网络计数器,既包括累计字节数,也提供即时速率。
///
/// 即时速率在 Agent 启动后第一次采样时不可用,因此为 `Option`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NetworkCounters {
    pub total_rx_bytes: u64,
    pub total_tx_bytes: u64,
    pub rx_bytes_per_sec: Option<f64>,
    pub tx_bytes_per_sec: Option<f64>,
}

/// 单次采样得到的完整节点快照。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NodeSnapshot {
    pub collected_at: DateTime<Utc>,
    pub cpu_usage_percent: f64,
    pub load: LoadAverage,
    pub memory: MemoryUsage,
    pub uptime_secs: u64,
    #[serde(default)]
    pub disks: Vec<DiskUsage>,
    pub network: NetworkCounters,
}

/// Server 端维护的节点运行态:身份 + 最新快照 + 在线状态。
///
/// `snapshot` 在 Hello 之后、首次 Metrics 之前可能为 `None`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NodeStatus {
    pub identity: NodeIdentity,
    #[serde(default)]
    pub remote_ip: Option<String>,
    pub snapshot: Option<NodeSnapshot>,
    pub last_seen: Option<DateTime<Utc>>,
    pub latency_ms: Option<u64>,
    pub online: bool,
}

/// 历史采样点,用于 SQLite 持久化与前端图表绘制。
///
/// 与 `NodeSnapshot` 的区别在于仅保留有损但够用的关键指标,降低存储成本。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HistoryPoint {
    pub node_id: String,
    pub recorded_at: DateTime<Utc>,
    pub cpu_usage_percent: f64,
    pub memory_used_percent: f64,
    pub rx_bytes_per_sec: Option<f64>,
    pub tx_bytes_per_sec: Option<f64>,
    pub latency_ms: Option<u64>,
    pub disk_used_percent: Option<f64>,
}

/// 仪表盘顶部的全局概览数据,由 Server 实时聚合得到。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OverviewData {
    pub generated_at: DateTime<Utc>,
    pub total_nodes: usize,
    pub online_nodes: usize,
    pub offline_nodes: usize,
    pub total_rx_bytes: u64,
    pub total_tx_bytes: u64,
    pub current_rx_bytes_per_sec: f64,
    pub current_tx_bytes_per_sec: f64,
    pub average_latency_ms: Option<f64>,
}

impl MemoryUsage {
    /// 内存使用百分比(已用 / 总量)。
    pub fn used_percent(&self) -> f64 {
        percentage(self.used_bytes, self.total_bytes)
    }

    /// 交换分区使用百分比;若主机未启用 swap,则返回 `None`。
    pub fn swap_used_percent(&self) -> Option<f64> {
        (self.swap_total_bytes > 0).then(|| percentage(self.swap_used_bytes, self.swap_total_bytes))
    }
}

/// 通用百分比工具:防止除零并直接返回 0..=100 区间外的值。
pub fn percentage(used: u64, total: u64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    (used as f64 / total as f64) * 100.0
}
