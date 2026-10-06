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
use std::sync::{Arc, Barrier, Mutex, OnceLock};
use std::thread::JoinHandle;

use agentstategraph::{CommitOptions, Repository};
use agentstategraph_core::{EpochStatus, IntentCategory};
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

const KNOWN_FAILURES: &[Known] = &[Known {
    case: "epoch_create_seal_export",
    backend: Backend::Postgres,
    owner: "Plan D t-003: PostgresStorage::create_epoch fails ('error serializing parameter 10')",
}];

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// Opens `Repository` values onto one test's store. Separate repositories
/// over separate storage handles stand in for separate processes.
struct Fixture {
    backend: Backend,
    opener: Arc<dyn Fn() -> Repository + Send + Sync>,
    runtime: Option<tokio::runtime::Handle>,
    cleanup: Option<std::path::PathBuf>,
}

impl Fixture {
    fn open(&self) -> Repository {
        (self.opener)()
    }

    fn spawn<T: Send + 'static>(&self, f: impl FnOnce() -> T + Send + 'static) -> JoinHandle<T> {
        let runtime = self.runtime.clone();
        std::thread::spawn(move || {
            let _ctx = runtime.as_ref().map(|h| h.enter());
            f()
        })
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

fn postgres_runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("build tokio runtime")
    })
}

/// One connect at a time: concurrent connects race on schema DDL (Plan D
/// t-006), and these scenarios shouldn't depend on that race.
fn postgres_connect(url: &str, tenant: &str) -> PostgresStorage {
    static CONNECT: Mutex<()> = Mutex::new(());
    let _one_at_a_time = CONNECT.lock().unwrap_or_else(|p| p.into_inner());
    postgres_runtime()
        .block_on(PostgresStorage::connect_tenant(url, tenant))
        .expect("connect to TEST_DATABASE_URL")
}

fn fixtures(case: &str) -> Vec<Fixture> {
    let shared: Arc<dyn Storage + Send + Sync> = Arc::new(MemoryStorage::new());
    let mut all = vec![Fixture {
        backend: Backend::Memory,
        opener: Arc::new(move || Repository::from_shared(Arc::clone(&shared))),
        runtime: None,
        cleanup: None,
    }];

    let path = std::env::temp_dir().join(format!(
        "asg-backend-conformance-{case}-{}.db",
        uuid::Uuid::new_v4().simple()
    ));
    let opened = path.clone();
    all.push(Fixture {
        backend: Backend::Sqlite,
        opener: Arc::new(move || {
            Repository::new(Box::new(SqliteStorage::open(&opened).expect("open sqlite")))
        }),
        runtime: None,
        cleanup: Some(path),
    });

    match std::env::var("TEST_DATABASE_URL") {
        Ok(url) if !url.is_empty() => {
            let tenant = format!("repo-conformance-{case}-{}", uuid::Uuid::new_v4().simple());
            all.push(Fixture {
                backend: Backend::Postgres,
                opener: Arc::new(move || {
                    Repository::new(Box::new(postgres_connect(&url, &tenant)))
                }),
                runtime: Some(postgres_runtime().handle().clone()),
                cleanup: None,
            });
        }
        _ => eprintln!("skip postgres for {case}: TEST_DATABASE_URL not set"),
    }
    all
}

fn run(case: &str, body: fn(&Fixture)) {
    let mut problems = Vec::new();
    for fixture in fixtures(case) {
        let backend = fixture.backend;
        let outcome = {
            let _ctx = fixture.runtime.as_ref().map(|h| h.enter());
            catch_unwind(AssertUnwindSafe(|| body(&fixture)))
        };
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
