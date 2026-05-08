//! Tests for the POLICY_V1.md §22.4 additive fields on `Task`:
//! `payload`, `parent_change`, `on_complete`.
//!
//! These fields MUST be backward-compatible: legacy JSON deserializes
//! with `None` in every new slot, and tasks that do not use the new
//! fields serialize byte-identically to the pre-extension shape.

mod common;

use agentstategraph_tasks::{AddTaskOptions, OnCompleteHook, Priority, Task, TaskId};
use serde_json::json;

use common::make_store;

fn add_default_task(store: &agentstategraph_tasks::TaskStore, plan: &str) -> Task {
    store
        .add_task("main", plan, "seed", Priority::Medium, None, vec![], None)
        .unwrap()
}

#[test]
fn test_task_with_payload_roundtrips() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();

    let payload = json!({
        "proposal": {
            "action": "rollback_deployment",
            "tokens": ["destructive", "prod"],
            "preferred_option": "spec-abc",
        }
    });

    let task = store
        .add_task_with_extensions(
            "main",
            "p",
            "approve rollback",
            Priority::High,
            None,
            vec![],
            Some("oncall".into()),
            AddTaskOptions { payload: Some(payload.clone()), ..Default::default() },
        )
        .unwrap();

    assert_eq!(task.payload.as_ref(), Some(&payload));

    let fetched = store.get_task("main", "p", &task.id).unwrap();
    assert_eq!(fetched, task);
    assert_eq!(fetched.payload, Some(payload));
}

#[test]
fn test_task_with_parent_change_roundtrips() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();

    let task = store
        .add_task_with_extensions(
            "main",
            "p",
            "review deferred change",
            Priority::Medium,
            None,
            vec![],
            None,
            AddTaskOptions { parent_change: Some("spec-deadbeef".into()), ..Default::default() },
        )
        .unwrap();

    assert_eq!(task.parent_change.as_deref(), Some("spec-deadbeef"));

    let fetched = store.get_task("main", "p", &task.id).unwrap();
    assert_eq!(fetched, task);
}

#[test]
fn test_task_on_complete_hook_promote_change_serializes() {
    let hook = OnCompleteHook::PromoteChange;
    let v = serde_json::to_value(&hook).unwrap();
    assert_eq!(v, json!({ "kind": "promote_change" }));

    let back: OnCompleteHook = serde_json::from_value(v).unwrap();
    assert_eq!(back, hook);
}

#[test]
fn test_task_on_complete_hook_named_serializes() {
    let hook = OnCompleteHook::Named {
        name: "notify-slack".into(),
    };
    let v = serde_json::to_value(&hook).unwrap();
    assert_eq!(v, json!({ "kind": "named", "name": "notify-slack" }));

    let back: OnCompleteHook = serde_json::from_value(v).unwrap();
    assert_eq!(back, hook);
}

#[test]
fn test_legacy_task_without_new_fields_deserializes() {
    // Shape as written by pre-extension versions of the crate.
    // Intentionally hardcoded — do NOT regenerate from the current
    // `Task` struct or this test loses its meaning.
    let legacy = json!({
        "id": "t-001",
        "title": "legacy",
        "status": "pending",
        "priority": "medium",
        "parent_id": null,
        "blocked_by": [],
        "created_at": "2026-04-16T09:00:00Z",
        "created_by": "claude-code",
        "started_at": null,
        "started_by": null,
        "completed_at": null,
        "completed_by": null,
        "proof": null,
        "abandoned_at": null,
        "abandoned_reason": null
    });

    let task: Task = serde_json::from_value(legacy).unwrap();
    assert_eq!(task.id, TaskId::new(1));
    assert_eq!(task.title, "legacy");
    assert_eq!(task.assigned_to, None);
    assert_eq!(task.payload, None);
    assert_eq!(task.parent_change, None);
    assert_eq!(task.on_complete, None);
}

#[test]
fn test_add_task_with_extensions_builder() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();

    let payload = json!({ "proposal": "spec-xyz" });
    let task = store
        .add_task_with_extensions(
            "main",
            "p",
            "full ext",
            Priority::Critical,
            None,
            vec![],
            Some("agent-a".into()),
            AddTaskOptions {
                payload: Some(payload.clone()),
                parent_change: Some("spec-xyz".into()),
                on_complete: Some(OnCompleteHook::PromoteChange),
            },
        )
        .unwrap();

    assert_eq!(task.title, "full ext");
    assert_eq!(task.priority, Priority::Critical);
    assert_eq!(task.assigned_to.as_deref(), Some("agent-a"));
    assert_eq!(task.payload, Some(payload));
    assert_eq!(task.parent_change.as_deref(), Some("spec-xyz"));
    assert_eq!(task.on_complete, Some(OnCompleteHook::PromoteChange));

    let fetched = store.get_task("main", "p", &task.id).unwrap();
    assert_eq!(fetched, task);
}

#[test]
fn test_new_fields_omitted_from_json_when_none() {
    // A task with none of the new fields set must serialize without
    // `"payload": null`, `"parent_change": null`, or `"on_complete": null`
    // keys — i.e. byte-identical to how the struct would have
    // serialized before the extension landed.
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();
    let task = add_default_task(&store, "p");

    let v = serde_json::to_value(&task).unwrap();
    let obj = v.as_object().expect("task serializes to a JSON object");

    assert!(
        !obj.contains_key("payload"),
        "payload key should be omitted when None, got: {}",
        v
    );
    assert!(
        !obj.contains_key("parent_change"),
        "parent_change key should be omitted when None, got: {}",
        v
    );
    assert!(
        !obj.contains_key("on_complete"),
        "on_complete key should be omitted when None, got: {}",
        v
    );
    // assigned_to uses the same skip-None contract and is the
    // previous-generation field, so it should also be absent here —
    // proving the shape is identical to the pre-extension struct.
    assert!(
        !obj.contains_key("assigned_to"),
        "assigned_to should stay omitted when None, got: {}",
        v
    );
}
