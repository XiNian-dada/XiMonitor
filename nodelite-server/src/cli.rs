use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use nodelite_proto::ServerConfig;
use thiserror::Error;

use crate::load_server_config;
use crate::registry::{
    IssueNodeRequest, IssueNodeResult, build_install_script_url, default_agent_release_base_url,
    issue_node, render_agent_config, render_install_command, render_upgrade_command,
};

/// 顶层 server CLI 对外暴露的稳定错误边界。
#[derive(Debug, Error)]
pub enum CliError {
    #[error("failed to load server config")]
    LoadConfig {
        #[source]
        source: anyhow::Error,
    },
    #[error("server command failed")]
    Registry {
        #[from]
        source: crate::registry::RegistryError,
    },
    #[error("server startup failed")]
    RunServer {
        #[source]
        source: anyhow::Error,
    },
}

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
}

struct IssuedNodeBundle {
    issued: IssueNodeResult,
    install_command: String,
    install_script_url: String,
    agent_release_base_url: String,
}

/// `server issue-node`:创建/更新节点并打印对应的 agent.toml 与安装命令。
pub(crate) async fn issue_node_command(
    config_path: &Path,
    args: NodeCommandArgs,
) -> std::result::Result<(), CliError> {
    let config = load_server_config(config_path)
        .await
        .map_err(|source| CliError::LoadConfig { source })?;
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
pub(crate) async fn install_agent_command(
    config_path: &Path,
    args: NodeCommandArgs,
) -> std::result::Result<(), CliError> {
    let config = load_server_config(config_path)
        .await
        .map_err(|source| CliError::LoadConfig { source })?;
    let bundle = issue_node_bundle(&config, &args).await?;
    println!("{}", bundle.install_command);
    Ok(())
}

/// `server upgrade-agent`:打印就地升级现有 Agent 的命令。
pub(crate) async fn upgrade_agent_command(
    config_path: &Path,
) -> std::result::Result<(), CliError> {
    let config = load_server_config(config_path)
        .await
        .map_err(|source| CliError::LoadConfig { source })?;
    let agent_release_base_url = default_agent_release_base_url()?;
    let upgrade_command = render_upgrade_command(&config.public_base_url, &agent_release_base_url)?;
    println!("{upgrade_command}");
    Ok(())
}

/// 同时完成"节点登记"和"安装命令渲染",供两个 CLI 子命令复用。
async fn issue_node_bundle(
    config: &ServerConfig,
    args: &NodeCommandArgs,
) -> std::result::Result<IssuedNodeBundle, CliError> {
    let issued = issue_node(
        config.node_registry_path.as_path(),
        IssueNodeRequest {
            node_id: args.node_id.clone(),
            node_label: args.node_label.clone(),
            tags: args.tags.clone(),
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

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use clap::Parser;
    use nodelite_proto::parse_server_config;

    use super::{Cli, Command, NodeCommandArgs, issue_node_bundle};

    fn unique_temp_dir(prefix: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic enough")
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}-{unique}"))
    }

    fn test_server_config(temp_dir: &std::path::Path) -> nodelite_proto::ServerConfig {
        let registry_path = temp_dir.join("registry.json");
        let history_path = temp_dir.join("history.sqlite3");
        let snapshot_path = temp_dir.join("snapshot.json");
        let config = format!(
            r#"
[server]
listen = "127.0.0.1:20043"
public_base_url = "https://monitor.example.com"
node_registry_path = "{}"
history_db_path = "{}"
snapshot_path = "{}"

[auth]
username = "viewer"
password = "StrongPassword!123"
"#,
            registry_path.display(),
            history_path.display(),
            snapshot_path.display(),
        );
        parse_server_config(&config).expect("server config should parse")
    }

    #[test]
    fn cli_defaults_to_config_server_toml() {
        let cli = Cli::parse_from(["nodelite-server"]);
        assert_eq!(cli.config, std::path::PathBuf::from("config/server.toml"));
        assert!(cli.command.is_none());
    }

    #[test]
    fn cli_parses_issue_node_arguments() {
        let cli = Cli::parse_from([
            "nodelite-server",
            "--config",
            "/tmp/server.toml",
            "issue-node",
            "--node-id",
            "hk-01",
            "--node-label",
            "Hong Kong 01",
            "--tag",
            "edge",
            "--tag",
            "prod",
        ]);
        assert_eq!(cli.config, std::path::PathBuf::from("/tmp/server.toml"));
        match cli.command {
            Some(Command::IssueNode(NodeCommandArgs {
                node_id,
                node_label,
                tags,
            })) => {
                assert_eq!(node_id, "hk-01");
                assert_eq!(node_label.as_deref(), Some("Hong Kong 01"));
                assert_eq!(tags, vec!["edge", "prod"]);
            }
            other => panic!("expected issue-node command, got {other:?}"),
        }
    }

    #[test]
    fn cli_parses_install_and_upgrade_subcommands() {
        let install = Cli::parse_from(["nodelite-server", "install-agent", "--node-id", "jp-01"]);
        assert!(matches!(
            install.command,
            Some(Command::InstallAgent(NodeCommandArgs { node_id, .. })) if node_id == "jp-01"
        ));

        let upgrade = Cli::parse_from(["nodelite-server", "upgrade-agent"]);
        assert!(matches!(upgrade.command, Some(Command::UpgradeAgent)));
    }

    #[tokio::test]
    async fn issue_node_bundle_renders_install_metadata_for_new_node() {
        let temp_dir = unique_temp_dir("nodelite-cli-bundle-test");
        std::fs::create_dir_all(&temp_dir).expect("temp dir should exist");
        let config = test_server_config(&temp_dir);
        let args = NodeCommandArgs {
            node_id: "hk-01".to_string(),
            node_label: Some("Hong Kong 01".to_string()),
            tags: vec!["edge".to_string(), "prod".to_string()],
        };

        let bundle = issue_node_bundle(&config, &args)
            .await
            .expect("node should be issued");

        assert!(bundle.issued.created);
        assert_eq!(bundle.issued.node.node_id, "hk-01");
        assert_eq!(bundle.issued.node.node_label, "Hong Kong 01");
        assert_eq!(bundle.issued.node.tags, vec!["edge", "prod"]);
        assert_eq!(
            bundle.install_script_url,
            "https://monitor.example.com/install/install-agent.sh"
        );
        assert!(
            bundle
                .install_command
                .contains("NODELITE_AGENT_INSTALL_TOKEN=")
        );
        assert!(
            bundle
                .install_command
                .contains("--bootstrap-url 'https://monitor.example.com/install/bootstrap'")
        );
        assert!(
            bundle
                .agent_release_base_url
                .contains("/releases/latest/download")
        );

        let _ = std::fs::remove_file(config.node_registry_path.as_path());
        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
