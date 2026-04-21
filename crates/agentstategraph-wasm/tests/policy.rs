//! PolicyStore wasm-bindgen binding tests.
//!
//! Mirrors the Python `test_policy.py` scenario set end-to-end through
//! the `WasmPolicyStore` wrapper. Runs under `wasm-bindgen-test` on a
//! `MemoryStorage`-backed repository so the browser runtime (IndexedDB)
//! is not required. Also audits the 0.6.5 Session + 0.6.0 Task
//! extension-field round-trip paths per §6 of the 0.7.0-beta.1 plan.

#![cfg(target_arch = "wasm32")]

use std::sync::Arc;

use agentstategraph::Repository;
use agentstategraph_storage::MemoryStorage;
use agentstategraph_tasks::TaskStore;
use agentstategraph_wasm::WasmPolicyStore;

use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

fn new_repo() -> Arc<Repository> {
    let repo = Repository::new(Box::new(MemoryStorage::new()));
    repo.init().expect("init");
    // Seed an initial commit so `main` is resolvable and session
    // creation can pick up a head.
    repo.set_json(
        "main",
        "/_bootstrap",
        &serde_json::json!({"ok": true}),
        agentstategraph::CommitOptions::new(
            "test",
            agentstategraph_core::IntentCategory::Checkpoint,
            "bootstrap",
        ),
    )
    .expect("bootstrap commit");
    Arc::new(repo)
}

fn new_store() -> WasmPolicyStore {
    WasmPolicyStore::from_repo(new_repo(), "/policies", "wasm-test")
}

fn policy_json(path: &str, extras: serde_json::Value) -> String {
    let mut v = serde_json::json!({
        "path": path,
        "version": 1,
        "situation": format!("situation for {path}"),
        "situation_selector": {"kind": "always"},
        "allow": [],
        "deny": [],
        "require_approval": [],
        "triggers": [],
        "required_fields": [],
        "severity": "low",
        "proposed_by": "wasm-test",
        "proposed_at": "2026-04-18T00:00:00Z",
        "active_from": "2026-04-18T00:00:00Z",
    });
    if let (serde_json::Value::Object(base), serde_json::Value::Object(more)) = (&mut v, extras) {
        for (k, val) in more {
            base.insert(k, val);
        }
    }
    serde_json::to_string(&v).unwrap()
}

#[wasm_bindgen_test]
fn propose_creates_unratified_policy() {
    let ps = new_store();
    let handle = ps
        .propose(
            "main",
            &policy_json("infra/k8s/pod-failing", serde_json::json!({})),
        )
        .unwrap();
    assert_eq!(handle, "infra/k8s/pod-failing@1");

    let fetched_json = ps.get("main", "infra/k8s/pod-failing", None).unwrap();
    let fetched: serde_json::Value = serde_json::from_str(&fetched_json).unwrap();
    assert_eq!(fetched["version"], 1);
    assert!(fetched["ratified_by"].is_null());
    assert_eq!(fetched["proposed_by"], "wasm-test");
}

#[wasm_bindgen_test]
fn ratify_promotes_policy() {
    let ps = new_store();
    ps.propose(
        "main",
        &policy_json(
            "infra/restart",
            serde_json::json!({"allow": [{"action": "restart_pod"}]}),
        ),
    )
    .unwrap();
    ps.ratify("main", "infra/restart", "ops-lead", "approved after review")
        .unwrap();
    let fetched: serde_json::Value =
        serde_json::from_str(&ps.get("main", "infra/restart", None).unwrap()).unwrap();
    assert_eq!(fetched["ratified_by"], "ops-lead");
    assert_eq!(fetched["ratification_reasoning"], "approved after review");
    assert!(!fetched["ratified_at"].is_null());
}

#[wasm_bindgen_test]
fn supersede_chain_and_history() {
    let ps = new_store();
    ps.propose(
        "main",
        &policy_json(
            "infra/scale",
            serde_json::json!({"allow": [{"action": "scale_up"}]}),
        ),
    )
    .unwrap();
    ps.ratify("main", "infra/scale", "ops", "v1").unwrap();

    let new_v = policy_json(
        "infra/scale",
        serde_json::json!({
            "allow": [{"action": "scale_up"}, {"action": "scale_down"}],
            "ratified_by": "ops",
            "ratified_at": "2026-04-18T01:00:00Z",
        }),
    );
    let handle = ps.supersede("main", "infra/scale", &new_v).unwrap();
    assert_eq!(handle, "infra/scale@2");

    let hist_json = ps.history("main", "infra/scale").unwrap();
    let hist: Vec<serde_json::Value> = serde_json::from_str(&hist_json).unwrap();
    let versions: Vec<i64> = hist
        .iter()
        .map(|p| p["version"].as_i64().unwrap())
        .collect();
    assert_eq!(versions, vec![1, 2]);
    assert_eq!(hist.last().unwrap()["supersedes"], "infra/scale@1");
}

#[wasm_bindgen_test]
fn evaluate_allow() {
    let ps = new_store();
    ps.propose(
        "main",
        &policy_json(
            "infra/restart",
            serde_json::json!({
                "allow": [{"action": "restart_pod"}],
                "situation_selector": {"kind": "eq", "key": "namespace", "value": "prod"},
            }),
        ),
    )
    .unwrap();
    ps.ratify("main", "infra/restart", "ops", "ok").unwrap();

    let d_json = ps
        .evaluate(
            "main",
            &serde_json::json!({"namespace": "prod"}).to_string(),
            "restart_pod",
            "agent-1",
            None,
        )
        .unwrap();
    let d: serde_json::Value = serde_json::from_str(&d_json).unwrap();
    assert_eq!(d["kind"], "allow");
    assert_eq!(d["matched_policy"], "infra/restart@1");
}

#[wasm_bindgen_test]
fn evaluate_deny() {
    let ps = new_store();
    ps.propose(
        "main",
        &policy_json(
            "infra/no-delete",
            serde_json::json!({"deny": [{"action": "delete_node", "condition": "always"}]}),
        ),
    )
    .unwrap();
    ps.ratify("main", "infra/no-delete", "ops", "ok").unwrap();

    let d: serde_json::Value = serde_json::from_str(
        &ps.evaluate("main", "{}", "delete_node", "agent-1", None)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(d["kind"], "deny");
}

#[wasm_bindgen_test]
fn evaluate_require_approval() {
    let ps = new_store();
    ps.propose(
        "main",
        &policy_json(
            "infra/risky",
            serde_json::json!({
                "require_approval": [{
                    "action": "truncate_index",
                    "approvers": ["human"],
                    "fallback": {"kind": "block"},
                }],
            }),
        ),
    )
    .unwrap();
    ps.ratify("main", "infra/risky", "ops", "ok").unwrap();

    let d: serde_json::Value = serde_json::from_str(
        &ps.evaluate("main", "{}", "truncate_index", "agent-1", None)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(d["kind"], "require_approval");
    assert_eq!(d["approvers"], serde_json::json!(["human"]));
    assert_eq!(d["fallback"]["kind"], "block");
}

#[wasm_bindgen_test]
fn evaluate_no_match() {
    let ps = new_store();
    let d: serde_json::Value = serde_json::from_str(
        &ps.evaluate("main", "{}", "anything", "agent-1", None)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(d["kind"], "no_policy_match");
}

#[wasm_bindgen_test]
fn evaluate_change_with_triggers_and_fallback() {
    let ps = new_store();
    ps.propose(
        "main",
        &policy_json(
            "infra/high-cost",
            serde_json::json!({
                "triggers": ["reindex", "downtime"],
                "required_fields": ["estimated_downtime"],
                "require_approval": [{
                    "action": "promote",
                    "approvers": ["human"],
                    "fallback": {"kind": "lowest_risk_alternative"},
                }],
                "severity": "high",
            }),
        ),
    )
    .unwrap();
    ps.ratify(
        "main",
        "infra/high-cost",
        "ops",
        "big changes need approval",
    )
    .unwrap();

    let proposal = serde_json::json!({
        "action": "promote",
        "agent_id": "agent-1",
        "intent": "merge option C",
        "preferred_option": "spec-7",
        "alternatives": ["spec-1", "spec-3"],
        "tokens": ["reindex"],
        "attached_fields": {"estimated_downtime": "5m"},
    })
    .to_string();

    let d: serde_json::Value =
        serde_json::from_str(&ps.evaluate_change("main", &proposal, None).unwrap()).unwrap();
    assert_eq!(d["kind"], "require_approval");
    assert_eq!(d["fallback"]["kind"], "lowest_risk_alternative");
}

#[wasm_bindgen_test]
fn check_tokens_filters_by_trigger_intersection() {
    let ps = new_store();
    ps.propose(
        "main",
        &policy_json(
            "infra/with-reindex",
            serde_json::json!({"triggers": ["reindex"]}),
        ),
    )
    .unwrap();
    ps.ratify("main", "infra/with-reindex", "ops", "ok")
        .unwrap();
    ps.propose(
        "main",
        &policy_json(
            "infra/with-network",
            serde_json::json!({"triggers": ["network"]}),
        ),
    )
    .unwrap();
    ps.ratify("main", "infra/with-network", "ops", "ok")
        .unwrap();

    let matched: Vec<serde_json::Value> =
        serde_json::from_str(&ps.check_tokens("main", r#"["reindex"]"#).unwrap()).unwrap();
    let mut paths: Vec<String> = matched
        .iter()
        .map(|p| p["path"].as_str().unwrap().to_string())
        .collect();
    paths.sort();
    assert_eq!(paths, vec!["infra/with-reindex"]);

    let matched_all: Vec<serde_json::Value> = serde_json::from_str(
        &ps.check_tokens("main", r#"["reindex", "network"]"#)
            .unwrap(),
    )
    .unwrap();
    let mut all_paths: Vec<String> = matched_all
        .iter()
        .map(|p| p["path"].as_str().unwrap().to_string())
        .collect();
    all_paths.sort();
    assert_eq!(all_paths, vec!["infra/with-network", "infra/with-reindex"]);
}

#[wasm_bindgen_test]
fn list_and_active_filters() {
    let ps = new_store();
    ps.propose("main", &policy_json("infra/a", serde_json::json!({})))
        .unwrap();
    ps.propose("main", &policy_json("infra/b", serde_json::json!({})))
        .unwrap();
    ps.ratify("main", "infra/b", "ops", "ok").unwrap();

    let listed: Vec<serde_json::Value> =
        serde_json::from_str(&ps.list("main", None, None).unwrap()).unwrap();
    let mut listed_paths: Vec<String> = listed
        .iter()
        .map(|p| p["path"].as_str().unwrap().to_string())
        .collect();
    listed_paths.sort();
    assert_eq!(listed_paths, vec!["infra/a", "infra/b"]);

    let actives: Vec<serde_json::Value> =
        serde_json::from_str(&ps.active("main", None, None).unwrap()).unwrap();
    let active_paths: Vec<String> = actives
        .iter()
        .map(|p| p["path"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(active_paths, vec!["infra/b"]);
}

#[wasm_bindgen_test]
fn evaluate_ignores_not_yet_active_policy() {
    // Mirrors §1 enforcement: ratified + active_from in the future ⇒
    // treated as not-yet-active.
    let ps = new_store();
    let future_pol = policy_json(
        "infra/future",
        serde_json::json!({
            "allow": [{"action": "do_it"}],
            // Far-future timestamp relative to any sensible test clock.
            "active_from": "2099-01-01T00:00:00Z",
        }),
    );
    ps.propose("main", &future_pol).unwrap();
    ps.ratify("main", "infra/future", "ops", "scheduled")
        .unwrap();

    let d: serde_json::Value =
        serde_json::from_str(&ps.evaluate("main", "{}", "do_it", "agent-1", None).unwrap())
            .unwrap();
    assert_eq!(d["kind"], "no_policy_match");

    let actives: Vec<serde_json::Value> =
        serde_json::from_str(&ps.active("main", None, None).unwrap()).unwrap();
    assert!(actives
        .iter()
        .all(|p| p["path"].as_str().unwrap() != "infra/future"));
}

// ---------------------------------------------------------------------------
// §6 audit: Session + Task extension round-trip through existing wrappers.
// ---------------------------------------------------------------------------

#[wasm_bindgen_test]
fn session_roundtrip_via_manager() {
    // Session and SessionStatus moved to agentstategraph-core in 0.6.5.
    // This test confirms they round-trip via the `Repository::sessions()`
    // path that the JS-facing create/get/end wrappers use.
    let repo = new_repo();
    let mgr = repo.sessions();
    let head = repo.log("main", 1).unwrap().into_iter().next().unwrap().id;
    let s = mgr
        .create(
            "agent/planner",
            "main",
            head,
            None,
            None,
            None,
            Some("/plans/".to_string()),
        )
        .unwrap();
    assert_eq!(s.agent_id, "agent/planner");
    assert_eq!(s.path_scope.as_deref(), Some("/plans/"));
    // JSON round-trip — the on-the-wire form JS sees.
    let encoded = serde_json::to_string(&s).unwrap();
    let decoded: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded["status"], "Active");
    assert!(decoded["ended_at"].is_null());

    mgr.end(&s.id, agentstategraph_core::SessionStatus::Completed)
        .unwrap();
    let reloaded = mgr.get(&s.id).unwrap().unwrap();
    let encoded2 = serde_json::to_string(&reloaded).unwrap();
    let decoded2: serde_json::Value = serde_json::from_str(&encoded2).unwrap();
    assert_eq!(decoded2["status"], "Completed");
    assert!(!decoded2["ended_at"].is_null());
}

#[wasm_bindgen_test]
fn task_extension_fields_roundtrip() {
    // Task.payload / parent_change / on_complete were added in 0.6.0.
    // The existing `tasksAddTask` wrapper doesn't take them, but the
    // `tasksAddTaskWithExtensions` wrapper added in §6 does, and the
    // returned Task JSON surfaces all three fields intact.
    let repo = new_repo();
    let store = TaskStore::new(repo.clone(), "/plans", "wasm-test");
    store.create_plan("main", "p", None).unwrap();

    let payload = Some(serde_json::json!({"proposal": {"preferred_option": "spec-7"}}));
    let on_complete = Some(agentstategraph_tasks::OnCompleteHook::PromoteChange);
    let task = store
        .add_task_with_extensions(
            "main",
            "p",
            "approve high-cost change",
            agentstategraph_tasks::Priority::High,
            None,
            Vec::new(),
            None,
            payload.clone(),
            Some("spec-7@42".to_string()),
            on_complete,
        )
        .unwrap();

    let encoded = serde_json::to_string(&task).unwrap();
    let decoded: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded["parent_change"], "spec-7@42");
    assert_eq!(
        decoded["payload"],
        serde_json::json!({"proposal": {"preferred_option": "spec-7"}})
    );
    assert_eq!(decoded["on_complete"]["kind"], "promote_change");

    // Named variant.
    let t2 = store
        .add_task_with_extensions(
            "main",
            "p",
            "custom hook",
            agentstategraph_tasks::Priority::Low,
            None,
            Vec::new(),
            None,
            None,
            None,
            Some(agentstategraph_tasks::OnCompleteHook::Named(
                "notify-slack".to_string(),
            )),
        )
        .unwrap();
    let enc2 = serde_json::to_string(&t2).unwrap();
    let dec2: serde_json::Value = serde_json::from_str(&enc2).unwrap();
    assert_eq!(dec2["on_complete"]["kind"], "named");
    assert_eq!(dec2["on_complete"]["name"], "notify-slack");

    // None variant (existing `add_task` path).
    let t3 = store
        .add_task(
            "main",
            "p",
            "plain",
            agentstategraph_tasks::Priority::Low,
            None,
            Vec::new(),
            None,
        )
        .unwrap();
    let enc3 = serde_json::to_string(&t3).unwrap();
    let dec3: serde_json::Value = serde_json::from_str(&enc3).unwrap();
    assert!(dec3["payload"].is_null());
    assert!(dec3["parent_change"].is_null());
    assert!(dec3["on_complete"].is_null());
}

// ---------------------------------------------------------------------------
// §5d: WASM pass-through for signing + multi-tenant + external evaluator.
//
// Mirrors the Python §5a pattern (5ddcd58). The three new optional
// Policy fields (`signature`, `tenant_id`, `external_evaluator`) and
// `Session.scope_tenant` auto-round-trip through serde because the
// WASM boundary is a JSON-string. `sign` / `verify` /
// `set_external_evaluator` are stubs returning `{"error": "not yet
// wired"}` envelopes; `evaluate` / `evaluate_change` / `active` /
// `list` gain an optional `tenant_filter` argument routed to the
// Rust `_scoped` variants.
// ---------------------------------------------------------------------------

#[wasm_bindgen_test]
fn test_wasm_policy_signature_field_round_trips() {
    let ps = new_store();
    let sig = serde_json::json!({
        "algorithm": "ed25519",
        "key_id": "ops-root-2026",
        "signature_b64": "YWJjZGVm",
        "signed_at": "2026-04-18T00:00:00Z",
    });
    let pol = policy_json(
        "infra/signed",
        serde_json::json!({ "signature": sig.clone() }),
    );
    ps.propose("main", &pol).unwrap();
    let fetched: serde_json::Value =
        serde_json::from_str(&ps.get("main", "infra/signed", None).unwrap()).unwrap();
    assert_eq!(fetched["signature"], sig);
    assert_eq!(fetched["signature"]["algorithm"], "ed25519");
    assert_eq!(fetched["signature"]["key_id"], "ops-root-2026");
}

#[wasm_bindgen_test]
fn test_wasm_policy_tenant_id_field_round_trips() {
    let ps = new_store();
    ps.propose(
        "main",
        &policy_json(
            "infra/acme-only",
            serde_json::json!({ "tenant_id": "acme" }),
        ),
    )
    .unwrap();
    let fetched: serde_json::Value =
        serde_json::from_str(&ps.get("main", "infra/acme-only", None).unwrap()).unwrap();
    assert_eq!(fetched["tenant_id"], "acme");

    // Global fallback: unset tenant_id serializes as null / missing.
    ps.propose("main", &policy_json("infra/global", serde_json::json!({})))
        .unwrap();
    let fetched2: serde_json::Value =
        serde_json::from_str(&ps.get("main", "infra/global", None).unwrap()).unwrap();
    assert!(
        fetched2["tenant_id"].is_null() || !fetched2.as_object().unwrap().contains_key("tenant_id"),
        "global policy should have no tenant_id, got {fetched2:?}"
    );
}

#[wasm_bindgen_test]
fn test_wasm_policy_external_evaluator_field_round_trips() {
    let ps = new_store();
    let ext = serde_json::json!({
        "kind": "webhook",
        "endpoint": "https://policy.example.com/evaluate",
        "timeout_ms": 2500,
    });
    ps.propose(
        "main",
        &policy_json(
            "infra/ext",
            serde_json::json!({ "external_evaluator": ext.clone() }),
        ),
    )
    .unwrap();
    let fetched: serde_json::Value =
        serde_json::from_str(&ps.get("main", "infra/ext", None).unwrap()).unwrap();
    assert_eq!(fetched["external_evaluator"], ext);
    assert_eq!(fetched["external_evaluator"]["kind"], "webhook");
}

#[wasm_bindgen_test]
fn test_wasm_evaluate_with_tenant_filter_scoped_policy() {
    // Policy scoped to `acme` should only be consulted when
    // tenant_filter is None or "acme"; filtering to a different tenant
    // excludes it, yielding no_policy_match.
    let ps = new_store();
    ps.propose(
        "main",
        &policy_json(
            "infra/acme-restart",
            serde_json::json!({
                "allow": [{"action": "restart_pod"}],
                "tenant_id": "acme",
            }),
        ),
    )
    .unwrap();
    ps.ratify("main", "infra/acme-restart", "ops", "ok")
        .unwrap();

    // Filter == "acme" → matches.
    let d: serde_json::Value = serde_json::from_str(
        &ps.evaluate(
            "main",
            "{}",
            "restart_pod",
            "agent-1",
            Some("acme".to_string()),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(d["kind"], "allow");
    assert_eq!(d["matched_policy"], "infra/acme-restart@1");

    // Filter == "globex" → excluded (tenant mismatch).
    let d2: serde_json::Value = serde_json::from_str(
        &ps.evaluate(
            "main",
            "{}",
            "restart_pod",
            "agent-1",
            Some("globex".to_string()),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(d2["kind"], "no_policy_match");

    // Filter == None → back-compat, all policies considered.
    let d3: serde_json::Value = serde_json::from_str(
        &ps.evaluate("main", "{}", "restart_pod", "agent-1", None)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(d3["kind"], "allow");
}

#[wasm_bindgen_test]
fn test_wasm_evaluate_with_tenant_filter_global_fallback() {
    // A global policy (no tenant_id) must still apply under any
    // tenant filter — the _scoped variant treats tenant_id == None as
    // applicable everywhere.
    let ps = new_store();
    ps.propose(
        "main",
        &policy_json(
            "infra/global-allow",
            serde_json::json!({
                "allow": [{"action": "read_config"}],
                // tenant_id intentionally omitted -> global policy.
            }),
        ),
    )
    .unwrap();
    ps.ratify("main", "infra/global-allow", "ops", "ok")
        .unwrap();

    for filter in [None, Some("acme".to_string()), Some("globex".to_string())] {
        let d: serde_json::Value = serde_json::from_str(
            &ps.evaluate("main", "{}", "read_config", "agent-1", filter.clone())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            d["kind"], "allow",
            "global policy should match under filter {filter:?}"
        );
    }

    // And `active` with tenant_filter still surfaces the global row.
    let actives_acme: Vec<serde_json::Value> =
        serde_json::from_str(&ps.active("main", None, Some("acme".to_string())).unwrap()).unwrap();
    let paths: Vec<&str> = actives_acme
        .iter()
        .map(|p| p["path"].as_str().unwrap())
        .collect();
    assert!(paths.contains(&"infra/global-allow"));
}

#[wasm_bindgen_test]
fn test_wasm_session_scope_tenant_field_round_trips() {
    // Session.scope_tenant landed alongside Policy.tenant_id in 0.7.5;
    // a ratification path that scopes a session to a tenant must
    // survive the JSON wire form JS sees via `createSession` /
    // `getSession`. We exercise the same manager path those wrappers
    // use (§6 audit test does similar for ended-at).
    use agentstategraph_core::Session;
    let repo = new_repo();
    let mgr = repo.sessions();
    let head = repo.log("main", 1).unwrap().into_iter().next().unwrap().id;
    let mut s: Session = mgr
        .create("agent/tenant-worker", "main", head, None, None, None, None)
        .unwrap();
    // No scope_tenant set initially → null on the wire.
    let encoded = serde_json::to_string(&s).unwrap();
    let decoded: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    assert!(
        decoded["scope_tenant"].is_null()
            || !decoded.as_object().unwrap().contains_key("scope_tenant"),
        "unset scope_tenant should be null/absent, got {decoded:?}"
    );

    // Set scope_tenant and re-serialize — the field must round-trip.
    s.scope_tenant = Some("acme".to_string());
    let encoded2 = serde_json::to_string(&s).unwrap();
    let decoded2: serde_json::Value = serde_json::from_str(&encoded2).unwrap();
    assert_eq!(decoded2["scope_tenant"], "acme");

    // And deserialization back into a Session preserves it.
    let reparsed: Session = serde_json::from_str(&encoded2).unwrap();
    assert_eq!(reparsed.scope_tenant.as_deref(), Some("acme"));
}

#[wasm_bindgen_test]
fn test_wasm_policystore_sign_returns_stub_envelope() {
    // All three §5d stubs return the same `{"error": "not yet wired",
    // "hint": "..."}` envelope shape so callers can pattern-match
    // without try/catch. Exercise each.
    let ps = new_store();
    ps.propose("main", &policy_json("infra/to-sign", serde_json::json!({})))
        .unwrap();

    let sign_env: serde_json::Value =
        serde_json::from_str(&ps.sign("main", "infra/to-sign", None).unwrap()).unwrap();
    assert_eq!(sign_env["error"], "not yet wired");
    assert!(sign_env["hint"].is_string());

    let sign_env_with_key: serde_json::Value = serde_json::from_str(
        &ps.sign("main", "infra/to-sign", Some("ops-root-2026".to_string()))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(sign_env_with_key["error"], "not yet wired");

    let verify_env: serde_json::Value =
        serde_json::from_str(&ps.verify("main", "infra/to-sign").unwrap()).unwrap();
    assert_eq!(verify_env["error"], "not yet wired");
    assert!(verify_env["hint"].is_string());

    let ext_env: serde_json::Value = serde_json::from_str(
        &ps.set_external_evaluator(
            "main",
            "infra/to-sign",
            Some(r#"{"kind":"webhook","endpoint":"https://x/"}"#.to_string()),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(ext_env["error"], "not yet wired");
    assert!(ext_env["hint"].is_string());
}
