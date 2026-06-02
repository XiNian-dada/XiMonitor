use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use nodelite_proto::{NoticeLevel, uses_insecure_remote_url};
use tokio::time::{MissedTickBehavior, interval};
use tracing::{info, warn};

use crate::collector::new_collector;
use crate::config_io::load_agent_config;
use crate::session::{AgentLogBuffer, run_forever};
use crate::support::{
    agent_build_version, init_tracing, install_rustls_crypto_provider, shutdown_signal,
};

/// 命令行参数。
#[derive(Debug, Parser)]
#[command(name = "nodelite-agent")]
#[command(about = "NodeLite agent for Linux and macOS")]
struct Cli {
    /// 配置文件路径,默认 `config/agent.toml`。
    #[arg(long, default_value = "config/agent.toml")]
    config: PathBuf,
    /// 仅采集一次快照并输出 JSON,常用于调试与排障。
    #[arg(long)]
    sample_once: bool,
}

pub async fn run() -> Result<()> {
    init_tracing();
    install_rustls_crypto_provider()?;

    let cli = Cli::parse();
    let config = load_agent_config(&cli.config).await?;
    let mut collector = new_collector();
    let identity = collector.collect_identity(&config, agent_build_version())?;

    info!(
        node_id = %identity.node_id,
        node_label = %identity.node_label,
        "agent configuration loaded"
    );
    let mut log_buffer = AgentLogBuffer::default();
    log_buffer.push(
        NoticeLevel::Info,
        format!(
            "agent configuration loaded for {} ({})",
            identity.node_label, identity.node_id
        ),
    );

    if cli.sample_once {
        return run_sample_once(&mut collector, &config);
    }

    spawn_insecure_transport_warning(
        config.server.clone(),
        config.insecure_transport_warn_interval_secs,
    );
    run_forever(
        config,
        collector,
        identity,
        cli.config,
        log_buffer,
        shutdown_signal(),
    )
    .await
}

fn run_sample_once(
    collector: &mut crate::collector::HostCollector,
    config: &nodelite_proto::AgentConfig,
) -> Result<()> {
    let snapshot = collector.collect_snapshot()?;
    let output = serde_json::json!({
        "identity": collector.collect_identity(config, agent_build_version())?,
        "snapshot": snapshot,
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&output).context("serialize sample output")?
    );
    Ok(())
}

/// 若 Agent 配置了未启用 TLS 的远程服务器,则周期性输出警告日志。
fn spawn_insecure_transport_warning(
    server_url: String,
    insecure_transport_warn_interval_secs: u64,
) {
    if !uses_insecure_remote_transport(&server_url) {
        return;
    }

    tokio::spawn(async move {
        let mut ticker = interval(Duration::from_secs(insecure_transport_warn_interval_secs));
        // 警告是节流型日志,跳过错过的 tick 即可,不要在恢复后连续 burst 多条相同警告。
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            warn!(
                server = %server_url,
                "agent is configured without TLS; use a wss:// server URL in production",
            );
        }
    });
}

/// 判定服务器 URL 是否属于"远程明文"传输:`ws://` 且主机不是本地回环。
fn uses_insecure_remote_transport(server_url: &str) -> bool {
    uses_insecure_remote_url(server_url, "ws")
}

#[cfg(test)]
mod tests {
    use super::uses_insecure_remote_transport;

    #[test]
    fn warns_for_remote_ws_transport() {
        assert!(uses_insecure_remote_transport(
            "ws://monitor.example.com/ws"
        ));
        assert!(uses_insecure_remote_transport("ws://203.0.113.10/ws"));
    }

    #[test]
    fn ignores_local_or_tls_agent_transport() {
        assert!(!uses_insecure_remote_transport(
            "wss://monitor.example.com/ws"
        ));
        assert!(!uses_insecure_remote_transport("ws://127.0.0.1:8080/ws"));
        assert!(!uses_insecure_remote_transport("ws://localhost:8080/ws"));
    }
}
