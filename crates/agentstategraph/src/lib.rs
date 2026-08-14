//! stategraph — AI-native versioned state store for intent-based systems.
//!
//! This is the high-level API crate that ties together agentstategraph-core
//! (types, algorithms) and agentstategraph-storage (pluggable backends)
//! into a usable Repository interface.
//!
//! # Quick Start
//!
//! ```rust,ignore
//! use agentstategraph::Repository;
//! use agentstategraph_storage::SqliteStorage;
//! use agentstategraph_core::{IntentCategory, Intent};
//!
//! let storage = SqliteStorage::in_memory().expect("in-memory sqlite");
//! let mut repo = Repository::new(Box::new(storage));
//! ```

pub mod repo;
pub mod session;
pub mod speculation;
pub mod taint;
pub mod tree;
pub mod watch;

// Re-export core and storage for convenience
pub use agentstategraph_core as core;
pub use agentstategraph_storage as storage;

// Re-export primary types
pub use repo::{
    CommitOptions, HistoryExtractReport, META_PATH_PREFIX, META_SCHEMA_VERSION_PATH, RepoError,
    Repository, RetentionPolicy, SCHEMA_VERSION,
};
pub use session::{CreateSessionParams, Session, SessionError, SessionManager};
pub use speculation::{SpecComparison, SpecHandle, SpeculationManager};
pub use watch::{PathPattern, SubscriptionId, WatchEvent, WatchManager};
