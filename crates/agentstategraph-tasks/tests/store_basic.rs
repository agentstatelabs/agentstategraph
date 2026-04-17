mod common;

use agentstategraph_tasks::{PlanStatus, Priority, TaskStatus, TaskStoreError};

use common::make_store;

#[test]
fn create_and_fetch_plan() {
    let (_repo, store) = make_store("/plans");
    let plan = store
        .create_plan("main", "website-v2", Some("Brand pivot".into()))
        .unwrap();
    assert_eq!(plan.name, "website-v2");
    assert_eq!(plan.status, PlanStatus::Active);
    assert_eq!(plan.created_by, "test-agent");

    let fetched = store.get_plan("main", "website-v2").unwrap();
    assert_eq!(fetched, plan);
}

#[test]
fn create_plan_twice_errors() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();
    let err = store.create_plan("main", "p", None).unwrap_err();
    assert!(matches!(err, TaskStoreError::PlanAlreadyExists(_)));
}

#[test]
fn list_plans_returns_all() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "alpha", None).unwrap();
    store.create_plan("main", "beta", None).unwrap();
    store.create_plan("main", "gamma", None).unwrap();
    let plans = store.list_plans("main").unwrap();
    assert_eq!(plans.len(), 3);
    let names: Vec<_> = plans.iter().map(|p| p.name.as_str()).collect();
    assert!(names.contains(&"alpha"));
    assert!(names.contains(&"beta"));
    assert!(names.contains(&"gamma"));
}

#[test]
fn list_plans_empty_when_unused() {
    let (_repo, store) = make_store("/plans");
    let plans = store.list_plans("main").unwrap();
    assert!(plans.is_empty());
}

#[test]
fn add_task_assigns_monotonic_ids() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();
    let t1 = store
        .add_task("main", "p", "A", Priority::Medium, None, vec![], None)
        .unwrap();
    let t2 = store
        .add_task("main", "p", "B", Priority::Medium, None, vec![], None)
        .unwrap();
    let t3 = store
        .add_task("main", "p", "C", Priority::Medium, None, vec![], None)
        .unwrap();
    assert_eq!(t1.id.as_str(), "t-001");
    assert_eq!(t2.id.as_str(), "t-002");
    assert_eq!(t3.id.as_str(), "t-003");
}

#[test]
fn list_tasks_sorted_and_excludes_meta() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();
    store
        .add_task("main", "p", "first", Priority::Low, None, vec![], None)
        .unwrap();
    store
        .add_task("main", "p", "second", Priority::High, None, vec![], None)
        .unwrap();
    let tasks = store.list_tasks("main", "p").unwrap();
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0].id.as_str(), "t-001");
    assert_eq!(tasks[1].id.as_str(), "t-002");
}

#[test]
fn get_task_missing_returns_not_found() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();
    let err = store
        .get_task("main", "p", &agentstategraph_tasks::TaskId::new(99))
        .unwrap_err();
    assert!(matches!(err, TaskStoreError::TaskNotFound { .. }));
}

#[test]
fn archive_plan_sets_status_archived() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();
    let plan = store.archive_plan("main", "p").unwrap();
    assert_eq!(plan.status, PlanStatus::Archived);
    assert!(plan.archived_at.is_some());
}

#[test]
fn delete_plan_removes_tasks() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();
    store
        .add_task("main", "p", "x", Priority::Medium, None, vec![], None)
        .unwrap();
    store.delete_plan("main", "p").unwrap();
    let err = store.get_plan("main", "p").unwrap_err();
    assert!(matches!(err, TaskStoreError::PlanNotFound(_)));
}

#[test]
fn add_task_to_missing_plan_errors() {
    let (_repo, store) = make_store("/plans");
    let err = store
        .add_task("main", "nope", "x", Priority::Medium, None, vec![], None)
        .unwrap_err();
    assert!(matches!(err, TaskStoreError::PlanNotFound(_)));
}

#[test]
fn set_priority_persists() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();
    let task = store
        .add_task("main", "p", "x", Priority::Low, None, vec![], None)
        .unwrap();
    store
        .set_priority("main", "p", &task.id, Priority::Critical)
        .unwrap();
    let fetched = store.get_task("main", "p", &task.id).unwrap();
    assert_eq!(fetched.priority, Priority::Critical);
    assert_eq!(fetched.status, TaskStatus::Pending);
}
