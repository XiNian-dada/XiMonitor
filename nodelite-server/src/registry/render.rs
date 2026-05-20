use url::Url;

use super::{RegisteredNode, RegistryError, RegistryResult};

/// 从 `public_base_url` 推导 Agent 应连接的 WebSocket URL(http → ws,https → wss)。
pub fn build_agent_server_url(public_base_url: &str) -> RegistryResult<String> {
    let mut url = Url::parse(public_base_url)
        .map_err(|error| RegistryError::invalid_config("server.public_base_url", error))?;
    let scheme = match url.scheme() {
        "http" => "ws",
        "https" => "wss",
        other => {
            return Err(RegistryError::UnsupportedPublicBaseUrlScheme(
                other.to_string(),
            ));
        }
    };
    url.set_scheme(scheme).map_err(|_| {
        RegistryError::internal(
            "failed to set websocket scheme",
            anyhow::anyhow!("url::Url refused to switch scheme"),
        )
    })?;
    url.set_path("/ws");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.into())
}

/// 拼装"安装脚本下载 URL"。
pub fn build_install_script_url(public_base_url: &str) -> RegistryResult<String> {
    let mut url = Url::parse(public_base_url)
        .map_err(|error| RegistryError::invalid_config("server.public_base_url", error))?;
    url.set_path("/install/install-agent.sh");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.into())
}

/// 拼装"安装引导 URL":Agent 安装脚本会带上 Bearer 安装令牌请求这个地址换取自己的 agent.toml。
pub fn build_install_bootstrap_url(public_base_url: &str) -> RegistryResult<String> {
    let mut url = Url::parse(public_base_url)
        .map_err(|error| RegistryError::invalid_config("server.public_base_url", error))?;
    url.set_path("/install/bootstrap");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.into())
}

/// 从 GitHub 仓库 URL 推导 `releases/latest/download` 形式的下载基地址。
/// 只支持 GitHub 仓库,避免误把任意 URL 当作发布源。
pub fn build_github_release_base_url(repository_url: &str) -> RegistryResult<String> {
    let url = Url::parse(repository_url)
        .map_err(|error| RegistryError::invalid_config("repository URL", error))?;
    let host = url.host_str().ok_or_else(|| {
        RegistryError::invalid_config("repository URL", "repository URL must include a host")
    })?;
    if host != "github.com" {
        return Err(RegistryError::invalid_config(
            "repository URL",
            "only GitHub repositories are supported for latest release installs",
        ));
    }

    let mut segments = url.path_segments().ok_or_else(|| {
        RegistryError::invalid_config(
            "repository URL",
            "repository URL must include an owner and repo",
        )
    })?;
    let owner = segments
        .next()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            RegistryError::invalid_config("repository URL", "repository URL must include an owner")
        })?
        .to_string();
    let repo = segments
        .next()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            RegistryError::invalid_config("repository URL", "repository URL must include a repo")
        })?
        .trim_end_matches(".git")
        .to_string();

    let mut release_url = url;
    release_url.set_path(&format!("{owner}/{repo}/releases/latest/download"));
    release_url.set_query(None);
    release_url.set_fragment(None);
    Ok(release_url.into())
}

/// 缺省下载源:由当前 crate 在编译期注入的仓库地址推导而来。
pub fn default_agent_release_base_url() -> RegistryResult<String> {
    build_github_release_base_url(env!("CARGO_PKG_REPOSITORY"))
}

/// 渲染"复制即可用"的安装命令文本。
///
/// 输出形如多行 `curl ... | sh ...`,把安装令牌通过环境变量传递,避免它出现在 `ps` 列表中。
pub fn render_install_command(
    public_base_url: &str,
    install_token: &str,
    agent_release_base_url: &str,
) -> RegistryResult<String> {
    let script_url = build_install_script_url(public_base_url)?;
    let bootstrap_url = build_install_bootstrap_url(public_base_url)?;
    let lines = [
        format!("curl -fsSL {} | \\", shell_quote(&script_url)),
        format!(
            "  NODELITE_AGENT_INSTALL_TOKEN={} sh -s -- \\",
            shell_quote(install_token)
        ),
        format!("  --bootstrap-url {} \\", shell_quote(&bootstrap_url)),
        format!("  --base-url {}", shell_quote(agent_release_base_url)),
    ];

    Ok(lines.join("\n"))
}

/// 渲染就地升级 Agent 的命令文本。与 `render_install_command` 类似,但不需要令牌。
pub fn render_upgrade_command(
    public_base_url: &str,
    agent_release_base_url: &str,
) -> RegistryResult<String> {
    let script_url = build_install_script_url(public_base_url)?;
    let lines = [
        format!("curl -fsSL {} | \\", shell_quote(&script_url)),
        "  NODELITE_AGENT_MODE=upgrade sh -s -- \\".to_string(),
        "  --mode upgrade \\".to_string(),
        format!("  --base-url {}", shell_quote(agent_release_base_url)),
    ];

    Ok(lines.join("\n"))
}

/// 渲染单个节点的 `agent.toml` 文本,作为引导接口的响应体。
pub fn render_agent_config(
    public_base_url: &str,
    node: &RegisteredNode,
    plaintext_token: &str,
) -> RegistryResult<String> {
    let server_url = build_agent_server_url(public_base_url)?;
    let mut content = String::new();
    content.push_str("[agent]\n");
    content.push_str(&format!("node_id = \"{}\"\n", toml_escape(&node.node_id)));
    content.push_str(&format!(
        "node_label = \"{}\"\n",
        toml_escape(&node.node_label)
    ));
    content.push_str(&format!("server = \"{}\"\n", toml_escape(&server_url)));
    content.push_str(&format!("token = \"{}\"\n", toml_escape(plaintext_token)));
    content.push_str("report_interval_secs = 5\n");
    if !node.tags.is_empty() {
        let tags = node
            .tags
            .iter()
            .map(|tag| format!("\"{}\"", toml_escape(tag)))
            .collect::<Vec<_>>()
            .join(", ");
        content.push_str(&format!("tags = [{tags}]\n"));
    }
    Ok(content)
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn toml_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}
