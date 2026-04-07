//! SQLite storage backend — durable, single-file, zero-config.
//!
//! This is the default production backend. All state, commits, and refs
//! are stored in a single SQLite file that survives process restarts.

use std::path::Path;
use std::sync::Mutex;

use agentstategraph_core::{Commit, Object, ObjectId};
use rusqlite::{Connection, OptionalExtension, params};

use crate::traits::{CommitStore, ObjectStore, RefStore, StorageError};

/// SQLite-backed storage. Thread-safe via Mutex around the connection.
///
/// Creates the database file and tables automatically on first use.
pub struct SqliteStorage {
    conn: Mutex<Connection>,
}

impl SqliteStorage {
    /// Open or create a SQLite database at the given path.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let conn = Connection::open(path)
            .map_err(|e| StorageError::Backend(format!("sqlite open: {}", e)))?;
        let storage = Self {
            conn: Mutex::new(conn),
        };
        storage.init_tables()?;
        Ok(storage)
    }

    /// Create an in-memory SQLite database (useful for testing).
    pub fn in_memory() -> Result<Self, StorageError> {
        let conn = Connection::open_in_memory()
            .map_err(|e| StorageError::Backend(format!("sqlite open: {}", e)))?;
        let storage = Self {
            conn: Mutex::new(conn),
        };
        storage.init_tables()?;
        Ok(storage)
    }

    fn init_tables(&self) -> Result<(), StorageError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Backend(e.to_string()))?;

        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS objects (
                id   BLOB PRIMARY KEY,
                data BLOB NOT NULL
            );

            CREATE TABLE IF NOT EXISTS commits (
                id        BLOB PRIMARY KEY,
                data      BLOB NOT NULL,
                timestamp TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS refs (
                name   TEXT PRIMARY KEY,
                target BLOB NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_commits_timestamp ON commits(timestamp DESC);
            ",
        )
        .map_err(|e| StorageError::Backend(format!("init tables: {}", e)))?;

        Ok(())
    }

    fn lock_conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>, StorageError> {
        self.conn
            .lock()
            .map_err(|e| StorageError::Backend(e.to_string()))
    }
}

impl ObjectStore for SqliteStorage {
    fn get_object(&self, id: &ObjectId) -> Result<Option<Object>, StorageError> {
        let conn = self.lock_conn()?;
        let result: Option<Vec<u8>> = conn
            .query_row(
                "SELECT data FROM objects WHERE id = ?1",
                params![id.as_bytes().as_slice()],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| StorageError::Backend(format!("get object: {}", e)))?;

        match result {
            Some(data) => {
                let obj: Object = serde_json::from_slice(&data)
                    .map_err(|e| StorageError::Serialization(e.to_string()))?;
                Ok(Some(obj))
            }
            None => Ok(None),
        }
    }

    fn put_object(&self, obj: &Object) -> Result<ObjectId, StorageError> {
        let id = obj.id();
        let data =
            serde_json::to_vec(obj).map_err(|e| StorageError::Serialization(e.to_string()))?;

        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT OR IGNORE INTO objects (id, data) VALUES (?1, ?2)",
            params![id.as_bytes().as_slice(), data],
        )
        .map_err(|e| StorageError::Backend(format!("put object: {}", e)))?;

        Ok(id)
    }

    fn has_object(&self, id: &ObjectId) -> Result<bool, StorageError> {
        let conn = self.lock_conn()?;
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM objects WHERE id = ?1)",
                params![id.as_bytes().as_slice()],
                |row| row.get(0),
            )
            .map_err(|e| StorageError::Backend(format!("has object: {}", e)))?;
        Ok(exists)
    }

    fn batch_put_objects(&self, objs: &[Object]) -> Result<Vec<ObjectId>, StorageError> {
        let conn = self.lock_conn()?;
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| StorageError::Backend(format!("begin tx: {}", e)))?;

        let mut ids = Vec::with_capacity(objs.len());
        {
            let mut stmt = tx
                .prepare_cached("INSERT OR IGNORE INTO objects (id, data) VALUES (?1, ?2)")
                .map_err(|e| StorageError::Backend(format!("prepare: {}", e)))?;

            for obj in objs {
                let id = obj.id();
                let data = serde_json::to_vec(obj)
                    .map_err(|e| StorageError::Serialization(e.to_string()))?;
                stmt.execute(params![id.as_bytes().as_slice(), data])
                    .map_err(|e| StorageError::Backend(format!("batch put: {}", e)))?;
                ids.push(id);
            }
        }

        tx.commit()
            .map_err(|e| StorageError::Backend(format!("commit tx: {}", e)))?;

        Ok(ids)
    }
}

impl CommitStore for SqliteStorage {
    fn get_commit(&self, id: &ObjectId) -> Result<Option<Commit>, StorageError> {
        let conn = self.lock_conn()?;
        let result: Option<Vec<u8>> = conn
            .query_row(
                "SELECT data FROM commits WHERE id = ?1",
                params![id.as_bytes().as_slice()],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| StorageError::Backend(format!("get commit: {}", e)))?;

        match result {
            Some(data) => {
                let commit: Commit = serde_json::from_slice(&data)
                    .map_err(|e| StorageError::Serialization(e.to_string()))?;
                Ok(Some(commit))
            }
            None => Ok(None),
        }
    }

    fn put_commit(&self, commit: &Commit) -> Result<(), StorageError> {
        let data =
            serde_json::to_vec(commit).map_err(|e| StorageError::Serialization(e.to_string()))?;
        let timestamp = commit.timestamp.to_rfc3339();

        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT OR IGNORE INTO commits (id, data, timestamp) VALUES (?1, ?2, ?3)",
            params![commit.id.as_bytes().as_slice(), data, timestamp],
        )
        .map_err(|e| StorageError::Backend(format!("put commit: {}", e)))?;

        Ok(())
    }

    fn has_commit(&self, id: &ObjectId) -> Result<bool, StorageError> {
        let conn = self.lock_conn()?;
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM commits WHERE id = ?1)",
                params![id.as_bytes().as_slice()],
                |row| row.get(0),
            )
            .map_err(|e| StorageError::Backend(format!("has commit: {}", e)))?;
        Ok(exists)
    }

    fn list_commits(&self, from: &ObjectId, limit: usize) -> Result<Vec<Commit>, StorageError> {
        // Walk the parent chain from the given commit
        let conn = self.lock_conn()?;
        let mut result = Vec::new();
        let mut current = Some(*from);

        while let Some(id) = current {
            if result.len() >= limit {
                break;
            }

            let data: Option<Vec<u8>> = conn
                .query_row(
                    "SELECT data FROM commits WHERE id = ?1",
                    params![id.as_bytes().as_slice()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| StorageError::Backend(format!("list commits: {}", e)))?;

            match data {
                Some(data) => {
                    let commit: Commit = serde_json::from_slice(&data)
                        .map_err(|e| StorageError::Serialization(e.to_string()))?;
                    current = commit.parents.first().copied();
                    result.push(commit);
                }
                None => break,
            }
        }

        Ok(result)
    }
}

impl RefStore for SqliteStorage {
    fn get_ref(&self, name: &str) -> Result<Option<ObjectId>, StorageError> {
        let conn = self.lock_conn()?;
        let result: Option<Vec<u8>> = conn
            .query_row(
                "SELECT target FROM refs WHERE name = ?1",
                params![name],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| StorageError::Backend(format!("get ref: {}", e)))?;

        match result {
            Some(bytes) => {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&bytes);
                Ok(Some(ObjectId::from_bytes(arr)))
            }
            None => Ok(None),
        }
    }

    fn set_ref(&self, name: &str, target: ObjectId) -> Result<(), StorageError> {
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT OR REPLACE INTO refs (name, target) VALUES (?1, ?2)",
            params![name, target.as_bytes().as_slice()],
        )
        .map_err(|e| StorageError::Backend(format!("set ref: {}", e)))?;
        Ok(())
    }

    fn cas_ref(&self, name: &str, expected: ObjectId, new: ObjectId) -> Result<bool, StorageError> {
        let conn = self.lock_conn()?;

        // Use UPDATE with WHERE to make it atomic
        let rows = conn
            .execute(
                "UPDATE refs SET target = ?1 WHERE name = ?2 AND target = ?3",
                params![
                    new.as_bytes().as_slice(),
                    name,
                    expected.as_bytes().as_slice()
                ],
            )
            .map_err(|e| StorageError::Backend(format!("cas ref: {}", e)))?;

        Ok(rows > 0)
    }

    fn list_refs(&self, prefix: &str) -> Result<Vec<(String, ObjectId)>, StorageError> {
        let conn = self.lock_conn()?;
        let mut stmt = conn
            .prepare("SELECT name, target FROM refs WHERE name LIKE ?1 ORDER BY name")
            .map_err(|e| StorageError::Backend(format!("list refs: {}", e)))?;

        let pattern = format!("{}%", prefix);
        let rows = stmt
            .query_map(params![pattern], |row| {
                let name: String = row.get(0)?;
                let bytes: Vec<u8> = row.get(1)?;
                Ok((name, bytes))
            })
            .map_err(|e| StorageError::Backend(format!("list refs query: {}", e)))?;

        let mut result = Vec::new();
        for row in rows {
            let (name, bytes) =
                row.map_err(|e| StorageError::Backend(format!("list refs row: {}", e)))?;
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&bytes);
            result.push((name, ObjectId::from_bytes(arr)));
        }

        Ok(result)
    }

    fn delete_ref(&self, name: &str) -> Result<bool, StorageError> {
        let conn = self.lock_conn()?;
        let rows = conn
            .execute("DELETE FROM refs WHERE name = ?1", params![name])
            .map_err(|e| StorageError::Backend(format!("delete ref: {}", e)))?;
        Ok(rows > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentstategraph_core::*;

    fn test_store() -> SqliteStorage {
        SqliteStorage::in_memory().unwrap()
    }

    #[test]
    fn test_object_roundtrip() {
        let store = test_store();
        let obj = Object::string("hello sqlite");
        let id = store.put_object(&obj).unwrap();
        let retrieved = store.get_object(&id).unwrap();
        assert_eq!(retrieved, Some(obj));
    }

    #[test]
    fn test_object_dedup() {
        let store = test_store();
        let obj = Object::string("dedup");
        let id1 = store.put_object(&obj).unwrap();
        let id2 = store.put_object(&obj).unwrap();
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_commit_roundtrip() {
        let store = test_store();
        let commit = CommitBuilder::new(
            ObjectId::hash(b"state"),
            "agent/test",
            Authority::simple("test"),
            Intent::new(IntentCategory::Checkpoint, "test commit"),
        )
        .reasoning("testing sqlite backend")
        .confidence(0.9)
        .build();

        store.put_commit(&commit).unwrap();
        let retrieved = store.get_commit(&commit.id).unwrap().unwrap();
        assert_eq!(retrieved.agent_id, "agent/test");
        assert_eq!(
            retrieved.reasoning,
            Some("testing sqlite backend".to_string())
        );
        assert_eq!(retrieved.confidence, Some(0.9));
    }

    #[test]
    fn test_ref_operations() {
        let store = test_store();
        let target = ObjectId::hash(b"commit-1");
        let new_target = ObjectId::hash(b"commit-2");

        store.set_ref("main", target).unwrap();
        assert_eq!(store.get_ref("main").unwrap(), Some(target));

        // CAS success
        assert!(store.cas_ref("main", target, new_target).unwrap());
        assert_eq!(store.get_ref("main").unwrap(), Some(new_target));

        // CAS failure
        let stale = ObjectId::hash(b"stale");
        assert!(!store.cas_ref("main", stale, target).unwrap());
    }

    #[test]
    fn test_list_refs() {
        let store = test_store();
        store.set_ref("agents/a", ObjectId::hash(b"a")).unwrap();
        store.set_ref("agents/b", ObjectId::hash(b"b")).unwrap();
        store.set_ref("main", ObjectId::hash(b"m")).unwrap();

        let agents = store.list_refs("agents/").unwrap();
        assert_eq!(agents.len(), 2);

        let all = store.list_refs("").unwrap();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn test_batch_put_objects() {
        let store = test_store();
        let objs = vec![
            Object::string("one"),
            Object::string("two"),
            Object::string("three"),
        ];
        let ids = store.batch_put_objects(&objs).unwrap();
        assert_eq!(ids.len(), 3);

        for id in ids.iter() {
            assert!(store.has_object(id).unwrap());
        }
    }

    #[test]
    fn test_commit_chain() {
        let store = test_store();

        let commit1 = CommitBuilder::new(
            ObjectId::hash(b"state-1"),
            "agent/test",
            Authority::simple("test"),
            Intent::new(IntentCategory::Checkpoint, "first"),
        )
        .build();
        store.put_commit(&commit1).unwrap();

        let commit2 = CommitBuilder::new(
            ObjectId::hash(b"state-2"),
            "agent/test",
            Authority::simple("test"),
            Intent::new(IntentCategory::Refine, "second"),
        )
        .parent(commit1.id)
        .build();
        store.put_commit(&commit2).unwrap();

        let commit3 = CommitBuilder::new(
            ObjectId::hash(b"state-3"),
            "agent/test",
            Authority::simple("test"),
            Intent::new(IntentCategory::Refine, "third"),
        )
        .parent(commit2.id)
        .build();
        store.put_commit(&commit3).unwrap();

        let log = store.list_commits(&commit3.id, 10).unwrap();
        assert_eq!(log.len(), 3);
        assert_eq!(log[0].intent.description, "third");
        assert_eq!(log[1].intent.description, "second");
        assert_eq!(log[2].intent.description, "first");
    }

    #[test]
    fn test_full_workflow_sqlite_storage_traits() {
        // Test that SqliteStorage works correctly through the trait interface
        let store = test_store();

        // Store objects
        let obj1 = Object::string("cluster-name");
        let id1 = store.put_object(&obj1).unwrap();

        // Store a commit referencing the object
        let commit = CommitBuilder::new(
            id1,
            "agent/test",
            Authority::simple("test"),
            Intent::new(IntentCategory::Checkpoint, "full workflow test"),
        )
        .build();
        store.put_commit(&commit).unwrap();

        // Set a ref
        store.set_ref("main", commit.id).unwrap();

        // Read it all back
        let ref_target = store.get_ref("main").unwrap().unwrap();
        assert_eq!(ref_target, commit.id);

        let retrieved_commit = store.get_commit(&ref_target).unwrap().unwrap();
        assert_eq!(retrieved_commit.state_root, id1);

        let retrieved_obj = store.get_object(&id1).unwrap().unwrap();
        assert_eq!(retrieved_obj, obj1);
    }
}
