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

/// The server's trace filter (DESIGN.md Plan H t-011). Spans at `info`, but
/// not one per SQL statement: that cost a small read more than anything else
/// telemetry does. `ASG_SQL_SPANS=on` adds them back, and `RUST_LOG`, when
/// set, replaces the whole filter. `db.client.operation.duration` records
/// either way.
pub fn traces_filter() -> String {
    if let Some(directives) = std::env::var("RUST_LOG")
        .ok()
        .filter(|d| !d.trim().is_empty())
    {
        return directives;
    }
    let sql_spans = std::env::var("ASG_SQL_SPANS").unwrap_or_default();
    match sql_spans.trim().to_ascii_lowercase().as_str() {
        "1" | "on" | "true" | "yes" => "info".to_string(),
        _ => "info,oxtel_sql=warn".to_string(),
    }
}

/// Whether each commit is logged as an `asg.commit` event (DESIGN.md Plan H
/// t-010): only when `ASG_COMMIT_EVENTS` is `1`, `on`, `true` or `yes`.
pub fn commit_events_from_env() -> bool {
    let value = std::env::var("ASG_COMMIT_EVENTS").unwrap_or_default();
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "on" | "true" | "yes"
    )
}

/// The log filter directive that lets `asg.commit` events through whatever
/// level the rest of the log runs at.
pub const COMMIT_EVENTS_DIRECTIVE: &str = "agentstategraph::provenance=info";

/// Whether commits are linked to their traces: yes, unless `ASG_TRACE_LINKS`
/// is `0`, `off`, `false` or `no`.
pub fn trace_links_from_env() -> bool {
    let value = std::env::var("ASG_TRACE_LINKS").unwrap_or_default();
    !matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "0" | "off" | "false" | "no"
    )
}
