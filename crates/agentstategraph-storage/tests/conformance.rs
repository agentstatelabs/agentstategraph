//! Backend conformance suite (DESIGN.md Plan D t-001).
//!
//! One set of behaviour checks runs against every built-in backend, so the
//! backends are held to one contract by tests rather than by reading code.
//! Memory and SQLite always run. Postgres runs when `TEST_DATABASE_URL` is
//! set (CI provides one; `scripts/test-postgres.sh` starts one locally), and
//! every test gets its own tenant so tests can share one database.
//!
//! A case a backend is known to fail is listed in [`KNOWN_FAILURES`] with the
//! Plan D task that owns the fix. A deterministic known failure must keep
//! failing: once it passes, the test fails until the entry is removed, so a
//! fix can't land without the suite noticing.

use std::collections::HashSet;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Barrier};
#[cfg(feature = "postgres")]
use std::sync::{Mutex, OnceLock};
use std::thread::JoinHandle;

use agentstategraph_core::{
    Authority, Commit, CommitBuilder, Epoch, EpochScope, EpochStatus, Intent, IntentCategory,
    Namespace, Object, ObjectId, Session, SessionStatus,
};
use agentstategraph_reminders::CreateReminder;
use agentstategraph_storage::{Storage, StorageError};
use agentstategraph_taint::{Taint, TaintEffect, TaintKind, TaintMetadata, TaintSeverity};
use chrono::Utc;

type Store = Arc<dyn Storage + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Backend {
    Memory,
    Sqlite,
    Postgres,
}

/// A case a backend fails today, and who owns the fix.
struct Known {
    case: &'static str,
    backend: Backend,
    owner: &'static str,
    /// Fails only sometimes (a race). Never fails the suite either way.
    flaky: bool,
}

const KNOWN_FAILURES: &[Known] = &[
    Known {
        case: "reminders_round_trip_and_cas",
        backend: Backend::Memory,
        owner: "Plan D t-005: MemoryStorage doesn't override update_if_unchanged, so it \
                uses the trait's non-atomic default",
        flaky: true,
    },
    Known {
        case: "reminders_round_trip_and_cas",
        backend: Backend::Postgres,
        owner: "Plan D t-005: PostgresStorage uses the trait's non-atomic \
                update_if_unchanged",
        flaky: true,
    },
    Known {
        case: "concurrent_first_connects",
        backend: Backend::Postgres,
        owner: "Plan D t-006: schema DDL runs unlocked on every connect, and concurrent \
                connects can deadlock",
        flaky: true,
    },
];

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// One test's store, plus a way to open more handles onto it. Separate
/// handles stand in for separate processes sharing one store.
struct Fixture {
    backend: Backend,
    store: Store,
    opener: Arc<dyn Fn() -> Store + Send + Sync>,
    /// `PostgresStorage` calls `Handle::current()`, so its callers must be
    /// inside the runtime's context.
    runtime: Option<tokio::runtime::Handle>,
    cleanup: Option<std::path::PathBuf>,
}

impl Fixture {
    fn store(&self) -> &Store {
        &self.store
    }

    fn open_another(&self) -> Store {
        (self.opener)()
    }

    /// Run `f` on a new thread with the runtime context the backend needs.
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

#[cfg(feature = "memory")]
fn memory_fixture() -> Fixture {
    let store: Store = Arc::new(agentstategraph_storage::MemoryStorage::new());
    let shared = Arc::clone(&store);
    Fixture {
        backend: Backend::Memory,
        store,
        // One process: every "handle" is the same instance.
        opener: Arc::new(move || Arc::clone(&shared)),
        runtime: None,
        cleanup: None,
    }
}

#[cfg(feature = "sqlite")]
fn sqlite_fixture(case: &str) -> Fixture {
    use agentstategraph_storage::SqliteStorage;
    let path = std::env::temp_dir().join(format!(
        "asg-conformance-{case}-{}.db",
        uuid::Uuid::new_v4().simple()
    ));
    let store: Store = Arc::new(SqliteStorage::open(&path).expect("open sqlite"));
    let opened = path.clone();
    Fixture {
        backend: Backend::Sqlite,
        store,
        opener: Arc::new(move || Arc::new(SqliteStorage::open(&opened).expect("reopen sqlite"))),
        runtime: None,
        cleanup: Some(path),
    }
}

#[cfg(feature = "postgres")]
fn postgres_runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("build tokio runtime")
    })
}

/// Held around every connect, because concurrent connects race on schema DDL
/// (see `concurrent_first_connects`) and the suite shouldn't depend on that.
#[cfg(feature = "postgres")]
fn connect_lock() -> std::sync::MutexGuard<'static, ()> {
    static CONNECT: Mutex<()> = Mutex::new(());
    CONNECT.lock().unwrap_or_else(|p| p.into_inner())
}

#[cfg(feature = "postgres")]
fn postgres_connect(url: &str, tenant: &str) -> Store {
    let _one_at_a_time = connect_lock();
    let storage = postgres_runtime()
        .block_on(agentstategraph_storage::PostgresStorage::connect_tenant(
            url, tenant,
        ))
        .expect("connect to TEST_DATABASE_URL");
    Arc::new(storage)
}

#[cfg(feature = "postgres")]
fn postgres_url() -> Option<String> {
    std::env::var("TEST_DATABASE_URL")
        .ok()
        .filter(|u| !u.is_empty())
}

#[cfg(feature = "postgres")]
fn postgres_fixture(case: &str) -> Option<Fixture> {
    let url = postgres_url()?;
    let tenant = format!("conformance-{case}-{}", uuid::Uuid::new_v4().simple());
    let store = postgres_connect(&url, &tenant);
    Some(Fixture {
        backend: Backend::Postgres,
        store,
        opener: Arc::new(move || postgres_connect(&url, &tenant)),
        runtime: Some(postgres_runtime().handle().clone()),
        cleanup: None,
    })
}

fn fixtures(case: &str) -> Vec<Fixture> {
    let mut all = Vec::new();
    #[cfg(feature = "memory")]
    all.push(memory_fixture());
    #[cfg(feature = "sqlite")]
    all.push(sqlite_fixture(case));
    #[cfg(feature = "postgres")]
    match postgres_fixture(case) {
        Some(f) => all.push(f),
        None => skip_postgres(case),
    }
    #[cfg(not(feature = "postgres"))]
    skip_postgres(case);
    all
}

/// CI sets `ASG_REQUIRE_POSTGRES` so a missing database fails the run
/// instead of silently skipping the Postgres legs.
fn skip_postgres(case: &str) {
    let required = std::env::var("ASG_REQUIRE_POSTGRES").is_ok_and(|v| !v.is_empty() && v != "0");
    assert!(
        !required,
        "ASG_REQUIRE_POSTGRES is set, but Postgres isn't available for {case} \
         (TEST_DATABASE_URL unset, or the `postgres` feature is off)"
    );
    eprintln!("skip postgres for {case}: TEST_DATABASE_URL not set");
}

/// Run `body` against every available backend and judge each outcome
/// against [`KNOWN_FAILURES`]. Every backend runs even if an earlier one
/// fails, so one report shows them all.
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
            (true, Some(k)) if k.flaky => {
                eprintln!(
                    "known-flaky case passed this time, {backend:?} {case}: {}",
                    k.owner
                )
            }
            (true, Some(k)) => problems.push(format!(
                "{backend:?}: now passes, but is listed in KNOWN_FAILURES ({}). Remove the entry.",
                k.owner
            )),
        }
    }
    assert!(problems.is_empty(), "{case}: {}", problems.join("; "));
}

// ---------------------------------------------------------------------------
// Builders
// ---------------------------------------------------------------------------

fn commit(store: &Store, tag: &str, parents: &[ObjectId]) -> Commit {
    let root = store.put_object(&Object::string(tag)).unwrap();
    let c = CommitBuilder::new(
        root,
        "agent/conformance",
        Authority::simple("conformance"),
        Intent::new(IntentCategory::Checkpoint, tag),
    )
    .parents(parents.to_vec())
    .build();
    store.put_commit(&c).unwrap();
    c
}

fn epoch(id: &str) -> Epoch {
    Epoch::new(id, format!("epoch {id}"), vec!["intent-1".into()])
}

fn session(id: &str) -> Session {
    Session {
        id: id.to_string(),
        agent_id: "agent/conformance".to_string(),
        working_branch: "agents/conformance".to_string(),
        head: ObjectId::hash(id.as_bytes()),
        parent_session: None,
        delegated_intent: Some("intent-1".to_string()),
        report_to: None,
        path_scope: Some("/scope".to_string()),
        scope_tenant: None,
        scope_namespace: None,
        status: SessionStatus::Active,
        created_at: Utc::now(),
        ended_at: None,
    }
}

fn taint(path: &str, name: &str) -> Taint {
    Taint {
        id: uuid::Uuid::new_v4().to_string(),
        path: path.into(),
        name: name.into(),
        kind: TaintKind::Taint,
        effect: TaintEffect::Block,
        severity: TaintSeverity::Medium,
        reason: "conformance".into(),
        agent_id: "agent/conformance".into(),
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

fn zero_hash() -> ObjectId {
    ObjectId::from_bytes([0u8; 32])
}

// ---------------------------------------------------------------------------
// Objects and commits
// ---------------------------------------------------------------------------

fn objects_round_trip(f: &Fixture) {
    let s = f.store();
    let a = Object::string("alpha");
    let id = s.put_object(&a).unwrap();
    assert_eq!(s.put_object(&a).unwrap(), id, "same content, same id");
    assert!(s.has_object(&id).unwrap());
    assert_eq!(s.get_object(&id).unwrap(), Some(a.clone()));

    let missing = ObjectId::hash(b"never stored");
    assert!(!s.has_object(&missing).unwrap());
    assert_eq!(s.get_object(&missing).unwrap(), None);

    let batch = vec![Object::string("b1"), Object::string("b2"), a.clone()];
    let ids = s.batch_put_objects(&batch).unwrap();
    assert_eq!(ids.len(), 3);
    assert_eq!(ids[2], id);
    let got = s.batch_get_objects(&[ids[0], missing, ids[1]]).unwrap();
    assert_eq!(
        got,
        vec![Some(batch[0].clone()), None, Some(batch[1].clone())]
    );

    let mut children = std::collections::BTreeMap::new();
    children.insert("a".to_string(), id);
    let node = Object::map(children);
    let node_id = s.put_object(&node).unwrap();
    assert_eq!(s.get_object(&node_id).unwrap(), Some(node));
}

fn commits_and_walks(f: &Fixture) {
    let s = f.store();
    let c1 = commit(s, "c1", &[]);
    let c2 = commit(s, "c2", &[c1.id]);
    let b1 = commit(s, "b1", &[c1.id]);
    let c3 = commit(s, "c3", &[c2.id]);
    let m = commit(s, "m", &[c3.id, b1.id]);

    assert!(s.has_commit(&m.id).unwrap());
    assert!(!s.has_commit(&ObjectId::hash(b"nope")).unwrap());
    assert!(s.get_commit(&ObjectId::hash(b"nope")).unwrap().is_none());
    let back = s
        .get_commit(&m.id)
        .unwrap()
        .expect("merge commit round-trips");
    assert_eq!(back.id, m.id);
    assert_eq!(
        back.parents,
        vec![c3.id, b1.id],
        "parent order is preserved"
    );
    assert_eq!(back.state_root, m.state_root);
    assert_eq!(back.agent_id, m.agent_id);

    let first_parent: Vec<ObjectId> = s
        .list_commits(&m.id, 10)
        .unwrap()
        .iter()
        .map(|c| c.id)
        .collect();
    assert_eq!(first_parent, vec![m.id, c3.id, c2.id, c1.id]);
    assert_eq!(
        s.list_commits(&m.id, 2).unwrap().len(),
        2,
        "limit is honoured"
    );

    let walk = s.list_commits_dag(&m.id, 10).unwrap();
    let reached: HashSet<ObjectId> = walk.commits.iter().map(|c| c.id).collect();
    let expected: HashSet<ObjectId> = [m.id, c3.id, c2.id, b1.id, c1.id].into();
    assert_eq!(reached, expected, "the DAG walk reaches second parents");
    assert_eq!(walk.commits.len(), 5, "each commit appears once");
    assert!(!walk.truncated);
    let short = s.list_commits_dag(&m.id, 2).unwrap();
    assert_eq!(short.commits.len(), 2);
    assert!(
        short.truncated,
        "a limit that stops the walk reports truncation"
    );

    let all: HashSet<ObjectId> = s.all_commit_ids().unwrap().into_iter().collect();
    assert!(
        expected.is_subset(&all),
        "all_commit_ids lists every commit"
    );
}

// ---------------------------------------------------------------------------
// Refs and namespaces
// ---------------------------------------------------------------------------

fn refs_and_namespaces(f: &Fixture) {
    let s = f.store();
    let def = Namespace::default_ns();
    let other = Namespace::new("conformance-other").unwrap();
    let (x, y) = (ObjectId::hash(b"x"), ObjectId::hash(b"y"));

    assert_eq!(s.get_ref(&def, "main").unwrap(), None);
    s.set_ref(&def, "main", x).unwrap();
    s.set_ref(&def, "feature/a", y).unwrap();
    s.set_ref(&def, "feature/b", x).unwrap();
    assert_eq!(s.get_ref(&def, "main").unwrap(), Some(x));

    let mut features = s.list_refs(&def, "feature/").unwrap();
    features.sort();
    assert_eq!(
        features,
        vec![("feature/a".to_string(), y), ("feature/b".to_string(), x)]
    );

    assert!(
        !s.cas_ref(&def, "main", y, x).unwrap(),
        "stale expected value"
    );
    assert_eq!(s.get_ref(&def, "main").unwrap(), Some(x));
    assert!(s.cas_ref(&def, "main", x, y).unwrap());
    assert_eq!(s.get_ref(&def, "main").unwrap(), Some(y));
    assert!(
        !s.cas_ref(&def, "absent", x, y).unwrap(),
        "CAS on a missing ref"
    );
    assert_eq!(s.get_ref(&def, "absent").unwrap(), None);

    assert!(matches!(
        s.set_ref(&other, "main", x),
        Err(StorageError::NamespaceNotFound(_))
    ));
    s.create_namespace(&other).unwrap();
    assert!(matches!(
        s.create_namespace(&other),
        Err(StorageError::NamespaceAlreadyExists(_))
    ));
    assert!(s.list_namespaces().unwrap().contains(&other));
    s.set_ref(&other, "main", x).unwrap();
    assert_eq!(s.get_ref(&other, "main").unwrap(), Some(x));
    assert_eq!(
        s.get_ref(&def, "main").unwrap(),
        Some(y),
        "namespaces are independent"
    );

    assert!(s.delete_ref(&def, "feature/a").unwrap());
    assert!(!s.delete_ref(&def, "feature/a").unwrap());
    assert!(s.delete_namespace(&other).unwrap());
    assert!(!s.list_namespaces().unwrap().contains(&other));
    assert!(!s.delete_namespace(&other).unwrap());
    assert!(matches!(
        s.delete_namespace(&def),
        Err(StorageError::InvalidOperation(_))
    ));
}

fn create_ref_exactly_once(f: &Fixture) {
    const RACERS: usize = 8;
    let ns = Namespace::default_ns();
    let handles: Vec<Store> = (0..RACERS).map(|_| f.open_another()).collect();
    for round in 0..10 {
        let name = format!("r{round}");
        let barrier = Arc::new(Barrier::new(RACERS));
        let racers: Vec<_> = handles
            .iter()
            .enumerate()
            .map(|(i, store)| {
                let (store, barrier, ns, name) = (
                    Arc::clone(store),
                    Arc::clone(&barrier),
                    ns.clone(),
                    name.clone(),
                );
                f.spawn(move || {
                    barrier.wait();
                    let target = ObjectId::hash(format!("{i}").as_bytes());
                    store
                        .create_ref(&ns, &name, target)
                        .unwrap()
                        .then_some(target)
                })
            })
            .collect();
        let won: Vec<ObjectId> = racers
            .into_iter()
            .filter_map(|h| h.join().unwrap())
            .collect();
        assert_eq!(won.len(), 1, "round {round}: {} creators won", won.len());
        assert_eq!(f.store().get_ref(&ns, &name).unwrap(), Some(won[0]));
    }
}

fn cas_delete_only_what_was_seen(f: &Fixture) {
    let s = f.store();
    let ns = Namespace::default_ns();
    let (seen, moved) = (ObjectId::hash(b"seen"), ObjectId::hash(b"moved"));
    s.set_ref(&ns, "b", seen).unwrap();
    s.set_ref(&ns, "b", moved).unwrap();
    assert!(!s.cas_delete_ref(&ns, "b", seen).unwrap());
    assert_eq!(s.get_ref(&ns, "b").unwrap(), Some(moved));
    assert!(s.cas_delete_ref(&ns, "b", moved).unwrap());
    assert_eq!(s.get_ref(&ns, "b").unwrap(), None);
    assert!(!s.cas_delete_ref(&ns, "b", moved).unwrap());
    s.set_ref(&ns, "c", seen).unwrap();
    assert!(
        !s.create_ref(&ns, "c", moved).unwrap(),
        "create never replaces"
    );
    assert_eq!(s.get_ref(&ns, "c").unwrap(), Some(seen));
}

/// Separate handles (separate processes, for SQLite and Postgres) increment
/// one counter through `cas_ref`, retrying on conflict. Every acknowledged
/// increment must survive: the v1.2.5 lost-update shape, at the storage layer.
fn cas_counter_loses_nothing(f: &Fixture) {
    const WRITERS: usize = 4;
    const PER_WRITER: usize = 15;
    let ns = Namespace::default_ns();
    let counter = |store: &Store, n: u64| store.put_object(&Object::int(n as i64)).unwrap();
    let start = counter(f.store(), 0);
    f.store().set_ref(&ns, "counter", start).unwrap();

    let barrier = Arc::new(Barrier::new(WRITERS));
    let writers: Vec<_> = (0..WRITERS)
        .map(|_| {
            let (store, barrier, ns) = (f.open_another(), Arc::clone(&barrier), ns.clone());
            f.spawn(move || {
                barrier.wait();
                for _ in 0..PER_WRITER {
                    loop {
                        let head = store.get_ref(&ns, "counter").unwrap().unwrap();
                        let n = match store.get_object(&head).unwrap() {
                            Some(Object::Atom(agentstategraph_core::Atom::Int(n))) => n as u64,
                            other => panic!("counter object: {other:?}"),
                        };
                        let next = store.put_object(&Object::int(n as i64 + 1)).unwrap();
                        if store.cas_ref(&ns, "counter", head, next).unwrap() {
                            break;
                        }
                    }
                }
            })
        })
        .collect();
    for w in writers {
        w.join().unwrap();
    }
    let head = f.store().get_ref(&ns, "counter").unwrap().unwrap();
    assert_eq!(
        f.store().get_object(&head).unwrap(),
        Some(Object::int((WRITERS * PER_WRITER) as i64)),
        "acknowledged increments were lost"
    );
}

fn concurrent_first_connects(f: &Fixture) {
    if f.backend != Backend::Postgres {
        return;
    }
    // Race connects against each other on purpose; that's the race the
    // harness lock hides. Hold the lock meanwhile so other tests' connects
    // can't be caught in it.
    #[cfg(feature = "postgres")]
    {
        let _others_wait = connect_lock();
        let url = postgres_url().expect("postgres fixture implies a url");
        let barrier = Arc::new(Barrier::new(6));
        let connects: Vec<_> = (0..6)
            .map(|i| {
                let (url, barrier) = (url.clone(), Arc::clone(&barrier));
                std::thread::spawn(move || {
                    barrier.wait();
                    postgres_runtime().block_on(
                        agentstategraph_storage::PostgresStorage::connect_tenant(
                            &url,
                            &format!("conformance-connect-{i}-{}", uuid::Uuid::new_v4().simple()),
                        ),
                    )
                })
            })
            .collect();
        // Join every racer before judging any: failing on the first error
        // would release the connect lock while the others still run DDL,
        // and pull an unrelated test's connect into the race.
        let results: Vec<_> = connects.into_iter().map(|c| c.join().unwrap()).collect();
        for r in results {
            r.expect("concurrent connect");
        }
    }
}

// ---------------------------------------------------------------------------
// Epochs, sessions, taints, reminders
// ---------------------------------------------------------------------------

fn epochs_round_trip(f: &Fixture) {
    let s = f.store();
    s.create_epoch(&epoch("e1")).unwrap();
    let mut scoped = epoch("e2");
    scoped.namespace = Some("conformance".to_string());
    scoped.scope = EpochScope::Branch("release".to_string());
    s.create_epoch(&scoped).unwrap();
    assert_eq!(s.list_epochs().unwrap().len(), 2);

    let e1 = s.get_epoch("e1").unwrap().expect("e1");
    assert_eq!(e1.status, EpochStatus::Active);
    assert!(e1.sealed_at.is_none());
    let e2 = s.get_epoch("e2").unwrap().expect("e2");
    assert_eq!(
        e2.namespace.as_deref(),
        Some("conformance"),
        "namespace round-trips"
    );
    assert_eq!(
        e2.scope,
        EpochScope::Branch("release".to_string()),
        "scope round-trips"
    );

    let seal = ObjectId::hash(b"seal");
    s.seal_epoch("e1", "wrapped up", Utc::now(), &[], &seal)
        .unwrap();
    let e1 = s.get_epoch("e1").unwrap().unwrap();
    assert_eq!(e1.status, EpochStatus::Sealed);
    assert!(e1.sealed_at.is_some());
    assert_eq!(e1.seal_summary.as_deref(), Some("wrapped up"));
    assert_eq!(e1.seal_hash, Some(seal), "seal hash round-trips");

    assert!(s.assign_epoch_namespace("e1", "later").unwrap());
    assert_eq!(
        s.get_epoch("e1").unwrap().unwrap().namespace.as_deref(),
        Some("later")
    );
    assert!(
        !s.assign_epoch_namespace("e1", "again").unwrap(),
        "only an unbound epoch"
    );
    assert!(s.get_epoch("missing").unwrap().is_none());
}

fn epochs_commit_membership_and_seal(f: &Fixture) {
    let s = f.store();
    s.create_epoch(&epoch("locked")).unwrap();
    let a = commit(s, "a", &[]);
    let b = commit(s, "b", &[a.id]);
    s.set_commit_epoch(&a.id, "locked").unwrap();
    s.set_commit_epoch(&b.id, "locked").unwrap();
    let members: HashSet<ObjectId> = s
        .get_epoch("locked")
        .unwrap()
        .unwrap()
        .commits
        .into_iter()
        .collect();
    assert_eq!(members, [a.id, b.id].into(), "an epoch lists its commits");

    s.seal_epoch("locked", "done", Utc::now(), &[a.id, b.id], &zero_hash())
        .unwrap();
    let after = commit(s, "after", &[b.id]);
    assert!(matches!(
        s.set_commit_epoch(&after.id, "locked"),
        Err(StorageError::EpochAlreadySealed { ref id }) if id == "locked"
    ));
    assert!(matches!(
        s.seal_epoch("locked", "again", Utc::now(), &[], &zero_hash()),
        Err(StorageError::EpochAlreadySealed { .. })
    ));
    let sealed = s.get_epoch("locked").unwrap().unwrap();
    assert_eq!(sealed.sealed_commits, vec![a.id, b.id]);
}

/// Separate handles race to seal one epoch: exactly one seal may win, and
/// every loser must see `EpochAlreadySealed`, never a silent overwrite.
fn epoch_seal_race_one_wins(f: &Fixture) {
    const RACERS: usize = 6;
    for round in 0..5 {
        let id = format!("race-{round}");
        f.store().create_epoch(&epoch(&id)).unwrap();
        let barrier = Arc::new(Barrier::new(RACERS));
        let racers: Vec<_> = (0..RACERS)
            .map(|i| {
                let (store, barrier, id) = (f.open_another(), Arc::clone(&barrier), id.clone());
                f.spawn(move || {
                    barrier.wait();
                    match store.seal_epoch(
                        &id,
                        &format!("sealer {i}"),
                        Utc::now(),
                        &[],
                        &zero_hash(),
                    ) {
                        Ok(()) => Some(i),
                        Err(StorageError::EpochAlreadySealed { .. }) => None,
                        Err(e) => panic!("unexpected seal error: {e:?}"),
                    }
                })
            })
            .collect();
        let winners: Vec<usize> = racers
            .into_iter()
            .filter_map(|h| h.join().unwrap())
            .collect();
        assert_eq!(
            winners.len(),
            1,
            "round {round}: {} seals won",
            winners.len()
        );
        let summary = f.store().get_epoch(&id).unwrap().unwrap().seal_summary;
        assert_eq!(
            summary,
            Some(format!("sealer {}", winners[0])),
            "the stored seal is the winner's"
        );
    }
}

fn sessions_round_trip_and_end(f: &Fixture) {
    let s = f.store();
    s.create_session(&session("s1")).unwrap();
    s.create_session(&session("s2")).unwrap();
    assert_eq!(s.list_sessions(None).unwrap().len(), 2);
    assert_eq!(s.list_sessions(Some("agent/conformance")).unwrap().len(), 2);
    assert!(s.list_sessions(Some("agent/nope")).unwrap().is_empty());
    let s1 = s.get_session("s1").unwrap().unwrap();
    assert_eq!(s1.status, SessionStatus::Active);
    assert_eq!(s1.path_scope.as_deref(), Some("/scope"));

    let c = commit(s, "in-session", &[]);
    s.set_commit_session(&c.id, "s1").unwrap();
    s.end_session("s1", SessionStatus::Completed, Utc::now())
        .unwrap();
    let s1 = s.get_session("s1").unwrap().unwrap();
    assert_eq!(s1.status, SessionStatus::Completed);
    assert!(s1.ended_at.is_some());

    let late = commit(s, "after-end", &[c.id]);
    assert!(matches!(
        s.set_commit_session(&late.id, "s1"),
        Err(StorageError::SessionEnded { ref id }) if id == "s1"
    ));
    assert!(matches!(
        s.end_session("s1", SessionStatus::Abandoned, Utc::now()),
        Err(StorageError::SessionEnded { .. })
    ));
}

fn taints_round_trip(f: &Fixture) {
    let s = f.store();
    let t = taint("/data/users", "pii");
    s.create_taint(&t).unwrap();
    let got = s.get_taint(&t.id).unwrap().expect("taint by id");
    assert_eq!(got.path, "/data/users");

    let active = s.list_taints(None, None, false).unwrap();
    assert_eq!(active.len(), 1);
    let below = s.check_taint("/data/users/42").unwrap();
    assert_eq!(below.len(), 1, "a propagating taint covers paths below it");
    assert!(s.check_taint("/data/other").unwrap().is_empty());

    s.resolve_taint(&t.id, "agent/reviewer", "cleaned", None, Utc::now())
        .unwrap();
    assert!(s.list_taints(None, None, false).unwrap().is_empty());
    assert_eq!(s.list_taints(None, None, true).unwrap().len(), 1);
    assert!(s.check_taint("/data/users/42").unwrap().is_empty());

    s.create_taint(&taint("/data/users", "pii"))
        .expect("a resolved taint doesn't block a new one on the same path");
}

fn reminders_round_trip_and_cas(f: &Fixture) {
    let s = f.store();
    let r =
        CreateReminder::new("check", "look at it", Utc::now(), "agent/conformance").into_reminder();
    s.save(&r).unwrap();
    let back = s.get(&r.id).unwrap().expect("reminder by id");
    assert_eq!(back.title, "check");

    let mut edited = back.clone();
    edited.title = "checked".to_string();
    assert!(s.update_if_unchanged(&back, &edited).unwrap());
    let mut stale_edit = back.clone();
    stale_edit.title = "lost".to_string();
    assert!(
        !s.update_if_unchanged(&back, &stale_edit).unwrap(),
        "a stale snapshot must not overwrite"
    );
    assert_eq!(s.get(&r.id).unwrap().unwrap().title, "checked");

    // Racing updates from one snapshot: exactly one wins.
    const RACERS: usize = 6;
    let snapshot = s.get(&r.id).unwrap().unwrap();
    let barrier = Arc::new(Barrier::new(RACERS));
    let racers: Vec<_> = (0..RACERS)
        .map(|i| {
            let (store, barrier, snapshot) =
                (f.open_another(), Arc::clone(&barrier), snapshot.clone());
            f.spawn(move || {
                let mut mine = snapshot.clone();
                mine.title = format!("racer {i}");
                barrier.wait();
                store.update_if_unchanged(&snapshot, &mine).unwrap()
            })
        })
        .collect();
    let winners = racers
        .into_iter()
        .map(|h| h.join().unwrap())
        .filter(|won| *won)
        .count();
    assert_eq!(winners, 1, "{winners} racing updates won");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn objects() {
    run("objects_round_trip", objects_round_trip);
}

#[test]
fn commits() {
    run("commits_and_walks", commits_and_walks);
}

#[test]
fn refs() {
    run("refs_and_namespaces", refs_and_namespaces);
}

#[test]
fn refs_create_race() {
    run("create_ref_exactly_once", create_ref_exactly_once);
}

#[test]
fn refs_cas_delete() {
    run(
        "cas_delete_only_what_was_seen",
        cas_delete_only_what_was_seen,
    );
}

#[test]
fn refs_cas_counter() {
    run("cas_counter_loses_nothing", cas_counter_loses_nothing);
}

#[test]
fn postgres_concurrent_connects() {
    run("concurrent_first_connects", concurrent_first_connects);
}

#[test]
fn epochs() {
    run("epochs_round_trip", epochs_round_trip);
}

#[test]
fn epochs_membership() {
    run(
        "epochs_commit_membership_and_seal",
        epochs_commit_membership_and_seal,
    );
}

#[test]
fn epochs_seal_race() {
    run("epoch_seal_race_one_wins", epoch_seal_race_one_wins);
}

#[test]
fn sessions() {
    run("sessions_round_trip_and_end", sessions_round_trip_and_end);
}

#[test]
fn taints() {
    run("taints_round_trip", taints_round_trip);
}

#[test]
fn reminders() {
    run("reminders_round_trip_and_cas", reminders_round_trip_and_cas);
}
