use anyhow::{Context, Result, anyhow, bail};
use nodelite_proto::{AlertingConfig, ReadonlyAuthConfig, parse_server_config};
use serde::Serialize;
use tokio::fs;
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, Value, value};

pub(super) async fn persist_auth_password_change(
    path: &std::path::Path,
    password: &str,
) -> Result<()> {
    let content = fs::read_to_string(path)
        .await
        .with_context(|| format!("failed to read server config from {}", path.display()))?;
    let updated = update_auth_password(&content, password)?;
    validate_server_config(&updated)?;
    persist_updated_content(path, updated).await
}

pub(super) async fn persist_auth_2fa_change(
    path: &std::path::Path,
    auth: &ReadonlyAuthConfig,
) -> Result<()> {
    let content = fs::read_to_string(path)
        .await
        .with_context(|| format!("failed to read server config from {}", path.display()))?;
    let updated = update_auth_2fa(&content, auth.enable_2fa, auth.totp_secret.as_deref())?;
    validate_server_config(&updated)?;
    persist_updated_content(path, updated).await
}

pub(super) async fn persist_alerting_change(
    path: &std::path::Path,
    alerting: &AlertingConfig,
) -> Result<()> {
    let content = fs::read_to_string(path)
        .await
        .with_context(|| format!("failed to read server config from {}", path.display()))?;
    let updated = update_alerting_settings(&content, alerting)?;
    validate_server_config(&updated)?;
    persist_updated_content(path, updated).await
}

fn update_auth_password(content: &str, password: &str) -> Result<String> {
    let mut document = parse_document(content)?;
    let auth = auth_table_mut(&mut document)?;
    set_value(auth, "password", Value::from(password))?;
    Ok(document.to_string())
}

fn update_auth_2fa(content: &str, enable_2fa: bool, totp_secret: Option<&str>) -> Result<String> {
    if enable_2fa && totp_secret.is_none() {
        bail!("totp_secret is required when enabling 2FA");
    }

    let mut document = parse_document(content)?;
    let auth = auth_table_mut(&mut document)?;
    set_value(auth, "enable_2fa", Value::from(enable_2fa))?;
    match totp_secret {
        Some(secret) => set_value(auth, "totp_secret", Value::from(secret))?,
        None => {
            auth.remove("totp_secret");
        }
    }
    Ok(document.to_string())
}

fn update_alerting_settings(content: &str, alerting: &AlertingConfig) -> Result<String> {
    let mut document = parse_document(content)?;
    upsert_alerts_item(document.as_table_mut(), build_alerts_item(alerting)?);
    Ok(document.to_string())
}

fn parse_document(content: &str) -> Result<DocumentMut> {
    content
        .parse::<DocumentMut>()
        .map_err(|error| anyhow!("failed to parse server.toml as TOML document: {error}"))
}

fn auth_table_mut(document: &mut DocumentMut) -> Result<&mut Table> {
    document
        .get_mut("auth")
        .and_then(Item::as_table_mut)
        .ok_or_else(|| anyhow!("server.toml does not contain an [auth] section"))
}

fn set_value(table: &mut Table, key: &str, new_value: Value) -> Result<()> {
    if let Some(item) = table.get_mut(key) {
        let Some(existing_value) = item.as_value_mut() else {
            bail!("auth.{key} is not a value");
        };
        let decor = existing_value.decor().clone();
        *existing_value = new_value;
        *existing_value.decor_mut() = decor;
    } else {
        table.insert(key, value(new_value));
    }
    Ok(())
}

fn validate_server_config(content: &str) -> Result<()> {
    parse_server_config(content)
        .map_err(|error| anyhow!("updated server config would be invalid: {error}"))?;
    Ok(())
}

fn build_alerts_item(alerting: &AlertingConfig) -> Result<Item> {
    let fragment = toml::to_string(&AlertingDocument { alerts: alerting })
        .map_err(|error| anyhow!("failed to serialize alerts section: {error}"))?;
    let mut fragment = parse_document(&fragment)?;
    fragment
        .remove("alerts")
        .ok_or_else(|| anyhow!("serialized alerting config did not produce an [alerts] section"))
}

// Preserve the existing TOML structure when possible so user-authored comments
// survive settings writes instead of being replaced with a fresh serialized tree.
fn upsert_alerts_item(root: &mut Table, new_alerts: Item) {
    if let Some(existing_alerts) = root.get_mut("alerts") {
        merge_item(existing_alerts, new_alerts);
    } else {
        root.insert("alerts", new_alerts);
    }
}

fn merge_item(existing: &mut Item, replacement: Item) {
    match replacement {
        Item::None => *existing = Item::None,
        Item::Value(replacement_value) => merge_value(existing, replacement_value),
        Item::Table(replacement_table) => merge_table_item(existing, replacement_table),
        Item::ArrayOfTables(replacement_tables) => {
            merge_array_of_tables_item(existing, replacement_tables);
        }
    }
}

fn merge_value(existing: &mut Item, replacement: Value) {
    let Some(existing_value) = existing.as_value_mut() else {
        *existing = Item::Value(replacement);
        return;
    };
    let decor = existing_value.decor().clone();
    *existing_value = replacement;
    *existing_value.decor_mut() = decor;
}

fn merge_table_item(existing: &mut Item, replacement: Table) {
    let Some(existing_table) = existing.as_table_mut() else {
        *existing = Item::Table(replacement);
        return;
    };
    merge_table(existing_table, replacement);
}

fn merge_table(existing: &mut Table, replacement: Table) {
    let mut stale_keys = existing
        .iter()
        .map(|(key, _)| key.to_string())
        .collect::<Vec<_>>();

    for (key, replacement_item) in replacement {
        let key = key.to_string();
        if let Some(existing_item) = existing.get_mut(&key) {
            merge_item(existing_item, replacement_item);
        } else {
            existing.insert(&key, replacement_item);
        }
        stale_keys.retain(|stale_key| stale_key != &key);
    }

    for key in stale_keys {
        existing.remove(&key);
    }
}

fn merge_array_of_tables_item(existing: &mut Item, replacement: ArrayOfTables) {
    let Some(existing_tables) = existing.as_array_of_tables_mut() else {
        *existing = Item::ArrayOfTables(replacement);
        return;
    };
    merge_array_of_tables(existing_tables, replacement);
}

fn merge_array_of_tables(existing: &mut ArrayOfTables, replacement: ArrayOfTables) {
    if all_tables_have_id(existing) && all_tables_have_id(&replacement) {
        merge_array_of_tables_by_id(existing, replacement);
        return;
    }

    let replacement_len = replacement.len();
    for (index, replacement_table) in replacement.into_iter().enumerate() {
        if let Some(existing_table) = existing.get_mut(index) {
            merge_table(existing_table, replacement_table);
        } else {
            existing.push(replacement_table);
        }
    }
    while existing.len() > replacement_len {
        existing.remove(existing.len() - 1);
    }
}

fn merge_array_of_tables_by_id(existing: &mut ArrayOfTables, replacement: ArrayOfTables) {
    let mut merged = ArrayOfTables::new();
    for replacement_table in replacement {
        let id = table_id(&replacement_table).map(ToOwned::to_owned);
        if let Some(index) = id
            .as_deref()
            .and_then(|id| find_table_index_by_id(existing, id))
        {
            if let Some(mut existing_table) = existing.get(index).cloned() {
                existing.remove(index);
                merge_table(&mut existing_table, replacement_table);
                merged.push(existing_table);
            } else {
                merged.push(replacement_table);
            }
        } else {
            merged.push(replacement_table);
        }
    }
    while !existing.is_empty() {
        existing.remove(existing.len() - 1);
    }
    for table in merged {
        existing.push(table);
    }
}

fn all_tables_have_id(tables: &ArrayOfTables) -> bool {
    tables.iter().all(|table| table_id(table).is_some())
}

fn find_table_index_by_id(tables: &ArrayOfTables, id: &str) -> Option<usize> {
    tables
        .iter()
        .enumerate()
        .find_map(|(index, table)| (table_id(table) == Some(id)).then_some(index))
}

fn table_id(table: &Table) -> Option<&str> {
    table.get("id")?.as_value()?.as_str()
}

async fn persist_updated_content(path: &std::path::Path, updated: String) -> Result<()> {
    let metadata = fs::metadata(path).await.ok();
    let temp_path = path.with_extension("toml.tmp");
    fs::write(&temp_path, updated).await.with_context(|| {
        format!(
            "failed to write temporary server config to {}",
            temp_path.display()
        )
    })?;
    if let Some(metadata) = metadata {
        fs::set_permissions(&temp_path, metadata.permissions())
            .await
            .with_context(|| {
                format!(
                    "failed to copy server config permissions onto {}",
                    temp_path.display()
                )
            })?;
    }
    fs::rename(&temp_path, path).await.with_context(|| {
        format!(
            "failed to replace server config {} with {}",
            path.display(),
            temp_path.display()
        )
    })?;
    Ok(())
}

#[derive(Serialize)]
struct AlertingDocument<'a> {
    alerts: &'a AlertingConfig,
}

#[cfg(test)]
mod tests {
    use super::{update_alerting_settings, update_auth_2fa, update_auth_password};
    use nodelite_proto::{
        AlertChannel, AlertComparator, AlertMetric, AlertRuleConfig, AlertScopeMode, AlertSeverity,
        AlertSmtpConfig, AlertSmtpTransport, AlertWebhookConfig, AlertingConfig, InspectionConfig,
    };

    #[test]
    fn update_auth_password_preserves_trailing_comment_and_multiline_neighbors() {
        let input = r#"[server]
listen = "127.0.0.1:8080"
public_base_url = "https://monitor.example.com"

[auth]
username = "viewer"
password = "old-pass" # keep this comment

[ui]
welcome = """
hello
world
"""
"#;

        let updated = update_auth_password(input, "new-pass")
            .expect("password change should preserve neighboring TOML");

        assert!(updated.contains(r#"password = "new-pass" # keep this comment"#));
        assert!(updated.contains("welcome = \"\"\"\nhello\nworld\n\"\"\""));
    }

    #[test]
    fn update_auth_2fa_enables_and_preserves_auth_section() {
        let input = r#"[server]
listen = "127.0.0.1:8080"
public_base_url = "https://monitor.example.com"

[auth]
username = "viewer"
password = "old-pass"

[ui]
refresh_interval_secs = 5
"#;

        let updated = update_auth_2fa(input, true, Some("JBSWY3DPEHPK3PXP"))
            .expect("2FA enable should update auth section");

        assert!(updated.contains("username = \"viewer\""));
        assert!(updated.contains("password = \"old-pass\""));
        assert!(updated.contains("enable_2fa = true"));
        assert!(updated.contains("totp_secret = \"JBSWY3DPEHPK3PXP\""));
        assert!(updated.contains("[ui]"));
    }

    #[test]
    fn update_auth_2fa_disables_and_removes_stale_secret() {
        let input = r#"[auth]
username = "viewer"
password = "old-pass"
enable_2fa = true
totp_secret = "JBSWY3DPEHPK3PXP" # stale
"#;

        let updated =
            update_auth_2fa(input, false, None).expect("2FA disable should update auth section");

        assert!(updated.contains("enable_2fa = false"));
        assert!(!updated.contains("totp_secret"));
    }

    #[test]
    fn update_auth_password_rejects_missing_auth_section() {
        let input = r#"[server]
listen = "127.0.0.1:8080"
"#;

        let error = update_auth_password(input, "new-pass").expect_err("missing auth should fail");
        assert!(error.to_string().contains("[auth] section"));
    }

    #[test]
    fn update_alerting_settings_replaces_alerts_section_and_preserves_other_sections() {
        let input = r#"[server]
listen = "127.0.0.1:8080"
public_base_url = "https://monitor.example.com"

[alerts]
enabled = false

[auth]
username = "viewer"
password = "old-pass"
"#;

        let alerting = AlertingConfig {
            enabled: true,
            smtp: AlertSmtpConfig {
                enabled: true,
                host: "smtp.example.com".to_string(),
                port: 587,
                username: "ops".to_string(),
                password: Some("smtp-secret".to_string()),
                sender: "nodelite@example.com".to_string(),
                recipients: vec!["ops@example.com".to_string()],
                transport: AlertSmtpTransport::StartTls,
                send_resolved: true,
            },
            webhook: AlertWebhookConfig {
                enabled: true,
                url: "https://hooks.example.com/nodelite".to_string(),
                secret: Some("hook-secret".to_string()),
                send_resolved: true,
            },
            rules: vec![AlertRuleConfig {
                id: "cpu-hot".to_string(),
                name: "CPU".to_string(),
                enabled: true,
                metric: AlertMetric::CpuUsagePercent,
                comparator: AlertComparator::Gt,
                threshold: 85,
                window_minutes: 5,
                severity: AlertSeverity::Critical,
                scope_mode: AlertScopeMode::All,
                node_ids: Vec::new(),
                tags: Vec::new(),
                delivery: vec![AlertChannel::Smtp],
                cooldown_minutes: 30,
                send_resolved: true,
            }],
            inspection: InspectionConfig::default(),
        };

        let updated = update_alerting_settings(input, &alerting)
            .expect("alert settings update should succeed");

        assert!(updated.contains("[alerts]"));
        assert!(updated.contains("host = \"smtp.example.com\""));
        assert!(updated.contains("[[alerts.rules]]"));
        assert!(updated.contains("metric = \"cpu_usage_percent\""));
        assert!(updated.contains("[auth]"));
        assert!(updated.contains("username = \"viewer\""));
    }

    #[test]
    fn update_alerting_settings_creates_alerts_section_when_missing() {
        let input = r#"[server]
listen = "127.0.0.1:8080"
public_base_url = "https://monitor.example.com"

[auth]
username = "viewer"
password = "old-pass"
"#;

        let updated = update_alerting_settings(input, &sample_alerting_config())
            .expect("missing alerts section should be created");

        assert!(updated.contains("[alerts]"));
        assert!(updated.contains("enabled = true"));
        assert!(updated.contains("[alerts.smtp]"));
    }

    #[test]
    fn update_alerting_settings_preserves_existing_alert_comments() {
        let input = r#"[server]
listen = "127.0.0.1:8080"
public_base_url = "https://monitor.example.com"

[alerts]
enabled = false # keep enabled comment

[alerts.smtp]
enabled = false
host = "old.example.com" # keep host comment

[auth]
username = "viewer"
password = "old-pass"
"#;

        let updated = update_alerting_settings(input, &sample_alerting_config())
            .expect("alert settings update should preserve existing comments");

        assert!(updated.contains(r#"enabled = true # keep enabled comment"#));
        assert!(updated.contains(r#"host = "smtp.example.com" # keep host comment"#));
    }

    #[test]
    fn update_alerting_settings_preserves_rule_comments_by_id() {
        let input = r#"[server]
listen = "127.0.0.1:8080"
public_base_url = "https://monitor.example.com"

[auth]
username = "viewer"
password = "old-pass"

[alerts]
enabled = true

[[alerts.rules]]
id = "cpu-hot" # cpu id comment
name = "Old CPU"
enabled = true
metric = "cpu_usage_percent"
comparator = "gt"
threshold = 80
window_minutes = 5
severity = "warning"
scope_mode = "all"
node_ids = []
tags = []
delivery = ["smtp"]
cooldown_minutes = 30
send_resolved = true

[[alerts.rules]]
id = "memory-hot" # memory id comment
name = "Old Memory"
enabled = true
metric = "memory_usage_percent"
comparator = "gt"
threshold = 80
window_minutes = 5
severity = "warning"
scope_mode = "all"
node_ids = []
tags = []
delivery = ["smtp"]
cooldown_minutes = 30
send_resolved = true
"#;
        let mut alerting = sample_alerting_config();
        alerting.rules = vec![
            AlertRuleConfig {
                id: "memory-hot".to_string(),
                name: "Memory".to_string(),
                metric: AlertMetric::MemoryUsagePercent,
                threshold: 90,
                ..sample_rule("memory-hot")
            },
            AlertRuleConfig {
                id: "cpu-hot".to_string(),
                name: "CPU".to_string(),
                metric: AlertMetric::CpuUsagePercent,
                threshold: 85,
                ..sample_rule("cpu-hot")
            },
        ];

        let updated = update_alerting_settings(input, &alerting)
            .expect("alert settings update should preserve rule comments by id");

        assert!(updated.contains(r#"id = "memory-hot" # memory id comment"#));
        assert!(updated.contains(r#"id = "cpu-hot" # cpu id comment"#));
        assert!(updated.contains(r#"name = "Memory""#));
        assert!(updated.contains(r#"name = "CPU""#));
    }

    fn sample_alerting_config() -> AlertingConfig {
        AlertingConfig {
            enabled: true,
            smtp: AlertSmtpConfig {
                enabled: true,
                host: "smtp.example.com".to_string(),
                port: 587,
                username: "ops".to_string(),
                password: Some("smtp-secret".to_string()),
                sender: "nodelite@example.com".to_string(),
                recipients: vec!["ops@example.com".to_string()],
                transport: AlertSmtpTransport::StartTls,
                send_resolved: true,
            },
            webhook: AlertWebhookConfig {
                enabled: true,
                url: "https://hooks.example.com/nodelite".to_string(),
                secret: Some("hook-secret".to_string()),
                send_resolved: true,
            },
            rules: vec![AlertRuleConfig {
                id: "cpu-hot".to_string(),
                name: "CPU".to_string(),
                enabled: true,
                metric: AlertMetric::CpuUsagePercent,
                comparator: AlertComparator::Gt,
                threshold: 85,
                window_minutes: 5,
                severity: AlertSeverity::Critical,
                scope_mode: AlertScopeMode::All,
                node_ids: Vec::new(),
                tags: Vec::new(),
                delivery: vec![AlertChannel::Smtp],
                cooldown_minutes: 30,
                send_resolved: true,
            }],
            inspection: InspectionConfig::default(),
        }
    }

    fn sample_rule(id: &str) -> AlertRuleConfig {
        AlertRuleConfig {
            id: id.to_string(),
            name: "Rule".to_string(),
            enabled: true,
            metric: AlertMetric::CpuUsagePercent,
            comparator: AlertComparator::Gt,
            threshold: 85,
            window_minutes: 5,
            severity: AlertSeverity::Critical,
            scope_mode: AlertScopeMode::All,
            node_ids: Vec::new(),
            tags: Vec::new(),
            delivery: vec![AlertChannel::Smtp],
            cooldown_minutes: 30,
            send_resolved: true,
        }
    }
}
