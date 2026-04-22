//! Smoke tests for the [`CedarEvaluator`] subprocess runner.
//!
//! These cover the error paths that don't require a working `cedar`
//! binary on $PATH: commit_ref source rejection, missing-binary
//! handling, and the kind tag. The happy-path / real-subprocess
//! tests live in the crate's unit tests and are gated behind a
//! `requires_cedar!()` skip.

use agentstategraph_policy::external::{ExternalError, ExternalEvaluator};
use agentstategraph_policy::selector::Situation;
use agentstategraph_policy::types::EvaluatorSource;
use agentstategraph_policy_cedar::CedarEvaluator;

#[test]
fn cedar_evaluator_kind_tag() {
    assert_eq!(CedarEvaluator::new().kind(), "cedar");
}

#[test]
fn cedar_evaluator_missing_binary_reports_execution_error() {
    // When `cedar` isn't on PATH (default config on CI without the
    // Cedar CLI installed), evaluating any inline source should
    // surface a clear Execution error instead of a silent miscompile.
    let eval = CedarEvaluator::new_with_path("/definitely/not/a/real/cedar-xyzzy");
    let err = eval
        .evaluate(
            &EvaluatorSource::Inline {
                body: "permit(principal, action, resource);".into(),
            },
            &Situation::new(),
            "deploy",
            "agent-1",
        )
        .expect_err("missing binary should error");
    match err {
        ExternalError::Execution(msg) => {
            assert!(
                msg.contains("cedar binary not found") || msg.contains("No such file"),
                "expected missing-binary error, got: {msg}"
            );
        }
        other => panic!("expected Execution, got {other:?}"),
    }
}

#[test]
fn cedar_evaluator_rejects_commit_ref_source() {
    let eval = CedarEvaluator::new();
    let err = eval
        .evaluate(
            &EvaluatorSource::CommitRef {
                path: "state/policies/my-cedar".into(),
            },
            &Situation::new(),
            "noop",
            "agent-1",
        )
        .expect_err("commit_ref should be unsupported");
    match err {
        ExternalError::SourceResolution(msg) => assert!(msg.contains("commit_ref")),
        other => panic!("expected SourceResolution, got {other:?}"),
    }
}

#[test]
fn cedar_evaluator_new_with_path_builds() {
    // API compatibility — the builder should at least construct.
    let eval = CedarEvaluator::new_with_path("/usr/local/bin/cedar");
    assert_eq!(eval.kind(), "cedar");
}
