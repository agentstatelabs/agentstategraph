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
pub mod store_gauges;

/// The trace the current work belongs to, from oxtel's current span context,
/// as a commit links to it (DESIGN.md Plan H t-009). The server passes it to
/// [`agentstategraph::link_commits_to_traces`] unless `ASG_TRACE_LINKS` is
/// off.
pub fn current_trace() -> Option<agentstategraph::CommitTrace> {
    oxtel::sdk::trace::current_span_context()
        .filter(|context| context.is_valid())
        .map(|context| agentstategraph::CommitTrace {
            trace_id: context.trace_id.to_string(),
            span_id: context.span_id.to_string(),
        })
}

/// Whether commits are linked to their traces: yes, unless `ASG_TRACE_LINKS`
/// is `0`, `off`, `false` or `no`.
pub fn trace_links_from_env() -> bool {
    let value = std::env::var("ASG_TRACE_LINKS").unwrap_or_default();
    !matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "0" | "off" | "false" | "no"
    )
}
