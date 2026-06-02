//! Proxy resolution and admission-control tests.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};

use axum::http::HeaderMap;

use super::support::trusted_proxies;
use crate::admission::{
    InstallAdmissionConfig, InstallAdmissionController, WsAdmissionController, WsAdmissionError,
    resolve_client_ip, sweep_expired_auth_failures,
};
use crate::handlers::is_well_formed_install_token;
use crate::sanitize::{
    METRIC_ANOMALY_SESSION_LIMIT, SanitizationReport, should_disconnect_for_metric_anomalies,
    update_metric_anomaly_window,
};
use nodelite_proto::WsConfig;

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
    use std::collections::VecDeque;
    use std::time::{Duration, Instant};

    let mut window: VecDeque<Instant> = VecDeque::new();
    let report = SanitizationReport {
        clamped_percents: 1,
        ..SanitizationReport::default()
    };

    let started_at = Instant::now();
    for hour in 0..24 {
        let now = started_at + Duration::from_secs(hour * 3600);
        update_metric_anomaly_window(&mut window, &report, now);
        assert!(
            !should_disconnect_for_metric_anomalies(&window),
            "long session with sparse anomalies should never be kicked",
        );
    }

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
    use std::collections::{HashMap, VecDeque};
    use std::time::{Duration, Instant};

    use crate::admission::AuthFailureState;

    let mut failures: HashMap<IpAddr, AuthFailureState> = HashMap::new();
    let now = Instant::now();
    let window = Duration::from_secs(60);

    let expired_ip: IpAddr = "203.0.113.10".parse().expect("ip");
    let mut expired = AuthFailureState::default();
    expired
        .recent_failures
        .push_back(now - Duration::from_secs(3600));
    failures.insert(expired_ip, expired);

    let blocked_ip: IpAddr = "203.0.113.20".parse().expect("ip");
    let blocked = AuthFailureState {
        recent_failures: VecDeque::new(),
        blocked_until: Some(now + Duration::from_secs(300)),
    };
    failures.insert(blocked_ip, blocked);

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
    let valid = "0123456789abcdef".repeat(4);
    assert!(is_well_formed_install_token(&valid));
    assert!(!is_well_formed_install_token(""));
    assert!(!is_well_formed_install_token(&"a".repeat(63)));
    assert!(!is_well_formed_install_token(&"a".repeat(65)));
    assert!(!is_well_formed_install_token(&"A".repeat(64)));
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

    assert!(controller.check(client_ip).is_ok());
    controller.record_auth_failure(client_ip);
    controller.record_auth_failure(client_ip);

    match controller.check(client_ip) {
        Err(retry_after_secs) => assert!(retry_after_secs > 0),
        Ok(()) => panic!("client should be temporarily blocked after threshold"),
    }
}
