//! Error type for `TaskStore` operations.

use thiserror::Error;

use crate::types::{TaskId, TaskStatus};

#[derive(Debug, Error)]
pub enum TaskStoreError {
    #[error("plan not found: {0}")]
    PlanNotFound(String),

    #[error("summary required to close a plan")]
    SummaryRequired,

    #[error("cannot close plan '{plan}': {reason}")]
    CannotClose { plan: String, reason: String },

    #[error("plan already exists: {0}")]
    PlanAlreadyExists(String),

    #[error("task not found: {plan}/{id:?}")]
    TaskNotFound { plan: String, id: TaskId },

    #[error("invalid state transition: {from:?} -> {to:?}")]
    InvalidTransition { from: TaskStatus, to: TaskStatus },

    #[error("task is blocked by: {blockers:?}")]
    Blocked { blockers: Vec<TaskId> },

    #[error("task references blocker(s) that no longer exist in the plan: {blockers:?}")]
    BlockerNotFound { blockers: Vec<TaskId> },

    #[error("proof required for transition to done")]
    ProofRequired,

    #[error("parent task not found: {0:?}")]
    ParentNotFound(TaskId),

    #[error("parent task is itself a subtask (nesting limit is 2): {0:?}")]
    ParentIsSubtask(TaskId),

    #[error("reason required for abandonment")]
    ReasonRequired,

    #[error("invalid task id format: {0}")]
    InvalidTaskId(String),

    #[error("invalid blocker id {0:?}: blocker ids must match ^t-\\d{{1,9}}$ (e.g. \"t-007\")")]
    InvalidBlockerId(String),

    #[error("write conflict: too many concurrent writers for this plan")]
    WriteConflict,

    #[error("repository error: {0}")]
    Repo(String),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}

impl From<agentstategraph::RepoError> for TaskStoreError {
    fn from(e: agentstategraph::RepoError) -> Self {
        TaskStoreError::Repo(e.to_string())
    }
}
