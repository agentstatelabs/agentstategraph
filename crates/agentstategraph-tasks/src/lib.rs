//! Shared task-store primitives for AgentStateGraph consumers.
//!
//! This crate establishes the pattern for how AgentStateGraph handles
//! opinionated-but-shared primitives: they live in sibling crates under
//! `crates/agentstategraph-<name>`, built on top of the primitive
//! Repository API. Consumers that don't need tasks don't depend on this
//! crate; the core stays minimal.
//!
//! # Example
//!
//! ```rust,no_run
//! use agentstategraph::Repository;
//! use agentstategraph_storage::MemoryStorage;
//! use agentstategraph_tasks::{Priority, Proof, TaskStore};
//! use std::sync::Arc;
//!
//! let repo = Arc::new(Repository::new(Box::new(MemoryStorage::new())));
//! repo.init().unwrap();
//!
//! let store = TaskStore::new(repo, "/plans", "claude-code");
//!
//! store
//!     .create_plan("main", "website-v2", Some("Brand pivot".into()))
//!     .unwrap();
//! let task = store
//!     .add_task(
//!         "main",
//!         "website-v2",
//!         "Rewrite hero",
//!         Priority::High,
//!         None,
//!         vec![],
//!         None,
//!     )
//!     .unwrap();
//! store.start_task("main", "website-v2", &task.id).unwrap();
//! store
//!     .complete_task(
//!         "main",
//!         "website-v2",
//!         &task.id,
//!         Proof::commit("abc123"),
//!     )
//!     .unwrap();
//! ```

pub mod error;
pub mod paths;
pub mod state;
pub mod store;
pub mod types;
pub mod verifier;

pub use error::TaskStoreError;
pub use state::Transition;
pub use store::TaskStore;
pub use types::{
    OnCompleteHook, Plan, PlanStatus, Priority, Proof, ProofKind, Task, TaskId, TaskStatus,
};
pub use verifier::{NoopVerifier, Verifier, VerifyEntry, VerifyReport, VerifyResult};
