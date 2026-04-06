//! In-memory storage backend.
//!
//! Fast, ephemeral storage suitable for testing, speculation,
//! and workflows that don't need durability.

use std::collections::{BTreeMap, HashMap};
use std::sync::RwLock;

use stategraph_core::{Commit, Object, ObjectId};

use crate::traits::{CommitStore, ObjectStore, RefStore, StorageError};

/// In-memory storage backend. Thread-safe via RwLock.
///
/// All data is lost when the process exits. Use SQLite for durable storage.
pub struct MemoryStorage {
    objects: RwLock<HashMap<ObjectId, Object>>,
    commits: RwLock<HashMap<ObjectId, Commit>>,
    refs: RwLock<BTreeMap<String, ObjectId>>,
}

impl MemoryStorage {
    pub fn new() -> Self {
        Self {
            objects: RwLock::new(HashMap::new()),
            commits: RwLock::new(HashMap::new()),
            refs: RwLock::new(BTreeMap::new()),
        }
    }
}

impl Default for MemoryStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl ObjectStore for MemoryStorage {
    fn get_object(&self, id: &ObjectId) -> Result<Option<Object>, StorageError> {
        let store = self.objects.read().map_err(|e| StorageError::Backend(e.to_string()))?;
        Ok(store.get(id).cloned())
    }

    fn put_object(&self, obj: &Object) -> Result<ObjectId, StorageError> {
        let id = obj.id();
        let mut store = self.objects.write().map_err(|e| StorageError::Backend(e.to_string()))?;
        store.entry(id).or_insert_with(|| obj.clone());
        Ok(id)
    }

    fn has_object(&self, id: &ObjectId) -> Result<bool, StorageError> {
        let store = self.objects.read().map_err(|e| StorageError::Backend(e.to_string()))?;
        Ok(store.contains_key(id))
    }
}

impl CommitStore for MemoryStorage {
    fn get_commit(&self, id: &ObjectId) -> Result<Option<Commit>, StorageError> {
        let store = self.commits.read().map_err(|e| StorageError::Backend(e.to_string()))?;
        Ok(store.get(id).cloned())
    }

    fn put_commit(&self, commit: &Commit) -> Result<(), StorageError> {
        let mut store = self.commits.write().map_err(|e| StorageError::Backend(e.to_string()))?;
        store.insert(commit.id, commit.clone());
        Ok(())
    }

    fn has_commit(&self, id: &ObjectId) -> Result<bool, StorageError> {
        let store = self.commits.read().map_err(|e| StorageError::Backend(e.to_string()))?;
        Ok(store.contains_key(id))
    }

    fn list_commits(&self, from: &ObjectId, limit: usize) -> Result<Vec<Commit>, StorageError> {
        let store = self.commits.read().map_err(|e| StorageError::Backend(e.to_string()))?;
        let mut result = Vec::new();
        let mut current = Some(*from);

        while let Some(id) = current {
            if result.len() >= limit {
                break;
            }
            if let Some(commit) = store.get(&id) {
                result.push(commit.clone());
                // Follow first parent for linear history traversal
                current = commit.parents.first().copied();
            } else {
                break;
            }
        }

        Ok(result)
    }
}

impl RefStore for MemoryStorage {
    fn get_ref(&self, name: &str) -> Result<Option<ObjectId>, StorageError> {
        let store = self.refs.read().map_err(|e| StorageError::Backend(e.to_string()))?;
        Ok(store.get(name).copied())
    }

    fn set_ref(&self, name: &str, target: ObjectId) -> Result<(), StorageError> {
        let mut store = self.refs.write().map_err(|e| StorageError::Backend(e.to_string()))?;
        store.insert(name.to_string(), target);
        Ok(())
    }

    fn cas_ref(
        &self,
        name: &str,
        expected: ObjectId,
        new: ObjectId,
    ) -> Result<bool, StorageError> {
        let mut store = self.refs.write().map_err(|e| StorageError::Backend(e.to_string()))?;
        match store.get(name) {
            Some(&current) if current == expected => {
                store.insert(name.to_string(), new);
                Ok(true)
            }
            Some(_) => Ok(false),
            None => {
                // Ref doesn't exist — only succeed if expected is also "empty"
                // For now, fail the CAS if the ref doesn't exist
                Ok(false)
            }
        }
    }

    fn list_refs(&self, prefix: &str) -> Result<Vec<(String, ObjectId)>, StorageError> {
        let store = self.refs.read().map_err(|e| StorageError::Backend(e.to_string()))?;
        let result = store
            .range(prefix.to_string()..)
            .take_while(|(k, _)| k.starts_with(prefix))
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        Ok(result)
    }

    fn delete_ref(&self, name: &str) -> Result<bool, StorageError> {
        let mut store = self.refs.write().map_err(|e| StorageError::Backend(e.to_string()))?;
        Ok(store.remove(name).is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stategraph_core::*;

    #[test]
    fn test_object_store_roundtrip() {
        let store = MemoryStorage::new();

        let obj = Object::string("hello world");
        let id = store.put_object(&obj).unwrap();

        let retrieved = store.get_object(&id).unwrap();
        assert_eq!(retrieved, Some(obj));
    }

    #[test]
    fn test_object_deduplication() {
        let store = MemoryStorage::new();

        let obj1 = Object::string("duplicate");
        let obj2 = Object::string("duplicate");

        let id1 = store.put_object(&obj1).unwrap();
        let id2 = store.put_object(&obj2).unwrap();

        assert_eq!(id1, id2, "identical objects should produce same ID");
    }

    #[test]
    fn test_ref_operations() {
        let store = MemoryStorage::new();
        let target = ObjectId::hash(b"test-commit");

        // Set
        store.set_ref("main", target).unwrap();
        assert_eq!(store.get_ref("main").unwrap(), Some(target));

        // CAS success
        let new_target = ObjectId::hash(b"new-commit");
        assert!(store.cas_ref("main", target, new_target).unwrap());
        assert_eq!(store.get_ref("main").unwrap(), Some(new_target));

        // CAS failure (stale expected value)
        let stale = ObjectId::hash(b"stale");
        let another = ObjectId::hash(b"another");
        assert!(!store.cas_ref("main", stale, another).unwrap());
        assert_eq!(store.get_ref("main").unwrap(), Some(new_target)); // unchanged
    }

    #[test]
    fn test_list_refs_with_prefix() {
        let store = MemoryStorage::new();

        store.set_ref("agents/planner/workspace", ObjectId::hash(b"a")).unwrap();
        store.set_ref("agents/storage/workspace", ObjectId::hash(b"b")).unwrap();
        store.set_ref("main", ObjectId::hash(b"c")).unwrap();

        let agent_refs = store.list_refs("agents/").unwrap();
        assert_eq!(agent_refs.len(), 2);

        let all_refs = store.list_refs("").unwrap();
        assert_eq!(all_refs.len(), 3);
    }

    #[test]
    fn test_delete_ref() {
        let store = MemoryStorage::new();
        let target = ObjectId::hash(b"test");

        store.set_ref("temp", target).unwrap();
        assert!(store.delete_ref("temp").unwrap());
        assert_eq!(store.get_ref("temp").unwrap(), None);
        assert!(!store.delete_ref("temp").unwrap()); // already deleted
    }

    #[test]
    fn test_commit_store() {
        let store = MemoryStorage::new();

        let commit = CommitBuilder::new(
            ObjectId::hash(b"state"),
            "agent/test",
            Authority::simple("test"),
            Intent::new(IntentCategory::Checkpoint, "initial state"),
        )
        .build();

        store.put_commit(&commit).unwrap();
        let retrieved = store.get_commit(&commit.id).unwrap();
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().agent_id, "agent/test");
    }
}
