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
    AgentLogsMessage, HelloMessage, MetricsMessage, PingMessage, PongMessage, ServerNoticeMessage,
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

struct ActiveSession {
    node_id: String,
    node_label: String,
    session_id: u64,
    session_token: String,
    session_generation: u64,
}

struct SessionLoopState {
    ping_ticker: tokio::time::Interval,
    ping_expiry: Duration,
    outstanding_pings: HashMap<u64, Instant>,
    next_ping_nonce: u64,
    metric_anomaly_window: VecDeque<Instant>,
}

impl SessionLoopState {
    fn new(ping_interval_secs: u64) -> Self {
        let ping_every = Duration::from_secs(ping_interval_secs);
        let ping_expiry = Duration::from_secs(ping_interval_secs.saturating_mul(3));
        let mut ping_ticker = interval(ping_every);
        ping_ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        Self {
            ping_ticker,
            ping_expiry,
            outstanding_pings: HashMap::new(),
            next_ping_nonce: 1,
            metric_anomaly_window: VecDeque::new(),
        }
    }
}

enum LoopAction {
    Continue,
    Break,
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
    let hello = wait_for_hello_message(&state, client_ip, &mut socket).await?;
    let authorized = authorize_hello(&state, client_ip, &mut socket, &hello).await?;
    let identity = authorized.identity;
    let mut session = ActiveSession {
        node_id: identity.node_id.clone(),
        node_label: identity.node_label.clone(),
        session_id: shared
            .register_node(identity, Some(client_ip.to_string()))
            .await,
        session_token: hello.token,
        session_generation: authorized.generation,
    };

    info!(
        node_id = %session.node_id,
        node_label = %session.node_label,
        session_id = session.session_id,
        "node authenticated"
    );

    let session_result = run_authenticated_session(&state, socket, &mut session).await;
    shared
        .mark_disconnected(&session.node_id, session.session_id)
        .await;
    info!(node_id = %session.node_id, session_id = session.session_id, "node disconnected");
    session_result
}

async fn wait_for_hello_message(
    state: &AppState,
    client_ip: IpAddr,
    socket: &mut WebSocket,
) -> Result<HelloMessage, ProtocolError> {
    let shared = state.shared.clone();
    let hello_timeout_secs = shared.config().hello_timeout_secs;
    let shutdown = state.shutdown.clone();

    tokio::select! {
        biased;
        _ = shutdown.cancelled() => {
            let _ = send_close_frame(socket, close_code::AWAY, "server shutting down").await;
            Err(ProtocolError::Client("server shutting down".to_string()))
        }
        outcome = tokio::time::timeout(
            Duration::from_secs(hello_timeout_secs),
            recv_hello(socket),
        ) => match outcome {
            Ok(Ok(hello)) => Ok(hello),
            Ok(Err(error)) => {
                state.ws_admission.record_auth_failure(client_ip);
                Err(error)
            }
            Err(_) => {
                state.ws_admission.record_auth_failure(client_ip);
                Err(ProtocolError::Client(
                    "timed out waiting for hello message".to_string(),
                ))
            }
        },
    }
}

async fn authorize_hello(
    state: &AppState,
    client_ip: IpAddr,
    socket: &mut WebSocket,
    hello: &HelloMessage,
) -> Result<crate::registry::AuthorizedNode, ProtocolError> {
    if hello.protocol_version != WIRE_PROTOCOL_VERSION {
        state.ws_admission.record_auth_failure(client_ip);
        let notice = WireMessage::ServerNotice(ServerNoticeMessage {
            level: nodelite_proto::NoticeLevel::Error,
            message: format!(
                "unsupported protocol version {}; server expects {}",
                hello.protocol_version, WIRE_PROTOCOL_VERSION
            ),
        });
        let _ = send_wire_message(socket, &notice).await;
        return Err(ProtocolError::Client(format!(
            "unsupported protocol version {}; expected {}",
            hello.protocol_version, WIRE_PROTOCOL_VERSION
        )));
    }

    match state
        .registry
        .authorize(&hello.identity, &hello.token)
        .await
    {
        Ok(authorized) => {
            state.ws_admission.clear_auth_failures(client_ip);
            Ok(authorized)
        }
        Err(error) => {
            warn!(
                client_ip = %client_ip,
                requested_node_id = %hello.identity.node_id,
                error = ?error,
                "websocket authentication rejected",
            );
            state.ws_admission.record_auth_failure(client_ip);
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
            let _ = send_wire_message(socket, &notice).await;
            Err(ProtocolError::Client(error_label.to_string()))
        }
    }
}

async fn run_authenticated_session(
    state: &AppState,
    socket: WebSocket,
    session: &mut ActiveSession,
) -> Result<(), ProtocolError> {
    let shared = state.shared.clone();
    let (mut sender, mut receiver) = socket.split();
    let (session_control_tx, mut session_control_rx) = mpsc::unbounded_channel();
    if !shared
        .attach_session_control(&session.node_id, session.session_id, session_control_tx)
        .await
    {
        warn!(
            node_id = %session.node_id,
            session_id = session.session_id,
            "failed to attach control channel for superseded session"
        );
        return Ok(());
    }

    let notice = WireMessage::ServerNotice(ServerNoticeMessage {
        level: nodelite_proto::NoticeLevel::Info,
        message: "authenticated".to_string(),
    });
    let payload = serde_json::to_string(&notice)
        .map_err(|error| anyhow!("failed to serialize authenticated notice: {error}"))?;
    sender
        .send(Message::Text(payload.into()))
        .await
        .map_err(|error| anyhow!("failed to send authenticated notice: {error}"))?;

    let mut loop_state = SessionLoopState::new(shared.config().ping_interval_secs);
    loop {
        tokio::select! {
            biased;
            _ = state.shutdown.cancelled() => {
                info!(node_id = %session.node_id, session_id = session.session_id, "closing websocket session due to server shutdown");
                let _ = sender
                    .send(Message::Close(Some(CloseFrame {
                        code: close_code::AWAY,
                        reason: "server shutting down".into(),
                    })))
                    .await;
                return Ok(());
            }
            incoming = receiver.next() => {
                let Some(frame) = incoming else {
                    return Ok(());
                };
                let frame = frame.map_err(|error| anyhow!("websocket receive failed: {error}"))?;
                if matches!(
                    handle_incoming_frame(state, &shared, session, &mut sender, &mut loop_state, frame).await?,
                    LoopAction::Break
                ) {
                    return Ok(());
                }
            }
            command = session_control_rx.recv() => {
                let Some(command) = command else {
                    return Ok(());
                };
                if matches!(
                    handle_session_command(state, session, &mut sender, command).await?,
                    LoopAction::Break
                ) {
                    return Ok(());
                }
            }
            _ = loop_state.ping_ticker.tick() => {
                if matches!(
                    handle_ping_tick(state, &shared, session, &mut sender, &mut loop_state).await?,
                    LoopAction::Break
                ) {
                    return Ok(());
                }
            }
        }
    }
}

async fn handle_incoming_frame(
    state: &AppState,
    shared: &crate::state::SharedState,
    session: &mut ActiveSession,
    sender: &mut futures::stream::SplitSink<WebSocket, Message>,
    loop_state: &mut SessionLoopState,
    frame: Message,
) -> Result<LoopAction, ProtocolError> {
    match parse_wire_message(frame)? {
        ParsedFrame::Close => Ok(LoopAction::Break),
        ParsedFrame::Control => Ok(LoopAction::Continue),
        ParsedFrame::Wire(message) => {
            handle_wire_message(state, shared, session, sender, loop_state, *message).await
        }
    }
}

async fn handle_wire_message(
    state: &AppState,
    shared: &crate::state::SharedState,
    session: &mut ActiveSession,
    sender: &mut futures::stream::SplitSink<WebSocket, Message>,
    loop_state: &mut SessionLoopState,
    message: WireMessage,
) -> Result<LoopAction, ProtocolError> {
    match message {
        WireMessage::Metrics(MetricsMessage { snapshot }) => {
            handle_metrics_message(state, shared, session, loop_state, snapshot).await
        }
        WireMessage::AgentLogs(AgentLogsMessage { entries }) => {
            handle_agent_logs_message(state, session, entries).await
        }
        WireMessage::Pong(PongMessage { nonce }) => {
            handle_pong_message(shared, state, session, loop_state, nonce).await
        }
        WireMessage::RefreshTokenRequest(request) => {
            handle_refresh_request(state, session, sender, request).await
        }
        WireMessage::Hello(_) => Err(ProtocolError::Client("duplicate hello message".to_string())),
        WireMessage::Ping(_) => Err(ProtocolError::Client(
            "agent must not send ping messages".to_string(),
        )),
        WireMessage::ServerNotice(_) => Err(ProtocolError::Client(
            "agent must not send server_notice messages".to_string(),
        )),
        WireMessage::RefreshTokenResponse(_) => Err(ProtocolError::Client(
            "agent must not send refresh_token_response messages".to_string(),
        )),
    }
}

async fn handle_metrics_message(
    state: &AppState,
    shared: &crate::state::SharedState,
    session: &ActiveSession,
    loop_state: &mut SessionLoopState,
    snapshot: nodelite_proto::NodeSnapshot,
) -> Result<LoopAction, ProtocolError> {
    if !ensure_current_token(
        state,
        session,
        "disconnecting session after registry token change",
    )
    .await
    {
        return Ok(LoopAction::Break);
    }
    let (snapshot, report) = sanitize_snapshot(shared.config(), snapshot);
    if report.modified() {
        update_metric_anomaly_window(
            &mut loop_state.metric_anomaly_window,
            &report,
            Instant::now(),
        );
        warn!(
            node_id = %session.node_id,
            session_id = session.session_id,
            anomalies = report.total(),
            anomaly_window_size = loop_state.metric_anomaly_window.len(),
            "agent reported out-of-range metrics; clamped before persistence",
        );
        if should_disconnect_for_metric_anomalies(&loop_state.metric_anomaly_window) {
            warn!(
                node_id = %session.node_id,
                session_id = session.session_id,
                limit = METRIC_ANOMALY_SESSION_LIMIT,
                window_secs = METRIC_ANOMALY_WINDOW_SECS,
                "disconnecting session after repeated metric anomalies",
            );
            return Ok(LoopAction::Break);
        }
    }
    let Some(status) = shared
        .update_snapshot(&session.node_id, session.session_id, snapshot)
        .await
    else {
        warn!(
            node_id = %session.node_id,
            session_id = session.session_id,
            "dropping metrics from superseded session"
        );
        return Ok(LoopAction::Break);
    };
    state.history.record_status(&status).await;
    Ok(LoopAction::Continue)
}

async fn handle_agent_logs_message(
    state: &AppState,
    session: &ActiveSession,
    entries: Vec<nodelite_proto::AgentLogEntry>,
) -> Result<LoopAction, ProtocolError> {
    if !ensure_current_token(
        state,
        session,
        "disconnecting session after registry token change",
    )
    .await
    {
        return Ok(LoopAction::Break);
    }
    let accepted = state
        .agent_logs
        .record_entries(&session.node_id, entries)
        .await;
    if accepted > 0 {
        info!(node_id = %session.node_id, accepted, "recorded agent runtime log entries");
    }
    Ok(LoopAction::Continue)
}

async fn handle_pong_message(
    shared: &crate::state::SharedState,
    state: &AppState,
    session: &ActiveSession,
    loop_state: &mut SessionLoopState,
    nonce: u64,
) -> Result<LoopAction, ProtocolError> {
    if !ensure_current_token(
        state,
        session,
        "disconnecting session after registry token change",
    )
    .await
    {
        return Ok(LoopAction::Break);
    }
    let Some(sent_at) = loop_state.outstanding_pings.remove(&nonce) else {
        return Ok(LoopAction::Continue);
    };
    let latency_ms = sent_at.elapsed().as_millis() as u64;
    if !shared
        .update_latency(&session.node_id, session.session_id, latency_ms)
        .await
    {
        warn!(
            node_id = %session.node_id,
            session_id = session.session_id,
            "dropping pong from superseded session"
        );
        return Ok(LoopAction::Break);
    }
    Ok(LoopAction::Continue)
}

async fn handle_refresh_request(
    state: &AppState,
    session: &mut ActiveSession,
    sender: &mut futures::stream::SplitSink<WebSocket, Message>,
    request: nodelite_proto::RefreshTokenRequestMessage,
) -> Result<LoopAction, ProtocolError> {
    if !ensure_current_token(
        state,
        session,
        "disconnecting session after token expiry before refresh",
    )
    .await
    {
        return Ok(LoopAction::Break);
    }
    if request.node_id != session.node_id {
        warn!(
            session_node_id = %session.node_id,
            client_supplied_node_id = %request.node_id,
            "ignoring client-supplied node_id in refresh_token_request",
        );
    }
    match state.registry.refresh_token(&session.node_id).await {
        Ok((new_token, expires_at, new_generation)) => {
            let response =
                WireMessage::RefreshTokenResponse(nodelite_proto::RefreshTokenResponseMessage {
                    new_token: new_token.clone(),
                    expires_at: expires_at.to_rfc3339(),
                });
            let payload = serde_json::to_string(&response)
                .map_err(|error| anyhow!("failed to serialize refresh response: {error}"))?;
            sender
                .send(Message::Text(payload.into()))
                .await
                .map_err(|error| anyhow!("failed to send refresh response: {error}"))?;
            session.session_token = new_token;
            session.session_generation = new_generation;
            info!(node_id = %session.node_id, "token refreshed successfully");
        }
        Err(error) => {
            warn!(node_id = %session.node_id, error = ?error, "failed to refresh token");
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
    Ok(LoopAction::Continue)
}

async fn handle_session_command(
    state: &AppState,
    session: &mut ActiveSession,
    sender: &mut futures::stream::SplitSink<WebSocket, Message>,
    command: SessionCommand,
) -> Result<LoopAction, ProtocolError> {
    match command {
        SessionCommand::RefreshToken { response } => {
            match refresh_session_token(
                sender,
                &state.registry,
                &session.node_id,
                &mut session.session_token,
                &mut session.session_generation,
                "manual",
            )
            .await
            {
                Ok(expires_at) => {
                    let _ = response.send(Ok(SessionRefreshReply {
                        token_expires_at: expires_at,
                    }));
                    Ok(LoopAction::Continue)
                }
                Err(error) => {
                    let message = error.to_string();
                    let _ = response.send(Err(message));
                    Err(ProtocolError::Server(error))
                }
            }
        }
    }
}

async fn handle_ping_tick(
    state: &AppState,
    shared: &crate::state::SharedState,
    session: &mut ActiveSession,
    sender: &mut futures::stream::SplitSink<WebSocket, Message>,
    loop_state: &mut SessionLoopState,
) -> Result<LoopAction, ProtocolError> {
    if !shared
        .is_current_session(&session.node_id, session.session_id)
        .await
    {
        warn!(
            node_id = %session.node_id,
            session_id = session.session_id,
            "closing superseded websocket session"
        );
        return Ok(LoopAction::Break);
    }
    if !ensure_current_token(
        state,
        session,
        "closing websocket session after registry token change",
    )
    .await
    {
        return Ok(LoopAction::Break);
    }
    if should_refresh_agent_token(&state.registry, &session.node_id).await? {
        refresh_session_token(
            sender,
            &state.registry,
            &session.node_id,
            &mut session.session_token,
            &mut session.session_generation,
            "pre-expiry",
        )
        .await?;
    }

    prune_outstanding_pings(
        &mut loop_state.outstanding_pings,
        loop_state.ping_expiry,
        shared.config().max_outstanding_pings,
    );
    let nonce = loop_state.next_ping_nonce;
    loop_state.next_ping_nonce = loop_state.next_ping_nonce.saturating_add(1);
    loop_state.outstanding_pings.insert(nonce, Instant::now());
    let ping = encode_ping_message(nonce);
    sender
        .send(Message::Text(ping.into()))
        .await
        .map_err(|error| anyhow!("failed to send ping: {error}"))?;
    Ok(LoopAction::Continue)
}

async fn ensure_current_token(
    state: &AppState,
    session: &ActiveSession,
    log_message: &str,
) -> bool {
    if state
        .registry
        .is_token_current(&session.node_id, session.session_generation)
        .await
    {
        return true;
    }
    warn!(node_id = %session.node_id, "{log_message}");
    false
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
fn prune_outstanding_pings(
    outstanding_pings: &mut HashMap<u64, Instant>,
    max_age: Duration,
    max_outstanding_pings: usize,
) {
    outstanding_pings.retain(|_, sent_at| sent_at.elapsed() < max_age);

    if outstanding_pings.len() < max_outstanding_pings {
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

fn encode_ping_message(nonce: u64) -> String {
    serde_json::to_string(&WireMessage::Ping(PingMessage { nonce }))
        .expect("ping serialization should not fail")
}

#[cfg(test)]
mod tests {
    use super::encode_ping_message;

    #[test]
    fn encode_ping_message_matches_wire_protocol_shape() {
        assert_eq!(encode_ping_message(42), r#"{"type":"ping","nonce":42}"#);
    }
}
