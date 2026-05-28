use std::path::PathBuf;

use ipnet::IpNet;

use super::{
    AlertChannel, AlertComparator, AlertMetric, AlertScopeMode, AlertSeverity, AlertSmtpTransport,
    DEFAULT_ALERT_INSPECTION_LOCAL_TIME, DEFAULT_AUDIT_RETENTION_DAYS, DEFAULT_MAX_MESSAGE_BYTES,
    DEFAULT_WS_AUTH_BLOCK_SECS, DEFAULT_WS_AUTH_FAIL_MAX_ATTEMPTS,
    DEFAULT_WS_AUTH_FAIL_WINDOW_SECS, DEFAULT_WS_MAX_CONNECTIONS_PER_IP,
    DEFAULT_WS_MAX_TOTAL_CONNECTIONS, MAX_NODE_TAG_BYTES, parse_agent_config, parse_server_config,
};

#[test]
fn parses_server_config_with_defaults() {
    let config = parse_server_config(
        r#"
        [server]
        listen = "127.0.0.1:8080"
        public_base_url = "http://127.0.0.1:8080"
        "#,
    )
    .expect("server config should parse");

    assert_eq!(config.listen.to_string(), "127.0.0.1:8080");
    assert!(!config.insecure_allow_http);
    assert_eq!(config.readonly_auth, None);
    assert!(config.trusted_proxies.is_empty());
    assert_eq!(config.max_message_bytes, DEFAULT_MAX_MESSAGE_BYTES);
    assert_eq!(
        config.ws.max_total_connections,
        DEFAULT_WS_MAX_TOTAL_CONNECTIONS
    );
    assert_eq!(
        config.ws.max_connections_per_ip,
        DEFAULT_WS_MAX_CONNECTIONS_PER_IP
    );
    assert_eq!(
        config.ws.auth_fail_window_secs,
        DEFAULT_WS_AUTH_FAIL_WINDOW_SECS
    );
    assert_eq!(
        config.ws.auth_fail_max_attempts,
        DEFAULT_WS_AUTH_FAIL_MAX_ATTEMPTS
    );
    assert_eq!(config.ws.auth_block_secs, DEFAULT_WS_AUTH_BLOCK_SECS);
    assert_eq!(
        config.node_registry_path,
        PathBuf::from("./config/server.json")
    );
    assert_eq!(
        config.ignored_filesystems,
        vec!["devtmpfs", "overlay", "tmpfs"]
    );
    assert!(config.audit.enabled);
    assert_eq!(config.audit.db_path, PathBuf::from("./data/audit.sqlite3"));
    assert_eq!(config.audit.retention_days, DEFAULT_AUDIT_RETENTION_DAYS);
    assert!(config.audit.log_successful_auth);
    assert!(config.audit.log_failed_auth);
    assert!(config.audit.log_token_events);
    assert!(config.audit.log_rate_limit);
    assert!(!config.alerting.enabled);
    assert_eq!(config.alerting.rules, Vec::new());
    assert_eq!(
        config.alerting.inspection.local_time,
        DEFAULT_ALERT_INSPECTION_LOCAL_TIME
    );
}

#[test]
fn parses_server_config_with_trusted_proxies() {
    let config = parse_server_config(
        r#"
        [server]
        listen = "127.0.0.1:8080"
        public_base_url = "https://monitor.example.com"
        trusted_proxies = ["203.0.113.0/24", "2001:db8::/32"]
        "#,
    )
    .expect("trusted proxy config should parse");

    assert_eq!(
        config.trusted_proxies,
        vec![
            "2001:db8::/32".parse::<IpNet>().expect("ipv6 cidr"),
            "203.0.113.0/24".parse::<IpNet>().expect("ipv4 cidr"),
        ]
    );
}

#[test]
fn rejects_invalid_server_listen_address() {
    let error = parse_server_config(
        r#"
        [server]
        listen = "oops"
        public_base_url = "http://127.0.0.1:8080"
        "#,
    )
    .expect_err("invalid config should fail");

    assert!(error.to_string().contains("server.listen"));
}

#[test]
fn rejects_invalid_trusted_proxy_cidr() {
    let error = parse_server_config(
        r#"
        [server]
        listen = "127.0.0.1:8080"
        public_base_url = "https://monitor.example.com"
        trusted_proxies = ["not-a-cidr"]
        "#,
    )
    .expect_err("invalid trusted proxy cidr should fail");

    assert!(error.to_string().contains("server.trusted_proxies"));
}

#[test]
fn rejects_invalid_agent_server_scheme() {
    let error = parse_agent_config(
        r#"
        [agent]
        node_id = "hk-01"
        node_label = "Hong Kong 01"
        server = "http://127.0.0.1:8080/ws"
        token = "token"
        "#,
    )
    .expect_err("invalid agent config should fail");

    assert!(error.to_string().contains("agent.server"));
}

#[test]
fn parses_agent_config() {
    let config = parse_agent_config(
        r#"
        [agent]
        node_id = "hk-01"
        node_label = "Hong Kong 01"
        server = "ws://127.0.0.1:8080/ws"
        token = "token"
        report_interval_secs = 7
        hostname_override = "hk-01.internal"
        tags = [" edge ", "apac"]
        "#,
    )
    .expect("agent config should parse");

    assert_eq!(config.node_id, "hk-01");
    assert_eq!(config.report_interval_secs, 7);
    assert_eq!(config.tags, vec!["apac", "edge"]);
}

#[test]
fn rejects_agent_config_with_too_many_tags() {
    let tags = (0..1000)
        .map(|index| format!("\"tag-{index}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let input = format!(
        r#"
        [agent]
        node_id = "hk-01"
        node_label = "Hong Kong 01"
        server = "ws://127.0.0.1:8080/ws"
        token = "token"
        tags = [{tags}]
        "#
    );

    let error = parse_agent_config(&input).expect_err("too many tags should fail");
    assert!(error.to_string().contains("agent.tags"));
}

#[test]
fn rejects_agent_config_with_oversized_tag() {
    let oversized = "x".repeat(MAX_NODE_TAG_BYTES + 1);
    let input = format!(
        r#"
        [agent]
        node_id = "hk-01"
        node_label = "Hong Kong 01"
        server = "ws://127.0.0.1:8080/ws"
        token = "token"
        tags = ["{oversized}"]
        "#
    );

    let error = parse_agent_config(&input).expect_err("oversized tag should fail");
    assert!(error.to_string().contains("agent.tags[0]"));
}

#[test]
fn parses_server_config_with_install() {
    let config = parse_server_config(
        r#"
        [server]
        listen = "127.0.0.1:8080"
        public_base_url = "https://monitor.example.com"
        node_registry_path = "/etc/nodelite/server.json"

        [auth]
        username = "viewer"
        password = "secret"

        [install]
        agent_release_base_url = "https://downloads.example.com/nodelite/releases/latest/download"
        agent_release_sha256_x86_64 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        agent_release_sha256_aarch64 = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
        "#,
    )
    .expect("server config should parse");

    assert_eq!(
        config
            .readonly_auth
            .as_ref()
            .map(|auth| auth.username.as_str()),
        Some("viewer")
    );
    assert_eq!(
        config.node_registry_path,
        PathBuf::from("/etc/nodelite/server.json")
    );
    assert_eq!(
        config.agent_release_base_url.as_deref(),
        Some("https://downloads.example.com/nodelite/releases/latest/download")
    );
    assert_eq!(
        config.agent_release_sha256_x86_64.as_deref(),
        Some("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
    );
    assert_eq!(
        config.agent_release_sha256_aarch64.as_deref(),
        Some("abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789")
    );
}

#[test]
fn parses_server_config_with_audit_overrides() {
    let config = parse_server_config(
        r#"
        [server]
        listen = "127.0.0.1:8080"
        public_base_url = "https://monitor.example.com"

        [audit]
        enabled = true
        db_path = "/var/lib/nodelite/audit.sqlite3"
        retention_days = 30
        log_successful_auth = false
        log_failed_auth = true
        log_token_events = true
        log_rate_limit = false
        "#,
    )
    .expect("audit config should parse");

    assert!(config.audit.enabled);
    assert_eq!(
        config.audit.db_path,
        PathBuf::from("/var/lib/nodelite/audit.sqlite3")
    );
    assert_eq!(config.audit.retention_days, 30);
    assert!(!config.audit.log_successful_auth);
    assert!(config.audit.log_failed_auth);
    assert!(config.audit.log_token_events);
    assert!(!config.audit.log_rate_limit);
}

#[test]
fn parses_server_config_with_alerting() {
    let config = parse_server_config(
        r#"
        [server]
        listen = "127.0.0.1:8080"
        public_base_url = "https://monitor.example.com"

        [alerts]
        enabled = true

        [alerts.smtp]
        enabled = true
        host = "smtp.example.com"
        port = 465
        username = "ops"
        password = "smtp-secret"
        sender = "nodelite@example.com"
        recipients = ["ops@example.com", "sre@example.com"]
        transport = "tls"
        send_resolved = false

        [alerts.webhook]
        enabled = true
        url = "https://hooks.example.com/nodelite"
        secret = "hook-secret"
        send_resolved = false

        [alerts.inspection]
        enabled = true
        local_time = "08:30"
        lookback_hours = 48
        delivery = ["smtp", "webhook"]
        offline_grace_minutes = 30
        latency_warn_ms = 420
        cpu_warn_percent = 90
        memory_warn_percent = 95

        [[alerts.rules]]
        id = "cpu-hot"
        name = "CPU 持续过高"
        enabled = true
        metric = "cpu_usage_percent"
        comparator = "gt"
        threshold = 85
        window_minutes = 10
        severity = "critical"
        scope_mode = "tags"
        tags = ["edge", "prod"]
        delivery = ["smtp"]
        cooldown_minutes = 45
        send_resolved = true
        "#,
    )
    .expect("alerting config should parse");

    assert!(config.alerting.enabled);
    assert!(config.alerting.smtp.enabled);
    assert_eq!(config.alerting.smtp.transport, AlertSmtpTransport::Tls);
    assert!(!config.alerting.smtp.send_resolved);
    assert_eq!(
        config.alerting.smtp.recipients,
        vec!["ops@example.com", "sre@example.com"]
    );
    assert!(config.alerting.webhook.enabled);
    assert_eq!(
        config.alerting.webhook.url,
        "https://hooks.example.com/nodelite"
    );
    assert_eq!(config.alerting.rules.len(), 1);
    assert_eq!(
        config.alerting.rules[0].metric,
        AlertMetric::CpuUsagePercent
    );
    assert_eq!(config.alerting.rules[0].comparator, AlertComparator::Gt);
    assert_eq!(config.alerting.rules[0].severity, AlertSeverity::Critical);
    assert_eq!(config.alerting.rules[0].scope_mode, AlertScopeMode::Tags);
    assert_eq!(config.alerting.rules[0].delivery, vec![AlertChannel::Smtp]);
}

#[test]
fn rejects_alert_rule_with_missing_scope_values() {
    let error = parse_server_config(
        r#"
        [server]
        listen = "127.0.0.1:8080"
        public_base_url = "https://monitor.example.com"

        [[alerts.rules]]
        id = "latency-hot"
        name = "Latency"
        metric = "latency_ms"
        comparator = "gt"
        threshold = 200
        severity = "warning"
        scope_mode = "node_ids"
        "#,
    )
    .expect_err("missing node ids should fail");

    assert!(error.to_string().contains("scope_mode = node_ids"));
}

#[test]
fn rejects_alert_inspection_with_bad_time() {
    let error = parse_server_config(
        r#"
        [server]
        listen = "127.0.0.1:8080"
        public_base_url = "https://monitor.example.com"

        [alerts.inspection]
        enabled = true
        local_time = "24:61"
        "#,
    )
    .expect_err("invalid inspection time should fail");

    assert!(error.to_string().contains("HH:MM"));
}

#[test]
fn rejects_zero_audit_retention_days() {
    let error = parse_server_config(
        r#"
        [server]
        listen = "127.0.0.1:8080"
        public_base_url = "https://monitor.example.com"

        [audit]
        retention_days = 0
        "#,
    )
    .expect_err("zero retention should fail");

    assert!(error.to_string().contains("audit.retention_days"));
}

#[test]
fn parses_server_config_with_totp_2fa() {
    let config = parse_server_config(
        r#"
        [server]
        listen = "127.0.0.1:8080"
        public_base_url = "https://monitor.example.com"

        [auth]
        username = "viewer"
        password = "secret123"
        enable_2fa = true
        totp_secret = "JBSWY3DPEHPK3PXP"
        "#,
    )
    .expect("2fa config should parse");

    let auth = config.readonly_auth.expect("auth should be configured");
    assert!(auth.enable_2fa);
    assert_eq!(auth.totp_secret.as_deref(), Some("JBSWY3DPEHPK3PXP"));
}

#[test]
fn parses_server_config_with_otpauth_totp_secret() {
    let config = parse_server_config(
        r#"
        [server]
        listen = "127.0.0.1:8080"
        public_base_url = "https://monitor.example.com"

        [auth]
        username = "viewer"
        password = "secret123"
        enable_2fa = true
        totp_secret = "otpauth://totp/NodeLite:viewer%40example.com?secret=jbsw y3dp-ehpk3pxp&issuer=NodeLite"
        "#,
    )
    .expect("otpauth uri should parse");

    let auth = config.readonly_auth.expect("auth should be configured");
    assert_eq!(auth.totp_secret.as_deref(), Some("JBSWY3DPEHPK3PXP"));
}

#[test]
fn parses_server_config_with_secret_query_totp_secret() {
    let config = parse_server_config(
        r#"
        [server]
        listen = "127.0.0.1:8080"
        public_base_url = "https://monitor.example.com"

        [auth]
        username = "viewer"
        password = "secret123"
        enable_2fa = true
        totp_secret = "secret=jbswy3dp ehpk3pxp&issuer=NodeLite"
        "#,
    )
    .expect("secret query string should parse");

    let auth = config.readonly_auth.expect("auth should be configured");
    assert_eq!(auth.totp_secret.as_deref(), Some("JBSWY3DPEHPK3PXP"));
}

#[test]
fn ignores_empty_totp_secret_when_2fa_is_disabled() {
    let config = parse_server_config(
        r#"
        [server]
        listen = "127.0.0.1:8080"
        public_base_url = "https://monitor.example.com"

        [auth]
        username = "viewer"
        password = "secret123"
        enable_2fa = false
        totp_secret = ""
        "#,
    )
    .expect("disabled 2fa should ignore empty totp secret");

    let auth = config.readonly_auth.expect("auth should be configured");
    assert!(!auth.enable_2fa);
    assert_eq!(auth.totp_secret, None);
}

#[test]
fn ignores_invalid_totp_secret_when_2fa_is_disabled() {
    let config = parse_server_config(
        r#"
        [server]
        listen = "127.0.0.1:8080"
        public_base_url = "https://monitor.example.com"

        [auth]
        username = "viewer"
        password = "secret123"
        enable_2fa = false
        totp_secret = "not-a-base32-secret"
        "#,
    )
    .expect("disabled 2fa should ignore invalid totp secret");

    let auth = config.readonly_auth.expect("auth should be configured");
    assert!(!auth.enable_2fa);
    assert_eq!(auth.totp_secret.as_deref(), Some("NOTABASE32SECRET"));
}

#[test]
fn rejects_2fa_without_totp_secret() {
    let error = parse_server_config(
        r#"
        [server]
        listen = "127.0.0.1:8080"
        public_base_url = "https://monitor.example.com"

        [auth]
        username = "viewer"
        password = "secret123"
        enable_2fa = true
        "#,
    )
    .expect_err("2fa without totp secret should fail");

    assert!(error.to_string().contains("auth.totp_secret"));
}

#[test]
fn rejects_2fa_with_plaintext_public_base_url() {
    let error = parse_server_config(
        r#"
        [server]
        listen = "127.0.0.1:8080"
        public_base_url = "http://monitor.example.com"
        insecure_allow_http = true

        [auth]
        username = "viewer"
        password = "secret123"
        enable_2fa = true
        totp_secret = "JBSWY3DPEHPK3PXP"
        "#,
    )
    .expect_err("2fa over plaintext http should be rejected");

    assert!(error.to_string().contains("public_base_url"));
    assert!(error.to_string().contains("https"));
}

#[test]
fn rejects_public_listener_without_auth() {
    let error = parse_server_config(
        r#"
        [server]
        listen = "0.0.0.0:8080"
        public_base_url = "https://monitor.example.com"
        "#,
    )
    .expect_err("public listener without auth should fail");

    assert!(error.to_string().contains("auth.username"));
}

#[test]
fn rejects_install_release_base_without_checksums() {
    let error = parse_server_config(
        r#"
        [server]
        listen = "127.0.0.1:8080"
        public_base_url = "https://monitor.example.com"

        [install]
        agent_release_base_url = "https://downloads.example.com/nodelite/releases/latest/download"
        "#,
    )
    .expect_err("release base without checksums should fail");

    assert!(error.to_string().contains("agent_release_sha256_x86_64"));
}

#[test]
fn rejects_invalid_ws_limits() {
    let error = parse_server_config(
        r#"
        [server]
        listen = "127.0.0.1:8080"
        public_base_url = "http://127.0.0.1:8080"

        [ws]
        max_total_connections = 4
        max_connections_per_ip = 8
        "#,
    )
    .expect_err("invalid ws limits should fail");

    assert!(error.to_string().contains("ws.max_connections_per_ip"));
}

#[test]
fn rejects_remote_http_without_explicit_opt_in() {
    let error = parse_server_config(
        r#"
        [server]
        listen = "0.0.0.0:8080"
        public_base_url = "http://monitor.example.com"

        [auth]
        username = "viewer"
        password = "secret"
        "#,
    )
    .expect_err("remote http without opt-in should fail");

    assert!(error.to_string().contains("server.insecure_allow_http"));
}

#[test]
fn allows_remote_http_with_explicit_opt_in() {
    let config = parse_server_config(
        r#"
        [server]
        listen = "0.0.0.0:8080"
        public_base_url = "http://monitor.example.com"
        insecure_allow_http = true

        [auth]
        username = "viewer"
        password = "secret"
        "#,
    )
    .expect("remote http should parse with explicit opt-in");

    assert!(config.insecure_allow_http);
}
