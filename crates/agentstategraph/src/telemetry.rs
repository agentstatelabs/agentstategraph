//! The repository's spans, events and metrics, as docs/OBSERVABILITY.md
//! lists them. They go through the `tracing` and `metrics` facades, which
//! cost almost nothing until a binary installs a subscriber and a recorder,
//! as the server does through oxtel. State paths and values are never
//! recorded, and neither are error messages, which can name paths.

use std::sync::{Once, OnceLock};
use std::time::Instant;

use agentstategraph_core::ObjectId;
use agentstategraph_storage::CommitTrace;
use agentstategraph_taint::TaintError;
use metrics::{Label, Unit};
use tracing::Span;

use crate::repo::RepoError;

/// The span of a write that makes a commit, named `$name`. `$ref_name` may be
/// `tracing::field::Empty`, for a write that learns its ref as it goes.
macro_rules! write_span {
    ($name:literal, $ref_name:expr, $options:expr) => {
        tracing::info_span!(
            $name,
            asg.ref.name = $ref_name,
            gen_ai.agent.id = $options.agent_id.as_str(),
            asg.intent.category = ?$options.intent.category,
            asg.speculation.id = tracing::field::Empty,
            asg.commit.id = tracing::field::Empty,
            asg.ref.cas_attempts = tracing::field::Empty,
            error.type = tracing::field::Empty,
            otel.status_code = tracing::field::Empty,
        )
    };
}
pub(crate) use write_span;

/// The span of an operation other than a write, at `$level`, with the
/// fields [`run`] records and any given.
macro_rules! op_span {
    ($level:ident, $name:literal $(, $($fields:tt)*)?) => {
        tracing::span!(
            tracing::Level::$level,
            $name,
            error.type = tracing::field::Empty,
            otel.status_code = tracing::field::Empty,
            $($($fields)*)?
        )
    };
}
pub(crate) use op_span;

/// Run a write in its span from [`write_span!`], recording the commit it
/// made or how it failed, and its time in `asg.commit.duration`.
pub(crate) fn write(
    operation: &'static str,
    span: Span,
    write: impl FnOnce() -> Result<ObjectId, RepoError>,
) -> Result<ObjectId, RepoError> {
    let _entered = span.enter();
    let started = now();
    let result = write();
    describe_metrics();
    let mut labels = vec![Label::new("asg.operation.name", operation)];
    match &result {
        Ok(id) => {
            span.record("asg.commit.id", id.to_string());
        }
        Err(e) => {
            failed(&span, e);
            labels.push(Label::new("error.type", e.code()));
        }
    }
    if let Some(started) = started {
        metrics::histogram!("asg.commit.duration", labels).record(started.elapsed().as_secs_f64());
    }
    result
}

/// The time, where there's a clock to read. In a browser there isn't one,
/// and `Instant::now` panics, so durations aren't measured there.
fn now() -> Option<Instant> {
    #[cfg(not(all(target_family = "wasm", target_os = "unknown")))]
    return Some(Instant::now());
    #[cfg(all(target_family = "wasm", target_os = "unknown"))]
    return None;
}

/// Run an operation in its span from [`op_span!`], recording how it failed.
pub(crate) fn run<T>(
    span: Span,
    op: impl FnOnce() -> Result<T, RepoError>,
) -> Result<T, RepoError> {
    let _entered = span.enter();
    let result = op();
    if let Err(e) = &result {
        failed(&span, e);
    }
    result
}

/// Describe the metrics to the recorder, before the first is recorded.
fn describe_metrics() {
    static DESCRIBED: Once = Once::new();
    DESCRIBED.call_once(|| {
        metrics::describe_histogram!(
            "asg.commit.duration",
            Unit::Seconds,
            "How long a write that makes a commit took, including any retries."
        );
        metrics::describe_counter!(
            "asg.ref.cas_retries",
            "How many times a write rebuilt its commit because another writer moved the ref."
        );
    });
}

/// Mark a span failed, with the error's code as `error.type`.
fn failed(span: &Span, error: &RepoError) {
    span.record("error.type", error.code());
    span.record("otel.status_code", "error");
}

/// Call at the top of each attempt of a compare-and-swap loop. From the
/// second attempt on, the ref moved under the write, which rebuilds its
/// commit on the new head: counted in `asg.ref.cas_retries` and recorded on
/// the write's span.
pub(crate) fn cas_attempt(operation: &'static str, attempt: usize) {
    if attempt == 0 {
        return;
    }
    describe_metrics();
    metrics::counter!("asg.ref.cas_retries", "asg.operation.name" => operation).increment(1);
    Span::current().record("asg.ref.cas_attempts", attempt + 1);
    tracing::debug!(
        asg.ref.cas_attempts = attempt + 1,
        "the ref moved; rebuilding the commit on its new head"
    );
}

/// A compare-and-swap loop gave up after `attempts`: other writers kept
/// moving the ref.
pub(crate) fn cas_gave_up(operation: &'static str, attempts: usize) {
    tracing::warn!(
        asg.operation.name = operation,
        asg.ref.cas_attempts = attempts,
        error.type = "write_conflict",
        "the ref kept moving under the write, which gave up"
    );
}

/// A taint refused a write: a blocking taint, a review taint the write's
/// confidence didn't meet, or a quarantine the agent isn't authorized for.
pub(crate) fn taint_refused(error: &TaintError, taint_id: Option<&str>) {
    let effect = match error {
        TaintError::Blocked { .. } => "block",
        TaintError::InsufficientConfidence { .. } => "review",
        TaintError::NotAuthorized { .. } => "quarantine",
        _ => return,
    };
    tracing::info!(
        asg.taint.id = taint_id,
        asg.taint.effect = effect,
        error.type = "taint_blocked",
        "a taint refused the write"
    );
}

/// Where a commit finds the trace it's made in. See [`link_commits_to_traces`].
type TraceSource = Box<dyn Fn() -> Option<CommitTrace> + Send + Sync>;

static TRACE_SOURCE: OnceLock<TraceSource> = OnceLock::new();

/// Link every commit this process makes to the trace it's made in
/// (DESIGN.md Plan H t-009). `source` returns the current trace, or `None`
/// outside one; the server passes oxtel's current span context. Each commit's
/// trace and span ids are kept beside it in the store, never in it, and
/// [`Repository::commit_trace`](crate::Repository::commit_trace) reads them
/// back. Without a source, nothing is recorded.
///
/// Set it once, at startup. Returns `false`, keeping the first source, if one
/// was already set.
pub fn link_commits_to_traces(
    source: impl Fn() -> Option<CommitTrace> + Send + Sync + 'static,
) -> bool {
    TRACE_SOURCE.set(Box::new(source)).is_ok()
}

/// The trace the current work belongs to, if a source was set and there is one.
pub(crate) fn current_trace() -> Option<CommitTrace> {
    TRACE_SOURCE.get().and_then(|source| source())
}
