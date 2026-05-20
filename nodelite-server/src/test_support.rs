use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use axum::Router;
use axum::middleware::{from_fn, from_fn_with_state};
use axum::routing::get;
use chrono::{DateTime, Utc};
use futures::{SinkExt, StreamExt};
use getrandom::fill as fill_random;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tower_http::trace::TraceLayer;

use crate::handlers::{metrics, node_history, node_status, nodes, overview, require_readonly_auth};
use crate::registry::{IssueNodeRequest, NodeRegistry, issue_node};
use crate::set_protected_response_headers;
use crate::state::{SessionRefreshReply, SharedState};
use crate::ws::ws_handler;
use nodelite_proto::{
    DiskUsage, HelloMessage, HistoryPoint, LoadAverage, MemoryUsage, NetworkCounters, NodeIdentity,
    NodeSnapshot, NodeStatus, NoticeLevel, OverviewData, ReadonlyAuthConfig,
    RefreshTokenResponseMessage, ServerConfig, WireMessage, WsConfig,
};

pub const TEST_BASIC_AUTH_HEADER: &str = "Basic dmlld2VyOnNlY3JldA==";
pub const TEST_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) type TestSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;

pub(crate) fn test_ws_config(
    max_total_connections: usize,
    max_connections_per_ip: usize,
) -> WsConfig {
    WsConfig {
        max_total_connections,
        max_connections_per_ip,
        auth_fail_window_secs: 300,
        auth_fail_max_attempts: 12,
        auth_block_secs: 900,
    }
}

pub(crate) fn test_server_config(
    listen: SocketAddr,
    public_base_url: String,
    registry_path: PathBuf,
    history_path: PathBuf,
    snapshot_path: PathBuf,
) -> ServerConfig {
    ServerConfig {
        listen,
        public_base_url,
        insecure_allow_http: false,
        readonly_auth: Some(ReadonlyAuthConfig {
            username: "viewer".to_string(),
            password: "secret".to_string(),
            enable_2fa: false,
            totp_secret: None,
        }),
        ws: test_ws_config(128, 128),
        node_registry_path: registry_path,
        history_db_path: history_path,
        snapshot_path,
        stale_after_secs: 5,
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
    }
}

pub(crate) fn synthetic_identity(
    node_id: &str,
    node_label: &str,
    agent_version: &str,
    kernel_version: Option<&str>,
    tag: &str,
) -> NodeIdentity {
    NodeIdentity {
        node_id: node_id.to_string(),
        node_label: node_label.to_string(),
        hostname: format!("{node_id}.example.internal"),
        os: "Linux".to_string(),
        kernel_version: kernel_version.map(str::to_string),
        cpu_model: Some("Rust Hypervisor".to_string()),
        cpu_cores: 4,
        agent_version: agent_version.to_string(),
        boot_time: Some(Utc::now()),
        tags: vec![tag.to_string()],
    }
}

#[derive(Debug, Clone)]
pub struct TestNode {
    pub node_id: String,
    pub node_label: String,
    pub token: String,
}

pub struct TestServer {
    addr: SocketAddr,
    registry: NodeRegistry,
    registry_path: PathBuf,
    shared: SharedState,
    shutdown: tokio_util::sync::CancellationToken,
    server_handle: JoinHandle<Result<(), std::io::Error>>,
    temp_dir: PathBuf,
}

impl TestServer {
    pub async fn start() -> Result<Self> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock moved backwards")?
            .as_nanos();
        let mut random_suffix = [0_u8; 8];
        fill_random(&mut random_suffix).context("generate random test suffix")?;
        let temp_dir = std::env::temp_dir().join(format!(
            "nodelite-integration-test-{}-{timestamp:016x}-{:016x}",
            std::process::id(),
            u64::from_be_bytes(random_suffix),
        ));
        tokio::fs::create_dir_all(&temp_dir)
            .await
            .with_context(|| format!("create temp dir {}", temp_dir.display()))?;

        let listener =
            TcpListener::bind(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))).await?;
        let addr = listener.local_addr()?;
        let registry_path = temp_dir.join("server.json");
        let history_path = temp_dir.join("history.sqlite3");
        let snapshot_path = temp_dir.join("snapshot.json");
        let config = Arc::new(test_server_config(
            addr,
            format!("http://{addr}"),
            registry_path.clone(),
            history_path,
            snapshot_path,
        ));
        let state =
            crate::AppState::test_fixture(config, Arc::new(temp_dir.join("server.toml"))).await?;
        let registry = state.registry.clone();
        let shared = state.shared.clone();
        let shutdown = state.shutdown.clone();
        let protected_routes = Router::new()
            .route("/api/overview", get(overview))
            .route("/metrics", get(metrics))
            .route("/api/nodes", get(nodes))
            .route("/api/nodes/{node_id}", get(node_status))
            .route("/api/nodes/{node_id}/history", get(node_history))
            .route_layer(from_fn(set_protected_response_headers))
            .route_layer(from_fn_with_state(state.clone(), require_readonly_auth));
        let app = Router::new()
            .route("/ws", get(ws_handler))
            .merge(protected_routes)
            .with_state(state)
            .layer(TraceLayer::new_for_http());

        let shutdown_for_axum = shutdown.clone();
        let server_handle = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(async move {
                // 测试通过 cancel CancellationToken 触发 graceful shutdown:
                // 这也是 production code 在 run_server 中走的同一条路径。
                shutdown_for_axum.cancelled().await;
            })
            .await
        });

        Ok(Self {
            addr,
            registry,
            registry_path,
            shared,
            shutdown,
            server_handle,
            temp_dir,
        })
    }

    pub async fn issue_node(&self, node_id: &str, node_label: &str) -> Result<TestNode> {
        let issued = issue_node(
            &self.registry_path,
            IssueNodeRequest {
                node_id: node_id.to_string(),
                node_label: Some(node_label.to_string()),
                tags: vec!["integration-test".to_string()],
            },
        )
        .await
        .with_context(|| format!("issue node {node_id}"))?;
        let _ = self.registry.reload().await?;

        Ok(TestNode {
            node_id: issued.node.node_id,
            node_label: issued.node.node_label,
            token: issued.node_session_token,
        })
    }

    pub async fn overview(&self) -> Result<OverviewData> {
        self.fetch_json("/api/overview").await
    }

    pub async fn nodes(&self) -> Result<Vec<NodeStatus>> {
        self.fetch_json("/api/nodes").await
    }

    pub async fn metrics_text(&self) -> Result<String> {
        fetch_http_body(self.addr, "/metrics").await
    }

    pub async fn node_status(&self, node_id: &str) -> Result<NodeStatus> {
        self.fetch_json(&format!("/api/nodes/{node_id}")).await
    }

    pub async fn node_history(
        &self,
        node_id: &str,
        max_points: usize,
    ) -> Result<Vec<HistoryPoint>> {
        self.fetch_json(&format!(
            "/api/nodes/{node_id}/history?window_hours=24&max_points={max_points}"
        ))
        .await
    }

    pub async fn wait_for_node_uptime(
        &self,
        node_id: &str,
        expected_uptime: u64,
        timeout_duration: Duration,
    ) -> Result<NodeStatus> {
        self.wait_for_status(
            timeout_duration,
            |status| {
                status.online
                    && status
                        .snapshot
                        .as_ref()
                        .is_some_and(|snapshot| snapshot.uptime_secs == expected_uptime)
            },
            node_id,
        )
        .await
    }

    pub async fn wait_for_node_offline(
        &self,
        node_id: &str,
        timeout_duration: Duration,
    ) -> Result<NodeStatus> {
        self.wait_for_status(timeout_duration, |status| !status.online, node_id)
            .await
    }

    pub async fn wait_for_history_points(
        &self,
        node_id: &str,
        min_points: usize,
        timeout_duration: Duration,
    ) -> Result<Vec<HistoryPoint>> {
        let started = std::time::Instant::now();
        loop {
            let points = self.node_history(node_id, 480).await?;
            if points.len() >= min_points {
                return Ok(points);
            }
            if started.elapsed() > timeout_duration {
                bail!(
                    "timed out waiting for {min_points} history points for {node_id}; only saw {}",
                    points.len()
                );
            }
            sleep(Duration::from_millis(25)).await;
        }
    }

    pub async fn request_live_token_refresh(&self, node_id: &str) -> Result<DateTime<Utc>> {
        let response_rx: tokio::sync::oneshot::Receiver<Result<SessionRefreshReply, String>> = self
            .shared
            .request_live_token_refresh(node_id)
            .await
            .map_err(|error| anyhow!("request live refresh for {node_id}: {error}"))?;
        let refresh_result = timeout(TEST_TIMEOUT, response_rx)
            .await
            .context("timed out waiting for live refresh response")?
            .map_err(|_| anyhow!("live refresh channel closed"))?
            .map_err(|message| anyhow!(message))?;
        Ok(refresh_result.token_expires_at)
    }

    pub async fn is_token_current(&self, node_id: &str, token: &str) -> bool {
        let identity = NodeIdentity {
            node_id: node_id.to_string(),
            node_label: node_id.to_string(),
            hostname: format!("{node_id}.test"),
            os: "test".to_string(),
            kernel_version: None,
            cpu_model: None,
            cpu_cores: 1,
            agent_version: "0.1.0-test".to_string(),
            boot_time: None,
            tags: Vec::new(),
        };
        let Ok(authorized) = self.registry.authorize(&identity, token).await else {
            return false;
        };
        self.registry
            .is_token_current(node_id, authorized.generation)
            .await
    }

    pub async fn shutdown(self) -> Result<()> {
        self.shutdown.cancel();
        let result = self
            .server_handle
            .await
            .map_err(|error| anyhow!("join server task: {error}"))?;
        result.map_err(|error| anyhow!("server task: {error}"))?;
        let _ = tokio::fs::remove_dir_all(&self.temp_dir).await;
        Ok(())
    }

    /// 触发关停信号但不等待 server 退出,留给测试自行 await `server_handle`
    /// 之外的步骤(例如观察 agent 是否收到 Close 帧)。
    pub fn cancel_shutdown(&self) {
        self.shutdown.cancel();
    }

    async fn fetch_json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let body = fetch_http_body(self.addr, path).await?;
        serde_json::from_str(&body).with_context(|| format!("decode json body for {path}"))
    }

    async fn wait_for_status<F>(
        &self,
        timeout_duration: Duration,
        predicate: F,
        node_id: &str,
    ) -> Result<NodeStatus>
    where
        F: Fn(&NodeStatus) -> bool,
    {
        let started = std::time::Instant::now();
        loop {
            if let Some(status) = self.shared.get_status(node_id).await
                && predicate(&status)
            {
                return Ok(status);
            }
            if started.elapsed() > timeout_duration {
                bail!("timed out waiting for node status for {node_id}");
            }
            sleep(Duration::from_millis(20)).await;
        }
    }
}

pub struct TestAgent {
    socket: TestSocket,
}

impl TestAgent {
    pub async fn connect(server: &TestServer, node: &TestNode) -> Result<Self> {
        let url = format!("ws://{}/ws", server.addr);
        let (mut socket, _response) = connect_async(url)
            .await
            .with_context(|| format!("connect fake agent {}", node.node_id))?;

        let hello = WireMessage::Hello(HelloMessage {
            protocol_version: nodelite_proto::WIRE_PROTOCOL_VERSION,
            token: node.token.clone(),
            identity: fake_identity(node),
        });
        send_wire_message(&mut socket, &hello).await?;
        wait_for_authenticated_notice(&mut socket, &node.node_id, TEST_TIMEOUT).await?;
        Ok(Self { socket })
    }

    pub async fn send_fake_metrics(&mut self, uptime_secs: u64) -> Result<()> {
        self.send_snapshot(fake_snapshot(uptime_secs)).await
    }

    pub async fn send_snapshot(&mut self, snapshot: NodeSnapshot) -> Result<()> {
        send_wire_message(
            &mut self.socket,
            &WireMessage::Metrics(nodelite_proto::MetricsMessage { snapshot }),
        )
        .await
    }

    pub async fn wait_for_refresh_response(
        &mut self,
        timeout_duration: Duration,
    ) -> Result<RefreshTokenResponseMessage> {
        let message = self.next_business_message(timeout_duration).await?;
        match message {
            WireMessage::RefreshTokenResponse(response) => Ok(response),
            other => bail!("expected refresh token response, got {other:?}"),
        }
    }

    pub async fn disconnect(mut self) -> Result<()> {
        self.socket
            .close(None)
            .await
            .context("close fake agent socket")
    }

    /// 等服务端主动断开,返回 close code 与 reason 文本。
    /// 用于覆盖优雅停机:服务端应当在收到 SIGTERM/cancel 后主动发 Close(1001)
    /// 而不是直接 drop TCP 连接。
    pub async fn wait_for_close_frame(
        &mut self,
        timeout_duration: Duration,
    ) -> Result<(u16, String)> {
        timeout(timeout_duration, async {
            loop {
                let Some(frame) = self.socket.next().await else {
                    bail!("socket closed without a Close frame");
                };
                match frame.context("receive websocket frame")? {
                    Message::Close(Some(close_frame)) => {
                        return Ok((u16::from(close_frame.code), close_frame.reason.to_string()));
                    }
                    Message::Close(None) => {
                        bail!("server closed the socket without a CloseFrame payload");
                    }
                    Message::Ping(payload) => {
                        self.socket
                            .send(Message::Pong(payload))
                            .await
                            .context("reply websocket ping while awaiting close")?;
                    }
                    // 服务端在 close 之前可能正在跑别的循环(比如下发刚收到的 ping),
                    // 这些都吞掉,我们只关心 Close。
                    _ => {}
                }
            }
        })
        .await
        .context("timed out waiting for close frame")?
    }

    async fn next_business_message(&mut self, timeout_duration: Duration) -> Result<WireMessage> {
        timeout(timeout_duration, async {
            loop {
                let Some(frame) = self.socket.next().await else {
                    bail!("socket closed before business message");
                };
                match frame.context("receive websocket frame")? {
                    Message::Text(text) => {
                        let message: WireMessage =
                            serde_json::from_str(&text).context("decode wire message")?;
                        match message {
                            WireMessage::Ping(ping) => {
                                send_wire_message(
                                    &mut self.socket,
                                    &WireMessage::Pong(nodelite_proto::PongMessage {
                                        nonce: ping.nonce,
                                    }),
                                )
                                .await?;
                            }
                            other => return Ok(other),
                        }
                    }
                    Message::Ping(payload) => {
                        self.socket
                            .send(Message::Pong(payload))
                            .await
                            .context("reply websocket ping")?;
                    }
                    Message::Close(frame) => {
                        bail!("socket closed before business message: {frame:?}");
                    }
                    _ => {}
                }
            }
        })
        .await
        .context("timed out waiting for business message")?
    }
}

async fn fetch_http_body(addr: SocketAddr, path: &str) -> Result<String> {
    let mut stream = TcpStream::connect(addr)
        .await
        .with_context(|| format!("connect test http client to {addr}"))?;
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: {TEST_BASIC_AUTH_HEADER}\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .with_context(|| format!("write test http request for {path}"))?;

    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .with_context(|| format!("read test http response for {path}"))?;

    let response_text = String::from_utf8_lossy(&response);
    if !response_text.starts_with("HTTP/1.1 200") && !response_text.starts_with("HTTP/1.0 200") {
        bail!("unexpected http response for {path}: {response_text}");
    }

    let Some((_, body)) = response_text.split_once("\r\n\r\n") else {
        bail!("missing http body separator for {path}");
    };
    Ok(body.to_string())
}

fn fake_identity(node: &TestNode) -> NodeIdentity {
    synthetic_identity(
        &node.node_id,
        &node.node_label,
        "integration-test",
        Some("6.8.0-integration-test"),
        "integration-test",
    )
}

pub fn fake_snapshot(uptime_secs: u64) -> NodeSnapshot {
    fake_snapshot_at(uptime_secs, Utc::now())
}

pub(crate) fn fake_snapshot_at(uptime_secs: u64, collected_at: DateTime<Utc>) -> NodeSnapshot {
    NodeSnapshot {
        collected_at,
        cpu_usage_percent: 12.5 + (uptime_secs % 7) as f64,
        load: LoadAverage {
            one: 0.3,
            five: 0.4,
            fifteen: 0.5,
        },
        memory: MemoryUsage {
            total_bytes: 4 * 1024 * 1024 * 1024,
            used_bytes: 1536 * 1024 * 1024,
            available_bytes: 2560 * 1024 * 1024,
            swap_total_bytes: 1024 * 1024 * 1024,
            swap_used_bytes: 64 * 1024 * 1024,
        },
        uptime_secs,
        disks: vec![DiskUsage {
            device: "/dev/vda".to_string(),
            mount_point: "/".to_string(),
            fs_type: "ext4".to_string(),
            total_bytes: 80 * 1024 * 1024 * 1024,
            available_bytes: 40 * 1024 * 1024 * 1024,
            used_bytes: 40 * 1024 * 1024 * 1024,
            used_percent: 50.0,
        }],
        network: NetworkCounters {
            total_rx_bytes: 512 * 1024 * uptime_secs,
            total_tx_bytes: 256 * 1024 * uptime_secs,
            rx_bytes_per_sec: Some(32_768.0 + uptime_secs as f64),
            tx_bytes_per_sec: Some(16_384.0 + uptime_secs as f64),
        },
    }
}

async fn send_wire_message(socket: &mut TestSocket, message: &WireMessage) -> Result<()> {
    let payload = serde_json::to_string(message).context("serialize wire message")?;
    socket
        .send(Message::Text(payload.into()))
        .await
        .context("send websocket message")
}

pub(crate) async fn wait_for_authenticated_notice(
    socket: &mut TestSocket,
    node_id: &str,
    timeout_duration: Duration,
) -> Result<()> {
    timeout(timeout_duration, async {
        loop {
            let Some(frame) = socket.next().await else {
                bail!("socket closed before authenticated notice");
            };
            match frame.context("receive websocket frame")? {
                Message::Text(text) => {
                    let message: WireMessage =
                        serde_json::from_str(&text).context("decode wire message")?;
                    match message {
                        WireMessage::ServerNotice(notice) if notice.message == "authenticated" => {
                            return Ok(());
                        }
                        WireMessage::Ping(ping) => {
                            send_wire_message(
                                socket,
                                &WireMessage::Pong(nodelite_proto::PongMessage {
                                    nonce: ping.nonce,
                                }),
                            )
                            .await?;
                        }
                        WireMessage::ServerNotice(notice) if notice.level == NoticeLevel::Error => {
                            bail!("server rejected {node_id}: {}", notice.message);
                        }
                        _ => {}
                    }
                }
                Message::Ping(payload) => {
                    socket
                        .send(Message::Pong(payload))
                        .await
                        .context("reply websocket ping")?;
                }
                Message::Close(frame) => {
                    bail!("socket closed before auth: {frame:?}");
                }
                _ => {}
            }
        }
    })
    .await
    .context("timed out waiting for authenticated notice")?
}
