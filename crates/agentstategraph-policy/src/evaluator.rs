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
}
