//! Subprocess-level tests for [`RegoEvaluator`].
//!
//! Tests that require a real OPA binary are gated by
//! `opa_available()` and silently skip when OPA is not on `$PATH`.
//! This matches the approach used by CI runners that don't ship OPA.

use std::path::PathBuf;

use agentstategraph_policy::external::{ExternalError, ExternalEvaluator};
use agentstategraph_policy::selector::Situation;
use agentstategraph_policy::types::{Decision, EvaluatorSource};
use agentstategraph_policy_rego::RegoEvaluator;

fn opa_available() -> bool {
    std::process::Command::new("opa")
        .arg("version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

macro_rules! requires_opa {
    () => {
        if !opa_available() {
            eprintln!("skipping: opa not on $PATH");
            return;
        }
    };
}

const ALLOW_REGO: &str = r#"
package policy

decision := {
  "kind": "allow",
  "matched_policy": "test/rego@1",
  "preconditions": []
}
"#;

const DENY_REGO: &str = r#"
package policy

decision := {
  "kind": "deny",
  "matched_policy": "test/rego-deny@1",
  "reason": "forbidden action"
}
"#;

#[test]
fn rego_evaluator_returns_allow_from_inline_source() {
    requires_opa!();
    let eval = RegoEvaluator::new();
    let d = eval
        .evaluate(
            &EvaluatorSource::Inline {
                body: ALLOW_REGO.into(),
            },
            &Situation::new().with("env", "prod"),
            "deploy",
            "agent-1",
        )
        .expect("evaluate should succeed");
    match d {
        Decision::Allow { matched_policy, .. } => {
            assert_eq!(matched_policy, "test/rego@1");
        }
        other => panic!("expected Allow, got {other:?}"),
    }
}

#[test]
fn rego_evaluator_rejects_missing_binary() {
    // No requires_opa — this test specifically covers the "binary
    // missing" path and should always run.
    let eval = RegoEvaluator::with_opa_path("nonexistent-opa-xyz-asg");
    let err = eval
        .evaluate(
            &EvaluatorSource::Inline {
                body: ALLOW_REGO.into(),
            },
            &Situation::new(),
            "deploy",
            "agent-1",
        )
        .expect_err("missing binary should fail");
    match err {
        ExternalError::Execution(msg) => {
            assert!(msg.contains("nonexistent-opa-xyz-asg"), "msg: {msg}");
        }
        other => panic!("expected Execution, got {other:?}"),
    }
}

#[test]
fn rego_evaluator_rejects_commit_ref_source() {
    // This path short-circuits before ever spawning opa, so no gate.
    let eval = RegoEvaluator::new();
    let err = eval
        .evaluate(
            &EvaluatorSource::CommitRef {
                path: "state/policies/my-rego".into(),
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
fn rego_evaluator_returns_deny_from_file_source() {
    requires_opa!();
    let tmp = std::env::temp_dir().join("asg-rego-deny-fixture.rego");
    std::fs::write(&tmp, DENY_REGO).unwrap();

    let eval = RegoEvaluator::new();
    let d = eval
        .evaluate(
            &EvaluatorSource::FilePath {
                path: PathBuf::from(&tmp),
            },
            &Situation::new(),
            "delete",
            "agent-1",
        )
        .expect("evaluate should succeed");
    match d {
        Decision::Deny {
            matched_policy,
            reason,
        } => {
            assert_eq!(matched_policy, "test/rego-deny@1");
            assert_eq!(reason, "forbidden action");
        }
        other => panic!("expected Deny, got {other:?}"),
    }

    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn rego_evaluator_kind_tag() {
    assert_eq!(RegoEvaluator::new().kind(), "rego");
}
