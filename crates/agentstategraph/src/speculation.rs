//! Speculative execution — first-class primitive for exploring alternatives.
//!
//! Agents don't execute linear scripts; they explore state spaces.
//! Speculations let an agent:
//!   1. Fork state cheaply (O(1) — just a pointer)
//!   2. Make changes in isolation
//!   3. Compare multiple speculations side-by-side
//!   4. Commit the winner or discard the losers
//!
//! Speculations are backed by the same object store (structural sharing),
//! so creating one is essentially free. Discarding one just drops the handle.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;

use agentstategraph_core::{DiffOp, Object, ObjectId, ObjectResolver};
use agentstategraph_storage::ObjectStore;

use crate::tree::{self, TreeError};

/// Opaque handle to a speculation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SpecHandle(u64);

impl SpecHandle {
    /// Create a SpecHandle from a raw ID (for MCP deserialization).
    pub fn from_id(id: u64) -> Self {
        Self(id)
    }

    /// Get the raw ID (for MCP serialization).
    pub fn id(&self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for SpecHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "spec-{}", self.0)
    }
}

static NEXT_SPEC_ID: AtomicU64 = AtomicU64::new(1);

/// A single speculation — an isolated, mutable fork of state.
struct Speculation {
    /// Human-readable label.
    label: Option<String>,
    /// The ref this speculation was created from.
    base_ref: String,
    /// The state root we started from (for comparison).
    base_root: ObjectId,
    /// Current state root (updated as changes are made).
    current_root: ObjectId,
    /// Local overlay: objects created within this speculation.
    /// These are written to the main store only on commit.
    overlay: HashMap<ObjectId, Object>,
}

/// Manages all active speculations for a repository.
pub struct SpeculationManager {
    specs: RwLock<HashMap<SpecHandle, Speculation>>,
}

/// A comparison between multiple speculations.
#[derive(Debug, Clone)]
pub struct SpecComparison {
    /// The base state all speculations forked from.
    pub base_ref: String,
    /// Per-speculation: (handle, label, diff from base).
    pub entries: Vec<SpecComparisonEntry>,
}

#[derive(Debug, Clone)]
pub struct SpecComparisonEntry {
    pub handle: SpecHandle,
    pub label: Option<String>,
    pub diff_from_base: Vec<DiffOp>,
}

/// Errors from speculation operations.
#[derive(Debug, thiserror::Error)]
pub enum SpecError {
    #[error("speculation not found: {0}")]
    NotFound(SpecHandle),

    #[error("tree error: {0}")]
    Tree(#[from] TreeError),

    #[error("storage error: {0}")]
    Storage(#[from] agentstategraph_storage::StorageError),
}

impl SpeculationManager {
    pub fn new() -> Self {
        Self {
            specs: RwLock::new(HashMap::new()),
        }
    }

    /// Create a new speculation forked from a state root.
    pub fn create(
        &self,
        base_ref: &str,
        base_root: ObjectId,
        label: Option<String>,
    ) -> SpecHandle {
        let handle = SpecHandle(NEXT_SPEC_ID.fetch_add(1, Ordering::Relaxed));
        let spec = Speculation {
            label,
            base_ref: base_ref.to_string(),
            base_root,
            current_root: base_root,
            overlay: HashMap::new(),
        };
        self.specs.write().unwrap().insert(handle, spec);
        handle
    }

    /// Get a value from a speculation's current state.
    pub fn get(
        &self,
        handle: SpecHandle,
        store: &dyn ObjectStore,
        path: &str,
    ) -> Result<Object, SpecError> {
        let specs = self.specs.read().unwrap();
        let spec = specs.get(&handle).ok_or(SpecError::NotFound(handle))?;
        let resolver = OverlayResolver {
            overlay: &spec.overlay,
            store,
        };
        let state_path = agentstategraph_core::StatePath::parse(path)
            .map_err(|e| TreeError::PathNotFound(e.to_string()))?;
        // Use tree_get but with overlay resolution
        let _root = resolver
            .resolve_to_object(&spec.current_root)
            .ok_or_else(|| TreeError::PathNotFound("root not found".to_string()))?;
        Ok(tree::tree_get(&OverlayObjectStore(&resolver), &spec.current_root, &state_path)?)
    }

    /// Set a value within a speculation.
    pub fn set(
        &self,
        handle: SpecHandle,
        store: &dyn ObjectStore,
        path: &str,
        value: &Object,
    ) -> Result<(), SpecError> {
        let mut specs = self.specs.write().unwrap();
        let spec = specs.get_mut(&handle).ok_or(SpecError::NotFound(handle))?;

        let state_path = agentstategraph_core::StatePath::parse(path)
            .map_err(|e| TreeError::PathNotFound(e.to_string()))?;

        // Store the value in the main store (it's content-addressed, so safe)
        let _value_id = store.put_object(value)?;

        // Perform the tree set using the main store
        let new_root = tree::tree_set(store, &spec.current_root, &state_path, value)?;
        spec.current_root = new_root;

        Ok(())
    }

    /// Delete a value within a speculation.
    pub fn delete(
        &self,
        handle: SpecHandle,
        store: &dyn ObjectStore,
        path: &str,
    ) -> Result<(), SpecError> {
        let mut specs = self.specs.write().unwrap();
        let spec = specs.get_mut(&handle).ok_or(SpecError::NotFound(handle))?;

        let state_path = agentstategraph_core::StatePath::parse(path)
            .map_err(|e| TreeError::PathNotFound(e.to_string()))?;

        let new_root = tree::tree_delete(store, &spec.current_root, &state_path)?;
        spec.current_root = new_root;

        Ok(())
    }

    /// Compare multiple speculations against their base state.
    pub fn compare(
        &self,
        handles: &[SpecHandle],
        store: &dyn ObjectStore,
    ) -> Result<SpecComparison, SpecError> {
        let specs = self.specs.read().unwrap();

        let mut entries = Vec::new();
        let mut base_ref = String::new();

        let resolver = StorageObjectResolver(store);

        for &handle in handles {
            let spec = specs.get(&handle).ok_or(SpecError::NotFound(handle))?;
            base_ref = spec.base_ref.clone();

            let diff_ops =
                agentstategraph_core::diff::diff(&resolver, &spec.base_root, &spec.current_root);

            entries.push(SpecComparisonEntry {
                handle,
                label: spec.label.clone(),
                diff_from_base: diff_ops,
            });
        }

        Ok(SpecComparison { base_ref, entries })
    }

    /// Commit a speculation — returns the final state root ObjectId.
    /// The speculation is consumed (removed from the manager).
    pub fn commit(&self, handle: SpecHandle) -> Result<(ObjectId, String), SpecError> {
        let mut specs = self.specs.write().unwrap();
        let spec = specs
            .remove(&handle)
            .ok_or(SpecError::NotFound(handle))?;
        Ok((spec.current_root, spec.base_ref))
    }

    /// Discard a speculation — all changes are lost.
    /// Since we use structural sharing, this is essentially free.
    pub fn discard(&self, handle: SpecHandle) -> Result<(), SpecError> {
        let mut specs = self.specs.write().unwrap();
        specs
            .remove(&handle)
            .ok_or(SpecError::NotFound(handle))?;
        Ok(())
    }

    /// Get the current state root of a speculation (for external use).
    pub fn current_root(&self, handle: SpecHandle) -> Result<ObjectId, SpecError> {
        let specs = self.specs.read().unwrap();
        let spec = specs.get(&handle).ok_or(SpecError::NotFound(handle))?;
        Ok(spec.current_root)
    }

    /// List all active speculations.
    pub fn list(&self) -> Vec<(SpecHandle, Option<String>)> {
        let specs = self.specs.read().unwrap();
        specs
            .iter()
            .map(|(&handle, spec)| (handle, spec.label.clone()))
            .collect()
    }

    /// How many active speculations.
    pub fn count(&self) -> usize {
        self.specs.read().unwrap().len()
    }
}

/// Bridges the ObjectStore to ObjectResolver for diff operations.
struct StorageObjectResolver<'a>(&'a dyn ObjectStore);

impl<'a> ObjectResolver for StorageObjectResolver<'a> {
    fn resolve(&self, id: &ObjectId) -> Option<Object> {
        self.0.get_object(id).ok().flatten()
    }
}

/// Resolver that checks the speculation overlay first, then falls back to store.
struct OverlayResolver<'a> {
    overlay: &'a HashMap<ObjectId, Object>,
    store: &'a dyn ObjectStore,
}

impl<'a> OverlayResolver<'a> {
    fn resolve_to_object(&self, id: &ObjectId) -> Option<Object> {
        self.overlay
            .get(id)
            .cloned()
            .or_else(|| self.store.get_object(id).ok().flatten())
    }
}

/// Wrapper to make OverlayResolver usable as ObjectStore for tree operations.
struct OverlayObjectStore<'a>(&'a OverlayResolver<'a>);

impl<'a> ObjectStore for OverlayObjectStore<'a> {
    fn get_object(&self, id: &ObjectId) -> Result<Option<Object>, agentstategraph_storage::StorageError> {
        Ok(self.0.resolve_to_object(id))
    }

    fn put_object(&self, obj: &Object) -> Result<ObjectId, agentstategraph_storage::StorageError> {
        // Delegate to the underlying store
        self.0.store.put_object(obj)
    }

    fn has_object(&self, id: &ObjectId) -> Result<bool, agentstategraph_storage::StorageError> {
        Ok(self.0.overlay.contains_key(id)
            || self.0.store.has_object(id).unwrap_or(false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentstategraph_core::IntentCategory;
    use agentstategraph_storage::MemoryStorage;

    fn setup() -> (MemoryStorage, ObjectId) {
        let store = MemoryStorage::new();
        let root_id = tree::json_to_tree(
            &store,
            &serde_json::json!({
                "storage": {"type": "none", "mount": "/mnt"},
                "network": {"subnet": "10.0.0.0/24"},
                "nodes": 5
            }),
        )
        .unwrap();
        (store, root_id)
    }

    #[test]
    fn test_create_and_discard() {
        let (store, root_id) = setup();
        let mgr = SpeculationManager::new();

        let h = mgr.create("main", root_id, Some("test".to_string()));
        assert_eq!(mgr.count(), 1);

        mgr.discard(h).unwrap();
        assert_eq!(mgr.count(), 0);
    }

    #[test]
    fn test_speculate_and_read() {
        let (store, root_id) = setup();
        let mgr = SpeculationManager::new();

        let h = mgr.create("main", root_id, None);
        let obj = mgr.get(h, &store, "/storage/type").unwrap();
        assert_eq!(obj, Object::string("none"));
    }

    #[test]
    fn test_speculate_modify_and_read() {
        let (store, root_id) = setup();
        let mgr = SpeculationManager::new();

        let h = mgr.create("main", root_id, None);
        mgr.set(h, &store, "/storage/type", &Object::string("nfs")).unwrap();

        // Speculation has the new value
        let obj = mgr.get(h, &store, "/storage/type").unwrap();
        assert_eq!(obj, Object::string("nfs"));

        // Original state unchanged (check via direct tree access)
        let original = tree::tree_get(
            &store,
            &root_id,
            &agentstategraph_core::StatePath::parse("/storage/type").unwrap(),
        )
        .unwrap();
        assert_eq!(original, Object::string("none"));
    }

    #[test]
    fn test_multiple_speculations() {
        let (store, root_id) = setup();
        let mgr = SpeculationManager::new();

        let nfs = mgr.create("main", root_id, Some("NFS approach".to_string()));
        let ceph = mgr.create("main", root_id, Some("Ceph approach".to_string()));

        mgr.set(nfs, &store, "/storage/type", &Object::string("nfs")).unwrap();
        mgr.set(nfs, &store, "/storage/mount", &Object::string("/shared/nfs")).unwrap();

        mgr.set(ceph, &store, "/storage/type", &Object::string("ceph")).unwrap();
        mgr.set(ceph, &store, "/storage/replicas", &Object::int(3)).unwrap();

        // Each speculation has its own state
        assert_eq!(
            mgr.get(nfs, &store, "/storage/type").unwrap(),
            Object::string("nfs")
        );
        assert_eq!(
            mgr.get(ceph, &store, "/storage/type").unwrap(),
            Object::string("ceph")
        );

        assert_eq!(mgr.count(), 2);
    }

    #[test]
    fn test_compare_speculations() {
        let (store, root_id) = setup();
        let mgr = SpeculationManager::new();

        let nfs = mgr.create("main", root_id, Some("NFS".to_string()));
        let ceph = mgr.create("main", root_id, Some("Ceph".to_string()));

        mgr.set(nfs, &store, "/storage/type", &Object::string("nfs")).unwrap();
        mgr.set(ceph, &store, "/storage/type", &Object::string("ceph")).unwrap();

        let comparison = mgr.compare(&[nfs, ceph], &store).unwrap();
        assert_eq!(comparison.entries.len(), 2);

        // Both should have diffs from base
        assert!(!comparison.entries[0].diff_from_base.is_empty());
        assert!(!comparison.entries[1].diff_from_base.is_empty());

        assert_eq!(comparison.entries[0].label, Some("NFS".to_string()));
        assert_eq!(comparison.entries[1].label, Some("Ceph".to_string()));
    }

    #[test]
    fn test_commit_speculation() {
        let (store, root_id) = setup();
        let mgr = SpeculationManager::new();

        let h = mgr.create("main", root_id, Some("winner".to_string()));
        mgr.set(h, &store, "/storage/type", &Object::string("nfs")).unwrap();

        let (committed_root, base_ref) = mgr.commit(h).unwrap();
        assert_eq!(base_ref, "main");
        assert_ne!(committed_root, root_id, "committed root should differ from base");

        // Speculation is consumed
        assert_eq!(mgr.count(), 0);
        assert!(mgr.get(h, &store, "/storage/type").is_err());
    }

    #[test]
    fn test_discard_is_instant() {
        let (store, root_id) = setup();
        let mgr = SpeculationManager::new();

        // Create many speculations
        let handles: Vec<_> = (0..100)
            .map(|i| mgr.create("main", root_id, Some(format!("spec-{}", i))))
            .collect();

        assert_eq!(mgr.count(), 100);

        // Discard all — should be instant (just dropping handles)
        for h in handles {
            mgr.discard(h).unwrap();
        }
        assert_eq!(mgr.count(), 0);
    }

    #[test]
    fn test_delete_in_speculation() {
        let (store, root_id) = setup();
        let mgr = SpeculationManager::new();

        let h = mgr.create("main", root_id, None);
        mgr.delete(h, &store, "/network").unwrap();

        // Network should be gone in speculation
        assert!(mgr.get(h, &store, "/network").is_err());

        // But still exists in base
        let original = tree::tree_get(
            &store,
            &root_id,
            &agentstategraph_core::StatePath::parse("/network").unwrap(),
        );
        assert!(original.is_ok());
    }

    #[test]
    fn test_list_speculations() {
        let (store, root_id) = setup();
        let mgr = SpeculationManager::new();

        mgr.create("main", root_id, Some("alpha".to_string()));
        mgr.create("main", root_id, Some("beta".to_string()));
        mgr.create("main", root_id, None);

        let list = mgr.list();
        assert_eq!(list.len(), 3);
    }
}
