//! Public data types for plans, tasks, and proofs.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::TaskStoreError;

/// A named container of tasks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Plan {
    pub name: String,
    pub description: Option<String>,
    pub status: PlanStatus,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
    pub archived_at: Option<DateTime<Utc>>,
}

/// A unit of work inside a plan.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Task {
    pub id: TaskId,
    pub title: String,
    pub status: TaskStatus,
    pub priority: Priority,
    pub parent_id: Option<TaskId>,
    #[serde(default)]
    pub blocked_by: Vec<TaskId>,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
    pub started_at: Option<DateTime<Utc>>,
    pub started_by: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
    pub completed_by: Option<String>,
    pub proof: Option<Proof>,
    pub abandoned_at: Option<DateTime<Utc>>,
    pub abandoned_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    Active,
    Completed,
    Archived,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    InProgress,
    Done,
    Abandoned,
}

impl TaskStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, TaskStatus::Done | TaskStatus::Abandoned)
    }
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Low,
    #[default]
    Medium,
    High,
    Critical,
}

/// A task identifier, unique within a plan. Format: "t-001", "t-002", ...
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TaskId(pub String);

impl TaskId {
    /// Construct a zero-padded id from a 1-based sequence number.
    pub fn new(n: u32) -> Self {
        Self(format!("t-{:03}", n))
    }

    /// Parse the numeric suffix from a `t-NNN` identifier.
    pub fn number(&self) -> Result<u32, TaskStoreError> {
        self.0
            .strip_prefix("t-")
            .and_then(|s| s.parse::<u32>().ok())
            .ok_or_else(|| TaskStoreError::InvalidTaskId(self.0.clone()))
    }

    /// The raw string form, e.g. `"t-001"`.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Evidence attached to a `done` task.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Proof {
    pub kind: ProofKind,
    pub value: String,
    pub note: Option<String>,
}

impl Proof {
    pub fn commit(sha: impl Into<String>) -> Self {
        Self {
            kind: ProofKind::Commit,
            value: sha.into(),
            note: None,
        }
    }

    pub fn file(path: impl Into<String>) -> Self {
        Self {
            kind: ProofKind::File,
            value: path.into(),
            note: None,
        }
    }

    pub fn test(name: impl Into<String>) -> Self {
        Self {
            kind: ProofKind::Test,
            value: name.into(),
            note: None,
        }
    }

    pub fn text(value: impl Into<String>) -> Self {
        Self {
            kind: ProofKind::Text,
            value: value.into(),
            note: None,
        }
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProofKind {
    /// A git commit SHA. Verifier should check reachability.
    Commit,
    /// A file path. Verifier should check existence.
    File,
    /// A test name. Verifier should check the test exists in the suite.
    Test,
    /// Free-form human-attested description. Unverifiable.
    Text,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_id_roundtrip() {
        let id = TaskId::new(42);
        assert_eq!(id.as_str(), "t-042");
        assert_eq!(id.number().unwrap(), 42);
    }

    #[test]
    fn task_id_padding() {
        assert_eq!(TaskId::new(1).as_str(), "t-001");
        assert_eq!(TaskId::new(999).as_str(), "t-999");
        assert_eq!(TaskId::new(1000).as_str(), "t-1000");
    }

    #[test]
    fn task_id_parse_invalid() {
        assert!(TaskId("nope".to_string()).number().is_err());
        assert!(TaskId("t-abc".to_string()).number().is_err());
    }

    #[test]
    fn priority_ordering() {
        assert!(Priority::Critical > Priority::High);
        assert!(Priority::High > Priority::Medium);
        assert!(Priority::Medium > Priority::Low);
    }

    #[test]
    fn task_status_terminal() {
        assert!(TaskStatus::Done.is_terminal());
        assert!(TaskStatus::Abandoned.is_terminal());
        assert!(!TaskStatus::Pending.is_terminal());
        assert!(!TaskStatus::InProgress.is_terminal());
    }

    #[test]
    fn plan_serde_roundtrip() {
        let plan = Plan {
            name: "website-v2".to_string(),
            description: Some("Brand pivot".to_string()),
            status: PlanStatus::Active,
            created_at: Utc::now(),
            created_by: "claude-code".to_string(),
            archived_at: None,
        };
        let json = serde_json::to_value(&plan).unwrap();
        let back: Plan = serde_json::from_value(json).unwrap();
        assert_eq!(plan, back);
    }

    #[test]
    fn task_serde_roundtrip() {
        let task = Task {
            id: TaskId::new(1),
            title: "Rewrite hero".to_string(),
            status: TaskStatus::Pending,
            priority: Priority::High,
            parent_id: None,
            blocked_by: vec![TaskId::new(2)],
            created_at: Utc::now(),
            created_by: "claude-code".to_string(),
            started_at: None,
            started_by: None,
            completed_at: None,
            completed_by: None,
            proof: None,
            abandoned_at: None,
            abandoned_reason: None,
        };
        let json = serde_json::to_value(&task).unwrap();
        let back: Task = serde_json::from_value(json).unwrap();
        assert_eq!(task, back);
    }

    #[test]
    fn proof_constructors() {
        let p = Proof::commit("abc123").with_note("verified");
        assert_eq!(p.kind, ProofKind::Commit);
        assert_eq!(p.value, "abc123");
        assert_eq!(p.note.as_deref(), Some("verified"));
    }
}
