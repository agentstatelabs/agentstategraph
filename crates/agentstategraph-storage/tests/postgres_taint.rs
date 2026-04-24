//! Postgres `TaintStore` conformance + tenant-isolation tests
//! (0.7.75-beta.2 Postgres-first work). Skipped unless
//! `TEST_DATABASE_URL` is set, matching the existing
//! `postgres_tenant_isolation.rs` pattern.
//!
//! Each test uses a unique tenant id derived from a monotonic
//! timestamp so re-running against the same database doesn't
//! collide on the partial unique index.
//!
//! **Run serially.** Concurrent `init_tables` calls race on
//! Postgres `CREATE UNIQUE INDEX IF NOT EXISTS`. Invoke with
//! `--test-threads=1` (or set `RUST_TEST_THREADS=1`).

#![cfg(feature = "postgres")]

use agentstategraph_storage::{PostgresStorage, TaintStore};
use agentstategraph_taint::{Taint, TaintEffect, TaintKind, TaintMetadata, TaintSeverity};
use chrono::{Duration as ChronoDuration, Utc};

fn db_url() -> Option<String> {
    match std::env::var("TEST_DATABASE_URL") {
        Ok(u) if !u.is_empty() => Some(u),
        _ => {
            eprintln!("skip: TEST_DATABASE_URL not set");
            None
        }
    }
}

fn tenant_id(label: &str) -> String {
    let ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("taint-{label}-{ns}")
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
}

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

#[test]
fn postgres_taint_store_conformance() {
    let Some(url) = db_url() else {
        return;
    };
    let tid = tenant_id("conformance");
    let rt = runtime();
    let store = rt
        .block_on(PostgresStorage::connect_tenant(&url, &tid))
        .expect("connect tenant");

    let _guard = rt.enter();

    // 1. create + get
    let t = make_taint(
        "/cluster",
        "disk-pressure",
        TaintKind::Taint,
        TaintEffect::Warn,
    );
    let id = t.id.clone();
    store.create_taint(&t).unwrap();
    let fetched = store.get_taint(&id).unwrap().expect("fetched");
    assert_eq!(fetched.path, "/cluster");
    assert_eq!(fetched.effect, TaintEffect::Warn);

    // 2. duplicate-active rejected by partial unique index
    let mut dup = t.clone();
    dup.id = uuid::Uuid::new_v4().to_string();
    assert!(store.create_taint(&dup).is_err());

    // 3. set_taint_commit_id patches
    store.set_taint_commit_id(&id, "commit-abc").unwrap();
    assert_eq!(
        store.get_taint(&id).unwrap().unwrap().commit_id,
        "commit-abc"
    );

    // 4. check_taint ancestor propagation
    let matches = store.check_taint("/cluster/nodes/a").unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].id, id);

    // 5. path boundary: /cluster-staging must NOT match /cluster
    let bounded = store.check_taint("/cluster-staging/x").unwrap();
    assert!(bounded.iter().all(|t| t.id != id));

    // 6. non-propagating leaf
    let mut np = make_taint("/other", "leaf", TaintKind::Taint, TaintEffect::Block);
    np.propagate = false;
    let np_id = np.id.clone();
    store.create_taint(&np).unwrap();
    assert!(
        store
            .check_taint("/other/child")
            .unwrap()
            .iter()
            .all(|t| t.id != np_id)
    );
    assert!(
        store
            .check_taint("/other")
            .unwrap()
            .iter()
            .any(|t| t.id == np_id)
    );

    // 7. list filters
    let all = store.list_taints(None, None, false).unwrap();
    assert!(all.iter().any(|t| t.id == id));
    assert!(all.iter().any(|t| t.id == np_id));
    let by_kind = store
        .list_taints(None, Some(TaintKind::Taint), false)
        .unwrap();
    assert!(by_kind.iter().all(|t| t.kind == TaintKind::Taint));
    let by_prefix = store.list_taints(Some("/cluster"), None, false).unwrap();
    assert!(by_prefix.iter().any(|t| t.id == id));
    assert!(by_prefix.iter().all(|t| t.path.starts_with("/cluster")));

    // 8. resolve + already-resolved rejection
    let now = Utc::now();
    store
        .resolve_taint(&id, "resolver", "fixed", Some("commit-xyz"), now)
        .unwrap();
    let r = store.get_taint(&id).unwrap().unwrap();
    assert!(r.resolved_at.is_some());
    assert_eq!(r.resolved_proof.as_deref(), Some("commit-xyz"));
    assert!(store.resolve_taint(&id, "r", "r", None, now).is_err());

    // 9. check_taint no longer matches resolved
    assert!(
        store
            .check_taint("/cluster/nodes/a")
            .unwrap()
            .iter()
            .all(|t| t.id != id)
    );

    // 10. include_resolved=true surfaces history
    let hist = store.list_taints(None, None, true).unwrap();
    assert!(hist.iter().any(|t| t.id == id && t.resolved_at.is_some()));

    // 11. can re-create same (path,name,kind) after resolve
    let fresh = make_taint(
        "/cluster",
        "disk-pressure",
        TaintKind::Taint,
        TaintEffect::Warn,
    );
    store.create_taint(&fresh).unwrap();

    // 12. expired taint ignored
    let mut exp = make_taint("/exp", "x", TaintKind::Taint, TaintEffect::Block);
    exp.expires_at = Some(Utc::now() - ChronoDuration::seconds(10));
    store.create_taint(&exp).unwrap();
    assert!(
        store
            .check_taint("/exp")
            .unwrap()
            .iter()
            .all(|t| t.id != exp.id)
    );
}

#[test]
fn postgres_tenants_cannot_see_each_others_taints() {
    let Some(url) = db_url() else {
        return;
    };
    let ta = tenant_id("iso-a");
    let tb = tenant_id("iso-b");
    let rt = runtime();

    let a = rt
        .block_on(PostgresStorage::connect_tenant(&url, &ta))
        .expect("tenant a");
    let b = rt
        .block_on(PostgresStorage::connect_tenant(&url, &tb))
        .expect("tenant b");

    let _guard = rt.enter();

    let ta_taint = make_taint(
        "/secret",
        "tenant-a-only",
        TaintKind::Taint,
        TaintEffect::Block,
    );
    let ta_id = ta_taint.id.clone();
    a.create_taint(&ta_taint).unwrap();

    // Tenant A sees it
    assert!(a.get_taint(&ta_id).unwrap().is_some());
    let a_check = a.check_taint("/secret").unwrap();
    assert_eq!(a_check.len(), 1);

    // Tenant B sees NOTHING — id, list, check all isolated
    assert!(b.get_taint(&ta_id).unwrap().is_none());
    assert!(b.list_taints(None, None, false).unwrap().is_empty());
    assert!(b.check_taint("/secret").unwrap().is_empty());

    // Each tenant can create an independent taint with the same
    // (path, name, kind) triple — the partial unique index is
    // scoped to tenant_id.
    let tb_taint = make_taint(
        "/secret",
        "tenant-a-only",
        TaintKind::Taint,
        TaintEffect::Block,
    );
    b.create_taint(&tb_taint).unwrap();
    assert_eq!(b.list_taints(None, None, false).unwrap().len(), 1);
    // And tenant A still only sees its own.
    assert_eq!(a.list_taints(None, None, false).unwrap().len(), 1);
}
