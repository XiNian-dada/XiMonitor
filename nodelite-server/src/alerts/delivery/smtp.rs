use std::sync::Arc;
use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use nodelite_proto::{AlertSmtpConfig, AlertSmtpTransport};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::{ClientConfig, RootCertStore, pki_types::ServerName};

use crate::alerts::AlertEvent;

use super::{AlertDeliveryError, InspectionSummary};

const SMTP_TIMEOUT: Duration = Duration::from_secs(15);
const SMTP_MAX_RESPONSE_BYTES: usize = 16 * 1024;
const SMTP_HELO_NAME: &str = "nodelite.local";
const INSPECTION_BOUNDARY: &str = "nodelite-inspection-alt";

pub(super) async fn send_alert_event(
    config: &AlertSmtpConfig,
    event: &AlertEvent,
) -> Result<(), AlertDeliveryError> {
    let message = build_alert_message(config, event)?;
    timeout(SMTP_TIMEOUT, send_smtp_inner(config, message))
        .await
        .map_err(|_| AlertDeliveryError::SmtpTimeout)?
}

pub(super) async fn send_inspection_summary(
    config: &AlertSmtpConfig,
    summary: &InspectionSummary<'_>,
) -> Result<(), AlertDeliveryError> {
    let message = build_inspection_message(config, summary)?;
    timeout(SMTP_TIMEOUT, send_smtp_inner(config, message))
        .await
        .map_err(|_| AlertDeliveryError::SmtpTimeout)?
}

async fn send_smtp_inner(
    config: &AlertSmtpConfig,
    message: String,
) -> Result<(), AlertDeliveryError> {
    validate_smtp_config(config)?;
    let tcp = TcpStream::connect((config.host.as_str(), config.port)).await?;
    match config.transport {
        AlertSmtpTransport::Tls => {
            let mut stream = tls_connect(tcp, &config.host).await?;
            run_smtp_dialog(&mut stream, config, &message, false).await
        }
        AlertSmtpTransport::StartTls => {
            let mut stream = tcp;
            expect_response(&mut stream, &[220]).await?;
            send_ehlo(&mut stream).await?;
            send_command(&mut stream, "STARTTLS").await?;
            expect_response(&mut stream, &[220]).await?;
            let mut stream = tls_connect(stream, &config.host).await?;
            run_smtp_dialog(&mut stream, config, &message, true).await
        }
        AlertSmtpTransport::Plain => {
            let mut stream = tcp;
            run_smtp_dialog(&mut stream, config, &message, false).await
        }
    }
}

async fn run_smtp_dialog<S>(
    stream: &mut S,
    config: &AlertSmtpConfig,
    message: &str,
    greeted: bool,
) -> Result<(), AlertDeliveryError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if !greeted {
        expect_response(stream, &[220]).await?;
        send_ehlo(stream).await?;
    } else {
        send_ehlo(stream).await?;
    }

    if !config.username.is_empty() {
        authenticate(stream, config).await?;
    }
    send_command(stream, &format!("MAIL FROM:<{}>", config.sender)).await?;
    expect_response(stream, &[250]).await?;
    for recipient in &config.recipients {
        send_command(stream, &format!("RCPT TO:<{recipient}>")).await?;
        expect_response(stream, &[250, 251]).await?;
    }
    send_command(stream, "DATA").await?;
    expect_response(stream, &[354]).await?;
    stream.write_all(dot_stuff(message).as_bytes()).await?;
    stream.write_all(b"\r\n.\r\n").await?;
    stream.flush().await?;
    expect_response(stream, &[250]).await?;
    send_command(stream, "QUIT").await?;
    let _ = read_response(stream).await;
    Ok(())
}

async fn authenticate<S>(stream: &mut S, config: &AlertSmtpConfig) -> Result<(), AlertDeliveryError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let password = config.password.as_deref().unwrap_or_default();
    let payload = STANDARD.encode(format!("\0{}\0{password}", config.username));
    send_command(stream, &format!("AUTH PLAIN {payload}")).await?;
    expect_response(stream, &[235]).await
}

async fn send_ehlo<S>(stream: &mut S) -> Result<(), AlertDeliveryError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    send_command(stream, &format!("EHLO {SMTP_HELO_NAME}")).await?;
    expect_response(stream, &[250]).await
}

async fn send_command<S>(stream: &mut S, command: &str) -> Result<(), AlertDeliveryError>
where
    S: AsyncWrite + Unpin,
{
    stream.write_all(command.as_bytes()).await?;
    stream.write_all(b"\r\n").await?;
    stream.flush().await?;
    Ok(())
}

async fn expect_response<S>(stream: &mut S, expected: &[u16]) -> Result<(), AlertDeliveryError>
where
    S: AsyncRead + Unpin,
{
    let response = read_response(stream).await?;
    if expected.contains(&response.code) {
        return Ok(());
    }
    Err(AlertDeliveryError::Smtp(response.message))
}

async fn read_response<S>(stream: &mut S) -> Result<SmtpResponse, AlertDeliveryError>
where
    S: AsyncRead + Unpin,
{
    let mut bytes = Vec::new();
    let mut line = Vec::new();
    let mut one = [0_u8; 1];
    loop {
        let read = stream.read(&mut one).await?;
        if read == 0 {
            return Err(AlertDeliveryError::Smtp(
                "connection closed before SMTP response completed".to_string(),
            ));
        }
        bytes.push(one[0]);
        line.push(one[0]);
        if bytes.len() > SMTP_MAX_RESPONSE_BYTES {
            return Err(AlertDeliveryError::Smtp(
                "SMTP response exceeded maximum size".to_string(),
            ));
        }
        if line.ends_with(b"\r\n") {
            if is_final_smtp_line(&line) {
                let message = String::from_utf8_lossy(&bytes).trim().to_string();
                let code = parse_smtp_code(&line)?;
                return Ok(SmtpResponse { code, message });
            }
            line.clear();
        }
    }
}

fn is_final_smtp_line(line: &[u8]) -> bool {
    line.len() >= 5 && line[0..3].iter().all(u8::is_ascii_digit) && line[3] == b' '
}

fn parse_smtp_code(line: &[u8]) -> Result<u16, AlertDeliveryError> {
    let code = std::str::from_utf8(&line[0..3])
        .map_err(|_| AlertDeliveryError::Smtp("SMTP response code was invalid".to_string()))?
        .parse::<u16>()
        .map_err(|_| AlertDeliveryError::Smtp("SMTP response code was invalid".to_string()))?;
    Ok(code)
}

#[derive(Debug)]
struct SmtpResponse {
    code: u16,
    message: String,
}

async fn tls_connect(
    stream: TcpStream,
    host: &str,
) -> Result<tokio_rustls::client::TlsStream<TcpStream>, AlertDeliveryError> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = TlsConnector::from(Arc::new(config));
    let server_name = ServerName::try_from(host.to_string())
        .map_err(|error| AlertDeliveryError::Tls(error.to_string()))?;
    connector
        .connect(server_name, stream)
        .await
        .map_err(|error| AlertDeliveryError::Tls(error.to_string()))
}

fn validate_smtp_config(config: &AlertSmtpConfig) -> Result<(), AlertDeliveryError> {
    validate_header_value(&config.sender)?;
    validate_header_value(&config.host)?;
    validate_header_value(&config.username)?;
    if let Some(password) = config.password.as_deref() {
        validate_header_value(password)?;
    }
    for recipient in &config.recipients {
        validate_header_value(recipient)?;
    }
    Ok(())
}

fn build_alert_message(
    config: &AlertSmtpConfig,
    event: &AlertEvent,
) -> Result<String, AlertDeliveryError> {
    validate_header_value(&event.rule.name)?;
    validate_header_value(&event.node_label)?;
    let subject = format!(
        "[NodeLite] {} {} on {}",
        event.kind.as_str(),
        event.rule.name,
        event.node_label
    );
    validate_header_value(&subject)?;
    let recipients = config.recipients.join(", ");
    validate_header_value(&recipients)?;

    Ok(format!(
        "From: {}\r\nTo: {}\r\nSubject: {}\r\nDate: {}\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n{}",
        config.sender,
        recipients,
        subject,
        event.occurred_at.to_rfc2822(),
        alert_message_body(event),
    ))
}

fn build_inspection_message(
    config: &AlertSmtpConfig,
    summary: &InspectionSummary<'_>,
) -> Result<String, AlertDeliveryError> {
    let subject = format!("[NodeLite] Daily inspection {}", summary.local_date);
    validate_header_value(&subject)?;
    let recipients = config.recipients.join(", ");
    validate_header_value(&recipients)?;

    let text = inspection_message_body(summary);
    let html = inspection_message_html(summary);

    Ok(format!(
        concat!(
            "From: {}\r\n",
            "To: {}\r\n",
            "Subject: {}\r\n",
            "Date: {}\r\n",
            "MIME-Version: 1.0\r\n",
            "Content-Type: multipart/alternative; boundary=\"{}\"\r\n",
            "\r\n",
            "--{}\r\n",
            "Content-Type: text/plain; charset=utf-8\r\n",
            "Content-Transfer-Encoding: 8bit\r\n",
            "\r\n",
            "{}\r\n",
            "--{}\r\n",
            "Content-Type: text/html; charset=utf-8\r\n",
            "Content-Transfer-Encoding: 8bit\r\n",
            "\r\n",
            "{}\r\n",
            "--{}--\r\n"
        ),
        config.sender,
        recipients,
        subject,
        summary.occurred_at.to_rfc2822(),
        INSPECTION_BOUNDARY,
        INSPECTION_BOUNDARY,
        text,
        INSPECTION_BOUNDARY,
        html,
        INSPECTION_BOUNDARY,
    ))
}

fn alert_message_body(event: &AlertEvent) -> String {
    let mut body = format!(
        "NodeLite alert {}\n\nRule: {} ({})\nSeverity: {:?}\nNode: {} ({})\nTime: {}\n",
        event.kind.as_str(),
        event.rule.name,
        event.rule.id,
        event.rule.severity,
        event.node_label,
        event.node_id,
        event.occurred_at.to_rfc3339(),
    );
    if let Some(reading) = event.reading.as_ref() {
        body.push_str(&format!(
            "Metric: {:?}\nValue: {}\nThreshold: {}\n",
            reading.metric, reading.value, reading.threshold
        ));
    }
    body
}

fn inspection_message_body(summary: &InspectionSummary<'_>) -> String {
    let report = summary.report;
    let mut body = format!(
        "NodeLite daily inspection summary\n\nDate: {}\nLookback: {}h\nGenerated: {}\n\nTotal nodes: {}\nOffline: {}\nHigh latency: {}\nCPU hot: {}\nMemory hot: {}\n",
        summary.local_date,
        summary.lookback_hours,
        summary.occurred_at.to_rfc3339(),
        report.total_nodes,
        report.offline_nodes,
        report.latency_nodes,
        report.cpu_hot_nodes,
        report.memory_hot_nodes,
    );
    if !report.highlights.is_empty() {
        body.push_str("\nHighlights:\n");
        for highlight in report.highlights.iter().take(20) {
            body.push_str(&format!(
                "- {} ({}): {}\n",
                highlight.node_label,
                highlight.node_id,
                highlight.reasons.join(", ")
            ));
        }
        if report.highlights.len() > 20 {
            body.push_str(&format!(
                "- ... {} more nodes\n",
                report.highlights.len() - 20
            ));
        }
    }
    body
}

fn inspection_message_html(summary: &InspectionSummary<'_>) -> String {
    let report = summary.report;
    let cards = [
        ("Total nodes", report.total_nodes, "#2563eb"),
        ("Offline", report.offline_nodes, "#ef4444"),
        ("High latency", report.latency_nodes, "#eab308"),
        ("CPU hot", report.cpu_hot_nodes, "#f97316"),
        ("Memory hot", report.memory_hot_nodes, "#f97316"),
    ]
    .iter()
    .map(|(label, value, color)| {
        format!(
            r#"<td style="padding:0 8px 8px 0;width:20%;"><div style="border:1px solid #dbe3f0;border-radius:12px;padding:14px 16px;background:#f8fafc;"><div style="font-size:12px;color:#64748b;">{}</div><div style="margin-top:6px;font-size:28px;line-height:1;font-weight:700;color:{};">{}</div></div></td>"#,
            escape_html(label),
            color,
            value,
        )
    })
    .collect::<String>();
    let highlights = inspection_highlights_html(report);

    format!(
        concat!(
            r#"<!doctype html><html><body style="margin:0;padding:0;background:#f1f5f9;font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif;color:#0f172a;">"#,
            r#"<div style="max-width:760px;margin:0 auto;padding:28px 18px;">"#,
            r#"<div style="border-radius:18px;background:#0f172a;color:#f8fafc;padding:24px 26px;">"#,
            r#"<div style="font-size:13px;color:#93c5fd;letter-spacing:.04em;text-transform:uppercase;">NodeLite Daily Inspection</div>"#,
            r#"<h1 style="margin:8px 0 0;font-size:24px;line-height:1.25;">{}</h1>"#,
            r#"<p style="margin:10px 0 0;color:#cbd5e1;font-size:14px;">Lookback {}h · Generated {}</p>"#,
            r#"</div>"#,
            r#"<table role="presentation" width="100%" cellspacing="0" cellpadding="0" style="margin-top:18px;border-collapse:collapse;"><tr>{}</tr></table>"#,
            r#"<div style="margin-top:18px;border:1px solid #dbe3f0;border-radius:16px;background:#ffffff;padding:20px 22px;">"#,
            r#"<h2 style="margin:0 0 12px;font-size:16px;">Inspection highlights</h2>"#,
            "{}",
            r#"</div>"#,
            r#"<p style="margin:18px 0 0;color:#64748b;font-size:12px;">This report was generated by NodeLite.</p>"#,
            r#"</div></body></html>"#
        ),
        escape_html(&summary.local_date.to_string()),
        summary.lookback_hours,
        escape_html(&summary.occurred_at.to_rfc3339()),
        cards,
        highlights,
    )
}

fn inspection_highlights_html(report: &crate::alerts::InspectionReport) -> String {
    if report.highlights.is_empty() {
        return r#"<p style="margin:0;color:#16a34a;font-size:14px;">No notable nodes in this inspection window.</p>"#
            .to_string();
    }

    let mut html = String::from(
        r#"<table role="presentation" width="100%" cellspacing="0" cellpadding="0" style="border-collapse:collapse;">"#,
    );
    for highlight in report.highlights.iter().take(20) {
        let reasons = highlight
            .reasons
            .iter()
            .map(|reason| {
                format!(
                    r#"<span style="display:inline-block;margin:2px 4px 2px 0;border-radius:999px;background:#fee2e2;color:#991b1b;padding:3px 8px;font-size:12px;">{}</span>"#,
                    escape_html(reason)
                )
            })
            .collect::<String>();
        html.push_str(&format!(
            concat!(
                r#"<tr><td style="padding:10px 0;border-top:1px solid #e2e8f0;">"#,
                r#"<div style="font-size:14px;font-weight:600;color:#0f172a;">{}</div>"#,
                r#"<div style="margin-top:2px;font-size:12px;color:#64748b;">{}</div>"#,
                r#"</td><td style="padding:10px 0;border-top:1px solid #e2e8f0;text-align:right;">{}</td></tr>"#
            ),
            escape_html(&highlight.node_label),
            escape_html(&highlight.node_id),
            reasons,
        ));
    }
    if report.highlights.len() > 20 {
        html.push_str(&format!(
            r#"<tr><td colspan="2" style="padding:10px 0 0;color:#64748b;font-size:13px;">{} more nodes omitted.</td></tr>"#,
            report.highlights.len() - 20
        ));
    }
    html.push_str("</table>");
    html
}

fn escape_html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn validate_header_value(value: &str) -> Result<(), AlertDeliveryError> {
    if value.contains('\r') || value.contains('\n') {
        return Err(AlertDeliveryError::InvalidMailHeader);
    }
    Ok(())
}

fn dot_stuff(message: &str) -> String {
    message
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .split('\n')
        .map(|line| {
            if line.starts_with('.') {
                format!(".{line}")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\r\n")
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use nodelite_proto::{
        AlertChannel, AlertComparator, AlertMetric, AlertRuleConfig, AlertScopeMode, AlertSeverity,
        AlertSmtpConfig, AlertSmtpTransport,
    };
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpListener;

    use super::{build_alert_message, build_inspection_message, dot_stuff, send_alert_event};
    use crate::alerts::evaluator::InspectionHighlight;
    use crate::alerts::{AlertEvent, AlertEventKind, AlertMetricReading};

    #[tokio::test]
    async fn send_smtp_delivers_plain_message() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener should bind");
        let addr = listener.local_addr().expect("listener should expose addr");
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.expect("smtp client should connect");
            run_fake_smtp(socket).await
        });
        let config = smtp_config(addr.port());

        send_alert_event(&config, &sample_event())
            .await
            .expect("smtp should send");
        let session = server.await.expect("fake smtp should join");

        assert!(
            session
                .commands
                .iter()
                .any(|line| line == "EHLO nodelite.local")
        );
        assert!(
            session
                .commands
                .iter()
                .any(|line| line == "MAIL FROM:<ops@example.com>")
        );
        assert!(
            session
                .commands
                .iter()
                .any(|line| line == "RCPT TO:<oncall@example.com>")
        );
        assert!(
            session
                .message
                .contains("Subject: [NodeLite] triggered CPU hot on Hong Kong")
        );
        assert!(session.message.contains("Metric: CpuUsagePercent"));
    }

    #[test]
    fn build_message_rejects_header_injection() {
        let mut event = sample_event();
        event.node_label = "good\r\nBcc: bad@example.com".to_string();

        assert!(build_alert_message(&smtp_config(25), &event).is_err());
    }

    #[test]
    fn dot_stuff_prefixes_lines_starting_with_dot() {
        assert_eq!(dot_stuff("first\n.second"), "first\r\n..second");
    }

    #[test]
    fn build_inspection_message_includes_totals_and_highlights() {
        let report = crate::alerts::InspectionReport {
            total_nodes: 2,
            offline_nodes: 1,
            latency_nodes: 1,
            cpu_hot_nodes: 0,
            memory_hot_nodes: 0,
            highlights: vec![InspectionHighlight {
                node_id: "hk-01".to_string(),
                node_label: "Hong Kong <edge>".to_string(),
                reasons: vec!["offline".to_string(), "latency & jitter".to_string()],
            }],
        };
        let summary = super::InspectionSummary {
            occurred_at: Utc::now(),
            local_date: chrono::NaiveDate::from_ymd_opt(2026, 5, 27).expect("date should be valid"),
            lookback_hours: 24,
            report: &report,
        };

        let message =
            build_inspection_message(&smtp_config(25), &summary).expect("message should build");

        assert!(message.contains("Subject: [NodeLite] Daily inspection 2026-05-27"));
        assert!(message.contains("Content-Type: multipart/alternative"));
        assert!(message.contains("Content-Type: text/plain; charset=utf-8"));
        assert!(message.contains("Content-Type: text/html; charset=utf-8"));
        assert!(message.contains("Total nodes: 2"));
        assert!(message.contains("- Hong Kong <edge> (hk-01): offline, latency & jitter"));
        assert!(message.contains("NodeLite Daily Inspection"));
        assert!(message.contains("High latency"));
        assert!(message.contains("Hong Kong &lt;edge&gt;"));
        assert!(message.contains("latency &amp; jitter"));
    }

    async fn run_fake_smtp(socket: tokio::net::TcpStream) -> SmtpSession {
        let (read_half, mut write_half) = socket.into_split();
        let mut reader = BufReader::new(read_half);
        let mut commands = Vec::new();
        let mut message = String::new();

        write_half
            .write_all(b"220 fake.smtp ESMTP\r\n")
            .await
            .expect("greeting should write");
        loop {
            let mut line = String::new();
            reader
                .read_line(&mut line)
                .await
                .expect("command should read");
            let command = line.trim_end_matches(['\r', '\n']).to_string();
            commands.push(command.clone());
            if command.starts_with("EHLO ") {
                write_half
                    .write_all(b"250-fake.smtp\r\n250 AUTH PLAIN\r\n")
                    .await
                    .expect("ehlo response should write");
            } else if command.starts_with("MAIL FROM:") || command.starts_with("RCPT TO:") {
                write_half
                    .write_all(b"250 OK\r\n")
                    .await
                    .expect("mail response should write");
            } else if command == "DATA" {
                write_half
                    .write_all(b"354 End data with <CR><LF>.<CR><LF>\r\n")
                    .await
                    .expect("data response should write");
                loop {
                    let mut body_line = String::new();
                    reader
                        .read_line(&mut body_line)
                        .await
                        .expect("message should read");
                    if body_line == ".\r\n" {
                        break;
                    }
                    message.push_str(&body_line);
                }
                write_half
                    .write_all(b"250 Queued\r\n")
                    .await
                    .expect("queued response should write");
            } else if command == "QUIT" {
                write_half
                    .write_all(b"221 Bye\r\n")
                    .await
                    .expect("quit response should write");
                break;
            } else {
                write_half
                    .write_all(b"250 OK\r\n")
                    .await
                    .expect("generic response should write");
            }
        }

        SmtpSession { commands, message }
    }

    struct SmtpSession {
        commands: Vec<String>,
        message: String,
    }

    fn smtp_config(port: u16) -> AlertSmtpConfig {
        AlertSmtpConfig {
            enabled: true,
            host: "127.0.0.1".to_string(),
            port,
            username: String::new(),
            password: None,
            sender: "ops@example.com".to_string(),
            recipients: vec!["oncall@example.com".to_string()],
            transport: AlertSmtpTransport::Plain,
            send_resolved: true,
        }
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
                delivery: vec![AlertChannel::Smtp],
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
}
