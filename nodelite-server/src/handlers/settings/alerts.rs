use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use tracing::error;

use crate::AppState;
use crate::alerts::{build_inspection_report, evaluate_rules};
use nodelite_proto::{AlertingConfig, NodeStatus};

use super::config_edit::persist_alerting_change;
use super::helpers::settings_json_error;
use super::security::settings_confirmation_error_for_sensitive_action;
use super::types::{
    AlertPreview, AlertRuleView, AlertSettingsResponse, AlertSettingsView, AlertSmtpSettingsView,
    AlertWebhookSettingsView, InspectionHighlight, InspectionPreview, InspectionSettingsView,
    TriggeredRulePreview, UpdateAlertSettingsRequest,
};

pub(crate) async fn alert_settings(State(state): State<AppState>) -> impl IntoResponse {
    Json(build_alert_settings_response(&state).await)
}

pub(crate) async fn update_alert_settings(
    State(state): State<AppState>,
    Json(request): Json<UpdateAlertSettingsRequest>,
) -> Response {
    let current_auth = {
        let auth = state.readonly_auth.read().await;
        auth.config.clone()
    };
    let Some(current_auth) = current_auth else {
        return settings_json_error(StatusCode::CONFLICT, "readonly auth is not enabled");
    };
    if let Some(response) = settings_confirmation_error_for_sensitive_action(
        &state,
        &current_auth,
        request.current_password.as_deref(),
        request.code.as_deref(),
    ) {
        return response;
    }

    let next_config = {
        let current = state.alerting.read().await;
        merge_alerting_request(&current, request)
    };

    if let Err(error) = persist_alerting_change(&state.config_path, &next_config).await {
        error!(error = ?error, path = %state.config_path.display(), "failed to persist alerting settings");
        let message = error.to_string();
        let status = if message.contains("updated server config would be invalid") {
            StatusCode::BAD_REQUEST
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        };
        return settings_json_error(status, message);
    }

    {
        let mut alerting = state.alerting.write().await;
        *alerting = next_config;
    }

    Json(build_alert_settings_response(&state).await).into_response()
}

async fn build_alert_settings_response(state: &AppState) -> AlertSettingsResponse {
    let alerting = {
        let alerting = state.alerting.read().await;
        alerting.clone()
    };
    let statuses = state.shared.list_statuses().await;
    AlertSettingsResponse {
        config: alert_settings_view(&alerting),
        preview: build_alert_preview(&alerting, &statuses),
    }
}

fn merge_alerting_request(
    current: &AlertingConfig,
    request: UpdateAlertSettingsRequest,
) -> AlertingConfig {
    AlertingConfig {
        enabled: request.enabled,
        smtp: nodelite_proto::AlertSmtpConfig {
            enabled: request.smtp.enabled,
            host: request.smtp.host,
            port: request.smtp.port,
            username: request.smtp.username,
            password: if request.smtp.clear_password {
                None
            } else {
                request
                    .smtp
                    .password
                    .filter(|value| !value.trim().is_empty())
                    .or_else(|| current.smtp.password.clone())
            },
            sender: request.smtp.sender,
            recipients: request.smtp.recipients,
            transport: request.smtp.transport,
        },
        webhook: nodelite_proto::AlertWebhookConfig {
            enabled: request.webhook.enabled,
            url: request.webhook.url,
            secret: if request.webhook.clear_secret {
                None
            } else {
                request
                    .webhook
                    .secret
                    .filter(|value| !value.trim().is_empty())
                    .or_else(|| current.webhook.secret.clone())
            },
            send_resolved: request.webhook.send_resolved,
        },
        rules: request
            .rules
            .into_iter()
            .map(|rule| nodelite_proto::AlertRuleConfig {
                id: rule.id,
                name: rule.name,
                enabled: rule.enabled,
                metric: rule.metric,
                comparator: rule.comparator,
                threshold: rule.threshold,
                window_minutes: rule.window_minutes,
                severity: rule.severity,
                scope_mode: rule.scope_mode,
                node_ids: rule.node_ids,
                tags: rule.tags,
                delivery: rule.delivery,
                cooldown_minutes: rule.cooldown_minutes,
                send_resolved: rule.send_resolved,
            })
            .collect(),
        inspection: nodelite_proto::InspectionConfig {
            enabled: request.inspection.enabled,
            local_time: request.inspection.local_time,
            lookback_hours: request.inspection.lookback_hours,
            delivery: request.inspection.delivery,
            offline_grace_minutes: request.inspection.offline_grace_minutes,
            latency_warn_ms: request.inspection.latency_warn_ms,
            cpu_warn_percent: request.inspection.cpu_warn_percent,
            memory_warn_percent: request.inspection.memory_warn_percent,
        },
    }
}

fn alert_settings_view(config: &AlertingConfig) -> AlertSettingsView {
    AlertSettingsView {
        enabled: config.enabled,
        smtp: AlertSmtpSettingsView {
            enabled: config.smtp.enabled,
            host: config.smtp.host.clone(),
            port: config.smtp.port,
            username: config.smtp.username.clone(),
            sender: config.smtp.sender.clone(),
            recipients: config.smtp.recipients.clone(),
            transport: config.smtp.transport.clone(),
            password_configured: config.smtp.password.is_some(),
        },
        webhook: AlertWebhookSettingsView {
            enabled: config.webhook.enabled,
            url: config.webhook.url.clone(),
            send_resolved: config.webhook.send_resolved,
            secret_configured: config.webhook.secret.is_some(),
        },
        rules: config
            .rules
            .iter()
            .map(|rule| AlertRuleView {
                id: rule.id.clone(),
                name: rule.name.clone(),
                enabled: rule.enabled,
                metric: rule.metric.clone(),
                comparator: rule.comparator.clone(),
                threshold: rule.threshold,
                window_minutes: rule.window_minutes,
                severity: rule.severity.clone(),
                scope_mode: rule.scope_mode.clone(),
                node_ids: rule.node_ids.clone(),
                tags: rule.tags.clone(),
                delivery: rule.delivery.clone(),
                cooldown_minutes: rule.cooldown_minutes,
                send_resolved: rule.send_resolved,
            })
            .collect(),
        inspection: InspectionSettingsView {
            enabled: config.inspection.enabled,
            local_time: config.inspection.local_time.clone(),
            lookback_hours: config.inspection.lookback_hours,
            delivery: config.inspection.delivery.clone(),
            offline_grace_minutes: config.inspection.offline_grace_minutes,
            latency_warn_ms: config.inspection.latency_warn_ms,
            cpu_warn_percent: config.inspection.cpu_warn_percent,
            memory_warn_percent: config.inspection.memory_warn_percent,
        },
    }
}

fn build_alert_preview(config: &AlertingConfig, statuses: &[NodeStatus]) -> AlertPreview {
    let now = Utc::now();
    let inspection = build_inspection_report(&config.inspection, statuses, now);
    AlertPreview {
        generated_at: now,
        triggered_rules: config
            .rules
            .iter()
            .filter(|rule| rule.enabled)
            .filter_map(|rule| {
                let node_ids = evaluate_rules(std::slice::from_ref(rule), statuses, now)
                    .iter()
                    .map(|matched| matched.node_id.clone())
                    .collect::<Vec<_>>();
                if node_ids.is_empty() {
                    return None;
                }
                Some(TriggeredRulePreview {
                    rule_id: rule.id.clone(),
                    rule_name: rule.name.clone(),
                    severity: rule.severity.clone(),
                    node_ids,
                })
            })
            .collect(),
        inspection: InspectionPreview {
            total_nodes: inspection.total_nodes,
            offline_nodes: inspection.offline_nodes,
            latency_nodes: inspection.latency_nodes,
            cpu_hot_nodes: inspection.cpu_hot_nodes,
            memory_hot_nodes: inspection.memory_hot_nodes,
            highlights: inspection
                .highlights
                .into_iter()
                .map(|highlight| InspectionHighlight {
                    node_id: highlight.node_id,
                    node_label: highlight.node_label,
                    reasons: highlight.reasons,
                })
                .collect(),
        },
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};

    use super::{build_alert_preview, merge_alerting_request};
    use crate::test_support::{fake_snapshot, synthetic_identity};
    use nodelite_proto::{
        AlertChannel, AlertComparator, AlertMetric, AlertRuleConfig, AlertScopeMode, AlertSeverity,
        AlertSmtpConfig, AlertSmtpTransport, AlertWebhookConfig, AlertingConfig, InspectionConfig,
        NodeStatus,
    };

    fn sample_status(
        node_id: &str,
        label: &str,
        online: bool,
        cpu: f64,
        latency_ms: u64,
    ) -> NodeStatus {
        let mut snapshot = fake_snapshot(300);
        snapshot.cpu_usage_percent = Some(cpu);
        snapshot.memory.used_bytes = 8;
        snapshot.memory.total_bytes = 10;
        NodeStatus {
            identity: synthetic_identity(node_id, label, "2.2.6", Some("6.8.0"), "edge"),
            snapshot: Some(snapshot),
            online,
            last_seen: Some(Utc::now() - Duration::minutes(30)),
            remote_ip: Some("203.0.113.8".to_string()),
            latency_ms: Some(latency_ms),
        }
    }

    #[test]
    fn merge_alerting_request_keeps_existing_secrets_when_not_overridden() {
        let current = AlertingConfig {
            enabled: true,
            smtp: AlertSmtpConfig {
                enabled: true,
                host: "smtp.example.com".to_string(),
                port: 587,
                username: "ops".to_string(),
                password: Some("smtp-secret".to_string()),
                sender: "ops@example.com".to_string(),
                recipients: vec!["ops@example.com".to_string()],
                transport: AlertSmtpTransport::StartTls,
            },
            webhook: AlertWebhookConfig {
                enabled: true,
                url: "https://hooks.example.com".to_string(),
                secret: Some("hook-secret".to_string()),
                send_resolved: true,
            },
            rules: Vec::new(),
            inspection: InspectionConfig::default(),
        };
        let request = super::UpdateAlertSettingsRequest {
            current_password: Some("readonly-secret".to_string()),
            code: None,
            enabled: true,
            smtp: super::super::types::UpdateAlertSmtpSettingsRequest {
                enabled: true,
                host: "smtp.example.com".to_string(),
                port: 587,
                username: "ops".to_string(),
                password: None,
                clear_password: false,
                sender: "ops@example.com".to_string(),
                recipients: vec!["ops@example.com".to_string()],
                transport: AlertSmtpTransport::StartTls,
            },
            webhook: super::super::types::UpdateAlertWebhookSettingsRequest {
                enabled: true,
                url: "https://hooks.example.com".to_string(),
                secret: None,
                clear_secret: false,
                send_resolved: true,
            },
            rules: Vec::new(),
            inspection: super::super::types::UpdateInspectionSettingsRequest {
                enabled: false,
                local_time: "09:00".to_string(),
                lookback_hours: 24,
                delivery: vec![AlertChannel::Smtp],
                offline_grace_minutes: 10,
                latency_warn_ms: 250,
                cpu_warn_percent: 85,
                memory_warn_percent: 90,
            },
        };

        let merged = merge_alerting_request(&current, request);

        assert_eq!(merged.smtp.password.as_deref(), Some("smtp-secret"));
        assert_eq!(merged.webhook.secret.as_deref(), Some("hook-secret"));
    }

    #[test]
    fn build_alert_preview_lists_triggered_rules_and_inspection_highlights() {
        let status = sample_status("hk-01", "Hong Kong", false, 88.0, 320);
        let config = AlertingConfig {
            enabled: true,
            smtp: AlertSmtpConfig::default(),
            webhook: AlertWebhookConfig::default(),
            rules: vec![AlertRuleConfig {
                id: "latency-hot".to_string(),
                name: "Latency".to_string(),
                enabled: true,
                metric: AlertMetric::LatencyMs,
                comparator: AlertComparator::Gt,
                threshold: 300,
                window_minutes: 5,
                severity: AlertSeverity::Warning,
                scope_mode: AlertScopeMode::All,
                node_ids: Vec::new(),
                tags: Vec::new(),
                delivery: vec![AlertChannel::Webhook],
                cooldown_minutes: 30,
                send_resolved: true,
            }],
            inspection: InspectionConfig::default(),
        };

        let preview = build_alert_preview(&config, &[status]);

        assert_eq!(preview.triggered_rules.len(), 1);
        assert_eq!(preview.inspection.offline_nodes, 1);
        assert_eq!(preview.inspection.latency_nodes, 1);
        assert_eq!(preview.inspection.highlights.len(), 1);
    }
}
