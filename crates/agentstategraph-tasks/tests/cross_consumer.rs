mod common;

use std::sync::Arc;

use agentstategraph::Repository;
use agentstategraph_storage::SqliteStorage;
use agentstategraph_tasks::{Priority, TaskStore};

/// Two TaskStores with different prefixes sharing one Repository must
/// not see each other's data. This is the core invariant of the
/// prefix-binding pattern — CTXone and ThreadWeaver can coexist in the
/// same repo by picking non-overlapping prefixes.
#[test]
fn two_stores_with_different_prefixes_do_not_interfere() {
    let repo = Arc::new(Repository::new(Box::new(SqliteStorage::in_memory().expect("in-memory sqlite"))));
    repo.init().unwrap();

    let plans = TaskStore::new(repo.clone(), "/plans", "ctxone-agent");
    let threads = TaskStore::new(repo.clone(), "/threads/tasks", "tw-agent");

    plans.create_plan("main", "website-v2", None).unwrap();
    threads.create_plan("main", "thread-1", None).unwrap();

    plans
        .add_task(
            "main",
            "website-v2",
            "hero",
            Priority::High,
            None,
            vec![],
            None,
        )
        .unwrap();
    threads
        .add_task(
            "main",
            "thread-1",
            "reply",
            Priority::Medium,
            None,
            vec![],
            None,
        )
        .unwrap();

    // Each store sees only its own plan.
    let plans_list = plans.list_plans("main").unwrap();
    assert_eq!(plans_list.len(), 1);
    assert_eq!(plans_list[0].name, "website-v2");

    let threads_list = threads.list_plans("main").unwrap();
    assert_eq!(threads_list.len(), 1);
    assert_eq!(threads_list[0].name, "thread-1");

    // Each store sees only its own tasks.
    assert_eq!(plans.list_tasks("main", "website-v2").unwrap().len(), 1);
    assert_eq!(threads.list_tasks("main", "thread-1").unwrap().len(), 1);

    // Cross-lookups fail cleanly.
    assert!(plans.get_plan("main", "thread-1").is_err());
    assert!(threads.get_plan("main", "website-v2").is_err());
}

#[test]
fn trailing_slash_in_prefix_is_stripped() {
    let repo = Arc::new(Repository::new(Box::new(SqliteStorage::in_memory().expect("in-memory sqlite"))));
    repo.init().unwrap();
    let store = TaskStore::new(repo, "/plans/", "agent");
    assert_eq!(store.prefix(), "/plans");
    store.create_plan("main", "p", None).unwrap();
    assert!(store.get_plan("main", "p").is_ok());
}
