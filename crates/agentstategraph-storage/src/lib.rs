//! agentstategraph-storage — Pluggable storage backends for AgentStateGraph.
//!
//! Provides the storage trait definitions and built-in backends:
//! - `MemoryStorage` — fast, ephemeral, for testing and speculation
//! - `SqliteStorage` — durable, single-file, the default for production use
//! - `PostgresStorage` — multi-tenant, connection-pooled, for SaaS and enterprise
//!
//! Custom backends can be added by implementing the `Storage` trait
//! (which is a blanket impl over `ObjectStore + CommitStore + RefStore`).

#[cfg(feature = "indexeddb")]
pub mod indexeddb;
#[cfg(feature = "memory")]
pub mod memory;
#[cfg(feature = "postgres")]
pub mod postgres;
#[cfg(feature = "sqlite")]
pub mod sqlite;
pub mod traits;

// Re-export primary types
#[cfg(feature = "indexeddb")]
pub use indexeddb::IndexedDbStorage;
#[cfg(feature = "memory")]
pub use memory::MemoryStorage;
#[cfg(feature = "postgres")]
pub use postgres::PostgresStorage;
#[cfg(feature = "sqlite")]
pub use sqlite::SqliteStorage;
pub use traits::{
    CommitStore, EpochStore, GcReachability, GcSweep, HistoryMilestoneRow, HistoryRollupRow,
    ObjectStore, RefStore, SessionStore, Storage, StorageError, StoreShape, TableBytes, TaintStore,
    VacuumStats,
};
