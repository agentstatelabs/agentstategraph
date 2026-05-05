//! 0.7.75 §4 — Repository-level taint methods + pre-commit hook
//! integration tests. Runs against `SqliteStorage::in_memory()` for speed.

use agentstategraph::{CommitOptions, Repository};
use agentstategraph_core::{IntentCategory, Object};
use agentstategraph_storage::SqliteStorage;
use agentstategraph_taint::{
    QuarantineParams, TaintEffect, TaintError, TaintKind, TaintMetadata, TaintParams,
    TaintSeverity, UntaintParams, UnwatchParams, WatchDirection, WatchParams,
};

fn repo() -> Repository {
    let r = Repository::new(Box::new(SqliteStorage::in_memory().expect("in-memory sqlite")));
    r.init().unwrap();
    r
}

fn opts(agent: &str, confidence: Option<f64>) -> CommitOptions {
    let mut o = CommitOptions::new(agent, IntentCategory::Refine, "write");
    if let Some(c) = confidence {
        o = o.with_confidence(c);
    }
    o
}

fn t_params(name: &str, effect: TaintEffect, agent: &str) -> TaintParams {
    TaintParams {
        name: name.into(),
        effect,
        reason: "test".into(),
        severity: TaintSeverity::Medium,
        expires_at: None,
        propagate: true,
        metadata: TaintMetadata::new(),
        agent_id: agent.into(),
    }
}

#[test]
fn taint_then_untaint_round_trip() {
    let r = repo();
    let id = r
        .taint("main", "/x", t_params("t1", TaintEffect::Warn, "ops"))
        .unwrap();
    let listed = r.list_taints(None, Some(TaintKind::Taint), false).unwrap();
    assert!(listed.iter().any(|t| t.id == id && !t.commit_id.is_empty()));

    r.untaint(
        "main",
        "/x",
        "t1",
        UntaintParams {
            reason: "resolved".into(),
            proof: Some("commit-xyz".into()),
            agent_id: "ops".into(),
        },
    )
    .unwrap();
    let active = r.list_taints(None, Some(TaintKind::Taint), false).unwrap();
    assert!(active.iter().all(|t| t.id != id));
}

#[test]
fn block_effect_rejects_set() {
    let r = repo();
    r.taint(
        "main",
        "/cluster",
        t_params("down", TaintEffect::Block, "ops"),
    )
    .unwrap();
    let err = r
        .set(
            "main",
            "/cluster/nodes/a",
            &Object::string("hi"),
            opts("agent-1", None),
        )
        .unwrap_err();
    assert!(matches!(
        err,
        agentstategraph::RepoError::Taint {
            source: TaintError::Blocked { .. },
            ..
        }
    ));
}

#[test]
fn review_effect_rejects_low_confidence_accepts_high() {
    let r = repo();
    r.taint(
        "main",
        "/cluster",
        t_params("rev", TaintEffect::Review, "ops"),
    )
    .unwrap();

    // confidence = 0.5 → rejected
    let low = r.set(
        "main",
        "/cluster/x",
        &Object::string("v"),
        opts("agent-1", Some(0.5)),
    );
    assert!(matches!(
        low,
        Err(agentstategraph::RepoError::Taint {
            source: TaintError::InsufficientConfidence { .. },
            ..
        })
    ));

    // confidence = 0.95 → accepted
    r.set(
        "main",
        "/cluster/y",
        &Object::string("v"),
        opts("agent-1", Some(0.95)),
    )
    .unwrap();
}

#[test]
fn quarantine_blocks_unauthorized_passes_authorized() {
    let r = repo();
    r.quarantine(
        "main",
        "/secrets",
        QuarantineParams {
            name: "sec".into(),
            reason: "audit".into(),
            severity: TaintSeverity::High,
            authorized_agents: vec!["agent/security".into()],
            expires_at: None,
            propagate: true,
            agent_id: "agent/security".into(),
        },
    )
    .unwrap();
    let denied = r.set(
        "main",
        "/secrets/x",
        &Object::string("v"),
        opts("agent-1", None),
    );
    assert!(matches!(
        denied,
        Err(agentstategraph::RepoError::Taint {
            source: TaintError::NotAuthorized { .. },
            ..
        })
    ));
    r.set(
        "main",
        "/secrets/y",
        &Object::string("v"),
        opts("agent/security", None),
    )
    .unwrap();
}

#[test]
fn warn_effect_allows_write() {
    let r = repo();
    r.taint(
        "main",
        "/cluster",
        t_params("deg", TaintEffect::Warn, "ops"),
    )
    .unwrap();
    // Should succeed even with no confidence because warn is advisory.
    r.set(
        "main",
        "/cluster/x",
        &Object::string("v"),
        opts("agent-1", None),
    )
    .unwrap();
}

#[test]
fn non_propagating_taint_does_not_block_descendants() {
    let r = repo();
    let mut params = t_params("leaf", TaintEffect::Block, "ops");
    params.propagate = false;
    r.taint("main", "/cluster", params).unwrap();
    r.set(
        "main",
        "/cluster/child",
        &Object::string("v"),
        opts("agent-1", None),
    )
    .unwrap();
}

#[test]
fn expired_taint_is_ignored_by_hook() {
    let r = repo();
    let mut params = t_params("old", TaintEffect::Block, "ops");
    params.expires_at = Some(chrono::Utc::now() - chrono::Duration::seconds(10));
    r.taint("main", "/cluster", params).unwrap();
    // Expired block → write succeeds.
    r.set(
        "main",
        "/cluster/x",
        &Object::string("v"),
        opts("agent-1", None),
    )
    .unwrap();
}

#[test]
fn check_taint_surfaces_full_status() {
    let r = repo();
    r.taint("main", "/x", t_params("a", TaintEffect::Warn, "ops"))
        .unwrap();
    r.quarantine(
        "main",
        "/x",
        QuarantineParams {
            name: "q".into(),
            reason: "audit".into(),
            severity: TaintSeverity::High,
            authorized_agents: vec!["agent/security".into()],
            expires_at: None,
            propagate: true,
            agent_id: "agent/security".into(),
        },
    )
    .unwrap();
    let c = r.check_taint("/x/inner", "agent-1", 1.0).unwrap();
    assert!(c.tainted && c.quarantined);
    assert!(!c.can_write);
    assert_eq!(c.authorized_agents, vec!["agent/security".to_string()]);
}

#[test]
fn watch_creates_and_lists() {
    let r = repo();
    let id = r
        .watch_path(
            "main",
            "/cluster/disk",
            WatchParams {
                name: "disk-80".into(),
                reason: "perf".into(),
                metric: Some("disk_used_pct".into()),
                threshold: Some(80.0),
                direction: WatchDirection::Above,
                check_interval_secs: Some(60),
                expires_at: None,
                severity: TaintSeverity::Low,
                propagate: true,
                agent_id: "ops".into(),
            },
        )
        .unwrap();
    let watches = r.list_taints(None, Some(TaintKind::Watch), false).unwrap();
    assert!(watches.iter().any(|w| w.id == id));

    // Watches are advisory — they don't block writes.
    r.set(
        "main",
        "/cluster/disk",
        &Object::string("50"),
        opts("agent-1", None),
    )
    .unwrap();

    r.unwatch(
        "main",
        "/cluster/disk",
        "disk-80",
        UnwatchParams {
            reason: Some("no longer needed".into()),
            agent_id: "ops".into(),
        },
    )
    .unwrap();
    let active = r.list_taints(None, Some(TaintKind::Watch), false).unwrap();
    assert!(active.iter().all(|w| w.id != id));
}

#[test]
fn taint_lifecycle_intents_bypass_hook() {
    let r = repo();
    // Pre-taint the path.
    r.taint(
        "main",
        "/cluster",
        t_params("block", TaintEffect::Block, "ops"),
    )
    .unwrap();
    // A regular write under the /cluster prefix should fail...
    assert!(
        r.set(
            "main",
            "/cluster/anything",
            &Object::string("x"),
            opts("a", None)
        )
        .is_err()
    );
    // ...but resolving the taint must succeed — untaint is a
    // lifecycle intent that bypasses the hook.
    r.untaint(
        "main",
        "/cluster",
        "block",
        UntaintParams {
            reason: "fixed".into(),
            proof: None,
            agent_id: "ops".into(),
        },
    )
    .unwrap();
}

#[test]
fn taint_commit_category_is_native() {
    let r = repo();
    r.taint("main", "/x", t_params("t", TaintEffect::Warn, "ops"))
        .unwrap();
    let listed = r.list_taints(None, None, false).unwrap();
    let taint = listed.first().expect("taint present");
    // Commit id populated post-insert (§4 invariant).
    assert!(!taint.commit_id.is_empty());
}

#[test]
fn list_taints_filters_by_kind_and_prefix() {
    let r = repo();
    r.taint("main", "/a", t_params("a", TaintEffect::Warn, "ops"))
        .unwrap();
    r.watch_path(
        "main",
        "/b",
        WatchParams {
            name: "w".into(),
            reason: "perf".into(),
            metric: None,
            threshold: None,
            direction: WatchDirection::Above,
            check_interval_secs: None,
            expires_at: None,
            severity: TaintSeverity::Low,
            propagate: true,
            agent_id: "ops".into(),
        },
    )
    .unwrap();
    let taints = r.list_taints(None, Some(TaintKind::Taint), false).unwrap();
    assert!(taints.iter().all(|t| t.kind == TaintKind::Taint));
    let a_only = r.list_taints(Some("/a"), None, false).unwrap();
    assert!(a_only.iter().all(|t| t.path.starts_with("/a")));
}
