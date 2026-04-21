//! End-to-end tests for [`WasmEvaluator`] against a tiny WAT fixture.
//!
//! The fixture implements the documented ABI with a trivial bump
//! allocator and a hardcoded `allow` decision. We exercise the full
//! host-side flow (alloc -> write -> evaluate -> read -> free -> parse).

use std::path::PathBuf;

use agentstategraph_policy::external::{ExternalError, ExternalEvaluator};
use agentstategraph_policy::selector::Situation;
use agentstategraph_policy::types::{Decision, EvaluatorSource};
use agentstategraph_policy_wasm::WasmEvaluator;

/// WAT module implementing the ABI:
/// - `memory` exported with 1 initial page (64 KiB).
/// - `asg_alloc` / `asg_free` as a bump allocator anchored at offset 1024
///   (global `$bump`). `asg_free` is a no-op; tests do not stress the
///   allocator's reclamation semantics.
/// - `asg_evaluate` ignores its input and writes a hardcoded Decision
///   JSON at offset 8192, then returns `(ptr << 32) | len`.
///
/// Hardcoded output:
/// `{"kind":"allow","matched_policy":"test/wasm@1","preconditions":[]}`
/// — 66 bytes.
const FIXTURE_WAT: &str = r#"
(module
  (memory (export "memory") 1)

  ;; Bump-allocator cursor, starts at 1024 so we don't collide with the
  ;; hardcoded output region at 8192.
  (global $bump (mut i32) (i32.const 1024))

  (func (export "asg_alloc") (param $size i32) (result i32)
    (local $p i32)
    (local.set $p (global.get $bump))
    (global.set $bump
      (i32.add (global.get $bump) (local.get $size)))
    (local.get $p))

  (func (export "asg_free") (param $ptr i32) (param $size i32)
    ;; no-op
    )

  ;; Output bytes: `{"kind":"allow","matched_policy":"test/wasm@1","preconditions":[]}`
  ;; Stored at a fixed offset (8192) via data segment below.
  (data (i32.const 8192)
    "{\22kind\22:\22allow\22,\22matched_policy\22:\22test/wasm@1\22,\22preconditions\22:[]}")

  (func (export "asg_evaluate")
    (param $in_ptr i32) (param $in_len i32)
    (result i64)
    ;; Output at 8192, length 66.
    (i64.or
      (i64.shl (i64.const 8192) (i64.const 32))
      (i64.const 66)))
)
"#;

/// A module that successfully compiles but does NOT export
/// `asg_evaluate` / `asg_alloc` / `asg_free`.
const MISSING_EXPORTS_WAT: &str = r#"
(module
  (memory (export "memory") 1)
  (func (export "not_the_right_name") (result i32) (i32.const 0))
)
"#;

fn wat_bytes(wat: &str) -> Vec<u8> {
    wat::parse_str(wat).expect("valid WAT")
}

#[test]
fn wasm_evaluator_returns_allow_from_fixture() {
    let bytes = wat_bytes(FIXTURE_WAT);
    // Write to a temp file so we can exercise the FilePath branch
    // (which is the realistic one — Inline stores text, not binary
    // WASM, in most uses).
    let tmp = std::env::temp_dir().join("asg-wasm-fixture-allow.wasm");
    std::fs::write(&tmp, &bytes).unwrap();

    let eval = WasmEvaluator::new();
    let decision = eval
        .evaluate(
            &EvaluatorSource::FilePath {
                path: PathBuf::from(&tmp),
            },
            &Situation::new().with("env", "prod"),
            "deploy",
            "agent-1",
        )
        .expect("evaluate should succeed");

    match decision {
        Decision::Allow {
            matched_policy,
            preconditions,
        } => {
            assert_eq!(matched_policy, "test/wasm@1");
            assert!(preconditions.is_empty());
        }
        other => panic!("expected Allow, got {other:?}"),
    }

    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn wasm_evaluator_rejects_non_wasm_bytes() {
    let eval = WasmEvaluator::new();
    let err = eval
        .evaluate(
            &EvaluatorSource::Inline {
                body: "definitely not wasm".into(),
            },
            &Situation::new(),
            "noop",
            "agent-1",
        )
        .expect_err("non-wasm bytes should fail");

    match err {
        ExternalError::Execution(msg) => assert!(
            msg.contains("compile"),
            "expected compile error, got: {msg}"
        ),
        other => panic!("expected Execution, got {other:?}"),
    }
}

#[test]
fn wasm_evaluator_rejects_commit_ref_source() {
    let eval = WasmEvaluator::new();
    let err = eval
        .evaluate(
            &EvaluatorSource::CommitRef {
                path: "state/policies/my-wasm".into(),
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
fn wasm_evaluator_rejects_missing_exports() {
    let bytes = wat_bytes(MISSING_EXPORTS_WAT);
    let tmp = std::env::temp_dir().join("asg-wasm-fixture-missing.wasm");
    std::fs::write(&tmp, &bytes).unwrap();

    let eval = WasmEvaluator::new();
    let err = eval
        .evaluate(
            &EvaluatorSource::FilePath {
                path: PathBuf::from(&tmp),
            },
            &Situation::new(),
            "noop",
            "agent-1",
        )
        .expect_err("module missing asg_evaluate should fail");

    match err {
        ExternalError::Execution(msg) => assert!(
            msg.contains("asg_alloc") || msg.contains("asg_free") || msg.contains("asg_evaluate"),
            "expected missing-export error, got: {msg}"
        ),
        other => panic!("expected Execution, got {other:?}"),
    }

    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn wasm_evaluator_kind_tag() {
    assert_eq!(WasmEvaluator::new().kind(), "wasm");
}
