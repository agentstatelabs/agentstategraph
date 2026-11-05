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

/// The missing set (DESIGN.md Plan G t-003) on each backend. In a history
/// with merges both ways, for several heads and haves, the commits are
/// exactly those `list_commits_dag` reaches from the heads and not from the
/// haves, parents first, and a remote holding the haves' history plus the
/// set has every commit it should with its whole tree.
fn missing_set_matches_the_dag(f: &Fixture) {
    use agentstategraph::sync::missing_set;
    use agentstategraph_core::ObjectId;
    use agentstategraph_storage::{CommitStore, ObjectStore};
    use std::collections::{HashMap, HashSet};

    let repo = f.open();
    repo.init().unwrap();
    repo.set_json(
        "main",
        "/config",
        &json!({"name": "prod", "replicas": 1}),
        opts("init"),
    )
    .unwrap();
    let start = repo.head("main").unwrap();
    repo.branch("feature", "main").unwrap();
    for i in 0..3 {
        repo.set_json(
            "feature",
            &format!("/feature/f{i}"),
            &json!(i),
            opts("feature"),
        )
        .unwrap();
        repo.set_json(
            "main",
            &format!("/main/m{i}"),
            &json!({"i": i, "list": [i, i + 1]}),
            opts("main"),
        )
        .unwrap();
    }
    let main_before = repo.head("main").unwrap();
    let feature_tip = repo.head("feature").unwrap();
    let merge = || CommitOptions::new("agent/conformance", IntentCategory::Merge, "merge");
    repo.merge("feature", "main", merge()).unwrap();
    repo.set_json("feature", "/feature/after", &json!(true), opts("after"))
        .unwrap();
    repo.merge("main", "feature", merge()).unwrap();
    let merged = repo.head("main").unwrap();
    let feature = repo.head("feature").unwrap();

    let store = f.storage();
    let dag = |heads: &[ObjectId]| -> HashSet<ObjectId> {
        heads
            .iter()
            .flat_map(|h| {
                store
                    .list_commits_dag(h, usize::MAX)
                    .unwrap()
                    .commits
                    .into_iter()
                    .map(|c| c.id)
            })
            .collect()
    };
    for (heads, haves) in [
        (vec![merged], vec![]),
        (vec![merged], vec![main_before]),
        (vec![merged], vec![feature_tip]),
        (vec![feature], vec![merged]),
        (vec![merged, feature], vec![start]),
        (vec![main_before], vec![merged]),
    ] {
        let m = missing_set(store.as_ref(), &heads, &haves, &[]).unwrap();
        let has = dag(&haves);
        let want: HashSet<ObjectId> = dag(&heads)
            .into_iter()
            .filter(|c| !has.contains(c))
            .collect();
        let got: HashSet<ObjectId> = m.commits.iter().copied().collect();
        assert_eq!(got, want, "{heads:?} over {haves:?}");
        assert_eq!(m.commits.len(), want.len(), "each commit once");
        assert!(m.shallow.is_empty());
        let pos: HashMap<ObjectId, usize> =
            m.commits.iter().enumerate().map(|(i, c)| (*c, i)).collect();
        for c in &m.commits {
            for p in store.get_commit(c).unwrap().unwrap().parents {
                assert!(pos.get(&p).is_none_or(|pp| *pp < pos[c]), "parents first");
            }
        }

        let remote = MemoryStorage::new();
        for id in &has {
            let c = store.get_commit(id).unwrap().unwrap();
            copy_tree(store.as_ref(), &remote, c.state_root);
            remote.put_commit(&c).unwrap();
        }
        for id in &m.objects {
            remote
                .put_object(&store.get_object(id).unwrap().unwrap())
                .unwrap();
        }
        for id in &m.commits {
            remote
                .put_commit(&store.get_commit(id).unwrap().unwrap())
                .unwrap();
        }
        for id in dag(&heads) {
            let c = remote
                .get_commit(&id)
                .unwrap()
                .unwrap_or_else(|| panic!("the remote lacks commit {id}"));
            assert_whole_tree(&remote, c.state_root);
        }
    }
}

fn object_children(o: &Object) -> Vec<agentstategraph_core::ObjectId> {
    use agentstategraph_core::Node;
    match o {
        Object::Atom(_) => vec![],
        Object::Node(Node::Map(m)) => m.values().copied().collect(),
        Object::Node(Node::List(v) | Node::Set(v)) => v.clone(),
    }
}

fn copy_tree(from: &dyn Storage, to: &MemoryStorage, root: agentstategraph_core::ObjectId) {
    use agentstategraph_storage::ObjectStore;
    let mut todo = vec![root];
    while let Some(id) = todo.pop() {
        let o = from.get_object(&id).unwrap().unwrap();
        todo.extend(object_children(&o));
        to.put_object(&o).unwrap();
    }
}

fn assert_whole_tree(store: &MemoryStorage, root: agentstategraph_core::ObjectId) {
    use agentstategraph_storage::ObjectStore;
    let mut todo = vec![root];
    while let Some(id) = todo.pop() {
        let o = store
            .get_object(&id)
            .unwrap()
            .unwrap_or_else(|| panic!("the remote lacks object {id}"));
        todo.extend(object_children(&o));
    }
}

// Control ref (DESIGN.md Plan G t-001) -------------------------------------

mod control_support {
    use super::*;
    use agentstategraph::control::{self, CONTROL_REF, ControlState};
    use agentstategraph_core::ObjectId;

    pub fn head(store: &dyn Storage) -> Option<ObjectId> {
        store
            .get_ref(&Namespace::default_ns(), CONTROL_REF)
            .unwrap()
    }

    pub fn state(store: &dyn Storage) -> ControlState {
        let head = head(store).expect("a control ref");
        let root = store.get_commit(&head).unwrap().unwrap().state_root;
        control::read(store, &root).unwrap()
    }

    pub fn taint_params(name: &str) -> agentstategraph_taint::TaintParams {
        agentstategraph_taint::TaintParams {
            name: name.into(),
            effect: agentstategraph_taint::TaintEffect::Block,
            reason: "holds pii".into(),
            severity: agentstategraph_taint::TaintSeverity::High,
            expires_at: None,
            propagate: true,
            metadata: agentstategraph_taint::TaintMetadata::new(),
            agent_id: "agent/conformance".into(),
        }
    }

    pub fn reminder(title: &str) -> agentstategraph_reminders::CreateReminder {
        agentstategraph_reminders::CreateReminder::new(
            title,
            "look",
            chrono::Utc::now(),
            "agent/conformance",
        )
    }

    /// Copy `name`'s whole history from `from` to `to`, and point `name` at
    /// it there, as a fetch would.
    pub fn copy_ref(from: &dyn Storage, to: &dyn Storage, name: &str) {
        let ns = Namespace::default_ns();
        let head = from.get_ref(&ns, name).unwrap().unwrap();
        let set = agentstategraph::sync::missing_set(from, &[head], &[], &[]).unwrap();
        for id in &set.objects {
            to.put_object(&from.get_object(id).unwrap().unwrap())
                .unwrap();
        }
        for id in &set.commits {
            to.put_commit(&from.get_commit(id).unwrap().unwrap())
                .unwrap();
        }
        to.set_ref(&ns, name, head).unwrap();
    }
}

/// Every taint, reminder and epoch change lands on the control ref, the ref
/// is guarded from ordinary writes, and a repository opened on tables that
/// match it changes nothing.
fn control_ref_follows_every_change(f: &Fixture) {
    use agentstategraph::control::ReminderEntry;
    use agentstategraph_core::EpochStatus;
    use agentstategraph_reminders::{ReminderManager, ReminderStatus, ReminderStore};
    use control_support::*;

    let repo = Arc::new(f.open());
    repo.init().unwrap();
    let taint_id = repo.taint("main", "/users", taint_params("pii")).unwrap();
    repo.untaint(
        "main",
        "/users",
        "pii",
        agentstategraph_taint::UntaintParams {
            reason: "scrubbed".into(),
            proof: None,
            agent_id: "person/reviewer".into(),
        },
    )
    .unwrap();
    let mgr = ReminderManager::new(repo.clone());
    let kept = mgr.create(reminder("kept")).unwrap();
    mgr.cancel(&kept.id).unwrap();
    let gone = mgr.create(reminder("gone")).unwrap();
    assert!(ReminderStore::delete(repo.as_ref(), &gone.id).unwrap());
    repo.create_epoch("e1", "work", vec![]).unwrap();
    repo.set_active_epoch(Some("e1".into())).unwrap();
    let member = repo
        .set_json("main", "/work", &json!(1), opts("in the epoch"))
        .unwrap();
    repo.set_active_epoch(None).unwrap();
    repo.seal_epoch("e1", "done").unwrap();
    repo.archive_epoch("e1").unwrap();

    let store = f.storage();
    let state = state(store.as_ref());
    let taint = &state.taints[&taint_id];
    assert_eq!(taint.resolved_by.as_deref(), Some("person/reviewer"));
    assert!(!taint.commit_id.is_empty(), "the intent commit is recorded");
    match &state.reminders[&kept.id] {
        ReminderEntry::Live { reminder, .. } => {
            assert_eq!(reminder.status, ReminderStatus::Cancelled)
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        state.reminders[&gone.id],
        ReminderEntry::Deleted { .. }
    ));
    let table_epoch = repo.get_epoch("e1").unwrap();
    let epoch = &state.epochs["e1"];
    assert_eq!(epoch.status, EpochStatus::Archived);
    assert_eq!(epoch.seal_hash, table_epoch.seal_hash);
    let mut sealed = table_epoch.sealed_commits.clone();
    sealed.sort();
    assert_eq!(epoch.sealed_commits, sealed);
    assert!(state.members["e1"].contains(&member));

    // Guarded, and not a branch.
    for result in [
        repo.set_json("_control", "/x", &json!(1), opts("no"))
            .map(|_| ()),
        repo.branch("_control", "main").map(|_| ()),
        repo.branch("_control-anything", "main").map(|_| ()),
        repo.delete_branch("_control").map(|_| ()),
    ] {
        assert!(
            matches!(result, Err(RepoError::ReservedRef(_))),
            "{result:?}"
        );
    }
    assert!(
        repo.list_branches(None)
            .unwrap()
            .iter()
            .all(|(name, _)| !name.starts_with("_control"))
    );

    // Opening again on matching tables changes nothing.
    let head = head(store.as_ref());
    let again = f.open();
    assert_eq!(again.reconcile_control().unwrap(), Default::default());
    assert_eq!(control_support::head(store.as_ref()), head);
}

/// Rows written before the control ref existed move into it when a
/// repository opens, once.
fn existing_rows_move_into_the_control_ref(f: &Fixture) {
    use agentstategraph_core::{Epoch, ObjectId};
    use control_support::*;

    let store = f.storage();
    let taint = agentstategraph_taint::Taint {
        id: "old-taint".into(),
        path: "/a".into(),
        name: "old".into(),
        kind: agentstategraph_taint::TaintKind::Taint,
        effect: agentstategraph_taint::TaintEffect::Warn,
        severity: agentstategraph_taint::TaintSeverity::Low,
        reason: "from before".into(),
        agent_id: "agent/old".into(),
        commit_id: String::new(),
        created_at: chrono::Utc::now(),
        expires_at: None,
        resolved_at: None,
        resolved_by: None,
        resolved_reason: None,
        resolved_proof: None,
        propagate: false,
        metadata: agentstategraph_taint::TaintMetadata::new(),
    };
    store.create_taint(&taint).unwrap();
    let r = reminder("old").into_reminder();
    store.save(&r).unwrap();
    store
        .create_epoch(&Epoch::new("old-epoch", "before", vec![]).with_namespace("default"))
        .unwrap();
    let seal = ObjectId::hash(b"seal");
    store
        .seal_epoch("old-epoch", "sealed before", chrono::Utc::now(), &[], &seal)
        .unwrap();
    assert!(head(store.as_ref()).is_none());

    let repo = f.open();
    let state = state(store.as_ref());
    assert_eq!(state.taints["old-taint"], taint);
    assert!(state.reminders[&r.id].live().is_some());
    assert_eq!(state.epochs["old-epoch"].seal_hash, Some(seal));
    let moved = head(store.as_ref());
    assert_eq!(repo.reconcile_control().unwrap(), Default::default());
    drop(f.open());
    assert_eq!(head(store.as_ref()), moved, "once");
}

/// A control ref brought from another store (as by a fetch) rebuilds this
/// store's tables when a repository opens: a resolved taint, a cancelled and
/// a deleted reminder, an archived epoch with its seal and members.
fn tables_rebuild_from_a_fetched_control_ref(f: &Fixture) {
    use agentstategraph_core::EpochStatus;
    use agentstategraph_reminders::{ReminderManager, ReminderStatus, ReminderStore};
    use control_support::*;

    let source_store: Arc<dyn Storage + Send + Sync> = Arc::new(MemoryStorage::new());
    let source = Arc::new(Repository::from_shared(source_store.clone()));
    source.init().unwrap();
    let taint_id = source.taint("main", "/users", taint_params("pii")).unwrap();
    source
        .untaint(
            "main",
            "/users",
            "pii",
            agentstategraph_taint::UntaintParams {
                reason: "scrubbed".into(),
                proof: None,
                agent_id: "person/reviewer".into(),
            },
        )
        .unwrap();
    let mgr = ReminderManager::new(source.clone());
    let kept = mgr.create(reminder("kept")).unwrap();
    mgr.cancel(&kept.id).unwrap();
    let gone = mgr.create(reminder("gone")).unwrap();
    ReminderStore::delete(source.as_ref(), &gone.id).unwrap();
    source.create_epoch("e1", "work", vec![]).unwrap();
    source.set_active_epoch(Some("e1".into())).unwrap();
    let member = source
        .set_json("main", "/work", &json!(1), opts("in the epoch"))
        .unwrap();
    source.set_active_epoch(None).unwrap();
    source.seal_epoch("e1", "done").unwrap();
    source.archive_epoch("e1").unwrap();
    let source_epoch = source.get_epoch("e1").unwrap();

    // This store has the deleted reminder, as if it predated the delete.
    let store = f.storage();
    store
        .save(&source_store.get(&gone.id).unwrap().unwrap_or_else(|| {
            let mut r = reminder("gone").into_reminder();
            r.id = gone.id.clone();
            r
        }))
        .unwrap();
    copy_ref(source_store.as_ref(), store.as_ref(), "main");
    copy_ref(source_store.as_ref(), store.as_ref(), "_control");

    let repo = f.open();
    let taint = repo
        .get_taint(&taint_id)
        .unwrap()
        .expect("the taint is rebuilt");
    assert_eq!(taint.resolved_by.as_deref(), Some("person/reviewer"));
    assert_eq!(
        store.get(&kept.id).unwrap().unwrap().status,
        ReminderStatus::Cancelled
    );
    assert!(
        store.get(&gone.id).unwrap().is_none(),
        "the delete is applied"
    );
    let epoch = repo.get_epoch("e1").unwrap();
    assert_eq!(epoch.status, EpochStatus::Archived);
    assert_eq!(epoch.seal_hash, source_epoch.seal_hash);
    assert_eq!(epoch.commits, vec![member]);
    let (mut a, mut b) = (
        epoch.sealed_commits.clone(),
        source_epoch.sealed_commits.clone(),
    );
    a.sort();
    b.sort();
    assert_eq!(a, b);
    assert_eq!(repo.reconcile_control().unwrap(), Default::default());
}

/// A write cut short after its table write, before its control commit, is
/// taken from the table when a repository next opens.
fn a_write_cut_short_is_taken_from_the_table(f: &Fixture) {
    use agentstategraph_core::ObjectId;
    use agentstategraph_reminders::{ReminderManager, ReminderStatus};
    use control_support::*;

    let repo = Arc::new(f.open());
    let r = ReminderManager::new(repo.clone())
        .create(reminder("r"))
        .unwrap();
    let store = f.storage();
    let mut changed = store.get(&r.id).unwrap().unwrap();
    changed.status = ReminderStatus::Cancelled;
    store.update(&changed).unwrap();
    let mark = format!("_control-pending/reminder/{}/cut-short", r.id);
    store
        .set_ref(&Namespace::default_ns(), &mark, ObjectId::hash(b"x"))
        .unwrap();

    drop(f.open());
    let state = state(store.as_ref());
    assert_eq!(
        state.reminders[&r.id].live().unwrap().status,
        ReminderStatus::Cancelled
    );
    assert!(
        store
            .get_ref(&Namespace::default_ns(), &mark)
            .unwrap()
            .is_none(),
        "the mark is cleared"
    );
}

/// A store exported from memory imports into this backend, taints and
/// epochs included, and this backend's export imports back into memory as
/// the same store (DESIGN.md Plan F t-008).
fn store_export_import_round_trips(f: &Fixture) {
    use agentstategraph::sync::bundle::BundleLimits;
    use control_support::*;

    let source = Repository::from_shared(Arc::new(MemoryStorage::new()));
    source.init().unwrap();
    source
        .set_json(
            "main",
            "/config",
            &json!({"name": "prod", "list": [1, 2.5]}),
            opts("init"),
        )
        .unwrap();
    source.branch("feature", "main").unwrap();
    source
        .set_json("feature", "/feature", &json!(true), opts("feature"))
        .unwrap();
    let taint = source
        .taint("main", "/config", taint_params("pii"))
        .unwrap();
    source.create_epoch("e", "work", vec![]).unwrap();
    source.seal_epoch("e", "done").unwrap();
    let (bytes, exported) = source.export_store(Vec::new()).unwrap();

    let here = f.open();
    let imported = here
        .import_store(&bytes[..], BundleLimits::default())
        .unwrap();
    assert_eq!(imported.commits, exported.commits);
    assert_eq!(here.get_json("feature", "/feature").unwrap(), json!(true));
    assert_eq!(
        here.get_json("main", "/config").unwrap(),
        source.get_json("main", "/config").unwrap()
    );
    assert!(
        here.get_taint(&taint).unwrap().is_some(),
        "the taint table is rebuilt"
    );
    assert_eq!(
        here.get_epoch("e").unwrap().seal_hash,
        source.get_epoch("e").unwrap().seal_hash
    );

    let (again, _) = f.open().export_store(Vec::new()).unwrap();
    let back = Repository::from_shared(Arc::new(MemoryStorage::new()));
    back.import_store(&again[..], BundleLimits::default())
        .unwrap();
    assert_eq!(
        back.list_branches(None).unwrap(),
        source.list_branches(None).unwrap()
    );
    assert!(back.get_taint(&taint).unwrap().is_some());
}

// Sync (DESIGN.md Plan G t-004) ---------------------------------------------

mod sync_support {
    use super::*;
    use agentstategraph::sync::InProcessRemote;

    pub fn memory() -> Repository {
        Repository::from_shared(Arc::new(MemoryStorage::new()))
    }

    pub fn hub(repo: &Repository) -> InProcessRemote<'_> {
        InProcessRemote::new(repo)
    }

    pub fn merge_opts() -> CommitOptions {
        CommitOptions::new("agent/conformance", IntentCategory::Merge, "pull")
    }
}

/// This backend as a hub: two memory stores push to it and pull from it,
/// a diverged push is rejected and resolved by fetch, merge and push, and a
/// taint made on one store holds on the other.
fn a_hub_on_this_backend(f: &Fixture) {
    use agentstategraph::sync::{PullOutcome, SyncError};
    use control_support::taint_params;
    use sync_support::*;

    let central = f.open();
    let (a, b) = (memory(), memory());
    a.init().unwrap();
    a.set_json("main", "/x", &json!({"n": 1}), opts("x"))
        .unwrap();
    let taint = a.taint("main", "/x", taint_params("pii")).unwrap();
    a.push("hub", &hub(&central), "main").unwrap();
    assert_eq!(central.get_json("main", "/x/n").unwrap(), json!(1));
    assert!(central.get_taint(&taint).unwrap().is_some());

    let pulled = b.pull("hub", &hub(&central), "main", merge_opts()).unwrap();
    assert!(
        matches!(pulled.outcome, PullOutcome::Created(_)),
        "{pulled:?}"
    );
    assert!(b.get_taint(&taint).unwrap().is_some());

    a.set_json("main", "/a", &json!("a"), opts("a")).unwrap();
    a.push("hub", &hub(&central), "main").unwrap();
    b.set_json("main", "/b", &json!("b"), opts("b")).unwrap();
    assert!(matches!(
        b.push("hub", &hub(&central), "main"),
        Err(SyncError::Rejected { .. })
    ));
    b.pull("hub", &hub(&central), "main", merge_opts()).unwrap();
    b.push("hub", &hub(&central), "main").unwrap();
    assert_eq!(central.get_json("main", "/a").unwrap(), json!("a"));
    assert_eq!(central.get_json("main", "/b").unwrap(), json!("b"));
    assert_eq!(central.head("main").unwrap(), b.head("main").unwrap());

    // The hub's own local-only ref isn't offered.
    central.branch("index", "main").unwrap();
    central.set_local_only("index", true).unwrap();
    assert!(matches!(
        a.fetch("hub", &hub(&central), &["index"]),
        Err(SyncError::NoSuchRef(_))
    ));
}

/// This backend as a client: it pulls from a memory hub, has its tables
/// rebuilt from the fetched control ref, and pushes back.
fn a_client_on_this_backend(f: &Fixture) {
    use agentstategraph::sync::PullOutcome;
    use control_support::taint_params;
    use sync_support::*;

    let central = memory();
    let other = memory();
    other.init().unwrap();
    other
        .set_json("main", "/config", &json!({"name": "prod"}), opts("init"))
        .unwrap();
    let taint = other.taint("main", "/config", taint_params("pii")).unwrap();
    other.create_epoch("e", "work", vec![]).unwrap();
    other.seal_epoch("e", "done").unwrap();
    other.push("hub", &hub(&central), "main").unwrap();

    // It ran init, as an embedder's store does: its empty first commit
    // gives way to the hub's history.
    let here = f.open();
    here.init().unwrap();
    let pulled = here
        .pull("hub", &hub(&central), "main", merge_opts())
        .unwrap();
    assert!(
        matches!(pulled.outcome, PullOutcome::Created(_)),
        "{pulled:?}"
    );
    assert_eq!(
        here.get_json("main", "/config/name").unwrap(),
        json!("prod")
    );
    assert!(here.get_taint(&taint).unwrap().is_some());
    assert_eq!(
        here.get_epoch("e").unwrap().seal_hash,
        other.get_epoch("e").unwrap().seal_hash
    );
    assert_eq!(
        here.list_branches(None).unwrap(),
        vec![("main".to_string(), central.head("main").unwrap())],
        "the tracking ref isn't a branch"
    );

    here.set_json("main", "/here", &json!(true), opts("here"))
        .unwrap();
    here.push("hub", &hub(&central), "main").unwrap();
    assert_eq!(central.get_json("main", "/here").unwrap(), json!(true));

    // A reopened repository sees the same tracking refs.
    let reopened = f.open();
    assert_eq!(
        reopened.remote_refs("hub").unwrap(),
        vec![("main".to_string(), here.head("main").unwrap())]
    );
}

/// This backend as a hub that takes a shallow push: its history starts at
/// the boundary, which it records, walks and blame stop there, a sweep runs,
/// and a store that fetches from it gets the same boundary.
fn a_shallow_history_on_this_backend(f: &Fixture) {
    use agentstategraph::sync::PushOptions;
    use sync_support::*;

    let source = memory();
    source.init().unwrap();
    source
        .set_json("main", "/keep", &json!("old"), opts("keep"))
        .unwrap();
    let ids: Vec<_> = (0..4)
        .map(|i| source.set_json("main", "/v", &json!(i), opts("v")).unwrap())
        .collect();
    let central = f.open();
    let options = PushOptions::default().shallow_from(ids[1]);
    source
        .push_with("hub", &hub(&central), "main", &options)
        .unwrap();

    assert_eq!(central.shallow_commits().unwrap(), vec![ids[1]]);
    assert_eq!(central.get_json("main", "/keep").unwrap(), json!("old"));
    assert_eq!(central.log("main", 100).unwrap().len(), 3);
    assert_eq!(central.log_dag("main", 100).unwrap().commits.len(), 3);
    assert_eq!(
        central.blame("main", "/keep").unwrap().commit_id,
        ids[1].short()
    );
    central.gc_sweep(keep_only_tips(), true, false).unwrap();
    assert_eq!(central.get_json("main", "/v").unwrap(), json!(3));

    let other = memory();
    other.fetch("hub", &hub(&central), &[]).unwrap();
    assert_eq!(other.shallow_commits().unwrap(), vec![ids[1]]);
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

#[test]
fn sync_missing_set() {
    run("missing_set_matches_the_dag", missing_set_matches_the_dag);
}

#[test]
fn control_ref_writes() {
    run(
        "control_ref_follows_every_change",
        control_ref_follows_every_change,
    );
}

#[test]
fn control_ref_migration() {
    run(
        "existing_rows_move_into_the_control_ref",
        existing_rows_move_into_the_control_ref,
    );
}

#[test]
fn control_ref_rebuild() {
    run(
        "tables_rebuild_from_a_fetched_control_ref",
        tables_rebuild_from_a_fetched_control_ref,
    );
}

#[test]
fn control_ref_cut_short() {
    run(
        "a_write_cut_short_is_taken_from_the_table",
        a_write_cut_short_is_taken_from_the_table,
    );
}

#[test]
fn store_export_import() {
    run(
        "store_export_import_round_trips",
        store_export_import_round_trips,
    );
}

#[test]
fn sync_hub() {
    run("a_hub_on_this_backend", a_hub_on_this_backend);
}

#[test]
fn sync_client() {
    run("a_client_on_this_backend", a_client_on_this_backend);
}

#[test]
fn sync_shallow() {
    run(
        "a_shallow_history_on_this_backend",
        a_shallow_history_on_this_backend,
    );
}
