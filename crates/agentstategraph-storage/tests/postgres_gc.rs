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

/// `url` for connections that refuse to wait for a lock: a statement that
/// would wait more than a second fails instead. A writer here touches no row a
/// sweep deletes, so it has nothing to wait for. That makes "it never waits
/// for the sweep" checkable without timing a write, which a busy machine can
/// slow by seconds without any waiting (CI saw 2.4 s and 3.2 s).
fn refusing_lock_waits(url: &str) -> String {
    if url.starts_with("postgres://") || url.starts_with("postgresql://") {
        let sep = if url.contains('?') { '&' } else { '?' };
        format!("{url}{sep}options=-c%20lock_timeout%3D1s")
    } else {
        format!("{url} options='-c lock_timeout=1s'")
    }
}

/// A writer that refuses to wait for a lock (see [`refusing_lock_waits`]). An
/// ordinary connection brings the schema up to date first: on a fresh
/// database another test may be creating it, and waiting for that is fine.
fn connect_refusing_lock_waits(url: &str, tenant: &str) -> PostgresStorage {
    drop(connect(url, tenant));
    connect(&refusing_lock_waits(url), tenant)
}

/// A commit of a one-key map, `v` = `value`, after `parent`.
fn commit_once(
    s: &PostgresStorage,
    value: String,
    parent: Option<ObjectId>,
) -> (ObjectId, Vec<ObjectId>) {
    let leaf = s.put_object(&Object::string(value.clone())).unwrap();
    let root = s
        .put_object(&Object::map([("v".to_string(), leaf)].into()))
        .unwrap();
    let c = CommitBuilder::new(
        root,
        "agent/writer",
        Authority::simple("test"),
        Intent::new(IntentCategory::Refine, value),
    )
    .parents(parent.into_iter().collect())
    .build();
    s.put_commit(&c).unwrap();
    s.set_ref(&Namespace::default_ns(), "main", c.id).unwrap();
    (c.id, vec![leaf, root])
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

/// What the writers in the tests below rely on: a connection from
/// [`refusing_lock_waits`] fails a write that has to wait for a lock.
#[test]
fn a_writer_refusing_lock_waits_fails_instead_of_waiting() {
    let Some(url) = url() else { return };
    let t = tenant("lock-timeout");
    let s = connect_refusing_lock_waits(&url, &t);
    let (c, _) = commit_once(&s, "first".to_string(), None);

    // Another connection holds the ref's row until the write returns (or for
    // 10 s, should the write wait after all). The storage's errors don't say
    // why the database refused, so the proof is that the write fails while
    // the row is held and goes through once it isn't.
    let rt = tokio::runtime::Runtime::new().unwrap();
    let holder = rt.block_on(async {
        let (client, conn) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
            .await
            .expect("admin connect");
        tokio::spawn(conn);
        client
            .batch_execute(&format!(
                "BEGIN; SELECT 1 FROM refs WHERE tenant_id = '{t}' FOR UPDATE"
            ))
            .await
            .expect("hold the ref");
        client
    });
    let (returned, wait) = std::sync::mpsc::channel::<()>();
    let handle = rt.handle().clone();
    let release = std::thread::spawn(move || {
        let _ = wait.recv_timeout(Duration::from_secs(10));
        handle.block_on(holder.batch_execute("ROLLBACK")).unwrap();
    });

    let ns = Namespace::default_ns();
    let result = s.set_ref(&ns, "main", c);
    let _ = returned.send(());
    release.join().unwrap();
    result.expect_err("the write waited for the lock instead of failing");
    s.set_ref(&ns, "main", c)
        .expect("with the row free, the same write goes through");
}

/// A writer commits throughout a sweep that is deleting old garbage. Nothing
/// it wrote may go, and it never waits for the sweep: its connections refuse
/// to wait for a lock, and it keeps finishing writes while the sweep runs.
/// How long a write takes is not asserted (see [`refusing_lock_waits`]).
#[test]
fn a_sweep_spares_writes_made_while_it_runs() {
    let Some(url) = url() else { return };
    let t = tenant("concurrent");
    let s = connect(&url, &t);
    admin(
        &url,
        &format!(
            "INSERT INTO objects (tenant_id, id, data, written_at)
             SELECT '{t}', sha256(i::TEXT::BYTEA), '{{\"Atom\": {{\"String\": \"old\"}}}}', '-infinity'
             FROM generate_series(1, 20000) i"
        ),
    );

    let sweeping = Arc::new(AtomicBool::new(true));
    let (first_write, wrote_once) = std::sync::mpsc::channel();
    let writer = {
        let (s, sweeping) = (connect_refusing_lock_waits(&url, &t), Arc::clone(&sweeping));
        std::thread::spawn(move || {
            let (mut head, mut written, mut finished) = (None, Vec::new(), Vec::new());
            let mut i = 0;
            while sweeping.load(Ordering::SeqCst) || i < 20 {
                let (c, objects) = commit_once(&s, format!("write {i}"), head);
                finished.push(Instant::now());
                if i == 0 {
                    first_write.send(()).unwrap();
                }
                head = Some(c);
                written.extend(objects);
                i += 1;
            }
            (written, finished)
        })
    };

    // Sweep once the writer is up and writing.
    wrote_once.recv().expect("the writer's first write failed");
    let started = Instant::now();
    let sweep = s.history_gc_sweep(&[], 500).unwrap();
    let swept = Instant::now();
    sweeping.store(false, Ordering::SeqCst);
    let (written, finished) = writer
        .join()
        .expect("a write failed: a lock timeout means it waited on the sweep");

    assert_eq!(sweep.objects_deleted, 20000, "the old garbage went");
    for id in &written {
        assert!(s.has_object(id).unwrap(), "the sweep took a write");
    }
    let during = finished
        .iter()
        .filter(|&&at| at > started && at < swept)
        .count();
    assert!(
        during > 0,
        "no write finished during the {:?} sweep",
        swept - started
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

    // Writers carry on while the lock is held: a writer that refuses to wait
    // for a lock still commits.
    let writer = connect_refusing_lock_waits(&url, &t);
    commit_once(&writer, "during".to_string(), None);

    // The holder sweeps under its lock; released, the lock is free again.
    first.history_gc_sweep(&[], 100).unwrap();
    first.history_gc_unlock_writers(true).unwrap();
    first.history_gc_unlock_writers(true).unwrap();
    second.history_gc_lock_writers().unwrap();
    second.history_gc_unlock_writers(false).unwrap();
    other.history_gc_unlock_writers(false).unwrap();
}
