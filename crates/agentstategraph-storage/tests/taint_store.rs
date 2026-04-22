//! Shared TaintStore conformance tests — run against both
//! MemoryStorage and SqliteStorage so the two backends stay in
//! lock-step. 0.7.75 §3.

use agentstategraph_storage::{MemoryStorage, SqliteStorage, TaintStore};
use agentstategraph_taint::{Taint, TaintEffect, TaintKind, TaintMetadata, TaintSeverity};
use chrono::{Duration, Utc};

fn make_taint(path: &str, name: &str, kind: TaintKind, effect: TaintEffect) -> Taint {
    Taint {
        id: uuid::Uuid::new_v4().to_string(),
        path: path.into(),
        name: name.into(),
        kind,
        effect,
        severity: TaintSeverity::Medium,
        reason: "test".into(),
        agent_id: "test-agent".into(),
        commit_id: String::new(),
        created_at: Utc::now(),
        expires_at: None,
        resolved_at: None,
        resolved_by: None,
        resolved_reason: None,
        resolved_proof: None,
        propagate: true,
        metadata: TaintMetadata::new(),
    }
}

fn run_conformance<S: TaintStore>(store: &S) {
    let t = make_taint("/cluster", "test", TaintKind::Taint, TaintEffect::Warn);
    let id = t.id.clone();

    // create + get
    store.create_taint(&t).unwrap();
    let fetched = store.get_taint(&id).unwrap().expect("fetched");
    assert_eq!(fetched.id, id);
    assert_eq!(fetched.path, "/cluster");
    assert!(!fetched.propagate ^ true);

    // duplicate-active insert rejected
    let mut dup = t.clone();
    dup.id = uuid::Uuid::new_v4().to_string();
    assert!(store.create_taint(&dup).is_err());

    // set_taint_commit_id patches
    store.set_taint_commit_id(&id, "commit-abc").unwrap();
    let fetched = store.get_taint(&id).unwrap().unwrap();
    assert_eq!(fetched.commit_id, "commit-abc");

    // check_taint returns this + propagates to descendants
    let matches = store.check_taint("/cluster/nodes/a").unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].id, id);

    // non-propagating taint limits the match
    let mut non_prop = make_taint("/other", "leaf", TaintKind::Taint, TaintEffect::Block);
    non_prop.propagate = false;
    let np_id = non_prop.id.clone();
    store.create_taint(&non_prop).unwrap();
    let direct = store.check_taint("/other").unwrap();
    assert!(direct.iter().any(|t| t.id == np_id));
    let desc = store.check_taint("/other/child").unwrap();
    assert!(!desc.iter().any(|t| t.id == np_id));

    // path boundary: `/cluster-staging` does NOT match a taint on
    // `/cluster`.
    let bounded = store.check_taint("/cluster-staging/x").unwrap();
    assert!(bounded.iter().all(|t| t.id != id));

    // list_taints with kind filter
    let tlist = store
        .list_taints(None, Some(TaintKind::Taint), false)
        .unwrap();
    assert!(tlist.iter().any(|t| t.id == id));

    // list_taints with path prefix
    let plist = store.list_taints(Some("/cluster"), None, false).unwrap();
    assert!(plist.iter().all(|t| t.path.starts_with("/cluster")));

    // resolve
    let now = Utc::now();
    store
        .resolve_taint(&id, "resolver", "fixed it", Some("commit-xyz"), now)
        .unwrap();
    let resolved = store.get_taint(&id).unwrap().unwrap();
    assert!(resolved.resolved_at.is_some());
    assert_eq!(resolved.resolved_by.as_deref(), Some("resolver"));
    assert_eq!(resolved.resolved_proof.as_deref(), Some("commit-xyz"));

    // resolve again → error
    assert!(store.resolve_taint(&id, "r", "r", None, now).is_err());

    // After resolve, check_taint no longer matches
    let after = store.check_taint("/cluster/nodes/a").unwrap();
    assert!(after.iter().all(|t| t.id != id));

    // But we can now re-create with same (path, name, kind).
    let fresh = make_taint("/cluster", "test", TaintKind::Taint, TaintEffect::Warn);
    store.create_taint(&fresh).unwrap();

    // include_resolved surfaces the historical row
    let hist = store.list_taints(None, None, true).unwrap();
    assert!(hist.iter().any(|t| t.id == id && t.resolved_at.is_some()));

    // Expired taint is ignored by check_taint
    let mut expired = make_taint("/exp", "x", TaintKind::Taint, TaintEffect::Block);
    expired.expires_at = Some(Utc::now() - Duration::seconds(10));
    store.create_taint(&expired).unwrap();
    let ex_match = store.check_taint("/exp").unwrap();
    assert!(ex_match.iter().all(|t| t.id != expired.id));
}

#[test]
fn memory_storage_taint_conformance() {
    run_conformance(&MemoryStorage::new());
}

#[test]
fn sqlite_storage_taint_conformance() {
    run_conformance(&SqliteStorage::in_memory().unwrap());
}
