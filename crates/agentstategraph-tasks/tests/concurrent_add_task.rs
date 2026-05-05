//! Regression test: concurrent `add_task` calls must not produce duplicate
//! task ids even when multiple threads race on the same plan.
//!
//! The previous fix used a `Mutex<HashMap<…>>` which only protected within a
//! single process. The current fix uses a CAS retry loop on the storage ref,
//! which is safe for both multi-thread and multi-process writers.

mod common;

use std::sync::Arc;
use std::thread;

use agentstategraph_tasks::{Priority, TaskStoreError};

use common::make_store;

/// Spawn N threads that each add one task to the same plan concurrently.
/// All must succeed, and all resulting task ids must be unique.
#[test]
fn concurrent_add_task_produces_unique_ids() {
    const THREAD_COUNT: usize = 8;

    let (_repo, store) = make_store("/plans");
    let store = Arc::new(store);

    store.create_plan("main", "race", None).unwrap();

    let handles: Vec<_> = (0..THREAD_COUNT)
        .map(|i| {
            let store = Arc::clone(&store);
            thread::spawn(move || {
                store.add_task(
                    "main",
                    "race",
                    &format!("task-{}", i),
                    Priority::Medium,
                    None,
                    vec![],
                    None,
                )
            })
        })
        .collect();

    let mut ids = Vec::new();
    for handle in handles {
        let task = handle
            .join()
            .expect("thread panicked")
            .expect("add_task failed");
        ids.push(task.id);
    }

    ids.sort();
    ids.dedup();
    assert_eq!(
        ids.len(),
        THREAD_COUNT,
        "expected {} unique task ids, got {}: {:?}",
        THREAD_COUNT,
        ids.len(),
        ids,
    );
}

/// Verify that `create_plan` under concurrent pressure also produces exactly
/// one winner and returns `PlanAlreadyExists` for every other racer.
#[test]
fn concurrent_create_plan_exactly_one_winner() {
    const THREAD_COUNT: usize = 8;

    let (_repo, store) = make_store("/plans");
    let store = Arc::new(store);

    let handles: Vec<_> = (0..THREAD_COUNT)
        .map(|_| {
            let store = Arc::clone(&store);
            thread::spawn(move || store.create_plan("main", "contested", None))
        })
        .collect();

    let mut successes = 0usize;
    let mut already_exists = 0usize;

    for handle in handles {
        match handle.join().expect("thread panicked") {
            Ok(_) => successes += 1,
            Err(TaskStoreError::PlanAlreadyExists(_)) => already_exists += 1,
            Err(e) => panic!("unexpected error: {}", e),
        }
    }

    assert_eq!(successes, 1, "exactly one thread should win");
    assert_eq!(
        already_exists,
        THREAD_COUNT - 1,
        "all others should see AlreadyExists"
    );
}

/// A write-conflict (MAX_CAS_RETRIES exhausted) must not panic — it should
/// surface as `TaskStoreError::WriteConflict`. This is hard to trigger
/// deterministically in a unit test (it would require a pathological
/// adversary), so we just confirm the variant exists and can be matched.
#[test]
fn write_conflict_variant_is_matchable() {
    let err = TaskStoreError::WriteConflict;
    assert!(matches!(err, TaskStoreError::WriteConflict));
    // Display should mention "write conflict"
    let msg = err.to_string().to_lowercase();
    assert!(msg.contains("write conflict"), "message was: {}", msg);
}
