//! 历史数据库初始化与权限加固。

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::Connection;

use crate::fs_security::{create_private_dir_all, ensure_directory_mode};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// 建库:如果父目录不存在则创建,然后建表 / 建索引并收紧权限。
/// 返回已配置好的持久化连接(WAL 模式 + busy_timeout),供后续写入/查询复用。
pub(super) fn initialize_database(
    db_path: &PathBuf,
    sqlite_busy_timeout_secs: u64,
) -> Result<Connection> {
    if let Some(parent) = db_path.parent()
        && !parent.as_os_str().is_empty()
    {
        create_private_dir_all(parent)?;
    }

    let connection = open_database_connection(db_path, true, sqlite_busy_timeout_secs)?;
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

/// 打开 SQLite 连接,可选启用 WAL 模式以提升并发写入吞吐。
fn open_database_connection(
    db_path: &PathBuf,
    enable_wal: bool,
    sqlite_busy_timeout_secs: u64,
) -> Result<Connection> {
    let connection = Connection::open(db_path)
        .with_context(|| format!("failed to open history database {}", db_path.display()))?;
    connection
        .busy_timeout(Duration::from_secs(sqlite_busy_timeout_secs))
        .context("failed to configure sqlite busy timeout")?;
    if enable_wal {
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .context("failed to enable sqlite WAL mode")?;
    }
    Ok(connection)
}

/// 收紧主库文件以及 WAL / SHM 辅助文件的权限。
pub(super) fn harden_database_artifacts(db_path: &PathBuf) -> Result<()> {
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
