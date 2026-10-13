//! Integration tests for Phase 3 — 9 policy MCP tools + `commit_spec`
//! gate on change evaluation.
//!
//! These tests exercise the `PolicyStore` bound inside
//! `AgentStateGraphServer` and the token-inference helper that drives the
//! `commit_spec` gate. They do not boot the MCP stdio transport; the
//! tool handlers are thin wrappers around the store + helpers, so the
//! behaviour lives in what we exercise here.

use std::collections::HashMap;
use std::sync::Arc;

use agentstategraph::{CommitOptions, Repository};
use agentstategraph_core::{DiffOp, DiffValue, IntentCategory};
use agentstategraph_mcp::server::{
    AgentStateGraphServer, LARGE_CHANGE_THRESHOLD, infer_change_tokens, infer_tokens_from_diff,
    render_decision_with_fail_safe,
};
use agentstategraph_policy::{
    ApprovalRule, AuthorizedAction, ChangeProposal, Decision, FallbackAction, Policy, Selector,
    Severity, Situation,
};
use agentstategraph_storage::SqliteStorage;
use chrono::Utc;

fn fresh_repo() -> Arc<Repository> {
    let repo = Arc::new(Repository::new(Box::new(
        SqliteStorage::in_memory().expect("in-memory sqlite"),
    )));
    repo.init().expect("init repo");
    repo
}

fn fresh_server() -> AgentStateGraphServer {
    AgentStateGraphServer::new(fresh_repo())
}

fn policy(path: &str) -> Policy {
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

#[test]
fn test_policy_propose_then_ratify_flow() {
    let server = fresh_server();
    let store = server.policies();
    let handle = store
        .propose("main", policy("test/basic"))
        .expect("propose");
    assert_eq!(handle, "test/basic@1");

    let active = store.active("main", None).expect("active");
    assert!(
        active.is_empty(),
        "unratified proposal must not appear in active()"
    );

    store
        .ratify("main", "test/basic", "alice", "reviewed, looks good")
        .expect("ratify");

    let active = store.active("main", None).expect("active");
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].ratified_by.as_deref(), Some("alice"));
    assert_eq!(
        active[0].ratification_reasoning.as_deref(),
        Some("reviewed, looks good")
    );
}

#[test]
fn test_policy_supersede_chain_via_mcp() {
    let server = fresh_server();
    let store = server.policies();

    store
        .propose("main", policy("infra/net-tuning"))
        .expect("propose v1");
    store
        .ratify("main", "infra/net-tuning", "alice", "v1 ok")
        .expect("ratify v1");

    let mut v2 = policy("infra/net-tuning");
    v2.situation = "net-tuning v2".to_string();
    v2.ratified_by = Some("alice".to_string());
    v2.ratified_at = Some(Utc::now());
    let v2_handle = store
        .supersede("main", "infra/net-tuning", v2)
        .expect("supersede");
    assert_eq!(v2_handle, "infra/net-tuning@2");

    let history = store.history("main", "infra/net-tuning").expect("history");
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].version, 1);
    assert_eq!(history[1].version, 2);
    assert_eq!(history[1].supersedes.as_deref(), Some("infra/net-tuning@1"));
}

#[test]
fn test_policy_evaluate_returns_decision_json() {
    let server = fresh_server();
    let store = server.policies();

    let mut p = policy("infra/restart");
    p.allow.push(AuthorizedAction {
        action: "restart_pod".to_string(),
        condition: None,
        preconditions: vec!["investigate_logs".to_string()],
    });
    store.propose("main", p).expect("propose");
    store
        .ratify("main", "infra/restart", "alice", "ok")
        .expect("ratify");

    let dec = store
        .evaluate("main", &Situation::new(), "restart_pod", "agent-1")
        .expect("evaluate");
    match dec {
        Decision::Allow {
            matched_policy,
            preconditions,
        } => {
            assert_eq!(matched_policy, "infra/restart@1");
            assert_eq!(preconditions, vec!["investigate_logs".to_string()]);
        }
        other => panic!("expected Allow, got {:?}", other),
    }
}

#[test]
fn test_policy_evaluate_change_triggers_fallback() {
    let server = fresh_server();
    let store = server.policies();

    let mut p = policy("change-control/high-cost");
    p.triggers = vec!["reindex".to_string(), "downtime".to_string()];
    p.required_fields = vec!["rollback_plan".to_string()];
    p.require_approval.push(ApprovalRule {
        action: "*".to_string(),
        approvers: vec!["human".to_string()],
        timeout: None,
        fallback: FallbackAction::LowestRiskAlternative,
    });
    store.propose("main", p).expect("propose");
    store
        .ratify("main", "change-control/high-cost", "alice", "ok")
        .expect("ratify");

    // Missing required field → RequireApproval short-circuit.
    let proposal =
        ChangeProposal::new("promote", "agent-1", "risky", "spec-1").with_tokens(["reindex"]);
    let dec = store.evaluate_change("main", &proposal).expect("evaluate");
    match dec {
        Decision::RequireApproval { fallback, .. } => {
            assert!(matches!(fallback, FallbackAction::LowestRiskAlternative));
        }
        other => panic!("expected RequireApproval, got {:?}", other),
    }
}

#[test]
fn test_commit_spec_blocks_on_require_approval() {
    let repo = fresh_repo();
    let server = AgentStateGraphServer::new(repo.clone());
    let store = server.policies();

    let mut p = policy("change-control/destructive");
    p.triggers = vec!["destructive".to_string()];
    p.required_fields = vec!["rollback_plan".to_string()];
    p.require_approval.push(ApprovalRule {
        action: "*".to_string(),
        approvers: vec!["human".to_string()],
        timeout: None,
        fallback: FallbackAction::KeepCurrentState,
    });
    store.propose("main", p).expect("propose");
    store
        .ratify("main", "change-control/destructive", "alice", "ok")
        .expect("ratify");

    // Seed a value we will delete in the speculation.
    repo.set_json(
        "main",
        "/apps/victim",
        &serde_json::json!("bye"),
        CommitOptions::new("seed", IntentCategory::Checkpoint, "seed"),
    )
    .expect("seed");

    let handle = repo.speculate("main", Some("test".into())).expect("spec");
    repo.spec_delete(handle, "/apps/victim").expect("delete");

    let tokens = infer_change_tokens(&repo, handle).expect("infer");
    assert!(
        tokens.iter().any(|t| t == "destructive"),
        "expected destructive token, got {:?}",
        tokens
    );

    let proposal =
        ChangeProposal::new("promote_speculation", "mcp-agent", "drop /apps/victim", "1")
            .with_tokens(tokens);
    let dec = store
        .evaluate_change("main", &proposal)
        .expect("evaluate_change");
    assert!(
        matches!(dec, Decision::RequireApproval { .. }),
        "expected RequireApproval; got {:?}",
        dec
    );

    // The speculation must still be alive (policy gate short-circuited).
    // We simulate the gate not committing by calling discard and checking
    // it exists.
    repo.discard_speculation(handle).expect("discard live spec");
}

#[test]
fn test_commit_spec_allows_when_no_policy_matches() {
    let repo = fresh_repo();
    let server = AgentStateGraphServer::new(repo.clone());

    // No policies proposed at all.
    let handle = repo.speculate("main", None).expect("spec");
    repo.spec_set(handle, "/foo", &agentstategraph_core::Object::int(1))
        .expect("spec_set");

    let proposal = ChangeProposal::new("promote_speculation", "mcp-agent", "add /foo", "1");
    let dec = server
        .policies()
        .evaluate_change("main", &proposal)
        .expect("evaluate_change");
    assert!(matches!(dec, Decision::NoPolicyMatch));

    // Now actually commit — the gate's allow branch runs commit_speculation.
    let opts = CommitOptions::new("mcp-agent", IntentCategory::Explore, "commit");
    let commit_id = repo.commit_speculation(handle, opts).expect("commit");
    assert!(!format!("{}", commit_id).is_empty());
}

#[test]
fn test_commit_spec_allows_on_explicit_allow_policy() {
    let repo = fresh_repo();
    let server = AgentStateGraphServer::new(repo.clone());
    let store = server.policies();

    let mut p = policy("change-control/explicit-allow");
    p.triggers = vec!["safe-change".to_string()];
    p.allow.push(AuthorizedAction {
        action: "promote_speculation".to_string(),
        condition: None,
        preconditions: Vec::new(),
    });
    store.propose("main", p).expect("propose");
    store
        .ratify("main", "change-control/explicit-allow", "alice", "ok")
        .expect("ratify");

    let proposal = ChangeProposal::new("promote_speculation", "mcp-agent", "safe", "1")
        .with_tokens(["safe-change"]);
    let dec = store.evaluate_change("main", &proposal).expect("evaluate");
    assert!(matches!(dec, Decision::Allow { .. }));
}

#[test]
fn test_policy_check_tokens_lists_matching_policies() {
    let repo = fresh_repo();
    let server = AgentStateGraphServer::new(repo);
    let store = server.policies();

    let mut high_cost = policy("change-control/high-cost");
    high_cost.triggers = vec!["reindex".to_string(), "migration".to_string()];
    store.propose("main", high_cost).expect("propose");
    store
        .ratify("main", "change-control/high-cost", "alice", "ok")
        .expect("ratify");

    let mut unrelated = policy("change-control/net");
    unrelated.triggers = vec!["ref-rewrite".to_string()];
    store.propose("main", unrelated).expect("propose");
    store
        .ratify("main", "change-control/net", "alice", "ok")
        .expect("ratify");

    let all = store.active("main", None).expect("active");
    let token_set: std::collections::HashSet<&str> = ["reindex"].into_iter().collect();
    let hits: Vec<_> = all
        .iter()
        .filter(|p| p.triggers.iter().any(|t| token_set.contains(t.as_str())))
        .map(|p| p.path.clone())
        .collect();
    assert_eq!(hits, vec!["change-control/high-cost".to_string()]);
}

#[test]
fn test_fail_safe_translation_when_no_match() {
    // Default deny translation.
    let deny = render_decision_with_fail_safe(&Decision::NoPolicyMatch, "deny");
    let v: serde_json::Value = serde_json::from_str(&deny).expect("json");
    assert_eq!(v["original"]["kind"], "no_policy_match");
    assert_eq!(v["translated"]["kind"], "deny");
    assert_eq!(v["fail_safe"], "deny");

    // Explicit allow translation.
    let allow = render_decision_with_fail_safe(&Decision::NoPolicyMatch, "allow");
    let v: serde_json::Value = serde_json::from_str(&allow).expect("json");
    assert_eq!(v["original"]["kind"], "no_policy_match");
    assert_eq!(v["translated"]["kind"], "allow");
    assert_eq!(v["fail_safe"], "allow");

    // Non-NoPolicyMatch decisions pass through unchanged.
    let allow_real = Decision::Allow {
        matched_policy: "x@1".into(),
        preconditions: Vec::new(),
    };
    let rendered = render_decision_with_fail_safe(&allow_real, "deny");
    let v: serde_json::Value = serde_json::from_str(&rendered).expect("json");
    assert_eq!(v["kind"], "allow");
    assert_eq!(v["matched_policy"], "x@1");
}

#[test]
fn test_infer_change_tokens_destructive() {
    let diff = vec![DiffOp::RemoveKey {
        path: "/apps".into(),
        key: "victim".into(),
        old_value: DiffValue::String("bye".into()),
    }];
    let tokens = infer_tokens_from_diff(&diff);
    assert!(tokens.contains(&"destructive".to_string()));
    assert!(!tokens.contains(&"large".to_string()));
}

#[test]
fn test_infer_change_tokens_schema_change() {
    let diff = vec![DiffOp::SetValue {
        path: "/_meta/schema_version".into(),
        old: DiffValue::String("1".into()),
        new: DiffValue::String("2".into()),
    }];
    let tokens = infer_tokens_from_diff(&diff);
    assert!(tokens.contains(&"schema-change".to_string()));
}

#[test]
fn test_infer_change_tokens_migration_and_reindex() {
    let diff = vec![
        DiffOp::AddKey {
            path: "/_meta/migrations/0001".into(),
            key: "at".into(),
            value: DiffValue::String("now".into()),
        },
        DiffOp::SetValue {
            path: "/index/foo".into(),
            old: DiffValue::Null,
            new: DiffValue::Int(1),
        },
    ];
    let tokens = infer_tokens_from_diff(&diff);
    assert!(tokens.contains(&"migration".to_string()));
    assert!(tokens.contains(&"reindex".to_string()));
}

#[test]
fn test_infer_change_tokens_large_threshold() {
    // Exactly at threshold → NOT large.
    let at: Vec<DiffOp> = (0..LARGE_CHANGE_THRESHOLD)
        .map(|i| DiffOp::SetValue {
            path: format!("/p{}", i),
            old: DiffValue::Null,
            new: DiffValue::Int(i as i64),
        })
        .collect();
    assert!(!infer_tokens_from_diff(&at).contains(&"large".to_string()));

    // One over → large.
    let over: Vec<DiffOp> = (0..=LARGE_CHANGE_THRESHOLD)
        .map(|i| DiffOp::SetValue {
            path: format!("/p{}", i),
            old: DiffValue::Null,
            new: DiffValue::Int(i as i64),
        })
        .collect();
    assert!(infer_tokens_from_diff(&over).contains(&"large".to_string()));
}

#[test]
fn test_infer_change_tokens_ref_rewrite() {
    let diff = vec![DiffOp::ChangeType {
        path: "/nodes/0".into(),
        old_type: "map".into(),
        new_type: "list".into(),
    }];
    let tokens = infer_tokens_from_diff(&diff);
    assert!(tokens.contains(&"ref-rewrite".to_string()));
}

#[test]
fn test_infer_change_tokens_reindexed_marker() {
    let diff = vec![DiffOp::AddKey {
        path: "/search/catalog".into(),
        key: "reindexed".into(),
        value: DiffValue::Bool(true),
    }];
    let tokens = infer_tokens_from_diff(&diff);
    assert!(tokens.contains(&"reindex".to_string()));
}

#[test]
fn test_situation_map_roundtrips_into_store() {
    // Guard the happy path the `policy_evaluate` MCP tool relies on:
    // a plain string map deserializes into `Situation` via its
    // `From<HashMap<String,String>>` impl.
    let mut m: HashMap<String, String> = HashMap::new();
    m.insert("namespace".into(), "prod".into());
    let s: Situation = m.into();
    assert_eq!(s.get("namespace").map(|s| s.as_str()), Some("prod"));
}

#[test]
fn test_compare_tokens_match_commit_spec_inference() {
    // 0.6.75-beta.1 §6: the `compare` tool now emits the same token
    // vector per handle that `commit_spec` would compute internally.
    // Agents pre-flighting policy gates can read `tokens` from the
    // compare response before deciding whether to promote.
    let repo = fresh_repo();

    // Seed a destructive spec.
    repo.set_json(
        "main",
        "/apps/victim",
        &serde_json::json!("bye"),
        CommitOptions::new("seed", IntentCategory::Checkpoint, "seed"),
    )
    .expect("seed");
    let handle = repo
        .speculate("main", Some("destructive".into()))
        .expect("spec");
    repo.spec_delete(handle, "/apps/victim").expect("delete");

    // Direct parity: `infer_change_tokens` over the handle (same
    // function the tool uses) yields the same vector as
    // `infer_tokens_from_diff` applied to the compare entry's diff.
    let via_handle = infer_change_tokens(&repo, handle).expect("infer");
    let comparison = repo.compare_speculations(&[handle]).expect("compare");
    let via_compare = infer_tokens_from_diff(
        &comparison
            .entries
            .first()
            .map(|e| e.diff_from_base.clone())
            .unwrap_or_default(),
    );
    assert_eq!(
        via_handle, via_compare,
        "compare token inference must match commit_spec token inference"
    );
    assert!(via_compare.iter().any(|t| t == "destructive"));
}
