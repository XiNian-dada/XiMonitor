//! Route-surface and router-build library-unit tests.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, HeaderValue, Request, StatusCode, header};
use axum::routing::get;
use tokio::runtime::Runtime;
use tower::util::ServiceExt;
use tower_http::trace::TraceLayer;

use super::AppState;
use crate::handlers::{
    audit_log, bootstrap, healthz, index, install_agent_script, install_bootstrap, node_detail,
    node_history, node_logs, node_status, nodes, overview, readyz, static_asset,
};
use crate::registry::{IssueNodeRequest, issue_node};
use crate::test_support::{test_server_config, test_ws_config};
use crate::ws::ws_handler;

#[test]
fn router_builds_with_v08_path_syntax() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be monotonic enough")
        .as_nanos();
    let registry_path = std::env::temp_dir().join(format!("nodelite-router-test-{unique}.json"));
    let mut config = test_server_config(
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 8080)),
        "http://127.0.0.1:8080".to_string(),
        registry_path,
        PathBuf::from("./data/history.sqlite3"),
        PathBuf::from("./data/snapshot.json"),
    );
    config.readonly_auth = None;
    config.ws = test_ws_config(32, 8);
    config.stale_after_secs = 20;
    config.ping_interval_secs = 10;
    config.ignored_filesystems = vec!["tmpfs".to_string()];
    let config = Arc::new(config);
    let runtime = Runtime::new().expect("runtime should build");
    let state = runtime
        .block_on(AppState::test_fixture(
            config,
            Arc::new(PathBuf::from("config/server.toml")),
        ))
        .expect("state fixture should build");

    let _app: Router = Router::new()
        .route("/", get(index))
        .route("/nodes/{node_id}", get(node_detail))
        .route("/assets/{*path}", get(static_asset))
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/install/install-agent.sh", get(install_agent_script))
        .route("/install/bootstrap", get(install_bootstrap))
        .route("/api/bootstrap", get(bootstrap))
        .route("/api/overview", get(overview))
        .route("/api/nodes", get(nodes))
        .route("/api/audit-log", get(audit_log))
        .route("/api/nodes/{node_id}", get(node_status))
        .route("/api/nodes/{node_id}/history", get(node_history))
        .route("/api/nodes/{node_id}/logs", get(node_logs))
        .route("/ws", get(ws_handler))
        .with_state(state)
        .layer(TraceLayer::new_for_http());
}

#[test]
fn install_endpoints_disable_caching() {
    let runtime = Runtime::new().expect("runtime should build");
    runtime.block_on(async {
        let script_response = install_agent_script().await;
        assert_eq!(
            script_response.headers().get(header::CACHE_CONTROL),
            Some(&header::HeaderValue::from_static(
                "no-store, no-cache, must-revalidate",
            )),
        );
        assert_eq!(
            script_response.headers().get(header::PRAGMA),
            Some(&header::HeaderValue::from_static("no-cache")),
        );

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic enough")
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!("nodelite-bootstrap-cache-test-{unique}"));
        std::fs::create_dir_all(&temp_dir).expect("temp dir should exist");
        let registry_path = temp_dir.join("server.json");
        let issued = issue_node(
            &registry_path,
            IssueNodeRequest {
                node_id: "osaka-01".to_string(),
                node_label: Some("Osaka 01".to_string()),
                tags: Vec::new(),
            },
        )
        .await
        .expect("node should be issued");
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
        let request = Request::builder()
            .uri("/install/bootstrap")
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", issued.install_token),
            )
            .body(Body::empty())
            .expect("request should build");
        let peer_addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 51234));
        let bootstrap_response = install_bootstrap(
            State(state),
            ConnectInfo(peer_addr),
            HeaderMap::new(),
            request,
        )
        .await;
        assert_eq!(bootstrap_response.status(), StatusCode::OK);
        assert_eq!(
            bootstrap_response.headers().get(header::CACHE_CONTROL),
            Some(&header::HeaderValue::from_static(
                "no-store, no-cache, must-revalidate",
            )),
        );
        assert_eq!(
            bootstrap_response.headers().get(header::PRAGMA),
            Some(&header::HeaderValue::from_static("no-cache")),
        );

        let _ = std::fs::remove_dir_all(&temp_dir);
    });
}

#[test]
fn router_compresses_text_assets_but_not_webp() {
    let runtime = Runtime::new().expect("runtime should build");
    runtime.block_on(async {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic enough")
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!("nodelite-compression-test-{unique}"));
        std::fs::create_dir_all(&temp_dir).expect("temp dir should exist");
        let registry_path = temp_dir.join("server.json");
        let mut config = test_server_config(
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 8080)),
            "https://monitor.example.com".to_string(),
            registry_path,
            temp_dir.join("history.sqlite3"),
            temp_dir.join("snapshot.json"),
        );
        config.readonly_auth = None;
        config.ws = test_ws_config(32, 8);
        let state = AppState::test_fixture(config.into(), Arc::new(temp_dir.join("server.toml")))
            .await
            .expect("state fixture should build");
        let app = crate::startup::build_router(state.clone());

        for path in ["/", "/assets/ui-i18n.json", "/metrics"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .header(header::ACCEPT_ENCODING, "br, gzip")
                        .body(Body::empty())
                        .expect("request should build"),
                )
                .await
                .expect("response should be produced");
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            assert!(
                response
                    .headers()
                    .get(header::CONTENT_ENCODING)
                    .is_some_and(|value| value == "br" || value == "gzip"),
                "{path} should be compressed",
            );
        }

        let webp_response = app
            .oneshot(
                Request::builder()
                    .uri("/assets/brand-logo-dark.webp")
                    .header(header::ACCEPT_ENCODING, "br, gzip")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("response should be produced");
        assert_eq!(webp_response.status(), StatusCode::OK);
        assert_eq!(
            webp_response.headers().get(header::CONTENT_TYPE),
            Some(&HeaderValue::from_static("image/webp")),
        );
        assert!(
            webp_response
                .headers()
                .get(header::CONTENT_ENCODING)
                .is_none(),
            "WebP assets should not be recompressed",
        );

        state.history.shutdown().await;
        state.audit_log.shutdown().await;
        let _ = std::fs::remove_dir_all(&temp_dir);
    });
}

#[test]
fn spa_history_mode_routes_serve_index_shell() {
    let runtime = Runtime::new().expect("runtime should build");
    runtime.block_on(async {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic enough")
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!("nodelite-spa-routes-test-{unique}"));
        std::fs::create_dir_all(&temp_dir).expect("temp dir should exist");
        let registry_path = temp_dir.join("server.json");
        let mut config = test_server_config(
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 8080)),
            "https://monitor.example.com".to_string(),
            registry_path,
            temp_dir.join("history.sqlite3"),
            temp_dir.join("snapshot.json"),
        );
        config.readonly_auth = None;
        config.ws = test_ws_config(32, 8);
        let state = AppState::test_fixture(config.into(), Arc::new(temp_dir.join("server.toml")))
            .await
            .expect("state fixture should build");
        let app = crate::startup::build_router(state.clone());

        for path in ["/", "/nodes/osaka-01", "/settings", "/account", "/alerts"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .body(Body::empty())
                        .expect("request should build"),
                )
                .await
                .expect("response should be produced");
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "{path} should serve the SPA shell"
            );
            assert_eq!(
                response.headers().get(header::CONTENT_TYPE),
                Some(&HeaderValue::from_static("text/html; charset=utf-8")),
                "{path} should return index.html",
            );
        }

        state.history.shutdown().await;
        state.audit_log.shutdown().await;
        let _ = std::fs::remove_dir_all(&temp_dir);
    });
}
