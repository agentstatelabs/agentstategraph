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
use std::sync::Arc;

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
