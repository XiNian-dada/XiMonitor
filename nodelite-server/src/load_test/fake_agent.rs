use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use chrono::Utc;
use futures::{SinkExt, StreamExt};
use tokio::sync::{Barrier, mpsc, watch};
use tokio::time::{sleep, timeout};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

use super::{AgentCredential, AgentWorkload, LOAD_TEST_TIMEOUT_SECS, TestSocket};
use crate::history::HistoryStore;
use crate::state::SharedState;
use nodelite_proto::{
    DiskUsage, HelloMessage, LoadAverage, MemoryUsage, MetricsMessage, NetworkCounters,
    NodeIdentity, NodeSnapshot, NodeStatus, WireMessage,
};

pub(super) async fn run_fake_agent(
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

pub(super) async fn run_fake_agent_session(
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

pub(super) fn fake_identity(credential: &AgentCredential) -> NodeIdentity {
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

pub(super) fn fake_snapshot(uptime_secs: u64) -> NodeSnapshot {
    fake_snapshot_at(uptime_secs, Utc::now())
}

pub(super) fn fake_snapshot_at(
    uptime_secs: u64,
    collected_at: chrono::DateTime<Utc>,
) -> NodeSnapshot {
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

pub(super) async fn wait_for_final_snapshots(
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

pub(super) async fn wait_for_all_offline(
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

pub(super) async fn seed_history_points(
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
