//! 历史数据存储:把 `NodeStatus` 中的关键指标写入本地 SQLite 表,
//! 供前端绘制趋势图。设计目标:
//!
//! - 不阻塞实时 WebSocket 流程:`record_status` 只做"节流检查 + push 到 channel",
//!   真正的 SQLite I/O 在独立的 writer task 中以 batch transaction 形式执行;
//! - 节流:同一节点两次写入至少间隔 `DEFAULT_HISTORY_WRITE_INTERVAL_SECS` 秒;
//! - 自清理:每 5 分钟最多触发一次 `DELETE`,把超过保留期的旧记录删除;
//! - 自降级:数据库初始化失败时不阻断服务,而是把 `available=false`,实时视图照常运行;
//! - 背压:bounded channel + try_send,溢出时记日志并自增 dropped 计数,
//!   而不是反压实时心跳路径(实时数据比历史趋势更重要)。
//! - 对外暴露的查询入口统一返回 [`HistoryError`],让 handler 可以按类别区分
//!   "任务调度失败" / "连接未初始化" / "查询本身失败",而不是匹配错误字符串。

mod init;
mod query;
mod writer;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, anyhow};
use chrono::{DateTime, Utc};
use nodelite_proto::{
    DEFAULT_HISTORY_RETENTION_HOURS, DEFAULT_HISTORY_WRITE_INTERVAL_SECS, HistoryPoint, NodeStatus,
};
use rusqlite::Connection;
use tokio::sync::{Mutex, RwLock, mpsc};
use tokio::task::JoinHandle;
use tracing::{error, warn};

use self::init::initialize_database;
#[cfg(test)]
use self::query::HISTORY_QUERY_SQL;
use self::query::{query_history, query_history_between};
use self::writer::{WriterContext, build_history_point, run_history_writer};
#[cfg(test)]
use self::writer::{sqlite_busy_retry_delay, write_history_point};

/// SQLite 在并发写入冲突时的等待时长。
const SQLITE_BUSY_MAX_RETRIES: u32 = 10;
const SQLITE_BUSY_RETRY_BASE_MS: u64 = 50;
const SQLITE_BUSY_RETRY_MAX_MS: u64 = 1_000;

/// Writer 任务的 channel 容量。1000 节点 × 1Hz 上报理论峰值是 1000 msg/s,
/// 在 100ms flush 间隔下,稳态 backlog ≤ ~100。1024 留出 10x 余量
/// (突发重连 / Burst 上报),仍然不至于让 record_status 真的被反压。
const HISTORY_CHANNEL_CAPACITY: usize = 1024;
/// 一次事务里最多打包多少条 INSERT。给上限是为了在极端 burst 下
/// 防止单个 fsync 周期被一直推迟。
const HISTORY_BATCH_MAX: usize = 128;
/// 没有新写入到达时,writer 也每隔这段时间 flush 一次当前 batch,
/// 保证最后几条样本不会无限期停留在内存。
const HISTORY_BATCH_FLUSH_INTERVAL: Duration = Duration::from_millis(100);
/// 距上一次 DELETE 至少要过这么长时间才再次触发清理。
const HISTORY_PRUNE_MIN_INTERVAL: Duration = Duration::from_secs(300);

pub type HistoryResult<T> = std::result::Result<T, HistoryError>;

/// 历史查询路径对外暴露的稳定错误边界。
#[derive(Debug)]
pub enum HistoryError {
    ConnectionNotInitialized,
    Query(anyhow::Error),
    TaskFailed(anyhow::Error),
}

impl std::fmt::Display for HistoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConnectionNotInitialized => f.write_str("history connection not initialized"),
            Self::Query(_) => f.write_str("history query failed"),
            Self::TaskFailed(_) => f.write_str("history query task failed"),
        }
    }
}

impl std::error::Error for HistoryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Query(error) | Self::TaskFailed(error) => Some(error.root_cause()),
            Self::ConnectionNotInitialized => None,
        }
    }
}

/// 对外暴露的历史存储句柄,可被低成本克隆给多个异步任务。
#[derive(Clone)]
pub struct HistoryStore {
    db_path: Arc<PathBuf>,
    available: Arc<AtomicBool>,
    /// 持久化 SQLite 连接,只在 query 路径短暂持有;写入由 writer task 独占。
    connection: Arc<Mutex<Option<Connection>>>,
    /// 节点 → 上一次成功节流通过的时间。仅短暂持有 lock(check + 乐观更新),
    /// 不再跨越 spawn_blocking,因此与 SQLite I/O 解耦。
    last_written_at: Arc<Mutex<HashMap<String, DateTime<Utc>>>>,
    /// 最近一次执行清理删除的 Unix 时间戳(0 = 从未)。Writer task 内部更新,
    /// 用 AtomicI64 避免再多一把 mutex。
    last_pruned_at: Arc<AtomicI64>,
    /// WAL/SHM 文件通常在第一次真实写入后才出现,此标志确保后续权限加固只做一次。
    artifacts_hardened_after_write: Arc<AtomicBool>,
    /// SQLite 忙等待超时(秒)。
    sqlite_busy_timeout_secs: u64,
    /// Writer task 的入口。Some 表示已初始化;关停时清空以让 record_status 进入空操作分支。
    writer_tx: Arc<RwLock<Option<mpsc::Sender<HistoryPoint>>>>,
    /// Writer task 的 join handle,用于在关停时显式 await。
    writer_handle: Arc<Mutex<Option<JoinHandle<()>>>>,
    /// 历史写入被静默丢弃的总数(channel 满或已关闭)。监控该值可观察反压。
    dropped_writes: Arc<AtomicU64>,
}

impl HistoryStore {
    pub fn new(db_path: PathBuf, sqlite_busy_timeout_secs: u64) -> Self {
        Self {
            db_path: Arc::new(db_path),
            available: Arc::new(AtomicBool::new(false)),
            connection: Arc::new(Mutex::new(None)),
            last_written_at: Arc::new(Mutex::new(HashMap::new())),
            last_pruned_at: Arc::new(AtomicI64::new(0)),
            artifacts_hardened_after_write: Arc::new(AtomicBool::new(false)),
            sqlite_busy_timeout_secs,
            writer_tx: Arc::new(RwLock::new(None)),
            writer_handle: Arc::new(Mutex::new(None)),
            dropped_writes: Arc::new(AtomicU64::new(0)),
        }
    }

    /// 初始化数据库 + 启动 writer task。失败不会抛出,仅记录警告并保持 `available=false`。
    pub async fn initialize(&self) {
        let db_path = Arc::clone(&self.db_path);
        let sqlite_busy_timeout_secs = self.sqlite_busy_timeout_secs;
        let result = tokio::task::spawn_blocking(move || {
            initialize_database(db_path.as_ref(), sqlite_busy_timeout_secs)
        })
        .await
        .context("history database task failed");

        match result {
            Ok(Ok(connection)) => {
                {
                    let mut guard = self.connection.lock().await;
                    *guard = Some(connection);
                }
                self.available.store(true, Ordering::Relaxed);
                self.spawn_writer_task().await;
            }
            Ok(Err(error)) => {
                error!(
                    error = ?error,
                    "history database unavailable; real-time views will continue"
                );
            }
            Err(error) => {
                error!(error = ?error, "history database initialization join failed");
            }
        }
    }

    /// 启动后台 writer task:消费 channel,做 batch transaction 写入。
    async fn spawn_writer_task(&self) {
        let (tx, rx) = mpsc::channel::<HistoryPoint>(HISTORY_CHANNEL_CAPACITY);
        {
            let mut guard = self.writer_tx.write().await;
            *guard = Some(tx);
        }
        let context = WriterContext {
            db_path: Arc::clone(&self.db_path),
            connection: Arc::clone(&self.connection),
            last_pruned_at: Arc::clone(&self.last_pruned_at),
            artifacts_hardened_after_write: Arc::clone(&self.artifacts_hardened_after_write),
        };
        let handle = tokio::spawn(run_history_writer(rx, context));
        let mut guard = self.writer_handle.lock().await;
        *guard = Some(handle);
    }

    pub fn is_available(&self) -> bool {
        self.available.load(Ordering::Relaxed)
    }

    /// 统计有多少次 record_status 因为 channel 满而被静默丢弃。
    /// 监控接入后,这个计数应当长期保持在 0;持续非零表示 batch 速率跟不上上报速率。
    pub fn dropped_writes(&self) -> u64 {
        self.dropped_writes.load(Ordering::Relaxed)
    }

    /// 尝试把一次节点状态记录到历史表。
    ///
    /// 节流通过 + channel 有空闲槽位时立即返回;否则静默 drop 并自增计数。
    /// 调用方(WebSocket 心跳路径)永远不会因为 SQLite I/O 而被阻塞。
    pub async fn record_status(&self, status: &NodeStatus) {
        self.record_status_with_builder(status, build_history_point)
            .await;
    }

    async fn record_status_with_builder<F>(&self, status: &NodeStatus, build_point: F)
    where
        F: FnOnce(&NodeStatus) -> Option<HistoryPoint>,
    {
        if !self.is_available() {
            return;
        }

        let initial_recorded_at = status.last_seen.unwrap_or_else(Utc::now);
        if self
            .is_throttled(status.identity.node_id.as_str(), initial_recorded_at)
            .await
        {
            return;
        }

        // 把样本推给 writer task。try_send 在 channel 满时立即失败,这里宁可丢一条样本
        // 也不要让 WS 处理路径被反压;丢弃由 dropped_writes 计数提示运维。
        let tx = {
            let guard = self.writer_tx.read().await;
            guard.as_ref().cloned()
        };
        let Some(tx) = tx else {
            return;
        };

        let Some(point) = build_point(status) else {
            return;
        };
        let node_id = point.node_id.clone();
        let recorded_at = point.recorded_at;
        let mut throttle = self.last_written_at.lock().await;
        if let Some(previous) = throttle.get(&node_id) {
            let Ok(elapsed) = recorded_at
                .signed_duration_since(previous.to_owned())
                .to_std()
            else {
                return;
            };
            if elapsed < Duration::from_secs(DEFAULT_HISTORY_WRITE_INTERVAL_SECS) {
                return;
            }
        }

        match tx.try_send(point) {
            Ok(()) => {
                throttle.insert(node_id, recorded_at);
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.dropped_writes.fetch_add(1, Ordering::Relaxed);
                warn!(
                    capacity = HISTORY_CHANNEL_CAPACITY,
                    dropped_total = self.dropped_writes.load(Ordering::Relaxed),
                    "history writer queue full; dropping write"
                );
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                // Writer 已退出 — 标记 store 整体不可用,后续 record_status 会快速 return。
                self.available.store(false, Ordering::Relaxed);
            }
        }
    }

    async fn is_throttled(&self, node_id: &str, recorded_at: DateTime<Utc>) -> bool {
        let throttle = self.last_written_at.lock().await;
        let Some(previous) = throttle.get(node_id) else {
            return false;
        };
        let Ok(elapsed) = recorded_at
            .signed_duration_since(previous.to_owned())
            .to_std()
        else {
            return true;
        };
        elapsed < Duration::from_secs(DEFAULT_HISTORY_WRITE_INTERVAL_SECS)
    }

    /// 关停:drop sender → writer task 自动 drain 残留 batch 后退出,
    /// 然后 await 它结束。在 `run_server` 的 shutdown 阶段调用,
    /// 保证 SIGTERM 时已经被 enqueue 的样本不会随进程退出而丢失。
    pub async fn shutdown(&self) {
        let sender = {
            let mut guard = self.writer_tx.write().await;
            guard.take()
        };
        drop(sender);

        let handle = {
            let mut guard = self.writer_handle.lock().await;
            guard.take()
        };
        if let Some(handle) = handle
            && let Err(error) = handle.await
        {
            warn!(error = ?error, "history writer task join failed during shutdown");
        }
    }

    /// 按"过去 N 小时"窗口查询历史记录。
    pub async fn query_history(
        &self,
        node_id: &str,
        window_hours: u64,
        max_points: usize,
    ) -> HistoryResult<Vec<HistoryPoint>> {
        if !self.is_available() {
            return Ok(Vec::new());
        }

        let connection_arc = Arc::clone(&self.connection);
        let node_id = node_id.to_string();
        let clamped_window_hours = window_hours.clamp(1, DEFAULT_HISTORY_RETENTION_HOURS);
        let clamped_max_points = max_points.max(60);
        let since = Utc::now() - chrono::Duration::hours(clamped_window_hours as i64);

        tokio::task::spawn_blocking(move || {
            let guard = connection_arc.blocking_lock();
            let Some(ref connection) = *guard else {
                return Err(HistoryError::ConnectionNotInitialized);
            };
            query_history(connection, &node_id, since, clamped_max_points)
                .map_err(HistoryError::Query)
        })
        .await
        .map_err(|error| HistoryError::TaskFailed(anyhow!("history query task failed: {error}")))?
    }

    /// 按"任意时间区间"查询历史记录。超出保留期或反向区间会被自动裁剪。
    pub async fn query_history_range(
        &self,
        node_id: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        max_points: usize,
    ) -> HistoryResult<Vec<HistoryPoint>> {
        if !self.is_available() {
            return Ok(Vec::new());
        }

        let now = Utc::now();
        let retention_floor = now - chrono::Duration::hours(DEFAULT_HISTORY_RETENTION_HOURS as i64);
        let clamped_start = start.max(retention_floor);
        let clamped_end = end.min(now);
        if clamped_end <= clamped_start {
            return Ok(Vec::new());
        }

        let connection_arc = Arc::clone(&self.connection);
        let node_id = node_id.to_string();
        let clamped_max_points = max_points.max(60);

        tokio::task::spawn_blocking(move || {
            let guard = connection_arc.blocking_lock();
            let Some(ref connection) = *guard else {
                return Err(HistoryError::ConnectionNotInitialized);
            };
            query_history_between(
                connection,
                &node_id,
                clamped_start,
                clamped_end,
                clamped_max_points,
            )
            .map_err(HistoryError::Query)
        })
        .await
        .map_err(|error| {
            HistoryError::TaskFailed(anyhow!("history range query task failed: {error}"))
        })?
    }

    /// 清理已经不在注册表中的节点节流状态,避免长期运行时条目只增不减。
    pub async fn forget_missing(&self, live_node_ids: &[String]) -> usize {
        let live_node_ids: HashSet<&str> = live_node_ids.iter().map(String::as_str).collect();
        let mut guard = self.last_written_at.lock().await;
        let before = guard.len();
        guard.retain(|node_id, _| live_node_ids.contains(node_id.as_str()));
        before.saturating_sub(guard.len())
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use chrono::{Duration, Utc};
    use nodelite_proto::{
        HistoryPoint, LoadAverage, MemoryUsage, NetworkCounters, NodeIdentity, NodeSnapshot,
        NodeStatus,
    };
    use tokio::runtime::Runtime;

    use super::{
        HISTORY_CHANNEL_CAPACITY, HISTORY_QUERY_SQL, HistoryError, HistoryStore,
        SQLITE_BUSY_MAX_RETRIES, build_history_point, initialize_database, query_history_between,
        sqlite_busy_retry_delay, write_history_point,
    };

    #[test]
    fn history_point_uses_server_last_seen_timestamp() {
        let now = Utc::now();
        let status = NodeStatus {
            identity: NodeIdentity {
                node_id: "hk-01".to_string(),
                node_label: "Hong Kong 01".to_string(),
                hostname: "hk-01.internal".to_string(),
                os: "Ubuntu".to_string(),
                kernel_version: None,
                cpu_model: None,
                cpu_cores: 2,
                agent_version: "0.1.0".to_string(),
                boot_time: None,
                tags: vec!["edge".to_string()],
            },
            remote_ip: Some("198.51.100.24".to_string()),
            snapshot: Some(NodeSnapshot {
                collected_at: now + Duration::hours(24),
                cpu_usage_percent: 42.0,
                load: LoadAverage {
                    one: 0.1,
                    five: 0.2,
                    fifteen: 0.3,
                },
                memory: MemoryUsage {
                    total_bytes: 1024,
                    used_bytes: 512,
                    available_bytes: 512,
                    swap_total_bytes: 0,
                    swap_used_bytes: 0,
                },
                uptime_secs: 60,
                disks: Vec::new(),
                network: NetworkCounters {
                    total_rx_bytes: 1,
                    total_tx_bytes: 2,
                    rx_bytes_per_sec: Some(3.0),
                    tx_bytes_per_sec: Some(4.0),
                },
            }),
            last_seen: Some(now),
            latency_ms: Some(12),
            online: true,
        };

        let point = build_history_point(&status).expect("history point should exist");
        assert_eq!(point.recorded_at, now);
    }

    #[test]
    #[cfg(unix)]
    fn history_database_artifacts_are_mode_600() {
        let runtime = Runtime::new().expect("runtime should build");
        runtime.block_on(async {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should be monotonic enough")
                .as_nanos();
            let temp_dir = std::env::temp_dir().join(format!("nodelite-history-mode-{unique}"));
            std::fs::create_dir_all(&temp_dir).expect("temp dir should exist");
            let data_dir = temp_dir.join("data");
            let db_path = data_dir.join("history.sqlite3");

            let mut connection =
                initialize_database(&db_path, 5).expect("database should initialize");
            write_history_point(
                &db_path,
                &mut connection,
                &HistoryPoint {
                    node_id: "hk-01".to_string(),
                    recorded_at: Utc::now(),
                    cpu_usage_percent: 1.0,
                    memory_used_percent: 2.0,
                    rx_bytes_per_sec: Some(3.0),
                    tx_bytes_per_sec: Some(4.0),
                    latency_ms: Some(5),
                    disk_used_percent: Some(6.0),
                },
                None,
                &AtomicBool::new(false),
            )
            .expect("history point should persist");

            assert_mode_700(&data_dir);
            assert_mode_600(&db_path);
            for suffix in ["-wal", "-shm"] {
                let mut artifact = std::ffi::OsString::from(db_path.as_os_str());
                artifact.push(suffix);
                let artifact = std::path::PathBuf::from(artifact);
                if artifact.exists() {
                    assert_mode_600(&artifact);
                    let _ = std::fs::remove_file(&artifact);
                }
            }

            let _ = std::fs::remove_file(&db_path);
            let _ = std::fs::remove_dir(&data_dir);
            let _ = std::fs::remove_dir(&temp_dir);
        });
    }

    #[test]
    fn forget_missing_prunes_retired_nodes_from_write_throttle_state() {
        let runtime = Runtime::new().expect("runtime should build");
        runtime.block_on(async {
            let store = HistoryStore::new(PathBuf::from("./data/history.sqlite3"), 5);
            {
                let mut guard = store.last_written_at.lock().await;
                guard.insert("hk-01".to_string(), Utc::now());
                guard.insert("jp-01".to_string(), Utc::now());
                guard.insert("us-01".to_string(), Utc::now());
            }

            let removed = store
                .forget_missing(&["jp-01".to_string(), "us-01".to_string()])
                .await;
            assert_eq!(removed, 1);

            let guard = store.last_written_at.lock().await;
            assert!(!guard.contains_key("hk-01"));
            assert!(guard.contains_key("jp-01"));
            assert!(guard.contains_key("us-01"));
        });
    }

    #[tokio::test]
    async fn query_history_reports_connection_not_initialized() {
        let store = HistoryStore::new(PathBuf::from("./data/history.sqlite3"), 5);
        store.available.store(true, Ordering::Relaxed);

        let error = store
            .query_history("hk-01", 1, 60)
            .await
            .expect_err("query should surface typed connection error");

        assert!(matches!(error, HistoryError::ConnectionNotInitialized));
    }

    #[tokio::test]
    async fn query_history_range_reports_connection_not_initialized() {
        let store = HistoryStore::new(PathBuf::from("./data/history.sqlite3"), 5);
        store.available.store(true, Ordering::Relaxed);

        let now = Utc::now();
        let error = store
            .query_history_range("hk-01", now - Duration::hours(1), now, 60)
            .await
            .expect_err("range query should surface typed connection error");

        assert!(matches!(error, HistoryError::ConnectionNotInitialized));
    }

    #[test]
    fn query_history_between_buckets_and_limits_results() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic enough")
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!("nodelite-history-query-{unique}"));
        std::fs::create_dir_all(&temp_dir).expect("temp dir should exist");
        let db_path = temp_dir.join("history.sqlite3");
        let mut connection = initialize_database(&db_path, 5).expect("database should initialize");
        let hardened = AtomicBool::new(false);
        let start = Utc::now() - Duration::hours(6);
        for index in 0..180 {
            write_history_point(
                &db_path,
                &mut connection,
                &HistoryPoint {
                    node_id: "hk-01".to_string(),
                    recorded_at: start + Duration::seconds(index * 120),
                    cpu_usage_percent: index as f64,
                    memory_used_percent: 50.0,
                    rx_bytes_per_sec: Some(index as f64),
                    tx_bytes_per_sec: Some(index as f64 / 2.0),
                    latency_ms: Some((index % 10) as u64),
                    disk_used_percent: Some(60.0),
                },
                None,
                &hardened,
            )
            .expect("history point should persist");
        }

        let points = query_history_between(&connection, "hk-01", start, Utc::now(), 24)
            .expect("history query should succeed");
        assert!(!points.is_empty());
        assert!(points.len() <= 24);
        assert!(
            points
                .windows(2)
                .all(|pair| pair[0].recorded_at <= pair[1].recorded_at)
        );

        let _ = std::fs::remove_file(&db_path);
        let _ = std::fs::remove_dir(&temp_dir);
    }

    #[test]
    fn query_history_between_uses_covering_index() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic enough")
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!("nodelite-history-query-plan-{unique}"));
        std::fs::create_dir_all(&temp_dir).expect("temp dir should exist");
        let db_path = temp_dir.join("history.sqlite3");
        let connection = initialize_database(&db_path, 5).expect("database should initialize");
        let explain_sql = format!("EXPLAIN QUERY PLAN {HISTORY_QUERY_SQL}");
        let mut statement = connection
            .prepare(&explain_sql)
            .expect("query plan should prepare");
        let details = statement
            .query_map(
                rusqlite::params!["hk-01", 0_i64, i64::MAX, 60_i64, 24_i64],
                |row| row.get::<_, String>(3),
            )
            .expect("query plan should run")
            .collect::<Result<Vec<_>, _>>()
            .expect("query plan rows should decode");
        let plan = details.join("\n");

        assert!(
            plan.contains("USING COVERING INDEX idx_history_points_covering_metrics"),
            "history query should use covering index, got:\n{plan}"
        );

        drop(statement);
        drop(connection);
        let _ = std::fs::remove_file(&db_path);
        let _ = std::fs::remove_dir(&temp_dir);
    }

    #[test]
    fn sqlite_busy_retry_delay_uses_capped_exponential_backoff() {
        let delays_ms = (1..=8)
            .map(|attempt| sqlite_busy_retry_delay(attempt).as_millis())
            .collect::<Vec<_>>();

        assert_eq!(SQLITE_BUSY_MAX_RETRIES, 10);
        assert_eq!(delays_ms, vec![50, 100, 200, 400, 800, 1000, 1000, 1000]);
    }

    fn fake_status_for(node_id: &str, recorded_at: chrono::DateTime<Utc>) -> NodeStatus {
        NodeStatus {
            identity: NodeIdentity {
                node_id: node_id.to_string(),
                node_label: format!("{node_id}-label"),
                hostname: format!("{node_id}.internal"),
                os: "Ubuntu".to_string(),
                kernel_version: None,
                cpu_model: None,
                cpu_cores: 2,
                agent_version: "0.1.0".to_string(),
                boot_time: None,
                tags: Vec::new(),
            },
            remote_ip: Some("198.51.100.24".to_string()),
            snapshot: Some(NodeSnapshot {
                collected_at: recorded_at,
                cpu_usage_percent: 42.0,
                load: LoadAverage {
                    one: 0.1,
                    five: 0.2,
                    fifteen: 0.3,
                },
                memory: MemoryUsage {
                    total_bytes: 1024,
                    used_bytes: 512,
                    available_bytes: 512,
                    swap_total_bytes: 0,
                    swap_used_bytes: 0,
                },
                uptime_secs: 60,
                disks: Vec::new(),
                network: NetworkCounters {
                    total_rx_bytes: 1,
                    total_tx_bytes: 2,
                    rx_bytes_per_sec: Some(3.0),
                    tx_bytes_per_sec: Some(4.0),
                },
            }),
            last_seen: Some(recorded_at),
            latency_ms: Some(12),
            online: true,
        }
    }

    fn temp_history_db_path(test_name: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic enough")
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!("nodelite-history-{test_name}-{unique}"));
        std::fs::create_dir_all(&temp_dir).expect("temp dir should exist");
        temp_dir.join("history.sqlite3")
    }

    /// 集成 record_status → writer task → SQLite 全链路:
    /// 多次写入要在 channel + batch 模型下全部落库,而不仅仅是最后一条。
    #[tokio::test]
    async fn record_status_flushes_through_writer_task_to_sqlite() {
        let db_path = temp_history_db_path("writer-task");
        let store = HistoryStore::new(db_path.clone(), 5);
        store.initialize().await;
        assert!(store.is_available());

        // 写入 5 个不同节点的样本(同节点会被 throttle 拦掉,所以这里用不同 node_id)。
        let now = Utc::now();
        for i in 0..5 {
            let node_id = format!("node-{i:02}");
            let status = fake_status_for(&node_id, now);
            store.record_status(&status).await;
        }

        // 触发 shutdown — writer 会把已经入队但还没 flush 的样本 drain 出来。
        store.shutdown().await;
        assert_eq!(
            store.dropped_writes(),
            0,
            "no writes should have been dropped"
        );

        // 验证 5 条样本都成功落库。
        let connection = initialize_database(&db_path, 5).expect("re-open database");
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM history_points", [], |row| row.get(0))
            .expect("count query");
        assert_eq!(count, 5);

        let _ = std::fs::remove_file(&db_path);
        if let Some(parent) = db_path.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }

    #[tokio::test]
    async fn record_status_does_not_throttle_after_queue_full_drop() {
        let db_path = temp_history_db_path("queue-full-throttle");
        let store = HistoryStore::new(db_path.clone(), 5);
        store.available.store(true, Ordering::Relaxed);
        let (tx, _rx) = tokio::sync::mpsc::channel::<HistoryPoint>(HISTORY_CHANNEL_CAPACITY);
        for index in 0..HISTORY_CHANNEL_CAPACITY {
            tx.try_send(HistoryPoint {
                node_id: format!("queued-{index}"),
                recorded_at: Utc::now(),
                cpu_usage_percent: 1.0,
                memory_used_percent: 2.0,
                rx_bytes_per_sec: Some(3.0),
                tx_bytes_per_sec: Some(4.0),
                latency_ms: Some(5),
                disk_used_percent: Some(6.0),
            })
            .expect("test channel should accept prefilled point");
        }
        {
            let mut guard = store.writer_tx.write().await;
            *guard = Some(tx);
        }

        let status = fake_status_for("hk-01", Utc::now());
        store.record_status(&status).await;

        assert_eq!(store.dropped_writes(), 1);
        let guard = store.last_written_at.lock().await;
        assert!(
            !guard.contains_key("hk-01"),
            "dropped writes must not advance the throttle window"
        );

        let _ = std::fs::remove_file(&db_path);
        if let Some(parent) = db_path.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }

    #[tokio::test]
    async fn record_status_skips_point_build_when_throttled() {
        let db_path = temp_history_db_path("throttled-builder");
        let store = HistoryStore::new(db_path.clone(), 5);
        store.available.store(true, Ordering::Relaxed);
        let (tx, _rx) = tokio::sync::mpsc::channel::<HistoryPoint>(1);
        {
            let mut guard = store.writer_tx.write().await;
            *guard = Some(tx);
        }

        let now = Utc::now();
        {
            let mut guard = store.last_written_at.lock().await;
            guard.insert("hk-01".to_string(), now);
        }

        let builds = AtomicUsize::new(0);
        let status = fake_status_for("hk-01", now);
        store
            .record_status_with_builder(&status, |_| {
                builds.fetch_add(1, Ordering::Relaxed);
                build_history_point(&status)
            })
            .await;

        assert_eq!(
            builds.load(Ordering::Relaxed),
            0,
            "throttled samples should return before building a HistoryPoint"
        );

        let _ = std::fs::remove_file(&db_path);
        if let Some(parent) = db_path.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }

    /// shutdown() 之后再调用 record_status 必须立刻返回,
    /// 不能 panic 也不能阻塞;此时 sender 已被清空,store 会进入 unavailable 状态。
    #[tokio::test]
    async fn record_status_is_noop_after_shutdown() {
        let db_path = temp_history_db_path("after-shutdown");
        let store = HistoryStore::new(db_path.clone(), 5);
        store.initialize().await;
        store.shutdown().await;

        // shutdown 不会触发 dropped 计数 —— 它走的是 "writer_tx 被 take 走" 的快速 return 路径,
        // 不会走 try_send。
        let status = fake_status_for("hk-01", Utc::now());
        store.record_status(&status).await;
        assert_eq!(store.dropped_writes(), 0);

        let _ = std::fs::remove_file(&db_path);
        if let Some(parent) = db_path.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }

    #[cfg(unix)]
    fn assert_mode_700(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;

        let mode = std::fs::metadata(path)
            .expect("artifact metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[cfg(unix)]
    fn assert_mode_600(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;

        let mode = std::fs::metadata(path)
            .expect("artifact metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}
