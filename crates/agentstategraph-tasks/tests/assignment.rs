mod common;

use agentstategraph_tasks::{PlanStatus, Priority, Proof};

use common::make_store;

#[test]
fn add_task_with_assignment() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();
    let task = store
        .add_task(
            "main",
            "p",
            "assigned",
            Priority::Medium,
            None,
            vec![],
            Some("codex".into()),
        )
        .unwrap();
    assert_eq!(task.assigned_to.as_deref(), Some("codex"));

    let fetched = store.get_task("main", "p", &task.id).unwrap();
    assert_eq!(fetched.assigned_to.as_deref(), Some("codex"));
}

#[test]
fn add_task_without_assignment() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();
    let task = store
        .add_task("main", "p", "free", Priority::Medium, None, vec![], None)
        .unwrap();
    assert!(task.assigned_to.is_none());
}

#[test]
fn assign_and_unassign_task() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();
    let task = store
        .add_task("main", "p", "x", Priority::Medium, None, vec![], None)
        .unwrap();

    let task = store.assign_task("main", "p", &task.id, "claude-code").unwrap();
    assert_eq!(task.assigned_to.as_deref(), Some("claude-code"));

    let task = store.unassign_task("main", "p", &task.id).unwrap();
    assert!(task.assigned_to.is_none());
}

#[test]
fn next_task_for_filters_by_agent() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();

    store
        .add_task(
            "main",
            "p",
            "for codex",
            Priority::High,
            None,
            vec![],
            Some("codex".into()),
        )
        .unwrap();
    store
        .add_task(
            "main",
            "p",
            "for claude",
            Priority::Medium,
            None,
            vec![],
            Some("claude-code".into()),
        )
        .unwrap();
    store
        .add_task(
            "main",
            "p",
            "unassigned",
            Priority::Low,
            None,
            vec![],
            None,
        )
        .unwrap();

    // Codex sees its own task (highest priority among its candidates).
    let next = store
        .next_task_for("main", "p", Some("codex"), true)
        .unwrap()
        .unwrap();
    assert_eq!(next.title, "for codex");

    // Claude sees its own task.
    let next = store
        .next_task_for("main", "p", Some("claude-code"), true)
        .unwrap()
        .unwrap();
    assert_eq!(next.title, "for claude");

    // Unknown agent with include_unassigned=true sees unassigned tasks.
    let next = store
        .next_task_for("main", "p", Some("other"), true)
        .unwrap()
        .unwrap();
    assert_eq!(next.title, "unassigned");

    // Unknown agent with include_unassigned=false sees nothing.
    let next = store
        .next_task_for("main", "p", Some("other"), false)
        .unwrap();
    assert!(next.is_none());
}

#[test]
fn next_task_for_with_none_returns_any() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();
    store
        .add_task(
            "main",
            "p",
            "x",
            Priority::High,
            None,
            vec![],
            Some("codex".into()),
        )
        .unwrap();
    // No filter — should return the task regardless of assignment.
    let next = store.next_task_for("main", "p", None, true).unwrap();
    assert!(next.is_some());
}

#[test]
fn list_plans_by_status_filters() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "alpha", None).unwrap();
    store.create_plan("main", "beta", None).unwrap();
    store.archive_plan("main", "beta").unwrap();

    let active = store
        .list_plans_by_status("main", Some(PlanStatus::Active))
        .unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].name, "alpha");

    let archived = store
        .list_plans_by_status("main", Some(PlanStatus::Archived))
        .unwrap();
    assert_eq!(archived.len(), 1);
    assert_eq!(archived[0].name, "beta");

    let all = store.list_plans_by_status("main", None).unwrap();
    assert_eq!(all.len(), 2);
}

#[test]
fn assignment_survives_transitions() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();
    let task = store
        .add_task(
            "main",
            "p",
            "x",
            Priority::Medium,
            None,
            vec![],
            Some("codex".into()),
        )
        .unwrap();

    let task = store.start_task("main", "p", &task.id).unwrap();
    assert_eq!(task.assigned_to.as_deref(), Some("codex"));

    let task = store
        .complete_task("main", "p", &task.id, Proof::commit("abc"))
        .unwrap();
    assert_eq!(task.assigned_to.as_deref(), Some("codex"));
}
