mod common;

use agentstategraph_tasks::{Priority, Proof, TaskStatus, TaskStoreError};

use common::make_store;

#[test]
fn pending_to_in_progress_to_done() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();
    let task = store
        .add_task("main", "p", "x", Priority::Medium, None, vec![], None)
        .unwrap();
    assert_eq!(task.status, TaskStatus::Pending);

    let task = store.start_task("main", "p", &task.id).unwrap();
    assert_eq!(task.status, TaskStatus::InProgress);
    assert!(task.started_at.is_some());
    assert_eq!(task.started_by.as_deref(), Some("test-agent"));

    let task = store
        .complete_task("main", "p", &task.id, Proof::commit("abc"))
        .unwrap();
    assert_eq!(task.status, TaskStatus::Done);
    assert!(task.completed_at.is_some());
    assert_eq!(task.completed_by.as_deref(), Some("test-agent"));
    assert!(task.proof.is_some());
}

#[test]
fn abandon_from_pending() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();
    let task = store
        .add_task("main", "p", "x", Priority::Low, None, vec![], None)
        .unwrap();
    let task = store
        .abandon_task("main", "p", &task.id, "deprioritized")
        .unwrap();
    assert_eq!(task.status, TaskStatus::Abandoned);
    assert_eq!(task.abandoned_reason.as_deref(), Some("deprioritized"));
}

#[test]
fn abandon_from_in_progress() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();
    let task = store
        .add_task("main", "p", "x", Priority::Low, None, vec![], None)
        .unwrap();
    store.start_task("main", "p", &task.id).unwrap();
    let task = store
        .abandon_task("main", "p", &task.id, "blocked externally")
        .unwrap();
    assert_eq!(task.status, TaskStatus::Abandoned);
}

#[test]
fn abandon_requires_reason() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();
    let task = store
        .add_task("main", "p", "x", Priority::Low, None, vec![], None)
        .unwrap();
    let err = store
        .abandon_task("main", "p", &task.id, "   ")
        .unwrap_err();
    assert!(matches!(err, TaskStoreError::ReasonRequired));
}

#[test]
fn cannot_complete_without_starting() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();
    let task = store
        .add_task("main", "p", "x", Priority::Low, None, vec![], None)
        .unwrap();
    let err = store
        .complete_task("main", "p", &task.id, Proof::text("ok"))
        .unwrap_err();
    assert!(matches!(err, TaskStoreError::InvalidTransition { .. }));
}

#[test]
fn cannot_restart_done_task() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();
    let task = store
        .add_task("main", "p", "x", Priority::Low, None, vec![], None)
        .unwrap();
    store.start_task("main", "p", &task.id).unwrap();
    store
        .complete_task("main", "p", &task.id, Proof::commit("abc"))
        .unwrap();
    let err = store.start_task("main", "p", &task.id).unwrap_err();
    assert!(matches!(err, TaskStoreError::InvalidTransition { .. }));
}

#[test]
fn cannot_abandon_done_task() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();
    let task = store
        .add_task("main", "p", "x", Priority::Low, None, vec![], None)
        .unwrap();
    store.start_task("main", "p", &task.id).unwrap();
    store
        .complete_task("main", "p", &task.id, Proof::commit("abc"))
        .unwrap();
    let err = store
        .abandon_task("main", "p", &task.id, "too late")
        .unwrap_err();
    assert!(matches!(err, TaskStoreError::InvalidTransition { .. }));
}
