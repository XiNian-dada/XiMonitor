use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use axum::Router;
use axum::middleware::from_fn_with_state;
use axum::routing::get;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tower_http::trace::TraceLayer;

use super::AgentCredential;
use crate::handlers::{
    node_history, node_logs, node_status, nodes, overview, require_readonly_auth,
};
use crate::history::HistoryStore;
use crate::registry::{IssueNodeRequest, issue_node};
use crate::state::SharedState;
use crate::test_support::{test_server_config, test_ws_config};
use crate::ws::ws_handler;

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

        let mut config = test_server_config(
            addr,
            format!("http://{addr}"),
            registry_path,
            history_path,
            snapshot_path,
        );
        config.ws = test_ws_config(node_count.saturating_add(32), node_count.saturating_add(32));
        config.stale_after_secs = 20;
        let config = std::sync::Arc::new(config);
        let state = crate::AppState::test_fixture(
            config,
            std::sync::Arc::new(temp_dir.join("server.toml")),
        )
        .await?;
        let history = state.history.clone();

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
