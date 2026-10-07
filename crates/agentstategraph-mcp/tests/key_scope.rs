//! What a key may reach (DESIGN.md Plan E t-006): its tenant must be the one
//! this server serves, and a key bound to a namespace works only there, by
//! branch name, and can't run store-wide operations. Before, keys carried a
//! tenant and namespace that nothing read.

use std::net::SocketAddr;
use std::sync::Arc;

use agentstategraph::{CommitOptions, Repository};
use agentstategraph_core::{IntentCategory, Namespace};
use agentstategraph_mcp::auth::{ApiKey, TenantManager};
use agentstategraph_mcp::http;
use agentstategraph_storage::SqliteStorage;
use serde_json::json;

const ADMIN: &str = "asg_scope_admin_key_00000000000000000";
const DEFAULT_KEY: &str = "asg_scope_default_key_000000000000000";
const TEAM_KEY: &str = "asg_scope_team_a_key_000000000000000";
const OTHER_TENANT_KEY: &str = "asg_scope_other_tenant_key_0000000000";

fn key(raw: &str, tenant: &str, namespace: Option<&str>, admin: bool) -> ApiKey {
    ApiKey {
        id: String::new(),
        key: raw.into(),
        tenant_id: tenant.into(),
        name: "test".into(),
        plan: "free".into(),
        enabled: true,
        created_at: chrono::Utc::now().to_rfc3339(),
        namespace_id: namespace.map(str::to_string),
        commit_agent_id: None,
        can_migrate: false,
        is_admin: admin,
        expires_at: None,
        last_used_at: None,
    }
}

fn opts(desc: &str) -> CommitOptions {
    CommitOptions::new("agent/test", IntentCategory::Refine, desc)
}

/// A store with data in the default namespace and in `team-a`, served to the
/// test keys. Returns the base URL and the default namespace's head commit.
async fn serve() -> (String, String) {
    let repo = Arc::new(Repository::new(Box::new(
        SqliteStorage::in_memory().expect("in-memory sqlite"),
    )));
    repo.init().unwrap();
    repo.set_json("main", "/secret", &json!("default data"), opts("default"))
        .unwrap();
    repo.create_namespace("team-a").unwrap();
    let team = repo.fork_namespace(Namespace::new("team-a").unwrap());
    team.init().unwrap();
    team.set_json("main", "/x", &json!("team data"), opts("team"))
        .unwrap();
    let head = repo.head("main").unwrap().to_hex();

    let mgr = TenantManager::multi_tenant_for(Arc::clone(&repo), None, "acme").unwrap();
    mgr.register_key(key(ADMIN, "admin", None, true)).unwrap();
    mgr.register_key(key(DEFAULT_KEY, "acme", None, false))
        .unwrap();
    mgr.register_key(key(TEAM_KEY, "acme", Some("team-a"), false))
        .unwrap();
    mgr.register_key(key(OTHER_TENANT_KEY, "globex", None, false))
        .unwrap();

    let app = http::build_router_for_test(repo, mgr, 0);
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
    (format!("http://{addr}"), head)
}

async fn get(base: &str, key: &str, path: &str) -> (u16, String) {
    let resp = reqwest::Client::new()
        .get(format!("{base}{path}"))
        .bearer_auth(key)
        .send()
        .await
        .unwrap();
    (resp.status().as_u16(), resp.text().await.unwrap())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_key_for_another_tenant_is_refused() {
    let (base, _) = serve().await;
    let (status, body) = get(&base, OTHER_TENANT_KEY, "/api/stats/main").await;
    assert_eq!(status, 403);
    assert!(
        body.contains("'globex'") && body.contains("'acme'"),
        "{body}"
    );
    assert_eq!(get(&base, DEFAULT_KEY, "/api/stats/main").await.0, 200);
    // Admin keys serve the whole process.
    assert_eq!(get(&base, ADMIN, "/api/stats/main").await.0, 200);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_namespace_bound_key_reads_and_writes_only_its_namespace() {
    let (base, _) = serve().await;
    let (status, body) = get(&base, TEAM_KEY, "/api/state/main?path=/x").await;
    assert_eq!((status, body.as_str()), (200, r#""team data""#));
    let (status, body) = get(&base, TEAM_KEY, "/api/state/main?path=/secret").await;
    assert_ne!(status, 200, "{body}");
    assert!(!body.contains("default data"));

    let written = reqwest::Client::new()
        .post(format!("{base}/api/state/main/set"))
        .bearer_auth(TEAM_KEY)
        .json(&json!({
            "path": "/written",
            "value": "by team-a",
            "intent_category": "Refine",
            "intent_description": "scoped write",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(written.status().as_u16(), 200);
    assert_eq!(
        get(&base, TEAM_KEY, "/api/state/main?path=/written")
            .await
            .1,
        r#""by team-a""#
    );
    assert_ne!(
        get(&base, DEFAULT_KEY, "/api/state/main?path=/written")
            .await
            .0,
        200,
        "the scoped write landed in the default namespace"
    );
}

/// Commit ids resolve over every commit in the store, so they would reach
/// other namespaces' state.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_namespace_bound_key_cant_name_commits() {
    let (base, head) = serve().await;
    for spec in [head.clone(), format!("sg_{head}"), head[..12].to_string()] {
        let (status, body) = get(&base, TEAM_KEY, &format!("/api/state/{spec}?path=/secret")).await;
        assert_eq!(status, 403, "{spec}: {body}");
        assert!(body.contains("only branch names"), "{spec}: {body}");
        assert!(!body.contains("default data"));
    }
    // The same commit id works for a key without a namespace.
    let (status, body) = get(
        &base,
        DEFAULT_KEY,
        &format!("/api/state/{head}?path=/secret"),
    )
    .await;
    assert_eq!((status, body.as_str()), (200, r#""default data""#));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn store_wide_endpoints_refuse_a_namespace_bound_key() {
    let (base, _) = serve().await;
    for path in ["/api/gc/dry-run", "/api/history"] {
        let (status, body) = get(&base, TEAM_KEY, path).await;
        assert_eq!(status, 403, "{path}: {body}");
        assert!(body.contains("team-a"), "{body}");
        assert_eq!(get(&base, DEFAULT_KEY, path).await.0, 200, "{path}");
    }
    let swept = reqwest::Client::new()
        .post(format!("{base}/api/gc/sweep"))
        .bearer_auth(TEAM_KEY)
        .json(&json!({"mutate": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(swept.status().as_u16(), 403);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admins_create_namespace_bound_keys_for_this_tenant_by_default() {
    let (base, _) = serve().await;
    let client = reqwest::Client::new();
    let create = |body: serde_json::Value| {
        client
            .post(format!("{base}/api/admin/keys"))
            .bearer_auth(ADMIN)
            .json(&body)
            .send()
    };

    let created: serde_json::Value = create(json!({"name": "team bot", "namespace_id": "team-a"}))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(created["tenant_id"], "acme", "{created}");
    assert_eq!(created["namespace_id"], "team-a");
    let new_key = created["key"].as_str().unwrap();
    assert_eq!(
        get(&base, new_key, "/api/state/main?path=/x").await.1,
        r#""team data""#
    );
    assert_eq!(get(&base, new_key, "/api/history").await.0, 403);

    let bad = create(json!({"name": "bad", "namespace_id": "not a namespace!"}))
        .await
        .unwrap();
    assert_eq!(bad.status().as_u16(), 400);
}
