use std::fs;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use anyhow::Result;
use nodelite_agent::collector::new_collector;
use nodelite_agent::session::{AgentLogBuffer, run_forever};
use nodelite_proto::{AgentConfig, NodeIdentity};
use tokio::net::TcpListener;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_agent_reconnect_backoff_with_mock_time() -> Result<()> {
    // 1. Pause time so we can precisely control the clock
    tokio::time::pause();

    // 2. Set up local TcpListener to receive agent connection attempts
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let local_addr = listener.local_addr()?;

    // Create a temporary configuration file
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_dir = std::env::temp_dir().join(format!("nodelite-agent-reconnect-test-{timestamp}"));
    fs::create_dir_all(&temp_dir)?;
    let config_path = temp_dir.join("agent.toml");

    let config = AgentConfig {
        node_id: "reconnect-node-01".to_string(),
        node_label: "Reconnect Node 01".to_string(),
        server: format!("ws://{local_addr}/ws"),
        token: "reconnect-token".to_string(),
        connect_timeout_secs: 2,
        report_interval_secs: 5,
        max_incoming_message_bytes: 65536,
        insecure_transport_warn_interval_secs: 900,
        tags: vec![],
        hostname_override: None,
    };

    let collector = new_collector();
    let identity = NodeIdentity {
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

    let log_buffer = AgentLogBuffer::default();

    // 3. Spawn the run_forever loop in a separate task
    let config_clone = config.clone();
    let identity_clone = identity.clone();
    let config_path_clone = config_path.clone();
    let agent_task = tokio::spawn(async move {
        let _ = run_forever(
            config_clone,
            collector,
            identity_clone,
            config_path_clone,
            log_buffer,
        )
        .await;
    });

    // 4. Accept first connection
    let (stream1, _) = tokio::select! {
        res = listener.accept() => res?,
        _ = tokio::time::sleep(Duration::from_secs(5)) => {
            panic!("timed out waiting for first connection");
        }
    };
    // Close the connection immediately to trigger reconnect logic
    drop(stream1);

    // 5. Sleep briefly (in virtual time) to let the failure register
    // The backoff delay for the first retry (attempt 0) is 1s to 5s.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Advance virtual time by 6 seconds to ensure the backoff timer has fired
    tokio::time::advance(Duration::from_secs(6)).await;

    // 6. Verify that a second connection attempt is made
    let (stream2, _) = tokio::select! {
        res = listener.accept() => res?,
        _ = tokio::time::sleep(Duration::from_secs(5)) => {
            panic!("timed out waiting for second connection");
        }
    };
    drop(stream2);

    // Clean up
    agent_task.abort();
    let _ = fs::remove_dir_all(&temp_dir);

    Ok(())
}
