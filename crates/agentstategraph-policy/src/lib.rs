//! Policy primitive for AgentStateGraph.
//!
//! Fourth-primitive sibling crate, parallel to `agentstategraph-tasks`.
//! Implements the authorization + change-cost primitive described in
//! `/strategy/POLICY_V1.md` (v1.1, §§2-7 and §22). Ships the engine
//! surface only — MCP tools, CLI, and Lens UI live in their respective
//! consumer crates.
//!
//! # What this crate is
//!
//! - `Policy` schema (situation-matching rules + procedure + change-cost
//!   gating fields)
//! - `PolicyStore` — a handle bound to a `Repository` + prefix with
//!   `propose` / `ratify` / `supersede` / `evaluate` / `evaluate_change`
//! - `Selector` — a tagged-enum boolean expression over a `Situation`
//! - `Decision` — the evaluator's result, including `RequireApproval`
//!   with a `FallbackAction` ("what to do while it waits")
//!
//! # What this crate is NOT
//!
//! - Not an enforcement layer (soft model — see POLICY_V1.md §11)
//! - Not a Rego/Cedar clone (the selector is a minimal DSL; external
//!   policy engines are a future escape hatch)
//! - Not an MCP tool or CLI — those belong to consumers
//!
//! # Example
//!
//! ```rust,no_run
//! use agentstategraph::Repository;
//! use agentstategraph_storage::MemoryStorage;
//! use agentstategraph_policy::{PolicyStore, Situation, Selector};
//! use std::sync::Arc;
//!
//! let repo = Arc::new(Repository::new(Box::new(MemoryStorage::new())));
//! repo.init().unwrap();
//! let store = PolicyStore::new(repo, "/policies", "claude-code");
//! // propose, ratify, evaluate ...
//! let _ = store.evaluate("main", &Situation::new(), "restart_pod", "agent-1");
//! ```

pub mod error;
pub mod evaluator;
pub mod paths;
pub mod selector;
pub mod store;
pub mod types;
pub mod verifier;

pub use error::PolicyError;
pub use selector::{Selector, Situation};
pub use store::PolicyStore;
pub use types::{
    ApprovalRule, AuthorizedAction, ChangeProposal, Decision, FallbackAction, Policy,
    PolicySignature, ProcedureStep, Severity,
};
pub use verifier::{SignatureVerificationError, SignatureVerifier};
