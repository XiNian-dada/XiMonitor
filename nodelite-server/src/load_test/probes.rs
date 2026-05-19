use std::net::SocketAddr;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::sleep;

use super::{LOAD_TEST_BASIC_AUTH, LatencySummary};
use nodelite_proto::NodeStatus;

pub(super) async fn probe_overview_latencies(
    addr: SocketAddr,
    samples: usize,
) -> Result<Vec<Duration>> {
    let mut latencies = Vec::with_capacity(samples);
    for _ in 0..samples {
        let (latency, body) = fetch_http_latency(addr, "/api/overview").await?;
        validate_overview_body(&body)?;
        latencies.push(latency);
        sleep(Duration::from_millis(25)).await;
    }
    Ok(latencies)
}

pub(super) async fn probe_nodes_latencies(
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

pub(super) async fn probe_node_status_latencies(
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

pub(super) async fn probe_node_history_latencies(
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

pub(super) fn summarize_latencies(latencies: &[Duration]) -> Result<LatencySummary> {
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
