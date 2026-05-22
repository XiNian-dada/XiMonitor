use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use futures::{SinkExt, StreamExt};
use getrandom::fill as fill_random;
use nodelite_proto::{
    AgentConfig, AgentLogEntry, AgentLogsMessage, HelloMessage, MetricsMessage, NoticeLevel,
    PingMessage, PongMessage, ServerNoticeMessage, WIRE_PROTOCOL_VERSION, WireMessage,
    truncate_to_byte_boundary,
};
use tokio::time::{MissedTickBehavior, interval, sleep, timeout};
use tokio_tungstenite::connect_async_with_config;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tracing::{info, warn};

use crate::collector::HostCollector;
use crate::config_io::update_token_in_config;
use crate::support::shutdown_signal;

/// Agent 本地最多暂存的待上报日志条数。超出后丢弃最旧项,避免断线期间内存无限增长。
pub(crate) const MAX_PENDING_AGENT_LOGS: usize = 256;
/// 单次推送到服务端的最大日志条数,控制消息体积。
const MAX_AGENT_LOG_BATCH: usize = 32;
/// 单条日志消息的最大字节数,避免异常长错误串撑爆 WebSocket 消息。
const MAX_AGENT_LOG_MESSAGE_BYTES: usize = 240;
/// Token 过期后的重连间隔。
const TOKEN_EXPIRED_RECONNECT_DELAY: Duration = Duration::from_secs(3600);

#[derive(Debug)]
pub(crate) struct SessionError {
    pub(crate) established_session: bool,
    pub(crate) token_expired: bool,
    pub(crate) source: anyhow::Error,
}

type AgentWsSender = futures::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    Message,
>;

#[derive(Default)]
pub(crate) struct AgentLogBuffer {
    entries: VecDeque<AgentLogEntry>,
}

impl AgentLogBuffer {
    pub(crate) fn push(&mut self, level: NoticeLevel, message: impl Into<String>) {
        let message = truncate_to_byte_boundary(&message.into(), MAX_AGENT_LOG_MESSAGE_BYTES)
            .trim()
            .to_string();
        if message.is_empty() {
            return;
        }
        if self.entries.len() >= MAX_PENDING_AGENT_LOGS {
            self.entries.pop_front();
        }
        self.entries.push_back(AgentLogEntry {
            occurred_at: Utc::now().to_rfc3339(),
            level,
            message,
        });
    }

    fn peek_batch(&self) -> Vec<AgentLogEntry> {
        self.entries
            .iter()
            .take(MAX_AGENT_LOG_BATCH)
            .cloned()
            .collect()
    }

    fn discard_sent(&mut self, count: usize) {
        for _ in 0..count {
            self.entries.pop_front();
        }
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// 无限重连循环:无论会话以何种方式结束,都会按指数退避重试。
pub(crate) async fn run_forever(
    mut config: AgentConfig,
    mut collector: HostCollector,
    identity: nodelite_proto::NodeIdentity,
    config_path: PathBuf,
    mut log_buffer: AgentLogBuffer,
) -> Result<()> {
    let mut attempt = 0_u32;

    loop {
        let next = async {
            match run_session(
                &mut config,
                &mut collector,
                &identity,
                &config_path,
                &mut log_buffer,
            )
            .await
            {
                Ok(()) => {
                    attempt = 0;
                }
                Err(error) => {
                    if error.established_session {
                        attempt = 0;
                    }
                    let delay = if error.token_expired {
                        attempt = 0;
                        TOKEN_EXPIRED_RECONNECT_DELAY
                    } else {
                        reconnect_delay(attempt)
                    };
                    let reason = error.source.to_string();
                    let level = if error.token_expired {
                        NoticeLevel::Error
                    } else if error.established_session {
                        NoticeLevel::Warn
                    } else {
                        NoticeLevel::Info
                    };
                    log_buffer.push(
                        level,
                        format!(
                            "session ended: {}; retrying in {}s",
                            reason,
                            delay.as_secs()
                        ),
                    );
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
pub(crate) async fn run_session(
    config: &mut AgentConfig,
    collector: &mut HostCollector,
    identity: &nodelite_proto::NodeIdentity,
    config_path: &Path,
    log_buffer: &mut AgentLogBuffer,
) -> std::result::Result<(), SessionError> {
    log_buffer.push(
        NoticeLevel::Info,
        format!("connecting to {}", config.server),
    );
    let (socket, _) = timeout(
        Duration::from_secs(config.connect_timeout_secs),
        connect_async_with_config(
            config.server.as_str(),
            Some(incoming_ws_config(config.max_incoming_message_bytes)),
            false,
        ),
    )
    .await
    .map_err(|_| session_error(false, anyhow!("timed out connecting to {}", config.server)))?
    .map_err(|error| anyhow!("failed to connect to {}: {error}", config.server))
    .map_err(|error| session_error(false, error))?;
    let (mut sender, mut receiver) = socket.split();

    send_wire_message(
        &mut sender,
        &WireMessage::Hello(HelloMessage {
            protocol_version: WIRE_PROTOCOL_VERSION,
            token: config.token.clone(),
            identity: identity.clone(),
        }),
    )
    .await
    .map_err(|error| session_error(false, error))?;

    let mut report_ticker = interval(Duration::from_secs(config.report_interval_secs));
    report_ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut authenticated = false;

    loop {
        tokio::select! {
            _ = report_ticker.tick(), if authenticated => {
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
                                    log_buffer.push(
                                        NoticeLevel::Info,
                                        format!("authenticated with {}", config.server),
                                    );
                                    flush_agent_logs(&mut sender, log_buffer)
                                        .await
                                        .map_err(|error| session_error(authenticated, error))?;
                                }

                                if matches!(level, NoticeLevel::Error)
                                    && message.contains("token expired")
                                {
                                    log_buffer.push(
                                        NoticeLevel::Error,
                                        "agent token expired; waiting for operator to rotate token",
                                    );
                                    tracing::error!(
                                        message = %message,
                                        "agent token expired; sleeping until operator rotates token",
                                    );
                                    return Err(token_expired_error(anyhow!(
                                        "agent token expired"
                                    )));
                                }

                                log_notice(level, &message);
                                if message != "authenticated" {
                                    log_buffer.push(level, format!("server notice: {message}"));
                                    if authenticated {
                                        flush_agent_logs(&mut sender, log_buffer)
                                            .await
                                            .map_err(|error| session_error(authenticated, error))?;
                                    }
                                }
                            }
                            WireMessage::RefreshTokenResponse(response) => {
                                info!("received new token, expires at {}", response.expires_at);
                                log_buffer.push(
                                    NoticeLevel::Info,
                                    format!("received refreshed token expiring at {}", response.expires_at),
                                );
                                config.token = response.new_token.clone();

                                if let Err(error) = update_token_in_config(config_path, &response.new_token).await {
                                    warn!("failed to persist new token: {}", error);
                                    log_buffer.push(
                                        NoticeLevel::Warn,
                                        format!("failed to persist refreshed token: {error}"),
                                    );
                                } else {
                                    info!("successfully persisted new token to config file");
                                    log_buffer.push(
                                        NoticeLevel::Info,
                                        "persisted refreshed token to local config",
                                    );
                                }
                                flush_agent_logs(&mut sender, log_buffer)
                                    .await
                                    .map_err(|error| session_error(authenticated, error))?;
                            }
                            WireMessage::Hello(_)
                            | WireMessage::Metrics(_)
                            | WireMessage::Pong(_)
                            | WireMessage::RefreshTokenRequest(_)
                            | WireMessage::AgentLogs(_) => {
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

/// 构造接收侧的 WebSocket 配置。
fn incoming_ws_config(max_incoming_message_bytes: usize) -> WebSocketConfig {
    WebSocketConfig::default()
        .max_frame_size(Some(max_incoming_message_bytes))
        .max_message_size(Some(max_incoming_message_bytes))
}

async fn send_metrics(sender: &mut AgentWsSender, collector: &mut HostCollector) -> Result<()> {
    let snapshot = collector.collect_snapshot()?;
    send_wire_message(sender, &WireMessage::Metrics(MetricsMessage { snapshot })).await
}

async fn send_wire_message(sender: &mut AgentWsSender, message: &WireMessage) -> Result<()> {
    let payload = serde_json::to_string(message).context("serialize websocket message")?;
    sender
        .send(Message::Text(payload.into()))
        .await
        .context("send websocket message")?;
    Ok(())
}

async fn flush_agent_logs(
    sender: &mut AgentWsSender,
    log_buffer: &mut AgentLogBuffer,
) -> Result<()> {
    while !log_buffer.is_empty() {
        let batch = log_buffer.peek_batch();
        if batch.is_empty() {
            break;
        }
        send_wire_message(
            sender,
            &WireMessage::AgentLogs(AgentLogsMessage {
                entries: batch.clone(),
            }),
        )
        .await?;
        log_buffer.discard_sent(batch.len());
    }
    Ok(())
}

fn log_notice(level: NoticeLevel, message: &str) {
    match level {
        NoticeLevel::Info => info!(message = %message, "server notice"),
        NoticeLevel::Warn => tracing::warn!(message = %message, "server notice"),
        NoticeLevel::Error => tracing::error!(message = %message, "server notice"),
    }
}

fn reconnect_delay(attempt: u32) -> Duration {
    let (floor_secs, ceiling_secs): (u64, u64) = match attempt {
        0 => (1, 5),
        1 => (2, 10),
        2 => (5, 20),
        3 => (10, 40),
        4 => (15, 60),
        _ => (30, 120),
    };
    let floor_ms = floor_secs.saturating_mul(1000);
    let ceiling_ms = ceiling_secs.saturating_mul(1000);
    let span_ms = ceiling_ms.saturating_sub(floor_ms);
    let jitter_ms = sample_random_u64()
        .map(|value| value % span_ms.saturating_add(1))
        .unwrap_or(span_ms);
    Duration::from_millis(floor_ms.saturating_add(jitter_ms))
}

fn sample_random_u64() -> Option<u64> {
    let mut buf = [0_u8; 8];
    fill_random(&mut buf).ok()?;
    Some(u64::from_le_bytes(buf))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::time::Duration;

    use nodelite_proto::NoticeLevel;

    use super::{AgentLogBuffer, MAX_PENDING_AGENT_LOGS, reconnect_delay};

    #[test]
    fn reconnect_delay_is_within_jitter_window_and_disperses() {
        let cases: &[(u32, u64, u64)] = &[
            (0, 1, 5),
            (1, 2, 10),
            (2, 5, 20),
            (3, 10, 40),
            (4, 15, 60),
            (5, 30, 120),
            (1024, 30, 120),
        ];
        for &(attempt, floor_secs, ceiling_secs) in cases {
            let lower = Duration::from_secs(floor_secs);
            let upper = Duration::from_secs(ceiling_secs);
            let mut samples: HashSet<u128> = HashSet::new();
            for _ in 0..32 {
                let delay = reconnect_delay(attempt);
                assert!(
                    delay >= lower && delay <= upper,
                    "attempt {attempt}: {delay:?} not in [{lower:?}, {upper:?}]",
                );
                samples.insert(delay.as_millis());
            }
            assert!(
                samples.len() > 1,
                "attempt {attempt}: 32 samples all identical, jitter not active",
            );
        }
    }

    #[test]
    fn agent_log_buffer_keeps_recent_entries() {
        let mut buffer = AgentLogBuffer::default();
        for index in 0..(MAX_PENDING_AGENT_LOGS + 4) {
            buffer.push(NoticeLevel::Info, format!("entry-{index}"));
        }
        let batch = buffer.peek_batch();
        assert_eq!(batch.len(), 32);
        assert_eq!(
            buffer.entries.front().map(|entry| entry.message.as_str()),
            Some("entry-4")
        );
    }

    #[test]
    fn truncate_to_byte_boundary_preserves_utf8() {
        assert_eq!(
            nodelite_proto::truncate_to_byte_boundary("日志abcdef", 4),
            "日"
        );
    }
}
