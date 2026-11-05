//! A task's notes (`TaskStore::set_notes`, `AddTaskOptions::notes`) on every
//! built-in backend. Postgres runs when `TEST_DATABASE_URL` is set, with a
//! tenant per case, and must when `ASG_REQUIRE_POSTGRES` is.
//!
//! Covered: notes are kept exactly as given, multi-line and with a NUL;
//! they can be set and cleared at every status, each edit a commit by its
//! agent; the cap counts bytes and refuses one past it without touching the
//! task; a task stored before the field reads as it did and, once its notes
//! are cleared again, is stored byte for byte as before; and a notes edit
//! racing a retitle from another process loses neither.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::{Arc, Barrier};

use agentstategraph::Repository;
use agentstategraph_core::IntentCategory;
use agentstategraph_storage::{MemoryStorage, PostgresStorage, SqliteStorage, Storage};
use agentstategraph_tasks::{
    AddTaskOptions, MAX_NOTES_BYTES, Priority, Proof, TaskId, TaskStatus, TaskStore, TaskStoreError,
};
use serde_json::json;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum Backend {
    Memory,
    Sqlite,
    Postgres,
}

/// Opens repositories onto one case's store. Separate repositories over
/// separate storage handles stand in for separate processes.
struct Fixture {
    opener: Arc<dyn Fn() -> Repository + Send + Sync>,
    cleanup: Option<PathBuf>,
}

impl Fixture {
    fn repo(&self) -> Arc<Repository> {
        let repo = Arc::new((self.opener)());
        repo.init().unwrap();
        repo
    }

    fn store(&self) -> (Arc<Repository>, TaskStore) {
        let repo = self.repo();
        let store = TaskStore::new(Arc::clone(&repo), "/plans", "agent/notes");
        (repo, store)
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

fn fixtures(case: &str) -> Vec<(Backend, Fixture)> {
    let shared: Arc<dyn Storage + Send + Sync> = Arc::new(MemoryStorage::new());
    let mut all = vec![(
        Backend::Memory,
        Fixture {
            opener: Arc::new(move || Repository::from_shared(Arc::clone(&shared))),
            cleanup: None,
        },
    )];

    let path = std::env::temp_dir().join(format!(
        "asg-task-notes-{case}-{}.db",
        uuid::Uuid::new_v4().simple()
    ));
    let opened = path.clone();
    all.push((
        Backend::Sqlite,
        Fixture {
            opener: Arc::new(move || {
                Repository::new(Box::new(SqliteStorage::open(&opened).expect("open sqlite")))
            }),
            cleanup: Some(path),
        },
    ));

    match std::env::var("TEST_DATABASE_URL") {
        Ok(url) if !url.is_empty() => {
            let tenant = format!("task-notes-{case}-{}", uuid::Uuid::new_v4().simple());
            all.push((
                Backend::Postgres,
                Fixture {
                    opener: Arc::new(move || {
                        Repository::new(Box::new(
                            PostgresStorage::connect_tenant_blocking(&url, &tenant, 8)
                                .expect("connect to TEST_DATABASE_URL"),
                        ))
                    }),
                    cleanup: None,
                },
            ));
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
    let mut failed = Vec::new();
    for (backend, fixture) in fixtures(case) {
        if catch_unwind(AssertUnwindSafe(|| body(&fixture))).is_err() {
            failed.push(format!("{backend:?}"));
        }
    }
    assert!(
        failed.is_empty(),
        "{case} failed on {} (see panics above)",
        failed.join(", ")
    );
}

fn id(n: u32) -> TaskId {
    TaskId::new(n)
}

fn with_notes(notes: &str) -> AddTaskOptions {
    AddTaskOptions {
        notes: Some(notes.to_string()),
        ..Default::default()
    }
}

fn add(store: &TaskStore, title: &str, opts: AddTaskOptions) -> TaskId {
    store
        .add_task_with_extensions(
            "main",
            "p",
            title,
            Priority::Medium,
            None,
            vec![],
            None,
            opts,
        )
        .unwrap()
        .id
}

/// Notes as a brief arrives: leading and trailing whitespace, blank lines,
/// CRLF, tabs, quotes, backslashes, non-ASCII, and a NUL from pasted text.
const BRIEF: &str = "  Repo: infra/gitops.\n\n1. Write the manifests.\r\n\t- \"quoted\" \\ path\n2. Ask the user. ✓\u{0}end\n";

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

fn notes_are_kept_exactly(f: &Fixture) {
    let (_repo, store) = f.store();
    store.create_plan("main", "p", None).unwrap();
    let with = add(&store, "with notes", with_notes(BRIEF));
    let without = add(&store, "without", AddTaskOptions::default());
    let blank = add(&store, "blank notes", with_notes(" \n\t "));

    // Read back from a fresh repository, as another process would.
    let (_repo, reader) = f.store();
    let task = reader.get_task("main", "p", &with).unwrap();
    assert_eq!(task.notes.as_deref(), Some(BRIEF));
    assert_eq!(reader.get_task("main", "p", &without).unwrap().notes, None);
    assert_eq!(reader.get_task("main", "p", &blank).unwrap().notes, None);

    let listed = reader.list_tasks("main", "p").unwrap();
    assert_eq!(listed[0], task);
    let (_, tasks) = reader
        .list_plans_with_tasks("main")
        .unwrap()
        .into_iter()
        .find(|(plan, _)| plan.name == "p")
        .unwrap();
    assert_eq!(tasks[0].notes.as_deref(), Some(BRIEF));
}

fn notes_change_at_every_status(f: &Fixture) {
    let (repo, store) = f.store();
    store.create_plan("main", "p", None).unwrap();
    let pending = add(&store, "pending", AddTaskOptions::default());
    let in_progress = add(&store, "in progress", AddTaskOptions::default());
    let done = add(&store, "done", AddTaskOptions::default());
    let abandoned = add(&store, "abandoned", AddTaskOptions::default());
    store.start_task("main", "p", &in_progress).unwrap();
    store.start_task("main", "p", &done).unwrap();
    store
        .complete_task("main", "p", &done, Proof::commit("abc123"))
        .unwrap();
    store
        .abandon_task("main", "p", &abandoned, "not needed")
        .unwrap();

    for task in [&pending, &in_progress, &done, &abandoned] {
        let before = store.get_task("main", "p", task).unwrap();
        let text = format!("notes for {task}\nsecond line");

        let set = store.set_notes("main", "p", task, Some(&text)).unwrap();
        assert_eq!(
            set,
            agentstategraph_tasks::Task {
                notes: Some(text.clone()),
                ..before.clone()
            },
            "set_notes changed more than the notes on {task}"
        );
        assert_eq!(store.get_task("main", "p", task).unwrap(), set);
        let head = repo.log("main", 1).unwrap().remove(0);
        assert_eq!(head.agent_id, "agent/notes");
        assert_eq!(head.intent.category, IntentCategory::Plan);
        assert_eq!(head.intent.description, format!("Set notes on p/{task}"));

        let cleared = store.set_notes("main", "p", task, None).unwrap();
        assert_eq!(cleared, before);
        let head = repo.log("main", 1).unwrap().remove(0);
        assert_eq!(head.intent.description, format!("Clear notes on p/{task}"));
    }

    let done_task = store.get_task("main", "p", &done).unwrap();
    assert_eq!(done_task.status, TaskStatus::Done);
    assert_eq!(done_task.proof, Some(Proof::commit("abc123")));

    // Blank text clears, like None.
    for blank in ["", "   ", "\n\t"] {
        store
            .set_notes("main", "p", &pending, Some("keep"))
            .unwrap();
        let cleared = store.set_notes("main", "p", &pending, Some(blank)).unwrap();
        assert_eq!(cleared.notes, None, "{blank:?} should clear the notes");
    }

    let err = store
        .set_notes("main", "p", &id(99), Some("x"))
        .unwrap_err();
    assert!(
        matches!(err, TaskStoreError::TaskNotFound { .. }),
        "{err:?}"
    );
}

fn the_cap_counts_bytes(f: &Fixture) {
    let (_repo, store) = f.store();
    store.create_plan("main", "p", None).unwrap();
    // "é" is two bytes, so this is MAX_NOTES_BYTES bytes in half as many
    // characters.
    let at_cap = "é".repeat(MAX_NOTES_BYTES / 2);
    assert_eq!(at_cap.len(), MAX_NOTES_BYTES);
    let over = format!("{at_cap}!");

    let task = add(&store, "at the cap", with_notes(&at_cap));
    assert_eq!(
        store.get_task("main", "p", &task).unwrap().notes.as_deref(),
        Some(at_cap.as_str())
    );

    let err = store
        .set_notes("main", "p", &task, Some(&over))
        .unwrap_err();
    match err {
        TaskStoreError::NotesTooLong { len, max } => {
            assert_eq!((len, max), (MAX_NOTES_BYTES + 1, MAX_NOTES_BYTES))
        }
        other => panic!("expected NotesTooLong, got {other:?}"),
    }
    assert_eq!(
        store.get_task("main", "p", &task).unwrap().notes.as_deref(),
        Some(at_cap.as_str()),
        "a refused edit must leave the notes as they were"
    );

    let err = store
        .add_task_with_extensions(
            "main",
            "p",
            "too long",
            Priority::Medium,
            None,
            vec![],
            None,
            with_notes(&over),
        )
        .unwrap_err();
    assert!(
        matches!(err, TaskStoreError::NotesTooLong { .. }),
        "{err:?}"
    );
    assert_eq!(store.task_ids("main", "p").unwrap(), vec![task]);
}

fn a_task_stored_before_notes_reads_unchanged(f: &Fixture) {
    let (repo, store) = f.store();
    store.create_plan("main", "p", None).unwrap();
    // A task as 1.5.0 stored it: no `notes` key.
    let stored = json!({
        "id": "t-001", "title": "old task", "status": "in_progress",
        "priority": "high", "parent_id": null, "blocked_by": [],
        "created_at": "2026-10-01T00:00:00Z", "created_by": "agent/old",
        "started_at": "2026-10-02T00:00:00Z", "started_by": "agent/old",
        "completed_at": null, "completed_by": null, "proof": null,
        "abandoned_at": null, "abandoned_reason": null,
        "assigned_to": "agent/old",
    });
    repo.set_json(
        "main",
        "/plans/p/t-001",
        &stored,
        agentstategraph::CommitOptions::new("agent/old", IntentCategory::Plan, "old write"),
    )
    .unwrap();

    let task = store.get_task("main", "p", &id(1)).unwrap();
    assert_eq!(task.notes, None);
    assert_eq!(task.title, "old task");

    let set = store
        .set_notes("main", "p", &id(1), Some("added later"))
        .unwrap();
    assert_eq!(set.notes.as_deref(), Some("added later"));
    assert_eq!(set.status, TaskStatus::InProgress);
    assert_eq!(set.assigned_to.as_deref(), Some("agent/old"));

    store.set_notes("main", "p", &id(1), None).unwrap();
    assert_eq!(repo.get_json("main", "/plans/p/t-001").unwrap(), stored);
}

fn a_notes_edit_racing_a_retitle_loses_neither(f: &Fixture) {
    const EDITS: usize = 6;
    let (_repo, store) = f.store();
    store.create_plan("main", "p", None).unwrap();
    let task = add(&store, "title 0", AddTaskOptions::default());

    let barrier = Arc::new(Barrier::new(2));
    let notes_writer = {
        let (_repo, store) = f.store();
        let (barrier, task) = (Arc::clone(&barrier), task.clone());
        std::thread::spawn(move || {
            barrier.wait();
            for i in 1..=EDITS {
                store
                    .set_notes("main", "p", &task, Some(&format!("notes {i}")))
                    .unwrap();
            }
        })
    };
    let title_writer = {
        let (_repo, store) = f.store();
        let (barrier, task) = (Arc::clone(&barrier), task.clone());
        std::thread::spawn(move || {
            barrier.wait();
            for i in 1..=EDITS {
                store
                    .set_title("main", "p", &task, &format!("title {i}"))
                    .unwrap();
            }
        })
    };
    notes_writer.join().unwrap();
    title_writer.join().unwrap();

    let (_repo, reader) = f.store();
    let task = reader.get_task("main", "p", &task).unwrap();
    assert_eq!(task.notes, Some(format!("notes {EDITS}")));
    assert_eq!(task.title, format!("title {EDITS}"));
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn notes_are_kept_exactly_on_every_backend() {
    run("kept", notes_are_kept_exactly);
}

#[test]
fn notes_change_at_every_status_on_every_backend() {
    run("status", notes_change_at_every_status);
}

#[test]
fn the_cap_counts_bytes_on_every_backend() {
    run("cap", the_cap_counts_bytes);
}

#[test]
fn a_task_stored_before_notes_reads_unchanged_on_every_backend() {
    run("legacy", a_task_stored_before_notes_reads_unchanged);
}

#[test]
fn a_notes_edit_racing_a_retitle_loses_neither_on_every_backend() {
    run("race", a_notes_edit_racing_a_retitle_loses_neither);
}
