//! Storage trait definitions — the pluggable backend contract.
//!
//! Any backend that implements these traits can be used with StateGraph.
//! The in-memory and SQLite backends are provided; custom backends
//! can be added by implementing these traits.

use agentstategraph_core::{Commit, Object, ObjectId};

/// Errors from storage operations.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("object not found: {0}")]
    ObjectNotFound(String),

    #[error("commit not found: {0}")]
    CommitNotFound(String),

    #[error("ref not found: {0}")]
    RefNotFound(String),

    #[error("CAS conflict: ref '{name}' expected {expected}, found {actual}")]
    CasConflict {
        name: String,
        expected: String,
        actual: String,
    },

    #[error("duplicate ref: {0}")]
    DuplicateRef(String),

    #[error("storage backend error: {0}")]
    Backend(String),

    #[error("serialization error: {0}")]
    Serialization(String),
}

/// Content-addressed object storage.
/// Objects are stored and retrieved by their BLAKE3 hash (ObjectId).
pub trait ObjectStore: Send + Sync {
    /// Retrieve an object by its ID. Returns None if not found.
    fn get_object(&self, id: &ObjectId) -> Result<Option<Object>, StorageError>;

    /// Store an object. Returns its ObjectId (which is its content hash).
    /// Storing an object that already exists is a no-op (idempotent).
    fn put_object(&self, obj: &Object) -> Result<ObjectId, StorageError>;

    /// Check if an object exists in the store.
    fn has_object(&self, id: &ObjectId) -> Result<bool, StorageError>;

    /// Retrieve multiple objects at once. Returns None for missing objects.
    fn batch_get_objects(&self, ids: &[ObjectId]) -> Result<Vec<Option<Object>>, StorageError> {
        // Default implementation: sequential gets. Backends can optimize.
        ids.iter().map(|id| self.get_object(id)).collect()
    }

    /// Store multiple objects at once. Returns their ObjectIds.
    fn batch_put_objects(&self, objs: &[Object]) -> Result<Vec<ObjectId>, StorageError> {
        // Default implementation: sequential puts. Backends can optimize.
        objs.iter().map(|obj| self.put_object(obj)).collect()
    }
}

/// Commit storage. Commits are also content-addressed but stored
/// separately from objects for efficient history queries.
pub trait CommitStore: Send + Sync {
    /// Retrieve a commit by its ID.
    fn get_commit(&self, id: &ObjectId) -> Result<Option<Commit>, StorageError>;

    /// Store a commit.
    fn put_commit(&self, commit: &Commit) -> Result<(), StorageError>;

    /// Check if a commit exists.
    fn has_commit(&self, id: &ObjectId) -> Result<bool, StorageError>;

    /// List commits reachable from a given commit, in reverse chronological order.
    /// Returns at most `limit` commits.
    fn list_commits(&self, from: &ObjectId, limit: usize) -> Result<Vec<Commit>, StorageError>;
}

/// Named ref management with atomic compare-and-swap.
/// Refs are named pointers to commit IDs (branches, tags, heads).
pub trait RefStore: Send + Sync {
    /// Get the commit ID a ref points to. Returns None if the ref doesn't exist.
    fn get_ref(&self, name: &str) -> Result<Option<ObjectId>, StorageError>;

    /// Set a ref to point to a commit. Creates the ref if it doesn't exist.
    fn set_ref(&self, name: &str, target: ObjectId) -> Result<(), StorageError>;

    /// Atomic compare-and-swap on a ref.
    /// Updates the ref only if it currently points to `expected`.
    /// Returns true if the swap succeeded, false if the ref's current value
    /// didn't match `expected`.
    fn cas_ref(&self, name: &str, expected: ObjectId, new: ObjectId) -> Result<bool, StorageError>;

    /// List all refs matching a prefix.
    fn list_refs(&self, prefix: &str) -> Result<Vec<(String, ObjectId)>, StorageError>;

    /// Delete a ref. Returns true if the ref existed.
    fn delete_ref(&self, name: &str) -> Result<bool, StorageError>;
}

/// Combined storage trait for convenience.
/// A backend that implements all three sub-traits.
pub trait Storage: ObjectStore + CommitStore + RefStore {}

/// Blanket implementation: anything implementing all three traits is a Storage.
impl<T: ObjectStore + CommitStore + RefStore> Storage for T {}
