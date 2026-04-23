//! Taint / quarantine / watch wasm-bindgen binding tests (0.7.75 §9d).
//!
//! Mirrors the §9 pattern used by `tests/policy.rs`: every scenario
//! routes through the `WasmAgentStateGraph` wrappers and asserts the
//! JSON payload JS sees on the wire. `IndexedDbStorage` is
//! `MemoryStorage`-backed under `wasm-pack test --node`, so the
//! browser runtime isn't needed.

#![cfg(target_arch = "wasm32")]

use agentstategraph_wasm::WasmAgentStateGraph;

use wasm_bindgen_test::*;

fn new_asg() -> WasmAgentStateGraph {
    let asg = WasmAgentStateGraph::new(Some("wasm-taint-test".to_string())).expect("new");
    // Seed an initial commit so `main` resolves for any downstream
    // taint intent commits that need a head.
    asg.set(
        "/_bootstrap",
        "true",
        "Checkpoint",
        "bootstrap",
        None,
        None,
        None,
    )
    .expect("bootstrap");
    asg
}

#[wasm_bindgen_test]
fn taint_round_trip_list_and_resolve() {
    let asg = new_asg();
    let params = serde_json::json!({
        "name": "disk-pressure",
        "effect": "warn",
        "reason": "disk > 90%",
        "severity": "high",
        "propagate": true,
        "agent_id": "agent/ops",
    })
    .to_string();
    let id = asg
        .taint("main", "/cluster/nodes/picoup2", &params)
        .expect("taint");
    assert!(!id.is_empty());

    // Listed while active.
    let list_json = asg
        .list_taints(None, Some("taint".into()), false)
        .expect("list");
    let list: Vec<serde_json::Value> = serde_json::from_str(&list_json).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["name"], "disk-pressure");
    assert_eq!(list[0]["kind"], "taint");
    assert_eq!(list[0]["effect"], "warn");
    assert_eq!(list[0]["severity"], "high");
    assert!(list[0]["resolved_at"].is_null());

    // Resolve.
    let untaint_params = serde_json::json!({
        "reason": "freed space",
        "proof": "commit:abc123",
        "agent_id": "agent/ops",
    })
    .to_string();
    asg.untaint(
        "main",
        "/cluster/nodes/picoup2",
        "disk-pressure",
        &untaint_params,
    )
    .expect("untaint");

    let active_json = asg
        .list_taints(None, Some("taint".into()), false)
        .expect("list active");
    let active: Vec<serde_json::Value> = serde_json::from_str(&active_json).unwrap();
    assert!(
        active.is_empty(),
        "resolved taint should be hidden: {active:?}"
    );

    let all_json = asg
        .list_taints(None, Some("taint".into()), true)
        .expect("list all");
    let all: Vec<serde_json::Value> = serde_json::from_str(&all_json).unwrap();
    assert_eq!(all.len(), 1);
    assert!(!all[0]["resolved_at"].is_null());
    assert_eq!(all[0]["resolved_reason"], "freed space");
    assert_eq!(all[0]["resolved_proof"], "commit:abc123");
}

#[wasm_bindgen_test]
fn quarantine_round_trip_and_authorized_agents() {
    let asg = new_asg();
    let params = serde_json::json!({
        "name": "security-review",
        "reason": "pending audit",
        "severity": "critical",
        "authorized_agents": ["agent/security", "human/sre-lead"],
        "propagate": true,
        "agent_id": "agent/sec",
    })
    .to_string();
    let id = asg
        .quarantine("main", "/secrets/prod", &params)
        .expect("quarantine");
    assert!(!id.is_empty());

    let list_json = asg
        .list_taints(Some("/secrets".into()), Some("quarantine".into()), false)
        .expect("list");
    let list: Vec<serde_json::Value> = serde_json::from_str(&list_json).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["kind"], "quarantine");
    assert_eq!(list[0]["severity"], "critical");
    let auth = list[0]["metadata"]["authorized_agents"]
        .as_array()
        .expect("authorized_agents metadata");
    let auth_strs: Vec<&str> = auth.iter().map(|v| v.as_str().unwrap()).collect();
    assert!(auth_strs.contains(&"agent/security"));
    assert!(auth_strs.contains(&"human/sre-lead"));

    // Release.
    let release = serde_json::json!({
        "reason": "audit cleared",
        "proof": null,
        "agent_id": "agent/sec",
    })
    .to_string();
    asg.unquarantine("main", "/secrets/prod", "security-review", &release)
        .expect("unquarantine");
    let still_active: Vec<serde_json::Value> = serde_json::from_str(
        &asg.list_taints(None, Some("quarantine".into()), false)
            .unwrap(),
    )
    .unwrap();
    assert!(still_active.is_empty());
}

#[wasm_bindgen_test]
fn watch_round_trip_with_metric_and_direction() {
    let asg = new_asg();
    let params = serde_json::json!({
        "name": "latency-slo",
        "reason": "p99 above target",
        "metric": "http_latency_p99_ms",
        "threshold": 250.0,
        "direction": "above",
        "check_interval_secs": 60,
        "severity": "medium",
        "propagate": false,
        "agent_id": "agent/observability",
    })
    .to_string();
    let id = asg.watch("main", "/services/api", &params).expect("watch");
    assert!(!id.is_empty());

    let list: Vec<serde_json::Value> =
        serde_json::from_str(&asg.list_taints(None, Some("watch".into()), false).unwrap()).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["kind"], "watch");
    // Watches are advisory by default in the storage row.
    assert_eq!(list[0]["effect"], "advisory");
    assert_eq!(list[0]["metadata"]["metric"], "http_latency_p99_ms");
    assert_eq!(list[0]["metadata"]["direction"], "above");
    // threshold may serialize as f64 or i64 depending on the value; just
    // confirm presence.
    assert!(list[0]["metadata"]["threshold"].is_number());

    // Unwatch.
    let un = serde_json::json!({
        "reason": "dashboard deprecated",
        "agent_id": "agent/observability",
    })
    .to_string();
    asg.unwatch("main", "/services/api", "latency-slo", &un)
        .expect("unwatch");
    let actives: Vec<serde_json::Value> =
        serde_json::from_str(&asg.list_taints(None, Some("watch".into()), false).unwrap()).unwrap();
    assert!(actives.is_empty());
}

#[wasm_bindgen_test]
fn list_taints_filters_by_kind_and_prefix() {
    let asg = new_asg();
    // Apply one of each kind across two prefixes.
    let t = serde_json::json!({
        "name": "flaky", "effect": "warn", "reason": "x", "agent_id": "a",
    })
    .to_string();
    asg.taint("main", "/apps/foo", &t).unwrap();

    let q = serde_json::json!({
        "name": "lock", "reason": "x", "authorized_agents": [], "agent_id": "a",
    })
    .to_string();
    asg.quarantine("main", "/apps/bar", &q).unwrap();

    let w = serde_json::json!({
        "name": "watch1", "reason": "x", "agent_id": "a",
    })
    .to_string();
    asg.watch("main", "/infra/node-3", &w).unwrap();

    // Filter by kind.
    let only_taints: Vec<serde_json::Value> =
        serde_json::from_str(&asg.list_taints(None, Some("taint".into()), false).unwrap()).unwrap();
    assert_eq!(only_taints.len(), 1);
    assert_eq!(only_taints[0]["name"], "flaky");

    // Filter by prefix — only `/apps/*`.
    let apps_only: Vec<serde_json::Value> =
        serde_json::from_str(&asg.list_taints(Some("/apps".into()), None, false).unwrap()).unwrap();
    let mut kinds: Vec<&str> = apps_only
        .iter()
        .map(|v| v.get("kind").unwrap().as_str().unwrap())
        .collect();
    kinds.sort();
    assert_eq!(kinds, vec!["quarantine", "taint"]);

    // Unknown kind is rejected.
    assert!(asg.list_taints(None, Some("bogus".into()), false).is_err());
}

#[wasm_bindgen_test]
fn check_taint_surfaces_full_status() {
    let asg = new_asg();
    let block = serde_json::json!({
        "name": "corrupt",
        "effect": "block",
        "reason": "checksum mismatch",
        "severity": "critical",
        "propagate": true,
        "agent_id": "agent/integrity",
    })
    .to_string();
    asg.taint("main", "/data/table-7", &block).unwrap();

    let check_json = asg
        .check_taint("/data/table-7", Some("agent/reader".into()), Some(0.9))
        .expect("check");
    let check: serde_json::Value = serde_json::from_str(&check_json).unwrap();
    assert_eq!(check["tainted"], true);
    assert_eq!(check["can_write"], false);
    let taints = check["taints"].as_array().expect("taints array");
    assert_eq!(taints.len(), 1);
    assert_eq!(taints[0]["name"], "corrupt");

    // A path that has no taint: everything false / empty.
    let clean: serde_json::Value =
        serde_json::from_str(&asg.check_taint("/data/clean", None, None).unwrap()).unwrap();
    assert_eq!(clean["tainted"], false);
    assert_eq!(clean["quarantined"], false);
    assert_eq!(clean["watched"], false);
    assert_eq!(clean["can_write"], true);
}

#[wasm_bindgen_test]
fn taint_params_validation_errors() {
    let asg = new_asg();
    // Missing `name`.
    let bad = serde_json::json!({
        "effect": "warn", "reason": "r", "agent_id": "a",
    })
    .to_string();
    let err = asg.taint("main", "/x", &bad).unwrap_err();
    assert!(
        format!("{err:?}").contains("name"),
        "expected missing-name error, got {err:?}"
    );

    // Invalid effect.
    let bad2 = serde_json::json!({
        "name": "n", "effect": "nope", "reason": "r", "agent_id": "a",
    })
    .to_string();
    let err2 = asg.taint("main", "/x", &bad2).unwrap_err();
    assert!(
        format!("{err2:?}").contains("effect"),
        "expected effect error, got {err2:?}"
    );

    // Malformed JSON.
    let err3 = asg.taint("main", "/x", "{not json").unwrap_err();
    assert!(format!("{err3:?}").contains("invalid params"));
}

#[wasm_bindgen_test]
fn quarantine_authorized_agent_can_write() {
    // Quarantine restricts non-authorized agents but allows authorized
    // ones; `check_taint` must surface that decision through the wire
    // form JS sees.
    let asg = new_asg();
    let q = serde_json::json!({
        "name": "audit",
        "reason": "pending review",
        "severity": "high",
        "authorized_agents": ["agent/security"],
        "propagate": true,
        "agent_id": "agent/security",
    })
    .to_string();
    asg.quarantine("main", "/vault/keys", &q)
        .expect("quarantine");

    let outsider: serde_json::Value = serde_json::from_str(
        &asg.check_taint("/vault/keys", Some("agent/reader".into()), Some(1.0))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(outsider["quarantined"], true);
    assert_eq!(outsider["can_write"], false);
    let auth = outsider["authorized_agents"].as_array().unwrap();
    assert_eq!(auth.len(), 1);
    assert_eq!(auth[0], "agent/security");

    let allowed: serde_json::Value = serde_json::from_str(
        &asg.check_taint("/vault/keys", Some("agent/security".into()), Some(1.0))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(allowed["quarantined"], true);
    assert_eq!(allowed["can_write"], true);
}
