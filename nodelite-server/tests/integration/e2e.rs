use std::fs;
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, anyhow};

use super::*;

/// RAII 临时目录:即便测试 panic,Drop 也会清理,避免临时目录泄漏。
/// 仅在运行 e2e 测试的平台上编译,避免在其它平台引入未使用项。
#[cfg(any(target_os = "linux", target_os = "macos"))]
struct TempDir {
    path: std::path::PathBuf,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl TempDir {
    fn new(prefix: &str) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("{prefix}-{}-{unique}", std::process::id()));
        std::fs::create_dir_all(&path).expect("create unique temp dir for e2e test");
        Self { path }
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_e2e_agent_server_handshake() -> Result<()> {
    // 1. Locate the nodelite-agent binary target
    let mut agent_bin = std::env::current_exe()?;
    agent_bin.pop(); // pop executable name
    if agent_bin.file_name().and_then(|s| s.to_str()) == Some("deps") {
        agent_bin.pop(); // pop "deps"
    }
    agent_bin.push("nodelite-agent");
    #[cfg(windows)]
    agent_bin.set_extension("exe");

    assert!(
        agent_bin.exists(),
        "nodelite-agent binary not found at {}",
        agent_bin.display()
    );

    // 2. Start the TestServer
    let server = TestServer::start().await?;
    let node = server.issue_node("e2e-agent-01", "E2E Agent 01").await?;

    // 3. Create a temporary config file for the agent (RAII cleanup, panic-safe).
    let temp_dir = TempDir::new("nodelite-e2e-test");
    let config_path = temp_dir.path().join("agent.toml");

    let agent_config_toml = format!(
        r#"[agent]
node_id = "{}"
node_label = "{}"
server = "ws://{}/ws"
token = "{}"
report_interval_secs = 1
"#,
        node.node_id, node.node_label, server.addr, node.token
    );
    fs::write(&config_path, agent_config_toml)?;

    // 4. Spawn the actual nodelite-agent binary process
    let mut child = Command::new(&agent_bin)
        .arg("--config")
        .arg(&config_path)
        .spawn()
        .context("failed to spawn nodelite-agent process")?;

    // 5. Wait for the agent to connect, authenticate, and report a snapshot.
    //    The real agent reads uptime from the host's /proc/uptime, so we only
    //    assert it is online and has reported metrics (not a specific uptime).
    let status = tokio::time::timeout(
        TEST_TIMEOUT,
        server.wait_for_node_online(&node.node_id, TEST_TIMEOUT),
    )
    .await;

    // Check if we successfully got status or if agent failed to start
    let status = match status {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            let _ = child.kill();
            return Err(anyhow!("Failed waiting for node uptime status: {e}"));
        }
        Err(_) => {
            let _ = child.kill();
            return Err(anyhow!(
                "Timed out waiting for agent to connect and report uptime"
            ));
        }
    };

    assert!(status.online);
    assert_eq!(status.identity.node_label, node.node_label);

    // Verify server overview exposes node
    let overview = server.overview().await?;
    assert_eq!(overview.total_nodes, 1);
    assert_eq!(overview.online_nodes, 1);

    // 6. Kill agent and verify offline status
    child.kill().context("failed to kill agent process")?;
    let _ = child.wait(); // prevent zombie process

    let offline_status = tokio::time::timeout(
        Duration::from_secs(10),
        server.wait_for_node_offline(&node.node_id, TEST_TIMEOUT),
    )
    .await
    .context("Timed out waiting for agent to go offline")??;
    assert!(!offline_status.online);

    // Cleanup
    server.shutdown().await?;

    Ok(())
}
