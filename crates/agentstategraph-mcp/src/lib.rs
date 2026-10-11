//! Library surface of `agentstategraph-mcp`.
//!
//! The binary (`src/main.rs`) re-uses these modules. Exposing them as a
//! library lets integration tests under `tests/` exercise the HTTP
//! router, auth middleware, and rate-limit wiring directly against the
//! same code paths the server runs in production.

pub mod auth;
pub mod http;
pub mod policy_signing;
pub mod server;
