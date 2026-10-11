//! PostgreSQL storage backend — multi-tenant, connection-pooled.
//!
//! Uses tokio-postgres + deadpool for connection pooling.
//! Each tenant's data is isolated via a `tenant_id` column on every table.
//!
//! Usage:
//!   let storage = PostgresStorage::connect("postgres://localhost/agentstategraph").await?;
//!   let storage = PostgresStorage::connect_tenant("postgres://...", "tenant-123").await?;
//!   // From sync code, with or without an async runtime:
//!   let storage = PostgresStorage::connect_tenant_blocking("postgres://...", "tenant-123", 8)?;
//!
//! The storage traits are synchronous. Every connection is driven by one
//! process-wide runtime owned by this module, so callers can use the storage
//! from plain threads, from a multi-threaded tokio runtime, or from a
//! single-threaded one, and the runtime that connected needn't outlive it.
//!
//! TLS (the `postgres-tls` feature) is set in the connection string, the way
//! libpq does it:
//!   postgres://user@db.example.com/app?sslmode=verify-full&sslrootcert=/etc/ca.pem
//! - `sslmode=require`, `verify-ca` and `verify-full` all mean verified TLS: the
//!   server's certificate must chain to Mozilla's roots or to a certificate in
//!   `sslrootcert` (or `PGSSLROOTCERT`), and must match the host name. There is
//!   no unverified TLS. `verify-ca` is stricter here than in libpq, because the
//!   host name is checked too.
//! - `sslmode=prefer` uses TLS when the server offers it, and still verifies it.
//! - No `sslmode`, or `sslmode=disable`, is plaintext, as before. Use
//!   `sslmode=require` or `verify-full` for any host that isn't local.
//! Without the `postgres-tls` feature, asking for TLS is an error that names
//! the feature, never a silent plaintext connection.
//!
//! As of 0.6.75-beta.1 this backend persists epochs and sessions too,
//! with the same schema shape as SQLite (JSON-as-TEXT). Multi-tenant
//! isolation continues via a `tenant_id` column on every table.
//!
//! History walks, the project-history rollups and value search work as they
//! do on SQLite. Search uses a trigram index when the `pg_trgm` extension can
//! be installed, and scans the ref's rows when it can't. Requires PostgreSQL
//! 13 or later.
//!
//! GC sweeps one tenant at a time and never makes writers wait: a sweep
//! spares objects written within a grace period (see
//! [`PostgresStorage::with_gc_grace`]) instead of locking writers out.

use deadpool_postgres::{
    Config, ManagerConfig, Pool, PoolConfig, RecyclingMethod, Runtime as PoolRuntime,
};
use std::sync::OnceLock;
use tokio::runtime::RuntimeFlavor;
#[cfg(not(feature = "postgres-tls"))]
use tokio_postgres::NoTls;

/// Default Postgres pool cap (v2-M1). Overridable via constructor or
/// the binary's `--pg-pool-size` flag.
pub const DEFAULT_POOL_SIZE: usize = 32;

/// Default GC grace period: a sweep spares unreachable objects written more
/// recently than this. See [`PostgresStorage::with_gc_grace`].
pub const DEFAULT_GC_GRACE: std::time::Duration = std::time::Duration::from_secs(10 * 60);

use agentstategraph_core::{
    Commit, Epoch, EpochScope, EpochStatus, IntentCategory, Namespace, Node, Object, ObjectId,
    Session, SessionStatus, TAG_GIT_REVISION, TAG_PIN_STATE,
};
use chrono::{DateTime, Utc};

use crate::traits::{
    CommitStore, CommitWalk, EpochStore, GcReachability, GcSweep, HISTORY_NAMESPACE_UNKNOWN,
    HistoryMilestoneRow, HistoryRollupRow, ObjectStore, RefStore, SessionStore, StorageError,
    StoreShape, TableBytes, TaintStore,
};

/// TLS settings read from a connection string, and the string tokio-postgres
/// is given in its place: it knows only `sslmode=disable|prefer|require` and
/// rejects `sslrootcert`, so `verify-ca`/`verify-full` become `require` (TLS
/// here is always verified) and `sslrootcert` is taken out. A string with no
/// `sslmode` gets `sslmode=disable`, so it stays plaintext as it always was;
/// tokio-postgres alone would default to `prefer`.
#[derive(Debug, PartialEq)]
struct ConnectionTls {
    url: String,
    requires_tls: bool,
    root_cert: Option<std::path::PathBuf>,
}

impl ConnectionTls {
    /// `env_root_cert` is `PGSSLROOTCERT`, used when the string names none.
    fn parse(database_url: &str, env_root_cert: Option<String>) -> Result<Self, StorageError> {
        let is_url =
            database_url.starts_with("postgres://") || database_url.starts_with("postgresql://");
        // URL form keeps options in the query string; key/value form is
        // space-separated `key=value` pairs (quoted values aren't supported
        // for these two keys).
        let (head, options, sep) = if is_url {
            match database_url.split_once('?') {
                Some((head, query)) => (head, query, '&'),
                None => (database_url, "", '&'),
            }
        } else {
            ("", database_url, ' ')
        };

        let mut kept: Vec<String> = Vec::new();
        let mut mode: Option<String> = None;
        let mut root_cert = None;
        for part in options.split(sep).filter(|p| !p.is_empty()) {
            let (key, value) = part.split_once('=').unwrap_or((part, ""));
            match key {
                "sslmode" => {
                    let mapped = match value {
                        "verify-ca" | "verify-full" => "require",
                        other => other,
                    };
                    mode = Some(value.to_string());
                    kept.push(format!("sslmode={mapped}"));
                }
                "sslrootcert" => {
                    let path = if is_url {
                        percent_decode(value)?
                    } else {
                        value.to_string()
                    };
                    root_cert = Some(std::path::PathBuf::from(path));
                }
                _ => kept.push(part.to_string()),
            }
        }
        if mode.is_none() {
            kept.push("sslmode=disable".to_string());
        }
        let options = kept.join(&sep.to_string());
        let url = if is_url {
            format!("{head}?{options}")
        } else {
            options
        };
        Ok(Self {
            url,
            requires_tls: matches!(
                mode.as_deref(),
                Some("require") | Some("verify-ca") | Some("verify-full")
            ),
            root_cert: root_cert.or_else(|| env_root_cert.map(std::path::PathBuf::from)),
        })
    }
}

/// Decode `%XX` escapes in a URL query value (a file path may contain them).
fn percent_decode(value: &str) -> Result<String, StorageError> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = value
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
                .ok_or_else(|| {
                    StorageError::Backend(format!(
                        "bad percent-escape in connection string: {value}"
                    ))
                })?;
            out.push(hex);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out)
        .map_err(|_| StorageError::Backend(format!("connection string value isn't UTF-8: {value}")))
}

/// rustls for tokio-postgres: always-verified TLS against Mozilla's roots
/// plus any `sslrootcert`. Channel binding (SCRAM-SHA-256-PLUS) isn't
/// offered; ordinary SCRAM authentication works over the TLS connection.
#[cfg(feature = "postgres-tls")]
mod tls {
    use std::future::Future;
    use std::io;
    use std::path::Path;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::task::{Context, Poll};

    use rustls::pki_types::pem::PemObject;
    use rustls::pki_types::{CertificateDer, ServerName};
    use rustls::{ClientConfig, RootCertStore};
    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
    use tokio_postgres::tls::{ChannelBinding, MakeTlsConnect, TlsConnect, TlsStream};

    use crate::traits::StorageError;

    pub(super) fn connector(root_cert: Option<&Path>) -> Result<MakeRustlsConnect, StorageError> {
        let mut roots = RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        if let Some(path) = root_cert {
            let bad = |e: &dyn std::fmt::Display| {
                StorageError::Backend(format!("sslrootcert {}: {e}", path.display()))
            };
            let certs = CertificateDer::pem_file_iter(path)
                .map_err(|e| bad(&e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| bad(&e))?;
            if certs.is_empty() {
                return Err(bad(&"no PEM certificates in the file"));
            }
            for cert in certs {
                roots.add(cert).map_err(|e| bad(&e))?;
            }
        }
        let config =
            ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .map_err(|e| StorageError::Backend(format!("postgres tls: {e}")))?
                .with_root_certificates(roots)
                .with_no_client_auth();
        Ok(MakeRustlsConnect(Arc::new(config)))
    }

    #[derive(Clone)]
    pub(super) struct MakeRustlsConnect(Arc<ClientConfig>);

    impl<S> MakeTlsConnect<S> for MakeRustlsConnect
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        type Stream = RustlsStream<S>;
        type TlsConnect = RustlsConnect;
        type Error = io::Error;

        fn make_tls_connect(&mut self, host: &str) -> io::Result<RustlsConnect> {
            let name = ServerName::try_from(host.to_string())
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
            Ok(RustlsConnect {
                config: Arc::clone(&self.0),
                name,
            })
        }
    }

    pub(super) struct RustlsConnect {
        config: Arc<ClientConfig>,
        name: ServerName<'static>,
    }

    impl<S> TlsConnect<S> for RustlsConnect
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        type Stream = RustlsStream<S>;
        type Error = io::Error;
        type Future = Pin<Box<dyn Future<Output = io::Result<RustlsStream<S>>> + Send>>;

        fn connect(self, stream: S) -> Self::Future {
            Box::pin(async move {
                tokio_rustls::TlsConnector::from(self.config)
                    .connect(self.name, stream)
                    .await
                    .map(RustlsStream)
            })
        }
    }

    pub(super) struct RustlsStream<S>(tokio_rustls::client::TlsStream<S>);

    impl<S: AsyncRead + AsyncWrite + Unpin> TlsStream for RustlsStream<S> {
        fn channel_binding(&self) -> ChannelBinding {
            ChannelBinding::none()
        }
    }

    impl<S: AsyncRead + AsyncWrite + Unpin> AsyncRead for RustlsStream<S> {
        fn poll_read(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Pin::new(&mut self.get_mut().0).poll_read(cx, buf)
        }
    }

    impl<S: AsyncRead + AsyncWrite + Unpin> AsyncWrite for RustlsStream<S> {
        fn poll_write(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            Pin::new(&mut self.get_mut().0).poll_write(cx, buf)
        }

        fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.get_mut().0).poll_flush(cx)
        }

        fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.get_mut().0).poll_shutdown(cx)
        }
    }
}

/// The schema this build expects. Bump it whenever `SCHEMA_DDL` changes:
/// databases stamped with an older version re-run the (idempotent) DDL once.
///
/// 2: history walks in one query, the history rollup tables, and the leaf
/// search index (Plan D t-008).
/// 3: GC: object write times and speculation pins (Plan D t-009).
const SCHEMA_VERSION: i32 = 3;

/// Every table, column and index, each `IF NOT EXISTS` so it's safe to re-run
/// over any earlier schema, plus the version stamp table.
const SCHEMA_DDL: &str = "
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

                CREATE TABLE IF NOT EXISTS namespaces (
                    tenant_id  TEXT NOT NULL,
                    name       TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    PRIMARY KEY (tenant_id, name)
                );

                CREATE TABLE IF NOT EXISTS refs (
                    tenant_id TEXT NOT NULL,
                    namespace TEXT NOT NULL,
                    name      TEXT NOT NULL,
                    target    BYTEA NOT NULL,
                    PRIMARY KEY (tenant_id, namespace, name)
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
                    scope_namespace TEXT,
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
                ALTER TABLE sessions ADD COLUMN IF NOT EXISTS scope_namespace TEXT;

                -- Epoch fidelity and scoping, as in the SQLite backend: the
                -- seal digest, the owning namespace, and what the seal covers.
                -- Nullable or defaulted, so existing rows read back as before
                -- (global, scope = all).
                ALTER TABLE epochs ADD COLUMN IF NOT EXISTS seal_hash BYTEA;
                ALTER TABLE epochs ADD COLUMN IF NOT EXISTS namespace TEXT;
                ALTER TABLE epochs ADD COLUMN IF NOT EXISTS scope TEXT NOT NULL DEFAULT 'all';
                CREATE INDEX IF NOT EXISTS idx_epochs_tenant_namespace
                    ON epochs(tenant_id, namespace);

                CREATE INDEX IF NOT EXISTS idx_commits_tenant_epoch
                    ON commits(tenant_id, epoch_id);
                CREATE INDEX IF NOT EXISTS idx_commits_tenant_session
                    ON commits(tenant_id, session_id);

                -- History walks (version 2). A walk follows parent ids from
                -- the commit JSON to the commits holding them, in one
                -- recursive query; this index makes each step a lookup.
                -- Reading the JSON rather than a separate edges table means a
                -- commit written by an older binary is walkable too.
                CREATE INDEX IF NOT EXISTS idx_commits_data_id
                    ON commits USING hash ((data -> 'id'));

                -- The id of the transaction that wrote each commit (version
                -- 2), which orders commits for the history extractor. Rows
                -- from before this column read 0, so they come first.
                ALTER TABLE commits ADD COLUMN IF NOT EXISTS xid BIGINT NOT NULL DEFAULT 0;
                ALTER TABLE commits
                    ALTER COLUMN xid SET DEFAULT (pg_current_xact_id()::text::bigint);
                CREATE INDEX IF NOT EXISTS idx_commits_tenant_xid
                    ON commits(tenant_id, xid, id);

                -- Distilled project history (version 2), as in the SQLite
                -- backend. Derived and rebuildable, never authoritative. Text
                -- columns compare bytewise (COLLATE \"C\"), as SQLite's do, so
                -- RFC 3339 timestamps order correctly and output order
                -- matches SQLite's.
                CREATE TABLE IF NOT EXISTS asg_history_cursor (
                    tenant_id TEXT PRIMARY KEY,
                    xid       BIGINT NOT NULL,
                    commit_id BYTEA NOT NULL,
                    folded    BIGINT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS asg_history_commit_rollup (
                    tenant_id       TEXT NOT NULL,
                    day             TEXT COLLATE \"C\" NOT NULL,
                    namespace       TEXT COLLATE \"C\" NOT NULL,
                    agent_id        TEXT COLLATE \"C\" NOT NULL,
                    intent_category TEXT COLLATE \"C\" NOT NULL,
                    commit_count    BIGINT NOT NULL DEFAULT 0,
                    first_ts        TEXT COLLATE \"C\" NOT NULL,
                    last_ts         TEXT COLLATE \"C\" NOT NULL,
                    PRIMARY KEY (tenant_id, day, namespace, agent_id, intent_category)
                );
                CREATE TABLE IF NOT EXISTS asg_history_milestone (
                    tenant_id   TEXT NOT NULL,
                    commit_id   BYTEA NOT NULL,
                    kind        TEXT NOT NULL,
                    timestamp   TEXT COLLATE \"C\" NOT NULL,
                    day         TEXT NOT NULL,
                    namespace   TEXT NOT NULL,
                    agent_id    TEXT NOT NULL,
                    description TEXT NOT NULL,
                    state_root  BYTEA,
                    git_sha     TEXT,
                    PRIMARY KEY (tenant_id, commit_id, kind)
                );
                CREATE INDEX IF NOT EXISTS idx_asg_history_milestone_ts
                    ON asg_history_milestone(tenant_id, timestamp);

                -- Leaf-value search index (version 2), as in the SQLite
                -- backend: the string leaves of a (namespace, ref), kept up to
                -- date as the ref moves. A path holds one value. `root` is the
                -- state root the set reflects (NULL when unknown), so a diff
                -- applies only to the tree it was computed from.
                CREATE TABLE IF NOT EXISTS leaf_rows (
                    tenant_id TEXT NOT NULL,
                    namespace TEXT NOT NULL,
                    ref       TEXT NOT NULL,
                    path      TEXT NOT NULL,
                    value     TEXT NOT NULL,
                    PRIMARY KEY (tenant_id, namespace, ref, path)
                );
                CREATE TABLE IF NOT EXISTS leaf_index_refs (
                    tenant_id TEXT NOT NULL,
                    namespace TEXT NOT NULL,
                    ref       TEXT NOT NULL,
                    root      BYTEA,
                    PRIMARY KEY (tenant_id, namespace, ref)
                );
                -- Substring search uses a trigram index when pg_trgm can be
                -- installed (it is a trusted extension, so a role with CREATE
                -- on the database can). Without it, search still works by
                -- scanning the ref's rows.
                DO $$
                BEGIN
                    CREATE EXTENSION IF NOT EXISTS pg_trgm;
                    CREATE INDEX IF NOT EXISTS idx_leaf_rows_value_trgm
                        ON leaf_rows USING gin (value gin_trgm_ops);
                EXCEPTION WHEN OTHERS THEN
                    RAISE NOTICE 'leaf search runs without a trigram index: %', SQLERRM;
                END
                $$;

                -- GC (version 3). When each object was last written: a sweep
                -- spares unreachable objects written within its grace period,
                -- which is what keeps a write in flight safe (it stores its
                -- objects before committing them). Rows from before read
                -- -infinity, so they can go as soon as nothing reaches them.
                ALTER TABLE objects
                    ADD COLUMN IF NOT EXISTS written_at TIMESTAMPTZ NOT NULL DEFAULT '-infinity';
                ALTER TABLE objects ALTER COLUMN written_at SET DEFAULT now();
                -- Roots of uncommitted work (open speculations) that a sweep
                -- must keep, from any process. See gc_pin_root.
                CREATE TABLE IF NOT EXISTS gc_pending_roots (
                    tenant_id  TEXT NOT NULL,
                    key        TEXT NOT NULL,
                    root       BYTEA NOT NULL,
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
                    PRIMARY KEY (tenant_id, key)
                );

                CREATE TABLE IF NOT EXISTS asg_schema (
                    component TEXT PRIMARY KEY,
                    version   INTEGER NOT NULL
                );
";

/// A database error with its whole cause chain. tokio-postgres's Display for
/// a server error is just "db error"; the server's message is the source.
fn with_causes(context: &str, e: &dyn std::error::Error) -> String {
    let mut msg = format!("{context}: {e}");
    let mut source = e.source();
    while let Some(cause) = source {
        msg.push_str(&format!(": {cause}"));
        source = cause.source();
    }
    msg
}

/// A failure to get a pooled connection, with its whole cause chain: the pool
/// and tokio-postgres errors alone say only "error performing TLS handshake",
/// and the reason (an untrusted issuer, a name mismatch, a refused login) is
/// further down the chain.
fn conn_error(e: impl std::error::Error) -> StorageError {
    let mut msg = format!("get conn: {e}");
    let mut source = e.source();
    while let Some(cause) = source {
        msg.push_str(&format!(": {cause}"));
        source = cause.source();
    }
    StorageError::Backend(msg)
}

/// What storing an object that already exists does: refresh its write time
/// if it is over a minute old. A sweep spares recently written objects, so an
/// object a writer has just stored (or stored again, since content addressing
/// dedups) can't be taken before the writer commits it. The conflicting row is
/// locked whether or not it is updated, and the sweep re-checks the write
/// time when it deletes, so the two can't interleave.
const FRESHEN_ON_CONFLICT: &str = "ON CONFLICT (tenant_id, id) DO UPDATE SET written_at = now()
                         WHERE objects.written_at < now() - interval '1 minute'";

/// Advisory-lock key that serializes schema setup across every process using
/// a database ("asg-sch" as bytes).
const SCHEMA_LOCK_KEY: i64 = 0x6173_672d_7363_68;

/// The runtime that drives every `PostgresStorage` connection in the process.
///
/// A pooled connection lives on the runtime it was opened in. Owning that
/// runtime here means no caller's runtime has to exist, be multi-threaded, or
/// outlive the pool, and it is never dropped inside an async context.
fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        let workers = std::thread::available_parallelism().map_or(2, |n| n.get().clamp(2, 8));
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(workers)
            .thread_name("asg-postgres")
            .enable_all()
            .build()
            .expect("start the Postgres storage runtime")
    })
}

/// PostgreSQL-backed storage with connection pooling and optional multi-tenancy.
pub struct PostgresStorage {
    pool: Pool,
    tenant_id: String,
    /// How recently written an unreachable object must be for a GC sweep to
    /// spare it. See [`PostgresStorage::with_gc_grace`].
    gc_grace: std::time::Duration,
    /// The connection holding this tenant's sweep lock, between
    /// `history_gc_lock_writers` and `history_gc_unlock_writers`.
    gc_conn: std::sync::Mutex<Option<deadpool_postgres::Object>>,
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
        let storage = Self::with_pool(database_url, tenant_id, max_size)?;
        // Schema setup runs on the storage runtime, so the connection it opens
        // (which then goes back into the pool) is driven there rather than by
        // the caller's runtime, which may be gone when that connection is reused.
        let setup = Self {
            pool: storage.pool.clone(),
            tenant_id: storage.tenant_id.clone(),
            gc_grace: storage.gc_grace,
            gc_conn: std::sync::Mutex::new(None),
        };
        runtime()
            .spawn(async move { setup.init_tables().await })
            .await
            .map_err(|e| StorageError::Backend(format!("postgres setup task: {e}")))??;
        Ok(storage)
    }

    /// Connect from synchronous code. Needs no async runtime, and also works
    /// when called from inside one.
    pub fn connect_tenant_blocking(
        database_url: &str,
        tenant_id: &str,
        max_size: usize,
    ) -> Result<Self, StorageError> {
        let storage = Self::with_pool(database_url, tenant_id, max_size)?;
        storage.block_on(storage.init_tables())?;
        Ok(storage)
    }

    fn with_pool(
        database_url: &str,
        tenant_id: &str,
        max_size: usize,
    ) -> Result<Self, StorageError> {
        let tls = ConnectionTls::parse(
            database_url,
            std::env::var("PGSSLROOTCERT")
                .ok()
                .filter(|v| !v.is_empty()),
        )?;
        let mut cfg = Config::new();
        cfg.url = Some(tls.url.clone());
        cfg.manager = Some(ManagerConfig {
            recycling_method: RecyclingMethod::Fast,
        });
        cfg.pool = Some(PoolConfig {
            max_size: max_size.max(1),
            ..Default::default()
        });

        #[cfg(feature = "postgres-tls")]
        let pool = cfg.create_pool(
            Some(PoolRuntime::Tokio1),
            tls::connector(tls.root_cert.as_deref())?,
        );
        #[cfg(not(feature = "postgres-tls"))]
        let pool = {
            if tls.requires_tls || tls.root_cert.is_some() {
                return Err(StorageError::Backend(
                    "the connection string asks for TLS (sslmode=require/verify-*, or \
                     sslrootcert), but this build has no Postgres TLS support: build with \
                     the `postgres-tls` feature"
                        .into(),
                ));
            }
            cfg.create_pool(Some(PoolRuntime::Tokio1), NoTls)
        };
        let pool = pool.map_err(|e| StorageError::Backend(format!("postgres pool: {}", e)))?;
        Ok(Self {
            pool,
            tenant_id: tenant_id.to_string(),
            gc_grace: DEFAULT_GC_GRACE,
            gc_conn: std::sync::Mutex::new(None),
        })
    }

    /// Set how recently written an unreachable object must be for a GC sweep
    /// to spare it (default [`DEFAULT_GC_GRACE`]).
    ///
    /// Writers don't wait for a sweep here; the grace period protects them
    /// instead. A write stores its new objects before it commits them, and an
    /// object stored again refreshes its write time (at most a minute old
    /// after any write), so a sweep never takes an object a write in flight
    /// is about to commit. A sweep spares everything written since it started,
    /// whatever the grace. A grace under a minute narrows the protection for
    /// writes already in flight when a sweep starts; tests use zero to see
    /// garbage go at once.
    pub fn with_gc_grace(mut self, grace: std::time::Duration) -> Self {
        self.gc_grace = grace;
        self
    }

    /// Return the configured pool's maximum size. Useful for tests.
    pub fn pool_max_size(&self) -> usize {
        self.pool.status().max_size
    }

    /// Bring the schema up to date and seed this tenant's default namespace.
    ///
    /// The DDL runs only when the database's stamped schema version is behind
    /// this build's. `ALTER TABLE ... IF NOT EXISTS` takes an exclusive table
    /// lock even when it changes nothing, so running it on every connect let a
    /// starting process deadlock with writers holding locks on several tables
    /// (any multi-statement transaction). Up-to-date databases now take no
    /// table locks at startup. An actual upgrade still can collide with
    /// writers, so a deadlock victim retries.
    async fn init_tables(&self) -> Result<(), StorageError> {
        let mut client = self.pool.get().await.map_err(conn_error)?;
        let mut attempt = 0;
        loop {
            attempt += 1;
            match self.try_init_tables(&mut client).await {
                Err(e)
                    if attempt < 5
                        && e.code()
                            == Some(&tokio_postgres::error::SqlState::T_R_DEADLOCK_DETECTED) =>
                {
                    continue;
                }
                Err(e) => return Err(StorageError::Backend(with_causes("init tables", &e))),
                Ok(()) => return Ok(()),
            }
        }
    }

    async fn try_init_tables(
        &self,
        client: &mut deadpool_postgres::Client,
    ) -> Result<(), tokio_postgres::Error> {
        // Serialized across every process using the database.
        let tx = client.transaction().await?;
        tx.execute("SELECT pg_advisory_xact_lock($1)", &[&SCHEMA_LOCK_KEY])
            .await?;
        let stamped: bool = tx
            .query_one("SELECT to_regclass('asg_schema') IS NOT NULL", &[])
            .await?
            .get(0);
        let version: Option<i32> = if stamped {
            tx.query_opt(
                "SELECT version FROM asg_schema WHERE component = 'postgres'",
                &[],
            )
            .await?
            .map(|row| row.get(0))
        } else {
            None
        };
        if version.is_none_or(|v| v < SCHEMA_VERSION) {
            tx.batch_execute(SCHEMA_DDL).await?;
            tx.execute(
                "INSERT INTO asg_schema (component, version) VALUES ('postgres', $1)
                 ON CONFLICT (component) DO UPDATE SET version = EXCLUDED.version",
                &[&SCHEMA_VERSION],
            )
            .await?;
        }
        // Seed the default namespace for this tenant.
        tx.execute(
            "INSERT INTO namespaces (tenant_id, name, created_at)
             VALUES ($1, 'default', NOW()::TEXT)
             ON CONFLICT DO NOTHING",
            &[&self.tenant_id],
        )
        .await?;
        tx.commit().await
    }

    /// Run an async operation for the sync storage API, on whatever kind of
    /// thread the caller is on. Before, this assumed a multi-threaded tokio
    /// runtime: plain threads panicked (no runtime), as did single-threaded
    /// runtimes (`block_in_place` is multi-thread only).
    fn block_on<F, T>(&self, f: F) -> Result<T, StorageError>
    where
        F: std::future::Future<Output = Result<T, StorageError>> + Send,
        T: Send,
    {
        let rt = runtime().handle();
        match tokio::runtime::Handle::try_current().map(|h| h.runtime_flavor()) {
            // Not on a runtime: drive the operation on this thread.
            Err(_) => rt.block_on(f),
            // A multi-threaded runtime's thread: hand its other work off first.
            Ok(RuntimeFlavor::MultiThread) => tokio::task::block_in_place(|| rt.block_on(f)),
            // A single-threaded runtime can neither give up its only thread nor
            // start another runtime on it, so wait on a helper thread.
            Ok(_) => std::thread::scope(|scope| {
                scope
                    .spawn(|| rt.block_on(f))
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            }),
        }
    }

    async fn require_namespace(
        &self,
        client: &deadpool_postgres::Client,
        namespace: &Namespace,
    ) -> Result<(), StorageError> {
        let row = client
            .query_opt(
                "SELECT 1 FROM namespaces WHERE tenant_id = $1 AND name = $2",
                &[&self.tenant_id, &namespace.as_str()],
            )
            .await
            .map_err(|e| StorageError::Backend(format!("check namespace: {}", e)))?;
        match row {
            Some(_) => Ok(()),
            None => Err(StorageError::NamespaceNotFound(
                namespace.as_str().to_string(),
            )),
        }
    }
}

impl PostgresStorage {
    /// Replace the `(namespace, ref)` leaf set with `entries`, marking it
    /// built at `root` (unknown when `None`). The set's row is written first,
    /// so a build and an advance on one set lock in the same order.
    fn leaf_set_replace(
        &self,
        namespace: &str,
        ref_name: &str,
        root: Option<&ObjectId>,
        entries: &[(String, String)],
    ) -> Result<(), StorageError> {
        let root = root.map(|r| r.as_bytes().as_slice());
        self.block_on(async {
            let mut client = self.pool.get().await.map_err(conn_error)?;
            let tx = client
                .transaction()
                .await
                .map_err(|e| leaf_error("build", &e))?;
            tx.execute(
                "INSERT INTO leaf_index_refs (tenant_id, namespace, ref, root)
                 VALUES ($1, $2, $3, $4)
                 ON CONFLICT (tenant_id, namespace, ref) DO UPDATE SET root = EXCLUDED.root",
                &[&self.tenant_id, &namespace, &ref_name, &root],
            )
            .await
            .map_err(|e| leaf_error("build mark", &e))?;
            tx.execute(
                "DELETE FROM leaf_rows WHERE tenant_id = $1 AND namespace = $2 AND ref = $3",
                &[&self.tenant_id, &namespace, &ref_name],
            )
            .await
            .map_err(|e| leaf_error("build clear", &e))?;
            upsert_leaves(&tx, &self.tenant_id, namespace, ref_name, entries).await?;
            tx.commit()
                .await
                .map_err(|e| leaf_error("build commit", &e))
        })
    }
}

// ─── ObjectStore ────────────────────────────────────────────

impl ObjectStore for PostgresStorage {
    fn get_object(&self, id: &ObjectId) -> Result<Option<Object>, StorageError> {
        self.block_on(async {
            let client = self.pool.get().await.map_err(conn_error)?;

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
            let client = self.pool.get().await.map_err(conn_error)?;

            client
                .execute(
                    &format!(
                        "INSERT INTO objects (tenant_id, id, data) VALUES ($1, $2, $3)
                         {FRESHEN_ON_CONFLICT}"
                    ),
                    &[&self.tenant_id, &id.as_bytes().as_slice(), &data],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("put object: {}", e)))?;

            Ok(id)
        })
    }

    /// One multi-row INSERT per chunk, all in one transaction: callers such as
    /// merge write a batch as a unit, and the trait's default made one
    /// independent round trip per object.
    fn batch_put_objects(&self, objs: &[Object]) -> Result<Vec<ObjectId>, StorageError> {
        // Each row is two parameters ($1 is the tenant), well under Postgres's
        // 65,535 per statement.
        const CHUNK: usize = 1000;
        let ids: Vec<ObjectId> = objs.iter().map(Object::id).collect();
        // One row per id: an upsert may not touch a row twice in a statement.
        let mut seen = std::collections::HashSet::with_capacity(ids.len());
        let rows: Vec<(ObjectId, serde_json::Value)> = objs
            .iter()
            .zip(&ids)
            .filter(|(_, id)| seen.insert(**id))
            .map(|(obj, id)| {
                serde_json::to_value(obj)
                    .map(|data| (*id, data))
                    .map_err(|e| StorageError::Serialization(e.to_string()))
            })
            .collect::<Result<_, _>>()?;
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        self.block_on(async {
            let mut client = self.pool.get().await.map_err(conn_error)?;
            let tx = client
                .transaction()
                .await
                .map_err(|e| StorageError::Backend(format!("begin batch put: {e}")))?;
            for chunk in rows.chunks(CHUNK) {
                let ids: Vec<&[u8]> = chunk
                    .iter()
                    .map(|(id, _)| id.as_bytes().as_slice())
                    .collect();
                let mut params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
                    Vec::with_capacity(1 + chunk.len() * 2);
                params.push(&self.tenant_id);
                let mut values = Vec::with_capacity(chunk.len());
                for (i, (_, data)) in chunk.iter().enumerate() {
                    values.push(format!("($1, ${}, ${})", 2 + i * 2, 3 + i * 2));
                    params.push(&ids[i]);
                    params.push(data);
                }
                tx.execute(
                    &format!(
                        "INSERT INTO objects (tenant_id, id, data) VALUES {}
                         {FRESHEN_ON_CONFLICT}",
                        values.join(", ")
                    ),
                    &params,
                )
                .await
                .map_err(|e| StorageError::Backend(format!("batch put objects: {e}")))?;
            }
            tx.commit()
                .await
                .map_err(|e| StorageError::Backend(format!("commit batch put: {e}")))?;
            Ok(())
        })?;
        Ok(ids)
    }

    /// One query for the whole batch, results in the order asked.
    fn batch_get_objects(&self, ids: &[ObjectId]) -> Result<Vec<Option<Object>>, StorageError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let wanted: Vec<Vec<u8>> = ids.iter().map(|id| id.as_bytes().to_vec()).collect();
        let found = self.block_on(async {
            let client = self.pool.get().await.map_err(conn_error)?;
            let rows = client
                .query(
                    "SELECT id, data FROM objects WHERE tenant_id = $1 AND id = ANY($2)",
                    &[&self.tenant_id, &wanted],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("batch get objects: {e}")))?;
            let mut found = std::collections::HashMap::with_capacity(rows.len());
            for row in rows {
                let id: Vec<u8> = row.get("id");
                let data: serde_json::Value = row.get("data");
                let obj: Object = serde_json::from_value(data)
                    .map_err(|e| StorageError::Serialization(e.to_string()))?;
                found.insert(id, obj);
            }
            Ok(found)
        })?;
        Ok(wanted.iter().map(|id| found.get(id).cloned()).collect())
    }

    fn leaf_index_is_built(&self, namespace: &str, ref_name: &str) -> Result<bool, StorageError> {
        self.block_on(async {
            let client = self.pool.get().await.map_err(conn_error)?;
            leaf_set_built(&**client, &self.tenant_id, namespace, ref_name).await
        })
    }

    fn leaf_index_build(
        &self,
        namespace: &str,
        ref_name: &str,
        entries: &[(String, String)],
    ) -> Result<(), StorageError> {
        self.leaf_set_replace(namespace, ref_name, None, entries)
    }

    fn leaf_index_apply(
        &self,
        namespace: &str,
        ref_name: &str,
        removed_paths: &[String],
        added: &[(String, String)],
    ) -> Result<(), StorageError> {
        self.block_on(async {
            let mut client = self.pool.get().await.map_err(conn_error)?;
            let tx = client
                .transaction()
                .await
                .map_err(|e| leaf_error("apply", &e))?;
            // Only a built set is maintained; an unbuilt one is filled from
            // the current tree on its first search. The set no longer
            // reflects a known tree, so its root is cleared.
            let built = tx
                .execute(
                    "UPDATE leaf_index_refs SET root = NULL
                     WHERE tenant_id = $1 AND namespace = $2 AND ref = $3",
                    &[&self.tenant_id, &namespace, &ref_name],
                )
                .await
                .map_err(|e| leaf_error("apply check", &e))?
                > 0;
            if !built {
                return Ok(());
            }
            if !removed_paths.is_empty() {
                tx.execute(
                    "DELETE FROM leaf_rows
                     WHERE tenant_id = $1 AND namespace = $2 AND ref = $3 AND path = ANY($4)",
                    &[&self.tenant_id, &namespace, &ref_name, &removed_paths],
                )
                .await
                .map_err(|e| leaf_error("apply delete", &e))?;
            }
            upsert_leaves(&tx, &self.tenant_id, namespace, ref_name, added).await?;
            tx.commit()
                .await
                .map_err(|e| leaf_error("apply commit", &e))
        })
    }

    fn leaf_index_tracks_roots(&self) -> bool {
        true
    }

    fn leaf_index_root(
        &self,
        namespace: &str,
        ref_name: &str,
    ) -> Result<Option<ObjectId>, StorageError> {
        self.block_on(async {
            let client = self.pool.get().await.map_err(conn_error)?;
            let row = client
                .query_opt(
                    "SELECT root FROM leaf_index_refs
                     WHERE tenant_id = $1 AND namespace = $2 AND ref = $3",
                    &[&self.tenant_id, &namespace, &ref_name],
                )
                .await
                .map_err(|e| leaf_error("root", &e))?;
            Ok(row.and_then(|row| {
                row.get::<_, Option<&[u8]>>(0)
                    .and_then(object_id_from_bytes)
            }))
        })
    }

    fn leaf_index_build_at(
        &self,
        namespace: &str,
        ref_name: &str,
        root: &ObjectId,
        entries: &[(String, String)],
    ) -> Result<(), StorageError> {
        self.leaf_set_replace(namespace, ref_name, Some(root), entries)
    }

    fn leaf_index_advance(
        &self,
        namespace: &str,
        ref_name: &str,
        from: &ObjectId,
        to: &ObjectId,
        removed_paths: &[String],
        added: &[(String, String)],
    ) -> Result<bool, StorageError> {
        self.block_on(async {
            let mut client = self.pool.get().await.map_err(conn_error)?;
            let tx = client
                .transaction()
                .await
                .map_err(|e| leaf_error("advance", &e))?;
            // Moving the root locks the set's row, so racing advances take
            // turns, and a loser finds the root already moved.
            let moved = tx
                .execute(
                    "UPDATE leaf_index_refs SET root = $5
                     WHERE tenant_id = $1 AND namespace = $2 AND ref = $3 AND root = $4",
                    &[
                        &self.tenant_id,
                        &namespace,
                        &ref_name,
                        &from.as_bytes().as_slice(),
                        &to.as_bytes().as_slice(),
                    ],
                )
                .await
                .map_err(|e| leaf_error("advance root", &e))?
                > 0;
            if !moved {
                return Ok(false);
            }
            if !removed_paths.is_empty() {
                tx.execute(
                    "DELETE FROM leaf_rows
                     WHERE tenant_id = $1 AND namespace = $2 AND ref = $3 AND path = ANY($4)",
                    &[&self.tenant_id, &namespace, &ref_name, &removed_paths],
                )
                .await
                .map_err(|e| leaf_error("advance delete", &e))?;
            }
            upsert_leaves(&tx, &self.tenant_id, namespace, ref_name, added).await?;
            tx.commit()
                .await
                .map_err(|e| leaf_error("advance commit", &e))?;
            Ok(true)
        })
    }

    /// Case-insensitive substring match. The trigram index serves it when
    /// pg_trgm is installed; `%`, `_` and `\` in the query match themselves.
    fn leaf_index_search(
        &self,
        namespace: &str,
        ref_name: &str,
        query_lower: &str,
        limit: usize,
    ) -> Result<Option<Vec<(String, String)>>, StorageError> {
        self.block_on(async {
            let client = self.pool.get().await.map_err(conn_error)?;
            if !leaf_set_built(&**client, &self.tenant_id, namespace, ref_name).await? {
                return Ok(None);
            }
            let rows = client
                .query(
                    "SELECT path, value FROM leaf_rows
                     WHERE tenant_id = $1 AND namespace = $2 AND ref = $3 AND value ILIKE $4
                     LIMIT $5",
                    &[
                        &self.tenant_id,
                        &namespace,
                        &ref_name,
                        &like_contains(query_lower),
                        &sql_count(limit),
                    ],
                )
                .await
                .map_err(|e| leaf_error("search", &e))?;
            Ok(Some(
                rows.iter()
                    .map(|row| (row.get("path"), row.get("value")))
                    .collect(),
            ))
        })
    }

    fn has_object(&self, id: &ObjectId) -> Result<bool, StorageError> {
        self.block_on(async {
            let client = self.pool.get().await.map_err(conn_error)?;

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
    fn all_commit_ids(&self) -> Result<Vec<ObjectId>, StorageError> {
        self.block_on(async {
            let client = self.pool.get().await.map_err(conn_error)?;
            let rows = client
                .query(
                    "SELECT id FROM commits WHERE tenant_id = $1",
                    &[&self.tenant_id],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("all commit ids: {}", e)))?;
            let mut ids = Vec::new();
            for row in rows {
                let bytes: Vec<u8> = row.get("id");
                if bytes.len() == 32 {
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(&bytes);
                    ids.push(ObjectId::from_bytes(arr));
                }
            }
            Ok(ids)
        })
    }

    fn get_commit(&self, id: &ObjectId) -> Result<Option<Commit>, StorageError> {
        self.block_on(async {
            let client = self.pool.get().await.map_err(conn_error)?;

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
            let client = self.pool.get().await.map_err(conn_error)?;

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
            let client = self.pool.get().await.map_err(conn_error)?;

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

    /// The first-parent line in one recursive query, instead of one round
    /// trip per commit.
    fn list_commits(&self, from: &ObjectId, limit: usize) -> Result<Vec<Commit>, StorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        self.block_on(async {
            let client = self.pool.get().await.map_err(conn_error)?;
            let rows = client
                .query(
                    "WITH RECURSIVE line(data, n) AS (
                         SELECT data, 1::BIGINT FROM commits WHERE tenant_id = $1 AND id = $2
                       UNION ALL
                         SELECT parent.data, line.n + 1
                         FROM line
                         CROSS JOIN LATERAL (
                             SELECT c.data FROM commits c
                             WHERE c.data -> 'id' = line.data -> 'parents' -> 0
                               AND c.tenant_id = $1
                             LIMIT 1
                         ) parent
                         WHERE line.n < $3
                     )
                     SELECT data FROM line ORDER BY n",
                    &[
                        &self.tenant_id,
                        &from.as_bytes().as_slice(),
                        &sql_count(limit),
                    ],
                )
                .await
                .map_err(|e| StorageError::Backend(with_causes("list commits", &e)))?;
            rows.iter()
                .map(|row| commit_from_row(row, "data"))
                .collect()
        })
    }

    /// Every parent edge in one recursive query. `UNION` keeps each commit
    /// once however many paths reach it, so merge-heavy histories stay
    /// linear, and the walk stops once it has one commit more than `limit`
    /// (which is how truncation is detected). Commits come out breadth
    /// first; order within one generation is unspecified. Each parent is
    /// looked up through `LATERAL ... LIMIT 1`, which keeps the step an index
    /// lookup: given a plain join, the planner may hash the tenant's whole
    /// commit table instead.
    fn list_commits_dag(&self, from: &ObjectId, limit: usize) -> Result<CommitWalk, StorageError> {
        let mut commits = self.block_on(async {
            let client = self.pool.get().await.map_err(conn_error)?;
            let rows = client
                .query(
                    "WITH RECURSIVE walk(id, parents) AS (
                         SELECT id, data -> 'parents' FROM commits
                         WHERE tenant_id = $1 AND id = $2
                       UNION
                         SELECT parent.id, parent.parents
                         FROM walk
                         CROSS JOIN LATERAL jsonb_array_elements(walk.parents) AS p(id)
                         CROSS JOIN LATERAL (
                             SELECT c.id, c.data -> 'parents' AS parents FROM commits c
                             WHERE c.data -> 'id' = p.id AND c.tenant_id = $1
                             LIMIT 1
                         ) parent
                     ),
                     reached AS (
                         SELECT id, row_number() OVER () AS n
                         FROM (SELECT id FROM walk LIMIT $3) first
                     )
                     SELECT c.data
                     FROM reached JOIN commits c ON c.tenant_id = $1 AND c.id = reached.id
                     ORDER BY reached.n",
                    &[
                        &self.tenant_id,
                        &from.as_bytes().as_slice(),
                        &sql_count(limit).saturating_add(1),
                    ],
                )
                .await
                .map_err(|e| StorageError::Backend(with_causes("list commits dag", &e)))?;
            rows.iter()
                .map(|row| commit_from_row(row, "data"))
                .collect::<Result<Vec<_>, _>>()
        })?;
        let truncated = commits.len() > limit;
        commits.truncate(limit);
        Ok(CommitWalk { commits, truncated })
    }

    /// Fold the next commits into the history tables, as the SQLite backend
    /// does, but ordered by the id of the transaction that wrote each commit
    /// rather than by insertion order. With several writers, a commit can
    /// become visible after a later-inserted one has already been folded, and
    /// an insertion-order cursor would step past it for good. Only commits
    /// whose transaction is older than every transaction still running are
    /// read, so no commit can appear behind the cursor later. When commits
    /// are waiting on an older transaction, this waits up to `HISTORY_WAIT`
    /// for it to finish; a writing transaction that runs longer than that,
    /// anywhere on the server, delays history until it ends.
    fn history_extract_batch(&self, batch_size: usize) -> Result<usize, StorageError> {
        let batch_size = sql_count(batch_size.max(1));
        self.block_on(async {
            let mut client = self.pool.get().await.map_err(conn_error)?;
            let tx = client
                .transaction()
                .await
                .map_err(|e| history_error("begin", &e))?;
            // The cursor row is the lock: one extractor per tenant at a time,
            // so concurrent extractors can't fold a commit twice.
            tx.execute(
                "INSERT INTO asg_history_cursor (tenant_id, xid, commit_id, folded)
                 VALUES ($1, 0, ''::BYTEA, 0)
                 ON CONFLICT (tenant_id) DO NOTHING",
                &[&self.tenant_id],
            )
            .await
            .map_err(|e| history_error("cursor", &e))?;
            let cursor = tx
                .query_one(
                    "SELECT xid, commit_id FROM asg_history_cursor
                     WHERE tenant_id = $1 FOR UPDATE",
                    &[&self.tenant_id],
                )
                .await
                .map_err(|e| history_error("cursor", &e))?;
            let (after_xid, after_id): (i64, Vec<u8>) = (cursor.get(0), cursor.get(1));

            // A commit made outside a scoped session is attributed to
            // HISTORY_NAMESPACE_UNKNOWN, as in SQLite.
            let deadline = tokio::time::Instant::now() + HISTORY_WAIT;
            let rows = loop {
                let rows = tx
                    .query(
                        "SELECT c.xid, c.id, c.data, COALESCE(s.scope_namespace, $4) AS namespace
                         FROM commits c
                         LEFT JOIN sessions s
                           ON s.tenant_id = c.tenant_id AND s.id = c.session_id
                         WHERE c.tenant_id = $1
                           AND (c.xid, c.id) > ($2, $3)
                           AND c.xid < pg_snapshot_xmin(pg_current_snapshot())::text::BIGINT
                         ORDER BY c.xid, c.id
                         LIMIT $5",
                        &[
                            &self.tenant_id,
                            &after_xid,
                            &after_id,
                            &HISTORY_NAMESPACE_UNKNOWN,
                            &batch_size,
                        ],
                    )
                    .await
                    .map_err(|e| history_error("select", &e))?;
                if !rows.is_empty() {
                    break rows;
                }
                // Nothing is old enough. If newer commits are waiting on an
                // older transaction, give it a moment to finish.
                let waiting: bool = tx
                    .query_one(
                        "SELECT EXISTS (SELECT 1 FROM commits
                                        WHERE tenant_id = $1 AND (xid, id) > ($2, $3))",
                        &[&self.tenant_id, &after_xid, &after_id],
                    )
                    .await
                    .map_err(|e| history_error("pending", &e))?
                    .get(0);
                if !waiting || tokio::time::Instant::now() >= deadline {
                    return Ok(0);
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            };
            let last = rows.last().expect("a non-empty batch");
            let (last_xid, last_id): (i64, Vec<u8>) = (last.get("xid"), last.get("id"));

            let mut fold = HistoryFold::default();
            for row in &rows {
                let commit = commit_from_row(row, "data")?;
                fold.add(&commit, row.get("namespace"));
            }
            fold.write(&tx, &self.tenant_id).await?;

            // The cursor moves in the same transaction as the rows it covers.
            tx.execute(
                "UPDATE asg_history_cursor SET xid = $2, commit_id = $3, folded = folded + $4
                 WHERE tenant_id = $1",
                &[&self.tenant_id, &last_xid, &last_id, &(rows.len() as i64)],
            )
            .await
            .map_err(|e| history_error("cursor write", &e))?;
            tx.commit().await.map_err(|e| history_error("commit", &e))?;
            Ok(rows.len())
        })
    }

    /// How many commits have been folded. (SQLite reports its last folded
    /// rowid; either way it only grows, and 0 means nothing is extracted.)
    fn history_cursor(&self) -> Result<i64, StorageError> {
        self.block_on(async {
            let client = self.pool.get().await.map_err(conn_error)?;
            let row = client
                .query_opt(
                    "SELECT folded FROM asg_history_cursor WHERE tenant_id = $1",
                    &[&self.tenant_id],
                )
                .await
                .map_err(|e| history_error("cursor read", &e))?;
            Ok(row.map_or(0, |row| row.get(0)))
        })
    }

    fn history_undistilled_commit_count(&self) -> Result<i64, StorageError> {
        self.block_on(async {
            let client = self.pool.get().await.map_err(conn_error)?;
            let row = client
                .query_one(
                    "SELECT COUNT(*) FROM commits c
                     LEFT JOIN asg_history_cursor k ON k.tenant_id = c.tenant_id
                     WHERE c.tenant_id = $1
                       AND (c.xid, c.id) > (COALESCE(k.xid, 0), COALESCE(k.commit_id, ''::BYTEA))",
                    &[&self.tenant_id],
                )
                .await
                .map_err(|e| history_error("undistilled count", &e))?;
            Ok(row.get(0))
        })
    }

    fn history_is_commit_distilled(&self, id: &ObjectId) -> Result<bool, StorageError> {
        self.block_on(async {
            let client = self.pool.get().await.map_err(conn_error)?;
            let row = client
                .query_one(
                    "SELECT EXISTS (
                         SELECT 1 FROM commits c
                         JOIN asg_history_cursor k ON k.tenant_id = c.tenant_id
                         WHERE c.tenant_id = $1 AND c.id = $2
                           AND (c.xid, c.id) <= (k.xid, k.commit_id)
                     )",
                    &[&self.tenant_id, &id.as_bytes().as_slice()],
                )
                .await
                .map_err(|e| history_error("distilled", &e))?;
            Ok(row.get(0))
        })
    }

    fn history_rollup(&self) -> Result<Vec<HistoryRollupRow>, StorageError> {
        self.block_on(async {
            let client = self.pool.get().await.map_err(conn_error)?;
            let rows = client
                .query(
                    "SELECT day, namespace, agent_id, intent_category, commit_count,
                            first_ts, last_ts
                     FROM asg_history_commit_rollup
                     WHERE tenant_id = $1
                     ORDER BY day, namespace, agent_id, intent_category",
                    &[&self.tenant_id],
                )
                .await
                .map_err(|e| history_error("rollup read", &e))?;
            Ok(rows
                .iter()
                .map(|row| HistoryRollupRow {
                    day: row.get(0),
                    namespace: row.get(1),
                    agent_id: row.get(2),
                    intent_category: row.get(3),
                    commit_count: row.get(4),
                    first_ts: row.get(5),
                    last_ts: row.get(6),
                })
                .collect())
        })
    }

    fn history_milestones(&self, limit: usize) -> Result<Vec<HistoryMilestoneRow>, StorageError> {
        self.block_on(async {
            let client = self.pool.get().await.map_err(conn_error)?;
            let rows = client
                .query(
                    "SELECT commit_id, kind, timestamp, day, namespace, agent_id, description,
                            state_root, git_sha
                     FROM asg_history_milestone
                     WHERE tenant_id = $1
                     ORDER BY timestamp DESC
                     LIMIT $2",
                    &[&self.tenant_id, &sql_count(limit)],
                )
                .await
                .map_err(|e| history_error("milestones read", &e))?;
            Ok(rows
                .iter()
                .filter_map(|row| {
                    let commit_id = object_id_from_bytes(row.get(0))?;
                    let state_root: Option<&[u8]> = row.get(7);
                    Some(HistoryMilestoneRow {
                        commit_id,
                        kind: row.get(1),
                        timestamp: row.get(2),
                        day: row.get(3),
                        namespace: row.get(4),
                        agent_id: row.get(5),
                        description: row.get(6),
                        state_root: state_root.and_then(object_id_from_bytes),
                        git_sha: row.get(8),
                    })
                })
                .collect())
        })
    }

    fn history_retained_state_roots(&self) -> Result<Vec<ObjectId>, StorageError> {
        self.block_on(async {
            let client = self.pool.get().await.map_err(conn_error)?;
            let rows = client
                .query(
                    "SELECT DISTINCT state_root FROM asg_history_milestone
                     WHERE tenant_id = $1 AND state_root IS NOT NULL",
                    &[&self.tenant_id],
                )
                .await
                .map_err(|e| history_error("retained roots", &e))?;
            Ok(rows
                .iter()
                .filter_map(|row| object_id_from_bytes(row.get(0)))
                .collect())
        })
    }

    fn history_gc_reachability(
        &self,
        roots: &[ObjectId],
        batch: usize,
    ) -> Result<GcReachability, StorageError> {
        let batch = sql_count(batch.max(1));
        self.block_on(async {
            // The mark lives in temp tables, so it runs on a connection of
            // its own that is closed afterwards rather than pooled.
            let client = self.pool.get().await.map_err(conn_error)?;
            let result = async {
                gc_mark(&client, &self.tenant_id, roots, batch).await?;
                let row = client
                    .query_one(
                        "SELECT (SELECT count(*) FROM gc_mark),
                                (SELECT count(*) FROM objects WHERE tenant_id = $1)",
                        &[&self.tenant_id],
                    )
                    .await
                    .map_err(|e| gc_error("counts", &e))?;
                Ok::<_, StorageError>((row.get::<_, i64>(0), row.get::<_, i64>(1)))
            }
            .await;
            drop(deadpool_postgres::Object::take(client));
            let (live, total) = result?;
            Ok(GcReachability {
                total_objects: total,
                live_objects: live,
                reclaimable_objects: (total - live).max(0),
                roots_walked: roots.len(),
            })
        })
    }

    /// Mark everything reachable from `roots`, the current ref tips and the
    /// pinned roots, then delete this tenant's other objects in batches, each
    /// its own transaction. Writers keep going meanwhile: whatever was written
    /// since the sweep started, or within the grace period before it, is
    /// spared, and each delete re-checks the write time, so an object stored
    /// again mid-sweep stays. `deleted_target` can therefore exceed
    /// `objects_deleted`.
    fn history_gc_sweep(&self, roots: &[ObjectId], batch: usize) -> Result<GcSweep, StorageError> {
        let batch = sql_count(batch.max(1));
        let held = self
            .gc_conn
            .lock()
            .map_err(|e| StorageError::Backend(e.to_string()))?
            .take();
        self.block_on(async {
            let (client, own) = match held {
                Some(client) => (client, false),
                None => (self.gc_lock().await?, true),
            };
            let result = gc_sweep(
                &client,
                &self.tenant_id,
                roots,
                batch,
                self.gc_grace.as_secs_f64(),
            )
            .await;
            if own {
                gc_release(client).await;
            } else {
                *self
                    .gc_conn
                    .lock()
                    .map_err(|e| StorageError::Backend(e.to_string()))? = Some(client);
            }
            result
        })
    }

    /// Take this tenant's sweep lock, so two sweeps can't run at once. Unlike
    /// SQLite's, it doesn't hold writers off: the grace period keeps their
    /// work safe (see [`PostgresStorage::with_gc_grace`]).
    fn history_gc_lock_writers(&self) -> Result<(), StorageError> {
        let mut held = self
            .gc_conn
            .lock()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        if held.is_some() {
            return Err(StorageError::Backend(
                "gc lock writers: a sweep already holds the lock".into(),
            ));
        }
        *held = Some(self.block_on(self.gc_lock())?);
        Ok(())
    }

    /// Release the sweep lock. A sweep's deletes are already committed batch
    /// by batch, so `commit` changes nothing here.
    fn history_gc_unlock_writers(&self, _commit: bool) -> Result<(), StorageError> {
        let held = self
            .gc_conn
            .lock()
            .map_err(|e| StorageError::Backend(e.to_string()))?
            .take();
        if let Some(client) = held {
            self.block_on(async {
                gc_release(client).await;
                Ok(())
            })?;
        }
        Ok(())
    }

    fn gc_pin_root(&self, key: &str, root: &ObjectId) -> Result<(), StorageError> {
        self.block_on(async {
            let client = self.pool.get().await.map_err(conn_error)?;
            client
                .execute(
                    "INSERT INTO gc_pending_roots (tenant_id, key, root, updated_at)
                     VALUES ($1, $2, $3, now())
                     ON CONFLICT (tenant_id, key)
                     DO UPDATE SET root = EXCLUDED.root, updated_at = now()",
                    &[&self.tenant_id, &key, &root.as_bytes().as_slice()],
                )
                .await
                .map_err(|e| gc_error("pin root", &e))?;
            Ok(())
        })
    }

    fn gc_unpin_root(&self, key: &str) -> Result<(), StorageError> {
        self.block_on(async {
            let client = self.pool.get().await.map_err(conn_error)?;
            client
                .execute(
                    "DELETE FROM gc_pending_roots WHERE tenant_id = $1 AND key = $2",
                    &[&self.tenant_id, &key],
                )
                .await
                .map_err(|e| gc_error("unpin root", &e))?;
            Ok(())
        })
    }

    fn gc_pinned_roots(&self, max_age: std::time::Duration) -> Result<Vec<ObjectId>, StorageError> {
        self.block_on(async {
            let client = self.pool.get().await.map_err(conn_error)?;
            gc_pinned(&**client, &self.tenant_id, max_age.as_secs_f64()).await
        })
    }

    /// Newest first, by the order the commits' transactions got ids (commits
    /// from before that was recorded come last, newest first by timestamp).
    fn commit_state_roots_recent_first(&self) -> Result<Vec<ObjectId>, StorageError> {
        self.block_on(async {
            let client = self.pool.get().await.map_err(conn_error)?;
            let rows = client
                .query(
                    "SELECT data -> 'state_root' FROM commits WHERE tenant_id = $1
                     ORDER BY xid DESC, timestamp DESC, id DESC",
                    &[&self.tenant_id],
                )
                .await
                .map_err(|e| gc_error("commit roots", &e))?;
            rows.iter()
                .map(|row| {
                    serde_json::from_value(row.get(0))
                        .map_err(|e| StorageError::Serialization(e.to_string()))
                })
                .collect()
        })
    }

    /// This tenant's object and commit counts, and the bytes its rows take
    /// in each table (before indexes and free space; the tables are shared
    /// with other tenants). Reads every row, so it is slow on a large store.
    fn history_store_shape(&self) -> Result<StoreShape, StorageError> {
        self.block_on(async {
            let client = self.pool.get().await.map_err(conn_error)?;
            let row = client
                .query_one(
                    "SELECT (SELECT count(*) FROM objects WHERE tenant_id = $1),
                            (SELECT count(*) FROM commits WHERE tenant_id = $1),
                            (SELECT COALESCE(sum(pg_column_size(o.*)), 0)::BIGINT
                               FROM objects o WHERE tenant_id = $1),
                            (SELECT COALESCE(sum(pg_column_size(c.*)), 0)::BIGINT
                               FROM commits c WHERE tenant_id = $1)",
                    &[&self.tenant_id],
                )
                .await
                .map_err(|e| gc_error("store shape", &e))?;
            let (objects_bytes, commits_bytes): (i64, i64) = (row.get(2), row.get(3));
            let mut tables = vec![
                TableBytes {
                    name: "objects".into(),
                    bytes: objects_bytes,
                },
                TableBytes {
                    name: "commits".into(),
                    bytes: commits_bytes,
                },
            ];
            tables.sort_by_key(|t| std::cmp::Reverse(t.bytes));
            Ok(StoreShape {
                objects: row.get(0),
                commits: row.get(1),
                total_bytes: objects_bytes + commits_bytes,
                tables,
                dbstat_available: true,
            })
        })
    }
}

impl PostgresStorage {
    /// A connection holding this tenant's sweep lock (a session advisory
    /// lock). Fails at once if another sweep holds it. Closing the connection
    /// releases the lock, so it is never returned to the pool.
    async fn gc_lock(&self) -> Result<deadpool_postgres::Object, StorageError> {
        let client = self.pool.get().await.map_err(conn_error)?;
        let locked: bool = client
            .query_one(
                "SELECT pg_try_advisory_lock(hashtextextended('asg-gc:' || $1, 0))",
                &[&self.tenant_id],
            )
            .await
            .map_err(|e| gc_error("lock", &e))?
            .get(0);
        if !locked {
            return Err(StorageError::Backend(
                "gc lock writers: another sweep is running on this store".into(),
            ));
        }
        Ok(client)
    }
}

/// Release a sweep connection: unlock, then close it rather than pool it, so
/// its temp tables go too. The explicit unlock matters: the server only
/// notices a closed connection a moment later, and a sweep starting in that
/// moment would find the lock still held. (If the unlock fails, the
/// connection is broken and the server releases the lock with it.)
async fn gc_release(client: deadpool_postgres::Object) {
    let _ = client
        .batch_execute("SELECT pg_advisory_unlock_all()")
        .await;
    drop(deadpool_postgres::Object::take(client));
}

/// Fill the temp table `gc_mark` with every object of `tenant_id` reachable
/// from `roots`, breadth first in batches of `batch`, so the mark set lives in
/// the database rather than in memory. Each statement commits on its own: a
/// long transaction would hold back every tenant's history extraction.
async fn gc_mark(
    client: &deadpool_postgres::Object,
    tenant_id: &str,
    roots: &[ObjectId],
    batch: i64,
) -> Result<(), StorageError> {
    client
        .batch_execute(
            "CREATE TEMP TABLE IF NOT EXISTS gc_mark (id BYTEA PRIMARY KEY);
             CREATE TEMP TABLE IF NOT EXISTS gc_frontier (id BYTEA PRIMARY KEY);
             TRUNCATE gc_mark, gc_frontier;",
        )
        .await
        .map_err(|e| gc_error("mark setup", &e))?;
    // Marks only ids that are stored objects, so the mark is a subset of the
    // tenant's objects; each newly marked id joins the frontier.
    let mark = "WITH marked AS (
                    INSERT INTO gc_mark (id)
                    SELECT o.id FROM objects o WHERE o.tenant_id = $1 AND o.id = ANY($2)
                    ON CONFLICT DO NOTHING
                    RETURNING id
                )
                INSERT INTO gc_frontier (id) SELECT id FROM marked";
    let seeds: Vec<&[u8]> = roots.iter().map(|r| r.as_bytes().as_slice()).collect();
    client
        .execute(mark, &[&tenant_id, &seeds])
        .await
        .map_err(|e| gc_error("mark roots", &e))?;
    loop {
        let rows = client
            .query(
                "WITH next AS (
                     DELETE FROM gc_frontier
                     WHERE id IN (SELECT id FROM gc_frontier LIMIT $2)
                     RETURNING id
                 )
                 SELECT o.data FROM next
                 LEFT JOIN objects o ON o.tenant_id = $1 AND o.id = next.id",
                &[&tenant_id, &batch],
            )
            .await
            .map_err(|e| gc_error("mark frontier", &e))?;
        if rows.is_empty() {
            return Ok(());
        }
        let mut children = std::collections::HashSet::new();
        for row in &rows {
            let Some(data) = row.get::<_, Option<serde_json::Value>>(0) else {
                continue;
            };
            let obj: Object = serde_json::from_value(data)
                .map_err(|e| StorageError::Serialization(e.to_string()))?;
            children.extend(child_ids(&obj));
        }
        let children: Vec<&[u8]> = children.iter().map(|c| c.as_bytes().as_slice()).collect();
        client
            .execute(mark, &[&tenant_id, &children])
            .await
            .map_err(|e| gc_error("mark children", &e))?;
    }
}

/// The body of [`PostgresStorage::history_gc_sweep`], on a connection that
/// holds the tenant's sweep lock.
async fn gc_sweep(
    client: &deadpool_postgres::Object,
    tenant_id: &str,
    roots: &[ObjectId],
    batch: i64,
    grace_secs: f64,
) -> Result<GcSweep, StorageError> {
    // Fixed before anything is read: everything written after this time, or
    // within the grace period before it, is spared.
    let cutoff: DateTime<Utc> = client
        .query_one("SELECT now() - make_interval(secs => $1)", &[&grace_secs])
        .await
        .map_err(|e| gc_error("cutoff", &e))?
        .get(0);
    let count = || async {
        client
            .query_one(
                "SELECT count(*) FROM objects WHERE tenant_id = $1",
                &[&tenant_id],
            )
            .await
            .map(|row| row.get::<_, i64>(0))
            .map_err(|e| gc_error("count", &e))
    };
    let objects_before = count().await?;

    // Whatever the caller's roots, keep every current ref tip and every
    // open speculation.
    let mut keep = roots.to_vec();
    let tips = client
        .query(
            "SELECT c.data -> 'state_root' FROM refs r
             JOIN commits c ON c.tenant_id = r.tenant_id AND c.id = r.target
             WHERE r.tenant_id = $1",
            &[&tenant_id],
        )
        .await
        .map_err(|e| gc_error("ref tips", &e))?;
    for row in &tips {
        keep.push(
            serde_json::from_value(row.get(0))
                .map_err(|e| StorageError::Serialization(e.to_string()))?,
        );
    }
    keep.extend(gc_pinned(&***client, tenant_id, GC_PIN_MAX_AGE.as_secs_f64()).await?);

    gc_mark(client, tenant_id, &keep, batch).await?;

    // Walk the tenant's objects in key order, a batch of unmarked ones at a
    // time, and delete each batch on its own. Each statement is short, so no
    // long write transaction holds back history extraction (every tenant's).
    // The delete re-checks the write time: an object stored again since the
    // batch was read stays.
    let mut after: Vec<u8> = Vec::new();
    let (mut deleted_target, mut objects_deleted) = (0, 0);
    loop {
        let dead: Vec<Vec<u8>> = client
            .query(
                "SELECT o.id FROM objects o
                 WHERE o.tenant_id = $1 AND o.id > $2 AND o.written_at < $3
                   AND NOT EXISTS (SELECT 1 FROM gc_mark m WHERE m.id = o.id)
                 ORDER BY o.id
                 LIMIT $4",
                &[&tenant_id, &after, &cutoff, &batch],
            )
            .await
            .map_err(|e| gc_error("dead batch", &e))?
            .iter()
            .map(|row| row.get(0))
            .collect();
        let Some(last) = dead.last() else {
            break;
        };
        after = last.clone();
        deleted_target += dead.len() as i64;
        objects_deleted += client
            .execute(
                "DELETE FROM objects
                 WHERE tenant_id = $1 AND id = ANY($2) AND written_at < $3",
                &[&tenant_id, &dead, &cutoff],
            )
            .await
            .map_err(|e| gc_error("delete", &e))? as i64;
    }

    Ok(GcSweep {
        objects_before,
        objects_deleted,
        objects_after: count().await?,
        deleted_target,
    })
}

async fn gc_pinned(
    client: &(impl tokio_postgres::GenericClient + Sync),
    tenant_id: &str,
    max_age_secs: f64,
) -> Result<Vec<ObjectId>, StorageError> {
    let rows = client
        .query(
            "SELECT root FROM gc_pending_roots
             WHERE tenant_id = $1 AND updated_at >= now() - make_interval(secs => $2)",
            &[&tenant_id, &max_age_secs],
        )
        .await
        .map_err(|e| gc_error("pinned roots", &e))?;
    Ok(rows
        .iter()
        .filter_map(|row| object_id_from_bytes(row.get(0)))
        .collect())
}

/// The child object ids a node references (none for atoms): the edges the
/// GC mark follows.
fn child_ids(obj: &Object) -> Vec<ObjectId> {
    match obj {
        Object::Node(Node::Map(entries)) => entries.values().copied().collect(),
        Object::Node(Node::List(items)) | Object::Node(Node::Set(items)) => items.clone(),
        Object::Atom(_) => Vec::new(),
    }
}

/// How long an open speculation's pin protects its objects without being
/// refreshed, as in SQLite.
const GC_PIN_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

fn gc_error(step: &str, e: &tokio_postgres::Error) -> StorageError {
    StorageError::Backend(with_causes(&format!("gc {step}"), e))
}

async fn leaf_set_built(
    client: &(impl tokio_postgres::GenericClient + Sync),
    tenant_id: &str,
    namespace: &str,
    ref_name: &str,
) -> Result<bool, StorageError> {
    let row = client
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM leaf_index_refs
                            WHERE tenant_id = $1 AND namespace = $2 AND ref = $3)",
            &[&tenant_id, &namespace, &ref_name],
        )
        .await
        .map_err(|e| leaf_error("built check", &e))?;
    Ok(row.get(0))
}

/// Write `(path, value)` leaves, replacing any value already at a path. A
/// path given twice keeps its last value.
async fn upsert_leaves(
    tx: &deadpool_postgres::Transaction<'_>,
    tenant_id: &str,
    namespace: &str,
    ref_name: &str,
    leaves: &[(String, String)],
) -> Result<(), StorageError> {
    const CHUNK: usize = 1000;
    let latest: std::collections::HashMap<&str, &str> = leaves
        .iter()
        .map(|(path, value)| (path.as_str(), value.as_str()))
        .collect();
    let latest: Vec<(&str, &str)> = latest.into_iter().collect();
    for chunk in latest.chunks(CHUNK) {
        let paths: Vec<&str> = chunk.iter().map(|(p, _)| *p).collect();
        let values: Vec<&str> = chunk.iter().map(|(_, v)| *v).collect();
        tx.execute(
            "INSERT INTO leaf_rows (tenant_id, namespace, ref, path, value)
             SELECT $1, $2, $3, l.path, l.value FROM unnest($4::TEXT[], $5::TEXT[]) AS l(path, value)
             ON CONFLICT (tenant_id, namespace, ref, path) DO UPDATE SET value = EXCLUDED.value",
            &[&tenant_id, &namespace, &ref_name, &paths, &values],
        )
        .await
        .map_err(|e| leaf_error("write", &e))?;
    }
    Ok(())
}

/// An `ILIKE` pattern matching values that contain `needle` literally.
fn like_contains(needle: &str) -> String {
    let mut pattern = String::with_capacity(needle.len() + 2);
    pattern.push('%');
    for ch in needle.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            pattern.push('\\');
        }
        pattern.push(ch);
    }
    pattern.push('%');
    pattern
}

fn leaf_error(step: &str, e: &tokio_postgres::Error) -> StorageError {
    StorageError::Backend(with_causes(&format!("leaf index {step}"), e))
}

/// How long the history extractor waits for an older transaction that newer
/// commits are waiting on. Writes take milliseconds; this only runs out when
/// some write transaction on the server stays open.
const HISTORY_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// A `usize` count or limit as the `BIGINT` Postgres compares it with.
fn sql_count(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

fn object_id_from_bytes(bytes: &[u8]) -> Option<ObjectId> {
    <[u8; 32]>::try_from(bytes).ok().map(ObjectId::from_bytes)
}

fn commit_from_row(row: &tokio_postgres::Row, column: &str) -> Result<Commit, StorageError> {
    let data: serde_json::Value = row.get(column);
    serde_json::from_value(data).map_err(|e| StorageError::Serialization(e.to_string()))
}

fn history_error(step: &str, e: &tokio_postgres::Error) -> StorageError {
    StorageError::Backend(with_causes(&format!("history {step}"), e))
}

/// A rollup bucket's key: (day, namespace, agent, intent category).
type BucketKey = (String, String, String, String);

/// A rollup bucket's commit count and first and last timestamps.
type Bucket = (i64, String, String);

/// One batch of commits folded into rollup buckets and milestones, written
/// with one statement each.
#[derive(Default)]
struct HistoryFold {
    rollup: std::collections::BTreeMap<BucketKey, Bucket>,
    milestones: Vec<Milestone>,
}

struct Milestone {
    commit_id: Vec<u8>,
    timestamp: String,
    day: String,
    namespace: String,
    agent_id: String,
    description: String,
    state_root: Option<Vec<u8>>,
    git_sha: Option<String>,
}

impl HistoryFold {
    /// The same fields, bucketing and milestone rules as SQLite's extractor.
    fn add(&mut self, commit: &Commit, namespace: String) {
        let day = commit.timestamp.format("%Y-%m-%d").to_string();
        let ts = commit.timestamp.to_rfc3339();
        let key = (
            day.clone(),
            namespace.clone(),
            commit.agent_id.clone(),
            format!("{:?}", commit.intent.category),
        );
        let bucket = self
            .rollup
            .entry(key)
            .or_insert_with(|| (0, ts.clone(), ts.clone()));
        bucket.0 += 1;
        if ts < bucket.1 {
            bucket.1 = ts.clone();
        }
        if ts > bucket.2 {
            bucket.2 = ts.clone();
        }

        if commit.intent.category == IntentCategory::Checkpoint {
            // Pin the snapshot only when the checkpoint asks to (see SQLite).
            let state_root = commit
                .intent
                .tags
                .iter()
                .any(|t| t == TAG_PIN_STATE)
                .then(|| commit.state_root.as_bytes().to_vec());
            let git_sha = commit
                .intent
                .tags
                .iter()
                .find_map(|t| t.strip_prefix(TAG_GIT_REVISION))
                .filter(|sha| !sha.is_empty())
                .map(str::to_string);
            self.milestones.push(Milestone {
                commit_id: commit.id.as_bytes().to_vec(),
                timestamp: ts,
                day,
                namespace,
                agent_id: commit.agent_id.clone(),
                description: commit.intent.description.clone(),
                state_root,
                git_sha,
            });
        }
    }

    async fn write(
        self,
        tx: &deadpool_postgres::Transaction<'_>,
        tenant_id: &str,
    ) -> Result<(), StorageError> {
        let mut days = Vec::new();
        let mut namespaces = Vec::new();
        let mut agents = Vec::new();
        let mut categories = Vec::new();
        let mut counts = Vec::new();
        let mut firsts = Vec::new();
        let mut lasts = Vec::new();
        for ((day, namespace, agent, category), (count, first, last)) in self.rollup {
            days.push(day);
            namespaces.push(namespace);
            agents.push(agent);
            categories.push(category);
            counts.push(count);
            firsts.push(first);
            lasts.push(last);
        }
        tx.execute(
            "INSERT INTO asg_history_commit_rollup AS r
                 (tenant_id, day, namespace, agent_id, intent_category, commit_count,
                  first_ts, last_ts)
             SELECT $1, * FROM unnest($2::TEXT[], $3::TEXT[], $4::TEXT[], $5::TEXT[],
                                      $6::BIGINT[], $7::TEXT[], $8::TEXT[])
             ON CONFLICT (tenant_id, day, namespace, agent_id, intent_category) DO UPDATE SET
                 commit_count = r.commit_count + EXCLUDED.commit_count,
                 first_ts = LEAST(r.first_ts, EXCLUDED.first_ts),
                 last_ts = GREATEST(r.last_ts, EXCLUDED.last_ts)",
            &[
                &tenant_id,
                &days,
                &namespaces,
                &agents,
                &categories,
                &counts,
                &firsts,
                &lasts,
            ],
        )
        .await
        .map_err(|e| history_error("rollup upsert", &e))?;

        if self.milestones.is_empty() {
            return Ok(());
        }
        let m = &self.milestones;
        let ids: Vec<&[u8]> = m.iter().map(|m| m.commit_id.as_slice()).collect();
        let timestamps: Vec<&str> = m.iter().map(|m| m.timestamp.as_str()).collect();
        let days: Vec<&str> = m.iter().map(|m| m.day.as_str()).collect();
        let namespaces: Vec<&str> = m.iter().map(|m| m.namespace.as_str()).collect();
        let agents: Vec<&str> = m.iter().map(|m| m.agent_id.as_str()).collect();
        let descriptions: Vec<&str> = m.iter().map(|m| m.description.as_str()).collect();
        let roots: Vec<Option<&[u8]>> = m.iter().map(|m| m.state_root.as_deref()).collect();
        let shas: Vec<Option<&str>> = m.iter().map(|m| m.git_sha.as_deref()).collect();
        tx.execute(
            "INSERT INTO asg_history_milestone
                 (tenant_id, commit_id, kind, timestamp, day, namespace, agent_id,
                  description, state_root, git_sha)
             SELECT $1, m.id, 'checkpoint', m.ts, m.day, m.ns, m.agent, m.description,
                    m.root, m.sha
             FROM unnest($2::BYTEA[], $3::TEXT[], $4::TEXT[], $5::TEXT[], $6::TEXT[],
                         $7::TEXT[], $8::BYTEA[], $9::TEXT[])
                  AS m(id, ts, day, ns, agent, description, root, sha)
             ON CONFLICT DO NOTHING",
            &[
                &tenant_id,
                &ids,
                &timestamps,
                &days,
                &namespaces,
                &agents,
                &descriptions,
                &roots,
                &shas,
            ],
        )
        .await
        .map_err(|e| history_error("milestones", &e))?;
        Ok(())
    }
}

// ─── RefStore ───────────────────────────────────────────────

impl RefStore for PostgresStorage {
    fn create_namespace(&self, namespace: &Namespace) -> Result<(), StorageError> {
        self.block_on(async {
            let client = self.pool.get().await.map_err(conn_error)?;

            let rows = client
                .execute(
                    "INSERT INTO namespaces (tenant_id, name, created_at)
                     VALUES ($1, $2, NOW()::TEXT)
                     ON CONFLICT DO NOTHING",
                    &[&self.tenant_id, &namespace.as_str()],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("create namespace: {}", e)))?;
            if rows == 0 {
                return Err(StorageError::NamespaceAlreadyExists(
                    namespace.as_str().to_string(),
                ));
            }
            Ok(())
        })
    }

    fn list_namespaces(&self) -> Result<Vec<Namespace>, StorageError> {
        self.block_on(async {
            let client = self.pool.get().await.map_err(conn_error)?;

            let rows = client
                .query(
                    "SELECT name FROM namespaces WHERE tenant_id = $1 ORDER BY name",
                    &[&self.tenant_id],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("list namespaces: {}", e)))?;

            let mut result = Vec::new();
            for row in rows {
                let name: String = row.get("name");
                let ns = Namespace::new(name).map_err(|e| {
                    StorageError::Backend(format!("invalid namespace in db: {}", e))
                })?;
                result.push(ns);
            }
            Ok(result)
        })
    }

    fn get_ref(&self, namespace: &Namespace, name: &str) -> Result<Option<ObjectId>, StorageError> {
        self.block_on(async {
            let client = self.pool.get().await.map_err(conn_error)?;

            let ns_row = client
                .query_opt(
                    "SELECT 1 FROM namespaces WHERE tenant_id = $1 AND name = $2",
                    &[&self.tenant_id, &namespace.as_str()],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("check namespace: {}", e)))?;
            if ns_row.is_none() {
                return Err(StorageError::NamespaceNotFound(
                    namespace.as_str().to_string(),
                ));
            }

            let row = client
                .query_opt(
                    "SELECT target FROM refs
                     WHERE tenant_id = $1 AND namespace = $2 AND name = $3",
                    &[&self.tenant_id, &namespace.as_str(), &name],
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

    fn set_ref(
        &self,
        namespace: &Namespace,
        name: &str,
        target: ObjectId,
    ) -> Result<(), StorageError> {
        self.block_on(async {
            let client = self.pool.get().await.map_err(conn_error)?;

            let ns_row = client
                .query_opt(
                    "SELECT 1 FROM namespaces WHERE tenant_id = $1 AND name = $2",
                    &[&self.tenant_id, &namespace.as_str()],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("check namespace: {}", e)))?;
            if ns_row.is_none() {
                return Err(StorageError::NamespaceNotFound(
                    namespace.as_str().to_string(),
                ));
            }

            client
                .execute(
                    "INSERT INTO refs (tenant_id, namespace, name, target)
                     VALUES ($1, $2, $3, $4)
                     ON CONFLICT (tenant_id, namespace, name) DO UPDATE SET target = $4",
                    &[
                        &self.tenant_id,
                        &namespace.as_str(),
                        &name,
                        &target.as_bytes().as_slice(),
                    ],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("set ref: {}", e)))?;

            Ok(())
        })
    }

    fn cas_ref(
        &self,
        namespace: &Namespace,
        name: &str,
        expected: ObjectId,
        new: ObjectId,
    ) -> Result<bool, StorageError> {
        self.block_on(async {
            let client = self.pool.get().await.map_err(conn_error)?;

            let ns_row = client
                .query_opt(
                    "SELECT 1 FROM namespaces WHERE tenant_id = $1 AND name = $2",
                    &[&self.tenant_id, &namespace.as_str()],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("check namespace: {}", e)))?;
            if ns_row.is_none() {
                return Err(StorageError::NamespaceNotFound(
                    namespace.as_str().to_string(),
                ));
            }

            let rows = client
                .execute(
                    "UPDATE refs SET target = $1
                     WHERE tenant_id = $2 AND namespace = $3
                       AND name = $4 AND target = $5",
                    &[
                        &new.as_bytes().as_slice(),
                        &self.tenant_id,
                        &namespace.as_str(),
                        &name,
                        &expected.as_bytes().as_slice(),
                    ],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("cas ref: {}", e)))?;

            Ok(rows > 0)
        })
    }

    fn create_ref(
        &self,
        namespace: &Namespace,
        name: &str,
        target: ObjectId,
    ) -> Result<bool, StorageError> {
        self.block_on(async {
            let client = self.pool.get().await.map_err(conn_error)?;
            self.require_namespace(&client, namespace).await?;
            let rows = client
                .execute(
                    "INSERT INTO refs (tenant_id, namespace, name, target)
                     VALUES ($1, $2, $3, $4)
                     ON CONFLICT DO NOTHING",
                    &[
                        &self.tenant_id,
                        &namespace.as_str(),
                        &name,
                        &target.as_bytes().as_slice(),
                    ],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("create ref: {}", e)))?;
            Ok(rows > 0)
        })
    }

    fn cas_delete_ref(
        &self,
        namespace: &Namespace,
        name: &str,
        expected: ObjectId,
    ) -> Result<bool, StorageError> {
        self.block_on(async {
            let client = self.pool.get().await.map_err(conn_error)?;
            self.require_namespace(&client, namespace).await?;
            let rows = client
                .execute(
                    "DELETE FROM refs
                     WHERE tenant_id = $1 AND namespace = $2
                       AND name = $3 AND target = $4",
                    &[
                        &self.tenant_id,
                        &namespace.as_str(),
                        &name,
                        &expected.as_bytes().as_slice(),
                    ],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("cas delete ref: {}", e)))?;
            Ok(rows > 0)
        })
    }

    fn list_refs(
        &self,
        namespace: &Namespace,
        prefix: &str,
    ) -> Result<Vec<(String, ObjectId)>, StorageError> {
        self.block_on(async {
            let client = self.pool.get().await.map_err(conn_error)?;

            let ns_row = client
                .query_opt(
                    "SELECT 1 FROM namespaces WHERE tenant_id = $1 AND name = $2",
                    &[&self.tenant_id, &namespace.as_str()],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("check namespace: {}", e)))?;
            if ns_row.is_none() {
                return Err(StorageError::NamespaceNotFound(
                    namespace.as_str().to_string(),
                ));
            }

            let pattern = format!("{}%", prefix);
            let rows = client
                .query(
                    "SELECT name, target FROM refs
                     WHERE tenant_id = $1 AND namespace = $2 AND name LIKE $3
                     ORDER BY name",
                    &[&self.tenant_id, &namespace.as_str(), &pattern],
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

    fn delete_ref(&self, namespace: &Namespace, name: &str) -> Result<bool, StorageError> {
        self.block_on(async {
            let client = self.pool.get().await.map_err(conn_error)?;

            let ns_row = client
                .query_opt(
                    "SELECT 1 FROM namespaces WHERE tenant_id = $1 AND name = $2",
                    &[&self.tenant_id, &namespace.as_str()],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("check namespace: {}", e)))?;
            if ns_row.is_none() {
                return Err(StorageError::NamespaceNotFound(
                    namespace.as_str().to_string(),
                ));
            }

            let rows = client
                .execute(
                    "DELETE FROM refs WHERE tenant_id = $1 AND namespace = $2 AND name = $3",
                    &[&self.tenant_id, &namespace.as_str(), &name],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("delete ref: {}", e)))?;

            Ok(rows > 0)
        })
    }

    fn delete_namespace(&self, namespace: &Namespace) -> Result<bool, StorageError> {
        if namespace.as_str() == Namespace::DEFAULT {
            return Err(StorageError::InvalidOperation(
                "cannot delete the 'default' namespace".to_string(),
            ));
        }
        let ns = namespace.as_str().to_string();
        let tid = self.tenant_id.clone();
        self.block_on(async {
            let mut client = self.pool.get().await.map_err(conn_error)?;
            // Refs and the namespace row go together or not at all; separately,
            // a failure in between left refs deleted under a namespace that
            // still existed.
            let tx = client
                .transaction()
                .await
                .map_err(|e| StorageError::Backend(format!("begin delete namespace: {e}")))?;
            tx.execute(
                "DELETE FROM refs WHERE tenant_id = $1 AND namespace = $2",
                &[&tid, &ns],
            )
            .await
            .map_err(|e| StorageError::Backend(format!("delete namespace refs: {e}")))?;
            let rows = tx
                .execute(
                    "DELETE FROM namespaces WHERE tenant_id = $1 AND name = $2",
                    &[&tid, &ns],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("delete namespace: {e}")))?;
            tx.commit()
                .await
                .map_err(|e| StorageError::Backend(format!("commit delete namespace: {e}")))?;
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
    seal_hash: Option<Vec<u8>>,
    namespace: Option<String>,
    scope: String,
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
        // A 32-byte digest or nothing; a short or corrupt value reads as
        // absent rather than failing the whole read, as in SQLite.
        seal_hash: seal_hash.and_then(|b| {
            <[u8; 32]>::try_from(b.as_slice())
                .ok()
                .map(ObjectId::from_bytes)
        }),
        commits: Vec::new(),
        agents,
        branches: Vec::new(),
        scope: EpochScope::from_storage_str(&scope),
        namespace,
        tags,
        sealed_commits,
    })
}

/// `commit_count` is an `INTEGER` column, so it must be bound as `i32`:
/// binding an `i64` fails every statement ("error serializing parameter").
fn commit_count_param(n: usize) -> i32 {
    i32::try_from(n).unwrap_or(i32::MAX)
}

/// The columns `row_to_epoch` reads, in the order it takes them.
const EPOCH_COLUMNS: &str = "id, description, status, created_at, sealed_at, summary,
     root_intents, agents, tags, sealed_commits, seal_hash, namespace, scope";

fn epoch_from_row(row: &tokio_postgres::Row) -> Result<Epoch, StorageError> {
    row_to_epoch(
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
        row.get("seal_hash"),
        row.get("namespace"),
        row.get("scope"),
    )
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
        scope_namespace: scope_namespace
            .as_deref()
            .and_then(|s| Namespace::new(s).ok()),
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
            let client = self.pool.get().await.map_err(conn_error)?;

            client
                .execute(
                    "INSERT INTO epochs
                     (tenant_id, id, description, status, created_at, sealed_at,
                      summary, root_intents, agents, tags, commit_count, sealed_commits,
                      namespace, scope)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
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
                        &commit_count_param(epoch.commits.len()),
                        &sealed_commits,
                        &epoch.namespace,
                        &epoch.scope.as_storage_str(),
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
        seal_hash: &ObjectId,
    ) -> Result<(), StorageError> {
        let sc_json = serde_json::to_string(sealed_commits)
            .map_err(|e| StorageError::Serialization(e.to_string()))?;

        self.block_on(async {
            let client = self.pool.get().await.map_err(conn_error)?;

            // One conditional UPDATE, so two sealers can't both pass a status
            // check and overwrite each other's seal.
            let sealed = client
                .execute(
                    "UPDATE epochs
                     SET status = 'Sealed', sealed_at = $1, summary = $2,
                         sealed_commits = $3, seal_hash = $4, commit_count = $5
                     WHERE tenant_id = $6 AND id = $7
                       AND status NOT IN ('Sealed', 'Archived')",
                    &[
                        &sealed_at.to_rfc3339(),
                        &summary,
                        &sc_json,
                        &seal_hash.as_bytes().as_slice(),
                        &commit_count_param(sealed_commits.len()),
                        &self.tenant_id,
                        &id,
                    ],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("seal epoch: {}", e)))?;
            if sealed > 0 {
                return Ok(());
            }
            let exists = client
                .query_opt(
                    "SELECT 1 FROM epochs WHERE tenant_id = $1 AND id = $2",
                    &[&self.tenant_id, &id],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("seal lookup: {}", e)))?;
            match exists {
                Some(_) => Err(StorageError::EpochAlreadySealed { id: id.to_string() }),
                None => Err(StorageError::Backend(format!("epoch not found: {}", id))),
            }
        })
    }

    fn assign_epoch_namespace(&self, id: &str, namespace: &str) -> Result<bool, StorageError> {
        self.block_on(async {
            let client = self.pool.get().await.map_err(conn_error)?;
            // `namespace IS NULL` makes this assign-only — see the trait docs.
            let changed = client
                .execute(
                    "UPDATE epochs SET namespace = $1
                     WHERE tenant_id = $2 AND id = $3 AND namespace IS NULL",
                    &[&namespace, &self.tenant_id, &id],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("assign epoch namespace: {}", e)))?;
            if changed > 0 {
                return Ok(true);
            }
            let row = client
                .query_opt(
                    "SELECT 1 FROM epochs WHERE tenant_id = $1 AND id = $2",
                    &[&self.tenant_id, &id],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("assign epoch lookup: {}", e)))?;
            match row {
                Some(_) => Ok(false),
                None => Err(StorageError::Backend(format!("epoch not found: {}", id))),
            }
        })
    }

    fn list_epochs(&self) -> Result<Vec<Epoch>, StorageError> {
        self.block_on(async {
            let client = self.pool.get().await.map_err(conn_error)?;

            let rows = client
                .query(
                    &format!(
                        "SELECT {EPOCH_COLUMNS} FROM epochs
                         WHERE tenant_id = $1
                         ORDER BY created_at DESC"
                    ),
                    &[&self.tenant_id],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("list epochs: {}", e)))?;
            rows.iter().map(epoch_from_row).collect()
        })
    }

    fn get_epoch(&self, id: &str) -> Result<Option<Epoch>, StorageError> {
        self.block_on(async {
            let client = self.pool.get().await.map_err(conn_error)?;

            let row = client
                .query_opt(
                    &format!("SELECT {EPOCH_COLUMNS} FROM epochs WHERE tenant_id = $1 AND id = $2"),
                    &[&self.tenant_id, &id],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("get epoch: {}", e)))?;

            match row {
                None => Ok(None),
                Some(row) => {
                    let mut epoch = epoch_from_row(&row)?;
                    // Rehydrate the commit list in commit order. `commits` has
                    // no `created_at`; ordering by it failed every call. The id
                    // breaks ties between commits made in the same instant.
                    let rows = client
                        .query(
                            "SELECT id FROM commits
                              WHERE tenant_id = $1 AND epoch_id = $2
                              ORDER BY timestamp, id",
                            &[&self.tenant_id, &epoch.id],
                        )
                        .await
                        .map_err(|e| StorageError::Backend(format!("epoch commits: {}", e)))?;
                    epoch.commits = rows
                        .iter()
                        .filter_map(|r| {
                            let b: Vec<u8> = r.get("id");
                            if b.len() == 32 {
                                let mut arr = [0u8; 32];
                                arr.copy_from_slice(&b);
                                Some(ObjectId::from_bytes(arr))
                            } else {
                                None
                            }
                        })
                        .collect();
                    Ok(Some(epoch))
                }
            }
        })
    }

    fn archive_epoch(&self, id: &str) -> Result<(), StorageError> {
        self.block_on(async {
            let client = self.pool.get().await.map_err(conn_error)?;
            let n = client
                .execute(
                    "UPDATE epochs SET status = 'Archived'
                      WHERE tenant_id = $1 AND id = $2 AND status = 'Sealed'",
                    &[&self.tenant_id, &id],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("archive epoch: {}", e)))?;
            if n == 0 {
                return Err(StorageError::Backend(format!(
                    "epoch '{}' not found or not sealed",
                    id
                )));
            }
            Ok(())
        })
    }

    fn set_commit_epoch(&self, commit_id: &ObjectId, epoch_id: &str) -> Result<(), StorageError> {
        self.block_on(async {
            let mut client = self.pool.get().await.map_err(conn_error)?;
            // One transaction with the epoch row locked: a seal waits for it,
            // so a commit can't join an epoch that is being sealed, and the
            // status check can't go stale before the write.
            let tx = client
                .transaction()
                .await
                .map_err(|e| StorageError::Backend(format!("begin assoc: {e}")))?;
            let row = tx
                .query_opt(
                    "SELECT status FROM epochs WHERE tenant_id = $1 AND id = $2 FOR UPDATE",
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
            let joined = tx
                .execute(
                    "UPDATE commits SET epoch_id = $1
                     WHERE tenant_id = $2 AND id = $3 AND epoch_id IS DISTINCT FROM $1",
                    &[&epoch_id, &self.tenant_id, &commit_id.as_bytes().as_slice()],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("set commit epoch: {}", e)))?;
            // A running count, bumped only when the commit actually joins (the
            // old full COUNT(*) per commit grew with the epoch). Sealing writes
            // the exact count.
            if joined > 0 {
                tx.execute(
                    "UPDATE epochs SET commit_count = commit_count + 1
                     WHERE tenant_id = $1 AND id = $2",
                    &[&self.tenant_id, &epoch_id],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("update commit_count: {}", e)))?;
            }
            tx.commit()
                .await
                .map_err(|e| StorageError::Backend(format!("commit assoc: {e}")))?;
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
            "scope_tenant": session.scope_tenant,
        })
        .to_string();

        self.block_on(async {
            let client = self.pool.get().await.map_err(conn_error)?;

            client
                .execute(
                    "INSERT INTO sessions
                     (tenant_id, id, agent_id, parent_id, scope_path, scope_branch,
                      scope_namespace, status, created_at, ended_at, metadata, commit_count)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11,
                             COALESCE(
                                 (SELECT commit_count FROM sessions
                                  WHERE tenant_id = $1 AND id = $2),
                                 0))
                     ON CONFLICT (tenant_id, id) DO UPDATE
                       SET agent_id = EXCLUDED.agent_id,
                           parent_id = EXCLUDED.parent_id,
                           scope_path = EXCLUDED.scope_path,
                           scope_branch = EXCLUDED.scope_branch,
                           scope_namespace = EXCLUDED.scope_namespace,
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
                        &session
                            .scope_namespace
                            .as_ref()
                            .map(|n| n.as_str().to_string()),
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
            let client = self.pool.get().await.map_err(conn_error)?;
            // One conditional UPDATE, so two callers ending one session can't
            // both pass a status check; exactly one ends it.
            let ended = client
                .execute(
                    "UPDATE sessions SET status = $1, ended_at = $2
                     WHERE tenant_id = $3 AND id = $4 AND status = 'Active'",
                    &[
                        &status.as_str(),
                        &ended_at.to_rfc3339(),
                        &self.tenant_id,
                        &id,
                    ],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("end session: {}", e)))?;
            if ended > 0 {
                return Ok(());
            }
            let exists = client
                .query_opt(
                    "SELECT 1 FROM sessions WHERE tenant_id = $1 AND id = $2",
                    &[&self.tenant_id, &id],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("end session lookup: {}", e)))?;
            match exists {
                Some(_) => Err(StorageError::SessionEnded { id: id.to_string() }),
                None => Err(StorageError::Backend(format!("session not found: {}", id))),
            }
        })
    }

    fn list_sessions(&self, agent_filter: Option<&str>) -> Result<Vec<Session>, StorageError> {
        self.block_on(async {
            let client = self.pool.get().await.map_err(conn_error)?;

            let rows = client
                .query(
                    "SELECT id, agent_id, parent_id, scope_path, scope_branch, scope_namespace,
                            status, created_at, ended_at, metadata
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
                    row.get("scope_namespace"),
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
            let client = self.pool.get().await.map_err(conn_error)?;

            let row = client
                .query_opt(
                    "SELECT id, agent_id, parent_id, scope_path, scope_branch, scope_namespace,
                            status, created_at, ended_at, metadata
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
                    row.get("scope_namespace"),
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
            let mut client = self.pool.get().await.map_err(conn_error)?;
            // As set_commit_epoch: one transaction, the session row locked so
            // ending the session waits and the status check can't go stale.
            let tx = client
                .transaction()
                .await
                .map_err(|e| StorageError::Backend(format!("begin assoc: {e}")))?;
            let row = tx
                .query_opt(
                    "SELECT status FROM sessions WHERE tenant_id = $1 AND id = $2 FOR UPDATE",
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
            let joined = tx
                .execute(
                    "UPDATE commits SET session_id = $1
                     WHERE tenant_id = $2 AND id = $3 AND session_id IS DISTINCT FROM $1",
                    &[
                        &session_id,
                        &self.tenant_id,
                        &commit_id.as_bytes().as_slice(),
                    ],
                )
                .await
                .map_err(|e| StorageError::Backend(format!("set commit session: {}", e)))?;
            if joined > 0 {
                tx.execute(
                    "UPDATE sessions SET commit_count = commit_count + 1
                     WHERE tenant_id = $1 AND id = $2",
                    &[&self.tenant_id, &session_id],
                )
                .await
                .map_err(|e| {
                    StorageError::Backend(format!("update session commit_count: {}", e))
                })?;
            }
            tx.commit()
                .await
                .map_err(|e| StorageError::Backend(format!("commit assoc: {e}")))?;
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
            let client = self.pool.get().await.map_err(conn_error)?;
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
            let client = self.pool.get().await.map_err(conn_error)?;
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
            let client = self.pool.get().await.map_err(conn_error)?;
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
            let client = self.pool.get().await.map_err(conn_error)?;
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
            let client = self.pool.get().await.map_err(conn_error)?;
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
            let client = self.pool.get().await.map_err(conn_error)?;
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

fn pg_reminder_status_to_str(s: agentstategraph_reminders::types::ReminderStatus) -> &'static str {
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

/// Write every field of `reminder` over its row, on a plain connection or
/// inside a transaction. Returns the rows changed: 0 means no such reminder.
async fn write_reminder<C: deadpool_postgres::GenericClient + Sync>(
    client: &C,
    tenant_id: &str,
    reminder: &agentstategraph_reminders::Reminder,
) -> Result<u64, StorageError> {
    let json = |what: &str, v: Result<String, serde_json::Error>| {
        v.map_err(|e| StorageError::Serialization(format!("{what}: {e}")))
    };
    let commands_json = json("commands", serde_json::to_string(&reminder.commands))?;
    let refs_json = json("refs", serde_json::to_string(&reminder.refs))?;
    let schedule_json = reminder
        .schedule
        .as_ref()
        .map(|s| json("schedule", serde_json::to_string(s)))
        .transpose()?;
    let executions_json = json("executions", serde_json::to_string(&reminder.executions))?;
    let tags_json = json("tags", serde_json::to_string(&reminder.tags))?;
    let priority = pg_priority_to_i32(reminder.priority);
    let status = pg_reminder_status_to_str(reminder.status).to_string();
    let due_at = reminder.due_at.to_rfc3339();
    let created_at = reminder.created_at.to_rfc3339();
    let snoozed_until = reminder.snoozed_until.map(|t| t.to_rfc3339());
    client
        .execute(
            "UPDATE reminders SET
                title = $3, instructions = $4, commands = $5, refs = $6,
                priority = $7, due_at = $8, schedule = $9, autonomous = $10,
                created_by = $11, created_at = $12, status = $13,
                snoozed_until = $14, executions = $15, tags = $16
             WHERE tenant_id = $1 AND id = $2",
            &[
                &tenant_id,
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
        .map_err(|e| StorageError::Backend(format!("update reminder: {e}")))
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
        let commands_json = serde_json::to_string(&reminder.commands).map_err(|e| {
            agentstategraph_reminders::ReminderError::Store(format!("commands: {e}"))
        })?;
        let refs_json = serde_json::to_string(&reminder.refs)
            .map_err(|e| agentstategraph_reminders::ReminderError::Store(format!("refs: {e}")))?;
        let schedule_json = reminder
            .schedule
            .as_ref()
            .map(|s| serde_json::to_string(s))
            .transpose()
            .map_err(|e| {
                agentstategraph_reminders::ReminderError::Store(format!("schedule: {e}"))
            })?;
        let executions_json = serde_json::to_string(&reminder.executions).map_err(|e| {
            agentstategraph_reminders::ReminderError::Store(format!("executions: {e}"))
        })?;
        let tags_json = serde_json::to_string(&reminder.tags)
            .map_err(|e| agentstategraph_reminders::ReminderError::Store(format!("tags: {e}")))?;
        let priority = pg_priority_to_i32(reminder.priority);
        let status = pg_reminder_status_to_str(reminder.status).to_string();
        let due_at = reminder.due_at.to_rfc3339();
        let created_at = reminder.created_at.to_rfc3339();
        let snoozed_until = reminder.snoozed_until.map(|t| t.to_rfc3339());

        self.block_on(async {
            let client = self.pool.get().await.map_err(conn_error)?;
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
            let client = self.pool.get().await.map_err(conn_error)?;
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
        let written = self
            .block_on(async {
                let client = self.pool.get().await.map_err(conn_error)?;
                write_reminder(&client, &self.tenant_id, reminder).await
            })
            .map_err(|e| agentstategraph_reminders::ReminderError::Store(e.to_string()))?;
        if written == 0 {
            return Err(agentstategraph_reminders::ReminderError::NotFound(
                reminder.id.clone(),
            ));
        }
        Ok(())
    }

    /// Compare and write in one transaction with the row locked, so no other
    /// writer can change the reminder in between. The trait's default compared
    /// and wrote separately: six racing updates from one snapshot all "won".
    fn update_if_unchanged(
        &self,
        expected: &agentstategraph_reminders::Reminder,
        new: &agentstategraph_reminders::Reminder,
    ) -> Result<bool, agentstategraph_reminders::ReminderError> {
        enum Outcome {
            Missing,
            Changed,
            Written,
        }
        let outcome = self
            .block_on(async {
                let mut client = self.pool.get().await.map_err(conn_error)?;
                let tx = client
                    .transaction()
                    .await
                    .map_err(|e| StorageError::Backend(format!("begin reminder update: {e}")))?;
                let row = tx
                    .query_opt(
                        "SELECT * FROM reminders WHERE tenant_id = $1 AND id = $2 FOR UPDATE",
                        &[&self.tenant_id, &expected.id],
                    )
                    .await
                    .map_err(|e| StorageError::Backend(format!("get reminder: {e}")))?;
                let Some(row) = row else {
                    return Ok(Outcome::Missing);
                };
                if !agentstategraph_reminders::same_reminder(&pg_row_to_reminder(&row)?, expected) {
                    return Ok(Outcome::Changed); // dropping the transaction rolls it back
                }
                write_reminder(&tx, &self.tenant_id, new).await?;
                tx.commit()
                    .await
                    .map_err(|e| StorageError::Backend(format!("commit reminder update: {e}")))?;
                Ok(Outcome::Written)
            })
            .map_err(|e| agentstategraph_reminders::ReminderError::Store(e.to_string()))?;
        match outcome {
            Outcome::Missing => Err(agentstategraph_reminders::ReminderError::NotFound(
                expected.id.clone(),
            )),
            Outcome::Changed => Ok(false),
            Outcome::Written => Ok(true),
        }
    }

    fn delete(&self, id: &str) -> Result<bool, agentstategraph_reminders::ReminderError> {
        self.block_on(async {
            let client = self.pool.get().await.map_err(conn_error)?;
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
            let client = self.pool.get().await.map_err(conn_error)?;

            let mut sql = "SELECT * FROM reminders WHERE tenant_id = $1".to_string();
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

            let mut params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = vec![&self.tenant_id];
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
            .create_pool(Some(PoolRuntime::Tokio1), tokio_postgres::NoTls)
            .expect("build pool");
        assert_eq!(pool.status().max_size, 7);
    }

    fn tls_of(url: &str) -> ConnectionTls {
        ConnectionTls::parse(url, None).unwrap()
    }

    #[test]
    fn no_sslmode_stays_plaintext() {
        let t = tls_of("postgres://u@h/db");
        assert_eq!(t.url, "postgres://u@h/db?sslmode=disable");
        assert!(!t.requires_tls);
        assert_eq!(t.root_cert, None);
        assert_eq!(
            tls_of("postgres://u@h/db?application_name=asg").url,
            "postgres://u@h/db?application_name=asg&sslmode=disable"
        );
    }

    #[test]
    fn verify_modes_become_require_and_sslrootcert_is_taken_out() {
        let t = tls_of(
            "postgres://u@h/db?sslmode=verify-full&sslrootcert=/etc/ssl/My%20CA.pem&application_name=asg",
        );
        assert_eq!(
            t.url,
            "postgres://u@h/db?sslmode=require&application_name=asg"
        );
        assert!(t.requires_tls);
        assert_eq!(
            t.root_cert,
            Some(std::path::PathBuf::from("/etc/ssl/My CA.pem"))
        );
        assert!(tls_of("postgresql://h/db?sslmode=verify-ca").requires_tls);
        assert!(tls_of("postgres://h/db?sslmode=require").requires_tls);
        let prefer = tls_of("postgres://h/db?sslmode=prefer");
        assert!(!prefer.requires_tls);
        assert_eq!(prefer.url, "postgres://h/db?sslmode=prefer");
    }

    #[test]
    fn key_value_strings_are_handled_too() {
        let t = tls_of("host=db.example.com user=asg sslmode=verify-full sslrootcert=/ca.pem");
        assert_eq!(t.url, "host=db.example.com user=asg sslmode=require");
        assert!(t.requires_tls);
        assert_eq!(t.root_cert, Some(std::path::PathBuf::from("/ca.pem")));
    }

    #[test]
    fn pgsslrootcert_is_the_fallback() {
        let env = Some("/env/ca.pem".to_string());
        let t = ConnectionTls::parse("postgres://h/db?sslmode=require", env.clone()).unwrap();
        assert_eq!(t.root_cert, Some(std::path::PathBuf::from("/env/ca.pem")));
        let t = ConnectionTls::parse("postgres://h/db?sslrootcert=/url/ca.pem", env).unwrap();
        assert_eq!(
            t.root_cert,
            Some(std::path::PathBuf::from("/url/ca.pem")),
            "the connection string wins"
        );
    }

    #[cfg(not(feature = "postgres-tls"))]
    #[test]
    fn asking_for_tls_without_the_feature_is_an_error() {
        let err = PostgresStorage::with_pool("postgres://h/db?sslmode=require", "t", 1)
            .err()
            .expect("refused");
        assert!(format!("{err:?}").contains("postgres-tls"), "{err:?}");
        assert!(
            PostgresStorage::with_pool("postgres://h/db?sslmode=prefer", "t", 1).is_ok(),
            "prefer may fall back to plaintext"
        );
    }

    #[cfg(feature = "postgres-tls")]
    #[test]
    fn an_unreadable_sslrootcert_is_a_clear_error() {
        let err = PostgresStorage::with_pool(
            "postgres://h/db?sslmode=require&sslrootcert=/nonexistent/ca.pem",
            "t",
            1,
        )
        .err()
        .expect("refused");
        assert!(format!("{err:?}").contains("sslrootcert"), "{err:?}");
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
        assert_eq!(
            pg_reminder_status_to_str(ReminderStatus::AwaitingPermission),
            "awaiting_permission"
        );
        assert_eq!(
            pg_reminder_status_to_str(ReminderStatus::InProgress),
            "in_progress"
        );
    }
}
