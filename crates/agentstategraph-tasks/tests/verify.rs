mod common;

use agentstategraph_tasks::{
    Priority, Proof, ProofKind, VerifyResult, Verifier,
};

use common::make_store;

struct TestVerifier;

impl Verifier for TestVerifier {
    fn verify(&self, proof: &agentstategraph_tasks::Proof) -> VerifyResult {
        match proof.kind {
            ProofKind::Commit => VerifyResult::Verified {
                message: format!("commit {} reachable", proof.value),
            },
            ProofKind::File => VerifyResult::Decayed {
                reason: format!("file {} missing", proof.value),
            },
            ProofKind::Test => VerifyResult::Verified {
                message: format!("test {} exists", proof.value),
            },
            ProofKind::Text => VerifyResult::Unverifiable {
                reason: "free-form text".to_string(),
            },
        }
    }
}

fn complete(
    store: &agentstategraph_tasks::TaskStore,
    plan: &str,
    title: &str,
    proof: Proof,
) {
    let task = store
        .add_task("main", plan, title, Priority::Medium, None, vec![], None)
        .unwrap();
    store.start_task("main", plan, &task.id).unwrap();
    store
        .complete_task("main", plan, &task.id, proof)
        .unwrap();
}

#[test]
fn verify_plan_reports_per_task_result() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();

    complete(&store, "p", "commit-one", Proof::commit("deadbeef"));
    complete(&store, "p", "file-one", Proof::file("/tmp/gone"));
    complete(&store, "p", "test-one", Proof::test("test_foo"));
    complete(&store, "p", "text-one", Proof::text("trust me"));

    let report = store.verify_plan("main", "p", &TestVerifier).unwrap();
    assert_eq!(report.results.len(), 4);
    assert_eq!(report.verified_count(), 2);
    assert_eq!(report.decayed_count(), 1);
    assert_eq!(report.unverifiable_count(), 1);
    assert!(!report.is_all_verified());
}

#[test]
fn verify_plan_skips_non_done_tasks() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();

    let a = store
        .add_task("main", "p", "a", Priority::Medium, None, vec![], None)
        .unwrap();
    store.start_task("main", "p", &a.id).unwrap();
    store
        .complete_task("main", "p", &a.id, Proof::commit("abc"))
        .unwrap();

    // Second task is pending; it should not appear in the report.
    store
        .add_task("main", "p", "b", Priority::Medium, None, vec![], None)
        .unwrap();

    let report = store.verify_plan("main", "p", &TestVerifier).unwrap();
    assert_eq!(report.results.len(), 1);
}

#[test]
fn noop_verifier_flags_everything_as_unverifiable() {
    let (_repo, store) = make_store("/plans");
    store.create_plan("main", "p", None).unwrap();
    complete(&store, "p", "x", Proof::commit("abc"));
    let report = store
        .verify_plan("main", "p", &agentstategraph_tasks::NoopVerifier)
        .unwrap();
    assert_eq!(report.unverifiable_count(), 1);
    assert_eq!(report.verified_count(), 0);
}
