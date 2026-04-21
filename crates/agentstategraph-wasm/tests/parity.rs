//! WASM parity runner for the cross-binding policy fixture.
//!
//! §7 of the 0.7.0-beta.1 plan. Runs under `wasm-bindgen-test` and
//! exercises `WasmPolicyStore` against the shared scenario at
//! `spec/policy_parity_fixture.json`. The fixture content is embedded
//! at compile time via `include_str!` so it's reachable from the
//! browser/Node runtime (no filesystem access required).

#![cfg(target_arch = "wasm32")]

use std::sync::Arc;

use agentstategraph::Repository;
use agentstategraph_storage::MemoryStorage;
use agentstategraph_wasm::WasmPolicyStore;
use serde_json::Value;
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

const FIXTURE: &str = include_str!("../../../spec/policy_parity_fixture.json");

fn new_store(prefix: &str, agent_id: &str) -> WasmPolicyStore {
    let repo = Repository::new(Box::new(MemoryStorage::new()));
    repo.init().expect("init");
    repo.set_json(
        "main",
        "/_bootstrap",
        &serde_json::json!({"ok": true}),
        agentstategraph::CommitOptions::new(
            "parity",
            agentstategraph_core::IntentCategory::Checkpoint,
            "bootstrap",
        ),
    )
    .expect("bootstrap commit");
    WasmPolicyStore::from_repo(Arc::new(repo), prefix, agent_id)
}

#[wasm_bindgen_test]
fn parity_fixture_matches_through_wasm() {
    let fixture: Value = serde_json::from_str(FIXTURE).expect("fixture json");
    let prefix = fixture["prefix"].as_str().unwrap_or("/policies");
    let agent_id = fixture["agent_id"].as_str().unwrap_or("parity-runner");
    let ref_name = fixture["ref"].as_str().unwrap_or("main");

    let ps = new_store(prefix, agent_id);

    for pol in fixture["policies"].as_array().unwrap() {
        ps.propose(ref_name, &pol.to_string())
            .unwrap_or_else(|e| panic!("propose {}: {e:?}", pol["path"]));
    }
    for r in fixture["ratify"].as_array().unwrap() {
        ps.ratify(
            ref_name,
            r["path"].as_str().unwrap(),
            r["ratifier"].as_str().unwrap(),
            r["reasoning"].as_str().unwrap(),
        )
        .unwrap_or_else(|e| panic!("ratify {}: {e:?}", r["path"]));
    }

    for entry in fixture["change_proposals"].as_array().unwrap() {
        let label = entry["label"].as_str().unwrap_or("<unlabelled>");
        let expected_kind = entry["expected_decision_kind"].as_str().unwrap();
        let out = ps
            .evaluate_change(ref_name, &entry["proposal"].to_string())
            .unwrap_or_else(|e| panic!("evaluate_change {label}: {e:?}"));
        let d: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            d["kind"].as_str().unwrap_or(""),
            expected_kind,
            "{label}: decision.kind mismatch (got {out})"
        );
        if let Some(prefix_expected) = entry
            .get("expected_matched_policy_prefix")
            .and_then(|v| v.as_str())
        {
            let matched = d["matched_policy"].as_str().unwrap_or("");
            assert!(
                matched.starts_with(prefix_expected),
                "{label}: matched_policy {matched:?} should start with {prefix_expected:?}"
            );
        }
    }

    for entry in fixture["evaluate"].as_array().unwrap() {
        let label = entry["label"].as_str().unwrap_or("<unlabelled>");
        let expected_kind = entry["expected_decision_kind"].as_str().unwrap();
        let out = ps
            .evaluate(
                ref_name,
                &entry["situation"].to_string(),
                entry["action"].as_str().unwrap(),
                entry["agent_id"].as_str().unwrap(),
            )
            .unwrap_or_else(|e| panic!("evaluate {label}: {e:?}"));
        let d: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            d["kind"].as_str().unwrap_or(""),
            expected_kind,
            "{label}: decision.kind mismatch (got {out})"
        );
    }
}
