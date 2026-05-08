//! End-to-end integration test: sessions created through the
//! high-level `Repository`/`SessionManager` API survive a
//! process-restart when backed by `SqliteStorage`.

use agentstategraph::{CreateSessionParams, Repository};
use agentstategraph_core::{ObjectId, SessionStatus};
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
fn session_survives_restart_via_repository() {
    let db = scratch_path("session-restart");

    let created_id = {
        let storage = SqliteStorage::open(&db).unwrap();
        let repo = Repository::new(Box::new(storage));
        repo.init().unwrap();
        let s = repo
            .sessions()
            .create(
                "agent/planner",
                "agents/planner/workspace",
                ObjectId::hash(b"head"),
                CreateSessionParams {
                    delegated_intent: Some("intent-001".to_string()),
                    path_scope: Some("/scope".to_string()),
                    ..Default::default()
                },
            )
            .unwrap();
        s.id
    };

    {
        let storage = SqliteStorage::open(&db).unwrap();
        let repo = Repository::new(Box::new(storage));
        let list = repo.sessions().list(None).unwrap();
        assert_eq!(list.len(), 1, "session must survive restart");
        let got = repo.sessions().get(&created_id).unwrap().unwrap();
        assert_eq!(got.agent_id, "agent/planner");
        assert_eq!(got.status, SessionStatus::Active);
        assert_eq!(got.delegated_intent.as_deref(), Some("intent-001"));
        assert_eq!(got.path_scope.as_deref(), Some("/scope"));
    }

    let _ = std::fs::remove_file(&db);
}
