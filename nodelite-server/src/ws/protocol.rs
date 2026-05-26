//! WebSocket 帧解析与协议层收发辅助。

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use axum::extract::ws::{CloseFrame, Message, WebSocket};
use nodelite_proto::{HelloMessage, PingMessage, WireMessage};

/// WebSocket 处理流程中的错误来源区分:
/// `Client` 表示因对方原因(协议错误、未认证)而断开,只记 warn;
/// `Server` 表示我们这边出现异常,记 error。
#[derive(Debug)]
pub(crate) enum ProtocolError {
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
pub(crate) enum ParsedFrame {
    Wire(Box<WireMessage>),
    Control,
    Close,
}

/// 阻塞接收 Hello 帧;期间收到的 Ping/Pong 等控制帧会被忽略,其他业务帧视为协议错误。
pub(crate) async fn recv_hello(socket: &mut WebSocket) -> Result<HelloMessage, ProtocolError> {
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
pub(crate) fn parse_wire_message(message: Message) -> Result<ParsedFrame, ProtocolError> {
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
pub(crate) async fn send_wire_message(
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
pub(crate) async fn send_close_frame(
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
pub(crate) fn prune_outstanding_pings(
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

pub(crate) fn encode_ping_message(nonce: u64) -> Result<String, ProtocolError> {
    serde_json::to_string(&WireMessage::Ping(PingMessage { nonce }))
        .map_err(|error| anyhow!("failed to serialize ping message: {error}").into())
}
