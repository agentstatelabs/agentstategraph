//! Integration tests for MCP server-layer logic that is specific to the
//! server (not already tested through the Repository API).
//!
//! Covers:
//!   - `render_decision_with_fail_safe` output for all Decision variants
//!   - `infer_tokens_from_diff` for every token rule
//!   - `LARGE_CHANGE_THRESHOLD` boundary
//!   - Token deduplication

use agentstategraph_core::{DiffOp, DiffValue};
use agentstategraph_mcp::server::{
    LARGE_CHANGE_THRESHOLD, infer_tokens_from_diff, render_decision_with_fail_safe,
};
use agentstategraph_policy::types::Decision;

// ---------------------------------------------------------------------------
// render_decision_with_fail_safe
// ---------------------------------------------------------------------------

#[test]
fn render_no_policy_match_with_allow_fail_safe() {
    let out = render_decision_with_fail_safe(&Decision::NoPolicyMatch, "allow");
    let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
    assert_eq!(
        parsed["translated"]["kind"].as_str(),
        Some("allow"),
        "fail-safe:allow should translate NoPolicyMatch → allow"
    );
    assert!(parsed["fail_safe"].as_str() == Some("allow"));
}

#[test]
fn render_no_policy_match_with_deny_fail_safe() {
    let out = render_decision_with_fail_safe(&Decision::NoPolicyMatch, "deny");
    let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
    assert_eq!(
        parsed["translated"]["kind"].as_str(),
        Some("deny"),
        "fail-safe:deny should translate NoPolicyMatch → deny"
    );
    assert!(parsed["fail_safe"].as_str() == Some("deny"));
    assert!(
        parsed["translated"]["reason"]
            .as_str()
            .map(|r| r.contains("fail-safe"))
            .unwrap_or(false),
        "deny output should mention fail-safe"
    );
}

#[test]
fn render_no_policy_match_preserves_original_in_output() {
    let out = render_decision_with_fail_safe(&Decision::NoPolicyMatch, "allow");
    let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
    assert_eq!(parsed["original"]["kind"].as_str(), Some("no_policy_match"));
}

#[test]
fn render_allow_decision_passes_through() {
    let decision = Decision::Allow {
        matched_policy: "allow-all".into(),
        preconditions: vec![],
    };
    let out = render_decision_with_fail_safe(&decision, "deny");
    let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
    // Non-NoPolicyMatch decisions pass through directly — should not contain fail_safe wrapper
    assert!(
        parsed.get("fail_safe").is_none(),
        "non-NoPolicyMatch decisions should not be wrapped"
    );
}

#[test]
fn render_deny_decision_passes_through() {
    let decision = Decision::Deny {
        matched_policy: "security-gate".into(),
        reason: "unauthorized path".into(),
    };
    let out = render_decision_with_fail_safe(&decision, "allow");
    let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
    assert!(parsed.get("fail_safe").is_none());
}

// ---------------------------------------------------------------------------
// infer_tokens_from_diff
// ---------------------------------------------------------------------------

fn set_value(path: &str) -> DiffOp {
    DiffOp::SetValue {
        path: path.to_string(),
        old: DiffValue::Null,
        new: DiffValue::String("x".into()),
    }
}

fn remove_key(path: &str) -> DiffOp {
    DiffOp::RemoveKey {
        path: path.to_string(),
        key: "key".to_string(),
        old_value: DiffValue::Null,
    }
}

fn change_type(path: &str) -> DiffOp {
    DiffOp::ChangeType {
        path: path.to_string(),
        old_type: "map".to_string(),
        new_type: "list".to_string(),
    }
}

#[test]
fn empty_diff_produces_no_tokens() {
    let tokens = infer_tokens_from_diff(&[]);
    assert!(tokens.is_empty());
}

#[test]
fn remove_key_produces_destructive_token() {
    let tokens = infer_tokens_from_diff(&[remove_key("/nodes/pico1")]);
    assert!(tokens.contains(&"destructive".to_string()));
}

#[test]
fn remove_element_produces_destructive_token() {
    let diff = vec![DiffOp::RemoveElement {
        path: "/list".to_string(),
        index: 0,
        old_value: DiffValue::Null,
    }];
    let tokens = infer_tokens_from_diff(&diff);
    assert!(tokens.contains(&"destructive".to_string()));
}

#[test]
fn remove_from_set_produces_destructive_token() {
    let diff = vec![DiffOp::RemoveFromSet {
        path: "/set".to_string(),
        old_value: DiffValue::Null,
    }];
    let tokens = infer_tokens_from_diff(&diff);
    assert!(tokens.contains(&"destructive".to_string()));
}

#[test]
fn change_type_produces_ref_rewrite_token() {
    let tokens = infer_tokens_from_diff(&[change_type("/nodes/pico1")]);
    assert!(tokens.contains(&"ref-rewrite".to_string()));
}

#[test]
fn schema_version_path_produces_schema_change_token() {
    let tokens = infer_tokens_from_diff(&[set_value("/_meta/schema_version")]);
    assert!(tokens.contains(&"schema-change".to_string()));
}

#[test]
fn migrations_path_produces_migration_token() {
    let tokens = infer_tokens_from_diff(&[set_value("/_meta/migrations/v0_4_0")]);
    assert!(tokens.contains(&"migration".to_string()));
}

#[test]
fn index_path_produces_reindex_token() {
    let tokens = infer_tokens_from_diff(&[set_value("/index/node-1")]);
    assert!(tokens.contains(&"reindex".to_string()));
}

#[test]
fn reindexed_true_marker_produces_reindex_token() {
    let diff = vec![DiffOp::AddKey {
        path: "/nodes/pico1/reindexed".to_string(),
        key: "reindexed".to_string(),
        value: DiffValue::Bool(true),
    }];
    let tokens = infer_tokens_from_diff(&diff);
    assert!(tokens.contains(&"reindex".to_string()));
}

#[test]
fn reindexed_false_does_not_produce_reindex_token() {
    let diff = vec![DiffOp::AddKey {
        path: "/nodes/pico1/reindexed".to_string(),
        key: "reindexed".to_string(),
        value: DiffValue::Bool(false),
    }];
    let tokens = infer_tokens_from_diff(&diff);
    assert!(!tokens.contains(&"reindex".to_string()));
}

#[test]
fn large_change_threshold_boundary() {
    // Exactly at the threshold — NOT large
    let at_threshold: Vec<DiffOp> = (0..LARGE_CHANGE_THRESHOLD)
        .map(|i| set_value(&format!("/p{}", i)))
        .collect();
    assert_eq!(at_threshold.len(), LARGE_CHANGE_THRESHOLD);
    let tokens = infer_tokens_from_diff(&at_threshold);
    assert!(
        !tokens.contains(&"large".to_string()),
        "exactly {} ops should NOT produce 'large' token",
        LARGE_CHANGE_THRESHOLD
    );

    // One over the threshold — large
    let over_threshold: Vec<DiffOp> = (0..=LARGE_CHANGE_THRESHOLD)
        .map(|i| set_value(&format!("/p{}", i)))
        .collect();
    let tokens = infer_tokens_from_diff(&over_threshold);
    assert!(
        tokens.contains(&"large".to_string()),
        "{} ops should produce 'large' token",
        LARGE_CHANGE_THRESHOLD + 1
    );
}

#[test]
fn tokens_are_deduplicated() {
    // Multiple remove-key ops should still yield only one "destructive" token
    let diff = vec![
        remove_key("/a"),
        remove_key("/b"),
        remove_key("/c"),
    ];
    let tokens = infer_tokens_from_diff(&diff);
    let destructive_count = tokens.iter().filter(|t| t.as_str() == "destructive").count();
    assert_eq!(destructive_count, 1, "'destructive' must appear at most once");
}

#[test]
fn multiple_token_types_in_one_diff() {
    // A diff that triggers destructive + schema-change + migration
    let diff = vec![
        remove_key("/nodes/old"),
        set_value("/_meta/schema_version"),
        set_value("/_meta/migrations/v0_5_0"),
    ];
    let tokens = infer_tokens_from_diff(&diff);
    assert!(tokens.contains(&"destructive".to_string()));
    assert!(tokens.contains(&"schema-change".to_string()));
    assert!(tokens.contains(&"migration".to_string()));
    // No large, no reindex, no ref-rewrite
    assert!(!tokens.contains(&"large".to_string()));
    assert!(!tokens.contains(&"reindex".to_string()));
    assert!(!tokens.contains(&"ref-rewrite".to_string()));
}

#[test]
fn regular_set_value_produces_no_tokens() {
    let diff = vec![
        set_value("/nodes/pico1/status"),
        set_value("/config/network/subnet"),
    ];
    let tokens = infer_tokens_from_diff(&diff);
    assert!(
        tokens.is_empty(),
        "plain SetValue ops with ordinary paths should produce no tokens: {:?}",
        tokens
    );
}
