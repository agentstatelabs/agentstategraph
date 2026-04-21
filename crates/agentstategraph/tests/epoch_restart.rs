//! End-to-end integration test: epochs created through the high-level
//! `Repository` API survive a process-restart when backed by
//! `SqliteStorage`.
//!
//! This is the acceptance test for the 0.6.5-beta.1 compliance story —
//! a sealed epoch that vanished on restart defeated the audit bundle.

use agentstategraph::Repository;
use agentstategraph_storage::SqliteStorage;

fn scratch_path(tag: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "asg-repo-{}-{}",
        tag,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    p
}

#[test]
fn sealed_epoch_survives_restart_via_repository() {
    let db = scratch_path("epoch-restart");
    std::fs::create_dir_all(db.parent().unwrap()).ok();

    // Session 1 — create + seal an epoch through the Repository surface.
    {
        let storage = SqliteStorage::open(&db).unwrap();
        let repo = Repository::new(Box::new(storage));
        repo.init().unwrap();
        repo.create_epoch("2026-Q2-audit", "quarterly audit", vec![])
            .unwrap();
        repo.seal_epoch("2026-Q2-audit", "all checks passed")
            .unwrap();
    }

    // Session 2 — reopen the same DB, list via the Repository surface.
    {
        let storage = SqliteStorage::open(&db).unwrap();
        let repo = Repository::new(Box::new(storage));
        let list = repo.list_epochs().unwrap();
        assert_eq!(list.len(), 1, "sealed epoch must survive restart");
        let e = repo.get_epoch("2026-Q2-audit").unwrap();
        assert_eq!(e.status, agentstategraph_core::EpochStatus::Sealed);
        assert_eq!(e.seal_summary.as_deref(), Some("all checks passed"));
        assert!(e.sealed_at.is_some());
    }

    let _ = std::fs::remove_file(&db);
}

#[test]
fn active_epoch_associates_commits_via_repository() {
    use agentstategraph_core::{IntentCategory, Object};

    let storage = SqliteStorage::in_memory().unwrap();
    let repo = Repository::new(Box::new(storage));
    repo.init().unwrap();
    repo.create_epoch("e1", "scoped work", vec![]).unwrap();
    repo.set_active_epoch(Some("e1".to_string())).unwrap();

    repo.set(
        "main",
        "/x",
        &Object::string("1"),
        agentstategraph::CommitOptions::new("agent/test", IntentCategory::Checkpoint, "write"),
    )
    .unwrap();

    // Seal — subsequent commits with the active epoch must fail.
    repo.seal_epoch("e1", "sealed").unwrap();

    let err = repo.set(
        "main",
        "/y",
        &Object::string("2"),
        agentstategraph::CommitOptions::new("agent/test", IntentCategory::Checkpoint, "second"),
    );
    assert!(
        err.is_err(),
        "commit with a sealed active epoch must surface an error"
    );

    // Clearing the active epoch lets commits proceed again.
    repo.set_active_epoch(None).unwrap();
    repo.set(
        "main",
        "/z",
        &Object::string("3"),
        agentstategraph::CommitOptions::new("agent/test", IntentCategory::Checkpoint, "third"),
    )
    .unwrap();
}
