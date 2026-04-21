//! Ed25519 signing and verification for AgentStateGraph policies.
//!
//! This crate is an **opt-in sibling** of `agentstategraph-policy`.
//! Consumers that never sign policies (the common case in 0.7.x)
//! don't pay the `ed25519-dalek` / `sha2` compile-time cost. §2c of
//! the 0.7.5 plan wires the types exposed here into the MCP server.
//!
//! # Layering
//!
//! - [`canonicalize`] produces the deterministic UTF-8 byte
//!   representation of a `Policy` that is fed to the signer and
//!   verifier. The `signature` field is stripped before serialization
//!   so sign/verify round-trip cleanly once §2b adds the field.
//! - [`PolicySigner`] / [`Ed25519Signer`] produces
//!   `(signer_key_id, 64-byte signature)` over those bytes.
//! - [`PolicyVerifier`] / [`Ed25519Verifier`] checks such a pair
//!   against a [`KeyRegistry`] lookup.
//!
//! # Canonical-JSON spec choice
//!
//! This crate uses "sorted keys + no whitespace + UTF-8" rather than
//! full RFC 8785 JCS. Policy values never contain `f64` fields today,
//! so the extra IEEE-754 canonicalization JCS requires is pure
//! overhead. See [`canonical`] for the full contract.

mod canonical;
mod error;
mod keys;
mod signer;
mod verifier;

pub use canonical::{canonicalize, canonicalize_value};
pub use error::{SignError, VerifyError};
pub use keys::{InMemoryKeyRegistry, KeyRegistry};
pub use signer::{Ed25519Signer, PolicySigner};
pub use verifier::{Ed25519Verifier, PolicyVerifier};
