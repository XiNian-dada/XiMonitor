//! WebSocket 入站会话处理。
//!
//! 从 [`ws_handler`](/ws 路由入口)进来后,流程是:
//! 1. 通过 `WsAdmissionController` 拿到连接配额(RAII permit);
//! 2. 升级到 WebSocket;
//! 3. `handle_socket` 接管单个会话:Hello → registry.authorize → 进入 Ping
//!    心跳 + Metrics 数据循环 + 主动 token 轮换 + Refresh 请求处理;
//! 4. 会话退出时 SharedState/连接计数自动回收。
//!
//! 这是 server 内部最大的一段状态机,把它放到独立模块,使 main.rs 只剩
//! "组装路由 + 启动后台任务"的骨架。

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use axum::extract::ws::{CloseFrame, Message, WebSocket, close_code};
use axum::extract::{ConnectInfo, State, WebSocketUpgrade};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use futures::{SinkExt, StreamExt};
use nodelite_proto::{
    AgentLogsMessage, HelloMessage, MetricsMessage, PongMessage, ServerNoticeMessage,
    WIRE_PROTOCOL_VERSION, WireMessage,
};
use tokio::sync::mpsc;
use tokio::time::{MissedTickBehavior, interval};
use tracing::{error, info, warn};

use crate::AppState;
use crate::admission::{WsConnectionPermit, resolve_client_ip, ws_admission_error_response};
use crate::registry::NodeRegistry;
use crate::sanitize::{
    METRIC_ANOMALY_SESSION_LIMIT, METRIC_ANOMALY_WINDOW_SECS, sanitize_snapshot,
    should_disconnect_for_metric_anomalies, update_metric_anomaly_window,
};
use crate::state::{SessionCommand, SessionRefreshReply};

/// 等待 Hello 报文的超时时间(秒)。
const HELLO_TIMEOUT_SECS: u64 = 10;
/// 同时未应答的 Ping 上限,超过后会丢弃最老的一条,避免内存占用无限增长。
const MAX_OUTSTANDING_PINGS: usize = 32;
/// Token 距离过期不足该天数时,服务端在已认证会话内主动续期并下发新 token。
const AGENT_TOKEN_REFRESH_BEFORE_EXPIRY_DAYS: i64 = 7;

/// WebSocket 处理流程中的错误来源区分:
/// `Client` 表示因对方原因(协议错误、未认证)而断开,只记 warn;
/// `Server` 表示我们这边出现异常,记 error。
#[derive(Debug)]
pub enum ProtocolError {
    Client(String),
    Server(anyhow::Error),
}

impl From<anyhow::Error> for ProtocolError {
    fn from(error: anyhow::Error) -> Self {
        Self::Server(error)
    }
}

/// 单帧解析结果:
/// `Wire` 是携带 JSON 业务消息的文本帧;
/// `Control` 是底层心跳(Ping/Pong)等,无需上层处理;
/// `Close` 表示对方发起了关闭。
#[derive(Debug)]
enum ParsedFrame {
    Wire(Box<WireMessage>),
    Control,
    Close,
}

/// `/ws` 入口:在 WebSocket 升级前先做准入检查与帧大小限制。
pub async fn ws_handler(
    State(state): State<AppState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let max_message_bytes = state.shared.config().max_message_bytes;
    let client_ip = resolve_client_ip(state.shared.config().listen, peer_addr, &headers);
    let connection_permit = match state.ws_admission.try_acquire(client_ip) {
        Ok(permit) => permit,
        Err(error) => return ws_admission_error_response(error),
    };
    ws.max_frame_size(max_message_bytes)
        .max_message_size(max_message_bytes)
        .on_upgrade(move |socket| async move {
            if let Err(error) = handle_socket(state, client_ip, connection_permit, socket).await {
                match error {
                    ProtocolError::Client(message) => {
                        warn!(reason = %message, "websocket client disconnected");
                    }
                    ProtocolError::Server(error) => {
                        error!(error = ?error, "websocket session failed");
                    }
                }
            }
        })
        .into_response()
}

/// 一次完整的 WebSocket 会话:握手 → 认证 → 数据循环 → 资源回收。
async fn handle_socket(
    state: AppState,
    client_ip: IpAddr,
    _connection_permit: WsConnectionPermit,
    mut socket: WebSocket,
) -> Result<(), ProtocolError> {
    let shared = state.shared.clone();
    let shutdown = state.shutdown.clone();

    // 处于"等 hello"阶段时也要响应进程级关停:握手未完成的连接如果不在这里
    // 主动断开,后续 axum graceful shutdown 期间它们会拖住整个进程退出。
    let hello = tokio::select! {
        biased;
        _ = shutdown.cancelled() => {
            let _ = send_close_frame(&mut socket, close_code::AWAY, "server shutting down").await;
            return Ok(());
        }
        outcome = tokio::time::timeout(
            Duration::from_secs(HELLO_TIMEOUT_SECS),
            recv_hello(&mut socket),
        ) => match outcome {
            Ok(Ok(hello)) => hello,
            Ok(Err(error)) => {
                state.ws_admission.record_auth_failure(client_ip);
                return Err(error);
            }
            Err(_) => {
                state.ws_admission.record_auth_failure(client_ip);
                return Err(ProtocolError::Client(
                    "timed out waiting for hello message".to_string(),
                ));
            }
        },
    };
    if hello.protocol_version != WIRE_PROTOCOL_VERSION {
        state.ws_admission.record_auth_failure(client_ip);
        let notice = WireMessage::ServerNotice(ServerNoticeMessage {
            level: nodelite_proto::NoticeLevel::Error,
            message: format!(
                "unsupported protocol version {}; server expects {}",
                hello.protocol_version, WIRE_PROTOCOL_VERSION
            ),
        });
        let _ = send_wire_message(&mut socket, &notice).await;
        return Err(ProtocolError::Client(format!(
            "unsupported protocol version {}; expected {}",
            hello.protocol_version, WIRE_PROTOCOL_VERSION
        )));
    }
    let mut session_token = hello.token.clone();
    let authorized = match state
        .registry
        .authorize(&hello.identity, &session_token)
        .await
    {
        Ok(authorized) => authorized,
        Err(error) => {
            warn!(
                client_ip = %client_ip,
                requested_node_id = %hello.identity.node_id,
                error = ?error,
                "websocket authentication rejected",
            );
            state.ws_admission.record_auth_failure(client_ip);

            // 拒绝前先通过 ServerNotice 告知 Agent 失败原因,使 Agent 端日志
            // 与运维报警能直接区分"token 过期需要重新颁发"与"通用拒绝"。
            // 发送失败不影响后续关闭逻辑;只是 best-effort 的诊断信息。
            let error_msg = error.to_string();
            let (notice_message, error_label): (&str, &str) = if error_msg.contains("token expired")
            {
                (
                    "token expired; run `nodelite-server install-agent --rotate-token` and reinstall this node",
                    "token expired",
                )
            } else {
                ("unauthorized", "unauthorized")
            };
            let notice = WireMessage::ServerNotice(ServerNoticeMessage {
                level: nodelite_proto::NoticeLevel::Error,
                message: notice_message.to_string(),
            });
            let _ = send_wire_message(&mut socket, &notice).await;
            return Err(ProtocolError::Client(error_label.to_string()));
        }
    };
    state.ws_admission.clear_auth_failures(client_ip);

    let identity = authorized.identity;
    // 捕获 hello 时刻的 token 代次; 之后所有 `is_token_current` 调用都比对这个值,
    // 一旦操作员轮换 token, 代次就会 +1, 当前会话会被立刻断开。
    let mut session_generation = authorized.generation;
    let node_id = identity.node_id.clone();
    let node_label = identity.node_label.clone();
    let session_id = shared
        .register_node(identity, Some(client_ip.to_string()))
        .await;

    info!(node_id = %node_id, node_label = %node_label, session_id, "node authenticated");

    let session_result: Result<(), ProtocolError> = async {
        let notice = WireMessage::ServerNotice(ServerNoticeMessage {
            level: nodelite_proto::NoticeLevel::Info,
            message: "authenticated".to_string(),
        });
        send_wire_message(&mut socket, &notice).await?;

        let (mut sender, mut receiver) = socket.split();
        let (session_control_tx, mut session_control_rx) = mpsc::unbounded_channel();
        if !shared
            .attach_session_control(&node_id, session_id, session_control_tx)
            .await
        {
            warn!(node_id = %node_id, session_id, "failed to attach control channel for superseded session");
            return Ok(());
        }
        let ping_every = Duration::from_secs(shared.config().ping_interval_secs);
        let ping_expiry = Duration::from_secs(shared.config().ping_interval_secs.saturating_mul(3));
        let mut ping_ticker = interval(ping_every);
        // 会话挂起/恢复后不要"补打"积压的 tick,否则会瞬间灌满 outstanding_pings。
        ping_ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut outstanding_pings: HashMap<u64, Instant> = HashMap::new();
        let mut next_ping_nonce = 1_u64;
        let mut metric_anomaly_window: VecDeque<Instant> = VecDeque::new();

        loop {
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => {
                    info!(node_id = %node_id, session_id, "closing websocket session due to server shutdown");
                    let _ = sender
                        .send(Message::Close(Some(CloseFrame {
                            code: close_code::AWAY,
                            reason: "server shutting down".into(),
                        })))
                        .await;
                    break Ok(());
                }
                incoming = receiver.next() => {
                    let Some(frame) = incoming else {
                        break Ok(());
                    };
                    let frame = frame.map_err(|error| anyhow!("websocket receive failed: {error}"))?;

                    match parse_wire_message(frame)? {
                        ParsedFrame::Close => break Ok(()),
                        ParsedFrame::Control => continue,
                        ParsedFrame::Wire(message) => match *message {
                            WireMessage::Metrics(MetricsMessage { snapshot }) => {
                                if !state.registry.is_token_current(&node_id, session_generation).await {
                                    warn!(node_id = %node_id, "disconnecting session after registry token change");
                                    break Ok(());
                                }
                                let (snapshot, report) = sanitize_snapshot(shared.config(), snapshot);
                                if report.modified() {
                                    update_metric_anomaly_window(
                                        &mut metric_anomaly_window,
                                        &report,
                                        Instant::now(),
                                    );
                                    warn!(
                                        node_id = %node_id,
                                        session_id,
                                        anomalies = report.total(),
                                        anomaly_window_size = metric_anomaly_window.len(),
                                        "agent reported out-of-range metrics; clamped before persistence",
                                    );
                                    if should_disconnect_for_metric_anomalies(&metric_anomaly_window) {
                                        warn!(
                                            node_id = %node_id,
                                            session_id,
                                            limit = METRIC_ANOMALY_SESSION_LIMIT,
                                            window_secs = METRIC_ANOMALY_WINDOW_SECS,
                                            "disconnecting session after repeated metric anomalies",
                                        );
                                        break Ok(());
                                    }
                                }
                                let Some(status) = shared.update_snapshot(&node_id, session_id, snapshot).await else {
                                    warn!(node_id = %node_id, session_id, "dropping metrics from superseded session");
                                    break Ok(());
                                };
                                state.history.record_status(&status).await;
                            }
                            WireMessage::AgentLogs(AgentLogsMessage { entries }) => {
                                if !state.registry.is_token_current(&node_id, session_generation).await {
                                    warn!(node_id = %node_id, "disconnecting session after registry token change");
                                    break Ok(());
                                }
                                let accepted = state.agent_logs.record_entries(&node_id, entries).await;
                                if accepted > 0 {
                                    info!(node_id = %node_id, accepted, "recorded agent runtime log entries");
                                }
                            }
                            WireMessage::Pong(PongMessage { nonce }) => {
                                if !state.registry.is_token_current(&node_id, session_generation).await {
                                    warn!(node_id = %node_id, "disconnecting session after registry token change");
                                    break Ok(());
                                }
                                let Some(sent_at) = outstanding_pings.remove(&nonce) else {
                                    continue;
                                };
                                let latency_ms = sent_at.elapsed().as_millis() as u64;
                                if !shared.update_latency(&node_id, session_id, latency_ms).await {
                                    warn!(node_id = %node_id, session_id, "dropping pong from superseded session");
                                    break Ok(());
                                }
                            }
                            WireMessage::Hello(_) => {
                                break Err(ProtocolError::Client("duplicate hello message".to_string()));
                            }
                            WireMessage::Ping(_) => {
                                break Err(ProtocolError::Client("agent must not send ping messages".to_string()));
                            }
                            WireMessage::ServerNotice(_) => {
                                break Err(ProtocolError::Client("agent must not send server_notice messages".to_string()));
                            }
                            WireMessage::RefreshTokenRequest(request) => {
                                if !state.registry.is_token_current(&node_id, session_generation).await {
                                    warn!(node_id = %node_id, "disconnecting session after token expiry before refresh");
                                    break Ok(());
                                }
                                // `request.node_id` 完全由客户端控制,我们不应该
                                // 信任它来决定"为谁刷 token"。会话握手期间的认证
                                // 已经把这条连接绑定到 `node_id`,接下来所有的
                                // refresh 都只对它生效。字段保留是为了不破坏旧
                                // Agent 的请求格式;如果客户端发了别的 node_id,
                                // 那要么是 bug 要么是恶意,但都不会得到非本会话
                                // 节点的 token。这里 silently ignore + debug 记录
                                // 即可,而不再像以前那样直接断开连接。
                                if request.node_id != node_id {
                                    warn!(
                                        session_node_id = %node_id,
                                        client_supplied_node_id = %request.node_id,
                                        "ignoring client-supplied node_id in refresh_token_request",
                                    );
                                }
                                match state.registry.refresh_token(&node_id).await {
                                    Ok((new_token, expires_at, new_generation)) => {
                                        let response = WireMessage::RefreshTokenResponse(
                                            nodelite_proto::RefreshTokenResponseMessage {
                                                new_token: new_token.clone(),
                                                expires_at: expires_at.to_rfc3339(),
                                            },
                                        );
                                        let payload = serde_json::to_string(&response)
                                            .map_err(|error| anyhow!("failed to serialize refresh response: {error}"))?;
                                        // 先发送、再替换本地 session_token + generation。理由同
                                        // `maybe_refresh_agent_token`:send 失败时不能
                                        // 让 server 进入一个 agent 看不到的新状态。
                                        sender
                                            .send(Message::Text(payload.into()))
                                            .await
                                            .map_err(|error| anyhow!("failed to send refresh response: {error}"))?;
                                        session_token = new_token;
                                        session_generation = new_generation;
                                        info!(node_id = %node_id, "token refreshed successfully");
                                    }
                                    Err(error) => {
                                        warn!(node_id = %node_id, error = ?error, "failed to refresh token");
                                        let notice = WireMessage::ServerNotice(ServerNoticeMessage {
                                            level: nodelite_proto::NoticeLevel::Error,
                                            message: "Failed to refresh token".to_string(),
                                        });
                                        let payload = serde_json::to_string(&notice)
                                            .map_err(|error| anyhow!("failed to serialize notice: {error}"))?;
                                        sender
                                            .send(Message::Text(payload.into()))
                                            .await
                                            .map_err(|error| anyhow!("failed to send notice: {error}"))?;
                                    }
                                }
                            }
                            WireMessage::RefreshTokenResponse(_) => {
                                break Err(ProtocolError::Client("agent must not send refresh_token_response messages".to_string()));
                            }
                        },
                    }
                }
                command = session_control_rx.recv() => {
                    let Some(command) = command else {
                        break Ok(());
                    };
                    match command {
                        SessionCommand::RefreshToken { response } => {
                            match refresh_session_token(
                                &mut sender,
                                &state.registry,
                                &node_id,
                                &mut session_token,
                                &mut session_generation,
                                "manual",
                            )
                            .await
                            {
                                Ok(expires_at) => {
                                    let _ = response.send(Ok(SessionRefreshReply {
                                        token_expires_at: expires_at,
                                    }));
                                }
                                Err(error) => {
                                    let message = error.to_string();
                                    let _ = response.send(Err(message));
                                    break Err(ProtocolError::Server(error));
                                }
                            }
                        }
                    }
                }
                _ = ping_ticker.tick() => {
                    if !shared.is_current_session(&node_id, session_id).await {
                        warn!(node_id = %node_id, session_id, "closing superseded websocket session");
                        break Ok(());
                    }
                    if !state.registry.is_token_current(&node_id, session_generation).await {
                        warn!(node_id = %node_id, "closing websocket session after registry token change");
                        break Ok(());
                    }
                    if should_refresh_agent_token(&state.registry, &node_id).await? {
                        refresh_session_token(
                            &mut sender,
                            &state.registry,
                            &node_id,
                            &mut session_token,
                            &mut session_generation,
                            "pre-expiry",
                        )
                        .await?;
                    }

                    prune_outstanding_pings(&mut outstanding_pings, ping_expiry);
                    let nonce = next_ping_nonce;
                    next_ping_nonce = next_ping_nonce.saturating_add(1);
                    outstanding_pings.insert(nonce, Instant::now());
                    let ping = encode_ping_message(nonce);
                    sender
                        .send(Message::Text(ping.into()))
                        .await
                        .map_err(|error| anyhow!("failed to send ping: {error}"))?;
                }
            }
        }
    }
    .await;

    shared.mark_disconnected(&node_id, session_id).await;
    info!(node_id = %node_id, session_id, "node disconnected");
    session_result
}

async fn should_refresh_agent_token(registry: &NodeRegistry, node_id: &str) -> Result<bool> {
    let refresh_after = Utc::now() + ChronoDuration::days(AGENT_TOKEN_REFRESH_BEFORE_EXPIRY_DAYS);
    Ok(registry
        .token_expires_at(node_id)
        .await
        .is_none_or(|expires_at| expires_at <= refresh_after))
}

/// 通过当前在线会话把新 token 下发给 Agent。
///
/// 关键顺序:
/// 1. registry 先刷新并持久化新 token;
/// 2. 只有成功把 `refresh_token_response` 推到 TCP 缓冲区后,才更新本地
///    `session_token`;
///    这样 send 失败时当前 session 会立刻结束,避免 server 内存和 agent 视图
///    长时间不一致。
async fn refresh_session_token(
    sender: &mut futures::stream::SplitSink<WebSocket, Message>,
    registry: &NodeRegistry,
    node_id: &str,
    session_token: &mut String,
    session_generation: &mut u64,
    trigger: &str,
) -> Result<DateTime<Utc>> {
    let (new_token, expires_at, new_generation) = registry.refresh_token(node_id).await?;
    info!(
        node_id = %node_id,
        trigger,
        expires_at = %expires_at.to_rfc3339(),
        generation = new_generation,
        "refreshed agent token",
    );
    let response = WireMessage::RefreshTokenResponse(nodelite_proto::RefreshTokenResponseMessage {
        new_token: new_token.clone(),
        expires_at: expires_at.to_rfc3339(),
    });
    let payload = serde_json::to_string(&response)
        .map_err(|error| anyhow!("failed to serialize token refresh response: {error}"))?;
    sender
        .send(Message::Text(payload.into()))
        .await
        .map_err(|error| anyhow!("failed to send token refresh response: {error}"))?;
    *session_token = new_token;
    *session_generation = new_generation;
    Ok(expires_at)
}

/// 阻塞接收 Hello 帧;期间收到的 Ping/Pong 等控制帧会被忽略,其他业务帧视为协议错误。
async fn recv_hello(socket: &mut WebSocket) -> Result<HelloMessage, ProtocolError> {
    loop {
        let Some(message) = socket
            .recv()
            .await
            .transpose()
            .map_err(|error| anyhow!("failed to receive hello: {error}"))?
        else {
            return Err(ProtocolError::Client(
                "connection closed before hello message".to_string(),
            ));
        };

        match parse_wire_message(message)? {
            ParsedFrame::Control => continue,
            ParsedFrame::Wire(message) => match *message {
                WireMessage::Hello(hello) => return Ok(hello),
                _ => {
                    return Err(ProtocolError::Client(
                        "first websocket message must be hello".to_string(),
                    ));
                }
            },
            ParsedFrame::Close => {
                return Err(ProtocolError::Client(
                    "connection closed before hello message".to_string(),
                ));
            }
        }
    }
}

/// 解析底层 WebSocket 帧,把它归类为业务消息 / 控制帧 / 关闭。
fn parse_wire_message(message: Message) -> Result<ParsedFrame, ProtocolError> {
    match message {
        Message::Text(text) => serde_json::from_str::<WireMessage>(&text)
            .map(Box::new)
            .map(ParsedFrame::Wire)
            .map_err(|error| ProtocolError::Client(format!("invalid websocket json: {error}"))),
        Message::Binary(_) => Err(ProtocolError::Client(
            "binary websocket messages are not supported".to_string(),
        )),
        Message::Close(_) => Ok(ParsedFrame::Close),
        Message::Ping(_) | Message::Pong(_) => Ok(ParsedFrame::Control),
    }
}

/// 把 `WireMessage` 序列化为 JSON 文本帧后发送。
async fn send_wire_message(
    socket: &mut WebSocket,
    message: &WireMessage,
) -> Result<(), ProtocolError> {
    let payload = serde_json::to_string(message)
        .map_err(|error| anyhow!("failed to serialize websocket message: {error}"))?;
    socket
        .send(Message::Text(payload.into()))
        .await
        .map_err(|error| anyhow!("failed to send websocket message: {error}"))?;
    Ok(())
}

/// 主动断开未完成握手的连接:给客户端一个明确的 close code,
/// 而不是直接 drop socket(会被 agent 当成网络异常并立即重连)。
async fn send_close_frame(
    socket: &mut WebSocket,
    code: u16,
    reason: &'static str,
) -> Result<(), anyhow::Error> {
    socket
        .send(Message::Close(Some(CloseFrame {
            code,
            reason: reason.into(),
        })))
        .await
        .map_err(|error| anyhow!("failed to send close frame: {error}"))?;
    Ok(())
}

/// 清理"过期或过多"的 Ping 记录,避免在 Agent 异常时无限制堆积。
fn prune_outstanding_pings(outstanding_pings: &mut HashMap<u64, Instant>, max_age: Duration) {
    outstanding_pings.retain(|_, sent_at| sent_at.elapsed() < max_age);

    if outstanding_pings.len() < MAX_OUTSTANDING_PINGS {
        return;
    }

    if let Some(oldest_nonce) = outstanding_pings
        .iter()
        .min_by_key(|(_, sent_at)| *sent_at)
        .map(|(nonce, _)| *nonce)
    {
        outstanding_pings.remove(&oldest_nonce);
    }
}

/// Ping 是高频热路径,协议结构又极其固定,这里直接拼接 JSON 文本以避免
/// 每个心跳都走一次 `serde_json::to_string`。
fn encode_ping_message(nonce: u64) -> String {
    format!(r#"{{"type":"ping","nonce":{nonce}}}"#)
}

#[cfg(test)]
mod tests {
    use super::encode_ping_message;

    #[test]
    fn encode_ping_message_matches_wire_protocol_shape() {
        assert_eq!(encode_ping_message(42), r#"{"type":"ping","nonce":42}"#);
    }
}
