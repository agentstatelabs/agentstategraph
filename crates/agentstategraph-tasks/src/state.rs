//! Task state-machine validation.
//!
//! Valid transitions:
//!
//! ```text
//!   pending ──start──▶ in_progress ──complete(proof)──▶ done        (terminal)
//!      │                    │
//!      └───abandon(reason)──┴──▶ abandoned                          (terminal)
//! ```
//!
//! `done` and `abandoned` are terminal. Consumers who need to redo work
//! create a new task rather than reopening an old one.

use crate::error::TaskStoreError;
use crate::types::TaskStatus;

/// The transition being attempted. Used for validation and error reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    Start,
    Complete,
    Abandon,
}

impl Transition {
    pub fn target(self) -> TaskStatus {
        match self {
            Transition::Start => TaskStatus::InProgress,
            Transition::Complete => TaskStatus::Done,
            Transition::Abandon => TaskStatus::Abandoned,
        }
    }
}

/// Validate a state transition. Returns `Ok(())` if legal, else
/// `TaskStoreError::InvalidTransition`.
///
/// Blocker checks and proof-presence checks are layered on top of this
/// inside `TaskStore` — this function cares only about the current/next
/// status pair.
pub fn check_transition(from: TaskStatus, t: Transition) -> Result<(), TaskStoreError> {
    if matches!(
        (from, t),
        (TaskStatus::Pending, Transition::Start)
            | (TaskStatus::InProgress, Transition::Complete)
            | (TaskStatus::Pending, Transition::Abandon)
            | (TaskStatus::InProgress, Transition::Abandon)
    ) {
        Ok(())
    } else {
        Err(TaskStoreError::InvalidTransition {
            from,
            to: t.target(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legal_transitions() {
        check_transition(TaskStatus::Pending, Transition::Start).unwrap();
        check_transition(TaskStatus::InProgress, Transition::Complete).unwrap();
        check_transition(TaskStatus::Pending, Transition::Abandon).unwrap();
        check_transition(TaskStatus::InProgress, Transition::Abandon).unwrap();
    }

    #[test]
    fn cannot_start_in_progress() {
        let err = check_transition(TaskStatus::InProgress, Transition::Start).unwrap_err();
        assert!(matches!(err, TaskStoreError::InvalidTransition { .. }));
    }

    #[test]
    fn cannot_complete_pending() {
        assert!(check_transition(TaskStatus::Pending, Transition::Complete).is_err());
    }

    #[test]
    fn terminal_states_cannot_transition() {
        assert!(check_transition(TaskStatus::Done, Transition::Start).is_err());
        assert!(check_transition(TaskStatus::Done, Transition::Complete).is_err());
        assert!(check_transition(TaskStatus::Done, Transition::Abandon).is_err());
        assert!(check_transition(TaskStatus::Abandoned, Transition::Start).is_err());
        assert!(check_transition(TaskStatus::Abandoned, Transition::Complete).is_err());
        assert!(check_transition(TaskStatus::Abandoned, Transition::Abandon).is_err());
    }
}
