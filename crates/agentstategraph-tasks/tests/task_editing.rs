//! Editing a task after it's added: a new title, and new blockers that must
//! not make any task wait on itself.

mod common;

use agentstategraph_core::IntentCategory;
use agentstategraph_tasks::{Priority, Proof, TaskId, TaskStatus, TaskStore, TaskStoreError};

use common::make_store;

/// A plan with three tasks: t-002 waits on t-001, t-003 on t-002.
fn chain() -> (std::sync::Arc<agentstategraph::Repository>, TaskStore) {
    let (repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();
    let mut prev: Vec<TaskId> = vec![];
    for title in ["one", "two", "three"] {
        let t = store
            .add_task("main", "p", title, Priority::Medium, None, prev, None)
            .unwrap();
        prev = vec![t.id];
    }
    (repo, store)
}

fn id(n: u32) -> TaskId {
    TaskId::new(n)
}

#[test]
fn set_title_replaces_only_the_title() {
    let (repo, store) = chain();
    store.start_task("main", "p", &id(1)).unwrap();
    let before = store.get_task("main", "p", &id(2)).unwrap();

    let edited = store
        .set_title("main", "p", &id(2), "two, retitled")
        .unwrap();
    assert_eq!(edited.title, "two, retitled");
    let fetched = store.get_task("main", "p", &id(2)).unwrap();
    assert_eq!(fetched, edited);
    assert_eq!(
        fetched,
        agentstategraph_tasks::Task {
            title: "two, retitled".into(),
            ..before
        }
    );

    let head = repo.log("main", 1).unwrap().remove(0);
    assert_eq!(head.agent_id, "test-agent");
    assert_eq!(head.intent.category, IntentCategory::Plan);
    assert_eq!(head.intent.description, "Retitle p/t-002");
}

#[test]
fn set_title_works_on_a_finished_task() {
    let (_repo, store) = chain();
    store.start_task("main", "p", &id(1)).unwrap();
    store
        .complete_task("main", "p", &id(1), Proof::commit("abc"))
        .unwrap();
    let edited = store.set_title("main", "p", &id(1), "one (done)").unwrap();
    assert_eq!(edited.status, TaskStatus::Done);
    assert_eq!(edited.proof, Some(Proof::commit("abc")));
}

#[test]
fn a_blank_title_is_refused() {
    let (_repo, store) = chain();
    for blank in ["", "   ", "\t"] {
        let err = store.set_title("main", "p", &id(1), blank).unwrap_err();
        assert!(matches!(err, TaskStoreError::TitleRequired), "{err:?}");
    }
    assert_eq!(store.get_task("main", "p", &id(1)).unwrap().title, "one");
}

#[test]
fn retitling_a_missing_task_is_task_not_found() {
    let (_repo, store) = chain();
    let err = store.set_title("main", "p", &id(9), "x").unwrap_err();
    assert!(
        matches!(err, TaskStoreError::TaskNotFound { .. }),
        "{err:?}"
    );
}

#[test]
fn a_task_cannot_block_itself() {
    let (_repo, store) = chain();
    let err = store
        .set_blockers("main", "p", &id(1), vec![id(1)])
        .unwrap_err();
    match err {
        TaskStoreError::BlockerCycle { cycle } => assert_eq!(cycle, vec![id(1), id(1)]),
        other => panic!("expected BlockerCycle, got {other:?}"),
    }
}

#[test]
fn a_blocker_that_waits_on_the_task_is_refused() {
    let (_repo, store) = chain();
    // t-002 waits on t-001, so t-001 can't wait on t-002.
    let err = store
        .set_blockers("main", "p", &id(1), vec![id(2)])
        .unwrap_err();
    match err {
        TaskStoreError::BlockerCycle { cycle } => {
            assert_eq!(cycle, vec![id(1), id(2), id(1)])
        }
        other => panic!("expected BlockerCycle, got {other:?}"),
    }
    // Through a longer chain: t-003 → t-002 → t-001.
    let err = store
        .set_blockers("main", "p", &id(1), vec![id(3)])
        .unwrap_err();
    match err {
        TaskStoreError::BlockerCycle { cycle } => {
            assert_eq!(cycle, vec![id(1), id(3), id(2), id(1)])
        }
        other => panic!("expected BlockerCycle, got {other:?}"),
    }
    assert!(
        store
            .get_task("main", "p", &id(1))
            .unwrap()
            .blocked_by
            .is_empty()
    );
}

#[test]
fn blockers_that_keep_the_plan_acyclic_are_accepted() {
    let (_repo, store) = chain();
    // t-003 waits on t-001 directly as well as through t-002: a diamond, not
    // a cycle.
    let edited = store
        .set_blockers("main", "p", &id(3), vec![id(2), id(1)])
        .unwrap();
    assert_eq!(edited.blocked_by, vec![id(2), id(1)]);
    // Reversing the chain's last link is fine once t-003 no longer waits on t-002.
    store.set_blockers("main", "p", &id(3), vec![]).unwrap();
    store
        .set_blockers("main", "p", &id(2), vec![id(3)])
        .unwrap();
    assert_eq!(
        store.get_task("main", "p", &id(2)).unwrap().blocked_by,
        vec![id(3)]
    );
}

#[test]
fn replacing_a_blocker_unblocks_the_task() {
    let (_repo, store) = chain();
    store.start_task("main", "p", &id(1)).unwrap();
    store
        .abandon_task("main", "p", &id(1), "not needed")
        .unwrap();
    // An abandoned blocker never counts as done...
    let err = store.start_task("main", "p", &id(2)).unwrap_err();
    assert!(matches!(err, TaskStoreError::Blocked { .. }), "{err:?}");
    // ...so the dependent's blockers are edited instead.
    store.set_blockers("main", "p", &id(2), vec![]).unwrap();
    store.start_task("main", "p", &id(2)).unwrap();
}
