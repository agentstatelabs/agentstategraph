//! Error types for the reminders crate.

use thiserror::Error;

use crate::types::ReminderStatus;

#[derive(Debug, Error)]
pub enum ReminderError {
    #[error("reminder not found: {0}")]
    NotFound(String),

    #[error("reminder {0} is already in a terminal state ({1:?}) and cannot be modified")]
    AlreadyTerminal(String, ReminderStatus),

    #[error("invalid status transition for reminder {id}: {from:?} → {to:?}")]
    InvalidTransition {
        id: String,
        from: ReminderStatus,
        to: ReminderStatus,
    },

    #[error("reminder {0} requires approval before execution (autonomous=false)")]
    RequiresApproval(String),

    #[error("storage error: {0}")]
    Store(String),
}
