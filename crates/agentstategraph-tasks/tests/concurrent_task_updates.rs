//! Task transitions read the whole task, change a field, and write the whole
//! task back. Two concurrent transitions on one task must both survive; before
//! the fix the ref move was a CAS but the value written was the stale read, so
//! one update was silently reverted while both calls returned Ok (data-loss
//! audit, 2026-10: 40 of 40 tasks).
mod common;

use std::sync::{Arc, Barrier};
use std::thread;

use agentstategraph_tasks::{Priority, Proof, TaskStatus};

use common::make_store;

#[test]
fn concurrent_complete_and_assign_both_survive() {
    const TASKS: usize = 40;
    let (_repo, store) = make_store("/plans");
    let store = Arc::new(store);
    store.create_plan("main", "p", None).unwrap();
    let ids: Vec<_> = (0..TASKS)
        .map(|i| {
            let t = store
                .add_task(
                    "main",
                    "p",
                    &format!("t{i}"),
                    Priority::Medium,
                    None,
                    vec![],
                    None,
                )
                .unwrap();
            store.start_task("main", "p", &t.id).unwrap();
            t.id
        })
        .collect();

    let mut lost = Vec::new();
    for id in &ids {
        let barrier = Arc::new(Barrier::new(2));
        let (s1, b1, i1) = (Arc::clone(&store), Arc::clone(&barrier), id.clone());
        let completer = thread::spawn(move || {
            b1.wait();
            s1.complete_task("main", "p", &i1, Proof::commit("abc"))
                .map(|_| ())
        });
        let (s2, b2, i2) = (Arc::clone(&store), Arc::clone(&barrier), id.clone());
        let assigner = thread::spawn(move || {
            b2.wait();
            s2.assign_task("main", "p", &i2, "agent-b").map(|_| ())
        });
        completer
            .join()
            .unwrap()
            .expect("complete_task returned an error");
        assigner
            .join()
            .unwrap()
            .expect("assign_task returned an error");

        let t = store.get_task("main", "p", id).unwrap();
        if t.status != TaskStatus::Done || t.assigned_to.as_deref() != Some("agent-b") {
            lost.push(format!(
                "{}: status={:?} assigned_to={:?}",
                id.0, t.status, t.assigned_to
            ));
        }
    }
    assert!(
        lost.is_empty(),
        "{} of {TASKS} tasks lost an acknowledged update: {:?}",
        lost.len(),
        &lost[..lost.len().min(5)]
    );
}
