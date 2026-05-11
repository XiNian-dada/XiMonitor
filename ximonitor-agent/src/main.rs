// XiMonitor Agent 入口程序。
//
// 角色:运行在被监控的 Linux 节点上,周期性采集系统指标,
// 通过 WebSocket 推送至中心 Server。
//
// 主要流程:
// 1. 读取 TOML 配置 → 初始化日志与 rustls。
// 2. 用 `HostCollector` 采集节点身份与首张快照(`--sample-once` 模式下直接打印退出)。
// 3. 进入 `run_forever` 重连循环,内部通过 `run_session` 维护一次具体的会话。
// 4. 在会话中处理:Hello → 等待服务器 `authenticated` 通知 → 周期性发送 Metrics
//    → 响应 Ping / 处理 Close。

mod collector;

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use clap::Parser;
use futures::{SinkExt, StreamExt};
use tokio::fs;
use tokio::time::{interval, sleep, timeout};
use tokio_tungstenite::connect_async_with_config;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tracing::{info, warn};
use url::Url;
use ximonitor_proto::{
    AgentConfig, HelloMessage, MetricsMessage, NoticeLevel, PingMessage, PongMessage,
    ServerNoticeMessage, WireMessage, parse_agent_config,
};

use crate::collector::new_collector;

/// 不安全传输警告的输出间隔(秒)。
const INSECURE_TRANSPORT_WARN_INTERVAL_SECS: u64 = 900;
/// 建立 WebSocket 连接时的超时阈值(秒)。
const CONNECT_TIMEOUT_SECS: u64 = 20;
/// 允许接收的单条 WebSocket 消息上限(字节)。
///
/// 服务端只会向 Agent 下发 Ping 与 ServerNotice 这两种短消息,正常体量不超过
/// 几百字节;这里收紧到 64 KiB,既给协议未来扩展留出余量,又能在被攻陷的
/// 服务端推送超大帧时由底层库主动断开,而不是让 Agent 在帧拼接阶段 OOM。
const MAX_INCOMING_MESSAGE_BYTES: usize = 64 * 1024;

/// 命令行参数。
#[derive(Debug, Parser)]
#[command(name = "ximonitor-agent")]
#[command(about = "XiMonitor Linux agent")]
struct Cli {
    /// 配置文件路径,默认 `config/agent.toml`。
    #[arg(long, default_value = "config/agent.toml")]
    config: PathBuf,
    /// 仅采集一次快照并输出 JSON,常用于调试与排障。
    #[arg(long)]
    sample_once: bool,
}

/// 单次会话失败时携带的上下文。
///
/// `established_session` 表示在出错前是否已完成认证;
/// 重连退避逻辑会据此判断要不要重置失败计数。
#[derive(Debug)]
struct SessionError {
    established_session: bool,
    source: anyhow::Error,
}

#[tokio::main]
async fn main() -> Result<()> {
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

    spawn_insecure_transport_warning(config.server.clone());
    run_forever(config, collector, identity).await
}

/// 安装 rustls 默认的密码套件提供者(ring 后端)。
fn install_rustls_crypto_provider() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| anyhow!("failed to install rustls crypto provider"))
}

/// 获取 Agent 版本号:优先使用打包时通过环境变量注入的版本,缺失则回退到 Cargo 包版本。
fn agent_build_version() -> &'static str {
    option_env!("XIMONITOR_BUILD_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
}

/// 从磁盘读取并解析 Agent 配置文件。
async fn load_agent_config(path: &Path) -> Result<AgentConfig> {
    let content = fs::read_to_string(path)
        .await
        .with_context(|| format!("failed to read config file {}", path.display()))?;
    parse_agent_config(&content)
        .map_err(|error| anyhow!("failed to parse {}: {error}", path.display()))
}

/// 初始化 `tracing` 日志:支持通过 `RUST_LOG` 调整级别。
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

/// 无限重连循环:无论会话以何种方式结束,都会按指数退避重试。
async fn run_forever(
    config: AgentConfig,
    mut collector: crate::collector::HostCollector,
    identity: ximonitor_proto::NodeIdentity,
) -> Result<()> {
    let mut attempt = 0_u32;

    loop {
        match run_session(&config, &mut collector, &identity).await {
            Ok(()) => {
                attempt = 0;
            }
            Err(error) => {
                // 已建立过认证会话的失败不计入连续失败次数,避免被偶发网络故障误判为暴力重试。
                if error.established_session {
                    attempt = 0;
                }
                let delay = reconnect_delay(attempt);
                warn!(
                    server = %config.server,
                    delay_secs = delay.as_secs(),
                    established_session = error.established_session,
                    error = ?error.source,
                    "agent session ended; retrying after backoff"
                );
                sleep(delay).await;
                attempt = attempt.saturating_add(1);
            }
        }
    }
}

/// 与 Server 进行一次完整的 WebSocket 会话。
///
/// 状态机:连接 → Hello → 等待服务器 `authenticated` 通知 → 周期上报 Metrics 直至连接断开。
async fn run_session(
    config: &AgentConfig,
    collector: &mut crate::collector::HostCollector,
    identity: &ximonitor_proto::NodeIdentity,
) -> std::result::Result<(), SessionError> {
    let (socket, _) = timeout(
        Duration::from_secs(CONNECT_TIMEOUT_SECS),
        connect_async_with_config(config.server.as_str(), Some(incoming_ws_config()), false),
    )
    .await
    .map_err(|_| session_error(false, anyhow!("timed out connecting to {}", config.server)))?
    .map_err(|error| anyhow!("failed to connect to {}: {error}", config.server))
    .map_err(|error| session_error(false, error))?;
    let (mut sender, mut receiver) = socket.split();

    send_wire_message(
        &mut sender,
        &WireMessage::Hello(HelloMessage {
            token: config.token.clone(),
            identity: identity.clone(),
        }),
    )
    .await
    .map_err(|error| session_error(false, error))?;

    let mut report_ticker = interval(Duration::from_secs(config.report_interval_secs));
    let mut authenticated = false;

    loop {
        tokio::select! {
            _ = report_ticker.tick(), if authenticated => {
                // 必须等到服务器明确下发"已认证"通知后再启动周期上报,
                // 这样重连退避逻辑就能清晰区分"认证前失败"与"认证后断开"。
                send_metrics(&mut sender, collector)
                    .await
                    .map_err(|error| session_error(true, error))?;
            }
            incoming = receiver.next() => {
                let Some(frame) = incoming else {
                    return Err(session_error(
                        authenticated,
                        anyhow!("server closed websocket connection"),
                    ));
                };
                let frame = frame.map_err(|error| session_error(authenticated, anyhow!(error)))?;
                match frame {
                    Message::Text(text) => {
                        match serde_json::from_str::<WireMessage>(&text).context("invalid websocket json").map_err(|error| session_error(authenticated, error))? {
                            WireMessage::Ping(PingMessage { nonce }) => {
                                send_wire_message(&mut sender, &WireMessage::Pong(PongMessage { nonce }))
                                    .await
                                    .map_err(|error| session_error(authenticated, error))?;
                            }
                            WireMessage::ServerNotice(ServerNoticeMessage { level, message }) => {
                                if !authenticated
                                    && matches!(level, NoticeLevel::Info)
                                    && message == "authenticated"
                                {
                                    authenticated = true;
                                }
                                log_notice(level, &message);
                            }
                            WireMessage::Hello(_) | WireMessage::Metrics(_) | WireMessage::Pong(_) => {
                                return Err(session_error(
                                    authenticated,
                                    anyhow!("received unexpected websocket message from server"),
                                ));
                            }
                        }
                    }
                    Message::Ping(payload) => {
                        sender.send(Message::Pong(payload)).await.context("failed to reply to ping frame")
                            .map_err(|error| session_error(authenticated, error))?;
                    }
                    Message::Pong(_) => {}
                    Message::Close(frame) => {
                        return Err(session_error(
                            authenticated,
                            anyhow!("server closed websocket connection: {:?}", frame),
                        ));
                    }
                    Message::Binary(_) | Message::Frame(_) => {
                        return Err(session_error(
                            authenticated,
                            anyhow!("binary websocket frames are not supported"),
                        ));
                    }
                }
            }
        }
    }
}

fn session_error(established_session: bool, source: anyhow::Error) -> SessionError {
    SessionError {
        established_session,
        source,
    }
}

/// 构造接收侧的 WebSocket 配置:把单帧与单消息上限收紧到 `MAX_INCOMING_MESSAGE_BYTES`,
/// 防止被攻陷的服务端通过下发巨型帧把 Agent 进程拖到 OOM。
fn incoming_ws_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_frame_size(Some(MAX_INCOMING_MESSAGE_BYTES))
        .max_message_size(Some(MAX_INCOMING_MESSAGE_BYTES))
}

/// 采集一次快照并以 `Metrics` 帧发送出去。
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

/// 把任意 `WireMessage` 序列化为 JSON 文本帧并发送。
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

/// 将服务器通知按级别映射到对应的 `tracing` 日志级别。
fn log_notice(level: NoticeLevel, message: &str) {
    match level {
        NoticeLevel::Info => info!(message = %message, "server notice"),
        NoticeLevel::Warn => tracing::warn!(message = %message, "server notice"),
        NoticeLevel::Error => tracing::error!(message = %message, "server notice"),
    }
}

/// 指数退避表:前几次快速重试,后续上限为 60 秒。
fn reconnect_delay(attempt: u32) -> Duration {
    let seconds = match attempt {
        0 => 1,
        1 => 2,
        2 => 4,
        3 => 8,
        4 => 16,
        5 => 32,
        _ => 60,
    };
    Duration::from_secs(seconds)
}

/// 若 Agent 配置了未启用 TLS 的远程服务器,则周期性输出警告日志。
fn spawn_insecure_transport_warning(server_url: String) {
    if !uses_insecure_remote_transport(&server_url) {
        return;
    }

    tokio::spawn(async move {
        let mut ticker = interval(Duration::from_secs(INSECURE_TRANSPORT_WARN_INTERVAL_SECS));
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
    let Ok(url) = Url::parse(server_url) else {
        return false;
    };
    if url.scheme() != "ws" {
        return false;
    }

    !host_is_local(url.host_str())
}

/// 判定主机字段是否表示本机:`localhost` 或回环 IP。
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
