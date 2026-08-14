//! Plan A t-004: backfill + reconcile the history extractor against a real
//! field DB.
//!
//! Ignored by default — it needs a large local database that is not shipped to
//! CI, and the extractor WRITES the `asg_history_*` tables, so point it at a
//! *copy* (on APFS, `cp -c` gives an instant copy-on-write clone). Reconciles
//! the distilled rollup against `COUNT(*)` on the commits table and prints the
//! store-shape / amplification numbers Plan B keys off.
//!
//! Run:
//!   ASG_HISTORY_BACKFILL_DB=/path/to/copy.db \
//!     cargo test -p agentstategraph --features sqlite \
//!     --test history_backfill_field_db -- --ignored --nocapture

use std::time::Instant;

use agentstategraph::Repository;
use agentstategraph_storage::SqliteStorage;

#[test]
#[ignore = "needs ASG_HISTORY_BACKFILL_DB pointing at a writable copy of a real field DB"]
fn backfill_reconciles_against_commit_count() {
    let Ok(path) = std::env::var("ASG_HISTORY_BACKFILL_DB") else {
        eprintln!("skip: set ASG_HISTORY_BACKFILL_DB to a writable copy of a field DB");
        return;
    };

    let storage = SqliteStorage::open(&path).expect("open field db copy");
    let repo = Repository::new(Box::new(storage));

    // Ground truth, straight from the commits table (raw COUNT(*)).
    let shape0 = repo.history_store_shape().expect("store shape");
    let commit_count = shape0["commits"].as_i64().unwrap();
    println!(
        "field db: {} commits, {} objects, {:.2} GB on disk, dbstat={}",
        commit_count,
        shape0["objects"].as_i64().unwrap(),
        shape0["total_bytes"].as_i64().unwrap() as f64 / 1e9,
        shape0["dbstat_available"].as_bool().unwrap(),
    );

    let t0 = Instant::now();
    let report = repo.extract_history(5000).expect("extract");
    let elapsed = t0.elapsed();
    println!(
        "backfill: folded {} commits in {:.2?} ({:.0} commits/s)",
        report.commits_processed,
        elapsed,
        report.commits_processed as f64 / elapsed.as_secs_f64().max(1e-9),
    );

    let rollup = repo.history_rollup().expect("rollup");
    let rollup_total: i64 = rollup.iter().map(|r| r.commit_count).sum();
    println!(
        "rollup: {} buckets, {} commits total",
        rollup.len(),
        rollup_total
    );

    let shape = repo.history_store_shape().unwrap();
    println!(
        "amplification: {:.1} objects/commit",
        shape["path_copy_amplification"]["objects_per_commit"]
            .as_f64()
            .unwrap()
    );
    if let Some(tables) = shape["tables"].as_array() {
        for t in tables.iter().take(6) {
            println!(
                "  table {:<28} {:>13} bytes",
                t["name"].as_str().unwrap_or("?"),
                t["bytes"].as_i64().unwrap_or(0)
            );
        }
    }

    // Reconcile: the walk folded exactly as many commits as the table holds,
    // and every commit landed in exactly one rollup bucket.
    assert_eq!(
        report.commits_processed as i64, commit_count,
        "extractor must process every commit"
    );
    assert_eq!(
        rollup_total, commit_count,
        "rollup total must reconcile with COUNT(*) commits"
    );

    // A second pass is a no-op — the cursor is caught up — and doesn't
    // double-count.
    let again = repo.extract_history(5000).expect("re-extract");
    assert_eq!(again.commits_processed, 0, "re-run must be a no-op");
    let rollup2: i64 = repo
        .history_rollup()
        .unwrap()
        .iter()
        .map(|r| r.commit_count)
        .sum();
    assert_eq!(rollup2, commit_count, "idempotent — no double counting");
}

/// Plan B t-001: run the reachability marker over a real field DB and report
/// the reclaimable estimate under "keep current tips + milestones". Validates
/// bounded memory over 14.8M objects. Same setup/caveats as the backfill test.
#[test]
#[ignore = "needs ASG_HISTORY_BACKFILL_DB pointing at a writable copy of a real field DB"]
fn gc_reachability_on_field_db() {
    let Ok(path) = std::env::var("ASG_HISTORY_BACKFILL_DB") else {
        eprintln!("skip: set ASG_HISTORY_BACKFILL_DB to a writable copy of a field DB");
        return;
    };
    let storage = SqliteStorage::open(&path).expect("open field db copy");
    let repo = Repository::new(Box::new(storage));

    // Populate milestones so retained roots are part of the keep-set.
    repo.extract_history(5000).expect("extract");

    let t0 = Instant::now();
    let report = repo.gc_reachability_report().expect("gc reachability");
    let elapsed = t0.elapsed();
    println!(
        "gc mark (tips + milestones) in {:.2?}: {}",
        elapsed,
        serde_json::to_string_pretty(&report).unwrap()
    );

    let total = report["total_objects"].as_i64().unwrap();
    let live = report["live_objects"].as_i64().unwrap();
    let reclaimable = report["reclaimable_objects"].as_i64().unwrap();
    assert_eq!(live + reclaimable, total, "live + reclaimable == total");
    assert!(live > 0 && live <= total);
}

/// Plan B t-004: sweep a real field DB under a lean retention policy, then
/// VACUUM, and measure the file shrink end-to-end. Ignored (needs a writable
/// copy). Run:
///   ASG_HISTORY_BACKFILL_DB=/path/to/copy.db \
///     cargo test -p agentstategraph --features sqlite \
///     --test history_backfill_field_db gc_sweep_vacuum_on_field_db -- --ignored --nocapture
#[test]
#[ignore = "needs ASG_HISTORY_BACKFILL_DB pointing at a writable copy of a real field DB"]
fn gc_sweep_vacuum_on_field_db() {
    let Ok(path) = std::env::var("ASG_HISTORY_BACKFILL_DB") else {
        eprintln!("skip: set ASG_HISTORY_BACKFILL_DB to a writable copy of a field DB");
        return;
    };
    let storage = SqliteStorage::open(&path).expect("open field db copy");
    let repo = Repository::new(Box::new(storage));

    // Distill first (so the sweep's safety gate passes).
    repo.extract_history(5000).expect("extract");

    // Lean policy: keep only recent + sparse checkpoints (+ tips, sealed,
    // milestones always).
    let policy = agentstategraph::RetentionPolicy {
        keep_recent: 200,
        checkpoint_every: 1000,
        keep_milestones: true,
    };

    let t0 = Instant::now();
    let report = repo.gc_sweep(policy, true, true).expect("sweep+vacuum");
    println!(
        "sweep+vacuum in {:.2?}: {}",
        t0.elapsed(),
        serde_json::to_string_pretty(&report).unwrap()
    );

    assert_eq!(report["mutated"], true);
    let before = report["objects_before"].as_i64().unwrap();
    let after = report["objects_after"].as_i64().unwrap();
    assert!(after < before, "sweep should delete objects");
    let vac = &report["vacuum"];
    assert!(vac["bytes_after"].as_i64().unwrap() <= vac["bytes_before"].as_i64().unwrap());
}
