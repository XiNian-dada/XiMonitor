use chrono::{DateTime, Utc};
use nodelite_proto::{
    AlertChannel, AlertComparator, AlertMetric, AlertScopeMode, AlertSeverity, AlertSmtpTransport,
};
use serde::{Deserialize, Serialize};

/// 设置页读取的服务端与安全状态。这里刻意不包含任何 token / password 明文。
#[derive(Debug, Serialize)]
pub(crate) struct SettingsResponse {
    pub(crate) service: &'static str,
    pub(crate) server_version: &'static str,
    pub(crate) repository: &'static str,
    pub(crate) public_base_url: String,
    pub(crate) listen: String,
    pub(crate) config_path: String,
    pub(crate) registry_path: String,
    pub(crate) history_db_path: String,
    pub(crate) snapshot_path: String,
    pub(crate) history_retention_hours: u64,
    pub(crate) refresh_interval_secs: u64,
    pub(crate) auth: SettingsAuth,
    pub(crate) updates: SettingsUpdates,
    pub(crate) agents: Vec<SettingsAgentToken>,
}

#[derive(Debug, Serialize)]
pub(crate) struct SettingsAuth {
    pub(crate) enabled: bool,
    pub(crate) username: Option<String>,
    pub(crate) two_factor_enabled: bool,
    pub(crate) totp_secret_configured: bool,
    pub(crate) session_ttl_secs: u64,
    pub(crate) pending_ttl_secs: u64,
}

#[derive(Debug, Serialize)]
pub(crate) struct SettingsUpdates {
    pub(crate) latest_release_url: String,
    pub(crate) server_upgrade_command: String,
    pub(crate) agent_upgrade_command: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct SettingsAgentToken {
    pub(crate) node_id: String,
    pub(crate) node_label: String,
    pub(crate) online: bool,
    pub(crate) agent_version: Option<String>,
    pub(crate) remote_ip: Option<String>,
    pub(crate) tags: Vec<String>,
    pub(crate) token_expires_at: Option<DateTime<Utc>>,
    pub(crate) token_expires_in_secs: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ChangePasswordRequest {
    pub(crate) current_password: String,
    pub(crate) new_password: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct StartServerUpdateRequest {
    pub(crate) current_password: Option<String>,
    pub(crate) code: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ServerUpdateLogQuery {
    pub(crate) offset: Option<u64>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ServerUpdateLogResponse {
    pub(crate) exists: bool,
    pub(crate) offset: u64,
    pub(crate) next_offset: u64,
    pub(crate) text: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct TwoFactorSetupResponse {
    pub(crate) secret: String,
    pub(crate) otpauth_uri: String,
    pub(crate) qr_svg: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct EnableTwoFactorRequest {
    pub(crate) current_password: String,
    pub(crate) secret: String,
    pub(crate) code: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DisableTwoFactorRequest {
    pub(crate) current_password: String,
    pub(crate) code: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct SettingsActionResponse {
    pub(crate) ok: bool,
    pub(crate) message: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct NodeTokenRefreshResponse {
    pub(crate) ok: bool,
    pub(crate) message: String,
    pub(crate) token_expires_at: Option<DateTime<Utc>>,
    pub(crate) token_expires_in_secs: Option<i64>,
}

#[derive(Debug, Serialize)]
pub(crate) struct AlertSettingsResponse {
    pub(crate) config: AlertSettingsView,
    pub(crate) preview: AlertPreview,
}

#[derive(Debug, Serialize)]
pub(crate) struct AlertSettingsView {
    pub(crate) enabled: bool,
    pub(crate) smtp: AlertSmtpSettingsView,
    pub(crate) webhook: AlertWebhookSettingsView,
    pub(crate) rules: Vec<AlertRuleView>,
    pub(crate) inspection: InspectionSettingsView,
}

#[derive(Debug, Serialize)]
pub(crate) struct AlertSmtpSettingsView {
    pub(crate) enabled: bool,
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) username: String,
    pub(crate) sender: String,
    pub(crate) recipients: Vec<String>,
    pub(crate) transport: AlertSmtpTransport,
    pub(crate) password_configured: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct AlertWebhookSettingsView {
    pub(crate) enabled: bool,
    pub(crate) url: String,
    pub(crate) send_resolved: bool,
    pub(crate) secret_configured: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct AlertRuleView {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) enabled: bool,
    pub(crate) metric: AlertMetric,
    pub(crate) comparator: AlertComparator,
    pub(crate) threshold: u64,
    pub(crate) window_minutes: u64,
    pub(crate) severity: AlertSeverity,
    pub(crate) scope_mode: AlertScopeMode,
    pub(crate) node_ids: Vec<String>,
    pub(crate) tags: Vec<String>,
    pub(crate) delivery: Vec<AlertChannel>,
    pub(crate) cooldown_minutes: u64,
    pub(crate) send_resolved: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct InspectionSettingsView {
    pub(crate) enabled: bool,
    pub(crate) local_time: String,
    pub(crate) lookback_hours: u64,
    pub(crate) delivery: Vec<AlertChannel>,
    pub(crate) offline_grace_minutes: u64,
    pub(crate) latency_warn_ms: u64,
    pub(crate) cpu_warn_percent: u64,
    pub(crate) memory_warn_percent: u64,
}

#[derive(Debug, Serialize)]
pub(crate) struct AlertPreview {
    pub(crate) generated_at: DateTime<Utc>,
    pub(crate) triggered_rules: Vec<TriggeredRulePreview>,
    pub(crate) inspection: InspectionPreview,
}

#[derive(Debug, Serialize)]
pub(crate) struct TriggeredRulePreview {
    pub(crate) rule_id: String,
    pub(crate) rule_name: String,
    pub(crate) severity: AlertSeverity,
    pub(crate) node_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct InspectionPreview {
    pub(crate) total_nodes: usize,
    pub(crate) offline_nodes: usize,
    pub(crate) latency_nodes: usize,
    pub(crate) cpu_hot_nodes: usize,
    pub(crate) memory_hot_nodes: usize,
    pub(crate) highlights: Vec<InspectionHighlight>,
}

#[derive(Debug, Serialize)]
pub(crate) struct InspectionHighlight {
    pub(crate) node_id: String,
    pub(crate) node_label: String,
    pub(crate) reasons: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateAlertSettingsRequest {
    pub(crate) current_password: Option<String>,
    pub(crate) code: Option<String>,
    pub(crate) enabled: bool,
    pub(crate) smtp: UpdateAlertSmtpSettingsRequest,
    pub(crate) webhook: UpdateAlertWebhookSettingsRequest,
    pub(crate) rules: Vec<UpdateAlertRuleRequest>,
    pub(crate) inspection: UpdateInspectionSettingsRequest,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateAlertSmtpSettingsRequest {
    pub(crate) enabled: bool,
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) username: String,
    pub(crate) password: Option<String>,
    #[serde(default)]
    pub(crate) clear_password: bool,
    pub(crate) sender: String,
    pub(crate) recipients: Vec<String>,
    pub(crate) transport: AlertSmtpTransport,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateAlertWebhookSettingsRequest {
    pub(crate) enabled: bool,
    pub(crate) url: String,
    pub(crate) secret: Option<String>,
    #[serde(default)]
    pub(crate) clear_secret: bool,
    pub(crate) send_resolved: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateAlertRuleRequest {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) enabled: bool,
    pub(crate) metric: AlertMetric,
    pub(crate) comparator: AlertComparator,
    pub(crate) threshold: u64,
    pub(crate) window_minutes: u64,
    pub(crate) severity: AlertSeverity,
    pub(crate) scope_mode: AlertScopeMode,
    #[serde(default)]
    pub(crate) node_ids: Vec<String>,
    #[serde(default)]
    pub(crate) tags: Vec<String>,
    #[serde(default)]
    pub(crate) delivery: Vec<AlertChannel>,
    pub(crate) cooldown_minutes: u64,
    pub(crate) send_resolved: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateInspectionSettingsRequest {
    pub(crate) enabled: bool,
    pub(crate) local_time: String,
    pub(crate) lookback_hours: u64,
    pub(crate) delivery: Vec<AlertChannel>,
    pub(crate) offline_grace_minutes: u64,
    pub(crate) latency_warn_ms: u64,
    pub(crate) cpu_warn_percent: u64,
    pub(crate) memory_warn_percent: u64,
}
