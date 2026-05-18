//! HTTP 路由处理器:面板页面、只读 JSON API、认证流程与 Agent 安装脚本下发。
//!
//! 本模块包含所有 Axum handler 函数,按功能分为:
//! - 面板页面(`/`、`/nodes/:id`)与静态资源;
//! - 只读 JSON API(`/api/overview`、`/api/nodes/:id/history` 等);
//! - 认证中间件与 2FA 流程;
//! - Agent 安装脚本下发(`/install/install-agent.sh`)与节点 bootstrap。
//!
//! 子模块 `settings` 处理管理面板的配置变更操作(密码修改、2FA 开关、服务端更新)。

use std::fmt::Write;
use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Path as AxumPath, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::{AppendHeaders, Html, IntoResponse, Response};
use axum::{Json, extract::Request};
use chrono::{TimeZone, Utc};
use serde::{Deserialize, Serialize};
use tracing::error;

use crate::AppState;
use crate::admission::resolve_client_ip;
use crate::auth::{
    TWO_FACTOR_AUTH_COOKIE, TWO_FACTOR_AUTH_SECS, TWO_FACTOR_PENDING_COOKIE,
    TWO_FACTOR_PENDING_SECS, Verify2FAError, Verify2FARequest, auth_cookie, cookie_value,
    expire_cookie, secure_cookies, verify_totp_step,
};
use crate::registry::render_agent_config;
use crate::ui::{UI_I18N_JSON, index_html, node_html};
use nodelite_proto::AgentLogEntry;

mod settings;
pub(crate) use settings::{
    change_readonly_password, disable_two_factor, enable_two_factor, refresh_node_token,
    server_update_log, settings, start_server_update, start_two_factor_setup,
};

/// 把 `scripts/install-agent.sh` 在编译期嵌入到二进制内。
const INSTALL_AGENT_SCRIPT: &str = include_str!("../../scripts/install-agent.sh");
const BRAND_LOGO_LIGHT_WEBP: &[u8] = include_bytes!("../../logo/brand-logo-light.webp");
const BRAND_LOGO_DARK_WEBP: &[u8] = include_bytes!("../../logo/brand-logo-dark.webp");
/// 历史接口默认查询窗口(小时)。
const DEFAULT_HISTORY_WINDOW_HOURS: u64 = 24;
/// 历史接口默认返回的样本点数。
const DEFAULT_HISTORY_MAX_POINTS: usize = 480;
/// 历史接口最多返回的样本点数。
const MAX_HISTORY_MAX_POINTS: usize = 1440;
/// Prometheus exposition format 的 content-type。
const PROMETHEUS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";
/// 节点日志接口默认返回条数。
const DEFAULT_NODE_LOG_LIMIT: usize = 120;
/// 节点日志接口最多返回条数。
const MAX_NODE_LOG_LIMIT: usize = 200;

/// `/api/bootstrap` 的响应结构,只读、用于前端启动期获取基本元数据。
#[derive(Debug, Serialize)]
struct BootstrapResponse {
    service: &'static str,
    status: &'static str,
    ready: bool,
    history_available: bool,
    public_base_url: String,
    refresh_interval_secs: u64,
    registered_nodes: usize,
}

/// 历史接口查询参数。默认查询最近 24 小时,也可用 start/end 指定 unix 秒级区间。
#[derive(Debug, Deserialize)]
pub(crate) struct HistoryQuery {
    window_hours: Option<u64>,
    max_points: Option<usize>,
    start: Option<i64>,
    end: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct NodeLogsQuery {
    limit: Option<usize>,
}

/// 首页 HTML:把刷新周期等参数注入模板。
pub(crate) async fn index(State(state): State<AppState>) -> Html<&'static str> {
    Html(index_html(state.shared.config().refresh_interval_secs))
}

/// 节点详情页 HTML。
pub(crate) async fn node_detail(
    State(state): State<AppState>,
    AxumPath(node_id): AxumPath<String>,
) -> Html<String> {
    Html(node_html(
        &node_id,
        state.shared.config().refresh_interval_secs,
    ))
}

/// 把前端 i18n 字典作为静态 JSON 文件提供。
pub(crate) async fn ui_i18n_asset() -> Response {
    (
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        UI_I18N_JSON,
    )
        .into_response()
}

pub(crate) async fn brand_logo_light_asset() -> Response {
    webp_asset(BRAND_LOGO_LIGHT_WEBP)
}

pub(crate) async fn brand_logo_dark_asset() -> Response {
    webp_asset(BRAND_LOGO_DARK_WEBP)
}

/// 2FA 验证页面。
pub(crate) async fn verify_2fa_page() -> Html<&'static str> {
    Html(include_str!("../assets/verify-2fa.html"))
}

fn webp_asset(bytes: &'static [u8]) -> Response {
    (
        [
            (header::CONTENT_TYPE, "image/webp"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        bytes,
    )
        .into_response()
}

/// 2FA 验证 API:验证 TOTP 码,成功后设置完整认证 cookie。
pub(crate) async fn verify_2fa_api(
    State(state): State<AppState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<Verify2FARequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Verify2FAError>)> {
    let client_ip = resolve_client_ip(state.shared.config().listen, peer_addr, &headers);
    if let Err(retry_after_secs) = state.verify_2fa_admission.check(client_ip) {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            Json(Verify2FAError {
                error: format!("Too many failed attempts; retry after {retry_after_secs}s"),
            }),
        ));
    }

    let Some(pending_token) = cookie_value(&headers, TWO_FACTOR_PENDING_COOKIE) else {
        state.verify_2fa_admission.record_auth_failure(client_ip);
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(Verify2FAError {
                error: "Verification failed".to_string(),
            }),
        ));
    };

    if !state.two_factor_sessions.pending_exists(&pending_token) {
        state.verify_2fa_admission.record_auth_failure(client_ip);
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(Verify2FAError {
                error: "Verification failed".to_string(),
            }),
        ));
    }

    // 验证 TOTP 码并解析出匹配到的 time_step
    let totp_secret = {
        let auth = state.readonly_auth.read().await;
        auth.totp_secret.clone()
    };
    let totp_step = verify_totp_step(totp_secret.as_deref(), &request.code);
    // Replay 检查:即便 code 数学上正确,如果它对应的 step 已经被消费过,
    // 同样按"验证失败"处理 —— 否则攻击者捕获一次合法 verify 请求后,
    // 可以在同一 30 秒窗口内换一个 pending session 重发同一 code。
    let totp_step = totp_step.filter(|step| !state.two_factor_sessions.is_totp_step_used(*step));
    let Some(totp_step) = totp_step else {
        let pending_invalidated = state
            .two_factor_sessions
            .record_failed_attempt(&pending_token);
        state.verify_2fa_admission.record_auth_failure(client_ip);
        let secure = secure_cookies(state.shared.config());
        let body = Json(Verify2FAError {
            error: "Verification failed".to_string(),
        });
        // 该 pending token 已经被 record_failed_attempt 强制失效,主动让浏览器
        // 删掉对应的 cookie 以免下一次请求继续无谓地带上它。
        let response = if pending_invalidated {
            (
                StatusCode::UNAUTHORIZED,
                AppendHeaders([expire_cookie(TWO_FACTOR_PENDING_COOKIE, secure)]),
                body,
            )
                .into_response()
        } else {
            (StatusCode::UNAUTHORIZED, body).into_response()
        };
        return Ok(response);
    };
    // 标记 step 已被使用,阻断未来 90 秒内同 step 的重放。
    state.two_factor_sessions.mark_totp_step_used(totp_step);

    let auth_token = state
        .two_factor_sessions
        .create_authenticated()
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(Verify2FAError {
                    error: "Failed to create authenticated session".to_string(),
                }),
            )
        })?;
    state.two_factor_sessions.consume_pending(&pending_token);
    state.verify_2fa_admission.clear_auth_failures(client_ip);
    let secure = secure_cookies(state.shared.config());

    // 验证成功:设置只包含随机票据的完整认证 cookie。
    Ok((
        StatusCode::OK,
        AppendHeaders([
            auth_cookie(
                TWO_FACTOR_AUTH_COOKIE,
                &auth_token,
                TWO_FACTOR_AUTH_SECS,
                secure,
            ),
            expire_cookie(TWO_FACTOR_PENDING_COOKIE, secure),
        ]),
    )
        .into_response())
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
    let auth = state.readonly_auth.read().await;

    // 如果未启用认证,直接放行
    if auth.expected_authorization.is_none() {
        drop(auth);
        return next.run(request).await;
    }

    // 如果启用了 2FA,先检查是否有完整认证 cookie
    if auth.enable_2fa {
        if cookie_value(&headers, TWO_FACTOR_AUTH_COOKIE)
            .as_deref()
            .is_some_and(|token| state.two_factor_sessions.is_authenticated(token))
        {
            // 已完成 2FA 验证
            drop(auth);
            return next.run(request).await;
        }

        // 检查 Basic Auth
        if auth.is_authorized(&request) {
            drop(auth);
            // Basic Auth 通过,但需要 2FA 验证
            // 设置服务端随机 pending token 并重定向到 2FA 页面。
            let pending_token = match state.two_factor_sessions.create_pending() {
                Ok(token) => token,
                Err(error) => {
                    error!(error = ?error, "failed to create pending 2FA session");
                    return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                }
            };
            let secure = secure_cookies(state.shared.config());

            return (
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
                .into_response();
        }
    } else {
        // 未启用 2FA,只检查 Basic Auth
        if auth.is_authorized(&request) {
            drop(auth);
            return next.run(request).await;
        }
    }

    // 认证失败
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Basic realm=\"NodeLite\"")],
        "authentication required",
    )
        .into_response()
}

/// 提供给前端读取的"引导信息":服务名、刷新周期与已登记节点数。
pub(crate) async fn bootstrap(State(state): State<AppState>) -> impl IntoResponse {
    Json(BootstrapResponse {
        service: "nodelite-server",
        status: state.readiness.status_label(),
        ready: state.readiness.is_ready(),
        history_available: state.readiness.history_available(),
        public_base_url: state.shared.config().public_base_url.clone(),
        refresh_interval_secs: state.shared.config().refresh_interval_secs,
        registered_nodes: state.registry.count().await,
    })
}

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
///
/// 该端点是公网可达且无凭证的入口,所以在落到 registry 文件锁之前就要拦截掉
/// 显式无效的请求与已被封禁的 IP。`InstallAdmissionController` 与 `/ws` 的限流
/// 是同型逻辑,但只关心"该 IP 短期内累计了多少次无效尝试",因为安装请求本身
/// 没有"长连接"概念。
pub(crate) async fn install_bootstrap(
    State(state): State<AppState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    request: Request,
) -> Response {
    let client_ip = resolve_client_ip(state.shared.config().listen, peer_addr, &headers);
    if let Err(retry_after_secs) = state.install_admission.check(client_ip) {
        return install_blocked_response(retry_after_secs);
    }

    let Some(token) = bearer_token_from_request(&request) else {
        // 没带 Bearer:同样按一次失败计入,使无 Authorization 的扫描脚本也无法
        // 用零成本反复触发 handler。
        state.install_admission.record_auth_failure(client_ip);
        return install_unauthorized_response("missing install token");
    };

    // 在文件锁之前先做廉价的格式检查 —— install token 是 32 字节随机数的 hex
    // 编码,长度必须为 64 且仅含 0-9a-f。任何不符合格式的输入直接 401,无需
    // 进入 registry 的 spawn_blocking + flock 路径。
    if !is_well_formed_install_token(token) {
        state.install_admission.record_auth_failure(client_ip);
        return install_unauthorized_response("invalid install token");
    }

    let node = match state.registry.consume_install_token(token).await {
        Ok(Some(node)) => node,
        Ok(None) => {
            state.install_admission.record_auth_failure(client_ip);
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

    // 命中合法 token → 清理该 IP 的失败历史,避免合法的 install 流程被前一次
    // 失败计数误伤。
    state.install_admission.clear_auth_failures(client_ip);

    match render_agent_config(&state.shared.config().public_base_url, &node) {
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

/// 统一构造"无效安装 token"类响应,使每个失败分支输出的头部一致(包括
/// WWW-Authenticate 与 no-cache),同时把响应正文集中在一处便于审计。
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

/// 被封禁的 IP 在限流窗口结束前重试时返回 429 + Retry-After,与 `/ws` 入口
/// `WsAdmissionError::Blocked` 对外语义保持一致。
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

const INSTALL_TOKEN_HEX_LEN: usize = 64;
const INSTALL_TOKEN_MIN_UNIQUE_NIBBLES: u32 = 10;

/// install token 在 [registry.rs](../registry.rs) 中由 `generate_token` 生成,
/// 固定为 32 字节随机数的 lowercase hex。这里的廉价检查会在文件锁前拒绝
/// 明显无效或低熵的输入。
pub(crate) fn is_well_formed_install_token(token: &str) -> bool {
    if token.len() != INSTALL_TOKEN_HEX_LEN {
        return false;
    }

    let mut seen_nibbles = 0_u16;
    for byte in token.bytes() {
        let Some(nibble) = lowercase_hex_nibble(byte) else {
            return false;
        };
        seen_nibbles |= 1 << nibble;
    }

    seen_nibbles.count_ones() >= INSTALL_TOKEN_MIN_UNIQUE_NIBBLES
}

fn lowercase_hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

/// 仪表盘顶部的总览数据。
pub(crate) async fn overview(State(state): State<AppState>) -> Response {
    match state.shared.overview_json_bytes().await {
        Ok(body) => (
            [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
            body,
        )
            .into_response(),
        Err(error) => {
            error!(error = ?error, "failed to serialize overview response");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to render overview",
            )
                .into_response()
        }
    }
}

/// 所有节点的最新状态。
pub(crate) async fn nodes(State(state): State<AppState>) -> Response {
    match state.shared.nodes_json_bytes().await {
        Ok(body) => (
            [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
            body,
        )
            .into_response(),
        Err(error) => {
            error!(error = ?error, "failed to serialize nodes response");
            (StatusCode::INTERNAL_SERVER_ERROR, "failed to render nodes").into_response()
        }
    }
}

/// Prometheus 指标导出,供外部监控抓取全局概览与节点在线状态。
pub(crate) async fn metrics(State(state): State<AppState>) -> Response {
    let (statuses, overview) = state.shared.statuses_and_overview().await;
    let body = render_prometheus_metrics(&state, &statuses, &overview);
    (
        [
            (header::CONTENT_TYPE, PROMETHEUS_CONTENT_TYPE),
            (header::CACHE_CONTROL, "no-store, no-cache, must-revalidate"),
            (header::PRAGMA, "no-cache"),
        ],
        body,
    )
        .into_response()
}

/// 单个节点的最新状态;不存在时返回 404。
pub(crate) async fn node_status(
    State(state): State<AppState>,
    AxumPath(node_id): AxumPath<String>,
) -> Response {
    match state.shared.get_status(&node_id).await {
        Some(status) => Json(status).into_response(),
        None => (StatusCode::NOT_FOUND, "node not found").into_response(),
    }
}

/// 节点历史趋势接口。支持"过去 N 小时"或"指定区间"两种调用方式。
pub(crate) async fn node_history(
    State(state): State<AppState>,
    AxumPath(node_id): AxumPath<String>,
    Query(query): Query<HistoryQuery>,
) -> Response {
    let max_points = query
        .max_points
        .unwrap_or(DEFAULT_HISTORY_MAX_POINTS)
        .clamp(60, MAX_HISTORY_MAX_POINTS);

    let history_result = match (query.start, query.end) {
        (Some(start), Some(end)) => {
            let Some(start_at) = Utc.timestamp_opt(start, 0).single() else {
                return (StatusCode::BAD_REQUEST, "invalid history start timestamp")
                    .into_response();
            };
            let Some(end_at) = Utc.timestamp_opt(end, 0).single() else {
                return (StatusCode::BAD_REQUEST, "invalid history end timestamp").into_response();
            };
            if end_at <= start_at {
                return (StatusCode::BAD_REQUEST, "history end must be after start")
                    .into_response();
            }
            state
                .history
                .query_history_range(&node_id, start_at, end_at, max_points)
                .await
        }
        (None, None) => {
            let window_hours = query.window_hours.unwrap_or(DEFAULT_HISTORY_WINDOW_HOURS);
            state
                .history
                .query_history(&node_id, window_hours, max_points)
                .await
        }
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                "history start and end must be provided together",
            )
                .into_response();
        }
    };

    match history_result {
        Ok(points) => Json(points).into_response(),
        Err(error) => {
            error!(node_id = %node_id, error = ?error, "failed to query node history");
            (StatusCode::SERVICE_UNAVAILABLE, "history store unavailable").into_response()
        }
    }
}

/// 节点最近的 Agent 运行日志。用于排查断链、重连、token 续期等偶发问题。
pub(crate) async fn node_logs(
    State(state): State<AppState>,
    AxumPath(node_id): AxumPath<String>,
    Query(query): Query<NodeLogsQuery>,
) -> Json<Vec<AgentLogEntry>> {
    let limit = query
        .limit
        .unwrap_or(DEFAULT_NODE_LOG_LIMIT)
        .clamp(1, MAX_NODE_LOG_LIMIT);
    Json(state.agent_logs.list(&node_id, limit).await)
}

/// 从请求头中解析 `Authorization: Bearer <token>`,缺失或为空时返回 `None`。
fn bearer_token_from_request(request: &Request) -> Option<&str> {
    request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn render_prometheus_metrics(
    state: &AppState,
    statuses: &[nodelite_proto::NodeStatus],
    overview: &nodelite_proto::OverviewData,
) -> String {
    let mut body = String::new();

    push_gauge_metric(
        &mut body,
        "nodelite_server_ready",
        "Whether the NodeLite server is ready to serve protected traffic.",
        &[],
        if state.readiness.is_ready() { 1 } else { 0 },
    );
    push_gauge_metric(
        &mut body,
        "nodelite_history_available",
        "Whether the history store is currently available.",
        &[],
        if state.readiness.history_available() {
            1
        } else {
            0
        },
    );
    push_gauge_metric(
        &mut body,
        "nodelite_registry_reload_healthy",
        "Whether the registry reload loop is currently healthy.",
        &[],
        if state.readiness.registry_reload_healthy() {
            1
        } else {
            0
        },
    );
    push_gauge_metric(
        &mut body,
        "nodelite_nodes_total",
        "Number of registered nodes known to the dashboard.",
        &[],
        overview.total_nodes,
    );
    push_gauge_metric(
        &mut body,
        "nodelite_nodes_online",
        "Number of nodes currently considered online.",
        &[],
        overview.online_nodes,
    );
    push_gauge_metric(
        &mut body,
        "nodelite_nodes_offline",
        "Number of nodes currently considered offline.",
        &[],
        overview.offline_nodes,
    );
    push_gauge_metric(
        &mut body,
        "nodelite_network_total_bytes",
        "Summed network bytes reported by all nodes.",
        &[("direction", "rx")],
        overview.total_rx_bytes,
    );
    push_gauge_metric(
        &mut body,
        "nodelite_network_total_bytes",
        "Summed network bytes reported by all nodes.",
        &[("direction", "tx")],
        overview.total_tx_bytes,
    );
    push_gauge_metric(
        &mut body,
        "nodelite_network_rate_bytes_per_second",
        "Summed instantaneous network rate reported by all nodes.",
        &[("direction", "rx")],
        overview.current_rx_bytes_per_sec,
    );
    push_gauge_metric(
        &mut body,
        "nodelite_network_rate_bytes_per_second",
        "Summed instantaneous network rate reported by all nodes.",
        &[("direction", "tx")],
        overview.current_tx_bytes_per_sec,
    );
    if let Some(average_latency_ms) = overview.average_latency_ms
        && average_latency_ms.is_finite()
    {
        push_gauge_metric(
            &mut body,
            "nodelite_latency_average_milliseconds",
            "Average latency across online nodes.",
            &[],
            average_latency_ms,
        );
    }

    for status in statuses {
        let node_labels = [
            ("node_id", status.identity.node_id.as_str()),
            ("node_label", status.identity.node_label.as_str()),
            ("hostname", status.identity.hostname.as_str()),
            ("os", status.identity.os.as_str()),
        ];
        push_gauge_metric(
            &mut body,
            "nodelite_node_online",
            "Whether the node is currently online.",
            &node_labels,
            if status.online { 1 } else { 0 },
        );
        if let Some(last_seen) = status.last_seen {
            push_gauge_metric(
                &mut body,
                "nodelite_node_last_seen_timestamp_seconds",
                "Last time the node was seen by the server as a Unix timestamp.",
                &node_labels,
                last_seen.timestamp(),
            );
        }
        if let Some(latency_ms) = status.latency_ms {
            push_gauge_metric(
                &mut body,
                "nodelite_node_latency_milliseconds",
                "Latest measured node latency in milliseconds.",
                &node_labels,
                latency_ms,
            );
        }
        if let Some(snapshot) = status.snapshot.as_ref() {
            push_gauge_metric(
                &mut body,
                "nodelite_node_snapshot_timestamp_seconds",
                "Collection time of the latest node snapshot as a Unix timestamp.",
                &node_labels,
                snapshot.collected_at.timestamp(),
            );
        }
    }

    body
}

fn push_gauge_metric<T: std::fmt::Display>(
    body: &mut String,
    name: &str,
    help: &str,
    labels: &[(&str, &str)],
    value: T,
) {
    let _ = writeln!(body, "# HELP {name} {help}");
    let _ = writeln!(body, "# TYPE {name} gauge");
    body.push_str(name);
    if !labels.is_empty() {
        body.push('{');
        for (index, (key, raw_value)) in labels.iter().enumerate() {
            if index > 0 {
                body.push(',');
            }
            let escaped = escape_prometheus_label_value(raw_value);
            let _ = write!(body, "{key}=\"{escaped}\"");
        }
        body.push('}');
    }
    let _ = writeln!(body, " {value}");
}

fn escape_prometheus_label_value(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            _ => escaped.push(ch),
        }
    }
    escaped
}
