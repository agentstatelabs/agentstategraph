//! Pure evaluation logic. Operates on an already-loaded slice of
//! policies, so it can be exercised in unit tests without touching
//! storage. The `PolicyStore` default impl is the production caller.
//!
//! Semantics follow POLICY_V1.md §5 (authorization) and §22.2 (change-
//! cost gating).

use crate::types::{
    ApprovalRule, AuthorizedAction, ChangeProposal, Decision, FallbackAction, Policy,
};

/// Evaluate an (action, agent_id) request against the already-filtered
/// list of *matching ratified* policies. Matching = `situation_selector`
/// evaluated to `true` for the caller's situation.
///
/// Precedence (POLICY_V1.md §5): `deny` > `require_approval` > `allow`.
/// If none of the matched policies mention the requested action, returns
/// `NoPolicyMatch` — callers (MCP layer) are responsible for translating
/// that to a fail-safe `Deny`.
pub fn evaluate_matched(policies: &[&Policy], action: &str, _agent_id: &str) -> Decision {
    // First pass: denies win outright.
    for p in policies {
        if let Some(rule) = match_action(&p.deny, action) {
            return Decision::Deny {
                matched_policy: p.handle(),
                reason: rule
                    .condition
                    .clone()
                    .unwrap_or_else(|| format!("denied by {}", p.path)),
            };
        }
    }
    // Second pass: require_approval.
    for p in policies {
        if let Some(rule) = match_approval(&p.require_approval, action) {
            return Decision::RequireApproval {
                matched_policy: p.handle(),
                approvers: rule.approvers.clone(),
                timeout: rule.timeout,
                fallback: rule.fallback.clone(),
                approval_task_path: None,
            };
        }
    }
    // Third pass: allow.
    for p in policies {
        if let Some(rule) = match_action(&p.allow, action) {
            return Decision::Allow {
                matched_policy: p.handle(),
                preconditions: rule.preconditions.clone(),
            };
        }
    }
    Decision::NoPolicyMatch
}

/// Evaluate a `ChangeProposal` against the already-filtered list of
/// ratified policies (typically *all* active policies — token-based
/// filtering happens here).
///
/// Semantics (POLICY_V1.md §22.2.2):
/// 1. Select policies whose `triggers` intersect the proposal's `tokens`.
/// 2. If any such policy has `required_fields` missing from
///    `proposal.attached_fields`, short-circuit to `RequireApproval`
///    with that policy's fallback (first require_approval rule matching
///    `proposal.action` or `"*"`, falling back to
///    `FallbackAction::Block`).
/// 3. Otherwise apply the same precedence as `evaluate_matched` across
///    the token-matched policies for `proposal.action`.
pub fn evaluate_change(policies: &[&Policy], proposal: &ChangeProposal) -> Decision {
    let consulted: Vec<&Policy> = policies
        .iter()
        .copied()
        .filter(|p| tokens_intersect(&p.triggers, &proposal.tokens))
        .collect();

    if consulted.is_empty() {
        return Decision::NoPolicyMatch;
    }

    for p in &consulted {
        let missing: Vec<&String> = p
            .required_fields
            .iter()
            .filter(|f| !proposal.attached_fields.contains_key(f.as_str()))
            .collect();
        if !missing.is_empty() {
            let (approvers, timeout, fallback) = approval_hint(p, &proposal.action);
            return Decision::RequireApproval {
                matched_policy: p.handle(),
                approvers,
                timeout,
                fallback,
                approval_task_path: None,
            };
        }
    }

    evaluate_matched(&consulted, &proposal.action, &proposal.agent_id)
}

fn match_action<'a>(rules: &'a [AuthorizedAction], action: &str) -> Option<&'a AuthorizedAction> {
    rules.iter().find(|r| r.action == action || r.action == "*")
}

fn match_approval<'a>(rules: &'a [ApprovalRule], action: &str) -> Option<&'a ApprovalRule> {
    rules.iter().find(|r| r.action == action || r.action == "*")
}

fn tokens_intersect(a: &[String], b: &[String]) -> bool {
    a.iter().any(|t| b.iter().any(|u| t == u))
}

/// Pull approval hint off a policy: prefer a rule matching `action`,
/// else `"*"`, else synthesise `(["human"], None, Block)` so a missing-
/// field short-circuit always has *some* fallback to return.
fn approval_hint(
    p: &Policy,
    action: &str,
) -> (Vec<String>, Option<std::time::Duration>, FallbackAction) {
    if let Some(r) = match_approval(&p.require_approval, action) {
        return (r.approvers.clone(), r.timeout, r.fallback.clone());
    }
    (vec!["human".to_string()], None, FallbackAction::Block)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selector::Selector;
    use chrono::Utc;

    fn base(path: &str) -> Policy {
        Policy {
            path: path.into(),
            version: 1,
            situation: "test".into(),
            situation_selector: Selector::Always,
            allow: vec![],
            deny: vec![],
            require_approval: vec![],
            procedure: None,
            triggers: vec![],
            required_fields: vec![],
            severity: Default::default(),
            proposed_by: "claude".into(),
            proposed_at: Utc::now(),
            ratified_by: Some("alice".into()),
            ratified_at: Some(Utc::now()),
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
    fn no_policies_returns_no_match() {
        let d = evaluate_matched(&[], "restart_pod", "agent-1");
        assert_eq!(d, Decision::NoPolicyMatch);
    }

    #[test]
    fn deny_wins_over_approval_and_allow() {
        let mut allow_p = base("a");
        allow_p.allow = vec![AuthorizedAction {
            action: "x".into(),
            condition: None,
            preconditions: vec![],
        }];
        let mut appr_p = base("b");
        appr_p.require_approval = vec![ApprovalRule {
            action: "x".into(),
            approvers: vec!["human".into()],
            timeout: None,
            fallback: FallbackAction::Block,
        }];
        let mut deny_p = base("c");
        deny_p.deny = vec![AuthorizedAction {
            action: "x".into(),
            condition: Some("no".into()),
            preconditions: vec![],
        }];
        let d = evaluate_matched(&[&allow_p, &appr_p, &deny_p], "x", "a1");
        assert!(matches!(d, Decision::Deny { .. }));
    }

    #[test]
    fn approval_wins_over_allow() {
        let mut allow_p = base("a");
        allow_p.allow = vec![AuthorizedAction {
            action: "x".into(),
            condition: None,
            preconditions: vec![],
        }];
        let mut appr_p = base("b");
        appr_p.require_approval = vec![ApprovalRule {
            action: "x".into(),
            approvers: vec!["human".into()],
            timeout: None,
            fallback: FallbackAction::KeepCurrentState,
        }];
        let d = evaluate_matched(&[&allow_p, &appr_p], "x", "a1");
        assert!(matches!(d, Decision::RequireApproval { .. }));
    }

    #[test]
    fn wildcard_action_matches_any_action_in_allow() {
        let mut p = base("a");
        p.allow = vec![AuthorizedAction {
            action: "*".into(),
            condition: None,
            preconditions: vec![],
        }];
        let d = evaluate_matched(&[&p], "anything_at_all", "a1");
        assert!(matches!(d, Decision::Allow { .. }));
    }

    #[test]
    fn wildcard_action_matches_any_action_in_deny() {
        let mut p = base("a");
        p.deny = vec![AuthorizedAction {
            action: "*".into(),
            condition: Some("blanket deny".into()),
            preconditions: vec![],
        }];
        let d = evaluate_matched(&[&p], "some_action", "a1");
        assert!(matches!(d, Decision::Deny { .. }));
    }

    #[test]
    fn wildcard_in_require_approval_matches_any_action() {
        let mut p = base("a");
        p.require_approval = vec![ApprovalRule {
            action: "*".into(),
            approvers: vec!["lead".into()],
            timeout: None,
            fallback: FallbackAction::Block,
        }];
        let d = evaluate_matched(&[&p], "deploy", "a1");
        assert!(matches!(d, Decision::RequireApproval { .. }));
    }

    #[test]
    fn deny_condition_none_uses_policy_path_in_reason() {
        let mut p = base("infra/firewall");
        p.deny = vec![AuthorizedAction {
            action: "open_port".into(),
            condition: None,
            preconditions: vec![],
        }];
        let d = evaluate_matched(&[&p], "open_port", "a1");
        if let Decision::Deny { reason, .. } = d {
            assert!(reason.contains("infra/firewall"), "reason: {reason}");
        } else {
            panic!("expected Deny");
        }
    }

    #[test]
    fn deny_condition_some_uses_condition_as_reason() {
        let mut p = base("a");
        p.deny = vec![AuthorizedAction {
            action: "x".into(),
            condition: Some("no external writes".into()),
            preconditions: vec![],
        }];
        let d = evaluate_matched(&[&p], "x", "a1");
        if let Decision::Deny { reason, .. } = d {
            assert_eq!(reason, "no external writes");
        } else {
            panic!("expected Deny");
        }
    }

    #[test]
    fn allow_returns_preconditions() {
        let mut p = base("a");
        p.allow = vec![AuthorizedAction {
            action: "deploy".into(),
            condition: None,
            preconditions: vec!["tests_green".into(), "review_approved".into()],
        }];
        let d = evaluate_matched(&[&p], "deploy", "a1");
        if let Decision::Allow { preconditions, .. } = d {
            assert_eq!(preconditions, vec!["tests_green", "review_approved"]);
        } else {
            panic!("expected Allow");
        }
    }

    #[test]
    fn unknown_action_in_populated_policy_returns_no_match() {
        let mut p = base("a");
        p.allow = vec![AuthorizedAction {
            action: "specific_action".into(),
            condition: None,
            preconditions: vec![],
        }];
        let d = evaluate_matched(&[&p], "other_action", "a1");
        assert_eq!(d, Decision::NoPolicyMatch);
    }

    // --- evaluate_change ---

    fn change(action: &str, tokens: &[&str]) -> ChangeProposal {
        ChangeProposal::new(action, "agent-1", "intent", "option-A")
            .with_tokens(tokens.iter().copied())
    }

    #[test]
    fn evaluate_change_no_token_intersection_returns_no_match() {
        let mut p = base("a");
        p.triggers = vec!["destructive".into()];
        p.allow = vec![AuthorizedAction {
            action: "*".into(),
            condition: None,
            preconditions: vec![],
        }];
        let proposal = change("delete", &["reindex"]);
        let d = evaluate_change(&[&p], &proposal);
        assert_eq!(d, Decision::NoPolicyMatch);
    }

    #[test]
    fn evaluate_change_token_match_consults_policy() {
        let mut p = base("a");
        p.triggers = vec!["destructive".into()];
        p.allow = vec![AuthorizedAction {
            action: "delete".into(),
            condition: None,
            preconditions: vec![],
        }];
        let proposal = change("delete", &["destructive"]);
        let d = evaluate_change(&[&p], &proposal);
        assert!(matches!(d, Decision::Allow { .. }));
    }

    #[test]
    fn evaluate_change_missing_required_field_short_circuits() {
        let mut p = base("a");
        p.triggers = vec!["schema-change".into()];
        p.required_fields = vec!["justification".into()];
        p.allow = vec![AuthorizedAction {
            action: "migrate".into(),
            condition: None,
            preconditions: vec![],
        }];
        let proposal = change("migrate", &["schema-change"]); // no attached fields
        let d = evaluate_change(&[&p], &proposal);
        assert!(matches!(d, Decision::RequireApproval { .. }));
    }

    #[test]
    fn evaluate_change_all_required_fields_present_proceeds_to_evaluate() {
        let mut p = base("a");
        p.triggers = vec!["schema-change".into()];
        p.required_fields = vec!["justification".into()];
        p.allow = vec![AuthorizedAction {
            action: "migrate".into(),
            condition: None,
            preconditions: vec![],
        }];
        let proposal = change("migrate", &["schema-change"])
            .with_field("justification", "upgrade for v2");
        let d = evaluate_change(&[&p], &proposal);
        assert!(matches!(d, Decision::Allow { .. }));
    }

    #[test]
    fn evaluate_change_missing_field_uses_matching_approval_rule_hint() {
        let mut p = base("a");
        p.triggers = vec!["large".into()];
        p.required_fields = vec!["runbook".into()];
        p.require_approval = vec![ApprovalRule {
            action: "bulk_update".into(),
            approvers: vec!["ops-team".into()],
            timeout: None,
            fallback: FallbackAction::Block,
        }];
        let proposal = change("bulk_update", &["large"]); // missing runbook
        let d = evaluate_change(&[&p], &proposal);
        if let Decision::RequireApproval { approvers, .. } = d {
            assert_eq!(approvers, vec!["ops-team"]);
        } else {
            panic!("expected RequireApproval, got {d:?}");
        }
    }

    #[test]
    fn evaluate_change_missing_field_no_approval_rule_falls_back_to_human() {
        let mut p = base("a");
        p.triggers = vec!["large".into()];
        p.required_fields = vec!["runbook".into()];
        // No require_approval rules
        let proposal = change("bulk_update", &["large"]);
        let d = evaluate_change(&[&p], &proposal);
        if let Decision::RequireApproval { approvers, .. } = d {
            assert_eq!(approvers, vec!["human"]);
        } else {
            panic!("expected RequireApproval, got {d:?}");
        }
    }

    #[test]
    fn evaluate_change_multiple_tokens_any_intersection_triggers_policy() {
        let mut p = base("a");
        p.triggers = vec!["migration".into()];
        p.deny = vec![AuthorizedAction {
            action: "rollback".into(),
            condition: Some("no rollback during migration".into()),
            preconditions: vec![],
        }];
        // proposal has several tokens; "migration" is one of them
        let proposal = change("rollback", &["large", "migration", "reindex"]);
        let d = evaluate_change(&[&p], &proposal);
        assert!(matches!(d, Decision::Deny { .. }));
    }
}
