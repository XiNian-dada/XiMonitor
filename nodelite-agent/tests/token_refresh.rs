use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use anyhow::{Context, Result, anyhow};
use futures::{SinkExt, StreamExt};
use nodelite_agent::collector::new_collector;
use nodelite_agent::session::{AgentLogBuffer, run_session};
use nodelite_proto::{
    AgentConfig, HelloMessage, NoticeLevel, RefreshTokenResponseMessage, ServerNoticeMessage,
    WireMessage,
};
use tokio::net::TcpListener;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;

// A simple mock WebSocket server that completes the handshake and issues a token refresh
async fn run_mock_server(listener: TcpListener, new_token: String) -> Result<()> {
    let (stream, _) = listener.accept().await?;
    let mut ws_stream = accept_async(stream)
        .await
        .context("Failed to accept websocket connection")?;

    // 1. Await Hello message from the agent
    let first_msg = ws_stream
        .next()
        .await
        .ok_or_else(|| anyhow!("Connection closed before Hello"))??;

    let text = match first_msg {
        Message::Text(t) => t,
        _ => return Err(anyhow!("Expected text frame, got {:?}", first_msg)),
    };

    let hello_msg: WireMessage = serde_json::from_str(&text)?;
    let initial_token = match hello_msg {
        WireMessage::Hello(HelloMessage { token, .. }) => token,
        _ => return Err(anyhow!("Expected Hello message, got {:?}", hello_msg)),
    };
    assert_eq!(initial_token, "initial-token");

    // 2. Send notice that authentication succeeded
    let auth_notice = WireMessage::ServerNotice(ServerNoticeMessage {
        level: NoticeLevel::Info,
        message: "authenticated".to_string(),
    });
    ws_stream
        .send(Message::Text(serde_json::to_string(&auth_notice)?.into()))
        .await?;

    // 3. Send RefreshTokenResponse
    let refresh_response = WireMessage::RefreshTokenResponse(RefreshTokenResponseMessage {
        new_token,
        expires_at: "2026-06-02T00:00:00Z".to_string(),
    });
    ws_stream
        .send(Message::Text(serde_json::to_string(&refresh_response)?.into()))
        .await?;

    // 4. Close the websocket
    ws_stream.close(None).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_agent_token_refresh_lifecycle() -> Result<()> {
    // Start local TcpListener
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let local_addr = listener.local_addr()?;

    // Create a temporary configuration file
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_dir = std::env::temp_dir().join(format!("nodelite-agent-refresh-test-{timestamp}"));
    fs::create_dir_all(&temp_dir)?;
    let config_path = temp_dir.join("agent.toml");

    let initial_toml = format!(
        r#"[agent]
node_id = "test-node-refresh"
node_label = "Test Node Refresh"
server = "ws://{local_addr}/ws"
token = "initial-token"
report_interval_secs = 5
"#
    );
    fs::write(&config_path, &initial_toml)?;

    let mut config = AgentConfig {
        node_id: "test-node-refresh".to_string(),
        node_label: "Test Node Refresh".to_string(),
        server: format!("ws://{local_addr}/ws"),
        token: "initial-token".to_string(),
        connect_timeout_secs: 5,
        report_interval_secs: 5,
        max_incoming_message_bytes: 65536,
        insecure_transport_warn_interval_secs: 900,
        tags: vec![],
        hostname_override: None,
    };

    let expected_new_token = "refreshed-rotated-token-12345".to_string();

    // Spawn mock server in the background
    let server_token = expected_new_token.clone();
    let server_task = tokio::spawn(async move {
        let _ = run_mock_server(listener, server_token).await;
    });

    let mut collector = new_collector();
    let identity = nodelite_proto::NodeIdentity {
        node_id: config.node_id.clone(),
        node_label: config.node_label.clone(),
        hostname: "localhost".to_string(),
        os: "test".to_string(),
        kernel_version: None,
        cpu_model: None,
        cpu_cores: 1,
        agent_version: "0.1.0-test".to_string(),
        boot_time: None,
        tags: vec![],
    };

    let mut log_buffer = AgentLogBuffer::default();

    // Run a single agent session - it should connect, authenticate, get token, persist it, and exit when server closes socket.
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        run_session(&mut config, &mut collector, &identity, &config_path, &mut log_buffer),
    )
    .await;

    // Check that run_session completed and returned the expected connection closure error
    let session_res = result.context("Session timed out")?;
    assert!(session_res.is_err());
    let session_err = session_res.unwrap_err();
    assert!(session_err.established_session);

    // Verify token was updated in config file
    let updated_toml = fs::read_to_string(&config_path)?;
    assert!(updated_toml.contains("token = \"refreshed-rotated-token-12345\""));

    // Verify token was updated in memory config
    assert_eq!(config.token, "refreshed-rotated-token-12345");

    // Cleanup
    let _ = server_task.await;
    let _ = fs::remove_dir_all(&temp_dir);

    Ok(())
}
