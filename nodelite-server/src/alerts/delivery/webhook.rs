use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, NaiveDate, Utc};
use hmac::{Hmac, Mac};
use nodelite_proto::{AlertMetric, AlertSeverity, AlertWebhookConfig};
use serde::Serialize;
use sha2::Sha256;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::{ClientConfig, RootCertStore, pki_types::ServerName};
use url::Url;

use super::{AlertDeliveryError, InspectionSummary};
use crate::alerts::AlertEvent;

const WEBHOOK_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RESPONSE_HEADER_BYTES: usize = 32 * 1024;

type HmacSha256 = Hmac<Sha256>;

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

pub(crate) async fn send_alert_event(
    config: &AlertWebhookConfig,
    event: &AlertEvent,
) -> Result<(), AlertDeliveryError> {
    let notification = notification_from_event(event);
    send_notification(config, &notification).await
}

pub(crate) async fn send_inspection_summary(
    config: &AlertWebhookConfig,
    summary: &InspectionSummary<'_>,
) -> Result<(), AlertDeliveryError> {
    let notification = inspection_notification(summary);
    send_notification(config, &notification).await
}

pub(crate) fn endpoint_label(url: &str) -> String {
    let Ok(parsed) = Url::parse(url) else {
        return "invalid-webhook-url".to_string();
    };
    let host = parsed.host_str().unwrap_or("unknown-host");
    format!("{}://{}{}", parsed.scheme(), host, parsed.path())
}

async fn send_notification<T: Serialize>(
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
