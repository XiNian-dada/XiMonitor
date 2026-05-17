//! WebSocket 与 install 端点的准入控制。
//!
//! 三件事在一起:
//! 1. [`WsAdmissionController`] 对 `/ws` 做"总量 + 单 IP"并发限流,叠加认证失败封禁。
//! 2. [`InstallAdmissionController`] 仅按 IP 做认证失败封禁,因为 install 是
//!    一次性短请求,没有"活动连接数"概念,被复用为 `/api/verify-2fa` 的 IP 限流。
//! 3. [`resolve_client_ip`] 在反向代理场景下解析真实客户端 IP,以及 [`AuthFailureState`]
//!    及其 prune/sweep helpers 这两个 controller 共享。

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use nodelite_proto::WsConfig;

/// 软上限:超过后顺手扫一次表清掉已过期条目。攻击者用大量伪造 IP 制造
/// 一次性失败时,本表只在攻击侧累积代价,稳态体积可控。
const WS_AUTH_FAILURE_TABLE_SOFT_LIMIT: usize = 1024;
const INSTALL_AUTH_FAILURE_TABLE_SOFT_LIMIT: usize = 1024;

/// 单个 IP 的认证失败历史。
#[derive(Debug, Default)]
pub struct AuthFailureState {
    pub recent_failures: VecDeque<Instant>,
    pub blocked_until: Option<Instant>,
}

/// WebSocket 准入控制器:封装总量限流、IP 限流与认证失败封禁。
#[derive(Clone)]
pub struct WsAdmissionController {
    config: WsConfig,
    state: Arc<Mutex<WsAdmissionState>>,
}

#[derive(Debug, Default)]
struct WsAdmissionState {
    total_active_connections: usize,
    active_by_ip: HashMap<IpAddr, usize>,
    auth_failures: HashMap<IpAddr, AuthFailureState>,
}

/// RAII 句柄:存在意味着占用一个 WebSocket 连接槽,析构时归还。
pub struct WsConnectionPermit {
    controller: WsAdmissionController,
    client_ip: IpAddr,
}

/// 准入失败的具体原因,对应到不同的 HTTP 响应。
#[derive(Debug)]
pub enum WsAdmissionError {
    TotalCapacity,
    IpCapacity,
    Blocked { retry_after_secs: u64 },
}

impl WsAdmissionController {
    pub fn new(config: &WsConfig) -> Self {
        Self {
            config: config.clone(),
            state: Arc::new(Mutex::new(WsAdmissionState::default())),
        }
    }

    /// 尝试占用一个 WebSocket 连接配额。
    ///
    /// 返回 RAII 句柄;它一旦析构,连接计数会被自动回退,无需手动 release。
    pub fn try_acquire(&self, client_ip: IpAddr) -> Result<WsConnectionPermit, WsAdmissionError> {
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
    pub fn record_auth_failure(&self, client_ip: IpAddr) {
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
    pub fn clear_auth_failures(&self, client_ip: IpAddr) {
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

    /// 锁住状态;锁中毒说明上一次持锁路径发生 panic,此时重置为安全默认值。
    fn lock_state(&self) -> std::sync::MutexGuard<'_, WsAdmissionState> {
        self.state.lock().unwrap_or_else(|poisoned| {
            tracing::error!("WsAdmissionController mutex poisoned; resetting admission state");
            let mut guard = poisoned.into_inner();
            *guard = WsAdmissionState::default();
            self.state.clear_poison();
            guard
        })
    }
}

impl Drop for WsConnectionPermit {
    fn drop(&mut self) {
        self.controller.release_connection(self.client_ip);
    }
}

/// `/install/bootstrap` 与 `/api/verify-2fa` 共用的 IP 维度认证失败计数器,
/// 与 `WsAdmissionController` 的封禁逻辑完全同型,但不维护"活动连接数":
/// 这些路径是一次性短请求,只需要按 IP 限制无效尝试,防止远端攻击者把
/// registry flock + IO 或 TOTP 校验当作免费 DoS 通道。
#[derive(Clone)]
pub struct InstallAdmissionController {
    config: InstallAdmissionConfig,
    state: Arc<Mutex<InstallAdmissionState>>,
}

#[derive(Debug, Clone, Copy)]
pub struct InstallAdmissionConfig {
    pub auth_fail_window_secs: u64,
    pub auth_fail_max_attempts: usize,
    pub auth_block_secs: u64,
}

#[derive(Debug, Default)]
struct InstallAdmissionState {
    auth_failures: HashMap<IpAddr, AuthFailureState>,
}

impl InstallAdmissionController {
    pub fn new(config: InstallAdmissionConfig) -> Self {
        Self {
            config,
            state: Arc::new(Mutex::new(InstallAdmissionState::default())),
        }
    }

    /// 检查给定 client_ip 是否仍处于封禁窗口。封禁中返回 `retry_after_secs`;
    /// 否则正常放行(不递增计数 —— `record_auth_failure` 才会递增)。
    pub fn check(&self, client_ip: IpAddr) -> Result<(), u64> {
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

    pub fn record_auth_failure(&self, client_ip: IpAddr) {
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

    pub fn clear_auth_failures(&self, client_ip: IpAddr) {
        let mut state = self.lock_state();
        state.auth_failures.remove(&client_ip);
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, InstallAdmissionState> {
        self.state.lock().unwrap_or_else(|poisoned| {
            tracing::error!("InstallAdmissionController mutex poisoned; resetting admission state");
            let mut guard = poisoned.into_inner();
            *guard = InstallAdmissionState::default();
            self.state.clear_poison();
            guard
        })
    }
}

/// 解析客户端真实 IP。
///
/// 当 TCP 直连对端是本机反代时,允许读取 `X-Forwarded-For` / `X-Real-IP`;
/// 其他直连请求一律使用 socket peer IP,避免公网客户端伪造代理头。
pub fn resolve_client_ip(
    _listen: SocketAddr,
    peer_addr: SocketAddr,
    headers: &HeaderMap,
) -> IpAddr {
    if !peer_addr.ip().is_loopback() {
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
pub fn prune_auth_failure_state(
    state: &mut AuthFailureState,
    now: Instant,
    failure_window: Duration,
) {
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
pub fn sweep_expired_auth_failures(
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
pub fn ws_admission_error_response(error: WsAdmissionError) -> Response {
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

#[cfg(test)]
mod tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::str::FromStr;

    use super::*;

    fn test_ws_config() -> WsConfig {
        WsConfig {
            max_total_connections: 2,
            max_connections_per_ip: 1,
            auth_fail_window_secs: 60,
            auth_fail_max_attempts: 2,
            auth_block_secs: 60,
        }
    }

    fn test_install_config() -> InstallAdmissionConfig {
        InstallAdmissionConfig {
            auth_fail_window_secs: 60,
            auth_fail_max_attempts: 2,
            auth_block_secs: 60,
        }
    }

    #[test]
    fn websocket_admission_resets_state_after_mutex_poison() {
        let controller = WsAdmissionController::new(&test_ws_config());
        let client_ip = IpAddr::from_str("198.51.100.10").expect("valid test IP");

        let _ = catch_unwind(AssertUnwindSafe(|| {
            let mut state = controller.state.lock().expect("lock state");
            state.total_active_connections = 99;
            state.active_by_ip.insert(client_ip, 99);
            panic!("poison admission state");
        }));

        let permit = controller
            .try_acquire(client_ip)
            .expect("poisoned state should be reset before admitting new sessions");
        drop(permit);
        assert!(controller.state.lock().is_ok());
    }

    #[test]
    fn install_admission_resets_state_after_mutex_poison() {
        let controller = InstallAdmissionController::new(test_install_config());
        let client_ip = IpAddr::from_str("198.51.100.11").expect("valid test IP");

        let _ = catch_unwind(AssertUnwindSafe(|| {
            let mut state = controller.state.lock().expect("lock state");
            state.auth_failures.insert(
                client_ip,
                AuthFailureState {
                    recent_failures: VecDeque::from([Instant::now(), Instant::now()]),
                    blocked_until: Some(Instant::now() + Duration::from_secs(60)),
                },
            );
            panic!("poison install admission state");
        }));

        assert!(controller.check(client_ip).is_ok());
        assert!(controller.state.lock().is_ok());
    }
}
