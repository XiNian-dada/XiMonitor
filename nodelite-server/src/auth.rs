//! Web 面板的认证层:
//!
//! - [`ReadonlyRouteAuth`] 把 `[auth]` 配置预先转成"期望的 Basic 头"+ 2FA 配置;
//! - [`TwoFactorSessions`] 把 pending / authenticated 票据保留在服务端内存里,
//!   并跟踪每个 pending 的连续失败次数和已经被消费过的 TOTP `time_step`;
//! - 顶层 helper(`verify_totp_step` / `cookie_*` / 常量时间比较)只暴露纯输入
//!   输出的小函数,使路由层不需要关心 TOTP / Base32 / cookie 字符串细节。
//!
//! 这一层不直接持有 `AppState`,避免 main.rs 的总状态结构反过来产生循环依赖。
//! 调用方在 handler 里把 `AppState` 拆成所需字段后再调用本模块。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use axum::extract::Request;
use axum::http::{HeaderMap, header};
use base64::Engine;
use chrono::Utc;
use getrandom::fill as fill_random;
use nodelite_proto::{ReadonlyAuthConfig, ServerConfig, normalize_totp_secret};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use totp_lite::{Sha1, totp_custom};
use tracing::error;

use crate::encoding::hex_encode;

/// Basic Auth 通过后等待输入 TOTP 的窗口。
pub const TWO_FACTOR_PENDING_SECS: u64 = 300;
/// 2FA 完成后的浏览器会话有效期。
pub const TWO_FACTOR_AUTH_SECS: u64 = 24 * 60 * 60;
/// 单个 pending session 允许的最大 TOTP 错误尝试次数。达到后该 pending token
/// 立即失效,客户端必须重新通过 Basic Auth 才能再次进入 verify-2fa 页面。
/// 这与 `InstallAdmissionController` 的 IP 维度限流共同把 TOTP 暴力破解
/// 的代价压到不可接受的水平。
pub const TWO_FACTOR_MAX_FAILED_ATTEMPTS: u32 = 5;
/// 成功消费过的 TOTP `time_step` 在 store 中保留的时长。RFC 6238 §5.2
/// 要求拒绝同一 step 的重复使用;这里保留足够长的时间覆盖 step 边界附近
/// 的调度延迟,同时避免条目无界增长。
pub const TWO_FACTOR_TOTP_REPLAY_RETENTION_SECS: u64 = 150;
pub const TWO_FACTOR_PENDING_COOKIE: &str = "nodelite_2fa_pending";
pub const TWO_FACTOR_AUTH_COOKIE: &str = "nodelite_auth";

/// 包装 HTTP 基本认证,用于保护 `/api/*` 与 HTML 视图。
#[derive(Debug, Clone)]
pub struct ReadonlyRouteAuth {
    pub expected_authorization: Option<String>,
    pub enable_2fa: bool,
    pub totp_secret: Option<Vec<u8>>,
    pub config: Option<ReadonlyAuthConfig>,
}

impl ReadonlyRouteAuth {
    /// 根据可选的基本认证配置预先计算"期望的 Authorization 头",免去每次请求都重新编码。
    pub fn from_config(config: Option<ReadonlyAuthConfig>) -> Self {
        let (expected_authorization, enable_2fa, totp_secret) = match config.as_ref() {
            Some(config) => {
                let credentials = format!("{}:{}", config.username, config.password);
                let encoded = base64::engine::general_purpose::STANDARD.encode(credentials);
                let auth = format!("Basic {encoded}");

                let totp_secret = if config.enable_2fa {
                    config.totp_secret.as_deref().and_then(decode_totp_secret)
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
            config,
        }
    }

    /// 判断单次请求是否带有合法的 Basic 凭证;未启用认证时直接放行。
    pub fn is_authorized(&self, request: &Request) -> bool {
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

/// 服务端内存中的 2FA 会话票据。
///
/// Cookie 里只保存随机 token,真正的有效期与状态留在服务端内存里,避免前端
/// 伪造 `nodelite_auth=verified` 之类的静态 cookie 绕过二次验证。
#[derive(Debug, Clone)]
pub struct TwoFactorSessions {
    inner: Arc<Mutex<TwoFactorSessionStore>>,
}

#[derive(Debug, Default)]
struct TwoFactorSessionStore {
    pending: HashMap<String, PendingSession>,
    authenticated: HashMap<String, Instant>,
    /// 最近被成功消费过的 TOTP `time_step`(30 秒一个步长)。一旦某个 step
    /// 被用过,同一 step 的后续验证码必须拒绝,以阻止攻击者捕获一次 verify
    /// 请求后在同窗口内重放。条目会定期 prune,避免无界增长。
    used_totp_steps: HashMap<u64, Instant>,
}

/// 一条待二次验证的会话:除了过期时间,还跟踪该 pending token 的连续失败
/// 验证次数。达到 `TWO_FACTOR_MAX_FAILED_ATTEMPTS` 后立即失效,迫使客户端
/// 重新通过 Basic Auth 拿一个新的 pending token,从而把暴力破解的代价
/// 抬高到与一次完整登录相当。
#[derive(Debug, Clone, Copy)]
struct PendingSession {
    expires_at: Instant,
    failed_attempts: u32,
}

/// 2FA 验证请求。
#[derive(Debug, Deserialize)]
pub struct Verify2FARequest {
    pub code: String,
}

/// 2FA 验证失败响应。
#[derive(Debug, Serialize)]
pub struct Verify2FAError {
    pub error: String,
}

impl TwoFactorSessions {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(TwoFactorSessionStore::default())),
        }
    }

    pub fn create_pending(&self) -> Result<String> {
        let token = generate_session_token()?;
        let expires_at = Instant::now() + Duration::from_secs(TWO_FACTOR_PENDING_SECS);
        let mut store = lock_mutex(&self.inner);
        prune_expired_sessions(&mut store, Instant::now());
        store.pending.insert(
            token.clone(),
            PendingSession {
                expires_at,
                failed_attempts: 0,
            },
        );
        Ok(token)
    }

    pub fn pending_exists(&self, token: &str) -> bool {
        let mut store = lock_mutex(&self.inner);
        prune_expired_sessions(&mut store, Instant::now());
        store.pending.contains_key(token)
    }

    pub fn consume_pending(&self, token: &str) {
        let mut store = lock_mutex(&self.inner);
        store.pending.remove(token);
    }

    /// 记录一次 TOTP 错误尝试。返回值表示该 pending token 是否已经因连续
    /// 失败而被强制失效;调用方据此决定向客户端返回的状态码与是否同时
    /// 清掉 pending cookie。
    pub fn record_failed_attempt(&self, token: &str) -> bool {
        let mut store = lock_mutex(&self.inner);
        prune_expired_sessions(&mut store, Instant::now());
        let Some(session) = store.pending.get_mut(token) else {
            // pending 已经被 prune 或 consume,等同于"已经失效"。
            return true;
        };
        session.failed_attempts = session.failed_attempts.saturating_add(1);
        if session.failed_attempts >= TWO_FACTOR_MAX_FAILED_ATTEMPTS {
            store.pending.remove(token);
            return true;
        }
        false
    }

    /// 标记某个 TOTP `time_step` 已经被成功消费过;同一个 step 再次出现时,
    /// `is_totp_step_used` 会返回 true,从而拒绝重放。
    /// 标记会在该 step 离开 ±1 漂移窗口后自动过期。
    pub fn mark_totp_step_used(&self, step: u64) {
        let mut store = lock_mutex(&self.inner);
        prune_expired_sessions(&mut store, Instant::now());
        let expires_at =
            Instant::now() + Duration::from_secs(TWO_FACTOR_TOTP_REPLAY_RETENTION_SECS);
        store.used_totp_steps.insert(step, expires_at);
    }

    pub fn is_totp_step_used(&self, step: u64) -> bool {
        let mut store = lock_mutex(&self.inner);
        prune_expired_sessions(&mut store, Instant::now());
        store.used_totp_steps.contains_key(&step)
    }

    pub fn create_authenticated(&self) -> Result<String> {
        let token = generate_session_token()?;
        let expires_at = Instant::now() + Duration::from_secs(TWO_FACTOR_AUTH_SECS);
        let mut store = lock_mutex(&self.inner);
        prune_expired_sessions(&mut store, Instant::now());
        store.authenticated.insert(token.clone(), expires_at);
        Ok(token)
    }

    pub fn is_authenticated(&self, token: &str) -> bool {
        let mut store = lock_mutex(&self.inner);
        prune_expired_sessions(&mut store, Instant::now());
        store.authenticated.contains_key(token)
    }

    pub fn remove_authenticated(&self, token: &str) {
        let mut store = lock_mutex(&self.inner);
        store.authenticated.remove(token);
    }

    /// 密码轮换后清空已完成 2FA 的浏览器会话,避免旧凭据换出的会话继续可用。
    pub fn clear_authenticated(&self) {
        let mut store = lock_mutex(&self.inner);
        store.authenticated.clear();
    }
}

fn prune_expired_sessions(store: &mut TwoFactorSessionStore, now: Instant) {
    store.pending.retain(|_, session| session.expires_at > now);
    store
        .authenticated
        .retain(|_, expires_at| *expires_at > now);
    store
        .used_totp_steps
        .retain(|_, expires_at| *expires_at > now);
}

fn lock_mutex(mutex: &Mutex<TwoFactorSessionStore>) -> MutexGuard<'_, TwoFactorSessionStore> {
    mutex.lock().unwrap_or_else(|poisoned| {
        error!("two-factor session mutex poisoned; resetting session state");
        let mut guard = poisoned.into_inner();
        *guard = TwoFactorSessionStore::default();
        mutex.clear_poison();
        guard
    })
}

fn generate_session_token() -> Result<String> {
    let mut bytes = [0_u8; 32];
    fill_random(&mut bytes).context("failed to gather secure random bytes")?;
    Ok(hex_encode(&bytes))
}

pub fn decode_totp_secret(value: &str) -> Option<Vec<u8>> {
    let normalized = normalize_totp_secret(value);
    base32::decode(base32::Alphabet::Rfc4648 { padding: false }, &normalized)
        .or_else(|| base32::decode(base32::Alphabet::Rfc4648 { padding: true }, &normalized))
}

/// 验证 TOTP 码并返回匹配到的当前 30 秒 `time_step`。
///
/// 调用方收到 `Some(step)` 后需要进一步检查该 step 是否已经被消费过
/// (`TwoFactorSessions::is_totp_step_used`),以满足 RFC 6238 §5.2 的
/// "同一步骤的代码不允许重复使用"要求。
pub fn verify_totp_step(totp_secret: Option<&[u8]>, code: &str) -> Option<u64> {
    // 时钟回拨到 1970 之前 chrono 会返回负值;退化到 0 而不是 panic。
    let now_secs = Utc::now().timestamp().max(0) as u64;
    let now_step = now_secs / 30;
    verify_totp_step_at(totp_secret, code, now_step)
}

fn verify_totp_step_at(totp_secret: Option<&[u8]>, code: &str, now_step: u64) -> Option<u64> {
    let secret = totp_secret?;

    // 验证码必须正好 6 位 ASCII 数字。
    if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }

    let expected = totp_code_for_step(secret, now_step);
    constant_time_compare_bytes(expected.as_bytes(), code.as_bytes()).then_some(now_step)
}

fn totp_code_for_step(secret: &[u8], step: u64) -> String {
    // `totp_lite` expects Unix seconds and divides by `period` internally.
    // We track replay protection by step, so convert the step back to the
    // first second in that 30-second window before generating the code.
    totp_custom::<Sha1>(30, 6, secret, step.saturating_mul(30))
}

/// 长度 + 内容都按常量时间比较,避免依据"首个不同字节位置"做旁路。
/// 在 verify_totp_step 的调用点两边都已经被检查为 6 字节,但保留通用实现
/// 以便未来其它处复用。
pub fn constant_time_compare_bytes(left: &[u8], right: &[u8]) -> bool {
    left.ct_eq(right).into()
}

pub fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
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

pub fn auth_cookie(
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

pub fn expire_cookie(name: &'static str, secure: bool) -> (header::HeaderName, String) {
    let secure_suffix = if secure { "; Secure" } else { "" };
    (
        header::SET_COOKIE,
        format!("{name}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0{secure_suffix}"),
    )
}

pub fn secure_cookies(config: &ServerConfig) -> bool {
    config.public_base_url.starts_with("https://")
}

#[cfg(test)]
mod tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use super::totp_code_for_step;
    use super::*;

    #[test]
    fn totp_generation_uses_unix_seconds_for_rfc_6238_compatibility() {
        let secret = b"12345678901234567890";

        // RFC 6238 Appendix B gives SHA1/8-digit code 94287082 at Unix time
        // 59. With 6 digits the same dynamic truncation becomes 287082.
        assert_eq!(totp_code_for_step(secret, 59 / 30), "287082");
    }

    #[test]
    fn verify_totp_step_accepts_only_current_step() {
        let secret = b"12345678901234567890";
        let current_step = (1_000_000..1_000_100)
            .find(|step| {
                let current = totp_code_for_step(secret, *step);
                let previous = totp_code_for_step(secret, step - 1);
                let next = totp_code_for_step(secret, step + 1);
                current != previous && current != next
            })
            .expect("fixture should find non-colliding adjacent TOTP codes");

        let current_code = totp_code_for_step(secret, current_step);
        let previous_code = totp_code_for_step(secret, current_step - 1);
        let next_code = totp_code_for_step(secret, current_step + 1);

        assert_eq!(
            verify_totp_step_at(Some(secret), &current_code, current_step),
            Some(current_step)
        );
        assert_eq!(
            verify_totp_step_at(Some(secret), &previous_code, current_step),
            None
        );
        assert_eq!(
            verify_totp_step_at(Some(secret), &next_code, current_step),
            None
        );
    }

    #[test]
    fn two_factor_sessions_reset_after_mutex_poison() {
        let sessions = TwoFactorSessions::new();
        let pending = sessions.create_pending().expect("create pending session");
        assert!(sessions.pending_exists(&pending));

        let _ = catch_unwind(AssertUnwindSafe(|| {
            let _guard = sessions.inner.lock().expect("lock session store");
            panic!("poison two-factor session store");
        }));

        assert!(!sessions.pending_exists(&pending));
        let replacement = sessions
            .create_pending()
            .expect("create replacement session");
        assert!(sessions.pending_exists(&replacement));
    }
}
