use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use getrandom::fill as fill_random;
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::sync::RwLock;
use url::Url;
use ximonitor_proto::{NodeIdentity, ReadonlyAuthConfig};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RegisteredNode {
    pub node_id: String,
    pub node_label: String,
    pub token: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NodeRegistry {
    path: Arc<PathBuf>,
    state: Arc<RwLock<RegistryState>>,
    legacy_shared_token: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct RegistryState {
    entries: HashMap<String, RegisteredNode>,
}

#[derive(Debug, Clone)]
pub struct IssueNodeRequest {
    pub node_id: String,
    pub node_label: Option<String>,
    pub tags: Vec<String>,
    pub rotate_token: bool,
}

#[derive(Debug, Clone)]
pub struct IssueNodeResult {
    pub node: RegisteredNode,
    pub created: bool,
    pub rotated_token: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
struct RegistryFile {
    #[serde(default)]
    nodes: Vec<RegisteredNode>,
}

impl NodeRegistry {
    pub async fn load(path: &Path, legacy_shared_token: Option<String>) -> Result<Self> {
        let entries = load_registry_entries(path).await?;

        Ok(Self {
            path: Arc::new(path.to_path_buf()),
            state: Arc::new(RwLock::new(RegistryState { entries })),
            legacy_shared_token,
        })
    }

    pub async fn authorize(&self, identity: &NodeIdentity, token: &str) -> Result<NodeIdentity> {
        validate_runtime_identity(identity)?;
        validate_non_empty("hello.token", token)?;
        let state = self.state.read().await;
        authorize_identity(&state.entries, self.legacy_shared_token.as_deref(), identity, token)
    }

    pub async fn is_token_current(&self, node_id: &str, token: &str) -> bool {
        let state = self.state.read().await;
        is_token_current(
            &state.entries,
            self.legacy_shared_token.as_deref(),
            node_id,
            token,
        )
    }

    pub async fn reload(&self) -> Result<bool> {
        let entries = load_registry_entries(self.path.as_path()).await?;
        let mut state = self.state.write().await;
        if state.entries == entries {
            return Ok(false);
        }

        state.entries = entries;
        Ok(true)
    }

    pub async fn count(&self) -> usize {
        let state = self.state.read().await;
        state.entries.len()
    }

    pub fn uses_legacy_shared_token(&self) -> bool {
        self.legacy_shared_token.is_some()
    }

    pub fn path(&self) -> &Path {
        self.path.as_path()
    }
}

pub async fn issue_node(path: &Path, request: IssueNodeRequest) -> Result<IssueNodeResult> {
    validate_identifier("node_id", &request.node_id)?;
    if let Some(node_label) = request.node_label.as_deref() {
        validate_non_empty("node_label", node_label)?;
    }

    let mut file = load_registry_file(path).await?;
    let mut rotated_token = false;
    let now = Utc::now();

    if let Some(index) = file
        .nodes
        .iter()
        .position(|node| node.node_id == request.node_id)
    {
        if let Some(node_label) = request.node_label.as_ref() {
            file.nodes[index].node_label = node_label.trim().to_string();
        }
        if !request.tags.is_empty() {
            file.nodes[index].tags = normalize_string_list(request.tags);
        }
        if request.rotate_token {
            file.nodes[index].token = generate_token()?;
            rotated_token = true;
        }

        validate_registered_node(&file.nodes[index])?;
        let node = file.nodes[index].clone();
        save_registry_file(path, &file).await?;
        return Ok(IssueNodeResult {
            node,
            created: false,
            rotated_token,
        });
    }

    let node = RegisteredNode {
        node_id: request.node_id.trim().to_string(),
        node_label: request
            .node_label
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(request.node_id.as_str())
            .to_string(),
        token: generate_token()?,
        tags: normalize_string_list(request.tags),
        created_at: now,
    };
    validate_registered_node(&node)?;

    file.nodes.push(node.clone());
    file.nodes
        .sort_by(|left, right| left.node_id.cmp(&right.node_id));
    save_registry_file(path, &file).await?;

    Ok(IssueNodeResult {
        node,
        created: true,
        rotated_token,
    })
}

pub fn build_agent_server_url(public_base_url: &str) -> Result<String> {
    let mut url = Url::parse(public_base_url)
        .with_context(|| "invalid server.public_base_url".to_string())?;
    let scheme = match url.scheme() {
        "http" => "ws",
        "https" => "wss",
        other => bail!("unsupported public_base_url scheme for agent install: {other}"),
    };
    url.set_scheme(scheme)
        .map_err(|_| anyhow!("failed to set websocket scheme"))?;
    url.set_path("/ws");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.into())
}

pub fn build_install_script_url(public_base_url: &str) -> Result<String> {
    let mut url = Url::parse(public_base_url)
        .with_context(|| "invalid server.public_base_url".to_string())?;
    url.set_path("/install/install-agent.sh");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.into())
}

pub fn render_install_command(
    public_base_url: &str,
    node: &RegisteredNode,
    readonly_auth: Option<&ReadonlyAuthConfig>,
    agent_release_base_url: Option<&str>,
) -> Result<String> {
    let script_url = build_install_script_url(public_base_url)?;
    let server_url = build_agent_server_url(public_base_url)?;
    let mut curl_command = "curl -fsSL".to_string();
    if let Some(readonly_auth) = readonly_auth {
        curl_command.push_str(&format!(
            " --user {}",
            shell_quote(&format!(
                "{}:{}",
                readonly_auth.username, readonly_auth.password
            ))
        ));
    }

    let mut lines = vec![
        format!("{curl_command} {} | sh -s -- \\", shell_quote(&script_url)),
        format!("  --server {} \\", shell_quote(&server_url)),
        format!("  --node-id {} \\", shell_quote(&node.node_id)),
        format!("  --node-label {} \\", shell_quote(&node.node_label)),
        format!("  --token {}", shell_quote(&node.token)),
    ];

    if let Some(agent_release_base_url) = agent_release_base_url {
        lines.push(format!(
            "  --base-url {}",
            shell_quote(agent_release_base_url)
        ));
        let token_line_index = lines.len().saturating_sub(2);
        lines[token_line_index].push_str(" \\");
    }

    Ok(lines.join("\n"))
}

pub fn render_agent_config(public_base_url: &str, node: &RegisteredNode) -> Result<String> {
    let server_url = build_agent_server_url(public_base_url)?;
    let mut content = String::new();
    content.push_str("[agent]\n");
    content.push_str(&format!("node_id = \"{}\"\n", toml_escape(&node.node_id)));
    content.push_str(&format!(
        "node_label = \"{}\"\n",
        toml_escape(&node.node_label)
    ));
    content.push_str(&format!("server = \"{}\"\n", toml_escape(&server_url)));
    content.push_str(&format!("token = \"{}\"\n", toml_escape(&node.token)));
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

async fn load_registry_file(path: &Path) -> Result<RegistryFile> {
    let content = match fs::read_to_string(path).await {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RegistryFile::default());
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read node registry {}", path.display()));
        }
    };

    let file: RegistryFile = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse node registry {}", path.display()))?;
    validate_registry_file(path, &file)?;
    Ok(file)
}

async fn load_registry_entries(path: &Path) -> Result<HashMap<String, RegisteredNode>> {
    let file = load_registry_file(path).await?;
    let mut entries = HashMap::with_capacity(file.nodes.len());
    for node in file.nodes {
        if entries.insert(node.node_id.clone(), node).is_some() {
            bail!("duplicate node_id found in {}", path.display());
        }
    }
    Ok(entries)
}

async fn save_registry_file(path: &Path, file: &RegistryFile) -> Result<()> {
    validate_registry_file(path, file)?;

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .await
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
    }

    let payload =
        serde_json::to_string_pretty(file).context("failed to serialize node registry")?;
    let tmp_path = temporary_registry_path(path);
    fs::write(&tmp_path, payload)
        .await
        .with_context(|| format!("failed to write {}", tmp_path.display()))?;
    fs::rename(&tmp_path, path)
        .await
        .with_context(|| format!("failed to replace {}", path.display()))?;
    Ok(())
}

fn temporary_registry_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("server.json");
    path.with_file_name(format!("{file_name}.tmp"))
}

fn validate_registry_file(path: &Path, file: &RegistryFile) -> Result<()> {
    let mut seen = HashMap::with_capacity(file.nodes.len());
    for node in &file.nodes {
        validate_registered_node(node)?;
        if seen.insert(node.node_id.as_str(), ()).is_some() {
            bail!("duplicate node_id {} in {}", node.node_id, path.display());
        }
    }
    Ok(())
}

fn validate_registered_node(node: &RegisteredNode) -> Result<()> {
    validate_identifier("node.node_id", &node.node_id)?;
    validate_non_empty("node.node_label", &node.node_label)?;
    validate_non_empty("node.token", &node.token)?;
    Ok(())
}

fn authorize_identity(
    entries: &HashMap<String, RegisteredNode>,
    legacy_shared_token: Option<&str>,
    identity: &NodeIdentity,
    token: &str,
) -> Result<NodeIdentity> {
    if let Some(entry) = entries.get(identity.node_id.as_str()) {
        if token != entry.token {
            bail!("invalid token for enrolled node {}", entry.node_id);
        }

        let mut identity = identity.clone();
        identity.node_id = entry.node_id.clone();
        identity.node_label = entry.node_label.clone();
        identity.tags = entry.tags.clone();
        return Ok(identity);
    }

    if let Some(shared_token) = legacy_shared_token {
        if token == shared_token {
            return Ok(identity.clone());
        }
    }

    bail!("node {} is not enrolled", identity.node_id);
}

fn is_token_current(
    entries: &HashMap<String, RegisteredNode>,
    legacy_shared_token: Option<&str>,
    node_id: &str,
    token: &str,
) -> bool {
    if let Some(entry) = entries.get(node_id) {
        return token == entry.token;
    }

    legacy_shared_token == Some(token)
}

fn validate_runtime_identity(identity: &NodeIdentity) -> Result<()> {
    validate_identifier("identity.node_id", &identity.node_id)?;
    validate_non_empty("identity.node_label", &identity.node_label)?;
    validate_non_empty("identity.agent_version", &identity.agent_version)?;
    validate_non_empty("identity.hostname", &identity.hostname)?;
    validate_non_empty("identity.os", &identity.os)?;
    Ok(())
}

fn validate_identifier(field: &str, value: &str) -> Result<()> {
    validate_non_empty(field, value)?;
    if value.len() > 128 {
        bail!("{field} must be <= 128 characters");
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        bail!("{field} must use only ASCII letters, numbers, '-', '_' or '.'");
    }
    Ok(())
}

fn validate_non_empty(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("{field} must not be empty");
    }
    Ok(())
}

fn normalize_string_list(values: Vec<String>) -> Vec<String> {
    let mut values: Vec<String> = values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect();
    values.sort();
    values.dedup();
    values
}

fn generate_token() -> Result<String> {
    let mut bytes = [0_u8; 32];
    fill_random(&mut bytes).context("failed to gather secure random bytes")?;
    Ok(hex_encode(&bytes))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn toml_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use chrono::Utc;
    use tokio::runtime::Runtime;

    use super::{
        IssueNodeRequest, NodeRegistry, RegisteredNode, RegistryFile, build_agent_server_url,
        issue_node, render_install_command,
    };
    use ximonitor_proto::{NodeIdentity, ReadonlyAuthConfig};

    #[test]
    fn agent_server_url_uses_wss_for_https() {
        let url = build_agent_server_url("https://monitor.example.com").expect("url should build");
        assert_eq!(url, "wss://monitor.example.com/ws");
    }

    #[test]
    fn registry_authorizes_per_node_token_and_overrides_metadata() {
        let runtime = Runtime::new().expect("runtime should build");
        runtime.block_on(async {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should be monotonic enough")
                .as_nanos();
            let temp_dir =
                std::env::temp_dir().join(format!("ximonitor-registry-auth-test-{unique}"));
            std::fs::create_dir_all(&temp_dir).expect("temp dir should exist");
            let path = temp_dir.join("server.json");
            let file = RegistryFile {
                nodes: vec![RegisteredNode {
                    node_id: "osaka-01".to_string(),
                    node_label: "Osaka 01".to_string(),
                    token: "secret".to_string(),
                    tags: vec!["edge".to_string()],
                    created_at: Utc::now(),
                }],
            };
            std::fs::write(&path, serde_json::to_string_pretty(&file).expect("json"))
                .expect("registry should be written");
            let registry = NodeRegistry::load(&path, None)
                .await
                .expect("registry should load");
            let identity = NodeIdentity {
                node_id: "osaka-01".to_string(),
                node_label: "Wrong".to_string(),
                hostname: "osaka-01.internal".to_string(),
                os: "Ubuntu".to_string(),
                kernel_version: None,
                cpu_model: None,
                cpu_cores: 2,
                agent_version: "0.1.0".to_string(),
                boot_time: None,
                tags: vec!["wrong".to_string()],
            };

            let authorized = registry
                .authorize(&identity, "secret")
                .await
                .expect("identity should authorize");
            assert_eq!(authorized.node_label, "Osaka 01");
            assert_eq!(authorized.tags, vec!["edge"]);

            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_dir(&temp_dir);
        });
    }

    #[test]
    fn issue_node_persists_registry_and_renders_install_command() {
        let runtime = Runtime::new().expect("runtime should build");
        runtime.block_on(async {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should be monotonic enough")
                .as_nanos();
            let temp_dir = std::env::temp_dir().join(format!("ximonitor-registry-test-{unique}"));
            std::fs::create_dir_all(&temp_dir).expect("temp dir should exist");
            let path = temp_dir.join("server.json");
            let issued = issue_node(
                &path,
                IssueNodeRequest {
                    node_id: "hk-01".to_string(),
                    node_label: Some("Hong Kong 01".to_string()),
                    tags: vec!["edge".to_string(), "apac".to_string()],
                    rotate_token: false,
                },
            )
            .await
            .expect("node should be issued");
            assert!(issued.created);

            let stored = std::fs::read_to_string(&path).expect("registry should be stored");
            let parsed: RegistryFile =
                serde_json::from_str(&stored).expect("stored registry should parse");
            assert_eq!(parsed.nodes.len(), 1);

            let command = render_install_command(
                "https://monitor.example.com",
                &issued.node,
                Some(&ReadonlyAuthConfig {
                    username: "viewer".to_string(),
                    password: "secret".to_string(),
                }),
                Some("https://downloads.example.com/releases/latest/download"),
            )
            .expect("install command should render");
            assert!(command.contains("--token"));
            assert!(command.contains("/install/install-agent.sh"));
            assert!(command.contains("--user"));

            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_dir(&temp_dir);
        });
    }

    #[test]
    fn registry_reload_picks_up_rotated_tokens() {
        let runtime = Runtime::new().expect("runtime should build");
        runtime.block_on(async {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should be monotonic enough")
                .as_nanos();
            let temp_dir =
                std::env::temp_dir().join(format!("ximonitor-registry-reload-test-{unique}"));
            std::fs::create_dir_all(&temp_dir).expect("temp dir should exist");
            let path = temp_dir.join("server.json");

            let issued = issue_node(
                &path,
                IssueNodeRequest {
                    node_id: "hk-01".to_string(),
                    node_label: Some("Hong Kong 01".to_string()),
                    tags: Vec::new(),
                    rotate_token: false,
                },
            )
            .await
            .expect("node should be issued");
            let old_token = issued.node.token.clone();
            let registry = NodeRegistry::load(&path, None)
                .await
                .expect("registry should load");
            assert!(registry.is_token_current("hk-01", &old_token).await);

            let rotated = issue_node(
                &path,
                IssueNodeRequest {
                    node_id: "hk-01".to_string(),
                    node_label: Some("Hong Kong 01".to_string()),
                    tags: Vec::new(),
                    rotate_token: true,
                },
            )
            .await
            .expect("node token should rotate");
            assert!(registry.reload().await.expect("reload should succeed"));
            assert!(!registry.is_token_current("hk-01", &old_token).await);
            assert!(registry.is_token_current("hk-01", &rotated.node.token).await);

            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_dir(&temp_dir);
        });
    }
}
