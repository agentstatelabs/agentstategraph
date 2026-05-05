//! Integration tests for the HTTP auth layer:
//!
//! 1. Key-bound `commit_agent_id` overrides body-supplied `agent`.
//! 2. A key without `can_migrate` cannot write to `/_meta/*` via
//!    `intent_category = "migrate"` — the server downgrades the
//!    category so the reserved-path guard rejects the commit.
//! 3. A key WITH `can_migrate` can write to `/_meta/*`.
//!
//! These tests exercise the live router end-to-end over a TCP socket so
//! the auth middleware, AuthContext injection, and handler plumbing are
//! all on the path.

use std::net::SocketAddr;
use std::sync::Arc;

use agentstategraph::Repository;
use agentstategraph_mcp::auth::{ApiKey, TenantManager};
use agentstategraph_mcp::http;
use agentstategraph_storage::SqliteStorage;

/// Boot an ASG HTTP server with auth enabled and a single seeded API
/// key, on an ephemeral port. Returns (base_url, api_key).
async fn boot_with_key(api_key: ApiKey) -> String {
    let repo = Arc::new(Repository::new(Box::new(
        SqliteStorage::in_memory().expect("in-memory sqlite"),
    )));
    repo.init().expect("init repo");

    let tenant_mgr = TenantManager::multi_tenant(repo.clone(), None);
    tenant_mgr.register_key(api_key);

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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authenticated_agent_id_overrides_body_agent() {
    let key = "asg_test_agent_binding".to_string();
    let api_key = ApiKey {
        key: key.clone(),
        tenant_id: "t1".into(),
        name: "bound".into(),
        plan: "free".into(),
        enabled: true,
        created_at: chrono::Utc::now().to_rfc3339(),
        commit_agent_id: Some("agent/real".into()),
        can_migrate: false,
        is_admin: false,
        expires_at: None,
        last_used_at: None,
    };
    let base = boot_with_key(api_key).await;
    let client = reqwest::Client::new();

    // POST /api/state/main/set with a spoofed agent field.
    let resp = client
        .post(format!("{}/api/state/main/set", base))
        .bearer_auth(&key)
        .json(&serde_json::json!({
            "path": "/foo",
            "value": {"ok": true},
            "intent_category": "explore",
            "intent_description": "test",
            "agent": "spoofed",
        }))
        .send()
        .await
        .expect("set request");
    assert!(
        resp.status().is_success(),
        "set failed: {} {}",
        resp.status(),
        resp.text().await.unwrap_or_default()
    );

    // Blame /foo; the agent_id on the commit must be "agent/real".
    let blame: serde_json::Value = client
        .get(format!("{}/api/blame/main?path=/foo", base))
        .bearer_auth(&key)
        .send()
        .await
        .expect("blame")
        .json()
        .await
        .expect("blame json");
    let agent = blame
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert_eq!(
        agent, "agent/real",
        "expected authenticated agent, got {:?}; blame={:?}",
        agent, blame
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn key_without_can_migrate_cannot_write_meta() {
    let key = "asg_test_no_migrate".to_string();
    let api_key = ApiKey {
        key: key.clone(),
        tenant_id: "t1".into(),
        name: "nomigrate".into(),
        plan: "free".into(),
        enabled: true,
        created_at: chrono::Utc::now().to_rfc3339(),
        commit_agent_id: Some("agent/bad".into()),
        can_migrate: false,
        is_admin: false,
        expires_at: None,
        last_used_at: None,
    };
    let base = boot_with_key(api_key).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{}/api/state/main/set", base))
        .bearer_auth(&key)
        .json(&serde_json::json!({
            "path": "/_meta/schema_version",
            "value": "99",
            "intent_category": "migrate",
            "intent_description": "bypass attempt",
        }))
        .send()
        .await
        .expect("set request");

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    assert!(
        status == reqwest::StatusCode::FORBIDDEN || status.is_client_error(),
        "expected 4xx (reserved-path rejection), got {} body={}",
        status,
        body
    );
    assert!(
        body.to_lowercase().contains("reserved") || body.to_lowercase().contains("_meta"),
        "expected ReservedPath-ish error, got {}",
        body
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn key_with_can_migrate_can_write_meta() {
    let key = "asg_test_can_migrate".to_string();
    let api_key = ApiKey {
        key: key.clone(),
        tenant_id: "t1".into(),
        name: "migrator".into(),
        plan: "free".into(),
        enabled: true,
        created_at: chrono::Utc::now().to_rfc3339(),
        commit_agent_id: Some("agent/migrate".into()),
        can_migrate: true,
        is_admin: false,
        expires_at: None,
        last_used_at: None,
    };
    let base = boot_with_key(api_key).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{}/api/state/main/set", base))
        .bearer_auth(&key)
        .json(&serde_json::json!({
            "path": "/_meta/schema_version",
            "value": "99",
            "intent_category": "migrate",
            "intent_description": "bump schema",
        }))
        .send()
        .await
        .expect("set request");

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    assert!(
        status.is_success(),
        "expected migrate write to succeed, got {} body={}",
        status,
        body
    );
}
