//! Error enum for the taint substrate. Surfaced through the
//! Repository methods (`taint` / `untaint` / `quarantine` / ...) and
//! the pre-commit hook.

use thiserror::Error;

#[derive(Debug, Error, PartialEq)]
pub enum TaintError {
    /// Pre-commit hook rejected a write because the path carries a
    /// `Block`-effect taint.
    #[error("path {path} is blocked by taint {taint}: {reason}")]
    Blocked {
        path: String,
        taint: String,
        reason: String,
    },

    /// Pre-commit hook rejected a write because the path is
    /// quarantined and the acting agent is not in the quarantine's
    /// `authorized_agents` list.
    #[error("path {path} is quarantined; agent {agent_id} is not authorized")]
    NotAuthorized { path: String, agent_id: String },

    /// Pre-commit hook rejected a write because the path carries a
    /// `Review`-effect taint and the commit's `confidence` is below
    /// the required threshold (default 0.9).
    #[error("review taint {taint} on {path} requires confidence >= {required}; got {got}")]
    InsufficientConfidence {
        path: String,
        taint: String,
        required: f64,
        got: f64,
    },

    /// A `taint` / `untaint` / `resolve_taint` call referred to an
    /// id that does not exist.
    #[error("taint {0} not found")]
    NotFound(String),

    /// Attempted to resolve a taint that is already resolved.
    #[error("taint {0} is already resolved")]
    AlreadyResolved(String),

    /// Storage-backend error; the message is opaque to the taint
    /// crate (the repository wrapper surfaces the real error).
    #[error("storage: {0}")]
    Storage(String),

    /// A proposed taint failed basic validation (empty path, empty
    /// name, unknown effect for the chosen kind, etc.).
    #[error("invalid taint: {0}")]
    InvalidTaint(String),
}
