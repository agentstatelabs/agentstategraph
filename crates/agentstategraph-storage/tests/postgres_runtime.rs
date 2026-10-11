//! `PostgresStorage` from every kind of caller (DESIGN.md Plan D t-002).
//!
//! The storage traits are synchronous, and embedders call them from plain
//! threads, from inside `#[tokio::main]`, and from single-threaded runtimes
//! (MCP stdio servers run on one). Before t-002 only a multi-threaded runtime
//! worked: with no runtime, `Handle::current()` panicked; on a single-threaded
//! runtime, `block_in_place` panicked; and a connection opened on a runtime
//! that was later dropped stayed in the pool with nothing driving it.
//!
//! Skipped unless `TEST_DATABASE_URL` is set; `ASG_REQUIRE_POSTGRES` turns the
//! skip into a failure.

#![cfg(feature = "postgres")]

use agentstategraph_core::{Namespace, Object};
use agentstategraph_storage::{ObjectStore, PostgresStorage, RefStore};

fn url() -> Option<String> {
    match std::env::var("TEST_DATABASE_URL") {
        Ok(u) if !u.is_empty() => Some(u),
        _ => {
            let required =
                std::env::var("ASG_REQUIRE_POSTGRES").is_ok_and(|v| !v.is_empty() && v != "0");
            assert!(
                !required,
                "ASG_REQUIRE_POSTGRES is set, but TEST_DATABASE_URL isn't"
            );
            eprintln!("skip: TEST_DATABASE_URL not set");
            None
        }
    }
}

fn tenant(tag: &str) -> String {
    format!("runtime-{tag}-{}", uuid::Uuid::new_v4().simple())
}

/// Write, read back, and move a ref: enough to need a live connection.
fn round_trip(storage: &PostgresStorage, tag: &str) {
    let obj = Object::string(tag);
    let id = storage.put_object(&obj).unwrap();
    assert_eq!(storage.get_object(&id).unwrap(), Some(obj));
    let ns = Namespace::default_ns();
    storage.set_ref(&ns, tag, id).unwrap();
    assert_eq!(storage.get_ref(&ns, tag).unwrap(), Some(id));
}

#[test]
fn plain_threads_need_no_runtime() {
    let Some(url) = url() else { return };
    let storage = PostgresStorage::connect_tenant_blocking(&url, &tenant("plain"), 4).unwrap();
    round_trip(&storage, "plain");
    std::thread::scope(|s| {
        for i in 0..4 {
            let storage = &storage;
            s.spawn(move || round_trip(storage, &format!("plain-{i}")));
        }
    });
}

/// Sync calls straight from async code, as an embedding server does.
#[tokio::test(flavor = "multi_thread")]
async fn inside_a_multi_threaded_runtime() {
    let Some(url) = url() else { return };
    let storage = PostgresStorage::connect_tenant(&url, &tenant("multi"))
        .await
        .unwrap();
    round_trip(&storage, "multi");
    let blocking = PostgresStorage::connect_tenant_blocking(&url, &tenant("multi-b"), 4).unwrap();
    round_trip(&blocking, "multi-b");
}

/// What an MCP stdio server does: it runs on a single-threaded runtime.
#[tokio::test(flavor = "current_thread")]
async fn inside_a_single_threaded_runtime() {
    let Some(url) = url() else { return };
    let storage = PostgresStorage::connect_tenant(&url, &tenant("single"))
        .await
        .unwrap();
    round_trip(&storage, "single");
    let blocking = PostgresStorage::connect_tenant_blocking(&url, &tenant("single-b"), 4).unwrap();
    round_trip(&blocking, "single-b");
    drop(storage);
    drop(blocking);
}

/// What the server binary used to do: connect on a throwaway runtime, drop
/// it, then use the storage.
#[test]
fn the_connecting_runtime_can_go_away() {
    let Some(url) = url() else { return };
    let storage = {
        let connecting = tokio::runtime::Runtime::new().unwrap();
        connecting
            .block_on(PostgresStorage::connect_tenant(&url, &tenant("dropped")))
            .unwrap()
    };
    round_trip(&storage, "dropped");
}
