//! GC on Postgres (DESIGN.md Plan D t-009): what the conformance suite can't
//! show. Writers don't wait for a sweep here; a grace period on each object's
//! write time keeps their work safe instead. Each sweep stays in its tenant,
//! and a tenant runs one sweep at a time.
//!
//! Skipped unless `TEST_DATABASE_URL` is set; `ASG_REQUIRE_POSTGRES` turns the
//! skip into a failure.

#![cfg(feature = "postgres")]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use agentstategraph_core::{
    Authority, CommitBuilder, Intent, IntentCategory, Namespace, Object, ObjectId,
};
use agentstategraph_storage::{CommitStore, ObjectStore, PostgresStorage, RefStore};

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
    format!("gc-{tag}-{}", uuid::Uuid::new_v4().simple())
}

fn connect(url: &str, tenant: &str) -> PostgresStorage {
    PostgresStorage::connect_tenant_blocking(url, tenant, 4).unwrap()
}

/// Run SQL against `url` directly, outside the storage layer.
fn admin(url: &str, sql: &str) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let (client, conn) = tokio_postgres::connect(url, tokio_postgres::NoTls)
            .await
            .expect("admin connect");
        tokio::spawn(conn);
        client.batch_execute(sql).await.expect("admin sql");
    });
}

/// Make every object of `tenant` look written an hour ago.
fn age_objects(url: &str, tenant: &str) {
    admin(
        url,
        &format!(
            "UPDATE objects SET written_at = now() - interval '1 hour' WHERE tenant_id = '{tenant}'"
        ),
    );
}

fn written_at(url: &str, tenant: &str, id: &ObjectId) -> String {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let (client, conn) = tokio_postgres::connect(url, tokio_postgres::NoTls)
            .await
            .expect("admin connect");
        tokio::spawn(conn);
        client
            .query_one(
                "SELECT written_at::TEXT FROM objects WHERE tenant_id = $1 AND id = $2",
                &[&tenant, &id.as_bytes().as_slice()],
            )
            .await
            .unwrap()
            .get(0)
    })
}

#[test]
fn the_grace_period_spares_recent_writes() {
    let Some(url) = url() else { return };
    let t = tenant("grace");
    let s = connect(&url, &t);
    let garbage = s.put_object(&Object::string("unreferenced")).unwrap();

    let sweep = s.history_gc_sweep(&[], 100).unwrap();
    assert_eq!(sweep.objects_deleted, 0, "a just-written object is spared");
    assert!(s.has_object(&garbage).unwrap());

    age_objects(&url, &t);
    assert_eq!(s.history_gc_sweep(&[], 100).unwrap().objects_deleted, 1);
    assert!(!s.has_object(&garbage).unwrap());
}

#[test]
fn storing_an_object_again_refreshes_it() {
    let Some(url) = url() else { return };
    let t = tenant("freshen");
    let s = connect(&url, &t);
    let one = Object::string("stored again");
    let (two, three) = (Object::string("batched again"), Object::string("batched"));
    let one_id = s.put_object(&one).unwrap();
    let ids = s
        .batch_put_objects(&[two.clone(), two.clone(), three.clone()])
        .unwrap();
    assert_eq!(
        ids,
        vec![two.id(), two.id(), three.id()],
        "one id per object given"
    );

    // Within a minute, storing again changes nothing.
    let first = written_at(&url, &t, &one_id);
    s.put_object(&one).unwrap();
    assert_eq!(written_at(&url, &t, &one_id), first);

    // An hour old and unreferenced, they would go; storing them again (as a
    // writer whose new tree dedups onto them does) keeps them.
    age_objects(&url, &t);
    s.put_object(&one).unwrap();
    s.batch_put_objects(&[two.clone(), three.clone(), two.clone()])
        .unwrap();
    assert_eq!(s.history_gc_sweep(&[], 100).unwrap().objects_deleted, 0);
    for id in [one_id, two.id(), three.id()] {
        assert!(s.has_object(&id).unwrap());
    }
}

/// A writer commits throughout a sweep that is deleting old garbage. Nothing
/// it wrote may go, and it never waits for the sweep.
#[test]
fn a_sweep_spares_writes_made_while_it_runs() {
    let Some(url) = url() else { return };
    let t = tenant("concurrent");
    let s = Arc::new(connect(&url, &t));
    admin(
        &url,
        &format!(
            "INSERT INTO objects (tenant_id, id, data, written_at)
             SELECT '{t}', sha256(i::TEXT::BYTEA), '{{\"Atom\": {{\"String\": \"old\"}}}}', '-infinity'
             FROM generate_series(1, 20000) i"
        ),
    );

    let sweeping = Arc::new(AtomicBool::new(true));
    let writer = {
        let (s, sweeping) = (connect(&url, &t), Arc::clone(&sweeping));
        std::thread::spawn(move || {
            let ns = Namespace::default_ns();
            let mut head: Option<ObjectId> = None;
            let mut written = Vec::new();
            let mut slowest = Duration::ZERO;
            let mut i = 0;
            while sweeping.load(Ordering::SeqCst) || i < 20 {
                let started = Instant::now();
                let leaf = s.put_object(&Object::string(format!("write {i}"))).unwrap();
                let root = s
                    .put_object(&Object::map([("v".to_string(), leaf)].into()))
                    .unwrap();
                let c = CommitBuilder::new(
                    root,
                    "agent/writer",
                    Authority::simple("test"),
                    Intent::new(IntentCategory::Refine, format!("write {i}")),
                )
                .parents(head.into_iter().collect())
                .build();
                s.put_commit(&c).unwrap();
                s.set_ref(&ns, "main", c.id).unwrap();
                slowest = slowest.max(started.elapsed());
                head = Some(c.id);
                written.extend([leaf, root]);
                i += 1;
            }
            (written, slowest)
        })
    };

    let sweep = s.history_gc_sweep(&[], 500).unwrap();
    sweeping.store(false, Ordering::SeqCst);
    let (written, slowest) = writer.join().unwrap();

    assert_eq!(sweep.objects_deleted, 20000, "the old garbage went");
    for id in &written {
        assert!(s.has_object(id).unwrap(), "the sweep took a write");
    }
    assert!(
        slowest < Duration::from_secs(2),
        "a write waited {slowest:?} on the sweep"
    );
}

#[test]
fn a_sweep_stays_in_its_tenant() {
    let Some(url) = url() else { return };
    let (a, b) = (tenant("a"), tenant("b"));
    let sa = connect(&url, &a).with_gc_grace(Duration::ZERO);
    let sb = connect(&url, &b).with_gc_grace(Duration::ZERO);
    // The same content in both tenants is two rows.
    let same = Object::string("in both tenants");
    sa.put_object(&same).unwrap();
    sb.put_object(&same).unwrap();
    let only_b = sb.put_object(&Object::string("only in b")).unwrap();

    assert_eq!(sa.history_gc_sweep(&[], 100).unwrap().objects_deleted, 1);
    assert!(!sa.has_object(&same.id()).unwrap());
    assert!(
        sb.has_object(&same.id()).unwrap(),
        "a sweep took another tenant's object"
    );
    assert!(sb.has_object(&only_b).unwrap());
}

#[test]
fn one_sweep_per_tenant_at_a_time() {
    let Some(url) = url() else { return };
    let t = tenant("lock");
    let (first, second) = (connect(&url, &t), connect(&url, &t));
    let other = connect(&url, &tenant("other"));

    first.history_gc_lock_writers().unwrap();
    let refused = second.history_gc_lock_writers().unwrap_err().to_string();
    assert!(refused.contains("another sweep"), "{refused}");
    assert!(second.history_gc_sweep(&[], 100).is_err());
    assert!(
        first.history_gc_lock_writers().is_err(),
        "a handle can't take its own lock twice"
    );
    other.history_gc_lock_writers().unwrap();

    // Writers carry on while the lock is held.
    let started = Instant::now();
    let root = second.put_object(&Object::string("during")).unwrap();
    let c = CommitBuilder::new(
        root,
        "agent/writer",
        Authority::simple("test"),
        Intent::new(IntentCategory::Refine, "during"),
    )
    .build();
    second.put_commit(&c).unwrap();
    second
        .set_ref(&Namespace::default_ns(), "main", c.id)
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(2));

    // The holder sweeps under its lock; released, the lock is free again.
    first.history_gc_sweep(&[], 100).unwrap();
    first.history_gc_unlock_writers(true).unwrap();
    first.history_gc_unlock_writers(true).unwrap();
    second.history_gc_lock_writers().unwrap();
    second.history_gc_unlock_writers(false).unwrap();
    other.history_gc_unlock_writers(false).unwrap();
}
