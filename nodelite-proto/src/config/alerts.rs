use serde::{Deserialize, Serialize};

pub const DEFAULT_ALERT_RULE_WINDOW_MINUTES: u64 = 5;
pub const DEFAULT_ALERT_RULE_COOLDOWN_MINUTES: u64 = 30;
pub const DEFAULT_ALERT_INSPECTION_LOOKBACK_HOURS: u64 = 24;
pub const DEFAULT_ALERT_INSPECTION_LOCAL_TIME: &str = "09:00";
pub const DEFAULT_ALERT_INSPECTION_OFFLINE_GRACE_MINUTES: u64 = 10;
pub const DEFAULT_ALERT_INSPECTION_LATENCY_WARN_MS: u64 = 250;
pub const DEFAULT_ALERT_INSPECTION_CPU_WARN_PERCENT: u64 = 85;
pub const DEFAULT_ALERT_INSPECTION_MEMORY_WARN_PERCENT: u64 = 90;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AlertChannel {
    Smtp,
    Webhook,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AlertSmtpTransport {
    StartTls,
    Tls,
    Plain,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AlertMetric {
    CpuUsagePercent,
    MemoryUsagePercent,
    DiskUsagePercent,
    LatencyMs,
    OfflineMinutes,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AlertComparator {
    Gt,
    Lt,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AlertSeverity {
    Warning,
    Critical,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AlertScopeMode {
    All,
    NodeIds,
    Tags,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AlertSmtpConfig {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: Option<String>,
    pub sender: String,
    pub recipients: Vec<String>,
    pub transport: AlertSmtpTransport,
    pub send_resolved: bool,
}

impl Default for AlertSmtpConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: String::new(),
            port: 587,
            username: String::new(),
            password: None,
            sender: String::new(),
            recipients: Vec::new(),
            transport: AlertSmtpTransport::StartTls,
            send_resolved: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AlertWebhookConfig {
    pub enabled: bool,
    pub url: String,
    pub secret: Option<String>,
    pub send_resolved: bool,
}

impl Default for AlertWebhookConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url: String::new(),
            secret: None,
            send_resolved: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AlertRuleConfig {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub metric: AlertMetric,
    pub comparator: AlertComparator,
    pub threshold: u64,
    pub window_minutes: u64,
    pub severity: AlertSeverity,
    pub scope_mode: AlertScopeMode,
    pub node_ids: Vec<String>,
    pub tags: Vec<String>,
    pub delivery: Vec<AlertChannel>,
    pub cooldown_minutes: u64,
    pub send_resolved: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InspectionConfig {
    pub enabled: bool,
    pub local_time: String,
    pub lookback_hours: u64,
    pub delivery: Vec<AlertChannel>,
    pub offline_grace_minutes: u64,
    pub latency_warn_ms: u64,
    pub cpu_warn_percent: u64,
    pub memory_warn_percent: u64,
}

impl Default for InspectionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            local_time: DEFAULT_ALERT_INSPECTION_LOCAL_TIME.to_string(),
            lookback_hours: DEFAULT_ALERT_INSPECTION_LOOKBACK_HOURS,
            delivery: vec![AlertChannel::Smtp],
            offline_grace_minutes: DEFAULT_ALERT_INSPECTION_OFFLINE_GRACE_MINUTES,
            latency_warn_ms: DEFAULT_ALERT_INSPECTION_LATENCY_WARN_MS,
            cpu_warn_percent: DEFAULT_ALERT_INSPECTION_CPU_WARN_PERCENT,
            memory_warn_percent: DEFAULT_ALERT_INSPECTION_MEMORY_WARN_PERCENT,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AlertingConfig {
    pub enabled: bool,
    pub smtp: AlertSmtpConfig,
    pub webhook: AlertWebhookConfig,
    pub rules: Vec<AlertRuleConfig>,
    pub inspection: InspectionConfig,
}
