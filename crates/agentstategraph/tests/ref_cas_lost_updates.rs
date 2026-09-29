//! Ref writes must not lose each other's commits.
//!
//! Every read-modify-write of a ref (`set`, `delete`, `commit_speculation`,
//! `merge`, taint intents) used to read the head, build a commit on it, and
//! then move the ref unconditionally. Any commit another writer landed in
//! between was discarded, and that writer had already been told `Ok`.
//!
//! Field impact: an AgentStateDeveloper store lost 1,684 of 12,085 ledger
//! entries to four or five processes appending at once plus `asd index`
//! committing speculations in between. These tests reproduce both shapes
//! against `SqliteStorage`, which is what those processes share.

use std::sync::{Arc, Barrier};

use agentstategraph::{CommitOptions, Repository};
use agentstategraph_core::IntentCategory;
use agentstategraph_storage::SqliteStorage;
use serde_json::json;

fn scratch_path(tag: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "asg-ref-cas-{}-{}",
        tag,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    p
}

fn open(db: &std::path::Path) -> Repository {
    Repository::new(Box::new(SqliteStorage::open(db).unwrap()))
}

fn opts(desc: &str) -> CommitOptions {
    CommitOptions::new("test", IntentCategory::Refine, desc)
}

/// Separate `Repository` instances on one SQLite file stand in for separate
/// processes. Every acknowledged write must be in the final state.
#[test]
fn concurrent_writers_on_one_store_lose_nothing() {
    const WRITERS: usize = 4;
    const PER_WRITER: usize = 20;

    let db = scratch_path("concurrent");
    open(&db).init().unwrap();

    let barrier = Arc::new(Barrier::new(WRITERS));
    let handles: Vec<_> = (0..WRITERS)
        .map(|w| {
            let db = db.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let repo = open(&db);
                barrier.wait();
                for i in 0..PER_WRITER {
                    // Shared parent maps, so writers contend on the same nodes.
                    repo.set_json(
                        "main",
                        &format!("/ledger/sym_{}/w{w}_{i}", i % 5),
                        &json!({ "writer": w, "i": i }),
                        opts("append"),
                    )
                    .unwrap();
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }

    let repo = open(&db);
    let mut missing = Vec::new();
    for w in 0..WRITERS {
        for i in 0..PER_WRITER {
            let path = format!("/ledger/sym_{}/w{w}_{i}", i % 5);
            if repo.get_json("main", &path).is_err() {
                missing.push(path);
            }
        }
    }
    assert!(
        missing.is_empty(),
        "{} of {} acknowledged writes lost: {:?}",
        missing.len(),
        WRITERS * PER_WRITER,
        missing
    );
    let _ = std::fs::remove_file(&db);
}

/// A write that lands while a speculation is open must survive the
/// speculation's commit — `asd index` opens one per pass. Both sides create
/// the same new parent (`/asd/v1`), which the merge used to treat as a
/// conflict and resolve by dropping one side's subtree.
#[test]
fn write_during_open_speculation_survives_its_commit() {
    let db = scratch_path("spec-preserve");
    let repo = open(&db);
    repo.init().unwrap();

    let spec = repo.speculate("main", Some("index".into())).unwrap();
    repo.spec_set_json(spec, "/asd/v1/index/by-qname", &json!({ "pkg.f": "sym_f" }))
        .unwrap();

    repo.set_json(
        "main",
        "/asd/v1/ledger/sym_f/led_1",
        &json!("invariant"),
        opts("ledger"),
    )
    .unwrap();

    repo.commit_speculation(
        spec,
        CommitOptions::new("t", IntentCategory::Checkpoint, "index"),
    )
    .unwrap();

    assert_eq!(
        repo.get_json("main", "/asd/v1/ledger/sym_f/led_1").unwrap(),
        json!("invariant"),
        "write made while the speculation was open was reverted by its commit"
    );
    assert_eq!(
        repo.get_json("main", "/asd/v1/index/by-qname/pkg.f")
            .unwrap(),
        json!("sym_f")
    );
    let _ = std::fs::remove_file(&db);
}

/// Where the speculation and a concurrent write change the same leaf, the
/// speculation still wins — the behaviour callers relied on for derived
/// data like an index. Only writes it did not touch are now preserved.
#[test]
fn speculation_still_wins_a_leaf_both_sides_changed() {
    let db = scratch_path("spec-wins");
    let repo = open(&db);
    repo.init().unwrap();
    repo.set_json("main", "/index/version", &json!(1), opts("seed"))
        .unwrap();

    let spec = repo.speculate("main", None).unwrap();
    repo.spec_set_json(spec, "/index/version", &json!(2))
        .unwrap();

    repo.set_json("main", "/index/version", &json!(3), opts("concurrent"))
        .unwrap();
    repo.set_json("main", "/other", &json!("kept"), opts("concurrent"))
        .unwrap();

    repo.commit_speculation(spec, opts("commit")).unwrap();

    assert_eq!(repo.get_json("main", "/index/version").unwrap(), json!(2));
    assert_eq!(repo.get_json("main", "/other").unwrap(), json!("kept"));
    let _ = std::fs::remove_file(&db);
}

/// A speculation that did not touch `/_meta` must not be rejected because a
/// concurrent commit did: the reserved-path gate judges the speculation's
/// own changes, not its difference from a moved head.
#[test]
fn meta_gate_judges_the_speculations_own_changes() {
    let db = scratch_path("spec-meta");
    let repo = open(&db);
    repo.init().unwrap();

    let spec = repo.speculate("main", None).unwrap();
    repo.spec_set_json(spec, "/index/a", &json!(1)).unwrap();

    // A Migrate commit is allowed to touch /_meta.
    repo.set_json(
        "main",
        "/_meta/migrated",
        &json!(true),
        CommitOptions::new("t", IntentCategory::Migrate, "migrate"),
    )
    .unwrap();

    repo.commit_speculation(spec, opts("commit")).unwrap();
    assert_eq!(repo.get_json("main", "/index/a").unwrap(), json!(1));
    let _ = std::fs::remove_file(&db);
}

/// `set_json_cas` keeps its contract: a stale expected head is a
/// `WriteConflict`, not an overwrite.
#[test]
fn set_json_cas_still_rejects_a_stale_head() {
    let db = scratch_path("cas-stale");
    let repo = open(&db);
    repo.init().unwrap();
    let stale = repo.head("main").unwrap();
    repo.set_json("main", "/a", &json!(1), opts("move"))
        .unwrap();

    let err = repo
        .set_json_cas("main", stale, "/b", &json!(2), opts("stale"))
        .unwrap_err();
    assert!(matches!(err, agentstategraph::RepoError::WriteConflict));
    assert_eq!(repo.get_json("main", "/a").unwrap(), json!(1));
    let _ = std::fs::remove_file(&db);
}
