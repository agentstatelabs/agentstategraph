//! Storage trait definitions — the pluggable backend contract.
//!
//! Any backend that implements these traits can be used with AgentStateGraph.
//! The in-memory and SQLite backends are provided; custom backends
//! can be added by implementing these traits.

use agentstategraph_core::{Commit, Epoch, Namespace, Object, ObjectId, Session, SessionStatus};
use agentstategraph_reminders::ReminderStore;
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

    #[error("namespace not found: '{0}' (create it with create_namespace before writing refs)")]
    NamespaceNotFound(String),

    #[error("namespace already exists: '{0}'")]
    NamespaceAlreadyExists(String),

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

    #[error("invalid operation: {0}")]
    InvalidOperation(String),
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

    /// Return the ids of every commit in the store, unordered.
    ///
    /// Unlike [`CommitStore::list_commits`] this is NOT reachability-scoped: it
    /// includes commits that no ref currently points at (e.g. the tip of a
    /// deleted branch). Used for prefix-based ref resolution and for recovering
    /// orphaned historical commits.
    fn all_commit_ids(&self) -> Result<Vec<ObjectId>, StorageError>;
}

/// Named ref management with atomic compare-and-swap.
///
/// All ref operations are scoped to a [`Namespace`]. The storage layer
/// enforces that refs in different namespaces are completely independent:
/// `get_ref(ns_a, "main")` and `get_ref(ns_b, "main")` are distinct rows.
///
/// Namespaces must be explicitly created with [`RefStore::create_namespace`]
/// before any ref can be written into them. `set_ref` returns
/// [`StorageError::NamespaceNotFound`] if the namespace does not exist.
pub trait RefStore: Send + Sync {
    /// Create a namespace. Returns `NamespaceAlreadyExists` if it already
    /// exists — callers that want idempotent creation should check first or
    /// ignore that variant.
    fn create_namespace(&self, namespace: &Namespace) -> Result<(), StorageError>;

    /// List all known namespaces.
    fn list_namespaces(&self) -> Result<Vec<Namespace>, StorageError>;

    /// Get the commit ID a ref points to. Returns None if the ref doesn't
    /// exist. Returns `NamespaceNotFound` if the namespace doesn't exist.
    fn get_ref(&self, namespace: &Namespace, name: &str) -> Result<Option<ObjectId>, StorageError>;

    /// Set a ref to point to a commit. Creates the ref if it doesn't exist
    /// but the namespace must already exist. Returns `NamespaceNotFound` if
    /// the namespace hasn't been created yet.
    fn set_ref(
        &self,
        namespace: &Namespace,
        name: &str,
        target: ObjectId,
    ) -> Result<(), StorageError>;

    /// Atomic compare-and-swap on a ref.
    /// Updates the ref only if it currently points to `expected`.
    /// Returns `true` if the swap succeeded, `false` if the current value
    /// didn't match `expected`. Returns `NamespaceNotFound` if the namespace
    /// doesn't exist.
    fn cas_ref(
        &self,
        namespace: &Namespace,
        name: &str,
        expected: ObjectId,
        new: ObjectId,
    ) -> Result<bool, StorageError>;

    /// List all refs in `namespace` whose name starts with `prefix`.
    /// Returns `NamespaceNotFound` if the namespace doesn't exist.
    fn list_refs(
        &self,
        namespace: &Namespace,
        prefix: &str,
    ) -> Result<Vec<(String, ObjectId)>, StorageError>;

    /// Delete a ref. Returns `true` if the ref existed. Returns
    /// `NamespaceNotFound` if the namespace doesn't exist.
    fn delete_ref(&self, namespace: &Namespace, name: &str) -> Result<bool, StorageError>;

    /// Delete a namespace and all its refs. Returns `true` if the namespace
    /// existed and was deleted, `false` if it was not found. The "default"
    /// namespace cannot be deleted and returns `InvalidOperation`.
    fn delete_namespace(&self, namespace: &Namespace) -> Result<bool, StorageError>;
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

    /// Transition a sealed epoch to Archived. Fails if the epoch is not
    /// found or is not in the Sealed state.
    fn archive_epoch(&self, id: &str) -> Result<(), StorageError>;
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

/// Durable storage of taints, quarantines, and watches (0.7.75 §3).
///
/// All six methods have default no-op implementations that return
/// `StorageError::Backend("taint storage not supported")`. Custom backends
/// that do not need taint support can rely on the defaults and satisfy the
/// `Storage` supertrait without additional boilerplate. Backends that do
/// support taints should override every method.
pub trait TaintStore: Send + Sync {
    /// Insert a freshly-created taint record. Storage must enforce
    /// the `(path, name, kind)` uniqueness invariant among unresolved
    /// rows — re-creating a resolved taint with the same triple is
    /// allowed (the old one stays as an audit row).
    fn create_taint(&self, _taint: &agentstategraph_taint::Taint) -> Result<(), StorageError> {
        Err(StorageError::Backend("taint storage not supported".into()))
    }

    /// Mark the taint with id `id` as resolved. Returns
    /// `StorageError::Backend` (wrapping `AlreadyResolved`) if the
    /// record is already resolved.
    fn resolve_taint(
        &self,
        _id: &str,
        _resolved_by: &str,
        _reason: &str,
        _proof: Option<&str>,
        _resolved_at: DateTime<Utc>,
    ) -> Result<(), StorageError> {
        Err(StorageError::Backend("taint storage not supported".into()))
    }

    /// List taints, optionally filtered by path prefix + kind +
    /// include-resolved flag. Results are most-recently-created
    /// first.
    fn list_taints(
        &self,
        _path_prefix: Option<&str>,
        _kind: Option<agentstategraph_taint::TaintKind>,
        _include_resolved: bool,
    ) -> Result<Vec<agentstategraph_taint::Taint>, StorageError> {
        Err(StorageError::Backend("taint storage not supported".into()))
    }

    /// Return every active taint (unresolved + not expired) whose
    /// `path` matches `request_path` exactly OR whose `path` is a
    /// propagating ancestor of `request_path`. The caller feeds the
    /// result into `agentstategraph_taint::evaluate_access`.
    fn check_taint(
        &self,
        _request_path: &str,
    ) -> Result<Vec<agentstategraph_taint::Taint>, StorageError> {
        Err(StorageError::Backend("taint storage not supported".into()))
    }

    /// Fetch a taint by its id. Returns `None` if missing.
    fn get_taint(&self, _id: &str) -> Result<Option<agentstategraph_taint::Taint>, StorageError> {
        Err(StorageError::Backend("taint storage not supported".into()))
    }

    /// Back-patch the `commit_id` onto a freshly-inserted taint
    /// after the repository has written the intent commit. A no-op
    /// if the taint is already resolved.
    fn set_taint_commit_id(&self, _id: &str, _commit_id: &str) -> Result<(), StorageError> {
        Err(StorageError::Backend("taint storage not supported".into()))
    }
}

/// Combined storage trait for convenience.
/// A backend that implements all seven sub-traits.
pub trait Storage:
    ObjectStore + CommitStore + RefStore + EpochStore + SessionStore + TaintStore + ReminderStore
{
}

/// Blanket implementation: anything implementing all seven traits is a Storage.
impl<
    T: ObjectStore + CommitStore + RefStore + EpochStore + SessionStore + TaintStore + ReminderStore,
> Storage for T
{
}
