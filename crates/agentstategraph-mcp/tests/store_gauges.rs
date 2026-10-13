//! The store gauges: sampled from the store, and recorded from what a
//! garbage collection run reports.

use agentstategraph::{CommitOptions, Repository};
use agentstategraph_core::{IntentCategory, Object};
use agentstategraph_mcp::store_gauges;
use agentstategraph_storage::SqliteStorage;
use oxtel::sdk::metrics::{MeterProvider, PointValue};

fn gauge(meters: &MeterProvider, name: &str) -> Option<f64> {
    let family = meters.collect().into_iter().find(|f| f.name == name)?;
    match family.points.first()?.value {
        PointValue::Gauge(v) => Some(v),
        ref other => panic!("{name} isn't a gauge: {other:?}"),
    }
}

#[test]
fn the_gauges_count_the_store() {
    let path = std::env::temp_dir().join(format!(
        "asg-store-gauges-{}.db",
        uuid::Uuid::new_v4().simple()
    ));
    let repo = Repository::new(Box::new(SqliteStorage::open(&path).unwrap()));
    repo.init().unwrap();
    for i in 0..3 {
        let opts = CommitOptions::new("agent/test", IntentCategory::Refine, format!("set {i}"));
        repo.set("main", &format!("/k{i}"), &Object::string("v"), opts)
            .unwrap();
    }

    let meters = MeterProvider::builder().build();
    let recorder = meters.recorder();
    let ok = metrics::with_local_recorder(&recorder, || {
        store_gauges::describe();
        store_gauges::sample(&repo)
    });
    assert!(ok, "sampling succeeded");
    let totals = repo.store_totals().unwrap().expect("SQLite counts");
    assert_eq!(
        gauge(&meters, "asg.store.objects"),
        Some(totals.objects as f64)
    );
    assert_eq!(
        gauge(&meters, "asg.store.commits"),
        Some(totals.commits as f64)
    );
    assert!(totals.commits >= 3, "{totals:?}");
    assert!(gauge(&meters, "asg.store.size").unwrap() > 0.0);
    assert_eq!(
        gauge(&meters, "asg.history.undistilled_commits"),
        Some(repo.undistilled_commits().unwrap() as f64)
    );
    let size = meters
        .collect()
        .into_iter()
        .find(|f| f.name == "asg.store.size")
        .unwrap();
    assert_eq!(size.unit.as_deref(), Some("By"));
    drop(repo);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_sweep_and_a_vacuum_are_recorded() {
    let meters = MeterProvider::builder().build();
    let recorder = meters.recorder();
    metrics::with_local_recorder(&recorder, || {
        store_gauges::record_sweep(&serde_json::json!({
            "mutated": true,
            "objects_deleted": 7,
            "vacuum": {"bytes_reclaimed": 4096},
        }));
        // A dry run changes nothing, so it isn't the last sweep.
        store_gauges::record_sweep(&serde_json::json!({
            "dry_run": true,
            "mutated": false,
            "objects_deleted": 99,
        }));
    });
    assert_eq!(
        gauge(&meters, "asg.gc.last_sweep.objects_deleted"),
        Some(7.0)
    );
    assert_eq!(
        gauge(&meters, "asg.gc.last_vacuum.bytes_reclaimed"),
        Some(4096.0)
    );
    assert!(gauge(&meters, "asg.gc.last_sweep.time").unwrap() > 1.7e9);
}
