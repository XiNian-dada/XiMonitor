use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::PathBuf;

use axum::http::HeaderMap;
use chrono::Utc;

use super::support::trusted_proxies;
use crate::admission::{
    InstallAdmissionConfig, InstallAdmissionController, WsAdmissionController, WsAdmissionError,
    resolve_client_ip, sweep_expired_auth_failures,
};
use crate::handlers::is_well_formed_install_token;
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

    // clean 报告不应推动 anomaly 窗口前进;modified 报告才入窗口。
    let mut window: std::collections::VecDeque<std::time::Instant> =
        std::collections::VecDeque::new();
    let now = std::time::Instant::now();
    let clean_report = SanitizationReport::default();
    update_metric_anomaly_window(&mut window, &clean_report, now);
    assert!(window.is_empty());

    // 在窗口内攒满 METRIC_ANOMALY_SESSION_LIMIT 条 → 触发断连。
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

#[test]
fn truncate_to_byte_boundary_respects_char_boundary() {
    // "中" 在 UTF-8 中占 3 字节;cutoff = 7 必须回退到 6 字节边界。
    let mut value = "中".repeat(100);
    nodelite_proto::truncate_string_to_byte_boundary(&mut value, 7);
    assert!(value.len() <= 7);
    assert!(value.is_char_boundary(value.len()));
    assert!(value.chars().all(|ch| ch == '中'));

    // 已经在限内的字符串保持不变。
    let mut short = "abc".to_string();
    nodelite_proto::truncate_string_to_byte_boundary(&mut short, 16);
    assert_eq!(short, "abc");
}

#[test]
fn truncate_to_byte_boundary_handles_utf8_widths_with_bounded_scan() {
    let cases = [
        ("aé", 2, "a"),
        ("ab中", 4, "ab"),
        ("abc🦀", 6, "abc"),
        ("🦀", 0, ""),
    ];

    for (input, max_bytes, expected) in cases {
        let mut value = input.to_string();
        nodelite_proto::truncate_string_to_byte_boundary(&mut value, max_bytes);

        assert_eq!(value, expected);
        assert!(value.len() <= max_bytes);
        assert!(value.is_char_boundary(value.len()));
    }
}

#[test]
fn loopback_proxy_peer_uses_forwarded_ip_for_ws_limits() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-forwarded-for",
        "198.51.100.24".parse().expect("header value"),
    );

    let client_ip = resolve_client_ip(
        &[],
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 51234)),
        &headers,
    );

    assert_eq!(client_ip, IpAddr::V4("198.51.100.24".parse().expect("ip")));
}

#[test]
fn public_listener_behind_local_proxy_uses_forwarded_ip() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-forwarded-for",
        "198.51.100.24".parse().expect("header value"),
    );

    let client_ip = resolve_client_ip(
        &[],
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 51234)),
        &headers,
    );

    assert_eq!(client_ip, IpAddr::V4("198.51.100.24".parse().expect("ip")));
}

#[test]
fn public_direct_peer_ignores_spoofed_forwarded_ip() {
    let mut headers = HeaderMap::new();
    headers.insert("x-forwarded-for", "8.8.8.8".parse().expect("header value"));

    let client_ip = resolve_client_ip(
        &[],
        SocketAddr::V4(SocketAddrV4::new(
            "198.51.100.24".parse().expect("ip"),
            51234,
        )),
        &headers,
    );

    assert_eq!(client_ip, IpAddr::V4("198.51.100.24".parse().expect("ip")));
}

#[test]
fn trusted_proxy_chain_uses_last_untrusted_forwarded_ip() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-forwarded-for",
        "8.8.8.8, 198.51.100.24, 203.0.113.11"
            .parse()
            .expect("header value"),
    );

    let client_ip = resolve_client_ip(
        &trusted_proxies(&["203.0.113.0/24"]),
        SocketAddr::V4(SocketAddrV4::new(
            "203.0.113.10".parse().expect("ip"),
            51234,
        )),
        &headers,
    );

    assert_eq!(client_ip, IpAddr::V4("198.51.100.24".parse().expect("ip")));
}

#[test]
fn trusted_proxy_prefers_x_real_ip_when_forwarded_chain_is_absent() {
    let mut headers = HeaderMap::new();
    headers.insert("x-real-ip", "198.51.100.24".parse().expect("header value"));

    let client_ip = resolve_client_ip(
        &trusted_proxies(&["203.0.113.0/24"]),
        SocketAddr::V4(SocketAddrV4::new(
            "203.0.113.10".parse().expect("ip"),
            51234,
        )),
        &headers,
    );

    assert_eq!(client_ip, IpAddr::V4("198.51.100.24".parse().expect("ip")));
}

#[test]
fn malformed_forwarded_chain_falls_back_to_x_real_ip() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-forwarded-for",
        "8.8.8.8, invalid-ip".parse().expect("header value"),
    );
    headers.insert("x-real-ip", "198.51.100.24".parse().expect("header value"));

    let client_ip = resolve_client_ip(
        &[],
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 51234)),
        &headers,
    );

    assert_eq!(client_ip, IpAddr::V4("198.51.100.24".parse().expect("ip")));
}

#[test]
fn repeated_auth_failures_trigger_ws_block() {
    let controller = WsAdmissionController::new(&WsConfig {
        max_total_connections: 16,
        max_connections_per_ip: 4,
        auth_fail_window_secs: 60,
        auth_fail_max_attempts: 2,
        auth_block_secs: 300,
    });
    let client_ip = IpAddr::V4("198.51.100.24".parse().expect("ip"));

    controller.record_auth_failure(client_ip);
    controller.record_auth_failure(client_ip);

    match controller.try_acquire(client_ip) {
        Err(WsAdmissionError::Blocked { retry_after_secs }) => {
            assert!(retry_after_secs > 0);
        }
        _ => panic!("client should be temporarily blocked"),
    }
}

#[test]
fn metric_anomaly_window_decays_so_long_sessions_avoid_false_positive_kicks() {
    // 旧实现:METRIC_ANOMALY_SESSION_LIMIT 是会话生命周期内的累计上限,
    // 因此长跑节点偶发 5 次异常就会被踢。
    // 新实现:计数滑动到 METRIC_ANOMALY_WINDOW_SECS 之外即衰减,只有
    // "在同一窗口内连续超阈值"才触发断连。
    use std::collections::VecDeque;
    use std::time::{Duration, Instant};

    let mut window: VecDeque<Instant> = VecDeque::new();
    let report = SanitizationReport {
        clamped_percents: 1,
        ..SanitizationReport::default()
    };

    // 模拟一个 24 小时的长会话,每隔 1 小时遇到一次偶发的 sanitize 修正。
    // 任何两次 anomaly 的间隔(3600 s)都远大于窗口长度(默认 300 s),
    // 因此每次入队前老条目都已被剔除,窗口始终最多只有 1 条。
    let started_at = Instant::now();
    for hour in 0..24 {
        let now = started_at + Duration::from_secs(hour * 3600);
        update_metric_anomaly_window(&mut window, &report, now);
        assert!(
            !should_disconnect_for_metric_anomalies(&window),
            "long session with sparse anomalies should never be kicked",
        );
    }

    // 反过来,同一窗口内的高频异常 → 窗口内累计达到阈值 → 触发断连。
    let burst_at = started_at + Duration::from_secs(48 * 3600);
    for tick in 0..METRIC_ANOMALY_SESSION_LIMIT {
        update_metric_anomaly_window(
            &mut window,
            &report,
            burst_at + Duration::from_secs(tick as u64),
        );
    }
    assert!(
        should_disconnect_for_metric_anomalies(&window),
        "burst within the window must still trigger the kick",
    );
}

#[test]
fn sweep_drops_expired_failure_entries_and_keeps_live_blocks() {
    // 验证 sweep:已过期且未封禁的条目被移除;仍封禁的条目保留;
    // 仍在统计窗口内的失败条目保留。
    use std::collections::{HashMap, VecDeque};
    use std::time::{Duration, Instant};

    use crate::admission::AuthFailureState;

    let mut failures: HashMap<IpAddr, AuthFailureState> = HashMap::new();
    let now = Instant::now();
    let window = Duration::from_secs(60);

    // 1. 过期 + 未封禁 → 应被 sweep 删除
    let expired_ip: IpAddr = "203.0.113.10".parse().expect("ip");
    let mut expired = AuthFailureState::default();
    expired
        .recent_failures
        .push_back(now - Duration::from_secs(3600));
    failures.insert(expired_ip, expired);

    // 2. 已封禁但封禁未到期 → 应保留
    let blocked_ip: IpAddr = "203.0.113.20".parse().expect("ip");
    let blocked = AuthFailureState {
        recent_failures: VecDeque::new(),
        blocked_until: Some(now + Duration::from_secs(300)),
    };
    failures.insert(blocked_ip, blocked);

    // 3. 窗口内的失败 → 应保留
    let recent_ip: IpAddr = "203.0.113.30".parse().expect("ip");
    let mut recent = AuthFailureState::default();
    recent
        .recent_failures
        .push_back(now - Duration::from_secs(10));
    failures.insert(recent_ip, recent);

    sweep_expired_auth_failures(&mut failures, now, window);

    assert!(
        !failures.contains_key(&expired_ip),
        "expired entry should be removed",
    );
    assert!(
        failures.contains_key(&blocked_ip),
        "active block should be preserved",
    );
    assert!(
        failures.contains_key(&recent_ip),
        "in-window failure should be preserved",
    );
}

#[test]
fn install_token_format_short_circuits_obvious_garbage() {
    // 32-byte hex token = 64 lowercase hex chars 才是合法格式;
    // 任何不符合的输入应在落到 registry flock 之前就被拒掉。
    let valid = "0123456789abcdef".repeat(4);
    assert!(is_well_formed_install_token(&valid));
    assert!(!is_well_formed_install_token(""));
    assert!(!is_well_formed_install_token(&"a".repeat(63)));
    assert!(!is_well_formed_install_token(&"a".repeat(65)));
    // 大写不被接受 —— 与 generate_token 的 lowercase hex 输出对齐。
    assert!(!is_well_formed_install_token(&"A".repeat(64)));
    // 非 hex 字符
    assert!(!is_well_formed_install_token(&"z".repeat(64)));
}

#[test]
fn install_admission_blocks_after_repeated_failures() {
    let controller = InstallAdmissionController::new(InstallAdmissionConfig {
        auth_fail_window_secs: 60,
        auth_fail_max_attempts: 2,
        auth_block_secs: 300,
    });
    let client_ip: IpAddr = "198.51.100.24".parse().expect("ip");

    // 阈值前应放行
    assert!(controller.check(client_ip).is_ok());
    controller.record_auth_failure(client_ip);
    controller.record_auth_failure(client_ip);

    match controller.check(client_ip) {
        Err(retry_after_secs) => assert!(retry_after_secs > 0),
        Ok(()) => panic!("client should be temporarily blocked after threshold"),
    }
}
