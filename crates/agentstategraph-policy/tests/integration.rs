//! Integration tests for `agentstategraph-policy`.
//!
//! Covers POLICY_V1.md §14.1 + the scenarios enumerated in the Phase 1
//! implementation plan §2.8.

use std::sync::Arc;
use std::time::Duration;

use agentstategraph::Repository;
use agentstategraph_storage::MemoryStorage;

use agentstategraph_policy::{
    ApprovalRule, AuthorizedAction, ChangeProposal, Decision, FallbackAction, Policy, PolicyError,
    PolicyStore, ProcedureStep, Selector, Severity, Situation,
};
use chrono::Utc;

const REF: &str = "main";

fn make_store(prefix: &str) -> (Arc<Repository>, PolicyStore) {
    let repo = Arc::new(Repository::new(Box::new(MemoryStorage::new())));
    repo.init().unwrap();
    let store = PolicyStore::new(repo.clone(), prefix, "test-agent");
    (repo, store)
}

fn skeleton(path: &str, selector: Selector) -> Policy {
    Policy {
        path: path.into(),
        version: 0,
        situation: "test".into(),
        situation_selector: selector,
        allow: vec![],
        deny: vec![],
        require_approval: vec![],
        procedure: None,
        triggers: vec![],
        required_fields: vec![],
        severity: Severity::Low,
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

fn allow_action(action: &str, preconds: Vec<String>) -> AuthorizedAction {
    AuthorizedAction {
        action: action.into(),
        condition: None,
        preconditions: preconds,
    }
}

fn deny_action(action: &str, reason: &str) -> AuthorizedAction {
    AuthorizedAction {
        action: action.into(),
        condition: Some(reason.into()),
        preconditions: vec![],
    }
}

fn approval(action: &str, fallback: FallbackAction) -> ApprovalRule {
    ApprovalRule {
        action: action.into(),
        approvers: vec!["human".into()],
        timeout: Some(Duration::from_secs(3600)),
        fallback,
    }
}

// -----------------------------------------------------------------------
// Storage roundtrip + propose/ratify/supersede
// -----------------------------------------------------------------------

#[test]
fn test_policy_roundtrip() {
    let (_r, store) = make_store("/policies");
    let mut p = skeleton("infra/k8s/pod-failing", Selector::Always);
    p.allow = vec![allow_action("restart_pod", vec!["investigate_logs".into()])];
    p.procedure = Some(vec![ProcedureStep {
        action: "investigate_logs".into(),
        if_previous_failed: None,
    }]);

    let handle = store.propose(REF, p.clone()).unwrap();
    assert_eq!(handle, "infra/k8s/pod-failing@1");

    let loaded = store.get(REF, "infra/k8s/pod-failing", None).unwrap();
    assert_eq!(loaded.path, "infra/k8s/pod-failing");
    assert_eq!(loaded.version, 1);
    assert_eq!(loaded.allow.len(), 1);
    assert_eq!(
        loaded.allow[0].preconditions,
        vec!["investigate_logs".to_string()]
    );
    assert_eq!(loaded.proposed_by, "test-agent");
    assert!(!loaded.is_ratified());
}

#[test]
fn test_propose_duplicate_errors() {
    let (_r, store) = make_store("/policies");
    let p = skeleton("infra/x", Selector::Always);
    store.propose(REF, p.clone()).unwrap();
    let err = store.propose(REF, p).unwrap_err();
    assert!(matches!(err, PolicyError::AlreadyExists(_)));
}

#[test]
fn test_propose_rejects_invalid_path() {
    let (_r, store) = make_store("/policies");
    let p = skeleton("Infra/../etc", Selector::Always);
    let err = store.propose(REF, p).unwrap_err();
    assert!(matches!(err, PolicyError::InvalidPath(_)));
}

#[test]
fn test_ratify_flips_state_and_attributes_caller() {
    let (_r, store) = make_store("/policies");
    let p = skeleton("infra/ratify", Selector::Always);
    store.propose(REF, p).unwrap();
    store
        .ratify(REF, "infra/ratify", "alice", "looks correct")
        .unwrap();
    let p = store.get(REF, "infra/ratify", None).unwrap();
    assert_eq!(p.ratified_by.as_deref(), Some("alice"));
    assert_eq!(p.ratification_reasoning.as_deref(), Some("looks correct"));
    assert!(p.ratified_at.is_some());
}

#[test]
fn test_ratify_rejects_empty_ratifier() {
    let (_r, store) = make_store("/policies");
    store
        .propose(REF, skeleton("infra/rat2", Selector::Always))
        .unwrap();
    let err = store.ratify(REF, "infra/rat2", "   ", "x").unwrap_err();
    assert!(matches!(err, PolicyError::Invalid(_)));
}

#[test]
fn test_ratify_twice_errors() {
    let (_r, store) = make_store("/policies");
    store
        .propose(REF, skeleton("infra/r3", Selector::Always))
        .unwrap();
    store.ratify(REF, "infra/r3", "alice", "ok").unwrap();
    let err = store.ratify(REF, "infra/r3", "alice", "again").unwrap_err();
    assert!(matches!(err, PolicyError::AlreadyRatified(_)));
}

#[test]
fn test_ratify_missing_errors() {
    let (_r, store) = make_store("/policies");
    let err = store.ratify(REF, "infra/nope", "alice", "x").unwrap_err();
    assert!(matches!(err, PolicyError::NotFound(_)));
}

#[test]
fn test_supersede_creates_new_version_and_links_prior() {
    let (_r, store) = make_store("/policies");
    let mut p = skeleton("infra/sup", Selector::Always);
    p.allow = vec![allow_action("x", vec![])];
    store.propose(REF, p).unwrap();
    store.ratify(REF, "infra/sup", "alice", "ok").unwrap();

    let mut newp = skeleton("infra/sup", Selector::Always);
    newp.allow = vec![allow_action("y", vec![])];
    newp.ratified_by = Some("alice".into());
    newp.ratified_at = Some(Utc::now());
    let handle = store.supersede(REF, "infra/sup", newp).unwrap();
    assert_eq!(handle, "infra/sup@2");

    let active = store.get(REF, "infra/sup", None).unwrap();
    assert_eq!(active.version, 2);
    assert_eq!(active.supersedes.as_deref(), Some("infra/sup@1"));
    assert_eq!(active.allow[0].action, "y");

    // Old version readable by pin.
    let old = store.get(REF, "infra/sup", Some(1)).unwrap();
    assert_eq!(old.version, 1);
    assert_eq!(old.allow[0].action, "x");
}

#[test]
fn test_history_walks_supersedes_chain() {
    let (_r, store) = make_store("/policies");
    let p = skeleton("infra/h", Selector::Always);
    store.propose(REF, p).unwrap();
    store.ratify(REF, "infra/h", "alice", "v1").unwrap();
    let mut v2 = skeleton("infra/h", Selector::Always);
    v2.ratified_by = Some("alice".into());
    v2.ratified_at = Some(Utc::now());
    store.supersede(REF, "infra/h", v2).unwrap();
    let mut v3 = skeleton("infra/h", Selector::Always);
    v3.ratified_by = Some("alice".into());
    v3.ratified_at = Some(Utc::now());
    store.supersede(REF, "infra/h", v3).unwrap();

    let history = store.history(REF, "infra/h").unwrap();
    assert_eq!(history.len(), 3);
    assert_eq!(history[0].version, 1);
    assert_eq!(history[1].version, 2);
    assert_eq!(history[2].version, 3);
}

#[test]
fn test_get_pinned_missing_version_errors() {
    let (_r, store) = make_store("/policies");
    store
        .propose(REF, skeleton("infra/g", Selector::Always))
        .unwrap();
    let err = store.get(REF, "infra/g", Some(99)).unwrap_err();
    assert!(matches!(err, PolicyError::NotFound(_)));
}

// -----------------------------------------------------------------------
// list / active / policies_for_situation
// -----------------------------------------------------------------------

#[test]
fn test_list_returns_all_policies() {
    let (_r, store) = make_store("/policies");
    store
        .propose(REF, skeleton("infra/a", Selector::Always))
        .unwrap();
    store
        .propose(REF, skeleton("infra/b", Selector::Always))
        .unwrap();
    store
        .propose(REF, skeleton("security/c", Selector::Always))
        .unwrap();
    let all = store.list(REF, None).unwrap();
    assert_eq!(all.len(), 3);

    let infra = store.list(REF, Some("infra")).unwrap();
    assert_eq!(infra.len(), 2);
}

#[test]
fn test_active_filters_proposals() {
    let (_r, store) = make_store("/policies");
    store
        .propose(REF, skeleton("infra/a", Selector::Always))
        .unwrap();
    store
        .propose(REF, skeleton("infra/b", Selector::Always))
        .unwrap();
    store.ratify(REF, "infra/a", "alice", "ok").unwrap();
    let active = store.active(REF, None).unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].path, "infra/a");
}

#[test]
fn test_list_empty_when_unused() {
    let (_r, store) = make_store("/policies");
    let got = store.list(REF, None).unwrap();
    assert!(got.is_empty());
}

#[test]
fn test_list_excludes_history_entries() {
    let (_r, store) = make_store("/policies");
    store
        .propose(REF, skeleton("infra/x", Selector::Always))
        .unwrap();
    store.ratify(REF, "infra/x", "alice", "ok").unwrap();
    let mut v2 = skeleton("infra/x", Selector::Always);
    v2.ratified_by = Some("alice".into());
    v2.ratified_at = Some(Utc::now());
    store.supersede(REF, "infra/x", v2).unwrap();
    let all = store.list(REF, None).unwrap();
    assert_eq!(all.len(), 1, "list should not surface historical versions");
    assert_eq!(all[0].version, 2);
}

#[test]
fn test_policies_for_situation_filters_by_selector() {
    let (_r, store) = make_store("/policies");
    let mut prod = skeleton("infra/prod", Selector::eq("namespace", "prod"));
    prod.ratified_by = Some("alice".into());
    prod.ratified_at = Some(Utc::now());
    let mut dev = skeleton("infra/dev", Selector::eq("namespace", "dev"));
    dev.ratified_by = Some("alice".into());
    dev.ratified_at = Some(Utc::now());
    store.propose(REF, prod).unwrap();
    store.ratify(REF, "infra/prod", "alice", "ok").unwrap();
    store.propose(REF, dev).unwrap();
    store.ratify(REF, "infra/dev", "alice", "ok").unwrap();

    let s = Situation::new().with("namespace", "prod");
    let got = store.policies_for_situation(REF, &s).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].path, "infra/prod");
}

// -----------------------------------------------------------------------
// evaluate
// -----------------------------------------------------------------------

#[test]
fn test_evaluate_no_match_returns_no_policy_match() {
    let (_r, store) = make_store("/policies");
    let d = store
        .evaluate(REF, &Situation::new(), "restart_pod", "agent-1")
        .unwrap();
    assert_eq!(d, Decision::NoPolicyMatch);
}

#[test]
fn test_evaluate_allow_returns_matched_policy_and_preconditions() {
    let (_r, store) = make_store("/policies");
    let mut p = skeleton("infra/pod", Selector::eq("namespace", "prod"));
    p.allow = vec![allow_action("restart_pod", vec!["investigate_logs".into()])];
    store.propose(REF, p).unwrap();
    store.ratify(REF, "infra/pod", "alice", "ok").unwrap();

    let s = Situation::new().with("namespace", "prod");
    let d = store.evaluate(REF, &s, "restart_pod", "agent-1").unwrap();
    match d {
        Decision::Allow {
            matched_policy,
            preconditions,
        } => {
            assert_eq!(matched_policy, "infra/pod@1");
            assert_eq!(preconditions, vec!["investigate_logs".to_string()]);
        }
        other => panic!("expected Allow, got {:?}", other),
    }
}

#[test]
fn test_evaluate_ignores_proposed_policies() {
    let (_r, store) = make_store("/policies");
    let mut p = skeleton("infra/pod", Selector::Always);
    p.allow = vec![allow_action("restart_pod", vec![])];
    store.propose(REF, p).unwrap();
    // Not ratified — should NOT match.
    let d = store
        .evaluate(REF, &Situation::new(), "restart_pod", "agent-1")
        .unwrap();
    assert_eq!(d, Decision::NoPolicyMatch);
}

#[test]
fn test_evaluate_prefers_most_recent_ratified_version() {
    let (_r, store) = make_store("/policies");
    let mut p = skeleton("infra/pod", Selector::Always);
    p.allow = vec![allow_action("restart_pod", vec![])];
    store.propose(REF, p).unwrap();
    store.ratify(REF, "infra/pod", "alice", "v1").unwrap();

    let mut v2 = skeleton("infra/pod", Selector::Always);
    v2.deny = vec![deny_action("restart_pod", "policy changed")];
    v2.ratified_by = Some("alice".into());
    v2.ratified_at = Some(Utc::now());
    store.supersede(REF, "infra/pod", v2).unwrap();

    let d = store
        .evaluate(REF, &Situation::new(), "restart_pod", "agent-1")
        .unwrap();
    match d {
        Decision::Deny { matched_policy, .. } => assert_eq!(matched_policy, "infra/pod@2"),
        other => panic!("expected Deny from v2, got {:?}", other),
    }
}

#[test]
fn test_evaluate_deny_wins_over_approval_wins_over_allow() {
    let (_r, store) = make_store("/policies");
    let mut allow_p = skeleton("infra/a", Selector::Always);
    allow_p.allow = vec![allow_action("x", vec![])];
    allow_p.ratified_by = Some("alice".into());
    allow_p.ratified_at = Some(Utc::now());

    let mut approve_p = skeleton("infra/b", Selector::Always);
    approve_p.require_approval = vec![approval("x", FallbackAction::Block)];
    approve_p.ratified_by = Some("alice".into());
    approve_p.ratified_at = Some(Utc::now());

    let mut deny_p = skeleton("infra/c", Selector::Always);
    deny_p.deny = vec![deny_action("x", "no")];
    deny_p.ratified_by = Some("alice".into());
    deny_p.ratified_at = Some(Utc::now());

    store.propose(REF, allow_p).unwrap();
    store.ratify(REF, "infra/a", "alice", "ok").unwrap();
    store.propose(REF, approve_p).unwrap();
    store.ratify(REF, "infra/b", "alice", "ok").unwrap();

    // Without the deny: approval wins over allow.
    let d = store.evaluate(REF, &Situation::new(), "x", "a1").unwrap();
    assert!(matches!(d, Decision::RequireApproval { .. }));

    // Now add the deny policy — it wins.
    store.propose(REF, deny_p).unwrap();
    store.ratify(REF, "infra/c", "alice", "ok").unwrap();
    let d = store.evaluate(REF, &Situation::new(), "x", "a1").unwrap();
    match d {
        Decision::Deny {
            matched_policy,
            reason,
        } => {
            assert_eq!(matched_policy, "infra/c@1");
            assert_eq!(reason, "no");
        }
        other => panic!("expected Deny, got {:?}", other),
    }
}

#[test]
fn test_evaluate_unmatched_selector_gives_no_policy_match() {
    let (_r, store) = make_store("/policies");
    let mut p = skeleton("infra/pod", Selector::eq("namespace", "prod"));
    p.allow = vec![allow_action("x", vec![])];
    store.propose(REF, p).unwrap();
    store.ratify(REF, "infra/pod", "alice", "ok").unwrap();

    let s = Situation::new().with("namespace", "dev");
    let d = store.evaluate(REF, &s, "x", "a1").unwrap();
    assert_eq!(d, Decision::NoPolicyMatch);
}

// -----------------------------------------------------------------------
// evaluate_change / cost-of-change
// -----------------------------------------------------------------------

#[test]
fn test_evaluate_change_triggers_match_tokens() {
    let (_r, store) = make_store("/policies");
    let mut p = skeleton("change/high-cost", Selector::Always);
    p.triggers = vec!["reindex".into(), "downtime".into()];
    p.require_approval = vec![approval("*", FallbackAction::LowestRiskAlternative)];
    store.propose(REF, p).unwrap();
    store
        .ratify(REF, "change/high-cost", "alice", "ok")
        .unwrap();

    let proposal = ChangeProposal::new("promote_spec", "agent-1", "merge C", "spec-7")
        .with_tokens(["reindex", "downtime"])
        .with_field("estimated_downtime", "5m")
        .with_field("rollback_plan", "restore snapshot")
        .with_field("approval_authority", "platform-lead");

    // required_fields empty ⇒ falls through to approval rule.
    let d = store.evaluate_change(REF, &proposal).unwrap();
    match d {
        Decision::RequireApproval {
            matched_policy,
            fallback,
            ..
        } => {
            assert_eq!(matched_policy, "change/high-cost@1");
            assert_eq!(fallback, FallbackAction::LowestRiskAlternative);
        }
        other => panic!("expected RequireApproval, got {:?}", other),
    }
}

#[test]
fn test_evaluate_change_no_token_intersection_is_no_match() {
    let (_r, store) = make_store("/policies");
    let mut p = skeleton("change/high-cost", Selector::Always);
    p.triggers = vec!["reindex".into()];
    p.require_approval = vec![approval("*", FallbackAction::Block)];
    store.propose(REF, p).unwrap();
    store
        .ratify(REF, "change/high-cost", "alice", "ok")
        .unwrap();

    let proposal =
        ChangeProposal::new("promote", "a1", "safe change", "spec-1").with_tokens(["cosmetic"]);
    assert_eq!(
        store.evaluate_change(REF, &proposal).unwrap(),
        Decision::NoPolicyMatch
    );
}

#[test]
fn test_evaluate_change_missing_required_field_shortcircuits_to_approval() {
    let (_r, store) = make_store("/policies");
    let mut p = skeleton("change/high-cost", Selector::Always);
    p.triggers = vec!["reindex".into()];
    p.required_fields = vec![
        "estimated_downtime".into(),
        "rollback_plan".into(),
        "approval_authority".into(),
    ];
    p.require_approval = vec![approval("*", FallbackAction::LowestRiskAlternative)];
    store.propose(REF, p).unwrap();
    store
        .ratify(REF, "change/high-cost", "alice", "ok")
        .unwrap();

    let proposal = ChangeProposal::new("promote_spec", "a1", "merge C", "spec-7")
        .with_tokens(["reindex"])
        .with_field("estimated_downtime", "5m");
    // Missing rollback_plan + approval_authority → short-circuit.
    let d = store.evaluate_change(REF, &proposal).unwrap();
    match d {
        Decision::RequireApproval {
            matched_policy,
            fallback,
            ..
        } => {
            assert_eq!(matched_policy, "change/high-cost@1");
            assert_eq!(fallback, FallbackAction::LowestRiskAlternative);
        }
        other => panic!("expected RequireApproval, got {:?}", other),
    }
}

#[test]
fn test_evaluate_change_missing_field_fallback_defaults_to_block() {
    let (_r, store) = make_store("/policies");
    // Policy with required_fields but NO matching require_approval rule
    // for the incoming action. Short-circuit should still produce a
    // RequireApproval with a Block fallback.
    let mut p = skeleton("change/gate", Selector::Always);
    p.triggers = vec!["destructive".into()];
    p.required_fields = vec!["rollback_plan".into()];
    store.propose(REF, p).unwrap();
    store.ratify(REF, "change/gate", "alice", "ok").unwrap();

    let proposal =
        ChangeProposal::new("wipe", "a1", "rm -rf", "spec-9").with_tokens(["destructive"]);
    let d = store.evaluate_change(REF, &proposal).unwrap();
    match d {
        Decision::RequireApproval { fallback, .. } => {
            assert_eq!(fallback, FallbackAction::Block);
        }
        other => panic!("expected RequireApproval, got {:?}", other),
    }
}

#[test]
fn test_evaluate_change_ignores_unratified_policy() {
    let (_r, store) = make_store("/policies");
    let mut p = skeleton("change/high-cost", Selector::Always);
    p.triggers = vec!["reindex".into()];
    p.required_fields = vec!["rollback_plan".into()];
    p.require_approval = vec![approval("*", FallbackAction::Block)];
    store.propose(REF, p).unwrap();
    // Don't ratify.

    let proposal =
        ChangeProposal::new("promote_spec", "a1", "merge C", "spec-7").with_tokens(["reindex"]);
    assert_eq!(
        store.evaluate_change(REF, &proposal).unwrap(),
        Decision::NoPolicyMatch
    );
}

#[test]
fn test_evaluate_change_allows_when_fields_present_and_only_allow_rule() {
    let (_r, store) = make_store("/policies");
    let mut p = skeleton("change/tracked", Selector::Always);
    p.triggers = vec!["migration".into()];
    p.required_fields = vec!["rollback_plan".into()];
    p.allow = vec![allow_action("promote_spec", vec![])];
    store.propose(REF, p).unwrap();
    store.ratify(REF, "change/tracked", "alice", "ok").unwrap();

    let proposal = ChangeProposal::new("promote_spec", "a1", "ok", "spec-1")
        .with_tokens(["migration"])
        .with_field("rollback_plan", "yes");
    let d = store.evaluate_change(REF, &proposal).unwrap();
    assert!(matches!(d, Decision::Allow { .. }));
}

// -----------------------------------------------------------------------
// Selector semantics + serde
// -----------------------------------------------------------------------

#[test]
fn test_selector_all_any_not_semantics() {
    let s = Situation::new()
        .with("namespace", "prod")
        .with("state", "Failed");

    let both = Selector::all(vec![
        Selector::eq("namespace", "prod"),
        Selector::eq("state", "Failed"),
    ]);
    assert!(both.matches(&s));

    let either = Selector::any(vec![
        Selector::eq("namespace", "dev"),
        Selector::eq("state", "Failed"),
    ]);
    assert!(either.matches(&s));

    let missing = Selector::negate(Selector::exists("region"));
    assert!(missing.matches(&s));

    let both_wrong = Selector::all(vec![
        Selector::eq("namespace", "dev"),
        Selector::eq("state", "Failed"),
    ]);
    assert!(!both_wrong.matches(&s));
}

#[test]
fn test_fallback_action_serializes_all_variants() {
    use FallbackAction::*;
    let variants = vec![
        Block,
        PickAlternative {
            action: "apply_safe".into(),
        },
        LowestRiskAlternative,
        KeepCurrentState,
        DelegateTo {
            policy_path: "change/fallback".into(),
        },
    ];
    for v in variants {
        let json = serde_json::to_string(&v).unwrap();
        // Each tagged form contains the kind key.
        assert!(json.contains("\"kind\""), "no kind in {}", json);
        let back: FallbackAction = serde_json::from_str(&json).unwrap();
        assert_eq!(v, back);
    }
}

#[test]
fn test_decision_json_tagged() {
    let d = Decision::Allow {
        matched_policy: "infra/x@1".into(),
        preconditions: vec!["a".into()],
    };
    let j = serde_json::to_value(&d).unwrap();
    assert_eq!(j.get("kind").unwrap(), "allow");
    let back: Decision = serde_json::from_value(j).unwrap();
    assert_eq!(d, back);
}

#[test]
fn test_no_policy_match_is_not_translated_by_engine() {
    // POLICY_V1.md §5 says fail-safe is the MCP layer's job. The crate
    // returns NoPolicyMatch verbatim — this test pins that contract.
    let (_r, store) = make_store("/policies");
    let d = store
        .evaluate(REF, &Situation::new(), "anything", "agent-1")
        .unwrap();
    assert_eq!(d, Decision::NoPolicyMatch);
}

#[test]
fn test_prefix_trailing_slash_normalized() {
    let (_r, store) = make_store("/policies/");
    store
        .propose(REF, skeleton("infra/a", Selector::Always))
        .unwrap();
    let all = store.list(REF, None).unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(store.prefix(), "/policies");
}

#[test]
fn test_get_leading_slash_tolerated() {
    let (_r, store) = make_store("/policies");
    store
        .propose(REF, skeleton("infra/a", Selector::Always))
        .unwrap();
    let with = store.get(REF, "/infra/a", None).unwrap();
    let without = store.get(REF, "infra/a", None).unwrap();
    assert_eq!(with, without);
}

#[test]
fn test_severity_field_persists_through_roundtrip() {
    let (_r, store) = make_store("/policies");
    let mut p = skeleton("infra/crit", Selector::Always);
    p.severity = Severity::Critical;
    store.propose(REF, p).unwrap();
    let loaded = store.get(REF, "infra/crit", None).unwrap();
    assert_eq!(loaded.severity, Severity::Critical);
}

#[test]
fn test_proposed_policies_not_consulted_by_evaluate_change() {
    // Mirrors test_evaluate_ignores_proposed_policies for evaluate_change.
    let (_r, store) = make_store("/policies");
    let mut p = skeleton("change/p", Selector::Always);
    p.triggers = vec!["destructive".into()];
    p.require_approval = vec![approval("*", FallbackAction::Block)];
    store.propose(REF, p).unwrap();
    // Not ratified.
    let proposal = ChangeProposal::new("rm", "a1", "drop", "spec-1").with_tokens(["destructive"]);
    assert_eq!(
        store.evaluate_change(REF, &proposal).unwrap(),
        Decision::NoPolicyMatch
    );
}

#[test]
fn test_supersede_missing_errors() {
    let (_r, store) = make_store("/policies");
    let err = store
        .supersede(REF, "infra/nope", skeleton("infra/nope", Selector::Always))
        .unwrap_err();
    assert!(matches!(err, PolicyError::NotFound(_)));
}

// -----------------------------------------------------------------------
// §1 (0.7.0) — active_from scheduled activation
// -----------------------------------------------------------------------

#[test]
fn test_evaluate_ignores_not_yet_active_policy() {
    // A ratified policy with active_from one hour in the future is
    // treated as not-yet-active: evaluate should skip it entirely.
    let (_r, store) = make_store("/policies");

    let mut p = skeleton("scheduled/future", Selector::Always);
    p.allow = vec![allow_action("any", vec![])];
    p.active_from = Utc::now() + chrono::Duration::hours(1);
    store.propose(REF, p).unwrap();
    store
        .ratify(REF, "scheduled/future", "alice", "scheduled rollout")
        .unwrap();

    let sit: Situation =
        std::collections::HashMap::from([("k".to_string(), "v".to_string())]).into();
    let dec = store.evaluate(REF, &sit, "any", "a1").unwrap();
    assert_eq!(
        dec,
        Decision::NoPolicyMatch,
        "ratified-but-not-yet-active policy must be skipped like a proposal"
    );
}

#[test]
fn test_evaluate_honors_past_active_from() {
    // A ratified policy with active_from in the past is consulted
    // normally.
    let (_r, store) = make_store("/policies");

    let mut p = skeleton("scheduled/live", Selector::Always);
    p.allow = vec![allow_action("any", vec![])];
    p.active_from = Utc::now() - chrono::Duration::hours(1);
    store.propose(REF, p).unwrap();
    store
        .ratify(REF, "scheduled/live", "alice", "long since live")
        .unwrap();

    let sit: Situation =
        std::collections::HashMap::from([("k".to_string(), "v".to_string())]).into();
    let dec = store.evaluate(REF, &sit, "any", "a1").unwrap();
    assert!(
        matches!(dec, Decision::Allow { .. }),
        "past active_from must not block evaluation; got {:?}",
        dec
    );
}

#[test]
fn test_evaluate_change_ignores_not_yet_active_policy() {
    // Same rule for evaluate_change: a ratified policy with a future
    // active_from does not contribute tokens to change-cost gating.
    let (_r, store) = make_store("/policies");

    let mut p = skeleton("scheduled/change", Selector::Always);
    p.triggers = vec!["destructive".into()];
    p.require_approval = vec![approval("*", FallbackAction::Block)];
    p.active_from = Utc::now() + chrono::Duration::hours(1);
    store.propose(REF, p).unwrap();
    store
        .ratify(REF, "scheduled/change", "alice", "scheduled change gate")
        .unwrap();

    let proposal = ChangeProposal::new("rm", "a1", "drop", "spec-1").with_tokens(["destructive"]);
    assert_eq!(
        store.evaluate_change(REF, &proposal).unwrap(),
        Decision::NoPolicyMatch,
        "not-yet-active change-cost policy must not fire"
    );
}

#[test]
fn test_is_currently_active_helper() {
    // Unit-coverage for Policy::is_currently_active — the helper used
    // by active() / policies_for_situation to filter.
    let mut p = skeleton("unit/helper", Selector::Always);
    let now = Utc::now();

    // Unratified — never active.
    p.active_from = now - chrono::Duration::hours(1);
    assert!(!p.is_currently_active(now));

    // Ratified but active_from in the future — not yet active.
    p.ratified_by = Some("alice".into());
    p.ratified_at = Some(now);
    p.active_from = now + chrono::Duration::hours(1);
    assert!(!p.is_currently_active(now));

    // Ratified and active_from <= now — active.
    p.active_from = now - chrono::Duration::hours(1);
    assert!(p.is_currently_active(now));

    // Boundary: active_from == now — inclusive, active.
    p.active_from = now;
    assert!(p.is_currently_active(now));
}

// -----------------------------------------------------------------------
// §1 (0.7.5) — expires_at scheduled deactivation
// -----------------------------------------------------------------------

#[test]
fn test_evaluate_ignores_expired_policy() {
    // A ratified policy whose expires_at is in the past is treated as
    // not-currently-active: evaluate skips it like an unratified
    // proposal.
    let (_r, store) = make_store("/policies");

    let mut p = skeleton("expired/p", Selector::Always);
    p.allow = vec![allow_action("any", vec![])];
    p.active_from = Utc::now() - chrono::Duration::hours(2);
    p.expires_at = Some(Utc::now() - chrono::Duration::hours(1));
    store.propose(REF, p).unwrap();
    store
        .ratify(REF, "expired/p", "alice", "expired now")
        .unwrap();

    let sit: Situation =
        std::collections::HashMap::from([("k".to_string(), "v".to_string())]).into();
    let dec = store.evaluate(REF, &sit, "any", "a1").unwrap();
    assert_eq!(
        dec,
        Decision::NoPolicyMatch,
        "expired policy must be skipped"
    );
}

#[test]
fn test_evaluate_honors_not_yet_expired_policy() {
    let (_r, store) = make_store("/policies");

    let mut p = skeleton("live/p", Selector::Always);
    p.allow = vec![allow_action("any", vec![])];
    p.active_from = Utc::now() - chrono::Duration::hours(1);
    p.expires_at = Some(Utc::now() + chrono::Duration::hours(1));
    store.propose(REF, p).unwrap();
    store
        .ratify(REF, "live/p", "alice", "expires tomorrow")
        .unwrap();

    let sit: Situation =
        std::collections::HashMap::from([("k".to_string(), "v".to_string())]).into();
    let dec = store.evaluate(REF, &sit, "any", "a1").unwrap();
    assert!(
        matches!(dec, Decision::Allow { .. }),
        "future-expiry policy must still match; got {:?}",
        dec
    );
}

#[test]
fn test_is_currently_active_honors_expires_at_boundary() {
    // expires_at is an exclusive upper bound: a policy whose
    // expires_at == now is already expired.
    let mut p = skeleton("boundary", Selector::Always);
    let now = Utc::now();
    p.ratified_by = Some("alice".into());
    p.ratified_at = Some(now - chrono::Duration::hours(1));
    p.active_from = now - chrono::Duration::hours(1);

    p.expires_at = Some(now);
    assert!(!p.is_currently_active(now));

    p.expires_at = Some(now + chrono::Duration::seconds(1));
    assert!(p.is_currently_active(now));
}

// -----------------------------------------------------------------------
// §2b — Policy.signature field + verifier hook
// -----------------------------------------------------------------------

mod signature_hook {
    use super::*;

    use agentstategraph_policy::{PolicySignature, SignatureVerificationError, SignatureVerifier};

    type VerifyFn = dyn Fn(&Policy) -> Result<(), SignatureVerificationError> + Send + Sync;

    /// Closure-backed mock verifier. Keeps the policy crate's test suite
    /// independent of `agentstategraph-policy-sign`.
    struct MockVerifier {
        f: Box<VerifyFn>,
    }

    impl MockVerifier {
        fn new<F>(f: F) -> Arc<Self>
        where
            F: Fn(&Policy) -> Result<(), SignatureVerificationError> + Send + Sync + 'static,
        {
            Arc::new(Self { f: Box::new(f) })
        }
    }

    impl SignatureVerifier for MockVerifier {
        fn verify_policy(&self, policy: &Policy) -> Result<(), SignatureVerificationError> {
            (self.f)(policy)
        }
    }

    fn ratified(path: &str) -> Policy {
        let mut p = skeleton(path, Selector::Always);
        p.allow = vec![allow_action("do_thing", vec![])];
        p.ratified_by = Some("alice".into());
        p.ratified_at = Some(Utc::now());
        p.active_from = Utc::now() - chrono::Duration::seconds(1);
        p
    }

    fn fake_sig(key_id: &str) -> PolicySignature {
        PolicySignature::Ed25519 {
            signer_key_id: key_id.into(),
            signature_hex: "00".repeat(64),
        }
    }

    fn build_store_with_verifier<F>(
        prefix: &str,
        require_signed: bool,
        verifier_fn: F,
    ) -> (Arc<Repository>, PolicyStore)
    where
        F: Fn(&Policy) -> Result<(), SignatureVerificationError> + Send + Sync + 'static,
    {
        let repo = Arc::new(Repository::new(Box::new(MemoryStorage::new())));
        repo.init().unwrap();
        let store = PolicyStore::new(repo.clone(), prefix, "test-agent")
            .with_verifier(MockVerifier::new(verifier_fn))
            .with_require_signed(require_signed);
        (repo, store)
    }

    fn build_store_no_verifier(prefix: &str) -> (Arc<Repository>, PolicyStore) {
        let repo = Arc::new(Repository::new(Box::new(MemoryStorage::new())));
        repo.init().unwrap();
        let store = PolicyStore::new(repo.clone(), prefix, "test-agent");
        (repo, store)
    }

    /// Write `policy` directly as an already-ratified active entry,
    /// bypassing `propose`/`ratify` (those reset the `signature`
    /// field's siblings like `ratified_by`). Uses the same state path
    /// layout `PolicyStore` itself uses.
    fn seed_active(repo: &Arc<Repository>, prefix: &str, policy: &Policy) {
        use agentstategraph::CommitOptions;
        use agentstategraph_core::IntentCategory;
        let path = format!(
            "{}/{}/_meta",
            prefix.trim_end_matches('/'),
            policy.path.trim_start_matches('/')
        );
        let value = serde_json::to_value(policy).unwrap();
        repo.set_json(
            REF,
            &path,
            &value,
            CommitOptions::new(
                "test-agent",
                IntentCategory::Custom("policy-seed".into()),
                format!("seed {}", policy.handle()),
            ),
        )
        .unwrap();
    }

    #[test]
    fn test_policy_without_verifier_accepts_any_signature_or_none() {
        // No verifier registered → both signed and unsigned policies
        // pass through regardless of signature content (back-compat).
        let (repo, store) = build_store_no_verifier("/policies");
        let mut unsigned = ratified("infra/unsigned");
        unsigned.signature = None;
        let mut signed = ratified("infra/signed");
        signed.signature = Some(fake_sig("k1"));
        seed_active(&repo, "/policies", &unsigned);
        seed_active(&repo, "/policies", &signed);

        let actives = store.active(REF, None).unwrap();
        assert_eq!(
            actives.len(),
            2,
            "pass-through mode must keep both policies; got {:?}",
            actives.iter().map(|p| &p.path).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_policy_with_verifier_rejects_missing_signature_when_required() {
        let (repo, store) = build_store_with_verifier("/policies", true, |_: &Policy| Ok(()));
        let mut unsigned = ratified("infra/unsigned");
        unsigned.signature = None;
        seed_active(&repo, "/policies", &unsigned);

        let actives = store.active(REF, None).unwrap();
        assert!(
            actives.is_empty(),
            "require_signed=true must skip unsigned policies; got {:?}",
            actives
        );

        // And evaluate() inherits the filter → no policy match.
        let dec = store
            .evaluate(REF, &Situation::new(), "do_thing", "a1")
            .unwrap();
        assert!(matches!(dec, Decision::NoPolicyMatch));
    }

    #[test]
    fn test_policy_with_verifier_allows_missing_signature_when_not_required() {
        let (repo, store) = build_store_with_verifier("/policies", false, |_: &Policy| {
            panic!("verifier must not be called on unsigned policy")
        });
        let mut unsigned = ratified("infra/unsigned");
        unsigned.signature = None;
        seed_active(&repo, "/policies", &unsigned);

        let actives = store.active(REF, None).unwrap();
        assert_eq!(
            actives.len(),
            1,
            "require_signed=false must keep unsigned policies"
        );
    }

    #[test]
    fn test_policy_with_verifier_keeps_valid_signature() {
        let (repo, store) = build_store_with_verifier("/policies", true, |p: &Policy| {
            // Assert the verifier sees the signature field.
            assert!(p.signature.is_some());
            Ok(())
        });
        let mut signed = ratified("infra/signed");
        signed.signature = Some(fake_sig("k1"));
        seed_active(&repo, "/policies", &signed);

        let actives = store.active(REF, None).unwrap();
        assert_eq!(actives.len(), 1);
        assert_eq!(actives[0].path, "infra/signed");

        let dec = store
            .evaluate(REF, &Situation::new(), "do_thing", "a1")
            .unwrap();
        assert!(matches!(dec, Decision::Allow { .. }));
    }

    #[test]
    fn test_policy_with_verifier_skips_invalid_signature() {
        let (repo, store) = build_store_with_verifier("/policies", false, |_: &Policy| {
            Err(SignatureVerificationError::Invalid("tampered bytes".into()))
        });
        let mut signed = ratified("infra/tampered");
        signed.signature = Some(fake_sig("k1"));
        seed_active(&repo, "/policies", &signed);

        let actives = store.active(REF, None).unwrap();
        assert!(
            actives.is_empty(),
            "invalid-signature policies must be filtered out; got {:?}",
            actives.iter().map(|p| &p.path).collect::<Vec<_>>()
        );

        let dec = store
            .evaluate_change(REF, &ChangeProposal::new("do_thing", "a1", "i", "opt"))
            .unwrap();
        assert!(matches!(dec, Decision::NoPolicyMatch));
    }

    #[test]
    fn test_policy_signature_field_serializes_omitted_when_none() {
        // Baseline serde roundtrip: None signature is omitted from
        // the emitted JSON; Some is round-tripped losslessly.
        let mut p = ratified("infra/roundtrip");
        p.signature = None;
        let j = serde_json::to_value(&p).unwrap();
        assert!(
            j.get("signature").is_none(),
            "None signature must be omitted; got JSON: {j}"
        );

        p.signature = Some(fake_sig("k9"));
        let j = serde_json::to_value(&p).unwrap();
        let sig = j.get("signature").expect("Some signature must emit key");
        assert_eq!(
            sig.get("algorithm").and_then(|v| v.as_str()),
            Some("ed25519")
        );
        assert_eq!(
            sig.get("signer_key_id").and_then(|v| v.as_str()),
            Some("k9")
        );
        let back: Policy = serde_json::from_value(j).unwrap();
        assert_eq!(back.signature, p.signature);
    }
}

// -----------------------------------------------------------------------
// §3a (0.7.5) — tenant_id serde
// -----------------------------------------------------------------------

#[test]
fn test_policy_tenant_id_roundtrips() {
    let mut p = skeleton("tenant/scoped", Selector::Always);
    p.tenant_id = Some("acme".to_string());
    let s = serde_json::to_string(&p).unwrap();
    assert!(s.contains("\"tenant_id\":\"acme\""));
    let parsed: Policy = serde_json::from_str(&s).unwrap();
    assert_eq!(parsed.tenant_id.as_deref(), Some("acme"));
}

#[test]
fn test_policy_tenant_id_omitted_when_none() {
    let p = skeleton("global/policy", Selector::Always);
    assert!(p.tenant_id.is_none());
    let s = serde_json::to_string(&p).unwrap();
    assert!(
        !s.contains("tenant_id"),
        "expected tenant_id omitted when None, got {}",
        s
    );
}

// -----------------------------------------------------------------------
// §3b (0.7.5) — tenant_filter on _scoped evaluator methods
// -----------------------------------------------------------------------

/// Build and ratify a policy with the given tenant scope.
fn seed_scoped_policy(
    store: &PolicyStore,
    path: &str,
    tenant_id: Option<&str>,
    apply: impl FnOnce(&mut Policy),
) {
    let mut p = skeleton(path, Selector::Always);
    p.tenant_id = tenant_id.map(str::to_string);
    apply(&mut p);
    store.propose(REF, p).unwrap();
    store.ratify(REF, path, "alice", "ok").unwrap();
}

#[test]
fn test_tenant_filter_none_sees_all_policies() {
    let (_r, store) = make_store("/policies");
    seed_scoped_policy(&store, "global/p", None, |p| {
        p.allow = vec![allow_action("x", vec![])];
    });
    seed_scoped_policy(&store, "acme/p", Some("acme"), |p| {
        p.allow = vec![allow_action("x", vec![])];
    });

    let actives = store.active_scoped(REF, None, None).unwrap();
    assert_eq!(
        actives.len(),
        2,
        "tenant_filter=None must surface every policy; got {:?}",
        actives.iter().map(|p| &p.path).collect::<Vec<_>>()
    );
}

#[test]
fn test_tenant_filter_matches_scoped_policy() {
    let (_r, store) = make_store("/policies");
    seed_scoped_policy(&store, "acme/p", Some("acme"), |p| {
        p.allow = vec![allow_action("x", vec![])];
    });

    let actives = store.active_scoped(REF, None, Some("acme")).unwrap();
    assert_eq!(actives.len(), 1);
    assert_eq!(actives[0].path, "acme/p");
    assert_eq!(actives[0].tenant_id.as_deref(), Some("acme"));
}

#[test]
fn test_tenant_filter_excludes_other_tenant_policies() {
    let (_r, store) = make_store("/policies");
    seed_scoped_policy(&store, "acme/p", Some("acme"), |p| {
        p.allow = vec![allow_action("x", vec![])];
    });

    let actives = store.active_scoped(REF, None, Some("other")).unwrap();
    assert!(
        actives.is_empty(),
        "acme-scoped policy must not surface under tenant_filter=Some(\"other\"); got {:?}",
        actives.iter().map(|p| &p.path).collect::<Vec<_>>()
    );
}

#[test]
fn test_tenant_filter_always_includes_globals() {
    let (_r, store) = make_store("/policies");
    seed_scoped_policy(&store, "global/p", None, |p| {
        p.allow = vec![allow_action("x", vec![])];
    });
    seed_scoped_policy(&store, "acme/p", Some("acme"), |p| {
        p.allow = vec![allow_action("x", vec![])];
    });

    let actives = store.active_scoped(REF, None, Some("other")).unwrap();
    assert_eq!(
        actives.len(),
        1,
        "globals must remain visible under any tenant_filter"
    );
    assert_eq!(actives[0].path, "global/p");
    assert!(actives[0].tenant_id.is_none());
}

#[test]
fn test_evaluate_scoped_respects_tenant_filter() {
    let (_r, store) = make_store("/policies");
    seed_scoped_policy(&store, "acme/deny", Some("acme"), |p| {
        p.deny = vec![deny_action("restart_pod", "acme-only deny")];
    });

    // Matching tenant → the scoped deny fires.
    let d = store
        .evaluate_scoped(REF, &Situation::new(), "restart_pod", "a1", Some("acme"))
        .unwrap();
    match d {
        Decision::Deny {
            matched_policy,
            reason,
        } => {
            assert_eq!(matched_policy, "acme/deny@1");
            assert_eq!(reason, "acme-only deny");
        }
        other => panic!("expected Deny under tenant_filter=acme, got {:?}", other),
    }

    // Different tenant → the scoped deny is filtered out.
    let d = store
        .evaluate_scoped(REF, &Situation::new(), "restart_pod", "a1", Some("other"))
        .unwrap();
    assert_eq!(
        d,
        Decision::NoPolicyMatch,
        "acme-scoped policy must not contribute under tenant_filter=other"
    );

    // Back-compat path: no filter → scoped policy still fires.
    let d = store
        .evaluate(REF, &Situation::new(), "restart_pod", "a1")
        .unwrap();
    assert!(matches!(d, Decision::Deny { .. }));
}

#[test]
fn test_evaluate_change_scoped_respects_tenant_filter() {
    let (_r, store) = make_store("/policies");
    seed_scoped_policy(&store, "acme/change", Some("acme"), |p| {
        p.triggers = vec!["reindex".into()];
        p.require_approval = vec![approval("*", FallbackAction::Block)];
    });

    let proposal = ChangeProposal::new("promote", "a1", "merge", "spec-7").with_tokens(["reindex"]);

    // Matching tenant → approval gate fires.
    let d = store
        .evaluate_change_scoped(REF, &proposal, Some("acme"))
        .unwrap();
    match d {
        Decision::RequireApproval { matched_policy, .. } => {
            assert_eq!(matched_policy, "acme/change@1");
        }
        other => panic!("expected RequireApproval, got {:?}", other),
    }

    // Different tenant → no match.
    let d = store
        .evaluate_change_scoped(REF, &proposal, Some("other"))
        .unwrap();
    assert_eq!(d, Decision::NoPolicyMatch);

    // Back-compat path: no filter → gate still fires.
    let d = store.evaluate_change(REF, &proposal).unwrap();
    assert!(matches!(d, Decision::RequireApproval { .. }));
}
