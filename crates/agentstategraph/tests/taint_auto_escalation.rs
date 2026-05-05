//! 0.7.75 §5 — watch auto-escalation integration tests.

use agentstategraph::{CommitOptions, Repository};
use agentstategraph_core::IntentCategory;
use agentstategraph_storage::SqliteStorage;
use agentstategraph_taint::{TaintKind, TaintSeverity, WatchDirection, WatchParams};
use serde_json::json;

fn repo() -> Repository {
    let r = Repository::new(Box::new(
        SqliteStorage::in_memory().expect("in-memory sqlite"),
    ));
    r.init().unwrap();
    r
}

fn watch(
    r: &Repository,
    path: &str,
    name: &str,
    metric: &str,
    threshold: f64,
    direction: WatchDirection,
) {
    r.watch_path(
        "main",
        path,
        WatchParams {
            name: name.into(),
            reason: "perf".into(),
            metric: Some(metric.into()),
            threshold: Some(threshold),
            direction,
            check_interval_secs: Some(60),
            expires_at: None,
            severity: TaintSeverity::Low,
            propagate: true,
            agent_id: "ops".into(),
        },
    )
    .unwrap();
}

fn write_json(r: &Repository, path: &str, value: serde_json::Value) {
    r.set_json(
        "main",
        path,
        &value,
        CommitOptions::new("agent-1", IntentCategory::Refine, "write"),
    )
    .unwrap();
}

#[test]
fn watch_above_threshold_auto_escalates() {
    let r = repo();
    watch(
        &r,
        "/cluster/disk",
        "disk-80",
        "disk_used_pct",
        80.0,
        WatchDirection::Above,
    );
    // Writing a value above threshold should create an auto-taint.
    write_json(&r, "/cluster/disk", json!({"disk_used_pct": 82.0}));
    let tainted = r
        .list_taints(Some("/cluster"), Some(TaintKind::Taint), false)
        .unwrap();
    assert_eq!(tainted.len(), 1, "expected 1 auto-taint, got {:?}", tainted);
    assert!(tainted[0].name.starts_with("watch-threshold-exceeded-"));
}

#[test]
fn watch_below_threshold_fires_when_direction_below() {
    let r = repo();
    watch(
        &r,
        "/metric",
        "low-free",
        "free_pct",
        10.0,
        WatchDirection::Below,
    );
    write_json(&r, "/metric", json!({"free_pct": 5.0}));
    let tainted = r.list_taints(None, Some(TaintKind::Taint), false).unwrap();
    assert_eq!(tainted.len(), 1);
}

#[test]
fn watch_threshold_not_crossed_does_not_fire() {
    let r = repo();
    watch(
        &r,
        "/cluster/disk",
        "disk-80",
        "disk_used_pct",
        80.0,
        WatchDirection::Above,
    );
    write_json(&r, "/cluster/disk", json!({"disk_used_pct": 75.0}));
    let tainted = r.list_taints(None, Some(TaintKind::Taint), false).unwrap();
    assert!(tainted.is_empty());
}

#[test]
fn auto_escalation_is_idempotent() {
    let r = repo();
    watch(
        &r,
        "/cluster/disk",
        "disk-80",
        "disk_used_pct",
        80.0,
        WatchDirection::Above,
    );
    // Cross twice in a row — should only produce ONE auto-taint.
    write_json(&r, "/cluster/disk", json!({"disk_used_pct": 82.0}));
    write_json(&r, "/cluster/disk", json!({"disk_used_pct": 85.0}));
    let tainted = r.list_taints(None, Some(TaintKind::Taint), false).unwrap();
    assert_eq!(tainted.len(), 1);
}

#[test]
fn auto_taint_metadata_cites_watch_source() {
    let r = repo();
    watch(
        &r,
        "/cluster/disk",
        "disk-80",
        "disk_used_pct",
        80.0,
        WatchDirection::Above,
    );
    write_json(&r, "/cluster/disk", json!({"disk_used_pct": 82.0}));
    let tainted = r.list_taints(None, Some(TaintKind::Taint), false).unwrap();
    let t = &tainted[0];
    let auto = t
        .metadata
        .get("auto_escalated")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    assert!(auto, "auto_escalated flag should be true");
    let source = t
        .metadata
        .get("source_watch_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(!source.is_empty(), "source_watch_id should be populated");
    let observed = t
        .metadata
        .get("observed")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    assert_eq!(observed, 82.0);
}
