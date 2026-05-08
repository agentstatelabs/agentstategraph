#![allow(dead_code)]
use std::sync::Arc;

use agentstategraph::Repository;
use agentstategraph_storage::SqliteStorage;
use agentstategraph_tasks::{Priority, Task, TaskId, TaskStatus, TaskStore};

pub fn make_repo() -> Arc<Repository> {
    let repo = Arc::new(Repository::new(Box::new(
        SqliteStorage::in_memory().expect("in-memory sqlite"),
    )));
    repo.init().unwrap();
    repo
}

pub fn make_store(prefix: &str) -> (Arc<Repository>, TaskStore) {
    let repo = make_repo();
    let store = TaskStore::new(repo.clone(), prefix, "test-agent");
    (repo, store)
}

/// Build a minimal Task for tests. Optional/new fields default to None/Pending.
/// Update this helper when Task gains fields so call sites stay stable.
pub fn make_task(id: &str, title: &str) -> Task {
    Task {
        id: TaskId(id.to_string()),
        title: title.to_string(),
        status: TaskStatus::Pending,
        priority: Priority::Medium,
        parent_id: None,
        blocked_by: vec![],
        created_at: chrono::Utc::now(),
        created_by: "test-agent".to_string(),
        started_at: None,
        started_by: None,
        completed_at: None,
        completed_by: None,
        proof: None,
        abandoned_at: None,
        abandoned_reason: None,
        assigned_to: None,
        payload: None,
        parent_change: None,
        on_complete: None,
    }
}
