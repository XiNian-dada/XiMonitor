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

mod history;
mod registry;
mod snapshot;
mod state;
mod ui;

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use axum::extract::ConnectInfo;
use axum::extract::Query;
use axum::extract::Request;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path as AxumPath, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{Next, from_fn_with_state};
use axum::response::{AppendHeaders, Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use chrono::{TimeZone, Utc};
use clap::{Parser, Subcommand};
use futures::{SinkExt, StreamExt};
use getrandom::fill as fill_random;
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::net::TcpListener;
use tokio::time::{MissedTickBehavior, interval};
use totp_lite::{totp_custom, Sha1};
use tower_http::trace::TraceLayer;
use tracing::{error, info, warn};
use url::Url;
use ximonitor_proto::{
    DiskUsage, HelloMessage, LoadAverage, MemoryUsage, MetricsMessage, NetworkCounters,
    NodeSnapshot, PingMessage, PongMessage, ReadonlyAuthConfig, ServerConfig,
    ServerNoticeMessage, WireMessage, WsConfig, parse_server_config, percentage,
};

use crate::history::HistoryStore;
use crate::registry::{
    IssueNodeRequest, NodeRegistry, build_install_script_url, default_agent_release_base_url,
    issue_node, render_agent_config, render_install_command, render_upgrade_command,
};
use crate::snapshot::{load_snapshot, persist_snapshot, spawn_snapshot_persistor};
use crate::state::SharedState;
use crate::ui::{UI_I18N_JSON, index_html, node_html};

/// 顶层命令行参数。
#[derive(Debug, Parser)]
#[command(name = "ximonitor-server")]
#[command(about = "XiMonitor central server")]
struct Cli {
    /// 配置文件路径,默认 `config/server.toml`。
    #[arg(long, global = true, default_value = "config/server.toml")]
    config: PathBuf,
    /// 可选子命令。不指定时进入"启动 Web 服务"模式。
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// 颁发节点凭证(仅打印,不安装到 Agent 节点上)。
    IssueNode(NodeCommandArgs),
    /// 颁发节点凭证并打印 Agent 上的安装命令。
    InstallAgent(NodeCommandArgs),
    /// 打印就地升级 Agent 所需的命令。
    UpgradeAgent,
}

/// 节点相关命令的共享参数。
#[derive(Debug, Parser, Clone)]
struct NodeCommandArgs {
    #[arg(long)]
    node_id: String,
    #[arg(long)]
    node_label: Option<String>,
    #[arg(long = "tag")]
    tags: Vec<String>,
    /// 是否强制轮换该节点的现有 token。
    #[arg(long)]
    rotate_token: bool,
}

/// 在各处理器之间共享的运行时上下文。
#[derive(Clone)]
struct AppState {
    history: HistoryStore,
    install_admission: InstallAdmissionController,
    readiness: ServerReadiness,
    registry: NodeRegistry,
    shared: SharedState,
    ws_admission: WsAdmissionController,
    readonly_auth: ReadonlyRouteAuth,
    two_factor_sessions: TwoFactorSessions,
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

/// 包装 HTTP 基本认证,用于保护 `/api/*` 与 HTML 视图。
#[derive(Debug, Clone)]
struct ReadonlyRouteAuth {
    expected_authorization: Option<String>,
    enable_2fa: bool,
    totp_secret: Option<Vec<u8>>,
}

/// 服务端内存中的 2FA 会话票据。
///
/// Cookie 里只保存随机 token,真正的有效期与状态留在服务端内存里,避免前端
/// 伪造 `ximonitor_auth=verified` 之类的静态 cookie 绕过二次验证。
#[derive(Debug, Clone)]
struct TwoFactorSessions {
    inner: Arc<Mutex<TwoFactorSessionStore>>,
}

#[derive(Debug, Default)]
struct TwoFactorSessionStore {
    pending: HashMap<String, Instant>,
    authenticated: HashMap<String, Instant>,
}

/// 2FA 验证请求。
#[derive(Debug, Deserialize)]
struct Verify2FARequest {
    code: String,
}

/// 2FA 验证失败响应。
#[derive(Debug, Serialize)]
struct Verify2FAError {
    error: String,
}

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

/// CLI 中 `issue-node` / `install-agent` 共享的产出结构。
struct IssuedNodeBundle {
    issued: crate::registry::IssueNodeResult,
    install_command: String,
    install_script_url: String,
    agent_release_base_url: String,
}

/// WebSocket 处理流程中的错误来源区分:
/// `Client` 表示因对方原因(协议错误、未认证)而断开,只记 warn;
/// `Server` 表示我们这边出现异常,记 error。
#[derive(Debug)]
enum ProtocolError {
    Client(String),
    Server(anyhow::Error),
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

/// WebSocket 准入控制器:封装总量限流、IP 限流与认证失败封禁。
#[derive(Clone)]
struct WsAdmissionController {
    config: WsConfig,
    state: Arc<Mutex<WsAdmissionState>>,
}

#[derive(Debug, Default)]
struct WsAdmissionState {
    total_active_connections: usize,
    active_by_ip: HashMap<IpAddr, usize>,
    auth_failures: HashMap<IpAddr, AuthFailureState>,
}

/// 单个 IP 的认证失败历史。
#[derive(Debug, Default)]
struct AuthFailureState {
    recent_failures: VecDeque<Instant>,
    blocked_until: Option<Instant>,
}

/// RAII 句柄:存在意味着占用一个 WebSocket 连接槽,析构时归还。
struct WsConnectionPermit {
    controller: WsAdmissionController,
    client_ip: IpAddr,
}

/// 准入失败的具体原因,对应到不同的 HTTP 响应。
enum WsAdmissionError {
    TotalCapacity,
    IpCapacity,
    Blocked { retry_after_secs: u64 },
}

/// `/install/bootstrap` 的认证失败计数器,与 `WsAdmissionController` 的封禁逻辑
/// 完全同型,但不维护"活动连接数":install 路径是一次性短请求,只需要按 IP
/// 限制无效 token 尝试,防止远端攻击者把 registry flock + IO 当作免费 DoS 通道。
#[derive(Clone)]
struct InstallAdmissionController {
    config: InstallAdmissionConfig,
    state: Arc<Mutex<InstallAdmissionState>>,
}

#[derive(Debug, Clone, Copy)]
struct InstallAdmissionConfig {
    auth_fail_window_secs: u64,
    auth_fail_max_attempts: usize,
    auth_block_secs: u64,
}

#[derive(Debug, Default)]
struct InstallAdmissionState {
    auth_failures: HashMap<IpAddr, AuthFailureState>,
}

/// 软上限:超过后顺手扫一次表清掉已过期条目,与 #28 中的 ws 失败表同源问题。
const INSTALL_AUTH_FAILURE_TABLE_SOFT_LIMIT: usize = 1024;

impl InstallAdmissionController {
    fn new(config: InstallAdmissionConfig) -> Self {
        Self {
            config,
            state: Arc::new(Mutex::new(InstallAdmissionState::default())),
        }
    }

    /// 检查给定 client_ip 是否仍处于封禁窗口。封禁中返回 `retry_after_secs`;
    /// 否则正常放行(不递增计数 —— `record_auth_failure` 才会递增)。
    fn check(&self, client_ip: IpAddr) -> Result<(), u64> {
        let now = Instant::now();
        let mut state = self.lock_state();
        let failure_window = Duration::from_secs(self.config.auth_fail_window_secs);
        let failure_state = state.auth_failures.entry(client_ip).or_default();
        prune_auth_failure_state(failure_state, now, failure_window);
        if let Some(blocked_until) = failure_state.blocked_until
            && blocked_until > now
        {
            return Err(blocked_until.duration_since(now).as_secs().max(1));
        }
        if failure_state.recent_failures.is_empty() && failure_state.blocked_until.is_none() {
            state.auth_failures.remove(&client_ip);
        }
        Ok(())
    }

    fn record_auth_failure(&self, client_ip: IpAddr) {
        let now = Instant::now();
        let failure_window = Duration::from_secs(self.config.auth_fail_window_secs);
        let mut state = self.lock_state();
        let failure_state = state.auth_failures.entry(client_ip).or_default();
        prune_auth_failure_state(failure_state, now, failure_window);
        failure_state.recent_failures.push_back(now);
        if failure_state.recent_failures.len() >= self.config.auth_fail_max_attempts {
            failure_state.blocked_until =
                Some(now + Duration::from_secs(self.config.auth_block_secs));
            failure_state.recent_failures.clear();
        }
        if state.auth_failures.len() > INSTALL_AUTH_FAILURE_TABLE_SOFT_LIMIT {
            sweep_expired_auth_failures(&mut state.auth_failures, now, failure_window);
        }
    }

    fn clear_auth_failures(&self, client_ip: IpAddr) {
        let mut state = self.lock_state();
        state.auth_failures.remove(&client_ip);
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, InstallAdmissionState> {
        self.state.lock().unwrap_or_else(|poisoned| {
            tracing::error!(
                "InstallAdmissionController mutex poisoned; recovering with stale state"
            );
            poisoned.into_inner()
        })
    }
}

/// 把 `scripts/install-agent.sh` 在编译期嵌入到二进制内。
const INSTALL_AGENT_SCRIPT: &str = include_str!("../../scripts/install-agent.sh");
/// 等待 Hello 报文的超时时间(秒)。
const HELLO_TIMEOUT_SECS: u64 = 10;
/// 同时未应答的 Ping 上限,超过后会丢弃最老的一条,避免内存占用无限增长。
const MAX_OUTSTANDING_PINGS: usize = 32;
/// 不安全传输警告的输出间隔(秒)。
const INSECURE_TRANSPORT_WARN_INTERVAL_SECS: u64 = 900;
/// Basic Auth 通过后等待输入 TOTP 的窗口。
const TWO_FACTOR_PENDING_SECS: u64 = 300;
/// 2FA 完成后的浏览器会话有效期。
const TWO_FACTOR_AUTH_SECS: u64 = 24 * 60 * 60;
const TWO_FACTOR_PENDING_COOKIE: &str = "ximonitor_2fa_pending";
const TWO_FACTOR_AUTH_COOKIE: &str = "ximonitor_auth";
/// 历史采样中允许的最大磁盘条目数,防止恶意 Agent 制造海量条目。
const MAX_SANITIZED_DISKS: usize = 64;
/// 单个磁盘字段(device/mount_point/fs_type)允许的最大字节数,防止 Agent 上报巨型字符串撑爆 UI 与历史库。
const MAX_SANITIZED_STRING_BYTES: usize = 256;
/// 网络速率字段的合法上限(字节/秒)。
const MAX_SANITIZED_RATE_BYTES_PER_SEC: f64 = 1_000_000_000_000.0;
/// 负载平均数的合法上限。
const MAX_SANITIZED_LOAD: f64 = 1_000_000.0;
/// 一个 WebSocket 会话在 `METRIC_ANOMALY_WINDOW_SECS` 窗口内允许出现的异常
/// metrics 报告次数,超过即主动断开。
/// 滑动窗口的设计避免了"长会话偶发异常累积"造成的误判:任何 anomaly 在窗口
/// 过去之后都会被忽略,只有真正持续上报异常的 Agent 才会触发断连。
const METRIC_ANOMALY_SESSION_LIMIT: usize = 5;
/// 计算 anomaly 触发阈值时使用的滑动窗口(秒),默认 5 分钟。
const METRIC_ANOMALY_WINDOW_SECS: u64 = 300;
/// 历史接口默认查询窗口(小时)。
const DEFAULT_HISTORY_WINDOW_HOURS: u64 = 24;
/// 历史接口默认返回的样本点数。
const DEFAULT_HISTORY_MAX_POINTS: usize = 480;
/// 历史接口允许的最大样本点数。
const MAX_HISTORY_MAX_POINTS: usize = 1440;
/// WebSocket 认证失败表的软上限:超过该规模后,`record_auth_failure` 会顺手
/// 做一次全表扫描,清理已过期且未封禁的 IP 条目。攻击者用大量伪造 IP 制造
/// 一次性失败时,本表只在攻击侧累积代价,稳态体积可控。
const WS_AUTH_FAILURE_TABLE_SOFT_LIMIT: usize = 1024;

/// 历史查询接口的查询字符串参数。
///
/// `start` / `end` 必须同时提供;否则使用 `window_hours` 表示"过去 N 小时"。
#[derive(Debug, Deserialize, Default)]
struct HistoryQuery {
    window_hours: Option<u64>,
    max_points: Option<usize>,
    start: Option<i64>,
    end: Option<i64>,
}

impl From<anyhow::Error> for ProtocolError {
    fn from(error: anyhow::Error) -> Self {
        Self::Server(error)
    }
}

impl ReadonlyRouteAuth {
    /// 根据可选的基本认证配置预先计算"期望的 Authorization 头",免去每次请求都重新编码。
    fn from_config(config: Option<ReadonlyAuthConfig>) -> Self {
        let (expected_authorization, enable_2fa, totp_secret) = match config {
            Some(config) => {
                let credentials = format!("{}:{}", config.username, config.password);
                let encoded = base64::engine::general_purpose::STANDARD.encode(credentials);
                let auth = format!("Basic {encoded}");

                let totp_secret = if config.enable_2fa {
                    config
                        .totp_secret
                        .as_deref()
                        .and_then(decode_totp_secret)
                } else {
                    None
                };

                (Some(auth), config.enable_2fa, totp_secret)
            }
            None => (None, false, None),
        };

        Self {
            expected_authorization,
            enable_2fa,
            totp_secret,
        }
    }

    /// 判断单次请求是否带有合法的 Basic 凭证;未启用认证时直接放行。
    fn is_authorized(&self, request: &Request) -> bool {
        let Some(expected_authorization) = self.expected_authorization.as_deref() else {
            return true;
        };

        request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            == Some(expected_authorization)
    }
}

impl TwoFactorSessions {
    fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(TwoFactorSessionStore::default())),
        }
    }

    fn create_pending(&self) -> Result<String> {
        let token = generate_session_token()?;
        let expires_at = Instant::now() + Duration::from_secs(TWO_FACTOR_PENDING_SECS);
        let mut store = lock_mutex(&self.inner);
        prune_expired_sessions(&mut store, Instant::now());
        store.pending.insert(token.clone(), expires_at);
        Ok(token)
    }

    fn pending_exists(&self, token: &str) -> bool {
        let mut store = lock_mutex(&self.inner);
        prune_expired_sessions(&mut store, Instant::now());
        store.pending.contains_key(token)
    }

    fn consume_pending(&self, token: &str) {
        let mut store = lock_mutex(&self.inner);
        store.pending.remove(token);
    }

    fn create_authenticated(&self) -> Result<String> {
        let token = generate_session_token()?;
        let expires_at = Instant::now() + Duration::from_secs(TWO_FACTOR_AUTH_SECS);
        let mut store = lock_mutex(&self.inner);
        prune_expired_sessions(&mut store, Instant::now());
        store.authenticated.insert(token.clone(), expires_at);
        Ok(token)
    }

    fn is_authenticated(&self, token: &str) -> bool {
        let mut store = lock_mutex(&self.inner);
        prune_expired_sessions(&mut store, Instant::now());
        store.authenticated.contains_key(token)
    }

    fn remove_authenticated(&self, token: &str) {
        let mut store = lock_mutex(&self.inner);
        store.authenticated.remove(token);
    }
}

fn prune_expired_sessions(store: &mut TwoFactorSessionStore, now: Instant) {
    store.pending.retain(|_, expires_at| *expires_at > now);
    store.authenticated.retain(|_, expires_at| *expires_at > now);
}

fn lock_mutex<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| {
        warn!("mutex poisoned; recovering with stale state");
        poisoned.into_inner()
    })
}

fn generate_session_token() -> Result<String> {
    let mut bytes = [0_u8; 32];
    fill_random(&mut bytes).context("failed to gather secure random bytes")?;
    Ok(hex_encode(&bytes))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn decode_totp_secret(value: &str) -> Option<Vec<u8>> {
    let normalized = value.replace(' ', "").to_ascii_uppercase();
    base32::decode(base32::Alphabet::Rfc4648 { padding: false }, &normalized)
        .or_else(|| base32::decode(base32::Alphabet::Rfc4648 { padding: true }, &normalized))
}

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

impl WsAdmissionController {
    fn new(config: &WsConfig) -> Self {
        Self {
            config: config.clone(),
            state: Arc::new(Mutex::new(WsAdmissionState::default())),
        }
    }

    /// 尝试占用一个 WebSocket 连接配额。
    ///
    /// 返回 RAII 句柄;它一旦析构,连接计数会被自动回退,无需手动 release。
    fn try_acquire(&self, client_ip: IpAddr) -> Result<WsConnectionPermit, WsAdmissionError> {
        let now = Instant::now();
        let mut state = self.lock_state();
        let failure_window = Duration::from_secs(self.config.auth_fail_window_secs);
        let failure_state = state.auth_failures.entry(client_ip).or_default();
        prune_auth_failure_state(failure_state, now, failure_window);
        if let Some(blocked_until) = failure_state.blocked_until
            && blocked_until > now
        {
            return Err(WsAdmissionError::Blocked {
                retry_after_secs: blocked_until.duration_since(now).as_secs().max(1),
            });
        }
        if failure_state.recent_failures.is_empty() && failure_state.blocked_until.is_none() {
            state.auth_failures.remove(&client_ip);
        }

        if state.total_active_connections >= self.config.max_total_connections {
            return Err(WsAdmissionError::TotalCapacity);
        }
        let active_for_ip = state.active_by_ip.get(&client_ip).copied().unwrap_or(0);
        if active_for_ip >= self.config.max_connections_per_ip {
            return Err(WsAdmissionError::IpCapacity);
        }

        state.total_active_connections = state.total_active_connections.saturating_add(1);
        state.active_by_ip.insert(client_ip, active_for_ip + 1);

        Ok(WsConnectionPermit {
            controller: self.clone(),
            client_ip,
        })
    }

    /// 记录一次认证失败,达到阈值后把客户端 IP 临时封禁。
    fn record_auth_failure(&self, client_ip: IpAddr) {
        let now = Instant::now();
        let failure_window = Duration::from_secs(self.config.auth_fail_window_secs);
        let mut state = self.lock_state();
        let failure_state = state.auth_failures.entry(client_ip).or_default();
        prune_auth_failure_state(failure_state, now, failure_window);
        failure_state.recent_failures.push_back(now);
        if failure_state.recent_failures.len() >= self.config.auth_fail_max_attempts {
            failure_state.blocked_until =
                Some(now + Duration::from_secs(self.config.auth_block_secs));
            failure_state.recent_failures.clear();
        }
        // 攻击者可以用一波伪造 IP 把失败表撑大,而单 IP 后续不再回访就不会被
        // `try_acquire` / `prune_auth_failure_state` 主动清理 —— 在长跑实例里
        // 这是一条慢速内存泄漏。表过大时顺手做一次全表扫描,把已过期且未封禁
        // 的条目删掉;代价 O(N) 但摊销到攻击侧,稳态查询路径仍然 O(1)。
        if state.auth_failures.len() > WS_AUTH_FAILURE_TABLE_SOFT_LIMIT {
            sweep_expired_auth_failures(&mut state.auth_failures, now, failure_window);
        }
    }

    /// 认证成功后清理该 IP 的失败历史。
    fn clear_auth_failures(&self, client_ip: IpAddr) {
        let mut state = self.lock_state();
        state.auth_failures.remove(&client_ip);
    }

    /// 由 `WsConnectionPermit::drop` 调用,把计数减回去。
    fn release_connection(&self, client_ip: IpAddr) {
        let mut state = self.lock_state();
        state.total_active_connections = state.total_active_connections.saturating_sub(1);
        if let Some(active_for_ip) = state.active_by_ip.get_mut(&client_ip) {
            *active_for_ip = active_for_ip.saturating_sub(1);
            if *active_for_ip == 0 {
                state.active_by_ip.remove(&client_ip);
            }
        }
    }

    /// 锁住状态;锁中毒时取出受污染状态继续,因为不愿意因此让整个服务崩溃。
    fn lock_state(&self) -> std::sync::MutexGuard<'_, WsAdmissionState> {
        self.state.lock().unwrap_or_else(|poisoned| {
            tracing::error!("WsAdmissionController mutex poisoned; recovering with stale state");
            poisoned.into_inner()
        })
    }
}

impl Drop for WsConnectionPermit {
    fn drop(&mut self) {
        self.controller.release_connection(self.client_ip);
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
        readiness,
        registry,
        shared,
        ws_admission: WsAdmissionController::new(&config.ws),
        readonly_auth: readonly_route_auth.clone(),
        two_factor_sessions: TwoFactorSessions::new(),
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
        .route_layer(from_fn_with_state(
            state.clone(),
            require_readonly_auth,
        ));
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

/// `server issue-node`:创建/更新节点并打印对应的 agent.toml 与安装命令。
async fn issue_node_command(config_path: &Path, args: NodeCommandArgs) -> Result<()> {
    let config = load_server_config(config_path).await?;
    let bundle = issue_node_bundle(&config, &args).await?;
    let agent_config = render_agent_config(&config.public_base_url, &bundle.issued.node)?;
    let action = if bundle.issued.created {
        "created"
    } else if bundle.issued.rotated_token {
        "rotated"
    } else {
        "reused"
    };

    println!("node_id: {}", bundle.issued.node.node_id);
    println!("node_label: {}", bundle.issued.node.node_label);
    println!("status: {action}");
    println!("registry_path: {}", config.node_registry_path.display());
    println!("install_script_url: {}", bundle.install_script_url);
    println!("agent_release_base_url: {}", bundle.agent_release_base_url);
    println!(
        "install_token_expires_at: {}",
        bundle.issued.install_token_expires_at.to_rfc3339()
    );
    println!();
    println!("# agent.toml");
    println!("{agent_config}");
    println!("# install command");
    println!("{}", bundle.install_command);
    println!();
    println!("note: the install command above already embeds a one-time install token.");

    Ok(())
}

/// `server install-agent`:只打印安装命令,适合管道式使用。
async fn install_agent_command(config_path: &Path, args: NodeCommandArgs) -> Result<()> {
    let config = load_server_config(config_path).await?;
    let bundle = issue_node_bundle(&config, &args).await?;
    println!("{}", bundle.install_command);
    Ok(())
}

/// `server upgrade-agent`:打印就地升级现有 Agent 的命令。
async fn upgrade_agent_command(config_path: &Path) -> Result<()> {
    let config = load_server_config(config_path).await?;
    let agent_release_base_url = default_agent_release_base_url()?;
    let upgrade_command = render_upgrade_command(&config.public_base_url, &agent_release_base_url)?;
    println!("{upgrade_command}");
    Ok(())
}

/// 同时完成"节点登记"和"安装命令渲染",供两个 CLI 子命令复用。
async fn issue_node_bundle(
    config: &ServerConfig,
    args: &NodeCommandArgs,
) -> Result<IssuedNodeBundle> {
    let issued = issue_node(
        config.node_registry_path.as_path(),
        IssueNodeRequest {
            node_id: args.node_id.clone(),
            node_label: args.node_label.clone(),
            tags: args.tags.clone(),
            rotate_token: args.rotate_token,
        },
    )
    .await?;

    let agent_release_base_url = default_agent_release_base_url()?;
    let install_command = render_install_command(
        &config.public_base_url,
        &issued.install_token,
        &agent_release_base_url,
    )?;
    let install_script_url = build_install_script_url(&config.public_base_url)?;

    Ok(IssuedNodeBundle {
        issued,
        install_command,
        install_script_url,
        agent_release_base_url,
    })
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

/// 首页 HTML:把刷新周期等参数注入模板。
async fn index(State(state): State<AppState>) -> Html<String> {
    Html(index_html(state.shared.config().refresh_interval_secs))
}

/// 节点详情页 HTML。
async fn node_detail(
    State(state): State<AppState>,
    AxumPath(node_id): AxumPath<String>,
) -> Html<String> {
    Html(node_html(
        &node_id,
        state.shared.config().refresh_interval_secs,
    ))
}

/// 把前端 i18n 字典作为静态 JSON 文件提供。
async fn ui_i18n_asset() -> Response {
    (
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        UI_I18N_JSON,
    )
        .into_response()
}

/// 2FA 验证页面。
async fn verify_2fa_page() -> Html<&'static str> {
    Html(include_str!("../assets/verify-2fa.html"))
}

/// 2FA 验证 API:验证 TOTP 码,成功后设置完整认证 cookie。
async fn verify_2fa_api(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<Verify2FARequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Verify2FAError>)> {
    let Some(pending_token) = cookie_value(&headers, TWO_FACTOR_PENDING_COOKIE) else {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(Verify2FAError {
                error: "No pending 2FA session".to_string(),
            }),
        ));
    };

    if !state.two_factor_sessions.pending_exists(&pending_token) {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(Verify2FAError {
                error: "2FA session expired".to_string(),
            }),
        ));
    }

    // 验证 TOTP 码
    if !verify_totp(&state, &request.code) {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(Verify2FAError {
                error: "Invalid code".to_string(),
            }),
        ));
    }

    let auth_token = state.two_factor_sessions.create_authenticated().map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(Verify2FAError {
                error: "Failed to create authenticated session".to_string(),
            }),
        )
    })?;
    state.two_factor_sessions.consume_pending(&pending_token);
    let secure = secure_cookies(&state);

    // 验证成功:设置只包含随机票据的完整认证 cookie。
    Ok((
        StatusCode::OK,
        AppendHeaders([
            auth_cookie(TWO_FACTOR_AUTH_COOKIE, &auth_token, TWO_FACTOR_AUTH_SECS, secure),
            expire_cookie(TWO_FACTOR_PENDING_COOKIE, secure),
        ]),
    ))
}

/// 验证 TOTP 码是否正确。
fn verify_totp(state: &AppState, code: &str) -> bool {
    let Some(ref secret) = state.readonly_auth.totp_secret else {
        return false;
    };

    // 验证码必须是 6 位数字
    if code.len() != 6 || !code.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }

    // 获取当前时间戳 (30 秒为一个周期)
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    // 验证当前时间窗口及前后各一个窗口 (允许 ±30 秒时钟偏差)
    for time_offset in [-1, 0, 1] {
        let time_step = (now as i64 + time_offset * 30) as u64 / 30;
        let expected = totp_custom::<Sha1>(30, 6, secret, time_step);
        let expected_str = format!("{:06}", expected);
        if expected_str == code {
            return true;
        }
    }

    false
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let prefix = format!("{name}=");
    headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|cookie_header| {
            cookie_header.split(';').find_map(|cookie| {
                let cookie = cookie.trim();
                cookie
                    .strip_prefix(prefix.as_str())
                    .map(ToString::to_string)
            })
        })
}

fn auth_cookie(
    name: &'static str,
    value: &str,
    max_age_secs: u64,
    secure: bool,
) -> (header::HeaderName, String) {
    let secure_suffix = if secure { "; Secure" } else { "" };
    (
        header::SET_COOKIE,
        format!(
            "{name}={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age_secs}{secure_suffix}"
        ),
    )
}

fn expire_cookie(name: &'static str, secure: bool) -> (header::HeaderName, String) {
    let secure_suffix = if secure { "; Secure" } else { "" };
    (
        header::SET_COOKIE,
        format!("{name}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0{secure_suffix}"),
    )
}

fn secure_cookies(state: &AppState) -> bool {
    state.shared.config().public_base_url.starts_with("https://")
}

/// 健康检查接口,始终返回 200。
async fn healthz() -> StatusCode {
    StatusCode::OK
}

/// 就绪检查接口:仅当关键依赖均可用时返回 200,否则返回 503。
async fn readyz(State(state): State<AppState>) -> StatusCode {
    if state.readiness.is_ready() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

/// 登出并强制重新认证:返回 401 + WWW-Authenticate 头,触发浏览器清除缓存的
/// Basic Auth 凭据。前端在检测到认证过期(24 小时)时会跳转到此路由。
async fn logout_and_reauth(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(token) = cookie_value(&headers, TWO_FACTOR_AUTH_COOKIE) {
        state.two_factor_sessions.remove_authenticated(&token);
    }
    let secure = secure_cookies(&state);
    (
        StatusCode::UNAUTHORIZED,
        AppendHeaders([
            expire_cookie(TWO_FACTOR_AUTH_COOKIE, secure),
            expire_cookie(TWO_FACTOR_PENDING_COOKIE, secure),
            (
                header::WWW_AUTHENTICATE,
                "Basic realm=\"XiMonitor\"".to_string(),
            ),
        ]),
        "Session expired. Please log in again.",
    )
        .into_response()
}

/// 中间件:对受保护路由强制基本认证;放行时把 Request 继续交给下一个处理器。
async fn require_readonly_auth(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Response {
    let auth = &state.readonly_auth;

    // 如果未启用认证,直接放行
    if auth.expected_authorization.is_none() {
        return next.run(request).await;
    }

    // 如果启用了 2FA,先检查是否有完整认证 cookie
    if auth.enable_2fa {
        if cookie_value(&headers, TWO_FACTOR_AUTH_COOKIE)
            .as_deref()
            .is_some_and(|token| state.two_factor_sessions.is_authenticated(token))
        {
            // 已完成 2FA 验证
            return next.run(request).await;
        }

        // 检查 Basic Auth
        if auth.is_authorized(&request) {
            // Basic Auth 通过,但需要 2FA 验证
            // 设置服务端随机 pending token 并重定向到 2FA 页面。
            let pending_token = match state.two_factor_sessions.create_pending() {
                Ok(token) => token,
                Err(error) => {
                    error!(error = ?error, "failed to create pending 2FA session");
                    return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                }
            };
            let secure = secure_cookies(&state);

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
            return next.run(request).await;
        }
    }

    // 认证失败
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Basic realm=\"XiMonitor\"")],
        "authentication required",
    )
        .into_response()
}

/// 提供给前端读取的"引导信息":服务名、刷新周期与已登记节点数。
async fn bootstrap(State(state): State<AppState>) -> impl IntoResponse {
    Json(BootstrapResponse {
        service: "ximonitor-server",
        status: state.readiness.status_label(),
        ready: state.readiness.is_ready(),
        history_available: state.readiness.history_available(),
        public_base_url: state.shared.config().public_base_url.clone(),
        refresh_interval_secs: state.shared.config().refresh_interval_secs,
        registered_nodes: state.registry.count().await,
    })
}

/// 暴露内置安装脚本,供 `curl | sh` 模式安装 Agent 时下载。
async fn install_agent_script() -> Response {
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
async fn install_bootstrap(
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
                "Bearer realm=\"XiMonitor Installer\"",
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

/// install token 在 [registry.rs](../registry.rs) 中由 `generate_token` 生成,
/// 固定为 32 字节随机数的 lowercase hex —— 即 64 字符 0-9a-f。这里的格式检查
/// 让显然不合法的输入在落到文件锁前就被拒掉。
fn is_well_formed_install_token(token: &str) -> bool {
    token.len() == 64
        && token
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// 仪表盘顶部的总览数据。
async fn overview(State(state): State<AppState>) -> impl IntoResponse {
    Json(state.shared.overview().await)
}

/// 所有节点的最新状态。
async fn nodes(State(state): State<AppState>) -> impl IntoResponse {
    Json(state.shared.list_statuses().await)
}

/// 单个节点的最新状态;不存在时返回 404。
async fn node_status(
    State(state): State<AppState>,
    AxumPath(node_id): AxumPath<String>,
) -> Response {
    match state.shared.get_status(&node_id).await {
        Some(status) => Json(status).into_response(),
        None => (StatusCode::NOT_FOUND, "node not found").into_response(),
    }
}

/// 节点历史趋势接口。支持"过去 N 小时"或"指定区间"两种调用方式。
async fn node_history(
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

/// `/ws` 入口:在 WebSocket 升级前先做准入检查与帧大小限制。
async fn ws_handler(
    State(state): State<AppState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
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
}

/// 一次完整的 WebSocket 会话:握手 → 认证 → 数据循环 → 资源回收。
async fn handle_socket(
    state: AppState,
    client_ip: IpAddr,
    _connection_permit: WsConnectionPermit,
    mut socket: WebSocket,
) -> Result<(), ProtocolError> {
    let shared = state.shared.clone();
    let hello = match tokio::time::timeout(
        Duration::from_secs(HELLO_TIMEOUT_SECS),
        recv_hello(&mut socket),
    )
    .await
    {
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
    };
    let session_token = hello.token.clone();
    let identity = match state
        .registry
        .authorize(&hello.identity, &session_token)
        .await
    {
        Ok(identity) => identity,
        Err(error) => {
            warn!(
                client_ip = %client_ip,
                requested_node_id = %hello.identity.node_id,
                error = ?error,
                "websocket authentication rejected",
            );
            state.ws_admission.record_auth_failure(client_ip);

            // 检查是否是 token 过期错误,如果是则返回具体错误信息
            let error_msg = error.to_string();
            if error_msg.contains("token expired") {
                return Err(ProtocolError::Client("token expired".to_string()));
            } else {
                return Err(ProtocolError::Client("unauthorized".to_string()));
            }
        }
    };
    state.ws_admission.clear_auth_failures(client_ip);

    let node_id = identity.node_id.clone();
    let node_label = identity.node_label.clone();
    let session_id = shared
        .register_node(identity, Some(client_ip.to_string()))
        .await;

    info!(node_id = %node_id, node_label = %node_label, session_id, "node authenticated");

    let session_result: Result<(), ProtocolError> = async {
        let notice = WireMessage::ServerNotice(ServerNoticeMessage {
            level: ximonitor_proto::NoticeLevel::Info,
            message: "authenticated".to_string(),
        });
        send_wire_message(&mut socket, &notice).await?;

        let (mut sender, mut receiver) = socket.split();
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
                                if !state.registry.is_token_current(&node_id, &session_token).await {
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
                            WireMessage::Pong(PongMessage { nonce }) => {
                                if !state.registry.is_token_current(&node_id, &session_token).await {
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
                                if request.node_id != node_id {
                                    break Err(ProtocolError::Client("refresh token request node_id mismatch".to_string()));
                                }
                                match state.registry.refresh_token(&node_id).await {
                                    Ok((new_token, expires_at)) => {
                                        let response = WireMessage::RefreshTokenResponse(
                                            ximonitor_proto::RefreshTokenResponseMessage {
                                                new_token,
                                                expires_at: expires_at.to_rfc3339(),
                                            },
                                        );
                                        let payload = serde_json::to_string(&response)
                                            .map_err(|error| anyhow!("failed to serialize refresh response: {error}"))?;
                                        sender
                                            .send(Message::Text(payload.into()))
                                            .await
                                            .map_err(|error| anyhow!("failed to send refresh response: {error}"))?;
                                        info!(node_id = %node_id, "token refreshed successfully");
                                    }
                                    Err(error) => {
                                        warn!(node_id = %node_id, error = ?error, "failed to refresh token");
                                        let notice = WireMessage::ServerNotice(ServerNoticeMessage {
                                            level: ximonitor_proto::NoticeLevel::Error,
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
                _ = ping_ticker.tick() => {
                    if !shared.is_current_session(&node_id, session_id).await {
                        warn!(node_id = %node_id, session_id, "closing superseded websocket session");
                        break Ok(());
                    }
                    if !state.registry.is_token_current(&node_id, &session_token).await {
                        warn!(node_id = %node_id, "closing websocket session after registry token change");
                        break Ok(());
                    }

                    prune_outstanding_pings(&mut outstanding_pings, ping_expiry);
                    let nonce = next_ping_nonce;
                    next_ping_nonce = next_ping_nonce.saturating_add(1);
                    outstanding_pings.insert(nonce, Instant::now());
                    let ping = serde_json::to_string(&WireMessage::Ping(PingMessage { nonce }))
                        .map_err(|error| anyhow!("failed to serialize ping: {error}"))?;
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

/// 解析客户端真实 IP。
///
/// 当 Server 仅监听回环地址(典型的反向代理部署),允许从 `X-Forwarded-For` / `X-Real-IP` 中读取上游 IP;
/// 否则直接使用 TCP 连接的对端地址,避免被恶意请求伪造来源。
fn resolve_client_ip(listen: SocketAddr, peer_addr: SocketAddr, headers: &HeaderMap) -> IpAddr {
    if !listen.ip().is_loopback() {
        return peer_addr.ip();
    }

    forwarded_ip_from_headers(headers).unwrap_or_else(|| peer_addr.ip())
}

fn forwarded_ip_from_headers(headers: &HeaderMap) -> Option<IpAddr> {
    // 优先信任直连反代写入的 X-Real-IP(Nginx 等会把它设为反代看到的对端 IP);
    // 退而求其次取 X-Forwarded-For 最右端 —— 该位置由可信反代追加,
    // 最左端则是客户端可任意写入的值,直接使用会被攻击者用来伪造 IP 绕过
    // 单 IP 限流与认证失败封禁。
    headers
        .get("x-real-ip")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(parse_ip_addr)
        .or_else(|| {
            headers
                .get("x-forwarded-for")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.rsplit(',').next())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .and_then(parse_ip_addr)
        })
}

fn parse_ip_addr(value: &str) -> Option<IpAddr> {
    value.parse::<IpAddr>().ok()
}

/// 清理失败计数与封禁状态:把已过期的项目逐出,使长时间不活跃的 IP 不会被无端封禁。
fn prune_auth_failure_state(state: &mut AuthFailureState, now: Instant, failure_window: Duration) {
    while state
        .recent_failures
        .front()
        .is_some_and(|timestamp| now.duration_since(*timestamp) > failure_window)
    {
        state.recent_failures.pop_front();
    }

    if state
        .blocked_until
        .is_some_and(|blocked_until| blocked_until <= now)
    {
        state.blocked_until = None;
    }
}

/// 全表扫描:对每个 IP 的失败状态做一次 `prune_auth_failure_state`,然后丢弃
/// 那些既无未过期失败、也无未到期封禁的条目。代价 O(N),由 `record_auth_failure`
/// 在表大小超过软上限时触发,使被攻击场景下表的稳态体积可控。
fn sweep_expired_auth_failures(
    auth_failures: &mut HashMap<IpAddr, AuthFailureState>,
    now: Instant,
    failure_window: Duration,
) {
    auth_failures.retain(|_, failure_state| {
        prune_auth_failure_state(failure_state, now, failure_window);
        !failure_state.recent_failures.is_empty() || failure_state.blocked_until.is_some()
    });
}

/// 把准入控制错误映射成对应的 HTTP 响应。
fn ws_admission_error_response(error: WsAdmissionError) -> Response {
    match error {
        WsAdmissionError::TotalCapacity => (
            StatusCode::SERVICE_UNAVAILABLE,
            "websocket capacity exhausted; retry later",
        )
            .into_response(),
        WsAdmissionError::IpCapacity => (
            StatusCode::TOO_MANY_REQUESTS,
            "too many concurrent websocket sessions for this client",
        )
            .into_response(),
        WsAdmissionError::Blocked { retry_after_secs } => (
            StatusCode::TOO_MANY_REQUESTS,
            [(header::RETRY_AFTER, retry_after_secs.to_string())],
            "too many recent websocket authentication failures",
        )
            .into_response(),
    }
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

/// 对来自 Agent 的快照进行二次校验。
/// 把所有疑似越界的字段统一约束到合法范围,避免它们污染 UI 汇总、聚合或历史表。
///
/// 返回值中的 `SanitizationReport` 记录了本次清洗触发的各类异常计数,
/// 上层会话循环据此输出告警并在异常持续时主动断开连接。
fn sanitize_snapshot(
    config: &ServerConfig,
    mut snapshot: NodeSnapshot,
) -> (NodeSnapshot, SanitizationReport) {
    // Agent 是不受信任的数据源:在进入聚合 / 历史表前,把不可能值卡到上限,
    // 否则它们会扭曲仪表盘汇总、压垮加和、或污染历史样本。
    let mut report = SanitizationReport::default();
    snapshot.cpu_usage_percent =
        sanitize_percentage(snapshot.cpu_usage_percent, &mut report.clamped_percents);
    snapshot.load = sanitize_load_average(snapshot.load, &mut report);
    snapshot.memory = sanitize_memory_usage(snapshot.memory, &mut report);
    snapshot.network = sanitize_network_counters(snapshot.network, &mut report);
    let mut sanitized_disks = Vec::new();
    let mut seen_disk_devices = HashSet::new();
    for disk in snapshot.disks {
        if config
            .ignored_filesystems
            .iter()
            .any(|fs| fs == &disk.fs_type)
        {
            continue;
        }

        let Some(disk) = sanitize_disk_usage(disk, &mut report) else {
            continue;
        };
        let disk_identity = disk_device_identity(&disk);
        if !seen_disk_devices.insert(disk_identity) {
            report.dropped_disks = report.dropped_disks.saturating_add(1);
            continue;
        }
        if sanitized_disks.len() >= MAX_SANITIZED_DISKS {
            report.dropped_disks = report.dropped_disks.saturating_add(1);
            continue;
        }
        sanitized_disks.push(disk);
    }
    snapshot.disks = sanitized_disks;
    (snapshot, report)
}

/// 记录一次 `sanitize_snapshot` 期间各类清洗操作的发生次数。
///
/// 字段都按"被改动的次数"计;上层只关心 `modified()` 与各字段非零情况,
/// 不依赖于精确次数语义,所以使用 `saturating_add` 即可。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct SanitizationReport {
    clamped_percents: u32,
    clamped_loads: u32,
    clamped_memory_bytes: u32,
    clamped_disk_bytes: u32,
    truncated_strings: u32,
    dropped_disks: u32,
    sanitized_rates: u32,
}

impl SanitizationReport {
    fn total(&self) -> u32 {
        self.clamped_percents
            .saturating_add(self.clamped_loads)
            .saturating_add(self.clamped_memory_bytes)
            .saturating_add(self.clamped_disk_bytes)
            .saturating_add(self.truncated_strings)
            .saturating_add(self.dropped_disks)
            .saturating_add(self.sanitized_rates)
    }

    fn modified(&self) -> bool {
        self.total() > 0
    }
}

/// 把"本次清洗是否触发了 anomaly"折算到滑动窗口里。
///
/// `window` 中保留的是最近若干次 anomaly 的发生时刻;早于
/// `now - METRIC_ANOMALY_WINDOW_SECS` 的条目在此被剔除,确保长会话里
/// 偶发的 sanitize 修正不会无限累积成"会话级"误判。
fn update_metric_anomaly_window(
    window: &mut VecDeque<Instant>,
    report: &SanitizationReport,
    now: Instant,
) {
    let horizon = Duration::from_secs(METRIC_ANOMALY_WINDOW_SECS);
    while window
        .front()
        .is_some_and(|recorded_at| now.duration_since(*recorded_at) > horizon)
    {
        window.pop_front();
    }
    if report.modified() {
        window.push_back(now);
    }
}

fn should_disconnect_for_metric_anomalies(window: &VecDeque<Instant>) -> bool {
    window.len() >= METRIC_ANOMALY_SESSION_LIMIT
}

fn sanitize_percentage(value: f64, counter: &mut u32) -> f64 {
    let sanitized = sanitize_non_negative_f64(value, 100.0);
    if value != sanitized {
        *counter = counter.saturating_add(1);
    }
    sanitized
}

fn sanitize_non_negative_f64(value: f64, max: f64) -> f64 {
    if value.is_nan() || value < 0.0 {
        return 0.0;
    }
    if value.is_infinite() {
        return max;
    }

    value.min(max)
}

fn sanitize_load_average(load: LoadAverage, report: &mut SanitizationReport) -> LoadAverage {
    LoadAverage {
        one: sanitize_load_value(load.one, &mut report.clamped_loads),
        five: sanitize_load_value(load.five, &mut report.clamped_loads),
        fifteen: sanitize_load_value(load.fifteen, &mut report.clamped_loads),
    }
}

fn sanitize_load_value(value: f64, counter: &mut u32) -> f64 {
    let sanitized = sanitize_non_negative_f64(value, MAX_SANITIZED_LOAD);
    if value != sanitized {
        *counter = counter.saturating_add(1);
    }
    sanitized
}

fn sanitize_memory_usage(mut memory: MemoryUsage, report: &mut SanitizationReport) -> MemoryUsage {
    let original_used = memory.used_bytes;
    let original_available = memory.available_bytes;
    let original_swap_used = memory.swap_used_bytes;

    memory.used_bytes = memory.used_bytes.min(memory.total_bytes);
    memory.available_bytes = memory.available_bytes.min(memory.total_bytes);
    if memory.used_bytes.saturating_add(memory.available_bytes) > memory.total_bytes {
        // 当 used + available 大于 total 时,以 used 为准重新算 available,保持口径一致。
        memory.available_bytes = memory.total_bytes.saturating_sub(memory.used_bytes);
    }

    memory.swap_used_bytes = memory.swap_used_bytes.min(memory.swap_total_bytes);

    if memory.used_bytes != original_used
        || memory.available_bytes != original_available
        || memory.swap_used_bytes != original_swap_used
    {
        report.clamped_memory_bytes = report.clamped_memory_bytes.saturating_add(1);
    }
    memory
}

fn sanitize_disk_usage(mut disk: DiskUsage, report: &mut SanitizationReport) -> Option<DiskUsage> {
    disk.device = disk.device.trim().to_string();
    disk.mount_point = disk.mount_point.trim().to_string();
    disk.fs_type = disk.fs_type.trim().to_string();
    if disk.device.is_empty() || disk.mount_point.is_empty() || disk.fs_type.is_empty() {
        report.dropped_disks = report.dropped_disks.saturating_add(1);
        return None;
    }
    // 字符串字段做硬截断,避免 Agent 上报巨型字符串污染 UI 或历史库。
    let original_device_len = disk.device.len();
    let original_mount_len = disk.mount_point.len();
    let original_fs_len = disk.fs_type.len();
    truncate_to_byte_boundary(&mut disk.device, MAX_SANITIZED_STRING_BYTES);
    truncate_to_byte_boundary(&mut disk.mount_point, MAX_SANITIZED_STRING_BYTES);
    truncate_to_byte_boundary(&mut disk.fs_type, MAX_SANITIZED_STRING_BYTES);
    if disk.device.len() != original_device_len
        || disk.mount_point.len() != original_mount_len
        || disk.fs_type.len() != original_fs_len
    {
        report.truncated_strings = report.truncated_strings.saturating_add(1);
    }

    let original_used = disk.used_bytes;
    let original_available = disk.available_bytes;
    disk.available_bytes = disk.available_bytes.min(disk.total_bytes);
    disk.used_bytes = disk.used_bytes.min(disk.total_bytes);
    if disk.used_bytes.saturating_add(disk.available_bytes) > disk.total_bytes {
        // 当两个字段相互矛盾时,以 available 为基线重算 used,得到自洽的 used 部分。
        disk.used_bytes = disk.total_bytes.saturating_sub(disk.available_bytes);
    }
    if disk.used_bytes != original_used || disk.available_bytes != original_available {
        report.clamped_disk_bytes = report.clamped_disk_bytes.saturating_add(1);
    }
    // 这里 percentage() 输入是已被裁剪过的 u64,理论上不会越界;
    // 用 sanitize_percentage 仍能在未来字段语义改变时兜底,并保持 used_percent 与 used_bytes 自洽。
    disk.used_percent = sanitize_percentage(
        percentage(disk.used_bytes, disk.total_bytes),
        &mut report.clamped_percents,
    );
    Some(disk)
}

fn disk_device_identity(disk: &DiskUsage) -> String {
    format!("{}:{}", disk.device, disk.total_bytes)
}

fn sanitize_network_counters(
    mut network: NetworkCounters,
    report: &mut SanitizationReport,
) -> NetworkCounters {
    network.rx_bytes_per_sec = sanitize_optional_rate(
        network.rx_bytes_per_sec,
        MAX_SANITIZED_RATE_BYTES_PER_SEC,
        &mut report.sanitized_rates,
    );
    network.tx_bytes_per_sec = sanitize_optional_rate(
        network.tx_bytes_per_sec,
        MAX_SANITIZED_RATE_BYTES_PER_SEC,
        &mut report.sanitized_rates,
    );
    network
}

fn sanitize_optional_rate(value: Option<f64>, max: f64, counter: &mut u32) -> Option<f64> {
    value.map(|v| {
        let sanitized = sanitize_non_negative_f64(v, max);
        if v != sanitized {
            *counter = counter.saturating_add(1);
        }
        sanitized
    })
}

/// 把字符串截到不超过 `max_bytes` 字节,且必须落在 UTF-8 字符边界上。
fn truncate_to_byte_boundary(value: &mut String, max_bytes: usize) {
    if value.len() <= max_bytes {
        return;
    }
    let mut cutoff = max_bytes;
    while !value.is_char_boundary(cutoff) {
        cutoff -= 1;
    }
    value.truncate(cutoff);
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

    use super::{
        AppState, InstallAdmissionConfig, InstallAdmissionController, MAX_SANITIZED_DISKS,
        MAX_SANITIZED_LOAD, MAX_SANITIZED_RATE_BYTES_PER_SEC, MAX_SANITIZED_STRING_BYTES,
        METRIC_ANOMALY_SESSION_LIMIT, ReadonlyRouteAuth, SanitizationReport, ServerReadiness,
        WsAdmissionController, WsAdmissionError, bootstrap, healthz, index, install_agent_script,
        install_bootstrap, is_well_formed_install_token, node_detail, node_history, node_status,
        nodes, overview, readyz, resolve_client_ip, sanitize_snapshot,
        should_disconnect_for_metric_anomalies, sweep_expired_auth_failures,
        truncate_to_byte_boundary, ui_i18n_asset, update_metric_anomaly_window,
        uses_insecure_remote_public_base_url, ws_handler,
    };
    use crate::history::HistoryStore;
    use crate::registry::{IssueNodeRequest, NodeRegistry, issue_node};
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
            readonly_auth: ReadonlyRouteAuth::from_config(None),
            two_factor_sessions: TwoFactorSessions::new(),
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
                readonly_auth: ReadonlyRouteAuth::from_config(None),
                two_factor_sessions: TwoFactorSessions::new(),
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

        use super::AuthFailureState;

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
