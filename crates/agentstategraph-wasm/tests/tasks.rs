//! Closing a plan through the wasm-bindgen task methods. Native, like
//! `persistence.rs`: the success path is plain Rust. The refusals build a
//! `JsValue`, which only works on wasm32; the tasks crate and the other
//! bindings test those.

#![cfg(not(target_arch = "wasm32"))]

use agentstategraph_wasm::WasmAgentStateGraph;
use serde_json::Value;

fn parse(s: String) -> Value {
    serde_json::from_str(&s).unwrap()
}

#[test]
fn a_plan_closes_with_a_summary_after_its_last_task() {
    let asg = WasmAgentStateGraph::new(None).unwrap();
    let (prefix, agent) = ("/plans", "wasm-test");
    asg.tasks_create_plan(prefix, agent, "main", "p", None)
        .unwrap();
    let task = parse(
        asg.tasks_add_task(prefix, agent, "main", "p", "x", "high", None, None, None)
            .unwrap(),
    );
    let id = task["id"].as_str().unwrap();
    asg.tasks_start_task(prefix, agent, "main", "p", id)
        .unwrap();
    asg.tasks_complete_task(prefix, agent, "main", "p", id, "commit", "abc123", None)
        .unwrap();

    let open = parse(asg.tasks_get_plan(prefix, agent, "main", "p").unwrap());
    assert_eq!(open["status"], "active");
    let closed = parse(
        asg.tasks_close_plan(prefix, agent, "main", "p", "shipped")
            .unwrap(),
    );
    assert_eq!(closed["status"], "completed");
    assert_eq!(closed["summary"], "shipped");
    assert_eq!(closed["closed_by"], "wasm-test");
    assert!(closed["closed_at"].is_string());
}
