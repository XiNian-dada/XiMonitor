//! 服务端共享状态:在内存中维护节点身份、最新快照与会话生命周期。
//!
//! [`SharedState`] 通过 `Arc<RwLock<Registry>>` 在多个异步任务之间共享:
//!   - WebSocket 处理任务写入最新快照、延迟值和在线状态;
//!   - HTTP API 任务读取整体视图;
//!   - 后台任务定期把超时节点标记为离线、把状态持久化到磁盘。
//!
//! 设计要点:用单调递增的 `session_id` 区分同一节点的多次连接,避免"旧会话"
//! 的延迟数据覆盖"新会话"的最新数据。

mod overview;
mod registry;
mod session_control;
mod sqlite_wal;
mod view_cache;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::body::Bytes;
use chrono::Utc;
use nodelite_proto::{NodeIdentity, NodeSnapshot, NodeStatus, OverviewData, ServerConfig};
use tokio::sync::{Mutex, RwLock, oneshot};

use self::registry::Registry;
pub(crate) use self::session_control::{
    SessionCommand, SessionCommandError, SessionControlHandle, SessionRefreshReply,
};
use self::sqlite_wal::SqliteWalCheckpointObserver;
use self::view_cache::{JsonViewSlot, MetricsViewSlot, ReadinessSnapshot};

#[derive(Debug, Clone, Copy)]
enum ApiBodyKind {
    Nodes,
    Overview,
}
use crate::ServerReadiness;
use crate::handlers::metrics_exporter::{
    ApiCacheMetrics, SqliteWalCheckpointMetrics, WsMessageMetrics, render_prometheus_metrics,
};

/// 共享状态的对外句柄,可以低成本地克隆给每个异步任务。
#[derive(Clone)]
pub struct SharedState {
    config: Arc<ServerConfig>,
    registry: Arc<RwLock<Registry>>,
    next_session_id: Arc<AtomicU64>,
    overview_revision: Arc<AtomicU64>,
    nodes_revision: Arc<AtomicU64>,
    metrics_revision: Arc<AtomicU64>,
    overview_cache: Arc<Mutex<JsonViewSlot>>,
    nodes_cache: Arc<Mutex<JsonViewSlot>>,
    metrics_cache: Arc<Mutex<MetricsViewSlot>>,
    api_nodes_cache_build_lock: Arc<Mutex<()>>,
    api_overview_cache_build_lock: Arc<Mutex<()>>,
    metrics_cache_build_lock: Arc<Mutex<()>>,
    sqlite_wal_checkpoint: SqliteWalCheckpointObserver,
    api_nodes_cache_hits: Arc<AtomicU64>,
    api_nodes_cache_misses: Arc<AtomicU64>,
    api_nodes_body_bytes: Arc<AtomicU64>,
    api_overview_cache_hits: Arc<AtomicU64>,
    api_overview_cache_misses: Arc<AtomicU64>,
    api_overview_body_bytes: Arc<AtomicU64>,
    metrics_cache_hits: Arc<AtomicU64>,
    metrics_cache_misses: Arc<AtomicU64>,
    metrics_body_bytes: Arc<AtomicU64>,
    ws_messages_metrics_total: Arc<AtomicU64>,
    ws_messages_agent_logs_total: Arc<AtomicU64>,
    ws_messages_pong_total: Arc<AtomicU64>,
    ws_messages_refresh_token_request_total: Arc<AtomicU64>,
    session_control_queue_full_total: Arc<AtomicU64>,
    #[cfg(test)]
    metrics_cache_builds: Arc<AtomicU64>,
}

impl SharedState {
    pub fn new(config: Arc<ServerConfig>) -> Self {
        Self {
            config: Arc::clone(&config),
            registry: Arc::new(RwLock::new(Registry::default())),
            next_session_id: Arc::new(AtomicU64::new(1)),
            overview_revision: Arc::new(AtomicU64::new(1)),
            nodes_revision: Arc::new(AtomicU64::new(1)),
            metrics_revision: Arc::new(AtomicU64::new(1)),
            overview_cache: Arc::new(Mutex::new(JsonViewSlot::default())),
            nodes_cache: Arc::new(Mutex::new(JsonViewSlot::default())),
            metrics_cache: Arc::new(Mutex::new(MetricsViewSlot::default())),
            api_nodes_cache_build_lock: Arc::new(Mutex::new(())),
            api_overview_cache_build_lock: Arc::new(Mutex::new(())),
            metrics_cache_build_lock: Arc::new(Mutex::new(())),
            sqlite_wal_checkpoint: SqliteWalCheckpointObserver::new(Arc::clone(&config)),
            api_nodes_cache_hits: Arc::new(AtomicU64::new(0)),
            api_nodes_cache_misses: Arc::new(AtomicU64::new(0)),
            api_nodes_body_bytes: Arc::new(AtomicU64::new(0)),
            api_overview_cache_hits: Arc::new(AtomicU64::new(0)),
            api_overview_cache_misses: Arc::new(AtomicU64::new(0)),
            api_overview_body_bytes: Arc::new(AtomicU64::new(0)),
            metrics_cache_hits: Arc::new(AtomicU64::new(0)),
            metrics_cache_misses: Arc::new(AtomicU64::new(0)),
            metrics_body_bytes: Arc::new(AtomicU64::new(0)),
            ws_messages_metrics_total: Arc::new(AtomicU64::new(0)),
            ws_messages_agent_logs_total: Arc::new(AtomicU64::new(0)),
            ws_messages_pong_total: Arc::new(AtomicU64::new(0)),
            ws_messages_refresh_token_request_total: Arc::new(AtomicU64::new(0)),
            session_control_queue_full_total: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            metrics_cache_builds: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn config(&self) -> &ServerConfig {
        self.config.as_ref()
    }

    pub(crate) fn nodes_revision(&self) -> u64 {
        self.nodes_revision.load(Ordering::Acquire)
    }

    /// 登记一个新的 WebSocket 会话并返回唯一的 `session_id`。
    /// 同一节点重连时会得到比上次更大的 ID,从而抢占老的会话。
    pub async fn register_node(&self, identity: NodeIdentity, remote_ip: Option<String>) -> u64 {
        let session_id = self.next_session_id.fetch_add(1, Ordering::Relaxed);
        let now = Utc::now();
        let mut registry = self.registry.write().await;
        registry.register_node(session_id, identity, remote_ip, now);
        self.bump_view_revision();
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
        let status = registry.update_snapshot(node_id, session_id, snapshot, Utc::now());
        if status.is_some() {
            self.bump_view_revision();
        }
        status
    }

    /// 更新某节点的最新延迟值,语义同 `update_snapshot`。
    pub async fn update_latency(&self, node_id: &str, session_id: u64, latency_ms: u64) -> bool {
        let mut registry = self.registry.write().await;
        let updated = registry.update_latency(node_id, session_id, latency_ms, Utc::now());
        if updated {
            self.bump_view_revision();
        }
        updated
    }

    /// 标记某会话的连接已断开。如果当前活跃 ID 不再等于该会话,则什么也不做。
    pub async fn mark_disconnected(&self, node_id: &str, session_id: u64) {
        let mut registry = self.registry.write().await;
        if registry.mark_disconnected(node_id, session_id) {
            self.bump_view_revision();
        }
    }

    /// 把在线会话的控制通道挂到节点上,供 HTTP 处理器向该节点下发命令。
    pub async fn attach_session_control(
        &self,
        node_id: &str,
        session_id: u64,
        control: SessionControlHandle,
    ) -> bool {
        let mut registry = self.registry.write().await;
        registry.attach_session_control(node_id, session_id, control)
    }

    /// 把超时(超过 `stale_after_secs`)的节点统一标记为离线,返回受影响节点数。
    pub async fn mark_stale(&self) -> usize {
        let mut registry = self.registry.write().await;
        let marked = registry.mark_stale(
            Duration::from_secs(self.config.stale_after_secs),
            Utc::now(),
        );
        if marked > 0 {
            self.bump_view_revision();
        }
        marked
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
        if let Err(error) = control_tx.try_enqueue_refresh(response_tx) {
            if matches!(error, SessionCommandError::QueueFull) {
                self.session_control_queue_full_total
                    .fetch_add(1, Ordering::Relaxed);
            }
            return Err(error);
        }
        Ok(response_rx)
    }

    /// 控制命令队列满时拒绝入队的总次数,用于暴露到 `/metrics`。
    pub fn session_control_queue_full_total(&self) -> u64 {
        self.session_control_queue_full_total
            .load(Ordering::Relaxed)
    }

    /// 返回缓存后的 `/api/overview` 响应体。只要对外节点视图没有变化,
    /// 高频轮询就直接复用上一份序列化结果。
    pub async fn overview_json_bytes(&self) -> Result<Bytes, serde_json::Error> {
        self.cached_api_json_bytes(ApiBodyKind::Overview).await
    }

    /// 返回缓存后的 `/api/nodes` 响应体。命中缓存时跳过整表克隆和重复序列化。
    pub async fn nodes_json_bytes(&self) -> Result<Bytes, serde_json::Error> {
        self.cached_api_json_bytes(ApiBodyKind::Nodes).await
    }

    pub(crate) fn api_cache_metrics(&self) -> ApiCacheMetrics {
        ApiCacheMetrics {
            nodes_hits: self.api_nodes_cache_hits.load(Ordering::Relaxed),
            nodes_misses: self.api_nodes_cache_misses.load(Ordering::Relaxed),
            nodes_body_bytes: self.api_nodes_body_bytes.load(Ordering::Relaxed),
            overview_hits: self.api_overview_cache_hits.load(Ordering::Relaxed),
            overview_misses: self.api_overview_cache_misses.load(Ordering::Relaxed),
            overview_body_bytes: self.api_overview_body_bytes.load(Ordering::Relaxed),
            metrics_hits: self.metrics_cache_hits.load(Ordering::Relaxed),
            metrics_misses: self.metrics_cache_misses.load(Ordering::Relaxed),
            metrics_body_bytes: self.metrics_body_bytes.load(Ordering::Relaxed),
        }
    }

    pub(crate) fn record_ws_metrics_message(&self) {
        self.ws_messages_metrics_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_ws_agent_logs_message(&self) {
        self.ws_messages_agent_logs_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_ws_pong_message(&self) {
        self.ws_messages_pong_total.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_ws_refresh_token_request_message(&self) {
        self.ws_messages_refresh_token_request_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn ws_message_metrics(&self) -> WsMessageMetrics {
        WsMessageMetrics {
            metrics_total: self.ws_messages_metrics_total.load(Ordering::Relaxed),
            agent_logs_total: self.ws_messages_agent_logs_total.load(Ordering::Relaxed),
            pong_total: self.ws_messages_pong_total.load(Ordering::Relaxed),
            refresh_token_request_total: self
                .ws_messages_refresh_token_request_total
                .load(Ordering::Relaxed),
        }
    }

    pub(crate) async fn sqlite_wal_checkpoint_metrics(&self) -> SqliteWalCheckpointMetrics {
        self.sqlite_wal_checkpoint.metrics().await
    }

    pub(crate) async fn registry_disk_entries_total(&self) -> u64 {
        let registry = self.registry.read().await;
        registry.disk_entries_total()
    }

    /// 返回缓存后的 `/metrics` 响应体。
    /// 缓存键由节点视图 revision、服务 readiness 摘要与最大存活时间共同决定。
    pub async fn metrics_text(&self, readiness: &ServerReadiness) -> Bytes {
        let revision = self.metrics_revision.load(Ordering::Acquire);
        let readiness_snapshot = ReadinessSnapshot::capture(readiness);
        let max_age = Duration::from_secs(self.config.refresh_interval_secs.max(1));

        {
            let cache = self.metrics_cache.lock().await;
            if let Some(body) = cache.get(revision, readiness_snapshot, max_age) {
                self.record_metrics_cache_hit();
                return body;
            }
        }

        let _build_guard = self.metrics_cache_build_lock.lock().await;
        let revision = self.metrics_revision.load(Ordering::Acquire);
        let readiness_snapshot = ReadinessSnapshot::capture(readiness);
        {
            let cache = self.metrics_cache.lock().await;
            if let Some(body) = cache.get(revision, readiness_snapshot, max_age) {
                self.record_metrics_cache_hit();
                return body;
            }
        }

        self.record_metrics_cache_miss();

        #[cfg(test)]
        self.metrics_cache_builds.fetch_add(1, Ordering::Relaxed);

        let (statuses, overview) = self.statuses_and_overview().await;
        let body = Bytes::from(render_prometheus_metrics(readiness, &statuses, &overview));
        self.metrics_body_bytes
            .store(body.len() as u64, Ordering::Relaxed);

        if self.metrics_revision.load(Ordering::Acquire) == revision {
            let mut cache = self.metrics_cache.lock().await;
            cache.store(revision, readiness_snapshot, body.clone());
        }

        body
    }

    /// 返回当前对外视图需要的节点状态与聚合概览。
    pub async fn statuses_and_overview(&self) -> (Vec<NodeStatus>, OverviewData) {
        let registry = self.registry.read().await;
        let statuses = registry.list_statuses();
        let overview = registry.overview_from_statuses(&statuses);
        (statuses, overview)
    }

    async fn overview_data(&self) -> OverviewData {
        let registry = self.registry.read().await;
        registry.overview()
    }

    /// 启动时从磁盘快照恢复状态,所有节点都视为离线直至首次心跳到达。
    pub async fn restore_statuses(&self, statuses: Vec<NodeStatus>) {
        let mut registry = self.registry.write().await;
        registry.restore_statuses(statuses);
        self.bump_view_revision();
    }

    /// 当前阶段:所有 mutator 都把三个视图 revision 一起 bump,
    /// 保持与单一 revision 的等价语义。后续 commit 会拆分,让 snapshot/latency
    /// 之类的更新只触达真正受影响的视图。
    fn bump_view_revision(&self) {
        self.overview_revision.fetch_add(1, Ordering::AcqRel);
        self.nodes_revision.fetch_add(1, Ordering::AcqRel);
        self.metrics_revision.fetch_add(1, Ordering::AcqRel);
    }

    fn json_slot_for(&self, kind: ApiBodyKind) -> &Arc<Mutex<JsonViewSlot>> {
        match kind {
            ApiBodyKind::Nodes => &self.nodes_cache,
            ApiBodyKind::Overview => &self.overview_cache,
        }
    }

    fn revision_atomic_for(&self, kind: ApiBodyKind) -> &AtomicU64 {
        match kind {
            ApiBodyKind::Nodes => &self.nodes_revision,
            ApiBodyKind::Overview => &self.overview_revision,
        }
    }

    async fn cached_api_json_bytes(&self, kind: ApiBodyKind) -> Result<Bytes, serde_json::Error> {
        let slot = self.json_slot_for(kind);
        let revision_atomic = self.revision_atomic_for(kind);
        let revision = revision_atomic.load(Ordering::Acquire);
        {
            let cache = slot.lock().await;
            if let Some(body) = cache.get(revision) {
                self.record_api_cache_hit(kind);
                return Ok(body);
            }
        }

        let build_lock = match kind {
            ApiBodyKind::Nodes => &self.api_nodes_cache_build_lock,
            ApiBodyKind::Overview => &self.api_overview_cache_build_lock,
        };
        let _build_guard = build_lock.lock().await;
        let revision = revision_atomic.load(Ordering::Acquire);
        {
            let cache = slot.lock().await;
            if let Some(body) = cache.get(revision) {
                self.record_api_cache_hit(kind);
                return Ok(body);
            }
        }

        self.record_api_cache_miss(kind);

        let body = match kind {
            ApiBodyKind::Nodes => {
                let statuses = self.list_statuses().await;
                Bytes::from(serde_json::to_vec(&statuses)?)
            }
            ApiBodyKind::Overview => {
                let overview = self.overview_data().await;
                Bytes::from(serde_json::to_vec(&overview)?)
            }
        };
        self.record_api_body_bytes(kind, body.len());

        if revision_atomic.load(Ordering::Acquire) == revision {
            let mut cache = slot.lock().await;
            cache.store(revision, body.clone());
        }

        Ok(body)
    }

    fn record_api_cache_hit(&self, kind: ApiBodyKind) {
        match kind {
            ApiBodyKind::Nodes => {
                self.api_nodes_cache_hits.fetch_add(1, Ordering::Relaxed);
            }
            ApiBodyKind::Overview => {
                self.api_overview_cache_hits.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn record_api_cache_miss(&self, kind: ApiBodyKind) {
        match kind {
            ApiBodyKind::Nodes => {
                self.api_nodes_cache_misses.fetch_add(1, Ordering::Relaxed);
            }
            ApiBodyKind::Overview => {
                self.api_overview_cache_misses
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn record_api_body_bytes(&self, kind: ApiBodyKind, bytes: usize) {
        let bytes = bytes as u64;
        match kind {
            ApiBodyKind::Nodes => self.api_nodes_body_bytes.store(bytes, Ordering::Relaxed),
            ApiBodyKind::Overview => self.api_overview_body_bytes.store(bytes, Ordering::Relaxed),
        }
    }

    fn record_metrics_cache_hit(&self) {
        self.metrics_cache_hits.fetch_add(1, Ordering::Relaxed);
    }

    fn record_metrics_cache_miss(&self) {
        self.metrics_cache_misses.fetch_add(1, Ordering::Relaxed);
    }

    #[cfg(test)]
    fn api_cache_build_count(&self) -> u64 {
        self.api_nodes_cache_build_count() + self.api_overview_cache_build_count()
    }

    #[cfg(test)]
    fn api_nodes_cache_build_count(&self) -> u64 {
        self.api_nodes_cache_misses.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    fn api_overview_cache_build_count(&self) -> u64 {
        self.api_overview_cache_misses.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    fn metrics_cache_build_count(&self) -> u64 {
        self.metrics_cache_builds.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    use chrono::{Duration as ChronoDuration, TimeZone, Utc};
    use nodelite_proto::{
        LoadAverage, MemoryUsage, NodeSnapshot, ReadonlyAuthConfig, ServerConfig, WsConfig,
    };
    use nodelite_proto::{NetworkCounters, percentage};

    use super::{Registry, SessionControlHandle, SharedState};
    use nodelite_proto::NodeIdentity;

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

        let (control, _control_rx) = SessionControlHandle::channel();
        assert!(registry.attach_session_control("hk-01", 7, control));
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

        let (control, _control_rx) = SessionControlHandle::channel();
        assert!(registry.attach_session_control("hk-01", 9, control));
        registry.mark_disconnected("hk-01", 9);

        assert!(registry.session_control("hk-01").is_none());
    }

    #[tokio::test]
    async fn cached_api_json_invalidates_after_visible_status_change() {
        let shared = SharedState::new(Arc::new(sample_config()));
        let session_id = shared
            .register_node(sample_identity(), Some("198.51.100.10".to_string()))
            .await;

        let first_nodes = shared.nodes_json_bytes().await.expect("nodes json");
        let first_overview = shared.overview_json_bytes().await.expect("overview json");
        assert_eq!(shared.api_nodes_cache_build_count(), 1);
        assert_eq!(shared.api_overview_cache_build_count(), 1);

        shared.mark_disconnected("hk-01", session_id).await;

        let second_overview = shared
            .overview_json_bytes()
            .await
            .expect("overview json after disconnect");
        assert_eq!(shared.api_nodes_cache_build_count(), 1);
        assert_eq!(shared.api_overview_cache_build_count(), 2);

        let second_nodes = shared
            .nodes_json_bytes()
            .await
            .expect("nodes json after disconnect");
        assert_eq!(shared.api_nodes_cache_build_count(), 2);
        assert_eq!(shared.api_overview_cache_build_count(), 2);

        assert_ne!(first_nodes, second_nodes);
        assert_ne!(first_overview, second_overview);
        assert!(
            std::str::from_utf8(&second_nodes)
                .expect("utf8")
                .contains("\"online\":false")
        );
    }

    #[tokio::test]
    async fn concurrent_api_cache_miss_serializes_once() {
        let shared = SharedState::new(Arc::new(sample_config()));
        shared
            .register_node(sample_identity(), Some("198.51.100.10".to_string()))
            .await;

        let mut tasks = Vec::new();
        for _ in 0..10 {
            let shared = shared.clone();
            tasks.push(tokio::spawn(async move {
                shared.nodes_json_bytes().await.expect("nodes json")
            }));
        }

        let mut first = None;
        for task in tasks {
            let body = task.await.expect("task join");
            if let Some(previous) = first.as_ref() {
                assert_eq!(previous, &body);
            } else {
                first = Some(body);
            }
        }

        assert_eq!(shared.api_cache_build_count(), 1);
    }

    #[tokio::test]
    async fn api_overview_and_nodes_caches_build_independently() {
        let shared = SharedState::new(Arc::new(sample_config()));
        shared
            .register_node(sample_identity(), Some("198.51.100.10".to_string()))
            .await;

        let first_overview = shared.overview_json_bytes().await.expect("overview json");
        assert_eq!(shared.api_overview_cache_build_count(), 1);
        assert_eq!(
            shared.api_nodes_cache_build_count(),
            0,
            "overview miss must not serialize or populate the nodes body",
        );

        let cached_overview = shared.overview_json_bytes().await.expect("overview json");
        assert_eq!(first_overview, cached_overview);
        assert_eq!(shared.api_overview_cache_build_count(), 1);
        assert_eq!(shared.api_nodes_cache_build_count(), 0);
        let metrics = shared.api_cache_metrics();
        assert_eq!(metrics.overview_hits, 1);
        assert_eq!(metrics.overview_misses, 1);
        assert!(metrics.overview_body_bytes > 0);
        assert_eq!(metrics.nodes_hits, 0);
        assert_eq!(metrics.nodes_misses, 0);
        assert_eq!(metrics.nodes_body_bytes, 0);

        let first_nodes = shared.nodes_json_bytes().await.expect("nodes json");
        assert_eq!(shared.api_overview_cache_build_count(), 1);
        assert_eq!(shared.api_nodes_cache_build_count(), 1);

        let cached_nodes = shared.nodes_json_bytes().await.expect("nodes json");
        assert_eq!(first_nodes, cached_nodes);
        assert_eq!(shared.api_overview_cache_build_count(), 1);
        assert_eq!(shared.api_nodes_cache_build_count(), 1);
        let metrics = shared.api_cache_metrics();
        assert_eq!(metrics.overview_hits, 1);
        assert_eq!(metrics.overview_misses, 1);
        assert_eq!(metrics.nodes_hits, 1);
        assert_eq!(metrics.nodes_misses, 1);
        assert!(metrics.nodes_body_bytes > metrics.overview_body_bytes);
    }

    #[tokio::test]
    async fn registry_disk_entries_total_counts_snapshot_disks() {
        let shared = SharedState::new(Arc::new(sample_config()));
        let first_session = shared
            .register_node(sample_identity(), Some("198.51.100.10".to_string()))
            .await;
        let second_session = shared
            .register_node(
                NodeIdentity {
                    node_id: "sg-01".to_string(),
                    node_label: "Singapore 01".to_string(),
                    ..sample_identity()
                },
                Some("198.51.100.11".to_string()),
            )
            .await;

        let mut first = sample_snapshot(Utc::now());
        first.disks.resize_with(2, sample_disk_usage);
        let mut second = sample_snapshot(Utc::now());
        second.disks.resize_with(3, sample_disk_usage);

        assert!(
            shared
                .update_snapshot("hk-01", first_session, first)
                .await
                .is_some()
        );
        assert!(
            shared
                .update_snapshot("sg-01", second_session, second)
                .await
                .is_some()
        );

        assert_eq!(shared.registry_disk_entries_total().await, 5);
    }

    #[tokio::test]
    async fn metrics_cache_reuses_and_invalidates_cleanly() {
        let shared = SharedState::new(Arc::new(sample_config()));
        let readiness = crate::ServerReadiness::new(true);
        let session_id = shared
            .register_node(sample_identity(), Some("198.51.100.10".to_string()))
            .await;
        assert!(
            shared
                .update_snapshot("hk-01", session_id, sample_snapshot(Utc::now()))
                .await
                .is_some()
        );

        let mut tasks = Vec::new();
        for _ in 0..10 {
            let shared = shared.clone();
            let readiness = readiness.clone();
            tasks.push(tokio::spawn(async move {
                shared.metrics_text(&readiness).await
            }));
        }

        let mut first = None;
        for task in tasks {
            let body = task.await.expect("task join");
            if let Some(previous) = first.as_ref() {
                assert_eq!(previous, &body);
            } else {
                first = Some(body);
            }
        }
        assert_eq!(shared.metrics_cache_build_count(), 1);

        let cached = shared.metrics_text(&readiness).await;
        assert_eq!(shared.metrics_cache_build_count(), 1);
        assert_eq!(first.expect("first metrics body"), cached);

        shared.mark_disconnected("hk-01", session_id).await;
        let after_disconnect = shared.metrics_text(&readiness).await;
        assert_eq!(shared.metrics_cache_build_count(), 2);
        assert_ne!(cached, after_disconnect);

        readiness.mark_history_available(false);
        let after_readiness = shared.metrics_text(&readiness).await;
        assert_eq!(shared.metrics_cache_build_count(), 3);
        assert_ne!(after_disconnect, after_readiness);
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

    fn sample_config() -> ServerConfig {
        ServerConfig {
            listen: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 8080)),
            public_base_url: "http://127.0.0.1:8080".to_string(),
            insecure_allow_http: false,
            trusted_proxies: Vec::new(),
            readonly_auth: Some(ReadonlyAuthConfig {
                username: "viewer".to_string(),
                password: "secret".to_string(),
                enable_2fa: false,
                totp_secret: None,
            }),
            ws: WsConfig {
                max_total_connections: 128,
                max_connections_per_ip: 64,
                auth_fail_window_secs: 300,
                auth_fail_max_attempts: 12,
                auth_block_secs: 900,
            },
            audit: nodelite_proto::AuditConfig {
                enabled: true,
                db_path: PathBuf::from("/tmp/nodelite-test-audit.sqlite3"),
                retention_days: 90,
                log_successful_auth: true,
                log_failed_auth: true,
                log_token_events: true,
                log_rate_limit: true,
            },
            node_registry_path: PathBuf::from("/tmp/nodelite-test-registry.json"),
            history_db_path: PathBuf::from("/tmp/nodelite-test-history.sqlite3"),
            snapshot_path: PathBuf::from("/tmp/nodelite-test-snapshot.json"),
            stale_after_secs: 5,
            ping_interval_secs: 60,
            max_message_bytes: 64 * 1024,
            refresh_interval_secs: 5,
            ignored_filesystems: vec!["tmpfs".to_string(), "devtmpfs".to_string()],
            agent_release_base_url: None,
            agent_release_sha256_x86_64: None,
            agent_release_sha256_aarch64: None,
            hello_timeout_secs: 10,
            max_outstanding_pings: 32,
            insecure_transport_warn_interval_secs: 900,
            max_sanitized_disks: 64,
            max_sanitized_string_bytes: 256,
            metric_anomaly_session_limit: 5,
            sqlite_busy_timeout_secs: 5,
        }
    }

    fn sample_snapshot(now: chrono::DateTime<Utc>) -> NodeSnapshot {
        NodeSnapshot {
            collected_at: now,
            cpu_usage_percent: Some(percentage(1, 2)),
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

    fn sample_disk_usage() -> nodelite_proto::DiskUsage {
        nodelite_proto::DiskUsage {
            device: "/dev/vda1".to_string(),
            mount_point: "/".to_string(),
            fs_type: "ext4".to_string(),
            total_bytes: 1024,
            available_bytes: 512,
            used_bytes: 512,
            used_percent: 50.0,
        }
    }
}
