//! `TaskStore::with_context`: every plan commit carries the host's
//! provenance (agent, principal, device), read back through the log.

mod common;

use agentstategraph::CallContext;
use agentstategraph_core::IntentCategory;
use agentstategraph_tasks::{Priority, Proof, TaskStore};

use common::make_repo;

const DEVICE: &str = "0123456789abcdef0123456789abcdef";

#[test]
fn every_plan_commit_carries_the_context() {
    let repo = make_repo();
    let ctx = CallContext::new("agent/claude")
        .with_principal("human/owner")
        .with_device(DEVICE);
    let store = TaskStore::with_context(repo.clone(), "/plans", ctx);
    assert_eq!(store.agent_id(), "agent/claude");
    assert_eq!(store.context().device.as_deref(), Some(DEVICE));

    let last = || repo.log("main", 1).unwrap().remove(0);
    let check = |what: &str| {
        let c = last();
        assert_eq!(c.intent.category, IntentCategory::Plan, "{what}");
        assert_eq!(c.agent_id, "agent/claude", "{what}");
        assert_eq!(c.authority.principal, "human/owner", "{what}");
        assert_eq!(c.device.as_deref(), Some(DEVICE), "{what}");
        assert_eq!(c.compute_id(), c.id, "{what}");
    };

    store.create_plan("main", "p", None).unwrap();
    check("create");
    let task = store
        .add_task("main", "p", "a", Priority::High, None, vec![], None)
        .unwrap();
    check("add");
    assert_eq!(task.created_by, "agent/claude");
    let started = store.start_task("main", "p", &task.id).unwrap();
    check("start");
    assert_eq!(started.started_by.as_deref(), Some("agent/claude"));
    let done = store
        .complete_task("main", "p", &task.id, Proof::commit("abc123"))
        .unwrap();
    check("done");
    assert_eq!(done.completed_by.as_deref(), Some("agent/claude"));
    let plan = store.close_plan("main", "p", "shipped").unwrap();
    check("close");
    assert_eq!(plan.closed_by.as_deref(), Some("agent/claude"));

    // Every plan commit in the history has it, not just the last.
    let plan_commits: Vec<_> = repo
        .log("main", 50)
        .unwrap()
        .into_iter()
        .filter(|c| c.intent.category == IntentCategory::Plan)
        .collect();
    assert_eq!(plan_commits.len(), 5);
    assert!(
        plan_commits
            .iter()
            .all(|c| c.device.as_deref() == Some(DEVICE))
    );
}

#[test]
fn a_store_from_new_records_no_device() {
    let repo = make_repo();
    let store = TaskStore::new(repo.clone(), "/plans", "test-agent");
    store.create_plan("main", "p", None).unwrap();
    let c = repo.log("main", 1).unwrap().remove(0);
    assert_eq!(c.agent_id, "test-agent");
    assert_eq!(c.authority.principal, "test-agent");
    assert_eq!(c.device, None);
}
