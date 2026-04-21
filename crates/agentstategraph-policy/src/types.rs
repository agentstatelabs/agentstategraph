//! Public data types for policies, change proposals, and decisions.
//!
//! Follows POLICY_V1.md §2.1 + §22.2/3. `triggers`, `required_fields`,
//! `severity`, `FallbackAction`, and `ChangeProposal` are the v1.1
//! cost-of-change additions.

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::selector::Selector;

/// The unit of authorization + procedure.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Policy {
    /// Normalized path under the store prefix, e.g. `"infra/k8s/pod-failing"`.
    pub path: String,
    /// Monotonically increasing version, starting at 1 for a fresh
    /// `propose`, incremented by `supersede`.
    pub version: u64,

    /// Human-readable situation description.
    pub situation: String,
    /// Machine-evaluable situation matcher.
    pub situation_selector: Selector,

    #[serde(default)]
    pub allow: Vec<AuthorizedAction>,
    #[serde(default)]
    pub deny: Vec<AuthorizedAction>,
    #[serde(default)]
    pub require_approval: Vec<ApprovalRule>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub procedure: Option<Vec<ProcedureStep>>,

    /// Opaque tokens used to match this policy against a `ChangeProposal`.
    /// See §22.2 — if any proposal token appears in `triggers`, this
    /// policy is consulted.
    #[serde(default)]
    pub triggers: Vec<String>,
    /// Fields that a `ChangeProposal` must carry to be promoted under
    /// this policy. Missing any required field short-circuits to
    /// `RequireApproval`.
    #[serde(default)]
    pub required_fields: Vec<String>,
    /// Advisory severity for sorting/rendering. Does not change decision
    /// semantics.
    #[serde(default)]
    pub severity: Severity,

    pub proposed_by: String,
    pub proposed_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ratified_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ratified_at: Option<DateTime<Utc>>,
    /// Ratifier's reasoning (free-form text captured at ratification).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ratification_reasoning: Option<String>,

    pub active_from: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// Prior `path@version` string if this policy supersedes another.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
}

impl Policy {
    /// `true` when this policy has been ratified (live).
    pub fn is_ratified(&self) -> bool {
        self.ratified_by.is_some()
    }

    /// `true` when this policy is currently active against the given
    /// clock: ratified AND `active_from <= now` AND the policy has not
    /// expired (`expires_at.is_none() || expires_at > now`).
    ///
    /// `expires_at` is an exclusive upper bound — a policy whose
    /// `expires_at == now` is already expired.
    pub fn is_currently_active(&self, now: DateTime<Utc>) -> bool {
        if !self.is_ratified() || self.active_from > now {
            return false;
        }
        !matches!(self.expires_at, Some(exp) if exp <= now)
    }

    /// Canonical `path@version` identifier.
    pub fn handle(&self) -> String {
        format!("{}@{}", self.path, self.version)
    }
}

/// One allow/deny rule.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AuthorizedAction {
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
    #[serde(default)]
    pub preconditions: Vec<String>,
}

/// One require-approval rule. `fallback` tells the caller what to do
/// while approval is pending (POLICY_V1.md §22.3).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApprovalRule {
    pub action: String,
    pub approvers: Vec<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "duration_opt"
    )]
    pub timeout: Option<Duration>,
    pub fallback: FallbackAction,
}

/// One step in a policy's procedure.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProcedureStep {
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_previous_failed: Option<String>,
}

/// Advisory severity.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    #[default]
    Low,
    Medium,
    High,
    Critical,
}

/// What the agent should do while a change is awaiting approval.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FallbackAction {
    /// Do nothing; wait for approval.
    Block,
    /// Run the named alternative action.
    PickAlternative { action: String },
    /// Pick the lowest-risk option from `ChangeProposal::alternatives`.
    LowestRiskAlternative,
    /// Leave current state unchanged; record the preferred option as
    /// a pending upgrade.
    KeepCurrentState,
    /// Delegate to another policy by path.
    DelegateTo { policy_path: String },
}

/// A proposed change evaluated against change-cost policies.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChangeProposal {
    pub action: String,
    pub agent_id: String,
    pub intent: String,
    pub preferred_option: String,
    #[serde(default)]
    pub alternatives: Vec<String>,
    #[serde(default)]
    pub tokens: Vec<String>,
    #[serde(default)]
    pub attached_fields: HashMap<String, String>,
}

impl ChangeProposal {
    pub fn new(
        action: impl Into<String>,
        agent_id: impl Into<String>,
        intent: impl Into<String>,
        preferred_option: impl Into<String>,
    ) -> Self {
        Self {
            action: action.into(),
            agent_id: agent_id.into(),
            intent: intent.into(),
            preferred_option: preferred_option.into(),
            alternatives: Vec::new(),
            tokens: Vec::new(),
            attached_fields: HashMap::new(),
        }
    }

    pub fn with_tokens<I, S>(mut self, tokens: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.tokens = tokens.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_field(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.attached_fields.insert(key.into(), value.into());
        self
    }
}

/// Result of a policy evaluation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Decision {
    Allow {
        matched_policy: String,
        #[serde(default)]
        preconditions: Vec<String>,
    },
    Deny {
        matched_policy: String,
        reason: String,
    },
    RequireApproval {
        matched_policy: String,
        approvers: Vec<String>,
        #[serde(default, with = "duration_opt")]
        timeout: Option<Duration>,
        fallback: FallbackAction,
        #[serde(default)]
        approval_task_path: Option<String>,
    },
    NoPolicyMatch,
}

/// Serde helper for `Option<Duration>` encoded as milliseconds.
mod duration_opt {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(d: &Option<Duration>, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match d {
            Some(d) => (d.as_millis() as u64).serialize(s),
            None => Option::<u64>::None.serialize(s),
        }
    }

    pub fn deserialize<'de, D>(d: D) -> Result<Option<Duration>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let opt = Option::<u64>::deserialize(d)?;
        Ok(opt.map(Duration::from_millis))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_roundtrips() {
        for s in [
            Severity::Low,
            Severity::Medium,
            Severity::High,
            Severity::Critical,
        ] {
            let j = serde_json::to_value(s).unwrap();
            let back: Severity = serde_json::from_value(j).unwrap();
            assert_eq!(s, back);
        }
    }

    #[test]
    fn fallback_action_all_variants_serialize() {
        let variants = vec![
            FallbackAction::Block,
            FallbackAction::PickAlternative {
                action: "apply_safe".into(),
            },
            FallbackAction::LowestRiskAlternative,
            FallbackAction::KeepCurrentState,
            FallbackAction::DelegateTo {
                policy_path: "infra/fallback".into(),
            },
        ];
        for v in variants {
            let json = serde_json::to_value(&v).unwrap();
            assert!(json.get("kind").is_some(), "tagged enum should emit kind");
            let back: FallbackAction = serde_json::from_value(json).unwrap();
            assert_eq!(v, back);
        }
    }

    #[test]
    fn decision_roundtrips() {
        let d = Decision::RequireApproval {
            matched_policy: "infra/k8s/pod-failing@1".into(),
            approvers: vec!["human".into()],
            timeout: Some(Duration::from_secs(3600)),
            fallback: FallbackAction::LowestRiskAlternative,
            approval_task_path: None,
        };
        let j = serde_json::to_value(&d).unwrap();
        let back: Decision = serde_json::from_value(j).unwrap();
        assert_eq!(d, back);
    }

    #[test]
    fn change_proposal_builder() {
        let p = ChangeProposal::new("promote", "agent-1", "merge option C", "spec-7")
            .with_tokens(["reindex", "downtime"])
            .with_field("estimated_downtime", "5m");
        assert_eq!(
            p.tokens,
            vec!["reindex".to_string(), "downtime".to_string()]
        );
        assert_eq!(p.attached_fields.get("estimated_downtime").unwrap(), "5m");
    }
}
