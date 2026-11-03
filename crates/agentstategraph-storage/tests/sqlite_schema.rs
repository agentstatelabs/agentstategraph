//! The SQLite schema stamp (DESIGN.md Plan F t-009): a build refuses a file a
//! newer build stamped as needing more than it understands, and otherwise
//! uses a newer build's file without lowering its stamp.

#![cfg(feature = "sqlite")]

use agentstategraph_core::{Namespace, Object};
use agentstategraph_storage::{ObjectStore, RefStore, SqliteStorage, StorageError};
use rusqlite::Connection;

fn stamp(path: &std::path::Path) -> (i64, i64) {
    Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT version, compatible_from FROM asg_schema WHERE component = 'sqlite'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}

fn set_stamp(path: &std::path::Path, version: i64, compatible_from: i64) {
    Connection::open(path)
        .unwrap()
        .execute(
            "UPDATE asg_schema SET version = ?1, compatible_from = ?2 WHERE component = 'sqlite'",
            rusqlite::params![version, compatible_from],
        )
        .unwrap();
}

#[test]
fn a_new_file_is_stamped_and_an_unstamped_one_is_stamped_on_open() {
    let dir = tempdir();
    let path = dir.join("store.db");
    drop(SqliteStorage::open(&path).unwrap());
    assert_eq!(stamp(&path), (1, 1));

    // A file from before the stamp: every build understands it.
    Connection::open(&path)
        .unwrap()
        .execute_batch("DROP TABLE asg_schema;")
        .unwrap();
    drop(SqliteStorage::open(&path).unwrap());
    assert_eq!(stamp(&path), (1, 1));
}

#[test]
fn a_newer_compatible_file_opens_and_keeps_its_stamp() {
    let dir = tempdir();
    let path = dir.join("store.db");
    let storage = SqliteStorage::open(&path).unwrap();
    let id = storage.put_object(&Object::string("kept")).unwrap();
    drop(storage);

    // A newer build expanded the schema; this one can still use it.
    set_stamp(&path, 7, 1);
    let storage = SqliteStorage::open(&path).unwrap();
    assert_eq!(
        storage.get_object(&id).unwrap(),
        Some(Object::string("kept"))
    );
    storage
        .set_ref(&Namespace::default_ns(), "main", id)
        .unwrap();
    drop(storage);
    assert_eq!(stamp(&path), (7, 1), "an older build lowers neither number");
}

#[test]
fn a_file_needing_a_newer_build_is_refused_and_left_alone() {
    let dir = tempdir();
    let path = dir.join("store.db");
    drop(SqliteStorage::open(&path).unwrap());
    set_stamp(&path, 9, 8);
    // A table this build would add if it migrated. It mustn't come back.
    Connection::open(&path)
        .unwrap()
        .execute_batch("DROP TABLE commit_traces;")
        .unwrap();

    let err = SqliteStorage::open(&path).err().expect("refused");
    match &err {
        StorageError::SchemaTooNew {
            backend,
            found,
            compatible_from,
            supported,
        } => {
            assert_eq!(*backend, "sqlite");
            assert_eq!((*found, *compatible_from, *supported), (9, 8, 1));
        }
        other => panic!("expected SchemaTooNew, got {other:?}"),
    }
    assert_eq!(err.code(), "schema_too_new");
    assert!(err.to_string().contains("Upgrade AgentStateGraph"), "{err}");

    let traces: bool = Connection::open(&path)
        .unwrap()
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE name = 'commit_traces')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!traces, "a refused file must not be migrated");
    assert_eq!(stamp(&path), (9, 8));
}

/// A fresh directory under the system temp dir.
fn tempdir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "asg-sqlite-schema-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
