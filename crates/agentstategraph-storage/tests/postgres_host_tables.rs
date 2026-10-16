//! A host's own tables beside the storage's, through
//! [`PostgresStorage::pool`], [`PostgresStorage::run`] and
//! [`tenant_id`](PostgresStorage::tenant_id). CTXone keeps its project registry
//! and session statistics this way, on the storage's pool rather than a second
//! one, so a hosted hub holds one set of connections with one TLS setup.
//!
//! Skipped unless `TEST_DATABASE_URL` is set; `ASG_REQUIRE_POSTGRES` turns the
//! skip into a failure.

#![cfg(feature = "postgres")]

use agentstategraph_storage::PostgresStorage;

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
    format!("host-{tag}-{}", uuid::Uuid::new_v4().simple())
}

/// Add one to this tenant's counter `key` in a host table and return it.
fn bump(storage: &PostgresStorage, key: &str) -> i64 {
    let pool = storage.pool();
    let tenant = storage.tenant_id().to_string();
    let key = key.to_string();
    PostgresStorage::run(async move {
        let client = pool.get().await.expect("a pooled connection");
        client
            .batch_execute(
                "CREATE TABLE IF NOT EXISTS host_counters (
                     tenant_id TEXT NOT NULL,
                     key TEXT NOT NULL,
                     n BIGINT NOT NULL,
                     PRIMARY KEY (tenant_id, key)
                 )",
            )
            .await
            .expect("create the host table");
        client
            .query_one(
                "INSERT INTO host_counters (tenant_id, key, n) VALUES ($1, $2, 1)
                 ON CONFLICT (tenant_id, key) DO UPDATE SET n = host_counters.n + 1
                 RETURNING n",
                &[&tenant, &key],
            )
            .await
            .expect("bump the counter")
            .get(0)
    })
}

#[test]
fn a_host_table_from_a_plain_thread_stays_in_its_tenant() {
    let Some(url) = url() else { return };
    let a = PostgresStorage::connect_tenant_blocking(&url, &tenant("a"), 4).unwrap();
    let b = PostgresStorage::connect_tenant_blocking(&url, &tenant("b"), 4).unwrap();
    assert_ne!(a.tenant_id(), b.tenant_id());
    assert_eq!(bump(&a, "k"), 1);
    assert_eq!(bump(&a, "k"), 2);
    assert_eq!(bump(&b, "k"), 1, "another tenant's rows are its own");
}

#[test]
fn run_works_inside_a_multi_threaded_runtime() {
    let Some(url) = url() else { return };
    let storage = PostgresStorage::connect_tenant_blocking(&url, &tenant("multi"), 4).unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    assert_eq!(rt.block_on(async { bump(&storage, "k") }), 1);
}

#[test]
fn run_works_inside_a_single_threaded_runtime() {
    let Some(url) = url() else { return };
    let storage = PostgresStorage::connect_tenant_blocking(&url, &tenant("single"), 4).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    assert_eq!(rt.block_on(async { bump(&storage, "k") }), 1);
}

/// The connections `run` opens are driven by the storage's runtime, so they
/// keep working after the runtime that called it is gone.
#[test]
fn connections_outlive_the_callers_runtime() {
    let Some(url) = url() else { return };
    let storage = PostgresStorage::connect_tenant_blocking(&url, &tenant("outlive"), 2).unwrap();
    {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            // Two at once, so the pool opens a second connection here.
            let (x, y) = tokio::join!(async { bump(&storage, "x") }, async { bump(&storage, "y") });
            assert_eq!((x, y), (1, 1));
        });
    }
    for round in 2..5 {
        assert_eq!(bump(&storage, "x"), round);
        assert_eq!(bump(&storage, "y"), round);
    }
}
