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

                CREATE TABLE IF NOT EXISTS taints (
                    tenant_id       TEXT NOT NULL,
                    id              TEXT NOT NULL,
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
                    propagate       BOOLEAN NOT NULL DEFAULT true,
                    metadata        TEXT NOT NULL DEFAULT '{}',
                    PRIMARY KEY (tenant_id, id)
                );

                CREATE INDEX IF NOT EXISTS idx_epochs_tenant_status
                    ON epochs(tenant_id, status);
                CREATE INDEX IF NOT EXISTS idx_sessions_tenant_agent
                    ON sessions(tenant_id, agent_id);
                CREATE TABLE IF NOT EXISTS reminders (
                    tenant_id       TEXT NOT NULL,
                    id              TEXT NOT NULL,
                    title           TEXT NOT NULL,
                    instructions    TEXT NOT NULL,
                    commands        TEXT NOT NULL DEFAULT '[]',
                    refs            TEXT NOT NULL DEFAULT '[]',
                    priority        INTEGER NOT NULL DEFAULT 3,
                    due_at          TEXT NOT NULL,
                    schedule        TEXT,
                    autonomous      BOOLEAN NOT NULL DEFAULT true,
                    created_by      TEXT NOT NULL,
                    created_at      TEXT NOT NULL,
                    status          TEXT NOT NULL DEFAULT 'pending',
                    snoozed_until   TEXT,
                    executions      TEXT NOT NULL DEFAULT '[]',
                    tags            TEXT NOT NULL DEFAULT '[]',
                    PRIMARY KEY (tenant_id, id)
                );

                CREATE INDEX IF NOT EXISTS idx_taints_tenant_path
                    ON taints(tenant_id, path);
                CREATE INDEX IF NOT EXISTS idx_taints_tenant_kind
                    ON taints(tenant_id, kind);
                CREATE UNIQUE INDEX IF NOT EXISTS idx_taints_unique_active
                    ON taints(tenant_id, path, name, kind) WHERE resolved_at IS NULL;
                CREATE INDEX IF NOT EXISTS idx_reminders_tenant_status
                    ON reminders(tenant_id, status);
                CREATE INDEX IF NOT EXISTS idx_reminders_tenant_due
                    ON reminders(tenant_id, due_at);
                CREATE INDEX IF NOT EXISTS idx_reminders_tenant_priority
                    ON reminders(tenant_id, priority);
                CREATE INDEX IF NOT EXISTS idx_reminders_tenant_created_by
                    ON reminders(tenant_id, created_by);

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

// ---------------------------------------------------------------------------
// TaintStore for PostgresStorage (0.7.75-beta.2)
// ---------------------------------------------------------------------------

fn pg_kind_to_str(k: agentstategraph_taint::TaintKind) -> &'static str {
    match k {
        agentstategraph_taint::TaintKind::Taint => "taint",
        agentstategraph_taint::TaintKind::Quarantine => "quarantine",
        agentstategraph_taint::TaintKind::Watch => "watch",
    }
}

fn pg_kind_from_str(s: &str) -> Result<agentstategraph_taint::TaintKind, StorageError> {
    match s {
        "taint" => Ok(agentstategraph_taint::TaintKind::Taint),
        "quarantine" => Ok(agentstategraph_taint::TaintKind::Quarantine),
        "watch" => Ok(agentstategraph_taint::TaintKind::Watch),
        other => Err(StorageError::Backend(format!(
            "unknown taint kind: {other}"
        ))),
    }
}

fn pg_effect_to_str(e: agentstategraph_taint::TaintEffect) -> &'static str {
    match e {
        agentstategraph_taint::TaintEffect::Warn => "warn",
        agentstategraph_taint::TaintEffect::Block => "block",
        agentstategraph_taint::TaintEffect::Review => "review",
        agentstategraph_taint::TaintEffect::Isolate => "isolate",
        agentstategraph_taint::TaintEffect::Advisory => "advisory",
    }
}

fn pg_effect_from_str(s: &str) -> Result<agentstategraph_taint::TaintEffect, StorageError> {
    match s {
        "warn" => Ok(agentstategraph_taint::TaintEffect::Warn),
        "block" => Ok(agentstategraph_taint::TaintEffect::Block),
        "review" => Ok(agentstategraph_taint::TaintEffect::Review),
        "isolate" => Ok(agentstategraph_taint::TaintEffect::Isolate),
        "advisory" => Ok(agentstategraph_taint::TaintEffect::Advisory),
        other => Err(StorageError::Backend(format!(
            "unknown taint effect: {other}"
        ))),
    }
}

fn pg_severity_to_str(s: agentstategraph_taint::TaintSeverity) -> &'static str {
    match s {
        agentstategraph_taint::TaintSeverity::Low => "low",
        agentstategraph_taint::TaintSeverity::Medium => "medium",
        agentstategraph_taint::TaintSeverity::High => "high",
        agentstategraph_taint::TaintSeverity::Critical => "critical",
    }
}

fn pg_severity_from_str(s: &str) -> Result<agentstategraph_taint::TaintSeverity, StorageError> {
    match s {
        "low" => Ok(agentstategraph_taint::TaintSeverity::Low),
        "medium" => Ok(agentstategraph_taint::TaintSeverity::Medium),
        "high" => Ok(agentstategraph_taint::TaintSeverity::High),
        "critical" => Ok(agentstategraph_taint::TaintSeverity::Critical),
        other => Err(StorageError::Backend(format!(
            "unknown taint severity: {other}"
        ))),
    }
}

fn pg_row_to_taint(
    row: &tokio_postgres::Row,
) -> Result<agentstategraph_taint::Taint, StorageError> {
    use agentstategraph_taint::TaintMetadata;
    let kind: String = row.get("kind");
    let effect: String = row.get("effect");
    let severity: String = row.get("severity");
    let metadata_raw: String = row.get("metadata");
    let created_at_s: String = row.get("created_at");
    let expires_at_s: Option<String> = row.get("expires_at");
    let resolved_at_s: Option<String> = row.get("resolved_at");

    let metadata: TaintMetadata = serde_json::from_str(&metadata_raw)
        .map_err(|e| StorageError::Serialization(format!("taint metadata: {e}")))?;
    let created_at = DateTime::parse_from_rfc3339(&created_at_s)
        .map_err(|e| StorageError::Serialization(format!("created_at: {e}")))?
        .with_timezone(&Utc);
    let expires_at = match expires_at_s {
        Some(s) => Some(
            DateTime::parse_from_rfc3339(&s)
                .map_err(|e| StorageError::Serialization(format!("expires_at: {e}")))?
                .with_timezone(&Utc),
        ),
        None => None,
    };
    let resolved_at = match resolved_at_s {
        Some(s) => Some(
            DateTime::parse_from_rfc3339(&s)
                .map_err(|e| StorageError::Serialization(format!("resolved_at: {e}")))?
                .with_timezone(&Utc),
        ),
        None => None,
    };

    Ok(agentstategraph_taint::Taint {
        id: row.get("id"),
        path: row.get("path"),
        name: row.get("name"),
        kind: pg_kind_from_str(&kind)?,
        effect: pg_effect_from_str(&effect)?,
        severity: pg_severity_from_str(&severity)?,
        reason: row.get("reason"),
        agent_id: row.get("agent_id"),
        commit_id: row.get("commit_id"),
        created_at,
        expires_at,
        resolved_at,
        resolved_by: row.get("resolved_by"),
        resolved_reason: row.get("resolved_reason"),
        resolved_proof: row.get("resolved_proof"),
        propagate: row.get("propagate"),
        metadata,
    })
}

impl TaintStore for PostgresStorage {
    fn create_taint(&self, taint: &agentstategraph_taint::Taint) -> Result<(), StorageError> {
        self.block_on(async {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| StorageError::Backend(format!("get conn: {e}")))?;
            let metadata_json = serde_json::to_string(&taint.metadata)
                .map_err(|e| StorageError::Serialization(format!("taint metadata: {e}")))?;
            let expires = taint.expires_at.map(|t| t.to_rfc3339());
            let resolved_at = taint.resolved_at.map(|t| t.to_rfc3339());
            client
                .execute(
                    "INSERT INTO taints (
                        tenant_id, id, path, name, kind, effect, severity, reason,
                        agent_id, commit_id, created_at, expires_at, resolved_at,
                        resolved_by, resolved_reason, resolved_proof, propagate, metadata
                    ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18)",
                    &[
                        &self.tenant_id,
                        &taint.id,
                        &taint.path,
                        &taint.name,
                        &pg_kind_to_str(taint.kind),
                        &pg_effect_to_str(taint.effect),
                        &pg_severity_to_str(taint.severity),
                        &taint.reason,
                        &taint.agent_id,
                        &taint.commit_id,
                        &taint.created_at.to_rfc3339(),
                        &expires,
                        &resolved_at,
                        &taint.resolved_by,
                        &taint.resolved_reason,
                        &taint.resolved_proof,
                        &taint.propagate,
                        &metadata_json,
                    ],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("insert taint: {e}")))?;
            Ok(())
        })
    }

    fn resolve_taint(
        &self,
        id: &str,
        resolved_by: &str,
        reason: &str,
        proof: Option<&str>,
        resolved_at: DateTime<Utc>,
    ) -> Result<(), StorageError> {
        self.block_on(async {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| StorageError::Backend(format!("get conn: {e}")))?;
            // Guard: fail on already-resolved / not-found. Partial
            // unique index would force failures on re-activation;
            // we want explicit control here so the error is legible.
            let row = client
                .query_opt(
                    "SELECT resolved_at FROM taints WHERE tenant_id = $1 AND id = $2",
                    &[&self.tenant_id, &id],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("resolve check: {e}")))?
                .ok_or_else(|| StorageError::Backend(format!("taint {id} not found")))?;
            let already: Option<String> = row.get(0);
            if already.is_some() {
                return Err(StorageError::Backend(format!(
                    "taint {id} is already resolved"
                )));
            }
            let proof_owned = proof.map(str::to_string);
            client
                .execute(
                    "UPDATE taints
                        SET resolved_at = $3, resolved_by = $4,
                            resolved_reason = $5, resolved_proof = $6
                      WHERE tenant_id = $1 AND id = $2",
                    &[
                        &self.tenant_id,
                        &id,
                        &resolved_at.to_rfc3339(),
                        &resolved_by,
                        &reason,
                        &proof_owned,
                    ],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("resolve taint: {e}")))?;
            Ok(())
        })
    }

    fn list_taints(
        &self,
        path_prefix: Option<&str>,
        kind: Option<agentstategraph_taint::TaintKind>,
        include_resolved: bool,
    ) -> Result<Vec<agentstategraph_taint::Taint>, StorageError> {
        self.block_on(async {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| StorageError::Backend(format!("get conn: {e}")))?;
            // Build dynamic WHERE without sacrificing parameterization:
            // we always pass tenant_id + optional prefix/plike/kind.
            let prefix = path_prefix.map(|p| p.trim_end_matches('/').to_string());
            let plike = prefix.as_ref().map(|p| format!("{p}/%"));
            let kind_str = kind.map(|k| pg_kind_to_str(k).to_string());

            let rows = match (prefix.as_ref(), kind_str.as_ref(), include_resolved) {
                (None, None, true) => {
                    client
                        .query(
                            "SELECT * FROM taints WHERE tenant_id = $1
                              ORDER BY created_at DESC",
                            &[&self.tenant_id],
                        )
                        .await
                }
                (None, None, false) => {
                    client
                        .query(
                            "SELECT * FROM taints
                              WHERE tenant_id = $1 AND resolved_at IS NULL
                              ORDER BY created_at DESC",
                            &[&self.tenant_id],
                        )
                        .await
                }
                (Some(p), None, true) => {
                    client
                        .query(
                            "SELECT * FROM taints
                              WHERE tenant_id = $1 AND (path = $2 OR path LIKE $3)
                              ORDER BY created_at DESC",
                            &[&self.tenant_id, p, plike.as_ref().unwrap()],
                        )
                        .await
                }
                (Some(p), None, false) => {
                    client
                        .query(
                            "SELECT * FROM taints
                              WHERE tenant_id = $1 AND resolved_at IS NULL
                                AND (path = $2 OR path LIKE $3)
                              ORDER BY created_at DESC",
                            &[&self.tenant_id, p, plike.as_ref().unwrap()],
                        )
                        .await
                }
                (None, Some(k), true) => {
                    client
                        .query(
                            "SELECT * FROM taints
                              WHERE tenant_id = $1 AND kind = $2
                              ORDER BY created_at DESC",
                            &[&self.tenant_id, k],
                        )
                        .await
                }
                (None, Some(k), false) => {
                    client
                        .query(
                            "SELECT * FROM taints
                              WHERE tenant_id = $1 AND resolved_at IS NULL AND kind = $2
                              ORDER BY created_at DESC",
                            &[&self.tenant_id, k],
                        )
                        .await
                }
                (Some(p), Some(k), true) => {
                    client
                        .query(
                            "SELECT * FROM taints
                              WHERE tenant_id = $1 AND kind = $2
                                AND (path = $3 OR path LIKE $4)
                              ORDER BY created_at DESC",
                            &[&self.tenant_id, k, p, plike.as_ref().unwrap()],
                        )
                        .await
                }
                (Some(p), Some(k), false) => {
                    client
                        .query(
                            "SELECT * FROM taints
                              WHERE tenant_id = $1 AND resolved_at IS NULL AND kind = $2
                                AND (path = $3 OR path LIKE $4)
                              ORDER BY created_at DESC",
                            &[&self.tenant_id, k, p, plike.as_ref().unwrap()],
                        )
                        .await
                }
            }
            .map_err(|e| StorageError::Backend(format!("list_taints: {e}")))?;

            let mut out = Vec::with_capacity(rows.len());
            for row in rows {
                out.push(pg_row_to_taint(&row)?);
            }
            Ok(out)
        })
    }

    fn check_taint(
        &self,
        request_path: &str,
    ) -> Result<Vec<agentstategraph_taint::Taint>, StorageError> {
        self.block_on(async {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| StorageError::Backend(format!("get conn: {e}")))?;
            let now = Utc::now().to_rfc3339();
            // Ancestor match: exact OR (propagate=true AND request_path
            // LIKE path || '/%'). Postgres LIKE is case-sensitive.
            let rows = client
                .query(
                    "SELECT * FROM taints
                      WHERE tenant_id = $1
                        AND resolved_at IS NULL
                        AND (expires_at IS NULL OR expires_at > $2)
                        AND (path = $3 OR (propagate AND $3 LIKE path || '/%'))
                      ORDER BY created_at DESC",
                    &[&self.tenant_id, &now, &request_path],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("check_taint: {e}")))?;
            let mut out = Vec::with_capacity(rows.len());
            for row in rows {
                out.push(pg_row_to_taint(&row)?);
            }
            Ok(out)
        })
    }

    fn get_taint(&self, id: &str) -> Result<Option<agentstategraph_taint::Taint>, StorageError> {
        self.block_on(async {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| StorageError::Backend(format!("get conn: {e}")))?;
            let row = client
                .query_opt(
                    "SELECT * FROM taints WHERE tenant_id = $1 AND id = $2",
                    &[&self.tenant_id, &id],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("get_taint: {e}")))?;
            match row {
                Some(r) => Ok(Some(pg_row_to_taint(&r)?)),
                None => Ok(None),
            }
        })
    }

    fn set_taint_commit_id(&self, id: &str, commit_id: &str) -> Result<(), StorageError> {
        self.block_on(async {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| StorageError::Backend(format!("get conn: {e}")))?;
            client
                .execute(
                    "UPDATE taints SET commit_id = $3
                      WHERE tenant_id = $1 AND id = $2 AND resolved_at IS NULL",
                    &[&self.tenant_id, &id, &commit_id],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("set_taint_commit_id: {e}")))?;
            Ok(())
        })
    }
}

// ---------------------------------------------------------------------------
// ReminderStore — durable Postgres reminder storage
// ---------------------------------------------------------------------------

fn pg_reminder_status_to_str(
    s: agentstategraph_reminders::types::ReminderStatus,
) -> &'static str {
    use agentstategraph_reminders::types::ReminderStatus;
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

fn pg_reminder_status_from_str(
    s: &str,
) -> Result<agentstategraph_reminders::types::ReminderStatus, StorageError> {
    use agentstategraph_reminders::types::ReminderStatus;
    match s {
        "pending" => Ok(ReminderStatus::Pending),
        "due" => Ok(ReminderStatus::Due),
        "awaiting_permission" => Ok(ReminderStatus::AwaitingPermission),
        "in_progress" => Ok(ReminderStatus::InProgress),
        "completed" => Ok(ReminderStatus::Completed),
        "snoozed" => Ok(ReminderStatus::Snoozed),
        "cancelled" => Ok(ReminderStatus::Cancelled),
        other => Err(StorageError::Backend(format!(
            "unknown reminder status: {other}"
        ))),
    }
}

fn pg_priority_to_i32(p: agentstategraph_reminders::types::Priority) -> i32 {
    p.as_u8() as i32
}

fn pg_priority_from_i32(
    n: i32,
) -> Result<agentstategraph_reminders::types::Priority, StorageError> {
    use agentstategraph_reminders::types::Priority;
    match n {
        1 => Ok(Priority::Critical),
        2 => Ok(Priority::High),
        3 => Ok(Priority::Medium),
        4 => Ok(Priority::Low),
        5 => Ok(Priority::Minimal),
        other => Err(StorageError::Backend(format!(
            "unknown priority value: {other}"
        ))),
    }
}

fn pg_row_to_reminder(
    row: &tokio_postgres::Row,
) -> Result<agentstategraph_reminders::Reminder, StorageError> {
    use agentstategraph_reminders::types::ExecutionRecord;
    use agentstategraph_reminders::{Reminder, types::ReminderRef, types::Schedule};

    let status_s: String = row.get("status");
    let priority_n: i32 = row.get("priority");
    let due_at_s: String = row.get("due_at");
    let created_at_s: String = row.get("created_at");
    let snoozed_until_s: Option<String> = row.get("snoozed_until");
    let commands_s: String = row.get("commands");
    let refs_s: String = row.get("refs");
    let schedule_s: Option<String> = row.get("schedule");
    let executions_s: String = row.get("executions");
    let tags_s: String = row.get("tags");
    let autonomous: bool = row.get("autonomous");

    let status = pg_reminder_status_from_str(&status_s)?;
    let priority = pg_priority_from_i32(priority_n)?;
    let due_at = DateTime::parse_from_rfc3339(&due_at_s)
        .map_err(|e| StorageError::Serialization(format!("due_at: {e}")))?
        .with_timezone(&Utc);
    let created_at = DateTime::parse_from_rfc3339(&created_at_s)
        .map_err(|e| StorageError::Serialization(format!("created_at: {e}")))?
        .with_timezone(&Utc);
    let snoozed_until = match snoozed_until_s {
        Some(s) => Some(
            DateTime::parse_from_rfc3339(&s)
                .map_err(|e| StorageError::Serialization(format!("snoozed_until: {e}")))?
                .with_timezone(&Utc),
        ),
        None => None,
    };
    let commands: Vec<String> = serde_json::from_str(&commands_s)
        .map_err(|e| StorageError::Serialization(format!("commands: {e}")))?;
    let refs: Vec<ReminderRef> = serde_json::from_str(&refs_s)
        .map_err(|e| StorageError::Serialization(format!("refs: {e}")))?;
    let schedule: Option<Schedule> = match schedule_s {
        Some(s) => Some(
            serde_json::from_str(&s)
                .map_err(|e| StorageError::Serialization(format!("schedule: {e}")))?,
        ),
        None => None,
    };
    let executions: Vec<ExecutionRecord> = serde_json::from_str(&executions_s)
        .map_err(|e| StorageError::Serialization(format!("executions: {e}")))?;
    let tags: Vec<String> = serde_json::from_str(&tags_s)
        .map_err(|e| StorageError::Serialization(format!("tags: {e}")))?;

    Ok(Reminder {
        id: row.get("id"),
        title: row.get("title"),
        instructions: row.get("instructions"),
        commands,
        refs,
        priority,
        due_at,
        schedule,
        autonomous,
        created_by: row.get("created_by"),
        created_at,
        status,
        snoozed_until,
        executions,
        tags,
    })
}

impl agentstategraph_reminders::ReminderStore for PostgresStorage {
    fn save(
        &self,
        reminder: &agentstategraph_reminders::Reminder,
    ) -> Result<(), agentstategraph_reminders::ReminderError> {
        let commands_json = serde_json::to_string(&reminder.commands)
            .map_err(|e| agentstategraph_reminders::ReminderError::Store(format!("commands: {e}")))?;
        let refs_json = serde_json::to_string(&reminder.refs)
            .map_err(|e| agentstategraph_reminders::ReminderError::Store(format!("refs: {e}")))?;
        let schedule_json = reminder
            .schedule
            .as_ref()
            .map(|s| serde_json::to_string(s))
            .transpose()
            .map_err(|e| agentstategraph_reminders::ReminderError::Store(format!("schedule: {e}")))?;
        let executions_json = serde_json::to_string(&reminder.executions)
            .map_err(|e| agentstategraph_reminders::ReminderError::Store(format!("executions: {e}")))?;
        let tags_json = serde_json::to_string(&reminder.tags)
            .map_err(|e| agentstategraph_reminders::ReminderError::Store(format!("tags: {e}")))?;
        let priority = pg_priority_to_i32(reminder.priority);
        let status = pg_reminder_status_to_str(reminder.status).to_string();
        let due_at = reminder.due_at.to_rfc3339();
        let created_at = reminder.created_at.to_rfc3339();
        let snoozed_until = reminder.snoozed_until.map(|t| t.to_rfc3339());

        self.block_on(async {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| StorageError::Backend(format!("get conn: {e}")))?;
            client
                .execute(
                    "INSERT INTO reminders (
                        tenant_id, id, title, instructions, commands, refs,
                        priority, due_at, schedule, autonomous, created_by,
                        created_at, status, snoozed_until, executions, tags
                    ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16)",
                    &[
                        &self.tenant_id,
                        &reminder.id,
                        &reminder.title,
                        &reminder.instructions,
                        &commands_json,
                        &refs_json,
                        &priority,
                        &due_at,
                        &schedule_json,
                        &reminder.autonomous,
                        &reminder.created_by,
                        &created_at,
                        &status,
                        &snoozed_until,
                        &executions_json,
                        &tags_json,
                    ],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("insert reminder: {e}")))?;
            Ok(())
        })
        .map_err(|e| agentstategraph_reminders::ReminderError::Store(e.to_string()))
    }

    fn get(
        &self,
        id: &str,
    ) -> Result<Option<agentstategraph_reminders::Reminder>, agentstategraph_reminders::ReminderError>
    {
        self.block_on(async {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| StorageError::Backend(format!("get conn: {e}")))?;
            let row = client
                .query_opt(
                    "SELECT * FROM reminders WHERE tenant_id = $1 AND id = $2",
                    &[&self.tenant_id, &id],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("get reminder: {e}")))?;
            match row {
                Some(r) => Ok(Some(pg_row_to_reminder(&r)?)),
                None => Ok(None),
            }
        })
        .map_err(|e| agentstategraph_reminders::ReminderError::Store(e.to_string()))
    }

    fn update(
        &self,
        reminder: &agentstategraph_reminders::Reminder,
    ) -> Result<(), agentstategraph_reminders::ReminderError> {
        let commands_json = serde_json::to_string(&reminder.commands)
            .map_err(|e| agentstategraph_reminders::ReminderError::Store(format!("commands: {e}")))?;
        let refs_json = serde_json::to_string(&reminder.refs)
            .map_err(|e| agentstategraph_reminders::ReminderError::Store(format!("refs: {e}")))?;
        let schedule_json = reminder
            .schedule
            .as_ref()
            .map(|s| serde_json::to_string(s))
            .transpose()
            .map_err(|e| agentstategraph_reminders::ReminderError::Store(format!("schedule: {e}")))?;
        let executions_json = serde_json::to_string(&reminder.executions)
            .map_err(|e| agentstategraph_reminders::ReminderError::Store(format!("executions: {e}")))?;
        let tags_json = serde_json::to_string(&reminder.tags)
            .map_err(|e| agentstategraph_reminders::ReminderError::Store(format!("tags: {e}")))?;
        let priority = pg_priority_to_i32(reminder.priority);
        let status = pg_reminder_status_to_str(reminder.status).to_string();
        let due_at = reminder.due_at.to_rfc3339();
        let created_at = reminder.created_at.to_rfc3339();
        let snoozed_until = reminder.snoozed_until.map(|t| t.to_rfc3339());
        let id = reminder.id.clone();

        self.block_on(async {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| StorageError::Backend(format!("get conn: {e}")))?;
            let n = client
                .execute(
                    "UPDATE reminders SET
                        title = $3, instructions = $4, commands = $5, refs = $6,
                        priority = $7, due_at = $8, schedule = $9, autonomous = $10,
                        created_by = $11, created_at = $12, status = $13,
                        snoozed_until = $14, executions = $15, tags = $16
                     WHERE tenant_id = $1 AND id = $2",
                    &[
                        &self.tenant_id,
                        &id,
                        &reminder.title,
                        &reminder.instructions,
                        &commands_json,
                        &refs_json,
                        &priority,
                        &due_at,
                        &schedule_json,
                        &reminder.autonomous,
                        &reminder.created_by,
                        &created_at,
                        &status,
                        &snoozed_until,
                        &executions_json,
                        &tags_json,
                    ],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("update reminder: {e}")))?;
            if n == 0 {
                return Err(StorageError::Backend(format!("reminder {id} not found")));
            }
            Ok(())
        })
        .map_err(|e| {
            if e.to_string().contains("not found") {
                agentstategraph_reminders::ReminderError::NotFound(reminder.id.clone())
            } else {
                agentstategraph_reminders::ReminderError::Store(e.to_string())
            }
        })
    }

    fn delete(
        &self,
        id: &str,
    ) -> Result<bool, agentstategraph_reminders::ReminderError> {
        self.block_on(async {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| StorageError::Backend(format!("get conn: {e}")))?;
            let n = client
                .execute(
                    "DELETE FROM reminders WHERE tenant_id = $1 AND id = $2",
                    &[&self.tenant_id, &id],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("delete reminder: {e}")))?;
            Ok(n > 0)
        })
        .map_err(|e| agentstategraph_reminders::ReminderError::Store(e.to_string()))
    }

    fn list(
        &self,
        filter: &agentstategraph_reminders::ReminderFilter,
    ) -> Result<Vec<agentstategraph_reminders::Reminder>, agentstategraph_reminders::ReminderError>
    {
        // Pre-compute owned values so they outlive the async block.
        let status_str = filter
            .status
            .map(|s| pg_reminder_status_to_str(s).to_string());
        let priority_val: Option<i32> = filter.priority_at_most.map(pg_priority_to_i32);
        let created_by = filter.created_by.clone();
        let due_before = filter.due_before.map(|d| d.to_rfc3339());
        let ref_id = filter.ref_id.clone();
        let tags = filter.tags.clone();

        self.block_on(async {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| StorageError::Backend(format!("get conn: {e}")))?;

            let mut sql =
                "SELECT * FROM reminders WHERE tenant_id = $1".to_string();
            let mut idx = 2usize;
            if status_str.is_some() {
                sql.push_str(&format!(" AND status = ${idx}"));
                idx += 1;
            }
            if priority_val.is_some() {
                sql.push_str(&format!(" AND priority <= ${idx}"));
                idx += 1;
            }
            if created_by.is_some() {
                sql.push_str(&format!(" AND created_by = ${idx}"));
                idx += 1;
            }
            if due_before.is_some() {
                sql.push_str(&format!(" AND due_at <= ${idx}"));
                // idx not needed after this
            }
            sql.push_str(" ORDER BY priority ASC, due_at ASC");

            let mut params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
                vec![&self.tenant_id];
            if let Some(ref s) = status_str {
                params.push(s);
            }
            if let Some(ref p) = priority_val {
                params.push(p);
            }
            if let Some(ref c) = created_by {
                params.push(c);
            }
            if let Some(ref d) = due_before {
                params.push(d);
            }

            let rows = client
                .query(&sql, &params)
                .await
                .map_err(|e| StorageError::Backend(format!("list reminders: {e}")))?;

            let mut out = Vec::with_capacity(rows.len());
            for row in rows {
                let reminder = pg_row_to_reminder(&row)?;
                if let Some(ref rid) = ref_id {
                    if !reminder.refs.iter().any(|rf| &rf.id == rid) {
                        continue;
                    }
                }
                if tags.iter().any(|tag| !reminder.tags.contains(tag)) {
                    continue;
                }
                out.push(reminder);
            }
            Ok(out)
        })
        .map_err(|e| agentstategraph_reminders::ReminderError::Store(e.to_string()))
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

    // -----------------------------------------------------------------------
    // ReminderStore helper unit tests (no live Postgres connection needed)
    // -----------------------------------------------------------------------

    use agentstategraph_reminders::types::{Priority, ReminderStatus};

    #[test]
    fn pg_reminder_status_round_trip() {
        let statuses = [
            ReminderStatus::Pending,
            ReminderStatus::Due,
            ReminderStatus::AwaitingPermission,
            ReminderStatus::InProgress,
            ReminderStatus::Completed,
            ReminderStatus::Snoozed,
            ReminderStatus::Cancelled,
        ];
        for s in statuses {
            let encoded = pg_reminder_status_to_str(s);
            let decoded = pg_reminder_status_from_str(encoded)
                .unwrap_or_else(|e| panic!("decode failed for {s:?}: {e}"));
            assert_eq!(decoded, s, "status round-trip failed for {s:?}");
        }
    }

    #[test]
    fn pg_reminder_status_from_str_rejects_unknown() {
        assert!(pg_reminder_status_from_str("bogus").is_err());
    }

    #[test]
    fn pg_priority_round_trip() {
        let priorities = [
            Priority::Critical,
            Priority::High,
            Priority::Medium,
            Priority::Low,
            Priority::Minimal,
        ];
        for p in priorities {
            let encoded = pg_priority_to_i32(p);
            let decoded = pg_priority_from_i32(encoded)
                .unwrap_or_else(|e| panic!("decode failed for {p:?}: {e}"));
            assert_eq!(decoded, p, "priority round-trip failed for {p:?}");
        }
    }

    #[test]
    fn pg_priority_from_i32_rejects_out_of_range() {
        assert!(pg_priority_from_i32(0).is_err());
        assert!(pg_priority_from_i32(6).is_err());
        assert!(pg_priority_from_i32(-1).is_err());
    }

    #[test]
    fn pg_priority_values_match_ordinal() {
        assert_eq!(pg_priority_to_i32(Priority::Critical), 1);
        assert_eq!(pg_priority_to_i32(Priority::High), 2);
        assert_eq!(pg_priority_to_i32(Priority::Medium), 3);
        assert_eq!(pg_priority_to_i32(Priority::Low), 4);
        assert_eq!(pg_priority_to_i32(Priority::Minimal), 5);
    }

    #[test]
    fn pg_reminder_status_strings_are_snake_case() {
        assert_eq!(pg_reminder_status_to_str(ReminderStatus::AwaitingPermission), "awaiting_permission");
        assert_eq!(pg_reminder_status_to_str(ReminderStatus::InProgress), "in_progress");
    }
}
