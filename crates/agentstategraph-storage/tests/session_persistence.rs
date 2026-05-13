//! Integration tests for `SessionStore` across all built-in backends.
//!
//! Mirrors the structure of `epoch_persistence.rs`: round-trip,
//! restart survival on SQLite, end-of-life enforcement, and migration
//! safety is covered by `epoch_persistence::sqlite_migration_safety`.

use agentstategraph_core::{ObjectId, Session, SessionStatus};
use agentstategraph_storage::{
    CommitStore, ObjectStore, SessionStore, SqliteStorage, StorageError,
};
use chrono::Utc;

fn sample_session(id: &str) -> Session {
    Session {
        id: id.to_string(),
        agent_id: "agent/test".to_string(),
        working_branch: "agents/test/workspace".to_string(),
        head: ObjectId::hash(id.as_bytes()),
        parent_session: None,
        delegated_intent: Some("intent-1".to_string()),
        report_to: Some("agent/root".to_string()),
        path_scope: Some("/scope/test".to_string()),
        scope_tenant: None,
        scope_namespace: None,
        status: SessionStatus::Active,
        created_at: Utc::now(),
        ended_at: None,
    }
}

fn round_trip<S: SessionStore>(store: &S) {
    store.create_session(&sample_session("s1")).unwrap();
    store.create_session(&sample_session("s2")).unwrap();

    let list = store.list_sessions(None).unwrap();
    assert_eq!(list.len(), 2);
    let filtered = store.list_sessions(Some("agent/test")).unwrap();
    assert_eq!(filtered.len(), 2);
    let other = store.list_sessions(Some("agent/nope")).unwrap();
    assert!(other.is_empty());

    let s1 = store.get_session("s1").unwrap().unwrap();
    assert_eq!(s1.id, "s1");
    assert_eq!(s1.status, SessionStatus::Active);
    assert_eq!(s1.delegated_intent.as_deref(), Some("intent-1"));
    assert_eq!(s1.path_scope.as_deref(), Some("/scope/test"));

    store
        .end_session("s1", SessionStatus::Completed, Utc::now())
        .unwrap();
    let s1 = store.get_session("s1").unwrap().unwrap();
    assert_eq!(s1.status, SessionStatus::Completed);
    assert!(s1.ended_at.is_some());
}

#[test]
fn round_trip_sqlite() {
    round_trip(&SqliteStorage::in_memory().unwrap());
}

#[test]
fn sqlite_session_survives_reopen() {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "asg-session-restart-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    let db = p.join("state.db");
    {
        let s = SqliteStorage::open(&db).unwrap();
        s.create_session(&sample_session("survivor")).unwrap();
        s.end_session("survivor", SessionStatus::Abandoned, Utc::now())
            .unwrap();
    }
    let reopened = SqliteStorage::open(&db).unwrap();
    let got = reopened.get_session("survivor").unwrap().unwrap();
    assert_eq!(got.status, SessionStatus::Abandoned);
    assert!(got.ended_at.is_some());
    assert_eq!(got.delegated_intent.as_deref(), Some("intent-1"));
    let _ = std::fs::remove_file(&db);
    let _ = std::fs::remove_dir(&p);
}

fn end_enforcement<S: SessionStore + CommitStore + ObjectStore>(store: &S) {
    store.create_session(&sample_session("s")).unwrap();
    let commit = demo_commit(store);
    store.set_commit_session(&commit.id, "s").unwrap();

    store
        .end_session("s", SessionStatus::Completed, Utc::now())
        .unwrap();

    let second = demo_commit_with_tag(store, "after-end");
    let err = store.set_commit_session(&second.id, "s").unwrap_err();
    assert!(
        matches!(err, StorageError::SessionEnded { ref id } if id == "s"),
        "expected SessionEnded, got {:?}",
        err
    );

    let err = store
        .end_session("s", SessionStatus::Abandoned, Utc::now())
        .unwrap_err();
    assert!(matches!(err, StorageError::SessionEnded { .. }));
}

#[test]
fn end_enforcement_sqlite() {
    end_enforcement(&SqliteStorage::in_memory().unwrap());
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
