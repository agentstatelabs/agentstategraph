//! PostgreSQL storage backend — multi-tenant, connection-pooled.
//!
//! Uses tokio-postgres + deadpool for connection pooling.
//! Each tenant's data is isolated via a `tenant_id` column on every table.
//!
//! Usage:
//!   let storage = PostgresStorage::connect("postgres://localhost/agentstategraph").await?;
//!   let storage = PostgresStorage::connect_tenant("postgres://...", "tenant-123").await?;
//!
//! As of 0.6.75-beta.1 this backend persists epochs and sessions too,
//! with the same schema shape as SQLite (JSON-as-TEXT). Multi-tenant
//! isolation continues via a `tenant_id` column on every table.

use deadpool_postgres::{Config, ManagerConfig, Pool, PoolConfig, RecyclingMethod, Runtime};
use tokio_postgres::NoTls;

/// Default Postgres pool cap (v2-M1). Overridable via constructor or
/// the binary's `--pg-pool-size` flag.
pub const DEFAULT_POOL_SIZE: usize = 32;

use agentstategraph_core::{Commit, Epoch, EpochStatus, Object, ObjectId, Session, SessionStatus};
use chrono::{DateTime, Utc};

use crate::traits::{
    CommitStore, EpochStore, ObjectStore, RefStore, SessionStore, StorageError, TaintStore,
};

/// PostgreSQL-backed storage with connection pooling and optional multi-tenancy.
pub struct PostgresStorage {
    pool: Pool,
    tenant_id: String,
}

impl PostgresStorage {
    /// Connect to Postgres with the default tenant.
    pub async fn connect(database_url: &str) -> Result<Self, StorageError> {
        Self::connect_tenant(database_url, "default").await
    }

    /// Connect to Postgres with a specific tenant ID for data isolation.
    ///
    /// Applies the default pool size cap (`DEFAULT_POOL_SIZE`).
    pub async fn connect_tenant(database_url: &str, tenant_id: &str) -> Result<Self, StorageError> {
        Self::connect_tenant_with_pool_size(database_url, tenant_id, DEFAULT_POOL_SIZE).await
    }

    /// Connect to Postgres with a specific tenant ID and an explicit pool
    /// size cap (v2-M1). `max_size` is clamped to at least 1.
    pub async fn connect_tenant_with_pool_size(
        database_url: &str,
        tenant_id: &str,
        max_size: usize,
    ) -> Result<Self, StorageError> {
        let mut cfg = Config::new();
        cfg.url = Some(database_url.to_string());
        cfg.manager = Some(ManagerConfig {
            recycling_method: RecyclingMethod::Fast,
        });
        cfg.pool = Some(PoolConfig {
            max_size: max_size.max(1),
            ..Default::default()
        });

        let pool = cfg
            .create_pool(Some(Runtime::Tokio1), NoTls)
            .map_err(|e| StorageError::Backend(format!("postgres pool: {}", e)))?;

        let storage = Self {
            pool,
            tenant_id: tenant_id.to_string(),
        };
        storage.init_tables().await?;
        Ok(storage)
    }

    /// Return the configured pool's maximum size. Useful for tests.
    pub fn pool_max_size(&self) -> usize {
        self.pool.status().max_size
    }

    async fn init_tables(&self) -> Result<(), StorageError> {
        let client = self
            .pool
            .get()
            .await
            .map_err(|e| StorageError::Backend(format!("get conn: {}", e)))?;

        client
            .batch_execute(
                "
                CREATE TABLE IF NOT EXISTS objects (
                    tenant_id TEXT NOT NULL,
                    id        BYTEA NOT NULL,
                    data      JSONB NOT NULL,
                    PRIMARY KEY (tenant_id, id)
                );

                CREATE TABLE IF NOT EXISTS commits (
                    tenant_id TEXT NOT NULL,
                    id        BYTEA NOT NULL,
                    data      JSONB NOT NULL,
                    timestamp TIMESTAMPTZ NOT NULL,
                    PRIMARY KEY (tenant_id, id)
                );

                CREATE TABLE IF NOT EXISTS refs (
                    tenant_id TEXT NOT NULL,
                    name      TEXT NOT NULL,
                    target    BYTEA NOT NULL,
                    PRIMARY KEY (tenant_id, name)
                );

                CREATE INDEX IF NOT EXISTS idx_commits_tenant_ts
                    ON commits(tenant_id, timestamp DESC);

                CREATE TABLE IF NOT EXISTS epochs (
                    tenant_id       TEXT NOT NULL,
                    id              TEXT NOT NULL,
                    description     TEXT NOT NULL DEFAULT '',
                    status          TEXT NOT NULL DEFAULT 'Active',
                    created_at      TEXT NOT NULL,
                    sealed_at       TEXT,
                    summary         TEXT,
                    root_intents    TEXT NOT NULL DEFAULT '[]',
                    agents          TEXT NOT NULL DEFAULT '[]',
                    tags            TEXT NOT NULL DEFAULT '[]',
                    commit_count    INTEGER NOT NULL DEFAULT 0,
                    sealed_commits  TEXT NOT NULL DEFAULT '[]',
                    PRIMARY KEY (tenant_id, id)
                );

                CREATE TABLE IF NOT EXISTS sessions (
                    tenant_id       TEXT NOT NULL,
                    id              TEXT NOT NULL,
                    agent_id        TEXT NOT NULL,
                    parent_id       TEXT,
                    scope_path      TEXT,
                    scope_branch    TEXT,
                    status          TEXT NOT NULL DEFAULT 'Active',
                    created_at      TEXT NOT NULL,
                    ended_at        TEXT,
                    metadata        TEXT NOT NULL DEFAULT '{}',
                    commit_count    INTEGER NOT NULL DEFAULT 0,
                    PRIMARY KEY (tenant_id, id)
                );

                CREATE INDEX IF NOT EXISTS idx_epochs_tenant_status
                    ON epochs(tenant_id, status);
                CREATE INDEX IF NOT EXISTS idx_sessions_tenant_agent
                    ON sessions(tenant_id, agent_id);

                ALTER TABLE commits ADD COLUMN IF NOT EXISTS epoch_id TEXT;
                ALTER TABLE commits ADD COLUMN IF NOT EXISTS session_id TEXT;

                CREATE INDEX IF NOT EXISTS idx_commits_tenant_epoch
                    ON commits(tenant_id, epoch_id);
                CREATE INDEX IF NOT EXISTS idx_commits_tenant_session
                    ON commits(tenant_id, session_id);
                ",
            )
            .await
            .map_err(|e| StorageError::Backend(format!("init tables: {}", e)))?;

        Ok(())
    }

    /// Helper: run an async operation from a sync context.
    fn block_on<F: std::future::Future<Output = Result<T, StorageError>>, T>(
        &self,
        f: F,
    ) -> Result<T, StorageError> {
        tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(f))
    }
}

// ─── ObjectStore ────────────────────────────────────────────

impl ObjectStore for PostgresStorage {
    fn get_object(&self, id: &ObjectId) -> Result<Option<Object>, StorageError> {
        self.block_on(async {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| StorageError::Backend(format!("get conn: {}", e)))?;

            let row = client
                .query_opt(
                    "SELECT data FROM objects WHERE tenant_id = $1 AND id = $2",
                    &[&self.tenant_id, &id.as_bytes().as_slice()],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("get object: {}", e)))?;

            match row {
                Some(row) => {
                    let data: serde_json::Value = row.get("data");
                    let obj: Object = serde_json::from_value(data)
                        .map_err(|e| StorageError::Serialization(e.to_string()))?;
                    Ok(Some(obj))
                }
                None => Ok(None),
            }
        })
    }

    fn put_object(&self, obj: &Object) -> Result<ObjectId, StorageError> {
        let id = obj.id();
        let data =
            serde_json::to_value(obj).map_err(|e| StorageError::Serialization(e.to_string()))?;

        self.block_on(async {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| StorageError::Backend(format!("get conn: {}", e)))?;

            client
                .execute(
                    "INSERT INTO objects (tenant_id, id, data) VALUES ($1, $2, $3)
                     ON CONFLICT (tenant_id, id) DO NOTHING",
                    &[&self.tenant_id, &id.as_bytes().as_slice(), &data],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("put object: {}", e)))?;

            Ok(id)
        })
    }

    fn has_object(&self, id: &ObjectId) -> Result<bool, StorageError> {
        self.block_on(async {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| StorageError::Backend(format!("get conn: {}", e)))?;

            let row = client
                .query_one(
                    "SELECT EXISTS(SELECT 1 FROM objects WHERE tenant_id = $1 AND id = $2) as e",
                    &[&self.tenant_id, &id.as_bytes().as_slice()],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("has object: {}", e)))?;

            Ok(row.get::<_, bool>("e"))
        })
    }
}

// ─── CommitStore ────────────────────────────────────────────

impl CommitStore for PostgresStorage {
    fn get_commit(&self, id: &ObjectId) -> Result<Option<Commit>, StorageError> {
        self.block_on(async {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| StorageError::Backend(format!("get conn: {}", e)))?;

            let row = client
                .query_opt(
                    "SELECT data FROM commits WHERE tenant_id = $1 AND id = $2",
                    &[&self.tenant_id, &id.as_bytes().as_slice()],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("get commit: {}", e)))?;

            match row {
                Some(row) => {
                    let data: serde_json::Value = row.get("data");
                    let commit: Commit = serde_json::from_value(data)
                        .map_err(|e| StorageError::Serialization(e.to_string()))?;
                    Ok(Some(commit))
                }
                None => Ok(None),
            }
        })
    }

    fn put_commit(&self, commit: &Commit) -> Result<(), StorageError> {
        let data =
            serde_json::to_value(commit).map_err(|e| StorageError::Serialization(e.to_string()))?;
        let timestamp = commit.timestamp;

        self.block_on(async {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| StorageError::Backend(format!("get conn: {}", e)))?;

            client
                .execute(
                    "INSERT INTO commits (tenant_id, id, data, timestamp) VALUES ($1, $2, $3, $4)
                     ON CONFLICT (tenant_id, id) DO NOTHING",
                    &[
                        &self.tenant_id,
                        &commit.id.as_bytes().as_slice(),
                        &data,
                        &timestamp,
                    ],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("put commit: {}", e)))?;

            Ok(())
        })
    }

    fn has_commit(&self, id: &ObjectId) -> Result<bool, StorageError> {
        self.block_on(async {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| StorageError::Backend(format!("get conn: {}", e)))?;

            let row = client
                .query_one(
                    "SELECT EXISTS(SELECT 1 FROM commits WHERE tenant_id = $1 AND id = $2) as e",
                    &[&self.tenant_id, &id.as_bytes().as_slice()],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("has commit: {}", e)))?;

            Ok(row.get::<_, bool>("e"))
        })
    }

    fn list_commits(&self, from: &ObjectId, limit: usize) -> Result<Vec<Commit>, StorageError> {
        self.block_on(async {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| StorageError::Backend(format!("get conn: {}", e)))?;

            let mut result = Vec::new();
            let mut current = Some(*from);

            while let Some(id) = current {
                if result.len() >= limit {
                    break;
                }

                let row = client
                    .query_opt(
                        "SELECT data FROM commits WHERE tenant_id = $1 AND id = $2",
                        &[&self.tenant_id, &id.as_bytes().as_slice()],
                    )
                    .await
                    .map_err(|e| StorageError::Backend(format!("list commits: {}", e)))?;

                match row {
                    Some(row) => {
                        let data: serde_json::Value = row.get("data");
                        let commit: Commit = serde_json::from_value(data)
                            .map_err(|e| StorageError::Serialization(e.to_string()))?;
                        current = commit.parents.first().copied();
                        result.push(commit);
                    }
                    None => break,
                }
            }

            Ok(result)
        })
    }
}

// ─── RefStore ───────────────────────────────────────────────

impl RefStore for PostgresStorage {
    fn get_ref(&self, name: &str) -> Result<Option<ObjectId>, StorageError> {
        self.block_on(async {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| StorageError::Backend(format!("get conn: {}", e)))?;

            let row = client
                .query_opt(
                    "SELECT target FROM refs WHERE tenant_id = $1 AND name = $2",
                    &[&self.tenant_id, &name.to_string()],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("get ref: {}", e)))?;

            match row {
                Some(row) => {
                    let bytes: Vec<u8> = row.get("target");
                    let mut arr = [0u8; 32];
                    if bytes.len() == 32 {
                        arr.copy_from_slice(&bytes);
                    }
                    Ok(Some(ObjectId::from_bytes(arr)))
                }
                None => Ok(None),
            }
        })
    }

    fn set_ref(&self, name: &str, target: ObjectId) -> Result<(), StorageError> {
        self.block_on(async {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| StorageError::Backend(format!("get conn: {}", e)))?;

            client
                .execute(
                    "INSERT INTO refs (tenant_id, name, target) VALUES ($1, $2, $3)
                     ON CONFLICT (tenant_id, name) DO UPDATE SET target = $3",
                    &[
                        &self.tenant_id,
                        &name.to_string(),
                        &target.as_bytes().as_slice(),
                    ],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("set ref: {}", e)))?;

            Ok(())
        })
    }

    fn cas_ref(&self, name: &str, expected: ObjectId, new: ObjectId) -> Result<bool, StorageError> {
        self.block_on(async {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| StorageError::Backend(format!("get conn: {}", e)))?;

            let rows = client
                .execute(
                    "UPDATE refs SET target = $1
                     WHERE tenant_id = $2 AND name = $3 AND target = $4",
                    &[
                        &new.as_bytes().as_slice(),
                        &self.tenant_id,
                        &name.to_string(),
                        &expected.as_bytes().as_slice(),
                    ],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("cas ref: {}", e)))?;

            Ok(rows > 0)
        })
    }

    fn list_refs(&self, prefix: &str) -> Result<Vec<(String, ObjectId)>, StorageError> {
        self.block_on(async {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| StorageError::Backend(format!("get conn: {}", e)))?;

            let pattern = format!("{}%", prefix);
            let rows = client
                .query(
                    "SELECT name, target FROM refs
                     WHERE tenant_id = $1 AND name LIKE $2
                     ORDER BY name",
                    &[&self.tenant_id, &pattern],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("list refs: {}", e)))?;

            let mut result = Vec::new();
            for row in rows {
                let name: String = row.get("name");
                let bytes: Vec<u8> = row.get("target");
                let mut arr = [0u8; 32];
                if bytes.len() == 32 {
                    arr.copy_from_slice(&bytes);
                }
                result.push((name, ObjectId::from_bytes(arr)));
            }

            Ok(result)
        })
    }

    fn delete_ref(&self, name: &str) -> Result<bool, StorageError> {
        self.block_on(async {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| StorageError::Backend(format!("get conn: {}", e)))?;

            let rows = client
                .execute(
                    "DELETE FROM refs WHERE tenant_id = $1 AND name = $2",
                    &[&self.tenant_id, &name.to_string()],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("delete ref: {}", e)))?;

            Ok(rows > 0)
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers for Epoch / Session row assembly. Parallel to the SQLite backend.
// ---------------------------------------------------------------------------

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
        scope_tenant: None,
        status: SessionStatus::from_wire(&status),
        created_at: parse_rfc3339(&created_at)?,
        ended_at: ended_at.as_deref().map(parse_rfc3339).transpose()?,
    })
}

// ---------------------------------------------------------------------------
// EpochStore
// ---------------------------------------------------------------------------

impl EpochStore for PostgresStorage {
    fn create_epoch(&self, epoch: &Epoch) -> Result<(), StorageError> {
        let root_intents = serde_json::to_string(&epoch.root_intents)
            .map_err(|e| StorageError::Serialization(e.to_string()))?;
        let agents = serde_json::to_string(&epoch.agents)
            .map_err(|e| StorageError::Serialization(e.to_string()))?;
        let tags = serde_json::to_string(&epoch.tags)
            .map_err(|e| StorageError::Serialization(e.to_string()))?;
        let sealed_commits = serde_json::to_string(&epoch.sealed_commits)
            .map_err(|e| StorageError::Serialization(e.to_string()))?;

        self.block_on(async {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| StorageError::Backend(format!("get conn: {}", e)))?;

            client
                .execute(
                    "INSERT INTO epochs
                     (tenant_id, id, description, status, created_at, sealed_at,
                      summary, root_intents, agents, tags, commit_count, sealed_commits)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
                    &[
                        &self.tenant_id,
                        &epoch.id,
                        &epoch.description,
                        &epoch_status_str(&epoch.status),
                        &epoch.created_at.to_rfc3339(),
                        &epoch.sealed_at.map(|t| t.to_rfc3339()),
                        &epoch.seal_summary,
                        &root_intents,
                        &agents,
                        &tags,
                        &(epoch.commits.len() as i64),
                        &sealed_commits,
                    ],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("create epoch: {}", e)))?;
            Ok(())
        })
    }

    fn seal_epoch(
        &self,
        id: &str,
        summary: &str,
        sealed_at: DateTime<Utc>,
        sealed_commits: &[ObjectId],
    ) -> Result<(), StorageError> {
        let sc_json = serde_json::to_string(sealed_commits)
            .map_err(|e| StorageError::Serialization(e.to_string()))?;

        self.block_on(async {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| StorageError::Backend(format!("get conn: {}", e)))?;

            let row = client
                .query_opt(
                    "SELECT status FROM epochs WHERE tenant_id = $1 AND id = $2",
                    &[&self.tenant_id, &id],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("seal lookup: {}", e)))?;

            match row.as_ref().map(|r| r.get::<_, String>("status")) {
                None => Err(StorageError::Backend(format!("epoch not found: {}", id))),
                Some(s) if s == "Sealed" || s == "Archived" => {
                    Err(StorageError::EpochAlreadySealed { id: id.to_string() })
                }
                _ => {
                    client
                        .execute(
                            "UPDATE epochs
                             SET status = 'Sealed', sealed_at = $1, summary = $2,
                                 sealed_commits = $3
                             WHERE tenant_id = $4 AND id = $5",
                            &[
                                &sealed_at.to_rfc3339(),
                                &summary,
                                &sc_json,
                                &self.tenant_id,
                                &id,
                            ],
                        )
                        .await
                        .map_err(|e| StorageError::Backend(format!("seal epoch: {}", e)))?;
                    Ok(())
                }
            }
        })
    }

    fn list_epochs(&self) -> Result<Vec<Epoch>, StorageError> {
        self.block_on(async {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| StorageError::Backend(format!("get conn: {}", e)))?;

            let rows = client
                .query(
                    "SELECT id, description, status, created_at, sealed_at, summary,
                            root_intents, agents, tags, sealed_commits
                     FROM epochs
                     WHERE tenant_id = $1
                     ORDER BY created_at DESC",
                    &[&self.tenant_id],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("list epochs: {}", e)))?;

            let mut out = Vec::with_capacity(rows.len());
            for row in rows {
                out.push(row_to_epoch(
                    row.get("id"),
                    row.get("description"),
                    row.get("status"),
                    row.get("created_at"),
                    row.get("sealed_at"),
                    row.get("summary"),
                    row.get("root_intents"),
                    row.get("agents"),
                    row.get("tags"),
                    row.get("sealed_commits"),
                )?);
            }
            Ok(out)
        })
    }

    fn get_epoch(&self, id: &str) -> Result<Option<Epoch>, StorageError> {
        self.block_on(async {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| StorageError::Backend(format!("get conn: {}", e)))?;

            let row = client
                .query_opt(
                    "SELECT id, description, status, created_at, sealed_at, summary,
                            root_intents, agents, tags, sealed_commits
                     FROM epochs WHERE tenant_id = $1 AND id = $2",
                    &[&self.tenant_id, &id],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("get epoch: {}", e)))?;

            match row {
                None => Ok(None),
                Some(row) => Ok(Some(row_to_epoch(
                    row.get("id"),
                    row.get("description"),
                    row.get("status"),
                    row.get("created_at"),
                    row.get("sealed_at"),
                    row.get("summary"),
                    row.get("root_intents"),
                    row.get("agents"),
                    row.get("tags"),
                    row.get("sealed_commits"),
                )?)),
            }
        })
    }

    fn set_commit_epoch(&self, commit_id: &ObjectId, epoch_id: &str) -> Result<(), StorageError> {
        self.block_on(async {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| StorageError::Backend(format!("get conn: {}", e)))?;

            let row = client
                .query_opt(
                    "SELECT status FROM epochs WHERE tenant_id = $1 AND id = $2",
                    &[&self.tenant_id, &epoch_id],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("assoc lookup: {}", e)))?;

            match row.as_ref().map(|r| r.get::<_, String>("status")) {
                None => {
                    return Err(StorageError::Backend(format!(
                        "epoch not found: {}",
                        epoch_id
                    )));
                }
                Some(s) if s == "Sealed" || s == "Archived" => {
                    return Err(StorageError::EpochAlreadySealed {
                        id: epoch_id.to_string(),
                    });
                }
                _ => {}
            }

            client
                .execute(
                    "UPDATE commits SET epoch_id = $1 WHERE tenant_id = $2 AND id = $3",
                    &[&epoch_id, &self.tenant_id, &commit_id.as_bytes().as_slice()],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("set commit epoch: {}", e)))?;

            client
                .execute(
                    "UPDATE epochs SET commit_count = (
                         SELECT COUNT(*) FROM commits
                          WHERE tenant_id = $1 AND epoch_id = epochs.id
                     )
                     WHERE tenant_id = $1 AND id = $2",
                    &[&self.tenant_id, &epoch_id],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("update commit_count: {}", e)))?;

            Ok(())
        })
    }
}

// ---------------------------------------------------------------------------
// SessionStore
// ---------------------------------------------------------------------------

impl SessionStore for PostgresStorage {
    fn create_session(&self, session: &Session) -> Result<(), StorageError> {
        let metadata = serde_json::json!({
            "head": session.head,
            "delegated_intent": session.delegated_intent,
            "report_to": session.report_to,
            "working_branch": session.working_branch,
        })
        .to_string();

        self.block_on(async {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| StorageError::Backend(format!("get conn: {}", e)))?;

            client
                .execute(
                    "INSERT INTO sessions
                     (tenant_id, id, agent_id, parent_id, scope_path, scope_branch,
                      status, created_at, ended_at, metadata, commit_count)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
                             COALESCE(
                                 (SELECT commit_count FROM sessions
                                  WHERE tenant_id = $1 AND id = $2),
                                 0))
                     ON CONFLICT (tenant_id, id) DO UPDATE
                       SET agent_id = EXCLUDED.agent_id,
                           parent_id = EXCLUDED.parent_id,
                           scope_path = EXCLUDED.scope_path,
                           scope_branch = EXCLUDED.scope_branch,
                           status = EXCLUDED.status,
                           created_at = EXCLUDED.created_at,
                           ended_at = EXCLUDED.ended_at,
                           metadata = EXCLUDED.metadata",
                    &[
                        &self.tenant_id,
                        &session.id,
                        &session.agent_id,
                        &session.parent_session,
                        &session.path_scope,
                        &session.working_branch,
                        &session.status.as_str(),
                        &session.created_at.to_rfc3339(),
                        &session.ended_at.map(|t| t.to_rfc3339()),
                        &metadata,
                    ],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("create session: {}", e)))?;
            Ok(())
        })
    }

    fn end_session(
        &self,
        id: &str,
        status: SessionStatus,
        ended_at: DateTime<Utc>,
    ) -> Result<(), StorageError> {
        self.block_on(async {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| StorageError::Backend(format!("get conn: {}", e)))?;

            let row = client
                .query_opt(
                    "SELECT status FROM sessions WHERE tenant_id = $1 AND id = $2",
                    &[&self.tenant_id, &id],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("end session lookup: {}", e)))?;

            match row.as_ref().map(|r| r.get::<_, String>("status")) {
                None => Err(StorageError::Backend(format!("session not found: {}", id))),
                Some(s) if s != "Active" => Err(StorageError::SessionEnded { id: id.to_string() }),
                _ => {
                    client
                        .execute(
                            "UPDATE sessions SET status = $1, ended_at = $2
                             WHERE tenant_id = $3 AND id = $4",
                            &[
                                &status.as_str(),
                                &ended_at.to_rfc3339(),
                                &self.tenant_id,
                                &id,
                            ],
                        )
                        .await
                        .map_err(|e| StorageError::Backend(format!("end session: {}", e)))?;
                    Ok(())
                }
            }
        })
    }

    fn list_sessions(&self, agent_filter: Option<&str>) -> Result<Vec<Session>, StorageError> {
        self.block_on(async {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| StorageError::Backend(format!("get conn: {}", e)))?;

            let rows = client
                .query(
                    "SELECT id, agent_id, parent_id, scope_path, scope_branch, status,
                            created_at, ended_at, metadata
                     FROM sessions
                     WHERE tenant_id = $1 AND ($2::text IS NULL OR agent_id = $2)
                     ORDER BY created_at DESC",
                    &[&self.tenant_id, &agent_filter],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("list sessions: {}", e)))?;

            let mut out = Vec::with_capacity(rows.len());
            for row in rows {
                out.push(row_to_session(
                    row.get("id"),
                    row.get("agent_id"),
                    row.get("parent_id"),
                    row.get("scope_path"),
                    row.get("scope_branch"),
                    row.get("status"),
                    row.get("created_at"),
                    row.get("ended_at"),
                    row.get("metadata"),
                )?);
            }
            Ok(out)
        })
    }

    fn get_session(&self, id: &str) -> Result<Option<Session>, StorageError> {
        self.block_on(async {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| StorageError::Backend(format!("get conn: {}", e)))?;

            let row = client
                .query_opt(
                    "SELECT id, agent_id, parent_id, scope_path, scope_branch, status,
                            created_at, ended_at, metadata
                     FROM sessions WHERE tenant_id = $1 AND id = $2",
                    &[&self.tenant_id, &id],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("get session: {}", e)))?;

            match row {
                None => Ok(None),
                Some(row) => Ok(Some(row_to_session(
                    row.get("id"),
                    row.get("agent_id"),
                    row.get("parent_id"),
                    row.get("scope_path"),
                    row.get("scope_branch"),
                    row.get("status"),
                    row.get("created_at"),
                    row.get("ended_at"),
                    row.get("metadata"),
                )?)),
            }
        })
    }

    fn set_commit_session(
        &self,
        commit_id: &ObjectId,
        session_id: &str,
    ) -> Result<(), StorageError> {
        self.block_on(async {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| StorageError::Backend(format!("get conn: {}", e)))?;

            let row = client
                .query_opt(
                    "SELECT status FROM sessions WHERE tenant_id = $1 AND id = $2",
                    &[&self.tenant_id, &session_id],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("assoc lookup: {}", e)))?;

            match row.as_ref().map(|r| r.get::<_, String>("status")) {
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

            client
                .execute(
                    "UPDATE commits SET session_id = $1 WHERE tenant_id = $2 AND id = $3",
                    &[
                        &session_id,
                        &self.tenant_id,
                        &commit_id.as_bytes().as_slice(),
                    ],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("set commit session: {}", e)))?;

            client
                .execute(
                    "UPDATE sessions SET commit_count = (
                         SELECT COUNT(*) FROM commits
                          WHERE tenant_id = $1 AND session_id = sessions.id
                     )
                     WHERE tenant_id = $1 AND id = $2",
                    &[&self.tenant_id, &session_id],
                )
                .await
                .map_err(|e| {
                    StorageError::Backend(format!("update session commit_count: {}", e))
                })?;

            Ok(())
        })
    }
}

/// Stub TaintStore impl for Postgres. The taint substrate landed in
/// 0.7.75 before the Postgres CI path was active; wiring the real
/// SQL goes in the next milestone's post-production queue. For now
/// every method returns a `Backend` error so callers see a clear
/// signal instead of a silent miscompile.
impl TaintStore for PostgresStorage {
    fn create_taint(&self, _taint: &agentstategraph_taint::Taint) -> Result<(), StorageError> {
        Err(StorageError::Backend(
            "taint-store not yet implemented for PostgresStorage".into(),
        ))
    }

    fn resolve_taint(
        &self,
        _id: &str,
        _resolved_by: &str,
        _reason: &str,
        _proof: Option<&str>,
        _resolved_at: DateTime<Utc>,
    ) -> Result<(), StorageError> {
        Err(StorageError::Backend(
            "taint-store not yet implemented for PostgresStorage".into(),
        ))
    }

    fn list_taints(
        &self,
        _path_prefix: Option<&str>,
        _kind: Option<agentstategraph_taint::TaintKind>,
        _include_resolved: bool,
    ) -> Result<Vec<agentstategraph_taint::Taint>, StorageError> {
        Ok(Vec::new())
    }

    fn check_taint(
        &self,
        _request_path: &str,
    ) -> Result<Vec<agentstategraph_taint::Taint>, StorageError> {
        Ok(Vec::new())
    }

    fn get_taint(&self, _id: &str) -> Result<Option<agentstategraph_taint::Taint>, StorageError> {
        Ok(None)
    }

    fn set_taint_commit_id(&self, _id: &str, _commit_id: &str) -> Result<(), StorageError> {
        Err(StorageError::Backend(
            "taint-store not yet implemented for PostgresStorage".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies the constructor applies the supplied `max_size` to the
    /// underlying deadpool. Does NOT open a Postgres connection —
    /// `create_pool` only validates config.
    #[test]
    fn connect_with_pool_size_applies_cap() {
        let mut cfg = Config::new();
        cfg.url = Some("postgres://localhost/agentstategraph_unused".to_string());
        cfg.manager = Some(ManagerConfig {
            recycling_method: RecyclingMethod::Fast,
        });
        cfg.pool = Some(PoolConfig {
            max_size: 7,
            ..Default::default()
        });
        let pool = cfg
            .create_pool(Some(Runtime::Tokio1), NoTls)
            .expect("build pool");
        assert_eq!(pool.status().max_size, 7);
    }

    #[test]
    fn default_pool_size_is_32() {
        assert_eq!(DEFAULT_POOL_SIZE, 32);
    }
}
