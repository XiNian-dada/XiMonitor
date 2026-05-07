mod collector;

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use clap::Parser;
use futures::{SinkExt, StreamExt};
use tokio::fs;
use tokio::time::interval;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tracing::info;
use ximonitor_proto::{
    AgentConfig, HelloMessage, MetricsMessage, NoticeLevel, PingMessage, PongMessage,
    ServerNoticeMessage, WireMessage, parse_agent_config,
};

use crate::collector::new_collector;

#[derive(Debug, Parser)]
#[command(name = "ximonitor-agent")]
#[command(about = "XiMonitor Linux agent")]
struct Cli {
    #[arg(long, default_value = "config/agent.toml")]
    config: PathBuf,
    #[arg(long)]
    sample_once: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let cli = Cli::parse();
    let config = load_agent_config(&cli.config).await?;
    let mut collector = new_collector();
    let identity = collector.collect_identity(&config, env!("CARGO_PKG_VERSION"))?;

    info!(
        node_id = %identity.node_id,
        node_label = %identity.node_label,
        "agent configuration loaded"
    );

    if cli.sample_once {
        let snapshot = collector.collect_snapshot()?;
        let output = serde_json::json!({
            "identity": identity,
            "snapshot": snapshot,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&output).context("serialize sample output")?
        );
        return Ok(());
    }

    run_session(config, collector, identity).await
}

async fn load_agent_config(path: &Path) -> Result<AgentConfig> {
    let content = fs::read_to_string(path)
        .await
        .with_context(|| format!("failed to read config file {}", path.display()))?;
    parse_agent_config(&content)
        .map_err(|error| anyhow!("failed to parse {}: {error}", path.display()))
}

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ximonitor_agent=info".into()),
        )
        .with_target(false)
        .compact()
        .init();
}

async fn run_session(
    config: AgentConfig,
    mut collector: crate::collector::HostCollector,
    identity: ximonitor_proto::NodeIdentity,
) -> Result<()> {
    let (socket, _) = connect_async(config.server.as_str())
        .await
        .with_context(|| format!("failed to connect to {}", config.server))?;
    let (mut sender, mut receiver) = socket.split();

    send_wire_message(
        &mut sender,
        &WireMessage::Hello(HelloMessage {
            token: config.token.clone(),
            identity,
        }),
    )
    .await?;

    send_metrics(&mut sender, &mut collector).await?;

    let mut report_ticker = interval(Duration::from_secs(config.report_interval_secs));

    loop {
        tokio::select! {
            _ = report_ticker.tick() => {
                send_metrics(&mut sender, &mut collector).await?;
            }
            incoming = receiver.next() => {
                let Some(frame) = incoming else {
                    return Err(anyhow!("server closed websocket connection"));
                };
                let frame = frame.context("failed to read websocket frame")?;
                match frame {
                    Message::Text(text) => {
                        match serde_json::from_str::<WireMessage>(&text).context("invalid websocket json")? {
                            WireMessage::Ping(PingMessage { nonce }) => {
                                send_wire_message(&mut sender, &WireMessage::Pong(PongMessage { nonce })).await?;
                            }
                            WireMessage::ServerNotice(ServerNoticeMessage { level, message }) => {
                                log_notice(level, &message);
                            }
                            WireMessage::Hello(_) | WireMessage::Metrics(_) | WireMessage::Pong(_) => {
                                return Err(anyhow!("received unexpected websocket message from server"));
                            }
                        }
                    }
                    Message::Ping(payload) => {
                        sender.send(Message::Pong(payload)).await.context("failed to reply to ping frame")?;
                    }
                    Message::Pong(_) => {}
                    Message::Close(frame) => {
                        return Err(anyhow!("server closed websocket connection: {:?}", frame));
                    }
                    Message::Binary(_) | Message::Frame(_) => {
                        return Err(anyhow!("binary websocket frames are not supported"));
                    }
                }
            }
        }
    }
}

async fn send_metrics(
    sender: &mut futures::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        Message,
    >,
    collector: &mut crate::collector::HostCollector,
) -> Result<()> {
    let snapshot = collector.collect_snapshot()?;
    send_wire_message(sender, &WireMessage::Metrics(MetricsMessage { snapshot })).await
}

async fn send_wire_message(
    sender: &mut futures::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        Message,
    >,
    message: &WireMessage,
) -> Result<()> {
    let payload = serde_json::to_string(message).context("serialize websocket message")?;
    sender
        .send(Message::Text(payload.into()))
        .await
        .context("send websocket message")?;
    Ok(())
}

fn log_notice(level: NoticeLevel, message: &str) {
    match level {
        NoticeLevel::Info => info!(message = %message, "server notice"),
        NoticeLevel::Warn => tracing::warn!(message = %message, "server notice"),
        NoticeLevel::Error => tracing::error!(message = %message, "server notice"),
    }
}
