//! Regression tests for `archive_plan`.
//!
//! Archive is a soft, reversible transition valid for any plan status.
//! Historically it used a plain `set_json` (blind ref advance), so two
//! concurrent archives on the same branch could silently lose one
//! another's write — the second archive branched off the pre-archive
//! head and overwrote the first, leaving the "archived" plan active
//! again. It now uses a CAS retry loop; these tests lock that in.

mod common;

use std::sync::Arc;
use std::thread;

use agentstategraph_tasks::{PlanStatus, Priority};

use common::make_store;

/// Archiving an active plan sets status + archived_at and preserves tasks.
#[test]
fn archive_active_plan_preserves_tasks() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "alpha", None).unwrap();
    store
        .add_task(
            "main",
            "alpha",
            "keep me",
            Priority::Medium,
            None,
            vec![],
            None,
        )
        .unwrap();

    let archived = store.archive_plan("main", "alpha").unwrap();
    assert_eq!(archived.status, PlanStatus::Archived);
    assert!(archived.archived_at.is_some(), "archived_at must be set");

    // Still readable, and its task survived intact.
    let reread = store.get_plan("main", "alpha").unwrap();
    assert_eq!(reread.status, PlanStatus::Archived);
    let tasks = store.list_tasks("main", "alpha").unwrap();
    assert_eq!(tasks.len(), 1, "archival must not drop tasks");
    assert_eq!(tasks[0].title, "keep me");
}

/// An empty, zero-task plan archives cleanly.
#[test]
fn archive_empty_plan() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "empty", None).unwrap();

    let archived = store.archive_plan("main", "empty").unwrap();
    assert_eq!(archived.status, PlanStatus::Archived);

    let listed = store
        .list_plans_by_status("main", Some(PlanStatus::Archived))
        .unwrap();
    assert!(listed.iter().any(|p| p.name == "empty"));
}

/// Two concurrent archives of *different* plans on the same branch must
/// both persist. With the old blind `set_json`, one archival was lost.
#[test]
fn concurrent_archive_no_lost_update() {
    const PLAN_COUNT: usize = 8;

    let (_repo, store) = make_store("/plans");
    let store = Arc::new(store);

    for i in 0..PLAN_COUNT {
        store.create_plan("main", &format!("p{}", i), None).unwrap();
    }

    let handles: Vec<_> = (0..PLAN_COUNT)
        .map(|i| {
            let store = Arc::clone(&store);
            thread::spawn(move || store.archive_plan("main", &format!("p{}", i)))
        })
        .collect();

    for handle in handles {
        handle
            .join()
            .expect("thread panicked")
            .expect("archive_plan failed");
    }

    // Every plan must read back as Archived — none clobbered by a racer.
    let archived = store
        .list_plans_by_status("main", Some(PlanStatus::Archived))
        .unwrap();
    assert_eq!(
        archived.len(),
        PLAN_COUNT,
        "all {} plans must remain archived, got {}: {:?}",
        PLAN_COUNT,
        archived.len(),
        archived.iter().map(|p| &p.name).collect::<Vec<_>>(),
    );
}
