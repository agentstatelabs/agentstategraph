//! Many tenants on one connection pool, opened with
//! [`PostgresStorage::for_tenant`] the way a multi-tenant host (a CTXone Hub
//! serving many users) opens them: each tenant sees only its own data, a sweep
//! stays in its tenant, opening a tenant skips schema setup, and the process
//! holds no more connections than the one pool allows, however many tenants
//! it opens.
//!
//! Skipped unless `TEST_DATABASE_URL` is set; `ASG_REQUIRE_POSTGRES` turns the
//! skip into a failure.

#![cfg(feature = "postgres")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use agentstategraph_core::{
    Authority, Commit, CommitBuilder, Intent, IntentCategory, Namespace, Object, ObjectId,
};
use agentstategraph_storage::{CommitStore, ObjectStore, PostgresStorage, RefStore, StorageError};

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
    format!("ft-{tag}-{}", uuid::Uuid::new_v4().simple())
}

fn connect(url: &str, tenant: &str, pool_size: usize) -> PostgresStorage {
    PostgresStorage::connect_tenant_blocking(url, tenant, pool_size).unwrap()
}

/// `url` with a connection option added: `url_value` (URL-encoded) for the URL
/// form, `kv_value` for the key/value form.
fn with_option(url: &str, key: &str, url_value: &str, kv_value: &str) -> String {
    if url.starts_with("postgres://") || url.starts_with("postgresql://") {
        let sep = if url.contains('?') { '&' } else { '?' };
        format!("{url}{sep}{key}={url_value}")
    } else {
        format!("{url} {key}={kv_value}")
    }
}

/// `url` for connections whose statements fail rather than wait more than a
/// second for a lock.
fn refusing_lock_waits(url: &str) -> String {
    with_option(
        url,
        "options",
        "-c%20lock_timeout%3D1s",
        "'-c lock_timeout=1s'",
    )
}

/// The advisory lock schema setup runs under (`SCHEMA_LOCK_KEY` in
/// postgres.rs). The test that holds it checks a connect waits for it, so a
/// change of key fails that test rather than letting it pass unexamined.
const SCHEMA_LOCK_KEY: i64 = 0x0061_7367_2d73_6368;

/// A connection of the test's own, outside the storage and its pool.
struct Admin {
    rt: tokio::runtime::Runtime,
    client: tokio_postgres::Client,
}

impl Admin {
    fn connect(url: &str) -> Self {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let client = rt.block_on(async {
            let (client, conn) = tokio_postgres::connect(url, tokio_postgres::NoTls)
                .await
                .expect("admin connect");
            tokio::spawn(conn);
            client
        });
        Self { rt, client }
    }

    fn execute(&self, sql: &str) {
        self.rt
            .block_on(self.client.batch_execute(sql))
            .expect("admin sql");
    }

    /// Connections open on the server under `application_name`.
    fn connections_named(&self, application_name: &str) -> i64 {
        self.rt
            .block_on(self.client.query_one(
                "SELECT count(*) FROM pg_stat_activity WHERE application_name = $1",
                &[&application_name],
            ))
            .expect("count connections")
            .get(0)
    }
}

/// A commit of `root`, after `parent`.
fn commit(root: ObjectId, parent: Option<ObjectId>) -> Commit {
    CommitBuilder::new(
        root,
        "agent/test",
        Authority::simple("test"),
        Intent::new(IntentCategory::Refine, "a commit"),
    )
    .parents(parent.into_iter().collect())
    .build()
}

fn leaves(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(p, v)| (p.to_string(), v.to_string()))
        .collect()
}

/// Two tenants opened from one host storage use the same names for
/// everything (namespace, ref, indexed ref), and still each see only their own.
#[test]
fn tenants_on_one_pool_see_only_their_own_data() {
    let Some(url) = url() else { return };
    let host = connect(&url, &tenant("host"), 4);
    let a = host.for_tenant(&tenant("a")).unwrap();
    let b = host.for_tenant(&tenant("b")).unwrap();
    assert_eq!(a.pool_max_size(), host.pool_max_size());
    let ns = Namespace::default_ns();

    // A new tenant has its default namespace, and nothing in it.
    assert_eq!(b.list_namespaces().unwrap(), vec![ns.clone()]);
    assert_eq!(b.get_ref(&ns, "main").unwrap(), None);

    // Objects. The same content in both tenants is two rows.
    let same = Object::string("in both tenants");
    a.put_object(&same).unwrap();
    b.put_object(&same).unwrap();
    let only_a = a.put_object(&Object::string("only in a")).unwrap();
    let only_b = b.put_object(&Object::string("only in b")).unwrap();
    assert!(a.get_object(&only_a).unwrap().is_some());
    assert!(b.get_object(&only_a).unwrap().is_none());
    assert!(!b.has_object(&only_a).unwrap());
    assert!(!a.has_object(&only_b).unwrap());
    assert!(!host.has_object(&only_a).unwrap());

    // Commits and the refs to them. B's commit names A's as its parent: a
    // history walk in B must stop there, not cross into A.
    let ca = commit(only_a, None);
    a.put_commit(&ca).unwrap();
    a.set_ref(&ns, "main", ca.id).unwrap();
    let cb = commit(only_b, Some(ca.id));
    b.put_commit(&cb).unwrap();
    b.set_ref(&ns, "main", cb.id).unwrap();
    assert_eq!(a.get_ref(&ns, "main").unwrap(), Some(ca.id));
    assert_eq!(b.get_ref(&ns, "main").unwrap(), Some(cb.id));
    assert!(b.get_commit(&ca.id).unwrap().is_none());
    assert!(a.get_commit(&cb.id).unwrap().is_none());
    assert!(b.list_commits(&ca.id, 10).unwrap().is_empty());
    let walked: Vec<ObjectId> = b
        .list_commits(&cb.id, 10)
        .unwrap()
        .iter()
        .map(|c| c.id)
        .collect();
    assert_eq!(walked, vec![cb.id], "B's history walked into A's commit");
    let targets = |s: &PostgresStorage| -> Vec<ObjectId> {
        s.list_refs(&ns, "")
            .unwrap()
            .into_iter()
            .map(|(_, target)| target)
            .collect()
    };
    assert_eq!(targets(&a), vec![ca.id]);
    assert_eq!(targets(&b), vec![cb.id]);
    assert!(targets(&host).is_empty());

    // Namespaces.
    let extra = Namespace::new("ft-extra").unwrap();
    a.create_namespace(&extra).unwrap();
    assert!(a.list_namespaces().unwrap().contains(&extra));
    assert!(!b.list_namespaces().unwrap().contains(&extra));
    assert!(matches!(
        b.get_ref(&extra, "main"),
        Err(StorageError::NamespaceNotFound(_))
    ));

    // Search rows, for the same (namespace, ref) in each.
    a.leaf_index_build("default", "main", &leaves(&[("/k", "alpha secret")]))
        .unwrap();
    assert!(!b.leaf_index_is_built("default", "main").unwrap());
    assert_eq!(
        b.leaf_index_search("default", "main", "alpha", 10).unwrap(),
        None
    );
    b.leaf_index_build("default", "main", &leaves(&[("/k", "beta")]))
        .unwrap();
    assert_eq!(
        b.leaf_index_search("default", "main", "alpha", 10).unwrap(),
        Some(Vec::new()),
        "B's search found A's row"
    );
    assert_eq!(
        a.leaf_index_search("default", "main", "alpha", 10).unwrap(),
        Some(leaves(&[("/k", "alpha secret")]))
    );
    assert_eq!(
        a.leaf_index_search("default", "main", "beta", 10).unwrap(),
        Some(Vec::new()),
        "A's search found B's row"
    );
}

/// A sweep in one tenant leaves another's objects alone, each tenant has its
/// own sweep lock, and a tenant takes its GC grace period from the host.
#[test]
fn a_sweep_on_a_shared_pool_stays_in_its_tenant() {
    let Some(url) = url() else { return };
    let host = connect(&url, &tenant("host"), 4).with_gc_grace(Duration::ZERO);
    let ta = tenant("a");
    let a = host.for_tenant(&ta).unwrap();
    let b = host.for_tenant(&tenant("b")).unwrap();
    let same = Object::string("in both tenants");
    a.put_object(&same).unwrap();
    b.put_object(&same).unwrap();
    let only_b = b.put_object(&Object::string("only in b")).unwrap();

    // With the host's zero grace, unreachable objects go at once (the default
    // grace would spare all of them).
    assert_eq!(a.history_gc_sweep(&[], 100).unwrap().objects_deleted, 1);
    assert!(!a.has_object(&same.id()).unwrap());
    assert!(
        b.has_object(&same.id()).unwrap(),
        "a sweep took another tenant's object"
    );
    assert!(b.has_object(&only_b).unwrap());

    // A tenant's sweep lock holds off a second sweep of that tenant, through
    // any handle, but not another tenant's.
    a.history_gc_lock_writers().unwrap();
    b.history_gc_lock_writers().unwrap();
    let a_again = host.for_tenant(&ta).unwrap();
    let refused = a_again.history_gc_lock_writers().unwrap_err().to_string();
    assert!(refused.contains("another sweep"), "{refused}");
    a.history_gc_unlock_writers(false).unwrap();
    b.history_gc_unlock_writers(false).unwrap();
    a_again.history_gc_lock_writers().unwrap();
    a_again.history_gc_unlock_writers(false).unwrap();
}

/// Schema setup runs when a storage connects, under a lock every process
/// shares. Opening a tenant doesn't run it, so it neither waits for that lock
/// nor takes it.
#[test]
fn opening_a_tenant_skips_schema_setup() {
    let Some(url) = url() else { return };
    // An ordinary connection brings the schema up to date first: on a fresh
    // database another test may be creating it, and waiting for that is fine.
    drop(connect(&url, &tenant("warm"), 1));
    let refusing = refusing_lock_waits(&url);
    let host = connect(&refusing, &tenant("host"), 2);

    let admin = Admin::connect(&url);
    admin.execute(&format!("SELECT pg_advisory_lock({SCHEMA_LOCK_KEY})"));
    let connected = PostgresStorage::connect_tenant_blocking(&refusing, &tenant("blocked"), 1);
    let opened = host.for_tenant(&tenant("opened"));
    admin.execute(&format!("SELECT pg_advisory_unlock({SCHEMA_LOCK_KEY})"));

    let refused = connected
        .err()
        .expect("connecting waits for schema setup's lock")
        .to_string();
    assert!(refused.contains("lock timeout"), "{refused}");
    let opened = opened.expect("opening a tenant doesn't wait for schema setup");
    assert_eq!(
        opened.get_ref(&Namespace::default_ns(), "main").unwrap(),
        None
    );
}

/// Every tenant opened from one host shares its pool: two dozen tenants busy
/// at once hold no more server connections than the pool's three.
#[test]
fn many_tenants_hold_no_more_connections_than_the_pool() {
    const POOL: usize = 3;
    const TENANTS: usize = 24;
    let Some(url) = url() else { return };
    // Tag the pool's connections so the server can count them.
    let app = format!("asg-ft-{}", uuid::Uuid::new_v4().simple());
    let host = connect(
        &with_option(&url, "application_name", &app, &app),
        &tenant("host"),
        POOL,
    );
    let tenants: Vec<PostgresStorage> = (0..TENANTS)
        .map(|i| host.for_tenant(&tenant(&format!("n{i}"))).unwrap())
        .collect();

    let done = AtomicBool::new(false);
    let peak = std::thread::scope(|scope| {
        // Count the server's connections for this pool while the tenants work.
        let sampler = scope.spawn(|| {
            let admin = Admin::connect(&url);
            let mut peak = 0;
            while !done.load(Ordering::Relaxed) {
                peak = peak.max(admin.connections_named(&app));
                std::thread::sleep(Duration::from_millis(5));
            }
            peak.max(admin.connections_named(&app))
        });
        let workers: Vec<_> = tenants
            .iter()
            .map(|s| {
                scope.spawn(move || {
                    let ns = Namespace::default_ns();
                    for round in 0..5 {
                        let value = format!("{} round {round}", s.tenant_id());
                        let id = s.put_object(&Object::string(value)).unwrap();
                        s.set_ref(&ns, "main", id).unwrap();
                        assert_eq!(s.get_ref(&ns, "main").unwrap(), Some(id));
                    }
                })
            })
            .collect();
        // Stop the sampler before reporting a failed worker, or it never ends.
        let results: Vec<_> = workers.into_iter().map(|w| w.join()).collect();
        done.store(true, Ordering::Relaxed);
        let peak = sampler.join().unwrap();
        for result in results {
            if let Err(panic) = result {
                std::panic::resume_unwind(panic);
            }
        }
        peak
    });

    assert!(peak >= 1, "the sampler never saw the pool's connections");
    assert!(
        peak <= POOL as i64,
        "{TENANTS} tenants held {peak} connections; the pool allows {POOL}"
    );
}
