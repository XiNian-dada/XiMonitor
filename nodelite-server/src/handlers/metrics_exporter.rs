use std::collections::HashSet;
use std::fmt::Write;

use crate::ServerReadiness;
use nodelite_proto::{NodeSnapshot, NodeStatus, OverviewData};

pub(crate) fn render_prometheus_metrics(
    readiness: &ServerReadiness,
    statuses: &[NodeStatus],
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
    pub(crate) audit_dropped_writes: u64,
    pub(crate) audit_write_failures: u64,
}

pub(crate) fn render_writer_metrics(metrics: WriterMetrics) -> String {
    let mut emitter = MetricEmitter::default();
    emitter.counter(
        "nodelite_history_dropped_writes_total",
        "Number of history samples dropped because the writer queue was full.",
        &[],
        metrics.history_dropped_writes,
    );
    emitter.counter(
        "nodelite_audit_dropped_writes_total",
        "Number of audit events dropped because the writer queue was full.",
        &[],
        metrics.audit_dropped_writes,
    );
    emitter.counter(
        "nodelite_audit_write_failures_total",
        "Number of audit writer failures while enqueueing or persisting events.",
        &[],
        metrics.audit_write_failures,
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
    }
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
    emitter.gauge(
        "nodelite_node_cpu_usage_ratio",
        "Latest CPU usage ratio reported by the node in the range 0..1.",
        &node_labels,
        snapshot.cpu_usage_percent / 100.0,
    );
    render_memory_metrics(emitter, node_id, snapshot);
    render_disk_metrics(emitter, node_id, snapshot);
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

fn render_disk_metrics(emitter: &mut MetricEmitter, node_id: &str, snapshot: &NodeSnapshot) {
    for disk in &snapshot.disks {
        for (state, value) in [("total", disk.total_bytes), ("used", disk.used_bytes)] {
            emitter.gauge(
                "nodelite_node_disk_bytes",
                "Latest per-mount disk totals reported by the node.",
                &[
                    ("node_id", node_id),
                    ("mount", disk.mount_point.as_str()),
                    ("state", state),
                ],
                value,
            );
        }
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
mod tests {
    use chrono::Utc;

    use super::{
        ApiCacheMetrics, WriterMetrics, render_api_cache_metrics, render_prometheus_metrics,
        render_writer_metrics,
    };
    use crate::ServerReadiness;
    use nodelite_proto::{
        DiskUsage, LoadAverage, MemoryUsage, NetworkCounters, NodeIdentity, NodeSnapshot,
        NodeStatus, OverviewData,
    };

    #[test]
    fn exporter_emits_help_and_type_once_per_family() {
        let readiness = ServerReadiness::new(true);
        let overview = sample_overview();
        let body = render_prometheus_metrics(&readiness, &sample_statuses(), &overview);

        assert_eq!(body.matches("# HELP nodelite_node_online ").count(), 1);
        assert_eq!(body.matches("# TYPE nodelite_node_online gauge").count(), 1);
        assert_eq!(
            body.matches("# HELP nodelite_node_cpu_usage_ratio ")
                .count(),
            1
        );
        assert_eq!(
            body.matches("# TYPE nodelite_node_network_bytes_total counter")
                .count(),
            1
        );
    }

    #[test]
    fn exporter_uses_info_metric_for_mutable_identity_labels() {
        let readiness = ServerReadiness::new(true);
        let overview = sample_overview();
        let body = render_prometheus_metrics(&readiness, &sample_statuses(), &overview);

        assert!(body.contains(
            "nodelite_node_info{node_id=\"node-1\",node_label=\"Node 1\",hostname=\"node-1.internal\",os=\"Linux\",agent_version=\"1.0.0\"} 1"
        ));
        assert!(body.contains("nodelite_node_online{node_id=\"node-1\"} 1"));
        assert!(!body.contains("nodelite_node_online{node_id=\"node-1\",node_label=\"Node 1\""));
    }

    #[test]
    fn exporter_exposes_snapshot_resource_metrics_for_each_node() {
        let readiness = ServerReadiness::new(true);
        let overview = sample_overview();
        let body = render_prometheus_metrics(&readiness, &sample_statuses(), &overview);

        assert_eq!(
            body.lines()
                .filter(|line| line.starts_with("nodelite_node_cpu_usage_ratio{"))
                .count(),
            2
        );
        assert_eq!(
            body.lines()
                .filter(|line| line.starts_with("nodelite_node_memory_bytes{"))
                .count(),
            6
        );
        assert!(body.contains(
            "nodelite_node_disk_bytes{node_id=\"node-1\",mount=\"/\",state=\"used\"} 500"
        ));
        assert!(
            body.contains("nodelite_node_load_average{node_id=\"node-2\",window=\"15m\"} 0.75")
        );
        assert!(body.contains(
            "nodelite_node_network_bytes_total{node_id=\"node-1\",direction=\"rx\"} 1500"
        ));
    }

    #[test]
    fn exporter_exposes_writer_counters() {
        let body = render_writer_metrics(WriterMetrics {
            history_dropped_writes: 3,
            audit_dropped_writes: 5,
            audit_write_failures: 7,
        });

        assert!(body.contains("# TYPE nodelite_history_dropped_writes_total counter"));
        assert!(body.contains("nodelite_history_dropped_writes_total 3"));
        assert!(body.contains("# TYPE nodelite_audit_dropped_writes_total counter"));
        assert!(body.contains("nodelite_audit_dropped_writes_total 5"));
        assert!(body.contains("# TYPE nodelite_audit_write_failures_total counter"));
        assert!(body.contains("nodelite_audit_write_failures_total 7"));
    }

    #[test]
    fn exporter_exposes_api_cache_metrics() {
        let body = render_api_cache_metrics(ApiCacheMetrics {
            nodes_hits: 11,
            nodes_misses: 2,
            nodes_body_bytes: 4096,
            overview_hits: 13,
            overview_misses: 3,
            overview_body_bytes: 256,
        });

        assert!(body.contains("# TYPE nodelite_api_cache_hits_total counter"));
        assert!(body.contains("nodelite_api_cache_hits_total{kind=\"nodes\"} 11"));
        assert!(body.contains("nodelite_api_cache_hits_total{kind=\"overview\"} 13"));
        assert!(body.contains("# TYPE nodelite_api_cache_misses_total counter"));
        assert!(body.contains("nodelite_api_cache_misses_total{kind=\"nodes\"} 2"));
        assert!(body.contains("nodelite_api_cache_misses_total{kind=\"overview\"} 3"));
        assert!(body.contains("# TYPE nodelite_api_body_bytes gauge"));
        assert!(body.contains("nodelite_api_body_bytes{kind=\"nodes\"} 4096"));
        assert!(body.contains("nodelite_api_body_bytes{kind=\"overview\"} 256"));
    }

    fn sample_statuses() -> Vec<NodeStatus> {
        vec![
            sample_status("node-1", "Node 1", 1, 0.5),
            sample_status("node-2", "Node 2", 2, 0.75),
        ]
    }

    fn sample_status(
        node_id: &str,
        node_label: &str,
        uptime_secs: u64,
        load_15m: f64,
    ) -> NodeStatus {
        NodeStatus {
            identity: NodeIdentity {
                node_id: node_id.to_string(),
                node_label: node_label.to_string(),
                hostname: format!("{node_id}.internal"),
                os: "Linux".to_string(),
                kernel_version: Some("6.8.0".to_string()),
                cpu_model: Some("test-cpu".to_string()),
                cpu_cores: 4,
                agent_version: "1.0.0".to_string(),
                boot_time: None,
                tags: vec!["tag".to_string()],
            },
            remote_ip: None,
            snapshot: Some(NodeSnapshot {
                collected_at: Utc::now(),
                cpu_usage_percent: 42.0,
                load: LoadAverage {
                    one: 0.25,
                    five: 0.5,
                    fifteen: load_15m,
                },
                memory: MemoryUsage {
                    total_bytes: 1024,
                    used_bytes: 768,
                    available_bytes: 256,
                    swap_total_bytes: 0,
                    swap_used_bytes: 0,
                },
                uptime_secs,
                disks: vec![DiskUsage {
                    device: "/dev/vda".to_string(),
                    mount_point: "/".to_string(),
                    fs_type: "ext4".to_string(),
                    total_bytes: 1000,
                    available_bytes: 500,
                    used_bytes: 500,
                    used_percent: 50.0,
                }],
                network: NetworkCounters {
                    total_rx_bytes: 1500,
                    total_tx_bytes: 900,
                    rx_bytes_per_sec: Some(128.0),
                    tx_bytes_per_sec: Some(64.0),
                },
            }),
            last_seen: Some(Utc::now()),
            latency_ms: Some(12),
            online: true,
        }
    }

    fn sample_overview() -> OverviewData {
        OverviewData {
            generated_at: Utc::now(),
            total_nodes: 2,
            online_nodes: 2,
            offline_nodes: 0,
            total_rx_bytes: 3000,
            total_tx_bytes: 1800,
            current_rx_bytes_per_sec: 256.0,
            current_tx_bytes_per_sec: 128.0,
            average_latency_ms: Some(12.0),
        }
    }
}
