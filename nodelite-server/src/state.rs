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
use nodelite_proto::{
    NodeIdentity, NodeListItem, NodeSnapshot, NodeStatus, OverviewData, ServerConfig,
};
use tokio::sync::{Mutex, RwLock, broadcast, oneshot};

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

/// Overview 聚合允许的最大"陈旧时间":即使 overview revision 没有递增,
/// 缓存超过这个时长后也会强制重建。配合后续 commit 把 snapshot/latency
/// 的 revision 影响范围收窄到 nodes,避免聚合数据无限期不刷新。
const OVERVIEW_CACHE_MAX_STALE: Duration = Duration::from_secs(1);
use crate::ServerReadiness;
use crate::handlers::metrics_exporter::{
    ApiCacheMetrics, SqliteWalCheckpointMetrics, WsMessageMetrics,
};

/// 浏览器视图脏信号。节点视图发生任意变化(注册 / 快照 / 延迟 / 离线 / 批量过期)时
/// 广播一次,促使每个浏览器 WebSocket 会话重算节点列表、与上次发送的快照做 diff,
/// 再发出增量。无 payload:会话总是重新读取完整视图,不需要携带变化详情。
#[derive(Debug, Clone, Copy)]
pub(crate) struct BrowserViewDirty;

/// 浏览器脏信号广播通道容量。200 节点 × 1Hz ≈ 200 信号/秒;某个会话若停顿超过
/// 约 1.3 秒(256 / 200)就会 Lagged,届时会话回退到重发 InitialState 全量同步。
const BROWSER_VIEW_DIRTY_CHANNEL_CAPACITY: usize = 256;

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
    /// 节点视图变化时向所有浏览器 WebSocket 会话广播脏信号。
    browser_view_dirty_tx: broadcast::Sender<BrowserViewDirty>,
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
            browser_view_dirty_tx: broadcast::channel(BROWSER_VIEW_DIRTY_CHANNEL_CAPACITY).0,
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
    ///
    /// 注意:只 bump nodes_revision。overview / metrics 视图通过各自的 TTL
    /// 容忍最多几秒的聚合数据滞后,以便高频上报场景下不再连带使三视图缓存
    /// 同时失效。
    pub async fn update_snapshot(
        &self,
        node_id: &str,
        session_id: u64,
        snapshot: NodeSnapshot,
    ) -> Option<NodeStatus> {
        let mut registry = self.registry.write().await;
        let status = registry.update_snapshot(node_id, session_id, snapshot, Utc::now());
        if status.is_some() {
            self.bump_nodes_revision_only();
        }
        status
    }

    /// 更新某节点的最新延迟值,语义同 `update_snapshot`(只 bump nodes_revision)。
    pub async fn update_latency(&self, node_id: &str, session_id: u64, latency_ms: u64) -> bool {
        let mut registry = self.registry.write().await;
        let updated = registry.update_latency(node_id, session_id, latency_ms, Utc::now());
        if updated {
            self.bump_nodes_revision_only();
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

    pub async fn list_node_summaries(&self) -> Vec<NodeListItem> {
        let registry = self.registry.read().await;
        registry.list_node_summaries()
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

        let body = {
            let registry = self.registry.read().await;
            Bytes::from(registry.render_metrics_body(readiness))
        };
        self.metrics_body_bytes
            .store(body.len() as u64, Ordering::Relaxed);

        if self.metrics_revision.load(Ordering::Acquire) == revision {
            let mut cache = self.metrics_cache.lock().await;
            cache.store(revision, readiness_snapshot, body.clone());
        }

        body
    }

    async fn overview_data(&self) -> OverviewData {
        let registry = self.registry.read().await;
        registry.overview()
    }

    /// 供浏览器 WebSocket 会话读取当前概览聚合(在准备发送 OverviewUpdate 时惰性调用)。
    pub(crate) async fn overview_snapshot(&self) -> OverviewData {
        self.overview_data().await
    }

    /// 订阅浏览器视图脏信号。每个浏览器会话持有一个 receiver,在信号到达后
    /// 重算节点列表并发出增量。
    pub(crate) fn subscribe_browser_updates(&self) -> broadcast::Receiver<BrowserViewDirty> {
        self.browser_view_dirty_tx.subscribe()
    }

    /// 广播一次浏览器视图脏信号。没有订阅者(无浏览器连接)时 `send` 返回
    /// `Err`,直接忽略即可。
    fn notify_browser_view_dirty(&self) {
        let _ = self.browser_view_dirty_tx.send(BrowserViewDirty);
    }

    /// 启动时从磁盘快照恢复状态,所有节点都视为离线直至首次心跳到达。
    pub async fn restore_statuses(&self, statuses: Vec<NodeStatus>) {
        let mut registry = self.registry.write().await;
        registry.restore_statuses(statuses);
        self.bump_view_revision();
    }

    /// 结构性变更(注册、断连、批量恢复)同时使三视图缓存失效。
    fn bump_view_revision(&self) {
        self.overview_revision.fetch_add(1, Ordering::AcqRel);
        self.nodes_revision.fetch_add(1, Ordering::AcqRel);
        self.metrics_revision.fetch_add(1, Ordering::AcqRel);
        self.notify_browser_view_dirty();
    }

    /// 单节点快照 / 延迟更新只让 nodes 视图立刻失效。
    ///
    /// overview 与 metrics 通过 TTL 容忍短期陈旧,从而避免高频 push 把三视图
    /// 缓存连带打穿(见 issue #160)。
    fn bump_nodes_revision_only(&self) {
        self.nodes_revision.fetch_add(1, Ordering::AcqRel);
        self.notify_browser_view_dirty();
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

    fn max_age_for(&self, kind: ApiBodyKind) -> Option<Duration> {
        match kind {
            ApiBodyKind::Nodes => None,
            ApiBodyKind::Overview => Some(OVERVIEW_CACHE_MAX_STALE),
        }
    }

    async fn cached_api_json_bytes(&self, kind: ApiBodyKind) -> Result<Bytes, serde_json::Error> {
        let slot = self.json_slot_for(kind);
        let revision_atomic = self.revision_atomic_for(kind);
        let max_age = self.max_age_for(kind);
        let revision = revision_atomic.load(Ordering::Acquire);
        {
            let cache = slot.lock().await;
            if let Some(body) = cache.get(revision, max_age) {
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
            if let Some(body) = cache.get(revision, max_age) {
                self.record_api_cache_hit(kind);
                return Ok(body);
            }
        }

        self.record_api_cache_miss(kind);

        let body = match kind {
            ApiBodyKind::Nodes => {
                let summaries = self.list_node_summaries().await;
                Bytes::from(serde_json::to_vec(&summaries)?)
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
mod tests;
