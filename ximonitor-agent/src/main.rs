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
use getrandom::fill as fill_random;
use tokio::fs;
use tokio::time::{MissedTickBehavior, interval, sleep, timeout};
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
///
/// `token_expired` 由服务端在认证阶段下发的 `token expired` 通知触发。
/// 此类失败不会因重连自愈 —— Agent 必须等待运维通过
/// `ximonitor-server install-agent --rotate-token` 颁发新 token 并替换
/// 本地 agent.toml,所以重连间隔需要拉长到小时级别,避免无谓的连接风暴。
#[derive(Debug)]
struct SessionError {
    established_session: bool,
    token_expired: bool,
    source: anyhow::Error,
}

/// Token 过期后的重连间隔:拉长到 1 小时,既给运维介入留足时间,也避免
/// Agent 反复连接服务端制造无用的失败日志与 IP 限流压力。
const TOKEN_EXPIRED_RECONNECT_DELAY: Duration = Duration::from_secs(3600);

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
    run_forever(config, collector, identity, cli.config).await
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

/// 等待 SIGTERM / SIGINT,任一信号到达即返回。
///
/// 仅在 unix 上监听 SIGTERM;其它平台只听 Ctrl-C。注册失败时退化为 `pending`,
/// 保证另一条信号路径仍能触发 —— 不会因为某个 handler 安装失败而吞掉所有信号。
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            warn!(error = ?error, "failed to listen for ctrl-c");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(error) => {
                warn!(error = ?error, "failed to install SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

/// 无限重连循环:无论会话以何种方式结束,都会按指数退避重试。
///
/// 当进程收到 SIGTERM / SIGINT 时立即从当前 await 点退出,使 Agent 能写一条
/// "shutting down" 日志后干净退出,而不是被 systemd 直接 KILL 截断。
async fn run_forever(
    mut config: AgentConfig,
    mut collector: crate::collector::HostCollector,
    identity: ximonitor_proto::NodeIdentity,
    config_path: PathBuf,
) -> Result<()> {
    let mut attempt = 0_u32;

    loop {
        let next = async {
            match run_session(&mut config, &mut collector, &identity, &config_path).await {
                Ok(()) => {
                    attempt = 0;
                }
                Err(error) => {
                    // 已建立过认证会话的失败不计入连续失败次数,避免被偶发网络故障误判为暴力重试。
                    if error.established_session {
                        attempt = 0;
                    }
                    let delay = if error.token_expired {
                        // token 过期需要人工干预,指数退避在小时级别才有意义。
                        attempt = 0;
                        TOKEN_EXPIRED_RECONNECT_DELAY
                    } else {
                        reconnect_delay(attempt)
                    };
                    warn!(
                        server = %config.server,
                        delay_secs = delay.as_secs(),
                        established_session = error.established_session,
                        token_expired = error.token_expired,
                        error = ?error.source,
                        "agent session ended; retrying after backoff"
                    );
                    sleep(delay).await;
                    if !error.token_expired {
                        attempt = attempt.saturating_add(1);
                    }
                }
            }
        };

        tokio::select! {
            _ = next => continue,
            _ = shutdown_signal() => {
                info!("agent shutting down");
                return Ok(());
            }
        }
    }
}

/// 与 Server 进行一次完整的 WebSocket 会话。
///
/// 状态机:连接 → Hello → 等待服务器 `authenticated` 通知 → 周期上报 Metrics 直至连接断开。
async fn run_session(
    config: &mut AgentConfig,
    collector: &mut crate::collector::HostCollector,
    identity: &ximonitor_proto::NodeIdentity,
    config_path: &Path,
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
    // 错过的上报视为"已错过",不要在挂起恢复后 burst 多帧 metrics —— 否则会触发
    // 服务端 sanitize 异常计数(METRIC_ANOMALY_SESSION_LIMIT)误判并主动断连。
    report_ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
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

                                // 服务端拒绝过期 token 时会先发一条带 "token expired"
                                // 的 Error notice 再关闭连接。Agent 无法在线 refresh 一个
                                // 已经过期的 token —— 必须由运维通过
                                // `ximonitor-server install-agent --rotate-token` 重新颁发
                                // 并替换本地 agent.toml。此处把这种失败提升为独立的
                                // `token_expired` 错误,使 run_forever 用小时级别的退避
                                // 代替默认的指数退避,避免无谓的连接风暴。
                                if matches!(level, NoticeLevel::Error)
                                    && message.contains("token expired")
                                {
                                    tracing::error!(
                                        message = %message,
                                        "agent token expired; sleeping until operator rotates token",
                                    );
                                    return Err(token_expired_error(anyhow!(
                                        "agent token expired"
                                    )));
                                }

                                log_notice(level, &message);
                            }
                            WireMessage::RefreshTokenResponse(response) => {
                                info!("received new token, expires at {}", response.expires_at);
                                config.token = response.new_token.clone();

                                // 持久化新 token 到配置文件
                                if let Err(e) = update_token_in_config(config_path, &response.new_token).await {
                                    warn!("failed to persist new token: {}", e);
                                } else {
                                    info!("successfully persisted new token to config file");
                                }
                            }
                            WireMessage::Hello(_)
                            | WireMessage::Metrics(_)
                            | WireMessage::Pong(_)
                            | WireMessage::RefreshTokenRequest(_) => {
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
        token_expired: false,
        source,
    }
}

fn token_expired_error(source: anyhow::Error) -> SessionError {
    SessionError {
        established_session: false,
        token_expired: true,
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

/// 指数退避表附 ±50 % 抖动:同一时刻批量失败的 Agent 不会"同步雪崩"。
///
/// 基础时长仍走 1/2/4/8/16/32/60 s 的指数序列,但每次返回值落在
/// `[base * 0.5, base * 1.5)` 内,使若干个 Agent 在同一服务端重启窗口里
/// 的下一次连接均匀地分散开;避免恢复瞬间被反代 `limit_conn` 拒绝 + 重试,
/// 反过来再放大震荡。
///
/// 当 getrandom 失败(极少见,例如刚启动时内核熵不足),退化到基础时长本身,
/// 这与未加 jitter 之前的行为等价,功能仍可用。
fn reconnect_delay(attempt: u32) -> Duration {
    let base_secs: u64 = match attempt {
        0 => 1,
        1 => 2,
        2 => 4,
        3 => 8,
        4 => 16,
        5 => 32,
        _ => 60,
    };
    let base_ms = base_secs.saturating_mul(1000);
    let half = base_ms / 2;
    let jitter_ms = sample_random_u64()
        .map(|value| value % base_ms)
        .unwrap_or(0);
    Duration::from_millis(half.saturating_add(jitter_ms))
}

/// 抽取 8 字节系统随机数;失败时返回 `None`,调用方需要给出合理的回退。
fn sample_random_u64() -> Option<u64> {
    let mut buf = [0_u8; 8];
    fill_random(&mut buf).ok()?;
    Some(u64::from_le_bytes(buf))
}

/// 若 Agent 配置了未启用 TLS 的远程服务器,则周期性输出警告日志。
fn spawn_insecure_transport_warning(server_url: String) {
    if !uses_insecure_remote_transport(&server_url) {
        return;
    }

    tokio::spawn(async move {
        let mut ticker = interval(Duration::from_secs(INSECURE_TRANSPORT_WARN_INTERVAL_SECS));
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

/// 更新配置文件中的 token。
///
/// 在 `spawn_blocking` 中以"读 → 改 → 写 → fsync → rename → fsync 父目录"
/// 的方式持久化新 token,等同于 server registry 的写入级别:
///
/// - 同步 IO 让我们能 fsync 临时文件与父目录,避免主机崩溃后 agent.toml
///   被 rename 走却没有数据落盘,造成节点永久失联;
/// - 临时文件名带 8 字节随机后缀,杜绝多 Agent 实例(或外部工具)
///   意外共享 `agent.toml.tmp` 造成的互踩;
/// - 写完后再 chmod 0o600,因为 token 是节点级别的长期凭证;
/// - "token = " 行用严格前缀匹配,而非朴素 `starts_with("token")`,
///   避免未来出现 `tokenization` / `token_secret` 等字段时被误替换。
async fn update_token_in_config(config_path: &Path, new_token: &str) -> Result<()> {
    let config_path = config_path.to_path_buf();
    let new_token = new_token.to_string();
    tokio::task::spawn_blocking(move || persist_token_sync(&config_path, &new_token))
        .await
        .context("token persistence task failed")?
}

fn persist_token_sync(config_path: &Path, new_token: &str) -> Result<()> {
    let content = std::fs::read_to_string(config_path)
        .with_context(|| format!("failed to read {}", config_path.display()))?;
    let updated = replace_token_line(&content, new_token)?;

    let tmp_path = temporary_config_path(config_path);
    write_config_payload(&tmp_path, &updated)
        .with_context(|| format!("failed to write {}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, config_path)
        .with_context(|| format!("failed to replace {}", config_path.display()))?;
    sync_parent_dir(config_path);
    harden_config_permissions(config_path)
        .with_context(|| format!("failed to chmod {}", config_path.display()))?;
    Ok(())
}

fn replace_token_line(content: &str, new_token: &str) -> Result<String> {
    let mut updated_lines: Vec<String> = Vec::with_capacity(content.lines().count() + 1);
    let mut token_updated = false;
    for line in content.lines() {
        if !token_updated && is_token_assignment_line(line) {
            let indent_len = line.len() - line.trim_start().len();
            let indent = &line[..indent_len];
            updated_lines.push(format!(
                "{indent}token = \"{}\"",
                escape_toml_string(new_token)
            ));
            token_updated = true;
        } else {
            updated_lines.push(line.to_string());
        }
    }

    if !token_updated {
        anyhow::bail!("token field not found in config file");
    }

    let mut result = updated_lines.join("\n");
    result.push('\n');
    Ok(result)
}

/// 判定某一行是不是 `token = "..."` 的赋值行。匹配规则:跳过注释行,
/// 必须以 `token` 开头,紧随其后只能是空白 + `=`。这样可以排除
/// `tokenization`、`token_secret` 等以 `token` 为前缀的字段。
fn is_token_assignment_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') {
        return false;
    }
    let Some(rest) = trimmed.strip_prefix("token") else {
        return false;
    };
    let rest = rest.trim_start_matches([' ', '\t']);
    rest.starts_with('=')
}

fn escape_toml_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn temporary_config_path(path: &Path) -> std::path::PathBuf {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("agent.toml");
    let mut suffix = [0u8; 8];
    if fill_random(&mut suffix).is_err() {
        // 极少见,但即便回退到固定后缀也好过崩溃。下面的 rename 仍是原子的。
        return path.with_file_name(format!("{file_name}.tmp"));
    }
    let suffix_hex: String = suffix.iter().map(|byte| format!("{byte:02x}")).collect();
    path.with_file_name(format!("{file_name}.tmp.{suffix_hex}"))
}

fn write_config_payload(path: &Path, payload: &str) -> Result<()> {
    use std::fs::OpenOptions;
    use std::io::Write;

    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    file.write_all(payload.as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to fsync {}", path.display()))?;
    Ok(())
}

fn sync_parent_dir(path: &Path) {
    let Some(parent) = path.parent() else {
        return;
    };
    if parent.as_os_str().is_empty() {
        return;
    }
    // 打不开父目录(权限等)时静默退出 —— 数据本身已经 fsync,目录项丢失
    // 最多让我们回退到旧 agent.toml,而旧 token 仍然有效。
    let _ = std::fs::File::open(parent).and_then(|dir| dir.sync_all());
}

fn harden_config_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::time::Duration;

    use super::{
        is_token_assignment_line, reconnect_delay, replace_token_line, uses_insecure_remote_transport,
    };

    #[test]
    fn token_assignment_line_matches_exact_token_field_only() {
        assert!(is_token_assignment_line("token = \"abc\""));
        assert!(is_token_assignment_line("  token = \"abc\""));
        assert!(is_token_assignment_line("\ttoken=\"abc\""));
        assert!(is_token_assignment_line("token=\"abc\""));

        // 不能误匹配以 token 为前缀的其它字段
        assert!(!is_token_assignment_line("tokenization = true"));
        assert!(!is_token_assignment_line("token_secret = \"xyz\""));
        assert!(!is_token_assignment_line("# token = \"old\""));
        assert!(!is_token_assignment_line("not_token = \"x\""));
    }

    #[test]
    fn replace_token_line_preserves_comments_and_indent() {
        let input = "[agent]\nnode_id = \"hk-01\"\n# token = \"old\"\n token = \"old\"\n";
        let result = replace_token_line(input, "newvalue").expect("should replace");
        // 注释行不动,真正的赋值行被替换,前导空格保留。
        assert!(result.contains("# token = \"old\""));
        assert!(result.contains(" token = \"newvalue\""));
        // 真正的赋值行不应该还保留旧 token。
        assert_eq!(result.matches("token = \"old\"").count(), 1, "only the comment line keeps the old value");
    }

    #[test]
    fn replace_token_line_only_replaces_first_occurrence() {
        // 一份配置正常只会有一处 token 赋值;但即便手工写错也只替换第一处,
        // 避免破坏配置中其它意外的 token 行(虽然概率很低)。
        let input = "token = \"a\"\ntoken = \"b\"\n";
        let result = replace_token_line(input, "c").expect("should replace first");
        assert_eq!(result, "token = \"c\"\ntoken = \"b\"\n");
    }

    #[test]
    fn replace_token_line_escapes_special_chars() {
        let result =
            replace_token_line("token = \"x\"\n", "with\"quote\\and-backslash").expect("ok");
        assert!(result.contains("\"with\\\"quote\\\\and-backslash\""));
    }

    #[test]
    fn replace_token_line_fails_when_no_token_field() {
        let result = replace_token_line("[agent]\nnode_id = \"x\"\n", "new");
        assert!(result.is_err());
    }

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

    #[test]
    fn reconnect_delay_is_within_jitter_window_and_disperses() {
        // 每次重连退避必须落在 [base * 0.5, base * 1.5) 内;
        // 同时,N 次取样必须出现 >1 个不同结果,证明 jitter 真的在生效
        // 而不是退化为常量。
        let cases: &[(u32, u64)] = &[
            (0, 1),
            (1, 2),
            (2, 4),
            (3, 8),
            (4, 16),
            (5, 32),
            (6, 60),
            (1024, 60),
        ];
        for &(attempt, base_secs) in cases {
            let base_ms = base_secs * 1000;
            let lower = Duration::from_millis(base_ms / 2);
            let upper = Duration::from_millis(base_ms / 2 + base_ms);
            let mut samples: HashSet<u128> = HashSet::new();
            for _ in 0..32 {
                let delay = reconnect_delay(attempt);
                assert!(
                    delay >= lower && delay < upper,
                    "attempt {attempt}: {delay:?} not in [{lower:?}, {upper:?})",
                );
                samples.insert(delay.as_millis());
            }
            assert!(
                samples.len() > 1,
                "attempt {attempt}: 32 samples all identical, jitter not active",
            );
        }
    }
}
