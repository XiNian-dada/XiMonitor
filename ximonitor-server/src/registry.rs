use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use getrandom::fill as fill_random;
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::sync::RwLock;
use url::Url;
use ximonitor_proto::NodeIdentity;

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RegisteredNode {
    pub node_id: String,
    pub node_label: String,
    pub token: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstallSession {
    pub token: String,
    pub node_id: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NodeRegistry {
    path: Arc<PathBuf>,
    state: Arc<RwLock<RegistryState>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct RegistryState {
    entries: HashMap<String, RegisteredNode>,
    install_sessions: HashMap<String, InstallSession>,
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
    pub install_token: String,
    pub install_token_expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
struct RegistryFile {
    #[serde(default)]
    nodes: Vec<RegisteredNode>,
    #[serde(default)]
    install_sessions: Vec<InstallSession>,
}

const INSTALL_TOKEN_TTL_MINUTES: i64 = 15;

impl NodeRegistry {
    pub async fn load(path: &Path) -> Result<Self> {
        let state = load_registry_state(path).await?;

        Ok(Self {
            path: Arc::new(path.to_path_buf()),
            state: Arc::new(RwLock::new(state)),
        })
    }

    pub async fn authorize(&self, identity: &NodeIdentity, token: &str) -> Result<NodeIdentity> {
        validate_runtime_identity(identity)?;
        validate_non_empty("hello.token", token)?;
        let state = self.state.read().await;
        authorize_identity(&state.entries, identity, token)
    }

    pub async fn is_token_current(&self, node_id: &str, token: &str) -> bool {
        let state = self.state.read().await;
        is_token_current(&state.entries, node_id, token)
    }

    pub async fn reload(&self) -> Result<bool> {
        let next_state = load_registry_state(self.path.as_path()).await?;
        let mut state = self.state.write().await;
        if *state == next_state {
            return Ok(false);
        }

        *state = next_state;
        Ok(true)
    }

    pub async fn count(&self) -> usize {
        let state = self.state.read().await;
        state.entries.len()
    }

    pub async fn consume_install_token(&self, token: &str) -> Result<Option<RegisteredNode>> {
        validate_non_empty("install token", token)?;

        let mut file = load_registry_file(self.path.as_path()).await?;
        let pruned = prune_expired_install_sessions(&mut file, Utc::now());
        let Some(index) = file
            .install_sessions
            .iter()
            .position(|session| session.token == token)
        else {
            if pruned {
                save_registry_file(self.path.as_path(), &file).await?;
                self.replace_state_from_file(file).await?;
            }
            return Ok(None);
        };

        let session = file.install_sessions.remove(index);
        let node = file
            .nodes
            .iter()
            .find(|node| node.node_id == session.node_id)
            .cloned();
        save_registry_file(self.path.as_path(), &file).await?;
        self.replace_state_from_file(file).await?;
        Ok(node)
    }

    pub fn path(&self) -> &Path {
        self.path.as_path()
    }

    async fn replace_state_from_file(&self, file: RegistryFile) -> Result<()> {
        let state = registry_state_from_file(self.path.as_path(), file)?;
        let mut guard = self.state.write().await;
        *guard = state;
        Ok(())
    }
}

pub async fn issue_node(path: &Path, request: IssueNodeRequest) -> Result<IssueNodeResult> {
    validate_identifier("node_id", &request.node_id)?;
    if let Some(node_label) = request.node_label.as_deref() {
        validate_non_empty("node_label", node_label)?;
    }

    let mut file = load_registry_file(path).await?;
    let now = Utc::now();
    prune_expired_install_sessions(&mut file, now);
    let mut rotated_token = false;

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
        let install_session = mint_install_session(&mut file, &node.node_id, now)?;
        save_registry_file(path, &file).await?;
        return Ok(IssueNodeResult {
            node,
            created: false,
            rotated_token,
            install_token: install_session.token,
            install_token_expires_at: install_session.expires_at,
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
    let install_session = mint_install_session(&mut file, &node.node_id, now)?;
    save_registry_file(path, &file).await?;

    Ok(IssueNodeResult {
        node,
        created: true,
        rotated_token,
        install_token: install_session.token,
        install_token_expires_at: install_session.expires_at,
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

pub fn build_install_bootstrap_url(public_base_url: &str) -> Result<String> {
    let mut url = Url::parse(public_base_url)
        .with_context(|| "invalid server.public_base_url".to_string())?;
    url.set_path("/install/bootstrap");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.into())
}

pub fn render_install_command(
    public_base_url: &str,
    agent_release_base_url: Option<&str>,
    agent_release_sha256_x86_64: Option<&str>,
    agent_release_sha256_aarch64: Option<&str>,
) -> Result<String> {
    let script_url = build_install_script_url(public_base_url)?;
    let bootstrap_url = build_install_bootstrap_url(public_base_url)?;
    let mut lines = vec![
        format!("curl -fsSL {} | sh -s -- \\", shell_quote(&script_url)),
        format!("  --bootstrap-url {}", shell_quote(&bootstrap_url)),
    ];

    if let Some(agent_release_base_url) = agent_release_base_url {
        let Some(agent_release_sha256_x86_64) = agent_release_sha256_x86_64 else {
            bail!("missing x86_64 agent checksum for install command");
        };
        let Some(agent_release_sha256_aarch64) = agent_release_sha256_aarch64 else {
            bail!("missing aarch64 agent checksum for install command");
        };
        lines[1].push_str(" \\");
        lines.push(format!(
            "  --base-url {} \\",
            shell_quote(agent_release_base_url)
        ));
        lines.push(format!(
            "  --sha256-x86_64 {} \\",
            shell_quote(agent_release_sha256_x86_64)
        ));
        lines.push(format!(
            "  --sha256-aarch64 {}",
            shell_quote(agent_release_sha256_aarch64)
        ));
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

async fn load_registry_state(path: &Path) -> Result<RegistryState> {
    let mut file = load_registry_file(path).await?;
    prune_expired_install_sessions(&mut file, Utc::now());
    registry_state_from_file(path, file)
}

fn registry_state_from_file(path: &Path, file: RegistryFile) -> Result<RegistryState> {
    let mut entries = HashMap::with_capacity(file.nodes.len());
    for node in file.nodes {
        if entries.insert(node.node_id.clone(), node).is_some() {
            bail!("duplicate node_id found in {}", path.display());
        }
    }
    let mut install_sessions = HashMap::with_capacity(file.install_sessions.len());
    for session in file.install_sessions {
        if install_sessions
            .insert(session.token.clone(), session)
            .is_some()
        {
            bail!("duplicate install token found in {}", path.display());
        }
    }

    Ok(RegistryState {
        entries,
        install_sessions,
    })
}

async fn save_registry_file(path: &Path, file: &RegistryFile) -> Result<()> {
    validate_registry_file(path, file)?;

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .await
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let payload =
        serde_json::to_string_pretty(file).context("failed to serialize node registry")?;
    let tmp_path = temporary_registry_path(path);
    write_registry_payload(&tmp_path, &payload)
        .with_context(|| format!("failed to write {}", tmp_path.display()))?;
    fs::rename(&tmp_path, path)
        .await
        .with_context(|| format!("failed to replace {}", path.display()))?;
    harden_registry_permissions(path)
        .with_context(|| format!("failed to set permissions on {}", path.display()))?;
    Ok(())
}

fn temporary_registry_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("server.json");
    path.with_file_name(format!("{file_name}.tmp"))
}

fn write_registry_payload(path: &Path, payload: &str) -> Result<()> {
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);

    let mut file = options
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    file.write_all(payload.as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn harden_registry_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to chmod {}", path.display()))?;
    }

    #[cfg(not(unix))]
    {
        let _ = path;
    }

    Ok(())
}

fn validate_registry_file(path: &Path, file: &RegistryFile) -> Result<()> {
    let mut seen_nodes = HashMap::with_capacity(file.nodes.len());
    for node in &file.nodes {
        validate_registered_node(node)?;
        if seen_nodes.insert(node.node_id.as_str(), ()).is_some() {
            bail!("duplicate node_id {} in {}", node.node_id, path.display());
        }
    }
    let mut seen_install_tokens = HashMap::with_capacity(file.install_sessions.len());
    for session in &file.install_sessions {
        validate_install_session(session)?;
        if !seen_nodes.contains_key(session.node_id.as_str()) {
            bail!(
                "install token for unknown node_id {} in {}",
                session.node_id,
                path.display()
            );
        }
        if seen_install_tokens
            .insert(session.token.as_str(), ())
            .is_some()
        {
            bail!("duplicate install token in {}", path.display());
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

fn validate_install_session(session: &InstallSession) -> Result<()> {
    validate_non_empty("install_session.token", &session.token)?;
    validate_identifier("install_session.node_id", &session.node_id)?;
    Ok(())
}

fn prune_expired_install_sessions(file: &mut RegistryFile, now: DateTime<Utc>) -> bool {
    let original_len = file.install_sessions.len();
    file.install_sessions
        .retain(|session| session.expires_at > now);
    original_len != file.install_sessions.len()
}

fn mint_install_session(
    file: &mut RegistryFile,
    node_id: &str,
    now: DateTime<Utc>,
) -> Result<InstallSession> {
    file.install_sessions
        .retain(|session| session.node_id != node_id);
    let session = InstallSession {
        token: generate_token()?,
        node_id: node_id.to_string(),
        created_at: now,
        expires_at: now + ChronoDuration::minutes(INSTALL_TOKEN_TTL_MINUTES),
    };
    file.install_sessions.push(session.clone());
    Ok(session)
}

fn authorize_identity(
    entries: &HashMap<String, RegisteredNode>,
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

    bail!("node {} is not enrolled", identity.node_id);
}

fn is_token_current(entries: &HashMap<String, RegisteredNode>, node_id: &str, token: &str) -> bool {
    if let Some(entry) = entries.get(node_id) {
        return token == entry.token;
    }

    false
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
    use ximonitor_proto::NodeIdentity;

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
                install_sessions: Vec::new(),
            };
            std::fs::write(&path, serde_json::to_string_pretty(&file).expect("json"))
                .expect("registry should be written");
            let registry = NodeRegistry::load(&path)
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
            assert_eq!(parsed.install_sessions.len(), 1);
            assert_eq!(parsed.install_sessions[0].token, issued.install_token);

            let command = render_install_command(
                "https://monitor.example.com",
                Some("https://downloads.example.com/releases/latest/download"),
                Some("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"),
                Some("abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"),
            )
            .expect("install command should render");
            assert!(command.contains("--bootstrap-url"));
            assert!(command.contains("/install-agent.sh"));
            assert!(command.contains("--sha256-x86_64"));
            assert!(!command.contains(&issued.node.token));
            assert!(!command.contains(&issued.install_token));

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
            let registry = NodeRegistry::load(&path)
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
            assert!(
                registry
                    .is_token_current("hk-01", &rotated.node.token)
                    .await
            );

            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_dir(&temp_dir);
        });
    }

    #[test]
    fn install_tokens_are_one_time_use() {
        let runtime = Runtime::new().expect("runtime should build");
        runtime.block_on(async {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should be monotonic enough")
                .as_nanos();
            let temp_dir =
                std::env::temp_dir().join(format!("ximonitor-install-token-test-{unique}"));
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
            let registry = NodeRegistry::load(&path)
                .await
                .expect("registry should load");

            let consumed = registry
                .consume_install_token(&issued.install_token)
                .await
                .expect("install token should be consumable")
                .expect("install token should resolve to a node");
            assert_eq!(consumed.node_id, issued.node.node_id);
            assert_eq!(consumed.token, issued.node.token);
            assert!(
                registry
                    .consume_install_token(&issued.install_token)
                    .await
                    .expect("second install token lookup should succeed")
                    .is_none()
            );

            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_dir(&temp_dir);
        });
    }

    #[test]
    fn unenrolled_nodes_are_rejected() {
        let runtime = Runtime::new().expect("runtime should build");
        runtime.block_on(async {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should be monotonic enough")
                .as_nanos();
            let temp_dir =
                std::env::temp_dir().join(format!("ximonitor-registry-legacy-test-{unique}"));
            std::fs::create_dir_all(&temp_dir).expect("temp dir should exist");
            let path = temp_dir.join("server.json");
            std::fs::write(&path, "{\"nodes\":[]}").expect("empty registry should be written");

            let registry = NodeRegistry::load(&path)
                .await
                .expect("registry should load");
            let identity = NodeIdentity {
                node_id: "legacy-01".to_string(),
                node_label: "Legacy 01".to_string(),
                hostname: "legacy-01.internal".to_string(),
                os: "Ubuntu".to_string(),
                kernel_version: None,
                cpu_model: None,
                cpu_cores: 2,
                agent_version: "0.1.0".to_string(),
                boot_time: None,
                tags: Vec::new(),
            };

            let error = registry
                .authorize(&identity, "some-token")
                .await
                .expect_err("unenrolled node should be rejected");
            assert!(error.to_string().contains("not enrolled"));

            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_dir(&temp_dir);
        });
    }

    #[cfg(unix)]
    #[test]
    fn issued_registry_file_is_mode_600() {
        use std::os::unix::fs::PermissionsExt;

        let runtime = Runtime::new().expect("runtime should build");
        runtime.block_on(async {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should be monotonic enough")
                .as_nanos();
            let temp_dir =
                std::env::temp_dir().join(format!("ximonitor-registry-mode-test-{unique}"));
            std::fs::create_dir_all(&temp_dir).expect("temp dir should exist");
            let path = temp_dir.join("server.json");

            issue_node(
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

            let mode = std::fs::metadata(&path)
                .expect("metadata should exist")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);

            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_dir(&temp_dir);
        });
    }
}
