//! SQLite storage backend — durable, single-file, zero-config.
//!
//! This is the default production backend. All state, commits, and refs
//! are stored in a single SQLite file that survives process restarts.

use std::path::Path;
use std::sync::Mutex;

use agentstategraph_core::{Commit, Epoch, EpochStatus, Object, ObjectId, Session, SessionStatus};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};

use crate::traits::{CommitStore, EpochStore, ObjectStore, RefStore, SessionStore, StorageError};

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
                status          TEXT NOT NULL DEFAULT 'Active',
                created_at      TEXT NOT NULL,
                ended_at        TEXT,
                metadata        TEXT NOT NULL DEFAULT '{}',
                commit_count    INTEGER NOT NULL DEFAULT 0
            );

            CREATE INDEX IF NOT EXISTS idx_commits_timestamp ON commits(timestamp DESC);
            CREATE INDEX IF NOT EXISTS idx_epochs_status ON epochs(status);
            CREATE INDEX IF NOT EXISTS idx_sessions_agent ON sessions(agent_id);
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
                Ok(Some(row_to_epoch(
                    id, desc, status, created_at, sealed_at, summary, ri, ag, tg, sc,
                )?))
            }
        }
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
        })
        .to_string();
        conn.execute(
            "INSERT OR REPLACE INTO sessions
             (id, agent_id, parent_id, scope_path, scope_branch, status,
              created_at, ended_at, metadata, commit_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
                     COALESCE((SELECT commit_count FROM sessions WHERE id = ?1), 0))",
            params![
                session.id,
                session.agent_id,
                session.parent_session,
                session.path_scope,
                session.working_branch,
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
                "SELECT id, agent_id, parent_id, scope_path, scope_branch, status,
                        created_at, ended_at, metadata
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
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, String>(8)?,
                ))
            })
            .map_err(|e| StorageError::Backend(format!("list sessions query: {}", e)))?;
        let mut out = Vec::new();
        for r in rows {
            let (id, agent, parent, sp, sb, st, ca, ea, md) =
                r.map_err(|e| StorageError::Backend(format!("list sessions row: {}", e)))?;
            out.push(row_to_session(id, agent, parent, sp, sb, st, ca, ea, md)?);
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
            String,
            String,
            Option<String>,
            String,
        );
        let row: Option<SessionRow> = conn
            .query_row(
                "SELECT id, agent_id, parent_id, scope_path, scope_branch, status,
                        created_at, ended_at, metadata
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
                    ))
                },
            )
            .optional()
            .map_err(|e| StorageError::Backend(format!("get session: {}", e)))?;
        match row {
            None => Ok(None),
            Some((id, agent, parent, sp, sb, st, ca, ea, md)) => Ok(Some(row_to_session(
                id, agent, parent, sp, sb, st, ca, ea, md,
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
