//! Smoke tests for the [`CedarEvaluator`] stub.
//!
//! These tests verify the stub error fires for the currently-unsupported
//! flows without actually requiring the `cedar` binary to be installed
//! — the stub short-circuits before invoking any subprocess.

use agentstategraph_policy::external::{ExternalError, ExternalEvaluator};
use agentstategraph_policy::selector::Situation;
use agentstategraph_policy::types::EvaluatorSource;
use agentstategraph_policy_cedar::CedarEvaluator;

#[test]
fn cedar_evaluator_kind_tag() {
    assert_eq!(CedarEvaluator::new().kind(), "cedar");
}

#[test]
fn cedar_evaluator_stub_errors_on_inline_source() {
    let eval = CedarEvaluator::new();
    let err = eval
        .evaluate(
            &EvaluatorSource::Inline {
                body: "permit(principal, action, resource);".into(),
            },
            &Situation::new(),
            "deploy",
            "agent-1",
        )
        .expect_err("stub should error");
    match err {
        ExternalError::Execution(msg) => {
            assert!(
                msg.contains("stub") || msg.contains("Cedar"),
                "expected stub error, got: {msg}"
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
fn cedar_evaluator_with_cedar_path_builds() {
    // API compatibility — the builder should at least construct.
    let eval = CedarEvaluator::with_cedar_path("/usr/local/bin/cedar");
    assert_eq!(eval.kind(), "cedar");
}
