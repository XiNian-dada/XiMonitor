use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, NaiveDate, Utc};
use hmac::{Hmac, Mac};
use nodelite_proto::{
    AlertChannel, AlertMetric, AlertSeverity, AlertSmtpConfig, AlertWebhookConfig, AlertingConfig,
};
use serde::Serialize;
use sha2::Sha256;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::{ClientConfig, RootCertStore, pki_types::ServerName};
use url::Url;

use super::{AlertEvent, AlertEventKind, InspectionReport};

mod smtp;

const WEBHOOK_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RESPONSE_HEADER_BYTES: usize = 32 * 1024;

type HmacSha256 = Hmac<Sha256>;

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
}

#[derive(Debug, Serialize)]
struct AlertNotification<'a> {
    version: u8,
    source: &'static str,
    event: &'static str,
    occurred_at: DateTime<Utc>,
    rule: AlertRuleNotification<'a>,
    node: AlertNodeNotification<'a>,
    reading: Option<AlertReadingNotification>,
}

#[derive(Debug, Serialize)]
struct AlertRuleNotification<'a> {
    id: &'a str,
    name: &'a str,
    severity: &'a AlertSeverity,
}

#[derive(Debug, Serialize)]
struct AlertNodeNotification<'a> {
    id: &'a str,
    label: &'a str,
}

#[derive(Debug, Serialize)]
struct AlertReadingNotification {
    metric: AlertMetric,
    value: u64,
    threshold: u64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct InspectionSummary<'a> {
    pub(crate) occurred_at: DateTime<Utc>,
    pub(crate) local_date: NaiveDate,
    pub(crate) lookback_hours: u64,
    pub(crate) report: &'a InspectionReport,
}

#[derive(Debug, Serialize)]
struct InspectionSummaryNotification<'a> {
    version: u8,
    source: &'static str,
    event: &'static str,
    occurred_at: DateTime<Utc>,
    local_date: NaiveDate,
    lookback_hours: u64,
    totals: InspectionTotalsNotification,
    highlights: Vec<InspectionHighlightNotification<'a>>,
}

#[derive(Debug, Serialize)]
struct InspectionTotalsNotification {
    total_nodes: usize,
    offline_nodes: usize,
    latency_nodes: usize,
    cpu_hot_nodes: usize,
    memory_hot_nodes: usize,
}

#[derive(Debug, Serialize)]
struct InspectionHighlightNotification<'a> {
    node: AlertNodeNotification<'a>,
    reasons: &'a [String],
}

pub(crate) async fn deliver_alert_event(
    config: &AlertingConfig,
    event: &AlertEvent,
) -> Result<(), AlertDeliveryError> {
    let mut first_error = None;
    if should_send_webhook(config, event) {
        if let Err(error) =
            send_webhook_notification(&config.webhook, &notification_from_event(event)).await
        {
            first_error = Some(error);
        }
    }
    if should_send_smtp(config, event)
        && let Err(error) = smtp::send_alert_event(&config.smtp, event).await
        && first_error.is_none()
    {
        first_error = Some(error);
    }

    delivery_result(first_error)
}

pub(crate) async fn deliver_inspection_summary(
    config: &AlertingConfig,
    summary: &InspectionSummary<'_>,
) -> Result<(), AlertDeliveryError> {
    let mut first_error = None;
    if should_send_inspection_webhook(config) {
        let notification = inspection_notification(summary);
        if let Err(error) = send_webhook_notification(&config.webhook, &notification).await {
            first_error = Some(error);
        }
    }
    if should_send_inspection_smtp(config)
        && let Err(error) = smtp::send_inspection_summary(&config.smtp, summary).await
        && first_error.is_none()
    {
        first_error = Some(error);
    }

    delivery_result(first_error)
}

fn delivery_result(first_error: Option<AlertDeliveryError>) -> Result<(), AlertDeliveryError> {
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

pub(crate) fn webhook_endpoint_label(url: &str) -> String {
    let Ok(parsed) = Url::parse(url) else {
        return "invalid-webhook-url".to_string();
    };
    let host = parsed.host_str().unwrap_or("unknown-host");
    format!("{}://{}{}", parsed.scheme(), host, parsed.path())
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
    !matches!(event.kind, AlertEventKind::Resolved) || config.webhook.send_resolved
}

fn should_send_smtp(config: &AlertingConfig, event: &AlertEvent) -> bool {
    config.smtp.enabled && event.rule.delivery.contains(&AlertChannel::Smtp)
}

fn should_send_inspection_webhook(config: &AlertingConfig) -> bool {
    config.webhook.enabled && config.inspection.delivery.contains(&AlertChannel::Webhook)
}

fn should_send_inspection_smtp(config: &AlertingConfig) -> bool {
    config.smtp.enabled && config.inspection.delivery.contains(&AlertChannel::Smtp)
}

async fn send_webhook_notification<T: Serialize>(
    config: &AlertWebhookConfig,
    notification: &T,
) -> Result<(), AlertDeliveryError> {
    let url = Url::parse(&config.url)?;
    let payload = serde_json::to_vec(notification)?;
    timeout(
        WEBHOOK_TIMEOUT,
        send_http_post(url, &payload, config.secret.as_deref()),
    )
    .await
    .map_err(|_| AlertDeliveryError::Timeout)?
}

fn notification_from_event(event: &AlertEvent) -> AlertNotification<'_> {
    AlertNotification {
        version: 1,
        source: "nodelite",
        event: event.kind.as_str(),
        occurred_at: event.occurred_at,
        rule: AlertRuleNotification {
            id: &event.rule.id,
            name: &event.rule.name,
            severity: &event.rule.severity,
        },
        node: AlertNodeNotification {
            id: &event.node_id,
            label: &event.node_label,
        },
        reading: event
            .reading
            .as_ref()
            .map(|reading| AlertReadingNotification {
                metric: reading.metric.clone(),
                value: reading.value,
                threshold: reading.threshold,
            }),
    }
}

fn inspection_notification<'a>(
    summary: &'a InspectionSummary<'a>,
) -> InspectionSummaryNotification<'a> {
    let report = summary.report;
    InspectionSummaryNotification {
        version: 1,
        source: "nodelite",
        event: "inspection_summary",
        occurred_at: summary.occurred_at,
        local_date: summary.local_date,
        lookback_hours: summary.lookback_hours,
        totals: InspectionTotalsNotification {
            total_nodes: report.total_nodes,
            offline_nodes: report.offline_nodes,
            latency_nodes: report.latency_nodes,
            cpu_hot_nodes: report.cpu_hot_nodes,
            memory_hot_nodes: report.memory_hot_nodes,
        },
        highlights: report
            .highlights
            .iter()
            .map(|highlight| InspectionHighlightNotification {
                node: AlertNodeNotification {
                    id: &highlight.node_id,
                    label: &highlight.node_label,
                },
                reasons: &highlight.reasons,
            })
            .collect(),
    }
}

async fn send_http_post(
    url: Url,
    payload: &[u8],
    secret: Option<&str>,
) -> Result<(), AlertDeliveryError> {
    let host = url
        .host_str()
        .ok_or(AlertDeliveryError::MissingWebhookHost)?
        .to_string();
    let port = url
        .port_or_known_default()
        .ok_or(AlertDeliveryError::UnsupportedWebhookScheme)?;
    let mut stream = connect_webhook_stream(url.scheme(), &host, port).await?;
    let request = build_webhook_request(&url, &host, payload, secret)?;
    stream.write_all(&request).await?;
    stream.flush().await?;
    let status = read_response_status(&mut stream).await?;
    if !(200..300).contains(&status) {
        return Err(AlertDeliveryError::HttpStatus { status });
    }
    Ok(())
}

trait AsyncReadWrite: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T> AsyncReadWrite for T where T: AsyncRead + AsyncWrite + Unpin + Send {}

async fn connect_webhook_stream(
    scheme: &str,
    host: &str,
    port: u16,
) -> Result<Box<dyn AsyncReadWrite>, AlertDeliveryError> {
    let tcp = TcpStream::connect((host, port)).await?;
    match scheme {
        "http" => Ok(Box::new(tcp)),
        "https" => {
            let mut roots = RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            let config = ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth();
            let connector = TlsConnector::from(Arc::new(config));
            let server_name = ServerName::try_from(host.to_string())
                .map_err(|error| AlertDeliveryError::Tls(error.to_string()))?;
            let tls = connector
                .connect(server_name, tcp)
                .await
                .map_err(|error| AlertDeliveryError::Tls(error.to_string()))?;
            Ok(Box::new(tls))
        }
        _ => Err(AlertDeliveryError::UnsupportedWebhookScheme),
    }
}

fn build_webhook_request(
    url: &Url,
    host: &str,
    payload: &[u8],
    secret: Option<&str>,
) -> Result<Vec<u8>, AlertDeliveryError> {
    let mut request = format!(
        "POST {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: NodeLite/{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        request_target(url),
        host_header(url, host),
        env!("CARGO_PKG_VERSION"),
        payload.len(),
    );
    if let Some(secret) = secret.filter(|secret| !secret.is_empty()) {
        request.push_str("X-NodeLite-Signature: ");
        request.push_str(&webhook_signature(secret, payload)?);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    let mut bytes = request.into_bytes();
    bytes.extend_from_slice(payload);
    Ok(bytes)
}

fn request_target(url: &Url) -> String {
    let path = if url.path().is_empty() {
        "/"
    } else {
        url.path()
    };
    match url.query() {
        Some(query) => format!("{path}?{query}"),
        None => path.to_string(),
    }
}

fn host_header(url: &Url, host: &str) -> String {
    match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    }
}

fn webhook_signature(secret: &str, payload: &[u8]) -> Result<String, AlertDeliveryError> {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|error| AlertDeliveryError::Signature(error.to_string()))?;
    mac.update(payload);
    Ok(format!(
        "sha256={}",
        hex::encode(mac.finalize().into_bytes())
    ))
}

async fn read_response_status<S>(stream: &mut S) -> Result<u16, AlertDeliveryError>
where
    S: AsyncRead + Unpin + ?Sized,
{
    let mut response = Vec::new();
    let mut buffer = [0_u8; 1024];
    let header_end = loop {
        if let Some(index) = header_end_index(&response) {
            break index;
        }
        if response.len() > MAX_RESPONSE_HEADER_BYTES {
            return Err(AlertDeliveryError::ResponseTooLarge);
        }

        let read = stream.read(&mut buffer).await?;
        if read == 0 {
            break response.len();
        }
        response.extend_from_slice(&buffer[..read]);
    };
    let headers = std::str::from_utf8(&response[..header_end])
        .map_err(|_| AlertDeliveryError::InvalidResponse)?;
    parse_status(headers)
}

fn header_end_index(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}

fn parse_status(response_headers: &str) -> Result<u16, AlertDeliveryError> {
    let status_line = response_headers
        .lines()
        .next()
        .ok_or(AlertDeliveryError::InvalidResponse)?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .ok_or(AlertDeliveryError::InvalidResponse)?
        .parse::<u16>()
        .map_err(|_| AlertDeliveryError::InvalidResponse)?;
    Ok(status)
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use nodelite_proto::{
        AlertChannel, AlertComparator, AlertMetric, AlertRuleConfig, AlertScopeMode, AlertSeverity,
        AlertWebhookConfig, AlertingConfig, InspectionConfig,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::{
        InspectionSummary, deliver_alert_event, deliver_inspection_summary, webhook_endpoint_label,
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

    #[test]
    fn webhook_endpoint_label_omits_query_values() {
        assert_eq!(
            webhook_endpoint_label("https://hooks.example.com/path?token=secret"),
            "https://hooks.example.com/path"
        );
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
