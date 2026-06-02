//! Audit event payloads, filters, and query errors.

use anyhow::Error;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuditEventType {
    LoginFailure,
    TotpVerifySuccess,
    TotpVerifyFailure,
    NodeConnected,
    TokenInvalid,
    RateLimitExceeded,
}

impl AuditEventType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LoginFailure => "login_failure",
            Self::TotpVerifySuccess => "totp_verify_success",
            Self::TotpVerifyFailure => "totp_verify_failure",
            Self::NodeConnected => "node_connected",
            Self::TokenInvalid => "token_invalid",
            Self::RateLimitExceeded => "rate_limit_exceeded",
        }
    }

    pub fn parse(input: &str) -> Option<Self> {
        match input {
            "login_failure" => Some(Self::LoginFailure),
            "totp_verify_success" => Some(Self::TotpVerifySuccess),
            "totp_verify_failure" => Some(Self::TotpVerifyFailure),
            "node_connected" => Some(Self::NodeConnected),
            "token_invalid" => Some(Self::TokenInvalid),
            "rate_limit_exceeded" => Some(Self::RateLimitExceeded),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AuditEvent {
    pub id: i64,
    pub timestamp: DateTime<Utc>,
    pub event_type: AuditEventType,
    pub user: Option<String>,
    pub node_id: Option<String>,
    pub ip_address: String,
    pub user_agent: Option<String>,
    pub success: bool,
    pub details: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewAuditEvent {
    pub timestamp: DateTime<Utc>,
    pub event_type: AuditEventType,
    pub user: Option<String>,
    pub node_id: Option<String>,
    pub ip_address: String,
    pub user_agent: Option<String>,
    pub success: bool,
    pub details: Value,
}

impl NewAuditEvent {
    pub fn now(event_type: AuditEventType, ip_address: impl Into<String>, success: bool) -> Self {
        Self {
            timestamp: Utc::now(),
            event_type,
            user: None,
            node_id: None,
            ip_address: ip_address.into(),
            user_agent: None,
            success,
            details: Value::Object(Default::default()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AuditQuery {
    pub start: Option<DateTime<Utc>>,
    pub end: Option<DateTime<Utc>>,
    pub event_type: Option<AuditEventType>,
    pub success: Option<bool>,
    pub limit: usize,
}

#[derive(Debug)]
pub enum AuditLogError {
    Disabled,
    Query(Error),
}

impl std::fmt::Display for AuditLogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disabled => f.write_str("audit log is disabled"),
            Self::Query(_) => f.write_str("audit log query failed"),
        }
    }
}

impl std::error::Error for AuditLogError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Query(error) => Some(error.root_cause()),
            Self::Disabled => None,
        }
    }
}
