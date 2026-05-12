//! SQLite storage backend — durable, single-file, zero-config.
//!
//! This is the default production backend. All state, commits, and refs
//! are stored in a single SQLite file that survives process restarts.

use std::path::Path;
use std::sync::Mutex;

use agentstategraph_core::{
    Commit, Epoch, EpochStatus, Namespace, Object, ObjectId, Session, SessionStatus,
};
use agentstategraph_reminders::{
    Reminder, ReminderError, ReminderFilter, ReminderStore,
    types::{Priority, ReminderStatus},
};
use agentstategraph_taint::{Taint, TaintEffect, TaintKind, TaintMetadata, TaintSeverity};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::traits::{
    CommitStore, EpochStore, ObjectStore, RefStore, SessionStore, StorageError, TaintStore,
};

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

            CREATE TABLE IF NOT EXISTS namespaces (
                name       TEXT PRIMARY KEY,
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS refs (
                namespace TEXT NOT NULL,
                name      TEXT NOT NULL,
                target    BLOB NOT NULL,
                PRIMARY KEY (namespace, name)
            );

            CREATE TABLE IF NOT EXISTS epochs (
                id              TEXT PRIMARY KEY,
                description     TEXT NOT NULL DEFAULT '',
                status          TEXT NOT NULL DEFAULT 'Active',
                created_at      TEXT NOT NULL,
                sealed_at       TEXT,
                summary         TEXT,
                root_intents    TEXT NOT NULL DEFAULT '[]',
                agents          TEXT NOT NULL DEFAULT '[]',
                tags            TEXT NOT NULL DEFAULT '[]',
                commit_count    INTEGER NOT NULL DEFAULT 0,
                sealed_commits  TEXT NOT NULL DEFAULT '[]'
            );

            CREATE TABLE IF NOT EXISTS sessions (
                id              TEXT PRIMARY KEY,
                agent_id        TEXT NOT NULL,
                parent_id       TEXT REFERENCES sessions(id),
                scope_path      TEXT,
                scope_branch    TEXT,
                scope_namespace TEXT,
                status          TEXT NOT NULL DEFAULT 'Active',
                created_at      TEXT NOT NULL,
                ended_at        TEXT,
                metadata        TEXT NOT NULL DEFAULT '{}',
                commit_count    INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS taints (
                id              TEXT PRIMARY KEY,
                path            TEXT NOT NULL,
                name            TEXT NOT NULL,
                kind            TEXT NOT NULL,
                effect          TEXT NOT NULL,
                severity        TEXT NOT NULL DEFAULT 'medium',
                reason          TEXT NOT NULL,
                agent_id        TEXT NOT NULL,
                commit_id       TEXT NOT NULL DEFAULT '',
                created_at      TEXT NOT NULL,
                expires_at      TEXT,
                resolved_at     TEXT,
                resolved_by     TEXT,
                resolved_reason TEXT,
                resolved_proof  TEXT,
                propagate       INTEGER NOT NULL DEFAULT 1,
                metadata        TEXT NOT NULL DEFAULT '{}'
            );

            CREATE TABLE IF NOT EXISTS reminders (
                id              TEXT PRIMARY KEY,
                title           TEXT NOT NULL,
                instructions    TEXT NOT NULL,
                commands        TEXT NOT NULL DEFAULT '[]',
                refs            TEXT NOT NULL DEFAULT '[]',
                priority        INTEGER NOT NULL DEFAULT 3,
                due_at          TEXT NOT NULL,
                schedule        TEXT,
                autonomous      INTEGER NOT NULL DEFAULT 1,
                created_by      TEXT NOT NULL,
                created_at      TEXT NOT NULL,
                status          TEXT NOT NULL DEFAULT 'pending',
                snoozed_until   TEXT,
                executions      TEXT NOT NULL DEFAULT '[]',
                tags            TEXT NOT NULL DEFAULT '[]'
            );

            CREATE INDEX IF NOT EXISTS idx_commits_timestamp ON commits(timestamp DESC);
            CREATE INDEX IF NOT EXISTS idx_epochs_status ON epochs(status);
            CREATE INDEX IF NOT EXISTS idx_sessions_agent ON sessions(agent_id);
            CREATE INDEX IF NOT EXISTS idx_taints_path    ON taints(path);
            CREATE INDEX IF NOT EXISTS idx_taints_kind    ON taints(kind);
            CREATE UNIQUE INDEX IF NOT EXISTS idx_taints_unique_active
                ON taints(path, name, kind) WHERE resolved_at IS NULL;
            CREATE INDEX IF NOT EXISTS idx_reminders_status     ON reminders(status);
            CREATE INDEX IF NOT EXISTS idx_reminders_due_at     ON reminders(due_at);
            CREATE INDEX IF NOT EXISTS idx_reminders_priority   ON reminders(priority);
            CREATE INDEX IF NOT EXISTS idx_reminders_created_by ON reminders(created_by);
            ",
        )
        .map_err(|e| StorageError::Backend(format!("init tables: {}", e)))?;

        // Migration-safe add of commits.epoch_id / commits.session_id.
        // SQLite's ALTER TABLE ADD COLUMN doesn't support IF NOT EXISTS
        // so we inspect the table first.
        let existing_cols: Vec<String> = {
            let mut stmt = conn
                .prepare("PRAGMA table_info(commits)")
                .map_err(|e| StorageError::Backend(format!("pragma commits: {}", e)))?;
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(1))
                .map_err(|e| StorageError::Backend(format!("pragma query: {}", e)))?;
            let mut cols = Vec::new();
            for r in rows {
                cols.push(r.map_err(|e| StorageError::Backend(format!("pragma row: {}", e)))?);
            }
            cols
        };
        if !existing_cols.iter().any(|c| c == "epoch_id") {
            conn.execute("ALTER TABLE commits ADD COLUMN epoch_id TEXT", [])
                .map_err(|e| StorageError::Backend(format!("add epoch_id col: {}", e)))?;
        }
        if !existing_cols.iter().any(|c| c == "session_id") {
            conn.execute("ALTER TABLE commits ADD COLUMN session_id TEXT", [])
                .map_err(|e| StorageError::Backend(format!("add session_id col: {}", e)))?;
        }
        // Migration-safe add of epochs.sealed_commits for DBs created
        // before 0.6.5-beta.1 shipped (V8 seal-violation enforcement
        // needs this persisted across restarts).
        let epoch_cols: Vec<String> = {
            let mut stmt = conn
                .prepare("PRAGMA table_info(epochs)")
                .map_err(|e| StorageError::Backend(format!("pragma epochs: {}", e)))?;
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(1))
                .map_err(|e| StorageError::Backend(format!("pragma query: {}", e)))?;
            let mut cols = Vec::new();
            for r in rows {
                cols.push(r.map_err(|e| StorageError::Backend(format!("pragma row: {}", e)))?);
            }
            cols
        };
        if !epoch_cols.iter().any(|c| c == "sealed_commits") {
            conn.execute(
                "ALTER TABLE epochs ADD COLUMN sealed_commits TEXT NOT NULL DEFAULT '[]'",
                [],
            )
            .map_err(|e| StorageError::Backend(format!("add sealed_commits: {}", e)))?;
        }
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_commits_epoch ON commits(epoch_id)",
            [],
        )
        .map_err(|e| StorageError::Backend(format!("idx epoch: {}", e)))?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_commits_session ON commits(session_id)",
            [],
        )
        .map_err(|e| StorageError::Backend(format!("idx session: {}", e)))?;

        // Migration: refs table — add namespace column + composite PK.
        // Check whether refs still has the old flat schema (single-column PK).
        let refs_cols: Vec<String> = {
            let mut stmt = conn
                .prepare("PRAGMA table_info(refs)")
                .map_err(|e| StorageError::Backend(format!("pragma refs: {}", e)))?;
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(1))
                .map_err(|e| StorageError::Backend(format!("pragma query: {}", e)))?;
            let mut cols = Vec::new();
            for r in rows {
                cols.push(r.map_err(|e| StorageError::Backend(format!("pragma row: {}", e)))?);
            }
            cols
        };
        if !refs_cols.iter().any(|c| c == "namespace") {
            // Old schema detected. Recreate with composite PK.
            conn.execute_batch(
                "
                CREATE TABLE refs_new (
                    namespace TEXT NOT NULL,
                    name      TEXT NOT NULL,
                    target    BLOB NOT NULL,
                    PRIMARY KEY (namespace, name)
                );
                INSERT INTO refs_new (namespace, name, target)
                    SELECT 'default', name, target FROM refs;
                DROP TABLE refs;
                ALTER TABLE refs_new RENAME TO refs;
                ",
            )
            .map_err(|e| StorageError::Backend(format!("migrate refs: {}", e)))?;
        }

        // Migration: ensure namespaces table exists and 'default' is seeded.
        conn.execute(
            "INSERT OR IGNORE INTO namespaces (name, created_at) VALUES ('default', ?1)",
            params![chrono::Utc::now().to_rfc3339()],
        )
        .map_err(|e| StorageError::Backend(format!("seed default namespace: {}", e)))?;

        // Migration: sessions — add scope_namespace column if missing.
        let session_cols: Vec<String> = {
            let mut stmt = conn
                .prepare("PRAGMA table_info(sessions)")
                .map_err(|e| StorageError::Backend(format!("pragma sessions: {}", e)))?;
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(1))
                .map_err(|e| StorageError::Backend(format!("pragma query: {}", e)))?;
            let mut cols = Vec::new();
            for r in rows {
                cols.push(r.map_err(|e| StorageError::Backend(format!("pragma row: {}", e)))?);
            }
            cols
        };
        if !session_cols.iter().any(|c| c == "scope_namespace") {
            conn.execute("ALTER TABLE sessions ADD COLUMN scope_namespace TEXT", [])
                .map_err(|e| StorageError::Backend(format!("add scope_namespace: {}", e)))?;
        }

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
    fn create_namespace(&self, namespace: &Namespace) -> Result<(), StorageError> {
        let conn = self.lock_conn()?;
        let rows = conn
            .execute(
                "INSERT OR IGNORE INTO namespaces (name, created_at) VALUES (?1, ?2)",
                params![namespace.as_str(), chrono::Utc::now().to_rfc3339()],
            )
            .map_err(|e| StorageError::Backend(format!("create namespace: {}", e)))?;
        if rows == 0 {
            return Err(StorageError::NamespaceAlreadyExists(
                namespace.as_str().to_string(),
            ));
        }
        Ok(())
    }

    fn list_namespaces(&self) -> Result<Vec<Namespace>, StorageError> {
        let conn = self.lock_conn()?;
        let mut stmt = conn
            .prepare("SELECT name FROM namespaces ORDER BY name")
            .map_err(|e| StorageError::Backend(format!("list namespaces: {}", e)))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| StorageError::Backend(format!("list namespaces query: {}", e)))?;
        let mut result = Vec::new();
        for row in rows {
            let name =
                row.map_err(|e| StorageError::Backend(format!("list namespaces row: {}", e)))?;
            let ns = Namespace::new(name)
                .map_err(|e| StorageError::Backend(format!("invalid namespace in db: {}", e)))?;
            result.push(ns);
        }
        Ok(result)
    }

    fn get_ref(&self, namespace: &Namespace, name: &str) -> Result<Option<ObjectId>, StorageError> {
        let conn = self.lock_conn()?;
        let ns_exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM namespaces WHERE name = ?1)",
                params![namespace.as_str()],
                |row| row.get(0),
            )
            .map_err(|e| StorageError::Backend(format!("check namespace: {}", e)))?;
        if !ns_exists {
            return Err(StorageError::NamespaceNotFound(
                namespace.as_str().to_string(),
            ));
        }

        let result: Option<Vec<u8>> = conn
            .query_row(
                "SELECT target FROM refs WHERE namespace = ?1 AND name = ?2",
                params![namespace.as_str(), name],
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

    fn set_ref(
        &self,
        namespace: &Namespace,
        name: &str,
        target: ObjectId,
    ) -> Result<(), StorageError> {
        let conn = self.lock_conn()?;
        let ns_exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM namespaces WHERE name = ?1)",
                params![namespace.as_str()],
                |row| row.get(0),
            )
            .map_err(|e| StorageError::Backend(format!("check namespace: {}", e)))?;
        if !ns_exists {
            return Err(StorageError::NamespaceNotFound(
                namespace.as_str().to_string(),
            ));
        }

        conn.execute(
            "INSERT OR REPLACE INTO refs (namespace, name, target) VALUES (?1, ?2, ?3)",
            params![namespace.as_str(), name, target.as_bytes().as_slice()],
        )
        .map_err(|e| StorageError::Backend(format!("set ref: {}", e)))?;
        Ok(())
    }

    fn cas_ref(
        &self,
        namespace: &Namespace,
        name: &str,
        expected: ObjectId,
        new: ObjectId,
    ) -> Result<bool, StorageError> {
        let conn = self.lock_conn()?;
        let ns_exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM namespaces WHERE name = ?1)",
                params![namespace.as_str()],
                |row| row.get(0),
            )
            .map_err(|e| StorageError::Backend(format!("check namespace: {}", e)))?;
        if !ns_exists {
            return Err(StorageError::NamespaceNotFound(
                namespace.as_str().to_string(),
            ));
        }

        let rows = conn
            .execute(
                "UPDATE refs SET target = ?1 \
                 WHERE namespace = ?2 AND name = ?3 AND target = ?4",
                params![
                    new.as_bytes().as_slice(),
                    namespace.as_str(),
                    name,
                    expected.as_bytes().as_slice()
                ],
            )
            .map_err(|e| StorageError::Backend(format!("cas ref: {}", e)))?;
        Ok(rows > 0)
    }

    fn list_refs(
        &self,
        namespace: &Namespace,
        prefix: &str,
    ) -> Result<Vec<(String, ObjectId)>, StorageError> {
        let conn = self.lock_conn()?;
        let ns_exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM namespaces WHERE name = ?1)",
                params![namespace.as_str()],
                |row| row.get(0),
            )
            .map_err(|e| StorageError::Backend(format!("check namespace: {}", e)))?;
        if !ns_exists {
            return Err(StorageError::NamespaceNotFound(
                namespace.as_str().to_string(),
            ));
        }

        let mut stmt = conn
            .prepare(
                "SELECT name, target FROM refs \
                 WHERE namespace = ?1 AND name LIKE ?2 ORDER BY name",
            )
            .map_err(|e| StorageError::Backend(format!("list refs: {}", e)))?;

        let pattern = format!("{}%", prefix);
        let rows = stmt
            .query_map(params![namespace.as_str(), pattern], |row| {
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

    fn delete_ref(&self, namespace: &Namespace, name: &str) -> Result<bool, StorageError> {
        let conn = self.lock_conn()?;
        let ns_exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM namespaces WHERE name = ?1)",
                params![namespace.as_str()],
                |row| row.get(0),
            )
            .map_err(|e| StorageError::Backend(format!("check namespace: {}", e)))?;
        if !ns_exists {
            return Err(StorageError::NamespaceNotFound(
                namespace.as_str().to_string(),
            ));
        }

        let rows = conn
            .execute(
                "DELETE FROM refs WHERE namespace = ?1 AND name = ?2",
                params![namespace.as_str(), name],
            )
            .map_err(|e| StorageError::Backend(format!("delete ref: {}", e)))?;
        Ok(rows > 0)
    }
}

fn epoch_status_str(s: &EpochStatus) -> &'static str {
    match s {
        EpochStatus::Active => "Active",
        EpochStatus::Sealed => "Sealed",
        EpochStatus::Archived => "Archived",
    }
}

fn parse_epoch_status(s: &str) -> EpochStatus {
    match s {
        "Sealed" => EpochStatus::Sealed,
        "Archived" => EpochStatus::Archived,
        _ => EpochStatus::Active,
    }
}

fn parse_rfc3339(s: &str) -> Result<DateTime<Utc>, StorageError> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| StorageError::Serialization(format!("timestamp parse: {}", e)))
}

#[allow(clippy::too_many_arguments)]
fn row_to_epoch(
    id: String,
    description: String,
    status: String,
    created_at: String,
    sealed_at: Option<String>,
    summary: Option<String>,
    root_intents: String,
    agents: String,
    tags: String,
    sealed_commits: String,
) -> Result<Epoch, StorageError> {
    let root_intents: Vec<String> = serde_json::from_str(&root_intents)
        .map_err(|e| StorageError::Serialization(format!("root_intents: {}", e)))?;
    let agents: Vec<String> = serde_json::from_str(&agents)
        .map_err(|e| StorageError::Serialization(format!("agents: {}", e)))?;
    let tags: Vec<String> = serde_json::from_str(&tags)
        .map_err(|e| StorageError::Serialization(format!("tags: {}", e)))?;
    let sealed_commits: Vec<ObjectId> = serde_json::from_str(&sealed_commits)
        .map_err(|e| StorageError::Serialization(format!("sealed_commits: {}", e)))?;

    Ok(Epoch {
        id,
        description,
        root_intents,
        status: parse_epoch_status(&status),
        created_at: parse_rfc3339(&created_at)?,
        sealed_at: sealed_at.as_deref().map(parse_rfc3339).transpose()?,
        seal_summary: summary,
        seal_hash: None,
        commits: Vec::new(),
        agents,
        branches: Vec::new(),
        tags,
        sealed_commits,
    })
}

impl EpochStore for SqliteStorage {
    fn create_epoch(&self, epoch: &Epoch) -> Result<(), StorageError> {
        let conn = self.lock_conn()?;
        let root_intents = serde_json::to_string(&epoch.root_intents)
            .map_err(|e| StorageError::Serialization(e.to_string()))?;
        let agents = serde_json::to_string(&epoch.agents)
            .map_err(|e| StorageError::Serialization(e.to_string()))?;
        let tags = serde_json::to_string(&epoch.tags)
            .map_err(|e| StorageError::Serialization(e.to_string()))?;
        let sealed_commits = serde_json::to_string(&epoch.sealed_commits)
            .map_err(|e| StorageError::Serialization(e.to_string()))?;
        conn.execute(
            "INSERT INTO epochs
             (id, description, status, created_at, sealed_at, summary,
              root_intents, agents, tags, commit_count, sealed_commits)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                epoch.id,
                epoch.description,
                epoch_status_str(&epoch.status),
                epoch.created_at.to_rfc3339(),
                epoch.sealed_at.map(|t| t.to_rfc3339()),
                epoch.seal_summary,
                root_intents,
                agents,
                tags,
                epoch.commits.len() as i64,
                sealed_commits,
            ],
        )
        .map_err(|e| StorageError::Backend(format!("create epoch: {}", e)))?;
        Ok(())
    }

    fn seal_epoch(
        &self,
        id: &str,
        summary: &str,
        sealed_at: DateTime<Utc>,
        sealed_commits: &[ObjectId],
    ) -> Result<(), StorageError> {
        let conn = self.lock_conn()?;
        let current: Option<String> = conn
            .query_row(
                "SELECT status FROM epochs WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| StorageError::Backend(format!("seal lookup: {}", e)))?;
        match current.as_deref() {
            None => {
                return Err(StorageError::Backend(format!("epoch not found: {}", id)));
            }
            Some("Sealed") | Some("Archived") => {
                return Err(StorageError::EpochAlreadySealed { id: id.to_string() });
            }
            _ => {}
        }
        let sc_json = serde_json::to_string(sealed_commits)
            .map_err(|e| StorageError::Serialization(e.to_string()))?;
        conn.execute(
            "UPDATE epochs
             SET status = 'Sealed', sealed_at = ?1, summary = ?2, sealed_commits = ?3
             WHERE id = ?4",
            params![sealed_at.to_rfc3339(), summary, sc_json, id],
        )
        .map_err(|e| StorageError::Backend(format!("seal epoch: {}", e)))?;
        Ok(())
    }

    fn list_epochs(&self) -> Result<Vec<Epoch>, StorageError> {
        let conn = self.lock_conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, description, status, created_at, sealed_at, summary,
                        root_intents, agents, tags, sealed_commits
                 FROM epochs ORDER BY created_at DESC",
            )
            .map_err(|e| StorageError::Backend(format!("list epochs prep: {}", e)))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                ))
            })
            .map_err(|e| StorageError::Backend(format!("list epochs query: {}", e)))?;
        let mut out = Vec::new();
        for r in rows {
            let (id, desc, status, created_at, sealed_at, summary, ri, ag, tg, sc) =
                r.map_err(|e| StorageError::Backend(format!("list epochs row: {}", e)))?;
            out.push(row_to_epoch(
                id, desc, status, created_at, sealed_at, summary, ri, ag, tg, sc,
            )?);
        }
        Ok(out)
    }

    fn get_epoch(&self, id: &str) -> Result<Option<Epoch>, StorageError> {
        let conn = self.lock_conn()?;
        type EpochRow = (
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            String,
            String,
            String,
            String,
        );
        let row: Option<EpochRow> = conn
            .query_row(
                "SELECT id, description, status, created_at, sealed_at, summary,
                        root_intents, agents, tags, sealed_commits
                 FROM epochs WHERE id = ?1",
                params![id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| StorageError::Backend(format!("get epoch: {}", e)))?;
        match row {
            None => Ok(None),
            Some((id, desc, status, created_at, sealed_at, summary, ri, ag, tg, sc)) => {
                let mut epoch = row_to_epoch(
                    id, desc, status, created_at, sealed_at, summary, ri, ag, tg, sc,
                )?;
                // Rehydrate commits list from the commits table.
                let mut stmt = conn
                    .prepare("SELECT id FROM commits WHERE epoch_id = ?1 ORDER BY rowid")
                    .map_err(|e| StorageError::Backend(format!("prepare epoch commits: {}", e)))?;
                let commit_ids: Vec<ObjectId> = stmt
                    .query_map(params![epoch.id], |row| {
                        let bytes: Vec<u8> = row.get(0)?;
                        Ok(bytes)
                    })
                    .map_err(|e| StorageError::Backend(format!("query epoch commits: {}", e)))?
                    .filter_map(|r| r.ok())
                    .filter_map(|b| {
                        if b.len() == 32 {
                            let mut arr = [0u8; 32];
                            arr.copy_from_slice(&b);
                            Some(ObjectId::from_bytes(arr))
                        } else {
                            None
                        }
                    })
                    .collect();
                epoch.commits = commit_ids;
                Ok(Some(epoch))
            }
        }
    }

    fn archive_epoch(&self, id: &str) -> Result<(), StorageError> {
        let conn = self.lock_conn()?;
        let rows = conn
            .execute(
                "UPDATE epochs SET status = 'Archived' WHERE id = ?1 AND status = 'Sealed'",
                params![id],
            )
            .map_err(|e| StorageError::Backend(format!("archive epoch: {}", e)))?;
        if rows == 0 {
            return Err(StorageError::Backend(format!(
                "epoch '{}' not found or not sealed",
                id
            )));
        }
        Ok(())
    }

    fn set_commit_epoch(&self, commit_id: &ObjectId, epoch_id: &str) -> Result<(), StorageError> {
        let conn = self.lock_conn()?;
        let status: Option<String> = conn
            .query_row(
                "SELECT status FROM epochs WHERE id = ?1",
                params![epoch_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| StorageError::Backend(format!("assoc lookup: {}", e)))?;
        match status.as_deref() {
            None => {
                return Err(StorageError::Backend(format!(
                    "epoch not found: {}",
                    epoch_id
                )));
            }
            Some("Sealed") | Some("Archived") => {
                return Err(StorageError::EpochAlreadySealed {
                    id: epoch_id.to_string(),
                });
            }
            _ => {}
        }
        conn.execute(
            "UPDATE commits SET epoch_id = ?1 WHERE id = ?2",
            params![epoch_id, commit_id.as_bytes().as_slice()],
        )
        .map_err(|e| StorageError::Backend(format!("set commit epoch: {}", e)))?;
        // Keep commit_count in sync — cheap best-effort.
        conn.execute(
            "UPDATE epochs SET commit_count = (
                 SELECT COUNT(*) FROM commits WHERE epoch_id = epochs.id
             ) WHERE id = ?1",
            params![epoch_id],
        )
        .map_err(|e| StorageError::Backend(format!("update commit_count: {}", e)))?;
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn row_to_session(
    id: String,
    agent_id: String,
    parent_id: Option<String>,
    scope_path: Option<String>,
    scope_branch: Option<String>,
    scope_namespace: Option<String>,
    status: String,
    created_at: String,
    ended_at: Option<String>,
    metadata: String,
) -> Result<Session, StorageError> {
    // The `metadata` JSON blob carries fields the Session struct has
    // but the schema doesn't surface as dedicated columns
    // (working_branch's full path, head object id, delegated_intent,
    // report_to). Parse it back; default missing pieces defensively.
    #[derive(serde::Deserialize, Default)]
    struct Meta {
        #[serde(default)]
        head: Option<ObjectId>,
        #[serde(default)]
        delegated_intent: Option<String>,
        #[serde(default)]
        report_to: Option<String>,
        #[serde(default)]
        working_branch: Option<String>,
        #[serde(default)]
        scope_tenant: Option<String>,
    }
    let meta: Meta = serde_json::from_str(&metadata).unwrap_or_default();
    let head = meta.head.unwrap_or_else(|| ObjectId::hash(b""));
    let working_branch = meta
        .working_branch
        .unwrap_or_else(|| scope_branch.clone().unwrap_or_default());
    Ok(Session {
        id,
        agent_id,
        working_branch,
        head,
        parent_session: parent_id,
        delegated_intent: meta.delegated_intent,
        report_to: meta.report_to,
        path_scope: scope_path,
        scope_tenant: meta.scope_tenant,
        scope_namespace,
        status: SessionStatus::from_wire(&status),
        created_at: parse_rfc3339(&created_at)?,
        ended_at: ended_at.as_deref().map(parse_rfc3339).transpose()?,
    })
}

impl SessionStore for SqliteStorage {
    fn create_session(&self, session: &Session) -> Result<(), StorageError> {
        let conn = self.lock_conn()?;
        let metadata = serde_json::json!({
            "head": session.head,
            "delegated_intent": session.delegated_intent,
            "report_to": session.report_to,
            "working_branch": session.working_branch,
            "scope_tenant": session.scope_tenant,
        })
        .to_string();
        conn.execute(
            "INSERT OR REPLACE INTO sessions
             (id, agent_id, parent_id, scope_path, scope_branch, scope_namespace,
              status, created_at, ended_at, metadata, commit_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                     COALESCE((SELECT commit_count FROM sessions WHERE id = ?1), 0))",
            params![
                session.id,
                session.agent_id,
                session.parent_session,
                session.path_scope,
                session.working_branch,
                session.scope_namespace,
                session.status.as_str(),
                session.created_at.to_rfc3339(),
                session.ended_at.map(|t| t.to_rfc3339()),
                metadata,
            ],
        )
        .map_err(|e| StorageError::Backend(format!("create session: {}", e)))?;
        Ok(())
    }

    fn end_session(
        &self,
        id: &str,
        status: SessionStatus,
        ended_at: DateTime<Utc>,
    ) -> Result<(), StorageError> {
        let conn = self.lock_conn()?;
        let current: Option<String> = conn
            .query_row(
                "SELECT status FROM sessions WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| StorageError::Backend(format!("end session lookup: {}", e)))?;
        match current.as_deref() {
            None => {
                return Err(StorageError::Backend(format!("session not found: {}", id)));
            }
            Some(s) if s != "Active" => {
                return Err(StorageError::SessionEnded { id: id.to_string() });
            }
            _ => {}
        }
        conn.execute(
            "UPDATE sessions SET status = ?1, ended_at = ?2 WHERE id = ?3",
            params![status.as_str(), ended_at.to_rfc3339(), id],
        )
        .map_err(|e| StorageError::Backend(format!("end session: {}", e)))?;
        Ok(())
    }

    fn list_sessions(&self, agent_filter: Option<&str>) -> Result<Vec<Session>, StorageError> {
        let conn = self.lock_conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, agent_id, parent_id, scope_path, scope_branch, scope_namespace,
                        status, created_at, ended_at, metadata
                 FROM sessions
                 WHERE (?1 IS NULL OR agent_id = ?1)
                 ORDER BY created_at DESC",
            )
            .map_err(|e| StorageError::Backend(format!("list sessions prep: {}", e)))?;
        let rows = stmt
            .query_map(params![agent_filter], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, String>(9)?,
                ))
            })
            .map_err(|e| StorageError::Backend(format!("list sessions query: {}", e)))?;
        let mut out = Vec::new();
        for r in rows {
            let (id, agent, parent, sp, sb, sns, st, ca, ea, md) =
                r.map_err(|e| StorageError::Backend(format!("list sessions row: {}", e)))?;
            out.push(row_to_session(id, agent, parent, sp, sb, sns, st, ca, ea, md)?);
        }
        Ok(out)
    }

    fn get_session(&self, id: &str) -> Result<Option<Session>, StorageError> {
        let conn = self.lock_conn()?;
        type SessionRow = (
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            String,
            String,
            Option<String>,
            String,
        );
        let row: Option<SessionRow> = conn
            .query_row(
                "SELECT id, agent_id, parent_id, scope_path, scope_branch, scope_namespace,
                        status, created_at, ended_at, metadata
                 FROM sessions WHERE id = ?1",
                params![id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| StorageError::Backend(format!("get session: {}", e)))?;
        match row {
            None => Ok(None),
            Some((id, agent, parent, sp, sb, sns, st, ca, ea, md)) => Ok(Some(row_to_session(
                id, agent, parent, sp, sb, sns, st, ca, ea, md,
            )?)),
        }
    }

    fn set_commit_session(
        &self,
        commit_id: &ObjectId,
        session_id: &str,
    ) -> Result<(), StorageError> {
        let conn = self.lock_conn()?;
        let status: Option<String> = conn
            .query_row(
                "SELECT status FROM sessions WHERE id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| StorageError::Backend(format!("assoc lookup: {}", e)))?;
        match status.as_deref() {
            None => {
                return Err(StorageError::Backend(format!(
                    "session not found: {}",
                    session_id
                )));
            }
            Some(s) if s != "Active" => {
                return Err(StorageError::SessionEnded {
                    id: session_id.to_string(),
                });
            }
            _ => {}
        }
        conn.execute(
            "UPDATE commits SET session_id = ?1 WHERE id = ?2",
            params![session_id, commit_id.as_bytes().as_slice()],
        )
        .map_err(|e| StorageError::Backend(format!("set commit session: {}", e)))?;
        conn.execute(
            "UPDATE sessions SET commit_count = (
                 SELECT COUNT(*) FROM commits WHERE session_id = sessions.id
             ) WHERE id = ?1",
            params![session_id],
        )
        .map_err(|e| StorageError::Backend(format!("update session commit_count: {}", e)))?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// TaintStore (0.7.75 §3)
// ---------------------------------------------------------------------------

fn kind_to_str(k: TaintKind) -> &'static str {
    match k {
        TaintKind::Taint => "taint",
        TaintKind::Quarantine => "quarantine",
        TaintKind::Watch => "watch",
    }
}

fn kind_from_str(s: &str) -> Result<TaintKind, StorageError> {
    match s {
        "taint" => Ok(TaintKind::Taint),
        "quarantine" => Ok(TaintKind::Quarantine),
        "watch" => Ok(TaintKind::Watch),
        other => Err(StorageError::Backend(format!(
            "unknown taint kind: {other}"
        ))),
    }
}

fn effect_to_str(e: TaintEffect) -> &'static str {
    match e {
        TaintEffect::Warn => "warn",
        TaintEffect::Block => "block",
        TaintEffect::Review => "review",
        TaintEffect::Isolate => "isolate",
        TaintEffect::Advisory => "advisory",
    }
}

fn effect_from_str(s: &str) -> Result<TaintEffect, StorageError> {
    match s {
        "warn" => Ok(TaintEffect::Warn),
        "block" => Ok(TaintEffect::Block),
        "review" => Ok(TaintEffect::Review),
        "isolate" => Ok(TaintEffect::Isolate),
        "advisory" => Ok(TaintEffect::Advisory),
        other => Err(StorageError::Backend(format!(
            "unknown taint effect: {other}"
        ))),
    }
}

fn severity_to_str(s: TaintSeverity) -> &'static str {
    match s {
        TaintSeverity::Low => "low",
        TaintSeverity::Medium => "medium",
        TaintSeverity::High => "high",
        TaintSeverity::Critical => "critical",
    }
}

fn severity_from_str(s: &str) -> Result<TaintSeverity, StorageError> {
    match s {
        "low" => Ok(TaintSeverity::Low),
        "medium" => Ok(TaintSeverity::Medium),
        "high" => Ok(TaintSeverity::High),
        "critical" => Ok(TaintSeverity::Critical),
        other => Err(StorageError::Backend(format!(
            "unknown taint severity: {other}"
        ))),
    }
}

/// Row extraction returns `rusqlite::Error` so the closure fits
/// `query_map` / `query_row` signatures. Decode-side parsing errors
/// (serde, chrono) are remapped to
/// `rusqlite::Error::FromSqlConversionFailure` so callers see them
/// as backend errors; all cases are converted to `StorageError` in
/// the calling impls via `map_err`.
fn row_to_taint(row: &Row<'_>) -> rusqlite::Result<Taint> {
    use rusqlite::types::{FromSqlError, Type};

    fn decode_err<E: std::fmt::Display>(col: usize, ty: Type, e: E) -> rusqlite::Error {
        rusqlite::Error::FromSqlConversionFailure(
            col,
            ty,
            Box::new(FromSqlError::Other(format!("{e}").into())),
        )
    }

    let kind: String = row.get("kind")?;
    let effect: String = row.get("effect")?;
    let severity: String = row.get("severity")?;
    let metadata_raw: String = row.get("metadata")?;
    let created_at_s: String = row.get("created_at")?;
    let expires_at_s: Option<String> = row.get("expires_at")?;
    let resolved_at_s: Option<String> = row.get("resolved_at")?;

    let metadata: TaintMetadata = serde_json::from_str(&metadata_raw)
        .map_err(|e| decode_err(0, Type::Text, format!("taint metadata: {e}")))?;
    let created_at = DateTime::parse_from_rfc3339(&created_at_s)
        .map_err(|e| decode_err(0, Type::Text, format!("created_at: {e}")))?
        .with_timezone(&Utc);
    let expires_at = match expires_at_s {
        Some(s) => Some(
            DateTime::parse_from_rfc3339(&s)
                .map_err(|e| decode_err(0, Type::Text, format!("expires_at: {e}")))?
                .with_timezone(&Utc),
        ),
        None => None,
    };
    let resolved_at = match resolved_at_s {
        Some(s) => Some(
            DateTime::parse_from_rfc3339(&s)
                .map_err(|e| decode_err(0, Type::Text, format!("resolved_at: {e}")))?
                .with_timezone(&Utc),
        ),
        None => None,
    };
    let kind = kind_from_str(&kind).map_err(|e| decode_err(0, Type::Text, e.to_string()))?;
    let effect = effect_from_str(&effect).map_err(|e| decode_err(0, Type::Text, e.to_string()))?;
    let severity =
        severity_from_str(&severity).map_err(|e| decode_err(0, Type::Text, e.to_string()))?;

    Ok(Taint {
        id: row.get("id")?,
        path: row.get("path")?,
        name: row.get("name")?,
        kind,
        effect,
        severity,
        reason: row.get("reason")?,
        agent_id: row.get("agent_id")?,
        commit_id: row.get("commit_id")?,
        created_at,
        expires_at,
        resolved_at,
        resolved_by: row.get("resolved_by")?,
        resolved_reason: row.get("resolved_reason")?,
        resolved_proof: row.get("resolved_proof")?,
        propagate: row.get::<_, i64>("propagate")? != 0,
        metadata,
    })
}

impl TaintStore for SqliteStorage {
    fn create_taint(&self, taint: &Taint) -> Result<(), StorageError> {
        let conn = self.lock_conn()?;
        let metadata_json = serde_json::to_string(&taint.metadata)
            .map_err(|e| StorageError::Serialization(format!("taint metadata: {e}")))?;
        conn.execute(
            "INSERT INTO taints (
                id, path, name, kind, effect, severity, reason, agent_id,
                commit_id, created_at, expires_at, resolved_at, resolved_by,
                resolved_reason, resolved_proof, propagate, metadata
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
                ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17
            )",
            params![
                taint.id,
                taint.path,
                taint.name,
                kind_to_str(taint.kind),
                effect_to_str(taint.effect),
                severity_to_str(taint.severity),
                taint.reason,
                taint.agent_id,
                taint.commit_id,
                taint.created_at.to_rfc3339(),
                taint.expires_at.map(|t| t.to_rfc3339()),
                taint.resolved_at.map(|t| t.to_rfc3339()),
                taint.resolved_by,
                taint.resolved_reason,
                taint.resolved_proof,
                if taint.propagate { 1_i64 } else { 0_i64 },
                metadata_json,
            ],
        )
        .map_err(|e| StorageError::Backend(format!("insert taint: {e}")))?;
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
        let conn = self.lock_conn()?;
        // Guard: fail on already-resolved.
        let already: Option<String> = conn
            .query_row(
                "SELECT resolved_at FROM taints WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| StorageError::Backend(format!("resolve check: {e}")))?
            .ok_or_else(|| StorageError::Backend(format!("taint {id} not found")))?;
        if already.is_some() {
            return Err(StorageError::Backend(format!(
                "taint {id} is already resolved"
            )));
        }
        conn.execute(
            "UPDATE taints SET resolved_at = ?1, resolved_by = ?2,
                resolved_reason = ?3, resolved_proof = ?4
             WHERE id = ?5",
            params![resolved_at.to_rfc3339(), resolved_by, reason, proof, id],
        )
        .map_err(|e| StorageError::Backend(format!("resolve taint: {e}")))?;
        Ok(())
    }

    fn list_taints(
        &self,
        path_prefix: Option<&str>,
        kind: Option<TaintKind>,
        include_resolved: bool,
    ) -> Result<Vec<Taint>, StorageError> {
        let conn = self.lock_conn()?;
        let mut sql = String::from("SELECT * FROM taints WHERE 1=1");
        if !include_resolved {
            sql.push_str(" AND resolved_at IS NULL");
        }
        if let Some(_p) = path_prefix {
            sql.push_str(" AND (path = :p OR path LIKE :plike)");
        }
        if kind.is_some() {
            sql.push_str(" AND kind = :k");
        }
        sql.push_str(" ORDER BY created_at DESC");
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| StorageError::Backend(format!("list taints prep: {e}")))?;
        let mut params_named: Vec<(&str, Box<dyn rusqlite::ToSql>)> = Vec::new();
        let plike;
        if let Some(p) = path_prefix {
            let p = p.trim_end_matches('/');
            plike = format!("{}/%", p);
            params_named.push((":p", Box::new(p.to_string())));
            params_named.push((":plike", Box::new(plike.clone())));
        }
        if let Some(k) = kind {
            params_named.push((":k", Box::new(kind_to_str(k).to_string())));
        }
        let refs: Vec<(&str, &dyn rusqlite::ToSql)> =
            params_named.iter().map(|(n, v)| (*n, v.as_ref())).collect();
        let rows = stmt
            .query_map(refs.as_slice(), row_to_taint)
            .map_err(|e| StorageError::Backend(format!("list taints query: {e}")))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| StorageError::Backend(format!("list taints row: {e}")))?);
        }
        Ok(out)
    }

    fn check_taint(&self, request_path: &str) -> Result<Vec<Taint>, StorageError> {
        let conn = self.lock_conn()?;
        let now = Utc::now().to_rfc3339();
        // Ancestor match: exact OR (propagate=1 AND request_path LIKE
        // path || '/%'). SQLite LIKE is case-sensitive by default
        // which matches our path semantics.
        let mut stmt = conn
            .prepare(
                "SELECT * FROM taints
                 WHERE resolved_at IS NULL
                   AND (expires_at IS NULL OR expires_at > :now)
                   AND (path = :path
                        OR (propagate = 1 AND :path LIKE path || '/%'))
                 ORDER BY created_at DESC",
            )
            .map_err(|e| StorageError::Backend(format!("check_taint prep: {e}")))?;
        let rows = stmt
            .query_map(
                &[
                    (":now", &now as &dyn rusqlite::ToSql),
                    (":path", &request_path),
                ],
                row_to_taint,
            )
            .map_err(|e| StorageError::Backend(format!("check_taint query: {e}")))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| StorageError::Backend(format!("check_taint row: {e}")))?);
        }
        Ok(out)
    }

    fn get_taint(&self, id: &str) -> Result<Option<Taint>, StorageError> {
        let conn = self.lock_conn()?;
        let mut stmt = conn
            .prepare("SELECT * FROM taints WHERE id = ?1")
            .map_err(|e| StorageError::Backend(format!("get_taint prep: {e}")))?;
        let t = stmt
            .query_row(params![id], row_to_taint)
            .optional()
            .map_err(|e| StorageError::Backend(format!("get_taint: {e}")))?;
        Ok(t)
    }

    fn set_taint_commit_id(&self, id: &str, commit_id: &str) -> Result<(), StorageError> {
        let conn = self.lock_conn()?;
        conn.execute(
            "UPDATE taints SET commit_id = ?1 WHERE id = ?2 AND resolved_at IS NULL",
            params![commit_id, id],
        )
        .map_err(|e| StorageError::Backend(format!("set_taint_commit_id: {e}")))?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ReminderStore — durable SQLite reminder storage
// ---------------------------------------------------------------------------

fn reminder_status_to_str(s: ReminderStatus) -> &'static str {
    match s {
        ReminderStatus::Pending => "pending",
        ReminderStatus::Due => "due",
        ReminderStatus::AwaitingPermission => "awaiting_permission",
        ReminderStatus::InProgress => "in_progress",
        ReminderStatus::Completed => "completed",
        ReminderStatus::Snoozed => "snoozed",
        ReminderStatus::Cancelled => "cancelled",
    }
}

fn reminder_status_from_str(s: &str) -> Result<ReminderStatus, ReminderError> {
    match s {
        "pending" => Ok(ReminderStatus::Pending),
        "due" => Ok(ReminderStatus::Due),
        "awaiting_permission" => Ok(ReminderStatus::AwaitingPermission),
        "in_progress" => Ok(ReminderStatus::InProgress),
        "completed" => Ok(ReminderStatus::Completed),
        "snoozed" => Ok(ReminderStatus::Snoozed),
        "cancelled" => Ok(ReminderStatus::Cancelled),
        other => Err(ReminderError::Store(format!(
            "unknown reminder status: {other}"
        ))),
    }
}

fn priority_to_i64(p: Priority) -> i64 {
    p.as_u8() as i64
}

fn priority_from_i64(n: i64) -> Result<Priority, ReminderError> {
    match n {
        1 => Ok(Priority::Critical),
        2 => Ok(Priority::High),
        3 => Ok(Priority::Medium),
        4 => Ok(Priority::Low),
        5 => Ok(Priority::Minimal),
        other => Err(ReminderError::Store(format!(
            "unknown priority value: {other}"
        ))),
    }
}

fn row_to_reminder(row: &Row<'_>) -> rusqlite::Result<Reminder> {
    use rusqlite::types::{FromSqlError, Type};

    fn decode_err<E: std::fmt::Display>(e: E) -> rusqlite::Error {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            Type::Text,
            Box::new(FromSqlError::Other(format!("{e}").into())),
        )
    }

    let status_s: String = row.get("status")?;
    let priority_n: i64 = row.get("priority")?;
    let due_at_s: String = row.get("due_at")?;
    let created_at_s: String = row.get("created_at")?;
    let snoozed_until_s: Option<String> = row.get("snoozed_until")?;
    let commands_s: String = row.get("commands")?;
    let refs_s: String = row.get("refs")?;
    let schedule_s: Option<String> = row.get("schedule")?;
    let executions_s: String = row.get("executions")?;
    let tags_s: String = row.get("tags")?;
    let autonomous: i64 = row.get("autonomous")?;

    let status = reminder_status_from_str(&status_s).map_err(|e| decode_err(e))?;
    let priority = priority_from_i64(priority_n).map_err(|e| decode_err(e))?;
    let due_at = DateTime::parse_from_rfc3339(&due_at_s)
        .map_err(|e| decode_err(format!("due_at: {e}")))?
        .with_timezone(&Utc);
    let created_at = DateTime::parse_from_rfc3339(&created_at_s)
        .map_err(|e| decode_err(format!("created_at: {e}")))?
        .with_timezone(&Utc);
    let snoozed_until = match snoozed_until_s {
        Some(s) => Some(
            DateTime::parse_from_rfc3339(&s)
                .map_err(|e| decode_err(format!("snoozed_until: {e}")))?
                .with_timezone(&Utc),
        ),
        None => None,
    };
    let commands: Vec<String> =
        serde_json::from_str(&commands_s).map_err(|e| decode_err(format!("commands: {e}")))?;
    let refs = serde_json::from_str(&refs_s).map_err(|e| decode_err(format!("refs: {e}")))?;
    let schedule = match schedule_s {
        Some(s) => {
            Some(serde_json::from_str(&s).map_err(|e| decode_err(format!("schedule: {e}")))?)
        }
        None => None,
    };
    let executions: Vec<agentstategraph_reminders::types::ExecutionRecord> =
        serde_json::from_str(&executions_s).map_err(|e| decode_err(format!("executions: {e}")))?;
    let tags: Vec<String> =
        serde_json::from_str(&tags_s).map_err(|e| decode_err(format!("tags: {e}")))?;

    Ok(Reminder {
        id: row.get("id")?,
        title: row.get("title")?,
        instructions: row.get("instructions")?,
        commands,
        refs,
        priority,
        due_at,
        schedule,
        autonomous: autonomous != 0,
        created_by: row.get("created_by")?,
        created_at,
        status,
        snoozed_until,
        executions,
        tags,
    })
}

impl ReminderStore for SqliteStorage {
    fn save(&self, reminder: &Reminder) -> Result<(), ReminderError> {
        let conn = self
            .lock_conn()
            .map_err(|e| ReminderError::Store(e.to_string()))?;
        let commands_json = serde_json::to_string(&reminder.commands)
            .map_err(|e| ReminderError::Store(format!("commands: {e}")))?;
        let refs_json = serde_json::to_string(&reminder.refs)
            .map_err(|e| ReminderError::Store(format!("refs: {e}")))?;
        let schedule_json = reminder
            .schedule
            .as_ref()
            .map(|s| serde_json::to_string(s))
            .transpose()
            .map_err(|e| ReminderError::Store(format!("schedule: {e}")))?;
        let executions_json = serde_json::to_string(&reminder.executions)
            .map_err(|e| ReminderError::Store(format!("executions: {e}")))?;
        let tags_json = serde_json::to_string(&reminder.tags)
            .map_err(|e| ReminderError::Store(format!("tags: {e}")))?;
        conn.execute(
            "INSERT INTO reminders (
                id, title, instructions, commands, refs, priority, due_at,
                schedule, autonomous, created_by, created_at, status,
                snoozed_until, executions, tags
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15
            )",
            params![
                reminder.id,
                reminder.title,
                reminder.instructions,
                commands_json,
                refs_json,
                priority_to_i64(reminder.priority),
                reminder.due_at.to_rfc3339(),
                schedule_json,
                if reminder.autonomous { 1_i64 } else { 0_i64 },
                reminder.created_by,
                reminder.created_at.to_rfc3339(),
                reminder_status_to_str(reminder.status),
                reminder.snoozed_until.map(|t| t.to_rfc3339()),
                executions_json,
                tags_json,
            ],
        )
        .map_err(|e| ReminderError::Store(format!("insert reminder: {e}")))?;
        Ok(())
    }

    fn get(&self, id: &str) -> Result<Option<Reminder>, ReminderError> {
        let conn = self
            .lock_conn()
            .map_err(|e| ReminderError::Store(e.to_string()))?;
        let mut stmt = conn
            .prepare("SELECT * FROM reminders WHERE id = ?1")
            .map_err(|e| ReminderError::Store(format!("get reminder prep: {e}")))?;
        let result = stmt
            .query_row(params![id], row_to_reminder)
            .optional()
            .map_err(|e| ReminderError::Store(format!("get reminder: {e}")))?;
        Ok(result)
    }

    fn update(&self, reminder: &Reminder) -> Result<(), ReminderError> {
        let conn = self
            .lock_conn()
            .map_err(|e| ReminderError::Store(e.to_string()))?;
        let commands_json = serde_json::to_string(&reminder.commands)
            .map_err(|e| ReminderError::Store(format!("commands: {e}")))?;
        let refs_json = serde_json::to_string(&reminder.refs)
            .map_err(|e| ReminderError::Store(format!("refs: {e}")))?;
        let schedule_json = reminder
            .schedule
            .as_ref()
            .map(|s| serde_json::to_string(s))
            .transpose()
            .map_err(|e| ReminderError::Store(format!("schedule: {e}")))?;
        let executions_json = serde_json::to_string(&reminder.executions)
            .map_err(|e| ReminderError::Store(format!("executions: {e}")))?;
        let tags_json = serde_json::to_string(&reminder.tags)
            .map_err(|e| ReminderError::Store(format!("tags: {e}")))?;
        let n = conn
            .execute(
                "UPDATE reminders SET
                    title = ?2, instructions = ?3, commands = ?4, refs = ?5,
                    priority = ?6, due_at = ?7, schedule = ?8, autonomous = ?9,
                    created_by = ?10, created_at = ?11, status = ?12,
                    snoozed_until = ?13, executions = ?14, tags = ?15
                 WHERE id = ?1",
                params![
                    reminder.id,
                    reminder.title,
                    reminder.instructions,
                    commands_json,
                    refs_json,
                    priority_to_i64(reminder.priority),
                    reminder.due_at.to_rfc3339(),
                    schedule_json,
                    if reminder.autonomous { 1_i64 } else { 0_i64 },
                    reminder.created_by,
                    reminder.created_at.to_rfc3339(),
                    reminder_status_to_str(reminder.status),
                    reminder.snoozed_until.map(|t| t.to_rfc3339()),
                    executions_json,
                    tags_json,
                ],
            )
            .map_err(|e| ReminderError::Store(format!("update reminder: {e}")))?;
        if n == 0 {
            return Err(ReminderError::NotFound(reminder.id.clone()));
        }
        Ok(())
    }

    fn delete(&self, id: &str) -> Result<bool, ReminderError> {
        let conn = self
            .lock_conn()
            .map_err(|e| ReminderError::Store(e.to_string()))?;
        let n = conn
            .execute("DELETE FROM reminders WHERE id = ?1", params![id])
            .map_err(|e| ReminderError::Store(format!("delete reminder: {e}")))?;
        Ok(n > 0)
    }

    fn list(&self, filter: &ReminderFilter) -> Result<Vec<Reminder>, ReminderError> {
        let conn = self
            .lock_conn()
            .map_err(|e| ReminderError::Store(e.to_string()))?;

        // Build SQL for the parts expressible at the DB layer.
        let mut sql = String::from("SELECT * FROM reminders WHERE 1=1");
        if filter.status.is_some() {
            sql.push_str(" AND status = :status");
        }
        if filter.priority_at_most.is_some() {
            sql.push_str(" AND priority <= :priority");
        }
        if filter.created_by.is_some() {
            sql.push_str(" AND created_by = :created_by");
        }
        if filter.due_before.is_some() {
            sql.push_str(" AND due_at <= :due_before");
        }
        sql.push_str(" ORDER BY priority ASC, due_at ASC");

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| ReminderError::Store(format!("list reminders prep: {e}")))?;

        let mut named: Vec<(&str, Box<dyn rusqlite::ToSql>)> = Vec::new();
        if let Some(s) = filter.status {
            named.push((":status", Box::new(reminder_status_to_str(s).to_string())));
        }
        if let Some(p) = filter.priority_at_most {
            named.push((":priority", Box::new(priority_to_i64(p))));
        }
        if let Some(ref cb) = filter.created_by {
            named.push((":created_by", Box::new(cb.clone())));
        }
        if let Some(due) = filter.due_before {
            named.push((":due_before", Box::new(due.to_rfc3339())));
        }

        let refs_kv: Vec<(&str, &dyn rusqlite::ToSql)> =
            named.iter().map(|(n, v)| (*n, v.as_ref())).collect();

        let rows = stmt
            .query_map(refs_kv.as_slice(), row_to_reminder)
            .map_err(|e| ReminderError::Store(format!("list reminders query: {e}")))?;

        let mut out = Vec::new();
        for r in rows {
            let reminder =
                r.map_err(|e| ReminderError::Store(format!("list reminders row: {e}")))?;
            // Post-filter for ref_id and tags (JSON columns).
            if let Some(ref rid) = filter.ref_id {
                if !reminder.refs.iter().any(|rf| &rf.id == rid) {
                    continue;
                }
            }
            if filter.tags.iter().any(|tag| !reminder.tags.contains(tag)) {
                continue;
            }
            out.push(reminder);
        }
        Ok(out)
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

        let ns = Namespace::default_ns();
        store.set_ref(&ns, "main", target).unwrap();
        assert_eq!(store.get_ref(&ns, "main").unwrap(), Some(target));

        // CAS success
        assert!(store.cas_ref(&ns, "main", target, new_target).unwrap());
        assert_eq!(store.get_ref(&ns, "main").unwrap(), Some(new_target));

        // CAS failure
        let stale = ObjectId::hash(b"stale");
        assert!(!store.cas_ref(&ns, "main", stale, target).unwrap());
    }

    #[test]
    fn test_list_refs() {
        let store = test_store();
        let ns = Namespace::default_ns();
        store.set_ref(&ns, "agents/a", ObjectId::hash(b"a")).unwrap();
        store.set_ref(&ns, "agents/b", ObjectId::hash(b"b")).unwrap();
        store.set_ref(&ns, "main", ObjectId::hash(b"m")).unwrap();

        let agents = store.list_refs(&ns, "agents/").unwrap();
        assert_eq!(agents.len(), 2);

        let all = store.list_refs(&ns, "").unwrap();
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
        let ns = Namespace::default_ns();
        store.set_ref(&ns, "main", commit.id).unwrap();

        // Read it all back
        let ref_target = store.get_ref(&ns, "main").unwrap().unwrap();
        assert_eq!(ref_target, commit.id);

        let retrieved_commit = store.get_commit(&ref_target).unwrap().unwrap();
        assert_eq!(retrieved_commit.state_root, id1);

        let retrieved_obj = store.get_object(&id1).unwrap().unwrap();
        assert_eq!(retrieved_obj, obj1);
    }

    // -----------------------------------------------------------------------
    // ReminderStore tests
    // -----------------------------------------------------------------------

    use agentstategraph_reminders::{
        Reminder, ReminderFilter, ReminderStore,
        types::{CreateReminder, Priority, ReminderRef, ReminderStatus, Schedule},
    };
    use chrono::Duration;

    fn future(secs: i64) -> chrono::DateTime<chrono::Utc> {
        chrono::Utc::now() + Duration::seconds(secs)
    }

    fn past_ts(secs: i64) -> chrono::DateTime<chrono::Utc> {
        chrono::Utc::now() - Duration::seconds(secs)
    }

    fn make_reminder(title: &str, secs_from_now: i64) -> Reminder {
        CreateReminder::new(title, "do something", future(secs_from_now), "agent/test")
            .into_reminder()
    }

    #[test]
    fn reminder_save_and_get_roundtrip() {
        let store = test_store();
        let r = make_reminder("check server", 600);
        store.save(&r).unwrap();
        let got = store.get(&r.id).unwrap().unwrap();
        assert_eq!(got.id, r.id);
        assert_eq!(got.title, "check server");
        assert_eq!(got.status, ReminderStatus::Pending);
        assert!(got.autonomous);
    }

    #[test]
    fn reminder_get_missing_returns_none() {
        let store = test_store();
        let result = store.get("nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn reminder_save_duplicate_id_fails() {
        let store = test_store();
        let r = make_reminder("dup", 60);
        store.save(&r).unwrap();
        let err = store.save(&r).unwrap_err();
        assert!(err.to_string().contains("insert reminder"));
    }

    #[test]
    fn reminder_update_roundtrip() {
        let store = test_store();
        let mut r = make_reminder("update me", 300);
        store.save(&r).unwrap();
        r.status = ReminderStatus::Due;
        r.title = "updated title".to_string();
        store.update(&r).unwrap();
        let got = store.get(&r.id).unwrap().unwrap();
        assert_eq!(got.status, ReminderStatus::Due);
        assert_eq!(got.title, "updated title");
    }

    #[test]
    fn reminder_update_missing_returns_not_found() {
        let store = test_store();
        let r = make_reminder("ghost", 60);
        let err = store.update(&r).unwrap_err();
        assert!(matches!(
            err,
            agentstategraph_reminders::ReminderError::NotFound(_)
        ));
    }

    #[test]
    fn reminder_delete_existing_returns_true() {
        let store = test_store();
        let r = make_reminder("delete me", 60);
        store.save(&r).unwrap();
        assert!(store.delete(&r.id).unwrap());
        assert!(store.get(&r.id).unwrap().is_none());
    }

    #[test]
    fn reminder_delete_missing_returns_false() {
        let store = test_store();
        assert!(!store.delete("ghost").unwrap());
    }

    #[test]
    fn reminder_list_empty_filter_returns_all() {
        let store = test_store();
        let r1 = make_reminder("a", 100);
        let r2 = make_reminder("b", 200);
        store.save(&r1).unwrap();
        store.save(&r2).unwrap();
        let all = store.list(&ReminderFilter::default()).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn reminder_list_filter_by_status() {
        let store = test_store();
        let r1 = make_reminder("pending", 100);
        let mut r2 = make_reminder("due", 50);
        r2.status = ReminderStatus::Due;
        store.save(&r1).unwrap();
        store.save(&r2).unwrap();

        let filter = ReminderFilter {
            status: Some(ReminderStatus::Due),
            ..Default::default()
        };
        let results = store.list(&filter).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "due");
    }

    #[test]
    fn reminder_list_filter_by_priority_at_most() {
        let store = test_store();
        let critical = CreateReminder::new("c", "i", future(100), "a")
            .with_priority(Priority::Critical)
            .into_reminder();
        let low = CreateReminder::new("l", "i", future(200), "a")
            .with_priority(Priority::Low)
            .into_reminder();
        store.save(&critical).unwrap();
        store.save(&low).unwrap();

        let filter = ReminderFilter {
            priority_at_most: Some(Priority::High),
            ..Default::default()
        };
        let results = store.list(&filter).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "c");
    }

    #[test]
    fn reminder_list_filter_by_created_by() {
        let store = test_store();
        let r1 = CreateReminder::new("r1", "i", future(60), "agent/alice").into_reminder();
        let r2 = CreateReminder::new("r2", "i", future(60), "agent/bob").into_reminder();
        store.save(&r1).unwrap();
        store.save(&r2).unwrap();

        let filter = ReminderFilter {
            created_by: Some("agent/alice".into()),
            ..Default::default()
        };
        let results = store.list(&filter).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "r1");
    }

    #[test]
    fn reminder_list_filter_by_due_before() {
        let store = test_store();
        let soon = CreateReminder::new("soon", "i", future(60), "a").into_reminder();
        let later = CreateReminder::new("later", "i", future(7200), "a").into_reminder();
        store.save(&soon).unwrap();
        store.save(&later).unwrap();

        let filter = ReminderFilter {
            due_before: Some(future(600)),
            ..Default::default()
        };
        let results = store.list(&filter).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "soon");
    }

    #[test]
    fn reminder_list_filter_by_ref_id() {
        let store = test_store();
        let mut with_ref = make_reminder("with-ref", 60);
        with_ref.refs = vec![ReminderRef::branch("main", "main branch")];
        let without = make_reminder("without-ref", 60);
        store.save(&with_ref).unwrap();
        store.save(&without).unwrap();

        let filter = ReminderFilter {
            ref_id: Some("main".into()),
            ..Default::default()
        };
        let results = store.list(&filter).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "with-ref");
    }

    #[test]
    fn reminder_list_filter_by_tags() {
        let store = test_store();
        let mut tagged = make_reminder("tagged", 60);
        tagged.tags = vec!["cleanup".into(), "server".into()];
        let untagged = make_reminder("untagged", 60);
        store.save(&tagged).unwrap();
        store.save(&untagged).unwrap();

        let filter = ReminderFilter {
            tags: vec!["cleanup".into()],
            ..Default::default()
        };
        let results = store.list(&filter).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "tagged");
    }

    #[test]
    fn reminder_list_ordered_by_priority_then_due() {
        let store = test_store();
        let medium_late = CreateReminder::new("medium-late", "i", future(200), "a")
            .with_priority(Priority::Medium)
            .into_reminder();
        let high_early = CreateReminder::new("high-early", "i", future(100), "a")
            .with_priority(Priority::High)
            .into_reminder();
        let high_later = CreateReminder::new("high-later", "i", future(300), "a")
            .with_priority(Priority::High)
            .into_reminder();
        store.save(&medium_late).unwrap();
        store.save(&high_early).unwrap();
        store.save(&high_later).unwrap();

        let all = store.list(&ReminderFilter::default()).unwrap();
        assert_eq!(all[0].title, "high-early");
        assert_eq!(all[1].title, "high-later");
        assert_eq!(all[2].title, "medium-late");
    }

    #[test]
    fn reminder_persists_schedule_and_refs() {
        let store = test_store();
        let mut r = CreateReminder::new("scheduled", "i", future(60), "a")
            .with_schedule(Schedule::Interval {
                every_seconds: 3600,
            })
            .with_refs(vec![ReminderRef::plan("plan-123", "My plan")])
            .into_reminder();
        r.autonomous = false;
        store.save(&r).unwrap();

        let got = store.get(&r.id).unwrap().unwrap();
        assert!(!got.autonomous);
        assert!(matches!(
            got.schedule,
            Some(Schedule::Interval {
                every_seconds: 3600
            })
        ));
        assert_eq!(got.refs.len(), 1);
        assert_eq!(got.refs[0].id, "plan-123");
        assert_eq!(got.refs[0].label.as_deref(), Some("My plan"));
    }

    #[test]
    fn reminder_persists_snoozed_until() {
        let store = test_store();
        let mut r = make_reminder("snooze me", 60);
        let snooze_time = future(3600);
        r.status = ReminderStatus::Snoozed;
        r.snoozed_until = Some(snooze_time);
        store.save(&r).unwrap();

        let got = store.get(&r.id).unwrap().unwrap();
        assert_eq!(got.status, ReminderStatus::Snoozed);
        assert!(got.snoozed_until.is_some());
        let diff = (got.snoozed_until.unwrap() - snooze_time)
            .num_seconds()
            .abs();
        assert!(diff < 2, "snoozed_until timestamp drifted by {diff}s");
    }

    #[test]
    fn reminder_all_statuses_roundtrip() {
        let store = test_store();
        let statuses = [
            ReminderStatus::Pending,
            ReminderStatus::Due,
            ReminderStatus::AwaitingPermission,
            ReminderStatus::InProgress,
            ReminderStatus::Completed,
            ReminderStatus::Snoozed,
            ReminderStatus::Cancelled,
        ];
        for status in statuses {
            let mut r = make_reminder(&format!("{status:?}"), 60);
            r.status = status;
            store.save(&r).unwrap();
            let got = store.get(&r.id).unwrap().unwrap();
            assert_eq!(got.status, status, "status roundtrip failed for {status:?}");
        }
    }

    #[test]
    fn reminder_all_priorities_roundtrip() {
        let store = test_store();
        let priorities = [
            Priority::Critical,
            Priority::High,
            Priority::Medium,
            Priority::Low,
            Priority::Minimal,
        ];
        for p in priorities {
            let r = CreateReminder::new(&format!("{p:?}"), "i", future(60), "a")
                .with_priority(p)
                .into_reminder();
            store.save(&r).unwrap();
            let got = store.get(&r.id).unwrap().unwrap();
            assert_eq!(got.priority, p, "priority roundtrip failed for {p:?}");
        }
    }
}
