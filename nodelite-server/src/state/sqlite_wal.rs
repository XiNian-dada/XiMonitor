//! SQLite WAL checkpoint observation for runtime metrics.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use nodelite_proto::ServerConfig;
use rusqlite::{Connection, OpenFlags};
use tokio::sync::Mutex;

use crate::handlers::metrics_exporter::{SqliteWalCheckpointMetrics, SqliteWalCheckpointStats};

const SQLITE_WAL_CHECKPOINT_METRICS_TTL: Duration = Duration::from_secs(60);
const SQLITE_WAL_CHECKPOINT_BUSY_TIMEOUT: Duration = Duration::from_millis(100);

#[derive(Clone)]
pub(super) struct SqliteWalCheckpointObserver {
    config: Arc<ServerConfig>,
    cache: Arc<Mutex<SqliteWalCheckpointCache>>,
    build_lock: Arc<Mutex<()>>,
}

impl SqliteWalCheckpointObserver {
    pub(super) fn new(config: Arc<ServerConfig>) -> Self {
        Self {
            config,
            cache: Arc::new(Mutex::new(SqliteWalCheckpointCache::default())),
            build_lock: Arc::new(Mutex::new(())),
        }
    }

    pub(super) async fn metrics(&self) -> SqliteWalCheckpointMetrics {
        if let Some(metrics) = self.cached_metrics().await {
            return metrics;
        }

        let _build_guard = self.build_lock.lock().await;
        if let Some(metrics) = self.cached_metrics().await {
            return metrics;
        }

        let history_path = self.config.history_db_path.clone();
        let audit_path = self.config.audit.db_path.clone();
        let metrics = tokio::task::spawn_blocking(move || SqliteWalCheckpointMetrics {
            history: observe_sqlite_wal_checkpoint(history_path.as_path()),
            audit: observe_sqlite_wal_checkpoint(audit_path.as_path()),
        })
        .await
        .unwrap_or_default();

        let mut cache = self.cache.lock().await;
        *cache = SqliteWalCheckpointCache {
            observed_at: Some(Instant::now()),
            metrics,
        };
        metrics
    }

    async fn cached_metrics(&self) -> Option<SqliteWalCheckpointMetrics> {
        let cache = self.cache.lock().await;
        let observed_at = cache.observed_at?;
        (observed_at.elapsed() < SQLITE_WAL_CHECKPOINT_METRICS_TTL).then_some(cache.metrics)
    }
}

#[derive(Clone, Copy, Default)]
struct SqliteWalCheckpointCache {
    observed_at: Option<Instant>,
    metrics: SqliteWalCheckpointMetrics,
}

fn observe_sqlite_wal_checkpoint(path: &Path) -> SqliteWalCheckpointStats {
    let Ok(connection) = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)
    else {
        return SqliteWalCheckpointStats::default();
    };
    if connection
        .busy_timeout(SQLITE_WAL_CHECKPOINT_BUSY_TIMEOUT)
        .is_err()
    {
        return SqliteWalCheckpointStats::default();
    }
    let Ok(journal_mode) =
        connection.pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))
    else {
        return SqliteWalCheckpointStats::default();
    };
    if !journal_mode.eq_ignore_ascii_case("wal") {
        return SqliteWalCheckpointStats {
            observed: true,
            active: false,
            ..SqliteWalCheckpointStats::default()
        };
    }

    let result = connection.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |row| {
        let busy: i64 = row.get(0)?;
        let log_pages: i64 = row.get(1)?;
        let checkpointed_pages: i64 = row.get(2)?;
        Ok((busy, log_pages, checkpointed_pages))
    });

    match result {
        Ok((busy, log_pages, checkpointed_pages)) if log_pages >= 0 && checkpointed_pages >= 0 => {
            SqliteWalCheckpointStats {
                observed: true,
                active: true,
                busy: busy.max(0) as u64,
                log_pages: log_pages as u64,
                checkpointed_pages: checkpointed_pages as u64,
            }
        }
        _ => SqliteWalCheckpointStats::default(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::observe_sqlite_wal_checkpoint;

    #[test]
    fn sqlite_wal_checkpoint_observation_reports_wal_page_counts() {
        let temp_dir = unique_temp_dir("nodelite-wal-observe");
        std::fs::create_dir_all(&temp_dir).expect("temp dir should be created");
        let db_path = temp_dir.join("history.sqlite3");
        {
            let connection = rusqlite::Connection::open(&db_path).expect("db should open");
            connection
                .pragma_update(None, "journal_mode", "WAL")
                .expect("wal should enable");
            connection
                .execute_batch(
                    "CREATE TABLE samples (id INTEGER PRIMARY KEY, value TEXT);
                     INSERT INTO samples (value) VALUES ('one'), ('two');",
                )
                .expect("sample rows should write");
        }

        let stats = observe_sqlite_wal_checkpoint(db_path.as_path());

        assert!(stats.observed);
        assert!(stats.active);
        assert!(stats.log_pages >= stats.checkpointed_pages);
    }

    #[test]
    fn sqlite_wal_checkpoint_observation_treats_missing_database_as_unavailable() {
        let temp_dir = unique_temp_dir("nodelite-wal-missing");
        let db_path = temp_dir.join("missing.sqlite3");

        let stats = observe_sqlite_wal_checkpoint(db_path.as_path());

        assert!(!stats.observed);
        assert!(!stats.active);
        assert_eq!(stats.log_pages, 0);
        assert_eq!(stats.checkpointed_pages, 0);
    }

    #[test]
    fn sqlite_wal_checkpoint_observation_reports_non_wal_mode() {
        let temp_dir = unique_temp_dir("nodelite-wal-inactive");
        std::fs::create_dir_all(&temp_dir).expect("temp dir should be created");
        let db_path = temp_dir.join("audit.sqlite3");
        {
            let connection = rusqlite::Connection::open(&db_path).expect("db should open");
            connection
                .execute_batch("CREATE TABLE audit_log (id INTEGER PRIMARY KEY);")
                .expect("sample table should write");
        }

        let stats = observe_sqlite_wal_checkpoint(db_path.as_path());

        assert!(stats.observed);
        assert!(!stats.active);
        assert_eq!(stats.log_pages, 0);
        assert_eq!(stats.checkpointed_pages, 0);
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}-{unique}"))
    }
}
