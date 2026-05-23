use std::process::Command;

use anyhow::{Context, Result};

use super::server::{HistoryArtifactBytes, TestServer};

#[derive(Debug, Clone, Copy)]
pub(super) struct ResourceSnapshot {
    pub(super) rss_bytes: u64,
    pub(super) history_queue_depth: usize,
    pub(super) history_dropped_writes: u64,
    pub(super) history_artifacts: HistoryArtifactBytes,
}

impl ResourceSnapshot {
    pub(super) async fn capture(server: &TestServer) -> Result<Self> {
        let (history_queue_depth, _) = server.history.writer_queue_metrics().await;
        Ok(Self {
            rss_bytes: current_rss_bytes()?,
            history_queue_depth: history_queue_depth as usize,
            history_dropped_writes: server.history.dropped_writes(),
            history_artifacts: server.history_artifact_bytes().await?,
        })
    }
}

fn current_rss_bytes() -> Result<u64> {
    let pid = std::process::id().to_string();
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", pid.as_str()])
        .output()
        .context("run ps to sample current process rss")?;
    if !output.status.success() {
        anyhow::bail!("ps rss probe exited with {}", output.status);
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let rss_kib = text
        .trim()
        .parse::<u64>()
        .with_context(|| format!("parse ps rss output {text:?}"))?;
    Ok(rss_kib.saturating_mul(1024))
}
