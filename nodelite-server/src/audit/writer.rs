//! 审计日志后台写入任务。

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::time::MissedTickBehavior;
use tracing::{debug, warn};

use super::types::NewAuditEvent;

const AUDIT_BATCH_MAX: usize = 128;
const AUDIT_BATCH_FLUSH_INTERVAL: Duration = Duration::from_millis(100);

pub(super) struct AuditWriterContext {
    pub(super) connection: Arc<Mutex<Option<Connection>>>,
    pub(super) write_failures: Arc<AtomicU64>,
}

pub(super) enum AuditWriterCommand {
    Event(NewAuditEvent),
    Flush(oneshot::Sender<()>),
}

pub(super) async fn run_audit_writer(
    mut rx: mpsc::Receiver<AuditWriterCommand>,
    context: AuditWriterContext,
) {
    let mut batch = Vec::with_capacity(AUDIT_BATCH_MAX);
    let mut flush_timer = tokio::time::interval(AUDIT_BATCH_FLUSH_INTERVAL);
    flush_timer.set_missed_tick_behavior(MissedTickBehavior::Delay);
    flush_timer.tick().await;

    loop {
        tokio::select! {
            biased;
            received = rx.recv() => match received {
                Some(AuditWriterCommand::Event(event)) => {
                    batch.push(event);
                    if batch.len() >= AUDIT_BATCH_MAX {
                        flush_audit_batch(&mut batch, &context).await;
                    }
                }
                Some(AuditWriterCommand::Flush(ack)) => {
                    if !batch.is_empty() {
                        flush_audit_batch(&mut batch, &context).await;
                    }
                    let _ = ack.send(());
                }
                None => break,
            },
            _ = flush_timer.tick() => {
                if !batch.is_empty() {
                    flush_audit_batch(&mut batch, &context).await;
                }
            }
        }
    }

    while let Ok(command) = rx.try_recv() {
        match command {
            AuditWriterCommand::Event(event) => {
                batch.push(event);
                if batch.len() >= AUDIT_BATCH_MAX {
                    flush_audit_batch(&mut batch, &context).await;
                }
            }
            AuditWriterCommand::Flush(ack) => {
                if !batch.is_empty() {
                    flush_audit_batch(&mut batch, &context).await;
                }
                let _ = ack.send(());
            }
        }
    }
    if !batch.is_empty() {
        flush_audit_batch(&mut batch, &context).await;
    }
    debug!("audit writer task exited");
}

async fn flush_audit_batch(batch: &mut Vec<NewAuditEvent>, context: &AuditWriterContext) {
    if batch.is_empty() {
        return;
    }

    let events = std::mem::take(batch);
    let connection = Arc::clone(&context.connection);
    let result = tokio::task::spawn_blocking(move || {
        let mut guard = connection.blocking_lock();
        let Some(ref mut connection) = *guard else {
            anyhow::bail!("audit connection not initialized");
        };
        insert_event_batch(connection, &events)
    })
    .await;

    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            context.write_failures.fetch_add(1, Ordering::Relaxed);
            warn!(error = ?error, "failed to persist audit batch");
        }
        Err(error) => {
            context.write_failures.fetch_add(1, Ordering::Relaxed);
            warn!(error = ?error, "audit writer batch task join failed");
        }
    }
}

fn insert_event_batch(connection: &mut Connection, events: &[NewAuditEvent]) -> Result<()> {
    if events.is_empty() {
        return Ok(());
    }

    let transaction = connection
        .transaction()
        .context("failed to begin audit transaction")?;
    {
        let mut statement = transaction
            .prepare_cached(
                "INSERT INTO audit_log
                 (timestamp, event_type, user, node_id, ip_address, user_agent, success, details)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )
            .context("failed to prepare audit insert")?;

        for event in events {
            let details = serde_json::to_string(&event.details)
                .context("failed to serialize audit details")?;
            statement
                .execute(params![
                    event.timestamp.timestamp(),
                    event.event_type.as_str(),
                    event.user,
                    event.node_id,
                    event.ip_address,
                    event.user_agent,
                    event.success as i64,
                    details,
                ])
                .context("failed to insert audit event")?;
        }
    }
    transaction
        .commit()
        .context("failed to commit audit transaction")?;
    Ok(())
}
