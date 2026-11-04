//! End to end: stores sync through a hub over HTTP, with the hub on every
//! built-in backend (DESIGN.md Plan G t-009). Postgres runs when
//! `TEST_DATABASE_URL` is set, with a tenant per case, and must when
//! `ASG_REQUIRE_POSTGRES` is.
//!
//! Covered: concurrent pushes lose nothing, through fetch, merge and push
//! again; a taint made on one store holds on another once it pulls;
//! reminders and epochs go both ways; secrets and local-only branches never
//! leave; and a tampered bundle is refused whole.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;

use agentstategraph::storage::{MemoryStorage, PostgresStorage, SqliteStorage, Storage};
use agentstategraph::sync::{PushOptions, RefUpdate, Remote, SyncError};
use agentstategraph::{CommitOptions, RepoError, Repository};
use agentstategraph_core::{IntentCategory, Namespace};
use agentstategraph_reminders::{CreateReminder, ReminderManager, ReminderStatus};
use agentstategraph_sync_http::{HttpRemote, Rejection, Resolved, StoreResolver, router};
use axum::http::request::Parts;
use serde_json::json;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum Backend {
    Memory,
    Sqlite,
    Postgres,
}

/// A hub on one backend, served over HTTP.
struct Hub {
    repo: Arc<Repository>,
    url: String,
    cleanup: Option<PathBuf>,
}

impl Hub {
    fn remote(&self) -> HttpRemote {
        HttpRemote::new(&self.url, "team")
    }
}

impl Drop for Hub {
    fn drop(&mut self) {
        if let Some(path) = &self.cleanup {
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
            }
        }
    }
}

struct Serves(Arc<Repository>);

impl StoreResolver for Serves {
    async fn resolve(&self, store: &str, _request: &Parts) -> Result<Resolved, Rejection> {
        if store != "team" {
            return Err(Rejection::NotFound(store.to_string()));
        }
        Ok(Resolved {
            repo: Arc::clone(&self.0),
            pusher: "e2e".into(),
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
            axum::serve(listener, router(Arc::new(Serves(repo))))
                .await
                .unwrap();
        });
    });
    format!("http://{}", rx.recv().unwrap())
}

fn hubs(case: &str) -> Vec<(Backend, Hub)> {
    let mut all = Vec::new();
    let memory: Arc<dyn Storage + Send + Sync> = Arc::new(MemoryStorage::new());
    let repo = Arc::new(Repository::from_shared(memory));
    all.push((
        Backend::Memory,
        Hub {
            url: serve(Arc::clone(&repo)),
            repo,
            cleanup: None,
        },
    ));

    let path = std::env::temp_dir().join(format!(
        "asg-sync-e2e-{case}-{}.db",
        uuid::Uuid::new_v4().simple()
    ));
    let repo = Arc::new(Repository::new(Box::new(
        SqliteStorage::open(&path).expect("open sqlite"),
    )));
    all.push((
        Backend::Sqlite,
        Hub {
            url: serve(Arc::clone(&repo)),
            repo,
            cleanup: Some(path),
        },
    ));

    match std::env::var("TEST_DATABASE_URL") {
        Ok(url) if !url.is_empty() => {
            let tenant = format!("sync-e2e-{case}-{}", uuid::Uuid::new_v4().simple());
            let storage = PostgresStorage::connect_tenant_blocking(&url, &tenant, 8)
                .expect("connect to TEST_DATABASE_URL");
            let repo = Arc::new(Repository::new(Box::new(storage)));
            all.push((
                Backend::Postgres,
                Hub {
                    url: serve(Arc::clone(&repo)),
                    repo,
                    cleanup: None,
                },
            ));
        }
        _ => {
            let required =
                std::env::var("ASG_REQUIRE_POSTGRES").is_ok_and(|v| !v.is_empty() && v != "0");
            assert!(
                !required,
                "ASG_REQUIRE_POSTGRES is set, but TEST_DATABASE_URL isn't ({case})"
            );
            eprintln!("skip postgres for {case}: TEST_DATABASE_URL not set");
        }
    }
    all
}

fn run(case: &str, body: fn(&Hub)) {
    let mut problems = Vec::new();
    for (backend, hub) in hubs(case) {
        if catch_unwind(AssertUnwindSafe(|| body(&hub))).is_err() {
            problems.push(format!("{backend:?}: failed (see panic above)"));
        }
    }
    assert!(problems.is_empty(), "{case}: {}", problems.join("; "));
}

fn store() -> Repository {
    let repo =
        Repository::from_shared(Arc::new(MemoryStorage::new()) as Arc<dyn Storage + Send + Sync>);
    repo.init().unwrap();
    repo
}

fn opts(what: &str) -> CommitOptions {
    CommitOptions::new("agent/e2e", IntentCategory::Refine, what)
}

fn merge_opts() -> CommitOptions {
    CommitOptions::new("agent/e2e", IntentCategory::Merge, "pull")
}

/// A store with work, pushed to the empty hub, and a second store that has
/// pulled it.
fn two_stores(hub: &Hub) -> (Repository, Repository) {
    let a = store();
    a.set_json(
        "main",
        "/users/alice",
        &json!({"role": "admin"}),
        opts("alice"),
    )
    .unwrap();
    a.push("hub", &hub.remote(), "main").unwrap();
    let b = store();
    b.pull("hub", &hub.remote(), "main", merge_opts()).unwrap();
    assert_eq!(b.head("main").unwrap(), a.head("main").unwrap());
    (a, b)
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

/// Several stores push at once. Each push that loses the race is rejected,
/// and the store pulls (fetch and merge) and pushes again, until all land:
/// nothing any of them wrote is lost.
fn concurrent_pushes_lose_nothing(hub: &Hub) {
    const STORES: usize = 4;
    let (a, _) = two_stores(hub);
    drop(a);
    let url = hub.url.clone();
    let workers: Vec<_> = (0..STORES)
        .map(|i| {
            let url = url.clone();
            std::thread::spawn(move || {
                let remote = HttpRemote::new(&url, "team");
                let s = store();
                s.pull("hub", &remote, "main", merge_opts()).unwrap();
                s.set_json("main", &format!("/from/s{i}"), &json!(i), opts("mine"))
                    .unwrap();
                for _ in 0..100 {
                    match s.push("hub", &remote, "main") {
                        Ok(_) => return,
                        Err(SyncError::Rejected { .. }) => {
                            s.pull("hub", &remote, "main", merge_opts()).unwrap();
                        }
                        Err(e) => panic!("store {i}: {e}"),
                    }
                }
                panic!("store {i} never landed its push");
            })
        })
        .collect();
    for w in workers {
        w.join().expect("a store's pushes");
    }
    let from = hub.repo.get_json("main", "/from").unwrap();
    for i in 0..STORES {
        assert_eq!(from[format!("s{i}")], json!(i), "{from}");
    }
    assert_eq!(
        hub.repo.get_json("main", "/users/alice/role").unwrap(),
        json!("admin")
    );
}

/// A taint made on one store blocks a write on another once it pulls.
fn a_taint_holds_on_another_store_after_a_pull(hub: &Hub) {
    let (a, b) = two_stores(hub);
    a.taint(
        "main",
        "/users",
        agentstategraph_taint::TaintParams {
            name: "pii".into(),
            effect: agentstategraph_taint::TaintEffect::Block,
            reason: "holds pii".into(),
            severity: agentstategraph_taint::TaintSeverity::High,
            expires_at: None,
            propagate: true,
            metadata: agentstategraph_taint::TaintMetadata::new(),
            agent_id: "agent/e2e".into(),
        },
    )
    .unwrap();
    a.push("hub", &hub.remote(), "main").unwrap();

    b.pull("hub", &hub.remote(), "main", merge_opts()).unwrap();
    match b.set_json("main", "/users/bob", &json!({"role": "dev"}), opts("bob")) {
        Err(e @ RepoError::Taint { .. }) => assert_eq!(e.code(), "taint_blocked"),
        other => panic!("expected the taint to block, got {other:?}"),
    }
    b.set_json("main", "/elsewhere", &json!(1), opts("fine"))
        .unwrap();
}

/// A reminder and a sealed epoch go from one store to another, and a change
/// to the reminder comes back.
fn reminders_and_epochs_go_both_ways(hub: &Hub) {
    let (a, b) = two_stores(hub);
    let a = Arc::new(a);
    let b = Arc::new(b);
    let made = ReminderManager::new(a.clone())
        .create(CreateReminder::new(
            "rotate keys",
            "look",
            chrono::Utc::now(),
            "agent/e2e",
        ))
        .unwrap();
    a.create_epoch("q4", "the quarter", vec![]).unwrap();
    a.seal_epoch("q4", "done").unwrap();
    a.push("hub", &hub.remote(), "main").unwrap();

    b.pull("hub", &hub.remote(), "main", merge_opts()).unwrap();
    let on_b = ReminderManager::new(b.clone());
    assert_eq!(on_b.get(&made.id).unwrap().title, "rotate keys");
    assert_eq!(
        b.get_epoch("q4").unwrap().seal_hash,
        a.get_epoch("q4").unwrap().seal_hash
    );

    on_b.cancel(&made.id).unwrap();
    b.push("hub", &hub.remote(), "main").unwrap();
    a.pull("hub", &hub.remote(), "main", merge_opts()).unwrap();
    assert_eq!(
        ReminderManager::new(a.clone())
            .get(&made.id)
            .unwrap()
            .status,
        ReminderStatus::Cancelled
    );
}

/// Secrets and local-only branches never leave a store, overridden or not.
fn secrets_and_local_only_branches_never_leave(hub: &Hub) {
    let (a, _) = two_stores(hub);
    a.branch("index", "main").unwrap();
    a.set_local_only("index", true).unwrap();
    assert!(matches!(
        a.push("hub", &hub.remote(), "index"),
        Err(SyncError::LocalOnly(_))
    ));

    let before = hub.repo.head("main").unwrap();
    a.set_json(
        "main",
        "/_meta/_secret/key",
        &json!("k"),
        CommitOptions::new("agent/e2e", IntentCategory::Migrate, "secret"),
    )
    .unwrap();
    let finding = match a.push("hub", &hub.remote(), "main") {
        Err(SyncError::Blocked { findings, .. }) => findings[0].clone(),
        other => panic!("expected a blocked push, got {other:?}"),
    };
    let options = PushOptions::unfiltered().allow(finding.id, "please");
    assert!(matches!(
        a.push_with("hub", &hub.remote(), "main", &options),
        Err(SyncError::Blocked { .. })
    ));
    assert_eq!(hub.repo.head("main").unwrap(), before);
    assert!(hub.repo.head("index").is_err());
}

/// A bundle damaged on the way is refused whole: the hub's ref doesn't move
/// and nothing it holds becomes reachable.
fn a_tampered_bundle_is_refused(hub: &Hub) {
    let (a, _) = two_stores(hub);
    a.set_json("main", "/more", &json!([1, 2, 3]), opts("more"))
        .unwrap();
    let (mut bundle, _) = a.export_store(Vec::new()).unwrap();
    let at = bundle.len() / 2;
    bundle[at] ^= 0x5a;
    let before = hub.repo.head("main").unwrap();
    let update = RefUpdate {
        namespace: Namespace::default_ns(),
        name: "main".into(),
        old: Some(before),
        new: a.head("main").unwrap(),
        overrides: Vec::new(),
    };
    match hub.remote().push(&update, &bundle) {
        Err(SyncError::Remote(m)) => assert!(m.starts_with("400 "), "{m}"),
        other => panic!("expected a 400, got {other:?}"),
    }
    assert_eq!(hub.repo.head("main").unwrap(), before);

    // The real push still goes through.
    a.push("hub", &hub.remote(), "main").unwrap();
    assert_eq!(
        hub.repo.get_json("main", "/more").unwrap(),
        json!([1, 2, 3])
    );
}

#[test]
fn concurrent_pushes() {
    run("concurrent_pushes", concurrent_pushes_lose_nothing);
}

#[test]
fn taints_propagate() {
    run(
        "taints_propagate",
        a_taint_holds_on_another_store_after_a_pull,
    );
}

#[test]
fn reminders_and_epochs() {
    run("reminders_and_epochs", reminders_and_epochs_go_both_ways);
}

#[test]
fn secrets_and_local_only() {
    run(
        "secrets_and_local_only",
        secrets_and_local_only_branches_never_leave,
    );
}

#[test]
fn tampered_bundles() {
    run("tampered_bundles", a_tampered_bundle_is_refused);
}
