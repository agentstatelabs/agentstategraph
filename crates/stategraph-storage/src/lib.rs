//! stategraph-storage — Pluggable storage backends for StateGraph.
//!
//! Provides the storage trait definitions and built-in backends:
//! - `MemoryStorage` — fast, ephemeral, for testing and speculation
//! - `SqliteStorage` — durable, single-file, the default for production use
//!
//! Custom backends can be added by implementing the `Storage` trait
//! (which is a blanket impl over `ObjectStore + CommitStore + RefStore`).

pub mod memory;
#[cfg(feature = "sqlite")]
pub mod sqlite;
#[cfg(feature = "indexeddb")]
pub mod indexeddb;
pub mod traits;

// Re-export primary types
pub use memory::MemoryStorage;
#[cfg(feature = "sqlite")]
pub use sqlite::SqliteStorage;
#[cfg(feature = "indexeddb")]
pub use indexeddb::IndexedDbStorage;
pub use traits::{CommitStore, ObjectStore, RefStore, Storage, StorageError};
