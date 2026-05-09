mod history;
mod registry;
mod snapshot;
mod state;
mod ui;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use axum::extract::Request;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path as AxumPath, State};
use axum::http::{StatusCode, header};
use axum::middleware::{Next, from_fn_with_state};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use base64::Engine;
use clap::{Parser, Subcommand};
use futures::{SinkExt, StreamExt};
use serde::Serialize;
use tokio::fs;
use tokio::net::TcpListener;
use tokio::time::interval;
use tower_http::trace::TraceLayer;
use tracing::{error, info, warn};
use url::Url;
use ximonitor_proto::{
    HelloMessage, MetricsMessage, NodeSnapshot, PingMessage, PongMessage, ReadonlyAuthConfig,
    ServerConfig, ServerNoticeMessage, WireMessage, parse_server_config,
};

use crate::history::HistoryStore;
use crate::registry::{
    IssueNodeRequest, NodeRegistry, build_install_script_url, issue_node, render_agent_config,
    render_install_command,
};
use crate::snapshot::{load_snapshot, spawn_snapshot_persistor};
use crate::state::SharedState;
use crate::ui::{index_html, node_html};

#[derive(Debug, Parser)]
#[command(name = "ximonitor-server")]
#[command(about = "XiMonitor central server")]
struct Cli {
    #[arg(long, global = true, default_value = "config/server.toml")]
    config: PathBuf,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    IssueNode(IssueNodeArgs),
}

#[derive(Debug, Parser)]
struct IssueNodeArgs {
    #[arg(long)]
    node_id: String,
    #[arg(long)]
    node_label: Option<String>,
    #[arg(long = "tag")]
    tags: Vec<String>,
    #[arg(long)]
    rotate_token: bool,
}

#[derive(Clone)]
struct AppState {
    history: HistoryStore,
    registry: NodeRegistry,
    shared: SharedState,
}

#[derive(Debug, Clone)]
struct ReadonlyRouteAuth {
    expected_authorization: Option<String>,
}

#[derive(Debug, Serialize)]
struct BootstrapResponse {
    service: &'static str,
    status: &'static str,
    public_base_url: String,
    refresh_interval_secs: u64,
    registered_nodes: usize,
}

#[derive(Debug)]
enum ProtocolError {
    Client(String),
    Server(anyhow::Error),
}

#[derive(Debug)]
enum ParsedFrame {
    Wire(Box<WireMessage>),
    Control,
    Close,
}

const INSTALL_AGENT_SCRIPT: &str = include_str!("../../scripts/install-agent.sh");
const HELLO_TIMEOUT_SECS: u64 = 10;
const MAX_OUTSTANDING_PINGS: usize = 32;
const INSECURE_TRANSPORT_WARN_INTERVAL_SECS: u64 = 900;

impl From<anyhow::Error> for ProtocolError {
    fn from(error: anyhow::Error) -> Self {
        Self::Server(error)
    }
}

impl ReadonlyRouteAuth {
    fn from_config(config: Option<ReadonlyAuthConfig>) -> Self {
        let expected_authorization = config.map(|config| {
            let credentials = format!("{}:{}", config.username, config.password);
            let encoded = base64::engine::general_purpose::STANDARD.encode(credentials);
            format!("Basic {encoded}")
        });
        Self {
            expected_authorization,
        }
    }

    fn is_authorized(&self, request: &Request) -> bool {
        let Some(expected_authorization) = self.expected_authorization.as_deref() else {
            return true;
        };

        request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            == Some(expected_authorization)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let cli = Cli::parse();
    match cli.command {
        Some(Command::IssueNode(args)) => issue_node_command(cli.config.as_path(), args).await,
        None => run_server(cli.config.as_path()).await,
    }
}

async fn run_server(config_path: &Path) -> Result<()> {
    let config = Arc::new(load_server_config(config_path).await?);
    let listen_addr = config.listen;
    let public_base_url = config.public_base_url.clone();
    let refresh_interval_secs = config.refresh_interval_secs;
    let readonly_route_auth = ReadonlyRouteAuth::from_config(config.readonly_auth.clone());
    let registry = NodeRegistry::load(config.node_registry_path.as_path())
        .await
        .with_context(|| {
            format!(
                "failed to load node registry {}",
                config.node_registry_path.display()
            )
        })?;
    let shared = SharedState::new(Arc::clone(&config));
    let history = HistoryStore::new(config.history_db_path.clone());
    history.initialize().await;
    restore_snapshot_if_available(&shared, config.snapshot_path.as_path()).await;

    spawn_registry_reloader(registry.clone());
    spawn_stale_reaper(shared.clone());
    spawn_snapshot_persistor(shared.clone(), config.snapshot_path.clone());
    spawn_insecure_transport_warning(config.public_base_url.clone(), config.listen);

    let enrolled_nodes = registry.count().await;
    info!(
        registry_path = %config.node_registry_path.display(),
        enrolled_nodes,
        "node registry loaded",
    );

    let state = AppState {
        history,
        registry,
        shared,
    };
    let protected_routes = Router::new()
        .route("/", get(index))
        .route("/nodes/{node_id}", get(node_detail))
        .route("/api/bootstrap", get(bootstrap))
        .route("/api/overview", get(overview))
        .route("/api/nodes", get(nodes))
        .route("/api/nodes/{node_id}", get(node_status))
        .route("/api/nodes/{node_id}/history", get(node_history))
        .route_layer(from_fn_with_state(
            readonly_route_auth,
            require_readonly_auth,
        ));
    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/install/install-agent.sh", get(install_agent_script))
        .route("/install/bootstrap", get(install_bootstrap))
        .route("/ws", get(ws_handler))
        .merge(protected_routes)
        .with_state(state)
        .layer(TraceLayer::new_for_http());

    let listener = TcpListener::bind(listen_addr)
        .await
        .with_context(|| format!("failed to bind server listener to {listen_addr}"))?;

    info!(
        listen = %listen_addr,
        public_base_url = %public_base_url,
        refresh_interval_secs,
        "ximonitor server listening",
    );

    axum::serve(listener, app)
        .await
        .context("server exited unexpectedly")
}

async fn issue_node_command(config_path: &Path, args: IssueNodeArgs) -> Result<()> {
    let config = load_server_config(config_path).await?;
    let issued = issue_node(
        config.node_registry_path.as_path(),
        IssueNodeRequest {
            node_id: args.node_id,
            node_label: args.node_label,
            tags: args.tags,
            rotate_token: args.rotate_token,
        },
    )
    .await?;

    let install_command = render_install_command(
        &config.public_base_url,
        config.agent_release_base_url.as_deref(),
        config.agent_release_sha256_x86_64.as_deref(),
        config.agent_release_sha256_aarch64.as_deref(),
    )?;
    let agent_config = render_agent_config(&config.public_base_url, &issued.node)?;
    let install_script_url = build_install_script_url(&config.public_base_url)?;
    let action = if issued.created {
        "created"
    } else if issued.rotated_token {
        "rotated"
    } else {
        "reused"
    };

    println!("node_id: {}", issued.node.node_id);
    println!("node_label: {}", issued.node.node_label);
    println!("status: {action}");
    println!("registry_path: {}", config.node_registry_path.display());
    println!("install_script_url: {install_script_url}");
    println!("install_token: {}", issued.install_token);
    println!(
        "install_token_expires_at: {}",
        issued.install_token_expires_at.to_rfc3339()
    );
    println!();
    println!("# agent.toml");
    println!("{agent_config}");
    println!("# install command");
    println!("{install_command}");
    println!();
    println!("note: the installer will prompt for the one-time install token shown above.");

    if config.agent_release_base_url.is_none() {
        println!();
        println!(
            "note: set [install].agent_release_base_url in server.toml to print a fully self-contained install command."
        );
    }

    Ok(())
}

async fn load_server_config(path: &Path) -> Result<ServerConfig> {
    let content = fs::read_to_string(path)
        .await
        .with_context(|| format!("failed to read config file {}", path.display()))?;
    let config = parse_server_config(&content)
        .map_err(|error| anyhow!("failed to parse {}: {error}", path.display()))?;

    if let Some(parent) = config.snapshot_path.parent()
        && !parent.as_os_str().is_empty()
        && !parent.exists()
    {
        warn!(
            snapshot_dir = %parent.display(),
            "snapshot directory does not exist yet; it will be created later",
        );
    }
    if let Some(parent) = config.history_db_path.parent()
        && !parent.as_os_str().is_empty()
        && !parent.exists()
    {
        warn!(
            history_dir = %parent.display(),
            "history directory does not exist yet; it will be created later",
        );
    }

    Ok(config)
}

async fn index(State(state): State<AppState>) -> Html<String> {
    Html(index_html(state.shared.config().refresh_interval_secs))
}

async fn node_detail(
    State(state): State<AppState>,
    AxumPath(node_id): AxumPath<String>,
) -> Html<String> {
    Html(node_html(
        &node_id,
        state.shared.config().refresh_interval_secs,
    ))
}

async fn healthz() -> StatusCode {
    StatusCode::OK
}

async fn require_readonly_auth(
    State(auth): State<ReadonlyRouteAuth>,
    request: Request,
    next: Next,
) -> Response {
    if auth.is_authorized(&request) {
        return next.run(request).await;
    }

    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Basic realm=\"XiMonitor\"")],
        "authentication required",
    )
        .into_response()
}

async fn bootstrap(State(state): State<AppState>) -> impl IntoResponse {
    Json(BootstrapResponse {
        service: "ximonitor-server",
        status: "ok",
        public_base_url: state.shared.config().public_base_url.clone(),
        refresh_interval_secs: state.shared.config().refresh_interval_secs,
        registered_nodes: state.registry.count().await,
    })
}

async fn install_agent_script() -> Response {
    (
        [(header::CONTENT_TYPE, "text/x-shellscript; charset=utf-8")],
        INSTALL_AGENT_SCRIPT,
    )
        .into_response()
}

async fn install_bootstrap(State(state): State<AppState>, request: Request) -> Response {
    let Some(token) = bearer_token_from_request(&request) else {
        return (
            StatusCode::UNAUTHORIZED,
            [(
                header::WWW_AUTHENTICATE,
                "Bearer realm=\"XiMonitor Installer\"",
            )],
            "missing install token",
        )
            .into_response();
    };

    let node = match state.registry.consume_install_token(token).await {
        Ok(Some(node)) => node,
        Ok(None) => {
            return (
                StatusCode::UNAUTHORIZED,
                [(
                    header::WWW_AUTHENTICATE,
                    "Bearer realm=\"XiMonitor Installer\"",
                )],
                "invalid install token",
            )
                .into_response();
        }
        Err(error) => {
            error!(error = ?error, "failed to consume install token");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to prepare agent bootstrap",
            )
                .into_response();
        }
    };

    match render_agent_config(&state.shared.config().public_base_url, &node) {
        Ok(agent_config) => (
            [(header::CONTENT_TYPE, "application/toml; charset=utf-8")],
            agent_config,
        )
            .into_response(),
        Err(error) => {
            error!(error = ?error, "failed to render agent bootstrap config");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to render agent bootstrap config",
            )
                .into_response()
        }
    }
}

async fn overview(State(state): State<AppState>) -> impl IntoResponse {
    Json(state.shared.overview().await)
}

async fn nodes(State(state): State<AppState>) -> impl IntoResponse {
    Json(state.shared.list_statuses().await)
}

async fn node_status(
    State(state): State<AppState>,
    AxumPath(node_id): AxumPath<String>,
) -> Response {
    match state.shared.get_status(&node_id).await {
        Some(status) => Json(status).into_response(),
        None => (StatusCode::NOT_FOUND, "node not found").into_response(),
    }
}

async fn node_history(
    State(state): State<AppState>,
    AxumPath(node_id): AxumPath<String>,
) -> Response {
    match state.history.query_recent_history(&node_id).await {
        Ok(points) => Json(points).into_response(),
        Err(error) => {
            error!(node_id = %node_id, error = ?error, "failed to query node history");
            (StatusCode::SERVICE_UNAVAILABLE, "history store unavailable").into_response()
        }
    }
}

async fn ws_handler(State(state): State<AppState>, ws: WebSocketUpgrade) -> impl IntoResponse {
    let max_message_bytes = state.shared.config().max_message_bytes;
    ws.max_frame_size(max_message_bytes)
        .max_message_size(max_message_bytes)
        .on_upgrade(move |socket| async move {
            if let Err(error) = handle_socket(state, socket).await {
                match error {
                    ProtocolError::Client(message) => {
                        warn!(reason = %message, "websocket client disconnected");
                    }
                    ProtocolError::Server(error) => {
                        error!(error = ?error, "websocket session failed");
                    }
                }
            }
        })
}

async fn handle_socket(state: AppState, mut socket: WebSocket) -> Result<(), ProtocolError> {
    let shared = state.shared.clone();
    let hello = tokio::time::timeout(
        Duration::from_secs(HELLO_TIMEOUT_SECS),
        recv_hello(&mut socket),
    )
    .await
    .map_err(|_| ProtocolError::Client("timed out waiting for hello message".to_string()))??;
    let session_token = hello.token.clone();
    let identity = state
        .registry
        .authorize(&hello.identity, &session_token)
        .await
        .map_err(|error| ProtocolError::Client(error.to_string()))?;

    let node_id = identity.node_id.clone();
    let node_label = identity.node_label.clone();
    let session_id = shared.register_node(identity).await;

    info!(node_id = %node_id, node_label = %node_label, session_id, "node authenticated");

    let session_result: Result<(), ProtocolError> = async {
        let notice = WireMessage::ServerNotice(ServerNoticeMessage {
            level: ximonitor_proto::NoticeLevel::Info,
            message: "authenticated".to_string(),
        });
        send_wire_message(&mut socket, &notice).await?;

        let (mut sender, mut receiver) = socket.split();
        let ping_every = Duration::from_secs(shared.config().ping_interval_secs);
        let ping_expiry = Duration::from_secs(shared.config().ping_interval_secs.saturating_mul(3));
        let mut ping_ticker = interval(ping_every);
        let mut outstanding_pings: HashMap<u64, Instant> = HashMap::new();
        let mut next_ping_nonce = 1_u64;

        loop {
            tokio::select! {
                incoming = receiver.next() => {
                    let Some(frame) = incoming else {
                        break Ok(());
                    };
                    let frame = frame.map_err(|error| anyhow!("websocket receive failed: {error}"))?;

                    match parse_wire_message(frame)? {
                        ParsedFrame::Close => break Ok(()),
                        ParsedFrame::Control => continue,
                        ParsedFrame::Wire(message) => match *message {
                            WireMessage::Metrics(MetricsMessage { snapshot }) => {
                                if !state.registry.is_token_current(&node_id, &session_token).await {
                                    warn!(node_id = %node_id, "disconnecting session after registry token change");
                                    break Ok(());
                                }
                                let snapshot = sanitize_snapshot(shared.config(), snapshot);
                                let Some(status) = shared.update_snapshot(&node_id, session_id, snapshot).await else {
                                    warn!(node_id = %node_id, session_id, "dropping metrics from superseded session");
                                    break Ok(());
                                };
                                state.history.record_status(&status).await;
                            }
                            WireMessage::Pong(PongMessage { nonce }) => {
                                if !state.registry.is_token_current(&node_id, &session_token).await {
                                    warn!(node_id = %node_id, "disconnecting session after registry token change");
                                    break Ok(());
                                }
                                let Some(sent_at) = outstanding_pings.remove(&nonce) else {
                                    continue;
                                };
                                let latency_ms = sent_at.elapsed().as_millis() as u64;
                                if !shared.update_latency(&node_id, session_id, latency_ms).await {
                                    warn!(node_id = %node_id, session_id, "dropping pong from superseded session");
                                    break Ok(());
                                }
                            }
                            WireMessage::Hello(_) => {
                                break Err(ProtocolError::Client("duplicate hello message".to_string()));
                            }
                            WireMessage::Ping(_) => {
                                break Err(ProtocolError::Client("agent must not send ping messages".to_string()));
                            }
                            WireMessage::ServerNotice(_) => {
                                break Err(ProtocolError::Client("agent must not send server_notice messages".to_string()));
                            }
                        },
                    }
                }
                _ = ping_ticker.tick() => {
                    if !shared.is_current_session(&node_id, session_id).await {
                        warn!(node_id = %node_id, session_id, "closing superseded websocket session");
                        break Ok(());
                    }
                    if !state.registry.is_token_current(&node_id, &session_token).await {
                        warn!(node_id = %node_id, "closing websocket session after registry token change");
                        break Ok(());
                    }

                    prune_outstanding_pings(&mut outstanding_pings, ping_expiry);
                    let nonce = next_ping_nonce;
                    next_ping_nonce = next_ping_nonce.saturating_add(1);
                    outstanding_pings.insert(nonce, Instant::now());
                    let ping = serde_json::to_string(&WireMessage::Ping(PingMessage { nonce }))
                        .map_err(|error| anyhow!("failed to serialize ping: {error}"))?;
                    sender
                        .send(Message::Text(ping.into()))
                        .await
                        .map_err(|error| anyhow!("failed to send ping: {error}"))?;
                }
            }
        }
    }
    .await;

    shared.mark_disconnected(&node_id, session_id).await;
    info!(node_id = %node_id, session_id, "node disconnected");
    session_result
}

async fn recv_hello(socket: &mut WebSocket) -> Result<HelloMessage, ProtocolError> {
    loop {
        let Some(message) = socket
            .recv()
            .await
            .transpose()
            .map_err(|error| anyhow!("failed to receive hello: {error}"))?
        else {
            return Err(ProtocolError::Client(
                "connection closed before hello message".to_string(),
            ));
        };

        match parse_wire_message(message)? {
            ParsedFrame::Control => continue,
            ParsedFrame::Wire(message) => match *message {
                WireMessage::Hello(hello) => return Ok(hello),
                _ => {
                    return Err(ProtocolError::Client(
                        "first websocket message must be hello".to_string(),
                    ));
                }
            },
            ParsedFrame::Close => {
                return Err(ProtocolError::Client(
                    "connection closed before hello message".to_string(),
                ));
            }
        }
    }
}

fn parse_wire_message(message: Message) -> Result<ParsedFrame, ProtocolError> {
    match message {
        Message::Text(text) => serde_json::from_str::<WireMessage>(&text)
            .map(Box::new)
            .map(ParsedFrame::Wire)
            .map_err(|error| ProtocolError::Client(format!("invalid websocket json: {error}"))),
        Message::Binary(_) => Err(ProtocolError::Client(
            "binary websocket messages are not supported".to_string(),
        )),
        Message::Close(_) => Ok(ParsedFrame::Close),
        Message::Ping(_) | Message::Pong(_) => Ok(ParsedFrame::Control),
    }
}

async fn send_wire_message(
    socket: &mut WebSocket,
    message: &WireMessage,
) -> Result<(), ProtocolError> {
    let payload = serde_json::to_string(message)
        .map_err(|error| anyhow!("failed to serialize websocket message: {error}"))?;
    socket
        .send(Message::Text(payload.into()))
        .await
        .map_err(|error| anyhow!("failed to send websocket message: {error}"))?;
    Ok(())
}

fn spawn_stale_reaper(shared: SharedState) {
    tokio::spawn(async move {
        let mut ticker = interval(Duration::from_secs(1));
        loop {
            ticker.tick().await;
            let count = shared.mark_stale().await;
            if count > 0 {
                info!(count, "marked stale nodes offline");
            }
        }
    });
}

fn spawn_registry_reloader(registry: NodeRegistry) {
    tokio::spawn(async move {
        let mut ticker = interval(Duration::from_secs(1));
        loop {
            ticker.tick().await;
            match registry.reload().await {
                Ok(true) => {
                    let enrolled_nodes = registry.count().await;
                    info!(
                        registry_path = %registry.path().display(),
                        enrolled_nodes,
                        "reloaded node registry",
                    );
                }
                Ok(false) => {}
                Err(error) => {
                    warn!(
                        error = ?error,
                        registry_path = %registry.path().display(),
                        "failed to reload node registry; keeping previous in-memory snapshot",
                    );
                }
            }
        }
    });
}

fn bearer_token_from_request(request: &Request) -> Option<&str> {
    request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn spawn_insecure_transport_warning(public_base_url: String, listen: std::net::SocketAddr) {
    if !uses_insecure_remote_public_base_url(&public_base_url, listen) {
        return;
    }

    tokio::spawn(async move {
        let mut ticker = interval(Duration::from_secs(INSECURE_TRANSPORT_WARN_INTERVAL_SECS));
        loop {
            ticker.tick().await;
            warn!(
                listen = %listen,
                public_base_url = %public_base_url,
                "server is configured without TLS; use an https:// public_base_url and terminate TLS in front of XiMonitor",
            );
        }
    });
}

fn uses_insecure_remote_public_base_url(
    public_base_url: &str,
    listen: std::net::SocketAddr,
) -> bool {
    let Ok(url) = Url::parse(public_base_url) else {
        return false;
    };
    if url.scheme() != "http" {
        return false;
    }
    if !listen.ip().is_loopback() {
        return true;
    }

    !host_is_local(url.host_str())
}

fn host_is_local(host: Option<&str>) -> bool {
    let Some(host) = host else {
        return false;
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }

    host.parse::<std::net::IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

async fn restore_snapshot_if_available(shared: &SharedState, path: &Path) {
    if !path.exists() {
        return;
    }

    match load_snapshot(path).await {
        Ok(statuses) => {
            shared.restore_statuses(statuses).await;
        }
        Err(error) => {
            warn!(error = ?error, path = %path.display(), "failed to restore snapshot; continuing with empty state");
        }
    }
}

fn sanitize_snapshot(config: &ServerConfig, mut snapshot: NodeSnapshot) -> NodeSnapshot {
    snapshot.disks.retain(|disk| {
        !config
            .ignored_filesystems
            .iter()
            .any(|fs| fs == &disk.fs_type)
    });
    snapshot
}

fn prune_outstanding_pings(outstanding_pings: &mut HashMap<u64, Instant>, max_age: Duration) {
    outstanding_pings.retain(|_, sent_at| sent_at.elapsed() < max_age);

    if outstanding_pings.len() < MAX_OUTSTANDING_PINGS {
        return;
    }

    if let Some(oldest_nonce) = outstanding_pings
        .iter()
        .min_by_key(|(_, sent_at)| *sent_at)
        .map(|(nonce, _)| *nonce)
    {
        outstanding_pings.remove(&oldest_nonce);
    }
}

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ximonitor_server=info,tower_http=info".into()),
        )
        .with_target(false)
        .compact()
        .init();
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, header};
    use tokio::runtime::Runtime;

    use super::{
        AppState, ReadonlyRouteAuth, bootstrap, healthz, index, install_agent_script,
        install_bootstrap, node_detail, node_history, node_status, nodes, overview,
        uses_insecure_remote_public_base_url, ws_handler,
    };
    use crate::history::HistoryStore;
    use crate::registry::NodeRegistry;
    use crate::state::SharedState;
    use axum::routing::get;
    use tower_http::trace::TraceLayer;
    use ximonitor_proto::ServerConfig;

    #[test]
    fn router_builds_with_v08_path_syntax() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic enough")
            .as_nanos();
        let registry_path =
            std::env::temp_dir().join(format!("ximonitor-router-test-{unique}.json"));
        let config = Arc::new(ServerConfig {
            listen: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 8080)),
            public_base_url: "http://127.0.0.1:8080".to_string(),
            readonly_auth: None,
            node_registry_path: registry_path,
            history_db_path: PathBuf::from("./data/history.sqlite3"),
            snapshot_path: PathBuf::from("./data/snapshot.json"),
            stale_after_secs: 20,
            ping_interval_secs: 10,
            max_message_bytes: 65536,
            refresh_interval_secs: 5,
            ignored_filesystems: vec!["tmpfs".to_string()],
            agent_release_base_url: None,
            agent_release_sha256_x86_64: None,
            agent_release_sha256_aarch64: None,
        });
        let runtime = Runtime::new().expect("runtime should build");
        let state = AppState {
            history: HistoryStore::new(PathBuf::from("./data/history.sqlite3")),
            registry: runtime
                .block_on(NodeRegistry::load(config.node_registry_path.as_path()))
                .expect("registry should load"),
            shared: SharedState::new(config),
        };

        let _app: Router = Router::new()
            .route("/", get(index))
            .route("/nodes/{node_id}", get(node_detail))
            .route("/healthz", get(healthz))
            .route("/install/install-agent.sh", get(install_agent_script))
            .route("/install/bootstrap", get(install_bootstrap))
            .route("/api/bootstrap", get(bootstrap))
            .route("/api/overview", get(overview))
            .route("/api/nodes", get(nodes))
            .route("/api/nodes/{node_id}", get(node_status))
            .route("/api/nodes/{node_id}/history", get(node_history))
            .route("/ws", get(ws_handler))
            .with_state(state)
            .layer(TraceLayer::new_for_http());
    }

    #[test]
    fn readonly_route_auth_matches_basic_header() {
        let auth = ReadonlyRouteAuth::from_config(Some(ximonitor_proto::ReadonlyAuthConfig {
            username: "viewer".to_string(),
            password: "secret".to_string(),
        }));
        let request = Request::builder()
            .uri("/api/overview")
            .header(header::AUTHORIZATION, "Basic dmlld2VyOnNlY3JldA==")
            .body(Body::empty())
            .expect("request should build");

        assert!(auth.is_authorized(&request));
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
}
