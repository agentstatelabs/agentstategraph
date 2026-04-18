//! Integration tests for v2-C1 admin endpoint gating.
//!
//! The admin endpoints (`/api/admin/keys*`) must be reachable only by
//! requests bearing an `is_admin=true` key. Single-tenant mode is the
//! "trusted local process" affordance and skips auth entirely.

use std::net::SocketAddr;
use std::sync::Arc;

use agentstategraph::Repository;
use agentstategraph_mcp::auth::{ApiKey, TenantManager};
use agentstategraph_mcp::http;
use agentstategraph_storage::MemoryStorage;

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

fn non_admin_key(key: &str) -> ApiKey {
    ApiKey {
        key: key.to_string(),
        tenant_id: "t1".into(),
        name: "ordinary".into(),
        plan: "free".into(),
        enabled: true,
        created_at: chrono::Utc::now().to_rfc3339(),
        commit_agent_id: None,
        can_migrate: false,
        is_admin: false,
    }
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
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_endpoint_requires_key_in_multi_tenant() {
    let repo = Arc::new(Repository::new(Box::new(MemoryStorage::new())));
    repo.init().unwrap();
    let mgr = TenantManager::multi_tenant(repo.clone(), None);
    mgr.register_key(admin_key("asg_admin_bootstrap"));
    let base = boot(mgr, repo).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/api/admin/keys", base))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "unauth admin request must be 401"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_endpoint_rejects_non_admin_key() {
    let repo = Arc::new(Repository::new(Box::new(MemoryStorage::new())));
    repo.init().unwrap();
    let mgr = TenantManager::multi_tenant(repo.clone(), None);
    mgr.register_key(admin_key("asg_admin"));
    mgr.register_key(non_admin_key("asg_user"));
    let base = boot(mgr, repo).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/api/admin/keys", base))
        .bearer_auth("asg_user")
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::FORBIDDEN,
        "non-admin key must be 403"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_endpoint_accepts_admin_key() {
    let repo = Arc::new(Repository::new(Box::new(MemoryStorage::new())));
    repo.init().unwrap();
    let mgr = TenantManager::multi_tenant(repo.clone(), None);
    mgr.register_key(admin_key("asg_admin_ok"));
    let base = boot(mgr, repo).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/api/admin/keys", base))
        .bearer_auth("asg_admin_ok")
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "admin key must succeed, got {}",
        resp.status()
    );

    // create_key via admin endpoint works, and is_admin flag is honored.
    let created: serde_json::Value = client
        .post(format!("{}/api/admin/keys", base))
        .bearer_auth("asg_admin_ok")
        .json(&serde_json::json!({
            "tenant_id": "t1",
            "name": "second-admin",
            "is_admin": true,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        created.get("is_admin").and_then(|v| v.as_bool()),
        Some(true)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn single_tenant_mode_skips_admin_auth() {
    // Single-tenant == trusted local process. Admin endpoints are
    // accessible without any key, consistent with `can_migrate` behavior.
    let repo = Arc::new(Repository::new(Box::new(MemoryStorage::new())));
    repo.init().unwrap();
    let mgr = TenantManager::single_tenant(repo.clone());
    let base = boot(mgr, repo).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/api/admin/keys", base))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "single-tenant admin must be open, got {}",
        resp.status()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn health_remains_public() {
    let repo = Arc::new(Repository::new(Box::new(MemoryStorage::new())));
    repo.init().unwrap();
    let mgr = TenantManager::multi_tenant(repo.clone(), None);
    mgr.register_key(admin_key("asg_admin_health"));
    let base = boot(mgr, repo).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/api/health", base))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "health must be public");
}
