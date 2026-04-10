//! Intent and Authority types — the provenance metadata that makes
//! AgentStateGraph different from git.
//!
//! Every commit carries structured metadata about:
//! - Why the change was made (Intent)
//! - Who authorized it (Authority)
//! - What the lifecycle status is (IntentLifecycle)
//! - What was accomplished (Resolution)
//! - Who should be notified (NotificationPolicy)

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Unique identifier for an intent.
pub type IntentId = String;

/// Unique identifier for an agent.
pub type AgentId = String;

/// Unique identifier for a session.
pub type SessionId = String;

/// Unique identifier for a principal (human, agent, team, or policy).
pub type Principal = String;

// ---------------------------------------------------------------------------
// Intent
// ---------------------------------------------------------------------------

/// Why a state change is being made.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Intent {
    /// Unique identifier for this intent.
    pub id: IntentId,
    /// High-level category of this intent.
    pub category: IntentCategory,
    /// Human/agent-readable description of what this intent aims to accomplish.
    pub description: String,
    /// Queryable labels for filtering and search.
    pub tags: Vec<String>,
    /// If this intent was decomposed from a parent, the parent's ID.
    pub parent_intent: Option<IntentId>,
    /// Current lifecycle state.
    pub lifecycle: IntentLifecycle,
}

/// High-level categories for intents. These are queryable and filterable.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum IntentCategory {
    /// Trying an approach to evaluate it.
    Explore,
    /// Improving on a previous state.
    Refine,
    /// Correcting an error or regression.
    Fix,
    /// Reverting to a prior state.
    Rollback,
    /// Saving a known-good state.
    Checkpoint,
    /// Combining work from branches.
    Merge,
    /// Schema or structural change.
    Migrate,
    /// Application-defined category.
    Custom(String),
}

// ---------------------------------------------------------------------------
// Intent Lifecycle
// ---------------------------------------------------------------------------

/// Tracks the full arc from proposal through resolution and notification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntentLifecycle {
    /// Current status.
    pub status: IntentStatus,
    /// Agent(s) working on this intent.
    pub assigned_to: Vec<AgentId>,
    /// Filed when the intent reaches a terminal state.
    pub resolution: Option<Resolution>,
    /// Who should be notified and how.
    pub notification: Option<NotificationPolicy>,
}

/// Valid states for an intent's lifecycle.
///
/// State machine:
/// Proposed → Authorized → InProgress → Completed
///                │                │ → Failed
///                │                └─→ Blocked → InProgress
///                └─→ (rejected)                     → Failed
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum IntentStatus {
    /// Intent has been declared.
    Proposed,
    /// Authority has approved execution.
    Authorized,
    /// Agent(s) are actively working.
    InProgress,
    /// Work is done, resolution filed.
    Completed,
    /// Agent could not fulfill the intent.
    Failed,
    /// Waiting on external dependency.
    Blocked,
}

// ---------------------------------------------------------------------------
// Resolution — the "report back"
// ---------------------------------------------------------------------------

/// Filed when an intent reaches a terminal state.
/// This is the structured report that answers: "what was accomplished?"
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Resolution {
    /// Concise description of what was accomplished.
    pub summary: String,
    /// Where/why the agent diverged from the original plan.
    pub deviations: Vec<Deviation>,
    /// Commit IDs of state changes made while fulfilling this intent.
    pub commits: Vec<String>,
    /// Branches created during exploration.
    pub branches_explored: Vec<String>,
    /// Overall outcome.
    pub outcome: Outcome,
    /// Agent's self-assessed confidence in the result (0.0 to 1.0).
    pub confidence: f64,
}

/// A record of where and why the agent diverged from the plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Deviation {
    /// What was different from the plan.
    pub description: String,
    /// Why the deviation occurred.
    pub reason: String,
    /// Severity of the deviation.
    pub impact: DeviationImpact,
    /// Optional follow-up intent created to address this deviation.
    pub follow_up: Option<IntentId>,
}

/// Severity of a deviation from plan.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DeviationImpact {
    Low,
    Medium,
    High,
}

/// The overall outcome of an intent's execution.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Outcome {
    /// Intent fully satisfied.
    Fulfilled,
    /// Some aspects completed, others remain.
    PartiallyFulfilled,
    /// Could not satisfy the intent.
    Failed,
    /// Punted to a follow-up intent.
    Deferred,
}

// ---------------------------------------------------------------------------
// Notification Policy
// ---------------------------------------------------------------------------

/// Declares who should be informed about an intent's resolution,
/// at what urgency, and in what format.
///
/// AgentStateGraph does not deliver notifications directly — this is stored
/// as part of the provenance record and emitted as a structured event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NotificationPolicy {
    /// How urgent is this notification.
    pub urgency: Urgency,
    /// Principals who should be informed.
    pub audience: Vec<Principal>,
    /// Suggested format for the notification.
    pub format_hint: FormatHint,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Urgency {
    Routine,
    Priority,
    Critical,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FormatHint {
    Summary,
    Detailed,
    DiffOnly,
}

// ---------------------------------------------------------------------------
// Authority
// ---------------------------------------------------------------------------

/// Who authorized a state change, with the full delegation chain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Authority {
    /// The principal who authorized this action.
    pub principal: Principal,
    /// What was authorized.
    pub scope: AuthScope,
    /// When the authorization was granted.
    pub granted_at: DateTime<Utc>,
    /// When the authorization expires (None = no expiration).
    pub expires: Option<DateTime<Utc>>,
    /// Full authorization path from root policy to executing agent.
    pub delegation_chain: Vec<DelegationLink>,
}

/// What an authority grants permission for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AuthScope {
    /// Authorized for a specific intent.
    Intent(IntentId),
    /// Authorized for branches matching a pattern.
    Branch(String),
    /// Authorized for paths matching a pattern.
    Path(String),
    /// Full access.
    Wildcard,
    /// Application-defined scope.
    Custom(String),
}

/// A single hop in a delegation chain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DelegationLink {
    /// Delegating principal.
    pub from: Principal,
    /// Receiving principal.
    pub to: Principal,
    /// What was delegated.
    pub scope: AuthScope,
    /// When the delegation was granted.
    pub granted_at: DateTime<Utc>,
    /// When the delegation expires.
    pub expires: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// Tool Call provenance
// ---------------------------------------------------------------------------

/// A record of a tool call that contributed to a state change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// Name of the tool (e.g., "kubectl_apply", "stategraph_set").
    pub tool_name: String,
    /// Input arguments.
    pub arguments: serde_json::Value,
    /// Summary of the result (not the full output).
    pub result: Option<String>,
    /// When the tool was called.
    pub timestamp: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Convenience constructors
// ---------------------------------------------------------------------------

impl Intent {
    /// Create a new intent with minimal required fields.
    /// Lifecycle starts as Proposed.
    pub fn new(category: IntentCategory, description: impl Into<String>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            category,
            description: description.into(),
            tags: Vec::new(),
            parent_intent: None,
            lifecycle: IntentLifecycle {
                status: IntentStatus::Proposed,
                assigned_to: Vec::new(),
                resolution: None,
                notification: None,
            },
        }
    }

    /// Add tags to this intent.
    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }

    /// Set the parent intent (for decomposition / sub-intents).
    pub fn with_parent(mut self, parent_id: IntentId) -> Self {
        self.parent_intent = Some(parent_id);
        self
    }
}

impl Authority {
    /// Create a simple authority with no delegation chain.
    pub fn simple(principal: impl Into<String>) -> Self {
        Self {
            principal: principal.into(),
            scope: AuthScope::Wildcard,
            granted_at: Utc::now(),
            expires: None,
            delegation_chain: Vec::new(),
        }
    }

    /// Create an authority scoped to a specific intent.
    pub fn for_intent(principal: impl Into<String>, intent_id: IntentId) -> Self {
        Self {
            principal: principal.into(),
            scope: AuthScope::Intent(intent_id),
            granted_at: Utc::now(),
            expires: None,
            delegation_chain: Vec::new(),
        }
    }
}
