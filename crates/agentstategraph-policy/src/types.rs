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

    /// Detached signature over the canonical-JSON bytes of this policy
    /// (with the `signature` field itself excluded from canonicalization).
    /// Optional on every policy — unsigned policies remain valid unless
    /// the server sets `require_signed_policies` (§2c) and a verifier
    /// has been registered on the `PolicyStore`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<PolicySignature>,

    /// Tenant identifier for namespace isolation (0.7.5 §3). `None`
    /// means the policy is a "global" policy that applies to every
    /// tenant; `Some(id)` means the policy only applies to callers
    /// that pass a matching `tenant_filter` into `evaluate` /
    /// `evaluate_change`. See POLICY_V1.md §17 and ROADMAP D3 for
    /// the cheap-namespace-discriminator rationale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,

    /// External evaluator escape-hatch (0.7.5 §4). When `Some(_)` and
    /// the `PolicyStore` has a matching runner registered via
    /// `with_external_evaluators`, the dispatcher routes the policy to
    /// the external rule engine (Rego / Cedar / WASM) instead of the
    /// local evaluator. Policies referencing a runner kind that is not
    /// registered are treated as not-matching. See POLICY_V1.md §18
    /// and ROADMAP D4.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_evaluator: Option<ExternalEvaluatorRef>,
}

/// Reference to an external policy evaluator (0.7.5 §4). Tagged serde
/// union keyed by `kind`; each variant carries an [`EvaluatorSource`]
/// that the dispatcher resolves at evaluation time.
///
/// Concrete runners live in optional sibling crates
/// (`agentstategraph-policy-wasm` / `-rego` / `-cedar` — §4b). The
/// main policy crate only carries the ref + trait; unregistered kinds
/// cause the policy to be skipped during evaluation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExternalEvaluatorRef {
    Rego { source: EvaluatorSource },
    Cedar { source: EvaluatorSource },
    Wasm { source: EvaluatorSource },
}

/// Source location for the body of an external evaluator (0.7.5 §4).
/// Tagged serde union keyed by `kind`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EvaluatorSource {
    /// Rego / Cedar / WASM source embedded directly in the policy.
    Inline { body: String },
    /// Absolute or server-relative filesystem path.
    FilePath { path: std::path::PathBuf },
    /// A path within the state tree on the same repo — the dispatcher
    /// loads the value stored there and treats it as the evaluator
    /// source.
    CommitRef { path: String },
}

/// Detached signature carried on a [`Policy`]. Tagged serde union keyed
/// by `algorithm`; Ed25519 is the only variant shipped in 0.7.5.
///
/// The concrete verifier lives in `agentstategraph-policy-sign`
/// (`Ed25519Verifier`); the main policy crate only stores + passes
/// through the payload so unsigned policies don't pay the crypto
/// dep cost.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "algorithm", rename_all = "snake_case")]
pub enum PolicySignature {
    Ed25519 {
        /// Identifier of the key in the verifier's registry.
        signer_key_id: String,
        /// 128-character lowercase hex string (64 bytes decoded).
        signature_hex: String,
    },
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

    // --- Policy lifecycle helpers ---

    fn base_policy(path: &str) -> Policy {
        use crate::selector::Selector;
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
            severity: Severity::default(),
            proposed_by: "claude".into(),
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
    fn is_ratified_true_when_ratified_by_set() {
        let mut p = base_policy("a");
        p.ratified_by = Some("alice".into());
        assert!(p.is_ratified());
    }

    #[test]
    fn is_ratified_false_when_none() {
        assert!(!base_policy("a").is_ratified());
    }

    #[test]
    fn is_currently_active_requires_ratification() {
        let p = base_policy("a");
        assert!(!p.is_currently_active(Utc::now()));
    }

    #[test]
    fn is_currently_active_true_when_ratified_and_in_window() {
        use chrono::Duration;
        let mut p = base_policy("a");
        p.ratified_by = Some("alice".into());
        p.active_from = Utc::now() - Duration::hours(1);
        assert!(p.is_currently_active(Utc::now()));
    }

    #[test]
    fn is_currently_active_false_when_future_active_from() {
        use chrono::Duration;
        let mut p = base_policy("a");
        p.ratified_by = Some("alice".into());
        p.active_from = Utc::now() + Duration::hours(1);
        assert!(!p.is_currently_active(Utc::now()));
    }

    #[test]
    fn is_currently_active_false_when_expired() {
        use chrono::Duration;
        let mut p = base_policy("a");
        p.ratified_by = Some("alice".into());
        p.active_from = Utc::now() - Duration::hours(2);
        p.expires_at = Some(Utc::now() - Duration::hours(1)); // expired one hour ago
        assert!(!p.is_currently_active(Utc::now()));
    }

    #[test]
    fn is_currently_active_false_when_expires_at_equals_now() {
        // expires_at is exclusive upper bound
        let now = Utc::now();
        let mut p = base_policy("a");
        p.ratified_by = Some("alice".into());
        p.active_from = now;
        p.expires_at = Some(now);
        assert!(!p.is_currently_active(now));
    }

    #[test]
    fn is_currently_active_true_just_before_expiry() {
        use chrono::Duration;
        let now = Utc::now();
        let mut p = base_policy("a");
        p.ratified_by = Some("alice".into());
        p.active_from = now - Duration::hours(1);
        p.expires_at = Some(now + Duration::seconds(1));
        assert!(p.is_currently_active(now));
    }

    #[test]
    fn policy_handle_format() {
        let mut p = base_policy("infra/k8s/pod-failing");
        p.version = 3;
        assert_eq!(p.handle(), "infra/k8s/pod-failing@3");
    }

    // --- ExternalEvaluatorRef + EvaluatorSource ---

    #[test]
    fn external_evaluator_ref_all_variants_roundtrip() {
        use std::path::PathBuf;
        let sources = vec![
            EvaluatorSource::Inline { body: "package p\ndefault allow = false".into() },
            EvaluatorSource::FilePath { path: PathBuf::from("/policies/deny.rego") },
            EvaluatorSource::CommitRef { path: "/_policy/rego/main".into() },
        ];
        for src in sources {
            let refs = vec![
                ExternalEvaluatorRef::Rego { source: src.clone() },
                ExternalEvaluatorRef::Cedar { source: src.clone() },
                ExternalEvaluatorRef::Wasm { source: src.clone() },
            ];
            for r in refs {
                let j = serde_json::to_value(&r).unwrap();
                assert!(j.get("kind").is_some());
                let back: ExternalEvaluatorRef = serde_json::from_value(j).unwrap();
                assert_eq!(r, back);
            }
        }
    }

    #[test]
    fn policy_signature_roundtrip() {
        let sig = PolicySignature::Ed25519 {
            signer_key_id: "key-001".into(),
            signature_hex: "a".repeat(128),
        };
        let j = serde_json::to_value(&sig).unwrap();
        assert_eq!(j["algorithm"], "ed25519");
        let back: PolicySignature = serde_json::from_value(j).unwrap();
        assert_eq!(sig, back);
    }

    #[test]
    fn approval_rule_timeout_roundtrips_as_millis() {
        let rule = ApprovalRule {
            action: "deploy".into(),
            approvers: vec!["ops".into()],
            timeout: Some(Duration::from_secs(7200)),
            fallback: FallbackAction::Block,
        };
        let j = serde_json::to_value(&rule).unwrap();
        assert_eq!(j["timeout"], 7_200_000u64);
        let back: ApprovalRule = serde_json::from_value(j).unwrap();
        assert_eq!(back.timeout, Some(Duration::from_secs(7200)));
    }

    #[test]
    fn approval_rule_no_timeout_omitted_from_json() {
        let rule = ApprovalRule {
            action: "deploy".into(),
            approvers: vec![],
            timeout: None,
            fallback: FallbackAction::Block,
        };
        let j = serde_json::to_string(&rule).unwrap();
        assert!(!j.contains("timeout"), "None timeout should be omitted: {j}");
    }

    #[test]
    fn authorized_action_optional_condition_omitted_when_none() {
        let a = AuthorizedAction {
            action: "restart".into(),
            condition: None,
            preconditions: vec![],
        };
        let j = serde_json::to_string(&a).unwrap();
        assert!(!j.contains("\"condition\""), "None condition should be omitted: {j}");
    }

    #[test]
    fn severity_default_is_low() {
        assert_eq!(Severity::default(), Severity::Low);
    }

    #[test]
    fn change_proposal_defaults() {
        let p = ChangeProposal::new("act", "agent", "intent", "preferred");
        assert!(p.tokens.is_empty());
        assert!(p.attached_fields.is_empty());
        assert!(p.alternatives.is_empty());
    }
}
