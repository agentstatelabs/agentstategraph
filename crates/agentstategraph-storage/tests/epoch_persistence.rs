//! Integration tests for `EpochStore` across all built-in backends.
//!
//! Covers:
//! - Round-trip (create → list → seal → list)
//! - Restart survival for the SQLite backend (open path, drop, reopen)
//! - Seal enforcement (set_commit_epoch must fail after seal)
//! - Migration safety (create DB with the pre-0.6.5 schema and verify
//!   the current init() migrates cleanly without disturbing existing rows)

use agentstategraph_core::{Epoch, ObjectId};
use agentstategraph_storage::{
    CommitStore, EpochStore, ObjectStore, RefStore, SqliteStorage, StorageError,
};
use chrono::Utc;

fn sample_epoch(id: &str) -> Epoch {
    Epoch::new(id, format!("epoch {}", id), vec!["intent-1".into()])
}

// ---------------------------------------------------------------------------
// Round-trip (both backends)
// ---------------------------------------------------------------------------

fn round_trip<S: EpochStore>(store: &S) {
    store.create_epoch(&sample_epoch("e1")).unwrap();
    store.create_epoch(&sample_epoch("e2")).unwrap();

    let list = store.list_epochs().unwrap();
    assert_eq!(list.len(), 2);

    let e1 = store.get_epoch("e1").unwrap().unwrap();
    assert_eq!(e1.id, "e1");
    assert!(e1.sealed_at.is_none());
    assert!(e1.seal_summary.is_none());

    store
        .seal_epoch("e1", "wrapped up", Utc::now(), &[])
        .unwrap();

    let e1 = store.get_epoch("e1").unwrap().unwrap();
    assert_eq!(
        e1.status,
        agentstategraph_core::EpochStatus::Sealed,
        "status must persist as Sealed"
    );
    assert!(e1.sealed_at.is_some(), "sealed_at must be populated");
    assert_eq!(e1.seal_summary.as_deref(), Some("wrapped up"));
}

#[test]
fn round_trip_sqlite() {
    round_trip(&SqliteStorage::in_memory().unwrap());
}

// ---------------------------------------------------------------------------
// Restart survival (SQLite only — in-memory connections cannot persist by design).
// ---------------------------------------------------------------------------

#[test]
fn sqlite_epoch_survives_reopen() {
    let dir = tempdir_sibling("epoch-restart");
    let db_path = dir.join("state.db");
    {
        let storage = SqliteStorage::open(&db_path).unwrap();
        storage.create_epoch(&sample_epoch("survivor")).unwrap();
        storage
            .seal_epoch("survivor", "locked in", Utc::now(), &[])
            .unwrap();
    } // drop the handle — simulates process exit

    let reopened = SqliteStorage::open(&db_path).unwrap();
    let list = reopened.list_epochs().unwrap();
    assert_eq!(list.len(), 1);
    let e = reopened.get_epoch("survivor").unwrap().unwrap();
    assert_eq!(e.seal_summary.as_deref(), Some("locked in"));
    assert!(e.sealed_at.is_some());
    assert_eq!(e.status, agentstategraph_core::EpochStatus::Sealed);

    // Clean up — not strictly necessary but keeps the test dir tidy.
    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_dir(&dir);
}

// ---------------------------------------------------------------------------
// Seal enforcement
// ---------------------------------------------------------------------------

fn seal_enforcement<S: EpochStore + CommitStore + ObjectStore>(store: &S) {
    store.create_epoch(&sample_epoch("locked")).unwrap();

    // Associate an active commit first — must succeed.
    let commit = demo_commit(store);
    store.set_commit_epoch(&commit.id, "locked").unwrap();

    // Seal it.
    store.seal_epoch("locked", "done", Utc::now(), &[]).unwrap();

    // A second association attempt must fail with EpochAlreadySealed.
    let second = demo_commit_with_tag(store, "post-seal");
    let err = store.set_commit_epoch(&second.id, "locked").unwrap_err();
    assert!(
        matches!(err, StorageError::EpochAlreadySealed { ref id } if id == "locked"),
        "expected EpochAlreadySealed, got {:?}",
        err
    );

    // Double-sealing is also rejected.
    let err = store
        .seal_epoch("locked", "again", Utc::now(), &[])
        .unwrap_err();
    assert!(matches!(err, StorageError::EpochAlreadySealed { .. }));
}

#[test]
fn seal_enforcement_sqlite() {
    seal_enforcement(&SqliteStorage::in_memory().unwrap());
}

// ---------------------------------------------------------------------------
// Migration safety — pre-0.6.5 SQLite DB with only objects/commits/refs
// must open and gain the new columns/tables without disturbing any rows.
// ---------------------------------------------------------------------------

#[test]
fn sqlite_migration_safety() {
    let dir = tempdir_sibling("epoch-migration");
    let db_path = dir.join("legacy.db");

    // Build a pre-0.6.5 DB by hand — only the three original tables,
    // no epochs/sessions and no epoch_id/session_id columns.
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "
            CREATE TABLE objects (
                id   BLOB PRIMARY KEY,
                data BLOB NOT NULL
            );
            CREATE TABLE commits (
                id        BLOB PRIMARY KEY,
                data      BLOB NOT NULL,
                timestamp TEXT NOT NULL
            );
            CREATE TABLE refs (
                name   TEXT PRIMARY KEY,
                target BLOB NOT NULL
            );
            INSERT INTO objects (id, data) VALUES (x'01', x'99');
            INSERT INTO commits (id, data, timestamp)
                VALUES (x'02', x'99', '2026-01-01T00:00:00Z');
            INSERT INTO refs (name, target) VALUES ('main', x'02');
            ",
        )
        .unwrap();
    }

    // Open via current SqliteStorage — init runs and migrates.
    let storage = SqliteStorage::open(&db_path).unwrap();

    // Existing rows untouched.
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let obj_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM objects", [], |row| row.get(0))
        .unwrap();
    let commit_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM commits", [], |row| row.get(0))
        .unwrap();
    let ref_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM refs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(obj_count, 1);
    assert_eq!(commit_count, 1);
    assert_eq!(ref_count, 1);

    // New columns must exist.
    let cols: Vec<String> = conn
        .prepare("PRAGMA table_info(commits)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(cols.iter().any(|c| c == "epoch_id"));
    assert!(cols.iter().any(|c| c == "session_id"));

    // New tables must exist and be usable.
    storage.create_epoch(&sample_epoch("post-migrate")).unwrap();
    assert_eq!(storage.list_epochs().unwrap().len(), 1);

    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_dir(&dir);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn tempdir_sibling(tag: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "asg-test-{}-{}",
        tag,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn demo_commit<S: CommitStore + ObjectStore>(store: &S) -> agentstategraph_core::Commit {
    demo_commit_with_tag(store, "demo")
}

fn demo_commit_with_tag<S: CommitStore + ObjectStore>(
    store: &S,
    tag: &str,
) -> agentstategraph_core::Commit {
    use agentstategraph_core::{Authority, CommitBuilder, Intent, IntentCategory, Object};
    let obj = Object::string(tag);
    let id: ObjectId = store.put_object(&obj).unwrap();
    let commit = CommitBuilder::new(
        id,
        "agent/test",
        Authority::simple("test"),
        Intent::new(IntentCategory::Checkpoint, tag),
    )
    .build();
    store.put_commit(&commit).unwrap();
    commit
}

// Keep `RefStore` in scope via a no-op helper so the compiler doesn't
// drop the use statement even though we don't call it directly — a
// tidier alternative would be to remove the import, but we leave the
// hook for follow-up tests.
#[allow(dead_code)]
fn _ref_store_used(s: &dyn RefStore, name: &str) -> Option<ObjectId> {
    s.get_ref(name).ok().flatten()
}
