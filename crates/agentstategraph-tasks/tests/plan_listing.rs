//! Listing plans and their tasks reads the plans tree once: what each call
//! returns, and the not-found cases `list_tasks` keeps.

mod common;

use agentstategraph_tasks::{Priority, TaskStoreError};

use common::make_store;

fn store_with_plans() -> agentstategraph_tasks::TaskStore {
    let (_repo, store) = make_store("/plans");
    for (plan, tasks) in [("alpha", 3), ("beta", 0), ("gamma", 12)] {
        store
            .create_plan("main", plan, Some(format!("{plan} plan")))
            .unwrap();
        for i in 0..tasks {
            store
                .add_task(
                    "main",
                    plan,
                    &format!("{plan} task {i}"),
                    Priority::Medium,
                    None,
                    vec![],
                    None,
                )
                .unwrap();
        }
    }
    store
}

#[test]
fn list_plans_with_tasks_matches_the_per_plan_calls() {
    let store = store_with_plans();
    let together = store.list_plans_with_tasks("main").unwrap();
    let names: Vec<_> = together.iter().map(|(p, _)| p.name.as_str()).collect();
    assert_eq!(names, ["alpha", "beta", "gamma"]);
    for (plan, tasks) in &together {
        assert_eq!(plan, &store.get_plan("main", &plan.name).unwrap());
        assert_eq!(tasks, &store.list_tasks("main", &plan.name).unwrap());
    }
    let counts: Vec<_> = together.iter().map(|(_, t)| t.len()).collect();
    assert_eq!(counts, [3, 0, 12]);
    // Sorted by id, as list_tasks has always returned them.
    let gamma = &together[2].1;
    assert!(gamma.windows(2).all(|w| w[0].id <= w[1].id));
}

#[test]
fn list_plans_matches_get_plan() {
    let store = store_with_plans();
    let plans = store.list_plans("main").unwrap();
    assert_eq!(plans.len(), 3);
    for plan in &plans {
        assert_eq!(plan, &store.get_plan("main", &plan.name).unwrap());
    }
}

#[test]
fn listing_an_empty_store_is_empty() {
    let (_repo, store) = make_store("/plans");
    assert!(store.list_plans("main").unwrap().is_empty());
    assert!(store.list_plans_with_tasks("main").unwrap().is_empty());
}

#[test]
fn list_tasks_of_a_missing_plan_is_plan_not_found() {
    let store = store_with_plans();
    let err = store.list_tasks("main", "nope").unwrap_err();
    assert!(
        matches!(err, TaskStoreError::PlanNotFound(ref p) if p == "nope"),
        "{err:?}"
    );
}
