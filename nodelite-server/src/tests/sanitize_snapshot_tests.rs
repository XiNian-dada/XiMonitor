//! Snapshot sanitization tests.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::PathBuf;

use chrono::Utc;

use crate::sanitize::{
    MAX_SANITIZED_DISKS, MAX_SANITIZED_LOAD, MAX_SANITIZED_RATE_BYTES_PER_SEC,
    MAX_SANITIZED_STRING_BYTES, METRIC_ANOMALY_SESSION_LIMIT, SanitizationReport,
    sanitize_snapshot, should_disconnect_for_metric_anomalies, update_metric_anomaly_window,
};
use crate::test_support::test_server_config;
use nodelite_proto::{NodeSnapshot, ServerConfig, WsConfig};

#[test]
fn sanitize_snapshot_clamps_invalid_metrics() {
    let config = ServerConfig {
        listen: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 8080)),
        public_base_url: "http://127.0.0.1:8080".to_string(),
        insecure_allow_http: false,
        trusted_proxies: Vec::new(),
        readonly_auth: None,
        ws: WsConfig {
            max_total_connections: 32,
            max_connections_per_ip: 8,
            auth_fail_window_secs: 300,
            auth_fail_max_attempts: 6,
            auth_block_secs: 600,
        },
        audit: nodelite_proto::AuditConfig {
            enabled: true,
            db_path: PathBuf::from("./data/audit.sqlite3"),
            retention_days: 90,
            log_successful_auth: true,
            log_failed_auth: true,
            log_token_events: true,
            log_rate_limit: true,
        },
        alerting: nodelite_proto::AlertingConfig::default(),
        node_registry_path: PathBuf::from("./data/server.json"),
        history_db_path: PathBuf::from("./data/history.sqlite3"),
        snapshot_path: PathBuf::from("./data/snapshot.json"),
        stale_after_secs: 15,
        ping_interval_secs: 5,
        max_message_bytes: 64 * 1024,
        refresh_interval_secs: 5,
        ignored_filesystems: vec!["tmpfs".to_string()],
        agent_release_base_url: None,
        agent_release_sha256_x86_64: None,
        agent_release_sha256_aarch64: None,
        hello_timeout_secs: 10,
        max_outstanding_pings: 32,
        insecure_transport_warn_interval_secs: 900,
        max_sanitized_disks: 64,
        max_sanitized_string_bytes: 256,
        metric_anomaly_session_limit: 5,
        sqlite_busy_timeout_secs: 5,
    };
    let snapshot = NodeSnapshot {
        collected_at: Utc::now(),
        cpu_usage_percent: Some(f64::INFINITY),
        load: nodelite_proto::LoadAverage {
            one: -1.0,
            five: f64::NAN,
            fifteen: 2_000_000.0,
        },
        memory: nodelite_proto::MemoryUsage {
            total_bytes: 100,
            used_bytes: 200,
            available_bytes: 100,
            swap_total_bytes: 50,
            swap_used_bytes: 99,
        },
        uptime_secs: 5,
        disks: vec![
            nodelite_proto::DiskUsage {
                device: " /dev/vda1 ".to_string(),
                mount_point: " / ".to_string(),
                fs_type: " ext4 ".to_string(),
                total_bytes: 100,
                available_bytes: 80,
                used_bytes: 90,
                used_percent: 999.0,
            },
            nodelite_proto::DiskUsage {
                device: "tmp".to_string(),
                mount_point: "/run".to_string(),
                fs_type: "tmpfs".to_string(),
                total_bytes: 1,
                available_bytes: 0,
                used_bytes: 1,
                used_percent: 100.0,
            },
            nodelite_proto::DiskUsage {
                device: " ".to_string(),
                mount_point: "/bad".to_string(),
                fs_type: "xfs".to_string(),
                total_bytes: 100,
                available_bytes: 10,
                used_bytes: 90,
                used_percent: 90.0,
            },
        ],
        network: nodelite_proto::NetworkCounters {
            total_rx_bytes: 1,
            total_tx_bytes: 2,
            rx_bytes_per_sec: Some(-10.0),
            tx_bytes_per_sec: Some(f64::INFINITY),
        },
    };

    let (sanitized, report) = sanitize_snapshot(&config, snapshot);
    assert_eq!(sanitized.cpu_usage_percent, Some(100.0));
    assert_eq!(sanitized.load.five, 0.0);
    assert_eq!(sanitized.load.fifteen, MAX_SANITIZED_LOAD);
    assert_eq!(sanitized.memory.used_bytes, 100);
    assert_eq!(sanitized.memory.available_bytes, 0);
    assert_eq!(sanitized.memory.swap_used_bytes, 50);
    assert_eq!(sanitized.network.rx_bytes_per_sec, Some(0.0));
    assert_eq!(
        sanitized.network.tx_bytes_per_sec,
        Some(MAX_SANITIZED_RATE_BYTES_PER_SEC)
    );
    assert_eq!(sanitized.disks.len(), 1);
    assert_eq!(sanitized.disks[0].device, "/dev/vda1");
    assert_eq!(sanitized.disks[0].mount_point, "/");
    assert_eq!(sanitized.disks[0].fs_type, "ext4");
    assert_eq!(sanitized.disks[0].used_bytes, 20);
    assert_eq!(sanitized.disks[0].used_percent, 20.0);
    assert_eq!(report.clamped_percents, 1);
    assert_eq!(report.clamped_loads, 3);
    assert_eq!(report.clamped_memory_bytes, 1);
    assert_eq!(report.clamped_disk_bytes, 1);
    assert_eq!(report.dropped_disks, 1);
    assert_eq!(report.sanitized_rates, 2);
    assert!(report.modified());
}

#[test]
fn sanitize_snapshot_preserves_unknown_cpu_usage() {
    let config = test_server_config(
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 8080)),
        "http://127.0.0.1:8080".to_string(),
        PathBuf::from("./data/server.json"),
        PathBuf::from("./data/history.sqlite3"),
        PathBuf::from("./data/snapshot.json"),
    );
    let snapshot = NodeSnapshot {
        collected_at: Utc::now(),
        cpu_usage_percent: None,
        load: nodelite_proto::LoadAverage {
            one: 0.0,
            five: 0.0,
            fifteen: 0.0,
        },
        memory: nodelite_proto::MemoryUsage {
            total_bytes: 100,
            used_bytes: 50,
            available_bytes: 50,
            swap_total_bytes: 0,
            swap_used_bytes: 0,
        },
        uptime_secs: 5,
        disks: Vec::new(),
        network: nodelite_proto::NetworkCounters {
            total_rx_bytes: 1,
            total_tx_bytes: 2,
            rx_bytes_per_sec: None,
            tx_bytes_per_sec: None,
        },
    };

    let (sanitized, report) = sanitize_snapshot(&config, snapshot);
    assert_eq!(sanitized.cpu_usage_percent, None);
    assert!(!report.modified());
}

#[test]
fn sanitize_caps_disk_field_string_length() {
    let config = ServerConfig {
        listen: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 8080)),
        public_base_url: "http://127.0.0.1:8080".to_string(),
        insecure_allow_http: false,
        trusted_proxies: Vec::new(),
        readonly_auth: None,
        ws: WsConfig {
            max_total_connections: 32,
            max_connections_per_ip: 8,
            auth_fail_window_secs: 300,
            auth_fail_max_attempts: 6,
            auth_block_secs: 600,
        },
        audit: nodelite_proto::AuditConfig {
            enabled: true,
            db_path: PathBuf::from("./data/audit.sqlite3"),
            retention_days: 90,
            log_successful_auth: true,
            log_failed_auth: true,
            log_token_events: true,
            log_rate_limit: true,
        },
        alerting: nodelite_proto::AlertingConfig::default(),
        node_registry_path: PathBuf::from("./data/server.json"),
        history_db_path: PathBuf::from("./data/history.sqlite3"),
        snapshot_path: PathBuf::from("./data/snapshot.json"),
        stale_after_secs: 15,
        ping_interval_secs: 5,
        max_message_bytes: 64 * 1024,
        refresh_interval_secs: 5,
        ignored_filesystems: Vec::new(),
        agent_release_base_url: None,
        agent_release_sha256_x86_64: None,
        agent_release_sha256_aarch64: None,
        hello_timeout_secs: 10,
        max_outstanding_pings: 32,
        insecure_transport_warn_interval_secs: 900,
        max_sanitized_disks: 64,
        max_sanitized_string_bytes: 256,
        metric_anomaly_session_limit: 5,
        sqlite_busy_timeout_secs: 5,
    };
    let oversized = "x".repeat(MAX_SANITIZED_STRING_BYTES * 4);
    let snapshot = NodeSnapshot {
        collected_at: Utc::now(),
        cpu_usage_percent: Some(10.0),
        load: nodelite_proto::LoadAverage {
            one: 0.0,
            five: 0.0,
            fifteen: 0.0,
        },
        memory: nodelite_proto::MemoryUsage {
            total_bytes: 100,
            used_bytes: 50,
            available_bytes: 50,
            swap_total_bytes: 0,
            swap_used_bytes: 0,
        },
        uptime_secs: 1,
        disks: vec![nodelite_proto::DiskUsage {
            device: format!("/dev/{oversized}"),
            mount_point: format!("/mnt/{oversized}"),
            fs_type: oversized.clone(),
            total_bytes: 100,
            available_bytes: 50,
            used_bytes: 50,
            used_percent: 50.0,
        }],
        network: nodelite_proto::NetworkCounters {
            total_rx_bytes: 0,
            total_tx_bytes: 0,
            rx_bytes_per_sec: None,
            tx_bytes_per_sec: None,
        },
    };

    let (sanitized, report) = sanitize_snapshot(&config, snapshot);
    assert_eq!(sanitized.disks.len(), 1);
    assert!(sanitized.disks[0].device.len() <= MAX_SANITIZED_STRING_BYTES);
    assert!(sanitized.disks[0].mount_point.len() <= MAX_SANITIZED_STRING_BYTES);
    assert!(sanitized.disks[0].fs_type.len() <= MAX_SANITIZED_STRING_BYTES);
    assert_eq!(report.truncated_strings, 1);
    assert!(report.modified());
}

#[test]
fn sanitize_snapshot_caps_disk_count_and_tracks_clean_reports() {
    let config = ServerConfig {
        listen: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 8080)),
        public_base_url: "http://127.0.0.1:8080".to_string(),
        insecure_allow_http: false,
        trusted_proxies: Vec::new(),
        readonly_auth: None,
        ws: WsConfig {
            max_total_connections: 32,
            max_connections_per_ip: 8,
            auth_fail_window_secs: 300,
            auth_fail_max_attempts: 6,
            auth_block_secs: 600,
        },
        audit: nodelite_proto::AuditConfig {
            enabled: true,
            db_path: PathBuf::from("./data/audit.sqlite3"),
            retention_days: 90,
            log_successful_auth: true,
            log_failed_auth: true,
            log_token_events: true,
            log_rate_limit: true,
        },
        alerting: nodelite_proto::AlertingConfig::default(),
        node_registry_path: PathBuf::from("./data/server.json"),
        history_db_path: PathBuf::from("./data/history.sqlite3"),
        snapshot_path: PathBuf::from("./data/snapshot.json"),
        stale_after_secs: 15,
        ping_interval_secs: 5,
        max_message_bytes: 64 * 1024,
        refresh_interval_secs: 5,
        ignored_filesystems: Vec::new(),
        agent_release_base_url: None,
        agent_release_sha256_x86_64: None,
        agent_release_sha256_aarch64: None,
        hello_timeout_secs: 10,
        max_outstanding_pings: 32,
        insecure_transport_warn_interval_secs: 900,
        max_sanitized_disks: 64,
        max_sanitized_string_bytes: 256,
        metric_anomaly_session_limit: 5,
        sqlite_busy_timeout_secs: 5,
    };
    let disks = (0..(MAX_SANITIZED_DISKS + 3))
        .map(|index| nodelite_proto::DiskUsage {
            device: format!("/dev/vd{index}"),
            mount_point: format!("/mnt/{index}"),
            fs_type: "ext4".to_string(),
            total_bytes: 100,
            available_bytes: 40,
            used_bytes: 60,
            used_percent: 60.0,
        })
        .collect();
    let snapshot = NodeSnapshot {
        collected_at: Utc::now(),
        cpu_usage_percent: Some(10.0),
        load: nodelite_proto::LoadAverage {
            one: 0.5,
            five: 0.7,
            fifteen: 0.9,
        },
        memory: nodelite_proto::MemoryUsage {
            total_bytes: 100,
            used_bytes: 60,
            available_bytes: 40,
            swap_total_bytes: 10,
            swap_used_bytes: 5,
        },
        uptime_secs: 1,
        disks,
        network: nodelite_proto::NetworkCounters {
            total_rx_bytes: 1,
            total_tx_bytes: 2,
            rx_bytes_per_sec: Some(3.0),
            tx_bytes_per_sec: Some(4.0),
        },
    };

    let (sanitized, report) = sanitize_snapshot(&config, snapshot);
    assert_eq!(sanitized.disks.len(), MAX_SANITIZED_DISKS);
    assert_eq!(report.dropped_disks, 3);
    assert!(report.modified());

    let mut window: std::collections::VecDeque<std::time::Instant> =
        std::collections::VecDeque::new();
    let now = std::time::Instant::now();
    let clean_report = SanitizationReport::default();
    update_metric_anomaly_window(&mut window, &clean_report, now);
    assert!(window.is_empty());

    for tick in 0..METRIC_ANOMALY_SESSION_LIMIT {
        update_metric_anomaly_window(
            &mut window,
            &report,
            now + std::time::Duration::from_secs(tick as u64),
        );
    }
    assert!(should_disconnect_for_metric_anomalies(&window));
}

#[test]
fn sanitize_snapshot_deduplicates_repeated_disk_devices() {
    let config = ServerConfig {
        listen: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 8080)),
        public_base_url: "http://127.0.0.1:8080".to_string(),
        insecure_allow_http: false,
        trusted_proxies: Vec::new(),
        readonly_auth: None,
        ws: WsConfig {
            max_total_connections: 32,
            max_connections_per_ip: 8,
            auth_fail_window_secs: 300,
            auth_fail_max_attempts: 6,
            auth_block_secs: 600,
        },
        audit: nodelite_proto::AuditConfig {
            enabled: true,
            db_path: PathBuf::from("./data/audit.sqlite3"),
            retention_days: 90,
            log_successful_auth: true,
            log_failed_auth: true,
            log_token_events: true,
            log_rate_limit: true,
        },
        alerting: nodelite_proto::AlertingConfig::default(),
        node_registry_path: PathBuf::from("./data/server.json"),
        history_db_path: PathBuf::from("./data/history.sqlite3"),
        snapshot_path: PathBuf::from("./data/snapshot.json"),
        stale_after_secs: 15,
        ping_interval_secs: 5,
        max_message_bytes: 64 * 1024,
        refresh_interval_secs: 5,
        ignored_filesystems: Vec::new(),
        agent_release_base_url: None,
        agent_release_sha256_x86_64: None,
        agent_release_sha256_aarch64: None,
        hello_timeout_secs: 10,
        max_outstanding_pings: 32,
        insecure_transport_warn_interval_secs: 900,
        max_sanitized_disks: 64,
        max_sanitized_string_bytes: 256,
        metric_anomaly_session_limit: 5,
        sqlite_busy_timeout_secs: 5,
    };
    let snapshot = NodeSnapshot {
        collected_at: Utc::now(),
        cpu_usage_percent: Some(1.0),
        load: nodelite_proto::LoadAverage {
            one: 0.1,
            five: 0.1,
            fifteen: 0.1,
        },
        memory: nodelite_proto::MemoryUsage {
            total_bytes: 100,
            used_bytes: 50,
            available_bytes: 50,
            swap_total_bytes: 0,
            swap_used_bytes: 0,
        },
        uptime_secs: 60,
        disks: vec![
            nodelite_proto::DiskUsage {
                device: "/dev/vda1".to_string(),
                mount_point: "/".to_string(),
                fs_type: "ext4".to_string(),
                total_bytes: 100,
                available_bytes: 40,
                used_bytes: 60,
                used_percent: 60.0,
            },
            nodelite_proto::DiskUsage {
                device: "/dev/vda1".to_string(),
                mount_point: "/var".to_string(),
                fs_type: "ext4".to_string(),
                total_bytes: 100,
                available_bytes: 40,
                used_bytes: 60,
                used_percent: 60.0,
            },
            nodelite_proto::DiskUsage {
                device: "/dev/vdb".to_string(),
                mount_point: "/ssd".to_string(),
                fs_type: "ext4".to_string(),
                total_bytes: 200,
                available_bytes: 100,
                used_bytes: 100,
                used_percent: 50.0,
            },
        ],
        network: nodelite_proto::NetworkCounters {
            total_rx_bytes: 1,
            total_tx_bytes: 2,
            rx_bytes_per_sec: Some(3.0),
            tx_bytes_per_sec: Some(4.0),
        },
    };

    let (sanitized, report) = sanitize_snapshot(&config, snapshot);
    assert_eq!(sanitized.disks.len(), 2);
    assert_eq!(sanitized.disks[0].mount_point, "/");
    assert_eq!(sanitized.disks[1].mount_point, "/ssd");
    assert_eq!(report.dropped_disks, 1);
}
