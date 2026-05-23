//! 历史数据库初始化与权限加固。

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};

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
            cpu_usage_percent REAL,
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
    migrate_nullable_cpu_usage(&connection)?;
    harden_database_artifacts(db_path)?;

    Ok(connection)
}

fn migrate_nullable_cpu_usage(connection: &Connection) -> Result<()> {
    let cpu_not_null = connection
        .prepare("PRAGMA table_info(history_points)")?
        .query_map([], |row| {
            let column_name: String = row.get(1)?;
            let not_null: i64 = row.get(3)?;
            Ok((column_name, not_null))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .any(|(column_name, not_null)| column_name == "cpu_usage_percent" && not_null != 0);
    if !cpu_not_null {
        return Ok(());
    }

    connection.execute_batch(
        r#"
        DROP INDEX IF EXISTS idx_history_points_node_time;
        DROP INDEX IF EXISTS idx_history_points_covering_metrics;
        ALTER TABLE history_points RENAME TO history_points_legacy_not_null_cpu;
        CREATE TABLE history_points (
            node_id TEXT NOT NULL,
            recorded_at INTEGER NOT NULL,
            cpu_usage_percent REAL,
            memory_used_percent REAL NOT NULL,
            rx_bytes_per_sec REAL,
            tx_bytes_per_sec REAL,
            latency_ms INTEGER,
            disk_used_percent REAL
        );
        INSERT INTO history_points (
            node_id,
            recorded_at,
            cpu_usage_percent,
            memory_used_percent,
            rx_bytes_per_sec,
            tx_bytes_per_sec,
            latency_ms,
            disk_used_percent
        )
        SELECT
            node_id,
            recorded_at,
            cpu_usage_percent,
            memory_used_percent,
            rx_bytes_per_sec,
            tx_bytes_per_sec,
            latency_ms,
            disk_used_percent
        FROM history_points_legacy_not_null_cpu;
        DROP TABLE history_points_legacy_not_null_cpu;
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
    Ok(())
}

/// 打开查询专用连接。初始化阶段已经确保库文件和 schema 存在,
/// 这里使用只读连接,避免 history 查询与 writer task 争用同一个连接 mutex。
pub(super) fn open_read_connection(
    db_path: &PathBuf,
    sqlite_busy_timeout_secs: u64,
) -> Result<Connection> {
    let connection = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| {
            format!(
                "failed to open read-only history database {}",
                db_path.display()
            )
        })?;
    connection
        .busy_timeout(Duration::from_secs(sqlite_busy_timeout_secs))
        .context("failed to configure sqlite read busy timeout")?;
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
