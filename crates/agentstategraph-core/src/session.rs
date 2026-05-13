//! Sessions — durable records of sub-agent working contexts.
//!
//! Sessions formalize the parent-child agent relationship: a lead agent
//! delegates work by creating scoped sessions for sub-agents, each with
//! their own branch and (optionally) restricted path access.
//!
//! The type lives in `agentstategraph-core` so storage backends can
//! implement `SessionStore` without circular dependencies on the repo
//! crate. The `SessionManager` in `agentstategraph` wraps these records
//! with enforcement helpers (e.g. `check_scope`).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::intent::{AgentId, IntentId, SessionId};
use crate::namespace::Namespace;
use crate::object::ObjectId;

/// Lifecycle state of a persisted session.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SessionStatus {
    /// The session is in-flight; commits may be associated with it.
    Active,
    /// The session finished its delegated work successfully.
    Completed,
    /// The session was torn down without completing (timeout, cancel, etc).
    Abandoned,
}

impl SessionStatus {
    /// Wire-form string used by storage backends.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "Active",
            Self::Completed => "Completed",
            Self::Abandoned => "Abandoned",
        }
    }

    /// Parse the wire form. Unknown values map to `Abandoned` so a row
    /// written by a newer binary never panics an older reader.
    pub fn from_wire(s: &str) -> Self {
        match s {
            "Active" => Self::Active,
            "Completed" => Self::Completed,
            _ => Self::Abandoned,
        }
    }
}

/// A durable agent session record.
///
/// `head` tracks the current tip of the session's working branch; it's
/// updated by `SessionManager::update_head`. All other fields are
/// set at creation and frozen except for `status` / `ended_at` which
/// are updated by `end_session`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub agent_id: AgentId,
    pub working_branch: String,
    pub head: ObjectId,
    /// Who spawned this session (the parent session id, if any).
    pub parent_session: Option<SessionId>,
    /// The intent this session was created to fulfill.
    pub delegated_intent: Option<IntentId>,
    /// Who to report back to.
    pub report_to: Option<String>,
    /// Path scope restriction (if set, the agent can only modify paths
    /// under this prefix).
    pub path_scope: Option<String>,
    /// Tenant scope restriction. When set, `PolicyStore::evaluate` /
    /// `evaluate_change` consulted with a `tenant_filter` tied to this
    /// session consult only policies whose `tenant_id` matches or is
    /// `None` (global). Added in 0.7.5-beta.1 §3a. `None` = no tenant
    /// scoping, same behaviour as pre-0.7.5.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_tenant: Option<String>,
    /// Namespace scope. When set, this session's Repository will use this
    /// namespace for all ref operations, overriding the Repository's
    /// configured default namespace. `None` = use the Repository default.
    /// Added alongside the namespace primitive (§namespace).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_namespace: Option<Namespace>,
    /// Lifecycle status.
    #[serde(default = "default_status")]
    pub status: SessionStatus,
    /// When this session was created.
    pub created_at: DateTime<Utc>,
    /// When this session ended (None while Active).
    #[serde(default)]
    pub ended_at: Option<DateTime<Utc>>,
}

fn default_status() -> SessionStatus {
    SessionStatus::Active
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- SessionStatus::as_str ---

    #[test]
    fn as_str_active() {
        assert_eq!(SessionStatus::Active.as_str(), "Active");
    }

    #[test]
    fn as_str_completed() {
        assert_eq!(SessionStatus::Completed.as_str(), "Completed");
    }

    #[test]
    fn as_str_abandoned() {
        assert_eq!(SessionStatus::Abandoned.as_str(), "Abandoned");
    }

    // --- SessionStatus::from_wire ---

    #[test]
    fn from_wire_known_values() {
        assert_eq!(SessionStatus::from_wire("Active"), SessionStatus::Active);
        assert_eq!(
            SessionStatus::from_wire("Completed"),
            SessionStatus::Completed
        );
        assert_eq!(
            SessionStatus::from_wire("Abandoned"),
            SessionStatus::Abandoned
        );
    }

    #[test]
    fn from_wire_unknown_maps_to_abandoned() {
        assert_eq!(
            SessionStatus::from_wire("unknown-future-status"),
            SessionStatus::Abandoned
        );
        assert_eq!(SessionStatus::from_wire(""), SessionStatus::Abandoned);
        assert_eq!(
            SessionStatus::from_wire("active"), // wrong case
            SessionStatus::Abandoned
        );
    }

    // --- as_str → from_wire round-trip ---

    #[test]
    fn as_str_from_wire_roundtrip() {
        for s in [
            SessionStatus::Active,
            SessionStatus::Completed,
            SessionStatus::Abandoned,
        ] {
            assert_eq!(SessionStatus::from_wire(s.as_str()), s);
        }
    }

    // --- SessionStatus serialization ---

    #[test]
    fn session_status_serializes_and_deserializes() {
        for s in [
            SessionStatus::Active,
            SessionStatus::Completed,
            SessionStatus::Abandoned,
        ] {
            let j = serde_json::to_string(&s).unwrap();
            let back: SessionStatus = serde_json::from_str(&j).unwrap();
            assert_eq!(back, s);
        }
    }

    // --- Session optional fields ---

    #[test]
    fn session_optional_fields_skip_when_none() {
        let s = Session {
            id: "s1".into(),
            agent_id: "agent/test".into(),
            working_branch: "main".into(),
            head: ObjectId::hash(b"head"),
            parent_session: None,
            delegated_intent: None,
            report_to: None,
            path_scope: None,
            scope_tenant: None,
            scope_namespace: None,
            status: SessionStatus::Active,
            created_at: Utc::now(),
            ended_at: None,
        };

        let json = serde_json::to_string(&s).unwrap();
        assert!(
            !json.contains("scope_tenant"),
            "scope_tenant should be omitted when None (has skip_serializing_if)"
        );
        // ended_at uses #[serde(default)] without skip_serializing_if, so it serializes as null
        assert!(
            json.contains("ended_at"),
            "ended_at is always present in JSON (serializes as null when None)"
        );
    }

    #[test]
    fn session_round_trip_with_all_fields() {
        let s = Session {
            id: "sess-42".into(),
            agent_id: "agent/planner".into(),
            working_branch: "feature/planning".into(),
            head: ObjectId::hash(b"some-commit"),
            parent_session: Some("sess-1".into()),
            delegated_intent: Some("intent-7".into()),
            report_to: Some("lead/coordinator".into()),
            path_scope: Some("/cluster/nodes".into()),
            scope_tenant: Some("tenant-A".into()),
            scope_namespace: Some(Namespace::new("project-x").unwrap()),
            status: SessionStatus::Completed,
            created_at: Utc::now(),
            ended_at: Some(Utc::now()),
        };

        let json = serde_json::to_string(&s).unwrap();
        let restored: Session = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.id, s.id);
        assert_eq!(restored.agent_id, s.agent_id);
        assert_eq!(restored.working_branch, s.working_branch);
        assert_eq!(restored.status, SessionStatus::Completed);
        assert_eq!(restored.parent_session, s.parent_session);
        assert_eq!(restored.scope_tenant.as_deref(), Some("tenant-A"));
        assert_eq!(
            restored.scope_namespace.as_ref().map(|n| n.as_str()),
            Some("project-x")
        );
        assert!(restored.ended_at.is_some());
    }
}
