use std::future::Future;
use std::time::Duration;

use chrono::{DateTime, NaiveDate, Utc};
use nodelite_proto::{AlertChannel, AlertSmtpConfig, AlertingConfig};
use thiserror::Error;
use tokio::time::sleep;

use super::{AlertEvent, AlertEventKind, InspectionReport};

mod smtp;
mod webhook;

const DELIVERY_MAX_ATTEMPTS: usize = 3;
const DELIVERY_RETRY_DELAY: Duration = Duration::from_millis(50);

pub(crate) use webhook::endpoint_label as webhook_endpoint_label;

#[derive(Debug, Error)]
pub(crate) enum AlertDeliveryError {
    #[error("webhook url is invalid")]
    InvalidWebhookUrl(#[from] url::ParseError),
    #[error("webhook url must include a host")]
    MissingWebhookHost,
    #[error("webhook scheme must be http or https")]
    UnsupportedWebhookScheme,
    #[error("webhook request timed out")]
    Timeout,
    #[error("webhook network operation failed")]
    Io(#[from] std::io::Error),
    #[error("webhook tls handshake failed")]
    Tls(String),
    #[error("webhook signature generation failed")]
    Signature(String),
    #[error("webhook payload serialization failed")]
    Serialize(#[from] serde_json::Error),
    #[error("webhook response was invalid")]
    InvalidResponse,
    #[error("webhook response headers exceeded the maximum size")]
    ResponseTooLarge,
    #[error("webhook returned HTTP {status}")]
    HttpStatus { status: u16 },
    #[error("smtp delivery timed out")]
    SmtpTimeout,
    #[error("smtp server rejected command: {0}")]
    Smtp(String),
    #[error("smtp message contains an invalid header value")]
    InvalidMailHeader,
    #[error("webhook delivery failed: {webhook}; smtp delivery failed: {smtp}")]
    MultiChannel {
        webhook: Box<AlertDeliveryError>,
        smtp: Box<AlertDeliveryError>,
    },
}

impl AlertDeliveryError {
    fn is_retryable(&self) -> bool {
        match self {
            Self::Timeout | Self::Io(_) | Self::Tls(_) | Self::InvalidResponse => true,
            Self::HttpStatus { status } => *status == 429 || *status >= 500,
            Self::SmtpTimeout | Self::Smtp(_) => true,
            Self::InvalidWebhookUrl(_)
            | Self::MissingWebhookHost
            | Self::UnsupportedWebhookScheme
            | Self::Signature(_)
            | Self::Serialize(_)
            | Self::ResponseTooLarge
            | Self::InvalidMailHeader
            | Self::MultiChannel { .. } => false,
        }
    }
}

#[derive(Debug, Default)]
struct DeliveryErrors {
    webhook: Option<AlertDeliveryError>,
    smtp: Option<AlertDeliveryError>,
}

impl DeliveryErrors {
    fn record_webhook(&mut self, error: AlertDeliveryError) {
        self.webhook = Some(error);
    }

    fn record_smtp(&mut self, error: AlertDeliveryError) {
        self.smtp = Some(error);
    }

    fn into_result(self) -> Result<(), AlertDeliveryError> {
        match (self.webhook, self.smtp) {
            (Some(webhook), Some(smtp)) => Err(AlertDeliveryError::MultiChannel {
                webhook: Box::new(webhook),
                smtp: Box::new(smtp),
            }),
            (Some(webhook), None) => Err(webhook),
            (None, Some(smtp)) => Err(smtp),
            (None, None) => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct InspectionSummary<'a> {
    pub(crate) occurred_at: DateTime<Utc>,
    pub(crate) local_date: NaiveDate,
    pub(crate) lookback_hours: u64,
    pub(crate) report: &'a InspectionReport,
}

pub(crate) async fn deliver_alert_event(
    config: &AlertingConfig,
    event: &AlertEvent,
) -> Result<(), AlertDeliveryError> {
    let mut errors = DeliveryErrors::default();
    if should_send_webhook(config, event)
        && let Err(error) =
            retry_delivery(|| webhook::send_alert_event(&config.webhook, event)).await
    {
        errors.record_webhook(error);
    }
    if should_send_smtp(config, event)
        && let Err(error) = retry_delivery(|| smtp::send_alert_event(&config.smtp, event)).await
    {
        errors.record_smtp(error);
    }

    errors.into_result()
}

pub(crate) async fn deliver_inspection_summary(
    config: &AlertingConfig,
    summary: &InspectionSummary<'_>,
) -> Result<(), AlertDeliveryError> {
    let mut errors = DeliveryErrors::default();
    if should_send_inspection_webhook(config)
        && let Err(error) =
            retry_delivery(|| webhook::send_inspection_summary(&config.webhook, summary)).await
    {
        errors.record_webhook(error);
    }
    if should_send_inspection_smtp(config)
        && let Err(error) =
            retry_delivery(|| smtp::send_inspection_summary(&config.smtp, summary)).await
    {
        errors.record_smtp(error);
    }

    errors.into_result()
}

async fn retry_delivery<F, Fut>(mut operation: F) -> Result<(), AlertDeliveryError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<(), AlertDeliveryError>>,
{
    let mut attempts = 0;
    loop {
        attempts += 1;
        match operation().await {
            Ok(()) => return Ok(()),
            Err(error) if attempts >= DELIVERY_MAX_ATTEMPTS || !error.is_retryable() => {
                return Err(error);
            }
            Err(_) => sleep(DELIVERY_RETRY_DELAY).await,
        }
    }
}

pub(crate) fn smtp_endpoint_label(config: &AlertSmtpConfig) -> String {
    if config.host.is_empty() {
        return "smtp://unconfigured".to_string();
    }
    format!("smtp://{}:{}", config.host, config.port)
}

fn should_send_webhook(config: &AlertingConfig, event: &AlertEvent) -> bool {
    if !config.webhook.enabled || !event.rule.delivery.contains(&AlertChannel::Webhook) {
        return false;
    }
    should_send_resolved(event) && (!is_resolved(event) || config.webhook.send_resolved)
}

fn should_send_smtp(config: &AlertingConfig, event: &AlertEvent) -> bool {
    config.smtp.enabled
        && event.rule.delivery.contains(&AlertChannel::Smtp)
        && should_send_resolved(event)
        && (!is_resolved(event) || config.smtp.send_resolved)
}

fn should_send_resolved(event: &AlertEvent) -> bool {
    !is_resolved(event) || event.rule.send_resolved
}

fn is_resolved(event: &AlertEvent) -> bool {
    matches!(event.kind, AlertEventKind::Resolved)
}

fn should_send_inspection_webhook(config: &AlertingConfig) -> bool {
    config.webhook.enabled && config.inspection.delivery.contains(&AlertChannel::Webhook)
}

fn should_send_inspection_smtp(config: &AlertingConfig) -> bool {
    config.smtp.enabled && config.inspection.delivery.contains(&AlertChannel::Smtp)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use chrono::Utc;
    use nodelite_proto::{
        AlertChannel, AlertComparator, AlertMetric, AlertRuleConfig, AlertScopeMode, AlertSeverity,
        AlertSmtpConfig, AlertSmtpTransport, AlertWebhookConfig, AlertingConfig, InspectionConfig,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::{
        AlertDeliveryError, InspectionSummary, deliver_alert_event, deliver_inspection_summary,
        retry_delivery, webhook_endpoint_label,
    };
    use crate::alerts::{AlertEvent, AlertEventKind, AlertMetricReading, InspectionReport};

    #[tokio::test]
    async fn deliver_alert_event_posts_signed_webhook_payload() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener should bind");
        let addr = listener.local_addr().expect("listener should expose addr");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("request should arrive");
            let request = read_http_request(&mut socket).await;
            socket
                .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                .await
                .expect("response should write");
            request
        });

        let config = AlertingConfig {
            enabled: true,
            webhook: AlertWebhookConfig {
                enabled: true,
                url: format!("http://{addr}/alerts?team=ops"),
                secret: Some("hook-secret".to_string()),
                send_resolved: true,
            },
            ..AlertingConfig::default()
        };
        let event = sample_event();

        deliver_alert_event(&config, &event)
            .await
            .expect("webhook should send");
        let request = server.await.expect("server task should join");

        assert!(request.starts_with("POST /alerts?team=ops HTTP/1.1"));
        assert!(request.contains("X-NodeLite-Signature: sha256="));
        assert!(request.contains("\"event\":\"triggered\""));
        assert!(request.contains("\"id\":\"cpu-hot\""));
        assert!(request.contains("\"value\":91"));
    }

    #[tokio::test]
    async fn deliver_inspection_summary_posts_webhook_payload() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener should bind");
        let addr = listener.local_addr().expect("listener should expose addr");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("request should arrive");
            let request = read_http_request(&mut socket).await;
            socket
                .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                .await
                .expect("response should write");
            request
        });
        let config = AlertingConfig {
            enabled: true,
            webhook: AlertWebhookConfig {
                enabled: true,
                url: format!("http://{addr}/inspection"),
                secret: None,
                send_resolved: true,
            },
            inspection: InspectionConfig {
                enabled: true,
                delivery: vec![AlertChannel::Webhook],
                ..InspectionConfig::default()
            },
            ..AlertingConfig::default()
        };
        let report = InspectionReport {
            total_nodes: 3,
            offline_nodes: 1,
            latency_nodes: 1,
            cpu_hot_nodes: 0,
            memory_hot_nodes: 0,
            highlights: Vec::new(),
        };
        let summary = InspectionSummary {
            occurred_at: Utc::now(),
            local_date: chrono::NaiveDate::from_ymd_opt(2026, 5, 27).expect("date should be valid"),
            lookback_hours: 24,
            report: &report,
        };

        deliver_inspection_summary(&config, &summary)
            .await
            .expect("webhook should send");
        let request = server.await.expect("server task should join");

        assert!(request.starts_with("POST /inspection HTTP/1.1"));
        assert!(request.contains("\"event\":\"inspection_summary\""));
        assert!(request.contains("\"local_date\":\"2026-05-27\""));
        assert!(request.contains("\"offline_nodes\":1"));
    }

    #[tokio::test]
    async fn deliver_alert_event_reports_webhook_and_smtp_failures() {
        let mut event = sample_event();
        event.rule.delivery = vec![AlertChannel::Webhook, AlertChannel::Smtp];
        let config = AlertingConfig {
            enabled: true,
            smtp: invalid_smtp_config(),
            webhook: AlertWebhookConfig {
                enabled: true,
                url: "://bad-url".to_string(),
                secret: None,
                send_resolved: true,
            },
            ..AlertingConfig::default()
        };

        let error = deliver_alert_event(&config, &event)
            .await
            .expect_err("both channel failures should be returned");

        match error {
            AlertDeliveryError::MultiChannel { webhook, smtp } => {
                assert!(matches!(*webhook, AlertDeliveryError::InvalidWebhookUrl(_)));
                assert!(matches!(*smtp, AlertDeliveryError::InvalidMailHeader));
            }
            other => panic!("expected multi-channel error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn deliver_alert_event_skips_resolved_smtp_when_rule_disables_it() {
        let mut event = sample_event();
        event.kind = AlertEventKind::Resolved;
        event.rule.delivery = vec![AlertChannel::Smtp];
        event.rule.send_resolved = false;
        let config = AlertingConfig {
            enabled: true,
            smtp: invalid_smtp_config(),
            ..AlertingConfig::default()
        };

        deliver_alert_event(&config, &event)
            .await
            .expect("resolved smtp delivery should be skipped when rule disables it");
    }

    #[tokio::test]
    async fn deliver_alert_event_skips_resolved_smtp_when_channel_disables_it() {
        let mut event = sample_event();
        event.kind = AlertEventKind::Resolved;
        event.rule.delivery = vec![AlertChannel::Smtp];
        let config = AlertingConfig {
            enabled: true,
            smtp: AlertSmtpConfig {
                send_resolved: false,
                ..invalid_smtp_config()
            },
            ..AlertingConfig::default()
        };

        deliver_alert_event(&config, &event)
            .await
            .expect("resolved smtp delivery should be skipped when channel disables it");
    }

    #[tokio::test]
    async fn deliver_inspection_summary_reports_webhook_and_smtp_failures() {
        let report = InspectionReport {
            total_nodes: 3,
            offline_nodes: 1,
            latency_nodes: 1,
            cpu_hot_nodes: 0,
            memory_hot_nodes: 0,
            highlights: Vec::new(),
        };
        let summary = InspectionSummary {
            occurred_at: Utc::now(),
            local_date: chrono::NaiveDate::from_ymd_opt(2026, 5, 27).expect("date should be valid"),
            lookback_hours: 24,
            report: &report,
        };
        let config = AlertingConfig {
            enabled: true,
            smtp: invalid_smtp_config(),
            webhook: AlertWebhookConfig {
                enabled: true,
                url: "://bad-url".to_string(),
                secret: None,
                send_resolved: true,
            },
            inspection: InspectionConfig {
                enabled: true,
                delivery: vec![AlertChannel::Webhook, AlertChannel::Smtp],
                ..InspectionConfig::default()
            },
            ..AlertingConfig::default()
        };

        let error = deliver_inspection_summary(&config, &summary)
            .await
            .expect_err("both inspection channel failures should be returned");

        match error {
            AlertDeliveryError::MultiChannel { webhook, smtp } => {
                assert!(matches!(*webhook, AlertDeliveryError::InvalidWebhookUrl(_)));
                assert!(matches!(*smtp, AlertDeliveryError::InvalidMailHeader));
            }
            other => panic!("expected multi-channel error, got {other:?}"),
        }
    }

    #[test]
    fn webhook_endpoint_label_omits_query_values() {
        assert_eq!(
            webhook_endpoint_label("https://hooks.example.com/path?token=secret"),
            "https://hooks.example.com/path"
        );
    }

    #[tokio::test]
    async fn retry_delivery_retries_retryable_errors() {
        let attempts = Arc::new(AtomicUsize::new(0));

        retry_delivery(|| {
            let attempts = Arc::clone(&attempts);
            async move {
                if attempts.fetch_add(1, Ordering::SeqCst) < 2 {
                    return Err(AlertDeliveryError::HttpStatus { status: 503 });
                }
                Ok(())
            }
        })
        .await
        .expect("delivery should eventually succeed");

        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn retry_delivery_does_not_retry_invalid_payloads() {
        let attempts = Arc::new(AtomicUsize::new(0));

        let error = retry_delivery(|| {
            let attempts = Arc::clone(&attempts);
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err(AlertDeliveryError::InvalidMailHeader)
            }
        })
        .await
        .expect_err("invalid payload should fail");

        assert!(matches!(error, AlertDeliveryError::InvalidMailHeader));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    fn sample_event() -> AlertEvent {
        AlertEvent {
            kind: AlertEventKind::Triggered,
            occurred_at: Utc::now(),
            rule: AlertRuleConfig {
                id: "cpu-hot".to_string(),
                name: "CPU hot".to_string(),
                enabled: true,
                metric: AlertMetric::CpuUsagePercent,
                comparator: AlertComparator::Gt,
                threshold: 90,
                window_minutes: 5,
                severity: AlertSeverity::Critical,
                scope_mode: AlertScopeMode::All,
                node_ids: Vec::new(),
                tags: Vec::new(),
                delivery: vec![AlertChannel::Webhook],
                cooldown_minutes: 30,
                send_resolved: true,
            },
            node_id: "hk-01".to_string(),
            node_label: "Hong Kong".to_string(),
            reading: Some(AlertMetricReading {
                metric: AlertMetric::CpuUsagePercent,
                value: 91,
                threshold: 90,
            }),
        }
    }

    fn invalid_smtp_config() -> AlertSmtpConfig {
        AlertSmtpConfig {
            enabled: true,
            host: "smtp.example.com".to_string(),
            port: 587,
            username: "ops".to_string(),
            password: Some("smtp-secret".to_string()),
            sender: "ops@example.com\r\n".to_string(),
            recipients: vec!["ops@example.com".to_string()],
            transport: AlertSmtpTransport::StartTls,
            send_resolved: true,
        }
    }

    async fn read_http_request(socket: &mut tokio::net::TcpStream) -> String {
        let mut data = Vec::new();
        let mut buffer = [0_u8; 1024];
        let header_end = loop {
            let read = socket.read(&mut buffer).await.expect("request should read");
            assert!(read > 0, "request should include headers");
            data.extend_from_slice(&buffer[..read]);
            if let Some(index) = find_header_end(&data) {
                break index;
            }
        };
        let headers = String::from_utf8_lossy(&data[..header_end]).to_string();
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.strip_prefix("Content-Length: ")
                    .and_then(|value| value.parse::<usize>().ok())
            })
            .expect("content length should be present");
        while data.len() < header_end + 4 + content_length {
            let read = socket.read(&mut buffer).await.expect("body should read");
            assert!(read > 0, "body should be complete");
            data.extend_from_slice(&buffer[..read]);
        }
        String::from_utf8(data).expect("request should be utf8")
    }

    fn find_header_end(data: &[u8]) -> Option<usize> {
        data.windows(4).position(|window| window == b"\r\n\r\n")
    }
}
