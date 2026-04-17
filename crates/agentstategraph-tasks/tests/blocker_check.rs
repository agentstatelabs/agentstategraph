mod common;

use agentstategraph_tasks::{Priority, Proof, TaskId, TaskStoreError};

use common::make_store;

#[test]
fn start_refused_while_blocker_pending() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();

    let a = store
        .add_task("main", "p", "blocker", Priority::Medium, None, vec![], None)
        .unwrap();
    let b = store
        .add_task(
            "main",
            "p",
            "dependent",
            Priority::High,
            None,
            vec![a.id.clone()],
            None,
        )
        .unwrap();

    let err = store.start_task("main", "p", &b.id).unwrap_err();
    match err {
        TaskStoreError::Blocked { blockers } => {
            assert_eq!(blockers, vec![a.id.clone()]);
        }
        e => panic!("expected Blocked, got {:?}", e),
    }
}

#[test]
fn start_allowed_after_blocker_done() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();

    let a = store
        .add_task("main", "p", "blocker", Priority::Medium, None, vec![], None)
        .unwrap();
    let b = store
        .add_task(
            "main",
            "p",
            "dependent",
            Priority::High,
            None,
            vec![a.id.clone()],
            None,
        )
        .unwrap();

    store.start_task("main", "p", &a.id).unwrap();
    store
        .complete_task("main", "p", &a.id, Proof::commit("abc"))
        .unwrap();
    // Now `b` should start cleanly.
    store.start_task("main", "p", &b.id).unwrap();
}

#[test]
fn abandoned_blocker_does_not_unblock() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();

    let a = store
        .add_task("main", "p", "blocker", Priority::Medium, None, vec![], None)
        .unwrap();
    let b = store
        .add_task(
            "main",
            "p",
            "dependent",
            Priority::High,
            None,
            vec![a.id.clone()],
            None,
        )
        .unwrap();

    store
        .abandon_task("main", "p", &a.id, "not needed")
        .unwrap();
    let err = store.start_task("main", "p", &b.id).unwrap_err();
    assert!(matches!(err, TaskStoreError::Blocked { .. }));
}

#[test]
fn set_blockers_requires_existing_tasks() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();
    let a = store
        .add_task("main", "p", "x", Priority::Low, None, vec![], None)
        .unwrap();
    let err = store
        .set_blockers("main", "p", &a.id, vec![TaskId::new(99)])
        .unwrap_err();
    assert!(matches!(err, TaskStoreError::TaskNotFound { .. }));
}

/// If a blocker is removed by some process outside `TaskStore` (e.g. a
/// future `delete_task` op, or a direct `Repository::delete`), the
/// dependent task's `start_task` must surface `BlockerNotFound` rather
/// than `Blocked` — otherwise the task looks merely "pending more work"
/// when it's actually referencing a phantom dependency.
#[test]
fn missing_blocker_produces_distinct_error() {
    use agentstategraph::CommitOptions;
    use agentstategraph_core::IntentCategory;

    let (repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();

    let a = store
        .add_task("main", "p", "blocker", Priority::Medium, None, vec![], None)
        .unwrap();
    let b = store
        .add_task(
            "main",
            "p",
            "dependent",
            Priority::High,
            None,
            vec![a.id.clone()],
            None,
        )
        .unwrap();

    // Bypass TaskStore and delete the blocker directly.
    repo.delete(
        "main",
        "/plans/p/t-001",
        CommitOptions::new("test", IntentCategory::Plan, "simulate lost blocker"),
    )
    .unwrap();

    let err = store.start_task("main", "p", &b.id).unwrap_err();
    match err {
        TaskStoreError::BlockerNotFound { blockers } => {
            assert_eq!(blockers, vec![a.id]);
        }
        e => panic!("expected BlockerNotFound, got {:?}", e),
    }
}
