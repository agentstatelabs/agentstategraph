//! Push and pull AgentStateGraph stores over HTTP (DESIGN.md Plan G t-006
//! and t-012).
//!
//! - [`server`] (feature `server`): the `/sync/v1` router a host mounts,
//!   with a [`server::StoreResolver`] that authenticates each request and
//!   picks its store. `agentstategraph-mcp --hub` mounts it for one
//!   account; a host with many accounts mounts it behind its own.
//! - [`client`] (feature `client`): [`client::HttpRemote`], the
//!   [`Remote`](agentstategraph::sync::Remote) a store pushes to and
//!   fetches from.
//! - [`wire`]: what goes between them.
//!
//! The receiving side's work, checks and push log are the library's own
//! ([`agentstategraph::Repository::sync_receive`] and the rest); this crate
//! only carries them.

pub mod wire;

#[cfg(feature = "server")]
pub mod server;

#[cfg(feature = "client")]
pub mod client;

#[cfg(feature = "client")]
pub use client::HttpRemote;
#[cfg(feature = "server")]
pub use server::{Rejection, Resolved, StoreResolver, router, router_with_limits};
