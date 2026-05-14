//! 手动压测入口。
//!
//! 这是一个 `#[ignore]` 的真实链路压测:
//! - 启一个临时 server 实例(真实 `/ws` + `/api/overview`)
//! - 模拟 N 个 agent 连接并发送一轮 metrics burst
//! - 在 burst 期间并发探测 overview API 延迟
//!
//! 运行方式:
//! `cargo test -p ximonitor-server load_test_scaling_scores -- --ignored --nocapture`

use std::collections::{HashMap, HashSet};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use axum::Router;
use axum::middleware::from_fn_with_state;
use axum::routing::get;
use chrono::Utc;
use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;
use tokio::sync::{Barrier, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tower_http::trace::TraceLayer;
use ximonitor_proto::{
    DiskUsage, HelloMessage, LoadAverage, MemoryUsage, MetricsMessage, NetworkCounters,
    NodeIdentity, NodeSnapshot, ReadonlyAuthConfig, ServerConfig, WireMessage, WsConfig,
};

use crate::AppState;
use crate::ServerReadiness;
use crate::admission::{InstallAdmissionConfig, InstallAdmissionController, WsAdmissionController};
use crate::auth::{ReadonlyRouteAuth, TwoFactorSessions};
use crate::handlers::{overview, require_readonly_auth};
use crate::history::HistoryStore;
use crate::registry::{IssueNodeRequest, NodeRegistry, issue_node};
use crate::state::SharedState;
use crate::ws::ws_handler;

const LOAD_TEST_TIMEOUT_SECS: u64 = 30;
const LOAD_TEST_METRICS_PER_NODE: u64 = 12;
const LOAD_TEST_OVERVIEW_PROBES: usize = 24;
const LOAD_TEST_BASIC_AUTH: &str = "Basic dmlld2VyOnNlY3JldA==";

#[derive(Debug, Clone)]
struct AgentCredential {
    node_id: String,
    node_label: String,
    token: String,
}

#[derive(Debug)]
struct ScenarioResult {
    nodes: usize,
    metrics_total: usize,
    connect_ms: f64,
    settle_ms: f64,
    metrics_per_sec: f64,
    overview_p50_ms: f64,
    overview_p95_ms: f64,
    overview_max_ms: f64,
}

struct TestServer {
    addr: SocketAddr,
    shared: SharedState,
    shutdown_tx: Option<oneshot::Sender<()>>,
    server_handle: JoinHandle<Result<(), std::io::Error>>,
    temp_dir: PathBuf,
}

impl TestServer {
    async fn start(node_count: usize) -> Result<(Self, Vec<AgentCredential>)> {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should move forward")
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!("ximonitor-load-test-{unique}"));
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
                token: issued.node.token,
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
        });

        let history = HistoryStore::new(history_path);
        history.initialize().await;
        let readiness = ServerReadiness::new(history.is_available());
        let state = AppState {
            history,
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
        };

        let shared = state.shared.clone();
        let protected_routes = Router::new()
            .route("/api/overview", get(overview))
            .route_layer(from_fn_with_state(state.clone(), require_readonly_auth));
        let app = Router::new()
            .route("/ws", get(ws_handler))
            .merge(protected_routes)
            .with_state(state)
            .layer(TraceLayer::new_for_http());

        let (shutdown_tx, shutdown_rx) = oneshot::channel();
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
                shutdown_tx: Some(shutdown_tx),
                server_handle,
                temp_dir,
            },
            credentials,
        ))
    }

    async fn shutdown(mut self) -> Result<()> {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "manual load test; run with -- --ignored --nocapture"]
async fn load_test_scaling_scores() {
    if let Err(error) = run_scaling_load_test().await {
        panic!("{error:#}");
    }
}

async fn run_scaling_load_test() -> Result<()> {
    let scenarios = [20_usize, 50, 100, 200];
    println!(
        "LOAD_TEST starting scenarios={:?} metrics_per_node={} overview_probes={}",
        scenarios, LOAD_TEST_METRICS_PER_NODE, LOAD_TEST_OVERVIEW_PROBES
    );
    for &node_count in &scenarios {
        let result = run_single_scenario(node_count).await?;
        println!(
            "LOAD_RESULT nodes={} connect_ms={:.1} settle_ms={:.1} metrics_total={} metrics_per_sec={:.1} overview_p50_ms={:.2} overview_p95_ms={:.2} overview_max_ms={:.2}",
            result.nodes,
            result.connect_ms,
            result.settle_ms,
            result.metrics_total,
            result.metrics_per_sec,
            result.overview_p50_ms,
            result.overview_p95_ms,
            result.overview_max_ms,
        );
    }
    Ok(())
}

async fn run_single_scenario(node_count: usize) -> Result<ScenarioResult> {
    let (server, credentials) = TestServer::start(node_count).await?;
    let (ready_tx, mut ready_rx) = mpsc::unbounded_channel::<String>();
    let (stop_tx, stop_rx) = watch::channel(false);
    let burst_barrier = Arc::new(Barrier::new(node_count + 1));
    let mut handles = Vec::with_capacity(node_count);
    let expected_final_uptime = LOAD_TEST_METRICS_PER_NODE;
    let connect_started = Instant::now();

    for credential in credentials.clone() {
        handles.push(tokio::spawn(run_fake_agent(
            server.addr,
            credential,
            LOAD_TEST_METRICS_PER_NODE,
            ready_tx.clone(),
            burst_barrier.clone(),
            stop_rx.clone(),
        )));
    }
    drop(ready_tx);

    let mut ready_nodes = HashSet::with_capacity(node_count);
    while ready_nodes.len() < node_count {
        let next = timeout(Duration::from_secs(LOAD_TEST_TIMEOUT_SECS), ready_rx.recv())
            .await
            .context("timed out waiting for fake agents to authenticate")?;
        let Some(node_id) = next else {
            bail!(
                "fake agent ready channel closed early after {} / {} nodes",
                ready_nodes.len(),
                node_count
            );
        };
        ready_nodes.insert(node_id);
    }
    let connect_elapsed = connect_started.elapsed();

    let probe_task = tokio::spawn(probe_overview_latencies(
        server.addr,
        LOAD_TEST_OVERVIEW_PROBES,
    ));
    let settle_started = Instant::now();
    burst_barrier.wait().await;
    wait_for_final_snapshots(
        server.shared.clone(),
        &credentials,
        expected_final_uptime,
        Duration::from_secs(LOAD_TEST_TIMEOUT_SECS),
    )
    .await?;
    let settle_elapsed = settle_started.elapsed();

    let _ = stop_tx.send(true);
    for handle in handles {
        handle
            .await
            .map_err(|error| anyhow!("join fake agent task: {error}"))??;
    }
    let latencies = probe_task
        .await
        .map_err(|error| anyhow!("join overview probe task: {error}"))??;
    server.shutdown().await?;

    let metrics_total = node_count * LOAD_TEST_METRICS_PER_NODE as usize;
    let settle_secs = settle_elapsed.as_secs_f64().max(0.001);
    let (p50, p95, max) = summarize_latencies(&latencies)?;

    Ok(ScenarioResult {
        nodes: node_count,
        metrics_total,
        connect_ms: connect_elapsed.as_secs_f64() * 1000.0,
        settle_ms: settle_elapsed.as_secs_f64() * 1000.0,
        metrics_per_sec: metrics_total as f64 / settle_secs,
        overview_p50_ms: p50,
        overview_p95_ms: p95,
        overview_max_ms: max,
    })
}

async fn run_fake_agent(
    addr: SocketAddr,
    credential: AgentCredential,
    metrics_per_node: u64,
    ready_tx: mpsc::UnboundedSender<String>,
    burst_barrier: Arc<Barrier>,
    mut stop_rx: watch::Receiver<bool>,
) -> Result<()> {
    let url = format!("ws://{addr}/ws");
    let (mut socket, _response) = connect_async(url)
        .await
        .with_context(|| format!("connect fake agent {}", credential.node_id))?;

    let hello = WireMessage::Hello(HelloMessage {
        token: credential.token.clone(),
        identity: fake_identity(&credential),
    });
    send_wire_message(&mut socket, &hello).await?;
    wait_for_authenticated_notice(&mut socket, &credential.node_id).await?;
    ready_tx
        .send(credential.node_id.clone())
        .map_err(|_| anyhow!("ready channel closed"))?;

    burst_barrier.wait().await;
    for uptime_secs in 1..=metrics_per_node {
        let metrics = WireMessage::Metrics(MetricsMessage {
            snapshot: fake_snapshot(uptime_secs),
        });
        send_wire_message(&mut socket, &metrics).await?;
    }

    let _ = stop_rx.changed().await;
    let _ = socket.close(None).await;
    Ok(())
}

async fn wait_for_authenticated_notice(
    socket: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>,
    node_id: &str,
) -> Result<()> {
    timeout(Duration::from_secs(LOAD_TEST_TIMEOUT_SECS), async {
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
                                &WireMessage::Pong(ximonitor_proto::PongMessage {
                                    nonce: ping.nonce,
                                }),
                            )
                            .await?;
                        }
                        WireMessage::ServerNotice(notice)
                            if notice.level == ximonitor_proto::NoticeLevel::Error =>
                        {
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

async fn send_wire_message(
    socket: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>,
    message: &WireMessage,
) -> Result<()> {
    let payload = serde_json::to_string(message).context("serialize wire message")?;
    socket
        .send(Message::Text(payload.into()))
        .await
        .context("send websocket message")
}

fn fake_identity(credential: &AgentCredential) -> NodeIdentity {
    NodeIdentity {
        node_id: credential.node_id.clone(),
        node_label: credential.node_label.clone(),
        hostname: format!("{}.example.internal", credential.node_id),
        os: "Linux".to_string(),
        kernel_version: Some("6.8.0-load-test".to_string()),
        cpu_model: Some("Rust Hypervisor".to_string()),
        cpu_cores: 4,
        agent_version: "load-test".to_string(),
        boot_time: Some(Utc::now()),
        tags: vec!["load-test".to_string()],
    }
}

fn fake_snapshot(uptime_secs: u64) -> NodeSnapshot {
    NodeSnapshot {
        collected_at: Utc::now(),
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

async fn wait_for_final_snapshots(
    shared: SharedState,
    credentials: &[AgentCredential],
    expected_uptime: u64,
    timeout_duration: Duration,
) -> Result<()> {
    let started = Instant::now();
    let expected_nodes: HashSet<_> = credentials
        .iter()
        .map(|item| item.node_id.as_str())
        .collect();

    loop {
        let statuses = shared.list_statuses().await;
        let by_id: HashMap<_, _> = statuses
            .iter()
            .map(|status| (status.identity.node_id.as_str(), status))
            .collect();
        let all_ready = expected_nodes.iter().all(|node_id| {
            by_id.get(node_id).is_some_and(|status| {
                status.online
                    && status
                        .snapshot
                        .as_ref()
                        .is_some_and(|snapshot| snapshot.uptime_secs == expected_uptime)
            })
        });
        if all_ready {
            return Ok(());
        }
        if started.elapsed() > timeout_duration {
            let mut unfinished = Vec::new();
            for node_id in &expected_nodes {
                match by_id.get(node_id) {
                    Some(status) => unfinished.push(format!(
                        "{} online={} uptime={:?}",
                        node_id,
                        status.online,
                        status
                            .snapshot
                            .as_ref()
                            .map(|snapshot| snapshot.uptime_secs)
                    )),
                    None => unfinished.push(format!("{node_id} missing")),
                }
            }
            bail!(
                "timed out waiting for final snapshots: {}",
                unfinished.join(", ")
            );
        }
        sleep(Duration::from_millis(20)).await;
    }
}

async fn probe_overview_latencies(addr: SocketAddr, samples: usize) -> Result<Vec<Duration>> {
    let mut latencies = Vec::with_capacity(samples);
    for _ in 0..samples {
        latencies.push(fetch_overview_latency(addr).await?);
        sleep(Duration::from_millis(25)).await;
    }
    Ok(latencies)
}

async fn fetch_overview_latency(addr: SocketAddr) -> Result<Duration> {
    let started = Instant::now();
    let mut stream = TcpStream::connect(addr)
        .await
        .with_context(|| format!("connect overview probe to {addr}"))?;
    let request = format!(
        "GET /api/overview HTTP/1.1\r\nHost: {addr}\r\nAuthorization: {LOAD_TEST_BASIC_AUTH}\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .context("write overview request")?;

    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .context("read overview response")?;

    let response_text = String::from_utf8_lossy(&response);
    if !response_text.starts_with("HTTP/1.1 200") && !response_text.starts_with("HTTP/1.0 200") {
        bail!("unexpected overview response: {response_text}");
    }

    Ok(started.elapsed())
}

fn summarize_latencies(latencies: &[Duration]) -> Result<(f64, f64, f64)> {
    if latencies.is_empty() {
        bail!("no overview latencies captured");
    }
    let mut values: Vec<f64> = latencies
        .iter()
        .map(|duration| duration.as_secs_f64() * 1000.0)
        .collect();
    values.sort_by(|left, right| left.total_cmp(right));

    let percentile = |p: f64| -> f64 {
        let index = ((values.len() - 1) as f64 * p).round() as usize;
        values[index]
    };

    Ok((
        percentile(0.50),
        percentile(0.95),
        *values.last().unwrap_or(&0.0),
    ))
}
