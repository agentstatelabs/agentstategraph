//! Consumer-shaped integration test.
//!
//! Simulates a realistic workflow — a miniature "CTXone-like" consumer
//! that tracks a website rewrite plan, uses a stub git/filesystem
//! verifier, and queries blame + log at the end to answer
//! accountability questions. The test exists so we catch API-shape
//! regressions from the *consumer's* point of view before CTXone or
//! ThreadWeaver do.

mod common;

use std::collections::HashSet;
use std::sync::Arc;

use agentstategraph::{PathPattern, Repository};
use agentstategraph_core::IntentCategory;
use agentstategraph_storage::SqliteStorage;
use agentstategraph_tasks::{Priority, Proof, ProofKind, TaskStore, Verifier, VerifyResult};

/// A stand-in for CTXone's `GitFileTestVerifier`. Backed by in-memory
/// sets rather than a real repo/filesystem — the point is to show the
/// shape works end-to-end.
struct StubGitFileVerifier {
    reachable_commits: HashSet<String>,
    existing_files: HashSet<String>,
    known_tests: HashSet<String>,
}

impl Verifier for StubGitFileVerifier {
    fn verify(&self, proof: &Proof) -> VerifyResult {
        match proof.kind {
            ProofKind::Commit => {
                if self.reachable_commits.contains(&proof.value) {
                    VerifyResult::Verified {
                        message: format!("commit {} reachable from HEAD", proof.value),
                    }
                } else {
                    VerifyResult::Decayed {
                        reason: format!("commit {} no longer reachable", proof.value),
                    }
                }
            }
            ProofKind::File => {
                if self.existing_files.contains(&proof.value) {
                    VerifyResult::Verified {
                        message: format!("file {} exists", proof.value),
                    }
                } else {
                    VerifyResult::Decayed {
                        reason: format!("file {} not on disk", proof.value),
                    }
                }
            }
            ProofKind::Test => {
                if self.known_tests.contains(&proof.value) {
                    VerifyResult::Verified {
                        message: format!("test {} registered", proof.value),
                    }
                } else {
                    VerifyResult::Decayed {
                        reason: format!("test {} missing from suite", proof.value),
                    }
                }
            }
            ProofKind::Text => VerifyResult::Unverifiable {
                reason: "text proofs require human review".to_string(),
            },
        }
    }
}

#[test]
fn full_consumer_workflow() {
    let repo = Arc::new(Repository::new(Box::new(SqliteStorage::in_memory().expect("in-memory sqlite"))));
    repo.init().unwrap();

    let store = TaskStore::new(repo.clone(), "/plans", "claude-code");

    // --- Subscribe to plan activity before any writes happen. --------
    // (Watch-dispatch wiring inside Repository is a 0.4.0 roadmap item;
    // for now this just exercises the pattern a consumer will use.)
    let _watch = repo
        .watches()
        .subscribe(PathPattern::Prefix("/plans/website-v2/".to_string()));

    // --- Create a plan, seed tasks with a dependency. ----------------
    store
        .create_plan("main", "website-v2", Some("Brand pivot".into()))
        .unwrap();

    let design = store
        .add_task(
            "main",
            "website-v2",
            "Finalise brand palette",
            Priority::Critical,
            None,
            vec![],
            None,
        )
        .unwrap();
    let hero = store
        .add_task(
            "main",
            "website-v2",
            "Rewrite hero copy",
            Priority::High,
            None,
            vec![design.id.clone()],
            None,
        )
        .unwrap();
    let nav = store
        .add_task(
            "main",
            "website-v2",
            "Update nav",
            Priority::Medium,
            None,
            vec![],
            None,
        )
        .unwrap();

    // --- Walk the plan via next_task. --------------------------------
    // design is Critical, nav is Medium, hero is blocked → design first.
    let next = store.next_task("main", "website-v2").unwrap().unwrap();
    assert_eq!(next.id, design.id);

    store.start_task("main", "website-v2", &design.id).unwrap();
    store
        .complete_task(
            "main",
            "website-v2",
            &design.id,
            Proof::commit("deadbeef1234"),
        )
        .unwrap();

    // Now hero unblocks. It is High > nav's Medium.
    let next = store.next_task("main", "website-v2").unwrap().unwrap();
    assert_eq!(next.id, hero.id);

    store.start_task("main", "website-v2", &hero.id).unwrap();
    store
        .complete_task(
            "main",
            "website-v2",
            &hero.id,
            Proof::file("site/src/components/Hero.astro"),
        )
        .unwrap();

    // Abandon nav — deprioritised by the user.
    store
        .abandon_task("main", "website-v2", &nav.id, "scoped out of v2")
        .unwrap();

    // --- Verify the plan. --------------------------------------------
    let verifier = StubGitFileVerifier {
        reachable_commits: ["deadbeef1234".to_string()].into_iter().collect(),
        existing_files: ["site/src/components/Hero.astro".to_string()]
            .into_iter()
            .collect(),
        known_tests: HashSet::new(),
    };
    let report = store.verify_plan("main", "website-v2", &verifier).unwrap();
    assert_eq!(report.results.len(), 2);
    assert_eq!(report.verified_count(), 2);
    assert!(report.all_strongly_verified());

    // Decay the design proof and re-verify — we should see one Decayed.
    let verifier = StubGitFileVerifier {
        reachable_commits: HashSet::new(),
        existing_files: ["site/src/components/Hero.astro".to_string()]
            .into_iter()
            .collect(),
        known_tests: HashSet::new(),
    };
    let report = store.verify_plan("main", "website-v2", &verifier).unwrap();
    assert_eq!(report.verified_count(), 1);
    assert_eq!(report.decayed_count(), 1);
    assert!(!report.all_strongly_verified());

    // --- Accountability: blame, log filter, watch events. ------------
    // Every commit on the plan path should carry IntentCategory::Plan.
    let commits = repo.log("main", 100).unwrap();
    let plan_commits: Vec<_> = commits
        .iter()
        .filter(|c| c.intent.category == IntentCategory::Plan)
        .collect();
    // create_plan + 3 add_task + start + complete + start + complete + abandon = 9.
    assert_eq!(plan_commits.len(), 9);

    // Blame for the design task id should point to an agent/intent pair.
    let blame = repo.blame("main", "/plans/website-v2/t-001").unwrap();
    assert_eq!(blame.agent_id, "claude-code");
    assert!(blame.intent_category.contains("Plan"));

    // --- Plan should now be Completed since all tasks are terminal. --
    let plan = store.get_plan("main", "website-v2").unwrap();
    assert_eq!(
        plan.status,
        agentstategraph_tasks::PlanStatus::Completed,
        "plan should auto-complete when every task is terminal"
    );
}
