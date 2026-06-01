use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use getrandom::fill as fill_random;
use nodelite_proto::{AgentConfig, parse_agent_config};
use tokio::fs;
use toml_edit::{DocumentMut, Item, Value};

/// 从磁盘读取并解析 Agent 配置文件。
pub async fn load_agent_config(path: &Path) -> Result<AgentConfig> {
    let content = fs::read_to_string(path)
        .await
        .with_context(|| format!("failed to read config file {}", path.display()))?;
    parse_agent_config(&content)
        .map_err(|error| anyhow::anyhow!("failed to parse {}: {error}", path.display()))
}

/// 更新配置文件中的 token。
///
/// 在 `spawn_blocking` 中以"读 → 改 → 写 → fsync → rename → fsync 父目录"
/// 的方式持久化新 token,等同于 server registry 的写入级别。
pub async fn update_token_in_config(config_path: &Path, new_token: &str) -> Result<()> {
    let config_path = config_path.to_path_buf();
    let new_token = new_token.to_string();
    tokio::task::spawn_blocking(move || persist_token_sync(&config_path, &new_token))
        .await
        .context("token persistence task failed")?
}

fn persist_token_sync(config_path: &Path, new_token: &str) -> Result<()> {
    let content = std::fs::read_to_string(config_path)
        .with_context(|| format!("failed to read {}", config_path.display()))?;
    let updated = replace_token_line(&content, new_token)?;

    let tmp_path = temporary_config_path(config_path);
    write_config_payload(&tmp_path, &updated)
        .with_context(|| format!("failed to write {}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, config_path)
        .with_context(|| format!("failed to replace {}", config_path.display()))?;
    sync_parent_dir(config_path);
    harden_config_permissions(config_path)
        .with_context(|| format!("failed to chmod {}", config_path.display()))?;
    Ok(())
}

fn replace_token_line(content: &str, new_token: &str) -> Result<String> {
    let mut document = content
        .parse::<DocumentMut>()
        .map_err(|error| anyhow::anyhow!("failed to parse agent config as TOML: {error}"))?;
    let agent = document
        .get_mut("agent")
        .and_then(Item::as_table_mut)
        .ok_or_else(|| anyhow::anyhow!("agent section not found in config file"))?;

    if let Some(item) = agent.get_mut("token") {
        let Some(existing_value) = item.as_value_mut() else {
            anyhow::bail!("agent.token is not a value");
        };
        let decor = existing_value.decor().clone();
        *existing_value = Value::from(new_token);
        *existing_value.decor_mut() = decor;
    } else {
        anyhow::bail!("token field not found in config file");
    }

    Ok(document.to_string())
}

fn temporary_config_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("agent.toml");
    let mut suffix = [0u8; 8];
    if fill_random(&mut suffix).is_err() {
        return path.with_file_name(format!("{file_name}.tmp"));
    }
    let suffix_hex: String = suffix.iter().map(|byte| format!("{byte:02x}")).collect();
    path.with_file_name(format!("{file_name}.tmp.{suffix_hex}"))
}

fn write_config_payload(path: &Path, payload: &str) -> Result<()> {
    use std::fs::OpenOptions;
    use std::io::Write;

    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    file.write_all(payload.as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to fsync {}", path.display()))?;
    Ok(())
}

fn sync_parent_dir(path: &Path) {
    let Some(parent) = path.parent() else {
        return;
    };
    if parent.as_os_str().is_empty() {
        return;
    }
    let _ = std::fs::File::open(parent).and_then(|dir| dir.sync_all());
}

fn harden_config_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use toml_edit::DocumentMut;

    use nodelite_proto::parse_agent_config;

    use super::replace_token_line;

    #[test]
    fn replace_token_line_preserves_comments_and_indent() {
        let input = "[agent]\nnode_id = \"hk-01\"\nnode_label = \"Hong Kong 01\"\nserver = \"ws://127.0.0.1:8080/ws\"\n# token = \"old\"\n token = \"old\" # keep this\n";
        let result = replace_token_line(input, "newvalue").expect("should replace");
        assert!(result.contains("# token = \"old\""));
        assert!(result.contains(" token = \"newvalue\" # keep this"));
        assert_eq!(
            result.matches("token = \"old\"").count(),
            1,
            "only the comment line keeps the old value"
        );
        let parsed = parse_agent_config(&result).expect("updated config should stay valid");
        assert_eq!(parsed.token, "newvalue");
    }

    #[test]
    fn replace_token_line_preserves_multiline_neighbors() {
        let input = r#"[agent]
node_id = "hk-01"
node_label = "Hong Kong 01"
server = "ws://127.0.0.1:8080/ws"
token = "old"

[notes]
description = """
line1
line2
"""
"#;
        let result = replace_token_line(input, "newvalue").expect("should replace token");
        assert!(result.contains("description = \"\"\"\nline1\nline2\n\"\"\""));
        assert!(result.contains("token = \"newvalue\""));
        result
            .parse::<DocumentMut>()
            .expect("updated config should stay valid TOML");
    }

    #[test]
    fn replace_token_line_escapes_special_chars() {
        let result = replace_token_line(
            "[agent]\nnode_id = \"hk-01\"\nnode_label = \"Hong Kong 01\"\nserver = \"ws://127.0.0.1:8080/ws\"\ntoken = \"x\"\n",
            "with\"quote\\and-backslash",
        )
        .expect("ok");
        let parsed = parse_agent_config(&result).expect("updated config should stay valid");
        assert_eq!(parsed.token, "with\"quote\\and-backslash");
    }

    #[test]
    fn replace_token_line_fails_when_no_token_field() {
        let result = replace_token_line("[agent]\nnode_id = \"x\"\n", "new");
        assert!(result.is_err());
    }
}
