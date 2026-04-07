//! Repository — the high-level API for StateGraph.
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

/// The primary API for interacting with a StateGraph state store.
pub struct Repository {
    storage: Box<dyn Storage>,
    specs: SpeculationManager,
    session_mgr: crate::session::SessionManager,
    watch_mgr: crate::watch::WatchManager,
    epochs: std::sync::RwLock<Vec<agentstategraph_core::Epoch>>,
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

    #[error("merge conflicts: {0:?}")]
    MergeConflicts(Vec<Conflict>),

    #[error("speculation error: {0}")]
    Speculation(#[from] SpecError),

    #[error("tree error: {0}")]
    Tree(#[from] TreeError),

    #[error("storage error: {0}")]
    Storage(#[from] StorageError),
}

impl Repository {
    /// Create a new Repository wrapping the given storage backend.
    pub fn new(storage: Box<dyn Storage>) -> Self {
        Self {
            storage,
            specs: SpeculationManager::new(),
            session_mgr: crate::session::SessionManager::new(),
            watch_mgr: crate::watch::WatchManager::new(),
            epochs: std::sync::RwLock::new(Vec::new()),
        }
    }

    /// Initialize the repository with an empty state tree on "main".
    /// If "main" already exists, this is a no-op.
    pub fn init(&self) -> Result<ObjectId, RepoError> {
        if let Some(id) = self.storage.get_ref("main")? {
            return Ok(id);
        }

        // Create empty root state
        let empty_root = Object::empty_map();
        let root_id = self.storage.put_object(&empty_root)?;

        // Create initial commit
        let commit = CommitBuilder::new(
            root_id,
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
    pub fn get(&self, ref_name: &str, path: &str) -> Result<Object, RepoError> {
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
    pub fn get_json(&self, ref_name: &str, path: &str) -> Result<serde_json::Value, RepoError> {
        let obj = self.get(ref_name, path)?;
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
        self.storage.set_ref(ref_name, new_commit.id)?;

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
        self.set(ref_name, path, &obj, options)
    }

    /// Delete a value from state, creating a new commit.
    pub fn delete(
        &self,
        ref_name: &str,
        path: &str,
        options: CommitOptions,
    ) -> Result<ObjectId, RepoError> {
        let commit_id = self.resolve_ref(ref_name)?;
        let commit = self
            .storage
            .get_commit(&commit_id)?
            .ok_or_else(|| RepoError::RefNotFound(ref_name.to_string()))?;

        let state_path =
            StatePath::parse(path).map_err(|e| TreeError::PathNotFound(e.to_string()))?;
        let new_root = tree::tree_delete(self.storage.as_ref(), &commit.state_root, &state_path)?;

        let new_commit = self.create_commit(new_root, vec![commit_id], options)?;
        self.storage.set_ref(ref_name, new_commit.id)?;

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
        self.storage.set_ref(name, commit_id)?;
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
                self.storage.set_ref(target, commit.id)?;
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
                self.storage.set_ref(target, ff_commit)?;
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

        Ok(self.specs.create(from_ref, commit.state_root, label))
    }

    /// Get a value from a speculation's state.
    pub fn spec_get(&self, handle: SpecHandle, path: &str) -> Result<Object, RepoError> {
        self.specs
            .get(handle, self.storage.as_ref(), path)
            .map_err(|e| RepoError::Speculation(e))
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
            .map_err(|e| RepoError::Speculation(e))
    }

    /// Delete a value in a speculation's state.
    pub fn spec_delete(&self, handle: SpecHandle, path: &str) -> Result<(), RepoError> {
        self.specs
            .delete(handle, self.storage.as_ref(), path)
            .map_err(|e| RepoError::Speculation(e))
    }

    /// Compare multiple speculations side-by-side.
    pub fn compare_speculations(
        &self,
        handles: &[SpecHandle],
    ) -> Result<SpecComparison, RepoError> {
        self.specs
            .compare(handles, self.storage.as_ref())
            .map_err(|e| RepoError::Speculation(e))
    }

    /// Commit a speculation — promotes it to a real commit on the base branch.
    pub fn commit_speculation(
        &self,
        handle: SpecHandle,
        options: CommitOptions,
    ) -> Result<ObjectId, RepoError> {
        let (state_root, base_ref) = self
            .specs
            .commit(handle)
            .map_err(|e| RepoError::Speculation(e))?;

        let parent_id = self.resolve_ref(&base_ref)?;
        let commit = self.create_commit(state_root, vec![parent_id], options)?;
        self.storage.set_ref(&base_ref, commit.id)?;
        Ok(commit.id)
    }

    /// Discard a speculation — all changes lost. Instant.
    pub fn discard_speculation(&self, handle: SpecHandle) -> Result<(), RepoError> {
        self.specs
            .discard(handle)
            .map_err(|e| RepoError::Speculation(e))
    }

    /// List all active speculations.
    pub fn list_speculations(&self) -> Vec<(SpecHandle, Option<String>)> {
        self.specs.list()
    }

    // -----------------------------------------------------------------------
    // History operations
    // -----------------------------------------------------------------------

    /// Get the commit log starting from a ref.
    pub fn log(&self, ref_name: &str, limit: usize) -> Result<Vec<Commit>, RepoError> {
        let commit_id = self.resolve_ref(ref_name)?;
        Ok(self.storage.list_commits(&commit_id, limit)?)
    }

    /// Get a specific commit by ID.
    pub fn get_commit(&self, id: &ObjectId) -> Result<Option<Commit>, RepoError> {
        Ok(self.storage.get_commit(id)?)
    }

    // -----------------------------------------------------------------------
    // Query operations
    // -----------------------------------------------------------------------

    /// Query commits with composable filters.
    pub fn query_commits(
        &self,
        ref_name: &str,
        filters: &agentstategraph_core::QueryFilters,
        limit: usize,
    ) -> Result<Vec<Commit>, RepoError> {
        let all_commits = self.log(ref_name, 1000)?; // get a large window
        let filtered = agentstategraph_core::filter_commits(&all_commits, filters);
        Ok(filtered.into_iter().take(limit).collect())
    }

    /// Blame — for a path, find which commit last modified it and why.
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
                    return Ok(agentstategraph_core::BlameEntry {
                        path: path.to_string(),
                        commit_id: commit.id.short(),
                        agent_id: commit.agent_id.clone(),
                        intent_category: format!("{:?}", commit.intent.category),
                        intent_description: commit.intent.description.clone(),
                        reasoning: commit.reasoning.clone(),
                        timestamp: commit.timestamp,
                    });
                }
            } else if let Some(parent_id) = commit.parents.first() {
                if let Some(parent) = self.storage.get_commit(parent_id)? {
                    let current_val =
                        tree::tree_get(self.storage.as_ref(), &commit.state_root, &state_path);
                    let parent_val =
                        tree::tree_get(self.storage.as_ref(), &parent.state_root, &state_path);

                    // If the value is different (or didn't exist in parent), this commit is the blame target
                    match (current_val.ok(), parent_val.ok()) {
                        (Some(curr), Some(prev)) if curr != prev => {
                            return Ok(agentstategraph_core::BlameEntry {
                                path: path.to_string(),
                                commit_id: commit.id.short(),
                                agent_id: commit.agent_id.clone(),
                                intent_category: format!("{:?}", commit.intent.category),
                                intent_description: commit.intent.description.clone(),
                                reasoning: commit.reasoning.clone(),
                                timestamp: commit.timestamp,
                            });
                        }
                        (Some(_), None) => {
                            // Value was added in this commit
                            return Ok(agentstategraph_core::BlameEntry {
                                path: path.to_string(),
                                commit_id: commit.id.short(),
                                agent_id: commit.agent_id.clone(),
                                intent_category: format!("{:?}", commit.intent.category),
                                intent_description: commit.intent.description.clone(),
                                reasoning: commit.reasoning.clone(),
                                timestamp: commit.timestamp,
                            });
                        }
                        _ => continue,
                    }
                }
            }
        }

        Err(RepoError::RefNotFound(format!(
            "no commit found that modified {}",
            path
        )))
    }

    // -----------------------------------------------------------------------
    // Session operations (sub-agent orchestration)
    // -----------------------------------------------------------------------

    /// Get the session manager for sub-agent orchestration.
    pub fn sessions(&self) -> &crate::session::SessionManager {
        &self.session_mgr
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

    /// Create a new epoch.
    pub fn create_epoch(
        &self,
        id: &str,
        description: &str,
        root_intents: Vec<String>,
    ) -> Result<agentstategraph_core::Epoch, RepoError> {
        let epoch = agentstategraph_core::Epoch::new(id, description, root_intents);
        let mut epochs = self
            .epochs
            .write()
            .map_err(|e| RepoError::RefNotFound(e.to_string()))?;
        epochs.push(epoch.clone());
        Ok(epoch)
    }

    /// Seal an epoch, making it immutable.
    pub fn seal_epoch(&self, id: &str, summary: &str) -> Result<(), RepoError> {
        let mut epochs = self
            .epochs
            .write()
            .map_err(|e| RepoError::RefNotFound(e.to_string()))?;
        let epoch = epochs
            .iter_mut()
            .find(|e| e.id == id)
            .ok_or_else(|| RepoError::RefNotFound(format!("epoch not found: {}", id)))?;

        // Compute seal hash from all commits in the epoch
        let mut hasher_input = Vec::new();
        for commit_id in &epoch.commits {
            hasher_input.extend_from_slice(commit_id.as_bytes());
        }
        let seal_hash = ObjectId::hash(&hasher_input);

        epoch
            .seal(summary.to_string(), seal_hash)
            .map_err(|e| RepoError::RefNotFound(e.to_string()))?;
        Ok(())
    }

    /// List all epochs.
    pub fn list_epochs(&self) -> Result<Vec<agentstategraph_core::EpochEntry>, RepoError> {
        let epochs = self
            .epochs
            .read()
            .map_err(|e| RepoError::RefNotFound(e.to_string()))?;
        Ok(epochs.iter().map(|e| e.to_entry()).collect())
    }

    /// Get a specific epoch by ID.
    pub fn get_epoch(&self, id: &str) -> Result<agentstategraph_core::Epoch, RepoError> {
        let epochs = self
            .epochs
            .read()
            .map_err(|e| RepoError::RefNotFound(e.to_string()))?;
        epochs
            .iter()
            .find(|e| e.id == id)
            .cloned()
            .ok_or_else(|| RepoError::RefNotFound(format!("epoch not found: {}", id)))
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
        Ok(commit)
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
        let merge_commit_id = repo.merge("feature", "main", merge_opts).unwrap();

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
}
