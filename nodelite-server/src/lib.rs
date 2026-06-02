//! NodeLite 中心服务库与启动编排。
//!
//! 角色:
//! - 通过 `/ws` 接收 Agent 上报的 WebSocket 连接;
//! - 通过 `/api/*` 与静态 HTML 给前端提供只读视图;
//! - 通过 `install-agent` / `upgrade-agent` 子命令为运维生成安装脚本片段。
//!
//! 关键设计:
//! - `AppState` 由 `SharedState`(运行态)、`NodeRegistry`(凭证)与 `HistoryStore`(SQLite)组成,
//!   每个 HTTP / WebSocket 处理函数都得到一份廉价克隆。
//! - WebSocket 接入由 `WsAdmissionController` 做总量限流 + IP 限流 + 暴力破解封禁。
//! - 来自 Agent 的所有指标都经过 `sanitize_snapshot` 处理,防止异常值污染统计或图表。

mod admission;
mod agent_logs;
mod alerts;
mod app_state;
mod audit;
mod auth;
mod background;
mod cli;
mod encoding;
mod fs_security;
mod handlers;
mod history;
#[cfg(test)]
#[path = "../tests/integration/mod.rs"]
mod integration_tests;
// 压测模块全部由 `#[tokio::test]` 组成,只在 `cfg(test)` 下编译;`load_test` 特性仅用于
// 取消这些用例的 `#[ignore]`(见各用例上的 `cfg_attr`),因此模块本身无需随特性编译——
// 否则 `--all-features` 的非测试(lib)构建会把依赖 dev-dependency 的压测代码也拉进来而编译失败。
#[cfg(test)]
mod load_test;
mod qr;
mod registry;
mod sanitize;
mod snapshot;
mod startup;
mod state;
#[cfg(test)]
mod test_support;
mod web_assets;
mod ws;

#[cfg(test)]
#[path = "tests/mod.rs"]
mod tests;

use clap::Parser;

pub use crate::cli::CliError;
pub(crate) use app_state::{AppState, ServerReadiness};
#[cfg(test)]
pub(crate) use background::uses_insecure_remote_public_base_url;
#[cfg(test)]
pub(crate) use startup::PROTECTED_CACHE_CONTROL;
pub(crate) use startup::load_server_config;
#[cfg(test)]
pub(crate) use startup::set_protected_response_headers;

use crate::cli::{Cli, Command, install_agent_command, issue_node_command, upgrade_agent_command};

/// CLI 入口:根据子命令分发到具体动作。
pub async fn cli_main() -> std::result::Result<(), CliError> {
    startup::init_tracing();

    let cli = Cli::parse();
    match cli.command {
        Some(Command::IssueNode(args)) => issue_node_command(cli.config.as_path(), args).await,
        Some(Command::InstallAgent(args)) => {
            install_agent_command(cli.config.as_path(), args).await
        }
        Some(Command::UpgradeAgent) => upgrade_agent_command(cli.config.as_path()).await,
        None => startup::run_server(cli.config.as_path())
            .await
            .map_err(|source| CliError::RunServer { source }),
    }
}
