//! agentstategraph-storage — Pluggable storage backends for AgentStateGraph.
//!
//! Provides the storage trait definitions and built-in backends:
//! - `MemoryStorage` — fast, ephemeral, for testing and speculation
//! - `SqliteStorage` — durable, single-file, the default for production use
//!
//! Custom backends can be added by implementing the `Storage` trait
//! (which is a blanket impl over `ObjectStore + CommitStore + RefStore`).

#[cfg(feature = "indexeddb")]
pub mod indexeddb;
pub mod memory;
#[cfg(feature = "sqlite")]
pub mod sqlite;
pub mod traits;

// Re-export primary types
#[cfg(feature = "indexeddb")]
pub use indexeddb::IndexedDbStorage;
pub use memory::MemoryStorage;
#[cfg(feature = "sqlite")]
pub use sqlite::SqliteStorage;
pub use traits::{CommitStore, ObjectStore, RefStore, Storage, StorageError};
