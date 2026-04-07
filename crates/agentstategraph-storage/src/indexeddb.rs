//! IndexedDB storage backend for WASM — browser-native persistent storage.
//!
//! Uses the browser's IndexedDB to store objects, commits, and refs.
//! Data survives page refreshes and browser restarts.
//!
//! Three IndexedDB object stores:
//!   "objects"  → ObjectId (hex string) → Object (JSON)
//!   "commits"  → ObjectId (hex string) → Commit (JSON)
//!   "refs"     → name (string)         → ObjectId (hex string)
//!
//! Note: IndexedDB is async but our storage traits are sync.
//! We use a write-through in-memory cache backed by IndexedDB:
//! - All reads come from the in-memory cache (fast, sync)
//! - All writes go to both memory and IndexedDB (durability)
//! - On construction, the full store is loaded from IndexedDB into memory

use std::collections::{BTreeMap, HashMap};
use std::sync::RwLock;

use crate::memory::MemoryStorage;
use crate::traits::{CommitStore, ObjectStore, RefStore, StorageError};
use agentstategraph_core::{Commit, Object, ObjectId};

/// IndexedDB-backed storage with in-memory cache.
///
/// This wraps MemoryStorage and adds IndexedDB persistence.
/// The in-memory layer handles all sync reads; writes are flushed
/// to IndexedDB asynchronously.
///
/// Usage (from WASM):
/// ```js
/// const storage = await IndexedDbStorage.open("my-stategraph");
/// ```
pub struct IndexedDbStorage {
    /// The in-memory cache that handles all sync operations.
    memory: MemoryStorage,
    /// Database name (for IndexedDB).
    db_name: String,
    /// Pending writes queue — flushed to IndexedDB by the WASM layer.
    pending_objects: RwLock<Vec<(String, String)>>, // (hex_id, json)
    pending_commits: RwLock<Vec<(String, String)>>, // (hex_id, json)
    pending_refs: RwLock<Vec<(String, String)>>,    // (name, hex_id)
    deleted_refs: RwLock<Vec<String>>,              // names to delete
}

impl IndexedDbStorage {
    /// Create a new IndexedDbStorage. Call `load_from_json` after construction
    /// to hydrate from IndexedDB data.
    pub fn new(db_name: &str) -> Self {
        Self {
            memory: MemoryStorage::new(),
            db_name: db_name.to_string(),
            pending_objects: RwLock::new(Vec::new()),
            pending_commits: RwLock::new(Vec::new()),
            pending_refs: RwLock::new(Vec::new()),
            deleted_refs: RwLock::new(Vec::new()),
        }
    }

    /// Load objects from a JSON dump (called from JS after reading IndexedDB).
    pub fn load_objects(&self, json_pairs: &[(String, String)]) -> Result<(), StorageError> {
        for (hex_id, json) in json_pairs {
            let obj: Object = serde_json::from_str(json)
                .map_err(|e| StorageError::Serialization(e.to_string()))?;
            self.memory.put_object(&obj)?;
        }
        Ok(())
    }

    /// Load commits from a JSON dump.
    pub fn load_commits(&self, json_pairs: &[(String, String)]) -> Result<(), StorageError> {
        for (_hex_id, json) in json_pairs {
            let commit: Commit = serde_json::from_str(json)
                .map_err(|e| StorageError::Serialization(e.to_string()))?;
            self.memory.put_commit(&commit)?;
        }
        Ok(())
    }

    /// Load refs from key-value pairs.
    pub fn load_refs(&self, pairs: &[(String, String)]) -> Result<(), StorageError> {
        for (name, hex_id) in pairs {
            let bytes = hex_to_bytes(hex_id)
                .ok_or_else(|| StorageError::Serialization("invalid hex id".to_string()))?;
            let mut arr = [0u8; 32];
            if bytes.len() != 32 {
                return Err(StorageError::Serialization(
                    "id must be 32 bytes".to_string(),
                ));
            }
            arr.copy_from_slice(&bytes);
            let id = ObjectId::from_bytes(arr);
            self.memory.set_ref(name, id)?;
        }
        Ok(())
    }

    /// Drain pending object writes (for flushing to IndexedDB from JS).
    pub fn drain_pending_objects(&self) -> Vec<(String, String)> {
        let mut pending = self.pending_objects.write().unwrap();
        std::mem::take(&mut *pending)
    }

    /// Drain pending commit writes.
    pub fn drain_pending_commits(&self) -> Vec<(String, String)> {
        let mut pending = self.pending_commits.write().unwrap();
        std::mem::take(&mut *pending)
    }

    /// Drain pending ref writes.
    pub fn drain_pending_refs(&self) -> Vec<(String, String)> {
        let mut pending = self.pending_refs.write().unwrap();
        std::mem::take(&mut *pending)
    }

    /// Drain pending ref deletions.
    pub fn drain_deleted_refs(&self) -> Vec<String> {
        let mut deleted = self.deleted_refs.write().unwrap();
        std::mem::take(&mut *deleted)
    }

    /// Get the database name.
    pub fn db_name(&self) -> &str {
        &self.db_name
    }
}

impl ObjectStore for IndexedDbStorage {
    fn get_object(&self, id: &ObjectId) -> Result<Option<Object>, StorageError> {
        self.memory.get_object(id)
    }

    fn put_object(&self, obj: &Object) -> Result<ObjectId, StorageError> {
        let id = self.memory.put_object(obj)?;
        // Queue for IndexedDB flush
        let hex_id = format!("{}", id);
        let json =
            serde_json::to_string(obj).map_err(|e| StorageError::Serialization(e.to_string()))?;
        self.pending_objects.write().unwrap().push((hex_id, json));
        Ok(id)
    }

    fn has_object(&self, id: &ObjectId) -> Result<bool, StorageError> {
        self.memory.has_object(id)
    }
}

impl CommitStore for IndexedDbStorage {
    fn get_commit(&self, id: &ObjectId) -> Result<Option<Commit>, StorageError> {
        self.memory.get_commit(id)
    }

    fn put_commit(&self, commit: &Commit) -> Result<(), StorageError> {
        self.memory.put_commit(commit)?;
        let hex_id = format!("{}", commit.id);
        let json = serde_json::to_string(commit)
            .map_err(|e| StorageError::Serialization(e.to_string()))?;
        self.pending_commits.write().unwrap().push((hex_id, json));
        Ok(())
    }

    fn has_commit(&self, id: &ObjectId) -> Result<bool, StorageError> {
        self.memory.has_commit(id)
    }

    fn list_commits(&self, from: &ObjectId, limit: usize) -> Result<Vec<Commit>, StorageError> {
        self.memory.list_commits(from, limit)
    }
}

impl RefStore for IndexedDbStorage {
    fn get_ref(&self, name: &str) -> Result<Option<ObjectId>, StorageError> {
        self.memory.get_ref(name)
    }

    fn set_ref(&self, name: &str, target: ObjectId) -> Result<(), StorageError> {
        self.memory.set_ref(name, target)?;
        let hex_id = format!("{}", target);
        self.pending_refs
            .write()
            .unwrap()
            .push((name.to_string(), hex_id));
        Ok(())
    }

    fn cas_ref(&self, name: &str, expected: ObjectId, new: ObjectId) -> Result<bool, StorageError> {
        let result = self.memory.cas_ref(name, expected, new)?;
        if result {
            let hex_id = format!("{}", new);
            self.pending_refs
                .write()
                .unwrap()
                .push((name.to_string(), hex_id));
        }
        Ok(result)
    }

    fn list_refs(&self, prefix: &str) -> Result<Vec<(String, ObjectId)>, StorageError> {
        self.memory.list_refs(prefix)
    }

    fn delete_ref(&self, name: &str) -> Result<bool, StorageError> {
        let result = self.memory.delete_ref(name)?;
        if result {
            self.deleted_refs.write().unwrap().push(name.to_string());
        }
        Ok(result)
    }
}

/// Convert hex string to bytes.
fn hex_to_bytes(hex: &str) -> Option<Vec<u8>> {
    // Strip "sg_" prefix if present
    let hex = hex.strip_prefix("sg_").unwrap_or(hex);
    if hex.len() % 2 != 0 {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentstategraph_core::*;

    #[test]
    fn test_basic_operations() {
        let store = IndexedDbStorage::new("test-db");

        let obj = Object::string("hello indexeddb");
        let id = store.put_object(&obj).unwrap();

        let retrieved = store.get_object(&id).unwrap();
        assert_eq!(retrieved, Some(obj));
    }

    #[test]
    fn test_pending_writes_queued() {
        let store = IndexedDbStorage::new("test-db");

        store.put_object(&Object::string("a")).unwrap();
        store.put_object(&Object::string("b")).unwrap();

        let pending = store.drain_pending_objects();
        assert_eq!(pending.len(), 2);

        // After drain, no more pending
        let pending2 = store.drain_pending_objects();
        assert_eq!(pending2.len(), 0);
    }

    #[test]
    fn test_refs_pending() {
        let store = IndexedDbStorage::new("test-db");
        let target = ObjectId::hash(b"commit");

        store.set_ref("main", target).unwrap();

        let pending = store.drain_pending_refs();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].0, "main");
    }

    #[test]
    fn test_load_and_read() {
        let store = IndexedDbStorage::new("test-db");

        // Simulate loading from IndexedDB
        let obj = Object::string("persisted");
        let json = serde_json::to_string(&obj).unwrap();
        let id = obj.id();
        let hex = format!("{}", id);

        store.load_objects(&[(hex, json)]).unwrap();

        let retrieved = store.get_object(&id).unwrap();
        assert_eq!(retrieved, Some(obj));
    }

    #[test]
    fn test_commit_pending() {
        let store = IndexedDbStorage::new("test-db");

        let commit = CommitBuilder::new(
            ObjectId::hash(b"state"),
            "agent/test",
            Authority::simple("test"),
            Intent::new(IntentCategory::Checkpoint, "test"),
        )
        .build();

        store.put_commit(&commit).unwrap();

        let pending = store.drain_pending_commits();
        assert_eq!(pending.len(), 1);

        // Can still read from memory cache
        let retrieved = store.get_commit(&commit.id).unwrap();
        assert!(retrieved.is_some());
    }

    #[test]
    fn test_delete_ref_pending() {
        let store = IndexedDbStorage::new("test-db");
        let target = ObjectId::hash(b"commit");

        store.set_ref("temp", target).unwrap();
        store.drain_pending_refs(); // clear

        store.delete_ref("temp").unwrap();
        let deleted = store.drain_deleted_refs();
        assert_eq!(deleted, vec!["temp"]);
    }
}
