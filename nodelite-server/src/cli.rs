use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::{Parser, Subcommand};
use nodelite_proto::ServerConfig;

use crate::load_server_config;
use crate::registry::{
    IssueNodeRequest, IssueNodeResult, build_install_script_url, default_agent_release_base_url,
    issue_node, render_agent_config, render_install_command, render_upgrade_command,
};

/// 顶层命令行参数。
#[derive(Debug, Parser)]
#[command(name = "nodelite-server")]
#[command(about = "NodeLite central server")]
pub(crate) struct Cli {
    /// 配置文件路径,默认 `config/server.toml`。
    #[arg(long, global = true, default_value = "config/server.toml")]
    pub(crate) config: PathBuf,
    /// 可选子命令。不指定时进入"启动 Web 服务"模式。
    #[command(subcommand)]
    pub(crate) command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// 颁发节点凭证(仅打印,不安装到 Agent 节点上)。
    IssueNode(NodeCommandArgs),
    /// 颁发节点凭证并打印 Agent 上的安装命令。
    InstallAgent(NodeCommandArgs),
    /// 打印就地升级 Agent 所需的命令。
    UpgradeAgent,
}

/// 节点相关命令的共享参数。
#[derive(Debug, Parser, Clone)]
pub(crate) struct NodeCommandArgs {
    #[arg(long)]
    node_id: String,
    #[arg(long)]
    node_label: Option<String>,
    #[arg(long = "tag")]
    tags: Vec<String>,
    /// 是否强制轮换该节点的现有 token。
    #[arg(long)]
    rotate_token: bool,
}

struct IssuedNodeBundle {
    issued: IssueNodeResult,
    install_command: String,
    install_script_url: String,
    agent_release_base_url: String,
}

/// `server issue-node`:创建/更新节点并打印对应的 agent.toml 与安装命令。
pub(crate) async fn issue_node_command(config_path: &Path, args: NodeCommandArgs) -> Result<()> {
    let config = load_server_config(config_path).await?;
    let bundle = issue_node_bundle(&config, &args).await?;
    let agent_config = render_agent_config(
        &config.public_base_url,
        &bundle.issued.node,
        &bundle.issued.node_session_token,
    )?;
    let action = if bundle.issued.created {
        "created"
    } else if bundle.issued.rotated_token {
        "rotated"
    } else {
        "reused"
    };

    println!("node_id: {}", bundle.issued.node.node_id);
    println!("node_label: {}", bundle.issued.node.node_label);
    println!("status: {action}");
    println!("registry_path: {}", config.node_registry_path.display());
    println!("install_script_url: {}", bundle.install_script_url);
    println!("agent_release_base_url: {}", bundle.agent_release_base_url);
    println!(
        "install_token_expires_at: {}",
        bundle.issued.install_token_expires_at.to_rfc3339()
    );
    println!();
    println!("# agent.toml");
    println!("{agent_config}");
    println!("# install command");
    println!("{}", bundle.install_command);
    println!();
    println!("note: the install command above already embeds a one-time install token.");

    Ok(())
}

/// `server install-agent`:只打印安装命令,适合管道式使用。
pub(crate) async fn install_agent_command(config_path: &Path, args: NodeCommandArgs) -> Result<()> {
    let config = load_server_config(config_path).await?;
    let bundle = issue_node_bundle(&config, &args).await?;
    println!("{}", bundle.install_command);
    Ok(())
}

/// `server upgrade-agent`:打印就地升级现有 Agent 的命令。
pub(crate) async fn upgrade_agent_command(config_path: &Path) -> Result<()> {
    let config = load_server_config(config_path).await?;
    let agent_release_base_url = default_agent_release_base_url()?;
    let upgrade_command = render_upgrade_command(&config.public_base_url, &agent_release_base_url)?;
    println!("{upgrade_command}");
    Ok(())
}

/// 同时完成"节点登记"和"安装命令渲染",供两个 CLI 子命令复用。
async fn issue_node_bundle(
    config: &ServerConfig,
    args: &NodeCommandArgs,
) -> Result<IssuedNodeBundle> {
    let issued = issue_node(
        config.node_registry_path.as_path(),
        IssueNodeRequest {
            node_id: args.node_id.clone(),
            node_label: args.node_label.clone(),
            tags: args.tags.clone(),
            rotate_token: args.rotate_token,
        },
    )
    .await?;

    let agent_release_base_url = default_agent_release_base_url()?;
    let install_command = render_install_command(
        &config.public_base_url,
        &issued.install_token,
        &agent_release_base_url,
    )?;
    let install_script_url = build_install_script_url(&config.public_base_url)?;

    Ok(IssuedNodeBundle {
        issued,
        install_command,
        install_script_url,
        agent_release_base_url,
    })
}
