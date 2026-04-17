mod common;

use agentstategraph_tasks::{Priority, Proof};

use common::make_store;

#[test]
fn next_task_picks_highest_priority_unblocked() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();

    store
        .add_task("main", "p", "low", Priority::Low, None, vec![], None)
        .unwrap();
    let high = store
        .add_task("main", "p", "high", Priority::High, None, vec![], None)
        .unwrap();
    store
        .add_task("main", "p", "medium", Priority::Medium, None, vec![], None)
        .unwrap();

    let next = store.next_task("main", "p").unwrap().unwrap();
    assert_eq!(next.id, high.id);
}

#[test]
fn next_task_skips_blocked() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();

    let blocker = store
        .add_task("main", "p", "blocker", Priority::Low, None, vec![], None)
        .unwrap();
    store
        .add_task(
            "main",
            "p",
            "high-but-blocked",
            Priority::Critical,
            None,
            vec![blocker.id.clone()],
            None,
        )
        .unwrap();

    let next = store.next_task("main", "p").unwrap().unwrap();
    // Should be the blocker, since the Critical task is blocked.
    assert_eq!(next.id, blocker.id);
}

#[test]
fn next_task_none_when_all_done() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();
    let a = store
        .add_task("main", "p", "only", Priority::Medium, None, vec![], None)
        .unwrap();
    store.start_task("main", "p", &a.id).unwrap();
    store
        .complete_task("main", "p", &a.id, Proof::commit("abc"))
        .unwrap();
    assert!(store.next_task("main", "p").unwrap().is_none());
}
