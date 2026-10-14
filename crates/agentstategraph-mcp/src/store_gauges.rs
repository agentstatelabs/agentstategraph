//! The store gauges (DESIGN.md Plan H t-008): what's in the store, sampled
//! on a slow interval because counting reads every row's key, and what the
//! last garbage collection this server ran reclaimed.
//!
//! | Gauge | Unit | Meaning |
//! |---|---|---|
//! | `asg.store.available` | | 1 if the store answered the last sample, else 0 |
//! | `asg.store.objects` | | objects in the store (this tenant's, on Postgres) |
//! | `asg.store.commits` | | commits in the store |
//! | `asg.store.size` | `By` | the store's file on disk (SQLite only) |
//! | `asg.history.undistilled_commits` | | commits the history extractor hasn't distilled yet |
//! | `asg.gc.last_sweep.time` | `s` | when the last sweep this server ran finished, Unix time |
//! | `asg.gc.last_sweep.objects_deleted` | | what it deleted |
//! | `asg.gc.last_vacuum.bytes_reclaimed` | `By` | what the last vacuum returned to the OS |
//!
//! Measured on a 3.2 GB SQLite store (212,000 objects, 1.1 million commits),
//! one sample takes about 220 ms with a cold cache and 13 ms warm, holding
//! SQLite's connection meanwhile; hence the 60-second default.

use std::sync::{Arc, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use agentstategraph::Repository;

/// How often to sample by default.
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(60);

/// The interval from `ASG_STORE_GAUGES_INTERVAL`, in seconds: `0` turns
/// the gauges off. Unset or unreadable means [`DEFAULT_INTERVAL`].
pub fn interval_from_env() -> Option<Duration> {
    match std::env::var("ASG_STORE_GAUGES_INTERVAL") {
        Err(_) => Some(DEFAULT_INTERVAL),
        Ok(value) => match value.trim().parse::<u64>() {
            Ok(0) => None,
            Ok(seconds) => Some(Duration::from_secs(seconds)),
            Err(_) => {
                tracing::warn!(
                    value,
                    "ASG_STORE_GAUGES_INTERVAL isn't a number of seconds; sampling every 60"
                );
                Some(DEFAULT_INTERVAL)
            }
        },
    }
}

/// Describe the gauges to the current recorder.
pub fn describe() {
    metrics::describe_gauge!(
        "asg.store.available",
        "1 if the store answered the last sample, else 0."
    );
    metrics::describe_gauge!("asg.store.objects", "Objects in the store.");
    metrics::describe_gauge!("asg.store.commits", "Commits in the store.");
    metrics::describe_gauge!(
        "asg.store.size",
        metrics::Unit::Bytes,
        "The store's size on disk."
    );
    metrics::describe_gauge!(
        "asg.history.undistilled_commits",
        "Commits the history extractor hasn't distilled yet."
    );
    metrics::describe_gauge!(
        "asg.gc.last_sweep.time",
        metrics::Unit::Seconds,
        "When the last garbage collection sweep this server ran finished, as Unix time."
    );
    metrics::describe_gauge!(
        "asg.gc.last_sweep.objects_deleted",
        "Objects the last garbage collection sweep deleted."
    );
    metrics::describe_gauge!(
        "asg.gc.last_vacuum.bytes_reclaimed",
        metrics::Unit::Bytes,
        "Bytes the last vacuum returned to the operating system."
    );
}

/// Sample the store gauges now. Returns whether every count succeeded.
pub fn sample(repo: &Repository) -> bool {
    let mut ok = true;
    match repo.store_totals() {
        Ok(Some(totals)) => {
            metrics::gauge!("asg.store.objects").set(totals.objects as f64);
            metrics::gauge!("asg.store.commits").set(totals.commits as f64);
            if let Some(bytes) = totals.bytes {
                metrics::gauge!("asg.store.size").set(bytes as f64);
            }
        }
        Ok(None) => {}
        Err(e) => {
            ok = false;
            tracing::debug!(error = %e, "store gauges: counting the store failed");
        }
    }
    match repo.undistilled_commits() {
        Ok(n) => metrics::gauge!("asg.history.undistilled_commits").set(n as f64),
        Err(e) => {
            ok = false;
            tracing::debug!(error = %e, "store gauges: counting undistilled commits failed");
        }
    }
    metrics::gauge!("asg.store.available").set(if ok { 1.0 } else { 0.0 });
    ok
}

/// Sample every `interval`, on a thread of its own, for as long as `repo`
/// lives. A failure is logged once, when sampling starts to fail.
pub fn spawn(repo: &Arc<Repository>, interval: Duration) {
    describe();
    let repo: Weak<Repository> = Arc::downgrade(repo);
    let spawned = std::thread::Builder::new()
        .name("asg-store-gauges".into())
        .spawn(move || {
            let mut failing = false;
            loop {
                let Some(repo) = repo.upgrade() else { return };
                let ok = sample(&repo);
                drop(repo);
                if !ok && !failing {
                    tracing::warn!("store gauges: couldn't count the store; they'll be stale");
                }
                failing = !ok;
                std::thread::sleep(interval);
            }
        });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "store gauges: couldn't start the sampler");
    }
}

/// Record what a garbage collection sweep reported, if it changed anything.
pub fn record_sweep(report: &serde_json::Value) {
    if report.get("mutated").and_then(|v| v.as_bool()) != Some(true) {
        return;
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    metrics::gauge!("asg.gc.last_sweep.time").set(now);
    if let Some(deleted) = report.get("objects_deleted").and_then(|v| v.as_i64()) {
        metrics::gauge!("asg.gc.last_sweep.objects_deleted").set(deleted as f64);
    }
    if let Some(vacuum) = report.get("vacuum") {
        record_vacuum(vacuum);
    }
}

/// Record what a vacuum reported.
pub fn record_vacuum(report: &serde_json::Value) {
    if let Some(bytes) = report.get("bytes_reclaimed").and_then(|v| v.as_i64()) {
        metrics::gauge!("asg.gc.last_vacuum.bytes_reclaimed").set(bytes as f64);
    }
}
