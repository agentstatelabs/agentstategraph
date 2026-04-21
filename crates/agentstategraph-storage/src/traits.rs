//! Storage trait definitions — the pluggable backend contract.
//!
//! Any backend that implements these traits can be used with AgentStateGraph.
//! The in-memory and SQLite backends are provided; custom backends
//! can be added by implementing these traits.

use agentstategraph_core::{Commit, Epoch, Object, ObjectId, Session, SessionStatus};
use chrono::{DateTime, Utc};

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

    #[error("epoch '{id}' is already sealed")]
    EpochAlreadySealed { id: String },

    #[error("session '{id}' has already ended")]
    SessionEnded { id: String },
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

/// Durable storage of epochs and their association with commits.
///
/// Epochs are the compliance-relevant unit of work. Sealing an epoch
/// records a tamper-evident timestamp and summary; a backend that loses
/// sealed epochs across restart defeats the audit story.
pub trait EpochStore: Send + Sync {
    /// Insert a newly-created epoch. If an epoch with the same id already
    /// exists, implementations may return `StorageError::Backend` — the
    /// repo layer is expected to guard uniqueness.
    fn create_epoch(&self, epoch: &Epoch) -> Result<(), StorageError>;

    /// Seal an epoch: flip its status to `Sealed`, stamp `sealed_at`,
    /// record the seal summary, and persist the set of commits that
    /// must remain reachable for seal-violation enforcement (V8).
    /// Subsequent `set_commit_epoch` calls referencing this id must
    /// fail with `EpochAlreadySealed`.
    fn seal_epoch(
        &self,
        id: &str,
        summary: &str,
        sealed_at: DateTime<Utc>,
        sealed_commits: &[ObjectId],
    ) -> Result<(), StorageError>;

    /// List all epochs, most-recent first.
    fn list_epochs(&self) -> Result<Vec<Epoch>, StorageError>;

    /// Fetch a single epoch by id.
    fn get_epoch(&self, id: &str) -> Result<Option<Epoch>, StorageError>;

    /// Associate a commit with the given epoch. Must reject if the epoch
    /// is already sealed.
    fn set_commit_epoch(&self, commit_id: &ObjectId, epoch_id: &str) -> Result<(), StorageError>;
}

/// Durable storage of agent sessions and their association with commits.
pub trait SessionStore: Send + Sync {
    /// Insert a newly-created session record.
    fn create_session(&self, session: &Session) -> Result<(), StorageError>;

    /// End a session: update its status and stamp `ended_at`.
    fn end_session(
        &self,
        id: &str,
        status: SessionStatus,
        ended_at: DateTime<Utc>,
    ) -> Result<(), StorageError>;

    /// List sessions, optionally filtered to those belonging to a
    /// specific agent id.
    fn list_sessions(&self, agent_filter: Option<&str>) -> Result<Vec<Session>, StorageError>;

    /// Fetch a single session by id.
    fn get_session(&self, id: &str) -> Result<Option<Session>, StorageError>;

    /// Associate a commit with the given session. Must reject if the
    /// session has already ended.
    fn set_commit_session(
        &self,
        commit_id: &ObjectId,
        session_id: &str,
    ) -> Result<(), StorageError>;
}

/// Combined storage trait for convenience.
/// A backend that implements all five sub-traits.
pub trait Storage: ObjectStore + CommitStore + RefStore + EpochStore + SessionStore {}

/// Blanket implementation: anything implementing all five traits is a Storage.
impl<T: ObjectStore + CommitStore + RefStore + EpochStore + SessionStore> Storage for T {}
