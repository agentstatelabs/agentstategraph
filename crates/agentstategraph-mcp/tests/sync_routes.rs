//! The HTTP API's sync routes push to and pull from a `--hub` server by a
//! remote's name, the token coming from the environment variable the remote
//! names (DESIGN.md Plan G t-008). Needs the `sync-client` feature.

#![cfg(feature = "sync-client")]

use std::net::SocketAddr;
use std::sync::Arc;

use agentstategraph::sync::BundleLimits;
use agentstategraph::{CommitOptions, Repository};
use agentstategraph_core::IntentCategory;
use agentstategraph_mcp::auth::{ApiKey, TenantManager};
use agentstategraph_mcp::{http, hub};
use agentstategraph_storage::SqliteStorage;
use serde_json::{Value, json};

const KEY: &str = "asg_sync_routes_test_key_00000000000000";
const TOKEN_ENV: &str = "ASG_SYNC_ROUTES_TEST_TOKEN";

fn repo() -> Arc<Repository> {
    let repo = Arc::new(Repository::new(Box::new(
        SqliteStorage::in_memory().expect("in-memory sqlite"),
    )));
    repo.init().unwrap();
    repo
}

async fn serve(app: axum::Router) -> String {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_api_syncs_with_a_hub_by_remote_name() {
    // The hub, behind a key.
    let central = repo();
    let mgr = TenantManager::multi_tenant(central.clone(), None).unwrap();
    mgr.register_key(ApiKey {
        id: String::new(),
        key: KEY.into(),
        tenant_id: "default".into(),
        name: "sync".into(),
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
    let routes = hub::router("default", BundleLimits::default());
    let hub_url = serve(http::build_router_with(mgr, 0, Some(routes))).await;

    // A server whose store syncs with it.
    let local = repo();
    local
        .set_json(
            "main",
            "/x",
            &json!(1),
            CommitOptions::new("agent/test", IntentCategory::Refine, "x"),
        )
        .unwrap();
    let api = serve(http::build_router_with(
        TenantManager::single_tenant(local.clone()),
        0,
        None,
    ))
    .await;
    // SAFETY: only this test reads or writes the variable.
    unsafe { std::env::set_var(TOKEN_ENV, KEY) };

    let client = reqwest::Client::new();
    let post = |path: &str, body: Value| client.post(format!("{api}{path}")).json(&body).send();

    let added = post(
        "/api/admin/remotes",
        json!({"name": "hub", "url": hub_url, "token_env": TOKEN_ENV}),
    )
    .await
    .unwrap();
    assert_eq!(added.status().as_u16(), 200);

    // The hub's main is init's empty commit: nothing to take, then a push
    // replaces it.
    let pulled: Value = post("/api/sync/pull", json!({"remote": "hub"}))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(pulled["outcome"], "up_to_date", "{pulled}");
    let pushed = post("/api/sync/push", json!({"remote": "hub"}))
        .await
        .unwrap();
    assert_eq!(pushed.status().as_u16(), 200);
    let pushed: Value = pushed.json().await.unwrap();
    assert_eq!(pushed["updated"][0]["name"], "main", "{pushed}");
    assert_eq!(central.get_json("main", "/x").unwrap(), json!(1));

    let remotes: Value = client
        .get(format!("{api}/api/remotes"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        remotes[0]["refs"]["main"],
        local.head("main").unwrap().to_string()
    );

    let missing = post("/api/sync/fetch", json!({"remote": "nope"}))
        .await
        .unwrap();
    assert_eq!(missing.status().as_u16(), 404);
    let body: Value = missing.json().await.unwrap();
    assert_eq!(body["code"], "no_such_remote");

    // A store with history of its own joins the hub's only when asked to.
    let other = repo();
    other
        .set_json(
            "main",
            "/y",
            &json!(2),
            CommitOptions::new("agent/test", IntentCategory::Refine, "y"),
        )
        .unwrap();
    let other_api = serve(http::build_router_with(
        TenantManager::single_tenant(other.clone()),
        0,
        None,
    ))
    .await;
    let post_other =
        |path: &str, body: Value| client.post(format!("{other_api}{path}")).json(&body).send();
    let added = post_other(
        "/api/admin/remotes",
        json!({"name": "hub", "url": hub_url, "token_env": TOKEN_ENV}),
    )
    .await
    .unwrap();
    assert_eq!(added.status().as_u16(), 200);
    let refused = post_other("/api/sync/pull", json!({"remote": "hub"}))
        .await
        .unwrap();
    assert_ne!(refused.status().as_u16(), 200);
    let joined: Value = post_other(
        "/api/sync/pull",
        json!({"remote": "hub", "allow_unrelated": true}),
    )
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(joined["outcome"], "merged", "{joined}");
    assert_eq!(other.get_json("main", "/x").unwrap(), json!(1));
    assert_eq!(other.get_json("main", "/y").unwrap(), json!(2));
}
