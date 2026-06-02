//! Shared helpers for split library-unit tests.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Request, StatusCode, header};
use ipnet::IpNet;

use super::{AppState, PROTECTED_CACHE_CONTROL};
use crate::test_support::{TEST_BASIC_AUTH_HEADER, test_server_config};

pub(crate) fn assert_security_headers(headers: &HeaderMap) {
    assert_eq!(
        headers.get(header::X_CONTENT_TYPE_OPTIONS),
        Some(&header::HeaderValue::from_static("nosniff")),
    );
    assert_eq!(
        headers.get(header::REFERRER_POLICY),
        Some(&header::HeaderValue::from_static(
            "strict-origin-when-cross-origin",
        )),
    );
    assert_eq!(
        headers.get(header::CACHE_CONTROL),
        Some(&header::HeaderValue::from_static(PROTECTED_CACHE_CONTROL)),
    );
    assert_eq!(
        headers.get(header::PRAGMA),
        Some(&header::HeaderValue::from_static("no-cache")),
    );
}

pub(crate) async fn two_factor_auth_test_state(
    label: &str,
    enable_2fa: bool,
) -> (AppState, PathBuf) {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be monotonic enough")
        .as_nanos();
    let temp_dir = std::env::temp_dir().join(format!("nodelite-{label}-{unique}"));
    std::fs::create_dir_all(&temp_dir).expect("temp dir should exist");
    let mut config = test_server_config(
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 8080)),
        "https://monitor.example.com".to_string(),
        temp_dir.join("server.json"),
        temp_dir.join("history.sqlite3"),
        temp_dir.join("snapshot.json"),
    );
    config.readonly_auth = Some(nodelite_proto::ReadonlyAuthConfig {
        username: "viewer".to_string(),
        password: "secret".to_string(),
        enable_2fa,
        totp_secret: enable_2fa.then(|| "JBSWY3DPEHPK3PXP".to_string()),
    });
    let state = AppState::test_fixture(config.into(), Arc::new(temp_dir.join("server.toml")))
        .await
        .expect("state fixture should build");
    (state, temp_dir)
}

pub(crate) fn trusted_proxies(cidrs: &[&str]) -> Vec<IpNet> {
    cidrs
        .iter()
        .map(|cidr| cidr.parse::<IpNet>().expect("valid cidr"))
        .collect()
}

pub(crate) async fn protected_ok() -> StatusCode {
    StatusCode::OK
}

pub(crate) fn protected_request(
    method: &str,
    uri: &str,
    auth_header: Option<&str>,
    peer_addr: SocketAddr,
) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(auth_header) = auth_header {
        builder = builder.header(header::AUTHORIZATION, auth_header);
    }
    let mut request = builder.body(Body::empty()).expect("request should build");
    request.extensions_mut().insert(ConnectInfo(peer_addr));
    request
}

/// Build a GET request with websocket upgrade headers to simulate a browser
/// handshake in readonly-auth middleware tests.
pub(crate) fn ws_upgrade_request(
    uri: &str,
    auth_header: Option<&str>,
    peer_addr: SocketAddr,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method("GET")
        .uri(uri)
        .header(header::CONNECTION, "Upgrade")
        .header(header::UPGRADE, "websocket");
    if let Some(auth_header) = auth_header {
        builder = builder.header(header::AUTHORIZATION, auth_header);
    }
    let mut request = builder.body(Body::empty()).expect("request should build");
    request.extensions_mut().insert(ConnectInfo(peer_addr));
    request
}

pub(crate) fn json_request(
    method: &str,
    uri: &str,
    auth_header: Option<&str>,
    body: impl Into<Body>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(auth_header) = auth_header {
        builder = builder.header(header::AUTHORIZATION, auth_header);
    }
    let mut request = builder.body(body.into()).expect("request should build");
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::V4(SocketAddrV4::new(
            Ipv4Addr::LOCALHOST,
            51234,
        ))));
    request
}

pub(crate) fn json_write_routes() -> [(&'static str, Option<&'static str>); 7] {
    [
        ("/api/verify-2fa", None),
        (
            "/api/nodes/test-node/refresh-token",
            Some(TEST_BASIC_AUTH_HEADER),
        ),
        ("/api/settings/password", Some(TEST_BASIC_AUTH_HEADER)),
        ("/api/settings/alerts", Some(TEST_BASIC_AUTH_HEADER)),
        ("/api/settings/update/server", Some(TEST_BASIC_AUTH_HEADER)),
        ("/api/settings/2fa/enable", Some(TEST_BASIC_AUTH_HEADER)),
        ("/api/settings/2fa/disable", Some(TEST_BASIC_AUTH_HEADER)),
    ]
}

pub(crate) fn small_json_write_requests(
) -> [(&'static str, Option<&'static str>, &'static str); 7] {
    [
        ("/api/verify-2fa", None, r#"{"code":"000000"}"#),
        (
            "/api/nodes/test-node/refresh-token",
            Some(TEST_BASIC_AUTH_HEADER),
            r#"{"current_password":"wrong"}"#,
        ),
        (
            "/api/settings/password",
            Some(TEST_BASIC_AUTH_HEADER),
            r#"{"current_password":"wrong","new_password":"new-secret-password"}"#,
        ),
        (
            "/api/settings/alerts",
            Some(TEST_BASIC_AUTH_HEADER),
            r#"{"current_password":"wrong","enabled":false,"smtp":{"enabled":false,"host":"","port":587,"username":"","password":null,"sender":"","recipients":[],"transport":"starttls","send_resolved":true},"webhook":{"enabled":false,"url":"","secret":null,"send_resolved":true},"rules":[],"inspection":{"enabled":false,"local_time":"09:00","lookback_hours":24,"delivery":[],"offline_grace_minutes":10,"latency_warn_ms":250,"cpu_warn_percent":85,"memory_warn_percent":90}}"#,
        ),
        (
            "/api/settings/update/server",
            Some(TEST_BASIC_AUTH_HEADER),
            r#"{"current_password":"wrong"}"#,
        ),
        (
            "/api/settings/2fa/enable",
            Some(TEST_BASIC_AUTH_HEADER),
            r#"{"current_password":"wrong","secret":"JBSWY3DPEHPK3PXP","code":"000000"}"#,
        ),
        (
            "/api/settings/2fa/disable",
            Some(TEST_BASIC_AUTH_HEADER),
            r#"{"current_password":"wrong","code":"000000"}"#,
        ),
    ]
}
