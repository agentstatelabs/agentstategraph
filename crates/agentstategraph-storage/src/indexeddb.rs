//! IndexedDB storage backend for WASM — browser-native persistent storage.
//!
//! Uses the browser's IndexedDB to store objects, commits, and refs.
//! Data survives page refreshes and browser restarts.
//!
//! As of 0.6.75-beta.1 this backend also persists epochs and sessions,
//! using the same write-through-queue pattern as objects/commits/refs.
//!
//! Seven IndexedDB object stores:
//!   "objects"         → ObjectId (hex string) → Object (JSON)
//!   "commits"         → ObjectId (hex string) → Commit (JSON)
//!   "refs"            → name (string)         → ObjectId (hex string)
//!   "epochs"          → epoch id (string)     → Epoch (JSON snapshot)
//!   "sessions"        → session id (string)   → Session (JSON snapshot)
//!   "commit_epochs"   → commit hex id         → epoch id (string or null)
//!   "commit_sessions" → commit hex id         → session id (string or null)
//!
//! Migration: adding the four new stores is an onupgradeneeded version
//! bump on the JS side (from v1 → v2). Existing objects/commits/refs
//! records are untouched.
//!
//! Note: IndexedDB is async but our storage traits are sync.
//! We use a write-through in-memory cache backed by IndexedDB:
//! - All reads come from the in-memory cache (fast, sync)
//! - All writes go to both memory and IndexedDB (durability)
//! - On construction, the full store is loaded from IndexedDB into memory

use std::sync::RwLock;

use crate::memory::MemoryStorage;
use crate::traits::{
    CommitStore, EpochStore, ObjectStore, RefStore, SessionStore, StorageError, TaintStore,
};
use agentstategraph_core::{Commit, Epoch, Object, ObjectId, Session, SessionStatus};
use agentstategraph_reminders::ReminderStore;
use chrono::{DateTime, Utc};

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
    pending_epochs: RwLock<Vec<(String, String)>>,  // (epoch_id, epoch_json snapshot)
    pending_sessions: RwLock<Vec<(String, String)>>, // (session_id, session_json snapshot)
    pending_commit_epochs: RwLock<Vec<(String, String)>>, // (commit_hex_id, epoch_id)
    pending_commit_sessions: RwLock<Vec<(String, String)>>, // (commit_hex_id, session_id)
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
            pending_epochs: RwLock::new(Vec::new()),
            pending_sessions: RwLock::new(Vec::new()),
            pending_commit_epochs: RwLock::new(Vec::new()),
            pending_commit_sessions: RwLock::new(Vec::new()),
        }
    }

    /// Load objects from a JSON dump (called from JS after reading IndexedDB).
    pub fn load_objects(&self, json_pairs: &[(String, String)]) -> Result<(), StorageError> {
        for (_hex_id, json) in json_pairs {
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

    /// Load epochs from a JSON dump (called from JS after reading IndexedDB).
    pub fn load_epochs(&self, json_pairs: &[(String, String)]) -> Result<(), StorageError> {
        for (_id, json) in json_pairs {
            let epoch: Epoch = serde_json::from_str(json)
                .map_err(|e| StorageError::Serialization(format!("epoch load: {}", e)))?;
            self.memory.create_epoch(&epoch)?;
        }
        Ok(())
    }

    /// Load sessions from a JSON dump.
    pub fn load_sessions(&self, json_pairs: &[(String, String)]) -> Result<(), StorageError> {
        for (_id, json) in json_pairs {
            let session: Session = serde_json::from_str(json)
                .map_err(|e| StorageError::Serialization(format!("session load: {}", e)))?;
            self.memory.create_session(&session)?;
        }
        Ok(())
    }

    /// Apply commit→epoch associations loaded from IndexedDB.
    pub fn load_commit_epochs(&self, pairs: &[(String, String)]) -> Result<(), StorageError> {
        for (commit_hex, epoch_id) in pairs {
            let bytes = hex_to_bytes(commit_hex)
                .ok_or_else(|| StorageError::Serialization("invalid commit hex".to_string()))?;
            if bytes.len() != 32 {
                return Err(StorageError::Serialization("id must be 32 bytes".into()));
            }
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&bytes);
            let id = ObjectId::from_bytes(arr);
            self.memory.set_commit_epoch(&id, epoch_id)?;
        }
        Ok(())
    }

    /// Apply commit→session associations loaded from IndexedDB.
    pub fn load_commit_sessions(&self, pairs: &[(String, String)]) -> Result<(), StorageError> {
        for (commit_hex, session_id) in pairs {
            let bytes = hex_to_bytes(commit_hex)
                .ok_or_else(|| StorageError::Serialization("invalid commit hex".to_string()))?;
            if bytes.len() != 32 {
                return Err(StorageError::Serialization("id must be 32 bytes".into()));
            }
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&bytes);
            let id = ObjectId::from_bytes(arr);
            self.memory.set_commit_session(&id, session_id)?;
        }
        Ok(())
    }

    /// Drain pending epoch snapshots (for flushing to IndexedDB from JS).
    pub fn drain_pending_epochs(&self) -> Vec<(String, String)> {
        let mut pending = self.pending_epochs.write().unwrap();
        std::mem::take(&mut *pending)
    }

    /// Drain pending session snapshots.
    pub fn drain_pending_sessions(&self) -> Vec<(String, String)> {
        let mut pending = self.pending_sessions.write().unwrap();
        std::mem::take(&mut *pending)
    }

    /// Drain pending commit→epoch associations.
    pub fn drain_pending_commit_epochs(&self) -> Vec<(String, String)> {
        let mut pending = self.pending_commit_epochs.write().unwrap();
        std::mem::take(&mut *pending)
    }

    /// Drain pending commit→session associations.
    pub fn drain_pending_commit_sessions(&self) -> Vec<(String, String)> {
        let mut pending = self.pending_commit_sessions.write().unwrap();
        std::mem::take(&mut *pending)
    }

    /// Get the database name.
    pub fn db_name(&self) -> &str {
        &self.db_name
    }

    /// Internal: queue the current snapshot of an epoch for flush.
    fn queue_epoch_snapshot(&self, id: &str) -> Result<(), StorageError> {
        if let Some(epoch) = self.memory.get_epoch(id)? {
            let json = serde_json::to_string(&epoch)
                .map_err(|e| StorageError::Serialization(e.to_string()))?;
            self.pending_epochs
                .write()
                .unwrap()
                .push((id.to_string(), json));
        }
        Ok(())
    }

    /// Internal: queue the current snapshot of a session for flush.
    fn queue_session_snapshot(&self, id: &str) -> Result<(), StorageError> {
        if let Some(session) = self.memory.get_session(id)? {
            let json = serde_json::to_string(&session)
                .map_err(|e| StorageError::Serialization(e.to_string()))?;
            self.pending_sessions
                .write()
                .unwrap()
                .push((id.to_string(), json));
        }
        Ok(())
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
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect()
}

// ---------------------------------------------------------------------------
// EpochStore — write-through to the memory cache; queue JSON snapshots
// for the JS-side IndexedDB flush.
// ---------------------------------------------------------------------------

impl EpochStore for IndexedDbStorage {
    fn create_epoch(&self, epoch: &Epoch) -> Result<(), StorageError> {
        self.memory.create_epoch(epoch)?;
        self.queue_epoch_snapshot(&epoch.id)?;
        Ok(())
    }

    fn seal_epoch(
        &self,
        id: &str,
        summary: &str,
        sealed_at: DateTime<Utc>,
        sealed_commits: &[ObjectId],
    ) -> Result<(), StorageError> {
        self.memory
            .seal_epoch(id, summary, sealed_at, sealed_commits)?;
        self.queue_epoch_snapshot(id)?;
        Ok(())
    }

    fn list_epochs(&self) -> Result<Vec<Epoch>, StorageError> {
        self.memory.list_epochs()
    }

    fn get_epoch(&self, id: &str) -> Result<Option<Epoch>, StorageError> {
        self.memory.get_epoch(id)
    }

    fn set_commit_epoch(&self, commit_id: &ObjectId, epoch_id: &str) -> Result<(), StorageError> {
        self.memory.set_commit_epoch(commit_id, epoch_id)?;
        let hex = format!("{}", commit_id);
        self.pending_commit_epochs
            .write()
            .unwrap()
            .push((hex, epoch_id.to_string()));
        // commit_count changed on the epoch — re-snapshot for flush.
        self.queue_epoch_snapshot(epoch_id)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// SessionStore — same pattern.
// ---------------------------------------------------------------------------

impl SessionStore for IndexedDbStorage {
    fn create_session(&self, session: &Session) -> Result<(), StorageError> {
        self.memory.create_session(session)?;
        self.queue_session_snapshot(&session.id)?;
        Ok(())
    }

    fn end_session(
        &self,
        id: &str,
        status: SessionStatus,
        ended_at: DateTime<Utc>,
    ) -> Result<(), StorageError> {
        self.memory.end_session(id, status, ended_at)?;
        self.queue_session_snapshot(id)?;
        Ok(())
    }

    fn list_sessions(&self, agent_filter: Option<&str>) -> Result<Vec<Session>, StorageError> {
        self.memory.list_sessions(agent_filter)
    }

    fn get_session(&self, id: &str) -> Result<Option<Session>, StorageError> {
        self.memory.get_session(id)
    }

    fn set_commit_session(
        &self,
        commit_id: &ObjectId,
        session_id: &str,
    ) -> Result<(), StorageError> {
        self.memory.set_commit_session(commit_id, session_id)?;
        let hex = format!("{}", commit_id);
        self.pending_commit_sessions
            .write()
            .unwrap()
            .push((hex, session_id.to_string()));
        // commit_count changed on the session — re-snapshot for flush.
        self.queue_session_snapshot(session_id)?;
        Ok(())
    }
}

/// TaintStore impl delegates to the inner MemoryStorage. IndexedDB
/// persistence of taints lands in a later milestone; for 0.7.75 the
/// browser runtime gets in-session taints only (matching the
/// pattern other sub-stores used before their persistence path was
/// finalized — see the `queue_*_snapshot` helpers above).
impl TaintStore for IndexedDbStorage {
    fn create_taint(&self, taint: &agentstategraph_taint::Taint) -> Result<(), StorageError> {
        self.memory.create_taint(taint)
    }

    fn resolve_taint(
        &self,
        id: &str,
        resolved_by: &str,
        reason: &str,
        proof: Option<&str>,
        resolved_at: DateTime<Utc>,
    ) -> Result<(), StorageError> {
        self.memory
            .resolve_taint(id, resolved_by, reason, proof, resolved_at)
    }

    fn list_taints(
        &self,
        path_prefix: Option<&str>,
        kind: Option<agentstategraph_taint::TaintKind>,
        include_resolved: bool,
    ) -> Result<Vec<agentstategraph_taint::Taint>, StorageError> {
        self.memory.list_taints(path_prefix, kind, include_resolved)
    }

    fn check_taint(
        &self,
        request_path: &str,
    ) -> Result<Vec<agentstategraph_taint::Taint>, StorageError> {
        self.memory.check_taint(request_path)
    }

    fn get_taint(&self, id: &str) -> Result<Option<agentstategraph_taint::Taint>, StorageError> {
        self.memory.get_taint(id)
    }

    fn set_taint_commit_id(&self, id: &str, commit_id: &str) -> Result<(), StorageError> {
        self.memory.set_taint_commit_id(id, commit_id)
    }
}

/// IndexedDB delegates reminders to the in-memory store it already wraps.
impl ReminderStore for IndexedDbStorage {
    fn save(&self, reminder: &agentstategraph_reminders::Reminder) -> Result<(), agentstategraph_reminders::ReminderError> {
        self.memory.reminders.save(reminder)
    }
    fn get(&self, id: &str) -> Result<Option<agentstategraph_reminders::Reminder>, agentstategraph_reminders::ReminderError> {
        self.memory.reminders.get(id)
    }
    fn update(&self, reminder: &agentstategraph_reminders::Reminder) -> Result<(), agentstategraph_reminders::ReminderError> {
        self.memory.reminders.update(reminder)
    }
    fn delete(&self, id: &str) -> Result<bool, agentstategraph_reminders::ReminderError> {
        self.memory.reminders.delete(id)
    }
    fn list(&self, filter: &agentstategraph_reminders::ReminderFilter) -> Result<Vec<agentstategraph_reminders::Reminder>, agentstategraph_reminders::ReminderError> {
        self.memory.reminders.list(filter)
    }
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

    #[test]
    fn test_epoch_create_and_queue() {
        let store = IndexedDbStorage::new("test-db");
        let epoch = Epoch::new("e1", "first", vec![]);
        store.create_epoch(&epoch).unwrap();

        let pending = store.drain_pending_epochs();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].0, "e1");
        // Snapshot round-trips cleanly
        let parsed: Epoch = serde_json::from_str(&pending[0].1).unwrap();
        assert_eq!(parsed.id, "e1");

        // Memory reads work immediately
        assert!(store.get_epoch("e1").unwrap().is_some());
        assert_eq!(store.list_epochs().unwrap().len(), 1);
    }

    #[test]
    fn test_epoch_seal_re_snapshots() {
        let store = IndexedDbStorage::new("test-db");
        store.create_epoch(&Epoch::new("e1", "x", vec![])).unwrap();
        store.drain_pending_epochs(); // clear the create snapshot

        store.seal_epoch("e1", "done", Utc::now(), &[]).unwrap();

        let pending = store.drain_pending_epochs();
        assert_eq!(pending.len(), 1, "seal must re-snapshot");
        let sealed: Epoch = serde_json::from_str(&pending[0].1).unwrap();
        assert_eq!(sealed.status, EpochStatus::Sealed);
        assert!(sealed.sealed_at.is_some());
    }

    #[test]
    fn test_set_commit_epoch_queues_association() {
        let store = IndexedDbStorage::new("test-db");
        store.create_epoch(&Epoch::new("e1", "x", vec![])).unwrap();
        store.drain_pending_epochs();

        let cid = ObjectId::hash(b"c1");
        store.set_commit_epoch(&cid, "e1").unwrap();

        let assocs = store.drain_pending_commit_epochs();
        assert_eq!(assocs.len(), 1);
        assert_eq!(assocs[0].1, "e1");

        // Epoch is re-snapshotted because commit_count changed
        let epoch_pending = store.drain_pending_epochs();
        assert_eq!(epoch_pending.len(), 1);
    }

    #[test]
    fn test_session_create_end_and_queue() {
        let store = IndexedDbStorage::new("test-db");
        let session = Session {
            id: "s1".to_string(),
            agent_id: "agent/x".to_string(),
            working_branch: "main".to_string(),
            head: ObjectId::hash(b"head"),
            parent_session: None,
            delegated_intent: None,
            report_to: None,
            path_scope: None,
            scope_tenant: None,
            status: SessionStatus::Active,
            created_at: Utc::now(),
            ended_at: None,
        };
        store.create_session(&session).unwrap();

        assert_eq!(store.drain_pending_sessions().len(), 1);

        store
            .end_session("s1", SessionStatus::Completed, Utc::now())
            .unwrap();

        let after_end = store.drain_pending_sessions();
        assert_eq!(after_end.len(), 1);
        let snap: Session = serde_json::from_str(&after_end[0].1).unwrap();
        assert_eq!(snap.status, SessionStatus::Completed);
    }

    #[test]
    fn test_set_commit_session_queues_association() {
        let store = IndexedDbStorage::new("test-db");
        let session = Session {
            id: "s1".to_string(),
            agent_id: "agent/x".to_string(),
            working_branch: "main".to_string(),
            head: ObjectId::hash(b"head"),
            parent_session: None,
            delegated_intent: None,
            report_to: None,
            path_scope: None,
            scope_tenant: None,
            status: SessionStatus::Active,
            created_at: Utc::now(),
            ended_at: None,
        };
        store.create_session(&session).unwrap();
        store.drain_pending_sessions();

        let cid = ObjectId::hash(b"c1");
        store.set_commit_session(&cid, "s1").unwrap();

        let assocs = store.drain_pending_commit_sessions();
        assert_eq!(assocs.len(), 1);
        assert_eq!(assocs[0].1, "s1");
    }

    #[test]
    fn test_load_epoch_round_trip() {
        let store = IndexedDbStorage::new("test-db");
        let epoch = Epoch::new("e1", "loaded", vec![]);
        let json = serde_json::to_string(&epoch).unwrap();

        store.load_epochs(&[("e1".to_string(), json)]).unwrap();

        let got = store.get_epoch("e1").unwrap().unwrap();
        assert_eq!(got.id, "e1");
        assert_eq!(got.description, "loaded");
    }

    #[test]
    fn test_load_commit_epoch_association() {
        let store = IndexedDbStorage::new("test-db");
        store.create_epoch(&Epoch::new("e1", "x", vec![])).unwrap();

        let cid = ObjectId::hash(b"c-load");
        let hex = format!("{}", cid);
        store
            .load_commit_epochs(&[(hex, "e1".to_string())])
            .unwrap();

        // Association must be applied through memory — not observable as
        // a pending snapshot because it was a load, not a new write.
        let assocs = store.drain_pending_commit_epochs();
        assert!(assocs.is_empty());
    }
}
