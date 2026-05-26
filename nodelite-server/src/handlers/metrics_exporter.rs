use std::collections::HashSet;
use std::fmt::Write;

use crate::ServerReadiness;
use crate::agent_logs::AgentLogStats;
use nodelite_proto::{NodeSnapshot, NodeStatus, OverviewData};

#[cfg(test)]
pub(crate) fn render_prometheus_metrics(
    readiness: &ServerReadiness,
    statuses: &[NodeStatus],
    overview: &OverviewData,
) -> String {
    render_prometheus_metrics_from_iter(readiness, statuses.iter(), overview)
}

pub(crate) fn render_prometheus_metrics_from_iter<'a>(
    readiness: &ServerReadiness,
    statuses: impl IntoIterator<Item = &'a NodeStatus>,
    overview: &OverviewData,
) -> String {
    let mut emitter = MetricEmitter::default();
    render_server_metrics(&mut emitter, readiness);
    render_overview_metrics(&mut emitter, overview);
    for status in statuses {
        render_node_metrics(&mut emitter, status);
    }
    emitter.finish()
}

#[derive(Clone, Copy)]
pub(crate) struct WriterMetrics {
    pub(crate) history_dropped_writes: u64,
    pub(crate) history_queue_depth: u64,
    pub(crate) history_queue_capacity: u64,
    pub(crate) audit_dropped_writes: u64,
    pub(crate) audit_queue_depth: u64,
    pub(crate) audit_queue_capacity: u64,
    pub(crate) audit_write_failures: u64,
    pub(crate) session_control_queue_full_total: u64,
}

pub(crate) fn render_writer_metrics(metrics: WriterMetrics) -> String {
    let mut emitter = MetricEmitter::default();
    emitter.counter(
        "nodelite_history_dropped_writes_total",
        "Number of history samples dropped because the writer queue was full.",
        &[],
        metrics.history_dropped_writes,
    );
    emitter.gauge(
        "nodelite_history_queue_depth",
        "Number of history samples waiting in the writer queue.",
        &[],
        metrics.history_queue_depth,
    );
    emitter.gauge(
        "nodelite_history_queue_capacity",
        "Maximum number of history samples accepted by the writer queue.",
        &[],
        metrics.history_queue_capacity,
    );
    emitter.counter(
        "nodelite_audit_dropped_writes_total",
        "Number of audit events dropped because the writer queue was full.",
        &[],
        metrics.audit_dropped_writes,
    );
    emitter.gauge(
        "nodelite_audit_queue_depth",
        "Number of audit commands waiting in the writer queue.",
        &[],
        metrics.audit_queue_depth,
    );
    emitter.gauge(
        "nodelite_audit_queue_capacity",
        "Maximum number of audit commands accepted by the writer queue.",
        &[],
        metrics.audit_queue_capacity,
    );
    emitter.counter(
        "nodelite_audit_write_failures_total",
        "Number of audit writer failures while enqueueing or persisting events.",
        &[],
        metrics.audit_write_failures,
    );
    emitter.counter(
        "nodelite_session_control_queue_full_total",
        "Number of session control commands rejected because a bounded queue was full.",
        &[],
        metrics.session_control_queue_full_total,
    );
    emitter.finish()
}

pub(crate) fn render_agent_log_metrics(stats: AgentLogStats) -> String {
    let mut emitter = MetricEmitter::default();
    emitter.gauge(
        "nodelite_agent_log_nodes",
        "Number of nodes currently holding in-memory agent logs.",
        &[],
        stats.nodes,
    );
    emitter.gauge(
        "nodelite_agent_log_entries",
        "Number of in-memory agent log entries currently retained.",
        &[],
        stats.entries,
    );
    emitter.gauge(
        "nodelite_agent_log_estimated_bytes",
        "Estimated bytes currently retained by the in-memory agent log store.",
        &[],
        stats.estimated_bytes,
    );
    emitter.finish()
}

#[derive(Clone, Copy)]
pub(crate) struct ApiCacheMetrics {
    pub(crate) nodes_hits: u64,
    pub(crate) nodes_misses: u64,
    pub(crate) nodes_body_bytes: u64,
    pub(crate) overview_hits: u64,
    pub(crate) overview_misses: u64,
    pub(crate) overview_body_bytes: u64,
    pub(crate) metrics_hits: u64,
    pub(crate) metrics_misses: u64,
    pub(crate) metrics_body_bytes: u64,
}

pub(crate) fn render_api_cache_metrics(metrics: ApiCacheMetrics) -> String {
    let mut emitter = MetricEmitter::default();
    for (kind, hits, misses, body_bytes) in [
        (
            "nodes",
            metrics.nodes_hits,
            metrics.nodes_misses,
            metrics.nodes_body_bytes,
        ),
        (
            "overview",
            metrics.overview_hits,
            metrics.overview_misses,
            metrics.overview_body_bytes,
        ),
        (
            "metrics",
            metrics.metrics_hits,
            metrics.metrics_misses,
            metrics.metrics_body_bytes,
        ),
    ] {
        emitter.counter(
            "nodelite_api_cache_hits_total",
            "Number of cached API response body hits.",
            &[("kind", kind)],
            hits,
        );
        emitter.counter(
            "nodelite_api_cache_misses_total",
            "Number of cached API response body misses.",
            &[("kind", kind)],
            misses,
        );
        emitter.gauge(
            "nodelite_api_body_bytes",
            "Size in bytes of the most recently built cached API response body.",
            &[("kind", kind)],
            body_bytes,
        );
        emitter.counter(
            "nodelite_view_cache_hits_total",
            "Number of cached HTTP view response body hits.",
            &[("kind", kind)],
            hits,
        );
        emitter.counter(
            "nodelite_view_cache_misses_total",
            "Number of cached HTTP view response body misses.",
            &[("kind", kind)],
            misses,
        );
    }
    emitter.gauge(
        "nodelite_metrics_body_bytes",
        "Size in bytes of the most recently built cached base /metrics response body.",
        &[],
        metrics.metrics_body_bytes,
    );
    emitter.finish()
}

#[derive(Clone, Copy)]
pub(crate) struct WsMessageMetrics {
    pub(crate) metrics_total: u64,
    pub(crate) agent_logs_total: u64,
    pub(crate) pong_total: u64,
    pub(crate) refresh_token_request_total: u64,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct SqliteWalCheckpointStats {
    pub(crate) observed: bool,
    pub(crate) active: bool,
    pub(crate) busy: u64,
    pub(crate) log_pages: u64,
    pub(crate) checkpointed_pages: u64,
}

impl SqliteWalCheckpointStats {
    fn backlog_pages(self) -> u64 {
        self.log_pages.saturating_sub(self.checkpointed_pages)
    }
}

#[derive(Clone, Copy, Default)]
pub(crate) struct SqliteWalCheckpointMetrics {
    pub(crate) history: SqliteWalCheckpointStats,
    pub(crate) audit: SqliteWalCheckpointStats,
}

#[derive(Clone, Copy)]
pub(crate) struct RuntimeMetrics {
    pub(crate) process_resident_memory_bytes: Option<u64>,
    pub(crate) history_db_bytes: u64,
    pub(crate) history_wal_bytes: u64,
    pub(crate) history_shm_bytes: u64,
    pub(crate) audit_db_bytes: u64,
    pub(crate) audit_wal_bytes: u64,
    pub(crate) audit_shm_bytes: u64,
    pub(crate) sqlite_wal_checkpoint: SqliteWalCheckpointMetrics,
    pub(crate) registry_nodes: u64,
    pub(crate) registry_disk_entries_total: u64,
    pub(crate) ws_messages: WsMessageMetrics,
}

pub(crate) fn render_runtime_metrics(metrics: RuntimeMetrics) -> String {
    let mut emitter = MetricEmitter::default();
    if let Some(bytes) = metrics.process_resident_memory_bytes {
        emitter.gauge(
            "nodelite_process_resident_memory_bytes",
            "Resident set size of the nodelite-server process.",
            &[],
            bytes,
        );
    }
    for (kind, bytes) in [
        ("history_db", metrics.history_db_bytes),
        ("history_wal", metrics.history_wal_bytes),
        ("history_shm", metrics.history_shm_bytes),
        ("audit_db", metrics.audit_db_bytes),
        ("audit_wal", metrics.audit_wal_bytes),
        ("audit_shm", metrics.audit_shm_bytes),
    ] {
        emitter.gauge(
            "nodelite_sqlite_file_bytes",
            "Size in bytes of NodeLite SQLite database and journal artifacts.",
            &[("kind", kind)],
            bytes,
        );
    }
    emitter.gauge(
        "nodelite_history_db_bytes",
        "Size in bytes of the history SQLite database file.",
        &[],
        metrics.history_db_bytes,
    );
    emitter.gauge(
        "nodelite_history_wal_bytes",
        "Size in bytes of the history SQLite WAL file.",
        &[],
        metrics.history_wal_bytes,
    );
    render_sqlite_wal_checkpoint_metrics(&mut emitter, metrics.sqlite_wal_checkpoint);
    emitter.gauge(
        "nodelite_registry_nodes",
        "Number of registered nodes currently loaded in memory.",
        &[],
        metrics.registry_nodes,
    );
    emitter.gauge(
        "nodelite_registry_disk_entries_total",
        "Number of disk entries currently held across node snapshots.",
        &[],
        metrics.registry_disk_entries_total,
    );
    for (kind, total) in [
        ("metrics", metrics.ws_messages.metrics_total),
        ("agent_logs", metrics.ws_messages.agent_logs_total),
        ("pong", metrics.ws_messages.pong_total),
        (
            "refresh_token_request",
            metrics.ws_messages.refresh_token_request_total,
        ),
    ] {
        emitter.counter(
            "nodelite_ws_messages_total",
            "Number of authenticated WebSocket messages handled by type.",
            &[("type", kind)],
            total,
        );
    }
    emitter.finish()
}

fn render_sqlite_wal_checkpoint_metrics(
    emitter: &mut MetricEmitter,
    metrics: SqliteWalCheckpointMetrics,
) {
    for (database, stats) in [("history", metrics.history), ("audit", metrics.audit)] {
        emitter.gauge(
            "nodelite_sqlite_wal_checkpoint_observed",
            "Whether the latest passive SQLite WAL checkpoint probe succeeded.",
            &[("database", database)],
            if stats.observed { 1 } else { 0 },
        );
        emitter.gauge(
            "nodelite_sqlite_wal_checkpoint_active",
            "Whether the SQLite database is currently using WAL journal mode.",
            &[("database", database)],
            if stats.active { 1 } else { 0 },
        );
        emitter.gauge(
            "nodelite_sqlite_wal_checkpoint_busy",
            "Busy flag returned by PRAGMA wal_checkpoint(PASSIVE).",
            &[("database", database)],
            stats.busy,
        );
        for (state, pages) in [
            ("log", stats.log_pages),
            ("checkpointed", stats.checkpointed_pages),
            ("backlog", stats.backlog_pages()),
        ] {
            emitter.gauge(
                "nodelite_sqlite_wal_checkpoint_pages",
                "SQLite WAL checkpoint page counts from PRAGMA wal_checkpoint(PASSIVE).",
                &[("database", database), ("state", state)],
                pages,
            );
        }
    }
}

pub(crate) fn render_metrics_response_body_bytes(bytes: u64) -> String {
    let mut emitter = MetricEmitter::default();
    emitter.gauge(
        "nodelite_metrics_response_body_bytes",
        "Size in bytes of the uncompressed /metrics response body before HTTP compression.",
        &[],
        bytes,
    );
    emitter.finish()
}

fn render_server_metrics(emitter: &mut MetricEmitter, readiness: &ServerReadiness) {
    emitter.gauge(
        "nodelite_server_ready",
        "Whether the NodeLite server is ready to serve protected traffic.",
        &[],
        if readiness.is_ready() { 1 } else { 0 },
    );
    emitter.gauge(
        "nodelite_history_available",
        "Whether the history store is currently available.",
        &[],
        if readiness.history_available() { 1 } else { 0 },
    );
    emitter.gauge(
        "nodelite_registry_reload_healthy",
        "Whether the registry reload loop is currently healthy.",
        &[],
        if readiness.registry_reload_healthy() {
            1
        } else {
            0
        },
    );
}

fn render_overview_metrics(emitter: &mut MetricEmitter, overview: &OverviewData) {
    emitter.gauge(
        "nodelite_nodes_total",
        "Number of registered nodes known to the dashboard.",
        &[],
        overview.total_nodes,
    );
    emitter.gauge(
        "nodelite_nodes_online",
        "Number of nodes currently considered online.",
        &[],
        overview.online_nodes,
    );
    emitter.gauge(
        "nodelite_nodes_offline",
        "Number of nodes currently considered offline.",
        &[],
        overview.offline_nodes,
    );
    emitter.counter(
        "nodelite_network_total_bytes",
        "Summed network byte counters reported by all nodes.",
        &[("direction", "rx")],
        overview.total_rx_bytes,
    );
    emitter.counter(
        "nodelite_network_total_bytes",
        "Summed network byte counters reported by all nodes.",
        &[("direction", "tx")],
        overview.total_tx_bytes,
    );
    emitter.gauge(
        "nodelite_network_rate_bytes_per_second",
        "Summed instantaneous network rate reported by all nodes.",
        &[("direction", "rx")],
        overview.current_rx_bytes_per_sec,
    );
    emitter.gauge(
        "nodelite_network_rate_bytes_per_second",
        "Summed instantaneous network rate reported by all nodes.",
        &[("direction", "tx")],
        overview.current_tx_bytes_per_sec,
    );
    if let Some(average_latency_ms) = overview.average_latency_ms
        && average_latency_ms.is_finite()
    {
        emitter.gauge(
            "nodelite_latency_average_milliseconds",
            "Average latency across online nodes.",
            &[],
            average_latency_ms,
        );
    }
}

fn render_node_metrics(emitter: &mut MetricEmitter, status: &NodeStatus) {
    let node_id = status.identity.node_id.as_str();
    let node_labels = [("node_id", node_id)];
    let node_info_labels = [
        ("node_id", node_id),
        ("node_label", status.identity.node_label.as_str()),
        ("hostname", status.identity.hostname.as_str()),
        ("os", status.identity.os.as_str()),
        ("agent_version", status.identity.agent_version.as_str()),
    ];

    emitter.gauge(
        "nodelite_node_info",
        "Static node metadata exposed as an info metric.",
        &node_info_labels,
        1,
    );
    emitter.gauge(
        "nodelite_node_online",
        "Whether the node is currently online.",
        &node_labels,
        if status.online { 1 } else { 0 },
    );
    if let Some(last_seen) = status.last_seen {
        emitter.gauge(
            "nodelite_node_last_seen_timestamp_seconds",
            "Last time the node was seen by the server as a Unix timestamp.",
            &node_labels,
            last_seen.timestamp(),
        );
    }
    if let Some(latency_ms) = status.latency_ms {
        emitter.gauge(
            "nodelite_node_latency_milliseconds",
            "Latest measured node latency in milliseconds.",
            &node_labels,
            latency_ms,
        );
    }
    if let Some(snapshot) = status.snapshot.as_ref() {
        render_snapshot_metrics(emitter, node_id, snapshot);
    }
}

fn render_snapshot_metrics(emitter: &mut MetricEmitter, node_id: &str, snapshot: &NodeSnapshot) {
    let node_labels = [("node_id", node_id)];
    emitter.gauge(
        "nodelite_node_snapshot_timestamp_seconds",
        "Collection time of the latest node snapshot as a Unix timestamp.",
        &node_labels,
        snapshot.collected_at.timestamp(),
    );
    emitter.gauge(
        "nodelite_node_uptime_seconds",
        "Node uptime in seconds from the latest snapshot.",
        &node_labels,
        snapshot.uptime_secs,
    );
    if let Some(cpu_usage_percent) = snapshot.cpu_usage_percent.filter(|value| value.is_finite()) {
        emitter.gauge(
            "nodelite_node_cpu_usage_ratio",
            "Latest CPU usage ratio reported by the node in the range 0..1.",
            &node_labels,
            cpu_usage_percent / 100.0,
        );
    }
    render_memory_metrics(emitter, node_id, snapshot);
    render_load_metrics(emitter, node_id, snapshot);
    render_network_metrics(emitter, node_id, snapshot);
}

fn render_memory_metrics(emitter: &mut MetricEmitter, node_id: &str, snapshot: &NodeSnapshot) {
    for (state, value) in [
        ("total", snapshot.memory.total_bytes),
        ("used", snapshot.memory.used_bytes),
        ("available", snapshot.memory.available_bytes),
    ] {
        emitter.gauge(
            "nodelite_node_memory_bytes",
            "Latest memory totals reported by the node.",
            &[("node_id", node_id), ("state", state)],
            value,
        );
    }
}

fn render_load_metrics(emitter: &mut MetricEmitter, node_id: &str, snapshot: &NodeSnapshot) {
    for (window, value) in [
        ("1m", snapshot.load.one),
        ("5m", snapshot.load.five),
        ("15m", snapshot.load.fifteen),
    ] {
        emitter.gauge(
            "nodelite_node_load_average",
            "Latest node load average window.",
            &[("node_id", node_id), ("window", window)],
            value,
        );
    }
}

fn render_network_metrics(emitter: &mut MetricEmitter, node_id: &str, snapshot: &NodeSnapshot) {
    for (direction, value) in [
        ("rx", snapshot.network.total_rx_bytes),
        ("tx", snapshot.network.total_tx_bytes),
    ] {
        emitter.counter(
            "nodelite_node_network_bytes_total",
            "Latest aggregate network byte counters reported by the node.",
            &[("node_id", node_id), ("direction", direction)],
            value,
        );
    }
    for (direction, value) in [
        ("rx", snapshot.network.rx_bytes_per_sec),
        ("tx", snapshot.network.tx_bytes_per_sec),
    ] {
        if let Some(value) = value.filter(|value| value.is_finite()) {
            emitter.gauge(
                "nodelite_node_network_rate_bytes_per_second",
                "Latest aggregate network transfer rate reported by the node.",
                &[("node_id", node_id), ("direction", direction)],
                value,
            );
        }
    }
}

#[derive(Clone, Copy)]
enum MetricKind {
    Gauge,
    Counter,
}

impl MetricKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Gauge => "gauge",
            Self::Counter => "counter",
        }
    }
}

#[derive(Default)]
struct MetricEmitter {
    body: String,
    seen_metric_families: HashSet<&'static str>,
}

impl MetricEmitter {
    fn finish(self) -> String {
        self.body
    }

    fn gauge<T: std::fmt::Display>(
        &mut self,
        name: &'static str,
        help: &'static str,
        labels: &[(&str, &str)],
        value: T,
    ) {
        self.metric(MetricKind::Gauge, name, help, labels, value);
    }

    fn counter<T: std::fmt::Display>(
        &mut self,
        name: &'static str,
        help: &'static str,
        labels: &[(&str, &str)],
        value: T,
    ) {
        self.metric(MetricKind::Counter, name, help, labels, value);
    }

    fn metric<T: std::fmt::Display>(
        &mut self,
        kind: MetricKind,
        name: &'static str,
        help: &'static str,
        labels: &[(&str, &str)],
        value: T,
    ) {
        if self.seen_metric_families.insert(name) {
            let _ = writeln!(self.body, "# HELP {name} {help}");
            let _ = writeln!(self.body, "# TYPE {name} {}", kind.as_str());
        }
        self.body.push_str(name);
        if !labels.is_empty() {
            self.body.push('{');
            for (index, (key, raw_value)) in labels.iter().enumerate() {
                if index > 0 {
                    self.body.push(',');
                }
                let escaped = escape_prometheus_label_value(raw_value);
                let _ = write!(self.body, "{key}=\"{escaped}\"");
            }
            self.body.push('}');
        }
        let _ = writeln!(self.body, " {value}");
    }
}

fn escape_prometheus_label_value(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

#[cfg(test)]
mod tests;
