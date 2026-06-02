//! Security-header and request-body policy tests.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::middleware::{from_fn, from_fn_with_state};
use axum::routing::get;
use tokio::runtime::Runtime;
use tower::util::ServiceExt;

use super::support::{
    assert_security_headers, json_request, json_write_routes, small_json_write_requests,
};
use super::{AppState, set_protected_response_headers};
use crate::handlers::{index, logout_and_reauth, require_readonly_auth, verify_2fa_page};
use crate::test_support::{test_server_config, test_ws_config};

#[test]
fn protected_routes_attach_security_headers() {
    let runtime = Runtime::new().expect("runtime should build");
    runtime.block_on(async {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic enough")
            .as_nanos();
        let temp_dir =
            std::env::temp_dir().join(format!("nodelite-protected-header-test-{unique}"));
        std::fs::create_dir_all(&temp_dir).expect("temp dir should exist");
        let registry_path = temp_dir.join("server.json");
        let mut config = test_server_config(
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 8080)),
            "https://monitor.example.com".to_string(),
            registry_path.clone(),
            temp_dir.join("history.sqlite3"),
            temp_dir.join("snapshot.json"),
        );
        config.readonly_auth = None;
        config.ws = test_ws_config(32, 8);
        config.stale_after_secs = 20;
        config.ping_interval_secs = 10;
        config.ignored_filesystems = Vec::new();
        let state = AppState::test_fixture(config.into(), Arc::new(temp_dir.join("server.toml")))
            .await
            .expect("state fixture should build");
        let app: Router = Router::new()
            .route("/", get(index))
            .route_layer(from_fn(set_protected_response_headers))
            .route_layer(from_fn_with_state(state.clone(), require_readonly_auth))
            .with_state(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("response should be produced");

        assert_eq!(response.status(), StatusCode::OK);
        let index_csp = response
            .headers()
            .get(header::CONTENT_SECURITY_POLICY)
            .expect("spa index should set a CSP")
            .to_str()
            .expect("CSP should be valid ascii");
        assert!(
            index_csp.contains("script-src 'self' 'sha256-"),
            "spa index CSP should pin its inline shim: {index_csp}"
        );
        assert!(
            !index_csp.contains("'unsafe-inline'"),
            "spa index CSP must not relax to unsafe-inline: {index_csp}"
        );
        assert!(
            index_csp.contains("frame-ancestors 'none'"),
            "spa index CSP should retain the strict directives: {index_csp}"
        );
        assert_security_headers(response.headers());

        let _ = std::fs::remove_dir_all(&temp_dir);
    });
}

#[test]
fn public_auth_routes_attach_security_headers() {
    let runtime = Runtime::new().expect("runtime should build");
    runtime.block_on(async {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic enough")
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!("nodelite-public-header-test-{unique}"));
        std::fs::create_dir_all(&temp_dir).expect("temp dir should exist");
        let registry_path = temp_dir.join("server.json");
        let mut config = test_server_config(
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 8080)),
            "https://monitor.example.com".to_string(),
            registry_path,
            temp_dir.join("history.sqlite3"),
            temp_dir.join("snapshot.json"),
        );
        config.ws = test_ws_config(32, 8);
        let state = AppState::test_fixture(config.into(), Arc::new(temp_dir.join("server.toml")))
            .await
            .expect("state fixture should build");
        let app: Router = Router::new()
            .route("/verify-2fa", get(verify_2fa_page))
            .route("/logout-and-reauth", get(logout_and_reauth))
            .route_layer(from_fn(set_protected_response_headers))
            .with_state(state);

        let verify_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/verify-2fa")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("response should be produced");
        assert_eq!(verify_response.status(), StatusCode::OK);
        let verify_csp = verify_response
            .headers()
            .get(header::CONTENT_SECURITY_POLICY)
            .expect("verify-2fa should set a CSP")
            .to_str()
            .expect("CSP should be valid ascii");
        assert!(
            verify_csp.contains("script-src 'self' 'sha256-"),
            "verify-2fa CSP should pin its inline script: {verify_csp}"
        );
        assert!(
            verify_csp.contains("style-src 'self' 'unsafe-inline'"),
            "verify-2fa CSP should allow its inline styles: {verify_csp}"
        );
        assert_security_headers(verify_response.headers());

        let reauth_response = app
            .oneshot(
                Request::builder()
                    .uri("/logout-and-reauth")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("response should be produced");
        assert_eq!(reauth_response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            reauth_response
                .headers()
                .get(header::CONTENT_SECURITY_POLICY),
            Some(&header::HeaderValue::from_static(
                crate::startup::PROTECTED_CONTENT_SECURITY_POLICY,
            )),
        );
        assert_security_headers(reauth_response.headers());

        let _ = std::fs::remove_dir_all(&temp_dir);
    });
}

#[test]
fn json_write_routes_reject_oversized_bodies() {
    let runtime = Runtime::new().expect("runtime should build");
    runtime.block_on(async {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic enough")
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!("nodelite-json-limit-test-{unique}"));
        std::fs::create_dir_all(&temp_dir).expect("temp dir should exist");
        let registry_path = temp_dir.join("server.json");
        let config = test_server_config(
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 8080)),
            "https://monitor.example.com".to_string(),
            registry_path,
            temp_dir.join("history.sqlite3"),
            temp_dir.join("snapshot.json"),
        );
        let state = AppState::test_fixture(config.into(), Arc::new(temp_dir.join("server.toml")))
            .await
            .expect("state fixture should build");
        let app = crate::startup::build_router(state);

        let oversized_body = format!(
            "{{\"blob\":\"{}\"}}",
            "x".repeat(crate::startup::JSON_WRITE_BODY_LIMIT_BYTES + 1)
        );
        for (uri, auth_header) in json_write_routes() {
            let response = app
                .clone()
                .oneshot(json_request(
                    "POST",
                    uri,
                    auth_header,
                    oversized_body.clone(),
                ))
                .await
                .expect("response should be produced");
            assert_eq!(
                response.status(),
                StatusCode::PAYLOAD_TOO_LARGE,
                "{uri} should reject oversized JSON bodies",
            );
        }

        for (uri, auth_header, body) in small_json_write_requests() {
            let response = app
                .clone()
                .oneshot(json_request("POST", uri, auth_header, body))
                .await
                .expect("response should be produced");
            assert_ne!(
                response.status(),
                StatusCode::PAYLOAD_TOO_LARGE,
                "{uri} should still accept normal-sized JSON bodies",
            );
        }

        let _ = std::fs::remove_dir_all(&temp_dir);
    });
}
