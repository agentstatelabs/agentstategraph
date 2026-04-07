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
//! use agentstategraph_storage::MemoryStorage;
//! use agentstategraph_core::{IntentCategory, Intent};
//!
//! let storage = MemoryStorage::new();
//! let mut repo = Repository::new(Box::new(storage));
//! ```

pub mod repo;
pub mod session;
pub mod speculation;
pub mod tree;
pub mod watch;

// Re-export core and storage for convenience
pub use agentstategraph_core as core;
pub use agentstategraph_storage as storage;

// Re-export primary types
pub use repo::{CommitOptions, RepoError, Repository};
pub use session::{Session, SessionError, SessionManager};
pub use speculation::{SpecComparison, SpecHandle, SpeculationManager};
pub use watch::{PathPattern, SubscriptionId, WatchEvent, WatchManager};
