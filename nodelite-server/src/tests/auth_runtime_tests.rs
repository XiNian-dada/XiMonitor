//! Auth and runtime-focused library-unit tests.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use axum::body::Body;
use axum::http::{Request, header};

use super::{ServerReadiness, uses_insecure_remote_public_base_url};
use crate::auth::{ReadonlyRouteAuth, TwoFactorSessions};

#[test]
fn readonly_route_auth_matches_basic_header() {
    let auth = ReadonlyRouteAuth::from_config(Some(nodelite_proto::ReadonlyAuthConfig {
        username: "viewer".to_string(),
        password: "secret".to_string(),
        enable_2fa: false,
        totp_secret: None,
    }));
    let request = Request::builder()
        .uri("/api/overview")
        .header(header::AUTHORIZATION, "Basic dmlld2VyOnNlY3JldA==")
        .body(Body::empty())
        .expect("request should build");

    assert!(auth.is_authorized(&request));
}

#[test]
fn two_factor_session_cookie_must_be_server_issued() {
    let sessions = TwoFactorSessions::new();
    assert!(!sessions.is_authenticated("verified"));

    let token = sessions
        .create_authenticated()
        .expect("session token should be generated");
    assert!(sessions.is_authenticated(&token));
    sessions.remove_authenticated(&token);
    assert!(!sessions.is_authenticated(&token));
}

#[test]
fn pending_session_invalidated_after_max_failed_attempts() {
    let sessions = TwoFactorSessions::new();
    let token = sessions
        .create_pending()
        .expect("pending session should be created");
    assert!(sessions.pending_exists(&token));

    for _ in 0..(crate::auth::TWO_FACTOR_MAX_FAILED_ATTEMPTS - 1) {
        assert!(!sessions.record_failed_attempt(&token));
        assert!(sessions.pending_exists(&token));
    }

    assert!(sessions.record_failed_attempt(&token));
    assert!(!sessions.pending_exists(&token));
    assert!(sessions.record_failed_attempt(&token));
}

#[test]
fn totp_step_marked_used_blocks_replay() {
    let sessions = TwoFactorSessions::new();
    let step = 12345_u64;
    let replay_retention =
        std::time::Duration::from_secs(crate::auth::TWO_FACTOR_TOTP_REPLAY_RETENTION_SECS);
    assert!(replay_retention >= std::time::Duration::from_secs(150));
    assert!(!sessions.is_totp_step_used(step));
    sessions.mark_totp_step_used(step);
    assert!(sessions.is_totp_step_used(step));
    assert!(!sessions.is_totp_step_used(step + 1));
    assert!(!sessions.is_totp_step_used(step - 1));
}

#[test]
fn constant_time_compare_matches_only_identical_byte_slices() {
    assert!(crate::auth::constant_time_compare_bytes(
        b"abc123", b"abc123"
    ));
    assert!(!crate::auth::constant_time_compare_bytes(
        b"abc123", b"abc124"
    ));
    assert!(!crate::auth::constant_time_compare_bytes(b"abc", b"abc1"));
    assert!(!crate::auth::constant_time_compare_bytes(b"", b"a"));
    assert!(crate::auth::constant_time_compare_bytes(b"", b""));
}

#[test]
fn warns_for_remote_http_public_base_url() {
    assert!(uses_insecure_remote_public_base_url(
        "http://monitor.example.com",
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 8080)),
    ));
    assert!(uses_insecure_remote_public_base_url(
        "http://203.0.113.10:8080",
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 8080)),
    ));
}

#[test]
fn ignores_local_or_tls_public_base_url() {
    assert!(!uses_insecure_remote_public_base_url(
        "https://monitor.example.com",
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 8080)),
    ));
    assert!(!uses_insecure_remote_public_base_url(
        "http://127.0.0.1:8080",
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 8080)),
    ));
    assert!(!uses_insecure_remote_public_base_url(
        "http://localhost:8080",
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 8080)),
    ));
}

#[test]
fn server_readiness_tracks_dependency_health() {
    let readiness = ServerReadiness::new(true);
    assert!(readiness.is_ready());
    assert_eq!(readiness.status_label(), "ok");

    readiness.mark_registry_reload_healthy(false);
    assert!(!readiness.is_ready());
    assert_eq!(readiness.status_label(), "degraded");

    readiness.mark_registry_reload_healthy(true);
    readiness.mark_history_available(false);
    assert!(!readiness.is_ready());
    assert!(!readiness.history_available());
}
