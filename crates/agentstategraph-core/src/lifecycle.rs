//! Intent lifecycle state machine — enforces valid transitions.
//!
//! Proposed → Authorized → InProgress → Completed
//!                │                │ → Failed
//!                │                └─→ Blocked → InProgress
//!                └─→ (rejected)                     → Failed

use crate::intent::{IntentLifecycle, IntentStatus, Resolution};

/// Errors from lifecycle transitions.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum LifecycleError {
    #[error("invalid transition from {from:?} to {to:?}")]
    InvalidTransition {
        from: IntentStatus,
        to: IntentStatus,
    },
    #[error("resolution required to transition to {0:?}")]
    ResolutionRequired(IntentStatus),
}

/// Check if a lifecycle transition is valid.
pub fn is_valid_transition(from: &IntentStatus, to: &IntentStatus) -> bool {
    matches!(
        (from, to),
        (IntentStatus::Proposed, IntentStatus::Authorized)
            | (IntentStatus::Proposed, IntentStatus::InProgress) // skip auth for simple cases
            | (IntentStatus::Authorized, IntentStatus::InProgress)
            | (IntentStatus::InProgress, IntentStatus::Completed)
            | (IntentStatus::InProgress, IntentStatus::Failed)
            | (IntentStatus::InProgress, IntentStatus::Blocked)
            | (IntentStatus::Blocked, IntentStatus::InProgress)
            | (IntentStatus::Blocked, IntentStatus::Failed)
    )
}

/// Transition an intent lifecycle to a new status.
/// Validates the transition and enforces requirements (e.g., resolution for terminal states).
pub fn transition(
    lifecycle: &mut IntentLifecycle,
    to: IntentStatus,
    resolution: Option<Resolution>,
) -> Result<(), LifecycleError> {
    if !is_valid_transition(&lifecycle.status, &to) {
        return Err(LifecycleError::InvalidTransition {
            from: lifecycle.status.clone(),
            to,
        });
    }

    // Terminal states require a resolution
    match &to {
        IntentStatus::Completed | IntentStatus::Failed => {
            if resolution.is_none() && lifecycle.resolution.is_none() {
                return Err(LifecycleError::ResolutionRequired(to));
            }
            if let Some(res) = resolution {
                lifecycle.resolution = Some(res);
            }
        }
        _ => {}
    }

    lifecycle.status = to;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intent::{Outcome, Resolution};

    fn test_lifecycle() -> IntentLifecycle {
        IntentLifecycle {
            status: IntentStatus::Proposed,
            assigned_to: vec![],
            resolution: None,
            notification: None,
        }
    }

    fn test_resolution() -> Resolution {
        Resolution {
            summary: "Done".to_string(),
            deviations: vec![],
            commits: vec![],
            branches_explored: vec![],
            outcome: Outcome::Fulfilled,
            confidence: 0.9,
        }
    }

    #[test]
    fn test_valid_happy_path() {
        let mut lc = test_lifecycle();
        assert!(transition(&mut lc, IntentStatus::Authorized, None).is_ok());
        assert!(transition(&mut lc, IntentStatus::InProgress, None).is_ok());
        assert!(transition(&mut lc, IntentStatus::Completed, Some(test_resolution())).is_ok());
        assert_eq!(lc.status, IntentStatus::Completed);
        assert!(lc.resolution.is_some());
    }

    #[test]
    fn test_skip_auth() {
        let mut lc = test_lifecycle();
        assert!(transition(&mut lc, IntentStatus::InProgress, None).is_ok());
    }

    #[test]
    fn test_blocked_and_resume() {
        let mut lc = test_lifecycle();
        transition(&mut lc, IntentStatus::InProgress, None).unwrap();
        transition(&mut lc, IntentStatus::Blocked, None).unwrap();
        transition(&mut lc, IntentStatus::InProgress, None).unwrap();
        assert_eq!(lc.status, IntentStatus::InProgress);
    }

    #[test]
    fn test_invalid_transition() {
        let mut lc = test_lifecycle();
        assert!(transition(&mut lc, IntentStatus::Completed, Some(test_resolution())).is_err());
    }

    #[test]
    fn test_completed_requires_resolution() {
        let mut lc = test_lifecycle();
        transition(&mut lc, IntentStatus::InProgress, None).unwrap();
        assert!(transition(&mut lc, IntentStatus::Completed, None).is_err());
    }

    #[test]
    fn test_failed_requires_resolution() {
        let mut lc = test_lifecycle();
        transition(&mut lc, IntentStatus::InProgress, None).unwrap();
        assert!(transition(&mut lc, IntentStatus::Failed, None).is_err());
    }

    #[test]
    fn test_cannot_go_backwards() {
        let mut lc = test_lifecycle();
        transition(&mut lc, IntentStatus::Authorized, None).unwrap();
        transition(&mut lc, IntentStatus::InProgress, None).unwrap();
        assert!(transition(&mut lc, IntentStatus::Proposed, None).is_err());
        assert!(transition(&mut lc, IntentStatus::Authorized, None).is_err());
    }
}
