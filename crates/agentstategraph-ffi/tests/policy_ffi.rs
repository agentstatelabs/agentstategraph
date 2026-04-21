//! Round-trip integration tests for the PolicyStore FFI surface.
//!
//! Exercises every `agentstategraph_policy_*` extern C function end-to-end:
//! propose / ratify / supersede / list / active / get / history /
//! evaluate / evaluate_change / check_tokens, plus `active_from`
//! scheduled activation (§1 of the 0.7.0 plan).

use std::ffi::{CStr, CString};
use std::os::raw::c_char;

use agentstategraph_ffi::*;
use serde_json::{json, Value};

fn c(s: &str) -> CString {
    CString::new(s).unwrap()
}

unsafe fn read(p: *mut c_char) -> String {
    assert!(!p.is_null(), "expected non-null result");
    let s = unsafe { CStr::from_ptr(p).to_string_lossy().into_owned() };
    agentstategraph_free_string(p);
    s
}

fn policy_json(path: &str) -> Value {
    json!({
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
        "proposed_by": "ffi-test",
        "proposed_at": "2026-01-01T00:00:00Z",
        "active_from": "2026-01-01T00:00:00Z",
    })
}

fn policy_json_with(path: &str, overrides: Value) -> Value {
    let mut base = policy_json(path);
    if let Value::Object(ov) = overrides {
        if let Value::Object(ref mut b) = base {
            for (k, v) in ov {
                b.insert(k, v);
            }
        }
    }
    base
}

struct Harness {
    repo: *mut SgRepo,
    store: *mut SgPolicyStore,
}

impl Harness {
    fn new() -> Self {
        let repo = agentstategraph_new_memory();
        assert!(!repo.is_null());
        let prefix = c("/policies");
        let agent = c("ffi-test");
        let store = agentstategraph_policy_store_new(repo, prefix.as_ptr(), agent.as_ptr());
        assert!(!store.is_null());
        Self { repo, store }
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        agentstategraph_policy_store_free(self.store);
        agentstategraph_free(self.repo);
    }
}

fn propose(store: *mut SgPolicyStore, ref_name: &str, p: &Value) -> String {
    let rn = c(ref_name);
    let js = c(&serde_json::to_string(p).unwrap());
    unsafe {
        read(agentstategraph_policy_propose(
            store,
            rn.as_ptr(),
            js.as_ptr(),
        ))
    }
}

fn ratify(store: *mut SgPolicyStore, ref_name: &str, path: &str, who: &str, why: &str) -> String {
    let rn = c(ref_name);
    let p = c(path);
    let w = c(who);
    let r = c(why);
    unsafe {
        read(agentstategraph_policy_ratify(
            store,
            rn.as_ptr(),
            p.as_ptr(),
            w.as_ptr(),
            r.as_ptr(),
        ))
    }
}

#[test]
fn propose_creates_unratified_policy() {
    let h = Harness::new();
    let out = propose(h.store, "main", &policy_json("infra/k8s/pod-failing"));
    // out is JSON-wrapped handle string: "infra/k8s/pod-failing@1"
    let handle: String = serde_json::from_str(&out).unwrap();
    assert_eq!(handle, "infra/k8s/pod-failing@1");

    let rn = c("main");
    let p = c("infra/k8s/pod-failing");
    let got = unsafe { read(agentstategraph_policy_get(h.store, rn.as_ptr(), p.as_ptr())) };
    let v: Value = serde_json::from_str(&got).unwrap();
    assert_eq!(v["version"], 1);
    assert!(v["ratified_by"].is_null());
    assert_eq!(v["proposed_by"], "ffi-test");
}

#[test]
fn ratify_promotes_policy() {
    let h = Harness::new();
    propose(
        h.store,
        "main",
        &policy_json_with(
            "infra/restart",
            json!({"allow": [{"action": "restart_pod"}]}),
        ),
    );
    let r = ratify(h.store, "main", "infra/restart", "ops-lead", "approved");
    let ok: Value = serde_json::from_str(&r).unwrap();
    assert_eq!(ok["ok"], true);

    let rn = c("main");
    let p = c("infra/restart");
    let got = unsafe { read(agentstategraph_policy_get(h.store, rn.as_ptr(), p.as_ptr())) };
    let v: Value = serde_json::from_str(&got).unwrap();
    assert_eq!(v["ratified_by"], "ops-lead");
    assert_eq!(v["ratification_reasoning"], "approved");
    assert!(!v["ratified_at"].is_null());
}

#[test]
fn supersede_chain_and_history() {
    let h = Harness::new();
    propose(
        h.store,
        "main",
        &policy_json_with("infra/scale", json!({"allow": [{"action": "scale_up"}]})),
    );
    ratify(h.store, "main", "infra/scale", "ops", "v1");

    let new_v = policy_json_with(
        "infra/scale",
        json!({
            "allow": [{"action": "scale_up"}, {"action": "scale_down"}],
            "ratified_by": "ops",
            "ratified_at": "2026-01-02T00:00:00Z",
        }),
    );
    let rn = c("main");
    let path = c("infra/scale");
    let js = c(&serde_json::to_string(&new_v).unwrap());
    let out = unsafe {
        read(agentstategraph_policy_supersede(
            h.store,
            rn.as_ptr(),
            path.as_ptr(),
            js.as_ptr(),
        ))
    };
    let handle: String = serde_json::from_str(&out).unwrap();
    assert_eq!(handle, "infra/scale@2");

    let hist = unsafe {
        read(agentstategraph_policy_history(
            h.store,
            rn.as_ptr(),
            path.as_ptr(),
        ))
    };
    let arr: Vec<Value> = serde_json::from_str(&hist).unwrap();
    let versions: Vec<u64> = arr.iter().map(|p| p["version"].as_u64().unwrap()).collect();
    assert_eq!(versions, vec![1, 2]);
    assert_eq!(arr.last().unwrap()["supersedes"], "infra/scale@1");
}

#[test]
fn evaluate_allow_decision() {
    let h = Harness::new();
    propose(
        h.store,
        "main",
        &policy_json_with(
            "infra/restart",
            json!({
                "allow": [{"action": "restart_pod"}],
                "situation_selector": {"kind": "eq", "key": "namespace", "value": "prod"},
            }),
        ),
    );
    ratify(h.store, "main", "infra/restart", "ops", "ok");

    let rn = c("main");
    let sit = c(&json!({"namespace": "prod"}).to_string());
    let action = c("restart_pod");
    let agent = c("agent-1");
    let out = unsafe {
        read(agentstategraph_policy_evaluate(
            h.store,
            rn.as_ptr(),
            sit.as_ptr(),
            action.as_ptr(),
            agent.as_ptr(),
        ))
    };
    let d: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(d["kind"], "allow");
    assert_eq!(d["matched_policy"], "infra/restart@1");
}

#[test]
fn evaluate_no_match_when_not_yet_active() {
    let h = Harness::new();
    // active_from in the far future → ratified but not yet live.
    let future = "2099-01-01T00:00:00Z";
    propose(
        h.store,
        "main",
        &policy_json_with(
            "infra/future",
            json!({"allow": [{"action": "do_it"}], "active_from": future}),
        ),
    );
    ratify(h.store, "main", "infra/future", "ops", "scheduled");

    let rn = c("main");
    let sit = c("{}");
    let action = c("do_it");
    let agent = c("agent-1");
    let out = unsafe {
        read(agentstategraph_policy_evaluate(
            h.store,
            rn.as_ptr(),
            sit.as_ptr(),
            action.as_ptr(),
            agent.as_ptr(),
        ))
    };
    let d: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(d["kind"], "no_policy_match");

    let actives = unsafe {
        read(agentstategraph_policy_active(
            h.store,
            rn.as_ptr(),
            std::ptr::null(),
        ))
    };
    let arr: Vec<Value> = serde_json::from_str(&actives).unwrap();
    assert!(arr.iter().all(|p| p["path"] != "infra/future"));
}

#[test]
fn check_tokens_filters_by_trigger_intersection() {
    let h = Harness::new();
    propose(
        h.store,
        "main",
        &policy_json_with("infra/with-reindex", json!({"triggers": ["reindex"]})),
    );
    ratify(h.store, "main", "infra/with-reindex", "ops", "ok");
    propose(
        h.store,
        "main",
        &policy_json_with("infra/with-network", json!({"triggers": ["network"]})),
    );
    ratify(h.store, "main", "infra/with-network", "ops", "ok");

    let rn = c("main");
    let toks = c(&json!(["reindex"]).to_string());
    let out = unsafe {
        read(agentstategraph_policy_check_tokens(
            h.store,
            rn.as_ptr(),
            toks.as_ptr(),
        ))
    };
    let arr: Vec<Value> = serde_json::from_str(&out).unwrap();
    let mut paths: Vec<String> = arr
        .iter()
        .map(|p| p["path"].as_str().unwrap().to_string())
        .collect();
    paths.sort();
    assert_eq!(paths, vec!["infra/with-reindex".to_string()]);

    let toks_all = c(&json!(["reindex", "network"]).to_string());
    let out_all = unsafe {
        read(agentstategraph_policy_check_tokens(
            h.store,
            rn.as_ptr(),
            toks_all.as_ptr(),
        ))
    };
    let arr_all: Vec<Value> = serde_json::from_str(&out_all).unwrap();
    let mut paths_all: Vec<String> = arr_all
        .iter()
        .map(|p| p["path"].as_str().unwrap().to_string())
        .collect();
    paths_all.sort();
    assert_eq!(
        paths_all,
        vec![
            "infra/with-network".to_string(),
            "infra/with-reindex".to_string()
        ]
    );
}

#[test]
fn list_and_active_and_prefix_filter() {
    let h = Harness::new();
    propose(h.store, "main", &policy_json("infra/a"));
    propose(h.store, "main", &policy_json("infra/b"));
    propose(h.store, "main", &policy_json("other/c"));
    ratify(h.store, "main", "infra/a", "ops", "ok");
    // infra/b stays unratified → not in active

    let rn = c("main");
    let all_list = unsafe {
        read(agentstategraph_policy_list(
            h.store,
            rn.as_ptr(),
            std::ptr::null(),
        ))
    };
    let arr: Vec<Value> = serde_json::from_str(&all_list).unwrap();
    assert_eq!(arr.len(), 3);

    let infra_prefix = c("infra");
    let infra_list = unsafe {
        read(agentstategraph_policy_list(
            h.store,
            rn.as_ptr(),
            infra_prefix.as_ptr(),
        ))
    };
    let arr_infra: Vec<Value> = serde_json::from_str(&infra_list).unwrap();
    assert_eq!(arr_infra.len(), 2);

    let active = unsafe {
        read(agentstategraph_policy_active(
            h.store,
            rn.as_ptr(),
            std::ptr::null(),
        ))
    };
    let arr_active: Vec<Value> = serde_json::from_str(&active).unwrap();
    assert_eq!(arr_active.len(), 1);
    assert_eq!(arr_active[0]["path"], "infra/a");
}

#[test]
fn evaluate_change_require_approval() {
    let h = Harness::new();
    propose(
        h.store,
        "main",
        &policy_json_with(
            "infra/costly",
            json!({
                "triggers": ["reindex"],
                "require_approval": [{
                    "action": "promote",
                    "approvers": ["ops-lead"],
                    "timeout": null,
                    "fallback": {"kind": "block"},
                }],
            }),
        ),
    );
    ratify(h.store, "main", "infra/costly", "ops", "ok");

    let proposal = json!({
        "action": "promote",
        "agent_id": "agent-1",
        "intent": "merge option C",
        "preferred_option": "spec-7",
        "alternatives": [],
        "tokens": ["reindex"],
        "attached_fields": {},
    });
    let rn = c("main");
    let js = c(&proposal.to_string());
    let out = unsafe {
        read(agentstategraph_policy_evaluate_change(
            h.store,
            rn.as_ptr(),
            js.as_ptr(),
        ))
    };
    let d: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(d["kind"], "require_approval");
    assert_eq!(d["matched_policy"], "infra/costly@1");
    assert_eq!(d["approvers"], json!(["ops-lead"]));
}

#[test]
fn propose_rejects_duplicate_path() {
    let h = Harness::new();
    propose(h.store, "main", &policy_json("infra/dup"));
    let rn = c("main");
    let js = c(&policy_json("infra/dup").to_string());
    let out = unsafe {
        read(agentstategraph_policy_propose(
            h.store,
            rn.as_ptr(),
            js.as_ptr(),
        ))
    };
    let v: Value = serde_json::from_str(&out).unwrap();
    assert!(v.get("error").is_some(), "expected error JSON, got {out}");
}

#[test]
fn null_store_returns_null() {
    let rn = c("main");
    let p = c("any");
    let out = agentstategraph_policy_get(std::ptr::null(), rn.as_ptr(), p.as_ptr());
    assert!(out.is_null());
}
