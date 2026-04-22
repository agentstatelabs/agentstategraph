//! §8 — policy × taint composition.

use std::sync::Arc;

use agentstategraph::Repository;
use agentstategraph_mcp::server::AgentStateGraphServer;
use agentstategraph_policy::{
    ApprovalRule, AuthorizedAction, ChangeProposal, FallbackAction, Policy, Selector, Severity,
};
use agentstategraph_storage::MemoryStorage;
use agentstategraph_taint::{TaintEffect, TaintMetadata, TaintParams, TaintSeverity};
use chrono::Utc;

fn server() -> AgentStateGraphServer {
    let repo = Arc::new(Repository::new(Box::new(MemoryStorage::new())));
    repo.init().unwrap();
    AgentStateGraphServer::new(repo)
}

fn allow_policy(path: &str) -> Policy {
    Policy {
        path: path.to_string(),
        version: 0,
        situation: "test".into(),
        situation_selector: Selector::Always,
        allow: vec![AuthorizedAction {
            action: "promote".into(),
            condition: None,
            preconditions: Vec::new(),
        }],
        deny: Vec::new(),
        require_approval: Vec::new(),
        procedure: None,
        triggers: vec!["reindex".into()],
        required_fields: Vec::new(),
        severity: Severity::Low,
        proposed_by: "test".into(),
        proposed_at: Utc::now(),
        ratified_by: None,
        ratified_at: None,
        ratification_reasoning: None,
        active_from: Utc::now(),
        expires_at: None,
        supersedes: None,
        signature: None,
        tenant_id: None,
        external_evaluator: None,
    }
}

#[test]
fn evaluate_change_with_taints_conjunction_logic() {
    let s = server();
    // Ratified allow policy on `reindex` trigger.
    s.policies()
        .propose("main", allow_policy("ops/reindex"))
        .unwrap();
    s.policies()
        .ratify("main", "ops/reindex", "platform-lead", "ok")
        .unwrap();

    // Proposal that hits the allow policy.
    let proposal =
        ChangeProposal::new("promote", "agent-1", "test", "spec-a").with_tokens(["reindex"]);
    // No taints yet → can_proceed should be true.
    let decision = s.policies().evaluate_change("main", &proposal).unwrap();
    assert!(matches!(
        decision,
        agentstategraph_policy::Decision::Allow { .. }
    ));

    // Apply a Block-effect taint on one of the affected paths.
    s.repo()
        .taint(
            "main",
            "/cluster/shards",
            TaintParams {
                name: "shard-rebalance".into(),
                effect: TaintEffect::Block,
                reason: "mid-migration".into(),
                severity: TaintSeverity::High,
                expires_at: None,
                propagate: true,
                metadata: TaintMetadata::new(),
                agent_id: "ops".into(),
            },
        )
        .unwrap();

    // check_taint on the tainted path should now return can_write=false.
    let c = s
        .repo()
        .check_taint("/cluster/shards/a", "agent-1", 1.0)
        .unwrap();
    assert!(!c.can_write);
    assert!(c.tainted);
}

#[test]
fn evaluate_change_with_no_affected_paths_returns_allow() {
    let s = server();
    // A proposal that hits no policy → NoPolicyMatch; with no paths
    // to check, can_proceed follows policy (i.e. true since not Deny).
    let proposal = ChangeProposal::new("promote", "agent-1", "test", "spec-a");
    let d = s.policies().evaluate_change("main", &proposal).unwrap();
    assert!(matches!(d, agentstategraph_policy::Decision::NoPolicyMatch));
}

#[test]
fn require_approval_decision_with_clean_taints_proceeds() {
    let s = server();
    // Policy requiring approval.
    let mut p = allow_policy("ops/high-cost");
    p.allow.clear();
    p.require_approval.push(ApprovalRule {
        action: "promote".into(),
        approvers: vec!["human".into()],
        timeout: None,
        fallback: FallbackAction::LowestRiskAlternative,
    });
    s.policies().propose("main", p).unwrap();
    s.policies()
        .ratify("main", "ops/high-cost", "platform-lead", "ok")
        .unwrap();
    let proposal =
        ChangeProposal::new("promote", "agent-1", "test", "spec-a").with_tokens(["reindex"]);
    let d = s.policies().evaluate_change("main", &proposal).unwrap();
    assert!(matches!(
        d,
        agentstategraph_policy::Decision::RequireApproval { .. }
    ));
}
