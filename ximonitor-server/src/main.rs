// XiMonitor 中心服务入口。
//
// 角色:
// - 通过 `/ws` 接收 Agent 上报的 WebSocket 连接;
// - 通过 `/api/*` 与静态 HTML 给前端提供只读视图;
// - 通过 `install-agent` / `upgrade-agent` 子命令为运维生成安装脚本片段。
//
// 关键设计:
// - `AppState` 由 `SharedState`(运行态)、`NodeRegistry`(凭证)与 `HistoryStore`(SQLite)组成,
//   每个 HTTP / WebSocket 处理函数都得到一份廉价克隆。
// - WebSocket 接入由 `WsAdmissionController` 做总量限流 + IP 限流 + 暴力破解封禁。
// - 来自 Agent 的所有指标都经过 `sanitize_snapshot` 处理,防止异常值污染统计或图表。

mod admission;
mod auth;
mod cli;
mod handlers;
mod history;
mod qr;
mod registry;
mod sanitize;
mod snapshot;
mod state;
mod ui;
mod ws;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use axum::Router;
use axum::middleware::from_fn_with_state;
use axum::routing::{get, post};
use clap::Parser;
use tokio::fs;
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tokio::time::{MissedTickBehavior, interval};
use tower_http::trace::TraceLayer;
use tracing::{info, warn};
use url::Url;
use ximonitor_proto::{ServerConfig, parse_server_config};

use crate::admission::{InstallAdmissionConfig, InstallAdmissionController, WsAdmissionController};
use crate::auth::{ReadonlyRouteAuth, TwoFactorSessions};
use crate::cli::{Cli, Command, install_agent_command, issue_node_command, upgrade_agent_command};
use crate::handlers::{
    bootstrap, change_readonly_password, healthz, index, install_agent_script, install_bootstrap,
    logout_and_reauth, node_detail, node_history, node_status, nodes, overview, readyz,
    require_readonly_auth, settings, ui_i18n_asset, verify_2fa_api, verify_2fa_page,
};
use crate::history::HistoryStore;
use crate::registry::NodeRegistry;
use crate::snapshot::{load_snapshot, persist_snapshot, spawn_snapshot_persistor};
use crate::state::SharedState;
use crate::ws::ws_handler;

/// 在各处理器之间共享的运行时上下文。
#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) history: HistoryStore,
    pub(crate) install_admission: InstallAdmissionController,
    /// `/api/verify-2fa` 的 IP 维度限流器:与 `install_admission` 同型,
    /// 但实例独立,避免安装接口的失败计数误伤 2FA 登录,反之亦然。
    pub(crate) verify_2fa_admission: InstallAdmissionController,
    pub(crate) readiness: ServerReadiness,
    pub(crate) registry: NodeRegistry,
    pub(crate) shared: SharedState,
    pub(crate) ws_admission: WsAdmissionController,
    pub(crate) readonly_auth: Arc<RwLock<ReadonlyRouteAuth>>,
    pub(crate) two_factor_sessions: TwoFactorSessions,
    pub(crate) config_path: Arc<PathBuf>,
}

/// 只跟踪"对外是否可服务"所需的几个关键依赖状态。
///
/// - `healthz` 仍然只回答"进程是否存活";
/// - `readyz` 与 `/api/bootstrap.status` 则用这里的状态反映"是否已具备对外服务能力"。
#[derive(Clone)]
struct ServerReadiness {
    history_available: Arc<AtomicBool>,
    registry_reload_healthy: Arc<AtomicBool>,
}

/// 不安全传输警告的输出间隔(秒)。
const INSECURE_TRANSPORT_WARN_INTERVAL_SECS: u64 = 900;

impl ServerReadiness {
    fn new(history_available: bool) -> Self {
        Self {
            history_available: Arc::new(AtomicBool::new(history_available)),
            registry_reload_healthy: Arc::new(AtomicBool::new(true)),
        }
    }

    fn is_ready(&self) -> bool {
        self.history_available() && self.registry_reload_healthy()
    }

    fn status_label(&self) -> &'static str {
        if self.is_ready() { "ok" } else { "degraded" }
    }

    fn history_available(&self) -> bool {
        self.history_available.load(Ordering::Relaxed)
    }

    fn registry_reload_healthy(&self) -> bool {
        self.registry_reload_healthy.load(Ordering::Relaxed)
    }

    fn mark_history_available(&self, available: bool) {
        self.history_available.store(available, Ordering::Relaxed);
    }

    fn mark_registry_reload_healthy(&self, healthy: bool) {
        self.registry_reload_healthy
            .store(healthy, Ordering::Relaxed);
    }
}

/// 启动入口:根据 CLI 子命令分发到具体动作。
#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let cli = Cli::parse();
    match cli.command {
        Some(Command::IssueNode(args)) => issue_node_command(cli.config.as_path(), args).await,
        Some(Command::InstallAgent(args)) => {
            install_agent_command(cli.config.as_path(), args).await
        }
        Some(Command::UpgradeAgent) => upgrade_agent_command(cli.config.as_path()).await,
        None => run_server(cli.config.as_path()).await,
    }
}

/// 启动 Web 服务:加载配置 → 初始化各子系统 → 注册路由 → 监听端口。
async fn run_server(config_path: &Path) -> Result<()> {
    let config = Arc::new(load_server_config(config_path).await?);
    let listen_addr = config.listen;
    let public_base_url = config.public_base_url.clone();
    let refresh_interval_secs = config.refresh_interval_secs;
    let readonly_route_auth = ReadonlyRouteAuth::from_config(config.readonly_auth.clone());

    // 密码强度检查:如果启用了认证,验证密码是否满足最低安全要求
    if let Some(ref auth_config) = config.readonly_auth {
        validate_password_strength(&auth_config.password)?;
    }

    let registry = NodeRegistry::load(config.node_registry_path.as_path())
        .await
        .with_context(|| {
            format!(
                "failed to load node registry {}",
                config.node_registry_path.display()
            )
        })?;
    let shared = SharedState::new(Arc::clone(&config));
    let history = HistoryStore::new(config.history_db_path.clone());
    history.initialize().await;
    let readiness = ServerReadiness::new(history.is_available());
    readiness.mark_history_available(history.is_available());
    restore_snapshot_if_available(&shared, config.snapshot_path.as_path()).await;

    spawn_registry_reloader(registry.clone(), readiness.clone());
    spawn_stale_reaper(shared.clone());
    spawn_snapshot_persistor(shared.clone(), config.snapshot_path.clone());
    spawn_insecure_transport_warning(config.public_base_url.clone(), config.listen);

    let enrolled_nodes = registry.count().await;
    info!(
        registry_path = %config.node_registry_path.display(),
        enrolled_nodes,
        "node registry loaded",
    );

    let state = AppState {
        history,
        install_admission: InstallAdmissionController::new(InstallAdmissionConfig {
            // 复用 ws 子小节里同名的限流配置 —— 站在运维视角它们是同一组
            // "认证失败暴力策略"参数,没必要再多开一组。
            auth_fail_window_secs: config.ws.auth_fail_window_secs,
            auth_fail_max_attempts: config.ws.auth_fail_max_attempts,
            auth_block_secs: config.ws.auth_block_secs,
        }),
        verify_2fa_admission: InstallAdmissionController::new(InstallAdmissionConfig {
            // 与 install / ws 同一组阈值,但实例独立 —— 攻击者用 install
            // 失败把 IP 撞到封禁,不应该把同一时间的合法 2FA 登录也封掉。
            auth_fail_window_secs: config.ws.auth_fail_window_secs,
            auth_fail_max_attempts: config.ws.auth_fail_max_attempts,
            auth_block_secs: config.ws.auth_block_secs,
        }),
        readiness,
        registry,
        shared,
        ws_admission: WsAdmissionController::new(&config.ws),
        readonly_auth: Arc::new(RwLock::new(readonly_route_auth.clone())),
        two_factor_sessions: TwoFactorSessions::new(),
        config_path: Arc::new(config_path.to_path_buf()),
    };
    let shared_for_shutdown = state.shared.clone();
    let snapshot_path = config.snapshot_path.clone();
    let protected_routes = Router::new()
        .route("/", get(index))
        .route("/nodes/{node_id}", get(node_detail))
        .route("/assets/ui-i18n.json", get(ui_i18n_asset))
        .route("/api/bootstrap", get(bootstrap))
        .route("/api/overview", get(overview))
        .route("/api/nodes", get(nodes))
        .route("/api/nodes/{node_id}", get(node_status))
        .route("/api/nodes/{node_id}/history", get(node_history))
        .route("/api/settings", get(settings))
        .route("/api/settings/password", post(change_readonly_password))
        .route_layer(from_fn_with_state(state.clone(), require_readonly_auth));
    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/logout-and-reauth", get(logout_and_reauth))
        .route("/verify-2fa", get(verify_2fa_page))
        .route("/api/verify-2fa", post(verify_2fa_api))
        .route("/install/install-agent.sh", get(install_agent_script))
        .route("/install/bootstrap", get(install_bootstrap))
        .route("/ws", get(ws_handler))
        .merge(protected_routes)
        .with_state(state)
        .layer(TraceLayer::new_for_http());

    let listener = TcpListener::bind(listen_addr)
        .await
        .with_context(|| format!("failed to bind server listener to {listen_addr}"))?;

    info!(
        listen = %listen_addr,
        public_base_url = %public_base_url,
        refresh_interval_secs,
        "ximonitor server listening",
    );

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("server exited unexpectedly")?;

    // 周期持久化任务每 15 秒落盘一次,SIGTERM 期间最近一次 tick 之后的状态变更可能
    // 还没刷到磁盘。这里同步再落一次,确保 systemd restart 后看到的就是退出前最新视图。
    info!("flushing final snapshot before shutdown");
    let final_statuses = shared_for_shutdown.list_statuses().await;
    if let Err(error) = persist_snapshot(snapshot_path.as_path(), &final_statuses).await {
        warn!(error = ?error, path = %snapshot_path.display(), "failed to flush final snapshot");
    }
    info!("ximonitor server shutdown complete");
    Ok(())
}

/// 验证密码强度:至少 8 字符,包含字母和数字。
/// 如果密码过弱,返回错误并给出建议。
fn validate_password_strength(password: &str) -> Result<()> {
    const MIN_LENGTH: usize = 8;

    if password.len() < MIN_LENGTH {
        bail!(
            "READONLY_PASSWORD is too short ({} chars). Minimum {} characters required.\n\
             Recommendation: Use a strong random password, e.g.:\n  \
             export READONLY_PASSWORD=\"$(openssl rand -base64 24)\"",
            password.len(),
            MIN_LENGTH
        );
    }

    let has_letter = password.chars().any(|c| c.is_alphabetic());
    let has_digit = password.chars().any(|c| c.is_ascii_digit());

    if !has_letter || !has_digit {
        warn!(
            "READONLY_PASSWORD does not meet recommended strength (letters + digits).\n\
             Current password: {} letters, {} digits.\n\
             Recommendation: Use a strong random password, e.g.:\n  \
             export READONLY_PASSWORD=\"$(openssl rand -base64 24)\"",
            if has_letter { "has" } else { "no" },
            if has_digit { "has" } else { "no" }
        );
    }

    Ok(())
}

/// 加载并解析 server.toml,顺带对 snapshot / history 目录的不存在情况发出提醒。
async fn load_server_config(path: &Path) -> Result<ServerConfig> {
    let content = fs::read_to_string(path)
        .await
        .with_context(|| format!("failed to read config file {}", path.display()))?;
    let config = parse_server_config(&content)
        .map_err(|error| anyhow!("failed to parse {}: {error}", path.display()))?;

    if let Some(parent) = config.snapshot_path.parent()
        && !parent.as_os_str().is_empty()
        && !parent.exists()
    {
        warn!(
            snapshot_dir = %parent.display(),
            "snapshot directory does not exist yet; it will be created later",
        );
    }
    if let Some(parent) = config.history_db_path.parent()
        && !parent.as_os_str().is_empty()
        && !parent.exists()
    {
        warn!(
            history_dir = %parent.display(),
            "history directory does not exist yet; it will be created later",
        );
    }

    Ok(config)
}

/// 后台任务:每秒扫描一次注册表,把超时节点标记为离线。
fn spawn_stale_reaper(shared: SharedState) {
    tokio::spawn(async move {
        let mut ticker = interval(Duration::from_secs(1));
        // 进程或主机被挂起后,interval 默认会"补打"积压 tick;这里改为延后下一次,
        // 避免恢复瞬间连续多次扫描全表(对大规模注册表是无谓的 CPU 抖动)。
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let count = shared.mark_stale().await;
            if count > 0 {
                info!(count, "marked stale nodes offline");
            }
        }
    });
}

/// 后台任务:每秒检查一次注册表文件是否有外部更改(例如 CLI 颁发了新节点)。
fn spawn_registry_reloader(registry: NodeRegistry, readiness: ServerReadiness) {
    tokio::spawn(async move {
        let mut ticker = interval(Duration::from_secs(1));
        // 挂起恢复后只想做一次最近态的 reload,而不是连续 N 次磁盘 IO。
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            match registry.reload().await {
                Ok(true) => {
                    readiness.mark_registry_reload_healthy(true);
                    let enrolled_nodes = registry.count().await;
                    info!(
                        registry_path = %registry.path().display(),
                        enrolled_nodes,
                        "reloaded node registry",
                    );
                }
                Ok(false) => {
                    readiness.mark_registry_reload_healthy(true);
                }
                Err(error) => {
                    readiness.mark_registry_reload_healthy(false);
                    warn!(
                        error = ?error,
                        registry_path = %registry.path().display(),
                        "failed to reload node registry; keeping previous in-memory snapshot",
                    );
                }
            }
        }
    });
}

/// 在监听非回环地址但仍然使用 `http://` 公网基址时,周期性输出 TLS 警告。
fn spawn_insecure_transport_warning(public_base_url: String, listen: std::net::SocketAddr) {
    if !uses_insecure_remote_public_base_url(&public_base_url, listen) {
        return;
    }

    tokio::spawn(async move {
        let mut ticker = interval(Duration::from_secs(INSECURE_TRANSPORT_WARN_INTERVAL_SECS));
        // 警告是节流型日志,跳过错过的 tick 即可,不要在恢复后连续 burst 多条相同警告。
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            warn!(
                listen = %listen,
                public_base_url = %public_base_url,
                "server is configured without TLS; use an https:// public_base_url and terminate TLS in front of XiMonitor",
            );
        }
    });
}

fn uses_insecure_remote_public_base_url(
    public_base_url: &str,
    listen: std::net::SocketAddr,
) -> bool {
    let Ok(url) = Url::parse(public_base_url) else {
        return false;
    };
    if url.scheme() != "http" {
        return false;
    }
    if !listen.ip().is_loopback() {
        return true;
    }

    !host_is_local(url.host_str())
}

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

/// 初始化 `tracing` 日志,支持通过 `RUST_LOG` 调整级别。
fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ximonitor_server=info,tower_http=info".into()),
        )
        .with_target(false)
        .compact()
        .init();
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use axum::Router;
    use axum::body::Body;
    use axum::extract::{ConnectInfo, State};
    use axum::http::{HeaderMap, Request, StatusCode, header};
    use chrono::Utc;
    use tokio::runtime::Runtime;
    use tokio::sync::RwLock;

    use super::{
        AppState, ReadonlyRouteAuth, ServerReadiness, TwoFactorSessions,
        uses_insecure_remote_public_base_url, ws_handler,
    };
    use crate::admission::{
        InstallAdmissionConfig, InstallAdmissionController, WsAdmissionController,
        WsAdmissionError, resolve_client_ip, sweep_expired_auth_failures,
    };
    use crate::handlers::{
        bootstrap, healthz, index, install_agent_script, install_bootstrap,
        is_well_formed_install_token, node_detail, node_history, node_status, nodes, overview,
        readyz, ui_i18n_asset,
    };
    use crate::history::HistoryStore;
    use crate::registry::{IssueNodeRequest, NodeRegistry, issue_node};
    use crate::sanitize::{
        MAX_SANITIZED_DISKS, MAX_SANITIZED_LOAD, MAX_SANITIZED_RATE_BYTES_PER_SEC,
        MAX_SANITIZED_STRING_BYTES, METRIC_ANOMALY_SESSION_LIMIT, SanitizationReport,
        sanitize_snapshot, should_disconnect_for_metric_anomalies, truncate_to_byte_boundary,
        update_metric_anomaly_window,
    };
    use crate::state::SharedState;
    use axum::routing::get;
    use tower_http::trace::TraceLayer;
    use ximonitor_proto::{NodeSnapshot, ServerConfig, WsConfig};

    #[test]
    fn router_builds_with_v08_path_syntax() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic enough")
            .as_nanos();
        let registry_path =
            std::env::temp_dir().join(format!("ximonitor-router-test-{unique}.json"));
        let config = Arc::new(ServerConfig {
            listen: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 8080)),
            public_base_url: "http://127.0.0.1:8080".to_string(),
            insecure_allow_http: false,
            readonly_auth: None,
            ws: WsConfig {
                max_total_connections: 32,
                max_connections_per_ip: 8,
                auth_fail_window_secs: 300,
                auth_fail_max_attempts: 6,
                auth_block_secs: 600,
            },
            node_registry_path: registry_path,
            history_db_path: PathBuf::from("./data/history.sqlite3"),
            snapshot_path: PathBuf::from("./data/snapshot.json"),
            stale_after_secs: 20,
            ping_interval_secs: 10,
            max_message_bytes: 65536,
            refresh_interval_secs: 5,
            ignored_filesystems: vec!["tmpfs".to_string()],
            agent_release_base_url: None,
            agent_release_sha256_x86_64: None,
            agent_release_sha256_aarch64: None,
        });
        let runtime = Runtime::new().expect("runtime should build");
        let state = AppState {
            history: HistoryStore::new(PathBuf::from("./data/history.sqlite3")),
            install_admission: InstallAdmissionController::new(InstallAdmissionConfig {
                auth_fail_window_secs: 300,
                auth_fail_max_attempts: 6,
                auth_block_secs: 600,
            }),
            verify_2fa_admission: InstallAdmissionController::new(InstallAdmissionConfig {
                auth_fail_window_secs: 300,
                auth_fail_max_attempts: 6,
                auth_block_secs: 600,
            }),
            readiness: ServerReadiness::new(false),
            registry: runtime
                .block_on(NodeRegistry::load(config.node_registry_path.as_path()))
                .expect("registry should load"),
            shared: SharedState::new(config),
            ws_admission: WsAdmissionController::new(&WsConfig {
                max_total_connections: 32,
                max_connections_per_ip: 8,
                auth_fail_window_secs: 300,
                auth_fail_max_attempts: 6,
                auth_block_secs: 600,
            }),
            readonly_auth: Arc::new(RwLock::new(ReadonlyRouteAuth::from_config(None))),
            two_factor_sessions: TwoFactorSessions::new(),
            config_path: Arc::new(PathBuf::from("config/server.toml")),
        };

        let _app: Router = Router::new()
            .route("/", get(index))
            .route("/nodes/{node_id}", get(node_detail))
            .route("/assets/ui-i18n.json", get(ui_i18n_asset))
            .route("/healthz", get(healthz))
            .route("/readyz", get(readyz))
            .route("/install/install-agent.sh", get(install_agent_script))
            .route("/install/bootstrap", get(install_bootstrap))
            .route("/api/bootstrap", get(bootstrap))
            .route("/api/overview", get(overview))
            .route("/api/nodes", get(nodes))
            .route("/api/nodes/{node_id}", get(node_status))
            .route("/api/nodes/{node_id}/history", get(node_history))
            .route("/ws", get(ws_handler))
            .with_state(state)
            .layer(TraceLayer::new_for_http());
    }

    #[test]
    fn readonly_route_auth_matches_basic_header() {
        let auth = ReadonlyRouteAuth::from_config(Some(ximonitor_proto::ReadonlyAuthConfig {
            username: "viewer".to_string(),
            password: "secret".to_string(),
            enable_2fa: false,
            totp_secret: None,
        }));
        let request = Request::builder()
            .uri("/api/overview")
            .header(header::AUTHORIZATION, "Basic dmlld2VyOnNlY3JldA==")
            .body(Body::empty())
            .expect("request should build");

        assert!(auth.is_authorized(&request));
    }

    #[test]
    fn two_factor_session_cookie_must_be_server_issued() {
        let sessions = TwoFactorSessions::new();
        assert!(!sessions.is_authenticated("verified"));

        let token = sessions
            .create_authenticated()
            .expect("session token should be generated");
        assert!(sessions.is_authenticated(&token));
        sessions.remove_authenticated(&token);
        assert!(!sessions.is_authenticated(&token));
    }

    #[test]
    fn pending_session_invalidated_after_max_failed_attempts() {
        let sessions = TwoFactorSessions::new();
        let token = sessions
            .create_pending()
            .expect("pending session should be created");
        assert!(sessions.pending_exists(&token));

        // 前 N-1 次失败:pending 仍然有效。
        for _ in 0..(crate::auth::TWO_FACTOR_MAX_FAILED_ATTEMPTS - 1) {
            assert!(!sessions.record_failed_attempt(&token));
            assert!(sessions.pending_exists(&token));
        }

        // 第 N 次失败:pending 必须被强制失效。
        assert!(sessions.record_failed_attempt(&token));
        assert!(!sessions.pending_exists(&token));

        // 已经被失效的 token 再次记录失败时,应当也返回 true(等同已失效),
        // 防止调用方因为找不到 pending 而漏掉"通知客户端清 cookie"的动作。
        assert!(sessions.record_failed_attempt(&token));
    }

    #[test]
    fn totp_step_marked_used_blocks_replay() {
        let sessions = TwoFactorSessions::new();
        let step = 12345_u64;
        assert!(!sessions.is_totp_step_used(step));
        sessions.mark_totp_step_used(step);
        assert!(sessions.is_totp_step_used(step));
        // 不同 step 不会被误判
        assert!(!sessions.is_totp_step_used(step + 1));
        assert!(!sessions.is_totp_step_used(step - 1));
    }

    #[test]
    fn constant_time_compare_matches_only_identical_byte_slices() {
        assert!(crate::auth::constant_time_compare_bytes(
            b"abc123", b"abc123"
        ));
        assert!(!crate::auth::constant_time_compare_bytes(
            b"abc123", b"abc124"
        ));
        assert!(!crate::auth::constant_time_compare_bytes(b"abc", b"abc1"));
        assert!(!crate::auth::constant_time_compare_bytes(b"", b"a"));
        assert!(crate::auth::constant_time_compare_bytes(b"", b""));
    }

    #[test]
    fn warns_for_remote_http_public_base_url() {
        assert!(uses_insecure_remote_public_base_url(
            "http://monitor.example.com",
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 8080)),
        ));
        assert!(uses_insecure_remote_public_base_url(
            "http://203.0.113.10:8080",
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 8080)),
        ));
    }

    #[test]
    fn ignores_local_or_tls_public_base_url() {
        assert!(!uses_insecure_remote_public_base_url(
            "https://monitor.example.com",
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 8080)),
        ));
        assert!(!uses_insecure_remote_public_base_url(
            "http://127.0.0.1:8080",
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 8080)),
        ));
        assert!(!uses_insecure_remote_public_base_url(
            "http://localhost:8080",
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 8080)),
        ));
    }

    #[test]
    fn server_readiness_tracks_dependency_health() {
        let readiness = ServerReadiness::new(true);
        assert!(readiness.is_ready());
        assert_eq!(readiness.status_label(), "ok");

        readiness.mark_registry_reload_healthy(false);
        assert!(!readiness.is_ready());
        assert_eq!(readiness.status_label(), "degraded");

        readiness.mark_registry_reload_healthy(true);
        readiness.mark_history_available(false);
        assert!(!readiness.is_ready());
        assert!(!readiness.history_available());
    }

    #[test]
    fn install_endpoints_disable_caching() {
        let runtime = Runtime::new().expect("runtime should build");
        runtime.block_on(async {
            let script_response = install_agent_script().await;
            assert_eq!(
                script_response.headers().get(header::CACHE_CONTROL),
                Some(&header::HeaderValue::from_static(
                    "no-store, no-cache, must-revalidate",
                )),
            );
            assert_eq!(
                script_response.headers().get(header::PRAGMA),
                Some(&header::HeaderValue::from_static("no-cache")),
            );

            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should be monotonic enough")
                .as_nanos();
            let temp_dir =
                std::env::temp_dir().join(format!("ximonitor-bootstrap-cache-test-{unique}"));
            std::fs::create_dir_all(&temp_dir).expect("temp dir should exist");
            let registry_path = temp_dir.join("server.json");
            let issued = issue_node(
                &registry_path,
                IssueNodeRequest {
                    node_id: "osaka-01".to_string(),
                    node_label: Some("Osaka 01".to_string()),
                    tags: Vec::new(),
                    rotate_token: false,
                },
            )
            .await
            .expect("node should be issued");
            let config = Arc::new(ServerConfig {
                listen: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 8080)),
                public_base_url: "https://monitor.example.com".to_string(),
                insecure_allow_http: false,
                readonly_auth: None,
                ws: WsConfig {
                    max_total_connections: 32,
                    max_connections_per_ip: 8,
                    auth_fail_window_secs: 300,
                    auth_fail_max_attempts: 6,
                    auth_block_secs: 600,
                },
                node_registry_path: registry_path.clone(),
                history_db_path: temp_dir.join("history.sqlite3"),
                snapshot_path: temp_dir.join("snapshot.json"),
                stale_after_secs: 20,
                ping_interval_secs: 10,
                max_message_bytes: 65536,
                refresh_interval_secs: 5,
                ignored_filesystems: vec![],
                agent_release_base_url: None,
                agent_release_sha256_x86_64: None,
                agent_release_sha256_aarch64: None,
            });
            let state = AppState {
                history: HistoryStore::new(config.history_db_path.clone()),
                install_admission: InstallAdmissionController::new(InstallAdmissionConfig {
                    auth_fail_window_secs: 300,
                    auth_fail_max_attempts: 6,
                    auth_block_secs: 600,
                }),
                verify_2fa_admission: InstallAdmissionController::new(InstallAdmissionConfig {
                    auth_fail_window_secs: 300,
                    auth_fail_max_attempts: 6,
                    auth_block_secs: 600,
                }),
                readiness: ServerReadiness::new(false),
                registry: NodeRegistry::load(&registry_path)
                    .await
                    .expect("registry should load"),
                shared: SharedState::new(config),
                ws_admission: WsAdmissionController::new(&WsConfig {
                    max_total_connections: 32,
                    max_connections_per_ip: 8,
                    auth_fail_window_secs: 300,
                    auth_fail_max_attempts: 6,
                    auth_block_secs: 600,
                }),
                readonly_auth: Arc::new(RwLock::new(ReadonlyRouteAuth::from_config(None))),
                two_factor_sessions: TwoFactorSessions::new(),
                config_path: Arc::new(temp_dir.join("server.toml")),
            };
            let request = Request::builder()
                .uri("/install/bootstrap")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", issued.install_token),
                )
                .body(Body::empty())
                .expect("request should build");
            let peer_addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 51234));
            let bootstrap_response = install_bootstrap(
                State(state),
                ConnectInfo(peer_addr),
                HeaderMap::new(),
                request,
            )
            .await;
            assert_eq!(bootstrap_response.status(), StatusCode::OK);
            assert_eq!(
                bootstrap_response.headers().get(header::CACHE_CONTROL),
                Some(&header::HeaderValue::from_static(
                    "no-store, no-cache, must-revalidate",
                )),
            );
            assert_eq!(
                bootstrap_response.headers().get(header::PRAGMA),
                Some(&header::HeaderValue::from_static("no-cache")),
            );

            let _ = std::fs::remove_dir_all(&temp_dir);
        });
    }

    #[test]
    fn sanitize_snapshot_clamps_invalid_metrics() {
        let config = ServerConfig {
            listen: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 8080)),
            public_base_url: "http://127.0.0.1:8080".to_string(),
            insecure_allow_http: false,
            readonly_auth: None,
            ws: WsConfig {
                max_total_connections: 32,
                max_connections_per_ip: 8,
                auth_fail_window_secs: 300,
                auth_fail_max_attempts: 6,
                auth_block_secs: 600,
            },
            node_registry_path: PathBuf::from("./data/server.json"),
            history_db_path: PathBuf::from("./data/history.sqlite3"),
            snapshot_path: PathBuf::from("./data/snapshot.json"),
            stale_after_secs: 15,
            ping_interval_secs: 5,
            max_message_bytes: 64 * 1024,
            refresh_interval_secs: 5,
            ignored_filesystems: vec!["tmpfs".to_string()],
            agent_release_base_url: None,
            agent_release_sha256_x86_64: None,
            agent_release_sha256_aarch64: None,
        };
        let snapshot = NodeSnapshot {
            collected_at: Utc::now(),
            cpu_usage_percent: f64::INFINITY,
            load: ximonitor_proto::LoadAverage {
                one: -1.0,
                five: f64::NAN,
                fifteen: 2_000_000.0,
            },
            memory: ximonitor_proto::MemoryUsage {
                total_bytes: 100,
                used_bytes: 200,
                available_bytes: 100,
                swap_total_bytes: 50,
                swap_used_bytes: 99,
            },
            uptime_secs: 5,
            disks: vec![
                ximonitor_proto::DiskUsage {
                    device: " /dev/vda1 ".to_string(),
                    mount_point: " / ".to_string(),
                    fs_type: " ext4 ".to_string(),
                    total_bytes: 100,
                    available_bytes: 80,
                    used_bytes: 90,
                    used_percent: 999.0,
                },
                ximonitor_proto::DiskUsage {
                    device: "tmp".to_string(),
                    mount_point: "/run".to_string(),
                    fs_type: "tmpfs".to_string(),
                    total_bytes: 1,
                    available_bytes: 0,
                    used_bytes: 1,
                    used_percent: 100.0,
                },
                ximonitor_proto::DiskUsage {
                    device: " ".to_string(),
                    mount_point: "/bad".to_string(),
                    fs_type: "xfs".to_string(),
                    total_bytes: 100,
                    available_bytes: 10,
                    used_bytes: 90,
                    used_percent: 90.0,
                },
            ],
            network: ximonitor_proto::NetworkCounters {
                total_rx_bytes: 1,
                total_tx_bytes: 2,
                rx_bytes_per_sec: Some(-10.0),
                tx_bytes_per_sec: Some(f64::INFINITY),
            },
        };

        let (sanitized, report) = sanitize_snapshot(&config, snapshot);
        assert_eq!(sanitized.cpu_usage_percent, 100.0);
        assert_eq!(sanitized.load.five, 0.0);
        assert_eq!(sanitized.load.fifteen, MAX_SANITIZED_LOAD);
        assert_eq!(sanitized.memory.used_bytes, 100);
        assert_eq!(sanitized.memory.available_bytes, 0);
        assert_eq!(sanitized.memory.swap_used_bytes, 50);
        assert_eq!(sanitized.network.rx_bytes_per_sec, Some(0.0));
        assert_eq!(
            sanitized.network.tx_bytes_per_sec,
            Some(MAX_SANITIZED_RATE_BYTES_PER_SEC)
        );
        assert_eq!(sanitized.disks.len(), 1);
        assert_eq!(sanitized.disks[0].device, "/dev/vda1");
        assert_eq!(sanitized.disks[0].mount_point, "/");
        assert_eq!(sanitized.disks[0].fs_type, "ext4");
        assert_eq!(sanitized.disks[0].used_bytes, 20);
        assert_eq!(sanitized.disks[0].used_percent, 20.0);
        assert_eq!(report.clamped_percents, 1);
        assert_eq!(report.clamped_loads, 3);
        assert_eq!(report.clamped_memory_bytes, 1);
        assert_eq!(report.clamped_disk_bytes, 1);
        assert_eq!(report.dropped_disks, 1);
        assert_eq!(report.sanitized_rates, 2);
        assert!(report.modified());
    }

    #[test]
    fn sanitize_caps_disk_field_string_length() {
        let config = ServerConfig {
            listen: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 8080)),
            public_base_url: "http://127.0.0.1:8080".to_string(),
            insecure_allow_http: false,
            readonly_auth: None,
            ws: WsConfig {
                max_total_connections: 32,
                max_connections_per_ip: 8,
                auth_fail_window_secs: 300,
                auth_fail_max_attempts: 6,
                auth_block_secs: 600,
            },
            node_registry_path: PathBuf::from("./data/server.json"),
            history_db_path: PathBuf::from("./data/history.sqlite3"),
            snapshot_path: PathBuf::from("./data/snapshot.json"),
            stale_after_secs: 15,
            ping_interval_secs: 5,
            max_message_bytes: 64 * 1024,
            refresh_interval_secs: 5,
            ignored_filesystems: Vec::new(),
            agent_release_base_url: None,
            agent_release_sha256_x86_64: None,
            agent_release_sha256_aarch64: None,
        };
        let oversized = "x".repeat(MAX_SANITIZED_STRING_BYTES * 4);
        let snapshot = NodeSnapshot {
            collected_at: Utc::now(),
            cpu_usage_percent: 10.0,
            load: ximonitor_proto::LoadAverage {
                one: 0.0,
                five: 0.0,
                fifteen: 0.0,
            },
            memory: ximonitor_proto::MemoryUsage {
                total_bytes: 100,
                used_bytes: 50,
                available_bytes: 50,
                swap_total_bytes: 0,
                swap_used_bytes: 0,
            },
            uptime_secs: 1,
            disks: vec![ximonitor_proto::DiskUsage {
                device: format!("/dev/{oversized}"),
                mount_point: format!("/mnt/{oversized}"),
                fs_type: oversized.clone(),
                total_bytes: 100,
                available_bytes: 50,
                used_bytes: 50,
                used_percent: 50.0,
            }],
            network: ximonitor_proto::NetworkCounters {
                total_rx_bytes: 0,
                total_tx_bytes: 0,
                rx_bytes_per_sec: None,
                tx_bytes_per_sec: None,
            },
        };

        let (sanitized, report) = sanitize_snapshot(&config, snapshot);
        assert_eq!(sanitized.disks.len(), 1);
        assert!(sanitized.disks[0].device.len() <= MAX_SANITIZED_STRING_BYTES);
        assert!(sanitized.disks[0].mount_point.len() <= MAX_SANITIZED_STRING_BYTES);
        assert!(sanitized.disks[0].fs_type.len() <= MAX_SANITIZED_STRING_BYTES);
        assert_eq!(report.truncated_strings, 1);
        assert!(report.modified());
    }

    #[test]
    fn sanitize_snapshot_caps_disk_count_and_tracks_clean_reports() {
        let config = ServerConfig {
            listen: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 8080)),
            public_base_url: "http://127.0.0.1:8080".to_string(),
            insecure_allow_http: false,
            readonly_auth: None,
            ws: WsConfig {
                max_total_connections: 32,
                max_connections_per_ip: 8,
                auth_fail_window_secs: 300,
                auth_fail_max_attempts: 6,
                auth_block_secs: 600,
            },
            node_registry_path: PathBuf::from("./data/server.json"),
            history_db_path: PathBuf::from("./data/history.sqlite3"),
            snapshot_path: PathBuf::from("./data/snapshot.json"),
            stale_after_secs: 15,
            ping_interval_secs: 5,
            max_message_bytes: 64 * 1024,
            refresh_interval_secs: 5,
            ignored_filesystems: Vec::new(),
            agent_release_base_url: None,
            agent_release_sha256_x86_64: None,
            agent_release_sha256_aarch64: None,
        };
        let disks = (0..(MAX_SANITIZED_DISKS + 3))
            .map(|index| ximonitor_proto::DiskUsage {
                device: format!("/dev/vd{index}"),
                mount_point: format!("/mnt/{index}"),
                fs_type: "ext4".to_string(),
                total_bytes: 100,
                available_bytes: 40,
                used_bytes: 60,
                used_percent: 60.0,
            })
            .collect();
        let snapshot = NodeSnapshot {
            collected_at: Utc::now(),
            cpu_usage_percent: 10.0,
            load: ximonitor_proto::LoadAverage {
                one: 0.5,
                five: 0.7,
                fifteen: 0.9,
            },
            memory: ximonitor_proto::MemoryUsage {
                total_bytes: 100,
                used_bytes: 60,
                available_bytes: 40,
                swap_total_bytes: 10,
                swap_used_bytes: 5,
            },
            uptime_secs: 1,
            disks,
            network: ximonitor_proto::NetworkCounters {
                total_rx_bytes: 1,
                total_tx_bytes: 2,
                rx_bytes_per_sec: Some(3.0),
                tx_bytes_per_sec: Some(4.0),
            },
        };

        let (sanitized, report) = sanitize_snapshot(&config, snapshot);
        assert_eq!(sanitized.disks.len(), MAX_SANITIZED_DISKS);
        assert_eq!(report.dropped_disks, 3);
        assert!(report.modified());

        // clean 报告不应推动 anomaly 窗口前进;modified 报告才入窗口。
        let mut window: std::collections::VecDeque<std::time::Instant> =
            std::collections::VecDeque::new();
        let now = std::time::Instant::now();
        let clean_report = SanitizationReport::default();
        update_metric_anomaly_window(&mut window, &clean_report, now);
        assert!(window.is_empty());

        // 在窗口内攒满 METRIC_ANOMALY_SESSION_LIMIT 条 → 触发断连。
        for tick in 0..METRIC_ANOMALY_SESSION_LIMIT {
            update_metric_anomaly_window(
                &mut window,
                &report,
                now + std::time::Duration::from_secs(tick as u64),
            );
        }
        assert!(should_disconnect_for_metric_anomalies(&window));
    }

    #[test]
    fn sanitize_snapshot_deduplicates_repeated_disk_devices() {
        let config = ServerConfig {
            listen: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 8080)),
            public_base_url: "http://127.0.0.1:8080".to_string(),
            insecure_allow_http: false,
            readonly_auth: None,
            ws: WsConfig {
                max_total_connections: 32,
                max_connections_per_ip: 8,
                auth_fail_window_secs: 300,
                auth_fail_max_attempts: 6,
                auth_block_secs: 600,
            },
            node_registry_path: PathBuf::from("./data/server.json"),
            history_db_path: PathBuf::from("./data/history.sqlite3"),
            snapshot_path: PathBuf::from("./data/snapshot.json"),
            stale_after_secs: 15,
            ping_interval_secs: 5,
            max_message_bytes: 64 * 1024,
            refresh_interval_secs: 5,
            ignored_filesystems: Vec::new(),
            agent_release_base_url: None,
            agent_release_sha256_x86_64: None,
            agent_release_sha256_aarch64: None,
        };
        let snapshot = NodeSnapshot {
            collected_at: Utc::now(),
            cpu_usage_percent: 1.0,
            load: ximonitor_proto::LoadAverage {
                one: 0.1,
                five: 0.1,
                fifteen: 0.1,
            },
            memory: ximonitor_proto::MemoryUsage {
                total_bytes: 100,
                used_bytes: 50,
                available_bytes: 50,
                swap_total_bytes: 0,
                swap_used_bytes: 0,
            },
            uptime_secs: 60,
            disks: vec![
                ximonitor_proto::DiskUsage {
                    device: "/dev/vda1".to_string(),
                    mount_point: "/".to_string(),
                    fs_type: "ext4".to_string(),
                    total_bytes: 100,
                    available_bytes: 40,
                    used_bytes: 60,
                    used_percent: 60.0,
                },
                ximonitor_proto::DiskUsage {
                    device: "/dev/vda1".to_string(),
                    mount_point: "/var".to_string(),
                    fs_type: "ext4".to_string(),
                    total_bytes: 100,
                    available_bytes: 40,
                    used_bytes: 60,
                    used_percent: 60.0,
                },
                ximonitor_proto::DiskUsage {
                    device: "/dev/vdb".to_string(),
                    mount_point: "/ssd".to_string(),
                    fs_type: "ext4".to_string(),
                    total_bytes: 200,
                    available_bytes: 100,
                    used_bytes: 100,
                    used_percent: 50.0,
                },
            ],
            network: ximonitor_proto::NetworkCounters {
                total_rx_bytes: 1,
                total_tx_bytes: 2,
                rx_bytes_per_sec: Some(3.0),
                tx_bytes_per_sec: Some(4.0),
            },
        };

        let (sanitized, report) = sanitize_snapshot(&config, snapshot);
        assert_eq!(sanitized.disks.len(), 2);
        assert_eq!(sanitized.disks[0].mount_point, "/");
        assert_eq!(sanitized.disks[1].mount_point, "/ssd");
        assert_eq!(report.dropped_disks, 1);
    }

    #[test]
    fn truncate_to_byte_boundary_respects_char_boundary() {
        // "中" 在 UTF-8 中占 3 字节;cutoff = 7 必须回退到 6 字节边界。
        let mut value = "中".repeat(100);
        truncate_to_byte_boundary(&mut value, 7);
        assert!(value.len() <= 7);
        assert!(value.is_char_boundary(value.len()));
        assert!(value.chars().all(|ch| ch == '中'));

        // 已经在限内的字符串保持不变。
        let mut short = "abc".to_string();
        truncate_to_byte_boundary(&mut short, 16);
        assert_eq!(short, "abc");
    }

    #[test]
    fn loopback_listener_uses_forwarded_ip_for_ws_limits() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            "198.51.100.24".parse().expect("header value"),
        );

        let client_ip = resolve_client_ip(
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 8080)),
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 51234)),
            &headers,
        );

        assert_eq!(client_ip, IpAddr::V4("198.51.100.24".parse().expect("ip")));
    }

    #[test]
    fn rightmost_forwarded_ip_is_preferred_over_spoofed_leftmost() {
        // 反代会把客户端发来的 XFF 与真实远端 IP 顺序拼接,真实 IP 出现在最右侧。
        // 最左端可能是客户端伪造的值,绝不能用来做"信任来源"判定。
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            "8.8.8.8, 198.51.100.24".parse().expect("header value"),
        );

        let client_ip = resolve_client_ip(
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 8080)),
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 51234)),
            &headers,
        );

        assert_eq!(client_ip, IpAddr::V4("198.51.100.24".parse().expect("ip")));
    }

    #[test]
    fn x_real_ip_takes_precedence_over_forwarded_for() {
        // Nginx 推荐同时下发 X-Real-IP 与 X-Forwarded-For;X-Real-IP 来自反代
        // 本身的 $remote_addr,客户端无法影响,优先级最高。
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            "8.8.8.8, 1.1.1.1".parse().expect("header value"),
        );
        headers.insert("x-real-ip", "198.51.100.24".parse().expect("header value"));

        let client_ip = resolve_client_ip(
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 8080)),
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 51234)),
            &headers,
        );

        assert_eq!(client_ip, IpAddr::V4("198.51.100.24".parse().expect("ip")));
    }

    #[test]
    fn repeated_auth_failures_trigger_ws_block() {
        let controller = WsAdmissionController::new(&WsConfig {
            max_total_connections: 16,
            max_connections_per_ip: 4,
            auth_fail_window_secs: 60,
            auth_fail_max_attempts: 2,
            auth_block_secs: 300,
        });
        let client_ip = IpAddr::V4("198.51.100.24".parse().expect("ip"));

        controller.record_auth_failure(client_ip);
        controller.record_auth_failure(client_ip);

        match controller.try_acquire(client_ip) {
            Err(WsAdmissionError::Blocked { retry_after_secs }) => {
                assert!(retry_after_secs > 0);
            }
            _ => panic!("client should be temporarily blocked"),
        }
    }

    #[test]
    fn metric_anomaly_window_decays_so_long_sessions_avoid_false_positive_kicks() {
        // 旧实现:METRIC_ANOMALY_SESSION_LIMIT 是会话生命周期内的累计上限,
        // 因此长跑节点偶发 5 次异常就会被踢。
        // 新实现:计数滑动到 METRIC_ANOMALY_WINDOW_SECS 之外即衰减,只有
        // "在同一窗口内连续超阈值"才触发断连。
        use std::collections::VecDeque;
        use std::time::{Duration, Instant};

        let mut window: VecDeque<Instant> = VecDeque::new();
        let report = SanitizationReport {
            clamped_percents: 1,
            ..SanitizationReport::default()
        };

        // 模拟一个 24 小时的长会话,每隔 1 小时遇到一次偶发的 sanitize 修正。
        // 任何两次 anomaly 的间隔(3600 s)都远大于窗口长度(默认 300 s),
        // 因此每次入队前老条目都已被剔除,窗口始终最多只有 1 条。
        let started_at = Instant::now();
        for hour in 0..24 {
            let now = started_at + Duration::from_secs(hour * 3600);
            update_metric_anomaly_window(&mut window, &report, now);
            assert!(
                !should_disconnect_for_metric_anomalies(&window),
                "long session with sparse anomalies should never be kicked",
            );
        }

        // 反过来,同一窗口内的高频异常 → 窗口内累计达到阈值 → 触发断连。
        let burst_at = started_at + Duration::from_secs(48 * 3600);
        for tick in 0..METRIC_ANOMALY_SESSION_LIMIT {
            update_metric_anomaly_window(
                &mut window,
                &report,
                burst_at + Duration::from_secs(tick as u64),
            );
        }
        assert!(
            should_disconnect_for_metric_anomalies(&window),
            "burst within the window must still trigger the kick",
        );
    }

    #[test]
    fn sweep_drops_expired_failure_entries_and_keeps_live_blocks() {
        // 验证 sweep:已过期且未封禁的条目被移除;仍封禁的条目保留;
        // 仍在统计窗口内的失败条目保留。
        use std::collections::{HashMap, VecDeque};
        use std::time::{Duration, Instant};

        use crate::admission::AuthFailureState;

        let mut failures: HashMap<IpAddr, AuthFailureState> = HashMap::new();
        let now = Instant::now();
        let window = Duration::from_secs(60);

        // 1. 过期 + 未封禁 → 应被 sweep 删除
        let expired_ip: IpAddr = "203.0.113.10".parse().expect("ip");
        let mut expired = AuthFailureState::default();
        expired
            .recent_failures
            .push_back(now - Duration::from_secs(3600));
        failures.insert(expired_ip, expired);

        // 2. 已封禁但封禁未到期 → 应保留
        let blocked_ip: IpAddr = "203.0.113.20".parse().expect("ip");
        let blocked = AuthFailureState {
            recent_failures: VecDeque::new(),
            blocked_until: Some(now + Duration::from_secs(300)),
        };
        failures.insert(blocked_ip, blocked);

        // 3. 窗口内的失败 → 应保留
        let recent_ip: IpAddr = "203.0.113.30".parse().expect("ip");
        let mut recent = AuthFailureState::default();
        recent
            .recent_failures
            .push_back(now - Duration::from_secs(10));
        failures.insert(recent_ip, recent);

        sweep_expired_auth_failures(&mut failures, now, window);

        assert!(
            !failures.contains_key(&expired_ip),
            "expired entry should be removed",
        );
        assert!(
            failures.contains_key(&blocked_ip),
            "active block should be preserved",
        );
        assert!(
            failures.contains_key(&recent_ip),
            "in-window failure should be preserved",
        );
    }

    #[test]
    fn install_token_format_short_circuits_obvious_garbage() {
        // 32-byte hex token = 64 lowercase hex chars 才是合法格式;
        // 任何不符合的输入应在落到 registry flock 之前就被拒掉。
        let valid = "a".repeat(64);
        assert!(is_well_formed_install_token(&valid));
        assert!(!is_well_formed_install_token(""));
        assert!(!is_well_formed_install_token(&"a".repeat(63)));
        assert!(!is_well_formed_install_token(&"a".repeat(65)));
        // 大写不被接受 —— 与 generate_token 的 lowercase hex 输出对齐。
        assert!(!is_well_formed_install_token(&"A".repeat(64)));
        // 非 hex 字符
        assert!(!is_well_formed_install_token(&"z".repeat(64)));
    }

    #[test]
    fn install_admission_blocks_after_repeated_failures() {
        let controller = InstallAdmissionController::new(InstallAdmissionConfig {
            auth_fail_window_secs: 60,
            auth_fail_max_attempts: 2,
            auth_block_secs: 300,
        });
        let client_ip: IpAddr = "198.51.100.24".parse().expect("ip");

        // 阈值前应放行
        assert!(controller.check(client_ip).is_ok());
        controller.record_auth_failure(client_ip);
        controller.record_auth_failure(client_ip);

        match controller.check(client_ip) {
            Err(retry_after_secs) => assert!(retry_after_secs > 0),
            Ok(()) => panic!("client should be temporarily blocked after threshold"),
        }
    }
}
