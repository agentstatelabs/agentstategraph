//! C FFI parity runner for the cross-binding policy fixture.
//!
//! §7 of the 0.7.0-beta.1 plan. Loads
//! `spec/policy_parity_fixture.json` and exercises it through the
//! `agentstategraph_policy_*` extern C surface, asserting the same
//! `Decision.kind` (and matched_policy prefix) as every other binding.

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::path::PathBuf;

use agentstategraph_ffi::*;
use serde_json::Value;

fn c(s: &str) -> CString {
    CString::new(s).unwrap()
}

unsafe fn read(p: *mut c_char) -> String {
    assert!(!p.is_null(), "expected non-null FFI result");
    let s = unsafe { CStr::from_ptr(p).to_string_lossy().into_owned() };
    agentstategraph_free_string(p);
    s
}

fn load_fixture() -> Value {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop(); // crates
    p.pop(); // <repo>
    p.push("spec");
    p.push("policy_parity_fixture.json");
    let bytes = std::fs::read(&p).unwrap_or_else(|e| panic!("read fixture {p:?}: {e}"));
    serde_json::from_slice(&bytes).expect("fixture is valid JSON")
}

#[test]
fn parity_fixture_matches_through_ffi() {
    let fixture = load_fixture();
    let prefix = fixture["prefix"].as_str().unwrap_or("/policies");
    let agent_id = fixture["agent_id"].as_str().unwrap_or("parity-runner");
    let ref_name = fixture["ref"].as_str().unwrap_or("main");

    let repo = agentstategraph_new_memory();
    assert!(!repo.is_null());
    let c_prefix = c(prefix);
    let c_agent = c(agent_id);
    let store = agentstategraph_policy_store_new(repo, c_prefix.as_ptr(), c_agent.as_ptr());
    assert!(!store.is_null());

    let c_ref = c(ref_name);

    // 1. Propose.
    for pol in fixture["policies"].as_array().unwrap() {
        let js = c(&serde_json::to_string(pol).unwrap());
        let out = unsafe {
            read(agentstategraph_policy_propose(
                store,
                c_ref.as_ptr(),
                js.as_ptr(),
            ))
        };
        let v: Value = serde_json::from_str(&out).unwrap();
        assert!(v.get("error").is_none(), "propose {}: {out}", pol["path"]);
    }

    // 2. Ratify.
    for r in fixture["ratify"].as_array().unwrap() {
        let path = c(r["path"].as_str().unwrap());
        let who = c(r["ratifier"].as_str().unwrap());
        let why = c(r["reasoning"].as_str().unwrap());
        let out = unsafe {
            read(agentstategraph_policy_ratify(
                store,
                c_ref.as_ptr(),
                path.as_ptr(),
                who.as_ptr(),
                why.as_ptr(),
            ))
        };
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            v.get("ok").and_then(|b| b.as_bool()),
            Some(true),
            "ratify {}: {out}",
            r["path"]
        );
    }

    // 3. evaluate_change.
    for entry in fixture["change_proposals"].as_array().unwrap() {
        let label = entry["label"].as_str().unwrap_or("<unlabelled>");
        let expected_kind = entry["expected_decision_kind"].as_str().unwrap();
        let js = c(&entry["proposal"].to_string());
        let out = unsafe {
            read(agentstategraph_policy_evaluate_change(
                store,
                c_ref.as_ptr(),
                js.as_ptr(),
            ))
        };
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

    // 4. evaluate.
    for entry in fixture["evaluate"].as_array().unwrap() {
        let label = entry["label"].as_str().unwrap_or("<unlabelled>");
        let expected_kind = entry["expected_decision_kind"].as_str().unwrap();
        let sit = c(&entry["situation"].to_string());
        let action = c(entry["action"].as_str().unwrap());
        let agent = c(entry["agent_id"].as_str().unwrap());
        let out = unsafe {
            read(agentstategraph_policy_evaluate(
                store,
                c_ref.as_ptr(),
                sit.as_ptr(),
                action.as_ptr(),
                agent.as_ptr(),
            ))
        };
        let d: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            d["kind"].as_str().unwrap_or(""),
            expected_kind,
            "{label}: decision.kind mismatch (got {out})"
        );
    }

    agentstategraph_policy_store_free(store);
    agentstategraph_free(repo);
}
