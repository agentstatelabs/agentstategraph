//! Integration tests for v3-V5: API key expiry, rotation, and entropy.

use std::net::SocketAddr;
use std::sync::Arc;

use agentstategraph::Repository;
use agentstategraph_mcp::auth::{ApiKey, TenantManager};
use agentstategraph_mcp::http;
use agentstategraph_storage::SqliteStorage;

async fn boot(tenant_mgr: Arc<TenantManager>, repo: Arc<Repository>) -> String {
    let app = http::build_router_for_test(repo, tenant_mgr, 0);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    let base = format!("http://{}", addr);
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    base
}

fn admin_key(key: &str) -> ApiKey {
    ApiKey {
        key: key.to_string(),
        tenant_id: "admin".into(),
        name: "admin".into(),
        plan: "admin".into(),
        enabled: true,
        created_at: chrono::Utc::now().to_rfc3339(),
        commit_agent_id: None,
        can_migrate: true,
        is_admin: true,
        expires_at: None,
        last_used_at: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expired_key_is_rejected() {
    // A key whose expires_at is in the past must be treated as revoked.
    let repo = Arc::new(Repository::new(Box::new(
        SqliteStorage::in_memory().expect("in-memory sqlite"),
    )));
    repo.init().unwrap();
    let mgr = TenantManager::multi_tenant(repo.clone(), None);
    mgr.register_key(admin_key("asg_admin_expiry"));

    // Seed an already-expired user key (backdated).
    let expired = ApiKey {
        key: "asg_expired_user".into(),
        tenant_id: "t1".into(),
        name: "expired".into(),
        plan: "free".into(),
        enabled: true,
        created_at: chrono::Utc::now().to_rfc3339(),
        commit_agent_id: None,
        can_migrate: false,
        is_admin: false,
        expires_at: Some(chrono::Utc::now() - chrono::Duration::minutes(1)),
        last_used_at: None,
    };
    mgr.register_key(expired);

    let base = boot(mgr, repo).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/api/state/main?path=/", base))
        .bearer_auth("asg_expired_user")
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "expired key must be rejected as 401 (reason not leaked)"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_key_with_expires_in_days_applies_expiry() {
    // POST /api/admin/keys with expires_in_days sets expires_at on the new key.
    let repo = Arc::new(Repository::new(Box::new(
        SqliteStorage::in_memory().expect("in-memory sqlite"),
    )));
    repo.init().unwrap();
    let mgr = TenantManager::multi_tenant(repo.clone(), None);
    mgr.register_key(admin_key("asg_admin_create_exp"));
    let base = boot(mgr, repo).await;
    let client = reqwest::Client::new();

    let body: serde_json::Value = client
        .post(format!("{}/api/admin/keys", base))
        .bearer_auth("asg_admin_create_exp")
        .json(&serde_json::json!({
            "tenant_id": "t1",
            "name": "short-lived",
            "expires_in_days": 30,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let exp = body
        .get("expires_at")
        .and_then(|v| v.as_str())
        .expect("expires_at string");
    let parsed: chrono::DateTime<chrono::Utc> = chrono::DateTime::parse_from_rfc3339(exp)
        .expect("valid rfc3339")
        .with_timezone(&chrono::Utc);
    let days = (parsed - chrono::Utc::now()).num_days();
    assert!(
        (28..=31).contains(&days),
        "expected ~30 days ahead, got {} days",
        days
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rotate_key_invalidates_old_and_mints_new() {
    let repo = Arc::new(Repository::new(Box::new(
        SqliteStorage::in_memory().expect("in-memory sqlite"),
    )));
    repo.init().unwrap();
    let mgr = TenantManager::multi_tenant(repo.clone(), None);
    mgr.register_key(admin_key("asg_admin_rotate"));
    // Seed a known user key we'll rotate.
    let user = ApiKey {
        key: "asg_rot_user_abcdef".into(),
        tenant_id: "t1".into(),
        name: "rotates".into(),
        plan: "free".into(),
        enabled: true,
        created_at: chrono::Utc::now().to_rfc3339(),
        commit_agent_id: Some("agent/rot".into()),
        can_migrate: true,
        is_admin: false,
        expires_at: None,
        last_used_at: None,
    };
    mgr.register_key(user);

    let base = boot(mgr, repo).await;
    let client = reqwest::Client::new();

    // Old key works first.
    let resp = client
        .get(format!("{}/api/state/main?path=/", base))
        .bearer_auth("asg_rot_user_abcdef")
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "old key should work before rotate"
    );

    // Rotate.
    let rotated: serde_json::Value = client
        .post(format!("{}/api/admin/keys/rotate", base))
        .bearer_auth("asg_admin_rotate")
        .json(&serde_json::json!({ "key_prefix": "asg_rot_user_abc" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let new_key = rotated
        .get("key")
        .and_then(|v| v.as_str())
        .expect("new key")
        .to_string();
    assert_ne!(new_key, "asg_rot_user_abcdef");
    assert_eq!(
        rotated.get("commit_agent_id").and_then(|v| v.as_str()),
        Some("agent/rot"),
        "rotated key must preserve commit_agent_id"
    );
    assert_eq!(
        rotated.get("can_migrate").and_then(|v| v.as_bool()),
        Some(true),
        "rotated key must preserve can_migrate"
    );

    // Old key now rejected.
    let resp = client
        .get(format!("{}/api/state/main?path=/", base))
        .bearer_auth("asg_rot_user_abcdef")
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "old key must be 401 after rotate"
    );

    // New key works.
    let resp = client
        .get(format!("{}/api/state/main?path=/", base))
        .bearer_auth(&new_key)
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "new key must work after rotate, got {}",
        resp.status()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn created_key_entropy_matches_asg_hex64() {
    // Key strings minted by the admin endpoint match `^asg_[0-9a-f]{64}$`.
    let repo = Arc::new(Repository::new(Box::new(
        SqliteStorage::in_memory().expect("in-memory sqlite"),
    )));
    repo.init().unwrap();
    let mgr = TenantManager::multi_tenant(repo.clone(), None);
    mgr.register_key(admin_key("asg_admin_entropy"));
    let base = boot(mgr, repo).await;
    let client = reqwest::Client::new();

    let body: serde_json::Value = client
        .post(format!("{}/api/admin/keys", base))
        .bearer_auth("asg_admin_entropy")
        .json(&serde_json::json!({
            "tenant_id": "t1",
            "name": "entropy-check",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let key = body
        .get("key")
        .and_then(|v| v.as_str())
        .expect("key string");
    assert!(key.starts_with("asg_"), "missing prefix: {}", key);
    let hex = &key[4..];
    assert_eq!(hex.len(), 64, "hex must be 64 chars, got: {}", key);
    assert!(
        hex.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
        "hex must be lowercase hex only: {}",
        key
    );
}
