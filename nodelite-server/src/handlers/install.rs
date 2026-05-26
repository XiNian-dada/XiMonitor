use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::json;
use tracing::error;

use crate::AppState;
use crate::admission::resolve_client_ip;
use crate::audit::{AuditEventType, NewAuditEvent};
use crate::registry::render_agent_config;

const INSTALL_AGENT_SCRIPT: &str = include_str!("../../../scripts/install-agent.sh");
const INSTALL_TOKEN_HEX_LEN: usize = 64;

/// 暴露内置安装脚本,供 `curl | sh` 模式安装 Agent 时下载。
pub(crate) async fn install_agent_script() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/x-shellscript; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store, no-cache, must-revalidate"),
            (header::PRAGMA, "no-cache"),
        ],
        INSTALL_AGENT_SCRIPT,
    )
        .into_response()
}

/// Agent 安装脚本通过 Bearer 安装令牌请求该端点来换取自己的 agent.toml。
pub(crate) async fn install_bootstrap(
    State(state): State<AppState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    request: Request,
) -> Response {
    let client_ip = resolve_client_ip(&state.shared.config().trusted_proxies, peer_addr, &headers);
    let audit_user_agent = request_user_agent(&request);
    if let Err(retry_after_secs) = state.install_admission.check(client_ip) {
        record_install_block(&state, client_ip, &audit_user_agent, retry_after_secs).await;
        return install_blocked_response(retry_after_secs);
    }

    let Some(token) = bearer_token_from_request(&request) else {
        record_install_token_failure(&state, client_ip, &audit_user_agent, "missing_install_token")
            .await;
        return install_unauthorized_response("missing install token");
    };

    if !is_well_formed_install_token(token) {
        record_install_token_failure(
            &state,
            client_ip,
            &audit_user_agent,
            "malformed_install_token",
        )
        .await;
        return install_unauthorized_response("invalid install token");
    }

    let consumed = match state.registry.consume_install_token(token).await {
        Ok(Some(consumed)) => consumed,
        Ok(None) => {
            record_install_token_failure(
                &state,
                client_ip,
                &audit_user_agent,
                "unknown_install_token",
            )
            .await;
            return install_unauthorized_response("invalid install token");
        }
        Err(error) => {
            error!(error = ?error, "failed to consume install token");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                [
                    (header::CACHE_CONTROL, "no-store, no-cache, must-revalidate"),
                    (header::PRAGMA, "no-cache"),
                ],
                "failed to prepare agent bootstrap",
            )
                .into_response();
        }
    };

    state.install_admission.clear_auth_failures(client_ip);
    render_agent_bootstrap_response(&state, consumed)
}

fn render_agent_bootstrap_response(
    state: &AppState,
    consumed: crate::registry::ConsumedInstall,
) -> Response {
    match render_agent_config(
        &state.shared.config().public_base_url,
        &consumed.node,
        &consumed.node_session_token,
    ) {
        Ok(agent_config) => (
            [
                (header::CONTENT_TYPE, "application/toml; charset=utf-8"),
                (header::CACHE_CONTROL, "no-store, no-cache, must-revalidate"),
                (header::PRAGMA, "no-cache"),
            ],
            agent_config,
        )
            .into_response(),
        Err(error) => {
            error!(error = ?error, "failed to render agent bootstrap config");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                [
                    (header::CACHE_CONTROL, "no-store, no-cache, must-revalidate"),
                    (header::PRAGMA, "no-cache"),
                ],
                "failed to render agent bootstrap config",
            )
                .into_response()
        }
    }
}

async fn record_install_block(
    state: &AppState,
    client_ip: std::net::IpAddr,
    audit_user_agent: &Option<String>,
    retry_after_secs: u64,
) {
    let mut event = NewAuditEvent::now(
        AuditEventType::RateLimitExceeded,
        client_ip.to_string(),
        false,
    );
    event.user_agent = audit_user_agent.clone();
    event.details = json!({
        "endpoint": "/install/bootstrap",
        "retry_after_secs": retry_after_secs,
        "reason": "install_auth_block",
    });
    state.audit_log.record_best_effort(event).await;
}

async fn record_install_token_failure(
    state: &AppState,
    client_ip: std::net::IpAddr,
    audit_user_agent: &Option<String>,
    reason: &'static str,
) {
    state.install_admission.record_auth_failure(client_ip);
    let mut event = NewAuditEvent::now(AuditEventType::TokenInvalid, client_ip.to_string(), false);
    event.user_agent = audit_user_agent.clone();
    event.details = json!({
        "endpoint": "/install/bootstrap",
        "reason": reason,
    });
    state.audit_log.record_best_effort(event).await;
}

fn request_user_agent(request: &Request) -> Option<String> {
    request
        .headers()
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

fn install_unauthorized_response(detail: &'static str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [
            (
                header::WWW_AUTHENTICATE,
                "Bearer realm=\"NodeLite Installer\"",
            ),
            (header::CACHE_CONTROL, "no-store, no-cache, must-revalidate"),
            (header::PRAGMA, "no-cache"),
        ],
        detail,
    )
        .into_response()
}

fn install_blocked_response(retry_after_secs: u64) -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [
            (header::RETRY_AFTER, retry_after_secs.to_string()),
            (
                header::CACHE_CONTROL,
                "no-store, no-cache, must-revalidate".to_string(),
            ),
            (header::PRAGMA, "no-cache".to_string()),
        ],
        "too many recent install bootstrap failures",
    )
        .into_response()
}

/// install token 在 [registry.rs](../registry.rs) 中由 `generate_token` 生成,
/// 固定为 32 字节随机数的 lowercase hex。这里的廉价检查会在文件锁前拒绝
/// 明显无效的输入。
pub(crate) fn is_well_formed_install_token(token: &str) -> bool {
    if token.len() != INSTALL_TOKEN_HEX_LEN {
        return false;
    }

    for byte in token.bytes() {
        if !byte.is_ascii_hexdigit() || byte.is_ascii_uppercase() {
            return false;
        }
    }

    true
}

fn bearer_token_from_request(request: &Request) -> Option<&str> {
    request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}
