mod common;

use agentstategraph_tasks::{PlanStatus, Priority, Proof, TaskStatus};

use common::make_store;

/// Completing the last open task in a plan must atomically mark the
/// plan as `Completed` in the same commit as the task transition.
///
/// Atomicity is validated two ways:
/// 1. Reading the plan and the task under the *same* ref — both
///    show the new state with no intermediate read possible.
/// 2. Counting commits — completing the final task should produce
///    exactly one new commit, not two.
#[test]
fn final_complete_promotes_plan_in_single_commit() {
    let (repo, store) = make_store("/plans");
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

    // Plan still active — one task is open.
    assert_eq!(
        store.get_plan("main", "p").unwrap().status,
        PlanStatus::Active
    );

    let commits_before = repo.log("main", 1000).unwrap().len();

    store.start_task("main", "p", &b.id).unwrap();
    let start_commits = repo.log("main", 1000).unwrap().len();
    assert_eq!(start_commits, commits_before + 1);

    store
        .complete_task("main", "p", &b.id, Proof::commit("b"))
        .unwrap();

    // Plan flipped to Completed.
    assert_eq!(
        store.get_plan("main", "p").unwrap().status,
        PlanStatus::Completed
    );
    let task = store.get_task("main", "p", &b.id).unwrap();
    assert_eq!(task.status, TaskStatus::Done);

    // Exactly one additional commit for the combined task+plan update.
    let final_commits = repo.log("main", 1000).unwrap().len();
    assert_eq!(final_commits, start_commits + 1);
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

/// Prove the plan-and-task update is a SINGLE commit by reading state
/// at the commit-before and the commit-after. A naïve two-`set_json`
/// implementation would produce an intermediate state (task=Done,
/// plan=Active) — this test fails if that intermediate is ever observable.
#[test]
fn final_complete_flip_is_observable_only_as_a_single_step() {
    let (repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();
    let a = store
        .add_task("main", "p", "a", Priority::Medium, None, vec![], None)
        .unwrap();
    store.start_task("main", "p", &a.id).unwrap();

    // Snapshot the head right before the final complete.
    let before_head = repo.log("main", 1).unwrap()[0].id;
    // Point a throwaway branch at it so we can read state "before".
    repo.branch("before", "main").unwrap();

    store
        .complete_task("main", "p", &a.id, Proof::commit("a"))
        .unwrap();

    // At "before": task InProgress, plan Active.
    let task_before = store.get_task("before", "p", &a.id).unwrap();
    assert_eq!(task_before.status, TaskStatus::InProgress);
    let plan_before = store.get_plan("before", "p").unwrap();
    assert_eq!(plan_before.status, PlanStatus::Active);

    // At main (after the single complete commit): task Done, plan Completed.
    let task_after = store.get_task("main", "p", &a.id).unwrap();
    assert_eq!(task_after.status, TaskStatus::Done);
    let plan_after = store.get_plan("main", "p").unwrap();
    assert_eq!(plan_after.status, PlanStatus::Completed);

    // The head advanced by exactly one commit and its parent is the
    // before snapshot — no intermediate commit was produced.
    let head = repo.log("main", 1).unwrap()[0].clone();
    assert_eq!(head.parents, vec![before_head]);
}

#[test]
fn final_abandon_also_promotes_plan() {
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

    // Plan still active — b is open.
    assert_eq!(
        store.get_plan("main", "p").unwrap().status,
        PlanStatus::Active
    );

    // Abandoning the final open task must flip the plan to Completed
    // so the invariant "plan Completed iff every task terminal" holds
    // regardless of which transition closes the last task.
    store
        .abandon_task("main", "p", &b.id, "scoped out")
        .unwrap();
    assert_eq!(
        store.get_plan("main", "p").unwrap().status,
        PlanStatus::Completed
    );
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
