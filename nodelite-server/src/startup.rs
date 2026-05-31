use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use axum::Router;
use axum::extract::{DefaultBodyLimit, Request};
use axum::http::{HeaderValue, header};
use axum::middleware::{Next, from_fn, from_fn_with_state};
use axum::response::Response;
use axum::routing::{get, post};
use nodelite_proto::{ServerConfig, parse_server_config};
use tokio::fs;
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tower_http::compression::CompressionLayer;
use tower_http::trace::TraceLayer;
use tracing::{info, warn};

use crate::admission::{
    InstallAdmissionController, WsAdmissionController, auth_failure_admission_config,
    sensitive_auth_failure_admission_config,
};
use crate::agent_logs::AgentLogStore;
use crate::alerts::spawn_alert_runtime;
use crate::app_state::{AppState, ServerReadiness};
use crate::audit::AuditLog;
use crate::auth::{ReadonlyRouteAuth, TwoFactorSessions};
use crate::background::{
    spawn_insecure_transport_warning, spawn_registry_reloader, spawn_stale_reaper,
};
use crate::fs_security::log_if_directory_is_not_private;
use crate::handlers::{
    alert_settings, audit_log, bootstrap, brand_logo_dark_asset, brand_logo_light_asset,
    change_readonly_password, disable_two_factor, enable_two_factor, healthz, index,
    index_alert_settings_js_asset, index_settings_js_asset, install_agent_script,
    install_bootstrap, logout_and_reauth, metrics, node_detail, node_history, node_logs,
    node_status, nodes, overview, readyz, refresh_node_token, require_readonly_auth,
    server_update_log, settings, start_server_update, start_two_factor_setup, ui_i18n_asset,
    update_alert_settings, verify_2fa_api, verify_2fa_page,
};
use crate::history::HistoryStore;
use crate::registry::NodeRegistry;
use crate::snapshot::{load_snapshot, persist_snapshot, spawn_snapshot_persistor};
use crate::state::SharedState;
use crate::ws::{ws_browser_handler, ws_handler};

pub(crate) const PROTECTED_CONTENT_SECURITY_POLICY: &str = "default-src 'self'; img-src 'self' data:; \
     connect-src 'self' https://raw.githubusercontent.com https://api.github.com; font-src 'self'; \
     object-src 'none'; media-src 'none'; worker-src 'none'; base-uri 'none'; frame-ancestors 'none'; \
     form-action 'self'";
pub(crate) const PROTECTED_CACHE_CONTROL: &str = "no-store, no-cache, must-revalidate";
pub(crate) const JSON_WRITE_BODY_LIMIT_BYTES: usize = 16 * 1024;

struct ServerRuntime {
    state: AppState,
    background_tasks: Vec<JoinHandle<()>>,
    shutdown_artifacts: ShutdownArtifacts,
}

struct ShutdownArtifacts {
    shared: SharedState,
    history: HistoryStore,
    audit_log: AuditLog,
    snapshot_path: PathBuf,
}

/// 启动 Web 服务:加载配置 → 初始化各子系统 → 注册路由 → 监听端口。
pub(crate) async fn run_server(config_path: &Path) -> Result<()> {
    let config = Arc::new(load_server_config(config_path).await?);
    let listen_addr = config.listen;
    let public_base_url = config.public_base_url.clone();
    let refresh_interval_secs = config.refresh_interval_secs;
    let readonly_route_auth = ReadonlyRouteAuth::from_config(config.readonly_auth.clone());

    // 密码强度检查:如果启用了认证,验证密码是否满足最低安全要求
    if let Some(ref auth_config) = config.readonly_auth {
        validate_password_strength(&auth_config.password)?;
    }

    let runtime =
        initialize_server_runtime(config_path, Arc::clone(&config), readonly_route_auth).await?;
    let shutdown = runtime.state.shutdown.clone();
    let app = build_router(runtime.state);

    let listener = TcpListener::bind(listen_addr)
        .await
        .with_context(|| format!("failed to bind server listener to {listen_addr}"))?;

    info!(
        listen = %listen_addr,
        public_base_url = %public_base_url,
        refresh_interval_secs,
        "nodelite server listening",
    );

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("server exited unexpectedly")?;

    drain_server_shutdown(
        shutdown,
        runtime.background_tasks,
        runtime.shutdown_artifacts,
    )
    .await;
    Ok(())
}

async fn initialize_server_runtime(
    config_path: &Path,
    config: Arc<ServerConfig>,
    readonly_route_auth: ReadonlyRouteAuth,
) -> Result<ServerRuntime> {
    let registry = NodeRegistry::load(config.node_registry_path.as_path())
        .await
        .with_context(|| {
            format!(
                "failed to load node registry {}",
                config.node_registry_path.display()
            )
        })?;
    let shared = SharedState::new(Arc::clone(&config));
    let history = HistoryStore::new(
        config.history_db_path.clone(),
        config.sqlite_busy_timeout_secs,
    );
    let agent_logs = AgentLogStore::new();
    let audit_log = AuditLog::new(config.audit.clone(), config.sqlite_busy_timeout_secs);
    history.initialize().await;
    audit_log.initialize().await?;
    let readiness = ServerReadiness::new(history.is_available());
    readiness.mark_history_available(history.is_available());
    restore_snapshot_if_available(&shared, config.snapshot_path.as_path()).await;

    let shutdown = CancellationToken::new();
    let state = AppState {
        history,
        agent_logs,
        audit_log,
        install_admission: InstallAdmissionController::new(auth_failure_admission_config(
            &config.ws,
        )),
        verify_2fa_admission: InstallAdmissionController::new(auth_failure_admission_config(
            &config.ws,
        )),
        readonly_auth_admission: InstallAdmissionController::new(auth_failure_admission_config(
            &config.ws,
        )),
        sensitive_readonly_auth_admission: InstallAdmissionController::new(
            sensitive_auth_failure_admission_config(&config.ws),
        ),
        readiness,
        registry,
        shared,
        ws_admission: WsAdmissionController::new(&config.ws),
        browser_ws_admission: WsAdmissionController::new(&config.ws),
        readonly_auth: Arc::new(RwLock::new(readonly_route_auth)),
        alerting: Arc::new(RwLock::new(config.alerting.clone())),
        two_factor_sessions: TwoFactorSessions::new(),
        config_path: Arc::new(config_path.to_path_buf()),
        shutdown,
    };
    let background_tasks = spawn_server_background_tasks(&config, &state);
    log_registry_loaded(&config, &state.registry).await;
    let shutdown_artifacts = ShutdownArtifacts {
        shared: state.shared.clone(),
        history: state.history.clone(),
        audit_log: state.audit_log.clone(),
        snapshot_path: config.snapshot_path.clone(),
    };
    Ok(ServerRuntime {
        state,
        background_tasks,
        shutdown_artifacts,
    })
}

fn spawn_server_background_tasks(config: &ServerConfig, state: &AppState) -> Vec<JoinHandle<()>> {
    let mut background_tasks = vec![
        spawn_registry_reloader(
            state.registry.clone(),
            state.history.clone(),
            state.agent_logs.clone(),
            state.readiness.clone(),
            state.shutdown.clone(),
        ),
        state.audit_log.clone().spawn_pruner(state.shutdown.clone()),
        spawn_stale_reaper(state.shared.clone(), state.shutdown.clone()),
        spawn_snapshot_persistor(
            state.shared.clone(),
            config.snapshot_path.clone(),
            state.shutdown.clone(),
        ),
        spawn_alert_runtime(
            state.alerting.clone(),
            state.shared.clone(),
            state.shutdown.clone(),
        ),
    ];
    if let Some(handle) = spawn_insecure_transport_warning(
        config.public_base_url.clone(),
        config.listen,
        config.insecure_transport_warn_interval_secs,
        state.shutdown.clone(),
    ) {
        background_tasks.push(handle);
    }
    background_tasks
}

async fn log_registry_loaded(config: &ServerConfig, registry: &NodeRegistry) {
    let enrolled_nodes = registry.count().await;
    info!(
        registry_path = %config.node_registry_path.display(),
        enrolled_nodes,
        "node registry loaded",
    );
}

async fn drain_server_shutdown(
    shutdown: CancellationToken,
    background_tasks: Vec<JoinHandle<()>>,
    shutdown_artifacts: ShutdownArtifacts,
) {
    // axum 的 graceful shutdown 只 drain HTTP 请求,不会通知 WebSocket 会话或
    // 后台任务。这里在 HTTP 端 drain 完成后, cancel 全局 token, 让:
    //   - 每个 spawn_* 后台任务从各自的 select! 跳出, 结束 loop;
    //   - 每个活跃 WebSocket handle_socket 发出 Close 帧后退出。
    info!("propagating shutdown signal to background tasks and websocket sessions");
    shutdown.cancel();

    // 给所有后台任务最多 5 秒收尾;超时则强制 abort 避免拖延 systemd 的 TimeoutStopSec。
    let join_deadline = Duration::from_secs(5);
    for handle in background_tasks {
        match tokio::time::timeout(join_deadline, handle).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                warn!(error = ?error, "background task ended with error during shutdown");
            }
            Err(_) => {
                warn!(
                    timeout_secs = join_deadline.as_secs(),
                    "background task did not exit in time during shutdown"
                );
            }
        }
    }

    // 周期持久化任务每 15 秒落盘一次,SIGTERM 期间最近一次 tick 之后的状态变更可能
    // 还没刷到磁盘。这里同步再落一次,确保 systemd restart 后看到的就是退出前最新视图。
    info!("flushing final snapshot before shutdown");
    let final_statuses = shutdown_artifacts.shared.list_statuses().await;
    if let Err(error) =
        persist_snapshot(shutdown_artifacts.snapshot_path.as_path(), &final_statuses).await
    {
        warn!(error = ?error, path = %shutdown_artifacts.snapshot_path.display(), "failed to flush final snapshot");
    }

    // History writer 仍可能有入队但未 flush 的样本(WS 在收到 Close 之前
    // 最后那一拍上报的数据)。显式 drain 一次,避免 systemd restart 后历史断档。
    info!("draining history writer before shutdown");
    shutdown_artifacts.history.shutdown().await;

    info!("draining audit writer before shutdown");
    shutdown_artifacts.audit_log.shutdown().await;

    info!("nodelite server shutdown complete");
}

pub(crate) fn build_router(state: AppState) -> Router {
    let public_routes = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/logout-and-reauth", get(logout_and_reauth))
        .route("/verify-2fa", get(verify_2fa_page))
        .merge(
            Router::new()
                .route("/api/verify-2fa", post(verify_2fa_api))
                .layer(DefaultBodyLimit::max(JSON_WRITE_BODY_LIMIT_BYTES)),
        )
        .route("/install/install-agent.sh", get(install_agent_script))
        .route("/install/bootstrap", get(install_bootstrap))
        .route("/ws", get(ws_handler))
        .route_layer(from_fn(set_protected_response_headers));
    let protected_json_routes = Router::new()
        .route(
            "/api/nodes/{node_id}/refresh-token",
            post(refresh_node_token),
        )
        .route("/api/settings/password", post(change_readonly_password))
        .route("/api/settings/alerts", post(update_alert_settings))
        .route("/api/settings/update/server", post(start_server_update))
        .route("/api/settings/2fa/enable", post(enable_two_factor))
        .route("/api/settings/2fa/disable", post(disable_two_factor))
        .layer(DefaultBodyLimit::max(JSON_WRITE_BODY_LIMIT_BYTES));
    let protected_routes = Router::new()
        .route("/", get(index))
        .route("/nodes/{node_id}", get(node_detail))
        .route("/assets/brand-logo-dark.webp", get(brand_logo_dark_asset))
        .route("/assets/brand-logo-light.webp", get(brand_logo_light_asset))
        .route(
            "/assets/index-alert-settings.js",
            get(index_alert_settings_js_asset),
        )
        .route("/assets/index-settings.js", get(index_settings_js_asset))
        .route("/assets/ui-i18n.json", get(ui_i18n_asset))
        .route("/api/bootstrap", get(bootstrap))
        .route("/api/overview", get(overview))
        .route("/metrics", get(metrics))
        .route("/api/nodes", get(nodes))
        .route("/api/nodes/{node_id}", get(node_status))
        .route("/api/nodes/{node_id}/history", get(node_history))
        .route("/api/nodes/{node_id}/logs", get(node_logs))
        .route("/api/audit-log", get(audit_log))
        .route("/ws/browser", get(ws_browser_handler))
        .route("/api/settings", get(settings))
        .route("/api/settings/alerts", get(alert_settings))
        .route("/api/settings/update/server/log", get(server_update_log))
        .route("/api/settings/2fa/start", post(start_two_factor_setup))
        .merge(protected_json_routes)
        .route_layer(from_fn(set_protected_response_headers))
        .route_layer(from_fn_with_state(state.clone(), require_readonly_auth));
    Router::new()
        .merge(public_routes)
        .merge(protected_routes)
        .with_state(state)
        .layer(response_compression_layer())
        .layer(TraceLayer::new_for_http())
}

/// 对文本型 UI/API 响应启用 gzip/br,同时沿用 tower-http 默认规则跳过 image/*。
///
/// `/metrics` 是 text/plain,会随客户端 `Accept-Encoding` 被压缩;WebP logo 属于
/// image/*,不会被二次压缩。
pub(crate) fn response_compression_layer() -> CompressionLayer {
    CompressionLayer::new().no_deflate().no_zstd()
}

/// 统一给受保护的 UI / API 响应补齐安全头,避免每个 handler 重复手写。
pub(crate) async fn set_protected_response_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    if !headers.contains_key(header::CONTENT_SECURITY_POLICY) {
        headers.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(PROTECTED_CONTENT_SECURITY_POLICY),
        );
    }
    if !headers.contains_key(header::X_CONTENT_TYPE_OPTIONS) {
        headers.insert(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        );
    }
    if !headers.contains_key(header::REFERRER_POLICY) {
        headers.insert(
            header::REFERRER_POLICY,
            HeaderValue::from_static("strict-origin-when-cross-origin"),
        );
    }
    if !headers.contains_key(header::CACHE_CONTROL) {
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(PROTECTED_CACHE_CONTROL),
        );
        headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    }
    response
}

/// 加载并解析 server.toml,顺带对 snapshot / history 目录的不存在情况发出提醒。
pub(crate) async fn load_server_config(path: &Path) -> Result<ServerConfig> {
    let content = fs::read_to_string(path)
        .await
        .with_context(|| format!("failed to read config file {}", path.display()))?;
    let config = parse_server_config(&content)
        .map_err(|error| anyhow!("failed to parse {}: {error}", path.display()))?;

    if let Some(parent) = config.snapshot_path.parent()
        && !parent.as_os_str().is_empty()
    {
        if !parent.exists() {
            warn!(
                snapshot_dir = %parent.display(),
                "snapshot directory does not exist yet; it will be created later",
            );
        } else {
            log_if_directory_is_not_private(parent, "snapshot_path.parent");
        }
    }
    if let Some(parent) = config.history_db_path.parent()
        && !parent.as_os_str().is_empty()
    {
        if !parent.exists() {
            warn!(
                history_dir = %parent.display(),
                "history directory does not exist yet; it will be created later",
            );
        } else {
            log_if_directory_is_not_private(parent, "history_db_path.parent");
        }
    }
    if let Some(parent) = config.node_registry_path.parent()
        && !parent.as_os_str().is_empty()
        && parent.exists()
    {
        log_if_directory_is_not_private(parent, "node_registry_path.parent");
    }
    if config.audit.enabled
        && let Some(parent) = config.audit.db_path.parent()
        && !parent.as_os_str().is_empty()
    {
        if !parent.exists() {
            warn!(
                audit_dir = %parent.display(),
                "audit directory does not exist yet; it will be created later",
            );
        } else {
            log_if_directory_is_not_private(parent, "audit.db_path.parent");
        }
    }

    Ok(config)
}

/// 初始化 `tracing` 日志,支持通过 `RUST_LOG` 调整级别。
pub(crate) fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "nodelite_server=info,tower_http=info".into()),
        )
        .with_target(false)
        .compact()
        .init();
}

/// 启动期验证 `READONLY_PASSWORD`:复用 `auth::validate_password_strength`,
/// 把统一规则的 `&'static str` 错误包装成带环境变量上下文的 `anyhow::Error`。
fn validate_password_strength(password: &str) -> Result<()> {
    if let Err(reason) = crate::auth::validate_password_strength(password) {
        bail!(
            "READONLY_PASSWORD rejected: {reason}.\n\
             Recommendation: use a strong random password, e.g.:\n  \
             export READONLY_PASSWORD=\"$(openssl rand -base64 24)\""
        );
    }
    Ok(())
}

/// 启动期尝试从磁盘恢复一份 NodeStatus 列表,失败时记录日志并继续以空状态启动。
async fn restore_snapshot_if_available(shared: &SharedState, path: &Path) {
    if !path.exists() {
        return;
    }

    match load_snapshot(path).await {
        Ok(statuses) => {
            shared.restore_statuses(statuses).await;
        }
        Err(error) => {
            warn!(error = ?error, path = %path.display(), "failed to restore snapshot; continuing with empty state");
        }
    }
}

/// 等待 SIGTERM / SIGINT,任意一个到达即触发 axum 的优雅停机。
///
/// 仅在 unix 平台监听 SIGTERM;其它平台只听 Ctrl-C。两路任一就绪都会立即返回,
/// 因此即便其中一路注册失败也不会阻塞另一路 —— 否则 systemd 的 SIGTERM 会被静默忽略。
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
        _ = ctrl_c => info!("received SIGINT; initiating graceful shutdown"),
        _ = terminate => info!("received SIGTERM; initiating graceful shutdown"),
    }
}
