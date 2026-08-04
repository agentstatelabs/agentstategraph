mod common;

use agentstategraph_tasks::{PlanStatus, Priority, Proof, TaskStatus, TaskStoreError};

use common::make_store;

/// Completing the last open task no longer auto-completes the plan.
/// Closing is an explicit, summary-gated action (`close_plan`) — the
/// plan-level analog of a task's `proof`. A plan with every task
/// terminal stays `Active` until it is explicitly closed.
#[test]
fn final_complete_leaves_plan_active_until_explicit_close() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();

    let a = store
        .add_task("main", "p", "a", Priority::Medium, None, vec![], None)
        .unwrap();
    let b = store
        .add_task("main", "p", "b", Priority::Medium, None, vec![], None)
        .unwrap();

    store.start_task("main", "p", &a.id).unwrap();
    store
        .complete_task("main", "p", &a.id, Proof::commit("a"))
        .unwrap();
    store.start_task("main", "p", &b.id).unwrap();
    store
        .complete_task("main", "p", &b.id, Proof::commit("b"))
        .unwrap();

    // Every task terminal — but the plan stays Active (no auto-promote).
    assert_eq!(
        store.get_plan("main", "p").unwrap().status,
        PlanStatus::Active
    );

    // Explicit close records the summary and promotes to Completed.
    let closed = store.close_plan("main", "p", "shipped a + b").unwrap();
    assert_eq!(closed.status, PlanStatus::Completed);
    assert_eq!(closed.summary.as_deref(), Some("shipped a + b"));
    assert!(closed.closed_at.is_some());
    assert!(closed.closed_by.is_some());
}

#[test]
fn intermediate_complete_does_not_promote_plan() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();
    let a = store
        .add_task("main", "p", "a", Priority::Medium, None, vec![], None)
        .unwrap();
    store
        .add_task("main", "p", "b", Priority::Medium, None, vec![], None)
        .unwrap();

    store.start_task("main", "p", &a.id).unwrap();
    store
        .complete_task("main", "p", &a.id, Proof::commit("a"))
        .unwrap();

    assert_eq!(
        store.get_plan("main", "p").unwrap().status,
        PlanStatus::Active
    );
}

/// A terminal task transition writes a single commit and does NOT touch
/// the plan meta — the plan stays exactly as it was.
#[test]
fn final_complete_is_a_single_task_only_commit() {
    let (repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();
    let a = store
        .add_task("main", "p", "a", Priority::Medium, None, vec![], None)
        .unwrap();
    store.start_task("main", "p", &a.id).unwrap();

    let before_head = repo.log("main", 1).unwrap()[0].id;

    store
        .complete_task("main", "p", &a.id, Proof::commit("a"))
        .unwrap();

    // Task is Done; plan is untouched (still Active).
    let task_after = store.get_task("main", "p", &a.id).unwrap();
    assert_eq!(task_after.status, TaskStatus::Done);
    assert_eq!(
        store.get_plan("main", "p").unwrap().status,
        PlanStatus::Active
    );

    // Exactly one new commit whose parent is the pre-complete head.
    let head = repo.log("main", 1).unwrap()[0].clone();
    assert_eq!(head.parents, vec![before_head]);
}

/// Abandoning the last open task also leaves the plan `Active` — no
/// transition auto-closes the plan anymore.
#[test]
fn final_abandon_leaves_plan_active() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();

    let a = store
        .add_task("main", "p", "a", Priority::Medium, None, vec![], None)
        .unwrap();
    let b = store
        .add_task("main", "p", "b", Priority::Medium, None, vec![], None)
        .unwrap();

    store.start_task("main", "p", &a.id).unwrap();
    store
        .complete_task("main", "p", &a.id, Proof::commit("a"))
        .unwrap();
    store
        .abandon_task("main", "p", &b.id, "scoped out")
        .unwrap();

    assert_eq!(
        store.get_plan("main", "p").unwrap().status,
        PlanStatus::Active
    );

    // A plan can still be closed when its tasks are Done OR Abandoned.
    let closed = store
        .close_plan("main", "p", "a done, b scoped out")
        .unwrap();
    assert_eq!(closed.status, PlanStatus::Completed);
}

/// `close_plan` refuses an empty summary — the plan-level `proof` rule.
#[test]
fn close_plan_requires_a_summary() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();
    let a = store
        .add_task("main", "p", "a", Priority::Medium, None, vec![], None)
        .unwrap();
    store.start_task("main", "p", &a.id).unwrap();
    store
        .complete_task("main", "p", &a.id, Proof::commit("a"))
        .unwrap();

    assert!(matches!(
        store.close_plan("main", "p", "   "),
        Err(TaskStoreError::SummaryRequired)
    ));
    // Still Active — the failed close changed nothing.
    assert_eq!(
        store.get_plan("main", "p").unwrap().status,
        PlanStatus::Active
    );
}

/// `close_plan` refuses while any task is still open.
#[test]
fn close_plan_refuses_open_tasks() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();
    store
        .add_task("main", "p", "a", Priority::Medium, None, vec![], None)
        .unwrap();

    assert!(matches!(
        store.close_plan("main", "p", "done"),
        Err(TaskStoreError::CannotClose { .. })
    ));
}

/// Closing an already-closed plan is idempotent.
#[test]
fn close_plan_is_idempotent() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();
    let a = store
        .add_task("main", "p", "a", Priority::Medium, None, vec![], None)
        .unwrap();
    store.start_task("main", "p", &a.id).unwrap();
    store
        .complete_task("main", "p", &a.id, Proof::commit("a"))
        .unwrap();

    let first = store.close_plan("main", "p", "first summary").unwrap();
    let again = store.close_plan("main", "p", "different text").unwrap();
    // Idempotent: the stored summary is the original, unchanged.
    assert_eq!(again.status, PlanStatus::Completed);
    assert_eq!(again.summary, first.summary);
}

#[test]
fn plan_ops_use_intent_category_plan() {
    use agentstategraph_core::IntentCategory;

    let (repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();
    store
        .add_task("main", "p", "x", Priority::Medium, None, vec![], None)
        .unwrap();

    let commits = repo.log("main", 10).unwrap();
    // The two most recent commits (add_task and create_plan) are task ops.
    let recent = &commits[..2];
    for c in recent {
        assert_eq!(c.intent.category, IntentCategory::Plan, "commit: {:?}", c);
    }
}
