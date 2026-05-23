use std::convert::Infallible;
use std::path::{Path, PathBuf};

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::{TimeZone, Utc};
use futures::stream;
use serde::{Deserialize, Serialize};
use tracing::error;

use crate::AppState;
use crate::audit::{AuditEventType, AuditLogError, AuditQuery};
use crate::handlers::metrics_exporter::{
    RuntimeMetrics, WriterMetrics, render_agent_log_metrics, render_api_cache_metrics,
    render_metrics_response_body_bytes, render_runtime_metrics, render_writer_metrics,
};
use crate::history::HistoryError;
use nodelite_proto::AgentLogEntry;

const DEFAULT_HISTORY_WINDOW_HOURS: u64 = 24;
const DEFAULT_HISTORY_MAX_POINTS: usize = 480;
const MAX_HISTORY_MAX_POINTS: usize = 1440;
const PROMETHEUS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";
const DEFAULT_NODE_LOG_LIMIT: usize = 120;
const MAX_NODE_LOG_LIMIT: usize = 200;

#[derive(Debug, Serialize)]
struct BootstrapResponse {
    service: &'static str,
    status: &'static str,
    ready: bool,
    history_available: bool,
    public_base_url: String,
    refresh_interval_secs: u64,
    registered_nodes: usize,
}

#[derive(Debug, Deserialize)]
pub(crate) struct HistoryQuery {
    window_hours: Option<u64>,
    max_points: Option<usize>,
    start: Option<i64>,
    end: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct NodeLogsQuery {
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AuditLogQuery {
    start: Option<i64>,
    end: Option<i64>,
    event_type: Option<String>,
    success: Option<bool>,
    limit: Option<usize>,
}

/// 提供给前端读取的"引导信息":服务名、刷新周期与已登记节点数。
pub(crate) async fn bootstrap(State(state): State<AppState>) -> impl IntoResponse {
    Json(BootstrapResponse {
        service: "nodelite-server",
        status: state.readiness.status_label(),
        ready: state.readiness.is_ready(),
        history_available: state.readiness.history_available(),
        public_base_url: state.shared.config().public_base_url.clone(),
        refresh_interval_secs: state.shared.config().refresh_interval_secs,
        registered_nodes: state.registry.count().await,
    })
}

/// 仪表盘顶部的总览数据。
pub(crate) async fn overview(State(state): State<AppState>) -> Response {
    match state.shared.overview_json_bytes().await {
        Ok(body) => (
            [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
            body,
        )
            .into_response(),
        Err(error) => {
            error!(error = ?error, "failed to serialize overview response");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to render overview",
            )
                .into_response()
        }
    }
}

/// 所有节点的最新状态。
pub(crate) async fn nodes(State(state): State<AppState>) -> Response {
    match state.shared.nodes_json_bytes().await {
        Ok(body) => (
            [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
            body,
        )
            .into_response(),
        Err(error) => {
            error!(error = ?error, "failed to serialize nodes response");
            (StatusCode::INTERNAL_SERVER_ERROR, "failed to render nodes").into_response()
        }
    }
}

/// Prometheus 指标导出,供外部监控抓取全局概览与节点在线状态。
pub(crate) async fn metrics(State(state): State<AppState>) -> Response {
    let cached_body = state.shared.metrics_text(&state.readiness).await;
    let (history_queue_depth, history_queue_capacity) = state.history.writer_queue_metrics().await;
    let (audit_queue_depth, audit_queue_capacity) = state.audit_log.writer_queue_metrics().await;
    let writer_metrics = render_writer_metrics(WriterMetrics {
        history_dropped_writes: state.history.dropped_writes(),
        history_queue_depth,
        history_queue_capacity,
        audit_dropped_writes: state.audit_log.dropped_writes(),
        audit_queue_depth,
        audit_queue_capacity,
        audit_write_failures: state.audit_log.write_failures(),
        session_control_queue_full_total: state.shared.session_control_queue_full_total(),
    });
    let api_cache_metrics = render_api_cache_metrics(state.shared.api_cache_metrics());
    let agent_log_metrics = render_agent_log_metrics(state.agent_logs.stats().await);
    let config = state.shared.config();
    let (history_db_bytes, history_wal_bytes, history_shm_bytes) =
        sqlite_artifact_sizes(config.history_db_path.as_path()).await;
    let (audit_db_bytes, audit_wal_bytes, audit_shm_bytes) =
        sqlite_artifact_sizes(config.audit.db_path.as_path()).await;
    let registry_entries = state.registry.count().await as u64;
    let runtime_metrics = render_runtime_metrics(RuntimeMetrics {
        process_resident_memory_bytes: process_resident_memory_bytes(),
        history_db_bytes,
        history_wal_bytes,
        history_shm_bytes,
        audit_db_bytes,
        audit_wal_bytes,
        audit_shm_bytes,
        registry_nodes: registry_entries,
        registry_disk_entries_total: registry_entries,
        ws_messages: state.shared.ws_message_metrics(),
    });
    let dynamic_body = metrics_dynamic_body(
        cached_body.len(),
        format!("{writer_metrics}{api_cache_metrics}{agent_log_metrics}{runtime_metrics}"),
    );
    let body = Body::from_stream(stream::iter([
        Ok::<Bytes, Infallible>(cached_body),
        Ok(dynamic_body),
    ]));
    (
        [
            (header::CONTENT_TYPE, PROMETHEUS_CONTENT_TYPE),
            (header::CACHE_CONTROL, "no-store, no-cache, must-revalidate"),
            (header::PRAGMA, "no-cache"),
        ],
        body,
    )
        .into_response()
}

fn metrics_dynamic_body(cached_body_len: usize, mut dynamic_body: String) -> Bytes {
    let mut response_body_bytes = cached_body_len.saturating_add(dynamic_body.len());
    loop {
        let response_metric = render_metrics_response_body_bytes(response_body_bytes as u64);
        let next_response_body_bytes = cached_body_len
            .saturating_add(dynamic_body.len())
            .saturating_add(response_metric.len());
        if next_response_body_bytes == response_body_bytes {
            dynamic_body.push_str(&response_metric);
            return Bytes::from(dynamic_body);
        }
        response_body_bytes = next_response_body_bytes;
    }
}

async fn sqlite_artifact_sizes(path: &Path) -> (u64, u64, u64) {
    let wal_path = sqlite_sidecar_path(path, "wal");
    let shm_path = sqlite_sidecar_path(path, "shm");
    (
        file_len(path).await,
        file_len(wal_path.as_path()).await,
        file_len(shm_path.as_path()).await,
    )
}

fn sqlite_sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut path = path.as_os_str().to_os_string();
    path.push(format!("-{suffix}"));
    path.into()
}

async fn file_len(path: &Path) -> u64 {
    tokio::fs::metadata(path)
        .await
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

fn process_resident_memory_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let resident_pages = statm.split_whitespace().nth(1)?.parse::<u64>().ok()?;
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if page_size <= 0 {
            return None;
        }
        resident_pages.checked_mul(page_size as u64)
    }

    #[cfg(target_os = "macos")]
    {
        let mut info = std::mem::MaybeUninit::<libc::proc_taskinfo>::zeroed();
        let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
        let status = unsafe {
            libc::proc_pidinfo(
                libc::getpid(),
                libc::PROC_PIDTASKINFO,
                0,
                info.as_mut_ptr().cast(),
                size,
            )
        };
        if status != size {
            return None;
        }
        let info = unsafe { info.assume_init() };
        Some(info.pti_resident_size)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

/// 审计日志查询接口。默认按时间倒序返回最近 100 条。
pub(crate) async fn audit_log(
    State(state): State<AppState>,
    Query(query): Query<AuditLogQuery>,
) -> Response {
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let start = match query.start {
        Some(start) => match Utc.timestamp_opt(start, 0).single() {
            Some(value) => Some(value),
            None => {
                return (StatusCode::BAD_REQUEST, "invalid audit start timestamp").into_response();
            }
        },
        None => None,
    };
    let end = match query.end {
        Some(end) => match Utc.timestamp_opt(end, 0).single() {
            Some(value) => Some(value),
            None => {
                return (StatusCode::BAD_REQUEST, "invalid audit end timestamp").into_response();
            }
        },
        None => None,
    };
    if end.zip(start).is_some_and(|(end, start)| end < start) {
        return (StatusCode::BAD_REQUEST, "audit end must be after start").into_response();
    }

    let event_type = match query.event_type.as_deref() {
        Some(raw) => match AuditEventType::parse(raw) {
            Some(value) => Some(value),
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    format!("invalid audit event type: {raw}"),
                )
                    .into_response();
            }
        },
        None => None,
    };

    match state
        .audit_log
        .query(AuditQuery {
            start,
            end,
            event_type,
            success: query.success,
            limit,
        })
        .await
    {
        Ok(events) => Json(events).into_response(),
        Err(AuditLogError::Disabled) => {
            (StatusCode::SERVICE_UNAVAILABLE, "audit log is disabled").into_response()
        }
        Err(AuditLogError::Query(error)) => {
            error!(error = ?error, "failed to query audit log");
            (StatusCode::SERVICE_UNAVAILABLE, "audit log unavailable").into_response()
        }
    }
}

/// 单个节点的最新状态;不存在时返回 404。
pub(crate) async fn node_status(
    State(state): State<AppState>,
    AxumPath(node_id): AxumPath<String>,
) -> Response {
    match state.shared.get_status(&node_id).await {
        Some(status) => Json(status).into_response(),
        None => (StatusCode::NOT_FOUND, "node not found").into_response(),
    }
}

/// 节点历史趋势接口。支持"过去 N 小时"或"指定区间"两种调用方式。
pub(crate) async fn node_history(
    State(state): State<AppState>,
    AxumPath(node_id): AxumPath<String>,
    Query(query): Query<HistoryQuery>,
) -> Response {
    let max_points = query
        .max_points
        .unwrap_or(DEFAULT_HISTORY_MAX_POINTS)
        .clamp(60, MAX_HISTORY_MAX_POINTS);

    let history_result = match (query.start, query.end) {
        (Some(start), Some(end)) => {
            let Some(start_at) = Utc.timestamp_opt(start, 0).single() else {
                return (StatusCode::BAD_REQUEST, "invalid history start timestamp")
                    .into_response();
            };
            let Some(end_at) = Utc.timestamp_opt(end, 0).single() else {
                return (StatusCode::BAD_REQUEST, "invalid history end timestamp").into_response();
            };
            if end_at <= start_at {
                return (StatusCode::BAD_REQUEST, "history end must be after start")
                    .into_response();
            }
            state
                .history
                .query_history_range(&node_id, start_at, end_at, max_points)
                .await
        }
        (None, None) => {
            let window_hours = query.window_hours.unwrap_or(DEFAULT_HISTORY_WINDOW_HOURS);
            state
                .history
                .query_history(&node_id, window_hours, max_points)
                .await
        }
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                "history start and end must be provided together",
            )
                .into_response();
        }
    };

    match history_result {
        Ok(points) => Json(points).into_response(),
        Err(error) => {
            let history_error = match &error {
                HistoryError::ConnectionNotInitialized => "connection_not_initialized",
                HistoryError::Query(_) => "query_failed",
                HistoryError::TaskFailed(_) => "task_failed",
            };
            error!(
                node_id = %node_id,
                history_error,
                error = ?error,
                "failed to query node history"
            );
            (StatusCode::SERVICE_UNAVAILABLE, "history store unavailable").into_response()
        }
    }
}

/// 节点最近的 Agent 运行日志。用于排查断链、重连、token 续期等偶发问题。
pub(crate) async fn node_logs(
    State(state): State<AppState>,
    AxumPath(node_id): AxumPath<String>,
    Query(query): Query<NodeLogsQuery>,
) -> Json<Vec<AgentLogEntry>> {
    let limit = query
        .limit
        .unwrap_or(DEFAULT_NODE_LOG_LIMIT)
        .clamp(1, MAX_NODE_LOG_LIMIT);
    Json(state.agent_logs.list(&node_id, limit).await)
}
