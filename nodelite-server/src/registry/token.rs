use anyhow::anyhow;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use getrandom::fill as fill_random;
use nodelite_proto::NodeIdentity;

use crate::auth::constant_time_compare_bytes;
use crate::encoding::hex_encode;

use super::{
    ARGON2_ITERATIONS, ARGON2_MEMORY_KIB, ARGON2_PARALLELISM, AuthorizedNode,
    INSTALL_TOKEN_TTL_MINUTES, InstallSession, RegisteredNode, RegistryError, RegistryFile,
    RegistryResult,
};

pub(super) fn prune_expired_install_sessions(file: &mut RegistryFile, now: DateTime<Utc>) -> bool {
    let original_len = file.install_sessions.len();
    file.install_sessions
        .retain(|session| session.expires_at > now);
    original_len != file.install_sessions.len()
}

pub(super) fn mint_install_session(
    file: &mut RegistryFile,
    node_id: &str,
    now: DateTime<Utc>,
    node_session_token: String,
) -> RegistryResult<InstallSession> {
    file.install_sessions
        .retain(|session| session.node_id != node_id);
    let session = InstallSession {
        token: generate_token()?,
        node_id: node_id.to_string(),
        created_at: now,
        expires_at: now + ChronoDuration::minutes(INSTALL_TOKEN_TTL_MINUTES),
        node_session_token,
    };
    file.install_sessions.push(session.clone());
    Ok(session)
}

pub(super) fn token_is_unexpired(entry: &RegisteredNode, now: DateTime<Utc>) -> bool {
    entry
        .token_expires_at
        .map(|expires_at| now < expires_at)
        .unwrap_or(true)
}

pub(super) fn authorized_node_from_entry(
    identity: &NodeIdentity,
    entry: &RegisteredNode,
    registry_revision: u64,
) -> RegistryResult<AuthorizedNode> {
    if !token_is_unexpired(entry, Utc::now()) {
        return Err(RegistryError::TokenExpired {
            node_id: entry.node_id.clone(),
        });
    }

    let mut identity = identity.clone();
    identity.node_id = entry.node_id.clone();
    identity.node_label = entry.node_label.clone();
    identity.tags = entry.tags.clone();
    Ok(AuthorizedNode {
        identity,
        generation: entry.token_generation,
        token_expires_at: entry.token_expires_at,
        registry_revision,
    })
}

/// 用统一参数构造 Argon2id 实例。OWASP 2023 服务器档位:
/// memory 19 MiB / iterations 2 / parallelism 1。
fn argon2_instance() -> RegistryResult<Argon2<'static>> {
    let params = Params::new(
        ARGON2_MEMORY_KIB,
        ARGON2_ITERATIONS,
        ARGON2_PARALLELISM,
        None,
    )
    .map_err(|error| {
        RegistryError::internal("failed to build argon2 parameters", anyhow!(error))
    })?;
    Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
}

/// 把明文 token 哈希成 Argon2id PHC 字符串。返回的字符串自带 salt + params,
/// 可直接存入 registry.json 并在后续 verify 时无需额外参数。
pub(super) fn hash_token(token: &str) -> RegistryResult<String> {
    let mut salt_bytes = [0u8; 16];
    fill_random(&mut salt_bytes).map_err(|error| {
        RegistryError::internal("failed to generate token salt", anyhow!(error))
    })?;
    let salt = SaltString::encode_b64(&salt_bytes)
        .map_err(|error| RegistryError::internal("failed to encode token salt", anyhow!(error)))?;
    let hash = argon2_instance()?
        .hash_password(token.as_bytes(), &salt)
        .map_err(|error| RegistryError::internal("failed to hash token", anyhow!(error)))?;
    Ok(hash.to_string())
}

/// 用 PHC 字符串校验候选 token。失败 / 解析错误一律返回 false,
/// 永远不让密码学错误溢出成 panic。
pub(super) fn verify_token(candidate: &str, phc: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(phc) else {
        return false;
    };
    let Ok(argon2) = argon2_instance() else {
        return false;
    };
    argon2
        .verify_password(candidate.as_bytes(), &parsed)
        .is_ok()
}

/// 把还在用明文 `token` 字段的旧 registry 条目迁移到 `token_hash`。
///
/// 这一步在 [`load_registry_state`] 中完成,完成后调用方会把 file 写回磁盘,
/// 之后磁盘上不再保留明文。返回值表示是否触发了任何变更。
pub(super) fn migrate_legacy_tokens(file: &mut RegistryFile) -> RegistryResult<bool> {
    let mut changed = false;
    for node in &mut file.nodes {
        if node.token_hash.is_empty() && !node.token.is_empty() {
            node.token_hash = hash_token(&node.token).map_err(|error| {
                RegistryError::internal(
                    "failed to hash legacy token during registry migration",
                    anyhow!("node {}: {error}", node.node_id),
                )
            })?;
            // 用 zero-overwrite 清掉明文。即便后续 file 没立即写盘,内存里的副本
            // 也尽量短地存在明文。
            node.token.clear();
            if node.token_generation == 0 {
                node.token_generation = 1;
            }
            changed = true;
        }
    }
    Ok(changed)
}

/// 常量时间字符串比较,仅在旧版本明文 token 兼容路径使用。
pub(super) fn constant_time_eq(left: &str, right: &str) -> bool {
    constant_time_compare_bytes(left.as_bytes(), right.as_bytes())
}

/// 生成 256-bit 的随机 token 并以十六进制字符串形式返回。
pub(super) fn generate_token() -> RegistryResult<String> {
    let mut bytes = [0_u8; 32];
    fill_random(&mut bytes).map_err(|error| {
        RegistryError::internal("failed to gather secure random bytes", anyhow!(error))
    })?;
    Ok(hex_encode(&bytes))
}
