//! PostgreSQL storage backend — multi-tenant, connection-pooled.
//!
//! Uses tokio-postgres + deadpool for connection pooling.
//! Each tenant's data is isolated via a `tenant_id` column on every table.
//!
//! Usage:
//!   let storage = PostgresStorage::connect("postgres://localhost/agentstategraph").await?;
//!   let storage = PostgresStorage::connect_tenant("postgres://...", "tenant-123").await?;

use deadpool_postgres::{Config, Pool, Runtime, ManagerConfig, RecyclingMethod};
use tokio_postgres::NoTls;

use agentstategraph_core::{Commit, Object, ObjectId};

use crate::traits::{CommitStore, ObjectStore, RefStore, StorageError};

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
    pub async fn connect_tenant(database_url: &str, tenant_id: &str) -> Result<Self, StorageError> {
        let mut cfg = Config::new();
        cfg.url = Some(database_url.to_string());
        cfg.manager = Some(ManagerConfig {
            recycling_method: RecyclingMethod::Fast,
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

    async fn init_tables(&self) -> Result<(), StorageError> {
        let client = self.pool.get().await
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
            let client = self.pool.get().await
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
        let data = serde_json::to_value(obj)
            .map_err(|e| StorageError::Serialization(e.to_string()))?;

        self.block_on(async {
            let client = self.pool.get().await
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
            let client = self.pool.get().await
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
            let client = self.pool.get().await
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
        let data = serde_json::to_value(commit)
            .map_err(|e| StorageError::Serialization(e.to_string()))?;
        let timestamp = commit.timestamp;

        self.block_on(async {
            let client = self.pool.get().await
                .map_err(|e| StorageError::Backend(format!("get conn: {}", e)))?;

            client
                .execute(
                    "INSERT INTO commits (tenant_id, id, data, timestamp) VALUES ($1, $2, $3, $4)
                     ON CONFLICT (tenant_id, id) DO NOTHING",
                    &[&self.tenant_id, &commit.id.as_bytes().as_slice(), &data, &timestamp],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("put commit: {}", e)))?;

            Ok(())
        })
    }

    fn has_commit(&self, id: &ObjectId) -> Result<bool, StorageError> {
        self.block_on(async {
            let client = self.pool.get().await
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
            let client = self.pool.get().await
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
            let client = self.pool.get().await
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
            let client = self.pool.get().await
                .map_err(|e| StorageError::Backend(format!("get conn: {}", e)))?;

            client
                .execute(
                    "INSERT INTO refs (tenant_id, name, target) VALUES ($1, $2, $3)
                     ON CONFLICT (tenant_id, name) DO UPDATE SET target = $3",
                    &[&self.tenant_id, &name.to_string(), &target.as_bytes().as_slice()],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("set ref: {}", e)))?;

            Ok(())
        })
    }

    fn cas_ref(&self, name: &str, expected: ObjectId, new: ObjectId) -> Result<bool, StorageError> {
        self.block_on(async {
            let client = self.pool.get().await
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
            let client = self.pool.get().await
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
            let client = self.pool.get().await
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
