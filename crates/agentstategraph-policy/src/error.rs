//! Error type for `PolicyStore` operations.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum PolicyError {
    #[error("policy not found: {0}")]
    NotFound(String),

    #[error("policy already exists at path {0}")]
    AlreadyExists(String),

    #[error("policy is already ratified: {0}")]
    AlreadyRatified(String),

    #[error("policy is not a proposal (already ratified or superseded): {0}")]
    NotProposal(String),

    #[error("invalid policy path: {0}")]
    InvalidPath(String),

    #[error("invalid policy: {0}")]
    Invalid(String),

    #[error("repository error: {0}")]
    Repo(String),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}

impl From<agentstategraph::RepoError> for PolicyError {
    fn from(e: agentstategraph::RepoError) -> Self {
        PolicyError::Repo(e.to_string())
    }
}
