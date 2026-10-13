//! External-evaluator dispatcher (0.7.5 §4a).
//!
//! This module provides the trait + registry that let a `PolicyStore`
//! route evaluation to an external rule engine (Rego, Cedar, WASM,
//! ...) when a policy carries `external_evaluator: Some(_)`. The
//! concrete runner crates live as optional siblings
//! (`agentstategraph-policy-wasm` / `-rego` / `-cedar` — §4b); the
//! main policy crate ships only the trait, the registry, and the
//! dispatch wiring so callers that never use external evaluators pay
//! no extra dep cost.
//!
//! Dispatch semantics (see `PolicyStore::evaluate_scoped`):
//!
//! - Policies without an `external_evaluator` go through the local
//!   evaluator as before.
//! - Policies whose `external_evaluator.kind` matches a registered
//!   runner are evaluated by that runner; the runner's `Decision`
//!   participates in the normal `deny > require_approval > allow`
//!   precedence alongside local decisions.
//! - Policies whose `external_evaluator.kind` is *not* registered are
//!   skipped — treated as not-matching. This keeps a missing runner
//!   from crashing evaluation and matches the POLICY_V1.md §11
//!   soft-model principle.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::process::{Child, Output};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::selector::Situation;
use crate::types::{Decision, EvaluatorSource};

/// Failure modes for an external evaluator invocation.
#[derive(Debug, thiserror::Error)]
pub enum ExternalError {
    /// The policy's `external_evaluator.kind` has no runner in the
    /// registry. Dispatch treats this as a skip (policy not-matching);
    /// surfaced as an error only when the registry is asked directly.
    #[error("evaluator kind '{0}' not registered")]
    NotRegistered(String),

    /// The runner could not resolve the `EvaluatorSource` into the
    /// rule-engine-specific input (missing file, state-path lookup
    /// failed, bad Inline body, ...).
    #[error("source resolution failed: {0}")]
    SourceResolution(String),

    /// The rule engine ran but returned an error (syntax error in the
    /// policy body, runtime trap, subprocess non-zero exit, ...).
    #[error("evaluator failed: {0}")]
    Execution(String),

    /// Escape hatch for runner-specific errors.
    #[error(transparent)]
    Other(#[from] Box<dyn std::error::Error + Send + Sync>),
}

impl ExternalError {
    /// A stable, low-cardinality code for this error, which telemetry
    /// records as `error.type`. A runner that timed out returns
    /// [`TimedOut`] in [`ExternalError::Other`], and its code is
    /// `evaluator_timeout`.
    pub fn code(&self) -> &'static str {
        match self {
            ExternalError::NotRegistered(_) => "evaluator_not_registered",
            ExternalError::SourceResolution(_) => "evaluator_source_unresolved",
            ExternalError::Execution(_) => "evaluator_failed",
            ExternalError::Other(e) if e.is::<TimedOut>() => "evaluator_timeout",
            ExternalError::Other(_) => "evaluator_error",
        }
    }
}

/// How long a runner's process may run before [`wait_with_timeout`] kills
/// it, unless the runner sets its own limit.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// A runner's process ran past its time limit and was killed. Runners return
/// it as [`ExternalError::Other`].
#[derive(Debug, thiserror::Error)]
#[error("evaluator timed out after {0:?}")]
pub struct TimedOut(pub Duration);

/// Wait for a runner's process, writing `input` to its stdin, and collect its
/// output, as [`Child::wait_with_output`] does, but kill it if it runs longer
/// than `timeout`: a hung evaluator would otherwise hold the write waiting on
/// its decision. Spawn it with stdout and stderr piped, and stdin too when
/// there's input.
pub fn wait_with_timeout(
    mut child: Child,
    input: Option<Vec<u8>>,
    timeout: Duration,
) -> Result<Output, ExternalError> {
    let started = Instant::now();
    // Write and read on threads, so a process that fills one pipe while
    // we're blocked on another can't deadlock against us.
    let stdin = child.stdin.take();
    let writer = input.and_then(|input| {
        let mut stdin = stdin?;
        Some(thread::spawn(move || stdin.write_all(&input)))
    });
    let stdout = child.stdout.take().map(read_on_thread);
    let stderr = child.stderr.take().map(read_on_thread);

    let mut pause = Duration::from_millis(1);
    let status = loop {
        let exited = child
            .try_wait()
            .map_err(|e| ExternalError::Execution(format!("wait: {e}")))?;
        if let Some(status) = exited {
            break status;
        }
        let elapsed = started.elapsed();
        if elapsed >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ExternalError::Other(Box::new(TimedOut(timeout))));
        }
        thread::sleep(pause.min(timeout - elapsed));
        pause = (pause * 2).min(Duration::from_millis(50));
    };

    if let Some(Ok(Err(e))) = writer.map(JoinHandle::join) {
        // A process that exits without reading all its input closes the
        // pipe; its status and output say why.
        if e.kind() != std::io::ErrorKind::BrokenPipe {
            return Err(ExternalError::Execution(format!("write stdin: {e}")));
        }
    }
    let collect = |pipe: Option<JoinHandle<Vec<u8>>>| {
        pipe.and_then(|reader| reader.join().ok())
            .unwrap_or_default()
    };
    Ok(Output {
        status,
        stdout: collect(stdout),
        stderr: collect(stderr),
    })
}

fn read_on_thread(mut pipe: impl Read + Send + 'static) -> JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut out = Vec::new();
        let _ = pipe.read_to_end(&mut out);
        out
    })
}

/// Implementors evaluate a policy against a situation using some
/// external rule language.
///
/// Implementations MUST be `Send + Sync` because the registry is
/// shared across threads inside `PolicyStore` via `Arc`.
pub trait ExternalEvaluator: Send + Sync {
    /// Canonical kind tag — matches the `kind` field on
    /// `ExternalEvaluatorRef` variants (`"rego"`, `"cedar"`, `"wasm"`,
    /// or any custom value a third-party runner picks).
    fn kind(&self) -> &'static str;

    /// Evaluate against the policy's `external_evaluator.source`. The
    /// runner is responsible for resolving the source (`Inline` /
    /// `FilePath` / `CommitRef`) into whatever form its rule engine
    /// consumes.
    fn evaluate(
        &self,
        source: &EvaluatorSource,
        situation: &Situation,
        action: &str,
        agent_id: &str,
    ) -> Result<Decision, ExternalError>;
}

/// Registry of installed external evaluators.
///
/// A [`PolicyStore`](crate::PolicyStore) holds one (optional) via
/// `with_external_evaluators`. When a policy's `external_evaluator`
/// field is `Some(_)` and the registry contains a runner whose
/// `kind()` matches, dispatch routes to that runner. Otherwise the
/// store falls back to local evaluation for non-external policies,
/// and skips policies whose external kind is unregistered.
#[derive(Default)]
pub struct ExternalEvaluatorRegistry {
    impls: HashMap<&'static str, Arc<dyn ExternalEvaluator>>,
}

impl ExternalEvaluatorRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            impls: HashMap::new(),
        }
    }

    /// Register a runner. If a runner with the same `kind()` is
    /// already registered, the new one replaces it.
    pub fn register(&mut self, eval: Arc<dyn ExternalEvaluator>) {
        self.impls.insert(eval.kind(), eval);
    }

    /// Look up a runner by kind tag.
    pub fn get(&self, kind: &str) -> Option<&Arc<dyn ExternalEvaluator>> {
        self.impls.get(kind)
    }

    /// List every registered kind tag. Order is unspecified.
    pub fn kinds(&self) -> Vec<&'static str> {
        self.impls.keys().copied().collect()
    }

    /// Number of registered runners.
    pub fn len(&self) -> usize {
        self.impls.len()
    }

    /// `true` iff no runners are registered.
    pub fn is_empty(&self) -> bool {
        self.impls.is_empty()
    }
}

impl std::fmt::Debug for ExternalEvaluatorRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExternalEvaluatorRegistry")
            .field("kinds", &self.kinds())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockRunner;
    impl ExternalEvaluator for MockRunner {
        fn kind(&self) -> &'static str {
            "mock"
        }
        fn evaluate(
            &self,
            _source: &EvaluatorSource,
            _situation: &Situation,
            _action: &str,
            _agent_id: &str,
        ) -> Result<Decision, ExternalError> {
            Ok(Decision::NoPolicyMatch)
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_process_past_its_timeout_is_killed() {
        use std::process::{Command, Stdio};
        let child = Command::new("sleep")
            .arg("5")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let started = Instant::now();
        let error = wait_with_timeout(child, None, Duration::from_millis(100)).unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(4));
        assert_eq!(error.code(), "evaluator_timeout");
    }

    #[cfg(unix)]
    #[test]
    fn a_process_gets_its_input_and_returns_its_output() {
        use std::process::{Command, Stdio};
        let child = Command::new("cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // More than a pipe holds, so writing and reading must overlap.
        let input = vec![b'x'; 1 << 20];
        let output = wait_with_timeout(child, Some(input.clone()), DEFAULT_TIMEOUT).unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, input);
    }

    #[test]
    fn registry_register_and_get() {
        let mut reg = ExternalEvaluatorRegistry::new();
        assert!(reg.is_empty());
        reg.register(Arc::new(MockRunner));
        assert_eq!(reg.len(), 1);
        assert!(reg.get("mock").is_some());
        assert!(reg.get("missing").is_none());
        assert_eq!(reg.kinds(), vec!["mock"]);
    }
}
