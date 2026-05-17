//! 历史数据存储:把 `NodeStatus` 中的关键指标写入本地 SQLite 表,
//! 供前端绘制趋势图。设计目标:
//!
//! - 不阻塞实时 WebSocket 流程:所有 SQLite 调用都进入 `spawn_blocking`。
//! - 节流:同一节点两次写入至少间隔 `DEFAULT_HISTORY_WRITE_INTERVAL_SECS` 秒。
//! - 自清理:每 5 分钟最多触发一次 `DELETE`,把超过保留期的旧记录删除。
//! - 自降级:数据库初始化失败时不阻断服务,而是把 `available=false`,实时视图照常运行。

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, TimeZone, Utc};
use nodelite_proto::{
    DEFAULT_HISTORY_RETENTION_HOURS, DEFAULT_HISTORY_WRITE_INTERVAL_SECS, HistoryPoint, NodeStatus,
    percentage,
};
use rusqlite::{Connection, Error as SqliteError, ErrorCode, params};
use tokio::sync::Mutex;
use tracing::{error, warn};

use crate::fs_security::{create_private_dir_all, ensure_directory_mode};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// SQLite 在并发写入冲突时的等待时长。
const SQLITE_BUSY_TIMEOUT_SECS: u64 = 5;
const SQLITE_BUSY_MAX_RETRIES: u32 = 10;
const SQLITE_BUSY_RETRY_BASE_MS: u64 = 50;
const SQLITE_BUSY_RETRY_MAX_MS: u64 = 1_000;
const HISTORY_QUERY_SQL: &str = r#"
        SELECT
            ?1 AS node_id,
            MAX(recorded_at) AS recorded_at,
            AVG(cpu_usage_percent) AS cpu_usage_percent,
            AVG(memory_used_percent) AS memory_used_percent,
            AVG(rx_bytes_per_sec) AS rx_bytes_per_sec,
            AVG(tx_bytes_per_sec) AS tx_bytes_per_sec,
            AVG(latency_ms) AS latency_ms,
            AVG(disk_used_percent) AS disk_used_percent
        FROM history_points INDEXED BY idx_history_points_covering_metrics
        WHERE node_id = ?1 AND recorded_at >= ?2 AND recorded_at <= ?3
        GROUP BY ((recorded_at - ?2) / ?4)
        ORDER BY recorded_at ASC
        LIMIT ?5
        "#;

/// 对外暴露的历史存储句柄,可被低成本克隆给多个异步任务。
#[derive(Clone)]
pub struct HistoryStore {
    db_path: Arc<PathBuf>,
    available: Arc<AtomicBool>,
    /// 持久化 SQLite 连接,避免每次写入都重新打开 + 重设 PRAGMA。
    /// 用 `Option` 包裹是因为 `initialize()` 前连接尚未建立。
    connection: Arc<Mutex<Option<Connection>>>,
    /// 节点 → 上一次成功写入时间,用于实现"按时间节流"。
    last_written_at: Arc<Mutex<HashMap<String, DateTime<Utc>>>>,
    /// 最近一次执行清理删除的时间,用于避免每次写入都触发删除。
    last_pruned_at: Arc<Mutex<Option<DateTime<Utc>>>>,
    /// WAL/SHM 文件通常在第一次真实写入后才出现,此标志确保后续权限加固只做一次。
    artifacts_hardened_after_write: Arc<AtomicBool>,
    /// 写入互斥:多个节点的写入串行化,简化 SQLite 端的锁竞争。
    write_gate: Arc<Mutex<()>>,
}

impl HistoryStore {
    pub fn new(db_path: PathBuf) -> Self {
        Self {
            db_path: Arc::new(db_path),
            available: Arc::new(AtomicBool::new(false)),
            connection: Arc::new(Mutex::new(None)),
            last_written_at: Arc::new(Mutex::new(HashMap::new())),
            last_pruned_at: Arc::new(Mutex::new(None)),
            artifacts_hardened_after_write: Arc::new(AtomicBool::new(false)),
            write_gate: Arc::new(Mutex::new(())),
        }
    }

    /// 初始化数据库:建表、建索引、加锁权限。失败不会抛出,仅记录警告并保持 `available=false`。
    pub async fn initialize(&self) {
        let db_path = Arc::clone(&self.db_path);
        let result = tokio::task::spawn_blocking(move || initialize_database(db_path.as_ref()))
            .await
            .context("history database task failed");

        match result {
            Ok(Ok(connection)) => {
                let mut guard = self.connection.lock().await;
                *guard = Some(connection);
                self.available.store(true, Ordering::Relaxed);
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

    pub fn is_available(&self) -> bool {
        self.available.load(Ordering::Relaxed)
    }

    /// 尝试把一次节点状态记录到历史表。
    ///
    /// 当节点首次上报、距上次写入不足节流窗口、数据库不可用时,本调用静默返回。
    pub async fn record_status(&self, status: &NodeStatus) {
        if !self.is_available() {
            return;
        }

        let Some(point) = build_history_point(status) else {
            return;
        };

        let _write_guard = self.write_gate.lock().await;
        {
            let guard = self.last_written_at.lock().await;
            if let Some(previous) = guard.get(&point.node_id) {
                let Ok(elapsed) = point
                    .recorded_at
                    .signed_duration_since(previous.to_owned())
                    .to_std()
                else {
                    return;
                };
                if elapsed < Duration::from_secs(DEFAULT_HISTORY_WRITE_INTERVAL_SECS) {
                    return;
                }
            }
        }

        let prune_before = self.maybe_schedule_prune().await;
        let db_path = Arc::clone(&self.db_path);
        let connection_arc = Arc::clone(&self.connection);
        let artifacts_hardened_after_write = Arc::clone(&self.artifacts_hardened_after_write);
        let point_for_task = point.clone();
        let result = tokio::task::spawn_blocking(move || {
            let guard = connection_arc.blocking_lock();
            let Some(ref connection) = *guard else {
                anyhow::bail!("history connection not initialized");
            };
            write_history_point(
                db_path.as_ref(),
                connection,
                &point_for_task,
                prune_before,
                artifacts_hardened_after_write.as_ref(),
            )
        })
        .await;

        match result {
            Ok(Ok(())) => {
                let mut guard = self.last_written_at.lock().await;
                guard.insert(point.node_id, point.recorded_at);
            }
            Ok(Err(error)) => {
                warn!(error = ?error, "failed to persist history point");
            }
            Err(error) => {
                warn!(error = ?error, "history write task join failed");
            }
        }
    }

    /// 按"过去 N 小时"窗口查询历史记录。
    pub async fn query_history(
        &self,
        node_id: &str,
        window_hours: u64,
        max_points: usize,
    ) -> Result<Vec<HistoryPoint>> {
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
                anyhow::bail!("history connection not initialized");
            };
            query_history(connection, &node_id, since, clamped_max_points)
        })
        .await
        .context("history query task failed")?
    }

    /// 按"任意时间区间"查询历史记录。超出保留期或反向区间会被自动裁剪。
    pub async fn query_history_range(
        &self,
        node_id: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        max_points: usize,
    ) -> Result<Vec<HistoryPoint>> {
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
                anyhow::bail!("history connection not initialized");
            };
            query_history_between(
                connection,
                &node_id,
                clamped_start,
                clamped_end,
                clamped_max_points,
            )
        })
        .await
        .context("history range query task failed")?
    }

    /// 清理已经不在注册表中的节点节流状态,避免长期运行时条目只增不减。
    pub async fn forget_missing(&self, live_node_ids: &[String]) -> usize {
        let live_node_ids: HashSet<&str> = live_node_ids.iter().map(String::as_str).collect();
        let mut guard = self.last_written_at.lock().await;
        let before = guard.len();
        guard.retain(|node_id, _| live_node_ids.contains(node_id.as_str()));
        before.saturating_sub(guard.len())
    }

    /// 判断是否需要在本次写入时附带执行一次过期记录删除。
    /// 至少 5 分钟才会真正触发一次 DELETE。
    async fn maybe_schedule_prune(&self) -> Option<DateTime<Utc>> {
        let mut guard = self.last_pruned_at.lock().await;
        let now = Utc::now();
        let should_prune = guard
            .as_ref()
            .map(|last_pruned| {
                now.signed_duration_since(last_pruned.to_owned())
                    .to_std()
                    .map(|elapsed| elapsed >= Duration::from_secs(300))
                    .unwrap_or(false)
            })
            .unwrap_or(true);

        if should_prune {
            *guard = Some(now);
            Some(now - chrono::Duration::hours(DEFAULT_HISTORY_RETENTION_HOURS as i64))
        } else {
            None
        }
    }
}

/// 建库:如果父目录不存在则创建,然后建表 / 建索引并收紧权限。
/// 返回已配置好的持久化连接(WAL 模式 + busy_timeout),供后续写入/查询复用。
fn initialize_database(db_path: &PathBuf) -> Result<Connection> {
    if let Some(parent) = db_path.parent()
        && !parent.as_os_str().is_empty()
    {
        create_private_dir_all(parent)?;
    }

    let connection = open_database_connection(db_path, true)?;
    connection.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS history_points (
            node_id TEXT NOT NULL,
            recorded_at INTEGER NOT NULL,
            cpu_usage_percent REAL NOT NULL,
            memory_used_percent REAL NOT NULL,
            rx_bytes_per_sec REAL,
            tx_bytes_per_sec REAL,
            latency_ms INTEGER,
            disk_used_percent REAL
        );
        CREATE INDEX IF NOT EXISTS idx_history_points_node_time
            ON history_points (node_id, recorded_at);
        CREATE INDEX IF NOT EXISTS idx_history_points_covering_metrics
            ON history_points (
                node_id,
                recorded_at,
                cpu_usage_percent,
                memory_used_percent,
                rx_bytes_per_sec,
                tx_bytes_per_sec,
                latency_ms,
                disk_used_percent
            );
        "#,
    )?;
    harden_database_artifacts(db_path)?;

    Ok(connection)
}

/// 写入一条历史记录,同时按需删除过期记录。复用已打开的连接,避免重复 open + PRAGMA。
fn write_history_point(
    db_path: &PathBuf,
    connection: &Connection,
    point: &HistoryPoint,
    prune_before: Option<DateTime<Utc>>,
    artifacts_hardened_after_write: &AtomicBool,
) -> Result<()> {
    with_sqlite_busy_retry(|| {
        connection.execute(
            r#"
            INSERT INTO history_points (
                node_id,
                recorded_at,
                cpu_usage_percent,
                memory_used_percent,
                rx_bytes_per_sec,
                tx_bytes_per_sec,
                latency_ms,
                disk_used_percent
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
            params![
                &point.node_id,
                point.recorded_at.timestamp(),
                point.cpu_usage_percent,
                point.memory_used_percent,
                point.rx_bytes_per_sec,
                point.tx_bytes_per_sec,
                point.latency_ms,
                point.disk_used_percent,
            ],
        )?;

        if let Some(cutoff) = prune_before {
            connection.execute(
                "DELETE FROM history_points WHERE recorded_at < ?1",
                params![cutoff.timestamp()],
            )?;
        }

        Ok(())
    })?;
    if !artifacts_hardened_after_write.load(Ordering::Relaxed) {
        harden_database_artifacts(db_path)?;
        artifacts_hardened_after_write.store(true, Ordering::Relaxed);
    }

    Ok(())
}

fn query_history(
    connection: &Connection,
    node_id: &str,
    since: DateTime<Utc>,
    max_points: usize,
) -> Result<Vec<HistoryPoint>> {
    query_history_between(connection, node_id, since, Utc::now(), max_points)
}

/// 在 `[since, until]` 之间查询某节点的历史点,并按时间升序返回。
fn query_history_between(
    connection: &Connection,
    node_id: &str,
    since: DateTime<Utc>,
    until: DateTime<Utc>,
    max_points: usize,
) -> Result<Vec<HistoryPoint>> {
    let max_points = max_points.max(1);
    let span_seconds = (until.timestamp() - since.timestamp()).max(1);
    let bucket_seconds = ((span_seconds as usize).div_ceil(max_points)).max(1) as i64;
    let mut statement = connection.prepare(HISTORY_QUERY_SQL)?;
    let rows = statement.query_map(
        params![
            node_id,
            since.timestamp(),
            until.timestamp(),
            bucket_seconds,
            max_points as i64
        ],
        |row| {
            let recorded_at = row.get::<_, i64>(1)?;
            Ok(HistoryPoint {
                node_id: row.get(0)?,
                recorded_at: Utc
                    .timestamp_opt(recorded_at, 0)
                    .single()
                    .unwrap_or_else(Utc::now),
                cpu_usage_percent: row.get(2)?,
                memory_used_percent: row.get(3)?,
                rx_bytes_per_sec: row.get(4)?,
                tx_bytes_per_sec: row.get(5)?,
                latency_ms: row
                    .get::<_, Option<f64>>(6)?
                    .map(|value| value.max(0.0).round() as u64),
                disk_used_percent: row.get(7)?,
            })
        },
    )?;

    let mut points = Vec::new();
    for row in rows {
        points.push(row?);
    }
    Ok(points)
}

/// 打开 SQLite 连接,可选启用 WAL 模式以提升并发写入吞吐。
fn open_database_connection(db_path: &PathBuf, enable_wal: bool) -> Result<Connection> {
    let connection = Connection::open(db_path)
        .with_context(|| format!("failed to open history database {}", db_path.display()))?;
    connection
        .busy_timeout(Duration::from_secs(SQLITE_BUSY_TIMEOUT_SECS))
        .context("failed to configure sqlite busy timeout")?;
    if enable_wal {
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .context("failed to enable sqlite WAL mode")?;
    }
    Ok(connection)
}

/// 收紧主库文件以及 WAL / SHM 辅助文件的权限。
fn harden_database_artifacts(db_path: &PathBuf) -> Result<()> {
    if let Some(parent) = db_path.parent()
        && !parent.as_os_str().is_empty()
    {
        ensure_directory_mode(parent, 0o700)?;
    }
    harden_path_permissions(db_path)?;
    for suffix in ["-wal", "-shm"] {
        let mut artifact = OsString::from(db_path.as_os_str());
        artifact.push(suffix);
        let artifact = PathBuf::from(artifact);
        if artifact.exists() {
            harden_path_permissions(&artifact)?;
        }
    }
    Ok(())
}

fn harden_path_permissions(path: &PathBuf) -> Result<()> {
    #[cfg(unix)]
    {
        if path.exists() {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .with_context(|| format!("failed to chmod {}", path.display()))?;
        }
    }

    #[cfg(not(unix))]
    {
        let _ = path;
    }

    Ok(())
}

/// 由实时 `NodeStatus` 构造一条历史采样点;若节点尚无快照则返回 `None`。
fn build_history_point(status: &NodeStatus) -> Option<HistoryPoint> {
    let snapshot = status.snapshot.as_ref()?;
    let total_disk_bytes = snapshot
        .disks
        .iter()
        .fold(0_u64, |total, disk| total.saturating_add(disk.total_bytes));
    let used_disk_bytes = snapshot
        .disks
        .iter()
        .fold(0_u64, |total, disk| total.saturating_add(disk.used_bytes));
    let disk_used_percent =
        (total_disk_bytes > 0).then(|| percentage(used_disk_bytes, total_disk_bytes));
    let recorded_at = status.last_seen.unwrap_or_else(Utc::now);

    Some(HistoryPoint {
        node_id: status.identity.node_id.clone(),
        recorded_at,
        cpu_usage_percent: snapshot.cpu_usage_percent,
        memory_used_percent: snapshot.memory.used_percent(),
        rx_bytes_per_sec: snapshot.network.rx_bytes_per_sec,
        tx_bytes_per_sec: snapshot.network.tx_bytes_per_sec,
        latency_ms: status.latency_ms,
        disk_used_percent,
    })
}

fn with_sqlite_busy_retry<F>(mut operation: F) -> Result<()>
where
    F: FnMut() -> Result<()>,
{
    let mut attempt = 0_u32;
    loop {
        match operation() {
            Ok(()) => return Ok(()),
            Err(error) if is_sqlite_busy(&error) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                attempt = attempt.saturating_add(1);
                let delay = sqlite_busy_retry_delay(attempt);
                let delay_ms = delay.as_millis() as u64;
                warn!(
                    attempt,
                    max_retries = SQLITE_BUSY_MAX_RETRIES,
                    delay_ms,
                    "sqlite busy while writing history point; retrying"
                );
                std::thread::sleep(delay);
            }
            Err(error) => return Err(error),
        }
    }
}

fn sqlite_busy_retry_delay(attempt: u32) -> Duration {
    let exponent = attempt.saturating_sub(1).min(5);
    let delay_ms = SQLITE_BUSY_RETRY_BASE_MS
        .saturating_mul(1_u64 << exponent)
        .min(SQLITE_BUSY_RETRY_MAX_MS);
    Duration::from_millis(delay_ms)
}

fn is_sqlite_busy(error: &anyhow::Error) -> bool {
    error.downcast_ref::<SqliteError>().is_some_and(|error| {
        matches!(
            error,
            SqliteError::SqliteFailure(code, _)
                if code.code == ErrorCode::DatabaseBusy || code.code == ErrorCode::DatabaseLocked
        )
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;
    use std::time::{SystemTime, UNIX_EPOCH};

    use chrono::{Duration, Utc};
    use nodelite_proto::{
        HistoryPoint, LoadAverage, MemoryUsage, NetworkCounters, NodeIdentity, NodeSnapshot,
        NodeStatus,
    };
    use tokio::runtime::Runtime;

    use super::{
        HISTORY_QUERY_SQL, HistoryStore, SQLITE_BUSY_MAX_RETRIES, build_history_point,
        initialize_database, query_history_between, sqlite_busy_retry_delay, write_history_point,
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

            let connection = initialize_database(&db_path).expect("database should initialize");
            write_history_point(
                &db_path,
                &connection,
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
            let store = HistoryStore::new(PathBuf::from("./data/history.sqlite3"));
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

    #[test]
    fn query_history_between_buckets_and_limits_results() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic enough")
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!("nodelite-history-query-{unique}"));
        std::fs::create_dir_all(&temp_dir).expect("temp dir should exist");
        let db_path = temp_dir.join("history.sqlite3");
        let connection = initialize_database(&db_path).expect("database should initialize");
        let hardened = AtomicBool::new(false);
        let start = Utc::now() - Duration::hours(6);
        for index in 0..180 {
            write_history_point(
                &db_path,
                &connection,
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
        let connection = initialize_database(&db_path).expect("database should initialize");
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
