//! A GC sweep must never take state that live work still needs: an open
//! speculation's objects (in this process or another), a branch's fork point,
//! or another namespace's sealed epoch. Each test failed against v1.2.5 by
//! losing data while every call returned Ok (data-loss audit, 2026-10).

use agentstategraph::{CommitOptions, Repository, RetentionPolicy};
use agentstategraph_core::{IntentCategory, Namespace};
use agentstategraph_storage::SqliteStorage;
use serde_json::json;

fn scratch(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "asg-gc-live-{tag}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn open(db: &std::path::Path) -> Repository {
    Repository::new(Box::new(SqliteStorage::open(db).unwrap()))
}

fn opts(d: &str) -> CommitOptions {
    CommitOptions::new("gc-test", IntentCategory::Refine, d)
}

/// Keep only ref tips — what a sweep leaves once a fork point or a
/// speculation's base is older than the retention window.
fn sweep(repo: &Repository) {
    repo.extract_history(10_000).unwrap();
    let r = repo
        .gc_sweep(
            RetentionPolicy {
                keep_recent: 1,
                checkpoint_every: 0,
                keep_milestones: false,
            },
            true,
            false,
        )
        .unwrap();
    assert_eq!(r["mutated"], json!(true), "sweep did not run: {r}");
}

#[test]
fn merge_after_a_sweep_keeps_both_sides() {
    let db = scratch("merge");
    let repo = open(&db);
    repo.init().unwrap();
    repo.set_json("main", "/base", &json!("fork point"), opts("base"))
        .unwrap();
    repo.branch("feature", "main").unwrap();
    repo.set_json("feature", "/f", &json!("feature work"), opts("f"))
        .unwrap();
    repo.set_json("main", "/m", &json!("main work"), opts("m"))
        .unwrap();

    sweep(&repo);
    repo.merge("feature", "main", opts("merge")).unwrap();

    assert_eq!(repo.get_json("main", "/m").unwrap(), json!("main work"));
    assert_eq!(repo.get_json("main", "/f").unwrap(), json!("feature work"));
}

#[test]
fn speculation_open_across_a_sweep_commits_its_writes() {
    let db = scratch("spec-moved");
    let repo = open(&db);
    repo.init().unwrap();
    let spec = repo.speculate("main", Some("index".into())).unwrap();
    repo.spec_set_json(spec, "/spec_only", &json!("speculation 7f3a"))
        .unwrap();
    repo.set_json("main", "/concurrent", &json!("other writer"), opts("w"))
        .unwrap();

    sweep(&repo);
    repo.commit_speculation(spec, opts("commit")).unwrap();

    assert_eq!(
        repo.get_json("main", "/spec_only").unwrap(),
        json!("speculation 7f3a")
    );
    assert_eq!(
        repo.get_json("main", "/concurrent").unwrap(),
        json!("other writer")
    );
}

#[test]
fn speculation_open_across_a_sweep_lands_a_readable_tree() {
    let db = scratch("spec-still");
    let repo = open(&db);
    repo.init().unwrap();
    let spec = repo.speculate("main", None).unwrap();
    repo.spec_set_json(spec, "/spec_only", &json!("speculation 9c1d"))
        .unwrap();

    sweep(&repo);
    let id = repo.commit_speculation(spec, opts("commit")).unwrap();

    assert_eq!(repo.first_missing_object(&id).unwrap(), None);
    assert_eq!(
        repo.get_json("main", "/spec_only").unwrap(),
        json!("speculation 9c1d")
    );
}

/// The realistic case: `asd index` holds a speculation in one process while
/// `asd gc` sweeps from another. Two connections stand in for the processes.
#[test]
fn speculation_in_another_connection_survives_a_sweep() {
    let db = scratch("spec-xproc");
    let indexer = open(&db);
    indexer.init().unwrap();
    let spec = indexer.speculate("main", Some("index".into())).unwrap();
    indexer
        .spec_set_json(spec, "/index", &json!({"pkg.f": "sym_f"}))
        .unwrap();

    let gc = open(&db);
    gc.set_json("main", "/elsewhere", &json!(1), opts("w"))
        .unwrap();
    sweep(&gc);

    indexer.commit_speculation(spec, opts("index")).unwrap();
    assert_eq!(
        indexer.get_json("main", "/index/pkg.f").unwrap(),
        json!("sym_f")
    );
    assert_eq!(indexer.get_json("main", "/elsewhere").unwrap(), json!(1));
}

#[test]
fn sealed_epoch_in_another_namespace_survives_a_sweep() {
    let db = scratch("epoch-ns");
    let a = open(&db).with_namespace(Namespace::new("ws-a").unwrap());
    a.init().unwrap();
    a.create_epoch("audit-q3", "sealed audit trail", vec![])
        .unwrap();
    a.set_active_epoch(Some("audit-q3".into())).unwrap();
    let sealed = a
        .set_json("main", "/record", &json!("sealed v1"), opts("sealed"))
        .unwrap();
    a.set_active_epoch(None).unwrap();
    a.seal_epoch("audit-q3", "quarter closed").unwrap();
    a.set_json("main", "/record", &json!("v2"), opts("later"))
        .unwrap();

    let default_ns = open(&db);
    default_ns.init().unwrap();
    sweep(&default_ns);

    assert_eq!(a.first_missing_object(&sealed).unwrap(), None);
}
