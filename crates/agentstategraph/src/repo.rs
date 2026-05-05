//! Repository — the high-level API for AgentStateGraph.
//!
//! A Repository wraps a Storage backend and provides the primary
//! user-facing operations: get, set, delete, branch, merge, log.
//!
//! Every write operation is an atomic commit with intent metadata.
//! There is no staging area.

use agentstategraph_core::{
    Authority, Commit, CommitBuilder, Conflict, DiffOp, Intent, IntentCategory, MergeResult,
    Object, ObjectId, ObjectResolver, StatePath,
};
use agentstategraph_storage::{Storage, StorageError};

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
    storage: Box<dyn Storage>,
    specs: SpeculationManager,
    watch_mgr: crate::watch::WatchManager,
    /// Active epoch id — if set, all new commits are associated with it
    /// via `storage.set_commit_epoch` on commit finalization. Set via
    /// `set_active_epoch` / cleared via `clear_active_epoch`. Not a
    /// public MCP tool yet — that's a follow-up milestone.
    active_epoch: std::sync::RwLock<Option<String>>,
    /// Active session id — same semantics as `active_epoch`.
    active_session: std::sync::RwLock<Option<String>>,
    /// When true, ref updates that orphan sealed commits are rejected with
    /// `RepoError::EpochSealViolated`. When false (default), violations log
    /// a warning and the update proceeds. Opt-in via `Repository::with_epoch_seal_strict`
    /// or `ASG_EPOCH_SEAL_STRICT=1`.
    epoch_seal_strict: bool,
}

/// Options for creating a commit.
pub struct CommitOptions {
    pub agent_id: String,
    pub authority: Authority,
    pub intent: Intent,
    pub reasoning: Option<String>,
    pub confidence: Option<f64>,
}

impl CommitOptions {
    /// Create minimal commit options — the simplest way to commit.
    pub fn new(
        agent_id: impl Into<String>,
        intent_category: IntentCategory,
        description: impl Into<String>,
    ) -> Self {
        Self {
            agent_id: agent_id.into(),
            authority: Authority::simple("default"),
            intent: Intent::new(intent_category, description),
            reasoning: None,
            confidence: None,
        }
    }

    /// Set the authority.
    pub fn with_authority(mut self, authority: Authority) -> Self {
        self.authority = authority;
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

    #[error("repository not initialized — call init() first")]
    NotInitialized,

    #[error(
        "path {0} is reserved for schema metadata; only IntentCategory::Migrate commits may write here"
    )]
    ReservedPath(String),

    #[error("merge conflicts: {0:?}")]
    MergeConflicts(Vec<Conflict>),

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

    #[error("write conflict: ref moved before CAS could land")]
    WriteConflict,

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
    /// The epoch-seal enforcement mode defaults to warn. If the environment
    /// variable `ASG_EPOCH_SEAL_STRICT=1` is set at construction time, the
    /// repository starts in strict mode. Use
    /// [`Repository::with_epoch_seal_strict`] to opt in programmatically.
    pub fn new(storage: Box<dyn Storage>) -> Self {
        let strict = std::env::var(EPOCH_SEAL_STRICT_ENV)
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        Self {
            storage,
            specs: SpeculationManager::new(),
            watch_mgr: crate::watch::WatchManager::new(),
            active_epoch: std::sync::RwLock::new(None),
            active_session: std::sync::RwLock::new(None),
            epoch_seal_strict: strict,
        }
    }

    /// Return a Repository with epoch-seal enforcement set to the given mode.
    /// Overrides the `ASG_EPOCH_SEAL_STRICT` environment variable.
    ///
    /// Strict mode rejects ref updates that would render any sealed-epoch
    /// commit unreachable from the new target. Warn mode (default) logs
    /// a warning and proceeds. (security threat model v3+, V8)
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
        if let Some(id) = self.storage.get_ref("main")? {
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
        self.storage.set_ref("main", commit.id)?;

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

        match self
            .storage
            .cas_ref(ref_name, expected_head, new_commit.id)?
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
        // Check if branch already exists
        if self.storage.get_ref(name)?.is_some() {
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
        Ok(self.storage.delete_ref(name)?)
    }

    /// List all branches, optionally filtered by prefix.
    pub fn list_branches(
        &self,
        prefix: Option<&str>,
    ) -> Result<Vec<(String, ObjectId)>, RepoError> {
        Ok(self.storage.list_refs(prefix.unwrap_or(""))?)
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

        // Find common ancestor — walk both parent chains
        let base_commit_id = self.find_common_ancestor(&source_commit_id, &target_commit_id)?;
        let base_commit = self
            .storage
            .get_commit(&base_commit_id)?
            .ok_or_else(|| RepoError::RefNotFound("base".to_string()))?;

        let resolver = StorageResolver {
            storage: self.storage.as_ref(),
        };

        let result = agentstategraph_core::merge::three_way_merge(
            &resolver,
            &base_commit.state_root,
            &target_commit.state_root,
            &source_commit.state_root,
        );

        match result {
            MergeResult::Success(merged_obj) => {
                let merged_root = self.storage.put_object(&merged_obj)?;
                // Store all sub-objects that the merge created
                self.store_object_tree(&merged_obj)?;
                let commit = self.create_commit(
                    merged_root,
                    vec![target_commit_id, source_commit_id],
                    options,
                )?;
                self.guarded_set_ref(target, commit.id)?;
                Ok(commit.id)
            }
            MergeResult::FastForward(ff_id) => {
                // Find the commit that has this state root
                // In fast-forward, we just advance the target ref
                let ff_commit = if ff_id == source_commit.state_root {
                    source_commit_id
                } else {
                    target_commit_id
                };
                self.guarded_set_ref(target, ff_commit)?;
                Ok(ff_commit)
            }
            MergeResult::Conflicts { conflicts, .. } => Err(RepoError::MergeConflicts(conflicts)),
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
    pub fn set_active_session(&self, id: Option<String>) -> Result<(), RepoError> {
        *self
            .active_session
            .write()
            .map_err(|e| RepoError::RefNotFound(e.to_string()))? = id;
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
        let sealed_commits = match self.storage.get_ref("main")? {
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

    /// Get intent decomposition tree starting from a root commit.
    /// Walks the parent_intent chain to build the hierarchy.
    pub fn intent_tree(
        &self,
        ref_name: &str,
        root_commit_id: Option<&str>,
    ) -> Result<serde_json::Value, RepoError> {
        let commits = self.log(ref_name, 10000)?;

        // Find root commits (those with no parent_intent, or the specified root)
        let roots: Vec<&Commit> = if let Some(root_id) = root_commit_id {
            commits.iter().filter(|c| c.id.short() == root_id).collect()
        } else {
            commits.iter().filter(|c| c.parents.is_empty()).collect()
        };

        fn build_intent_node(commit: &Commit, all_commits: &[Commit]) -> serde_json::Value {
            // Find children: commits whose first parent is this commit
            let children: Vec<serde_json::Value> = all_commits
                .iter()
                .filter(|c| c.parents.first() == Some(&commit.id))
                .map(|child| build_intent_node(child, all_commits))
                .collect();

            serde_json::json!({
                "id": commit.id.short(),
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
            .map(|c| build_intent_node(c, &commits))
            .collect();

        Ok(serde_json::json!({
            "roots": tree,
            "total_commits": commits.len(),
        }))
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Find the common ancestor of two commits by walking parent chains.
    /// Simple implementation: collect all ancestors of one, find first match in other.
    fn find_common_ancestor(&self, a: &ObjectId, b: &ObjectId) -> Result<ObjectId, RepoError> {
        // Collect all ancestors of 'a'
        let mut ancestors_a = std::collections::HashSet::new();
        let mut current = Some(*a);
        while let Some(id) = current {
            ancestors_a.insert(id);
            if let Some(commit) = self.storage.get_commit(&id)? {
                current = commit.parents.first().copied();
            } else {
                break;
            }
        }

        // Walk ancestors of 'b' and find the first match
        let mut current = Some(*b);
        while let Some(id) = current {
            if ancestors_a.contains(&id) {
                return Ok(id);
            }
            if let Some(commit) = self.storage.get_commit(&id)? {
                current = commit.parents.first().copied();
            } else {
                break;
            }
        }

        // If no common ancestor found, use the initial commit of 'a'
        // (walk to the root)
        let mut current = Some(*a);
        let mut last = *a;
        while let Some(id) = current {
            last = id;
            if let Some(commit) = self.storage.get_commit(&id)? {
                current = commit.parents.first().copied();
            } else {
                break;
            }
        }
        Ok(last)
    }

    /// Store all sub-objects of a merged Object tree.
    /// The merge engine creates new Object instances that may contain
    /// ObjectIds computed from their content but not yet in the store.
    fn store_object_tree(&self, obj: &Object) -> Result<(), RepoError> {
        self.storage.put_object(obj)?;
        if let Object::Node(node) = obj {
            let children = match node {
                agentstategraph_core::Node::Map(entries) => {
                    entries.values().copied().collect::<Vec<_>>()
                }
                agentstategraph_core::Node::List(items) => items.clone(),
                agentstategraph_core::Node::Set(items) => items.clone(),
            };
            for _child_id in children {
                // Children should already be in the store (from the original branches)
                // Only new merge-created objects need storing, and those are the root nodes
            }
        }
        Ok(())
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
    /// In warn mode (default) logs to stderr and proceeds. In strict mode
    /// returns `RepoError::EpochSealViolated`.
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
        self.enforce_epoch_seals(ref_name, &new_target)?;
        self.storage.set_ref(ref_name, new_target)?;
        Ok(())
    }

    /// Low-level: move a ref to a specific commit id, subject to epoch-seal
    /// enforcement. Used by migration tooling and by tests that need to
    /// simulate rewinds. Prefer `set`/`merge`/etc for normal writes.
    pub fn set_ref(&self, ref_name: &str, target: ObjectId) -> Result<(), RepoError> {
        self.guarded_set_ref(ref_name, target)
    }

    /// Resolve a ref name to a commit ID.
    fn resolve_ref(&self, ref_name: &str) -> Result<ObjectId, RepoError> {
        self.storage
            .get_ref(ref_name)?
            .ok_or_else(|| RepoError::BranchNotFound(ref_name.to_string()))
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
    use agentstategraph_storage::MemoryStorage;

    fn test_repo() -> Repository {
        let storage = MemoryStorage::new();
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
        let repo = Repository::new(Box::new(MemoryStorage::new()));
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
        let repo = Repository::new(Box::new(MemoryStorage::new()));
        repo.init().unwrap();
        repo.set("main", "/a", &Object::string("v"), quick_opts("write"))
            .unwrap();
        let entry = repo.blame("main", "/a").unwrap();
        assert!(!entry.timestamp_anomaly);
    }

    // ---- v3+ epoch seal enforcement (V8) -------------------------------

    #[test]
    fn test_seal_epoch_captures_reachable_commits() {
        let repo = Repository::new(Box::new(MemoryStorage::new()));
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
        // Warn mode is the default — a rewind past a sealed commit logs
        // but still succeeds.
        let repo = Repository::new(Box::new(MemoryStorage::new()));
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
    fn test_epoch_seal_violation_in_strict_mode_is_rejected() {
        let repo = Repository::new(Box::new(MemoryStorage::new())).with_epoch_seal_strict(true);
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
        let repo = Repository::new(Box::new(MemoryStorage::new()));
        repo.init().unwrap();
        repo.create_epoch("e1", "first epoch", vec![]).unwrap();
        repo.set_active_epoch(Some("e1".to_string())).unwrap();

        repo.set("main", "/a", &Object::string("1"), quick_opts("a")).unwrap();
        repo.set("main", "/b", &Object::string("2"), quick_opts("b")).unwrap();

        repo.set_active_epoch(None).unwrap();
        repo.seal_epoch("e1", "done").unwrap();

        let epoch = repo.get_epoch("e1").unwrap();
        assert_eq!(epoch.commits.len(), 2, "get_epoch must rehydrate 2 commits");
    }

    #[test]
    fn test_archive_epoch() {
        let repo = Repository::new(Box::new(MemoryStorage::new()));
        repo.init().unwrap();
        repo.create_epoch("e1", "epoch", vec![]).unwrap();
        repo.seal_epoch("e1", "done").unwrap();

        repo.archive_epoch("e1").unwrap();

        let epoch = repo.get_epoch("e1").unwrap();
        assert_eq!(epoch.status, agentstategraph_core::EpochStatus::Archived);
    }

    #[test]
    fn test_archive_epoch_requires_sealed() {
        let repo = Repository::new(Box::new(MemoryStorage::new()));
        repo.init().unwrap();
        repo.create_epoch("e1", "epoch", vec![]).unwrap();

        // Cannot archive an active epoch.
        assert!(repo.archive_epoch("e1").is_err());
    }

    #[test]
    fn test_export_epoch() {
        let repo = Repository::new(Box::new(MemoryStorage::new()));
        repo.init().unwrap();
        repo.create_epoch("e1", "export test", vec![]).unwrap();
        repo.set_active_epoch(Some("e1".to_string())).unwrap();

        repo.set("main", "/x", &Object::string("hello"), quick_opts("x")).unwrap();

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
        let repo = Repository::new(Box::new(MemoryStorage::new()));
        repo.init().unwrap();
        repo.create_epoch("e1", "active", vec![]).unwrap();

        assert!(matches!(
            repo.export_epoch("e1"),
            Err(RepoError::InvalidOperation(_))
        ));
    }
}
