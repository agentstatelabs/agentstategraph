//! `Repository` scenarios against every built-in backend (DESIGN.md Plan D
//! t-001). The storage-level contract lives in
//! `agentstategraph-storage/tests/conformance.rs`; these check that the
//! repository built on each backend behaves the same end to end.
//!
//! Postgres runs when `TEST_DATABASE_URL` is set, with a tenant per test.
//! Known failures are listed in [`KNOWN_FAILURES`] with the Plan D task that
//! owns the fix; a deterministic one must keep failing until its entry is
//! removed.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Barrier};
use std::thread::JoinHandle;

use agentstategraph::{CommitOptions, RepoError, Repository, RetentionPolicy};
use agentstategraph_core::{
    Authority, CommitBuilder, EpochStatus, Intent, IntentCategory, Namespace, Object,
};
use agentstategraph_storage::{MemoryStorage, PostgresStorage, SqliteStorage, Storage};
use serde_json::json;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Backend {
    Memory,
    Sqlite,
    Postgres,
}

struct Known {
    case: &'static str,
    backend: Backend,
    owner: &'static str,
}

const KNOWN_FAILURES: &[Known] = &[];

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// Opens `Repository` values onto one test's store. Separate repositories
/// over separate storage handles stand in for separate processes.
struct Fixture {
    backend: Backend,
    opener: Arc<dyn Fn() -> Repository + Send + Sync>,
    storage: Arc<dyn Fn() -> Arc<dyn Storage + Send + Sync> + Send + Sync>,
    cleanup: Option<std::path::PathBuf>,
}

impl Fixture {
    fn open(&self) -> Repository {
        (self.opener)()
    }

    /// A storage handle onto the same store, below the repository.
    fn storage(&self) -> Arc<dyn Storage + Send + Sync> {
        (self.storage)()
    }

    /// A plain thread: every backend, Postgres included, must work from one.
    fn spawn<T: Send + 'static>(&self, f: impl FnOnce() -> T + Send + 'static) -> JoinHandle<T> {
        std::thread::spawn(f)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(path) = &self.cleanup {
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
            }
        }
    }
}

/// Connects from whatever thread calls it, with no runtime and no lock:
/// concurrent connects serialize their schema setup in the database.
fn postgres_connect(url: &str, tenant: &str) -> PostgresStorage {
    PostgresStorage::connect_tenant_blocking(url, tenant, 8).expect("connect to TEST_DATABASE_URL")
}

fn fixtures(case: &str) -> Vec<Fixture> {
    let shared: Arc<dyn Storage + Send + Sync> = Arc::new(MemoryStorage::new());
    let raw = Arc::clone(&shared);
    let mut all = vec![Fixture {
        backend: Backend::Memory,
        opener: Arc::new(move || Repository::from_shared(Arc::clone(&shared))),
        storage: Arc::new(move || Arc::clone(&raw)),
        cleanup: None,
    }];

    let path = std::env::temp_dir().join(format!(
        "asg-backend-conformance-{case}-{}.db",
        uuid::Uuid::new_v4().simple()
    ));
    let (opened, raw) = (path.clone(), path.clone());
    all.push(Fixture {
        backend: Backend::Sqlite,
        opener: Arc::new(move || {
            Repository::new(Box::new(SqliteStorage::open(&opened).expect("open sqlite")))
        }),
        storage: Arc::new(move || Arc::new(SqliteStorage::open(&raw).expect("open sqlite"))),
        cleanup: Some(path),
    });

    match std::env::var("TEST_DATABASE_URL") {
        Ok(url) if !url.is_empty() => {
            let tenant = format!("repo-conformance-{case}-{}", uuid::Uuid::new_v4().simple());
            let (raw_url, raw_tenant) = (url.clone(), tenant.clone());
            all.push(Fixture {
                backend: Backend::Postgres,
                opener: Arc::new(move || {
                    Repository::new(Box::new(postgres_connect(&url, &tenant)))
                }),
                storage: Arc::new(move || Arc::new(postgres_connect(&raw_url, &raw_tenant))),
                cleanup: None,
            });
        }
        _ => {
            // CI sets ASG_REQUIRE_POSTGRES so a missing database fails the
            // run instead of silently skipping the Postgres legs.
            let required =
                std::env::var("ASG_REQUIRE_POSTGRES").is_ok_and(|v| !v.is_empty() && v != "0");
            assert!(
                !required,
                "ASG_REQUIRE_POSTGRES is set, but TEST_DATABASE_URL isn't ({case})"
            );
            eprintln!("skip postgres for {case}: TEST_DATABASE_URL not set");
        }
    }
    all
}

fn run(case: &str, body: fn(&Fixture)) {
    let mut problems = Vec::new();
    for fixture in fixtures(case) {
        let backend = fixture.backend;
        let outcome = catch_unwind(AssertUnwindSafe(|| body(&fixture)));
        let known = KNOWN_FAILURES
            .iter()
            .find(|k| k.case == case && k.backend == backend);
        match (outcome.is_ok(), known) {
            (true, None) => {}
            (false, None) => problems.push(format!("{backend:?}: failed (see panic above)")),
            (false, Some(k)) => eprintln!("known failure, {backend:?} {case}: {}", k.owner),
            (true, Some(k)) => problems.push(format!(
                "{backend:?}: now passes, but is listed in KNOWN_FAILURES ({}). Remove the entry.",
                k.owner
            )),
        }
    }
    assert!(problems.is_empty(), "{case}: {}", problems.join("; "));
}

fn opts(desc: &str) -> CommitOptions {
    CommitOptions::new("agent/conformance", IntentCategory::Refine, desc)
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

fn write_branch_merge_history(f: &Fixture) {
    let repo = f.open();
    repo.init().unwrap();
    repo.set_json("main", "/config/name", &json!("prod"), opts("name"))
        .unwrap();
    repo.branch("feature", "main").unwrap();
    repo.set_json("feature", "/config/replicas", &json!(3), opts("scale"))
        .unwrap();
    repo.set_json("main", "/config/region", &json!("us-west"), opts("region"))
        .unwrap();
    repo.merge(
        "feature",
        "main",
        CommitOptions::new("agent/conformance", IntentCategory::Merge, "adopt feature"),
    )
    .unwrap();

    assert_eq!(
        repo.get_json("main", "/config").unwrap(),
        json!({
            "name": "prod",
            "replicas": 3,
            "region": "us-west",
        })
    );
    let walk = repo.log_dag("main", 50).unwrap();
    let descriptions: Vec<&str> = walk
        .commits
        .iter()
        .map(|c| c.intent.description.as_str())
        .collect();
    assert!(
        descriptions.contains(&"scale"),
        "the DAG walk reaches the merged branch's commit: {descriptions:?}"
    );
    let blame = repo.blame("main", "/config/replicas").unwrap();
    assert_eq!(blame.intent_description, "scale");

    // A second repository over the same store sees the same state.
    let other = f.open();
    assert_eq!(other.head("main").unwrap(), repo.head("main").unwrap());
}

/// The v1.2.5 lost-update test, on every backend: separate repositories
/// (separate processes, for SQLite and Postgres) append concurrently into
/// shared parent maps, and every acknowledged write must survive.
fn concurrent_writers_lose_nothing(f: &Fixture) {
    const WRITERS: usize = 4;
    const PER_WRITER: usize = 12;
    f.open().init().unwrap();

    let barrier = Arc::new(Barrier::new(WRITERS));
    let writers: Vec<_> = (0..WRITERS)
        .map(|w| {
            let (repo, barrier) = (f.open(), Arc::clone(&barrier));
            f.spawn(move || {
                barrier.wait();
                for i in 0..PER_WRITER {
                    repo.set_json(
                        "main",
                        &format!("/ledger/sym_{}/w{w}_{i}", i % 3),
                        &json!({ "writer": w, "i": i }),
                        opts("append"),
                    )
                    .unwrap();
                }
            })
        })
        .collect();
    for w in writers {
        w.join().unwrap();
    }

    let repo = f.open();
    let missing: Vec<String> = (0..WRITERS)
        .flat_map(|w| (0..PER_WRITER).map(move |i| format!("/ledger/sym_{}/w{w}_{i}", i % 3)))
        .filter(|path| repo.get_json("main", path).is_err())
        .collect();
    assert!(
        missing.is_empty(),
        "{} of {} acknowledged writes lost: {missing:?}",
        missing.len(),
        WRITERS * PER_WRITER
    );
}

fn epoch_create_seal_export(f: &Fixture) {
    let repo = f.open();
    repo.init().unwrap();
    repo.create_epoch("e1", "conformance epoch", vec![])
        .unwrap();
    repo.set_active_epoch(Some("e1".to_string())).unwrap();
    repo.set_json("main", "/a", &json!(1), opts("one")).unwrap();
    repo.set_json("main", "/b", &json!(2), opts("two")).unwrap();
    repo.set_active_epoch(None).unwrap();
    repo.seal_epoch("e1", "done").unwrap();

    let sealed = repo.get_epoch("e1").unwrap();
    assert_eq!(sealed.status, EpochStatus::Sealed);
    assert!(
        sealed.commits.len() >= 2,
        "commits made in the epoch are its members"
    );
    let bundle = repo.export_epoch("e1").unwrap();
    assert_eq!(bundle["epoch"]["id"], json!("e1"));
    assert!(
        bundle["commits"].as_array().is_some_and(|c| c.len() >= 2),
        "the export bundle lists the epoch's commits"
    );
}

/// A sealed epoch protects its own namespace's history and no other's: the
/// v1.1.x scoping that Postgres silently skipped while it dropped an epoch's
/// namespace.
fn sealed_epoch_binds_only_its_namespace(f: &Fixture) {
    let open_in = |ns: &str| {
        f.open()
            .with_namespace(Namespace::new(ns).unwrap())
            .with_epoch_seal_strict(true)
    };
    let (team_a, team_b) = (open_in("team-a"), open_in("team-b"));
    let mut before_seal = Vec::new();
    for repo in [&team_a, &team_b] {
        repo.init().unwrap();
        repo.set_json("main", "/a", &json!(1), opts("a")).unwrap();
        before_seal.push(repo.set_json("main", "/b", &json!(2), opts("b")).unwrap());
        repo.set_json("main", "/c", &json!(3), opts("c")).unwrap();
    }

    team_a
        .create_epoch("ship-a", "team a release", vec![])
        .unwrap();
    team_a.seal_epoch("ship-a", "shipped").unwrap();
    let sealed = team_a.get_epoch("ship-a").unwrap();
    assert_eq!(
        sealed.namespace.as_deref(),
        Some("team-a"),
        "the epoch keeps its namespace"
    );
    assert!(sealed.seal_hash.is_some(), "the seal digest is stored");

    // Rewinding team a's main past its sealed commits is refused...
    assert!(
        matches!(
            team_a.set_ref("main", before_seal[0]),
            Err(RepoError::EpochSealViolated { .. })
        ),
        "a sealed epoch must protect its own namespace"
    );
    // ...but team b's history isn't covered by team a's seal.
    team_b
        .set_ref("main", before_seal[1])
        .expect("another namespace is not bound by team a's seal");
}

/// Search answers from the current tree on every backend, whether from an
/// index (SQLite, Postgres) or a tree walk (memory), and keeps doing so as
/// writes replace and remove values.
fn search_follows_writes(f: &Fixture) {
    let repo = f.open();
    repo.init().unwrap();
    repo.set_json("main", "/config/name", &json!("Prod Cluster"), opts("name"))
        .unwrap();
    repo.set_json("main", "/config/owner", &json!("team-blue"), opts("owner"))
        .unwrap();
    let hits = |repo: &Repository, query: &str| -> Vec<(String, String)> {
        let mut hits = repo.search_values("main", query, None).unwrap();
        hits.sort();
        hits
    };
    let one = |path: &str, value: &str| vec![(path.to_string(), value.to_string())];

    // The first search builds the index where there is one.
    assert_eq!(hits(&repo, "prod"), one("/config/name", "Prod Cluster"));
    repo.set_json("main", "/config/name", &json!("Staging"), opts("rename"))
        .unwrap();
    assert!(hits(&repo, "prod").is_empty(), "a replaced value is gone");
    assert_eq!(hits(&repo, "stag"), one("/config/name", "Staging"));
    repo.delete("main", "/config/owner", opts("drop owner"))
        .unwrap();
    assert!(hits(&repo, "blue").is_empty(), "a deleted value is gone");

    // Another process sees the same answers.
    let other = f.open();
    assert_eq!(hits(&other, "stag"), one("/config/name", "Staging"));
    assert!(hits(&other, "blue").is_empty());
}

/// Writers racing on the same paths: once they finish, search reports each
/// path's current value and nothing older.
fn search_follows_concurrent_writers(f: &Fixture) {
    const WRITERS: usize = 4;
    const PER_WRITER: usize = 10;
    let repo = f.open();
    repo.init().unwrap();
    for slot in 0..3 {
        repo.set_json(
            "main",
            &format!("/slot/s{slot}"),
            &json!(format!("initial {slot}")),
            opts("seed"),
        )
        .unwrap();
    }
    assert_eq!(
        repo.search_values("main", "initial", None).unwrap().len(),
        3
    );

    let barrier = Arc::new(Barrier::new(WRITERS));
    let writers: Vec<_> = (0..WRITERS)
        .map(|w| {
            let (repo, barrier) = (f.open(), Arc::clone(&barrier));
            f.spawn(move || {
                barrier.wait();
                for i in 0..PER_WRITER {
                    repo.set_json(
                        "main",
                        &format!("/slot/s{}", i % 3),
                        &json!(format!("w{w} i{i} mark")),
                        opts("overwrite"),
                    )
                    .unwrap();
                }
            })
        })
        .collect();
    for w in writers {
        w.join().unwrap();
    }

    let current: std::collections::HashSet<(String, String)> = (0..3)
        .map(|slot| {
            let path = format!("/slot/s{slot}");
            let value = repo.get_json("main", &path).unwrap();
            (path, value.as_str().unwrap().to_string())
        })
        .collect();
    let found: std::collections::HashSet<(String, String)> = repo
        .search_values("main", "mark", Some(100))
        .unwrap()
        .into_iter()
        .collect();
    assert_eq!(found, current, "search reports exactly the current values");
}

/// The history extractor folds every commit on the SQL backends; memory keeps
/// no history.
fn history_through_the_repository(f: &Fixture) {
    let repo = f.open();
    repo.init().unwrap();
    for i in 0..5 {
        repo.set_json("main", &format!("/n/k{i}"), &json!(i), opts("count"))
            .unwrap();
    }
    let commits = repo.log_dag("main", 1000).unwrap().commits.len();
    let report = repo.extract_history(2).unwrap();
    let folded: i64 = repo
        .history_rollup()
        .unwrap()
        .iter()
        .map(|b| b.commit_count)
        .sum();
    if f.backend == Backend::Memory {
        assert_eq!(report.commits_processed, 0);
        assert_eq!(folded, 0);
        return;
    }
    assert_eq!(report.commits_processed, commits);
    assert_eq!(folded, commits as i64);
    assert_eq!(repo.extract_history(2).unwrap().commits_processed, 0);
}

fn keep_only_tips() -> RetentionPolicy {
    RetentionPolicy {
        keep_recent: 1,
        checkpoint_every: 0,
        keep_milestones: false,
    }
}

/// A sweep through the repository leaves every branch readable, and a later
/// merge still finds its base.
fn sweep_keeps_every_branch_readable(f: &Fixture) {
    let repo = f.open();
    repo.init().unwrap();
    for i in 0..4 {
        repo.set_json("main", &format!("/a/k{i}"), &json!(i), opts("main"))
            .unwrap();
    }
    repo.branch("feature", "main").unwrap();
    repo.set_json("feature", "/f", &json!("feature"), opts("feature"))
        .unwrap();
    repo.set_json("main", "/m", &json!("main"), opts("main"))
        .unwrap();
    repo.extract_history(1000).unwrap();

    let report = repo.gc_sweep(keep_only_tips(), true, false).unwrap();
    assert_eq!(report["mutated"], json!(true), "{report}");
    for branch in ["main", "feature"] {
        let head = repo.head(branch).unwrap();
        assert_eq!(repo.first_missing_object(&head).unwrap(), None, "{branch}");
    }
    repo.merge(
        "feature",
        "main",
        CommitOptions::new("agent/conformance", IntentCategory::Merge, "merge"),
    )
    .unwrap();
    assert_eq!(repo.get_json("main", "/f").unwrap(), json!("feature"));
    assert_eq!(repo.get_json("main", "/a/k3").unwrap(), json!(3));
}

/// Every write stores its new tree, then its commit, then moves the ref. A
/// sweep that runs after the first step must not take the tree, or the ref
/// lands on a tree with objects missing.
fn sweep_spares_a_write_in_flight(f: &Fixture) {
    let repo = f.open();
    repo.init().unwrap();
    for i in 0..3 {
        repo.set_json("main", "/v", &json!(i), opts("edit"))
            .unwrap();
    }
    repo.extract_history(1000).unwrap();
    let head = repo.head("main").unwrap();

    // A writer has stored its tree...
    let store = f.storage();
    let leaf = store.put_object(&Object::string("in flight")).unwrap();
    let root = store
        .put_object(&Object::map([("v".to_string(), leaf)].into()))
        .unwrap();
    // ...when a sweep runs.
    let report = repo.gc_sweep(keep_only_tips(), true, false).unwrap();
    assert_eq!(report["mutated"], json!(true), "{report}");
    // Then it commits.
    let c = CommitBuilder::new(
        root,
        "agent/conformance",
        Authority::simple("conformance"),
        Intent::new(IntentCategory::Refine, "in flight"),
    )
    .parents(vec![head])
    .build();
    store.put_commit(&c).unwrap();
    repo.set_ref("main", c.id).unwrap();
    assert_eq!(
        repo.first_missing_object(&c.id).unwrap(),
        None,
        "the sweep took a write in flight"
    );
}

#[test]
fn write_branch_merge() {
    run("write_branch_merge_history", write_branch_merge_history);
}

#[test]
fn concurrent_writers() {
    run(
        "concurrent_writers_lose_nothing",
        concurrent_writers_lose_nothing,
    );
}

#[test]
fn epochs() {
    run("epoch_create_seal_export", epoch_create_seal_export);
}

#[test]
fn epoch_namespace_binding() {
    run(
        "sealed_epoch_binds_only_its_namespace",
        sealed_epoch_binds_only_its_namespace,
    );
}

#[test]
fn search() {
    run("search_follows_writes", search_follows_writes);
}

#[test]
fn search_concurrent_writers() {
    run(
        "search_follows_concurrent_writers",
        search_follows_concurrent_writers,
    );
}

#[test]
fn history() {
    run(
        "history_through_the_repository",
        history_through_the_repository,
    );
}

#[test]
fn gc_sweep() {
    run(
        "sweep_keeps_every_branch_readable",
        sweep_keeps_every_branch_readable,
    );
}

#[test]
fn gc_write_in_flight() {
    run(
        "sweep_spares_a_write_in_flight",
        sweep_spares_a_write_in_flight,
    );
}
