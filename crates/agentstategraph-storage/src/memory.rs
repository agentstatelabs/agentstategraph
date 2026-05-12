//! In-memory storage backend.
//!
//! Fast, ephemeral storage suitable for testing, speculation,
//! and workflows that don't need durability.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::RwLock;

use chrono::{DateTime, Utc};

use agentstategraph_core::{
    Commit, Epoch, EpochStatus, Namespace, Object, ObjectId, Session, SessionStatus,
};
use agentstategraph_reminders::{
    MemoryReminderStore, Reminder, ReminderError, ReminderFilter, ReminderStore,
};
use agentstategraph_taint::{Taint, TaintKind};

use crate::traits::{
    CommitStore, EpochStore, ObjectStore, RefStore, SessionStore, StorageError, TaintStore,
};

/// In-memory storage backend. Thread-safe via RwLock.
///
/// All data is lost when the process exits. Use SQLite for durable storage.
pub struct MemoryStorage {
    objects: RwLock<HashMap<ObjectId, Object>>,
    commits: RwLock<HashMap<ObjectId, Commit>>,
    /// Refs keyed by (namespace, name). Namespace must exist in `namespaces`.
    refs: RwLock<BTreeMap<(String, String), ObjectId>>,
    /// Known namespaces. A ref cannot be created in an unknown namespace.
    namespaces: RwLock<HashSet<String>>,
    epochs: RwLock<Vec<Epoch>>,
    sessions: RwLock<HashMap<String, Session>>,
    /// (commit_id, epoch_id) associations, in insertion order.
    commit_epoch: RwLock<Vec<(ObjectId, String)>>,
    /// (commit_id, session_id) associations, in insertion order.
    commit_session: RwLock<Vec<(ObjectId, String)>>,
    /// Taints keyed by id, insertion-ordered for deterministic
    /// list output.
    taints: RwLock<Vec<Taint>>,
    /// Reminders delegated to the in-crate memory store.
    pub(crate) reminders: MemoryReminderStore,
}

impl MemoryStorage {
    pub fn new() -> Self {
        let mut namespaces = HashSet::new();
        namespaces.insert(Namespace::DEFAULT.to_string());
        Self {
            objects: RwLock::new(HashMap::new()),
            commits: RwLock::new(HashMap::new()),
            refs: RwLock::new(BTreeMap::new()),
            namespaces: RwLock::new(namespaces),
            epochs: RwLock::new(Vec::new()),
            sessions: RwLock::new(HashMap::new()),
            commit_epoch: RwLock::new(Vec::new()),
            commit_session: RwLock::new(Vec::new()),
            taints: RwLock::new(Vec::new()),
            reminders: MemoryReminderStore::new(),
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
        let store = self
            .objects
            .read()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        Ok(store.get(id).cloned())
    }

    fn put_object(&self, obj: &Object) -> Result<ObjectId, StorageError> {
        let id = obj.id();
        let mut store = self
            .objects
            .write()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        store.entry(id).or_insert_with(|| obj.clone());
        Ok(id)
    }

    fn has_object(&self, id: &ObjectId) -> Result<bool, StorageError> {
        let store = self
            .objects
            .read()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        Ok(store.contains_key(id))
    }
}

impl CommitStore for MemoryStorage {
    fn get_commit(&self, id: &ObjectId) -> Result<Option<Commit>, StorageError> {
        let store = self
            .commits
            .read()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        Ok(store.get(id).cloned())
    }

    fn put_commit(&self, commit: &Commit) -> Result<(), StorageError> {
        let mut store = self
            .commits
            .write()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        store.insert(commit.id, commit.clone());
        Ok(())
    }

    fn has_commit(&self, id: &ObjectId) -> Result<bool, StorageError> {
        let store = self
            .commits
            .read()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        Ok(store.contains_key(id))
    }

    fn list_commits(&self, from: &ObjectId, limit: usize) -> Result<Vec<Commit>, StorageError> {
        let store = self
            .commits
            .read()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
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
    fn create_namespace(&self, namespace: &Namespace) -> Result<(), StorageError> {
        let mut ns_store = self
            .namespaces
            .write()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        if !ns_store.insert(namespace.as_str().to_string()) {
            return Err(StorageError::NamespaceAlreadyExists(
                namespace.as_str().to_string(),
            ));
        }
        Ok(())
    }

    fn list_namespaces(&self) -> Result<Vec<Namespace>, StorageError> {
        let ns_store = self
            .namespaces
            .read()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        let mut names: Vec<Namespace> = ns_store
            .iter()
            .map(|s| Namespace::new(s).expect("stored namespace is always valid"))
            .collect();
        names.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        Ok(names)
    }

    fn get_ref(&self, namespace: &Namespace, name: &str) -> Result<Option<ObjectId>, StorageError> {
        let ns_store = self
            .namespaces
            .read()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        if !ns_store.contains(namespace.as_str()) {
            return Err(StorageError::NamespaceNotFound(
                namespace.as_str().to_string(),
            ));
        }
        drop(ns_store);
        let store = self
            .refs
            .read()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        Ok(store
            .get(&(namespace.as_str().to_string(), name.to_string()))
            .copied())
    }

    fn set_ref(
        &self,
        namespace: &Namespace,
        name: &str,
        target: ObjectId,
    ) -> Result<(), StorageError> {
        let ns_store = self
            .namespaces
            .read()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        if !ns_store.contains(namespace.as_str()) {
            return Err(StorageError::NamespaceNotFound(
                namespace.as_str().to_string(),
            ));
        }
        drop(ns_store);
        let mut store = self
            .refs
            .write()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        store.insert((namespace.as_str().to_string(), name.to_string()), target);
        Ok(())
    }

    fn cas_ref(
        &self,
        namespace: &Namespace,
        name: &str,
        expected: ObjectId,
        new: ObjectId,
    ) -> Result<bool, StorageError> {
        let ns_store = self
            .namespaces
            .read()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        if !ns_store.contains(namespace.as_str()) {
            return Err(StorageError::NamespaceNotFound(
                namespace.as_str().to_string(),
            ));
        }
        drop(ns_store);
        let mut store = self
            .refs
            .write()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        let key = (namespace.as_str().to_string(), name.to_string());
        match store.get(&key) {
            Some(&current) if current == expected => {
                store.insert(key, new);
                Ok(true)
            }
            Some(_) => Ok(false),
            None => Ok(false),
        }
    }

    fn list_refs(
        &self,
        namespace: &Namespace,
        prefix: &str,
    ) -> Result<Vec<(String, ObjectId)>, StorageError> {
        let ns_store = self
            .namespaces
            .read()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        if !ns_store.contains(namespace.as_str()) {
            return Err(StorageError::NamespaceNotFound(
                namespace.as_str().to_string(),
            ));
        }
        drop(ns_store);
        let store = self
            .refs
            .read()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        let ns = namespace.as_str().to_string();
        let result = store
            .iter()
            .filter(|((n, name), _)| n == &ns && name.starts_with(prefix))
            .map(|((_, name), id)| (name.clone(), *id))
            .collect();
        Ok(result)
    }

    fn delete_ref(&self, namespace: &Namespace, name: &str) -> Result<bool, StorageError> {
        let ns_store = self
            .namespaces
            .read()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        if !ns_store.contains(namespace.as_str()) {
            return Err(StorageError::NamespaceNotFound(
                namespace.as_str().to_string(),
            ));
        }
        drop(ns_store);
        let mut store = self
            .refs
            .write()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        Ok(store
            .remove(&(namespace.as_str().to_string(), name.to_string()))
            .is_some())
    }
}

impl EpochStore for MemoryStorage {
    fn create_epoch(&self, epoch: &Epoch) -> Result<(), StorageError> {
        let mut epochs = self
            .epochs
            .write()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        if epochs.iter().any(|e| e.id == epoch.id) {
            return Err(StorageError::Backend(format!(
                "epoch '{}' already exists",
                epoch.id
            )));
        }
        epochs.push(epoch.clone());
        Ok(())
    }

    fn seal_epoch(
        &self,
        id: &str,
        summary: &str,
        sealed_at: DateTime<Utc>,
        sealed_commits: &[ObjectId],
    ) -> Result<(), StorageError> {
        let mut epochs = self
            .epochs
            .write()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        let epoch = epochs
            .iter_mut()
            .find(|e| e.id == id)
            .ok_or_else(|| StorageError::Backend(format!("epoch not found: {}", id)))?;
        if epoch.status == EpochStatus::Sealed || epoch.status == EpochStatus::Archived {
            return Err(StorageError::EpochAlreadySealed { id: id.to_string() });
        }
        epoch.status = EpochStatus::Sealed;
        epoch.sealed_at = Some(sealed_at);
        epoch.seal_summary = Some(summary.to_string());
        epoch.sealed_commits = sealed_commits.to_vec();
        Ok(())
    }

    fn list_epochs(&self) -> Result<Vec<Epoch>, StorageError> {
        let epochs = self
            .epochs
            .read()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        let mut out: Vec<Epoch> = epochs.clone();
        out.sort_by_key(|b| std::cmp::Reverse(b.created_at));
        Ok(out)
    }

    fn get_epoch(&self, id: &str) -> Result<Option<Epoch>, StorageError> {
        let epochs = self
            .epochs
            .read()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        let Some(mut epoch) = epochs.iter().find(|e| e.id == id).cloned() else {
            return Ok(None);
        };
        // Rehydrate commits from the association map.
        let assoc = self
            .commit_epoch
            .read()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        epoch.commits = assoc
            .iter()
            .filter(|(_, eid)| eid == id)
            .map(|(cid, _)| *cid)
            .collect();
        Ok(Some(epoch))
    }

    fn archive_epoch(&self, id: &str) -> Result<(), StorageError> {
        let mut epochs = self
            .epochs
            .write()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        let epoch = epochs
            .iter_mut()
            .find(|e| e.id == id)
            .ok_or_else(|| StorageError::Backend(format!("epoch not found: {}", id)))?;
        if epoch.status != EpochStatus::Sealed {
            return Err(StorageError::Backend(format!(
                "epoch '{}' is not sealed",
                id
            )));
        }
        epoch.status = EpochStatus::Archived;
        Ok(())
    }

    fn set_commit_epoch(&self, commit_id: &ObjectId, epoch_id: &str) -> Result<(), StorageError> {
        // Enforce seal semantics first.
        {
            let epochs = self
                .epochs
                .read()
                .map_err(|e| StorageError::Backend(e.to_string()))?;
            let epoch = epochs
                .iter()
                .find(|e| e.id == epoch_id)
                .ok_or_else(|| StorageError::Backend(format!("epoch not found: {}", epoch_id)))?;
            if epoch.status == EpochStatus::Sealed || epoch.status == EpochStatus::Archived {
                return Err(StorageError::EpochAlreadySealed {
                    id: epoch_id.to_string(),
                });
            }
        }
        let mut assoc = self
            .commit_epoch
            .write()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        assoc.push((*commit_id, epoch_id.to_string()));
        Ok(())
    }
}

impl SessionStore for MemoryStorage {
    fn create_session(&self, session: &Session) -> Result<(), StorageError> {
        let mut sessions = self
            .sessions
            .write()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        sessions.insert(session.id.clone(), session.clone());
        Ok(())
    }

    fn end_session(
        &self,
        id: &str,
        status: SessionStatus,
        ended_at: DateTime<Utc>,
    ) -> Result<(), StorageError> {
        let mut sessions = self
            .sessions
            .write()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        let session = sessions
            .get_mut(id)
            .ok_or_else(|| StorageError::Backend(format!("session not found: {}", id)))?;
        if session.status != SessionStatus::Active {
            return Err(StorageError::SessionEnded { id: id.to_string() });
        }
        session.status = status;
        session.ended_at = Some(ended_at);
        Ok(())
    }

    fn list_sessions(&self, agent_filter: Option<&str>) -> Result<Vec<Session>, StorageError> {
        let sessions = self
            .sessions
            .read()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        Ok(sessions
            .values()
            .filter(|s| agent_filter.map(|f| s.agent_id == f).unwrap_or(true))
            .cloned()
            .collect())
    }

    fn get_session(&self, id: &str) -> Result<Option<Session>, StorageError> {
        let sessions = self
            .sessions
            .read()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        Ok(sessions.get(id).cloned())
    }

    fn set_commit_session(
        &self,
        commit_id: &ObjectId,
        session_id: &str,
    ) -> Result<(), StorageError> {
        {
            let sessions = self
                .sessions
                .read()
                .map_err(|e| StorageError::Backend(e.to_string()))?;
            let session = sessions.get(session_id).ok_or_else(|| {
                StorageError::Backend(format!("session not found: {}", session_id))
            })?;
            if session.status != SessionStatus::Active {
                return Err(StorageError::SessionEnded {
                    id: session_id.to_string(),
                });
            }
        }
        let mut assoc = self
            .commit_session
            .write()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        assoc.push((*commit_id, session_id.to_string()));
        Ok(())
    }
}

impl TaintStore for MemoryStorage {
    fn create_taint(&self, taint: &Taint) -> Result<(), StorageError> {
        let mut list = self
            .taints
            .write()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        // Uniqueness: no other active (unresolved) row for
        // (path, name, kind).
        for existing in list.iter() {
            if existing.resolved_at.is_some() {
                continue;
            }
            if existing.path == taint.path
                && existing.name == taint.name
                && existing.kind == taint.kind
            {
                return Err(StorageError::Backend(format!(
                    "duplicate active taint ({path}, {name}, {kind:?})",
                    path = taint.path,
                    name = taint.name,
                    kind = taint.kind,
                )));
            }
        }
        list.push(taint.clone());
        Ok(())
    }

    fn resolve_taint(
        &self,
        id: &str,
        resolved_by: &str,
        reason: &str,
        proof: Option<&str>,
        resolved_at: DateTime<Utc>,
    ) -> Result<(), StorageError> {
        let mut list = self
            .taints
            .write()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        let t = list
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| StorageError::Backend(format!("taint {id} not found")))?;
        if t.resolved_at.is_some() {
            return Err(StorageError::Backend(format!(
                "taint {id} is already resolved"
            )));
        }
        t.resolved_at = Some(resolved_at);
        t.resolved_by = Some(resolved_by.to_string());
        t.resolved_reason = Some(reason.to_string());
        t.resolved_proof = proof.map(str::to_string);
        Ok(())
    }

    fn list_taints(
        &self,
        path_prefix: Option<&str>,
        kind: Option<TaintKind>,
        include_resolved: bool,
    ) -> Result<Vec<Taint>, StorageError> {
        let list = self
            .taints
            .read()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        let mut out: Vec<Taint> = list
            .iter()
            .filter(|t| include_resolved || t.resolved_at.is_none())
            .filter(|t| match path_prefix {
                None => true,
                Some(p) => {
                    t.path == p || t.path.starts_with(&format!("{}/", p.trim_end_matches('/')))
                }
            })
            .filter(|t| kind.map(|k| k == t.kind).unwrap_or(true))
            .cloned()
            .collect();
        // Most-recently-created first for deterministic ordering.
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(out)
    }

    fn check_taint(&self, request_path: &str) -> Result<Vec<Taint>, StorageError> {
        let list = self
            .taints
            .read()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        let now = Utc::now();
        let out: Vec<Taint> = list
            .iter()
            .filter(|t| t.is_active(now))
            .filter(|t| {
                if t.path == request_path {
                    return true;
                }
                if !t.propagate {
                    return false;
                }
                let prefix = if t.path.ends_with('/') {
                    t.path.clone()
                } else {
                    format!("{}/", t.path)
                };
                request_path.starts_with(&prefix)
            })
            .cloned()
            .collect();
        Ok(out)
    }

    fn get_taint(&self, id: &str) -> Result<Option<Taint>, StorageError> {
        let list = self
            .taints
            .read()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        Ok(list.iter().find(|t| t.id == id).cloned())
    }

    fn set_taint_commit_id(&self, id: &str, commit_id: &str) -> Result<(), StorageError> {
        let mut list = self
            .taints
            .write()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        let t = list
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| StorageError::Backend(format!("taint {id} not found")))?;
        t.commit_id = commit_id.to_string();
        Ok(())
    }
}

impl ReminderStore for MemoryStorage {
    fn save(&self, reminder: &Reminder) -> Result<(), ReminderError> {
        self.reminders.save(reminder)
    }
    fn get(&self, id: &str) -> Result<Option<Reminder>, ReminderError> {
        self.reminders.get(id)
    }
    fn update(&self, reminder: &Reminder) -> Result<(), ReminderError> {
        self.reminders.update(reminder)
    }
    fn delete(&self, id: &str) -> Result<bool, ReminderError> {
        self.reminders.delete(id)
    }
    fn list(&self, filter: &ReminderFilter) -> Result<Vec<Reminder>, ReminderError> {
        self.reminders.list(filter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentstategraph_core::*;

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
        let ns = Namespace::default_ns();
        let target = ObjectId::hash(b"test-commit");

        // Set
        store.set_ref(&ns, "main", target).unwrap();
        assert_eq!(store.get_ref(&ns, "main").unwrap(), Some(target));

        // CAS success
        let new_target = ObjectId::hash(b"new-commit");
        assert!(store.cas_ref(&ns, "main", target, new_target).unwrap());
        assert_eq!(store.get_ref(&ns, "main").unwrap(), Some(new_target));

        // CAS failure (stale expected value)
        let stale = ObjectId::hash(b"stale");
        let another = ObjectId::hash(b"another");
        assert!(!store.cas_ref(&ns, "main", stale, another).unwrap());
        assert_eq!(store.get_ref(&ns, "main").unwrap(), Some(new_target)); // unchanged
    }

    #[test]
    fn test_list_refs_with_prefix() {
        let store = MemoryStorage::new();
        let ns = Namespace::default_ns();

        store
            .set_ref(&ns, "agents/planner/workspace", ObjectId::hash(b"a"))
            .unwrap();
        store
            .set_ref(&ns, "agents/storage/workspace", ObjectId::hash(b"b"))
            .unwrap();
        store.set_ref(&ns, "main", ObjectId::hash(b"c")).unwrap();

        let agent_refs = store.list_refs(&ns, "agents/").unwrap();
        assert_eq!(agent_refs.len(), 2);

        let all_refs = store.list_refs(&ns, "").unwrap();
        assert_eq!(all_refs.len(), 3);
    }

    #[test]
    fn test_delete_ref() {
        let store = MemoryStorage::new();
        let ns = Namespace::default_ns();
        let target = ObjectId::hash(b"test");

        store.set_ref(&ns, "temp", target).unwrap();
        assert!(store.delete_ref(&ns, "temp").unwrap());
        assert_eq!(store.get_ref(&ns, "temp").unwrap(), None);
        assert!(!store.delete_ref(&ns, "temp").unwrap()); // already deleted
    }

    #[test]
    fn test_namespace_isolation() {
        let store = MemoryStorage::new();
        let ns_a = Namespace::default_ns();
        let ns_b = Namespace::new("project-b").unwrap();
        store.create_namespace(&ns_b).unwrap();

        let id_a = ObjectId::hash(b"a");
        let id_b = ObjectId::hash(b"b");

        store.set_ref(&ns_a, "main", id_a).unwrap();
        store.set_ref(&ns_b, "main", id_b).unwrap();

        assert_eq!(store.get_ref(&ns_a, "main").unwrap(), Some(id_a));
        assert_eq!(store.get_ref(&ns_b, "main").unwrap(), Some(id_b));

        let list_a = store.list_refs(&ns_a, "").unwrap();
        let list_b = store.list_refs(&ns_b, "").unwrap();
        assert_eq!(list_a.len(), 1);
        assert_eq!(list_b.len(), 1);
        assert_eq!(list_a[0].0, "main");
        assert_eq!(list_b[0].0, "main");
    }

    #[test]
    fn test_namespace_not_found() {
        let store = MemoryStorage::new();
        let unknown = Namespace::new("unknown-ns").unwrap();
        let target = ObjectId::hash(b"x");

        let err = store.set_ref(&unknown, "main", target).unwrap_err();
        assert!(
            matches!(err, StorageError::NamespaceNotFound(_)),
            "expected NamespaceNotFound, got {err:?}"
        );
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

    // --- Object edge cases ---

    #[test]
    fn test_get_nonexistent_object_returns_none() {
        let store = MemoryStorage::new();
        let missing = ObjectId::hash(b"does-not-exist");
        assert_eq!(store.get_object(&missing).unwrap(), None);
    }

    #[test]
    fn test_has_object() {
        let store = MemoryStorage::new();
        let obj = Object::int(99);
        let id = store.put_object(&obj).unwrap();
        assert!(store.has_object(&id).unwrap());
        assert!(!store.has_object(&ObjectId::hash(b"missing")).unwrap());
    }

    // --- Commit edge cases ---

    #[test]
    fn test_get_nonexistent_commit_returns_none() {
        let store = MemoryStorage::new();
        let missing = ObjectId::hash(b"no-commit");
        assert_eq!(store.get_commit(&missing).unwrap(), None);
    }

    #[test]
    fn test_has_commit() {
        let store = MemoryStorage::new();
        let commit = CommitBuilder::new(
            ObjectId::hash(b"s"),
            "a",
            Authority::simple("a"),
            Intent::new(IntentCategory::Refine, "x"),
        )
        .build();
        store.put_commit(&commit).unwrap();
        assert!(store.has_commit(&commit.id).unwrap());
        assert!(!store.has_commit(&ObjectId::hash(b"missing")).unwrap());
    }

    #[test]
    fn test_list_commits_follows_parent_chain() {
        let store = MemoryStorage::new();

        let c1 = CommitBuilder::new(
            ObjectId::hash(b"s1"),
            "a",
            Authority::simple("a"),
            Intent::new(IntentCategory::Checkpoint, "first"),
        )
        .build();
        store.put_commit(&c1).unwrap();

        let c2 = CommitBuilder::new(
            ObjectId::hash(b"s2"),
            "a",
            Authority::simple("a"),
            Intent::new(IntentCategory::Refine, "second"),
        )
        .parent(c1.id)
        .build();
        store.put_commit(&c2).unwrap();

        let c3 = CommitBuilder::new(
            ObjectId::hash(b"s3"),
            "a",
            Authority::simple("a"),
            Intent::new(IntentCategory::Refine, "third"),
        )
        .parent(c2.id)
        .build();
        store.put_commit(&c3).unwrap();

        let log = store.list_commits(&c3.id, 10).unwrap();
        assert_eq!(log.len(), 3);
        assert_eq!(log[0].id, c3.id);
        assert_eq!(log[1].id, c2.id);
        assert_eq!(log[2].id, c1.id);
    }

    #[test]
    fn test_list_commits_respects_limit() {
        let store = MemoryStorage::new();

        let c1 = CommitBuilder::new(
            ObjectId::hash(b"l1"),
            "a",
            Authority::simple("a"),
            Intent::new(IntentCategory::Checkpoint, "1"),
        )
        .build();
        store.put_commit(&c1).unwrap();

        let c2 = CommitBuilder::new(
            ObjectId::hash(b"l2"),
            "a",
            Authority::simple("a"),
            Intent::new(IntentCategory::Refine, "2"),
        )
        .parent(c1.id)
        .build();
        store.put_commit(&c2).unwrap();

        let log = store.list_commits(&c2.id, 1).unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].id, c2.id);
    }

    // --- EpochStore ---

    fn test_epoch(id: &str) -> Epoch {
        Epoch {
            id: id.to_string(),
            description: "test epoch".into(),
            root_intents: Vec::new(),
            status: EpochStatus::Active,
            created_at: Utc::now(),
            sealed_at: None,
            seal_summary: None,
            seal_hash: None,
            commits: Vec::new(),
            agents: Vec::new(),
            branches: Vec::new(),
            tags: Vec::new(),
            sealed_commits: Vec::new(),
        }
    }

    #[test]
    fn test_epoch_create_get_list() {
        let store = MemoryStorage::new();
        let e = test_epoch("epoch-1");
        store.create_epoch(&e).unwrap();

        let got = store.get_epoch("epoch-1").unwrap();
        assert!(got.is_some());
        assert_eq!(got.unwrap().id, "epoch-1");

        let list = store.list_epochs().unwrap();
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn test_epoch_duplicate_is_error() {
        let store = MemoryStorage::new();
        let e = test_epoch("e1");
        store.create_epoch(&e).unwrap();
        assert!(store.create_epoch(&e).is_err());
    }

    #[test]
    fn test_epoch_seal() {
        let store = MemoryStorage::new();
        store.create_epoch(&test_epoch("e1")).unwrap();
        let cid = ObjectId::hash(b"c1");
        store
            .seal_epoch("e1", "sealed for testing", Utc::now(), &[cid])
            .unwrap();

        let epoch = store.get_epoch("e1").unwrap().unwrap();
        assert_eq!(epoch.status, EpochStatus::Sealed);
        assert!(epoch.seal_summary.as_deref() == Some("sealed for testing"));
        assert_eq!(epoch.sealed_commits, vec![cid]);
    }

    #[test]
    fn test_epoch_seal_twice_is_error() {
        let store = MemoryStorage::new();
        store.create_epoch(&test_epoch("e1")).unwrap();
        store
            .seal_epoch("e1", "first seal", Utc::now(), &[])
            .unwrap();
        assert!(
            store
                .seal_epoch("e1", "second seal", Utc::now(), &[])
                .is_err(),
            "sealing an already-sealed epoch must fail"
        );
    }

    #[test]
    fn test_set_commit_epoch_rejects_sealed() {
        let store = MemoryStorage::new();
        store.create_epoch(&test_epoch("e1")).unwrap();
        store.seal_epoch("e1", "done", Utc::now(), &[]).unwrap();

        let cid = ObjectId::hash(b"late-commit");
        assert!(
            store.set_commit_epoch(&cid, "e1").is_err(),
            "assigning a commit to a sealed epoch must fail"
        );
    }

    // --- SessionStore ---

    fn test_session(id: &str, agent: &str) -> Session {
        Session {
            id: id.to_string(),
            agent_id: agent.to_string(),
            working_branch: "main".into(),
            head: ObjectId::hash(id.as_bytes()),
            parent_session: None,
            delegated_intent: None,
            report_to: None,
            path_scope: None,
            scope_tenant: None,
            scope_namespace: None,
            status: SessionStatus::Active,
            created_at: Utc::now(),
            ended_at: None,
        }
    }

    #[test]
    fn test_session_create_get() {
        let store = MemoryStorage::new();
        store
            .create_session(&test_session("s1", "agent/a"))
            .unwrap();

        let got = store.get_session("s1").unwrap();
        assert!(got.is_some());
        assert_eq!(got.unwrap().agent_id, "agent/a");
    }

    #[test]
    fn test_session_get_nonexistent() {
        let store = MemoryStorage::new();
        assert!(store.get_session("no-such-session").unwrap().is_none());
    }

    #[test]
    fn test_session_end() {
        let store = MemoryStorage::new();
        store
            .create_session(&test_session("s1", "agent/a"))
            .unwrap();

        store
            .end_session("s1", SessionStatus::Completed, Utc::now())
            .unwrap();

        let got = store.get_session("s1").unwrap().unwrap();
        assert_eq!(got.status, SessionStatus::Completed);
        assert!(got.ended_at.is_some());
    }

    #[test]
    fn test_session_end_twice_is_error() {
        let store = MemoryStorage::new();
        store.create_session(&test_session("s1", "a")).unwrap();
        store
            .end_session("s1", SessionStatus::Completed, Utc::now())
            .unwrap();
        assert!(
            store
                .end_session("s1", SessionStatus::Abandoned, Utc::now())
                .is_err(),
            "ending an already-ended session must fail"
        );
    }

    #[test]
    fn test_session_list_with_agent_filter() {
        let store = MemoryStorage::new();
        store
            .create_session(&test_session("s1", "agent/alpha"))
            .unwrap();
        store
            .create_session(&test_session("s2", "agent/beta"))
            .unwrap();
        store
            .create_session(&test_session("s3", "agent/alpha"))
            .unwrap();

        let alpha = store.list_sessions(Some("agent/alpha")).unwrap();
        assert_eq!(alpha.len(), 2);

        let all = store.list_sessions(None).unwrap();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn test_set_commit_session_rejects_ended_session() {
        let store = MemoryStorage::new();
        store.create_session(&test_session("s1", "a")).unwrap();
        store
            .end_session("s1", SessionStatus::Completed, Utc::now())
            .unwrap();

        let cid = ObjectId::hash(b"late");
        assert!(
            store.set_commit_session(&cid, "s1").is_err(),
            "associating a commit with an ended session must fail"
        );
    }

    // --- TaintStore ---

    fn test_taint(id: &str, path: &str) -> Taint {
        use agentstategraph_taint::{TaintEffect, TaintMetadata, TaintSeverity};
        Taint {
            id: id.to_string(),
            path: path.to_string(),
            name: "test-taint".into(),
            kind: TaintKind::Taint,
            effect: TaintEffect::Warn,
            severity: TaintSeverity::Medium,
            reason: "test".into(),
            agent_id: "agent/test".into(),
            commit_id: String::new(),
            created_at: Utc::now(),
            expires_at: None,
            resolved_at: None,
            resolved_by: None,
            resolved_reason: None,
            resolved_proof: None,
            propagate: true,
            metadata: TaintMetadata::new(),
        }
    }

    #[test]
    fn test_taint_create_get() {
        let store = MemoryStorage::new();
        store
            .create_taint(&test_taint("t1", "/nodes/pico1"))
            .unwrap();

        let got = store.get_taint("t1").unwrap();
        assert!(got.is_some());
        assert_eq!(got.unwrap().path, "/nodes/pico1");
    }

    #[test]
    fn test_taint_get_nonexistent() {
        let store = MemoryStorage::new();
        assert_eq!(store.get_taint("no-taint").unwrap(), None);
    }

    #[test]
    fn test_taint_duplicate_active_is_error() {
        let store = MemoryStorage::new();
        store.create_taint(&test_taint("t1", "/x")).unwrap();
        // Same path+name+kind → error
        let mut t2 = test_taint("t2", "/x");
        t2.name = "test-taint".into(); // same name
        assert!(store.create_taint(&t2).is_err());
    }

    #[test]
    fn test_taint_resolve() {
        let store = MemoryStorage::new();
        store.create_taint(&test_taint("t1", "/x")).unwrap();
        store
            .resolve_taint("t1", "agent/ops", "fixed", Some("commit-abc"), Utc::now())
            .unwrap();

        let got = store.get_taint("t1").unwrap().unwrap();
        assert!(got.resolved_at.is_some());
        assert_eq!(got.resolved_by.as_deref(), Some("agent/ops"));
        assert_eq!(got.resolved_reason.as_deref(), Some("fixed"));
        assert_eq!(got.resolved_proof.as_deref(), Some("commit-abc"));
    }

    #[test]
    fn test_taint_resolve_twice_is_error() {
        let store = MemoryStorage::new();
        store.create_taint(&test_taint("t1", "/x")).unwrap();
        store
            .resolve_taint("t1", "a", "r", None, Utc::now())
            .unwrap();
        assert!(
            store
                .resolve_taint("t1", "a", "r2", None, Utc::now())
                .is_err()
        );
    }

    #[test]
    fn test_taint_list_excludes_resolved_by_default() {
        let store = MemoryStorage::new();
        store.create_taint(&test_taint("t1", "/x")).unwrap();
        store.create_taint(&test_taint("t2", "/y")).unwrap();
        store
            .resolve_taint("t2", "a", "done", None, Utc::now())
            .unwrap();

        let active = store.list_taints(None, None, false).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, "t1");

        let all = store.list_taints(None, None, true).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn test_taint_list_path_prefix_filter() {
        let store = MemoryStorage::new();
        store
            .create_taint(&test_taint("t1", "/nodes/pico1"))
            .unwrap();
        store
            .create_taint(&test_taint("t2", "/nodes/pico2"))
            .unwrap();
        store.create_taint(&test_taint("t3", "/config")).unwrap();

        let nodes = store.list_taints(Some("/nodes"), None, false).unwrap();
        assert_eq!(nodes.len(), 2);

        let pico1 = store
            .list_taints(Some("/nodes/pico1"), None, false)
            .unwrap();
        assert_eq!(pico1.len(), 1);
    }

    #[test]
    fn test_taint_check_propagation() {
        let store = MemoryStorage::new();
        // propagate=true — should match child paths
        store.create_taint(&test_taint("t1", "/nodes")).unwrap();
        // propagate=false — should only match exact path
        let mut t2 = test_taint("t2", "/config");
        t2.name = "no-propagate".to_string();
        t2.propagate = false;
        store.create_taint(&t2).unwrap();

        let child_hits = store.check_taint("/nodes/pico1").unwrap();
        assert_eq!(child_hits.len(), 1, "propagating taint should match child");

        let exact_hit = store.check_taint("/config").unwrap();
        assert_eq!(exact_hit.len(), 1, "exact match always fires");

        let child_no_prop = store.check_taint("/config/network").unwrap();
        assert!(
            child_no_prop.is_empty(),
            "non-propagating should not match child"
        );
    }

    #[test]
    fn test_taint_set_commit_id() {
        let store = MemoryStorage::new();
        store.create_taint(&test_taint("t1", "/x")).unwrap();
        store.set_taint_commit_id("t1", "commit-777").unwrap();

        let got = store.get_taint("t1").unwrap().unwrap();
        assert_eq!(got.commit_id, "commit-777");
    }
}
