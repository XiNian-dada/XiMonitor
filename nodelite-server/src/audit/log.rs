//! Audit log state machine and async lifecycle management.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, anyhow};
use nodelite_proto::AuditConfig;
use rusqlite::Connection;
use tokio::sync::{Mutex, RwLock, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::query::query_events;
use super::storage::{
    AUDIT_CHANNEL_CAPACITY, AUDIT_PRUNE_INTERVAL, open_audit_connection, prune_expired_records,
};
use super::writer::{AuditWriterCommand, AuditWriterContext, run_audit_writer};
use super::{AuditEvent, AuditEventType, AuditLogError, AuditQuery, NewAuditEvent};

#[derive(Clone)]
pub struct AuditLog {
    config: Arc<AuditConfig>,
    sqlite_busy_timeout_secs: u64,
    /// 持久化 SQLite 连接。审计写入属于安全 hot path,不能在每次认证失败时反复 open/prune/chmod。
    connection: Arc<Mutex<Option<Connection>>>,
    writer_tx: Arc<RwLock<Option<mpsc::Sender<AuditWriterCommand>>>>,
    writer_handle: Arc<Mutex<Option<JoinHandle<()>>>>,
    dropped_writes: Arc<AtomicU64>,
    write_failures: Arc<AtomicU64>,
}

impl AuditLog {
    pub fn new(config: AuditConfig, sqlite_busy_timeout_secs: u64) -> Self {
        Self {
            config: Arc::new(config),
            sqlite_busy_timeout_secs,
            connection: Arc::new(Mutex::new(None)),
            writer_tx: Arc::new(RwLock::new(None)),
            writer_handle: Arc::new(Mutex::new(None)),
            dropped_writes: Arc::new(AtomicU64::new(0)),
            write_failures: Arc::new(AtomicU64::new(0)),
        }
    }

    pub async fn initialize(&self) -> Result<()> {
        if !self.config.enabled {
            return Ok(());
        }

        let config = Arc::clone(&self.config);
        let sqlite_busy_timeout_secs = self.sqlite_busy_timeout_secs;
        let connection = tokio::task::spawn_blocking(move || {
            let connection = open_audit_connection(&config.db_path, sqlite_busy_timeout_secs)?;
            prune_expired_records(&connection, config.retention_days)?;
            Ok::<Connection, anyhow::Error>(connection)
        })
        .await
        .context("audit log initialization task failed")??;

        let mut guard = self.connection.lock().await;
        *guard = Some(connection);
        drop(guard);
        self.spawn_writer_task().await;
        Ok(())
    }

    async fn spawn_writer_task(&self) {
        let (tx, rx) = mpsc::channel::<AuditWriterCommand>(AUDIT_CHANNEL_CAPACITY);
        {
            let mut guard = self.writer_tx.write().await;
            *guard = Some(tx);
        }

        let context = AuditWriterContext {
            connection: Arc::clone(&self.connection),
            write_failures: Arc::clone(&self.write_failures),
        };
        let handle = tokio::spawn(run_audit_writer(rx, context));
        let mut guard = self.writer_handle.lock().await;
        *guard = Some(handle);
    }

    pub async fn record(&self, event: NewAuditEvent) -> Result<()> {
        if !self.should_record(event.event_type) {
            return Ok(());
        }

        let guard = self.writer_tx.read().await;
        let Some(tx) = guard.as_ref() else {
            anyhow::bail!("audit writer is not initialized");
        };
        match tx.try_send(AuditWriterCommand::Event(event)) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.dropped_writes.fetch_add(1, Ordering::Relaxed);
                warn!(
                    capacity = AUDIT_CHANNEL_CAPACITY,
                    dropped_total = self.dropped_writes.load(Ordering::Relaxed),
                    "audit writer queue full; dropping event"
                );
                Ok(())
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.write_failures.fetch_add(1, Ordering::Relaxed);
                anyhow::bail!("audit writer is closed");
            }
        }
    }

    pub async fn record_best_effort(&self, event: NewAuditEvent) {
        let event_type = event.event_type;
        if let Err(error) = self.record(event).await {
            warn!(event_type = event_type.as_str(), error = ?error, "failed to persist audit event");
        }
    }

    pub(crate) fn dropped_writes(&self) -> u64 {
        self.dropped_writes.load(Ordering::Relaxed)
    }

    pub(crate) fn write_failures(&self) -> u64 {
        self.write_failures.load(Ordering::Relaxed)
    }

    pub(crate) async fn writer_queue_metrics(&self) -> (u64, u64) {
        let guard = self.writer_tx.read().await;
        let Some(tx) = guard.as_ref() else {
            return (0, 0);
        };
        let capacity = tx.max_capacity();
        let depth = capacity.saturating_sub(tx.capacity());
        (depth as u64, capacity as u64)
    }

    pub async fn query(&self, query: AuditQuery) -> Result<Vec<AuditEvent>, AuditLogError> {
        if !self.config.enabled {
            return Err(AuditLogError::Disabled);
        }
        self.flush_pending().await.map_err(AuditLogError::Query)?;
        let connection = Arc::clone(&self.connection);
        tokio::task::spawn_blocking(move || {
            let guard = connection.blocking_lock();
            let Some(ref connection) = *guard else {
                return Err(AuditLogError::Query(anyhow!(
                    "audit connection not initialized"
                )));
            };
            query_events(connection, &query)
        })
        .await
        .map_err(|error| AuditLogError::Query(anyhow!("audit log query task failed: {error}")))?
    }

    async fn flush_pending(&self) -> Result<()> {
        let tx = {
            let guard = self.writer_tx.read().await;
            guard.as_ref().cloned()
        };
        let Some(tx) = tx else {
            return Ok(());
        };

        let (ack_tx, ack_rx) = oneshot::channel();
        tx.send(AuditWriterCommand::Flush(ack_tx))
            .await
            .context("audit writer is closed")?;
        ack_rx.await.context("audit writer flush was cancelled")
    }

    pub(crate) fn spawn_pruner(&self, shutdown: CancellationToken) -> JoinHandle<()> {
        let audit_log = self.clone();
        tokio::spawn(async move {
            let mut ticker = interval(AUDIT_PRUNE_INTERVAL);
            ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    _ = ticker.tick() => {
                        match audit_log.prune_expired().await {
                            Ok(pruned) => {
                                if pruned > 0 {
                                    info!(pruned, "pruned expired audit records");
                                }
                            }
                            Err(error) => {
                                warn!(error = ?error, "failed to prune expired audit records");
                            }
                        }
                    }
                }
            }
        })
    }

    pub(crate) async fn prune_expired(&self) -> Result<usize> {
        if !self.config.enabled {
            return Ok(0);
        }

        let retention_days = self.config.retention_days;
        let connection = Arc::clone(&self.connection);
        tokio::task::spawn_blocking(move || {
            let guard = connection.blocking_lock();
            let Some(ref connection) = *guard else {
                anyhow::bail!("audit connection not initialized");
            };
            prune_expired_records(connection, retention_days)
        })
        .await
        .context("audit log prune task failed")?
    }

    pub(crate) async fn shutdown(&self) {
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
            warn!(error = ?error, "audit writer task join failed during shutdown");
        }
    }

    fn should_record(&self, event_type: AuditEventType) -> bool {
        if !self.config.enabled {
            return false;
        }

        match event_type {
            AuditEventType::TotpVerifySuccess | AuditEventType::NodeConnected => {
                self.config.log_successful_auth
            }
            AuditEventType::LoginFailure | AuditEventType::TotpVerifyFailure => {
                self.config.log_failed_auth
            }
            AuditEventType::TokenInvalid => self.config.log_token_events,
            AuditEventType::RateLimitExceeded => self.config.log_rate_limit,
        }
    }
}
