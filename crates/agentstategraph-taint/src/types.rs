//! Core data types for the taint substrate.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A taint record. Persisted to the `taints` table in storage and
/// round-tripped across the MCP / FFI / binding surfaces.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Taint {
    /// UUID string generated at creation.
    pub id: String,
    /// Path the taint applies to (e.g. `/cluster/nodes/picoup2`).
    pub path: String,
    /// Human-readable taint name (e.g. `disk-pressure`,
    /// `crash-loop`, `security-review`).
    pub name: String,
    /// Discriminator between taint / quarantine / watch.
    pub kind: TaintKind,
    /// How the pre-commit hook should behave when this taint
    /// matches an incoming write.
    pub effect: TaintEffect,
    /// Advisory severity (does not change pre-commit semantics).
    pub severity: TaintSeverity,
    /// Why the taint was applied. Free-form.
    pub reason: String,
    /// Agent that applied the taint.
    pub agent_id: String,
    /// Commit id of the `Taint` / `Quarantine` / `Watch` intent
    /// commit that created this record. Empty until the commit is
    /// written (the repository patches it post-commit).
    #[serde(default)]
    pub commit_id: String,
    /// When the taint was created.
    pub created_at: DateTime<Utc>,
    /// Optional auto-expiry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// Set when resolved via `untaint` / `unquarantine` / `unwatch`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<DateTime<Utc>>,
    /// Agent that resolved the taint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_by: Option<String>,
    /// Reason given at resolution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_reason: Option<String>,
    /// Optional evidence (e.g. a remediation commit id) proving
    /// the issue is resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_proof: Option<String>,
    /// When true, the taint applies to all descendant paths of
    /// `path`.
    pub propagate: bool,
    /// Kind-specific metadata: `authorized_agents` for quarantines,
    /// `metric` / `threshold` / `direction` for watches, etc.
    #[serde(default)]
    pub metadata: TaintMetadata,
}

impl Taint {
    /// `true` when the taint is currently active against `now`:
    /// not resolved and not yet expired.
    pub fn is_active(&self, now: DateTime<Utc>) -> bool {
        if self.resolved_at.is_some() {
            return false;
        }
        match self.expires_at {
            Some(exp) => exp > now,
            None => true,
        }
    }

    /// Authorized-agents allowlist parsed from `metadata`. Applies
    /// to `Quarantine` kind; empty for others.
    pub fn authorized_agents(&self) -> Vec<String> {
        self.metadata
            .0
            .get("authorized_agents")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Discriminator between the three taint kinds.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum TaintKind {
    Taint,
    Quarantine,
    Watch,
}

/// Pre-commit-hook behavior for this taint.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum TaintEffect {
    /// Attach a warning to the commit metadata but allow the write.
    Warn,
    /// Reject the write with [`TaintError::Blocked`].
    Block,
    /// Require confidence >= 0.9; reject lower-confidence writes.
    Review,
    /// Allow the write; flag the path for query / search filtering.
    Isolate,
    /// Watch kind only — purely advisory.
    Advisory,
}

/// Advisory severity. Does not change pre-commit semantics; used
/// by consumers for triage ordering.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum TaintSeverity {
    Low,
    #[default]
    Medium,
    High,
    Critical,
}

/// Flat string-to-JSON map for kind-specific metadata. Wrapped so
/// we can control the on-disk shape without bleeding
/// `HashMap<String, serde_json::Value>` through every API.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(transparent)]
pub struct TaintMetadata(pub HashMap<String, serde_json::Value>);

impl TaintMetadata {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(
        &mut self,
        key: impl Into<String>,
        value: impl Into<serde_json::Value>,
    ) -> &mut Self {
        self.0.insert(key.into(), value.into());
        self
    }

    pub fn get(&self, key: &str) -> Option<&serde_json::Value> {
        self.0.get(key)
    }
}

/// Result of [`crate::evaluate_access`]. Consumed by the pre-commit
/// hook + `policy_evaluate_change` integration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaintCheck {
    pub tainted: bool,
    pub quarantined: bool,
    pub watched: bool,
    pub taints: Vec<Taint>,
    pub quarantines: Vec<Taint>,
    pub watches: Vec<Taint>,
    /// Given the current taints + quarantines + supplied
    /// `agent_id` + `confidence`, is writing allowed?
    pub can_write: bool,
    /// Minimum confidence the caller must supply to pass the
    /// pre-commit hook. `0.0` when no review-effect taint applies.
    pub required_confidence: f64,
    /// Union of `authorized_agents` across all active quarantines
    /// on this path. Empty when no quarantine applies.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub authorized_agents: Vec<String>,
    /// When any `Isolate`-effect taint applies, the path should be
    /// omitted from search / query / `get_tree` unless the caller
    /// explicitly opts in.
    pub isolated: bool,
}

impl TaintCheck {
    pub fn clear() -> Self {
        Self {
            tainted: false,
            quarantined: false,
            watched: false,
            taints: Vec::new(),
            quarantines: Vec::new(),
            watches: Vec::new(),
            can_write: true,
            required_confidence: 0.0,
            authorized_agents: Vec::new(),
            isolated: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Parameter bundles for the Repository surface (§4).
// ---------------------------------------------------------------------------

/// Parameters for `Repository::taint`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaintParams {
    pub name: String,
    pub effect: TaintEffect,
    pub reason: String,
    #[serde(default)]
    pub severity: TaintSeverity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default = "default_true")]
    pub propagate: bool,
    #[serde(default)]
    pub metadata: TaintMetadata,
    pub agent_id: String,
}

/// Parameters for `Repository::untaint`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UntaintParams {
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proof: Option<String>,
    pub agent_id: String,
}

/// Parameters for `Repository::quarantine`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuarantineParams {
    pub name: String,
    pub reason: String,
    #[serde(default)]
    pub severity: TaintSeverity,
    pub authorized_agents: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default = "default_true")]
    pub propagate: bool,
    pub agent_id: String,
}

/// Parameters for `Repository::unquarantine`. Same shape as untaint.
pub type UnquarantineParams = UntaintParams;

/// Parameters for `Repository::watch`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WatchParams {
    pub name: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metric: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold: Option<f64>,
    #[serde(default = "default_watch_direction")]
    pub direction: WatchDirection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_interval_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub severity: TaintSeverity,
    #[serde(default = "default_true")]
    pub propagate: bool,
    pub agent_id: String,
}

/// Parameters for `Repository::unwatch`. Watches are lightweight so
/// reason is optional.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UnwatchParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub agent_id: String,
}

/// Direction a watch threshold fires in.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum WatchDirection {
    #[default]
    Above,
    Below,
}

fn default_true() -> bool {
    true
}

fn default_watch_direction() -> WatchDirection {
    WatchDirection::Above
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taint_kind_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&TaintKind::Taint).unwrap(),
            "\"taint\""
        );
        assert_eq!(
            serde_json::to_string(&TaintKind::Quarantine).unwrap(),
            "\"quarantine\""
        );
        assert_eq!(
            serde_json::to_string(&TaintKind::Watch).unwrap(),
            "\"watch\""
        );
    }

    #[test]
    fn taint_effect_round_trips() {
        for e in [
            TaintEffect::Warn,
            TaintEffect::Block,
            TaintEffect::Review,
            TaintEffect::Isolate,
            TaintEffect::Advisory,
        ] {
            let j = serde_json::to_value(e).unwrap();
            let back: TaintEffect = serde_json::from_value(j).unwrap();
            assert_eq!(e, back);
        }
    }

    #[test]
    fn is_active_honors_resolved_and_expired() {
        let base = Taint {
            id: "t1".into(),
            path: "/x".into(),
            name: "n".into(),
            kind: TaintKind::Taint,
            effect: TaintEffect::Warn,
            severity: TaintSeverity::Low,
            reason: "r".into(),
            agent_id: "a".into(),
            commit_id: String::new(),
            created_at: Utc::now(),
            expires_at: None,
            resolved_at: None,
            resolved_by: None,
            resolved_reason: None,
            resolved_proof: None,
            propagate: true,
            metadata: TaintMetadata::new(),
        };
        assert!(base.is_active(Utc::now()));

        let mut expired = base.clone();
        expired.expires_at = Some(Utc::now() - chrono::Duration::seconds(10));
        assert!(!expired.is_active(Utc::now()));

        let mut resolved = base.clone();
        resolved.resolved_at = Some(Utc::now() - chrono::Duration::seconds(1));
        assert!(!resolved.is_active(Utc::now()));
    }

    #[test]
    fn authorized_agents_parses_metadata() {
        let mut t = Taint {
            id: "q1".into(),
            path: "/x".into(),
            name: "sec".into(),
            kind: TaintKind::Quarantine,
            effect: TaintEffect::Block,
            severity: TaintSeverity::High,
            reason: "r".into(),
            agent_id: "a".into(),
            commit_id: String::new(),
            created_at: Utc::now(),
            expires_at: None,
            resolved_at: None,
            resolved_by: None,
            resolved_reason: None,
            resolved_proof: None,
            propagate: true,
            metadata: TaintMetadata::new(),
        };
        t.metadata.insert(
            "authorized_agents",
            serde_json::json!(["agent/security", "human/sre-lead"]),
        );
        assert_eq!(
            t.authorized_agents(),
            vec!["agent/security".to_string(), "human/sre-lead".to_string()]
        );
    }
}
