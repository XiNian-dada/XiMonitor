//! Tests for Prometheus metrics rendering helpers.

use chrono::Utc;

use super::{
    ApiCacheMetrics, RuntimeMetrics, SqliteWalCheckpointMetrics, SqliteWalCheckpointStats,
    WriterMetrics, WsMessageMetrics, render_agent_log_metrics, render_api_cache_metrics,
    render_metrics_response_body_bytes, render_prometheus_metrics, render_runtime_metrics,
    render_writer_metrics,
};
use crate::ServerReadiness;
use crate::agent_logs::AgentLogStats;
use nodelite_proto::{
    DiskUsage, LoadAverage, MemoryUsage, NetworkCounters, NodeIdentity, NodeSnapshot, NodeStatus,
    OverviewData,
};

#[test]
fn exporter_emits_help_and_type_once_per_family() {
    let readiness = ServerReadiness::new(true);
    let overview = sample_overview();
    let body = render_prometheus_metrics(&readiness, &sample_statuses(), &overview);

    assert_eq!(body.matches("# HELP nodelite_node_online ").count(), 1);
    assert_eq!(body.matches("# TYPE nodelite_node_online gauge").count(), 1);
    assert_eq!(body.matches("# HELP nodelite_node_cpu_usage_ratio ").count(), 1);
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
    assert!(!body.contains("nodelite_node_disk_bytes{"));
    assert!(body.contains("nodelite_node_load_average{node_id=\"node-2\",window=\"15m\"} 0.75"));
    assert!(body.contains(
        "nodelite_node_network_bytes_total{node_id=\"node-1\",direction=\"rx\"} 1500"
    ));
}

#[test]
fn exporter_skips_unknown_cpu_usage() {
    let readiness = ServerReadiness::new(true);
    let overview = sample_overview();
    let mut statuses = sample_statuses();
    statuses[0]
        .snapshot
        .as_mut()
        .expect("sample node should have snapshot")
        .cpu_usage_percent = None;

    let body = render_prometheus_metrics(&readiness, &statuses, &overview);

    assert_eq!(
        body.lines()
            .filter(|line| line.starts_with("nodelite_node_cpu_usage_ratio{"))
            .count(),
        1
    );
    assert!(!body.contains("nodelite_node_cpu_usage_ratio{node_id=\"node-1\""));
    assert!(body.contains("nodelite_node_cpu_usage_ratio{node_id=\"node-2\""));
}

#[test]
fn exporter_exposes_writer_counters() {
    let body = render_writer_metrics(WriterMetrics {
        history_dropped_writes: 3,
        history_queue_depth: 17,
        history_queue_capacity: 1024,
        audit_dropped_writes: 5,
        audit_queue_depth: 19,
        audit_queue_capacity: 256,
        audit_write_failures: 7,
        session_control_queue_full_total: 11,
    });

    assert!(body.contains("# TYPE nodelite_history_dropped_writes_total counter"));
    assert!(body.contains("nodelite_history_dropped_writes_total 3"));
    assert!(body.contains("# TYPE nodelite_history_queue_depth gauge"));
    assert!(body.contains("nodelite_history_queue_depth 17"));
    assert!(body.contains("# TYPE nodelite_history_queue_capacity gauge"));
    assert!(body.contains("nodelite_history_queue_capacity 1024"));
    assert!(body.contains("# TYPE nodelite_audit_dropped_writes_total counter"));
    assert!(body.contains("nodelite_audit_dropped_writes_total 5"));
    assert!(body.contains("# TYPE nodelite_audit_queue_depth gauge"));
    assert!(body.contains("nodelite_audit_queue_depth 19"));
    assert!(body.contains("# TYPE nodelite_audit_queue_capacity gauge"));
    assert!(body.contains("nodelite_audit_queue_capacity 256"));
    assert!(body.contains("# TYPE nodelite_audit_write_failures_total counter"));
    assert!(body.contains("nodelite_audit_write_failures_total 7"));
    assert!(body.contains("# TYPE nodelite_session_control_queue_full_total counter"));
    assert!(body.contains("nodelite_session_control_queue_full_total 11"));
}

#[test]
fn exporter_exposes_agent_log_store_gauges() {
    let body = render_agent_log_metrics(AgentLogStats {
        nodes: 2,
        entries: 37,
        estimated_bytes: 4096,
        max_entries: 10_000,
        max_estimated_bytes: 8 * 1024 * 1024,
    });

    assert!(body.contains("# TYPE nodelite_agent_log_nodes gauge"));
    assert!(body.contains("nodelite_agent_log_nodes 2"));
    assert!(body.contains("# TYPE nodelite_agent_log_entries gauge"));
    assert!(body.contains("nodelite_agent_log_entries 37"));
    assert!(body.contains("# TYPE nodelite_agent_log_estimated_bytes gauge"));
    assert!(body.contains("nodelite_agent_log_estimated_bytes 4096"));
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
        metrics_hits: 17,
        metrics_misses: 4,
        metrics_body_bytes: 8192,
    });

    assert!(body.contains("# TYPE nodelite_api_cache_hits_total counter"));
    assert!(body.contains("nodelite_api_cache_hits_total{kind=\"nodes\"} 11"));
    assert!(body.contains("nodelite_api_cache_hits_total{kind=\"overview\"} 13"));
    assert!(body.contains("nodelite_api_cache_hits_total{kind=\"metrics\"} 17"));
    assert!(body.contains("# TYPE nodelite_api_cache_misses_total counter"));
    assert!(body.contains("nodelite_api_cache_misses_total{kind=\"nodes\"} 2"));
    assert!(body.contains("nodelite_api_cache_misses_total{kind=\"overview\"} 3"));
    assert!(body.contains("nodelite_api_cache_misses_total{kind=\"metrics\"} 4"));
    assert!(body.contains("# TYPE nodelite_api_body_bytes gauge"));
    assert!(body.contains("nodelite_api_body_bytes{kind=\"nodes\"} 4096"));
    assert!(body.contains("nodelite_api_body_bytes{kind=\"overview\"} 256"));
    assert!(body.contains("nodelite_api_body_bytes{kind=\"metrics\"} 8192"));
    assert!(body.contains("# TYPE nodelite_view_cache_hits_total counter"));
    assert!(body.contains("nodelite_view_cache_hits_total{kind=\"metrics\"} 17"));
    assert!(body.contains("# TYPE nodelite_view_cache_misses_total counter"));
    assert!(body.contains("nodelite_view_cache_misses_total{kind=\"metrics\"} 4"));
    assert!(body.contains("# TYPE nodelite_metrics_body_bytes gauge"));
    assert!(body.contains("nodelite_metrics_body_bytes 8192"));
}

#[test]
fn exporter_exposes_runtime_observability_metrics() {
    let body = render_runtime_metrics(RuntimeMetrics {
        process_resident_memory_bytes: Some(12_345),
        history_db_bytes: 4096,
        history_wal_bytes: 1024,
        history_shm_bytes: 512,
        audit_db_bytes: 2048,
        audit_wal_bytes: 256,
        audit_shm_bytes: 128,
        sqlite_wal_checkpoint: SqliteWalCheckpointMetrics {
            history: SqliteWalCheckpointStats {
                observed: true,
                active: true,
                busy: 0,
                log_pages: 24,
                checkpointed_pages: 20,
            },
            audit: SqliteWalCheckpointStats {
                observed: true,
                active: false,
                busy: 1,
                log_pages: 10,
                checkpointed_pages: 7,
            },
        },
        registry_nodes: 3,
        registry_disk_entries_total: 7,
        ws_messages: WsMessageMetrics {
            metrics_total: 11,
            agent_logs_total: 13,
            pong_total: 17,
            refresh_token_request_total: 19,
        },
    });

    assert!(body.contains("# TYPE nodelite_process_resident_memory_bytes gauge"));
    assert!(body.contains("nodelite_process_resident_memory_bytes 12345"));
    assert!(body.contains("# TYPE nodelite_sqlite_file_bytes gauge"));
    assert!(body.contains("nodelite_sqlite_file_bytes{kind=\"history_db\"} 4096"));
    assert!(body.contains("nodelite_sqlite_file_bytes{kind=\"history_wal\"} 1024"));
    assert!(body.contains("nodelite_sqlite_file_bytes{kind=\"audit_db\"} 2048"));
    assert!(body.contains("# TYPE nodelite_history_db_bytes gauge"));
    assert!(body.contains("nodelite_history_db_bytes 4096"));
    assert!(body.contains("# TYPE nodelite_history_wal_bytes gauge"));
    assert!(body.contains("nodelite_history_wal_bytes 1024"));
    assert!(body.contains("# TYPE nodelite_sqlite_wal_checkpoint_observed gauge"));
    assert!(body.contains("nodelite_sqlite_wal_checkpoint_observed{database=\"history\"} 1"));
    assert!(body.contains("# TYPE nodelite_sqlite_wal_checkpoint_active gauge"));
    assert!(body.contains("nodelite_sqlite_wal_checkpoint_active{database=\"history\"} 1"));
    assert!(body.contains("nodelite_sqlite_wal_checkpoint_active{database=\"audit\"} 0"));
    assert!(body.contains("# TYPE nodelite_sqlite_wal_checkpoint_busy gauge"));
    assert!(body.contains("nodelite_sqlite_wal_checkpoint_busy{database=\"audit\"} 1"));
    assert!(body.contains("# TYPE nodelite_sqlite_wal_checkpoint_pages gauge"));
    assert!(body.contains(
        "nodelite_sqlite_wal_checkpoint_pages{database=\"history\",state=\"log\"} 24"
    ));
    assert!(body.contains(
        "nodelite_sqlite_wal_checkpoint_pages{database=\"history\",state=\"backlog\"} 4"
    ));
    assert!(body.contains(
        "nodelite_sqlite_wal_checkpoint_pages{database=\"audit\",state=\"checkpointed\"} 7"
    ));
    assert!(body.contains("# TYPE nodelite_registry_nodes gauge"));
    assert!(body.contains("nodelite_registry_nodes 3"));
    assert!(body.contains("# TYPE nodelite_registry_disk_entries_total gauge"));
    assert!(body.contains("nodelite_registry_disk_entries_total 7"));
    assert!(body.contains("# TYPE nodelite_ws_messages_total counter"));
    assert!(body.contains("nodelite_ws_messages_total{type=\"metrics\"} 11"));
    assert!(body.contains("nodelite_ws_messages_total{type=\"agent_logs\"} 13"));
    assert!(body.contains("nodelite_ws_messages_total{type=\"pong\"} 17"));
    assert!(body.contains("nodelite_ws_messages_total{type=\"refresh_token_request\"} 19"));
}

#[test]
fn exporter_exposes_metrics_response_body_size() {
    let body = render_metrics_response_body_bytes(12_345);

    assert!(body.contains("# TYPE nodelite_metrics_response_body_bytes gauge"));
    assert!(body.contains("nodelite_metrics_response_body_bytes 12345"));
}

fn sample_statuses() -> Vec<NodeStatus> {
    vec![
        sample_status("node-1", "Node 1", 1, 0.5),
        sample_status("node-2", "Node 2", 2, 0.75),
    ]
}

fn sample_status(node_id: &str, node_label: &str, uptime_secs: u64, load_15m: f64) -> NodeStatus {
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
            cpu_usage_percent: Some(42.0),
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
