// 服务端共享状态:在内存中维护节点身份、最新快照与会话生命周期。
//
// `SharedState` 通过 `Arc<RwLock<Registry>>` 在多个异步任务之间共享:
//   - WebSocket 处理任务写入最新快照、延迟值和在线状态;
//   - HTTP API 任务读取整体视图;
//   - 后台任务定期把超时节点标记为离线、把状态持久化到磁盘。
//
// 设计要点:用单调递增的 `session_id` 区分同一节点的多次连接,避免"旧会话"
// 的延迟数据覆盖"新会话"的最新数据。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::sync::{RwLock, mpsc, oneshot};
use ximonitor_proto::{NodeIdentity, NodeSnapshot, NodeStatus, OverviewData, ServerConfig};

/// 运行中的 WebSocket 会话可接收的控制命令。
pub(crate) enum SessionCommand {
    RefreshToken {
        response: oneshot::Sender<Result<SessionRefreshReply, String>>,
    },
}

/// 一次在线 token 续期完成后返回给调用方的摘要。
#[derive(Debug, Clone)]
pub(crate) struct SessionRefreshReply {
    pub token_expires_at: DateTime<Utc>,
}

/// 向在线节点下发控制命令时可能遇到的失败类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionCommandError {
    NodeOffline,
    SessionClosed,
}

impl std::fmt::Display for SessionCommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NodeOffline => f.write_str("node is offline"),
            Self::SessionClosed => f.write_str("node session is no longer available"),
        }
    }
}

impl std::error::Error for SessionCommandError {}

/// 共享状态的对外句柄,可以低成本地克隆给每个异步任务。
#[derive(Clone)]
pub struct SharedState {
    config: Arc<ServerConfig>,
    registry: Arc<RwLock<Registry>>,
    next_session_id: Arc<AtomicU64>,
}

impl SharedState {
    pub fn new(config: Arc<ServerConfig>) -> Self {
        Self {
            config,
            registry: Arc::new(RwLock::new(Registry::default())),
            next_session_id: Arc::new(AtomicU64::new(1)),
        }
    }

    pub fn config(&self) -> &ServerConfig {
        self.config.as_ref()
    }

    /// 登记一个新的 WebSocket 会话并返回唯一的 `session_id`。
    /// 同一节点重连时会得到比上次更大的 ID,从而抢占老的会话。
    pub async fn register_node(&self, identity: NodeIdentity, remote_ip: Option<String>) -> u64 {
        let session_id = self.next_session_id.fetch_add(1, Ordering::Relaxed);
        let now = Utc::now();
        let mut registry = self.registry.write().await;
        registry.register_node(session_id, identity, remote_ip, now);
        session_id
    }

    /// 更新某节点的最新快照。若该会话已被新会话替代,则返回 `None` 告知调用方丢弃。
    pub async fn update_snapshot(
        &self,
        node_id: &str,
        session_id: u64,
        snapshot: NodeSnapshot,
    ) -> Option<NodeStatus> {
        let mut registry = self.registry.write().await;
        registry.update_snapshot(node_id, session_id, snapshot, Utc::now())
    }

    /// 更新某节点的最新延迟值,语义同 `update_snapshot`。
    pub async fn update_latency(&self, node_id: &str, session_id: u64, latency_ms: u64) -> bool {
        let mut registry = self.registry.write().await;
        registry.update_latency(node_id, session_id, latency_ms, Utc::now())
    }

    /// 标记某会话的连接已断开。如果当前活跃 ID 不再等于该会话,则什么也不做。
    pub async fn mark_disconnected(&self, node_id: &str, session_id: u64) {
        let mut registry = self.registry.write().await;
        registry.mark_disconnected(node_id, session_id);
    }

    /// 把在线会话的控制通道挂到节点上,供 HTTP 处理器向该节点下发命令。
    pub async fn attach_session_control(
        &self,
        node_id: &str,
        session_id: u64,
        control_tx: mpsc::UnboundedSender<SessionCommand>,
    ) -> bool {
        let mut registry = self.registry.write().await;
        registry.attach_session_control(node_id, session_id, control_tx)
    }

    /// 把超时(超过 `stale_after_secs`)的节点统一标记为离线,返回受影响节点数。
    pub async fn mark_stale(&self) -> usize {
        let mut registry = self.registry.write().await;
        registry.mark_stale(
            Duration::from_secs(self.config.stale_after_secs),
            Utc::now(),
        )
    }

    /// 判断给定 `session_id` 是否仍是该节点的当前会话。
    pub async fn is_current_session(&self, node_id: &str, session_id: u64) -> bool {
        let registry = self.registry.read().await;
        registry.is_current_session(node_id, session_id)
    }

    /// 列出所有节点的状态(按 `node_label`、`node_id` 升序)。
    pub async fn list_statuses(&self) -> Vec<NodeStatus> {
        let registry = self.registry.read().await;
        registry.list_statuses()
    }

    pub async fn get_status(&self, node_id: &str) -> Option<NodeStatus> {
        let registry = self.registry.read().await;
        registry.get_status(node_id)
    }

    /// 对在线 Agent 发起一次“立即续期”请求,返回一个用于等待结果的 receiver。
    pub async fn request_live_token_refresh(
        &self,
        node_id: &str,
    ) -> Result<oneshot::Receiver<Result<SessionRefreshReply, String>>, SessionCommandError> {
        let control_tx = {
            let registry = self.registry.read().await;
            registry
                .session_control(node_id)
                .ok_or(SessionCommandError::NodeOffline)?
        };

        let (response_tx, response_rx) = oneshot::channel();
        control_tx
            .send(SessionCommand::RefreshToken {
                response: response_tx,
            })
            .map_err(|_| SessionCommandError::SessionClosed)?;
        Ok(response_rx)
    }

    /// 生成全局概览数据,用于仪表盘顶部的统计卡片。
    pub async fn overview(&self) -> OverviewData {
        let registry = self.registry.read().await;
        registry.overview()
    }

    /// 启动时从磁盘快照恢复状态,所有节点都视为离线直至首次心跳到达。
    pub async fn restore_statuses(&self, statuses: Vec<NodeStatus>) {
        let mut registry = self.registry.write().await;
        registry.restore_statuses(statuses);
    }
}

#[derive(Debug, Default)]
struct Registry {
    nodes: HashMap<String, NodeEntry>,
}

/// 单节点的注册项:对外暴露的 `status` 与内部的"当前活跃会话 ID"。
#[derive(Debug, Clone)]
struct NodeEntry {
    status: NodeStatus,
    active_session_id: Option<u64>,
    control_tx: Option<mpsc::UnboundedSender<SessionCommand>>,
}

impl Registry {
    fn register_node(
        &mut self,
        session_id: u64,
        identity: NodeIdentity,
        remote_ip: Option<String>,
        now: DateTime<Utc>,
    ) {
        let node_id = identity.node_id.clone();
        let entry = self.nodes.entry(node_id).or_insert_with(|| NodeEntry {
            status: NodeStatus {
                identity: identity.clone(),
                remote_ip: remote_ip.clone(),
                snapshot: None,
                last_seen: Some(now),
                latency_ms: None,
                online: true,
            },
            active_session_id: Some(session_id),
            control_tx: None,
        });

        // 已存在的节点也要把身份与会话 ID 刷新成"最新连接"的版本。
        entry.status.identity = identity;
        entry.status.remote_ip = remote_ip;
        entry.status.online = true;
        entry.status.last_seen = Some(now);
        entry.status.latency_ms = None;
        entry.active_session_id = Some(session_id);
        entry.control_tx = None;
    }

    fn update_snapshot(
        &mut self,
        node_id: &str,
        session_id: u64,
        snapshot: NodeSnapshot,
        now: DateTime<Utc>,
    ) -> Option<NodeStatus> {
        let entry = self.nodes.get_mut(node_id)?;
        if entry.active_session_id != Some(session_id) {
            return None;
        }

        entry.status.snapshot = Some(snapshot);
        entry.status.last_seen = Some(now);
        entry.status.online = true;
        Some(entry.status.clone())
    }

    fn update_latency(
        &mut self,
        node_id: &str,
        session_id: u64,
        latency_ms: u64,
        now: DateTime<Utc>,
    ) -> bool {
        let Some(entry) = self.nodes.get_mut(node_id) else {
            return false;
        };
        if entry.active_session_id != Some(session_id) {
            return false;
        }

        entry.status.latency_ms = Some(latency_ms);
        entry.status.last_seen = Some(now);
        entry.status.online = true;
        true
    }

    fn mark_disconnected(&mut self, node_id: &str, session_id: u64) {
        let Some(entry) = self.nodes.get_mut(node_id) else {
            return;
        };
        if entry.active_session_id == Some(session_id) {
            entry.active_session_id = None;
            entry.status.online = false;
            entry.control_tx = None;
        }
    }

    fn attach_session_control(
        &mut self,
        node_id: &str,
        session_id: u64,
        control_tx: mpsc::UnboundedSender<SessionCommand>,
    ) -> bool {
        let Some(entry) = self.nodes.get_mut(node_id) else {
            return false;
        };
        if entry.active_session_id != Some(session_id) {
            return false;
        }

        entry.control_tx = Some(control_tx);
        true
    }

    fn mark_stale(&mut self, threshold: Duration, now: DateTime<Utc>) -> usize {
        let mut marked = 0;

        for entry in self.nodes.values_mut() {
            let Some(last_seen) = entry.status.last_seen else {
                continue;
            };
            let Ok(elapsed) = (now - last_seen).to_std() else {
                continue;
            };
            if elapsed >= threshold && entry.status.online {
                entry.status.online = false;
                entry.active_session_id = None;
                entry.control_tx = None;
                marked += 1;
            }
        }

        marked
    }

    fn is_current_session(&self, node_id: &str, session_id: u64) -> bool {
        self.nodes
            .get(node_id)
            .and_then(|entry| entry.active_session_id)
            == Some(session_id)
    }

    fn list_statuses(&self) -> Vec<NodeStatus> {
        let mut statuses: Vec<NodeStatus> = self
            .nodes
            .values()
            .map(|entry| entry.status.clone())
            .collect();
        statuses.sort_by(|left, right| {
            left.identity
                .node_label
                .cmp(&right.identity.node_label)
                .then_with(|| left.identity.node_id.cmp(&right.identity.node_id))
        });
        statuses
    }

    fn get_status(&self, node_id: &str) -> Option<NodeStatus> {
        self.nodes.get(node_id).map(|entry| entry.status.clone())
    }

    fn session_control(&self, node_id: &str) -> Option<mpsc::UnboundedSender<SessionCommand>> {
        let entry = self.nodes.get(node_id)?;
        if entry.active_session_id.is_none() || !entry.status.online {
            return None;
        }
        entry.control_tx.clone()
    }

    fn overview(&self) -> OverviewData {
        let statuses = self.list_statuses();
        let total_nodes = statuses.len();
        let online_nodes = statuses.iter().filter(|status| status.online).count();
        let offline_nodes = total_nodes.saturating_sub(online_nodes);
        let total_rx_bytes = statuses
            .iter()
            .filter_map(|status| status.snapshot.as_ref())
            .fold(0_u64, |total, snapshot| {
                total.saturating_add(snapshot.network.total_rx_bytes)
            });
        let total_tx_bytes = statuses
            .iter()
            .filter_map(|status| status.snapshot.as_ref())
            .fold(0_u64, |total, snapshot| {
                total.saturating_add(snapshot.network.total_tx_bytes)
            });
        let current_rx_bytes_per_sec = statuses
            .iter()
            .filter_map(|status| status.snapshot.as_ref())
            .filter_map(|snapshot| snapshot.network.rx_bytes_per_sec)
            .fold(0.0, sum_finite_f64);
        let current_tx_bytes_per_sec = statuses
            .iter()
            .filter_map(|status| status.snapshot.as_ref())
            .filter_map(|snapshot| snapshot.network.tx_bytes_per_sec)
            .fold(0.0, sum_finite_f64);

        let latencies: Vec<u64> = statuses
            .iter()
            .filter(|status| status.online)
            .filter_map(|status| status.latency_ms)
            .collect();
        // 用 u128 累加并取平均值,避免在罕见情况下(极大延迟、海量节点)
        // 触发 u64 加法溢出 —— debug 构建会 panic,release 构建会回绕成
        // 异常小的"平均延迟",污染仪表盘。
        let average_latency_ms = (!latencies.is_empty()).then(|| {
            let total: u128 = latencies.iter().map(|value| *value as u128).sum();
            total as f64 / latencies.len() as f64
        });

        OverviewData {
            generated_at: Utc::now(),
            total_nodes,
            online_nodes,
            offline_nodes,
            total_rx_bytes,
            total_tx_bytes,
            current_rx_bytes_per_sec,
            current_tx_bytes_per_sec,
            average_latency_ms,
        }
    }

    fn restore_statuses(&mut self, statuses: Vec<NodeStatus>) {
        self.nodes.clear();
        for mut status in statuses {
            // 重启后所有节点的真实状态都未知,统一标记为离线,等心跳到达后再上线。
            status.online = false;
            let node_id = status.identity.node_id.clone();
            self.nodes.insert(
                node_id,
                NodeEntry {
                    status,
                    active_session_id: None,
                    control_tx: None,
                },
            );
        }
    }
}

/// 把浮点数累加器中的非法值(NaN / 负值 / 溢出)安全过滤掉。
fn sum_finite_f64(total: f64, value: f64) -> f64 {
    if !value.is_finite() || value < 0.0 {
        return total;
    }

    let next = total + value;
    if next.is_finite() { next } else { f64::MAX }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use chrono::{Duration as ChronoDuration, TimeZone, Utc};
    use tokio::sync::mpsc;
    use ximonitor_proto::{LoadAverage, MemoryUsage, NodeSnapshot};
    use ximonitor_proto::{NetworkCounters, percentage};

    use super::{Registry, SessionCommand};
    use ximonitor_proto::NodeIdentity;

    #[test]
    fn newer_session_replaces_older_one() {
        let mut registry = Registry::default();
        let now = Utc.with_ymd_and_hms(2026, 5, 7, 0, 0, 0).unwrap();
        let identity = NodeIdentity {
            node_id: "hk-01".to_string(),
            node_label: "Hong Kong 01".to_string(),
            hostname: "hk-01".to_string(),
            os: "linux".to_string(),
            kernel_version: None,
            cpu_model: None,
            cpu_cores: 4,
            agent_version: "0.1.0".to_string(),
            boot_time: None,
            tags: Vec::new(),
        };

        registry.register_node(1, identity.clone(), Some("198.51.100.10".to_string()), now);
        registry.register_node(
            2,
            identity,
            Some("198.51.100.11".to_string()),
            now + ChronoDuration::seconds(3),
        );

        assert!(
            registry
                .update_snapshot("hk-01", 1, sample_snapshot(now), now)
                .is_none()
        );
        assert!(
            registry
                .update_snapshot(
                    "hk-01",
                    2,
                    sample_snapshot(now + ChronoDuration::seconds(4)),
                    now
                )
                .is_some()
        );
    }

    #[test]
    fn stale_nodes_are_marked_offline() {
        let mut registry = Registry::default();
        let now = Utc.with_ymd_and_hms(2026, 5, 7, 0, 0, 0).unwrap();

        registry.register_node(7, sample_identity(), Some("198.51.100.10".to_string()), now);
        assert_eq!(
            registry.mark_stale(Duration::from_secs(10), now + ChronoDuration::seconds(15)),
            1
        );
        assert!(
            !registry
                .list_statuses()
                .first()
                .expect("node status")
                .online
        );
    }

    #[test]
    fn overview_saturates_totals_and_skips_invalid_rates() {
        let mut registry = Registry::default();
        let now = Utc.with_ymd_and_hms(2026, 5, 7, 0, 0, 0).unwrap();

        registry.register_node(1, sample_identity(), Some("198.51.100.10".to_string()), now);
        registry.register_node(
            2,
            NodeIdentity {
                node_id: "sg-01".to_string(),
                node_label: "Singapore 01".to_string(),
                ..sample_identity()
            },
            Some("198.51.100.11".to_string()),
            now,
        );

        let mut first = sample_snapshot(now);
        first.network.total_rx_bytes = u64::MAX;
        first.network.total_tx_bytes = u64::MAX;
        first.network.rx_bytes_per_sec = Some(f64::INFINITY);
        first.network.tx_bytes_per_sec = Some(1.5);
        registry.update_snapshot("hk-01", 1, first, now);

        let mut second = sample_snapshot(now);
        second.network.total_rx_bytes = 42;
        second.network.total_tx_bytes = 99;
        second.network.rx_bytes_per_sec = Some(2.5);
        second.network.tx_bytes_per_sec = Some(-10.0);
        registry.update_snapshot("sg-01", 2, second, now);

        let overview = registry.overview();
        assert_eq!(overview.total_rx_bytes, u64::MAX);
        assert_eq!(overview.total_tx_bytes, u64::MAX);
        assert_eq!(overview.current_rx_bytes_per_sec, 2.5);
        assert_eq!(overview.current_tx_bytes_per_sec, 1.5);
    }

    #[test]
    fn overview_avoids_overflow_when_summing_latency() {
        // 用接近 u64::MAX 的延迟值复现"原始 sum::<u64>() 会溢出"的场景:
        // 旧实现在 debug 构建下 panic,release 构建下回绕成异常小的平均值。
        let mut registry = Registry::default();
        let now = Utc.with_ymd_and_hms(2026, 5, 7, 0, 0, 0).unwrap();

        registry.register_node(1, sample_identity(), Some("198.51.100.10".to_string()), now);
        registry.register_node(
            2,
            NodeIdentity {
                node_id: "sg-01".to_string(),
                node_label: "Singapore 01".to_string(),
                ..sample_identity()
            },
            Some("198.51.100.11".to_string()),
            now,
        );

        registry.update_snapshot("hk-01", 1, sample_snapshot(now), now);
        registry.update_snapshot("sg-01", 2, sample_snapshot(now), now);
        registry.update_latency("hk-01", 1, u64::MAX / 2 + 1, now);
        registry.update_latency("sg-01", 2, u64::MAX / 2 + 1, now);

        let overview = registry.overview();
        let average = overview
            .average_latency_ms
            .expect("average latency should be reported");
        assert!(average.is_finite());
        assert!(average > (u64::MAX as f64) / 4.0);
    }

    #[test]
    fn session_control_is_only_available_for_current_online_session() {
        let mut registry = Registry::default();
        let now = Utc.with_ymd_and_hms(2026, 5, 7, 0, 0, 0).unwrap();
        registry.register_node(7, sample_identity(), Some("198.51.100.10".to_string()), now);

        let (control_tx, _control_rx) = mpsc::unbounded_channel::<SessionCommand>();
        assert!(registry.attach_session_control("hk-01", 7, control_tx));
        assert!(registry.session_control("hk-01").is_some());

        registry.register_node(
            8,
            sample_identity(),
            Some("198.51.100.11".to_string()),
            now + ChronoDuration::seconds(1),
        );
        assert!(
            registry.session_control("hk-01").is_none(),
            "newer session should clear the previous control handle",
        );
    }

    #[test]
    fn mark_disconnected_clears_session_control() {
        let mut registry = Registry::default();
        let now = Utc.with_ymd_and_hms(2026, 5, 7, 0, 0, 0).unwrap();
        registry.register_node(9, sample_identity(), Some("198.51.100.10".to_string()), now);

        let (control_tx, _control_rx) = mpsc::unbounded_channel::<SessionCommand>();
        assert!(registry.attach_session_control("hk-01", 9, control_tx));
        registry.mark_disconnected("hk-01", 9);

        assert!(registry.session_control("hk-01").is_none());
    }

    fn sample_identity() -> NodeIdentity {
        NodeIdentity {
            node_id: "hk-01".to_string(),
            node_label: "Hong Kong 01".to_string(),
            hostname: "hk-01".to_string(),
            os: "linux".to_string(),
            kernel_version: None,
            cpu_model: None,
            cpu_cores: 4,
            agent_version: "0.1.0".to_string(),
            boot_time: None,
            tags: Vec::new(),
        }
    }

    fn sample_snapshot(now: chrono::DateTime<Utc>) -> NodeSnapshot {
        NodeSnapshot {
            collected_at: now,
            cpu_usage_percent: percentage(1, 2),
            load: LoadAverage {
                one: 0.1,
                five: 0.2,
                fifteen: 0.3,
            },
            memory: MemoryUsage {
                total_bytes: 1024,
                used_bytes: 512,
                available_bytes: 256,
                swap_total_bytes: 128,
                swap_used_bytes: 64,
            },
            uptime_secs: 60,
            disks: Vec::new(),
            network: NetworkCounters {
                total_rx_bytes: 100,
                total_tx_bytes: 200,
                rx_bytes_per_sec: Some(5.0),
                tx_bytes_per_sec: Some(7.0),
            },
        }
    }
}
