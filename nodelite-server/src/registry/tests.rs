use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Duration as ChronoDuration, TimeZone, Utc};
use proptest::prelude::*;
use tokio::runtime::Runtime;

use super::{
    IssueNodeRequest, MAX_NODE_TAG_BYTES, NodeRegistry, RegisteredNode, RegistryError,
    RegistryFile, TokenVerifyProbe, build_agent_server_url, build_github_release_base_url,
    default_agent_release_base_url, issue_node, release_registry_lock_with, render_install_command,
    token_is_unexpired, validate_registered_node, verify_token,
};
use nodelite_proto::NodeIdentity;

fn legacy_node(
    node_id: &str,
    node_label: &str,
    token: &str,
    token_expires_at: Option<DateTime<Utc>>,
) -> RegisteredNode {
    RegisteredNode {
        node_id: node_id.to_string(),
        node_label: node_label.to_string(),
        token_hash: String::new(),
        token_generation: 0,
        token: token.to_string(),
        tags: Vec::new(),
        created_at: Utc::now(),
        token_expires_at,
    }
}

fn identity_for(node_id: &str) -> NodeIdentity {
    NodeIdentity {
        node_id: node_id.to_string(),
        node_label: node_id.to_string(),
        hostname: format!("{node_id}.internal"),
        os: "Ubuntu".to_string(),
        kernel_version: None,
        cpu_model: None,
        cpu_cores: 2,
        agent_version: "0.1.0".to_string(),
        boot_time: None,
        tags: Vec::new(),
    }
}

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
        let temp_dir = std::env::temp_dir().join(format!("nodelite-registry-auth-test-{unique}"));
        std::fs::create_dir_all(&temp_dir).expect("temp dir should exist");
        let path = temp_dir.join("server.json");
        let mut node = legacy_node("osaka-01", "Osaka 01", "secret", None);
        node.tags = vec!["edge".to_string()];
        let file = RegistryFile {
            version: 0,
            nodes: vec![node],
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
        assert_eq!(authorized.identity.node_label, "Osaka 01");
        assert_eq!(authorized.identity.tags, vec!["edge"]);
        assert_eq!(authorized.generation, 1);

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&temp_dir);
    });
}

#[test]
fn load_hashes_legacy_plaintext_tokens_and_persists_migration() {
    let runtime = Runtime::new().expect("runtime should build");
    runtime.block_on(async {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic enough")
            .as_nanos();
        let temp_dir =
            std::env::temp_dir().join(format!("nodelite-registry-migration-test-{unique}"));
        std::fs::create_dir_all(&temp_dir).expect("temp dir should exist");
        let path = temp_dir.join("server.json");
        let file = RegistryFile {
            version: 0,
            nodes: vec![legacy_node("legacy-01", "Legacy 01", "legacy-secret", None)],
            install_sessions: Vec::new(),
        };
        std::fs::write(&path, serde_json::to_string_pretty(&file).expect("json"))
            .expect("registry should be written");

        let registry = NodeRegistry::load(&path)
            .await
            .expect("registry should load");
        let authorized = registry
            .authorize(&identity_for("legacy-01"), "legacy-secret")
            .await
            .expect("legacy token should still authorize after migration");
        assert_eq!(authorized.generation, 1);

        let stored = std::fs::read_to_string(&path).expect("registry should be readable");
        assert!(!stored.contains("legacy-secret"));
        let parsed: RegistryFile =
            serde_json::from_str(&stored).expect("stored registry should parse");
        assert_eq!(parsed.nodes.len(), 1);
        assert!(parsed.nodes[0].token.is_empty());
        assert!(parsed.nodes[0].token_hash.starts_with("$argon2id$"));
        assert!(verify_token("legacy-secret", &parsed.nodes[0].token_hash));
        assert_eq!(parsed.nodes[0].token_generation, 1);
        assert_eq!(parsed.version, 1);

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&temp_dir);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn token_verify_limiter_caps_parallel_argon2_verifies() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be monotonic enough")
        .as_nanos();
    let temp_dir = std::env::temp_dir().join(format!("nodelite-token-verify-limit-{unique}"));
    std::fs::create_dir_all(&temp_dir).expect("temp dir should exist");
    let path = temp_dir.join("server.json");

    let issued = issue_node(
        &path,
        IssueNodeRequest {
            node_id: "storm-01".to_string(),
            node_label: Some("Storm 01".to_string()),
            tags: Vec::new(),
        },
    )
    .await
    .expect("node should be issued");
    let probe = Arc::new(TokenVerifyProbe::new(Duration::from_millis(75)));
    let registry = NodeRegistry::load(&path)
        .await
        .expect("registry should load")
        .with_token_verify_limit_for_tests(1)
        .with_token_verify_probe_for_tests(Arc::clone(&probe));
    let identity = identity_for("storm-01");

    let mut handles = Vec::new();
    for _ in 0..6 {
        let registry = registry.clone();
        let identity = identity.clone();
        let token = issued.node_session_token.clone();
        handles.push(tokio::spawn(async move {
            registry.authorize(&identity, &token).await
        }));
    }

    for result in futures::future::join_all(handles).await {
        let authorized = result
            .expect("authorize task should complete")
            .expect("token should authorize");
        assert_eq!(authorized.identity.node_id, "storm-01");
    }
    assert_eq!(probe.max_active(), 1);

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&temp_dir);
}

#[test]
fn issue_node_persists_registry_and_renders_install_command() {
    let runtime = Runtime::new().expect("runtime should build");
    runtime.block_on(async {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic enough")
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!("nodelite-registry-test-{unique}"));
        std::fs::create_dir_all(&temp_dir).expect("temp dir should exist");
        let path = temp_dir.join("server.json");
        let issued = issue_node(
            &path,
            IssueNodeRequest {
                node_id: "hk-01".to_string(),
                node_label: Some("Hong Kong 01".to_string()),
                tags: vec!["edge".to_string(), "apac".to_string()],
            },
        )
        .await
        .expect("node should be issued");
        assert!(issued.created);

        let stored = std::fs::read_to_string(&path).expect("registry should be stored");
        let parsed: RegistryFile =
            serde_json::from_str(&stored).expect("stored registry should parse");
        assert_eq!(parsed.version, 1);
        assert_eq!(parsed.nodes.len(), 1);
        assert_eq!(parsed.install_sessions.len(), 1);
        assert!(parsed.nodes[0].token.is_empty());
        assert!(parsed.nodes[0].token_hash.starts_with("$argon2id$"));
        assert_ne!(parsed.nodes[0].token_hash, issued.node_session_token);
        assert_eq!(parsed.nodes[0].token_generation, 1);
        assert_eq!(parsed.install_sessions[0].token, issued.install_token);
        assert_eq!(
            parsed.install_sessions[0].node_session_token,
            issued.node_session_token
        );

        let command = render_install_command(
            "https://monitor.example.com",
            &issued.install_token,
            "https://github.com/XiNian-dada/NodeLite/releases/latest/download",
        )
        .expect("install command should render");
        assert!(command.contains("--bootstrap-url"));
        assert!(command.contains("/install-agent.sh"));
        assert!(command.contains("NODELITE_AGENT_INSTALL_TOKEN="));
        assert!(command.contains(&issued.install_token));
        assert!(!command.contains(&issued.node_session_token));

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
        let temp_dir = std::env::temp_dir().join(format!("nodelite-registry-reload-test-{unique}"));
        std::fs::create_dir_all(&temp_dir).expect("temp dir should exist");
        let path = temp_dir.join("server.json");

        let issued = issue_node(
            &path,
            IssueNodeRequest {
                node_id: "hk-01".to_string(),
                node_label: Some("Hong Kong 01".to_string()),
                tags: Vec::new(),
            },
        )
        .await
        .expect("node should be issued");
        let old_token = issued.node_session_token.clone();
        let registry = NodeRegistry::load(&path)
            .await
            .expect("registry should load");
        let identity = identity_for("hk-01");
        let old_authorized = registry
            .authorize(&identity, &old_token)
            .await
            .expect("old token should authorize before rotation");
        assert!(
            registry
                .is_token_current("hk-01", old_authorized.generation)
                .await
        );

        let rotated = issue_node(
            &path,
            IssueNodeRequest {
                node_id: "hk-01".to_string(),
                node_label: Some("Hong Kong 01".to_string()),
                tags: Vec::new(),
            },
        )
        .await
        .expect("node token should rotate");
        assert!(registry.reload().await.expect("reload should succeed"));
        assert!(
            !registry
                .is_token_current("hk-01", old_authorized.generation)
                .await
        );
        assert!(
            registry.authorize(&identity, &old_token).await.is_err(),
            "old plaintext token should no longer authorize"
        );
        let rotated_authorized = registry
            .authorize(&identity, &rotated.node_session_token)
            .await
            .expect("rotated token should authorize");
        assert!(
            registry
                .is_token_current("hk-01", rotated_authorized.generation)
                .await
        );

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&temp_dir);
    });
}

#[tokio::test]
async fn issued_tokens_default_to_thirty_day_expiry() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let temp_dir = std::env::temp_dir().join(format!("nodelite-registry-expiry-test-{unique}"));
    std::fs::create_dir_all(&temp_dir).expect("temp dir");
    let path = temp_dir.join("server.json");

    let issued = issue_node(
        &path,
        IssueNodeRequest {
            node_id: "hk-01".to_string(),
            node_label: Some("Hong Kong 01".to_string()),
            tags: Vec::new(),
        },
    )
    .await
    .expect("node should be issued");

    let expires_at = issued
        .node
        .token_expires_at
        .expect("issued token should carry expiry");
    let remaining = expires_at - Utc::now();
    assert!(
        remaining >= ChronoDuration::days(29)
            && remaining <= ChronoDuration::days(30) + ChronoDuration::minutes(1),
        "expected about 30 days of remaining validity, got {remaining:?}",
    );

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&temp_dir);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn normalize_string_list_returns_sorted_trimmed_deduped_values(
        values in proptest::collection::vec(".*", 0..32),
    ) {
        let normalized = super::normalize_string_list(values.clone());

        for value in &normalized {
            prop_assert!(!value.is_empty());
            prop_assert_eq!(value.trim(), value);
        }

        let mut sorted = normalized.clone();
        sorted.sort();
        prop_assert_eq!(normalized.as_slice(), sorted.as_slice());

        let mut deduped = normalized.clone();
        deduped.dedup();
        prop_assert_eq!(normalized.as_slice(), deduped.as_slice());

        for value in &normalized {
            prop_assert!(values.iter().any(|original| original.trim() == value));
        }
    }

    #[test]
    fn generate_token_always_returns_lowercase_hex(_case in any::<u8>()) {
        let token = super::generate_token().expect("token generation should succeed");
        prop_assert_eq!(token.len(), 64);
        prop_assert!(
            token
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        );
    }
}

#[test]
fn registry_lock_drop_cleanup_swallows_panics_and_runs_both_steps() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let cleanup_steps = Arc::new(AtomicUsize::new(0));
    let unlock_steps = Arc::clone(&cleanup_steps);
    let harden_steps = Arc::clone(&cleanup_steps);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        release_registry_lock_with(
            move || {
                unlock_steps.fetch_or(0b01, Ordering::SeqCst);
                panic!("unlock panic");
            },
            move || {
                harden_steps.fetch_or(0b10, Ordering::SeqCst);
                panic!("harden panic");
            },
        );
    }));

    assert!(
        result.is_ok(),
        "cleanup helper should swallow internal panics"
    );
    assert_eq!(cleanup_steps.load(Ordering::SeqCst), 0b11);
}

#[test]
fn install_tokens_are_one_time_use() {
    let runtime = Runtime::new().expect("runtime should build");
    runtime.block_on(async {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic enough")
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!("nodelite-install-token-test-{unique}"));
        std::fs::create_dir_all(&temp_dir).expect("temp dir should exist");
        let path = temp_dir.join("server.json");

        let issued = issue_node(
            &path,
            IssueNodeRequest {
                node_id: "hk-01".to_string(),
                node_label: Some("Hong Kong 01".to_string()),
                tags: Vec::new(),
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
        assert_eq!(consumed.node.node_id, issued.node.node_id);
        assert_eq!(consumed.node_session_token, issued.node_session_token);
        assert!(
            registry
                .consume_install_token(&issued.install_token)
                .await
                .expect("second install token lookup should succeed")
                .is_none()
        );
        let stored = std::fs::read_to_string(&path).expect("registry should be readable");
        assert!(!stored.contains(&issued.node_session_token));

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&temp_dir);
    });
}

#[test]
fn expired_tokens_are_not_current_after_handshake() {
    let runtime = Runtime::new().expect("runtime should build");
    runtime.block_on(async {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic enough")
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!("nodelite-expired-token-test-{unique}"));
        std::fs::create_dir_all(&temp_dir).expect("temp dir should exist");
        let path = temp_dir.join("server.json");
        let file = RegistryFile {
            version: 0,
            nodes: vec![legacy_node(
                "expired-01",
                "Expired 01",
                "secret",
                Some(Utc::now() - ChronoDuration::seconds(1)),
            )],
            install_sessions: Vec::new(),
        };
        std::fs::write(&path, serde_json::to_string_pretty(&file).expect("json"))
            .expect("registry should be written");
        let registry = NodeRegistry::load(&path)
            .await
            .expect("registry should load");

        let error = registry
            .authorize(&identity_for("expired-01"), "secret")
            .await
            .expect_err("expired token should not authorize");
        assert!(matches!(
            error,
            RegistryError::TokenExpired { ref node_id } if node_id == "expired-01"
        ));
        assert!(!registry.is_token_current("expired-01", 1).await);

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&temp_dir);
    });
}

#[test]
fn token_is_expired_at_exact_expiry_moment() {
    let expires_at = Utc.with_ymd_and_hms(2026, 5, 18, 12, 0, 0).unwrap();
    let entry = RegisteredNode {
        node_id: "boundary-01".to_string(),
        node_label: "Boundary 01".to_string(),
        token_hash: "hash".to_string(),
        token_generation: 1,
        token: "secret".to_string(),
        tags: Vec::new(),
        created_at: expires_at - ChronoDuration::minutes(5),
        token_expires_at: Some(expires_at),
    };

    assert!(!token_is_unexpired(&entry, expires_at));
    assert!(token_is_unexpired(
        &entry,
        expires_at - ChronoDuration::nanoseconds(1),
    ));
}

#[test]
fn unenrolled_nodes_are_rejected() {
    let runtime = Runtime::new().expect("runtime should build");
    runtime.block_on(async {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic enough")
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!("nodelite-registry-legacy-test-{unique}"));
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
        assert!(matches!(error, RegistryError::Unauthorized));

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&temp_dir);
    });
}

#[test]
fn wrong_tokens_use_the_same_auth_error() {
    let runtime = Runtime::new().expect("runtime should build");
    runtime.block_on(async {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic enough")
            .as_nanos();
        let temp_dir =
            std::env::temp_dir().join(format!("nodelite-registry-auth-error-test-{unique}"));
        std::fs::create_dir_all(&temp_dir).expect("temp dir should exist");
        let path = temp_dir.join("server.json");
        let file = RegistryFile {
            version: 0,
            nodes: vec![legacy_node("osaka-01", "Osaka 01", "secret", None)],
            install_sessions: Vec::new(),
        };
        std::fs::write(&path, serde_json::to_string_pretty(&file).expect("json"))
            .expect("registry should be written");
        let registry = NodeRegistry::load(&path)
            .await
            .expect("registry should load");
        let identity = NodeIdentity {
            node_id: "osaka-01".to_string(),
            node_label: "Osaka 01".to_string(),
            hostname: "osaka-01.internal".to_string(),
            os: "Ubuntu".to_string(),
            kernel_version: None,
            cpu_model: None,
            cpu_cores: 2,
            agent_version: "0.1.0".to_string(),
            boot_time: None,
            tags: Vec::new(),
        };

        let error = registry
            .authorize(&identity, "wrong-secret")
            .await
            .expect_err("wrong token should be rejected");
        assert!(matches!(error, RegistryError::Unauthorized));

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&temp_dir);
    });
}

#[tokio::test]
async fn refresh_token_reports_missing_nodes_with_typed_error() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let temp_dir = std::env::temp_dir().join(format!("nodelite-registry-missing-node-{unique}"));
    std::fs::create_dir_all(&temp_dir).expect("temp dir");
    let path = temp_dir.join("server.json");
    std::fs::write(&path, "{\"nodes\":[],\"install_sessions\":[]}")
        .expect("empty registry should be written");
    let registry = NodeRegistry::load(&path)
        .await
        .expect("registry should load");

    let error = registry
        .refresh_token("missing-01")
        .await
        .expect_err("missing nodes should surface a typed error");
    assert!(matches!(error, RegistryError::NodeNotFound(ref node_id) if node_id == "missing-01"));

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&temp_dir);
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
        let temp_dir = std::env::temp_dir().join(format!("nodelite-registry-mode-test-{unique}"));
        std::fs::create_dir_all(&temp_dir).expect("temp dir should exist");
        let config_dir = temp_dir.join("config");
        let path = config_dir.join("server.json");

        issue_node(
            &path,
            IssueNodeRequest {
                node_id: "hk-01".to_string(),
                node_label: Some("Hong Kong 01".to_string()),
                tags: Vec::new(),
            },
        )
        .await
        .expect("node should be issued");

        let dir_mode = std::fs::metadata(&config_dir)
            .expect("config dir should exist")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700);

        let mode = std::fs::metadata(&path)
            .expect("metadata should exist")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&config_dir);
        let _ = std::fs::remove_dir(&temp_dir);
    });
}

#[test]
fn github_release_base_url_uses_latest_download_path() {
    let release_url = build_github_release_base_url("https://github.com/XiNian-dada/NodeLite.git")
        .expect("release url should build");
    assert_eq!(
        release_url,
        "https://github.com/XiNian-dada/NodeLite/releases/latest/download"
    );
}

#[test]
fn default_release_base_url_points_at_github_latest_download() {
    let release_url = default_agent_release_base_url().expect("default release url should build");
    assert_eq!(
        release_url,
        "https://github.com/XiNian-dada/NodeLite/releases/latest/download"
    );
}

#[test]
fn validate_registered_node_rejects_oversized_tags() {
    let mut node = RegisteredNode {
        node_id: "hk-01".to_string(),
        node_label: "Hong Kong 01".to_string(),
        token_hash: "hash".to_string(),
        token_generation: 1,
        token: "secret-token".to_string(),
        tags: vec!["edge".to_string()],
        created_at: Utc::now(),
        token_expires_at: None,
    };
    node.tags = vec!["x".repeat(MAX_NODE_TAG_BYTES + 1)];

    let error = validate_registered_node(&node).expect_err("oversized tag should fail");
    assert!(error.to_string().contains("node.tags[0]"));
}

#[tokio::test]
async fn issue_node_rejects_excessive_tags() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let temp_dir = std::env::temp_dir().join(format!("nodelite-registry-tag-limit-test-{unique}"));
    std::fs::create_dir_all(&temp_dir).expect("temp dir");
    let path = temp_dir.join("server.json");

    let error = issue_node(
        &path,
        IssueNodeRequest {
            node_id: "hk-01".to_string(),
            node_label: Some("Hong Kong 01".to_string()),
            tags: (0..1000).map(|index| format!("tag-{index}")).collect(),
        },
    )
    .await
    .expect_err("too many tags should fail");

    assert!(error.to_string().contains("tags"));
    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[tokio::test]
async fn concurrent_issue_node_preserves_all_nodes() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let temp_dir = std::env::temp_dir().join(format!("nodelite-registry-concurrent-test-{unique}"));
    std::fs::create_dir_all(&temp_dir).expect("temp dir");
    let path = temp_dir.join("server.json");

    // 并发 issue 10 个不同节点,验证乐观版本提交 + 唯一 tmp 文件名能保证全部落盘。
    let mut handles = Vec::new();
    for i in 0..10 {
        let path = path.clone();
        let handle = tokio::spawn(async move {
            issue_node(
                &path,
                IssueNodeRequest {
                    node_id: format!("node-{i:02}"),
                    node_label: Some(format!("Node {i:02}")),
                    tags: Vec::new(),
                },
            )
            .await
            .expect("issue should succeed")
        });
        handles.push(handle);
    }

    let results = futures::future::join_all(handles).await;
    assert_eq!(results.len(), 10, "all tasks should complete");

    let registry = NodeRegistry::load(&path).await.expect("load");
    let stored = std::fs::read_to_string(&path).expect("registry should be readable");
    let parsed: RegistryFile = serde_json::from_str(&stored).expect("registry should parse");
    let node_ids: Vec<_> = registry
        .state
        .read()
        .await
        .entries
        .keys()
        .cloned()
        .collect();
    assert_eq!(
        node_ids.len(),
        10,
        "all 10 nodes should be present in registry"
    );
    assert_eq!(
        parsed.version, 10,
        "each successful mutation should advance version"
    );

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&temp_dir);
}
