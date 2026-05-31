use super::*;

use nodelite_proto::BrowserMessage;

/// 未认证的浏览器 WebSocket 升级握手必须被拒为 HTTP 401。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn browser_ws_rejects_unauthenticated_connection() -> Result<()> {
    let server = TestServer::start().await?;
    TestBrowserClient::expect_unauthorized(&server).await?;
    server.shutdown().await
}

/// 连接建立后,服务端立即下发一条全量 `InitialState`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn browser_ws_sends_initial_state_on_connect() -> Result<()> {
    let server = TestServer::start().await?;
    let mut browser = TestBrowserClient::connect(&server).await?;

    let message = browser.next_message(TEST_TIMEOUT).await?;
    match message {
        BrowserMessage::InitialState {
            overview, nodes, ..
        } => {
            assert_eq!(overview.total_nodes, 0);
            assert!(nodes.is_empty());
        }
        other => panic!("expected InitialState, got {other:?}"),
    }

    browser.close().await?;
    server.shutdown().await
}

/// 浏览器连接后再有 agent 注册并上报,浏览器应收到该节点的增量 `NodeUpsert`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn browser_ws_pushes_node_upsert_when_agent_registers() -> Result<()> {
    let server = TestServer::start().await?;
    // 先连浏览器,确保它的 InitialState 是空的,后续节点变化只能通过增量到达。
    let mut browser = TestBrowserClient::connect(&server).await?;
    let initial = browser.next_message(TEST_TIMEOUT).await?;
    assert!(matches!(initial, BrowserMessage::InitialState { .. }));

    // agent 注册 + 上报 → 触发 SharedState 脏信号。
    let node = server
        .issue_node("itest-browser-01", "Integration Browser 01")
        .await?;
    let mut agent = TestAgent::connect(&server, &node).await?;
    agent.send_fake_metrics(1).await?;

    // 浏览器应收到该节点的 NodeUpsert(跳过其间可能的 OverviewUpdate)。
    let upsert = browser
        .next_matching(TEST_TIMEOUT, |message| {
            matches!(
                message,
                BrowserMessage::NodeUpsert { node, .. } if node.identity.node_id == "itest-browser-01"
            )
        })
        .await?;
    match upsert {
        BrowserMessage::NodeUpsert { node, .. } => {
            assert_eq!(node.identity.node_id, "itest-browser-01");
            assert!(node.online);
        }
        other => panic!("expected NodeUpsert, got {other:?}"),
    }

    agent.disconnect().await?;
    browser.close().await?;
    server.shutdown().await
}

/// 客户端发送应用层 `Ping`,服务端必须回 `Pong`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn browser_ws_replies_pong_to_ping() -> Result<()> {
    let server = TestServer::start().await?;
    let mut browser = TestBrowserClient::connect(&server).await?;
    let initial = browser.next_message(TEST_TIMEOUT).await?;
    assert!(matches!(initial, BrowserMessage::InitialState { .. }));

    browser.send_ping().await?;
    let pong = browser
        .next_matching(TEST_TIMEOUT, |message| {
            matches!(message, BrowserMessage::Pong)
        })
        .await?;
    assert!(matches!(pong, BrowserMessage::Pong));

    browser.close().await?;
    server.shutdown().await
}
