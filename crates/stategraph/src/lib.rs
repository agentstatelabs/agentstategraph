//! stategraph — AI-native versioned state store for intent-based systems.
//!
//! This is the high-level API crate that ties together stategraph-core
//! (types, algorithms) and stategraph-storage (pluggable backends)
//! into a usable Repository interface.
//!
//! # Quick Start
//!
//! ```rust,ignore
//! use stategraph::Repository;
//! use stategraph_storage::MemoryStorage;
//! use stategraph_core::{IntentCategory, Intent};
//!
//! let storage = MemoryStorage::new();
//! let mut repo = Repository::new(Box::new(storage));
//! ```

pub mod repo;
pub mod speculation;
pub mod tree;

// Re-export core and storage for convenience
pub use stategraph_core as core;
pub use stategraph_storage as storage;

// Re-export primary types
pub use repo::{CommitOptions, RepoError, Repository};
pub use speculation::{SpecComparison, SpecHandle, SpeculationManager};
