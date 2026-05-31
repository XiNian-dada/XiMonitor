use std::net::SocketAddr;

use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::{AppendHeaders, IntoResponse, Response};
use axum::{Json, extract::Request};
use serde_json::json;
use tracing::error;

use super::record_audit_event;
use crate::AppState;
use crate::admission::resolve_client_ip;
use crate::audit::{AuditEventType, NewAuditEvent};
use crate::auth::{
    TWO_FACTOR_AUTH_COOKIE, TWO_FACTOR_AUTH_SECS, TWO_FACTOR_PENDING_COOKIE,
    TWO_FACTOR_PENDING_SECS, Verify2FAError, Verify2FARequest, auth_cookie, cookie_value,
    expire_cookie, secure_cookies, verify_totp_step,
};

type Verify2FAResult<T> = Result<T, (StatusCode, Json<Verify2FAError>)>;

struct ReadonlyAuthMeta {
    request_path: String,
    sensitive_path: bool,
    client_ip: std::net::IpAddr,
    audit_ip: String,
    audit_user_agent: Option<String>,
}

/// 2FA 验证 API:验证 TOTP 码,成功后设置完整认证 cookie。
pub(crate) async fn verify_2fa_api(
    State(state): State<AppState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<Verify2FARequest>,
) -> Verify2FAResult<Response> {
    let client_ip = resolve_client_ip(&state.shared.config().trusted_proxies, peer_addr, &headers);
    ensure_verify_2fa_not_blocked(&state, &headers, client_ip).await?;
    let pending_token = require_pending_token(&state, &headers, client_ip).await?;
    let Some(totp_step) = current_unused_totp_step(&state, &request.code).await else {
        return Ok(handle_invalid_totp(&state, &headers, client_ip, &pending_token).await);
    };
    state.two_factor_sessions.mark_totp_step_used(totp_step);

    let audit_user = readonly_auth_username(&state).await;
    let auth_token = create_authenticated_two_factor_session(&state)?;
    state.two_factor_sessions.consume_pending(&pending_token);
    state.verify_2fa_admission.clear_auth_failures(client_ip);
    record_totp_success(&state, &headers, client_ip, audit_user).await;

    Ok(successful_verify_2fa_response(&state, &auth_token))
}

/// 健康检查接口,始终返回 200。
pub(crate) async fn healthz() -> StatusCode {
    StatusCode::OK
}

/// 就绪检查接口:仅当关键依赖均可用时返回 200,否则返回 503。
pub(crate) async fn readyz(State(state): State<AppState>) -> StatusCode {
    if state.readiness.is_ready() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

/// 登出并强制重新认证:返回 401 + WWW-Authenticate 头,触发浏览器清除缓存的
/// Basic Auth 凭据。前端在检测到认证过期(24 小时)时会跳转到此路由。
pub(crate) async fn logout_and_reauth(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Some(token) = cookie_value(&headers, TWO_FACTOR_AUTH_COOKIE) {
        state.two_factor_sessions.remove_authenticated(&token);
    }
    let secure = secure_cookies(state.shared.config());
    (
        StatusCode::UNAUTHORIZED,
        AppendHeaders([
            expire_cookie(TWO_FACTOR_AUTH_COOKIE, secure),
            expire_cookie(TWO_FACTOR_PENDING_COOKIE, secure),
            (
                header::WWW_AUTHENTICATE,
                "Basic realm=\"NodeLite\"".to_string(),
            ),
        ]),
        "Session expired. Please log in again.",
    )
        .into_response()
}

/// 中间件:对受保护路由强制基本认证;放行时把 Request 继续交给下一个处理器。
pub(crate) async fn require_readonly_auth(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Response {
    let meta = readonly_auth_meta(&state, &headers, &request);
    let auth = state.readonly_auth.read().await;
    let evaluated = evaluate_readonly_auth(&state, &auth, &headers, &request);
    drop(auth);
    let Some((basic_authorized, two_factor_enabled)) = evaluated else {
        return next.run(request).await;
    };
    if let Some(response) = enforce_readonly_auth_limits(&state, &meta).await {
        return response;
    }
    if basic_authorized {
        clear_readonly_auth_failures(&state, &meta);
        if two_factor_enabled {
            // WebSocket 升级握手无法跟随 302 重定向(浏览器 `new WebSocket()` 不暴露
            // redirect),返回 401 + JSON。WS 客户端据此 fallback 到一次 `/api/bootstrap`
            // 探测,由统一 api 客户端完成到 /verify-2fa 的真实跳转。
            if is_websocket_upgrade(&headers) {
                return websocket_two_factor_required_response();
            }
            return issue_two_factor_redirect(&state).await;
        }
        return next.run(request).await;
    }

    record_readonly_login_failure(
        &state,
        &meta,
        headers.contains_key(header::AUTHORIZATION),
        two_factor_enabled,
    )
    .await;
    readonly_auth_unauthorized_response()
}

async fn ensure_verify_2fa_not_blocked(
    state: &AppState,
    headers: &HeaderMap,
    client_ip: std::net::IpAddr,
) -> Verify2FAResult<()> {
    let Err(retry_after_secs) = state.verify_2fa_admission.check(client_ip) else {
        return Ok(());
    };
    record_audit_event(
        state,
        AuditEventType::RateLimitExceeded,
        client_ip.to_string(),
        false,
        user_agent(headers),
        json!({
            "endpoint": "/api/verify-2fa",
            "retry_after_secs": retry_after_secs,
            "reason": "verify_2fa_block",
        }),
    )
    .await;
    Err((
        StatusCode::TOO_MANY_REQUESTS,
        Json(Verify2FAError {
            error: format!("Too many failed attempts; retry after {retry_after_secs}s"),
        }),
    ))
}

async fn require_pending_token(
    state: &AppState,
    headers: &HeaderMap,
    client_ip: std::net::IpAddr,
) -> Verify2FAResult<String> {
    let Some(pending_token) = cookie_value(headers, TWO_FACTOR_PENDING_COOKIE) else {
        record_totp_failure(
            state,
            headers,
            client_ip,
            json!({ "reason": "missing_pending_token" }),
        )
        .await;
        return Err(verify_2fa_unauthorized_error());
    };
    if !state.two_factor_sessions.pending_exists(&pending_token) {
        record_totp_failure(
            state,
            headers,
            client_ip,
            json!({ "reason": "unknown_pending_token" }),
        )
        .await;
        return Err(verify_2fa_unauthorized_error());
    }
    Ok(pending_token)
}

async fn current_unused_totp_step(state: &AppState, code: &str) -> Option<u64> {
    let totp_secret = {
        let auth = state.readonly_auth.read().await;
        auth.totp_secret.clone()
    };
    verify_totp_step(totp_secret.as_deref(), code)
        .filter(|step| !state.two_factor_sessions.is_totp_step_used(*step))
}

async fn handle_invalid_totp(
    state: &AppState,
    headers: &HeaderMap,
    client_ip: std::net::IpAddr,
    pending_token: &str,
) -> Response {
    let pending_invalidated = state
        .two_factor_sessions
        .record_failed_attempt(pending_token);
    record_totp_failure(
        state,
        headers,
        client_ip,
        json!({
            "reason": "invalid_or_replayed_totp",
            "pending_invalidated": pending_invalidated,
        }),
    )
    .await;
    let secure = secure_cookies(state.shared.config());
    let body = Json(Verify2FAError {
        error: "Verification failed".to_string(),
    });
    if pending_invalidated {
        (
            StatusCode::UNAUTHORIZED,
            AppendHeaders([expire_cookie(TWO_FACTOR_PENDING_COOKIE, secure)]),
            body,
        )
            .into_response()
    } else {
        (StatusCode::UNAUTHORIZED, body).into_response()
    }
}

async fn record_totp_failure(
    state: &AppState,
    headers: &HeaderMap,
    client_ip: std::net::IpAddr,
    details: serde_json::Value,
) {
    state.verify_2fa_admission.record_auth_failure(client_ip);
    record_audit_event(
        state,
        AuditEventType::TotpVerifyFailure,
        client_ip.to_string(),
        false,
        user_agent(headers),
        details,
    )
    .await;
}

async fn readonly_auth_username(state: &AppState) -> Option<String> {
    let auth = state.readonly_auth.read().await;
    auth.config.as_ref().map(|config| config.username.clone())
}

fn create_authenticated_two_factor_session(state: &AppState) -> Verify2FAResult<String> {
    state
        .two_factor_sessions
        .create_authenticated()
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(Verify2FAError {
                    error: "Failed to create authenticated session".to_string(),
                }),
            )
        })
}

async fn record_totp_success(
    state: &AppState,
    headers: &HeaderMap,
    client_ip: std::net::IpAddr,
    audit_user: Option<String>,
) {
    let mut event = NewAuditEvent::now(
        AuditEventType::TotpVerifySuccess,
        client_ip.to_string(),
        true,
    );
    event.user = audit_user;
    event.user_agent = user_agent(headers);
    event.details = json!({
        "endpoint": "/api/verify-2fa",
    });
    state.audit_log.record_best_effort(event).await;
}

fn successful_verify_2fa_response(state: &AppState, auth_token: &str) -> Response {
    let secure = secure_cookies(state.shared.config());
    (
        StatusCode::OK,
        AppendHeaders([
            auth_cookie(
                TWO_FACTOR_AUTH_COOKIE,
                auth_token,
                TWO_FACTOR_AUTH_SECS,
                secure,
            ),
            expire_cookie(TWO_FACTOR_PENDING_COOKIE, secure),
        ]),
    )
        .into_response()
}

fn verify_2fa_unauthorized_error() -> (StatusCode, Json<Verify2FAError>) {
    (
        StatusCode::UNAUTHORIZED,
        Json(Verify2FAError {
            error: "Verification failed".to_string(),
        }),
    )
}

fn readonly_auth_meta(
    state: &AppState,
    headers: &HeaderMap,
    request: &Request,
) -> ReadonlyAuthMeta {
    let request_path = request.uri().path().to_string();
    let sensitive_path = is_sensitive_readonly_path(&request_path, request);
    let client_ip = request_client_ip(state, headers, request);
    ReadonlyAuthMeta {
        request_path,
        sensitive_path,
        audit_ip: client_ip.to_string(),
        audit_user_agent: user_agent(headers),
        client_ip,
    }
}

fn evaluate_readonly_auth(
    state: &AppState,
    auth: &crate::auth::ReadonlyRouteAuth,
    headers: &HeaderMap,
    request: &Request,
) -> Option<(bool, bool)> {
    auth.expected_authorization.as_ref()?;
    if auth.enable_2fa && has_authenticated_two_factor_cookie(state, headers) {
        return None;
    }
    Some((auth.is_authorized(request), auth.enable_2fa))
}

fn has_authenticated_two_factor_cookie(state: &AppState, headers: &HeaderMap) -> bool {
    cookie_value(headers, TWO_FACTOR_AUTH_COOKIE)
        .as_deref()
        .is_some_and(|token| state.two_factor_sessions.is_authenticated(token))
}

async fn enforce_readonly_auth_limits(
    state: &AppState,
    meta: &ReadonlyAuthMeta,
) -> Option<Response> {
    if let Err(retry_after_secs) = state.readonly_auth_admission.check(meta.client_ip) {
        record_readonly_auth_block(
            state,
            &meta.audit_ip,
            &meta.audit_user_agent,
            &meta.request_path,
            meta.sensitive_path,
            retry_after_secs,
        )
        .await;
        return Some(readonly_auth_block_response(retry_after_secs));
    }
    if meta.sensitive_path
        && let Err(retry_after_secs) = state
            .sensitive_readonly_auth_admission
            .check(meta.client_ip)
    {
        record_readonly_auth_block(
            state,
            &meta.audit_ip,
            &meta.audit_user_agent,
            &meta.request_path,
            true,
            retry_after_secs,
        )
        .await;
        return Some(readonly_auth_block_response(retry_after_secs));
    }
    None
}

fn clear_readonly_auth_failures(state: &AppState, meta: &ReadonlyAuthMeta) {
    state
        .readonly_auth_admission
        .clear_auth_failures(meta.client_ip);
    if meta.sensitive_path {
        state
            .sensitive_readonly_auth_admission
            .clear_auth_failures(meta.client_ip);
    }
}

/// 判断请求是否为 WebSocket 升级握手。依据 `Upgrade` 头是否包含 `websocket`
/// token(大小写不敏感、容忍逗号分隔),与 axum `WebSocketUpgrade` 的识别一致。
fn is_websocket_upgrade(headers: &HeaderMap) -> bool {
    headers
        .get(header::UPGRADE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .any(|token| token.trim().eq_ignore_ascii_case("websocket"))
        })
}

/// 受保护的 WebSocket 端点要求 2FA 时的响应。浏览器 `new WebSocket()` 无法跟随
/// HTTP 302,因此返回 401 + JSON 而非重定向;WS 客户端据此跳转 /verify-2fa。
fn websocket_two_factor_required_response() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({
            "ok": false,
            "message": "two_factor_required",
            "endpoint": "/verify-2fa",
        })),
    )
        .into_response()
}

async fn issue_two_factor_redirect(state: &AppState) -> Response {
    let pending_token = match state.two_factor_sessions.create_pending() {
        Ok(token) => token,
        Err(error) => {
            error!(error = ?error, "failed to create pending 2FA session");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let secure = secure_cookies(state.shared.config());
    (
        StatusCode::FOUND,
        AppendHeaders([
            auth_cookie(
                TWO_FACTOR_PENDING_COOKIE,
                &pending_token,
                TWO_FACTOR_PENDING_SECS,
                secure,
            ),
            expire_cookie(TWO_FACTOR_AUTH_COOKIE, secure),
            (header::LOCATION, "/verify-2fa".to_string()),
        ]),
    )
        .into_response()
}

async fn record_readonly_login_failure(
    state: &AppState,
    meta: &ReadonlyAuthMeta,
    has_authorization_header: bool,
    two_factor_enabled: bool,
) {
    state
        .readonly_auth_admission
        .record_auth_failure(meta.client_ip);
    if meta.sensitive_path {
        state
            .sensitive_readonly_auth_admission
            .record_auth_failure(meta.client_ip);
    }
    record_audit_event(
        state,
        AuditEventType::LoginFailure,
        meta.audit_ip.clone(),
        false,
        meta.audit_user_agent.clone(),
        json!({
            "path": meta.request_path,
            "reason": if has_authorization_header {
                "invalid_basic_auth"
            } else {
                "missing_basic_auth"
            },
            "two_factor_enabled": two_factor_enabled,
            "sensitive_path": meta.sensitive_path,
        }),
    )
    .await;
}

fn readonly_auth_unauthorized_response() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Basic realm=\"NodeLite\"")],
        "authentication required",
    )
        .into_response()
}

fn is_sensitive_readonly_path(path: &str, request: &Request) -> bool {
    request.method() != axum::http::Method::GET && path.starts_with("/api/settings/")
}

async fn record_readonly_auth_block(
    state: &AppState,
    audit_ip: &str,
    audit_user_agent: &Option<String>,
    request_path: &str,
    sensitive_path: bool,
    retry_after_secs: u64,
) {
    record_audit_event(
        state,
        AuditEventType::RateLimitExceeded,
        audit_ip.to_string(),
        false,
        audit_user_agent.clone(),
        json!({
            "path": request_path,
            "reason": "readonly_auth_block",
            "retry_after_secs": retry_after_secs,
            "sensitive_path": sensitive_path,
        }),
    )
    .await;
}

fn readonly_auth_block_response(retry_after_secs: u64) -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(header::RETRY_AFTER, retry_after_secs.to_string())],
        "too many recent authentication failures",
    )
        .into_response()
}

fn user_agent(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

fn request_client_ip(state: &AppState, headers: &HeaderMap, request: &Request) -> std::net::IpAddr {
    request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map_or(
            std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED),
            |connect_info| {
                resolve_client_ip(
                    &state.shared.config().trusted_proxies,
                    connect_info.0,
                    headers,
                )
            },
        )
}
