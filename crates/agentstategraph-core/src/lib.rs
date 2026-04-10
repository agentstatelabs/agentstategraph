//! agentstategraph-core — Core types and algorithms for AgentStateGraph.
//!
//! This crate contains the foundational types with zero I/O dependencies:
//! - Objects (Atom, Node) with content-addressing via BLAKE3
//! - Commits with intent, authority, and provenance metadata
//! - Path addressing for navigating state trees
//! - Error types
//!
//! All types are serializable and designed for both agent and human use.

pub mod commit;
pub mod diff;
pub mod epoch;
pub mod error;
pub mod intent;
pub mod lifecycle;
pub mod merge;
pub mod object;
pub mod path;
pub mod query;
pub mod schema;

// Re-export primary types for convenience
pub use commit::{Commit, CommitBuilder};
pub use diff::{DiffOp, DiffValue, ObjectResolver};
pub use epoch::{
    CrossRefType, CrossReference, Epoch, EpochEntry, EpochError, EpochStatus, Registry,
};
pub use error::CoreError;
pub use intent::{
    AgentId, AuthScope, Authority, DelegationLink, Deviation, DeviationImpact, FormatHint, Intent,
    IntentCategory, IntentId, IntentLifecycle, IntentStatus, NotificationPolicy, Outcome,
    Principal, Resolution, SessionId, ToolCall, Urgency,
};
pub use lifecycle::{LifecycleError, is_valid_transition, transition};
pub use merge::{Conflict, ConflictValue, MergeResult};
pub use object::{Atom, Node, Object, ObjectId};
pub use path::{PathComponent, PathError, StatePath};
pub use query::{BlameEntry, Query, QueryFilters, QueryOptions, QueryTarget, filter_commits};
pub use schema::{EnforcementMode, MergeHint, Schema, ValidationResult};
