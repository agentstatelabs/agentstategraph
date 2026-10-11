//! Routes added beside the API (the ASD proxy) get the API's auth and rate
//! limit (DESIGN.md Plan E t-003). Before, the proxy was merged onto the
//! finished router, outside both. A stand-in route keeps this test free of
//! the `providers` feature; `asd_proxy.rs` runs the real proxy.

use std::net::SocketAddr;
use std::sync::Arc;

use agentstategraph::Repository;
use agentstategraph_mcp::auth::{ApiKey, TenantManager};
use agentstategraph_mcp::http;
use agentstategraph_storage::SqliteStorage;
use axum::{Router, routing::get};

fn repo() -> Arc<Repository> {
    let repo = Arc::new(Repository::new(Box::new(
        SqliteStorage::in_memory().expect("in-memory sqlite"),
    )));
    repo.init().unwrap();
    repo
}

/// A stand-in for the proxy, at its path.
fn extra() -> Router {
    Router::new().route("/api/code/{*path}", get(|| async { "proxied" }))
}

async fn serve(app: Router) -> String {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn extra_routes_need_a_key_when_auth_is_on() {
    let repo = repo();
    let mgr = TenantManager::multi_tenant(repo.clone(), None).unwrap();
    let id = mgr
        .register_key(ApiKey {
            id: String::new(),
            key: "asg_extra_routes_test_key_0000000000".into(),
            tenant_id: "default".into(),
            name: "test".into(),
            plan: "free".into(),
            enabled: true,
            created_at: chrono::Utc::now().to_rfc3339(),
            namespace_id: None,
            commit_agent_id: None,
            can_migrate: false,
            is_admin: false,
            expires_at: None,
            last_used_at: None,
        })
        .unwrap();
    let base = serve(http::build_router_with(Arc::clone(&mgr), 0, Some(extra()))).await;
    let client = reqwest::Client::new();
    let get = |key: Option<&'static str>| {
        let mut req = client.get(format!("{base}/api/code/symbols"));
        if let Some(key) = key {
            req = req.bearer_auth(key);
        }
        req.send()
    };

    assert_eq!(get(None).await.unwrap().status().as_u16(), 401);
    assert_eq!(get(Some("asg_wrong")).await.unwrap().status().as_u16(), 401);
    let ok = get(Some("asg_extra_routes_test_key_0000000000"))
        .await
        .unwrap();
    assert_eq!(ok.status().as_u16(), 200);
    assert_eq!(ok.text().await.unwrap(), "proxied");

    mgr.revoke_key(&id).unwrap();
    assert_eq!(
        get(Some("asg_extra_routes_test_key_0000000000"))
            .await
            .unwrap()
            .status()
            .as_u16(),
        401
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn extra_routes_count_against_the_rate_limit() {
    let repo = repo();
    let mgr = TenantManager::single_tenant(repo.clone());
    // One request a minute, with the floor burst of five: the sixth inside
    // a minute is refused.
    let base = serve(http::build_router_with(mgr, 1, Some(extra()))).await;
    let client = reqwest::Client::new();
    let mut statuses = Vec::new();
    for _ in 0..6 {
        statuses.push(
            client
                .get(format!("{base}/api/code/symbols"))
                .send()
                .await
                .unwrap()
                .status()
                .as_u16(),
        );
    }
    assert_eq!(statuses, vec![200, 200, 200, 200, 200, 429]);
}

#[test]
fn proxied_paths_stay_under_the_proxy_root() {
    for path in [
        "/api/code/symbols",
        "/api/code/a/b.json",
        "/api/code/a..b",
        "/api/code/.hidden",
    ] {
        assert!(!http::escapes_proxy_root(path), "{path}");
    }
    for path in [
        "/api/code/../admin",
        "/api/code/a/../../admin",
        "/api/code/./x",
        "/api/code/%2e%2E/admin",
        "/api/code/.%2e/admin",
        "/api/code/..\\admin",
    ] {
        assert!(http::escapes_proxy_root(path), "{path}");
    }
}
