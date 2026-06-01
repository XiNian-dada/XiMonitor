use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};
use anyhow::Result;
use nodelite_agent::config_io::{load_agent_config, update_token_in_config};

#[tokio::test]
async fn test_agent_config_load_and_token_update() -> Result<()> {
    // Generate a unique temporary path for the configuration file
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_dir = std::env::temp_dir().join(format!("nodelite-agent-test-{timestamp}"));
    fs::create_dir_all(&temp_dir)?;
    let config_path = temp_dir.join("agent.toml");

    let initial_toml = r#"[agent]
node_id = "test-node-01"
node_label = "Test Node 01"
server = "ws://127.0.0.1:8080/ws"
token = "initial-secret-token"
report_interval_secs = 5
"#;

    fs::write(&config_path, initial_toml)?;

    // 1. Load config and verify fields
    let config = load_agent_config(&config_path).await?;
    assert_eq!(config.node_id, "test-node-01");
    assert_eq!(config.node_label, "Test Node 01");
    assert_eq!(config.server, "ws://127.0.0.1:8080/ws");
    assert_eq!(config.token, "initial-secret-token");
    assert_eq!(config.report_interval_secs, 5);

    // 2. Update token in the config file
    update_token_in_config(&config_path, "refreshed-secret-token").await?;

    // 3. Reload config and verify token was updated
    let updated_config = load_agent_config(&config_path).await?;
    assert_eq!(updated_config.token, "refreshed-secret-token");
    assert_eq!(updated_config.node_id, "test-node-01"); // Other fields remain unchanged

    // Clean up temp directory
    let _ = fs::remove_dir_all(&temp_dir);
    Ok(())
}
