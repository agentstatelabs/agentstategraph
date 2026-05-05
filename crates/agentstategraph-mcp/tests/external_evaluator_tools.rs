//! §4c — integration tests for external-evaluator wiring on the MCP
//! server plus the `agentstategraph_policy_set_external_evaluator`
//! FFI stub.
//!
//! Covered:
//!
//! 1. A server built without any external runner falls through to the
//!    local evaluator and still returns a sensible decision.
//! 2. A mock `ExternalEvaluator` registered via `with_external_evaluator`
//!    wins over the local path for a policy that carries an
//!    `external_evaluator: Some(Wasm{..})` field.
//! 3. The FFI stub `agentstategraph_policy_set_external_evaluator`
//!    returns the documented error envelope.
//! 4. (feature-gated) The `with_wasm_evaluator` convenience builder
//!    constructs and registers the stock WASM runner without panicking.

use std::sync::Arc;

use agentstategraph::Repository;
use agentstategraph_mcp::server::AgentStateGraphServer;
use agentstategraph_policy::{
    AuthorizedAction, Decision, EvaluatorSource, ExternalError, ExternalEvaluator,
    ExternalEvaluatorRef, Policy, Selector, Severity, Situation,
};
use agentstategraph_storage::SqliteStorage;
use chrono::Utc;

fn fresh_repo() -> Arc<Repository> {
    let repo = Arc::new(Repository::new(Box::new(
        SqliteStorage::in_memory().expect("in-memory sqlite"),
    )));
    repo.init().expect("init");
    repo
}

fn base_policy(path: &str) -> Policy {
    Policy {
        path: path.to_string(),
        version: 0,
        situation: "test".to_string(),
        situation_selector: Selector::Always,
        allow: Vec::new(),
        deny: Vec::new(),
        require_approval: Vec::new(),
        procedure: None,
        triggers: Vec::new(),
        required_fields: Vec::new(),
        severity: Severity::Low,
        proposed_by: String::new(),
        proposed_at: Utc::now(),
        ratified_by: None,
        ratified_at: None,
        ratification_reasoning: None,
        active_from: Utc::now(),
        expires_at: None,
        supersedes: None,
        signature: None,
        tenant_id: None,
        external_evaluator: None,
    }
}

fn seed_allow_policy(server: &AgentStateGraphServer, path: &str) {
    let mut p = base_policy(path);
    p.allow.push(AuthorizedAction {
        action: "do_thing".into(),
        condition: None,
        preconditions: Vec::new(),
    });
    server.policies().propose("main", p).expect("propose");
    server
        .policies()
        .ratify("main", path, "alice", "ok")
        .expect("ratify");
}

/// Closure-backed mock runner whose `kind()` is `"wasm"` — matches the
/// `ExternalEvaluatorRef::Wasm` variant that tests seed on policies.
struct MockWasmRunner {
    decision: Decision,
}

impl ExternalEvaluator for MockWasmRunner {
    fn kind(&self) -> &'static str {
        "wasm"
    }
    fn evaluate(
        &self,
        _source: &EvaluatorSource,
        _situation: &Situation,
        _action: &str,
        _agent_id: &str,
    ) -> Result<Decision, ExternalError> {
        Ok(self.decision.clone())
    }
}

// ---------------------------------------------------------------------------
// 1. No external runner → local evaluator answers.
// ---------------------------------------------------------------------------

#[test]
fn server_without_external_evaluators_defaults_to_local() {
    let repo = fresh_repo();
    let server = AgentStateGraphServer::new(repo);
    seed_allow_policy(&server, "ext/local-default");

    let decision = server
        .policies()
        .evaluate("main", &Situation::new(), "do_thing", "agent-1")
        .expect("evaluate");

    match decision {
        Decision::Allow { matched_policy, .. } => {
            assert!(
                matched_policy.starts_with("ext/local-default"),
                "expected local policy to match, got {}",
                matched_policy
            );
        }
        other => panic!("expected Allow from local evaluator, got {:?}", other),
    }
}

// ---------------------------------------------------------------------------
// 2. Mock external runner wins when a policy names its kind.
// ---------------------------------------------------------------------------

#[test]
fn server_with_mock_external_evaluator_dispatches_to_it() {
    let repo = fresh_repo();
    // Mock returns a Deny so the verdict is unambiguously distinguishable
    // from the local evaluator's NoPolicyMatch.
    let mock = Arc::new(MockWasmRunner {
        decision: Decision::Deny {
            matched_policy: "<mock>".into(),
            reason: "mock said no".into(),
        },
    });
    let server = AgentStateGraphServer::new(repo).with_external_evaluator(mock);

    // Seed a ratified policy with external_evaluator = Wasm; the local
    // evaluator has nothing to say about it because local dispatch
    // skips externals-pinned policies.
    let mut p = base_policy("ext/routed-to-mock");
    p.external_evaluator = Some(ExternalEvaluatorRef::Wasm {
        source: EvaluatorSource::Inline {
            body: "ignored by the mock".into(),
        },
    });
    server.policies().propose("main", p).expect("propose");
    server
        .policies()
        .ratify("main", "ext/routed-to-mock", "alice", "ok")
        .expect("ratify");

    let decision = server
        .policies()
        .evaluate("main", &Situation::new(), "do_thing", "agent-1")
        .expect("evaluate");

    match decision {
        Decision::Deny { reason, .. } => {
            assert_eq!(reason, "mock said no", "mock runner's decision must win");
        }
        other => panic!("expected Deny from mock runner, got {:?}", other),
    }
}

// ---------------------------------------------------------------------------
// 3. FFI stub returns the documented error envelope.
// ---------------------------------------------------------------------------

#[test]
fn ffi_set_external_evaluator_returns_stub_error() {
    use std::ffi::{CStr, CString};

    // Build a PolicyStore via the FFI surface so the extern has a
    // non-null handle to validate. The Rust-side extern "C" fns are
    // safe to call from Rust; the FFI surface is declared without
    // `unsafe fn` precisely so bindings can import it directly.
    let repo = agentstategraph_ffi::agentstategraph_new_memory();
    assert!(!repo.is_null(), "new_memory returned null");
    let prefix = CString::new("/policies").unwrap();
    let agent = CString::new("mcp-agent").unwrap();
    let store = agentstategraph_ffi::agentstategraph_policy_store_new(
        repo,
        prefix.as_ptr(),
        agent.as_ptr(),
    );
    assert!(!store.is_null(), "policy_store_new returned null");

    let config = CString::new(r#"{"kind":"wasm","options":{}}"#).unwrap();
    let raw =
        agentstategraph_ffi::agentstategraph_policy_set_external_evaluator(store, config.as_ptr());
    assert!(!raw.is_null(), "stub FFI returned null");
    let out = unsafe { CStr::from_ptr(raw) }
        .to_string_lossy()
        .into_owned();

    assert!(
        out.contains("external evaluators not configured via FFI"),
        "unexpected FFI stub envelope: {}",
        out
    );

    // Clean up.
    agentstategraph_ffi::agentstategraph_free_string(raw);
    agentstategraph_ffi::agentstategraph_policy_store_free(store);
    agentstategraph_ffi::agentstategraph_free(repo);
}

// ---------------------------------------------------------------------------
// 4. Feature-gated: the WASM convenience builder compiles + runs.
// ---------------------------------------------------------------------------

#[cfg(feature = "policy-wasm")]
#[test]
fn with_wasm_evaluator_builds_when_feature_enabled() {
    let repo = fresh_repo();
    // We don't actually evaluate a real WASM module here — we just want
    // to prove the convenience builder wires up without panicking and
    // leaves the server in a state where `policies()` still answers.
    let server = AgentStateGraphServer::new(repo).with_wasm_evaluator();
    seed_allow_policy(&server, "ext/wasm-builder-smoke");

    let decision = server
        .policies()
        .evaluate("main", &Situation::new(), "do_thing", "agent-1")
        .expect("evaluate");
    assert!(matches!(decision, Decision::Allow { .. }));
}
