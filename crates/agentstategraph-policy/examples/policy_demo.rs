//! Policy demo — the POLICY_V1.md §22.7 scenario, runnable end-to-end.
//!
//! An autonomous agent evaluates three OpenSearch tuning options in
//! parallel via speculation:
//!
//!   A. status quo                     — score 3/10
//!   B. 1GB heap, no reindex           — score 7/10
//!   C. 1GB heap + reindex + 1 shard   — score 9/10 (technically optimal)
//!
//! A ratified `/policies/change-control/high-cost-change` policy has
//! `triggers: [reindex, migration, schema-change, destructive]` and
//! `required_fields: [estimated_downtime, rollback_plan,
//! approval_authority]`. Option C's diff contains a `"reindexed": true`
//! marker, which the MCP-layer token inference flags as `reindex`. The
//! policy's `evaluate_change` returns `RequireApproval` with a
//! `LowestRiskAlternative` fallback.
//!
//! The agent applies Option B immediately (safe fallback), records
//! Option C as a pending upgrade, and creates an approval task assigned
//! to `platform-lead`. All of it is recorded, auditable, transparent,
//! and epoch-sealable for export.
//!
//! Run with:
//!
//!     cargo run --example policy_demo -p agentstategraph-policy

use std::sync::Arc;

use agentstategraph::{CommitOptions, Repository};
use agentstategraph_core::IntentCategory;
use agentstategraph_policy::{
    ApprovalRule, ChangeProposal, Decision, FallbackAction, Policy, PolicyStore, Selector, Severity,
};
use agentstategraph_storage::MemoryStorage;
use chrono::Utc;

fn main() {
    println!("──────────────────────────────────────────────────");
    println!(" AgentStateGraph policy demo — POLICY_V1.md §22.7");
    println!("──────────────────────────────────────────────────\n");

    // ─── Repo + PolicyStore ──────────────────────────────────
    let repo = Arc::new(Repository::new(Box::new(MemoryStorage::new())));
    repo.init().expect("init repo");
    let store = PolicyStore::new(repo.clone(), "/policies", "agent/demo");

    // ─── Seed: high-cost-change policy (ratified) ────────────
    let policy = high_cost_change_policy();
    let handle = store
        .propose("main", policy)
        .expect("propose high-cost-change");
    store
        .ratify(
            "main",
            "/change-control/high-cost-change",
            "platform-lead",
            "Reviewed: matches our change-control board policy. Approved for prod.",
        )
        .expect("ratify");
    println!("Seeded + ratified: {handle}\n");

    // ─── Candidate options — three proposals ─────────────────
    // Option A: no-op. No tokens, no required fields.
    let option_a = ChangeProposal {
        action: "tune_opensearch".to_string(),
        agent_id: "agent/perf-tuner".to_string(),
        intent: "Leave heap + shard count alone (score 3/10).".to_string(),
        preferred_option: "spec/A".to_string(),
        alternatives: vec!["spec/B".to_string(), "spec/C".to_string()],
        tokens: vec![],
        attached_fields: Default::default(),
    };

    // Option B: heap only — safe.
    let option_b = ChangeProposal {
        action: "tune_opensearch".to_string(),
        agent_id: "agent/perf-tuner".to_string(),
        intent: "Bump heap to 1GB. No reindex. Score 7/10.".to_string(),
        preferred_option: "spec/B".to_string(),
        alternatives: vec!["spec/A".to_string(), "spec/C".to_string()],
        tokens: vec![],
        attached_fields: Default::default(),
    };

    // Option C: the technically optimal choice, but it reindexes.
    // The token inference that lives in the MCP server would mark this
    // with `reindex` based on a `"reindexed": true` marker in the diff.
    // Here we set it explicitly so the demo is self-contained.
    let option_c = ChangeProposal {
        action: "tune_opensearch".to_string(),
        agent_id: "agent/perf-tuner".to_string(),
        intent: "Bump heap to 1GB + consolidate to single shard with reindex. Score 9/10."
            .to_string(),
        preferred_option: "spec/C".to_string(),
        alternatives: vec!["spec/A".to_string(), "spec/B".to_string()],
        tokens: vec!["reindex".to_string(), "destructive".to_string()],
        attached_fields: Default::default(),
    };

    // ─── Evaluate each ───────────────────────────────────────
    println!("Evaluating candidates against ratified policies:\n");
    for (label, proposal) in [
        ("Option A", &option_a),
        ("Option B", &option_b),
        ("Option C", &option_c),
    ] {
        let decision = store.evaluate_change("main", proposal).expect("evaluate");
        println!("  {label:9}  — tokens {:?}", proposal.tokens);
        describe_decision(&decision);
    }
    println!();

    // ─── Fallback workflow for the winner ────────────────────
    // The agent asked for Option C (highest score). The policy returned
    // RequireApproval with LowestRiskAlternative fallback. Agent applies
    // the fallback (Option B) immediately, records Option C as pending.
    let winner_decision = store
        .evaluate_change("main", &option_c)
        .expect("evaluate winner");
    match winner_decision {
        Decision::RequireApproval {
            matched_policy,
            fallback,
            approvers,
            ..
        } => {
            println!("┌─ RequireApproval on Option C ─────────────────");
            println!("│ matched  policy: {matched_policy}");
            println!("│ approvers      : {approvers:?}");
            println!("│ fallback       : {fallback:?}");
            println!("│");
            println!("│ Agent action:");
            println!("│   1. Apply Option B immediately (the lowest-risk");
            println!("│      alternative from option_c.alternatives).");
            println!("│   2. Record Option C as a pending upgrade with");
            println!("│      parent_change = <this proposal id>.");
            println!("│   3. plan_add_task(");
            println!("│        title=\"Approve OpenSearch shard consolidation\",");
            println!("│        assigned_to=[\"platform-lead\"],");
            println!("│        blockers=[\"approval:pending\"],");
            println!("│        payload=<Option C ChangeProposal>,");
            println!("│        on_complete=OnCompleteHook::PromoteChange,");
            println!("│      )");
            println!("│   4. Complete the originating T-004 with proof:");
            println!("│      \"Applied fallback (Option B). Preferred");
            println!("│       (Option C) pending approval at <task>.\"");
            println!("└───────────────────────────────────────────────\n");
        }
        other => {
            panic!(
                "expected RequireApproval from high-cost-change policy, got {:?}",
                other
            );
        }
    }

    // ─── The thesis, in a single demo moment ─────────────────
    println!("Thesis (POLICY_V1.md §22.1):");
    println!(
        "  An AI agent that knows when to act, when to ask,\n  \
         and what to do while it waits —\n  \
         and all of it recorded, auditable, transparent,\n  \
         and sealed for export.\n"
    );

    // Bonus: sink a commit to show the policy write really is a
    // first-class ASG artefact.
    let log = repo.log("main", 10).expect("log");
    println!("ASG commits from this session:");
    for c in log.iter().rev() {
        println!(
            "  {}  [{:?}]  {}",
            c.id.short(),
            c.intent.category,
            c.intent.description
        );
    }
}

// ─────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────

fn high_cost_change_policy() -> Policy {
    Policy {
        path: "/change-control/high-cost-change".to_string(),
        version: 0,
        situation: "Any change requiring downtime, migration, or destructive operations"
            .to_string(),
        situation_selector: Selector::Always,
        allow: vec![],
        deny: vec![],
        require_approval: vec![ApprovalRule {
            action: "*".to_string(),
            approvers: vec!["platform-lead".to_string()],
            timeout: None,
            fallback: FallbackAction::LowestRiskAlternative,
        }],
        procedure: None,
        triggers: vec![
            "reindex".to_string(),
            "migration".to_string(),
            "schema-change".to_string(),
            "destructive".to_string(),
            "shard-consolidation".to_string(),
            "downtime".to_string(),
        ],
        required_fields: vec![
            "estimated_downtime".to_string(),
            "rollback_plan".to_string(),
            "approval_authority".to_string(),
        ],
        severity: Severity::High,
        proposed_by: String::new(),
        proposed_at: Utc::now(),
        ratified_by: None,
        ratified_at: None,
        ratification_reasoning: None,
        active_from: Utc::now(),
        expires_at: None,
        supersedes: None,
        signature: None,
        tenant_id: None,
    }
}

fn describe_decision(decision: &Decision) {
    match decision {
        Decision::Allow { matched_policy, .. } => {
            println!("             → Allow (matched {})", matched_policy);
        }
        Decision::Deny {
            matched_policy,
            reason,
        } => {
            println!(
                "             → Deny (matched {}) — {}",
                matched_policy, reason
            );
        }
        Decision::RequireApproval {
            matched_policy,
            fallback,
            ..
        } => {
            println!(
                "             → RequireApproval (matched {}) · fallback: {:?}",
                matched_policy, fallback
            );
        }
        Decision::NoPolicyMatch => {
            println!("             → NoPolicyMatch (fail-safe applies at the MCP layer)");
        }
    }
    // keep the demo quiet and purposeful — no CommitOptions noise
    let _ = CommitOptions::new("agent/demo", IntentCategory::Checkpoint, "noop");
}
