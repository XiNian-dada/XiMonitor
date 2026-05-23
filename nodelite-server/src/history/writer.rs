//! 历史写入任务、批量落盘与节流辅助逻辑。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Utc};
use nodelite_proto::{DEFAULT_HISTORY_RETENTION_HOURS, HistoryPoint, NodeStatus, percentage};
use rusqlite::{Connection, Error as SqliteError, ErrorCode, params};
use tokio::sync::{Mutex, mpsc};
use tokio::time::MissedTickBehavior;
use tracing::{debug, warn};

use super::{
    HISTORY_BATCH_FLUSH_INTERVAL, HISTORY_BATCH_MAX, HISTORY_PRUNE_MIN_INTERVAL,
    SQLITE_BUSY_MAX_RETRIES, SQLITE_BUSY_RETRY_BASE_MS, SQLITE_BUSY_RETRY_MAX_MS,
};
use crate::history::init::harden_database_artifacts;

/// Writer task 拥有的所有共享状态(都是 `Arc`,可以低成本 clone)。
pub(super) struct WriterContext {
    pub(super) db_path: Arc<PathBuf>,
    pub(super) write_connection: Arc<Mutex<Option<Connection>>>,
    pub(super) last_pruned_at: Arc<AtomicI64>,
    pub(super) artifacts_hardened_after_write: Arc<AtomicBool>,
}

/// 单一 writer 任务:对外的所有 record_status 都经过 mpsc 灌入此处。
///
/// 关键不变量:
/// - 任意时刻只有一个 `run_history_writer` task,所以 batch 状态可以是函数局部变量,
///   不再需要任何额外的 mutex 来保护 batch / last_pruned 等中间状态;
/// - 一次 flush 把 batch 内多条 INSERT 包进 BEGIN / COMMIT,把 N 次 fsync 折叠成 1 次;
/// - sender 被全部 drop 后(运行时关停或 store.shutdown())channel 进入 closed 状态,
///   writer 把残留 batch flush 出去再退出,保证关停时不丢数据。
pub(super) async fn run_history_writer(
    mut rx: mpsc::Receiver<HistoryPoint>,
    context: WriterContext,
) {
    let mut batch: Vec<HistoryPoint> = Vec::with_capacity(HISTORY_BATCH_MAX);
    let mut flush_timer = tokio::time::interval(HISTORY_BATCH_FLUSH_INTERVAL);
    flush_timer.set_missed_tick_behavior(MissedTickBehavior::Delay);
    flush_timer.tick().await;

    loop {
        tokio::select! {
            biased;
            received = rx.recv() => match received {
                Some(point) => {
                    batch.push(point);
                    if batch.len() >= HISTORY_BATCH_MAX {
                        flush_history_batch(&mut batch, &context).await;
                    }
                }
                None => break,
            },
            _ = flush_timer.tick() => {
                if !batch.is_empty() {
                    flush_history_batch(&mut batch, &context).await;
                }
            }
        }
    }

    while let Ok(point) = rx.try_recv() {
        batch.push(point);
        if batch.len() >= HISTORY_BATCH_MAX {
            flush_history_batch(&mut batch, &context).await;
        }
    }
    if !batch.is_empty() {
        flush_history_batch(&mut batch, &context).await;
    }
    debug!("history writer task exited");
}

/// 把 batch 里的所有样本 + 可能的清理任务,放进一个 SQLite 事务里提交。
async fn flush_history_batch(batch: &mut Vec<HistoryPoint>, context: &WriterContext) {
    if batch.is_empty() {
        return;
    }
    let points = std::mem::take(batch);
    let db_path = Arc::clone(&context.db_path);
    let connection_arc = Arc::clone(&context.write_connection);
    let artifacts_hardened = Arc::clone(&context.artifacts_hardened_after_write);
    let prune_before = should_prune_now(&context.last_pruned_at);

    let result = tokio::task::spawn_blocking(move || {
        let mut guard = connection_arc.blocking_lock();
        let Some(ref mut connection) = *guard else {
            anyhow::bail!("history connection not initialized");
        };
        write_history_batch(
            db_path.as_ref(),
            connection,
            &points,
            prune_before,
            artifacts_hardened.as_ref(),
        )
    })
    .await;

    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            warn!(error = ?error, "failed to persist history batch");
        }
        Err(error) => {
            warn!(error = ?error, "history writer batch task join failed");
        }
    }
}

/// 判断本轮 flush 是否需要顺带做一次 DELETE。基于原子时间戳的 CAS,
/// 多次同时调用不会重复触发,且 writer 是单线程消费者,这里实际上 race-free。
fn should_prune_now(last_pruned_at: &AtomicI64) -> Option<DateTime<Utc>> {
    let now_ts = Utc::now().timestamp();
    let prev_ts = last_pruned_at.load(Ordering::Relaxed);
    if prev_ts > 0 && now_ts.saturating_sub(prev_ts) < HISTORY_PRUNE_MIN_INTERVAL.as_secs() as i64 {
        return None;
    }
    last_pruned_at.store(now_ts, Ordering::Relaxed);
    Some(Utc::now() - chrono::Duration::hours(DEFAULT_HISTORY_RETENTION_HOURS as i64))
}

/// 写入一批历史记录,同时按需删除过期记录。
///
/// 关键点:
/// - 所有 INSERT 与可选 DELETE 都在同一个 `BEGIN ... COMMIT` 内,
///   把 N 次 fsync 折叠成 1 次 — 这是 #32 修复的核心改进;
/// - 失败时整个事务回滚,不会留下部分写入的状态;
/// - 沿用 `with_sqlite_busy_retry` 来吸收 BUSY/LOCKED 冲突。
fn write_history_batch(
    db_path: &PathBuf,
    connection: &mut Connection,
    points: &[HistoryPoint],
    prune_before: Option<DateTime<Utc>>,
    artifacts_hardened_after_write: &AtomicBool,
) -> Result<()> {
    if points.is_empty() {
        return Ok(());
    }
    with_sqlite_busy_retry(|| {
        let tx = connection.transaction()?;
        {
            let mut insert = tx.prepare_cached(
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
            )?;
            for point in points {
                insert.execute(params![
                    &point.node_id,
                    point.recorded_at.timestamp(),
                    point.cpu_usage_percent,
                    point.memory_used_percent,
                    point.rx_bytes_per_sec,
                    point.tx_bytes_per_sec,
                    point.latency_ms,
                    point.disk_used_percent,
                ])?;
            }
        }
        if let Some(cutoff) = prune_before {
            tx.execute(
                "DELETE FROM history_points WHERE recorded_at < ?1",
                params![cutoff.timestamp()],
            )?;
        }
        tx.commit()?;
        Ok(())
    })?;
    if !artifacts_hardened_after_write.load(Ordering::Relaxed) {
        harden_database_artifacts(db_path)?;
        artifacts_hardened_after_write.store(true, Ordering::Relaxed);
    }

    Ok(())
}

/// 单点写入,仅用于单元测试中"绕过 writer task 直接验证 SQL 行为"的场景。
/// 生产路径请使用 `HistoryStore::record_status` → writer task → `write_history_batch`。
#[cfg(test)]
pub(super) fn write_history_point(
    db_path: &PathBuf,
    connection: &mut Connection,
    point: &HistoryPoint,
    prune_before: Option<DateTime<Utc>>,
    artifacts_hardened_after_write: &AtomicBool,
) -> Result<()> {
    write_history_batch(
        db_path,
        connection,
        std::slice::from_ref(point),
        prune_before,
        artifacts_hardened_after_write,
    )
}

/// 由实时 `NodeStatus` 构造一条历史采样点;若节点尚无快照则返回 `None`。
pub(super) fn build_history_point(status: &NodeStatus) -> Option<HistoryPoint> {
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

pub(super) fn sqlite_busy_retry_delay(attempt: u32) -> Duration {
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
