//! `--hub`: stores push to and pull from the server's store over
//! `/sync/v1`, behind the API's keys (DESIGN.md Plan G t-006).

use std::net::SocketAddr;
use std::sync::Arc;

use agentstategraph::sync::{BundleLimits, PullOutcome, Remote, SyncError};
use agentstategraph::{CommitOptions, Repository};
use agentstategraph_core::IntentCategory;
use agentstategraph_mcp::auth::{ApiKey, TenantManager};
use agentstategraph_mcp::{http, hub};
use agentstategraph_storage::SqliteStorage;
use agentstategraph_sync_http::HttpRemote;
use serde_json::json;

const KEY: &str = "asg_sync_hub_test_key_000000000000000";
const BOUND: &str = "asg_sync_hub_bound_key_00000000000000";

fn repo() -> Arc<Repository> {
    let repo = Arc::new(Repository::new(Box::new(
        SqliteStorage::in_memory().expect("in-memory sqlite"),
    )));
    repo.init().unwrap();
    repo
}

fn key(key: &str, namespace: Option<&str>) -> ApiKey {
    ApiKey {
        id: String::new(),
        key: key.into(),
        tenant_id: "default".into(),
        name: "sync".into(),
        plan: "free".into(),
        enabled: true,
        created_at: chrono::Utc::now().to_rfc3339(),
        namespace_id: namespace.map(Into::into),
        commit_agent_id: None,
        can_migrate: false,
        is_admin: false,
        expires_at: None,
        last_used_at: None,
    }
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

fn opts(what: &str) -> CommitOptions {
    CommitOptions::new("agent/test", IntentCategory::Refine, what)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stores_push_and_pull_through_the_hub() {
    let served = repo();
    let mgr = TenantManager::multi_tenant(served.clone(), None).unwrap();
    let key_id = mgr.register_key(key(KEY, None)).unwrap();
    mgr.register_key(key(BOUND, Some("team"))).unwrap();
    let routes = hub::router("default", BundleLimits::default());
    let base = serve(http::build_router_with(Arc::clone(&mgr), 0, Some(routes))).await;

    tokio::task::spawn_blocking(move || {
        let remote = || HttpRemote::new(&base, "default").with_token(KEY);
        let pull_opts = || CommitOptions::new("agent/test", IntentCategory::Merge, "pull");

        // The served store ran init, so its main is init's empty commit: a
        // store with work pulls (taking nothing) and then pushes over it.
        let a = repo();
        a.set_json("main", "/x", &json!(1), opts("x")).unwrap();
        match a.push("hub", &remote(), "main") {
            Err(SyncError::Rejected { .. }) => {}
            other => panic!("expected a rejection, got {other:?}"),
        }
        let pulled = a.pull("hub", &remote(), "main", pull_opts()).unwrap();
        assert_eq!(pulled.outcome, PullOutcome::UpToDate);
        a.push("hub", &remote(), "main").unwrap();
        assert_eq!(served.get_json("main", "/x").unwrap(), json!(1));
        let logged = served.push_log(1).unwrap();
        assert_eq!(logged[0].pusher, format!("key/{key_id}"));

        // Another store pulls it.
        let b = repo();
        let pulled = b.pull("hub", &remote(), "main", pull_opts()).unwrap();
        assert!(
            matches!(pulled.outcome, PullOutcome::Created(_)),
            "{pulled:?}"
        );
        assert_eq!(b.get_json("main", "/x").unwrap(), json!(1));

        match HttpRemote::new(&base, "other").with_token(KEY).list_refs() {
            Err(SyncError::Remote(m)) => assert!(m.starts_with("404"), "{m}"),
            other => panic!("expected a 404, got {other:?}"),
        }
        match HttpRemote::new(&base, "default").list_refs() {
            Err(SyncError::Remote(m)) => assert!(m.starts_with("401"), "{m}"),
            other => panic!("expected a 401, got {other:?}"),
        }
        match HttpRemote::new(&base, "default")
            .with_token(BOUND)
            .list_refs()
        {
            Err(SyncError::Remote(m)) => assert!(m.starts_with("403"), "{m}"),
            other => panic!("expected a 403, got {other:?}"),
        }
    })
    .await
    .unwrap();
}
