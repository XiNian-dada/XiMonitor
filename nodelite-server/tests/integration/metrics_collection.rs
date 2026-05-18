use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn metrics_flow_reaches_status_and_history_endpoints() -> Result<()> {
    let server = TestServer::start().await?;
    let node = server
        .issue_node("itest-metrics-01", "Integration Metrics 01")
        .await?;
    let mut agent = TestAgent::connect(&server, &node).await?;

    agent.send_fake_metrics(7).await?;

    let status = server
        .wait_for_node_uptime(&node.node_id, 7, TEST_TIMEOUT)
        .await?;
    assert_eq!(
        status
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.uptime_secs),
        Some(7),
    );

    let history = server
        .wait_for_history_points(&node.node_id, 1, TEST_TIMEOUT)
        .await?;
    assert!(!history.is_empty());
    assert!(history.iter().all(|point| point.node_id == node.node_id));
    assert!(history[0].cpu_usage_percent >= 0.0);

    agent.disconnect().await?;
    server.shutdown().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prometheus_metrics_export_reflects_current_nodes() -> Result<()> {
    let server = TestServer::start().await?;
    let online_node = server
        .issue_node("itest-metrics-02", "Integration Metrics 02")
        .await?;
    let offline_node = server
        .issue_node("itest-metrics-03", "Integration Metrics 03")
        .await?;
    let mut agent = TestAgent::connect(&server, &online_node).await?;
    let offline_agent = TestAgent::connect(&server, &offline_node).await?;
    agent.send_fake_metrics(11).await?;
    offline_agent.disconnect().await?;

    let _ = server
        .wait_for_node_uptime(&online_node.node_id, 11, TEST_TIMEOUT)
        .await?;
    let _ = server
        .wait_for_node_offline(&offline_node.node_id, TEST_TIMEOUT)
        .await?;

    let metrics = server.metrics_text().await?;
    assert!(metrics.contains("# TYPE nodelite_server_ready gauge"));
    assert!(metrics.contains("nodelite_nodes_total 2"));
    assert!(metrics.contains("nodelite_nodes_online 1"));
    assert!(metrics.contains("nodelite_nodes_offline 1"));
    assert!(metrics.contains("nodelite_node_online{node_id=\"itest-metrics-02\""));
    assert!(metrics.contains("nodelite_node_online{node_id=\"itest-metrics-03\""));
    assert!(metrics.contains("nodelite_node_snapshot_timestamp_seconds"));
    assert!(metrics.contains("nodelite_network_rate_bytes_per_second{direction=\"rx\"}"));
    assert!(
        metrics.contains("nodelite_node_online{node_id=\"itest-metrics-02\",node_label=\"Integration Metrics 02\",hostname=\"itest-metrics-02.example.internal\",os=\"Linux\"} 1")
    );
    assert!(
        metrics.contains("nodelite_node_online{node_id=\"itest-metrics-03\",node_label=\"Integration Metrics 03\",hostname=\"itest-metrics-03.example.internal\",os=\"Linux\"} 0")
    );

    agent.disconnect().await?;
    server.shutdown().await
}
