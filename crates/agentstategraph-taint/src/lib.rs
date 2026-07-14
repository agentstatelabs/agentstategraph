//! Taint / quarantine / watch substrate for AgentStateGraph
//! (see spec/TAINT_SPEC.md).
//!
//! Dynamic runtime markers that bridge passive observation into
//! enforcement. Every taint is a first-class commit (auditable,
//! blameable, full intent metadata) and the pre-commit hook
//! consults [`evaluate_access`] to gate writes.
//!
//! This crate ships the types + the pure check algorithm. Storage
//! lives in `agentstategraph-storage`; commit-pipeline wiring lives
//! in `agentstategraph` (the main repository crate).

#![deny(rust_2018_idioms)]

mod check;
mod error;
mod types;

pub use check::{REVIEW_CONFIDENCE_THRESHOLD, ancestor_candidates, evaluate_access};
pub use error::TaintError;
pub use types::{
    QuarantineParams, Taint, TaintCheck, TaintEffect, TaintKind, TaintMetadata, TaintParams,
    TaintSeverity, UnquarantineParams, UntaintParams, UnwatchParams, WatchDirection, WatchParams,
};

/// Crate version exposed for downstream `MCP` / FFI introspection.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
