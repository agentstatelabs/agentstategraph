//! Repository — the high-level API for AgentStateGraph.
//!
//! A Repository wraps a Storage backend and provides the primary
//! user-facing operations: get, set, delete, branch, merge, log.
//!
//! Every write operation is an atomic commit with intent metadata.
//! There is no staging area.

use std::sync::Arc;

use agentstategraph_core::{
    Authority, Commit, CommitBuilder, Conflict, DiffOp, Intent, IntentCategory, MergeResult,
    Namespace, Object, ObjectId, ObjectResolver, StatePath, ToolCall,
};
use agentstategraph_storage::{HistoryMilestoneRow, HistoryRollupRow, Storage, StorageError};

use crate::speculation::{SpecComparison, SpecError, SpecHandle, SpeculationManager};
use crate::tree::{self, TreeError};

/// Reserved state path prefix for schema metadata. Writes under this
/// prefix are only permitted on commits tagged `IntentCategory::Migrate`.
pub const META_PATH_PREFIX: &str = "/_meta";

/// Reserved sub-prefix for secret-bearing metadata. BOTH reads AND writes
/// under `/_meta/_secret/*` are gated to `IntentCategory::Migrate`. The
/// broader `/_meta/*` prefix only gates writes — the secret sub-prefix
/// tightens this for values that must not be surfaced by casual
/// `get`/`list_paths`/`search_values` callers.
///
/// See `spec/UPGRADE-PATH.md`.
pub const META_SECRET_PREFIX: &str = "/_meta/_secret";

/// Path to the schema version sentinel written by `Repository::init()`
/// and bumped by migrations in the `agentstategraph-migrate` crate.
pub const META_SCHEMA_VERSION_PATH: &str = "/_meta/schema_version";

/// Schema version stamped into new repositories by `init()`.
///
/// This is deliberately **decoupled from the crate version** — it tracks
/// the last version at which the on-disk shape changed, not the release
/// tag of the binary. A 0.4.0-beta.3 binary stamping `"0.4.0"` here
/// reflects that the schema is compatible with any 0.4.x binary. Bump
/// this constant only when you ship a migration that advances the DB
/// shape. See `spec/UPGRADE-PATH.md` decision 5.
pub const SCHEMA_VERSION: &str = "0.4.0";

fn path_is_reserved(path: &str) -> bool {
    path == META_PATH_PREFIX || path.starts_with(&format!("{}/", META_PATH_PREFIX))
}

/// True iff `path` is inside the `/_meta/_secret` sub-tree (or is the
/// prefix itself). Used to gate READS on secret metadata.
fn path_is_secret(path: &str) -> bool {
    path == META_SECRET_PREFIX || path.starts_with(&format!("{}/", META_SECRET_PREFIX))
}

/// Returns `true` when the intent category represents a taint /
/// quarantine / watch lifecycle event. These commits bypass the
/// pre-commit taint hook because they are the mechanism by which
/// taints are created and resolved — gating them on themselves
/// would deadlock the substrate.
fn is_taint_lifecycle_intent(category: &IntentCategory) -> bool {
    matches!(
        category,
        IntentCategory::Taint
            | IntentCategory::Untaint
            | IntentCategory::Quarantine
            | IntentCategory::Unquarantine
            | IntentCategory::Watch
            | IntentCategory::Unwatch
    )
}

fn check_meta_guard(path: &str, intent: &Intent) -> Result<(), RepoError> {
    if path_is_reserved(path) && intent.category != IntentCategory::Migrate {
        return Err(RepoError::ReservedPath(path.to_string()));
    }
    Ok(())
}

/// Gate reads of `/_meta/_secret/*` unless the caller's intent is
/// `IntentCategory::Migrate`. Applied to every read surface
/// (`get`, `get_json`, `list_paths`, `search_values`).
fn check_secret_read_guard(path: &str, intent: &Intent) -> Result<(), RepoError> {
    if path_is_secret(path) && intent.category != IntentCategory::Migrate {
        return Err(RepoError::ReservedPath(path.to_string()));
    }
    Ok(())
}

/// Walk a diff and return the first path under `/_meta/*` touched, if any.
/// Used to enforce the meta guard on speculation commits.
fn reserved_path_in_diff(diff: &[DiffOp]) -> Option<String> {
    for op in diff {
        let candidate = match op {
            DiffOp::SetValue { path, .. } => path.clone(),
            DiffOp::AddKey { path, key, .. } | DiffOp::RemoveKey { path, key, .. } => {
                if path == "/" || path.is_empty() {
                    format!("/{}", key)
                } else {
                    format!("{}/{}", path, key)
                }
            }
            DiffOp::AddElement { path, .. }
            | DiffOp::RemoveElement { path, .. }
            | DiffOp::AddToSet { path, .. }
            | DiffOp::RemoveFromSet { path, .. }
            | DiffOp::ChangeType { path, .. } => path.clone(),
        };
        if path_is_reserved(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// Environment variable that switches epoch seal enforcement from warn
/// (default) to strict (reject ref updates that would orphan sealed commits).
/// (security threat model v3+, V8)
pub const EPOCH_SEAL_STRICT_ENV: &str = "ASG_EPOCH_SEAL_STRICT";

/// A record of a sealed commit that would become unreachable from a
/// proposed new ref target.
#[derive(Debug, Clone)]
pub struct EpochViolation {
    pub epoch_id: String,
    pub unreachable_commits: Vec<ObjectId>,
}

/// The primary API for interacting with an AgentStateGraph state store.
pub struct Repository {
    storage: Arc<dyn Storage + Send + Sync>,
    specs: SpeculationManager,
    watch_mgr: crate::watch::WatchManager,
    /// Configured namespace for this repository instance. The active session's
    /// `scope_namespace` takes priority when resolving the effective namespace
    /// (see `active_namespace()`). Defaults to `Namespace::default_ns()`.
    namespace: Namespace,
    /// Active epoch id — if set, all new commits are associated with it
    /// via `storage.set_commit_epoch` on commit finalization. Set via
    /// `set_active_epoch` / cleared via `clear_active_epoch`. Not a
    /// public MCP tool yet — that's a follow-up milestone.
    active_epoch: std::sync::RwLock<Option<String>>,
    /// Active session id — same semantics as `active_epoch`.
    active_session: std::sync::RwLock<Option<String>>,
    /// When true (**the default**), ref updates that would orphan a sealed
    /// commit are rejected with `RepoError::EpochSealViolated` — the guard
    /// Plan B's GC relies on so sealed history can't be silently dropped. Opt
    /// OUT to the legacy warn-and-proceed behavior via
    /// `Repository::with_epoch_seal_strict(false)` or a falsey
    /// `ASG_EPOCH_SEAL_STRICT` (`0`/`false`/`warn`/`off`).
    epoch_seal_strict: bool,
    /// Cached namespace override from the active session's scope_namespace.
    /// Populated eagerly in `set_active_session`; cleared when session is None.
    /// Avoids a storage round-trip on every ref operation.
    active_session_namespace: std::sync::RwLock<Option<Namespace>>,
}

/// Default batch size for [`Repository::extract_history`] — how many commits
/// are folded per transaction. Large enough to amortize the per-batch overhead,
/// small enough to keep peak memory bounded on a 512k-commit store.
const DEFAULT_HISTORY_BATCH: usize = 5000;

/// Map a `YYYY-MM-DD` day to its ISO-week key `YYYY-Www`, or `None` if the day
/// doesn't parse. Used to roll daily velocity up to weekly.
fn iso_week_key(day: &str) -> Option<String> {
    use chrono::Datelike;
    let d = chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()?;
    let wk = d.iso_week();
    Some(format!("{}-W{:02}", wk.year(), wk.week()))
}

/// Outcome of a [`Repository::extract_history`] run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryExtractReport {
    /// Commits folded into the history tables this run (0 = already current).
    pub commits_processed: usize,
    /// The `commits.rowid` cursor after the run.
    pub cursor: i64,
}

/// Options for creating a commit.
pub struct CommitOptions {
    pub agent_id: String,
    pub authority: Authority,
    pub intent: Intent,
    pub reasoning: Option<String>,
    pub confidence: Option<f64>,
    /// Tool calls that contributed to this change. Persisted on the commit as
    /// provenance; empty by default. Capped at [`Commit`]'s tool-call limit on
    /// build (excess is truncated).
    pub tool_calls: Vec<ToolCall>,
}

impl CommitOptions {
    /// Create minimal commit options — the simplest way to commit.
    ///
    /// The authorizing principal defaults to `agent_id`: absent delegation, the
    /// actor making the change is also the authorizer (Plan C t-002 — minimal
    /// authority capture). Previously every commit hardcoded a constant
    /// `"default"` principal, so the authority field carried no real
    /// provenance. Override with [`CommitOptions::with_principal`] when the
    /// authorizer differs from the actor, or [`CommitOptions::with_authority`]
    /// for a full scoped/delegated `Authority` (t-004).
    pub fn new(
        agent_id: impl Into<String>,
        intent_category: IntentCategory,
        description: impl Into<String>,
    ) -> Self {
        let agent_id = agent_id.into();
        Self {
            authority: Authority::simple(agent_id.clone()),
            agent_id,
            intent: Intent::new(intent_category, description),
            reasoning: None,
            confidence: None,
            tool_calls: Vec::new(),
        }
    }

    /// Set the full authority (scope, delegation chain, expiry). For the common
    /// case of just naming the authorizing principal, prefer
    /// [`CommitOptions::with_principal`].
    pub fn with_authority(mut self, authority: Authority) -> Self {
        self.authority = authority;
        self
    }

    /// Set the authorizing principal (minimal authority capture: principal +
    /// wildcard scope, no delegation). Use when the authorizer differs from the
    /// actor `agent_id`; for a full delegated `Authority`, use
    /// [`CommitOptions::with_authority`].
    pub fn with_principal(mut self, principal: impl Into<String>) -> Self {
        self.authority = Authority::simple(principal);
        self
    }

    /// Set reasoning.
    pub fn with_reasoning(mut self, reasoning: impl Into<String>) -> Self {
        self.reasoning = Some(reasoning.into());
        self
    }

    /// Set confidence.
    pub fn with_confidence(mut self, confidence: f64) -> Self {
        self.confidence = Some(confidence);
        self
    }

    /// Set the tool calls that contributed to this change.
    pub fn with_tool_calls(mut self, tool_calls: Vec<ToolCall>) -> Self {
        self.tool_calls = tool_calls;
        self
    }

    /// Set tags on the intent.
    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.intent.tags = tags;
        self
    }
}

/// Errors from Repository operations.
#[derive(Debug, thiserror::Error)]
pub enum RepoError {
    #[error("branch not found: {0}")]
    BranchNotFound(String),

    #[error("branch already exists: {0}")]
    BranchAlreadyExists(String),

    #[error("ref not found: {0}")]
    RefNotFound(String),

    #[error("commit not found: {0}")]
    CommitNotFound(String),

    #[error("ambiguous commit prefix: {prefix} matched {count} commits")]
    AmbiguousCommitPrefix { prefix: String, count: usize },

    #[error("repository not initialized — call init() first")]
    NotInitialized,

    #[error(
        "path {0} is reserved for schema metadata; only IntentCategory::Migrate commits may write here"
    )]
    ReservedPath(String),

    #[error("merge conflicts: {0:?}")]
    MergeConflicts(Vec<Conflict>),

    #[error("merge would delete top-level entries {0:?}; pass allow_deletions to proceed")]
    MergeWouldDelete(Vec<String>),

    #[error(
        "integrity violation: object {missing} reachable from state root {root} is missing from the store; ref not advanced"
    )]
    IntegrityViolation { root: ObjectId, missing: ObjectId },

    #[error("write conflict: ref moved before CAS could land")]
    WriteConflict,

    #[error("speculation error: {0}")]
    Speculation(#[from] SpecError),

    #[error("tree error: {0}")]
    Tree(#[from] TreeError),

    #[error("storage error: {0}")]
    Storage(#[from] StorageError),

    #[error(
        "epoch seal violated: epoch '{epoch_id}' sealed commit(s) {unreachable_commits:?} would be orphaned by this ref update"
    )]
    EpochSealViolated {
        epoch_id: String,
        unreachable_commits: Vec<ObjectId>,
    },

    /// Pre-commit taint hook (0.7.75 §4) rejected a write. `taint_id`
    /// is the storage id of the taint that caused the rejection — use
    /// `agentstategraph_policy` / MCP `check_taint` to surface the
    /// full context.
    ///
    /// `taint_id` is `Some` for all blocking rejections (NotAuthorized,
    /// Blocked, InsufficientConfidence) and `None` only for lookup
    /// failures (NotFound) where no taint id exists to report.
    #[error("invalid operation: {0}")]
    InvalidOperation(String),

    #[error("namespace not found: '{0}' — call create_namespace first")]
    NamespaceNotFound(String),

    #[error("cross-namespace merge denied: no PolicyStore configured")]
    CrossNamespaceAccessDenied,

    #[error("taint hook rejected write{}: {source}", match .taint_id {
        Some(id) => format!(" (taint: {id})"),
        None => String::new(),
    })]
    Taint {
        #[source]
        source: agentstategraph_taint::TaintError,
        taint_id: Option<String>,
    },
}

/// Summary of what merging one ref into another would do, produced by
/// [`Repository::preview_merge`] without mutating any ref. All key lists are
/// top-level entries of the state root (e.g. `plans`, `memory`).
#[derive(Debug, Clone)]
pub struct MergePreview {
    /// True if the merge resolves to a fast-forward.
    pub fast_forward: bool,
    /// Top-level keys present after the merge but not before.
    pub added: Vec<String>,
    /// Top-level keys whose subtree id changes.
    pub changed: Vec<String>,
    /// Top-level keys that would be removed (data-loss surface).
    pub removed: Vec<String>,
    /// Conflicts that block a clean merge, if any.
    pub conflicts: Vec<Conflict>,
}

impl MergePreview {
    /// Whether committing this merge would remove any top-level entry.
    pub fn has_deletions(&self) -> bool {
        !self.removed.is_empty()
    }
}

/// Intermediate result of a merge computed but not yet committed.
struct MergeComputation {
    source_commit_id: ObjectId,
    target_commit_id: ObjectId,
    source_state_root: ObjectId,
    target_state_root: ObjectId,
    result: MergeResult,
    created: Vec<Object>,
}

enum MergeResultKind {
    Success,
    Conflicts,
    FastForward,
}

/// Top-level map entries (key -> child id) of an object, or empty if it is not
/// a map node.
fn map_entries(obj: &Object) -> std::collections::BTreeMap<String, ObjectId> {
    match obj {
        Object::Node(agentstategraph_core::Node::Map(entries)) => entries.clone(),
        _ => std::collections::BTreeMap::new(),
    }
}

impl From<agentstategraph_taint::TaintError> for RepoError {
    /// Converts a bare TaintError with no associated taint id.
    /// Used only for lookup-not-found errors; blocking rejections always
    /// go through `taint_err_with_id` in taint.rs which sets `Some(id)`.
    fn from(source: agentstategraph_taint::TaintError) -> Self {
        RepoError::Taint {
            source,
            taint_id: None,
        }
    }
}

impl Repository {
    /// Create a new Repository wrapping the given storage backend.
    ///
    /// Epoch-seal enforcement is **strict by default**: a ref update that would
    /// orphan a sealed commit is rejected. Opt out to the legacy
    /// warn-and-proceed behavior by setting `ASG_EPOCH_SEAL_STRICT` to a falsey
    /// value (`0`/`false`/`warn`/`off`) at construction time, or programmatically
    /// via [`Repository::with_epoch_seal_strict`]`(false)`.
    pub fn new(storage: Box<dyn Storage + Send + Sync>) -> Self {
        // Strict unless explicitly disabled. An unset or unrecognized value
        // keeps the safe default; only a clearly falsey value opts out.
        let strict = std::env::var(EPOCH_SEAL_STRICT_ENV)
            .map(|v| {
                !(v == "0"
                    || v.eq_ignore_ascii_case("false")
                    || v.eq_ignore_ascii_case("warn")
                    || v.eq_ignore_ascii_case("off"))
            })
            .unwrap_or(true);
        Self {
            storage: Arc::from(storage),
            specs: SpeculationManager::new(),
            watch_mgr: crate::watch::WatchManager::new(),
            namespace: Namespace::default_ns(),
            active_session_namespace: std::sync::RwLock::new(None),
            active_epoch: std::sync::RwLock::new(None),
            active_session: std::sync::RwLock::new(None),
            epoch_seal_strict: strict,
        }
    }

    /// Return a Repository configured to use the given namespace for all ref
    /// operations. The active session's `scope_namespace` overrides this value
    /// when a session is active. Callers can chain this with other builders:
    /// `Repository::new(storage).with_namespace(ns).with_epoch_seal_strict(true)`.
    pub fn with_namespace(mut self, ns: Namespace) -> Self {
        self.namespace = ns;
        self
    }

    /// Create a new `Repository` that operates in a different namespace but
    /// shares the same underlying storage.  Used for per-call namespace
    /// overrides (MCP tool `namespace` param, WASM `namespace` argument)
    /// without mutating the server-wide repository configuration.
    ///
    /// The forked repository inherits `epoch_seal_strict` and the active
    /// epoch/session ids from the parent; speculation state and watch
    /// subscriptions start fresh (they are in-memory and call-scoped).
    pub fn fork_namespace(&self, ns: Namespace) -> Self {
        let epoch = self.active_epoch.read().unwrap().clone();
        let session = self.active_session.read().unwrap().clone();
        let session_ns = self.active_session_namespace.read().unwrap().clone();
        Self {
            storage: Arc::clone(&self.storage),
            specs: crate::speculation::SpeculationManager::new(),
            watch_mgr: crate::watch::WatchManager::new(),
            namespace: ns,
            active_epoch: std::sync::RwLock::new(epoch),
            active_session: std::sync::RwLock::new(session),
            epoch_seal_strict: self.epoch_seal_strict,
            active_session_namespace: std::sync::RwLock::new(session_ns),
        }
    }

    /// The namespace this repository was configured with. The *effective*
    /// namespace for a ref operation may differ when a session with its own
    /// `scope_namespace` is active — use `active_namespace()` for that.
    pub fn namespace(&self) -> &Namespace {
        &self.namespace
    }

    /// Return a Repository with epoch-seal enforcement set to the given mode.
    /// Overrides the `ASG_EPOCH_SEAL_STRICT` environment variable.
    ///
    /// Strict mode (the default) rejects ref updates that would render any
    /// sealed-epoch commit unreachable from the new target. Pass `false` for
    /// the legacy warn-and-proceed behavior. (security threat model v3+, V8)
    pub fn with_epoch_seal_strict(mut self, strict: bool) -> Self {
        self.epoch_seal_strict = strict;
        self
    }

    /// Initialize the repository with an empty state tree on "main".
    /// If "main" already exists, this is a no-op.
    ///
    /// The initial commit stamps `/_meta/schema_version` with the crate
    /// version. See `spec/UPGRADE-PATH.md`.
    pub fn init(&self) -> Result<ObjectId, RepoError> {
        let ns = self.active_namespace()?;
        // Ensure the namespace row exists. init() is idempotent so creating here
        // is safe; create_namespace absorbs NamespaceAlreadyExists.
        self.create_namespace(ns.as_str())?;
        if let Some(id) = self.storage.get_ref(&ns, "main")? {
            return Ok(id);
        }

        let empty_root = Object::empty_map();
        let empty_root_id = self.storage.put_object(&empty_root)?;

        let version_path = StatePath::parse(META_SCHEMA_VERSION_PATH)
            .map_err(|e| TreeError::PathNotFound(e.to_string()))?;
        let version_value = Object::Atom(agentstategraph_core::Atom::String(
            SCHEMA_VERSION.to_string(),
        ));
        let stamped_root_id = tree::tree_set(
            self.storage.as_ref(),
            &empty_root_id,
            &version_path,
            &version_value,
        )?;

        let commit = CommitBuilder::new(
            stamped_root_id,
            "system",
            Authority::simple("system"),
            Intent::new(IntentCategory::Checkpoint, "Initialize empty state"),
        )
        .build();

        self.storage.put_commit(&commit)?;
        self.storage.set_ref(&ns, "main", commit.id)?;

        Ok(commit.id)
    }

    // -----------------------------------------------------------------------
    // State operations
    // -----------------------------------------------------------------------

    /// Get a value from state at the given ref and path.
    ///
    /// Reads under `/_meta/_secret/*` are rejected — use
    /// [`Repository::get_with_intent`] with an
    /// `IntentCategory::Migrate` intent to access them.
    pub fn get(&self, ref_name: &str, path: &str) -> Result<Object, RepoError> {
        if path_is_secret(path) {
            return Err(RepoError::ReservedPath(path.to_string()));
        }
        let commit_id = self.resolve_ref(ref_name)?;
        let commit = self
            .storage
            .get_commit(&commit_id)?
            .ok_or_else(|| RepoError::RefNotFound(ref_name.to_string()))?;

        let state_path =
            StatePath::parse(path).map_err(|e| TreeError::PathNotFound(e.to_string()))?;
        let obj = tree::tree_get(self.storage.as_ref(), &commit.state_root, &state_path)?;
        Ok(obj)
    }

    /// Get a value as JSON.
    ///
    /// Reads under `/_meta/_secret/*` are rejected — use
    /// [`Repository::get_json_with_intent`] with
    /// `IntentCategory::Migrate` to access them.
    pub fn get_json(&self, ref_name: &str, path: &str) -> Result<serde_json::Value, RepoError> {
        let obj = self.get(ref_name, path)?;
        let json = tree::tree_to_json(self.storage.as_ref(), &obj)?;
        Ok(json)
    }

    /// Like [`Repository::get_json`] but stops materializing `max_depth` levels
    /// below `path`: nodes at the cap become `{ "_truncated": true, ... }`
    /// placeholders and their subtrees are never loaded. A cheap shallow read of
    /// a large ref — avoids walking and serializing the whole tree (plan t-007).
    pub fn get_json_capped(
        &self,
        ref_name: &str,
        path: &str,
        max_depth: usize,
    ) -> Result<serde_json::Value, RepoError> {
        let obj = self.get(ref_name, path)?;
        let json = tree::tree_to_json_capped(self.storage.as_ref(), &obj, max_depth)?;
        Ok(json)
    }

    /// Get a value with an explicit intent, permitting reads of
    /// `/_meta/_secret/*` when the intent category is `Migrate`.
    pub fn get_with_intent(
        &self,
        ref_name: &str,
        path: &str,
        intent: &Intent,
    ) -> Result<Object, RepoError> {
        check_secret_read_guard(path, intent)?;
        let commit_id = self.resolve_ref(ref_name)?;
        let commit = self
            .storage
            .get_commit(&commit_id)?
            .ok_or_else(|| RepoError::RefNotFound(ref_name.to_string()))?;
        let state_path =
            StatePath::parse(path).map_err(|e| TreeError::PathNotFound(e.to_string()))?;
        let obj = tree::tree_get(self.storage.as_ref(), &commit.state_root, &state_path)?;
        Ok(obj)
    }

    /// Like [`Repository::get_json`] but honors an explicit intent for
    /// reading the `/_meta/_secret/*` sub-tree.
    pub fn get_json_with_intent(
        &self,
        ref_name: &str,
        path: &str,
        intent: &Intent,
    ) -> Result<serde_json::Value, RepoError> {
        let obj = self.get_with_intent(ref_name, path, intent)?;
        let json = tree::tree_to_json(self.storage.as_ref(), &obj)?;
        Ok(json)
    }

    /// Set a value in state, creating a new commit.
    /// Returns the new commit ID.
    pub fn set(
        &self,
        ref_name: &str,
        path: &str,
        value: &Object,
        options: CommitOptions,
    ) -> Result<ObjectId, RepoError> {
        check_meta_guard(path, &options.intent)?;
        // 0.7.75 §4: taint pre-commit hook. Rejects writes to blocked
        // or quarantined paths; review-effect gate enforces the
        // confidence threshold. Taint/Untaint/Quarantine/... commits
        // themselves bypass the hook — they're creating the taint
        // record and must be able to reach the path.
        if !is_taint_lifecycle_intent(&options.intent.category) {
            self.pre_commit_taint_check(&[path], &options)?;
        }
        let commit_id = self.resolve_ref(ref_name)?;
        let commit = self
            .storage
            .get_commit(&commit_id)?
            .ok_or_else(|| RepoError::RefNotFound(ref_name.to_string()))?;

        let state_path =
            StatePath::parse(path).map_err(|e| TreeError::PathNotFound(e.to_string()))?;
        let new_root = tree::tree_set(
            self.storage.as_ref(),
            &commit.state_root,
            &state_path,
            value,
        )?;

        let new_commit = self.create_commit(new_root, vec![commit_id], options)?;
        self.guarded_set_ref(ref_name, new_commit.id)?;

        Ok(new_commit.id)
    }

    /// Set a value from JSON, creating a new commit.
    pub fn set_json(
        &self,
        ref_name: &str,
        path: &str,
        value: &serde_json::Value,
        options: CommitOptions,
    ) -> Result<ObjectId, RepoError> {
        let root_id = tree::json_to_tree(self.storage.as_ref(), value)?;
        let obj = self
            .storage
            .get_object(&root_id)?
            .ok_or_else(|| RepoError::RefNotFound("value".to_string()))?;
        // Snapshot the intent category so we can decide whether to
        // run auto-escalation *after* the main commit (the taint
        // lifecycle intents bypass it).
        let is_lifecycle = is_taint_lifecycle_intent(&options.intent.category);
        let commit_id = self.set(ref_name, path, &obj, options)?;
        if !is_lifecycle {
            // 0.7.75 §5: watch auto-escalation. Runs AFTER the
            // write — threshold-crossing creates a new taint commit
            // whose intent points at the watch-create commit.
            let _ = self.auto_escalate_watches(ref_name, path, value)?;
        }
        Ok(commit_id)
    }

    /// Return the current commit ID for `ref_name`. Needed for CAS-retry
    /// callers that must snapshot the head before a read-modify-write cycle.
    pub fn head(&self, ref_name: &str) -> Result<ObjectId, RepoError> {
        self.resolve_ref(ref_name)
    }

    /// Like `set_json` but uses a compare-and-swap on the ref rather than an
    /// unconditional `set_ref`. Builds the new commit on top of
    /// `expected_head` (not the current ref head), then calls `cas_ref`.
    ///
    /// Returns `Ok(new_commit_id)` on success, `Err(WriteConflict)` when the
    /// ref has moved since `expected_head` was snapshotted (allowing the
    /// caller to retry), or another error on hard failures.
    pub fn set_json_cas(
        &self,
        ref_name: &str,
        expected_head: ObjectId,
        path: &str,
        value: &serde_json::Value,
        options: CommitOptions,
    ) -> Result<ObjectId, RepoError> {
        check_meta_guard(path, &options.intent)?;
        let is_lifecycle = is_taint_lifecycle_intent(&options.intent.category);
        if !is_lifecycle {
            self.pre_commit_taint_check(&[path], &options)?;
        }

        // Build the new tree from *expected_head*, not the live ref.
        let base_commit = self
            .storage
            .get_commit(&expected_head)?
            .ok_or_else(|| RepoError::RefNotFound(ref_name.to_string()))?;

        let root_id = tree::json_to_tree(self.storage.as_ref(), value)?;
        let obj = self
            .storage
            .get_object(&root_id)?
            .ok_or_else(|| RepoError::RefNotFound("value".to_string()))?;

        let state_path =
            StatePath::parse(path).map_err(|e| TreeError::PathNotFound(e.to_string()))?;
        let new_root = tree::tree_set(
            self.storage.as_ref(),
            &base_commit.state_root,
            &state_path,
            &obj,
        )?;

        let new_commit = self.create_commit(new_root, vec![expected_head], options)?;

        // Epoch-seal check (same guard that guarded_set_ref runs).
        self.enforce_epoch_seals(ref_name, &new_commit.id)?;

        let ns = self.active_namespace()?;
        match self
            .storage
            .cas_ref(&ns, ref_name, expected_head, new_commit.id)?
        {
            false => Err(RepoError::WriteConflict),
            true => {
                if !is_lifecycle {
                    let _ = self.auto_escalate_watches(ref_name, path, value)?;
                }
                Ok(new_commit.id)
            }
        }
    }

    /// Delete a value from state, creating a new commit.
    pub fn delete(
        &self,
        ref_name: &str,
        path: &str,
        options: CommitOptions,
    ) -> Result<ObjectId, RepoError> {
        check_meta_guard(path, &options.intent)?;
        if !is_taint_lifecycle_intent(&options.intent.category) {
            self.pre_commit_taint_check(&[path], &options)?;
        }
        let commit_id = self.resolve_ref(ref_name)?;
        let commit = self
            .storage
            .get_commit(&commit_id)?
            .ok_or_else(|| RepoError::RefNotFound(ref_name.to_string()))?;

        let state_path =
            StatePath::parse(path).map_err(|e| TreeError::PathNotFound(e.to_string()))?;
        let new_root = tree::tree_delete(self.storage.as_ref(), &commit.state_root, &state_path)?;

        let new_commit = self.create_commit(new_root, vec![commit_id], options)?;
        self.guarded_set_ref(ref_name, new_commit.id)?;

        Ok(new_commit.id)
    }

    // -----------------------------------------------------------------------
    // Branch operations
    // -----------------------------------------------------------------------

    /// Create a new branch from the given ref.
    pub fn branch(&self, name: &str, from: &str) -> Result<ObjectId, RepoError> {
        let ns = self.active_namespace()?;
        // Check if branch already exists
        if self.storage.get_ref(&ns, name)?.is_some() {
            return Err(RepoError::BranchAlreadyExists(name.to_string()));
        }

        let commit_id = self.resolve_ref(from)?;
        // Branch creation is a new-pointer write; no existing commits become
        // unreachable, so epoch-seal enforcement is a no-op here. Route
        // through `guarded_set_ref` anyway for consistency.
        self.guarded_set_ref(name, commit_id)?;
        Ok(commit_id)
    }

    /// Delete a branch. Returns true if the branch existed.
    /// Does NOT delete any commits (they remain in the DAG).
    pub fn delete_branch(&self, name: &str) -> Result<bool, RepoError> {
        let ns = self.active_namespace()?;
        Ok(self.storage.delete_ref(&ns, name)?)
    }

    /// List all branches, optionally filtered by prefix.
    pub fn list_branches(
        &self,
        prefix: Option<&str>,
    ) -> Result<Vec<(String, ObjectId)>, RepoError> {
        let ns = self.active_namespace()?;
        Ok(self.storage.list_refs(&ns, prefix.unwrap_or(""))?)
    }

    /// Merge source branch into target branch.
    /// Uses three-way merge with the common ancestor (currently: the commit
    /// where the source branch was created from target).
    ///
    /// Returns Ok(commit_id) on success, or Err with conflicts.
    pub fn merge(
        &self,
        source: &str,
        target: &str,
        options: CommitOptions,
    ) -> Result<ObjectId, RepoError> {
        self.merge_checked(source, target, options, true)
    }

    /// Like [`Repository::merge`], but refuses to advance the target ref when
    /// the merge would remove any top-level entry (e.g. an entire `/plans` or
    /// `/memory` map) unless `allow_deletions` is true. This is a data-loss
    /// guard: the merge algorithm is correct, but a mistaken source or a
    /// genuine deletion should not silently drop a whole subtree.
    pub fn merge_checked(
        &self,
        source: &str,
        target: &str,
        options: CommitOptions,
        allow_deletions: bool,
    ) -> Result<ObjectId, RepoError> {
        let comp = self.compute_merge(source, target)?;

        if !allow_deletions {
            let removed = self.top_level_removals(&comp)?;
            if !removed.is_empty() {
                return Err(RepoError::MergeWouldDelete(removed));
            }
        }

        match comp.result {
            MergeResult::Success(merged_obj) => {
                // Persist every newly-created composite node AND the root in a
                // single atomic batch. The merged tree references children by
                // id; nodes the merge fabricated (key sets that existed on
                // neither branch) are not in any store yet, so a partial write
                // would leave the tree dangling with ObjectNotFound on readback.
                let merged_root = merged_obj.id();
                let mut to_store: Vec<Object> = Vec::with_capacity(comp.created.len() + 1);
                to_store.extend(comp.created.iter().cloned());
                to_store.push(merged_obj);
                self.storage.batch_put_objects(&to_store)?;

                // Integrity gate: never advance a ref to a tree that isn't
                // fully readable. This catches both a partial write above and a
                // `created` set that failed to include a node the root
                // references — the exact defect that silently corrupted a
                // `/plans` subtree and made every plan appear to vanish.
                if let Some(missing) = self.first_missing_reachable(&merged_root)? {
                    return Err(RepoError::IntegrityViolation {
                        root: merged_root,
                        missing,
                    });
                }

                let commit = self.create_commit(
                    merged_root,
                    vec![comp.target_commit_id, comp.source_commit_id],
                    options,
                )?;
                self.guarded_set_ref(target, commit.id)?;
                Ok(commit.id)
            }
            MergeResult::FastForward(ff_id) => {
                // Find the commit that has this state root
                // In fast-forward, we just advance the target ref
                let ff_commit = if ff_id == comp.source_state_root {
                    comp.source_commit_id
                } else {
                    comp.target_commit_id
                };
                self.guarded_set_ref(target, ff_commit)?;
                Ok(ff_commit)
            }
            MergeResult::Conflicts { conflicts, .. } => Err(RepoError::MergeConflicts(conflicts)),
        }
    }

    /// Resolve the merge base (lowest common ancestor commit) of two refs.
    /// Useful for callers that need to reason about what each side changed
    /// relative to the branch point — e.g. domain-level merge policies.
    pub fn merge_base(&self, source: &str, target: &str) -> Result<ObjectId, RepoError> {
        let source_commit_id = self.resolve_ref(source)?;
        let target_commit_id = self.resolve_ref(target)?;
        self.find_common_ancestor(&source_commit_id, &target_commit_id)
    }

    /// Compute what merging `source` into `target` WOULD do, without advancing
    /// any ref or storing a commit. Returns a summary of top-level additions,
    /// changes, and removals plus any conflicts — the basis for a `--dry-run`.
    pub fn preview_merge(&self, source: &str, target: &str) -> Result<MergePreview, RepoError> {
        let comp = self.compute_merge(source, target)?;
        let result_root = match &comp.result {
            MergeResult::Success(obj) => obj.id(),
            MergeResult::Conflicts { partial, .. } => partial.id(),
            MergeResult::FastForward(state_root) => *state_root,
        };
        let target_entries = self.top_level_entries(&comp.target_state_root)?;
        let merged_entries = self.top_level_entries_of_obj(&comp.result, result_root)?;

        let mut added = Vec::new();
        let mut changed = Vec::new();
        let mut removed = Vec::new();
        for (k, v) in &merged_entries {
            match target_entries.get(k) {
                None => added.push(k.clone()),
                Some(tv) if tv != v => changed.push(k.clone()),
                _ => {}
            }
        }
        for k in target_entries.keys() {
            if !merged_entries.contains_key(k) {
                removed.push(k.clone());
            }
        }
        added.sort();
        changed.sort();
        removed.sort();

        let fast_forward = matches!(self.result_kind(&comp), MergeResultKind::FastForward);
        let conflicts = match comp.result {
            MergeResult::Conflicts { conflicts, .. } => conflicts,
            _ => Vec::new(),
        };
        Ok(MergePreview {
            fast_forward,
            added,
            changed,
            removed,
            conflicts,
        })
    }

    fn result_kind(&self, comp: &MergeComputation) -> MergeResultKind {
        match &comp.result {
            MergeResult::Success(_) => MergeResultKind::Success,
            MergeResult::Conflicts { .. } => MergeResultKind::Conflicts,
            MergeResult::FastForward(_) => MergeResultKind::FastForward,
        }
    }

    /// The top-level entries that would disappear from `target` if this merge
    /// were committed.
    fn top_level_removals(&self, comp: &MergeComputation) -> Result<Vec<String>, RepoError> {
        let result_root = match &comp.result {
            MergeResult::Success(obj) => obj.id(),
            MergeResult::Conflicts { partial, .. } => partial.id(),
            MergeResult::FastForward(state_root) => *state_root,
        };
        let target_entries = self.top_level_entries(&comp.target_state_root)?;
        let merged_entries = self.top_level_entries_of_obj(&comp.result, result_root)?;
        let mut removed: Vec<String> = target_entries
            .keys()
            .filter(|k| !merged_entries.contains_key(*k))
            .cloned()
            .collect();
        removed.sort();
        Ok(removed)
    }

    /// Resolve, find base, and run the collecting three-way merge without
    /// committing. Shared by `merge_checked` and `preview_merge`.
    fn compute_merge(&self, source: &str, target: &str) -> Result<MergeComputation, RepoError> {
        let source_commit_id = self.resolve_ref(source)?;
        let target_commit_id = self.resolve_ref(target)?;

        let source_commit = self
            .storage
            .get_commit(&source_commit_id)?
            .ok_or_else(|| RepoError::RefNotFound(source.to_string()))?;
        let target_commit = self
            .storage
            .get_commit(&target_commit_id)?
            .ok_or_else(|| RepoError::RefNotFound(target.to_string()))?;

        let base_commit_id = self.find_common_ancestor(&source_commit_id, &target_commit_id)?;
        let base_commit = self
            .storage
            .get_commit(&base_commit_id)?
            .ok_or_else(|| RepoError::RefNotFound("base".to_string()))?;

        let resolver = StorageResolver {
            storage: self.storage.as_ref(),
        };
        let (result, created) = agentstategraph_core::merge::three_way_merge_collect(
            &resolver,
            &base_commit.state_root,
            &target_commit.state_root,
            &source_commit.state_root,
        );

        Ok(MergeComputation {
            source_commit_id,
            target_commit_id,
            source_state_root: source_commit.state_root,
            target_state_root: target_commit.state_root,
            result,
            created,
        })
    }

    /// Top-level map entries (key -> child ObjectId) at a state root id.
    fn top_level_entries(
        &self,
        root: &ObjectId,
    ) -> Result<std::collections::BTreeMap<String, ObjectId>, RepoError> {
        match self.storage.get_object(root)? {
            Some(obj) => Ok(map_entries(&obj)),
            None => Ok(std::collections::BTreeMap::new()),
        }
    }

    /// Top-level entries of a merge result. For Success/Conflicts the merged
    /// object is in hand (its children may not be stored yet); for FastForward
    /// only an id is available, so fall back to the store.
    fn top_level_entries_of_obj(
        &self,
        result: &MergeResult,
        fallback_root: ObjectId,
    ) -> Result<std::collections::BTreeMap<String, ObjectId>, RepoError> {
        match result {
            MergeResult::Success(obj) | MergeResult::Conflicts { partial: obj, .. } => {
                Ok(map_entries(obj))
            }
            MergeResult::FastForward(_) => self.top_level_entries(&fallback_root),
        }
    }

    // -----------------------------------------------------------------------
    // Diff operations
    // -----------------------------------------------------------------------

    /// Compute a structured diff between two refs.
    /// Returns typed DiffOps (SetValue, AddKey, RemoveKey, etc.), not text diffs.
    pub fn diff(&self, ref_a: &str, ref_b: &str) -> Result<Vec<DiffOp>, RepoError> {
        let commit_a = self.resolve_ref(ref_a)?;
        let commit_b = self.resolve_ref(ref_b)?;

        let ca = self
            .storage
            .get_commit(&commit_a)?
            .ok_or_else(|| RepoError::RefNotFound(ref_a.to_string()))?;
        let cb = self
            .storage
            .get_commit(&commit_b)?
            .ok_or_else(|| RepoError::RefNotFound(ref_b.to_string()))?;

        let resolver = StorageResolver {
            storage: self.storage.as_ref(),
        };
        Ok(agentstategraph_core::diff::diff(
            &resolver,
            &ca.state_root,
            &cb.state_root,
        ))
    }

    // -----------------------------------------------------------------------
    // Speculative execution
    // -----------------------------------------------------------------------

    /// Create a speculation forked from a ref. O(1) — just a pointer.
    pub fn speculate(
        &self,
        from_ref: &str,
        label: Option<String>,
    ) -> Result<SpecHandle, RepoError> {
        let commit_id = self.resolve_ref(from_ref)?;
        let commit = self
            .storage
            .get_commit(&commit_id)?
            .ok_or_else(|| RepoError::RefNotFound(from_ref.to_string()))?;

        self.specs
            .create(from_ref, commit.state_root, label)
            .map_err(RepoError::Speculation)
    }

    /// Get a value from a speculation's state.
    pub fn spec_get(&self, handle: SpecHandle, path: &str) -> Result<Object, RepoError> {
        self.specs
            .get(handle, self.storage.as_ref(), path)
            .map_err(RepoError::Speculation)
    }

    /// Set a value in a speculation's state.
    pub fn spec_set(
        &self,
        handle: SpecHandle,
        path: &str,
        value: &Object,
    ) -> Result<(), RepoError> {
        self.specs
            .set(handle, self.storage.as_ref(), path, value)
            .map_err(RepoError::Speculation)
    }

    /// Set a value from JSON in a speculation's state. Convenience wrapper
    /// around `spec_set` that mirrors `set_json` — converts the JSON value
    /// into an Object tree before applying.
    pub fn spec_set_json(
        &self,
        handle: SpecHandle,
        path: &str,
        value: &serde_json::Value,
    ) -> Result<(), RepoError> {
        let root_id = tree::json_to_tree(self.storage.as_ref(), value)?;
        let obj = self
            .storage
            .get_object(&root_id)?
            .ok_or_else(|| RepoError::RefNotFound("value".to_string()))?;
        self.spec_set(handle, path, &obj)
    }

    /// Delete a value in a speculation's state.
    pub fn spec_delete(&self, handle: SpecHandle, path: &str) -> Result<(), RepoError> {
        self.specs
            .delete(handle, self.storage.as_ref(), path)
            .map_err(RepoError::Speculation)
    }

    /// Compare multiple speculations side-by-side.
    pub fn compare_speculations(
        &self,
        handles: &[SpecHandle],
    ) -> Result<SpecComparison, RepoError> {
        self.specs
            .compare(handles, self.storage.as_ref())
            .map_err(RepoError::Speculation)
    }

    /// Commit a speculation — promotes it to a real commit on the base branch.
    ///
    /// If the speculation touched any `/_meta/*` path and the commit's
    /// intent is not `IntentCategory::Migrate`, the commit is rejected
    /// with `RepoError::ReservedPath`. This keeps the meta namespace
    /// enforced for speculation writes the same as for direct writes.
    pub fn commit_speculation(
        &self,
        handle: SpecHandle,
        options: CommitOptions,
    ) -> Result<ObjectId, RepoError> {
        let (state_root, base_ref) = self.specs.commit(handle).map_err(RepoError::Speculation)?;

        let parent_id = self.resolve_ref(&base_ref)?;

        // Gate /_meta/* writes on the commit's intent category.
        if options.intent.category != IntentCategory::Migrate {
            let parent_commit = self
                .storage
                .get_commit(&parent_id)?
                .ok_or_else(|| RepoError::RefNotFound(base_ref.clone()))?;
            let resolver = StorageResolver {
                storage: self.storage.as_ref(),
            };
            let diff =
                agentstategraph_core::diff::diff(&resolver, &parent_commit.state_root, &state_root);
            if let Some(path) = reserved_path_in_diff(&diff) {
                return Err(RepoError::ReservedPath(path));
            }
        }

        let commit = self.create_commit(state_root, vec![parent_id], options)?;
        self.guarded_set_ref(&base_ref, commit.id)?;
        Ok(commit.id)
    }

    /// Discard a speculation — all changes lost. Instant.
    pub fn discard_speculation(&self, handle: SpecHandle) -> Result<(), RepoError> {
        self.specs.discard(handle).map_err(RepoError::Speculation)
    }

    /// List all active speculations.
    pub fn list_speculations(&self) -> Vec<(SpecHandle, Option<String>)> {
        self.specs.list()
    }

    // -----------------------------------------------------------------------
    // History operations
    // -----------------------------------------------------------------------

    /// Get the commit log starting from a ref.
    ///
    /// Each returned commit has `enforce_caps` applied — the same length
    /// caps `CommitBuilder::build` uses. This bounds what a malicious or
    /// legacy `.db` can replay to readers. (security threat model v2, F3)
    pub fn log(&self, ref_name: &str, limit: usize) -> Result<Vec<Commit>, RepoError> {
        let commit_id = self.resolve_ref(ref_name)?;
        let mut commits = self.storage.list_commits(&commit_id, limit)?;
        for c in &mut commits {
            c.enforce_caps();
        }
        Ok(commits)
    }

    /// Get a specific commit by ID.
    ///
    /// Returns the commit with `enforce_caps` applied — see `log`.
    pub fn get_commit(&self, id: &ObjectId) -> Result<Option<Commit>, RepoError> {
        Ok(self.storage.get_commit(id)?.map(|mut c| {
            c.enforce_caps();
            c
        }))
    }

    // -----------------------------------------------------------------------
    // Query operations
    // -----------------------------------------------------------------------

    /// Query commits with composable filters. Supports offset for pagination.
    pub fn query_commits(
        &self,
        ref_name: &str,
        filters: &agentstategraph_core::QueryFilters,
        limit: usize,
    ) -> Result<Vec<Commit>, RepoError> {
        self.query_commits_paged(ref_name, filters, limit, 0)
    }

    /// Query commits with pagination (offset + limit).
    pub fn query_commits_paged(
        &self,
        ref_name: &str,
        filters: &agentstategraph_core::QueryFilters,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Commit>, RepoError> {
        let all_commits = self.log(ref_name, 10000)?;
        let filtered = agentstategraph_core::filter_commits(&all_commits, filters);
        Ok(filtered.into_iter().skip(offset).take(limit).collect())
    }

    /// Blame — for a path, find which commit last modified it and why.
    ///
    /// The returned entry has `timestamp_anomaly` set when the commit's
    /// timestamp is `<=` at least one of its parents' timestamps — a signal
    /// of possible clock rewind. This is detection, not prevention: honest
    /// NTP skew triggers occasional flags, but persistent or concentrated
    /// flags from one agent are worth investigating.
    /// (security threat model v3+, V4)
    pub fn blame(
        &self,
        ref_name: &str,
        path: &str,
    ) -> Result<agentstategraph_core::BlameEntry, RepoError> {
        let commits = self.log(ref_name, 1000)?;
        let state_path = StatePath::parse(path)
            .map_err(|e| RepoError::Tree(tree::TreeError::PathNotFound(e.to_string())))?;

        // Walk commits and find the first one where the value at this path differs from its parent
        for commit in &commits {
            if commit.parents.is_empty() {
                // Initial commit — this is where everything was "set"
                if tree::tree_get(self.storage.as_ref(), &commit.state_root, &state_path).is_ok() {
                    return self.blame_entry_for(path, commit);
                }
            } else if let Some(parent_id) = commit.parents.first()
                && let Some(mut parent) = self.storage.get_commit(parent_id)?
            {
                // Re-cap on read — see `Commit::enforce_caps`.
                parent.enforce_caps();
                let current_val =
                    tree::tree_get(self.storage.as_ref(), &commit.state_root, &state_path);
                let parent_val =
                    tree::tree_get(self.storage.as_ref(), &parent.state_root, &state_path);

                // If the value is different (or didn't exist in parent), this commit is the blame target
                match (current_val.ok(), parent_val.ok()) {
                    (Some(curr), Some(prev)) if curr != prev => {
                        return self.blame_entry_for(path, commit);
                    }
                    (Some(_), None) => {
                        // Value was added in this commit
                        return self.blame_entry_for(path, commit);
                    }
                    _ => continue,
                }
            }
        }

        Err(RepoError::RefNotFound(format!(
            "no commit found that modified {}",
            path
        )))
    }

    /// Build a `BlameEntry` from a commit, computing the `timestamp_anomaly`
    /// flag by comparing this commit's timestamp to each parent's timestamp.
    fn blame_entry_for(
        &self,
        path: &str,
        commit: &Commit,
    ) -> Result<agentstategraph_core::BlameEntry, RepoError> {
        let anomaly = self.commit_has_timestamp_anomaly(commit)?;
        Ok(agentstategraph_core::BlameEntry {
            path: path.to_string(),
            commit_id: commit.id.short(),
            agent_id: commit.agent_id.clone(),
            intent_category: format!("{:?}", commit.intent.category),
            intent_description: commit.intent.description.clone(),
            reasoning: commit.reasoning.clone(),
            timestamp: commit.timestamp,
            timestamp_anomaly: anomaly,
        })
    }

    /// True iff this commit's timestamp is `<=` at least one of its parents'
    /// timestamps. Root commits (no parents) are always monotonic by
    /// definition.
    fn commit_has_timestamp_anomaly(&self, commit: &Commit) -> Result<bool, RepoError> {
        for parent_id in &commit.parents {
            if let Some(mut parent) = self.storage.get_commit(parent_id)? {
                parent.enforce_caps();
                if commit.timestamp <= parent.timestamp {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// Detect timestamp anomalies reachable from a ref — commits whose
    /// timestamps are `<=` at least one parent's timestamp. Returns
    /// `(commit_id, reason)` pairs. An empty list means the reachable
    /// history is monotonic.
    ///
    /// This is a READ-TIME check, not a write-time guard: a single flag is
    /// consistent with clock skew or DAG-concurrent commits, but a pattern
    /// of flags from one agent or branch is a tamper signal. Audit
    /// alongside the commit id, not in place of it.
    /// (security threat model v3+, V4)
    pub fn detect_timestamp_anomalies(
        &self,
        ref_name: &str,
    ) -> Result<Vec<(ObjectId, String)>, RepoError> {
        let commits = self.log(ref_name, 100_000)?;
        let mut findings = Vec::new();
        for commit in &commits {
            for parent_id in &commit.parents {
                if let Some(mut parent) = self.storage.get_commit(parent_id)? {
                    parent.enforce_caps();
                    if commit.timestamp <= parent.timestamp {
                        findings.push((
                            commit.id,
                            format!(
                                "commit {} timestamp {} <= parent {} timestamp {}",
                                commit.id.short(),
                                commit.timestamp.to_rfc3339(),
                                parent.id.short(),
                                parent.timestamp.to_rfc3339(),
                            ),
                        ));
                        break;
                    }
                }
            }
        }
        Ok(findings)
    }

    // -----------------------------------------------------------------------
    // Session operations (sub-agent orchestration)
    // -----------------------------------------------------------------------

    /// Get a session manager for sub-agent orchestration. The manager
    /// borrows this repository's storage backend so all state is
    /// durable.
    pub fn sessions(&self) -> crate::session::SessionManager<'_> {
        crate::session::SessionManager::new(self.storage.as_ref())
    }

    /// Set (or clear) the active epoch id. When set, commits created via
    /// `commit`/`set`/`merge`/`commit_speculation` will be associated
    /// with that epoch in storage.
    pub fn set_active_epoch(&self, id: Option<String>) -> Result<(), RepoError> {
        *self
            .active_epoch
            .write()
            .map_err(|e| RepoError::RefNotFound(e.to_string()))? = id;
        Ok(())
    }

    /// Return the currently-active epoch id, if any.
    pub fn active_epoch(&self) -> Result<Option<String>, RepoError> {
        Ok(self
            .active_epoch
            .read()
            .map_err(|e| RepoError::RefNotFound(e.to_string()))?
            .clone())
    }

    /// Set (or clear) the active session id.
    /// Eagerly caches the session's `scope_namespace` to avoid per-call storage lookups.
    pub fn set_active_session(&self, id: Option<String>) -> Result<(), RepoError> {
        // Cache the session's scope_namespace to avoid per-call storage lookups.
        let ns_override = if let Some(ref session_id) = id {
            self.storage
                .get_session(session_id)?
                .and_then(|s| s.scope_namespace)
        } else {
            None
        };
        *self
            .active_session
            .write()
            .map_err(|e| RepoError::RefNotFound(e.to_string()))? = id;
        *self
            .active_session_namespace
            .write()
            .map_err(|e| RepoError::RefNotFound(e.to_string()))? = ns_override;
        Ok(())
    }

    /// Return the currently-active session id, if any.
    pub fn active_session(&self) -> Result<Option<String>, RepoError> {
        Ok(self
            .active_session
            .read()
            .map_err(|e| RepoError::RefNotFound(e.to_string()))?
            .clone())
    }

    /// Resolve the effective namespace for the current call.
    ///
    /// Priority: active session's `scope_namespace` > repository's configured
    /// `namespace`. Falls back to `Namespace::default_ns()` only when no
    /// session is active and no namespace was configured.
    fn active_namespace(&self) -> Result<Namespace, RepoError> {
        if let Some(ns) = self
            .active_session_namespace
            .read()
            .map_err(|e| RepoError::RefNotFound(e.to_string()))?
            .clone()
        {
            return Ok(ns);
        }
        Ok(self.namespace.clone())
    }

    // -----------------------------------------------------------------------
    // Namespace operations
    // -----------------------------------------------------------------------

    /// Create a namespace. Returns `Ok(())` if already exists.
    pub fn create_namespace(&self, name: &str) -> Result<(), RepoError> {
        let ns = Namespace::new(name).map_err(|e| RepoError::InvalidOperation(e.to_string()))?;
        match self.storage.create_namespace(&ns) {
            Ok(()) => Ok(()),
            Err(StorageError::NamespaceAlreadyExists(_)) => Ok(()),
            Err(e) => Err(RepoError::Storage(e)),
        }
    }

    /// List all namespaces.
    pub fn list_namespaces(&self) -> Result<Vec<Namespace>, RepoError> {
        Ok(self.storage.list_namespaces()?)
    }

    /// Delete a namespace and all its refs. The "default" namespace cannot be deleted.
    /// Returns `true` if it existed and was removed.
    pub fn delete_namespace(&self, name: &str) -> Result<bool, RepoError> {
        let ns = Namespace::new(name).map_err(|e| RepoError::InvalidOperation(e.to_string()))?;
        Ok(self.storage.delete_namespace(&ns)?)
    }

    /// Merge a branch from a different namespace into a branch in the active namespace.
    ///
    /// Cross-namespace merges are denied by default — they require a configured
    /// PolicyStore (future work). Returns `Err(CrossNamespaceAccessDenied)` until
    /// policy integration is complete.
    ///
    /// The merge is audited: the commit's intent tags include the source namespace
    /// and branch so the history remains inspectable.
    pub fn cross_namespace_merge(
        &self,
        source_namespace: &str,
        source_branch: &str,
        target_branch: &str,
        options: CommitOptions,
    ) -> Result<ObjectId, RepoError> {
        let source_ns = Namespace::new(source_namespace)
            .map_err(|e| RepoError::InvalidOperation(e.to_string()))?;
        let target_ns = self.active_namespace()?;

        if source_ns == target_ns {
            // Same namespace — just a normal merge.
            return self.merge(source_branch, target_branch, options);
        }

        // Cross-namespace: deny until PolicyStore integration lands.
        // When a PolicyStore is wired up this check will consult it.
        Err(RepoError::CrossNamespaceAccessDenied)
    }

    // -----------------------------------------------------------------------
    // Watch operations
    // -----------------------------------------------------------------------

    /// Get the watch manager for subscribing to state changes.
    pub fn watches(&self) -> &crate::watch::WatchManager {
        &self.watch_mgr
    }

    // -----------------------------------------------------------------------
    // Epoch operations
    // -----------------------------------------------------------------------

    /// Create a new epoch, persisted via the storage backend.
    pub fn create_epoch(
        &self,
        id: &str,
        description: &str,
        root_intents: Vec<String>,
    ) -> Result<agentstategraph_core::Epoch, RepoError> {
        let epoch = agentstategraph_core::Epoch::new(id, description, root_intents);
        self.storage.create_epoch(&epoch)?;
        Ok(epoch)
    }

    /// Seal an epoch, making it immutable.
    ///
    /// Captures the set of commits reachable from `main` at seal time
    /// so that subsequent ref mutations can be checked for seal
    /// violations (a rewind of main that would orphan a sealed commit).
    /// Persisted via the storage backend — survives process restart.
    /// (security threat model v3+, V8)
    pub fn seal_epoch(&self, id: &str, summary: &str) -> Result<(), RepoError> {
        // Walk main first — cheap, and we want the sealed_commits set
        // computed against the current tip.
        let ns = self.active_namespace()?;
        let sealed_commits = match self.storage.get_ref(&ns, "main")? {
            Some(head) => self.reachable_commits_from(&head)?,
            None => Vec::new(),
        };
        self.storage
            .seal_epoch(id, summary, chrono::Utc::now(), &sealed_commits)?;
        Ok(())
    }

    /// List all epochs (as lightweight index entries).
    pub fn list_epochs(&self) -> Result<Vec<agentstategraph_core::EpochEntry>, RepoError> {
        let epochs = self.storage.list_epochs()?;
        Ok(epochs.iter().map(|e| e.to_entry()).collect())
    }

    /// Get a specific epoch by ID.
    pub fn get_epoch(&self, id: &str) -> Result<agentstategraph_core::Epoch, RepoError> {
        self.storage
            .get_epoch(id)?
            .ok_or_else(|| RepoError::RefNotFound(format!("epoch not found: {}", id)))
    }

    /// Transition a sealed epoch to Archived.
    pub fn archive_epoch(&self, id: &str) -> Result<(), RepoError> {
        self.storage.archive_epoch(id).map_err(RepoError::Storage)
    }

    /// Export a sealed or archived epoch as a self-contained JSON audit bundle.
    ///
    /// The bundle includes the epoch metadata and the full Commit records for
    /// every commit associated with the epoch, making it independently
    /// verifiable without access to the live store.
    pub fn export_epoch(&self, id: &str) -> Result<serde_json::Value, RepoError> {
        let epoch = self.get_epoch(id)?;
        if epoch.status == agentstategraph_core::EpochStatus::Active {
            return Err(RepoError::InvalidOperation(
                "cannot export an active epoch; seal it first".to_string(),
            ));
        }
        let mut commits = Vec::new();
        for cid in &epoch.commits {
            if let Some(commit) = self.storage.get_commit(cid)? {
                commits.push(serde_json::to_value(&commit).map_err(|e| {
                    RepoError::InvalidOperation(format!("serialize commit: {}", e))
                })?);
            }
        }
        let bundle = serde_json::json!({
            "agentstategraph_export_version": 1,
            "epoch": serde_json::to_value(&epoch).map_err(|e| {
                RepoError::InvalidOperation(format!("serialize epoch: {}", e))
            })?,
            "commits": commits,
            "exported_at": chrono::Utc::now().to_rfc3339(),
        });
        Ok(bundle)
    }

    // -----------------------------------------------------------------------
    // Explorer / viewer APIs (0.4.0)
    // -----------------------------------------------------------------------

    /// List all leaf paths in the state tree under a prefix.
    /// Use "/" or "" for all paths. max_depth limits recursion (default 50).
    pub fn list_paths(
        &self,
        ref_name: &str,
        prefix: &str,
        max_depth: Option<usize>,
    ) -> Result<Vec<String>, RepoError> {
        if path_is_secret(prefix) {
            return Err(RepoError::ReservedPath(prefix.to_string()));
        }
        let commit_id = self.resolve_ref(ref_name)?;
        let commit = self
            .storage
            .get_commit(&commit_id)?
            .ok_or_else(|| RepoError::RefNotFound(ref_name.to_string()))?;
        let depth = max_depth.unwrap_or(50);
        let mut paths =
            tree::tree_list_paths(self.storage.as_ref(), &commit.state_root, prefix, depth)?;
        // Filter out any results that land under the secret sub-prefix —
        // a broader prefix like "/_meta" or "/" must not leak secret names.
        paths.retain(|p| !path_is_secret(p));
        Ok(paths)
    }

    /// Get an entire subtree as nested JSON. Batch alternative to N×get calls.
    pub fn get_tree(&self, ref_name: &str, prefix: &str) -> Result<serde_json::Value, RepoError> {
        // get_json already handles this — just delegate with the prefix as path
        let path = if prefix.is_empty() { "/" } else { prefix };
        self.get_json(ref_name, path)
    }

    /// Search state values for a query string. Returns matching (path, value) pairs.
    pub fn search_values(
        &self,
        ref_name: &str,
        query: &str,
        max_results: Option<usize>,
    ) -> Result<Vec<(String, String)>, RepoError> {
        let commit_id = self.resolve_ref(ref_name)?;
        let commit = self
            .storage
            .get_commit(&commit_id)?
            .ok_or_else(|| RepoError::RefNotFound(ref_name.to_string()))?;
        let limit = max_results.unwrap_or(50);

        // Leaf-value index fast path (plan perf-slow-endpoints t-006). The index
        // is keyed by (namespace, ref) and maintained incrementally on the write
        // path (see `guarded_set_ref`). The first search of a ref after the
        // feature is enabled backfills it once (a full walk of the current
        // tree); every search after that — and after every write — is a trigram
        // substring probe. Backends without an index return `None` and we fall
        // through to the un-indexed tree walk unchanged.
        //
        // The index over-fetches (`limit * 4`, capped) before the secret filter
        // so dropping secret rows can't starve the result below `limit`; the
        // walk applies the same filter after its own cap.
        let ns = self.active_namespace()?;
        let ns_str = ns.as_str();
        let query_lower = query.to_lowercase();
        let index_fetch = limit.saturating_mul(4).min(tree::LIST_PATHS_MAX_RESULTS);

        if !self.storage.leaf_index_is_built(ns_str, ref_name)? {
            // One-time backfill. Non-fatal on error — we simply fall back to the
            // tree walk below and can retry the backfill on a later search.
            if let Ok(leaves) = tree::tree_collect_leaves(self.storage.as_ref(), &commit.state_root)
            {
                let _ = self.storage.leaf_index_build(ns_str, ref_name, &leaves);
            }
        }

        if let Some(mut hits) =
            self.storage
                .leaf_index_search(ns_str, ref_name, &query_lower, index_fetch)?
        {
            hits.retain(|(path, _)| !path_is_secret(path));
            hits.truncate(limit);
            return Ok(hits);
        }

        // Fallback: un-indexed tree walk (backend has no leaf index).
        let mut results =
            tree::tree_search_values(self.storage.as_ref(), &commit.state_root, query, limit)?;
        // Never surface values from the secret sub-prefix.
        results.retain(|(path, _)| !path_is_secret(path));
        Ok(results)
    }

    /// Get summary statistics for a ref.
    pub fn stats(&self, ref_name: &str) -> Result<serde_json::Value, RepoError> {
        let commits = self.log(ref_name, 10000)?;
        let branches = self.list_branches(None)?;
        let paths = self.list_paths(ref_name, "/", Some(100))?;
        let epochs = self.list_epochs()?;

        // Collect unique agent IDs
        let mut agents: Vec<String> = commits
            .iter()
            .map(|c| c.agent_id.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        agents.sort();

        // Collect unique intent categories
        let mut categories: Vec<String> = commits
            .iter()
            .map(|c| format!("{:?}", c.intent.category))
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        categories.sort();

        Ok(serde_json::json!({
            "commit_count": commits.len(),
            "branch_count": branches.len(),
            "path_count": paths.len(),
            "epoch_count": epochs.len(),
            "agents": agents,
            "categories": categories,
            "latest_commit": commits.first().map(|c| serde_json::json!({
                "id": c.id.short(),
                "agent": c.agent_id,
                "intent": c.intent.description,
                "timestamp": c.timestamp.to_rfc3339(),
            })),
        }))
    }

    /// Get commit graph (DAG) for visualization.
    /// Returns commit nodes with their parents and metadata.
    /// Bring the distilled project-history tables (`asg_history_*`) up to date
    /// with the commit chain (Plan A t-001). Folds every commit newer than the
    /// stored cursor into the rollup/milestone tables in `batch_size` chunks,
    /// so a 512k-commit store is processed in bounded memory. Incremental: on a
    /// caught-up store this is a cheap no-op; after new commits it processes
    /// only those. Returns how many commits were folded and the new cursor.
    pub fn extract_history(&self, batch_size: usize) -> Result<HistoryExtractReport, RepoError> {
        let batch_size = batch_size.max(1);
        let mut total = 0usize;
        loop {
            let n = self.storage.history_extract_batch(batch_size)?;
            if n == 0 {
                break;
            }
            total += n;
        }
        Ok(HistoryExtractReport {
            commits_processed: total,
            cursor: self.storage.history_cursor()?,
        })
    }

    /// Read the commit-history rollup (see [`Repository::extract_history`]).
    pub fn history_rollup(&self) -> Result<Vec<HistoryRollupRow>, RepoError> {
        Ok(self.storage.history_rollup()?)
    }

    /// Read the milestone timeline, most recent first, capped at `limit`.
    pub fn history_milestones(&self, limit: usize) -> Result<Vec<HistoryMilestoneRow>, RepoError> {
        Ok(self.storage.history_milestones(limit)?)
    }

    /// Build the `asg history` report (Plan A t-002) from the distilled
    /// `asg_history_*` tables: commit velocity (by `day` or `week`), intent
    /// mix, authorship, and the milestone timeline. When `refresh` is set the
    /// extractor is brought current first, so the report always reflects the
    /// latest commits. `namespace`, when given, restricts every view to that
    /// namespace. The rollup is small (bounded by distinct day×ns×agent×category
    /// buckets), so aggregating it in memory is cheap even on a 512k-commit
    /// store. When `store` is set, a `store_shape` block (Plan A t-003:
    /// objects/commits/bytes/amplification) is attached.
    pub fn history_report(
        &self,
        namespace: Option<&str>,
        velocity_by: &str,
        milestone_limit: usize,
        refresh: bool,
        store: bool,
    ) -> Result<serde_json::Value, RepoError> {
        use std::collections::{BTreeMap, BTreeSet};

        if refresh {
            self.extract_history(DEFAULT_HISTORY_BATCH)?;
        }
        let by_week = velocity_by.eq_ignore_ascii_case("week");
        let period_label = if by_week { "week" } else { "day" };

        let mut rollup = self.history_rollup()?;
        let mut milestones = self.history_milestones(milestone_limit.max(1))?;
        if let Some(ns) = namespace {
            rollup.retain(|r| r.namespace == ns);
            milestones.retain(|m| m.namespace == ns);
        }

        let mut velocity: BTreeMap<String, i64> = BTreeMap::new();
        let mut mix: BTreeMap<String, i64> = BTreeMap::new();
        let mut authorship: BTreeMap<String, i64> = BTreeMap::new();
        let mut namespaces: BTreeSet<String> = BTreeSet::new();
        let mut total_commits = 0i64;
        for r in &rollup {
            total_commits += r.commit_count;
            let period = if by_week {
                iso_week_key(&r.day).unwrap_or_else(|| r.day.clone())
            } else {
                r.day.clone()
            };
            *velocity.entry(period).or_default() += r.commit_count;
            *mix.entry(r.intent_category.clone()).or_default() += r.commit_count;
            *authorship.entry(r.agent_id.clone()).or_default() += r.commit_count;
            namespaces.insert(r.namespace.clone());
        }

        // Velocity stays chronological (BTreeMap key order). Mix and authorship
        // are ranked by volume, ties broken by name for determinism.
        let velocity_series: Vec<serde_json::Value> = velocity
            .iter()
            .map(|(k, v)| serde_json::json!({ "period": k, "commits": v }))
            .collect();
        let rank = |m: BTreeMap<String, i64>, key: &str| -> Vec<serde_json::Value> {
            let mut v: Vec<(String, i64)> = m.into_iter().collect();
            v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            v.into_iter()
                .map(|(k, c)| serde_json::json!({ key: k, "commits": c }))
                .collect()
        };
        let milestones_arr: Vec<serde_json::Value> = milestones
            .iter()
            .map(|m| {
                serde_json::json!({
                    "commit": m.commit_id.short(),
                    "kind": m.kind,
                    "timestamp": m.timestamp,
                    "day": m.day,
                    "namespace": m.namespace,
                    "agent": m.agent_id,
                    "description": m.description,
                })
            })
            .collect();

        let mut report = serde_json::json!({
            "totals": {
                "commits": total_commits,
                "periods": velocity_series.len(),
                "agents": authorship.len(),
                "cursor": self.storage.history_cursor()?,
            },
            "velocity": { "by": period_label, "series": velocity_series },
            "intent_mix": rank(mix, "category"),
            "authorship": rank(authorship, "agent"),
            "namespaces": namespaces.into_iter().collect::<Vec<_>>(),
            "milestones": milestones_arr,
        });
        if store {
            if let Some(obj) = report.as_object_mut() {
                obj.insert("store_shape".to_string(), self.history_store_shape()?);
            }
        }
        Ok(report)
    }

    /// Physical store-shape report (Plan A t-003): object/commit counts, total
    /// bytes, per-table bytes (when the SQLite build exposes `dbstat`), and the
    /// path-copy amplification (avg objects created per commit) — the evidence
    /// Plan B's GC uses to set retention thresholds.
    pub fn history_store_shape(&self) -> Result<serde_json::Value, RepoError> {
        let s = self.storage.history_store_shape()?;
        let amplification = if s.commits > 0 {
            s.objects as f64 / s.commits as f64
        } else {
            0.0
        };
        let tables: Vec<serde_json::Value> = s
            .tables
            .iter()
            .map(|t| serde_json::json!({ "name": t.name, "bytes": t.bytes }))
            .collect();
        Ok(serde_json::json!({
            "objects": s.objects,
            "commits": s.commits,
            "total_bytes": s.total_bytes,
            "path_copy_amplification": { "objects_per_commit": amplification },
            "dbstat_available": s.dbstat_available,
            "tables": tables,
        }))
    }

    pub fn commit_graph(
        &self,
        ref_name: &str,
        depth: usize,
    ) -> Result<Vec<serde_json::Value>, RepoError> {
        let commits = self.log(ref_name, depth)?;
        let nodes: Vec<serde_json::Value> = commits
            .iter()
            .map(|c| {
                serde_json::json!({
                    "id": c.id.short(),
                    "full_id": c.id.to_string(),
                    "parents": c.parents.iter().map(|p| p.short()).collect::<Vec<_>>(),
                    "agent": c.agent_id,
                    "category": format!("{:?}", c.intent.category),
                    "description": c.intent.description,
                    "confidence": c.confidence,
                    "timestamp": c.timestamp.to_rfc3339(),
                    "is_merge": c.parents.len() > 1,
                })
            })
            .collect();
        Ok(nodes)
    }

    /// Build the intent-decomposition tree for a ref.
    ///
    /// Links intents by [`Intent::parent_intent`] — the sub-intent threading
    /// the RFC describes ("Sub-intent links to parent intent via
    /// `parent_intent`, forming a queryable tree"). Roots are the intents with
    /// no parent (or, when `root_commit_id` is given, that single commit's
    /// intent); a node's children are the commits whose `intent.parent_intent`
    /// equals this node's intent id.
    ///
    /// This is deliberately NOT a commit-lineage walk — for a tree structured
    /// by commit parents, use [`Repository::commit_graph`]. Until producers
    /// populate `parent_intent` (via [`Intent::with_parent`]), every intent is
    /// its own root and the result is a flat list — an honest "no threading
    /// recorded yet", not a lie about hierarchy that isn't there.
    pub fn intent_tree(
        &self,
        ref_name: &str,
        root_commit_id: Option<&str>,
    ) -> Result<serde_json::Value, RepoError> {
        let commits = self.log(ref_name, 10000)?;

        // Roots: an explicit commit's intent, else every intent with no parent.
        let roots: Vec<&Commit> = if let Some(root_id) = root_commit_id {
            commits.iter().filter(|c| c.id.short() == root_id).collect()
        } else {
            commits
                .iter()
                .filter(|c| c.intent.parent_intent.is_none())
                .collect()
        };

        // `visiting` carries the ancestor intent-ids on the current path so a
        // user-authored `parent_intent` cycle can't spin the recursion forever.
        fn build_intent_node(
            commit: &Commit,
            all_commits: &[Commit],
            visiting: &mut std::collections::HashSet<String>,
        ) -> serde_json::Value {
            let inserted = visiting.insert(commit.intent.id.clone());
            let children: Vec<serde_json::Value> = if inserted {
                all_commits
                    .iter()
                    .filter(|c| {
                        c.intent.parent_intent.as_deref() == Some(commit.intent.id.as_str())
                    })
                    .map(|child| build_intent_node(child, all_commits, visiting))
                    .collect()
            } else {
                // Cycle: this intent is already an ancestor on the path. Emit the
                // node without descending again.
                Vec::new()
            };
            if inserted {
                visiting.remove(&commit.intent.id);
            }

            serde_json::json!({
                "id": commit.id.short(),
                "intent_id": commit.intent.id,
                "agent": commit.agent_id,
                "category": format!("{:?}", commit.intent.category),
                "description": commit.intent.description,
                "reasoning": commit.reasoning,
                "confidence": commit.confidence,
                "timestamp": commit.timestamp.to_rfc3339(),
                "children": children,
            })
        }

        let tree: Vec<serde_json::Value> = roots
            .iter()
            .map(|c| {
                let mut visiting = std::collections::HashSet::new();
                build_intent_node(c, &commits, &mut visiting)
            })
            .collect();

        Ok(serde_json::json!({
            "roots": tree,
            "total_commits": commits.len(),
        }))
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Find the best common ancestor (merge base) of two commits.
    ///
    /// Walks the full commit DAG following EVERY parent (merge commits have
    /// two), not just the first — a first-parent-only walk misses the true
    /// lowest common ancestor once merge commits exist, and picking too old a
    /// base makes the target's own additions look like deletions to the merge
    /// engine. Among all common ancestors, returns the deepest one (closest to
    /// the two heads); ties are broken deterministically by commit id.
    fn find_common_ancestor(&self, a: &ObjectId, b: &ObjectId) -> Result<ObjectId, RepoError> {
        if a == b {
            return Ok(*a);
        }
        let ancestors_a = self.ancestor_set(a)?;

        // Collect every ancestor of `b` that is also an ancestor of `a`.
        let mut common = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut stack = vec![*b];
        while let Some(id) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            if ancestors_a.contains(&id) {
                common.push(id);
            }
            if let Some(commit) = self.storage.get_commit(&id)? {
                for p in &commit.parents {
                    if !seen.contains(p) {
                        stack.push(*p);
                    }
                }
            }
        }

        // Pick the deepest common ancestor (largest generation = closest to the
        // heads). Deterministic tie-break by id bytes keeps merges reproducible.
        let mut best: Option<(u64, ObjectId)> = None;
        for id in common {
            let depth = self.commit_depth(&id)?;
            let candidate = (depth, id);
            let better = match &best {
                None => true,
                Some(cur) => candidate.0 > cur.0 || (candidate.0 == cur.0 && candidate.1 > cur.1),
            };
            if better {
                best = Some(candidate);
            }
        }
        best.map(|(_, id)| id).ok_or_else(|| {
            // Disjoint histories share no ancestor — refuse rather than
            // silently merging against an empty/arbitrary base.
            RepoError::RefNotFound("no common ancestor between refs".to_string())
        })
    }

    /// Collect every ancestor of `id` (including `id` itself), following all
    /// parents of every commit.
    fn ancestor_set(
        &self,
        id: &ObjectId,
    ) -> Result<std::collections::HashSet<ObjectId>, RepoError> {
        let mut seen = std::collections::HashSet::new();
        let mut stack = vec![*id];
        while let Some(cur) = stack.pop() {
            if !seen.insert(cur) {
                continue;
            }
            if let Some(commit) = self.storage.get_commit(&cur)? {
                for p in &commit.parents {
                    if !seen.contains(p) {
                        stack.push(*p);
                    }
                }
            }
        }
        Ok(seen)
    }

    /// Generation depth of a commit: the number of commits on the longest path
    /// from `id` back to a root commit (a commit with no parents). Memoized per
    /// call is unnecessary for the small histories we merge; a plain recursive
    /// walk with a visited guard is sufficient and avoids unbounded recursion
    /// via an explicit stack.
    fn commit_depth(&self, id: &ObjectId) -> Result<u64, RepoError> {
        // Iterative longest-path via post-order over the ancestor DAG.
        let mut depth: std::collections::HashMap<ObjectId, u64> = std::collections::HashMap::new();
        // Process in an order where all parents precede a node: repeatedly
        // resolve nodes whose parents are all known.
        let ancestors = self.ancestor_set(id)?;
        let mut pending: Vec<ObjectId> = ancestors.iter().copied().collect();
        // Bounded number of passes (== number of nodes) guarantees termination
        // on a DAG.
        for _ in 0..=ancestors.len() {
            let mut progressed = false;
            pending.retain(|node| {
                if depth.contains_key(node) {
                    return false;
                }
                let parents = match self.storage.get_commit(node) {
                    Ok(Some(commit)) => commit.parents.clone(),
                    _ => Vec::new(),
                };
                if parents.iter().all(|p| depth.contains_key(p)) {
                    let d = parents
                        .iter()
                        .filter_map(|p| depth.get(p))
                        .map(|d| d + 1)
                        .max()
                        .unwrap_or(0);
                    depth.insert(*node, d);
                    progressed = true;
                    false
                } else {
                    true
                }
            });
            if pending.is_empty() || !progressed {
                break;
            }
        }
        Ok(depth.get(id).copied().unwrap_or(0))
    }

    /// Walk the state tree from `root`, returning the id of the first object
    /// that is referenced but absent from the store, or `None` if the whole
    /// tree is fully readable. Iterative with a visited-set so shared subtrees
    /// and cycles (content-addressing makes true cycles impossible, but the
    /// guard is cheap) are each visited once. Used as the pre-ref-advance
    /// integrity gate for merges.
    fn first_missing_reachable(&self, root: &ObjectId) -> Result<Option<ObjectId>, RepoError> {
        use agentstategraph_core::Node;
        let mut seen = std::collections::HashSet::new();
        let mut stack = vec![*root];
        while let Some(id) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            match self.storage.get_object(&id)? {
                None => return Ok(Some(id)),
                Some(Object::Node(node)) => match node {
                    Node::Map(entries) => stack.extend(entries.values().copied()),
                    Node::List(items) | Node::Set(items) => stack.extend(items.iter().copied()),
                },
                Some(Object::Atom(_)) => {}
            }
        }
        Ok(None)
    }

    /// Integrity check for tooling (e.g. `ctx db fsck`): the id of the first
    /// object referenced by `commit`'s state tree that is missing from the
    /// store, or `None` if the commit's tree is fully readable. Shares the
    /// exact walk used by the merge integrity gate.
    pub fn first_missing_object(
        &self,
        commit_id: &ObjectId,
    ) -> Result<Option<ObjectId>, RepoError> {
        let commit = self
            .storage
            .get_commit(commit_id)?
            .ok_or_else(|| RepoError::CommitNotFound(commit_id.short()))?;
        self.first_missing_reachable(&commit.state_root)
    }

    /// The nearest commit at or before `commit_id` (walking first-parent) whose
    /// state tree is fully readable — the safe rewind target when a ref points
    /// at a commit with a dangling tree. Returns `None` if no ancestor in the
    /// chain is intact (e.g. corruption at the root of history). Because no GC
    /// ever deletes objects, an intact ancestor's tree stays intact.
    pub fn nearest_readable_ancestor(
        &self,
        commit_id: &ObjectId,
    ) -> Result<Option<ObjectId>, RepoError> {
        let mut current = Some(*commit_id);
        while let Some(id) = current {
            match self.storage.get_commit(&id)? {
                None => return Ok(None),
                Some(commit) => {
                    if self.first_missing_reachable(&commit.state_root)?.is_none() {
                        return Ok(Some(id));
                    }
                    current = commit.parents.first().copied();
                }
            }
        }
        Ok(None)
    }

    /// Walk the commit DAG backwards from `head`, returning every reachable
    /// commit id (including `head` itself). Follows every parent — merge
    /// commits include both sides.
    fn reachable_commits_from(&self, head: &ObjectId) -> Result<Vec<ObjectId>, RepoError> {
        let mut seen = std::collections::HashSet::new();
        let mut stack = vec![*head];
        while let Some(id) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            if let Some(commit) = self.storage.get_commit(&id)? {
                for p in &commit.parents {
                    if !seen.contains(p) {
                        stack.push(*p);
                    }
                }
            }
        }
        Ok(seen.into_iter().collect())
    }

    /// Given a proposed new ref target, return one `EpochViolation` per
    /// sealed epoch whose `sealed_commits` include any commit that is NOT
    /// reachable from the new target.
    ///
    /// An empty result means the proposed update preserves every seal.
    /// (security threat model v3+, V8)
    pub fn check_epoch_seal_violations(
        &self,
        new_ref_target: &ObjectId,
    ) -> Result<Vec<EpochViolation>, RepoError> {
        let reachable: std::collections::HashSet<ObjectId> = self
            .reachable_commits_from(new_ref_target)?
            .into_iter()
            .collect();
        let epochs = self.storage.list_epochs()?;
        let mut violations = Vec::new();
        for epoch in epochs.iter() {
            if epoch.status != agentstategraph_core::EpochStatus::Sealed
                && epoch.status != agentstategraph_core::EpochStatus::Archived
            {
                continue;
            }
            let unreachable: Vec<ObjectId> = epoch
                .sealed_commits
                .iter()
                .copied()
                .filter(|c| !reachable.contains(c))
                .collect();
            if !unreachable.is_empty() {
                violations.push(EpochViolation {
                    epoch_id: epoch.id.clone(),
                    unreachable_commits: unreachable,
                });
            }
        }
        Ok(violations)
    }

    /// Enforce epoch seals against a proposed `set_ref` to `ref_name`.
    /// In strict mode (default) returns `RepoError::EpochSealViolated`. In
    /// warn mode (opt-out) logs to stderr and proceeds.
    fn enforce_epoch_seals(&self, ref_name: &str, new_target: &ObjectId) -> Result<(), RepoError> {
        // Only `main` is the canonical seal anchor; branch/spec updates
        // don't orphan sealed commits reachable from main because main
        // itself hasn't moved. We still check on `main` mutations and on
        // explicit low-level set_ref for any ref (cheap insurance).
        let violations = self.check_epoch_seal_violations(new_target)?;
        if violations.is_empty() {
            return Ok(());
        }
        // Prefer main-only enforcement: a side-branch can legitimately sit
        // on a state that doesn't include every sealed commit. Skip unless
        // the ref being mutated is `main`.
        if ref_name != "main" {
            return Ok(());
        }
        if self.epoch_seal_strict {
            let v = violations.into_iter().next().expect("non-empty");
            return Err(RepoError::EpochSealViolated {
                epoch_id: v.epoch_id,
                unreachable_commits: v.unreachable_commits,
            });
        }
        for v in &violations {
            eprintln!(
                "[WARN] asg epoch seal: ref '{}' → {} would orphan {} commit(s) from sealed epoch '{}' (e.g. {})",
                ref_name,
                new_target.short(),
                v.unreachable_commits.len(),
                v.epoch_id,
                v.unreachable_commits
                    .first()
                    .map(|c| c.short())
                    .unwrap_or_default(),
            );
        }
        Ok(())
    }

    /// Wrapper around `storage.set_ref` that first runs the epoch-seal
    /// enforcement check. Every internal code path that moves a ref goes
    /// through this.
    fn guarded_set_ref(&self, ref_name: &str, new_target: ObjectId) -> Result<(), RepoError> {
        let ns = self.active_namespace()?;
        self.enforce_epoch_seals(ref_name, &new_target)?;
        // Capture the outgoing state root before the ref moves, so the leaf
        // index can be updated with just the delta this write introduces.
        let old_root = self
            .storage
            .get_ref(&ns, ref_name)
            .ok()
            .flatten()
            .and_then(|cid| self.storage.get_commit(&cid).ok().flatten())
            .map(|c| c.state_root);
        self.storage.set_ref(&ns, ref_name, new_target)?;
        // Incremental leaf-index maintenance (plan t-006). Best-effort: index
        // upkeep must never fail a write, and it only runs for a set that has
        // already been backfilled — otherwise the first search will build it
        // from the current state anyway.
        if self
            .storage
            .leaf_index_is_built(ns.as_str(), ref_name)
            .unwrap_or(false)
            && let Ok(Some(new_commit)) = self.storage.get_commit(&new_target)
            && let Ok((removed, added)) = tree::tree_diff_leaves(
                self.storage.as_ref(),
                old_root.as_ref(),
                &new_commit.state_root,
            )
        {
            let _ = self
                .storage
                .leaf_index_apply(ns.as_str(), ref_name, &removed, &added);
        }
        Ok(())
    }

    /// Low-level: move a ref to a specific commit id, subject to epoch-seal
    /// enforcement. Used by migration tooling and by tests that need to
    /// simulate rewinds. Prefer `set`/`merge`/etc for normal writes.
    pub fn set_ref(&self, ref_name: &str, target: ObjectId) -> Result<(), RepoError> {
        self.guarded_set_ref(ref_name, target)
    }

    /// Resolve a ref name to a commit ID.
    /// Resolve a ref-spec to a commit id. Resolution order:
    ///   1. exact branch name
    ///   2. exact full commit hash (optionally `sg_`-prefixed)
    ///   3. unique `sg_`/hex commit-id prefix
    ///
    /// A value that is neither an existing branch nor hex-shaped yields
    /// `BranchNotFound`; a hex-shaped value that matches no commit yields
    /// `CommitNotFound`; a prefix matching more than one commit yields
    /// `AmbiguousCommitPrefix`.
    ///
    /// (Tags are reserved for step 2 in the spec but not yet stored, so they
    /// are skipped here.)
    fn resolve_ref(&self, ref_name: &str) -> Result<ObjectId, RepoError> {
        let ns = self.active_namespace()?;
        // 1. Exact branch name — always wins, even if it looks like hex.
        if let Some(id) = self.storage.get_ref(&ns, ref_name)? {
            return Ok(id);
        }
        // 2 & 3. Commit hash / prefix.
        self.resolve_commit_ref(ref_name)
    }

    /// Resolve a value that is not a branch name as a commit id or id prefix.
    fn resolve_commit_ref(&self, ref_name: &str) -> Result<ObjectId, RepoError> {
        // Full 64-char hash (with or without sg_): direct existence check.
        if let Some(id) = ObjectId::from_hex(ref_name) {
            if self.storage.has_commit(&id)? {
                return Ok(id);
            }
            return Err(RepoError::CommitNotFound(ref_name.to_string()));
        }

        // Non-hex input is not a commit ref — report it as a missing branch,
        // matching the previous behaviour for ordinary bad names.
        let prefix = match ObjectId::normalize_hex_prefix(ref_name) {
            Some(p) => p,
            None => return Err(RepoError::BranchNotFound(ref_name.to_string())),
        };

        // Unique prefix over ALL commits (including orphaned/unreferenced ones,
        // which is the whole point of resolving historical ids).
        let mut matches: Vec<ObjectId> = self
            .storage
            .all_commit_ids()?
            .into_iter()
            .filter(|id| id.to_hex().starts_with(&prefix))
            .collect();
        matches.sort();
        matches.dedup();
        match matches.len() {
            0 => Err(RepoError::CommitNotFound(ref_name.to_string())),
            1 => Ok(matches[0]),
            n => Err(RepoError::AmbiguousCommitPrefix {
                prefix: ref_name.to_string(),
                count: n,
            }),
        }
    }

    /// Create a commit and store it.
    fn create_commit(
        &self,
        state_root: ObjectId,
        parents: Vec<ObjectId>,
        options: CommitOptions,
    ) -> Result<Commit, RepoError> {
        let mut builder = CommitBuilder::new(
            state_root,
            options.agent_id,
            options.authority,
            options.intent,
        )
        .parents(parents);

        if let Some(reasoning) = options.reasoning {
            builder = builder.reasoning(reasoning);
        }
        if let Some(confidence) = options.confidence {
            builder = builder.confidence(confidence);
        }
        if !options.tool_calls.is_empty() {
            builder = builder.tool_calls(options.tool_calls);
        }

        let commit = builder.build();
        self.storage.put_commit(&commit)?;
        // Associate with the active epoch/session if any. Association
        // errors are surfaced — a sealed active epoch shouldn't silently
        // drop its mark. Callers can unset the active epoch before
        // retrying.
        if let Some(epoch_id) = self.active_epoch()? {
            self.storage.set_commit_epoch(&commit.id, &epoch_id)?;
        }
        if let Some(session_id) = self.active_session()? {
            self.storage.set_commit_session(&commit.id, &session_id)?;
        }
        Ok(commit)
    }

    // -----------------------------------------------------------------------
    // Accessors used by the taint module (0.7.75 §4)
    // -----------------------------------------------------------------------

    /// Access the underlying Storage (read-only).
    pub(crate) fn taint_storage(&self) -> &dyn Storage {
        self.storage.as_ref()
    }

    /// Write a bare "intent-only" commit with no state-tree change —
    /// used by the taint / quarantine / watch family to stamp an
    /// audit commit without mutating state. Returns the new commit id.
    pub(crate) fn write_taint_intent(
        &self,
        ref_name: &str,
        category: IntentCategory,
        description: String,
        agent_id: &str,
        reasoning: Option<String>,
    ) -> Result<ObjectId, RepoError> {
        let parent_id = self.resolve_ref(ref_name)?;
        let parent = self
            .storage
            .get_commit(&parent_id)?
            .ok_or_else(|| RepoError::RefNotFound(ref_name.to_string()))?;
        let mut options = CommitOptions::new(agent_id, category, description);
        if let Some(r) = reasoning {
            options = options.with_reasoning(r);
        }
        let commit = self.create_commit(parent.state_root, vec![parent_id], options)?;
        self.guarded_set_ref(ref_name, commit.id)?;
        Ok(commit.id)
    }
}

impl agentstategraph_reminders::ReminderStore for Repository {
    fn save(
        &self,
        reminder: &agentstategraph_reminders::Reminder,
    ) -> Result<(), agentstategraph_reminders::ReminderError> {
        self.storage.save(reminder)
    }

    fn get(
        &self,
        id: &str,
    ) -> Result<Option<agentstategraph_reminders::Reminder>, agentstategraph_reminders::ReminderError>
    {
        self.storage.get(id)
    }

    fn update(
        &self,
        reminder: &agentstategraph_reminders::Reminder,
    ) -> Result<(), agentstategraph_reminders::ReminderError> {
        self.storage.update(reminder)
    }

    fn delete(&self, id: &str) -> Result<bool, agentstategraph_reminders::ReminderError> {
        self.storage.delete(id)
    }

    fn list(
        &self,
        filter: &agentstategraph_reminders::ReminderFilter,
    ) -> Result<Vec<agentstategraph_reminders::Reminder>, agentstategraph_reminders::ReminderError>
    {
        self.storage.list(filter)
    }
}

/// Bridge between storage backends and the diff engine's ObjectResolver trait.
struct StorageResolver<'a> {
    storage: &'a dyn Storage,
}

impl<'a> ObjectResolver for StorageResolver<'a> {
    fn resolve(&self, id: &ObjectId) -> Option<Object> {
        self.storage.get_object(id).ok().flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentstategraph_storage::SqliteStorage;

    fn test_repo() -> Repository {
        let storage = SqliteStorage::in_memory().expect("in-memory sqlite");
        let repo = Repository::new(Box::new(storage));
        repo.init().unwrap();
        repo
    }

    fn quick_opts(desc: &str) -> CommitOptions {
        CommitOptions::new("agent/test", IntentCategory::Checkpoint, desc)
    }

    #[test]
    fn test_init_creates_main() {
        let repo = test_repo();
        let branches = repo.list_branches(None).unwrap();
        assert_eq!(branches.len(), 1);
        assert_eq!(branches[0].0, "main");
    }

    #[test]
    fn test_init_stamps_schema_version() {
        let repo = test_repo();
        let v = repo.get("main", META_SCHEMA_VERSION_PATH).unwrap();
        match v {
            Object::Atom(agentstategraph_core::Atom::String(s)) => {
                assert_eq!(s, SCHEMA_VERSION);
            }
            other => panic!("expected string atom, got {:?}", other),
        }
    }

    #[test]
    fn test_meta_guard_rejects_non_migrate_writes() {
        let repo = test_repo();
        let value = Object::Atom(agentstategraph_core::Atom::String("hack".into()));
        let err = repo
            .set(
                "main",
                META_SCHEMA_VERSION_PATH,
                &value,
                quick_opts("tamper"),
            )
            .unwrap_err();
        assert!(matches!(err, RepoError::ReservedPath(_)), "got {:?}", err);

        let err = repo
            .set("main", "/_meta/custom", &value, quick_opts("tamper2"))
            .unwrap_err();
        assert!(matches!(err, RepoError::ReservedPath(_)), "got {:?}", err);

        let err = repo
            .delete("main", META_SCHEMA_VERSION_PATH, quick_opts("tamper3"))
            .unwrap_err();
        assert!(matches!(err, RepoError::ReservedPath(_)), "got {:?}", err);
    }

    #[test]
    fn test_meta_guard_allows_migrate_writes() {
        let repo = test_repo();
        let value = Object::Atom(agentstategraph_core::Atom::String("0.5.0".into()));
        let opts = CommitOptions::new("agent/migrate", IntentCategory::Migrate, "bump schema");
        repo.set("main", META_SCHEMA_VERSION_PATH, &value, opts)
            .expect("migrate intent should be allowed to write /_meta");

        let got = repo.get("main", META_SCHEMA_VERSION_PATH).unwrap();
        match got {
            Object::Atom(agentstategraph_core::Atom::String(s)) => assert_eq!(s, "0.5.0"),
            other => panic!("unexpected {:?}", other),
        }
    }

    #[test]
    fn test_secret_read_guard_rejects_non_migrate_reads() {
        let repo = test_repo();

        // Seed a value under /_meta/_secret via a Migrate write.
        let value = Object::Atom(agentstategraph_core::Atom::String("shh".into()));
        let migrate_opts =
            CommitOptions::new("agent/migrate", IntentCategory::Migrate, "seed secret");
        repo.set("main", "/_meta/_secret/api_key", &value, migrate_opts)
            .expect("migrate can write to /_meta/_secret");

        // Default get() must reject.
        let err = repo
            .get("main", "/_meta/_secret/api_key")
            .expect_err("non-migrate read of secret must fail");
        assert!(matches!(err, RepoError::ReservedPath(_)), "got {:?}", err);

        // list_paths over /_meta must NOT surface the secret subtree.
        let paths = repo.list_paths("main", "/_meta", None).unwrap();
        assert!(
            paths.iter().all(|p| !p.starts_with("/_meta/_secret")),
            "list_paths leaked secret paths: {:?}",
            paths
        );

        // Directly listing the secret prefix is rejected outright.
        let err = repo
            .list_paths("main", "/_meta/_secret", None)
            .expect_err("list_paths on secret prefix must fail");
        assert!(matches!(err, RepoError::ReservedPath(_)), "got {:?}", err);
    }

    #[test]
    fn test_secret_read_guard_allows_migrate() {
        let repo = test_repo();
        let value = Object::Atom(agentstategraph_core::Atom::String("shh".into()));
        let opts = CommitOptions::new("agent/migrate", IntentCategory::Migrate, "seed");
        repo.set("main", "/_meta/_secret/token", &value, opts)
            .unwrap();

        let intent = Intent::new(IntentCategory::Migrate, "read secret for migration");
        let got = repo
            .get_with_intent("main", "/_meta/_secret/token", &intent)
            .expect("migrate read should succeed");
        match got {
            Object::Atom(agentstategraph_core::Atom::String(s)) => assert_eq!(s, "shh"),
            other => panic!("unexpected {:?}", other),
        }

        // Non-migrate intent, even via the explicit-intent method, must fail.
        let bad_intent = Intent::new(IntentCategory::Checkpoint, "sneak a peek");
        let err = repo
            .get_with_intent("main", "/_meta/_secret/token", &bad_intent)
            .unwrap_err();
        assert!(matches!(err, RepoError::ReservedPath(_)), "got {:?}", err);
    }

    #[test]
    fn test_meta_guard_does_not_match_user_paths_with_meta_substring() {
        let repo = test_repo();
        let value = Object::Atom(agentstategraph_core::Atom::Int(1));
        // user-space path that happens to share the prefix substring
        repo.set("main", "/_metadata_not_reserved", &value, quick_opts("ok"))
            .expect("unrelated path must not trip the guard");
    }

    #[test]
    fn test_set_and_get() {
        let repo = test_repo();

        repo.set(
            "main",
            "/name",
            &Object::string("my-cluster"),
            quick_opts("set name"),
        )
        .unwrap();

        let obj = repo.get("main", "/name").unwrap();
        assert_eq!(obj, Object::string("my-cluster"));
    }

    #[test]
    fn test_set_json_and_get_json() {
        let repo = test_repo();

        repo.set_json(
            "main",
            "/config",
            &serde_json::json!({
                "network": { "subnet": "10.0.0.0/24" },
                "gpu": { "enabled": true }
            }),
            quick_opts("set config"),
        )
        .unwrap();

        let json = repo.get_json("main", "/config/network/subnet").unwrap();
        assert_eq!(json, serde_json::json!("10.0.0.0/24"));

        let gpu = repo.get_json("main", "/config/gpu/enabled").unwrap();
        assert_eq!(gpu, serde_json::json!(true));
    }

    #[test]
    fn test_delete() {
        let repo = test_repo();

        repo.set(
            "main",
            "/temp",
            &Object::string("temporary"),
            quick_opts("add temp"),
        )
        .unwrap();

        repo.delete("main", "/temp", quick_opts("remove temp"))
            .unwrap();

        assert!(repo.get("main", "/temp").is_err());
    }

    #[test]
    fn test_branch_and_diverge() {
        let repo = test_repo();

        // Set initial state
        repo.set("main", "/value", &Object::int(1), quick_opts("initial"))
            .unwrap();

        // Create branch
        repo.branch("feature", "main").unwrap();

        // Modify main
        repo.set("main", "/value", &Object::int(2), quick_opts("update main"))
            .unwrap();

        // Modify branch
        repo.set(
            "feature",
            "/value",
            &Object::int(3),
            quick_opts("update feature"),
        )
        .unwrap();

        // Both diverged
        assert_eq!(repo.get("main", "/value").unwrap(), Object::int(2));
        assert_eq!(repo.get("feature", "/value").unwrap(), Object::int(3));
    }

    #[test]
    fn test_branch_already_exists() {
        let repo = test_repo();
        assert!(repo.branch("main", "main").is_err());
    }

    #[test]
    fn test_delete_branch() {
        let repo = test_repo();
        repo.branch("temp-branch", "main").unwrap();
        assert!(repo.delete_branch("temp-branch").unwrap());
        assert!(!repo.delete_branch("temp-branch").unwrap()); // already deleted
    }

    #[test]
    fn test_list_branches_with_prefix() {
        let repo = test_repo();
        repo.branch("agents/planner/workspace", "main").unwrap();
        repo.branch("agents/storage/workspace", "main").unwrap();
        repo.branch("explore/nfs", "main").unwrap();

        let agent_branches = repo.list_branches(Some("agents/")).unwrap();
        assert_eq!(agent_branches.len(), 2);

        let all_branches = repo.list_branches(None).unwrap();
        assert_eq!(all_branches.len(), 4); // main + 3 new
    }

    #[test]
    fn test_commit_log() {
        let repo = test_repo();

        repo.set("main", "/a", &Object::int(1), quick_opts("first"))
            .unwrap();
        repo.set("main", "/b", &Object::int(2), quick_opts("second"))
            .unwrap();
        repo.set("main", "/c", &Object::int(3), quick_opts("third"))
            .unwrap();

        let log = repo.log("main", 10).unwrap();
        assert_eq!(log.len(), 4); // 3 + init commit

        // Most recent first
        assert_eq!(log[0].intent.description, "third");
        assert_eq!(log[1].intent.description, "second");
        assert_eq!(log[2].intent.description, "first");
    }

    #[test]
    fn test_intent_metadata_preserved() {
        let repo = test_repo();

        let opts = CommitOptions::new(
            "agent/planner-v2",
            IntentCategory::Explore,
            "try NFS storage",
        )
        .with_reasoning("NFS is simpler than Ceph for 2-node clusters")
        .with_confidence(0.8)
        .with_tags(vec!["storage".to_string(), "nfs".to_string()]);

        repo.set("main", "/storage/type", &Object::string("nfs"), opts)
            .unwrap();

        let log = repo.log("main", 1).unwrap();
        let commit = &log[0];

        assert_eq!(commit.agent_id, "agent/planner-v2");
        assert_eq!(commit.intent.category, IntentCategory::Explore);
        assert_eq!(commit.intent.description, "try NFS storage");
        assert_eq!(commit.intent.tags, vec!["storage", "nfs"]);
        assert_eq!(
            commit.reasoning,
            Some("NFS is simpler than Ceph for 2-node clusters".to_string())
        );
        assert_eq!(commit.confidence, Some(0.8));
    }

    #[test]
    fn test_nested_set_creates_intermediate_maps() {
        let repo = test_repo();

        repo.set(
            "main",
            "/config/network/dns/primary",
            &Object::string("8.8.8.8"),
            quick_opts("set DNS"),
        )
        .unwrap();

        let dns = repo.get("main", "/config/network/dns/primary").unwrap();
        assert_eq!(dns, Object::string("8.8.8.8"));
    }

    #[test]
    fn test_immutability_across_branches() {
        let repo = test_repo();

        repo.set_json(
            "main",
            "/cluster",
            &serde_json::json!({ "name": "prod", "nodes": 5 }),
            quick_opts("init cluster"),
        )
        .unwrap();

        // Branch and modify
        repo.branch("staging", "main").unwrap();
        repo.set_json(
            "staging",
            "/cluster/name",
            &serde_json::json!("staging"),
            quick_opts("rename to staging"),
        )
        .unwrap();

        // main is untouched
        let main_name = repo.get_json("main", "/cluster/name").unwrap();
        assert_eq!(main_name, serde_json::json!("prod"));

        // staging has the change
        let staging_name = repo.get_json("staging", "/cluster/name").unwrap();
        assert_eq!(staging_name, serde_json::json!("staging"));
    }

    // -----------------------------------------------------------------------
    // Diff tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_diff_identical_branches() {
        let repo = test_repo();
        repo.set("main", "/x", &Object::int(1), quick_opts("set"))
            .unwrap();
        repo.branch("copy", "main").unwrap();

        let ops = repo.diff("main", "copy").unwrap();
        assert!(ops.is_empty(), "identical branches should produce no diff");
    }

    #[test]
    fn test_diff_value_change() {
        let repo = test_repo();
        repo.set(
            "main",
            "/status",
            &Object::string("healthy"),
            quick_opts("init"),
        )
        .unwrap();
        repo.branch("feature", "main").unwrap();
        repo.set(
            "feature",
            "/status",
            &Object::string("unhealthy"),
            quick_opts("break"),
        )
        .unwrap();

        let ops = repo.diff("main", "feature").unwrap();
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            agentstategraph_core::DiffOp::SetValue { path, .. } => {
                assert_eq!(path, "/status");
            }
            _ => panic!("expected SetValue"),
        }
    }

    #[test]
    fn test_diff_multiple_changes() {
        let repo = test_repo();
        repo.set_json(
            "main",
            "/cluster",
            &serde_json::json!({"name": "prod", "nodes": 3, "region": "us-east"}),
            quick_opts("init cluster"),
        )
        .unwrap();

        repo.branch("feature", "main").unwrap();

        // Change name, remove region, add version
        repo.set(
            "feature",
            "/cluster/name",
            &Object::string("staging"),
            quick_opts("rename"),
        )
        .unwrap();
        repo.delete("feature", "/cluster/region", quick_opts("remove region"))
            .unwrap();
        repo.set(
            "feature",
            "/cluster/version",
            &Object::string("v2"),
            quick_opts("add version"),
        )
        .unwrap();

        let ops = repo.diff("main", "feature").unwrap();
        assert!(
            ops.len() >= 3,
            "expected at least 3 diff ops, got {}",
            ops.len()
        );

        // Verify it's JSON-serializable (MCP-ready)
        let json = serde_json::to_string_pretty(&ops).unwrap();
        assert!(json.contains("SetValue") || json.contains("AddKey") || json.contains("RemoveKey"));
    }

    // -----------------------------------------------------------------------
    // Merge tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_merge_non_conflicting() {
        let repo = test_repo();

        // Set initial state with two keys
        repo.set("main", "/a", &Object::int(1), quick_opts("init a"))
            .unwrap();
        repo.set("main", "/b", &Object::int(2), quick_opts("init b"))
            .unwrap();

        // Branch
        repo.branch("feature", "main").unwrap();

        // Change different keys on each branch
        repo.set(
            "main",
            "/a",
            &Object::int(10),
            quick_opts("update a on main"),
        )
        .unwrap();
        repo.set(
            "feature",
            "/b",
            &Object::int(20),
            quick_opts("update b on feature"),
        )
        .unwrap();

        // Merge feature into main
        let merge_opts = CommitOptions::new(
            "agent/test",
            IntentCategory::Merge,
            "merge feature into main",
        );
        repo.merge("feature", "main", merge_opts).unwrap();

        // Both changes should be present
        assert_eq!(repo.get("main", "/a").unwrap(), Object::int(10)); // ours
        assert_eq!(repo.get("main", "/b").unwrap(), Object::int(20)); // theirs

        // The merged state tree must be fully readable — no dangling nodes.
        let head = repo.resolve_ref("main").unwrap();
        let root = repo.storage.get_commit(&head).unwrap().unwrap().state_root;
        assert_eq!(repo.first_missing_reachable(&root).unwrap(), None);
    }

    #[test]
    fn test_first_missing_reachable_detects_dangling_child() {
        let repo = test_repo();
        // A map whose "x" points at an object we never store — the exact shape
        // of the `/plans` corruption (parent present, interior child absent).
        let missing = Object::int(42).id();
        let mut entries = std::collections::BTreeMap::new();
        entries.insert("x".to_string(), missing);
        let root = Object::map(entries);
        let root_id = repo.storage.put_object(&root).unwrap();

        assert_eq!(
            repo.first_missing_reachable(&root_id).unwrap(),
            Some(missing),
            "a referenced-but-absent child must be reported"
        );

        // Once the child exists, the tree is clean.
        repo.storage.put_object(&Object::int(42)).unwrap();
        assert_eq!(repo.first_missing_reachable(&root_id).unwrap(), None);
    }

    #[test]
    fn test_first_missing_reachable_clean_tree() {
        let repo = test_repo();
        let head = repo.resolve_ref("main").unwrap();
        let root = repo.storage.get_commit(&head).unwrap().unwrap().state_root;
        // A freshly-initialised repo's root tree is fully present.
        assert_eq!(repo.first_missing_reachable(&root).unwrap(), None);
    }

    #[test]
    fn test_merge_with_conflict() {
        let repo = test_repo();

        repo.set("main", "/x", &Object::int(1), quick_opts("init"))
            .unwrap();
        repo.branch("feature", "main").unwrap();

        // Both change the same key to different values
        repo.set("main", "/x", &Object::int(2), quick_opts("main change"))
            .unwrap();
        repo.set(
            "feature",
            "/x",
            &Object::int(3),
            quick_opts("feature change"),
        )
        .unwrap();

        let merge_opts = CommitOptions::new("agent/test", IntentCategory::Merge, "merge");
        let result = repo.merge("feature", "main", merge_opts);

        match result {
            Err(RepoError::MergeConflicts(conflicts)) => {
                assert!(!conflicts.is_empty(), "should have conflicts");
            }
            Ok(_) => panic!("expected merge conflicts"),
            Err(e) => panic!("unexpected error: {}", e),
        }
    }

    #[test]
    fn test_merge_fast_forward() {
        let repo = test_repo();
        repo.set("main", "/x", &Object::int(1), quick_opts("init"))
            .unwrap();
        repo.branch("feature", "main").unwrap();

        // Only feature changes, main stays the same
        repo.set(
            "feature",
            "/x",
            &Object::int(2),
            quick_opts("feature change"),
        )
        .unwrap();

        let merge_opts = CommitOptions::new("agent/test", IntentCategory::Merge, "ff merge");
        repo.merge("feature", "main", merge_opts).unwrap();

        assert_eq!(repo.get("main", "/x").unwrap(), Object::int(2));
    }

    #[test]
    fn test_merge_creates_merge_commit() {
        let repo = test_repo();
        repo.set("main", "/a", &Object::int(1), quick_opts("init"))
            .unwrap();
        repo.branch("feature", "main").unwrap();

        repo.set("main", "/a", &Object::int(10), quick_opts("main"))
            .unwrap();
        repo.set("feature", "/b", &Object::int(20), quick_opts("feature"))
            .unwrap();

        let merge_opts = CommitOptions::new("agent/test", IntentCategory::Merge, "merge feature");
        let _merge_commit_id = repo.merge("feature", "main", merge_opts).unwrap();

        let log = repo.log("main", 1).unwrap();
        let merge_commit = &log[0];
        assert_eq!(merge_commit.intent.category, IntentCategory::Merge);
        assert_eq!(
            merge_commit.parents.len(),
            2,
            "merge commit should have 2 parents"
        );
    }

    #[test]
    fn test_merge_preserves_intent_metadata() {
        let repo = test_repo();
        repo.set("main", "/a", &Object::int(1), quick_opts("init"))
            .unwrap();
        repo.branch("feature", "main").unwrap();
        repo.set("main", "/c", &Object::int(99), quick_opts("main work"))
            .unwrap();
        repo.set("feature", "/b", &Object::int(2), quick_opts("feature work"))
            .unwrap();

        let merge_opts = CommitOptions::new(
            "agent/planner",
            IntentCategory::Merge,
            "integrate feature work",
        )
        .with_reasoning("Feature branch had the storage config we need")
        .with_confidence(0.9)
        .with_tags(vec!["storage".to_string(), "merge".to_string()]);

        repo.merge("feature", "main", merge_opts).unwrap();

        let log = repo.log("main", 1).unwrap();
        let commit = &log[0];
        assert_eq!(commit.agent_id, "agent/planner");
        assert_eq!(
            commit.reasoning,
            Some("Feature branch had the storage config we need".to_string())
        );
        assert_eq!(commit.confidence, Some(0.9));
        assert_eq!(commit.intent.tags, vec!["storage", "merge"]);
    }

    #[test]
    fn test_diff_is_json_serializable() {
        let repo = test_repo();
        repo.set("main", "/a", &Object::int(1), quick_opts("init"))
            .unwrap();
        repo.branch("b", "main").unwrap();
        repo.set("b", "/a", &Object::int(2), quick_opts("change"))
            .unwrap();

        let ops = repo.diff("main", "b").unwrap();
        let json = serde_json::to_value(&ops).unwrap();
        assert!(json.is_array());
        assert_eq!(json.as_array().unwrap().len(), 1);
    }

    // -----------------------------------------------------------------------
    // Speculation tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_speculate_and_read() {
        let repo = test_repo();
        repo.set("main", "/value", &Object::int(42), quick_opts("init"))
            .unwrap();

        let h = repo.speculate("main", Some("test".to_string())).unwrap();
        let obj = repo.spec_get(h, "/value").unwrap();
        assert_eq!(obj, Object::int(42));
    }

    #[test]
    fn test_speculate_modify_isolation() {
        let repo = test_repo();
        repo.set("main", "/x", &Object::int(1), quick_opts("init"))
            .unwrap();

        let h = repo.speculate("main", None).unwrap();
        repo.spec_set(h, "/x", &Object::int(99)).unwrap();

        // Speculation has new value
        assert_eq!(repo.spec_get(h, "/x").unwrap(), Object::int(99));

        // Main unchanged
        assert_eq!(repo.get("main", "/x").unwrap(), Object::int(1));
    }

    #[test]
    fn test_compare_two_speculations() {
        let repo = test_repo();
        repo.set_json(
            "main",
            "/storage",
            &serde_json::json!({"type": "none"}),
            quick_opts("init"),
        )
        .unwrap();

        let nfs = repo.speculate("main", Some("NFS".to_string())).unwrap();
        let ceph = repo.speculate("main", Some("Ceph".to_string())).unwrap();

        repo.spec_set(nfs, "/storage/type", &Object::string("nfs"))
            .unwrap();
        repo.spec_set(ceph, "/storage/type", &Object::string("ceph"))
            .unwrap();

        let comparison = repo.compare_speculations(&[nfs, ceph]).unwrap();
        assert_eq!(comparison.entries.len(), 2);
        assert!(!comparison.entries[0].diff_from_base.is_empty());
        assert!(!comparison.entries[1].diff_from_base.is_empty());
    }

    #[test]
    fn test_commit_speculation() {
        let repo = test_repo();
        repo.set("main", "/x", &Object::int(1), quick_opts("init"))
            .unwrap();

        let h = repo.speculate("main", Some("winner".to_string())).unwrap();
        repo.spec_set(h, "/x", &Object::int(42)).unwrap();

        // Commit the speculation
        let opts = CommitOptions::new(
            "agent/planner",
            IntentCategory::Refine,
            "picked best approach",
        )
        .with_reasoning("Option A was better because...");
        repo.commit_speculation(h, opts).unwrap();

        // Main now has the speculated value
        assert_eq!(repo.get("main", "/x").unwrap(), Object::int(42));

        // Verify commit metadata
        let log = repo.log("main", 1).unwrap();
        assert_eq!(log[0].intent.description, "picked best approach");
        assert_eq!(log[0].agent_id, "agent/planner");
    }

    #[test]
    fn test_discard_speculation() {
        let repo = test_repo();
        repo.set("main", "/x", &Object::int(1), quick_opts("init"))
            .unwrap();

        let h = repo.speculate("main", None).unwrap();
        repo.spec_set(h, "/x", &Object::int(999)).unwrap();
        repo.discard_speculation(h).unwrap();

        // Main unchanged
        assert_eq!(repo.get("main", "/x").unwrap(), Object::int(1));

        // Handle is invalid now
        assert!(repo.spec_get(h, "/x").is_err());
    }

    #[test]
    fn test_full_agent_speculation_workflow() {
        // The complete "explore, compare, pick winner" pattern
        let repo = test_repo();
        repo.set_json(
            "main",
            "/cluster",
            &serde_json::json!({
                "name": "prod",
                "storage": {"type": "none"},
                "network": {"subnet": "10.0.0.0/24"}
            }),
            quick_opts("initial cluster state"),
        )
        .unwrap();

        // Agent creates three speculations
        let nfs = repo
            .speculate("main", Some("NFS approach".to_string()))
            .unwrap();
        let ceph = repo
            .speculate("main", Some("Ceph approach".to_string()))
            .unwrap();
        let local = repo
            .speculate("main", Some("Local SSD".to_string()))
            .unwrap();

        // Each speculation explores a different approach
        repo.spec_set(nfs, "/cluster/storage/type", &Object::string("nfs"))
            .unwrap();
        repo.spec_set(nfs, "/cluster/storage/mount", &Object::string("/shared"))
            .unwrap();

        repo.spec_set(ceph, "/cluster/storage/type", &Object::string("ceph"))
            .unwrap();
        repo.spec_set(ceph, "/cluster/storage/replicas", &Object::int(3))
            .unwrap();

        repo.spec_set(local, "/cluster/storage/type", &Object::string("local-ssd"))
            .unwrap();
        repo.spec_set(
            local,
            "/cluster/storage/path",
            &Object::string("/dev/nvme0"),
        )
        .unwrap();

        // Compare all three
        let comparison = repo.compare_speculations(&[nfs, ceph, local]).unwrap();
        assert_eq!(comparison.entries.len(), 3);

        // Agent picks NFS (Ceph needs too many nodes, local isn't shared)
        let opts = CommitOptions::new(
            "agent/storage-planner",
            IntentCategory::Refine,
            "Selected NFS — Ceph requires 3+ nodes, local SSD not shared",
        )
        .with_reasoning("NFS provides shared storage with minimal node requirements")
        .with_confidence(0.85)
        .with_tags(vec!["storage".to_string(), "nfs".to_string()]);

        repo.commit_speculation(nfs, opts).unwrap();

        // Discard losers
        repo.discard_speculation(ceph).unwrap();
        repo.discard_speculation(local).unwrap();

        // Verify final state
        let storage_type = repo.get("main", "/cluster/storage/type").unwrap();
        assert_eq!(storage_type, Object::string("nfs"));

        let mount = repo.get("main", "/cluster/storage/mount").unwrap();
        assert_eq!(mount, Object::string("/shared"));

        // Verify full commit trail
        let log = repo.log("main", 2).unwrap();
        assert_eq!(log[0].intent.category, IntentCategory::Refine);
        assert_eq!(log[0].confidence, Some(0.85));
        assert_eq!(log[0].intent.tags, vec!["storage", "nfs"]);

        // No speculations left
        assert!(repo.list_speculations().is_empty());
    }

    // ---- v3+ timestamp anomaly detection (V4) --------------------------

    #[test]
    fn test_detect_timestamp_anomalies_flat_history() {
        // A normal history: every commit is strictly after its parent.
        let repo = Repository::new(Box::new(
            SqliteStorage::in_memory().expect("in-memory sqlite"),
        ));
        repo.init().unwrap();
        repo.set("main", "/a", &Object::string("1"), quick_opts("first"))
            .unwrap();
        repo.set("main", "/a", &Object::string("2"), quick_opts("second"))
            .unwrap();
        let anomalies = repo.detect_timestamp_anomalies("main").unwrap();
        assert!(
            anomalies.is_empty(),
            "expected no anomalies on monotonic history; got {anomalies:?}"
        );
    }

    #[test]
    fn test_blame_entry_timestamp_anomaly_is_false_by_default() {
        let repo = Repository::new(Box::new(
            SqliteStorage::in_memory().expect("in-memory sqlite"),
        ));
        repo.init().unwrap();
        repo.set("main", "/a", &Object::string("v"), quick_opts("write"))
            .unwrap();
        let entry = repo.blame("main", "/a").unwrap();
        assert!(!entry.timestamp_anomaly);
    }

    // ---- v3+ epoch seal enforcement (V8) -------------------------------

    #[test]
    fn test_seal_epoch_captures_reachable_commits() {
        let repo = Repository::new(Box::new(
            SqliteStorage::in_memory().expect("in-memory sqlite"),
        ));
        repo.init().unwrap();
        repo.set("main", "/a", &Object::string("1"), quick_opts("a"))
            .unwrap();
        repo.set("main", "/b", &Object::string("2"), quick_opts("b"))
            .unwrap();

        repo.create_epoch("e1", "first epoch", vec![]).unwrap();
        repo.seal_epoch("e1", "done").unwrap();

        let epoch = repo.get_epoch("e1").unwrap();
        assert!(
            !epoch.sealed_commits.is_empty(),
            "seal_epoch must populate sealed_commits"
        );
    }

    #[test]
    fn test_epoch_seal_violation_in_warn_mode_is_logged_not_rejected() {
        // Warn mode is now opt-out (strict is the default) — a rewind past a
        // sealed commit logs but still succeeds.
        let repo = Repository::new(Box::new(
            SqliteStorage::in_memory().expect("in-memory sqlite"),
        ))
        .with_epoch_seal_strict(false);
        repo.init().unwrap();
        let _ = repo
            .set("main", "/a", &Object::string("1"), quick_opts("a"))
            .unwrap();
        let pre_seal = repo
            .set("main", "/b", &Object::string("2"), quick_opts("b"))
            .unwrap();
        let _post_seal = repo
            .set("main", "/c", &Object::string("3"), quick_opts("c"))
            .unwrap();

        repo.create_epoch("e1", "scoped work", vec![]).unwrap();
        repo.seal_epoch("e1", "ship").unwrap();

        // Rewind main to pre_seal via set_ref. In warn mode this succeeds.
        let res = repo.set_ref("main", pre_seal);
        assert!(res.is_ok(), "warn mode must accept the rewind; got {res:?}");
    }

    #[test]
    fn test_epoch_seal_strict_is_the_default() {
        // The hard guard must fire through a PLAIN `Repository::new` — no
        // `.with_epoch_seal_strict(true)`, no env var. This is the production
        // constructor every shipped surface (MCP/HTTP/FFI/CLI) goes through, so
        // this asserts the guard Plan B relies on is engaged by default.
        assert!(
            std::env::var(EPOCH_SEAL_STRICT_ENV).is_err(),
            "test assumes {EPOCH_SEAL_STRICT_ENV} is unset in the test environment"
        );
        let repo = Repository::new(Box::new(
            SqliteStorage::in_memory().expect("in-memory sqlite"),
        ));
        repo.init().unwrap();
        repo.set("main", "/a", &Object::string("1"), quick_opts("a"))
            .unwrap();
        let pre_seal = repo
            .set("main", "/b", &Object::string("2"), quick_opts("b"))
            .unwrap();
        repo.set("main", "/c", &Object::string("3"), quick_opts("c"))
            .unwrap();

        repo.create_epoch("e1", "scoped work", vec![]).unwrap();
        repo.seal_epoch("e1", "ship").unwrap();

        match repo.set_ref("main", pre_seal) {
            Err(RepoError::EpochSealViolated { .. }) => {}
            other => panic!("expected EpochSealViolated by default, got {other:?}"),
        }
    }

    #[test]
    fn test_epoch_seal_violation_in_strict_mode_is_rejected() {
        let repo = Repository::new(Box::new(
            SqliteStorage::in_memory().expect("in-memory sqlite"),
        ))
        .with_epoch_seal_strict(true);
        repo.init().unwrap();
        let _ = repo
            .set("main", "/a", &Object::string("1"), quick_opts("a"))
            .unwrap();
        let pre_seal = repo
            .set("main", "/b", &Object::string("2"), quick_opts("b"))
            .unwrap();
        repo.set("main", "/c", &Object::string("3"), quick_opts("c"))
            .unwrap();

        repo.create_epoch("e1", "scoped work", vec![]).unwrap();
        repo.seal_epoch("e1", "ship").unwrap();

        // Now the attempt to rewind past a sealed commit must fail.
        let res = repo.set_ref("main", pre_seal);
        match res {
            Err(RepoError::EpochSealViolated { .. }) => {}
            other => panic!("expected EpochSealViolated, got {other:?}"),
        }
    }

    #[test]
    fn test_get_epoch_rehydrates_commits() {
        let repo = Repository::new(Box::new(
            SqliteStorage::in_memory().expect("in-memory sqlite"),
        ));
        repo.init().unwrap();
        repo.create_epoch("e1", "first epoch", vec![]).unwrap();
        repo.set_active_epoch(Some("e1".to_string())).unwrap();

        repo.set("main", "/a", &Object::string("1"), quick_opts("a"))
            .unwrap();
        repo.set("main", "/b", &Object::string("2"), quick_opts("b"))
            .unwrap();

        repo.set_active_epoch(None).unwrap();
        repo.seal_epoch("e1", "done").unwrap();

        let epoch = repo.get_epoch("e1").unwrap();
        assert_eq!(epoch.commits.len(), 2, "get_epoch must rehydrate 2 commits");
    }

    #[test]
    fn test_archive_epoch() {
        let repo = Repository::new(Box::new(
            SqliteStorage::in_memory().expect("in-memory sqlite"),
        ));
        repo.init().unwrap();
        repo.create_epoch("e1", "epoch", vec![]).unwrap();
        repo.seal_epoch("e1", "done").unwrap();

        repo.archive_epoch("e1").unwrap();

        let epoch = repo.get_epoch("e1").unwrap();
        assert_eq!(epoch.status, agentstategraph_core::EpochStatus::Archived);
    }

    #[test]
    fn test_archive_epoch_requires_sealed() {
        let repo = Repository::new(Box::new(
            SqliteStorage::in_memory().expect("in-memory sqlite"),
        ));
        repo.init().unwrap();
        repo.create_epoch("e1", "epoch", vec![]).unwrap();

        // Cannot archive an active epoch.
        assert!(repo.archive_epoch("e1").is_err());
    }

    #[test]
    fn test_export_epoch() {
        let repo = Repository::new(Box::new(
            SqliteStorage::in_memory().expect("in-memory sqlite"),
        ));
        repo.init().unwrap();
        repo.create_epoch("e1", "export test", vec![]).unwrap();
        repo.set_active_epoch(Some("e1".to_string())).unwrap();

        repo.set("main", "/x", &Object::string("hello"), quick_opts("x"))
            .unwrap();

        repo.set_active_epoch(None).unwrap();
        repo.seal_epoch("e1", "done").unwrap();

        let bundle = repo.export_epoch("e1").unwrap();
        assert_eq!(bundle["agentstategraph_export_version"], 1);
        assert_eq!(bundle["epoch"]["id"], "e1");
        assert!(bundle["commits"].is_array());
        assert_eq!(bundle["commits"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn test_export_epoch_rejects_active() {
        let repo = Repository::new(Box::new(
            SqliteStorage::in_memory().expect("in-memory sqlite"),
        ));
        repo.init().unwrap();
        repo.create_epoch("e1", "active", vec![]).unwrap();

        assert!(matches!(
            repo.export_epoch("e1"),
            Err(RepoError::InvalidOperation(_))
        ));
    }

    #[test]
    fn test_intent_tree_threads_by_parent_intent() {
        let repo = test_repo();

        // A parent intent, then a child intent decomposed from it via
        // `Intent::with_parent` — the RFC's sub-intent threading.
        let parent_opts = CommitOptions::new("agent/test", IntentCategory::Explore, "parent goal");
        let parent_intent_id = parent_opts.intent.id.clone();
        repo.set("main", "/a", &Object::string("1"), parent_opts)
            .unwrap();

        let mut child_opts = CommitOptions::new("agent/test", IntentCategory::Refine, "child goal");
        child_opts.intent = child_opts.intent.with_parent(parent_intent_id.clone());
        repo.set("main", "/b", &Object::string("2"), child_opts)
            .unwrap();

        let tree = repo.intent_tree("main", None).unwrap();
        let roots = tree["roots"].as_array().unwrap();

        // The child threads UNDER the parent — it must not appear as a root.
        assert!(
            roots.iter().all(|r| r["description"] != "child goal"),
            "child intent must not be a root; it is decomposed from a parent"
        );

        // The parent root carries the child among its children, matched by
        // intent id (NOT by commit lineage).
        let parent_node = roots
            .iter()
            .find(|r| r["description"] == "parent goal")
            .expect("parent intent should be a root");
        let children = parent_node["children"].as_array().unwrap();
        assert_eq!(
            children.len(),
            1,
            "parent should have exactly one sub-intent"
        );
        assert_eq!(children[0]["description"], "child goal");
        assert_eq!(parent_node["intent_id"], parent_intent_id.as_str());
    }

    #[test]
    fn test_intent_tree_flat_without_threading() {
        // With no parent_intent set (today's reality), every intent is its own
        // root — an honest flat list, not a fabricated hierarchy.
        let repo = test_repo();
        repo.set("main", "/a", &Object::string("1"), quick_opts("first"))
            .unwrap();
        repo.set("main", "/b", &Object::string("2"), quick_opts("second"))
            .unwrap();

        let tree = repo.intent_tree("main", None).unwrap();
        let roots = tree["roots"].as_array().unwrap();
        assert!(
            roots
                .iter()
                .all(|r| r["children"].as_array().unwrap().is_empty()),
            "no threading recorded → no node should have children"
        );
        // Both of our writes (plus the init commit) are top-level roots.
        assert!(roots.iter().any(|r| r["description"] == "first"));
        assert!(roots.iter().any(|r| r["description"] == "second"));
    }

    #[test]
    fn test_commit_persists_tool_calls() {
        let repo = test_repo();

        let tc = ToolCall {
            tool_name: "kubectl_apply".to_string(),
            arguments: serde_json::json!({ "file": "deploy.yaml" }),
            result: Some("configured".to_string()),
            timestamp: chrono::Utc::now(),
        };
        let opts = CommitOptions::new("agent/test", IntentCategory::Fix, "apply manifest")
            .with_tool_calls(vec![tc.clone()]);
        let commit_id = repo
            .set("main", "/deploy", &Object::string("ok"), opts)
            .unwrap();

        // Round-trips out of storage on the persisted commit.
        let commit = repo.get_commit(&commit_id).unwrap().unwrap();
        assert_eq!(commit.tool_calls.len(), 1);
        assert_eq!(commit.tool_calls[0].tool_name, "kubectl_apply");
        assert_eq!(commit.tool_calls[0].result.as_deref(), Some("configured"));
    }

    #[test]
    fn test_commit_without_tool_calls_is_empty() {
        let repo = test_repo();
        let commit_id = repo
            .set("main", "/a", &Object::string("1"), quick_opts("no tools"))
            .unwrap();
        let commit = repo.get_commit(&commit_id).unwrap().unwrap();
        assert!(commit.tool_calls.is_empty());
    }

    #[test]
    fn test_commit_authority_principal_defaults_to_agent() {
        // A commit records the acting agent as its authorizing principal — not
        // the old constant "default" (Plan C t-002).
        let repo = test_repo();
        let opts = CommitOptions::new("human/alice", IntentCategory::Fix, "patch");
        let commit_id = repo.set("main", "/a", &Object::string("1"), opts).unwrap();

        let commit = repo.get_commit(&commit_id).unwrap().unwrap();
        assert_eq!(commit.agent_id, "human/alice");
        assert_eq!(
            commit.authority.principal, "human/alice",
            "principal should default to the acting agent, not a constant"
        );
        assert_ne!(commit.authority.principal, "default");
    }

    #[test]
    fn test_commit_with_principal_overrides_actor() {
        // When the authorizer differs from the actor, `with_principal` wins for
        // the authority while `agent_id` still records who acted.
        let repo = test_repo();
        let opts = CommitOptions::new("agent/bot", IntentCategory::Refine, "act")
            .with_principal("human/alice");
        let commit_id = repo.set("main", "/a", &Object::string("1"), opts).unwrap();

        let commit = repo.get_commit(&commit_id).unwrap().unwrap();
        assert_eq!(commit.agent_id, "agent/bot");
        assert_eq!(commit.authority.principal, "human/alice");
    }

    fn refine_count(rows: &[HistoryRollupRow], agent: &str) -> Option<i64> {
        rows.iter()
            .find(|r| r.agent_id == agent && r.intent_category == "Refine")
            .map(|r| r.commit_count)
    }

    #[test]
    fn test_extract_history_rollup_and_milestones() {
        let repo = test_repo();
        let mk = |agent: &str, cat: IntentCategory, path: &str, desc: &str| {
            let opts = CommitOptions::new(agent, cat, desc);
            repo.set("main", path, &Object::string("v"), opts).unwrap();
        };
        mk("alice", IntentCategory::Refine, "/a", "r1");
        mk("alice", IntentCategory::Refine, "/b", "r2");
        mk("bob", IntentCategory::Fix, "/c", "f1");
        mk("alice", IntentCategory::Checkpoint, "/d", "ship it");

        // batch_size 2 forces the extractor to loop across several batches.
        let report = repo.extract_history(2).unwrap();
        assert!(report.commits_processed >= 4, "processed all our commits");
        assert!(report.cursor > 0);

        let rollup = repo.history_rollup().unwrap();
        let bucket = |agent: &str, cat: &str| {
            rollup
                .iter()
                .find(|r| r.agent_id == agent && r.intent_category == cat)
                .map(|r| r.commit_count)
        };
        assert_eq!(bucket("alice", "Refine"), Some(2));
        assert_eq!(bucket("bob", "Fix"), Some(1));
        assert_eq!(bucket("alice", "Checkpoint"), Some(1));
        // No sessions → everything attributes to the "default" namespace.
        assert!(rollup.iter().all(|r| r.namespace == "default"));

        let miles = repo.history_milestones(100).unwrap();
        assert!(
            miles
                .iter()
                .any(|m| m.description == "ship it" && m.kind == "checkpoint"),
            "the Checkpoint commit is on the milestone timeline"
        );
        // Non-checkpoint commits are not milestones.
        assert!(miles.iter().all(|m| m.description != "r1"));
    }

    #[test]
    fn test_extract_history_incremental_and_idempotent() {
        let repo = test_repo();
        repo.set(
            "main",
            "/a",
            &Object::string("1"),
            CommitOptions::new("alice", IntentCategory::Refine, "r1"),
        )
        .unwrap();

        let r1 = repo.extract_history(100).unwrap();
        assert!(r1.commits_processed >= 1);
        let before = repo.history_rollup().unwrap();
        assert_eq!(refine_count(&before, "alice"), Some(1));

        // Re-running with no new commits is a no-op and doesn't double-count.
        let r2 = repo.extract_history(100).unwrap();
        assert_eq!(r2.commits_processed, 0);
        assert_eq!(r2.cursor, r1.cursor);
        assert_eq!(
            refine_count(&repo.history_rollup().unwrap(), "alice"),
            Some(1)
        );

        // A new commit is folded incrementally — only it is processed.
        repo.set(
            "main",
            "/b",
            &Object::string("2"),
            CommitOptions::new("alice", IntentCategory::Refine, "r2"),
        )
        .unwrap();
        let r3 = repo.extract_history(100).unwrap();
        assert_eq!(r3.commits_processed, 1);
        assert!(r3.cursor > r1.cursor);
        assert_eq!(
            refine_count(&repo.history_rollup().unwrap(), "alice"),
            Some(2)
        );
    }

    #[test]
    fn test_history_report_aggregates_views() {
        let repo = test_repo();
        let mk = |agent: &str, cat: IntentCategory, path: &str, desc: &str| {
            repo.set(
                "main",
                path,
                &Object::string("v"),
                CommitOptions::new(agent, cat, desc),
            )
            .unwrap();
        };
        mk("alice", IntentCategory::Refine, "/a", "r1");
        mk("alice", IntentCategory::Refine, "/b", "r2");
        mk("bob", IntentCategory::Fix, "/c", "f1");
        mk("alice", IntentCategory::Checkpoint, "/d", "ship it");

        // refresh=true runs the extractor first, so the report is current even
        // though we never called extract_history explicitly.
        let report = repo.history_report(None, "day", 50, true, false).unwrap();

        // Intent mix, looked up by name (init also writes a Checkpoint, so we
        // assert our own categories rather than a leaderboard position).
        let mix = report["intent_mix"].as_array().unwrap();
        let mix_of = |cat: &str| {
            mix.iter()
                .find(|e| e["category"] == cat)
                .and_then(|e| e["commits"].as_i64())
        };
        assert_eq!(mix_of("Refine"), Some(2));
        assert_eq!(mix_of("Fix"), Some(1));

        // Authorship: our three "alice" commits and one "bob".
        let auth = report["authorship"].as_array().unwrap();
        let auth_of = |a: &str| {
            auth.iter()
                .find(|e| e["agent"] == a)
                .and_then(|e| e["commits"].as_i64())
        };
        assert_eq!(auth_of("alice"), Some(3));
        assert_eq!(auth_of("bob"), Some(1));

        // Velocity is a single day here; all commits land in one bucket.
        assert_eq!(report["velocity"]["by"], "day");
        assert_eq!(report["velocity"]["series"].as_array().unwrap().len(), 1);

        // The Checkpoint shows on the milestone timeline.
        let miles = report["milestones"].as_array().unwrap();
        assert!(miles.iter().any(|m| m["description"] == "ship it"));

        // refresh=false is a pure read — no extraction; week rollup collapses
        // the single day into one ISO-week bucket.
        let again = repo.history_report(None, "week", 50, false, false).unwrap();
        assert_eq!(again["velocity"]["by"], "week");
        assert_eq!(again["velocity"]["series"].as_array().unwrap().len(), 1);
        let again_auth = again["authorship"].as_array().unwrap();
        assert!(
            again_auth
                .iter()
                .any(|e| e["agent"] == "alice" && e["commits"] == 3)
        );

        // store=false omits the block; store=true attaches it.
        assert!(report.get("store_shape").is_none());
        let with_store = repo.history_report(None, "day", 50, false, true).unwrap();
        assert!(with_store["store_shape"].is_object());
    }

    #[test]
    fn test_history_store_shape() {
        let repo = test_repo();
        for i in 0..5 {
            repo.set(
                "main",
                &format!("/k{i}"),
                &Object::string("v"),
                CommitOptions::new("alice", IntentCategory::Refine, "r"),
            )
            .unwrap();
        }

        let shape = repo.history_store_shape().unwrap();
        // Counts are positive and bytes reflect a non-empty DB.
        assert!(shape["commits"].as_i64().unwrap() >= 5);
        assert!(shape["objects"].as_i64().unwrap() > 0);
        assert!(shape["total_bytes"].as_i64().unwrap() > 0);
        // Path-copy amplification = objects / commits > 0.
        assert!(
            shape["path_copy_amplification"]["objects_per_commit"]
                .as_f64()
                .unwrap()
                > 0.0
        );
        // Per-table breakdown is present only when the build exposes dbstat;
        // either way `tables` is an array and the flag matches its emptiness
        // expectation.
        assert!(shape["tables"].is_array());
        if shape["dbstat_available"].as_bool().unwrap() {
            assert!(!shape["tables"].as_array().unwrap().is_empty());
        }
    }
}
