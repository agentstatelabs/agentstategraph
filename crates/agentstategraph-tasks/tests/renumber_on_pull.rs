//! Tasks added to one plan on two stores apart land with distinct ids
//! after a pull, with their references intact (DESIGN.md Plan G t-010).

mod common;

use std::sync::Arc;

use agentstategraph::sync::{InProcessRemote, tracking_ref};
use agentstategraph::{CommitOptions, Repository};
use agentstategraph_core::IntentCategory;
use agentstategraph_storage::SqliteStorage;
use agentstategraph_tasks::{Priority, Renumbering, TaskId, TaskStore};

fn tasks(repo: &Arc<Repository>, agent: &str) -> TaskStore {
    TaskStore::new(repo.clone(), "/plans", agent)
}

fn empty_hub() -> Repository {
    Repository::new(Box::new(SqliteStorage::in_memory().unwrap()))
}

/// A pull that renumbers colliding tasks before it merges.
fn pull(repo: &Repository, store: &TaskStore, hub: &Repository) -> Renumbering {
    let remote = InProcessRemote::new(hub);
    repo.fetch("hub", &remote, &["main"]).unwrap();
    let renumbered = store
        .renumber_before_merge("main", &tracking_ref("hub", "main"))
        .unwrap();
    repo.merge_fetched(
        "hub",
        "main",
        CommitOptions::new("agent/test", IntentCategory::Merge, "pull"),
    )
    .unwrap();
    renumbered
}

fn push(repo: &Repository, hub: &Repository) {
    repo.push("hub", &InProcessRemote::new(hub), "main")
        .unwrap();
}

fn add(store: &TaskStore, title: &str, blocked_by: Vec<TaskId>) -> TaskId {
    store
        .add_task("main", "p", title, Priority::Medium, None, blocked_by, None)
        .unwrap()
        .id
}

fn titles(store: &TaskStore) -> Vec<(String, String, Vec<String>)> {
    store
        .list_tasks("main", "p")
        .unwrap()
        .into_iter()
        .map(|t| {
            let blockers = t.blocked_by.iter().map(|b| b.to_string()).collect();
            (t.id.to_string(), t.title, blockers)
        })
        .collect()
}

/// Two stores sharing plan `p` with one task.
fn two_stores() -> (
    Repository,
    Arc<Repository>,
    TaskStore,
    Arc<Repository>,
    TaskStore,
) {
    let hub = empty_hub();
    let a = common::make_repo();
    let b = common::make_repo();
    let (ta, tb) = (tasks(&a, "agent/a"), tasks(&b, "agent/b"));
    ta.create_plan("main", "p", None).unwrap();
    add(&ta, "first", vec![]);
    push(&a, &hub);
    assert!(pull(&b, &tb, &hub).is_empty());
    (hub, a, ta, b, tb)
}

#[test]
fn tasks_added_apart_land_with_distinct_ids_and_their_references_intact() {
    let (hub, a, ta, b, tb) = two_stores();
    assert_eq!(add(&ta, "a's second", vec![]), TaskId::new(2));
    let b2 = add(&tb, "b's second", vec![]);
    assert_eq!(b2, TaskId::new(2), "the same id, minted apart");
    add(&tb, "b's third", vec![b2]);
    push(&a, &hub);
    assert!(
        b.push("hub", &InProcessRemote::new(&hub), "main").is_err(),
        "b is behind"
    );

    let renumbered = pull(&b, &tb, &hub);
    assert_eq!(
        renumbered.renamed["p"],
        vec![(TaskId::new(2), TaskId::new(4))]
    );
    assert_eq!(renumbered.commits.len(), 1);
    let expected = vec![
        ("t-001".to_string(), "first".to_string(), vec![]),
        ("t-002".to_string(), "a's second".to_string(), vec![]),
        (
            "t-003".to_string(),
            "b's third".to_string(),
            vec!["t-004".to_string()],
        ),
        ("t-004".to_string(), "b's second".to_string(), vec![]),
    ];
    assert_eq!(titles(&tb), expected);
    push(&b, &hub);

    assert!(pull(&a, &ta, &hub).is_empty(), "a had nothing of its own");
    assert_eq!(titles(&ta), expected);
    assert_eq!(add(&ta, "next", vec![]), TaskId::new(5));
}

#[test]
fn a_task_already_shared_keeps_its_id() {
    let (hub, a, ta, b, tb) = two_stores();
    // t-001 was shared; b edits it and adds t-002, a adds its own t-002.
    tb.set_title("main", "p", &TaskId::new(1), "first, renamed")
        .unwrap();
    add(&tb, "b's", vec![TaskId::new(1)]);
    add(&ta, "a's", vec![]);
    push(&a, &hub);

    let renumbered = pull(&b, &tb, &hub);
    assert_eq!(
        renumbered.renamed["p"],
        vec![(TaskId::new(2), TaskId::new(3))]
    );
    let got = titles(&tb);
    assert_eq!(got[0].1, "first, renamed", "the shared task kept its id");
    assert_eq!(got[2], ("t-003".into(), "b's".into(), vec!["t-001".into()]));
}

#[test]
fn a_plan_both_sides_created_apart_is_reported() {
    let (hub, a, ta, b, tb) = two_stores();
    ta.create_plan("main", "q", None).unwrap();
    tb.create_plan("main", "q", None).unwrap();
    push(&a, &hub);
    b.fetch("hub", &InProcessRemote::new(&hub), &["main"])
        .unwrap();
    let renumbered = tb
        .renumber_before_merge("main", &tracking_ref("hub", "main"))
        .unwrap();
    assert_eq!(renumbered.plan_conflicts, vec!["q".to_string()]);
    assert!(renumbered.renamed.is_empty());
}
