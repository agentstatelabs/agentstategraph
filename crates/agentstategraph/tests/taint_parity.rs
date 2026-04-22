//! §10 — Rust reference runner for the taint blocks in
//! `spec/policy_parity_fixture.json`. The policy reference runner
//! lives in `agentstategraph-policy`; this file handles the
//! Repository-level taint / quarantine cases because the policy
//! crate doesn't depend on `agentstategraph-taint`.

use std::path::PathBuf;
use std::sync::Arc;

use agentstategraph::Repository;
use agentstategraph_storage::MemoryStorage;
use agentstategraph_taint::{
    QuarantineParams, TaintEffect, TaintMetadata, TaintParams, TaintSeverity,
};
use serde_json::Value;

fn load_fixture() -> Value {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p.pop();
    p.push("spec");
    p.push("policy_parity_fixture.json");
    let bytes = std::fs::read(&p).unwrap_or_else(|e| panic!("read {p:?}: {e}"));
    serde_json::from_slice(&bytes).expect("fixture is valid JSON")
}

fn parse_effect(s: &str) -> TaintEffect {
    match s {
        "warn" => TaintEffect::Warn,
        "block" => TaintEffect::Block,
        "review" => TaintEffect::Review,
        "isolate" => TaintEffect::Isolate,
        "advisory" => TaintEffect::Advisory,
        other => panic!("unknown effect {other}"),
    }
}

fn parse_severity(s: Option<&str>) -> TaintSeverity {
    match s.unwrap_or("medium") {
        "low" => TaintSeverity::Low,
        "high" => TaintSeverity::High,
        "critical" => TaintSeverity::Critical,
        _ => TaintSeverity::Medium,
    }
}

fn fresh() -> Arc<Repository> {
    let r = Arc::new(Repository::new(Box::new(MemoryStorage::new())));
    r.init().unwrap();
    r
}

#[test]
fn taint_cases_match_rust_reference() {
    let fixture = load_fixture();
    let cases = match fixture.get("taint_cases").and_then(|v| v.as_array()) {
        Some(cs) => cs,
        None => return, // pre-0.7.75 fixture — nothing to exercise
    };
    for case in cases {
        let label = case["label"].as_str().unwrap_or("<?>");
        let r = fresh();
        let apply = &case["apply"];
        let params = TaintParams {
            name: apply["name"].as_str().unwrap().to_string(),
            effect: parse_effect(apply["effect"].as_str().unwrap_or("warn")),
            reason: apply["reason"].as_str().unwrap_or("").to_string(),
            severity: parse_severity(apply["severity"].as_str()),
            expires_at: None,
            propagate: apply["propagate"].as_bool().unwrap_or(true),
            metadata: TaintMetadata::new(),
            agent_id: apply["agent_id"].as_str().unwrap_or("").to_string(),
        };
        r.taint("main", apply["path"].as_str().unwrap(), params)
            .unwrap_or_else(|e| panic!("{label}: taint apply: {e}"));

        // Low/high confidence variant.
        if case.get("check_low").is_some() {
            let low = &case["check_low"];
            let c = r
                .check_taint(
                    low["path"].as_str().unwrap(),
                    low["agent_id"].as_str().unwrap_or(""),
                    low["confidence"].as_f64().unwrap_or(1.0),
                )
                .unwrap();
            assert_eq!(
                c.can_write,
                case["expected_low_can_write"].as_bool().unwrap_or(false),
                "{label}: low-confidence can_write mismatch"
            );
            if let Some(req) = case.get("expected_required_confidence").and_then(|v| v.as_f64())
            {
                assert_eq!(c.required_confidence, req, "{label}: required_confidence");
            }

            let high = &case["check_high"];
            let c2 = r
                .check_taint(
                    high["path"].as_str().unwrap(),
                    high["agent_id"].as_str().unwrap_or(""),
                    high["confidence"].as_f64().unwrap_or(1.0),
                )
                .unwrap();
            assert_eq!(
                c2.can_write,
                case["expected_high_can_write"].as_bool().unwrap_or(true),
                "{label}: high-confidence can_write mismatch"
            );
        } else {
            let chk = &case["check"];
            let c = r
                .check_taint(
                    chk["path"].as_str().unwrap(),
                    chk["agent_id"].as_str().unwrap_or(""),
                    chk["confidence"].as_f64().unwrap_or(1.0),
                )
                .unwrap();
            let expected = &case["expected"];
            if let Some(v) = expected.get("tainted").and_then(|v| v.as_bool()) {
                assert_eq!(c.tainted, v, "{label}: tainted");
            }
            if let Some(v) = expected.get("can_write").and_then(|v| v.as_bool()) {
                assert_eq!(c.can_write, v, "{label}: can_write");
            }
            if let Some(v) = expected.get("required_confidence").and_then(|v| v.as_f64()) {
                assert_eq!(c.required_confidence, v, "{label}: required_confidence");
            }
        }
    }
}

#[test]
fn quarantine_case_matches_rust_reference() {
    let fixture = load_fixture();
    let Some(case) = fixture.get("quarantine_case") else {
        return;
    };
    let label = case["label"].as_str().unwrap_or("<?>");
    let r = fresh();
    let apply = &case["apply"];
    let authorized: Vec<String> = apply["authorized_agents"]
        .as_array()
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    r.quarantine(
        "main",
        apply["path"].as_str().unwrap(),
        QuarantineParams {
            name: apply["name"].as_str().unwrap().to_string(),
            reason: apply["reason"].as_str().unwrap_or("").to_string(),
            severity: parse_severity(apply["severity"].as_str()),
            authorized_agents: authorized,
            expires_at: None,
            propagate: apply["propagate"].as_bool().unwrap_or(true),
            agent_id: apply["agent_id"].as_str().unwrap_or("").to_string(),
        },
    )
    .unwrap_or_else(|e| panic!("{label}: quarantine apply: {e}"));

    let u = &case["check_unauthorized"];
    let c_u = r
        .check_taint(
            u["path"].as_str().unwrap(),
            u["agent_id"].as_str().unwrap_or(""),
            u["confidence"].as_f64().unwrap_or(1.0),
        )
        .unwrap();
    assert_eq!(
        c_u.can_write,
        case["expected_unauthorized_can_write"].as_bool().unwrap_or(false),
        "{label}: unauthorized can_write"
    );

    let a = &case["check_authorized"];
    let c_a = r
        .check_taint(
            a["path"].as_str().unwrap(),
            a["agent_id"].as_str().unwrap_or(""),
            a["confidence"].as_f64().unwrap_or(1.0),
        )
        .unwrap();
    assert_eq!(
        c_a.can_write,
        case["expected_authorized_can_write"].as_bool().unwrap_or(true),
        "{label}: authorized can_write"
    );
}
