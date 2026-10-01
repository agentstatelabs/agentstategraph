//! Policy writes are read-modify-writes of a whole policy record. They must
//! not lose a concurrent update to the same policy (data-loss audit, 2026-10).

use std::sync::{Arc, Barrier};
use std::thread;

use agentstategraph::Repository;
use agentstategraph_policy::{Policy, PolicySignature, PolicyStore, Selector, Severity};
use agentstategraph_storage::SqliteStorage;
use chrono::Utc;

const REF: &str = "main";

fn make_store() -> Arc<PolicyStore> {
    let repo = Arc::new(Repository::new(Box::new(
        SqliteStorage::in_memory().expect("in-memory sqlite"),
    )));
    repo.init().unwrap();
    Arc::new(PolicyStore::new(repo, "/policies", "test-agent"))
}

fn policy(path: &str) -> Policy {
    Policy {
        path: path.into(),
        version: 0,
        situation: "test".into(),
        situation_selector: Selector::Always,
        allow: vec![],
        deny: vec![],
        require_approval: vec![],
        procedure: None,
        triggers: vec![],
        required_fields: vec![],
        severity: Severity::Low,
        proposed_by: String::new(),
        proposed_at: Utc::now(),
        ratified_by: None,
        ratified_at: None,
        ratification_reasoning: None,
        active_from: Utc::now(),
        expires_at: None,
        supersedes: None,
        signature: None,
        tenant_id: None,
        external_evaluator: None,
    }
}

fn sig() -> PolicySignature {
    PolicySignature::Ed25519 {
        signer_key_id: "k1".into(),
        signature_hex: "00".repeat(64),
    }
}

/// Two threads released together, each running one closure.
fn race(a: impl FnOnce() + Send + 'static, b: impl FnOnce() + Send + 'static) {
    let barrier = Arc::new(Barrier::new(2));
    let (ba, bb) = (Arc::clone(&barrier), barrier);
    let ta = thread::spawn(move || {
        ba.wait();
        a()
    });
    let tb = thread::spawn(move || {
        bb.wait();
        b()
    });
    ta.join().unwrap();
    tb.join().unwrap();
}

#[test]
fn concurrent_ratify_and_sign_both_survive() {
    let store = make_store();
    let mut lost = Vec::new();
    for i in 0..30 {
        let path = format!("ops/p{i}");
        store.propose(REF, policy(&path)).unwrap();
        let (s1, p1) = (Arc::clone(&store), path.clone());
        let (s2, p2) = (Arc::clone(&store), path.clone());
        race(
            move || s1.ratify(REF, &p1, "alice", "ok").unwrap(),
            move || s2.set_signature(REF, &p2, sig()).unwrap(),
        );
        let p = store.get(REF, &path, None).unwrap();
        if p.ratified_by.is_none() || p.signature.is_none() {
            lost.push(format!(
                "{path}: ratified={} signed={}",
                p.ratified_by.is_some(),
                p.signature.is_some()
            ));
        }
    }
    assert!(
        lost.is_empty(),
        "{} of 30 lost an update: {lost:?}",
        lost.len()
    );
}

#[test]
fn concurrent_supersedes_each_get_their_own_version() {
    let store = make_store();
    store.propose(REF, policy("ops/q")).unwrap();
    let (s1, s2) = (Arc::clone(&store), Arc::clone(&store));
    race(
        move || {
            s1.supersede(REF, "ops/q", policy("ops/q")).unwrap();
        },
        move || {
            s2.supersede(REF, "ops/q", policy("ops/q")).unwrap();
        },
    );
    let active = store.get(REF, "ops/q", None).unwrap();
    assert_eq!(
        active.version, 3,
        "two supersedes of v1 must yield v2 then v3"
    );
    let history: Vec<u64> = store
        .history(REF, "ops/q")
        .unwrap()
        .iter()
        .map(|p| p.version)
        .collect();
    assert_eq!(history.len(), 3, "every version must be kept: {history:?}");
}
