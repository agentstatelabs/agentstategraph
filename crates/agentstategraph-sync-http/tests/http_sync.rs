//! Push, fetch and pull over HTTP: a router on a real socket, behind a
//! resolver that checks a bearer token, and blocking clients
//! (DESIGN.md Plan G t-006).

use std::sync::Arc;

use agentstategraph::storage::{MemoryStorage, Storage};
use agentstategraph::sync::{PullOutcome, Remote, SyncError};
use agentstategraph::{CommitOptions, Repository};
use agentstategraph_core::IntentCategory;
use agentstategraph_sync_http::{HttpRemote, Rejection, Resolved, StoreResolver, router};
use axum::http::request::Parts;
use serde_json::json;

const TOKEN: &str = "test-token-0123456789";

struct OneStore {
    repo: Arc<Repository>,
}

impl StoreResolver for OneStore {
    async fn resolve(&self, store: &str, request: &Parts) -> Result<Resolved, Rejection> {
        if store != "team" {
            return Err(Rejection::NotFound(format!("no store {store}")));
        }
        let auth = request
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok());
        if auth != Some(&format!("Bearer {TOKEN}")) {
            return Err(Rejection::Unauthorized("a bearer token, please".into()));
        }
        Ok(Resolved {
            repo: Arc::clone(&self.repo),
            pusher: "key/test".into(),
        })
    }
}

fn memory() -> Repository {
    Repository::from_shared(Arc::new(MemoryStorage::new()) as Arc<dyn Storage + Send + Sync>)
}

/// Serve `repo` on a free local port, returning its base URL. The server
/// runs until the test process ends.
fn serve(repo: Arc<Repository>) -> String {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(listener.local_addr().unwrap()).unwrap();
            axum::serve(listener, router(Arc::new(OneStore { repo })))
                .await
                .unwrap();
        });
    });
    format!("http://{}", rx.recv().unwrap())
}

fn opts(what: &str) -> CommitOptions {
    CommitOptions::new("agent/test", IntentCategory::Refine, what)
}

fn merge_opts() -> CommitOptions {
    CommitOptions::new("agent/test", IntentCategory::Merge, "pull")
}

#[test]
fn two_stores_sync_through_an_http_hub() {
    let hub = Arc::new(memory());
    let url = serve(Arc::clone(&hub));
    let remote = || HttpRemote::new(&url, "team").with_token(TOKEN);

    let a = memory();
    a.init().unwrap();
    a.set_json("main", "/x", &json!({"n": 1}), opts("x"))
        .unwrap();
    let report = a.push("hub", &remote(), "main").unwrap();
    assert_eq!(report.updated.len(), 1, "{report:?}");
    assert_eq!(hub.head("main").unwrap(), a.head("main").unwrap());
    assert_eq!(hub.push_log(1).unwrap()[0].pusher, "key/test");

    let b = memory();
    b.init().unwrap();
    let pulled = b.pull("hub", &remote(), "main", merge_opts()).unwrap();
    assert!(
        matches!(pulled.outcome, PullOutcome::Created(_)),
        "{pulled:?}"
    );
    assert_eq!(b.get_json("main", "/x/n").unwrap(), json!(1));

    // Both change it; the second push is rejected until it merges.
    a.set_json("main", "/a", &json!("a"), opts("a")).unwrap();
    a.push("hub", &remote(), "main").unwrap();
    b.set_json("main", "/b", &json!("b"), opts("b")).unwrap();
    match b.push("hub", &remote(), "main") {
        Err(SyncError::Rejected { .. }) => {}
        other => panic!("expected a rejection, got {other:?}"),
    }
    let pulled = b.pull("hub", &remote(), "main", merge_opts()).unwrap();
    assert!(
        matches!(pulled.outcome, PullOutcome::Merged(_)),
        "{pulled:?}"
    );
    b.push("hub", &remote(), "main").unwrap();
    assert_eq!(hub.get_json("main", "/a").unwrap(), json!("a"));
    assert_eq!(hub.get_json("main", "/b").unwrap(), json!("b"));

    let refs = remote().list_refs().unwrap();
    assert!(refs.iter().any(|r| r.name == "main"), "{refs:?}");
}

#[test]
fn the_hub_refuses_a_wrong_token_and_an_unknown_store() {
    let hub = Arc::new(memory());
    hub.init().unwrap();
    let url = serve(Arc::clone(&hub));

    match HttpRemote::new(&url, "team")
        .with_token("wrong")
        .list_refs()
    {
        Err(SyncError::Remote(m)) => assert!(m.starts_with("401 unauthorized"), "{m}"),
        other => panic!("expected a 401, got {other:?}"),
    }
    match HttpRemote::new(&url, "nope").with_token(TOKEN).list_refs() {
        Err(SyncError::Remote(m)) => assert!(m.starts_with("404 no_such_store"), "{m}"),
        other => panic!("expected a 404, got {other:?}"),
    }
    let a = memory();
    match a.fetch(
        "hub",
        &HttpRemote::new(&url, "team").with_token(TOKEN),
        &["absent"],
    ) {
        Err(SyncError::NoSuchRef(_)) => {}
        other => panic!("expected no such ref, got {other:?}"),
    }
}

#[test]
fn a_damaged_push_is_refused_over_http() {
    let hub = Arc::new(memory());
    let url = serve(Arc::clone(&hub));
    let remote = HttpRemote::new(&url, "team").with_token(TOKEN);
    let update = agentstategraph::sync::RefUpdate {
        namespace: agentstategraph_core::Namespace::default_ns(),
        name: "main".into(),
        old: None,
        new: agentstategraph_core::ObjectId::hash(b"nothing"),
        overrides: Vec::new(),
    };
    match remote.push(&update, b"not a bundle") {
        Err(SyncError::Remote(m)) => assert!(m.starts_with("400 "), "{m}"),
        other => panic!("expected a 400, got {other:?}"),
    }
    assert!(hub.list_branches(None).unwrap().is_empty());
}
