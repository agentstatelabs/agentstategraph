//! How API keys are kept (DESIGN.md Plan E t-002): the keys file holds
//! hashes, never keys; it is replaced atomically and owner-only; a file that
//! can't be read stops the server instead of being overwritten; an old
//! plaintext file is hashed when loaded; and admin calls name a key by id,
//! never by prefix.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use agentstategraph::Repository;
use agentstategraph_mcp::auth::{ApiKey, KeyRef, TenantManager};
use agentstategraph_mcp::http;
use agentstategraph_storage::SqliteStorage;

fn scratch_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("asg-keys-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn repo() -> Arc<Repository> {
    let repo = Arc::new(Repository::new(Box::new(
        SqliteStorage::in_memory().expect("in-memory sqlite"),
    )));
    repo.init().unwrap();
    repo
}

fn load(path: &Path) -> Arc<TenantManager> {
    TenantManager::multi_tenant(repo(), Some(path.to_str().unwrap())).expect("load keys file")
}

fn key(raw: &str, admin: bool) -> ApiKey {
    ApiKey {
        id: String::new(),
        key: raw.into(),
        tenant_id: "t1".into(),
        name: "test key".into(),
        plan: "free".into(),
        enabled: true,
        created_at: chrono::Utc::now().to_rfc3339(),
        namespace_id: None,
        commit_agent_id: None,
        can_migrate: false,
        is_admin: admin,
        expires_at: None,
        last_used_at: None,
    }
}

fn sha256_hex(raw: &str) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(raw.as_bytes()))
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// Nothing but the keys file in its directory: no temp file left behind.
fn only_the_keys_file(dir: &Path) {
    let names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["keys.json"], "stray files: {names:?}");
}

async fn serve(mgr: Arc<TenantManager>) -> String {
    let app = http::build_router_for_test(repo(), mgr, 0);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    format!("http://{addr}")
}

#[test]
fn the_keys_file_holds_hashes_not_keys() {
    let dir = scratch_dir("hashes");
    let path = dir.join("keys.json");
    let mgr = load(&path);
    let created = mgr.create_key("t1", "ci", "free").unwrap();
    let registered = mgr
        .register_key(key("asg_registered_by_an_operator_0001", false))
        .unwrap();

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains(&created.key), "a raw key reached the file");
    assert!(!text.contains("asg_registered_by_an_operator_0001"));
    assert!(text.contains(&sha256_hex(&created.key)));
    let file: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(file["version"], 2);
    let ids: Vec<&str> = file["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&created.id.as_str()) && ids.contains(&registered.as_str()));
    #[cfg(unix)]
    assert_eq!(mode(&path), 0o600, "the keys file is owner-only");
    only_the_keys_file(&dir);

    // A fresh start authenticates the same keys.
    let reloaded = load(&path);
    assert_eq!(reloaded.resolve_tenant(&created.key).as_deref(), Some("t1"));
    assert!(
        reloaded
            .resolve_tenant("asg_registered_by_an_operator_0001")
            .is_some()
    );
    assert_eq!(
        reloaded.key_id(KeyRef::Key(&created.key)),
        Some(created.id.clone())
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_plaintext_file_is_hashed_when_loaded() {
    let dir = scratch_dir("legacy");
    let path = dir.join("keys.json");
    // The version 1 format: a bare array with raw keys.
    let legacy = serde_json::json!([
        {"key": "asg_legacy_admin_key_00000000000000", "tenant_id": "admin", "name": "admin",
         "plan": "admin", "enabled": true, "created_at": "2026-01-01T00:00:00Z", "is_admin": true},
        {"key": "asg_legacy_user_key_000000000000000", "tenant_id": "t1", "name": "user",
         "plan": "free", "enabled": true, "created_at": "2026-01-02T00:00:00Z",
         "commit_agent_id": "agent/legacy"}
    ]);
    std::fs::write(&path, serde_json::to_string_pretty(&legacy).unwrap()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    }

    let mgr = load(&path);
    assert!(mgr.is_admin(Some("asg_legacy_admin_key_00000000000000")));
    assert_eq!(
        mgr.get_agent_id("asg_legacy_user_key_000000000000000")
            .as_deref(),
        Some("agent/legacy")
    );
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("asg_legacy_admin_key"), "{text}");
    assert!(!text.contains("asg_legacy_user_key"), "{text}");
    #[cfg(unix)]
    assert_eq!(mode(&path), 0o600);
    only_the_keys_file(&dir);

    // Ids assigned in the migration stay put.
    let id = mgr.key_id(KeyRef::Key("asg_legacy_user_key_000000000000000"));
    let reloaded = load(&path);
    assert_eq!(
        reloaded.key_id(KeyRef::Key("asg_legacy_user_key_000000000000000")),
        id
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn an_unreadable_file_is_an_error_and_is_left_alone() {
    let dir = scratch_dir("corrupt");
    let path = dir.join("keys.json");
    for contents in [
        "{ not json",
        "",
        r#"[{"key": "asg_x"}]"#,
        r#"{"version": 3, "keys": []}"#,
    ] {
        std::fs::write(&path, contents).unwrap();
        let err = TenantManager::multi_tenant(repo(), Some(path.to_str().unwrap()))
            .err()
            .unwrap_or_else(|| panic!("{contents:?} loaded"));
        assert!(err.to_string().contains("keys.json"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), contents);
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The data loss this fixes: a keys file that failed to parse was ignored,
/// and with a bootstrap admin key the server then rewrote it holding only
/// that key.
#[test]
fn the_server_wont_start_over_an_unreadable_keys_file() {
    let dir = scratch_dir("binary");
    let path = dir.join("keys.json");
    let contents = r#"[{"key": "asg_operator_key", "tenant_id": "t1", "name": "n""#;
    std::fs::write(&path, contents).unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_agentstategraph-mcp"))
        .args(["--http", "--storage", "memory", "--port", "0"])
        .args(["--keys-file", path.to_str().unwrap()])
        .args([
            "--initial-admin-key",
            "asg_bootstrap_admin_key_long_enough_000000000",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "the server started");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("keys.json"), "{stderr}");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        contents,
        "the keys file was rewritten"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_calls_name_a_key_by_id_not_prefix() {
    let dir = scratch_dir("by-id");
    let path = dir.join("keys.json");
    let mgr = load(&path);
    mgr.register_key(key("asg_admin_for_revoke_tests_00000000", true))
        .unwrap();
    let one = mgr
        .register_key(key("asg_shared_prefix_key_one_0000000000", false))
        .unwrap();
    let two = mgr
        .register_key(key("asg_shared_prefix_key_two_0000000000", false))
        .unwrap();
    let base = serve(Arc::clone(&mgr)).await;
    let client = reqwest::Client::new();
    let admin = |path: &str, body: serde_json::Value| {
        client
            .post(format!("{base}/api/admin/keys/{path}"))
            .bearer_auth("asg_admin_for_revoke_tests_00000000")
            .json(&body)
            .send()
    };

    let refused = admin("revoke", serde_json::json!({"key_prefix": "asg_shared"}))
        .await
        .unwrap();
    assert_eq!(refused.status().as_u16(), 400);
    assert!(refused.text().await.unwrap().contains("key_id"));
    assert_eq!(
        admin("revoke", serde_json::json!({"key_id": "key_missing"}))
            .await
            .unwrap()
            .status()
            .as_u16(),
        404
    );

    let revoked = admin("revoke", serde_json::json!({"key_id": two}))
        .await
        .unwrap();
    assert_eq!(revoked.status().as_u16(), 200);
    assert!(
        mgr.resolve_tenant("asg_shared_prefix_key_one_0000000000")
            .is_some()
    );
    assert!(
        mgr.resolve_tenant("asg_shared_prefix_key_two_0000000000")
            .is_none()
    );

    // The full key works too, for an admin who holds it rather than its id.
    let rotated: serde_json::Value = admin(
        "rotate",
        serde_json::json!({"key": "asg_shared_prefix_key_one_0000000000"}),
    )
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let new_key = rotated["key"].as_str().unwrap();
    assert_ne!(rotated["id"].as_str().unwrap(), one);
    assert!(
        mgr.resolve_tenant("asg_shared_prefix_key_one_0000000000")
            .is_none()
    );
    assert!(mgr.resolve_tenant(new_key).is_some());

    // The listing shows ids and previews, never keys or their hashes.
    let listed = client
        .get(format!("{base}/api/admin/keys"))
        .bearer_auth("asg_admin_for_revoke_tests_00000000")
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(listed.contains(&two) && listed.contains("asg_shar...0000"));
    assert!(!listed.contains("asg_shared_prefix_key_one_0000000000"));
    assert!(!listed.contains(new_key));
    assert!(!listed.contains(&sha256_hex(new_key)));

    // All of it survives a restart.
    let reloaded = load(&path);
    assert!(
        reloaded
            .resolve_tenant("asg_shared_prefix_key_two_0000000000")
            .is_none()
    );
    assert!(
        reloaded
            .resolve_tenant("asg_shared_prefix_key_one_0000000000")
            .is_none()
    );
    assert!(reloaded.resolve_tenant(new_key).is_some());
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A change the keys file can't record doesn't happen: a revoke that only
/// reached memory would come back after a restart.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_change_that_cant_be_saved_doesnt_happen() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch_dir("readonly");
    let path = dir.join("keys.json");
    let mgr = load(&path);
    mgr.register_key(key("asg_admin_for_readonly_tests_000000", true))
        .unwrap();
    let user = mgr
        .register_key(key("asg_user_for_readonly_tests_0000000", false))
        .unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();

    assert!(mgr.revoke_key(&user).is_err());
    assert!(
        mgr.resolve_tenant("asg_user_for_readonly_tests_0000000")
            .is_some(),
        "an unsaved revoke took effect"
    );
    assert!(mgr.create_key("t1", "n", "free").is_err());

    let base = serve(Arc::clone(&mgr)).await;
    let created = reqwest::Client::new()
        .post(format!("{base}/api/admin/keys"))
        .bearer_auth("asg_admin_for_readonly_tests_000000")
        .json(&serde_json::json!({"tenant_id": "t1", "name": "n"}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status().as_u16(), 500);
    assert_eq!(mgr.list_keys().len(), 2, "an unsaved key was added");

    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}
