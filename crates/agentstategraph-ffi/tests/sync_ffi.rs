//! The advanced ABI's remote.* and sync.* calls push to and pull from a hub
//! over HTTP (DESIGN.md Plan G t-008).

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::sync::Arc;

use agentstategraph::Repository;
use agentstategraph_ffi::*;
use agentstategraph_sync_http::{router, Rejection, Resolved, StoreResolver};
use axum::http::request::Parts;
use serde_json::{json, Value};

struct OneStore(Arc<Repository>);

impl StoreResolver for OneStore {
    async fn resolve(&self, store: &str, _request: &Parts) -> Result<Resolved, Rejection> {
        if store != "team" {
            return Err(Rejection::NotFound(store.to_string()));
        }
        Ok(Resolved {
            repo: Arc::clone(&self.0),
            pusher: "test".into(),
        })
    }
}

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
            axum::serve(listener, router(Arc::new(OneStore(repo))))
                .await
                .unwrap();
        });
    });
    format!("http://{}", rx.recv().unwrap())
}

fn call(repo: *mut SgRepo, operation: &str, request: Value) -> Value {
    let op = CString::new(operation).unwrap();
    let body = CString::new(request.to_string()).unwrap();
    let out: *mut c_char = agentstategraph_repository_call(repo, op.as_ptr(), body.as_ptr());
    assert!(!out.is_null(), "{operation}");
    let text = unsafe { CStr::from_ptr(out).to_string_lossy().into_owned() };
    agentstategraph_free_string(out);
    serde_json::from_str(&text).unwrap()
}

#[test]
fn stores_push_and_pull_through_a_hub_by_remote_name() {
    let hub = Arc::new(Repository::from_shared(Arc::new(
        agentstategraph::storage::SqliteStorage::in_memory().unwrap(),
    )));
    let url = serve(Arc::clone(&hub));

    let a = agentstategraph_new_memory();
    let b = agentstategraph_new_memory();
    for repo in [a, b] {
        let added = call(
            repo,
            "remote.add",
            json!({"name": "hub", "url": url, "store": "team"}),
        );
        assert_eq!(added["added"], true, "{added}");
    }

    let head = call(a, "head", json!({}))["head"]
        .as_str()
        .unwrap()
        .to_string();
    let set = call(
        a,
        "set_cas",
        json!({"expected_head": head, "path": "/x", "value": 1, "description": "x"}),
    );
    assert!(set.get("commit").is_some(), "{set}");

    let pushed = call(a, "sync.push", json!({"remote": "hub"}));
    assert_eq!(pushed["updated"][0]["name"], "main", "{pushed}");

    let pulled = call(b, "sync.pull", json!({"remote": "hub"}));
    assert_eq!(pulled["outcome"], "created", "{pulled}");
    assert_eq!(
        call(b, "head", json!({}))["head"],
        call(a, "head", json!({}))["head"]
    );

    let listed = call(b, "remote.list", json!({}));
    assert_eq!(listed[0]["name"], "hub");
    assert!(listed[0]["refs"]["main"].is_string(), "{listed}");

    let missing = call(a, "sync.fetch", json!({"remote": "nope"}));
    assert!(
        missing["error"]
            .as_str()
            .unwrap()
            .contains("no remote is named nope"),
        "{missing}"
    );
    assert_eq!(
        call(a, "remote.remove", json!({"name": "hub"}))["removed"],
        true
    );

    agentstategraph_free(a);
    agentstategraph_free(b);
}
