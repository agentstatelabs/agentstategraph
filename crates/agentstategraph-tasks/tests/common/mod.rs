#![allow(dead_code)]
use std::sync::Arc;

use agentstategraph::Repository;
use agentstategraph_storage::SqliteStorage;
use agentstategraph_tasks::TaskStore;

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
