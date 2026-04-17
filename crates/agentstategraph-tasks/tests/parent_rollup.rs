mod common;

use agentstategraph_tasks::{Priority, Proof, TaskStatus, TaskStoreError};

use common::make_store;

#[test]
fn parent_with_no_subtasks_reports_own_status() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();
    let parent = store
        .add_task("main", "p", "parent", Priority::Medium, None, vec![], None)
        .unwrap();
    let status = store.derived_status("main", "p", &parent.id).unwrap();
    assert_eq!(status, TaskStatus::Pending);
}

#[test]
fn parent_all_subtasks_done() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();
    let parent = store
        .add_task("main", "p", "parent", Priority::Medium, None, vec![], None)
        .unwrap();
    let a = store
        .add_task(
            "main",
            "p",
            "a",
            Priority::Medium,
            Some(parent.id.clone()),
            vec![],
            None,
        )
        .unwrap();
    let b = store
        .add_task(
            "main",
            "p",
            "b",
            Priority::Medium,
            Some(parent.id.clone()),
            vec![],
            None,
        )
        .unwrap();

    store.start_task("main", "p", &a.id).unwrap();
    store
        .complete_task("main", "p", &a.id, Proof::commit("a"))
        .unwrap();
    store.start_task("main", "p", &b.id).unwrap();
    store
        .complete_task("main", "p", &b.id, Proof::commit("b"))
        .unwrap();

    assert_eq!(
        store.derived_status("main", "p", &parent.id).unwrap(),
        TaskStatus::Done
    );
}

#[test]
fn parent_any_in_progress_reports_in_progress() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();
    let parent = store
        .add_task("main", "p", "parent", Priority::Medium, None, vec![], None)
        .unwrap();
    let a = store
        .add_task(
            "main",
            "p",
            "a",
            Priority::Medium,
            Some(parent.id.clone()),
            vec![],
            None,
        )
        .unwrap();
    store
        .add_task(
            "main",
            "p",
            "b",
            Priority::Medium,
            Some(parent.id.clone()),
            vec![],
            None,
        )
        .unwrap();

    store.start_task("main", "p", &a.id).unwrap();
    assert_eq!(
        store.derived_status("main", "p", &parent.id).unwrap(),
        TaskStatus::InProgress
    );
}

#[test]
fn parent_with_abandoned_and_done_reports_done() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();
    let parent = store
        .add_task("main", "p", "parent", Priority::Medium, None, vec![], None)
        .unwrap();
    let a = store
        .add_task(
            "main",
            "p",
            "a",
            Priority::Medium,
            Some(parent.id.clone()),
            vec![],
            None,
        )
        .unwrap();
    let b = store
        .add_task(
            "main",
            "p",
            "b",
            Priority::Medium,
            Some(parent.id.clone()),
            vec![],
            None,
        )
        .unwrap();

    store.start_task("main", "p", &a.id).unwrap();
    store
        .complete_task("main", "p", &a.id, Proof::commit("a"))
        .unwrap();
    store.abandon_task("main", "p", &b.id, "skipped").unwrap();

    assert_eq!(
        store.derived_status("main", "p", &parent.id).unwrap(),
        TaskStatus::Done
    );
}

#[test]
fn parent_with_all_abandoned_reports_pending() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();
    let parent = store
        .add_task("main", "p", "parent", Priority::Medium, None, vec![], None)
        .unwrap();
    let a = store
        .add_task(
            "main",
            "p",
            "a",
            Priority::Medium,
            Some(parent.id.clone()),
            vec![],
            None,
        )
        .unwrap();
    store.abandon_task("main", "p", &a.id, "no").unwrap();
    assert_eq!(
        store.derived_status("main", "p", &parent.id).unwrap(),
        TaskStatus::Pending
    );
}

#[test]
fn cannot_make_subtask_a_parent() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();
    let parent = store
        .add_task("main", "p", "parent", Priority::Medium, None, vec![], None)
        .unwrap();
    let child = store
        .add_task(
            "main",
            "p",
            "child",
            Priority::Medium,
            Some(parent.id.clone()),
            vec![],
            None,
        )
        .unwrap();

    let err = store
        .add_task(
            "main",
            "p",
            "grandchild",
            Priority::Medium,
            Some(child.id.clone()),
            vec![],
            None,
        )
        .unwrap_err();
    assert!(matches!(err, TaskStoreError::ParentIsSubtask(_)));
}
