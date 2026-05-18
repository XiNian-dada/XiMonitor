//! 手动压测入口。
//!
//! 这是一个 `#[ignore]` 的真实链路压测:
//! - 启一个临时 server 实例(真实 `/ws` + `/api/*`)
//! - 模拟 N 个 agent 连接并发送 burst / steady-state metrics
//! - 在上报期间并发探测 overview / nodes / node / history 等读取路径
//!
//! 运行方式:
//! `cargo test -p nodelite-server load_test_scaling_scores -- --ignored --nocapture`
//! `cargo test -p nodelite-server load_test_api_surface_scores -- --ignored --nocapture`
//! `cargo test -p nodelite-server load_test_reconnect_storm_scores -- --ignored --nocapture`

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
use nodelite_proto::{
    DiskUsage, HelloMessage, LoadAverage, MemoryUsage, MetricsMessage, NetworkCounters,
    NodeIdentity, NodeSnapshot, NodeStatus, ReadonlyAuthConfig, ServerConfig, WireMessage,
    WsConfig,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;
use tokio::sync::{Barrier, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tower_http::trace::TraceLayer;

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

const LOAD_TEST_TIMEOUT_SECS: u64 = 30;
const LOAD_TEST_METRICS_PER_NODE: u64 = 12;
const LOAD_TEST_OVERVIEW_PROBES: usize = 24;
const LOAD_TEST_READ_PROBES: usize = 20;
const LOAD_TEST_HISTORY_POINTS: usize = 360;
const LOAD_TEST_STEADY_METRICS_PER_NODE: u64 = 18;
const LOAD_TEST_STEADY_METRIC_DELAY_MS: u64 = 15;
const LOAD_TEST_STORM_CYCLES: usize = 4;
const LOAD_TEST_STORM_METRICS_PER_CYCLE: u64 = 6;
const LOAD_TEST_STORM_METRIC_DELAY_MS: u64 = 10;
const LOAD_TEST_STORM_READ_PROBES: usize = 12;
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

#[derive(Debug)]
struct ApiScenarioResult {
    nodes: usize,
    steady_metrics_total: usize,
    connect_ms: f64,
    settle_ms: f64,
    steady_metrics_per_sec: f64,
    history_seed_points: usize,
    overview: LatencySummary,
    nodes_api: LatencySummary,
    node_api: LatencySummary,
    history_api: LatencySummary,
}

#[derive(Debug)]
struct StormScenarioResult {
    nodes: usize,
    cycles: usize,
    sessions_total: usize,
    connect: LatencySummary,
    recover: LatencySummary,
    disconnect: LatencySummary,
    overview: LatencySummary,
    nodes_api: LatencySummary,
}

#[derive(Debug, Clone, Copy)]
struct LatencySummary {
    p50_ms: f64,
    p95_ms: f64,
    max_ms: f64,
}

#[derive(Debug, Clone, Copy)]
struct AgentWorkload {
    uptime_start: u64,
    metrics_per_node: u64,
    inter_message_delay: Duration,
    hold_after_send: Duration,
}

type TestSocket = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;

struct TestServer {
    addr: SocketAddr,
    shared: SharedState,
    history: HistoryStore,
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
        });

        let history = HistoryStore::new(history_path);
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
                history,
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

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "manual load test; run with -- --ignored --nocapture"]
async fn load_test_api_surface_scores() {
    if let Err(error) = run_api_surface_load_test().await {
        panic!("{error:#}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "manual load test; run with -- --ignored --nocapture"]
async fn load_test_reconnect_storm_scores() {
    if let Err(error) = run_reconnect_storm_load_test().await {
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

async fn run_api_surface_load_test() -> Result<()> {
    let scenarios = [20_usize, 50, 100, 200];
    println!(
        "API_LOAD_TEST starting scenarios={:?} steady_metrics_per_node={} read_probes={} history_seed_points={}",
        scenarios,
        LOAD_TEST_STEADY_METRICS_PER_NODE,
        LOAD_TEST_READ_PROBES,
        LOAD_TEST_HISTORY_POINTS,
    );
    for &node_count in &scenarios {
        let result = run_api_surface_scenario(node_count).await?;
        println!(
            "API_RESULT nodes={} connect_ms={:.1} settle_ms={:.1} steady_metrics_total={} steady_metrics_per_sec={:.1} history_seed_points={} overview_p50_ms={:.2} overview_p95_ms={:.2} overview_max_ms={:.2} nodes_p50_ms={:.2} nodes_p95_ms={:.2} nodes_max_ms={:.2} node_p50_ms={:.2} node_p95_ms={:.2} node_max_ms={:.2} history_p50_ms={:.2} history_p95_ms={:.2} history_max_ms={:.2}",
            result.nodes,
            result.connect_ms,
            result.settle_ms,
            result.steady_metrics_total,
            result.steady_metrics_per_sec,
            result.history_seed_points,
            result.overview.p50_ms,
            result.overview.p95_ms,
            result.overview.max_ms,
            result.nodes_api.p50_ms,
            result.nodes_api.p95_ms,
            result.nodes_api.max_ms,
            result.node_api.p50_ms,
            result.node_api.p95_ms,
            result.node_api.max_ms,
            result.history_api.p50_ms,
            result.history_api.p95_ms,
            result.history_api.max_ms,
        );
    }
    Ok(())
}

async fn run_reconnect_storm_load_test() -> Result<()> {
    let scenarios = [20_usize, 50, 100, 200];
    println!(
        "STORM_LOAD_TEST starting scenarios={:?} cycles={} metrics_per_cycle={} read_probes_per_cycle={}",
        scenarios,
        LOAD_TEST_STORM_CYCLES,
        LOAD_TEST_STORM_METRICS_PER_CYCLE,
        LOAD_TEST_STORM_READ_PROBES,
    );
    for &node_count in &scenarios {
        let result = run_reconnect_storm_scenario(node_count).await?;
        println!(
            "STORM_RESULT nodes={} cycles={} sessions_total={} connect_p50_ms={:.2} connect_p95_ms={:.2} connect_max_ms={:.2} recover_p50_ms={:.2} recover_p95_ms={:.2} recover_max_ms={:.2} disconnect_p50_ms={:.2} disconnect_p95_ms={:.2} disconnect_max_ms={:.2} overview_p50_ms={:.2} overview_p95_ms={:.2} overview_max_ms={:.2} nodes_p50_ms={:.2} nodes_p95_ms={:.2} nodes_max_ms={:.2}",
            result.nodes,
            result.cycles,
            result.sessions_total,
            result.connect.p50_ms,
            result.connect.p95_ms,
            result.connect.max_ms,
            result.recover.p50_ms,
            result.recover.p95_ms,
            result.recover.max_ms,
            result.disconnect.p50_ms,
            result.disconnect.p95_ms,
            result.disconnect.max_ms,
            result.overview.p50_ms,
            result.overview.p95_ms,
            result.overview.max_ms,
            result.nodes_api.p50_ms,
            result.nodes_api.p95_ms,
            result.nodes_api.max_ms,
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
    let workload = AgentWorkload {
        uptime_start: 1,
        metrics_per_node: LOAD_TEST_METRICS_PER_NODE,
        inter_message_delay: Duration::ZERO,
        hold_after_send: Duration::ZERO,
    };

    for credential in credentials.clone() {
        handles.push(tokio::spawn(run_fake_agent(
            server.addr,
            credential,
            workload,
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
        true,
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
    let overview = summarize_latencies(&latencies)?;

    Ok(ScenarioResult {
        nodes: node_count,
        metrics_total,
        connect_ms: connect_elapsed.as_secs_f64() * 1000.0,
        settle_ms: settle_elapsed.as_secs_f64() * 1000.0,
        metrics_per_sec: metrics_total as f64 / settle_secs,
        overview_p50_ms: overview.p50_ms,
        overview_p95_ms: overview.p95_ms,
        overview_max_ms: overview.max_ms,
    })
}

async fn run_api_surface_scenario(node_count: usize) -> Result<ApiScenarioResult> {
    let (server, credentials) = TestServer::start(node_count).await?;
    let representative = credentials
        .first()
        .cloned()
        .context("missing representative node credential")?;
    seed_history_points(
        server.history.clone(),
        &representative,
        LOAD_TEST_HISTORY_POINTS,
    )
    .await?;

    let (ready_tx, mut ready_rx) = mpsc::unbounded_channel::<String>();
    let (stop_tx, stop_rx) = watch::channel(false);
    let burst_barrier = Arc::new(Barrier::new(node_count + 1));
    let mut handles = Vec::with_capacity(node_count);
    let workload = AgentWorkload {
        uptime_start: 1,
        metrics_per_node: LOAD_TEST_STEADY_METRICS_PER_NODE,
        inter_message_delay: Duration::from_millis(LOAD_TEST_STEADY_METRIC_DELAY_MS),
        hold_after_send: Duration::ZERO,
    };
    let expected_final_uptime = workload.metrics_per_node;
    let connect_started = Instant::now();

    for credential in credentials.clone() {
        handles.push(tokio::spawn(run_fake_agent(
            server.addr,
            credential,
            workload,
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

    let representative_node_id = representative.node_id.clone();
    let overview_task = tokio::spawn(probe_overview_latencies(server.addr, LOAD_TEST_READ_PROBES));
    let nodes_task = tokio::spawn(probe_nodes_latencies(
        server.addr,
        LOAD_TEST_READ_PROBES,
        node_count,
    ));
    let node_task = tokio::spawn(probe_node_status_latencies(
        server.addr,
        representative_node_id.clone(),
        LOAD_TEST_READ_PROBES,
    ));
    let history_task = tokio::spawn(probe_node_history_latencies(
        server.addr,
        representative_node_id,
        LOAD_TEST_READ_PROBES,
        LOAD_TEST_HISTORY_POINTS / 2,
    ));

    let settle_started = Instant::now();
    burst_barrier.wait().await;
    wait_for_final_snapshots(
        server.shared.clone(),
        &credentials,
        expected_final_uptime,
        Duration::from_secs(LOAD_TEST_TIMEOUT_SECS),
        true,
    )
    .await?;
    let settle_elapsed = settle_started.elapsed();

    let _ = stop_tx.send(true);
    for handle in handles {
        handle
            .await
            .map_err(|error| anyhow!("join fake agent task: {error}"))??;
    }

    let overview = summarize_latencies(
        &overview_task
            .await
            .map_err(|error| anyhow!("join overview probe task: {error}"))??,
    )?;
    let nodes_api = summarize_latencies(
        &nodes_task
            .await
            .map_err(|error| anyhow!("join nodes probe task: {error}"))??,
    )?;
    let node_api = summarize_latencies(
        &node_task
            .await
            .map_err(|error| anyhow!("join node probe task: {error}"))??,
    )?;
    let history_api = summarize_latencies(
        &history_task
            .await
            .map_err(|error| anyhow!("join history probe task: {error}"))??,
    )?;

    server.shutdown().await?;

    let metrics_total = node_count * workload.metrics_per_node as usize;
    let settle_secs = settle_elapsed.as_secs_f64().max(0.001);

    Ok(ApiScenarioResult {
        nodes: node_count,
        steady_metrics_total: metrics_total,
        connect_ms: connect_elapsed.as_secs_f64() * 1000.0,
        settle_ms: settle_elapsed.as_secs_f64() * 1000.0,
        steady_metrics_per_sec: metrics_total as f64 / settle_secs,
        history_seed_points: LOAD_TEST_HISTORY_POINTS,
        overview,
        nodes_api,
        node_api,
        history_api,
    })
}

async fn run_reconnect_storm_scenario(node_count: usize) -> Result<StormScenarioResult> {
    let (server, credentials) = TestServer::start(node_count).await?;
    let mut connect_latencies = Vec::with_capacity(LOAD_TEST_STORM_CYCLES);
    let mut recover_latencies = Vec::with_capacity(LOAD_TEST_STORM_CYCLES);
    let mut disconnect_latencies = Vec::with_capacity(LOAD_TEST_STORM_CYCLES);
    let mut overview_latencies =
        Vec::with_capacity(LOAD_TEST_STORM_CYCLES * LOAD_TEST_STORM_READ_PROBES);
    let mut nodes_latencies =
        Vec::with_capacity(LOAD_TEST_STORM_CYCLES * LOAD_TEST_STORM_READ_PROBES);

    for cycle in 0..LOAD_TEST_STORM_CYCLES {
        let (ready_tx, mut ready_rx) = mpsc::unbounded_channel::<String>();
        let burst_barrier = Arc::new(Barrier::new(node_count + 1));
        let mut handles = Vec::with_capacity(node_count);
        let workload = AgentWorkload {
            uptime_start: cycle as u64 * 1_000 + 1,
            metrics_per_node: LOAD_TEST_STORM_METRICS_PER_CYCLE,
            inter_message_delay: Duration::from_millis(LOAD_TEST_STORM_METRIC_DELAY_MS),
            hold_after_send: Duration::from_millis(250),
        };
        let expected_final_uptime = workload.uptime_start + workload.metrics_per_node - 1;
        let connect_started = Instant::now();

        for credential in credentials.clone() {
            handles.push(tokio::spawn(run_fake_agent_session(
                server.addr,
                credential,
                workload,
                ready_tx.clone(),
                burst_barrier.clone(),
            )));
        }
        drop(ready_tx);

        let mut ready_nodes = HashSet::with_capacity(node_count);
        while ready_nodes.len() < node_count {
            let next = timeout(Duration::from_secs(LOAD_TEST_TIMEOUT_SECS), ready_rx.recv())
                .await
                .with_context(|| format!("timed out waiting for storm cycle {} auth", cycle + 1))?;
            let Some(node_id) = next else {
                bail!(
                    "storm cycle {} ready channel closed early after {} / {} nodes",
                    cycle + 1,
                    ready_nodes.len(),
                    node_count
                );
            };
            ready_nodes.insert(node_id);
        }
        connect_latencies.push(connect_started.elapsed());

        let overview_task = tokio::spawn(probe_overview_latencies(
            server.addr,
            LOAD_TEST_STORM_READ_PROBES,
        ));
        let nodes_task = tokio::spawn(probe_nodes_latencies(
            server.addr,
            LOAD_TEST_STORM_READ_PROBES,
            node_count,
        ));

        let recover_started = Instant::now();
        burst_barrier.wait().await;
        wait_for_final_snapshots(
            server.shared.clone(),
            &credentials,
            expected_final_uptime,
            Duration::from_secs(LOAD_TEST_TIMEOUT_SECS),
            false,
        )
        .await?;
        recover_latencies.push(recover_started.elapsed());

        for handle in handles {
            handle
                .await
                .map_err(|error| anyhow!("join storm agent task: {error}"))??;
        }

        let disconnect_started = Instant::now();
        wait_for_all_offline(
            server.shared.clone(),
            &credentials,
            Duration::from_secs(LOAD_TEST_TIMEOUT_SECS),
        )
        .await?;
        disconnect_latencies.push(disconnect_started.elapsed());

        overview_latencies.extend(
            overview_task
                .await
                .map_err(|error| anyhow!("join storm overview probe task: {error}"))??,
        );
        nodes_latencies.extend(
            nodes_task
                .await
                .map_err(|error| anyhow!("join storm nodes probe task: {error}"))??,
        );
    }

    server.shutdown().await?;

    Ok(StormScenarioResult {
        nodes: node_count,
        cycles: LOAD_TEST_STORM_CYCLES,
        sessions_total: node_count * LOAD_TEST_STORM_CYCLES,
        connect: summarize_latencies(&connect_latencies)?,
        recover: summarize_latencies(&recover_latencies)?,
        disconnect: summarize_latencies(&disconnect_latencies)?,
        overview: summarize_latencies(&overview_latencies)?,
        nodes_api: summarize_latencies(&nodes_latencies)?,
    })
}

async fn run_fake_agent(
    addr: SocketAddr,
    credential: AgentCredential,
    workload: AgentWorkload,
    ready_tx: mpsc::UnboundedSender<String>,
    burst_barrier: Arc<Barrier>,
    mut stop_rx: watch::Receiver<bool>,
) -> Result<()> {
    let mut socket = connect_authenticated_fake_agent(addr, &credential, ready_tx).await?;
    send_metrics_workload(&mut socket, workload, burst_barrier).await?;

    let _ = stop_rx.changed().await;
    let _ = socket.close(None).await;
    Ok(())
}

async fn run_fake_agent_session(
    addr: SocketAddr,
    credential: AgentCredential,
    workload: AgentWorkload,
    ready_tx: mpsc::UnboundedSender<String>,
    burst_barrier: Arc<Barrier>,
) -> Result<()> {
    let mut socket = connect_authenticated_fake_agent(addr, &credential, ready_tx).await?;
    send_metrics_workload(&mut socket, workload, burst_barrier).await?;
    if !workload.hold_after_send.is_zero() {
        sleep(workload.hold_after_send).await;
    }
    let _ = socket.close(None).await;
    Ok(())
}

async fn wait_for_authenticated_notice(socket: &mut TestSocket, node_id: &str) -> Result<()> {
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
                                &WireMessage::Pong(nodelite_proto::PongMessage {
                                    nonce: ping.nonce,
                                }),
                            )
                            .await?;
                        }
                        WireMessage::ServerNotice(notice)
                            if notice.level == nodelite_proto::NoticeLevel::Error =>
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

async fn send_wire_message(socket: &mut TestSocket, message: &WireMessage) -> Result<()> {
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
    fake_snapshot_at(uptime_secs, Utc::now())
}

fn fake_snapshot_at(uptime_secs: u64, collected_at: chrono::DateTime<Utc>) -> NodeSnapshot {
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

async fn wait_for_final_snapshots(
    shared: SharedState,
    credentials: &[AgentCredential],
    expected_uptime: u64,
    timeout_duration: Duration,
    require_online: bool,
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
                (!require_online || status.online)
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

async fn wait_for_all_offline(
    shared: SharedState,
    credentials: &[AgentCredential],
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
        let all_offline = expected_nodes
            .iter()
            .all(|node_id| by_id.get(node_id).is_some_and(|status| !status.online));
        if all_offline {
            return Ok(());
        }
        if started.elapsed() > timeout_duration {
            let mut unfinished = Vec::new();
            for node_id in &expected_nodes {
                match by_id.get(node_id) {
                    Some(status) => unfinished.push(format!("{node_id} online={}", status.online)),
                    None => unfinished.push(format!("{node_id} missing")),
                }
            }
            bail!(
                "timed out waiting for all nodes to disconnect: {}",
                unfinished.join(", ")
            );
        }
        sleep(Duration::from_millis(20)).await;
    }
}

async fn connect_authenticated_fake_agent(
    addr: SocketAddr,
    credential: &AgentCredential,
    ready_tx: mpsc::UnboundedSender<String>,
) -> Result<TestSocket> {
    let url = format!("ws://{addr}/ws");
    let (mut socket, _response) = connect_async(url)
        .await
        .with_context(|| format!("connect fake agent {}", credential.node_id))?;

    let hello = WireMessage::Hello(HelloMessage {
        protocol_version: nodelite_proto::WIRE_PROTOCOL_VERSION,
        token: credential.token.clone(),
        identity: fake_identity(credential),
    });
    send_wire_message(&mut socket, &hello).await?;
    wait_for_authenticated_notice(&mut socket, &credential.node_id).await?;
    ready_tx
        .send(credential.node_id.clone())
        .map_err(|_| anyhow!("ready channel closed"))?;
    Ok(socket)
}

async fn send_metrics_workload(
    socket: &mut TestSocket,
    workload: AgentWorkload,
    burst_barrier: Arc<Barrier>,
) -> Result<()> {
    burst_barrier.wait().await;
    for step in 0..workload.metrics_per_node {
        let metrics = WireMessage::Metrics(MetricsMessage {
            snapshot: fake_snapshot(workload.uptime_start + step),
        });
        send_wire_message(socket, &metrics).await?;
        if !workload.inter_message_delay.is_zero() {
            sleep(workload.inter_message_delay).await;
        }
    }
    Ok(())
}

async fn seed_history_points(
    history: HistoryStore,
    credential: &AgentCredential,
    points: usize,
) -> Result<()> {
    let now = Utc::now();
    let spacing_secs = nodelite_proto::DEFAULT_HISTORY_WRITE_INTERVAL_SECS as i64;
    let first_point_at = now - chrono::Duration::seconds((points as i64 - 1).max(0) * spacing_secs);
    for index in 0..points {
        let recorded_at = first_point_at + chrono::Duration::seconds(index as i64 * spacing_secs);
        let status = NodeStatus {
            identity: fake_identity(credential),
            remote_ip: Some("127.0.0.1".to_string()),
            snapshot: Some(fake_snapshot_at(index as u64 + 1, recorded_at)),
            last_seen: Some(recorded_at),
            latency_ms: Some(6 + (index as u64 % 17)),
            online: true,
        };
        history.record_status(&status).await;
    }
    Ok(())
}

async fn probe_overview_latencies(addr: SocketAddr, samples: usize) -> Result<Vec<Duration>> {
    let mut latencies = Vec::with_capacity(samples);
    for _ in 0..samples {
        let (latency, body) = fetch_http_latency(addr, "/api/overview").await?;
        validate_overview_body(&body)?;
        latencies.push(latency);
        sleep(Duration::from_millis(25)).await;
    }
    Ok(latencies)
}

async fn probe_nodes_latencies(
    addr: SocketAddr,
    samples: usize,
    expected_nodes: usize,
) -> Result<Vec<Duration>> {
    let mut latencies = Vec::with_capacity(samples);
    for _ in 0..samples {
        let (latency, body) = fetch_http_latency(addr, "/api/nodes").await?;
        validate_nodes_body(&body, expected_nodes)?;
        latencies.push(latency);
        sleep(Duration::from_millis(20)).await;
    }
    Ok(latencies)
}

async fn probe_node_status_latencies(
    addr: SocketAddr,
    node_id: String,
    samples: usize,
) -> Result<Vec<Duration>> {
    let mut latencies = Vec::with_capacity(samples);
    let path = format!("/api/nodes/{node_id}");
    for _ in 0..samples {
        let (latency, body) = fetch_http_latency(addr, &path).await?;
        validate_node_status_body(&body, &node_id)?;
        latencies.push(latency);
        sleep(Duration::from_millis(20)).await;
    }
    Ok(latencies)
}

async fn probe_node_history_latencies(
    addr: SocketAddr,
    node_id: String,
    samples: usize,
    min_points: usize,
) -> Result<Vec<Duration>> {
    let mut latencies = Vec::with_capacity(samples);
    let path = format!("/api/nodes/{node_id}/history?window_hours=24&max_points=480");
    for _ in 0..samples {
        let (latency, body) = fetch_http_latency(addr, &path).await?;
        validate_history_body(&body, &node_id, min_points)?;
        latencies.push(latency);
        sleep(Duration::from_millis(20)).await;
    }
    Ok(latencies)
}

async fn fetch_http_latency(addr: SocketAddr, path: &str) -> Result<(Duration, String)> {
    let started = Instant::now();
    let mut stream = TcpStream::connect(addr)
        .await
        .with_context(|| format!("connect http probe to {addr}"))?;
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: {LOAD_TEST_BASIC_AUTH}\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .with_context(|| format!("write http request for {path}"))?;

    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .with_context(|| format!("read http response for {path}"))?;

    let response_text = String::from_utf8_lossy(&response);
    if !response_text.starts_with("HTTP/1.1 200") && !response_text.starts_with("HTTP/1.0 200") {
        bail!("unexpected http response for {path}: {response_text}");
    }

    let Some((_, body)) = response_text.split_once("\r\n\r\n") else {
        bail!("missing http body separator for {path}");
    };

    Ok((started.elapsed(), body.to_string()))
}

fn validate_overview_body(body: &str) -> Result<()> {
    let overview: serde_json::Value = serde_json::from_str(body).context("decode overview body")?;
    let total_nodes = overview
        .get("total_nodes")
        .and_then(serde_json::Value::as_u64)
        .context("overview missing total_nodes")?;
    if total_nodes == 0 {
        bail!("overview returned zero nodes");
    }
    Ok(())
}

fn validate_nodes_body(body: &str, expected_nodes: usize) -> Result<()> {
    let statuses: Vec<NodeStatus> = serde_json::from_str(body).context("decode nodes body")?;
    if statuses.len() != expected_nodes {
        bail!(
            "nodes endpoint returned {} nodes, expected {expected_nodes}",
            statuses.len()
        );
    }
    Ok(())
}

fn validate_node_status_body(body: &str, node_id: &str) -> Result<()> {
    let status: NodeStatus = serde_json::from_str(body).context("decode node status body")?;
    if status.identity.node_id != node_id {
        bail!(
            "node status endpoint returned {} instead of {node_id}",
            status.identity.node_id
        );
    }
    Ok(())
}

fn validate_history_body(body: &str, node_id: &str, min_points: usize) -> Result<()> {
    let points: Vec<nodelite_proto::HistoryPoint> =
        serde_json::from_str(body).context("decode node history body")?;
    if points.len() < min_points {
        bail!(
            "history endpoint returned only {} points for {node_id}, expected at least {min_points}",
            points.len()
        );
    }
    if points.iter().any(|point| point.node_id != node_id) {
        bail!("history endpoint mixed node ids for {node_id}");
    }
    Ok(())
}

fn summarize_latencies(latencies: &[Duration]) -> Result<LatencySummary> {
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

    Ok(LatencySummary {
        p50_ms: percentile(0.50),
        p95_ms: percentile(0.95),
        max_ms: *values.last().unwrap_or(&0.0),
    })
}
