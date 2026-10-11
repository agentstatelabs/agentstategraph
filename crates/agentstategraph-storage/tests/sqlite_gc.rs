//! The GC grace period on SQLite (DESIGN.md finding 17). A sweep locks
//! writers out, but a write stores its objects before it commits them, so one
//! that stored them before the sweep began would find them gone. A sweep
//! spares recently written objects; storing an object again refreshes it.

#![cfg(feature = "sqlite")]

use agentstategraph_core::Object;
use agentstategraph_storage::{CommitStore, ObjectStore, SqliteStorage};

fn scratch(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "asg-sqlite-gc-{tag}-{}.db",
        uuid::Uuid::new_v4().simple()
    ))
}

fn remove(path: &std::path::Path) {
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

/// Make every object look written an hour ago.
fn age_objects(path: &std::path::Path) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute("UPDATE objects SET written_at = written_at - 3600000", [])
        .unwrap();
}

fn written_at(path: &std::path::Path, obj: &Object) -> i64 {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.query_row(
        "SELECT written_at FROM objects WHERE id = ?1",
        [obj.id().as_bytes().as_slice()],
        |row| row.get(0),
    )
    .unwrap()
}

#[test]
fn the_grace_period_spares_recent_writes() {
    let path = scratch("grace");
    let s = SqliteStorage::open(&path).unwrap();
    let garbage = s.put_object(&Object::string("unreferenced")).unwrap();

    assert_eq!(
        s.history_gc_sweep(&[], 100).unwrap().objects_deleted,
        0,
        "a just-written object is spared"
    );
    assert!(s.has_object(&garbage).unwrap());

    age_objects(&path);
    assert_eq!(s.history_gc_sweep(&[], 100).unwrap().objects_deleted, 1);
    assert!(!s.has_object(&garbage).unwrap());
    remove(&path);
}

#[test]
fn storing_an_object_again_refreshes_it() {
    let path = scratch("freshen");
    let s = SqliteStorage::open(&path).unwrap();
    let (one, two) = (Object::string("stored again"), Object::string("batched"));
    s.put_object(&one).unwrap();
    s.batch_put_objects(&[two.clone(), two.clone()]).unwrap();

    // Within a minute, storing again changes nothing.
    let first = written_at(&path, &one);
    s.put_object(&one).unwrap();
    assert_eq!(written_at(&path, &one), first);

    // An hour old and unreferenced, they would go; storing them again (as a
    // write whose new tree dedups onto them does) keeps them.
    age_objects(&path);
    s.put_object(&one).unwrap();
    s.batch_put_objects(std::slice::from_ref(&two)).unwrap();
    assert!(written_at(&path, &one) > first);
    assert_eq!(s.history_gc_sweep(&[], 100).unwrap().objects_deleted, 0);
    assert!(s.has_object(&one.id()).unwrap());
    assert!(s.has_object(&two.id()).unwrap());
    remove(&path);
}

/// A store from before write times were recorded: its objects count as
/// written long ago, so a sweep treats them as it always did.
#[test]
fn an_existing_store_gains_write_times() {
    let path = scratch("upgrade");
    let old = Object::string("from an older version");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE objects (id BLOB PRIMARY KEY, data BLOB NOT NULL);")
            .unwrap();
        conn.execute(
            "INSERT INTO objects (id, data) VALUES (?1, ?2)",
            rusqlite::params![
                old.id().as_bytes().as_slice(),
                serde_json::to_vec(&old).unwrap()
            ],
        )
        .unwrap();
    }

    let s = SqliteStorage::open(&path).unwrap();
    assert_eq!(s.get_object(&old.id()).unwrap(), Some(old.clone()));
    let fresh = s.put_object(&Object::string("new")).unwrap();
    assert_eq!(s.history_gc_sweep(&[], 100).unwrap().objects_deleted, 1);
    assert!(!s.has_object(&old.id()).unwrap());
    assert!(s.has_object(&fresh).unwrap());
    remove(&path);
}
