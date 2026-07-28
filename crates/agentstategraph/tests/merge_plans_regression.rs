//! Regression: map-level merge must not erase keys the target added after the
//! branch point. Reproduces the reported incident where merging a source branch
//! into target dropped target's newly-added /plans entries.

use agentstategraph::{CommitOptions, Repository};
use agentstategraph_core::{IntentCategory, Object};
use agentstategraph_storage::SqliteStorage;

fn repo() -> Repository {
    let r = Repository::new(Box::new(
        SqliteStorage::in_memory().expect("in-memory sqlite"),
    ));
    r.init().unwrap();
    r
}

fn opts(desc: &str) -> CommitOptions {
    CommitOptions::new("agent/test", IntentCategory::Checkpoint, desc)
}

fn merge_opts(desc: &str) -> CommitOptions {
    CommitOptions::new("agent/test", IntentCategory::Merge, desc)
}

#[test]
fn merge_preserves_target_added_plans() {
    let r = repo();

    // Base: target (main) already has plan p1.
    r.set(
        "main",
        "/plans/p1/status",
        &Object::string("pending"),
        opts("add p1"),
    )
    .unwrap();

    // Branch source from the current target state.
    r.branch("source", "main").unwrap();

    // Target adds new plans p2, p3 AFTER the branch point.
    r.set(
        "main",
        "/plans/p2/status",
        &Object::string("pending"),
        opts("add p2"),
    )
    .unwrap();
    r.set(
        "main",
        "/plans/p3/status",
        &Object::string("pending"),
        opts("add p3"),
    )
    .unwrap();

    // Source completes the older plan p1.
    r.set(
        "source",
        "/plans/p1/status",
        &Object::string("done"),
        opts("complete p1"),
    )
    .unwrap();

    // Merge source into target.
    r.merge("source", "main", merge_opts("merge source into main"))
        .unwrap();

    // Source's completion must be incorporated...
    assert_eq!(
        r.get("main", "/plans/p1/status").unwrap(),
        Object::string("done"),
        "source completion of p1 must be incorporated"
    );
    // ...and target's post-branch additions must survive.
    assert_eq!(
        r.get("main", "/plans/p2/status").unwrap(),
        Object::string("pending"),
        "target-added plan p2 must not be erased by merge"
    );
    assert_eq!(
        r.get("main", "/plans/p3/status").unwrap(),
        Object::string("pending"),
        "target-added plan p3 must not be erased by merge"
    );
}

/// Bug B guard: once merge commits exist, the merge base must still be the true
/// lowest common ancestor. A first-parent-only walk would pick too old a base
/// on the second merge and risk erasing or spuriously conflicting keys.
#[test]
fn repeated_merges_preserve_all_plans() {
    let r = repo();
    r.set(
        "main",
        "/plans/p1/status",
        &Object::string("pending"),
        opts("p1"),
    )
    .unwrap();

    // First round: source completes p1, target adds p2; merge back.
    r.branch("source", "main").unwrap();
    r.set(
        "main",
        "/plans/p2/status",
        &Object::string("pending"),
        opts("p2"),
    )
    .unwrap();
    r.set(
        "source",
        "/plans/p1/status",
        &Object::string("done"),
        opts("done p1"),
    )
    .unwrap();
    r.merge("source", "main", merge_opts("merge 1")).unwrap();

    // Second round on the SAME source branch (main now has a merge commit):
    // source adds p3, target adds p4; merge back again.
    r.set(
        "source",
        "/plans/p3/status",
        &Object::string("pending"),
        opts("p3"),
    )
    .unwrap();
    r.set(
        "main",
        "/plans/p4/status",
        &Object::string("pending"),
        opts("p4"),
    )
    .unwrap();
    r.merge("source", "main", merge_opts("merge 2")).unwrap();

    for (plan, expect) in [
        ("p1", "done"),
        ("p2", "pending"),
        ("p3", "pending"),
        ("p4", "pending"),
    ] {
        assert_eq!(
            r.get("main", &format!("/plans/{plan}/status")).unwrap(),
            Object::string(expect),
            "{plan} must survive repeated merges"
        );
    }
}

/// merge_checked with allow_deletions=false must refuse a merge that would drop
/// a whole top-level map, and must leave the target ref untouched.
#[test]
fn merge_checked_blocks_top_level_deletion() {
    let r = repo();
    // Target has /plans and /memory; source deletes all of /plans.
    r.set(
        "main",
        "/plans/p1/status",
        &Object::string("pending"),
        opts("p1"),
    )
    .unwrap();
    r.set("main", "/memory/m1", &Object::string("note"), opts("m1"))
        .unwrap();
    r.branch("source", "main").unwrap();
    // Target diverges so the merge is a real three-way, not a fast-forward.
    r.set("main", "/memory/m2", &Object::string("note2"), opts("m2"))
        .unwrap();
    // Source removes the entire /plans map.
    r.delete("source", "/plans", opts("drop plans")).unwrap();

    let before = r.head("main").unwrap();
    let err = r
        .merge_checked("source", "main", merge_opts("blocked"), false)
        .unwrap_err();
    assert!(
        matches!(&err, agentstategraph::RepoError::MergeWouldDelete(keys) if keys.contains(&"plans".to_string())),
        "expected MergeWouldDelete listing plans, got {err:?}"
    );
    assert_eq!(
        r.head("main").unwrap(),
        before,
        "target ref must be unchanged"
    );
    // /plans still intact.
    assert_eq!(
        r.get("main", "/plans/p1/status").unwrap(),
        Object::string("pending")
    );

    // With allow_deletions=true the same merge proceeds and drops /plans.
    r.merge_checked("source", "main", merge_opts("allowed"), true)
        .unwrap();
    assert!(
        r.get("main", "/plans/p1/status").is_err(),
        "/plans should be gone now"
    );
}

/// merge_base returns the branch-point commit shared by two refs.
#[test]
fn merge_base_is_branch_point() {
    let r = repo();
    let base = r.set("main", "/x", &Object::int(1), opts("base")).unwrap();
    r.branch("src", "main").unwrap();
    r.set("main", "/y", &Object::int(2), opts("target moves"))
        .unwrap();
    r.set("src", "/z", &Object::int(3), opts("source moves"))
        .unwrap();
    assert_eq!(r.merge_base("src", "main").unwrap(), base);
    assert_eq!(r.merge_base("main", "src").unwrap(), base);
}

/// preview_merge reports additions/removals without mutating the target.
#[test]
fn preview_merge_summarizes_without_committing() {
    let r = repo();
    r.set(
        "main",
        "/plans/p1/status",
        &Object::string("pending"),
        opts("p1"),
    )
    .unwrap();
    r.branch("source", "main").unwrap();
    r.set("main", "/memory/m1", &Object::string("note"), opts("m1"))
        .unwrap(); // target adds /memory
    r.set(
        "source",
        "/plans/p1/status",
        &Object::string("done"),
        opts("done"),
    )
    .unwrap();

    let before = r.head("main").unwrap();
    let preview = r.preview_merge("source", "main").unwrap();
    assert_eq!(
        r.head("main").unwrap(),
        before,
        "preview must not move the ref"
    );
    assert!(
        preview.removed.is_empty(),
        "nothing should be removed: {:?}",
        preview.removed
    );
    assert!(preview.conflicts.is_empty());
    // /plans subtree changes (p1 -> done); /memory is target-only so unchanged by the merge.
    assert!(
        preview.changed.contains(&"plans".to_string()),
        "changed={:?}",
        preview.changed
    );
}
