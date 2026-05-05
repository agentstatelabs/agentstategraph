//! Rust reference runner for the cross-binding policy parity fixture.
//!
//! §7 of the 0.7.0-beta.1 plan. Every other binding runner loads
//! `spec/policy_parity_fixture.json` and must produce the same
//! `Decision.kind` (plus matching `matched_policy` prefix) as this
//! reference implementation.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use agentstategraph::Repository;
use agentstategraph_policy::{ChangeProposal, Policy, PolicyStore};
use agentstategraph_storage::SqliteStorage;
use serde_json::Value;

fn load_fixture() -> Value {
    // CARGO_MANIFEST_DIR = crates/agentstategraph-policy; fixture lives
    // at <repo>/spec/policy_parity_fixture.json.
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop(); // crates
    p.pop(); // <repo>
    p.push("spec");
    p.push("policy_parity_fixture.json");
    let bytes = std::fs::read(&p).unwrap_or_else(|e| panic!("read fixture {p:?}: {e}"));
    serde_json::from_slice(&bytes).expect("fixture is valid JSON")
}

fn decision_kind(d: &Value) -> &str {
    d.get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("<missing kind>")
}

#[test]
fn parity_fixture_matches_rust_reference() {
    let fixture = load_fixture();
    let prefix = fixture["prefix"].as_str().unwrap_or("/policies");
    let agent_id = fixture["agent_id"].as_str().unwrap_or("parity-runner");
    let ref_name = fixture["ref"].as_str().unwrap_or("main");

    let repo = Arc::new(Repository::new(Box::new(
        SqliteStorage::in_memory().expect("in-memory sqlite"),
    )));
    repo.init().expect("init repo");
    let store = PolicyStore::new(repo.clone(), prefix, agent_id);

    // 1. Propose every policy.
    let policies = fixture["policies"].as_array().expect("policies array");
    for pol_json in policies {
        let pol: Policy = serde_json::from_value(pol_json.clone())
            .unwrap_or_else(|e| panic!("decode policy {}: {e}", pol_json["path"]));
        store
            .propose(ref_name, pol)
            .unwrap_or_else(|e| panic!("propose {}: {e}", pol_json["path"]));
    }

    // 2. Ratify entries.
    let ratifications = fixture["ratify"].as_array().expect("ratify array");
    for r in ratifications {
        let path = r["path"].as_str().unwrap();
        let ratifier = r["ratifier"].as_str().unwrap();
        let reasoning = r["reasoning"].as_str().unwrap();
        store
            .ratify(ref_name, path, ratifier, reasoning)
            .unwrap_or_else(|e| panic!("ratify {path}: {e}"));
    }

    // 3. evaluate_change assertions.
    let proposals = fixture["change_proposals"].as_array().unwrap();
    for entry in proposals {
        let label = entry["label"].as_str().unwrap_or("<unlabelled>");
        let expected_kind = entry["expected_decision_kind"].as_str().unwrap();
        let proposal: ChangeProposal =
            serde_json::from_value(entry["proposal"].clone()).expect("decode proposal");
        let decision = store
            .evaluate_change(ref_name, &proposal)
            .unwrap_or_else(|e| panic!("evaluate_change {label}: {e}"));
        let d_json = serde_json::to_value(&decision).expect("decision to json");
        assert_eq!(
            decision_kind(&d_json),
            expected_kind,
            "proposal {label}: decision.kind mismatch (got {d_json})"
        );
        if let Some(prefix_expected) = entry
            .get("expected_matched_policy_prefix")
            .and_then(|v| v.as_str())
        {
            let matched = d_json
                .get("matched_policy")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            assert!(
                matched.starts_with(prefix_expected),
                "proposal {label}: matched_policy {matched:?} should start with {prefix_expected:?}"
            );
        }
    }

    // 4. evaluate assertions.
    let evals = fixture["evaluate"].as_array().unwrap();
    for entry in evals {
        let label = entry["label"].as_str().unwrap_or("<unlabelled>");
        let expected_kind = entry["expected_decision_kind"].as_str().unwrap();
        let situation_map: HashMap<String, String> = entry["situation"]
            .as_object()
            .map(|o| {
                o.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        let situation = agentstategraph_policy::Situation::from(situation_map);
        let action = entry["action"].as_str().unwrap();
        let agent = entry["agent_id"].as_str().unwrap();
        let decision = store
            .evaluate(ref_name, &situation, action, agent)
            .unwrap_or_else(|e| panic!("evaluate {label}: {e}"));
        let d_json = serde_json::to_value(&decision).expect("decision to json");
        assert_eq!(
            decision_kind(&d_json),
            expected_kind,
            "evaluate {label}: decision.kind mismatch (got {d_json})"
        );
    }

    // 5. (0.7.5 §6) Optional extra_policies + ratify_extra + tenant/external
    //    evaluate blocks. Runners that pre-date 0.7.5 can ignore these keys.
    if let Some(extras) = fixture.get("extra_policies").and_then(|v| v.as_array()) {
        for pol_json in extras {
            let pol: Policy = serde_json::from_value(pol_json.clone())
                .unwrap_or_else(|e| panic!("decode extra policy {}: {e}", pol_json["path"]));
            store
                .propose(ref_name, pol)
                .unwrap_or_else(|e| panic!("propose extra {}: {e}", pol_json["path"]));
        }
    }
    if let Some(rats) = fixture.get("ratify_extra").and_then(|v| v.as_array()) {
        for r in rats {
            let path = r["path"].as_str().unwrap();
            let ratifier = r["ratifier"].as_str().unwrap();
            let reasoning = r["reasoning"].as_str().unwrap();
            store
                .ratify(ref_name, path, ratifier, reasoning)
                .unwrap_or_else(|e| panic!("ratify extra {path}: {e}"));
        }
    }

    if let Some(tenants) = fixture.get("tenant_evaluate").and_then(|v| v.as_array()) {
        for entry in tenants {
            let label = entry["label"].as_str().unwrap_or("<unlabelled>");
            let expected_kind = entry["expected_decision_kind"].as_str().unwrap();
            let situation_map: HashMap<String, String> = entry["situation"]
                .as_object()
                .map(|o| {
                    o.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                        .collect()
                })
                .unwrap_or_default();
            let situation = agentstategraph_policy::Situation::from(situation_map);
            let action = entry["action"].as_str().unwrap();
            let agent = entry["agent_id"].as_str().unwrap();
            let tenant = entry
                .get("tenant_filter")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let decision = store
                .evaluate_scoped(ref_name, &situation, action, agent, tenant.as_deref())
                .unwrap_or_else(|e| panic!("tenant evaluate {label}: {e}"));
            let d_json = serde_json::to_value(&decision).expect("decision to json");
            assert_eq!(
                decision_kind(&d_json),
                expected_kind,
                "tenant evaluate {label}: decision.kind mismatch (got {d_json})"
            );
            if let Some(prefix_expected) = entry
                .get("expected_matched_policy_prefix")
                .and_then(|v| v.as_str())
            {
                let matched = d_json
                    .get("matched_policy")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                assert!(
                    matched.starts_with(prefix_expected),
                    "tenant evaluate {label}: matched_policy {matched:?} should start with {prefix_expected:?}"
                );
            }
        }
    }

    if let Some(exts) = fixture.get("external_evaluate").and_then(|v| v.as_array()) {
        for entry in exts {
            let label = entry["label"].as_str().unwrap_or("<unlabelled>");
            let expected_kind = entry["expected_decision_kind"].as_str().unwrap();
            let situation_map: HashMap<String, String> = entry["situation"]
                .as_object()
                .map(|o| {
                    o.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                        .collect()
                })
                .unwrap_or_default();
            let situation = agentstategraph_policy::Situation::from(situation_map);
            let action = entry["action"].as_str().unwrap();
            let agent = entry["agent_id"].as_str().unwrap();
            // No external runner registered in this reference runner →
            // policy with external_evaluator set is skipped, falling
            // through to no_policy_match.
            let decision = store
                .evaluate(ref_name, &situation, action, agent)
                .unwrap_or_else(|e| panic!("external evaluate {label}: {e}"));
            let d_json = serde_json::to_value(&decision).expect("decision to json");
            assert_eq!(
                decision_kind(&d_json),
                expected_kind,
                "external evaluate {label}: decision.kind mismatch (got {d_json})"
            );
        }
    }
}
