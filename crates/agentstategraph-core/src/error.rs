//! Error types for agentstategraph-core.

use crate::path::PathError;

/// Errors that can occur in agentstategraph-core operations.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("object not found: {0}")]
    ObjectNotFound(String),

    #[error("invalid path: {0}")]
    InvalidPath(#[from] PathError),

    #[error("path does not exist in state tree: {0}")]
    PathNotFound(String),

    #[error("type mismatch at path {path}: expected {expected}, found {found}")]
    TypeMismatch {
        path: String,
        expected: String,
        found: String,
    },

    #[error("index {index} out of bounds for list of length {length}")]
    IndexOutOfBounds { index: usize, length: usize },

    #[error("cannot navigate into atom value at path: {0}")]
    CannotNavigateAtom(String),

    #[error("serialization error: {0}")]
    Serialization(String),
}
