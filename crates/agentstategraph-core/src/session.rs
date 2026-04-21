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
