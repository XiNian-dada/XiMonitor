//! SQLite schema and filesystem hardening helpers for audit persistence.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{Duration as ChronoDuration, Utc};
use rusqlite::{Connection, params};

use crate::fs_security::{create_private_dir_all, ensure_directory_mode};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

pub(super) const AUDIT_TABLE_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS audit_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp INTEGER NOT NULL,
    event_type TEXT NOT NULL,
    user TEXT,
    node_id TEXT,
    ip_address TEXT NOT NULL,
    user_agent TEXT,
    success INTEGER NOT NULL,
    details TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_audit_log_timestamp ON audit_log(timestamp);
CREATE INDEX IF NOT EXISTS idx_audit_log_timestamp_desc ON audit_log(timestamp DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_audit_log_event_type ON audit_log(event_type);
CREATE INDEX IF NOT EXISTS idx_audit_log_event_success_time
    ON audit_log(event_type, success, timestamp DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_audit_log_success_time
    ON audit_log(success, timestamp DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_audit_log_ip_address ON audit_log(ip_address);
"#;

pub(super) const AUDIT_CHANNEL_CAPACITY: usize = 4096;
pub(super) const AUDIT_PRUNE_INTERVAL: Duration = Duration::from_secs(60);

pub(super) fn open_audit_connection(
    path: &Path,
    sqlite_busy_timeout_secs: u64,
) -> Result<Connection> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        create_private_dir_all(parent)?;
    }

    let connection = Connection::open(path)
        .with_context(|| format!("failed to open audit database {}", path.display()))?;
    connection
        .busy_timeout(Duration::from_secs(sqlite_busy_timeout_secs))
        .with_context(|| format!("failed to set audit busy timeout for {}", path.display()))?;
    connection
        .execute_batch(AUDIT_TABLE_SQL)
        .with_context(|| format!("failed to initialize audit schema {}", path.display()))?;
    harden_audit_artifacts(path)?;
    Ok(connection)
}

pub(super) fn prune_expired_records(connection: &Connection, retention_days: u64) -> Result<usize> {
    let cutoff = Utc::now() - ChronoDuration::days(retention_days as i64);
    connection
        .execute(
            "DELETE FROM audit_log WHERE timestamp < ?1",
            params![cutoff.timestamp()],
        )
        .context("failed to prune expired audit records")
}

fn harden_audit_artifacts(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        ensure_directory_mode(parent, 0o700)?;
    }

    #[cfg(unix)]
    {
        for artifact in audit_artifact_paths(path) {
            if artifact.exists() {
                std::fs::set_permissions(&artifact, std::fs::Permissions::from_mode(0o600))
                    .with_context(|| format!("failed to chmod {}", artifact.display()))?;
            }
        }
    }

    #[cfg(not(unix))]
    {
        let _ = path;
    }

    Ok(())
}

#[cfg(unix)]
fn audit_artifact_paths(path: &Path) -> Vec<PathBuf> {
    let mut wal = path.as_os_str().to_os_string();
    wal.push("-wal");
    let mut shm = path.as_os_str().to_os_string();
    shm.push("-shm");
    vec![path.to_path_buf(), wal.into(), shm.into()]
}
