use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use axum::Router;
use axum::middleware::from_fn_with_state;
use axum::routing::get;
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tower_http::trace::TraceLayer;

use super::AgentCredential;
use crate::AppState;
use crate::ServerReadiness;
use crate::admission::{InstallAdmissionConfig, InstallAdmissionController, WsAdmissionController};
use crate::agent_logs::AgentLogStore;
use crate::auth::{ReadonlyRouteAuth, TwoFactorSessions};
use crate::handlers::{
    node_history, node_logs, node_status, nodes, overview, require_readonly_auth,
};
use crate::history::HistoryStore;
use crate::registry::{IssueNodeRequest, NodeRegistry, issue_node};
use crate::state::SharedState;
use crate::ws::ws_handler;
use nodelite_proto::{ReadonlyAuthConfig, ServerConfig, WsConfig};

pub(super) struct TestServer {
    pub(super) addr: SocketAddr,
    pub(super) shared: SharedState,
    pub(super) history: HistoryStore,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    server_handle: JoinHandle<Result<(), std::io::Error>>,
    temp_dir: PathBuf,
}

impl TestServer {
    pub(super) async fn start(node_count: usize) -> Result<(Self, Vec<AgentCredential>)> {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should move forward")
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!("nodelite-load-test-{unique}"));
        tokio::fs::create_dir_all(&temp_dir)
            .await
            .with_context(|| format!("create temp dir {}", temp_dir.display()))?;

        let listener =
            TcpListener::bind(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))).await?;
        let addr = listener.local_addr()?;
        let registry_path = temp_dir.join("server.json");
        let history_path = temp_dir.join("history.sqlite3");
        let snapshot_path = temp_dir.join("snapshot.json");

        let mut credentials = Vec::with_capacity(node_count);
        for index in 0..node_count {
            let node_id = format!("load-node-{index:03}");
            let node_label = format!("Load Node {index:03}");
            let issued = issue_node(
                &registry_path,
                IssueNodeRequest {
                    node_id: node_id.clone(),
                    node_label: Some(node_label.clone()),
                    tags: vec!["load-test".to_string()],
                    rotate_token: false,
                },
            )
            .await
            .with_context(|| format!("issue node {node_id}"))?;
            credentials.push(AgentCredential {
                node_id,
                node_label,
                token: issued.node_session_token,
            });
        }

        let config = Arc::new(ServerConfig {
            listen: addr,
            public_base_url: format!("http://{addr}"),
            insecure_allow_http: false,
            readonly_auth: Some(ReadonlyAuthConfig {
                username: "viewer".to_string(),
                password: "secret".to_string(),
                enable_2fa: false,
                totp_secret: None,
            }),
            ws: WsConfig {
                max_total_connections: node_count.saturating_add(32),
                max_connections_per_ip: node_count.saturating_add(32),
                auth_fail_window_secs: 300,
                auth_fail_max_attempts: 12,
                auth_block_secs: 900,
            },
            node_registry_path: registry_path.clone(),
            history_db_path: history_path.clone(),
            snapshot_path: snapshot_path.clone(),
            stale_after_secs: 20,
            ping_interval_secs: 60,
            max_message_bytes: 64 * 1024,
            refresh_interval_secs: 5,
            ignored_filesystems: vec!["tmpfs".to_string(), "devtmpfs".to_string()],
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
        });

        let history = HistoryStore::new(history_path, 5);
        history.initialize().await;
        let readiness = ServerReadiness::new(history.is_available());
        let state = AppState {
            history: history.clone(),
            agent_logs: AgentLogStore::new(),
            install_admission: InstallAdmissionController::new(InstallAdmissionConfig {
                auth_fail_window_secs: config.ws.auth_fail_window_secs,
                auth_fail_max_attempts: config.ws.auth_fail_max_attempts,
                auth_block_secs: config.ws.auth_block_secs,
            }),
            verify_2fa_admission: InstallAdmissionController::new(InstallAdmissionConfig {
                auth_fail_window_secs: config.ws.auth_fail_window_secs,
                auth_fail_max_attempts: config.ws.auth_fail_max_attempts,
                auth_block_secs: config.ws.auth_block_secs,
            }),
            readiness,
            registry: NodeRegistry::load(&registry_path).await?,
            shared: SharedState::new(config.clone()),
            ws_admission: WsAdmissionController::new(&config.ws),
            readonly_auth: Arc::new(RwLock::new(ReadonlyRouteAuth::from_config(
                config.readonly_auth.clone(),
            ))),
            two_factor_sessions: TwoFactorSessions::new(),
            config_path: Arc::new(temp_dir.join("server.toml")),
            shutdown: tokio_util::sync::CancellationToken::new(),
        };

        let shared = state.shared.clone();
        let protected_routes = Router::new()
            .route("/api/overview", get(overview))
            .route("/api/nodes", get(nodes))
            .route("/api/nodes/{node_id}", get(node_status))
            .route("/api/nodes/{node_id}/history", get(node_history))
            .route("/api/nodes/{node_id}/logs", get(node_logs))
            .route_layer(from_fn_with_state(state.clone(), require_readonly_auth));
        let app = Router::new()
            .route("/ws", get(ws_handler))
            .merge(protected_routes)
            .with_state(state)
            .layer(TraceLayer::new_for_http());

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let server_handle = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
        });

        Ok((
            Self {
                addr,
                shared,
                history,
                shutdown_tx: Some(shutdown_tx),
                server_handle,
                temp_dir,
            },
            credentials,
        ))
    }

    pub(super) async fn shutdown(mut self) -> Result<()> {
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }
        let result = self
            .server_handle
            .await
            .map_err(|error| anyhow!("join server task: {error}"))?;
        result.map_err(|error| anyhow!("server task: {error}"))?;
        let _ = tokio::fs::remove_dir_all(&self.temp_dir).await;
        Ok(())
    }
}
