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
use std::thread::JoinHandle;

use agentstategraph_core::{
    Authority, Commit, CommitBuilder, Epoch, EpochScope, EpochStatus, Intent, IntentCategory,
    Namespace, Object, ObjectId, Session, SessionStatus, TAG_GIT_REVISION, TAG_PIN_STATE,
};
use agentstategraph_reminders::CreateReminder;
use agentstategraph_storage::{HistoryRollupRow, Storage, StorageError};
use agentstategraph_taint::{Taint, TaintEffect, TaintKind, TaintMetadata, TaintSeverity};
use chrono::{DateTime, Utc};

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

const KNOWN_FAILURES: &[Known] = &[];

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// One test's store, plus a way to open more handles onto it. Separate
/// handles stand in for separate processes sharing one store.
struct Fixture {
    backend: Backend,
    store: Store,
    opener: Arc<dyn Fn() -> Store + Send + Sync>,
    cleanup: Option<std::path::PathBuf>,
}

impl Fixture {
    fn store(&self) -> &Store {
        &self.store
    }

    fn open_another(&self) -> Store {
        (self.opener)()
    }

    /// Run `f` on a new plain thread. Every backend must work from one; for
    /// Postgres that needs no async runtime (Plan D t-002).
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

#[cfg(feature = "memory")]
fn memory_fixture() -> Fixture {
    let store: Store = Arc::new(agentstategraph_storage::MemoryStorage::new());
    let shared = Arc::clone(&store);
    Fixture {
        backend: Backend::Memory,
        store,
        // One process: every "handle" is the same instance.
        opener: Arc::new(move || Arc::clone(&shared)),
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
        cleanup: Some(path),
    }
}

/// Connects from whatever thread calls it, with no runtime and no lock:
/// concurrent connects serialize their schema setup in the database.
#[cfg(feature = "postgres")]
fn postgres_connect(url: &str, tenant: &str) -> Store {
    let storage = agentstategraph_storage::PostgresStorage::connect_tenant_blocking(url, tenant, 8)
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
        let outcome = catch_unwind(AssertUnwindSafe(|| body(&fixture)));
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

/// Enough objects to cross the Postgres batch's chunk size (1,000 rows per
/// INSERT), with a duplicate and a missing id, all answered in order.
fn batch_objects_large(f: &Fixture) {
    let s = f.store();
    let objs: Vec<Object> = (0..2500)
        .map(|i| Object::string(format!("obj-{i}")))
        .collect();
    let ids = s.batch_put_objects(&objs).unwrap();
    assert_eq!(
        ids,
        objs.iter().map(|o| o.id()).collect::<Vec<_>>(),
        "ids in input order"
    );
    assert_eq!(
        s.batch_put_objects(&objs[..10]).unwrap(),
        ids[..10],
        "a repeated batch is a no-op with the same ids"
    );
    let mut ask: Vec<ObjectId> = ids.iter().rev().copied().collect();
    ask.insert(1234, ObjectId::hash(b"never stored"));
    let got = s.batch_get_objects(&ask).unwrap();
    assert_eq!(got.len(), ask.len());
    assert_eq!(
        got[1234], None,
        "a missing id reads back as None in its slot"
    );
    assert_eq!(got[0], Some(objs[2499].clone()));
    assert_eq!(got[2500], Some(objs[0].clone()));
    assert!(s.batch_get_objects(&[]).unwrap().is_empty());
    assert!(s.batch_put_objects(&[]).unwrap().is_empty());
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

/// Walks over long and merge-heavy histories, and over a pruned parent.
fn dag_walks_at_scale(f: &Fixture) {
    let s = f.store();

    // A long line comes back in order, whole or up to the limit.
    let mut line = vec![commit(s, "line-0", &[])];
    for i in 1..1200 {
        let parent = line[i - 1].id;
        line.push(commit(s, &format!("line-{i}"), &[parent]));
    }
    let tip = line.last().unwrap().id;
    let newest_first: Vec<ObjectId> = line.iter().rev().map(|c| c.id).collect();
    let ids = |commits: &[Commit]| commits.iter().map(|c| c.id).collect::<Vec<_>>();
    assert_eq!(ids(&s.list_commits(&tip, 5000).unwrap()), newest_first);
    assert_eq!(
        ids(&s.list_commits(&tip, 100).unwrap()),
        newest_first[..100]
    );
    assert!(s.list_commits(&tip, 0).unwrap().is_empty());
    let whole = s.list_commits_dag(&tip, 1200).unwrap();
    assert_eq!(ids(&whole.commits), newest_first, "a line walks in order");
    assert!(
        !whole.truncated,
        "a limit equal to the history is not truncation"
    );
    let cut = s.list_commits_dag(&tip, 1199).unwrap();
    assert_eq!(cut.commits.len(), 1199);
    assert!(cut.truncated);

    // A ladder of 40 diamonds has 2^40 paths from top to bottom but 121
    // commits. A walk that revisits shared ancestors never finishes.
    let bottom = commit(s, "ladder-0", &[]);
    let mut top = bottom.clone();
    let mut all: HashSet<ObjectId> = HashSet::from([bottom.id]);
    for i in 0..40 {
        let l = commit(s, &format!("ladder-{i}-l"), &[top.id]);
        let r = commit(s, &format!("ladder-{i}-r"), &[top.id]);
        top = commit(s, &format!("ladder-{i}-m"), &[l.id, r.id]);
        all.extend([l.id, r.id, top.id]);
    }
    let ladder = s.list_commits_dag(&top.id, 10_000).unwrap();
    assert_eq!(ladder.commits.len(), all.len(), "each commit appears once");
    assert_eq!(
        ids(&ladder.commits).into_iter().collect::<HashSet<_>>(),
        all
    );
    assert!(!ladder.truncated);
    assert_eq!(ladder.commits.first().unwrap().id, top.id);
    assert_eq!(
        ladder.commits.last().unwrap().id,
        bottom.id,
        "breadth first: the deepest commit comes last"
    );

    // A parent that isn't stored ends that edge.
    let orphan = commit(s, "orphan", &[ObjectId::hash(b"never stored")]);
    let walk = s.list_commits_dag(&orphan.id, 10).unwrap();
    assert_eq!(ids(&walk.commits), vec![orphan.id]);
    assert!(!walk.truncated);
    assert_eq!(
        ids(&s.list_commits(&orphan.id, 10).unwrap()),
        vec![orphan.id]
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
    // Processes starting at once each run the schema setup. Unserialized,
    // they deadlocked and one failed to start; the setup now holds an
    // advisory lock (Plan D t-002, ahead of t-006's versioned migrations).
    #[cfg(feature = "postgres")]
    {
        let url = postgres_url().expect("postgres fixture implies a url");
        let barrier = Arc::new(Barrier::new(6));
        let connects: Vec<_> = (0..6)
            .map(|i| {
                let (url, barrier) = (url.clone(), Arc::clone(&barrier));
                std::thread::spawn(move || {
                    barrier.wait();
                    agentstategraph_storage::PostgresStorage::connect_tenant_blocking(
                        &url,
                        &format!("conformance-connect-{i}-{}", uuid::Uuid::new_v4().simple()),
                        2,
                    )
                })
            })
            .collect();
        for c in connects {
            c.join().unwrap().expect("concurrent connect");
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

    // Linking a commit twice keeps it a single member.
    s.create_epoch(&epoch("twice")).unwrap();
    let c = commit(s, "c", &[]);
    s.set_commit_epoch(&c.id, "twice").unwrap();
    s.set_commit_epoch(&c.id, "twice").unwrap();
    assert_eq!(s.get_epoch("twice").unwrap().unwrap().commits, vec![c.id]);
}

/// Separate handles race to end one session: exactly one may end it, and the
/// rest must see `SessionEnded`.
fn session_end_race_one_wins(f: &Fixture) {
    const RACERS: usize = 6;
    for round in 0..5 {
        let id = format!("end-race-{round}");
        f.store().create_session(&session(&id)).unwrap();
        let barrier = Arc::new(Barrier::new(RACERS));
        let racers: Vec<_> = (0..RACERS)
            .map(|_| {
                let (store, barrier, id) = (f.open_another(), Arc::clone(&barrier), id.clone());
                f.spawn(move || {
                    barrier.wait();
                    match store.end_session(&id, SessionStatus::Completed, Utc::now()) {
                        Ok(()) => true,
                        Err(StorageError::SessionEnded { .. }) => false,
                        Err(e) => panic!("unexpected end_session error: {e:?}"),
                    }
                })
            })
            .collect();
        let winners = racers
            .into_iter()
            .map(|h| h.join().unwrap())
            .filter(|w| *w)
            .count();
        assert_eq!(
            winners, 1,
            "round {round}: {winners} callers ended the session"
        );
    }
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
// History and search
//
// The memory backend keeps neither (the trait defaults: nothing extracted,
// nothing indexed), so it is held to those defaults and the SQL backends to
// one contract.
// ---------------------------------------------------------------------------

fn at(rfc3339: &str) -> DateTime<Utc> {
    rfc3339.parse().expect("test timestamp")
}

fn history_commit(
    s: &Store,
    tag: &str,
    agent: &str,
    category: IntentCategory,
    tags: &[&str],
    timestamp: DateTime<Utc>,
) -> Commit {
    let root = s.put_object(&Object::string(tag)).unwrap();
    let mut c = CommitBuilder::new(
        root,
        agent,
        Authority::simple("conformance"),
        Intent::new(category, tag).with_tags(tags.iter().map(|t| t.to_string()).collect()),
    )
    .build();
    c.timestamp = timestamp;
    s.put_commit(&c).unwrap();
    c
}

fn extract_all(s: &Store, batch: usize) -> usize {
    let mut total = 0;
    loop {
        match s.history_extract_batch(batch).unwrap() {
            0 => return total,
            n => total += n,
        }
    }
}

fn bucket(
    day: &str,
    namespace: &str,
    agent: &str,
    category: &str,
    commits: i64,
    first: &str,
    last: &str,
) -> HistoryRollupRow {
    HistoryRollupRow {
        day: day.into(),
        namespace: namespace.into(),
        agent_id: agent.into(),
        intent_category: category.into(),
        commit_count: commits,
        first_ts: at(first).to_rfc3339(),
        last_ts: at(last).to_rfc3339(),
    }
}

fn history_rollup_and_milestones(f: &Fixture) {
    use IntentCategory::{Checkpoint, Refine};
    let s = f.store();
    let pin = TAG_PIN_STATE.to_string();
    let git = format!("{TAG_GIT_REVISION}abc123");
    let a1 = history_commit(s, "a1", "agent/a", Refine, &[], at("2026-03-01T09:00:00Z"));
    history_commit(s, "a2", "agent/a", Refine, &[], at("2026-03-01T10:00:00Z"));
    let pinned = history_commit(
        s,
        "pinned",
        "agent/b",
        Checkpoint,
        &[&pin, &git],
        at("2026-03-01T11:00:00Z"),
    );
    // Same second, one with a fraction: they must order by time, not by
    // the text's punctuation.
    let late = history_commit(
        s,
        "late",
        "agent/b",
        Checkpoint,
        &[],
        at("2026-03-02T17:30:00.250Z"),
    );
    let early = history_commit(
        s,
        "early",
        "agent/b",
        Checkpoint,
        &[],
        at("2026-03-02T17:30:00Z"),
    );
    // A commit in a scoped session is attributed to the session's namespace.
    let scoped = history_commit(
        s,
        "scoped",
        "agent/a",
        Refine,
        &[],
        at("2026-03-02T08:00:00Z"),
    );
    let mut sess = session("sess-team");
    sess.scope_namespace = Some(Namespace::new("team-a").unwrap());
    s.create_session(&sess).unwrap();
    s.set_commit_session(&scoped.id, "sess-team").unwrap();

    if f.backend == Backend::Memory {
        assert_eq!(s.history_extract_batch(10).unwrap(), 0);
        assert!(s.history_rollup().unwrap().is_empty());
        assert!(s.history_milestones(10).unwrap().is_empty());
        assert_eq!(s.history_cursor().unwrap(), 0);
        assert_eq!(s.history_undistilled_commit_count().unwrap(), 0);
        assert!(!s.history_is_commit_distilled(&a1.id).unwrap());
        return;
    }

    assert_eq!(s.history_undistilled_commit_count().unwrap(), 6);
    assert!(!s.history_is_commit_distilled(&a1.id).unwrap());
    assert_eq!(extract_all(s, 2), 6, "folded in batches of two");
    assert_eq!(extract_all(s, 2), 0, "a caught-up extractor does nothing");
    assert!(s.history_cursor().unwrap() > 0);
    assert!(s.history_is_commit_distilled(&a1.id).unwrap());
    assert!(
        !s.history_is_commit_distilled(&ObjectId::hash(b"absent"))
            .unwrap()
    );
    assert_eq!(s.history_undistilled_commit_count().unwrap(), 0);

    assert_eq!(
        s.history_rollup().unwrap(),
        vec![
            bucket(
                "2026-03-01",
                "unattributed",
                "agent/a",
                "Refine",
                2,
                "2026-03-01T09:00:00Z",
                "2026-03-01T10:00:00Z"
            ),
            bucket(
                "2026-03-01",
                "unattributed",
                "agent/b",
                "Checkpoint",
                1,
                "2026-03-01T11:00:00Z",
                "2026-03-01T11:00:00Z"
            ),
            bucket(
                "2026-03-02",
                "team-a",
                "agent/a",
                "Refine",
                1,
                "2026-03-02T08:00:00Z",
                "2026-03-02T08:00:00Z"
            ),
            bucket(
                "2026-03-02",
                "unattributed",
                "agent/b",
                "Checkpoint",
                2,
                "2026-03-02T17:30:00Z",
                "2026-03-02T17:30:00.250Z"
            ),
        ]
    );

    let milestones = s.history_milestones(10).unwrap();
    let order: Vec<ObjectId> = milestones.iter().map(|m| m.commit_id).collect();
    assert_eq!(order, vec![late.id, early.id, pinned.id], "newest first");
    let first = &milestones[2];
    assert_eq!(first.kind, "checkpoint");
    assert_eq!(first.day, "2026-03-01");
    assert_eq!(first.namespace, "unattributed");
    assert_eq!(first.agent_id, "agent/b");
    assert_eq!(first.description, "pinned");
    assert_eq!(first.timestamp, at("2026-03-01T11:00:00Z").to_rfc3339());
    assert_eq!(
        first.state_root,
        Some(pinned.state_root),
        "pinned on request"
    );
    assert_eq!(first.git_sha.as_deref(), Some("abc123"));
    assert_eq!(milestones[0].state_root, None, "not pinned by default");
    assert_eq!(milestones[0].git_sha, None);
    assert_eq!(s.history_milestones(1).unwrap().len(), 1);
    assert_eq!(
        s.history_retained_state_roots().unwrap(),
        vec![pinned.state_root]
    );

    // Later commits fold incrementally into the existing buckets.
    let a3 = history_commit(s, "a3", "agent/a", Refine, &[], at("2026-03-01T12:00:00Z"));
    history_commit(s, "a0", "agent/a", Refine, &[], at("2026-03-01T08:30:00Z"));
    assert_eq!(s.history_undistilled_commit_count().unwrap(), 2);
    assert!(!s.history_is_commit_distilled(&a3.id).unwrap());
    assert_eq!(extract_all(s, 100), 2);
    assert_eq!(
        s.history_rollup().unwrap()[0],
        bucket(
            "2026-03-01",
            "unattributed",
            "agent/a",
            "Refine",
            4,
            "2026-03-01T08:30:00Z",
            "2026-03-01T12:00:00Z"
        )
    );
}

/// Extractors running at once fold every commit exactly once. A backend may
/// make a loser fail rather than wait (SQLite does); it retries.
fn history_concurrent_extractors_count_once(f: &Fixture) {
    const COMMITS: usize = 120;
    const EXTRACTORS: usize = 4;
    let s = f.store();
    for i in 0..COMMITS {
        commit(s, &format!("c{i}"), &[]);
    }
    if f.backend == Backend::Memory {
        return;
    }
    let barrier = Arc::new(Barrier::new(EXTRACTORS));
    let extractors: Vec<_> = (0..EXTRACTORS)
        .map(|_| {
            let (store, barrier) = (f.open_another(), Arc::clone(&barrier));
            f.spawn(move || {
                barrier.wait();
                let mut failures = 0;
                loop {
                    match store.history_extract_batch(7) {
                        Ok(0) => return,
                        Ok(_) => {}
                        Err(e) => {
                            failures += 1;
                            assert!(failures < 1000, "extractor keeps failing: {e}");
                        }
                    }
                }
            })
        })
        .collect();
    for e in extractors {
        e.join().unwrap();
    }
    let folded: i64 = s
        .history_rollup()
        .unwrap()
        .iter()
        .map(|b| b.commit_count)
        .sum();
    assert_eq!(folded, COMMITS as i64, "every commit folded exactly once");
    assert_eq!(s.history_undistilled_commit_count().unwrap(), 0);
    assert_eq!(s.history_milestones(1000).unwrap().len(), COMMITS);
}

fn leaf_index_search(f: &Fixture) {
    let s = f.store();
    let leaves = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(p, v)| (p.to_string(), v.to_string()))
            .collect()
    };
    let search = |query: &str| -> HashSet<(String, String)> {
        s.leaf_index_search("default", "main", query, 50)
            .unwrap()
            .expect("the set is built")
            .into_iter()
            .collect()
    };
    let hits = |pairs: &[(&str, &str)]| -> HashSet<(String, String)> {
        leaves(pairs).into_iter().collect()
    };

    assert!(!s.leaf_index_is_built("default", "main").unwrap());
    assert_eq!(
        s.leaf_index_search("default", "main", "x", 10).unwrap(),
        None
    );
    s.leaf_index_apply("default", "main", &[], &leaves(&[("/x", "x")]))
        .unwrap();
    assert!(
        !s.leaf_index_is_built("default", "main").unwrap(),
        "maintaining an unbuilt set builds nothing"
    );
    if f.backend == Backend::Memory {
        s.leaf_index_build("default", "main", &leaves(&[("/a", "a")]))
            .unwrap();
        assert!(!s.leaf_index_is_built("default", "main").unwrap());
        return;
    }

    s.leaf_index_build(
        "default",
        "main",
        &leaves(&[
            ("/a", "Hello World"),
            ("/b", "100% sure"),
            ("/c", "snake_case"),
            ("/d", "back\\slash"),
            ("/e", "other"),
            ("/f", "ezc"),
        ]),
    )
    .unwrap();
    s.leaf_index_build("default", "dev", &leaves(&[("/a", "hello dev")]))
        .unwrap();
    s.leaf_index_build("team", "main", &leaves(&[("/a", "hello team")]))
        .unwrap();
    assert!(s.leaf_index_is_built("default", "main").unwrap());

    // Case-insensitive substrings, from one (namespace, ref) only.
    assert_eq!(search("hello"), hits(&[("/a", "Hello World")]));
    assert_eq!(search("lo w"), hits(&[("/a", "Hello World")]));
    // Wildcard characters match themselves, short queries and long.
    assert_eq!(search("%"), hits(&[("/b", "100% sure")]));
    assert_eq!(search("_"), hits(&[("/c", "snake_case")]));
    assert_eq!(search("e_c"), hits(&[("/c", "snake_case")]));
    assert_eq!(search("\\"), hits(&[("/d", "back\\slash")]));
    assert!(search("zzz").is_empty());
    assert_eq!(
        s.leaf_index_search("default", "main", "e", 2)
            .unwrap()
            .unwrap()
            .len(),
        2,
        "the limit is honoured"
    );

    // Maintenance removes and adds paths, and replaces a path's value.
    s.leaf_index_apply(
        "default",
        "main",
        &["/a".to_string(), "/e".to_string()],
        &leaves(&[("/g", "fresh hello"), ("/e", "renamed")]),
    )
    .unwrap();
    assert_eq!(search("hello"), hits(&[("/g", "fresh hello")]));
    assert!(search("other").is_empty());
    assert_eq!(search("renamed"), hits(&[("/e", "renamed")]));

    // A rebuild replaces the whole set.
    s.leaf_index_build("default", "main", &leaves(&[("/z", "only")]))
        .unwrap();
    assert!(search("hello").is_empty());
    assert_eq!(search("only"), hits(&[("/z", "only")]));
}

/// A set records the tree it reflects, and a diff applies only to the tree it
/// was computed from, so updates that arrive out of order can't leave an
/// overwritten value behind.
fn leaf_index_roots(f: &Fixture) {
    let s = f.store();
    let (r1, r2, r3) = (
        ObjectId::hash(b"tree 1"),
        ObjectId::hash(b"tree 2"),
        ObjectId::hash(b"tree 3"),
    );
    let one = |path: &str, value: &str| vec![(path.to_string(), value.to_string())];
    let found = |query: &str| s.leaf_index_search("default", "main", query, 10).unwrap();

    if f.backend == Backend::Memory {
        assert!(!s.leaf_index_tracks_roots());
        s.leaf_index_build_at("default", "main", &r1, &one("/a", "v1"))
            .unwrap();
        assert_eq!(s.leaf_index_root("default", "main").unwrap(), None);
        return;
    }
    assert!(s.leaf_index_tracks_roots());
    assert_eq!(s.leaf_index_root("default", "main").unwrap(), None);
    assert!(
        !s.leaf_index_advance("default", "main", &r1, &r2, &[], &[])
            .unwrap(),
        "an unbuilt set doesn't move"
    );

    s.leaf_index_build_at("default", "main", &r1, &one("/a", "v1"))
        .unwrap();
    assert!(s.leaf_index_is_built("default", "main").unwrap());
    assert_eq!(s.leaf_index_root("default", "main").unwrap(), Some(r1));

    // r1 -> r2 applies; a late r1 -> r3 (computed from a tree the set has
    // left) is refused and changes nothing.
    assert!(
        s.leaf_index_advance(
            "default",
            "main",
            &r1,
            &r2,
            &["/a".into()],
            &one("/a", "v2")
        )
        .unwrap()
    );
    assert_eq!(s.leaf_index_root("default", "main").unwrap(), Some(r2));
    assert!(
        !s.leaf_index_advance(
            "default",
            "main",
            &r1,
            &r3,
            &["/a".into()],
            &one("/a", "v3")
        )
        .unwrap()
    );
    assert_eq!(s.leaf_index_root("default", "main").unwrap(), Some(r2));
    assert_eq!(found("v"), Some(one("/a", "v2")));

    // Changes made without a root leave the set's tree unknown.
    s.leaf_index_apply("default", "main", &["/a".into()], &one("/a", "v4"))
        .unwrap();
    assert_eq!(s.leaf_index_root("default", "main").unwrap(), None);
    assert_eq!(found("v"), Some(one("/a", "v4")));
    s.leaf_index_build_at("default", "main", &r3, &one("/a", "v5"))
        .unwrap();
    assert_eq!(s.leaf_index_root("default", "main").unwrap(), Some(r3));
    s.leaf_index_build("default", "main", &one("/a", "v6"))
        .unwrap();
    assert_eq!(s.leaf_index_root("default", "main").unwrap(), None);
    assert!(s.leaf_index_is_built("default", "main").unwrap());
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn objects() {
    run("objects_round_trip", objects_round_trip);
}

#[test]
fn objects_batch() {
    run("batch_objects_large", batch_objects_large);
}

#[test]
fn commits() {
    run("commits_and_walks", commits_and_walks);
}

#[test]
fn commits_dag_scale() {
    run("dag_walks_at_scale", dag_walks_at_scale);
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
fn sessions_end_race() {
    run("session_end_race_one_wins", session_end_race_one_wins);
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

#[test]
fn history() {
    run(
        "history_rollup_and_milestones",
        history_rollup_and_milestones,
    );
}

#[test]
fn history_concurrent_extractors() {
    run(
        "history_concurrent_extractors_count_once",
        history_concurrent_extractors_count_once,
    );
}

#[test]
fn leaf_index() {
    run("leaf_index_search", leaf_index_search);
}

#[test]
fn leaf_index_root_tracking() {
    run("leaf_index_roots", leaf_index_roots);
}
