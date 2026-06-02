//! NodeLite Agent 入口程序。

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    nodelite_agent::runtime::run().await
}
