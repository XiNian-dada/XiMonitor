//! 在线 token 有效性检查与续期逻辑。

use anyhow::{Result, anyhow};
use axum::extract::ws::{Message, WebSocket};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use futures::SinkExt;
use tracing::{info, warn};

use super::ActiveSession;
use crate::AppState;
use crate::registry::NodeRegistry;

/// Token 距离过期不足该天数时,服务端在已认证会话内主动续期并下发新 token。
pub(crate) const AGENT_TOKEN_REFRESH_BEFORE_EXPIRY_DAYS: i64 = 7;

pub(crate) async fn handle_refresh_request(
    state: &AppState,
    session: &mut ActiveSession,
    sender: &mut futures::stream::SplitSink<WebSocket, Message>,
    request: nodelite_proto::RefreshTokenRequestMessage,
) -> Result<super::LoopAction, super::ProtocolError> {
    if !ensure_current_token(
        state,
        session,
        "disconnecting session after token expiry before refresh",
    )
    .await
    {
        return Ok(super::LoopAction::Break);
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
            let response = nodelite_proto::WireMessage::RefreshTokenResponse(
                nodelite_proto::RefreshTokenResponseMessage {
                    new_token: new_token.clone(),
                    expires_at: expires_at.to_rfc3339(),
                },
            );
            let payload = serde_json::to_string(&response)
                .map_err(|error| anyhow!("failed to serialize refresh response: {error}"))?;
            sender
                .send(Message::Text(payload.into()))
                .await
                .map_err(|error| anyhow!("failed to send refresh response: {error}"))?;
            session.session_token = new_token;
            session.session_generation = new_generation;
            session.token_expires_at = Some(expires_at);
            session.registry_revision = state.registry.registry_revision();
            info!(node_id = %session.node_id, "token refreshed successfully");
        }
        Err(error) => {
            warn!(node_id = %session.node_id, error = ?error, "failed to refresh token");
            let notice =
                nodelite_proto::WireMessage::ServerNotice(nodelite_proto::ServerNoticeMessage {
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
    Ok(super::LoopAction::Continue)
}

pub(crate) async fn ensure_current_token(
    state: &AppState,
    session: &mut ActiveSession,
    log_message: &str,
) -> bool {
    if session.registry_revision == state.registry.registry_revision()
        && session
            .token_expires_at
            .map(|expires_at| Utc::now() < expires_at)
            .unwrap_or(true)
    {
        return true;
    }

    let Some(status) = state.registry.token_status(&session.node_id).await else {
        warn!(node_id = %session.node_id, "{log_message}");
        return false;
    };
    if status.generation == session.session_generation {
        session.token_expires_at = status.token_expires_at;
        session.registry_revision = status.registry_revision;
        return true;
    }

    warn!(node_id = %session.node_id, "{log_message}");
    false
}

pub(crate) async fn should_refresh_agent_token(
    registry: &NodeRegistry,
    session: &mut ActiveSession,
) -> Result<bool> {
    let refresh_after = Utc::now() + ChronoDuration::days(AGENT_TOKEN_REFRESH_BEFORE_EXPIRY_DAYS);
    if session.registry_revision == registry.registry_revision() {
        return Ok(session
            .token_expires_at
            .map(|expires_at| expires_at <= refresh_after)
            .unwrap_or(true));
    }

    let Some(status) = registry.token_status(&session.node_id).await else {
        return Ok(true);
    };
    if status.generation != session.session_generation {
        return Ok(false);
    }

    session.token_expires_at = status.token_expires_at;
    session.registry_revision = status.registry_revision;
    Ok(session
        .token_expires_at
        .map(|expires_at| expires_at <= refresh_after)
        .unwrap_or(true))
}

/// 通过当前在线会话把新 token 下发给 Agent。
///
/// 关键顺序:
/// 1. registry 先刷新并持久化新 token;
/// 2. 只有成功把 `refresh_token_response` 推到 TCP 缓冲区后,才更新本地
///    `session_token`;
///    这样 send 失败时当前 session 会立刻结束,避免 server 内存和 agent 视图
///    长时间不一致。
pub(crate) async fn refresh_session_token(
    sender: &mut futures::stream::SplitSink<WebSocket, Message>,
    registry: &NodeRegistry,
    session: &mut ActiveSession,
    trigger: &str,
) -> Result<DateTime<Utc>> {
    let node_id = session.node_id.clone();
    let (new_token, expires_at, new_generation) = registry.refresh_token(&node_id).await?;
    info!(
        node_id = %session.node_id,
        trigger,
        expires_at = %expires_at.to_rfc3339(),
        generation = new_generation,
        "refreshed agent token",
    );
    let response = nodelite_proto::WireMessage::RefreshTokenResponse(
        nodelite_proto::RefreshTokenResponseMessage {
            new_token: new_token.clone(),
            expires_at: expires_at.to_rfc3339(),
        },
    );
    let payload = serde_json::to_string(&response)
        .map_err(|error| anyhow!("failed to serialize token refresh response: {error}"))?;
    sender
        .send(Message::Text(payload.into()))
        .await
        .map_err(|error| anyhow!("failed to send token refresh response: {error}"))?;
    session.session_token = new_token;
    session.session_generation = new_generation;
    session.token_expires_at = Some(expires_at);
    session.registry_revision = registry.registry_revision();
    Ok(expires_at)
}
