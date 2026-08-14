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

    // ---- Optional leaf-value index (plan perf-slow-endpoints t-006) ----
    //
    // An optional, backend-provided index over the string leaves of a ref's
    // tree, keyed by `(namespace, ref)` and maintained in place: a one-time
    // backfill, then incremental add/remove as the ref advances. It turns value
    // search from an un-indexed full DFS (linear in graph size; multi-second on
    // a large ref) into a trigram substring probe. A backend that does not
    // implement it reports "not indexed" so callers fall back to the tree walk,
    // making the whole feature purely additive.

    /// Whether the `(namespace, ref)` leaf set has been built.
    fn leaf_index_is_built(&self, _namespace: &str, _ref_name: &str) -> Result<bool, StorageError> {
        Ok(false)
    }

    /// One-time backfill: replace any existing rows for `(namespace, ref)` with
    /// `entries` and mark the set built. Called once, from the read path, the
    /// first time a ref is searched after the feature is enabled.
    fn leaf_index_build(
        &self,
        _namespace: &str,
        _ref_name: &str,
        _entries: &[(String, String)],
    ) -> Result<(), StorageError> {
        Ok(())
    }

    /// Incremental maintenance: for a built `(namespace, ref)`, delete the rows
    /// at `removed_paths` and insert `added` `(path, value)` rows. A no-op if
    /// the set has not been built (the backfill will pick up the new state).
    /// Called from the write path as a ref advances.
    fn leaf_index_apply(
        &self,
        _namespace: &str,
        _ref_name: &str,
        _removed_paths: &[String],
        _added: &[(String, String)],
    ) -> Result<(), StorageError> {
        Ok(())
    }

    /// Substring-search the built leaves of `(namespace, ref)`. `Ok(None)` means
    /// the set is not built / backend has no index (caller falls back to the
    /// tree walk); `Ok(Some(..))` is authoritative (possibly empty), capped at
    /// `limit`.
    fn leaf_index_search(
        &self,
        _namespace: &str,
        _ref_name: &str,
        _query_lower: &str,
        _limit: usize,
    ) -> Result<Option<Vec<(String, String)>>, StorageError> {
        Ok(None)
    }
}

/// One row of the commit-history rollup (Plan A t-001): commit activity for a
/// single (day, namespace, agent, intent category) bucket.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryRollupRow {
    pub day: String,
    pub namespace: String,
    pub agent_id: String,
    pub intent_category: String,
    pub commit_count: i64,
    pub first_ts: String,
    pub last_ts: String,
}

/// One milestone on the distilled history timeline (Plan A t-001). `state_root`
/// (Plan A t-005) names the snapshot the milestone preserves — the retention
/// hook Plan B's GC keeps reachable; `None` only for rows written before t-005.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryMilestoneRow {
    pub commit_id: ObjectId,
    pub kind: String,
    pub timestamp: String,
    pub day: String,
    pub namespace: String,
    pub agent_id: String,
    pub description: String,
    pub state_root: Option<ObjectId>,
}

/// Per-table on-disk size (Plan A t-003), largest first.
#[derive(Debug, Clone, PartialEq)]
pub struct TableBytes {
    pub name: String,
    pub bytes: i64,
}

/// Physical shape of the store (Plan A t-003) — the evidence surface Plan B's
/// GC keys its retention thresholds off: how many objects/commits, how many
/// bytes, and where they live. `tables` is populated from SQLite's `dbstat`
/// virtual table when the build exposes it; otherwise only `total_bytes`
/// (page_count × page_size) is available and `dbstat_available` is false.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StoreShape {
    pub objects: i64,
    pub commits: i64,
    pub total_bytes: i64,
    pub tables: Vec<TableBytes>,
    pub dbstat_available: bool,
}

/// Result of a GC reachability mark (Plan B t-001): how many objects are live
/// (reachable from the given roots) vs. total, and the reclaimable remainder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GcReachability {
    pub total_objects: i64,
    pub live_objects: i64,
    pub reclaimable_objects: i64,
    /// Root state roots the mark started from.
    pub roots_walked: usize,
}

/// Result of a GC sweep (Plan B t-003): how many objects were deleted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GcSweep {
    pub objects_before: i64,
    pub objects_deleted: i64,
    pub objects_after: i64,
    /// Objects the sweep set out to delete (unreachable from the keep-set).
    /// Equals `objects_deleted` on a complete run.
    pub deleted_target: i64,
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

    // -- Project-history metrics (Plan A t-001) ------------------------------
    // Derived, rebuildable tables distilled from the commit chain. A backend
    // that doesn't implement them is simply "no history" — the feature is
    // purely additive, like `leaf_index_*`.

    /// Fold the next batch of un-processed commits into the `asg_history_*`
    /// rollup/milestone tables, advancing the stored `commits.rowid` cursor
    /// atomically. Returns the number of commits processed this call — `0` once
    /// the extractor has caught up. Idempotent and resumable: the cursor only
    /// advances inside the same transaction that writes the rows, so a crash
    /// re-processes the batch rather than double-counting it. Bounded memory:
    /// at most `batch_size` commits are held at once.
    fn history_extract_batch(&self, _batch_size: usize) -> Result<usize, StorageError> {
        Ok(0)
    }

    /// The last `commits.rowid` folded into the history tables (0 = nothing
    /// extracted yet).
    fn history_cursor(&self) -> Result<i64, StorageError> {
        Ok(0)
    }

    /// Read the commit-history rollup, ordered by (day, namespace, agent,
    /// intent category).
    fn history_rollup(&self) -> Result<Vec<HistoryRollupRow>, StorageError> {
        Ok(Vec::new())
    }

    /// Read the milestone timeline in chronological order, most recent first,
    /// capped at `limit`.
    fn history_milestones(&self, _limit: usize) -> Result<Vec<HistoryMilestoneRow>, StorageError> {
        Ok(Vec::new())
    }

    /// Measure the physical shape of the store (Plan A t-003): object/commit
    /// counts, total bytes, and per-table bytes when the backend can report
    /// them. Backends that can't return an empty [`StoreShape`].
    fn history_store_shape(&self) -> Result<StoreShape, StorageError> {
        Ok(StoreShape::default())
    }

    // -- Retention hooks for GC (Plan A t-005) ------------------------------

    /// Whether `id`'s commit has been folded into the history tables — i.e. its
    /// position is at or before the extractor cursor. This is the "already
    /// captured" predicate Plan B's GC checks before pruning a commit's raw
    /// snapshot: a distilled commit's signal survives in the metric tables.
    /// Backends without history report `false` (nothing distilled → prune
    /// nothing on this basis).
    fn history_is_commit_distilled(&self, _id: &ObjectId) -> Result<bool, StorageError> {
        Ok(false)
    }

    /// State roots the GC must keep reachable: the snapshots preserved by
    /// recorded milestones (the human-meaningful spine). Distinct, order
    /// unspecified.
    fn history_retained_state_roots(&self) -> Result<Vec<ObjectId>, StorageError> {
        Ok(Vec::new())
    }

    /// Count commits not yet folded into the history tables — those with a
    /// rowid beyond the extractor cursor (Plan B t-002). `0` means the
    /// extractor is caught up, so every commit's signal is distilled and the GC
    /// can safely make historical state unmaterializable. Backends without
    /// history report `0` (they have no distillation gate).
    fn history_undistilled_commit_count(&self) -> Result<i64, StorageError> {
        Ok(0)
    }

    /// Mark every object reachable from `roots` (state roots) and report live
    /// vs. total object counts — the GC reclaimable estimate (Plan B t-001).
    ///
    /// Walks the Merkle DAG in bounded memory: the mark set is held **on disk**
    /// (a temp table), and unexpanded nodes are drained in `batch`-sized chunks,
    /// so a 14.8M-object store never materializes the full closure in RAM.
    /// Backends without object storage report all-zero.
    fn history_gc_reachability(
        &self,
        _roots: &[ObjectId],
        _batch: usize,
    ) -> Result<GcReachability, StorageError> {
        Ok(GcReachability::default())
    }

    /// Sweep: mark the closure of `roots` and DELETE every object outside it
    /// (Plan B t-003). Transactional and resumable — deletes in bounded batches,
    /// so a crash mid-sweep leaves a consistent DB and re-running finishes it.
    /// Destructive; callers gate it (dry-run default, safety predicate).
    /// Backends without object storage report an empty sweep.
    fn history_gc_sweep(
        &self,
        _roots: &[ObjectId],
        _batch: usize,
    ) -> Result<GcSweep, StorageError> {
        Ok(GcSweep::default())
    }

    /// Every commit's `state_root`, newest first (insertion order), for the GC
    /// retention policy (Plan B t-003) to pick keep-recent + checkpoint-every.
    fn commit_state_roots_recent_first(&self) -> Result<Vec<ObjectId>, StorageError> {
        Ok(Vec::new())
    }
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
