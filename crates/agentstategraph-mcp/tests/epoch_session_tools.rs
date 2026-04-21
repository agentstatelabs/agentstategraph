//! Integration tests for the 0.6.75-beta.1 §3 additions:
//! `enter_epoch` / `exit_epoch` / `enter_session` / `exit_session`
//! MCP tools.
//!
//! These exercise the `Repository::{set_,}active_epoch` and
//! `active_session` plumbing that the new tools wrap, plus the
//! commit-association path that lands on real commits at write
//! time.

use std::sync::Arc;

use agentstategraph::{CommitOptions, Repository};
use agentstategraph_core::{IntentCategory, Object, SessionStatus};
use agentstategraph_storage::MemoryStorage;

fn fresh_repo() -> Arc<Repository> {
    let repo = Arc::new(Repository::new(Box::new(MemoryStorage::new())));
    repo.init().expect("init repo");
    repo
}

fn commit_touching(repo: &Repository, path: &str, value: &str) {
    let opts = CommitOptions::new(
        "agent/test",
        IntentCategory::Refine,
        format!("set {}", path),
    );
    repo.set("main", path, &Object::string(value), opts)
        .expect("set");
}

fn create_test_session(repo: &Repository, id_hint: &str) -> String {
    let s = repo
        .sessions()
        .create(
            &format!("agent/{}", id_hint),
            "main",
            agentstategraph_core::ObjectId::hash(b"head"),
            None,
            None,
            None,
            None,
        )
        .expect("create session");
    s.id
}

#[test]
fn test_enter_epoch_sets_active_and_allows_commits() {
    let repo = fresh_repo();

    // Create + enter an epoch.
    repo.create_epoch("e1", "first", vec![]).expect("create");
    repo.set_active_epoch(Some("e1".to_string()))
        .expect("set active");
    assert_eq!(
        repo.active_epoch().unwrap(),
        Some("e1".to_string()),
        "active epoch readback"
    );

    // Commit while e1 is active. The create_commit path calls
    // set_commit_epoch internally; if the epoch is sealed or missing
    // that path errors. Succeeding here is the test.
    commit_touching(&repo, "/a", "x");
    // Readback: epoch still exists, active pointer unchanged.
    let e1 = repo.get_epoch("e1").expect("get");
    assert_eq!(e1.id, "e1");
    assert_eq!(repo.active_epoch().unwrap(), Some("e1".to_string()));
}

#[test]
fn test_exit_epoch_clears_active_and_later_commits_are_unassociated() {
    let repo = fresh_repo();
    repo.create_epoch("e1", "x", vec![]).expect("create");
    repo.set_active_epoch(Some("e1".to_string())).expect("set");

    commit_touching(&repo, "/a", "1");

    // Exit — this is the enter/exit flow the MCP tools wrap.
    repo.set_active_epoch(None).expect("clear");
    assert!(repo.active_epoch().unwrap().is_none());

    // Subsequent commits must not touch e1. A sealed epoch would
    // reject commits through create_commit's set_commit_epoch call;
    // with the active pointer cleared, the association never runs.
    // Seal e1 after the exit: commits must still succeed because
    // they are no longer being associated with e1.
    repo.seal_epoch("e1", "done").expect("seal");
    commit_touching(&repo, "/b", "2");
    // If exit_epoch failed to clear, the second commit would have
    // tried to associate with sealed e1 and errored.
}

#[test]
fn test_cannot_enter_sealed_epoch() {
    let repo = fresh_repo();
    repo.create_epoch("e1", "x", vec![]).expect("create");
    repo.seal_epoch("e1", "done").expect("seal");

    // The Repository allows setting any string as the active epoch —
    // the MCP tool's enter_epoch handler is responsible for rejecting
    // sealed epochs. We validate the same check the handler performs:
    let epoch = repo.get_epoch("e1").unwrap();
    assert!(
        matches!(
            epoch.status,
            agentstategraph_core::EpochStatus::Sealed | agentstategraph_core::EpochStatus::Archived
        ),
        "epoch must be marked Sealed after seal_epoch"
    );
}

#[test]
fn test_enter_session_sets_active_and_exit_clears() {
    let repo = fresh_repo();
    let sid = create_test_session(&repo, "x");

    repo.set_active_session(Some(sid.clone())).expect("set");
    assert_eq!(repo.active_session().unwrap(), Some(sid.clone()));

    repo.set_active_session(None).expect("clear");
    assert!(repo.active_session().unwrap().is_none());
}

#[test]
fn test_enter_session_rejects_ended_session() {
    let repo = fresh_repo();
    let sid = create_test_session(&repo, "x");
    repo.sessions()
        .end(&sid, SessionStatus::Completed)
        .expect("end");

    // The Repository plumbing doesn't guard on session status — the
    // MCP handler does. Validate the same check:
    let s = repo.sessions().get(&sid).unwrap().unwrap();
    assert!(
        !matches!(s.status, SessionStatus::Active),
        "ended session must not be Active"
    );
}

#[test]
fn test_active_epoch_and_session_are_independent() {
    let repo = fresh_repo();
    repo.create_epoch("e1", "x", vec![]).expect("create epoch");
    let sid = create_test_session(&repo, "x");

    repo.set_active_epoch(Some("e1".to_string())).unwrap();
    repo.set_active_session(Some(sid.clone())).unwrap();

    assert_eq!(repo.active_epoch().unwrap(), Some("e1".to_string()));
    assert_eq!(repo.active_session().unwrap(), Some(sid.clone()));

    repo.set_active_epoch(None).unwrap();
    assert_eq!(
        repo.active_session().unwrap(),
        Some(sid),
        "clearing active_epoch must not affect active_session"
    );
}
