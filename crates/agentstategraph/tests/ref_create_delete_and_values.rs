//! Data-loss audit, batch 3: refs created or deleted without checking what is
//! there, writes that replace a value they only meant to descend through, and
//! integers that do not round-trip.

use std::sync::{Arc, Barrier};

use agentstategraph::{CommitOptions, RepoError, Repository};
use agentstategraph_core::IntentCategory;
use agentstategraph_storage::SqliteStorage;
use serde_json::json;

fn scratch(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "asg-batch3-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

/// A handle with its own connection, as a separate process would have.
fn open(db: &std::path::Path) -> Repository {
    Repository::new(Box::new(SqliteStorage::open(db).unwrap()))
}

fn opts(d: &str) -> CommitOptions {
    CommitOptions::new("test", IntentCategory::Refine, d)
}

/// `init()` checked for `main` and then set it unconditionally, so two
/// processes initializing one store could each create a `main` — and the
/// later one replaced the earlier, discarding whatever had been written on it.
#[test]
fn concurrent_inits_share_one_main_and_keep_every_write() {
    const WRITERS: usize = 8;
    for round in 0..10 {
        let db = scratch(&format!("init-{round}"));
        // Opened up front: each handle has its own connection, as separate
        // processes would, and the race under test is init() alone.
        let repos: Vec<Repository> = (0..WRITERS).map(|_| open(&db)).collect();
        let barrier = Arc::new(Barrier::new(WRITERS));
        let handles: Vec<_> = repos
            .into_iter()
            .enumerate()
            .map(|(i, repo)| {
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    repo.init().unwrap();
                    repo.set_json("main", &format!("/w{i}"), &json!(i), opts("write"))
                        .unwrap();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        let repo = open(&db);
        let state = repo.get_json("main", "/").unwrap();
        for i in 0..WRITERS {
            assert_eq!(
                state[format!("w{i}")],
                json!(i),
                "round {round}: a write made after init() was lost: {state}"
            );
        }
        let roots = repo
            .log("main", 1_000)
            .unwrap()
            .into_iter()
            .filter(|c| c.parents.is_empty())
            .count();
        assert_eq!(
            roots, 1,
            "round {round}: `main` was initialized {roots} times"
        );
    }
}

/// `branch()` likewise: several callers creating one name each got `Ok`, and
/// all but the last were silently pointed somewhere else.
#[test]
fn a_branch_created_concurrently_has_exactly_one_creator() {
    const WRITERS: usize = 8;
    let db = scratch("branch");
    let repo = Arc::new(open(&db));
    repo.init().unwrap();
    let sources: Vec<_> = (0..WRITERS)
        .map(|i| {
            repo.set_json("main", "/n", &json!(i), opts("source"))
                .unwrap();
            repo.branch(&format!("src{i}"), "main").unwrap()
        })
        .collect();

    for round in 0..20 {
        let name = format!("feature-{round}");
        let barrier = Arc::new(Barrier::new(WRITERS));
        let handles: Vec<_> = (0..WRITERS)
            .map(|i| {
                let (repo, barrier, name) = (Arc::clone(&repo), Arc::clone(&barrier), name.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    repo.branch(&name, &format!("src{i}")).map(|_| i)
                })
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        let winners: Vec<usize> = results
            .iter()
            .filter_map(|r| r.as_ref().ok().copied())
            .collect();
        assert_eq!(
            winners.len(),
            1,
            "round {round}: {} creators got Ok",
            winners.len()
        );
        for r in &results {
            if let Err(e) = r {
                assert!(
                    matches!(e, RepoError::BranchAlreadyExists(_)),
                    "unexpected error: {e}"
                );
            }
        }
        assert_eq!(repo.head(&name).unwrap(), sources[winners[0]]);
    }
}

/// Deleting `main` stranded the namespace: the next `init()` created an empty
/// `main` over it, and a GC sweep then deleted everything it used to reach.
#[test]
fn main_cannot_be_deleted() {
    let repo = open(&scratch("delete-main"));
    repo.init().unwrap();
    let head = repo
        .set_json("main", "/kept", &json!(1), opts("w"))
        .unwrap();

    assert!(matches!(
        repo.delete_branch("main"),
        Err(RepoError::InvalidOperation(_))
    ));
    assert_eq!(repo.head("main").unwrap(), head);
}

/// `delete_branch_if` deletes only the head the caller looked at, so a commit
/// that lands in between is not orphaned for GC to sweep.
#[test]
fn delete_branch_if_keeps_a_branch_that_moved() {
    let repo = open(&scratch("delete-if"));
    repo.init().unwrap();
    let seen = repo.branch("feature", "main").unwrap();
    let moved = repo
        .set_json("feature", "/late", &json!("work"), opts("late write"))
        .unwrap();

    assert!(!repo.delete_branch_if("feature", seen).unwrap());
    assert_eq!(repo.head("feature").unwrap(), moved);
    assert!(repo.delete_branch_if("feature", moved).unwrap());
    assert!(repo.list_branches(Some("feature")).unwrap().is_empty());
}

/// Setting a path below an existing scalar replaced the scalar with a map:
/// `/config/timeout = 30`, then a write to `/config/timeout/unit`, and the 30
/// was gone — with `Ok`. A key containing `/` used in a path did the same.
#[test]
fn a_write_below_a_scalar_does_not_replace_it() {
    let repo = open(&scratch("below-scalar"));
    repo.init().unwrap();
    repo.set_json("main", "/config/timeout", &json!(30), opts("w"))
        .unwrap();

    let err = repo
        .set_json("main", "/config/timeout/unit", &json!("s"), opts("w"))
        .unwrap_err();
    assert!(
        matches!(err, RepoError::Tree(_)),
        "expected a type mismatch, got {err}"
    );
    assert_eq!(repo.get_json("main", "/config/timeout").unwrap(), json!(30));

    // A null placeholder is still a fine place to start a subtree.
    repo.set_json("main", "/slot", &json!(null), opts("w"))
        .unwrap();
    repo.set_json("main", "/slot/x", &json!(1), opts("w"))
        .unwrap();
    assert_eq!(repo.get_json("main", "/slot").unwrap(), json!({"x": 1}));
}

/// Integers above `i64::MAX` were stored as `f64`, so a u64 id or hash came
/// back as a different number.
#[test]
fn integers_round_trip_exactly() {
    let repo = open(&scratch("ints"));
    repo.init().unwrap();
    for (i, n) in [
        json!(i64::MIN),
        json!(-1),
        json!(0),
        json!(i64::MAX),
        json!(i64::MAX as u64 + 1),
        json!(u64::MAX),
        json!(1.5),
    ]
    .into_iter()
    .enumerate()
    {
        let path = format!("/n{i}");
        repo.set_json("main", &path, &n, opts("w")).unwrap();
        assert_eq!(repo.get_json("main", &path).unwrap(), n, "{n} changed");
    }
    // Inside a nested value, too.
    let doc = json!({"ids": [u64::MAX, 7], "hash": 18_000_000_000_000_000_000u64});
    repo.set_json("main", "/doc", &doc, opts("w")).unwrap();
    assert_eq!(repo.get_json("main", "/doc").unwrap(), doc);
}
