use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use tokio::sync::{Barrier, mpsc, watch};
use tokio::time::timeout;

use super::fake_agent::{
    run_fake_agent, run_fake_agent_session, seed_history_points, wait_for_all_offline,
    wait_for_final_snapshots,
};
use super::probes::{
    probe_node_history_latencies, probe_node_status_latencies, probe_nodes_latencies,
    probe_overview_latencies, summarize_latencies,
};
use super::server::TestServer;
use super::{
    AgentWorkload, ApiScenarioResult, LOAD_TEST_HISTORY_POINTS, LOAD_TEST_METRICS_PER_NODE,
    LOAD_TEST_OVERVIEW_PROBES, LOAD_TEST_READ_PROBES, LOAD_TEST_STEADY_METRIC_DELAY_MS,
    LOAD_TEST_STEADY_METRICS_PER_NODE, LOAD_TEST_STORM_CYCLES, LOAD_TEST_STORM_METRIC_DELAY_MS,
    LOAD_TEST_STORM_METRICS_PER_CYCLE, LOAD_TEST_STORM_READ_PROBES, LOAD_TEST_TIMEOUT_SECS,
    ScenarioResult, StormScenarioResult,
};

pub(super) async fn run_scaling_load_test() -> Result<()> {
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

pub(super) async fn run_api_surface_load_test() -> Result<()> {
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

pub(super) async fn run_reconnect_storm_load_test() -> Result<()> {
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

    wait_for_ready_nodes(&mut ready_rx, node_count, "fake agents").await?;
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

    wait_for_ready_nodes(&mut ready_rx, node_count, "fake agents").await?;
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

        wait_for_ready_nodes(
            &mut ready_rx,
            node_count,
            &format!("storm cycle {}", cycle + 1),
        )
        .await?;
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

async fn wait_for_ready_nodes(
    ready_rx: &mut mpsc::UnboundedReceiver<String>,
    node_count: usize,
    label: &str,
) -> Result<()> {
    let mut ready_nodes = HashSet::with_capacity(node_count);
    while ready_nodes.len() < node_count {
        let next = timeout(Duration::from_secs(LOAD_TEST_TIMEOUT_SECS), ready_rx.recv())
            .await
            .with_context(|| format!("timed out waiting for {label} to authenticate"))?;
        let Some(node_id) = next else {
            bail!(
                "{label} ready channel closed early after {} / {} nodes",
                ready_nodes.len(),
                node_count
            );
        };
        ready_nodes.insert(node_id);
    }
    Ok(())
}
