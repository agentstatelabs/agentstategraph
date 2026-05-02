//! Integration tests for agentstategraph::Repository.
//!
//! Covers the entire public API surface of repo.rs using MemoryStorage so
//! every test is in-process with no I/O.  Tests are grouped by domain and
//! ordered from simplest to most complex within each group.

use agentstategraph::{CommitOptions, RepoError, Repository, META_PATH_PREFIX, SCHEMA_VERSION};
use agentstategraph_core::{IntentCategory, Object, QueryFilters};
use agentstategraph_storage::MemoryStorage;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn repo() -> Repository {
    let r = Repository::new(Box::new(MemoryStorage::new()));
    r.init().unwrap();
    r
}

fn opts(category: IntentCategory, desc: &str) -> CommitOptions {
    CommitOptions::new("agent/test", category, desc)
}

fn opts_agent(agent: &str, category: IntentCategory, desc: &str) -> CommitOptions {
    CommitOptions::new(agent, category, desc)
}

// ---------------------------------------------------------------------------
// Initialization
// ---------------------------------------------------------------------------

#[test]
fn init_creates_main() {
    let r = Repository::new(Box::new(MemoryStorage::new()));
    r.init().unwrap();
    // Should be able to get the schema version stamp
    let ver = r
        .get_json("main", "/_meta/schema_version")
        .expect("schema_version should exist after init");
    assert_eq!(ver.as_str().unwrap(), SCHEMA_VERSION);
}

#[test]
fn init_is_idempotent() {
    let r = Repository::new(Box::new(MemoryStorage::new()));
    let id1 = r.init().unwrap();
    let id2 = r.init().unwrap();
    assert_eq!(id1, id2, "second init must return same commit id");
}

// ---------------------------------------------------------------------------
// Basic state operations
// ---------------------------------------------------------------------------

#[test]
fn set_and_get_string() {
    let r = repo();
    r.set(
        "main",
        "/name",
        &Object::string("prod"),
        opts(IntentCategory::Checkpoint, "set name"),
    )
    .unwrap();
    let val = r.get("main", "/name").unwrap();
    assert_eq!(val, Object::string("prod"));
}

#[test]
fn set_json_and_get_json() {
    let r = repo();
    r.set_json(
        "main",
        "/config",
        &serde_json::json!({"replicas": 3, "region": "us-east-1"}),
        opts(IntentCategory::Checkpoint, "set config"),
    )
    .unwrap();
    let val = r.get_json("main", "/config").unwrap();
    assert_eq!(val["replicas"], 3);
    assert_eq!(val["region"], "us-east-1");
}

#[test]
fn set_updates_existing_value() {
    let r = repo();
    r.set(
        "main",
        "/count",
        &Object::int(1),
        opts(IntentCategory::Checkpoint, "init"),
    )
    .unwrap();
    r.set(
        "main",
        "/count",
        &Object::int(2),
        opts(IntentCategory::Refine, "increment"),
    )
    .unwrap();
    let val = r.get("main", "/count").unwrap();
    assert_eq!(val, Object::int(2));
}

#[test]
fn delete_removes_value() {
    let r = repo();
    r.set(
        "main",
        "/tmp",
        &Object::string("ephemeral"),
        opts(IntentCategory::Explore, "temp"),
    )
    .unwrap();
    r.delete("main", "/tmp", opts(IntentCategory::Fix, "cleanup"))
        .unwrap();
    // After deletion the path should be gone
    let result = r.get("main", "/tmp");
    assert!(result.is_err(), "deleted path should not be readable");
}

#[test]
fn get_nested_path() {
    let r = repo();
    r.set_json(
        "main",
        "/cluster",
        &serde_json::json!({
            "nodes": [{"name": "node-1", "status": "Ready"}],
            "network": {"subnet": "10.0.0.0/24"}
        }),
        opts(IntentCategory::Checkpoint, "init cluster"),
    )
    .unwrap();
    let subnet = r.get_json("main", "/cluster/network/subnet").unwrap();
    assert_eq!(subnet.as_str().unwrap(), "10.0.0.0/24");
}

// ---------------------------------------------------------------------------
// Meta-path guard
// ---------------------------------------------------------------------------

#[test]
fn meta_write_requires_migrate_category() {
    let r = repo();
    let err = r
        .set(
            "main",
            "/_meta/custom_key",
            &Object::string("val"),
            opts(IntentCategory::Checkpoint, "should fail"),
        )
        .unwrap_err();
    assert!(
        matches!(err, RepoError::ReservedPath(_)),
        "expected ReservedPath, got {:?}",
        err
    );
}

#[test]
fn meta_write_succeeds_with_migrate() {
    let r = repo();
    r.set(
        "main",
        "/_meta/schema_version",
        &Object::string("0.5.0"),
        opts(IntentCategory::Migrate, "bump schema version"),
    )
    .unwrap();
    let val = r.get_json("main", "/_meta/schema_version").unwrap();
    assert_eq!(val.as_str().unwrap(), "0.5.0");
}

#[test]
fn secret_meta_read_is_blocked() {
    let r = repo();
    // Write a secret path via Migrate
    r.set(
        "main",
        "/_meta/_secret/api_key",
        &Object::string("s3cr3t"),
        opts(IntentCategory::Migrate, "store secret"),
    )
    .unwrap();
    // Normal get must be rejected
    let err = r.get("main", "/_meta/_secret/api_key").unwrap_err();
    assert!(
        matches!(err, RepoError::ReservedPath(_)),
        "expected ReservedPath for secret read, got {:?}",
        err
    );
}

#[test]
fn secret_meta_read_allowed_with_migrate_intent() {
    use agentstategraph_core::Intent;
    let r = repo();
    r.set(
        "main",
        "/_meta/_secret/api_key",
        &Object::string("s3cr3t"),
        opts(IntentCategory::Migrate, "store secret"),
    )
    .unwrap();
    let intent = Intent::new(IntentCategory::Migrate, "read secret for migration");
    let val = r
        .get_with_intent("main", "/_meta/_secret/api_key", &intent)
        .unwrap();
    assert_eq!(val, Object::string("s3cr3t"));
}

#[test]
fn list_paths_filters_secret_prefix() {
    let r = repo();
    r.set(
        "main",
        "/_meta/_secret/tok",
        &Object::string("x"),
        opts(IntentCategory::Migrate, "add secret"),
    )
    .unwrap();
    r.set(
        "main",
        "/public/data",
        &Object::string("ok"),
        opts(IntentCategory::Checkpoint, "add public"),
    )
    .unwrap();
    let paths = r.list_paths("main", "/", None).unwrap();
    assert!(
        !paths.iter().any(|p| p.starts_with("/_meta/_secret")),
        "secret paths must not appear in list_paths output"
    );
    assert!(
        paths.contains(&"/public/data".to_string()),
        "public path should be listed"
    );
}

// ---------------------------------------------------------------------------
// Branch operations
// ---------------------------------------------------------------------------

#[test]
fn branch_create_and_list() {
    let r = repo();
    r.set(
        "main",
        "/x",
        &Object::int(1),
        opts(IntentCategory::Checkpoint, "init"),
    )
    .unwrap();
    r.branch("feature", "main").unwrap();

    let branches = r.list_branches(None).unwrap();
    let names: Vec<&str> = branches.iter().map(|(n, _)| n.as_str()).collect();
    assert!(names.contains(&"main"));
    assert!(names.contains(&"feature"));
}

#[test]
fn branch_already_exists_error() {
    let r = repo();
    r.branch("feature", "main").unwrap();
    let err = r.branch("feature", "main").unwrap_err();
    assert!(
        matches!(err, RepoError::BranchAlreadyExists(_)),
        "expected BranchAlreadyExists, got {:?}",
        err
    );
}

#[test]
fn branch_from_unknown_ref_errors() {
    let r = repo();
    let err = r.branch("feat", "nonexistent").unwrap_err();
    assert!(
        matches!(err, RepoError::BranchNotFound(_)),
        "expected BranchNotFound, got {:?}",
        err
    );
}

#[test]
fn delete_branch() {
    let r = repo();
    r.branch("temp", "main").unwrap();
    assert!(r.delete_branch("temp").unwrap());
    // second delete returns false
    assert!(!r.delete_branch("temp").unwrap());
    let branches = r.list_branches(None).unwrap();
    assert!(!branches.iter().any(|(n, _)| n == "temp"));
}

#[test]
fn branch_prefix_filter() {
    let r = repo();
    r.branch("agents/a/workspace", "main").unwrap();
    r.branch("agents/b/workspace", "main").unwrap();
    r.branch("explore/v2", "main").unwrap();

    let agent_branches = r.list_branches(Some("agents/")).unwrap();
    assert_eq!(agent_branches.len(), 2);
    for (name, _) in &agent_branches {
        assert!(name.starts_with("agents/"));
    }
}

#[test]
fn changes_on_branch_do_not_affect_main() {
    let r = repo();
    r.set(
        "main",
        "/shared",
        &Object::string("main-value"),
        opts(IntentCategory::Checkpoint, "set on main"),
    )
    .unwrap();
    r.branch("fork", "main").unwrap();
    r.set(
        "fork",
        "/shared",
        &Object::string("fork-value"),
        opts(IntentCategory::Explore, "change on fork"),
    )
    .unwrap();

    let main_val = r.get("main", "/shared").unwrap();
    let fork_val = r.get("fork", "/shared").unwrap();
    assert_eq!(main_val, Object::string("main-value"));
    assert_eq!(fork_val, Object::string("fork-value"));
}

// ---------------------------------------------------------------------------
// Diff
// ---------------------------------------------------------------------------

#[test]
fn diff_empty_branches_is_empty() {
    let r = repo();
    r.branch("copy", "main").unwrap();
    let diff = r.diff("main", "copy").unwrap();
    assert!(diff.is_empty(), "identical branches must produce empty diff");
}

#[test]
fn diff_detects_changes() {
    use agentstategraph_core::DiffOp;
    let r = repo();
    r.branch("feature", "main").unwrap();
    r.set(
        "feature",
        "/version",
        &Object::string("2.0"),
        opts(IntentCategory::Explore, "bump version"),
    )
    .unwrap();
    let diff = r.diff("main", "feature").unwrap();
    assert!(
        !diff.is_empty(),
        "diff between diverged branches must not be empty"
    );
    // At least one AddKey or SetValue op for /version
    let has_version = diff.iter().any(|op| match op {
        DiffOp::AddKey { key, .. } => key == "version",
        DiffOp::SetValue { path, .. } => path.contains("version"),
        _ => false,
    });
    assert!(has_version, "diff should mention the version key");
}

// ---------------------------------------------------------------------------
// Merge
// ---------------------------------------------------------------------------

#[test]
fn fast_forward_merge() {
    let r = repo();
    r.branch("feature", "main").unwrap();
    r.set(
        "feature",
        "/x",
        &Object::int(42),
        opts(IntentCategory::Explore, "add x"),
    )
    .unwrap();
    // main has no new commits — fast-forward
    r.merge("feature", "main", opts(IntentCategory::Merge, "ff merge"))
        .unwrap();
    let val = r.get("main", "/x").unwrap();
    assert_eq!(val, Object::int(42));
}

#[test]
fn three_way_merge_non_conflicting() {
    let r = repo();
    r.branch("feature", "main").unwrap();
    r.set(
        "feature",
        "/feat",
        &Object::string("new"),
        opts(IntentCategory::Explore, "add feat"),
    )
    .unwrap();
    r.set(
        "main",
        "/other",
        &Object::string("also-new"),
        opts(IntentCategory::Checkpoint, "add other"),
    )
    .unwrap();

    r.merge("feature", "main", opts(IntentCategory::Merge, "merge feat"))
        .unwrap();

    // Both paths should be present after merge
    let feat = r.get("main", "/feat").unwrap();
    let other = r.get("main", "/other").unwrap();
    assert_eq!(feat, Object::string("new"));
    assert_eq!(other, Object::string("also-new"));
}

#[test]
fn merge_conflict_returns_error() {
    let r = repo();
    r.set(
        "main",
        "/x",
        &Object::string("base"),
        opts(IntentCategory::Checkpoint, "base"),
    )
    .unwrap();
    r.branch("feature", "main").unwrap();

    // Both branches modify the same path independently
    r.set(
        "main",
        "/x",
        &Object::string("main-update"),
        opts(IntentCategory::Refine, "main changes x"),
    )
    .unwrap();
    r.set(
        "feature",
        "/x",
        &Object::string("feature-update"),
        opts(IntentCategory::Explore, "feature changes x"),
    )
    .unwrap();

    let err = r
        .merge("feature", "main", opts(IntentCategory::Merge, "conflict merge"))
        .unwrap_err();
    assert!(
        matches!(err, RepoError::MergeConflicts(_)),
        "expected MergeConflicts, got {:?}",
        err
    );
}

// ---------------------------------------------------------------------------
// Log and blame
// ---------------------------------------------------------------------------

#[test]
fn log_returns_commits_in_reverse_order() {
    let r = repo();
    r.set(
        "main",
        "/a",
        &Object::int(1),
        opts(IntentCategory::Checkpoint, "first"),
    )
    .unwrap();
    r.set(
        "main",
        "/b",
        &Object::int(2),
        opts(IntentCategory::Refine, "second"),
    )
    .unwrap();
    r.set(
        "main",
        "/c",
        &Object::int(3),
        opts(IntentCategory::Fix, "third"),
    )
    .unwrap();

    let log = r.log("main", 10).unwrap();
    // Most recent first
    assert_eq!(log[0].intent.description, "third");
    assert_eq!(log[1].intent.description, "second");
}

#[test]
fn log_respects_limit() {
    let r = repo();
    for i in 0..10 {
        r.set(
            "main",
            "/counter",
            &Object::int(i),
            opts(IntentCategory::Refine, &format!("step {i}")),
        )
        .unwrap();
    }
    let log = r.log("main", 3).unwrap();
    assert_eq!(log.len(), 3);
}

#[test]
fn blame_identifies_last_modifier() {
    let r = repo();
    r.set(
        "main",
        "/city",
        &Object::string("Boston"),
        opts_agent("agent/alpha", IntentCategory::Checkpoint, "init city"),
    )
    .unwrap();
    r.set(
        "main",
        "/city",
        &Object::string("Seattle"),
        opts_agent("agent/beta", IntentCategory::Refine, "move city"),
    )
    .unwrap();

    let blame = r.blame("main", "/city").unwrap();
    assert_eq!(blame.agent_id, "agent/beta");
    assert_eq!(blame.intent_description, "move city");
}

#[test]
fn blame_on_unchanged_path_finds_creator() {
    let r = repo();
    r.set(
        "main",
        "/region",
        &Object::string("us-east-1"),
        opts_agent("agent/setup", IntentCategory::Checkpoint, "init region"),
    )
    .unwrap();
    // Several other commits that don't touch /region
    r.set(
        "main",
        "/other",
        &Object::int(1),
        opts(IntentCategory::Refine, "unrelated"),
    )
    .unwrap();

    let blame = r.blame("main", "/region").unwrap();
    assert_eq!(blame.agent_id, "agent/setup");
}

// ---------------------------------------------------------------------------
// Query
// ---------------------------------------------------------------------------

#[test]
fn query_by_agent() {
    let r = repo();
    r.set(
        "main",
        "/a",
        &Object::int(1),
        opts_agent("agent/alice", IntentCategory::Explore, "alice writes"),
    )
    .unwrap();
    r.set(
        "main",
        "/b",
        &Object::int(2),
        opts_agent("agent/bob", IntentCategory::Explore, "bob writes"),
    )
    .unwrap();
    r.set(
        "main",
        "/c",
        &Object::int(3),
        opts_agent("agent/alice", IntentCategory::Refine, "alice refines"),
    )
    .unwrap();

    let filters = QueryFilters {
        agent_id: Some("agent/alice".to_string()),
        ..Default::default()
    };
    let results = r.query_commits("main", &filters, 100).unwrap();
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|c| c.agent_id == "agent/alice"));
}

#[test]
fn query_by_intent_category() {
    let r = repo();
    r.set(
        "main",
        "/a",
        &Object::int(1),
        opts(IntentCategory::Explore, "explore"),
    )
    .unwrap();
    r.set(
        "main",
        "/b",
        &Object::int(2),
        opts(IntentCategory::Fix, "fix"),
    )
    .unwrap();
    r.set(
        "main",
        "/c",
        &Object::int(3),
        opts(IntentCategory::Explore, "another explore"),
    )
    .unwrap();

    let filters = QueryFilters {
        intent_category: Some("Explore".to_string()),
        ..Default::default()
    };
    let results = r.query_commits("main", &filters, 100).unwrap();
    assert_eq!(results.len(), 2);
}

#[test]
fn query_by_confidence_range() {
    let r = repo();
    r.set(
        "main",
        "/a",
        &Object::int(1),
        opts(IntentCategory::Explore, "low confidence")
            .with_confidence(0.4),
    )
    .unwrap();
    r.set(
        "main",
        "/b",
        &Object::int(2),
        opts(IntentCategory::Checkpoint, "high confidence")
            .with_confidence(0.95),
    )
    .unwrap();

    let filters = QueryFilters {
        confidence_range: Some((0.9, 1.0)),
        ..Default::default()
    };
    let results = r.query_commits("main", &filters, 100).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].confidence.unwrap(), 0.95);
}

// ---------------------------------------------------------------------------
// Explorer / viewer
// ---------------------------------------------------------------------------

#[test]
fn list_paths_returns_leaf_paths() {
    let r = repo();
    r.set_json(
        "main",
        "/cluster",
        &serde_json::json!({
            "name": "prod",
            "region": "us-east-1",
            "nodes": {"count": 5}
        }),
        opts(IntentCategory::Checkpoint, "init"),
    )
    .unwrap();

    let paths = r.list_paths("main", "/cluster", None).unwrap();
    assert!(paths.contains(&"/cluster/name".to_string()));
    assert!(paths.contains(&"/cluster/region".to_string()));
    assert!(paths.contains(&"/cluster/nodes/count".to_string()));
}

#[test]
fn get_tree_returns_subtree_as_json() {
    let r = repo();
    r.set_json(
        "main",
        "/net",
        &serde_json::json!({"subnet": "10.0.0.0/24", "dns": "1.1.1.1"}),
        opts(IntentCategory::Checkpoint, "init net"),
    )
    .unwrap();

    let tree = r.get_tree("main", "/net").unwrap();
    assert_eq!(tree["subnet"].as_str().unwrap(), "10.0.0.0/24");
    assert_eq!(tree["dns"].as_str().unwrap(), "1.1.1.1");
}

#[test]
fn search_values_finds_matching() {
    let r = repo();
    r.set_json(
        "main",
        "/config",
        &serde_json::json!({
            "storage": "nfs-mount",
            "network": "mesh",
            "env": "production"
        }),
        opts(IntentCategory::Checkpoint, "init"),
    )
    .unwrap();

    let results = r.search_values("main", "mesh", None).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, "/config/network");
    assert_eq!(results[0].1, "mesh");
}

#[test]
fn search_values_is_case_insensitive() {
    let r = repo();
    r.set(
        "main",
        "/env",
        &Object::string("Production"),
        opts(IntentCategory::Checkpoint, "init"),
    )
    .unwrap();
    // search with lowercase
    let results = r.search_values("main", "production", None).unwrap();
    assert!(!results.is_empty(), "case-insensitive search should match");
}

#[test]
fn stats_reflects_repo_state() {
    let r = repo();
    r.set(
        "main",
        "/x",
        &Object::int(1),
        opts_agent("agent/ops", IntentCategory::Checkpoint, "setup"),
    )
    .unwrap();
    r.branch("feature", "main").unwrap();

    let stats = r.stats("main").unwrap();
    assert!(stats["commit_count"].as_u64().unwrap() >= 2); // init + set
    assert!(stats["branch_count"].as_u64().unwrap() >= 2); // main + feature
    assert!(stats["path_count"].as_u64().unwrap() >= 1);
    let agents = stats["agents"].as_array().unwrap();
    assert!(agents.iter().any(|a| a.as_str() == Some("agent/ops")));
}

// ---------------------------------------------------------------------------
// Speculation (through Repository API)
// ---------------------------------------------------------------------------

#[test]
fn speculate_isolates_changes_from_base() {
    let r = repo();
    r.set(
        "main",
        "/storage",
        &Object::string("none"),
        opts(IntentCategory::Checkpoint, "init"),
    )
    .unwrap();

    let h = r.speculate("main", Some("try-nfs".to_string())).unwrap();
    r.spec_set(h, "/storage", &Object::string("nfs")).unwrap();

    // Speculation has new value
    let spec_val = r.spec_get(h, "/storage").unwrap();
    assert_eq!(spec_val, Object::string("nfs"));

    // Main is unchanged
    let main_val = r.get("main", "/storage").unwrap();
    assert_eq!(main_val, Object::string("none"));
}

#[test]
fn commit_speculation_promotes_to_real_commit() {
    let r = repo();
    r.set(
        "main",
        "/version",
        &Object::string("1.0"),
        opts(IntentCategory::Checkpoint, "init"),
    )
    .unwrap();

    let h = r.speculate("main", Some("v2".to_string())).unwrap();
    r.spec_set(h, "/version", &Object::string("2.0")).unwrap();
    r.commit_speculation(h, opts(IntentCategory::Checkpoint, "adopt v2"))
        .unwrap();

    let val = r.get("main", "/version").unwrap();
    assert_eq!(val, Object::string("2.0"));
    // Speculation was consumed
    assert!(
        r.spec_get(h, "/version").is_err(),
        "consumed speculation should error"
    );
}

#[test]
fn discard_speculation_frees_handle() {
    let r = repo();
    let h = r.speculate("main", None).unwrap();
    r.spec_set(h, "/tmp", &Object::string("val")).unwrap();
    r.discard_speculation(h).unwrap();

    assert_eq!(r.list_speculations().len(), 0);
}

#[test]
fn compare_speculations() {
    let r = repo();
    r.set(
        "main",
        "/storage",
        &Object::string("none"),
        opts(IntentCategory::Checkpoint, "init"),
    )
    .unwrap();

    let nfs = r.speculate("main", Some("nfs".to_string())).unwrap();
    let ceph = r.speculate("main", Some("ceph".to_string())).unwrap();
    r.spec_set(nfs, "/storage", &Object::string("nfs")).unwrap();
    r.spec_set(ceph, "/storage", &Object::string("ceph"))
        .unwrap();

    let cmp = r.compare_speculations(&[nfs, ceph]).unwrap();
    assert_eq!(cmp.entries.len(), 2);
    assert!(!cmp.entries[0].diff_from_base.is_empty());
    assert!(!cmp.entries[1].diff_from_base.is_empty());
    assert_eq!(cmp.entries[0].label.as_deref(), Some("nfs"));
    assert_eq!(cmp.entries[1].label.as_deref(), Some("ceph"));
}

// ---------------------------------------------------------------------------
// Commit graph and intent tree
// ---------------------------------------------------------------------------

#[test]
fn commit_graph_returns_nodes() {
    let r = repo();
    r.set(
        "main",
        "/x",
        &Object::int(1),
        opts(IntentCategory::Checkpoint, "step 1"),
    )
    .unwrap();
    r.set(
        "main",
        "/x",
        &Object::int(2),
        opts(IntentCategory::Refine, "step 2"),
    )
    .unwrap();

    let graph = r.commit_graph("main", 10).unwrap();
    assert!(graph.len() >= 2);
    // Each node should have the expected fields
    let node = &graph[0];
    assert!(node["id"].is_string());
    assert!(node["category"].is_string());
    assert!(node["is_merge"].is_boolean());
}

// ---------------------------------------------------------------------------
// Epochs
// ---------------------------------------------------------------------------

#[test]
fn create_and_list_epoch() {
    let r = repo();
    r.create_epoch("q1-2026", "Q1 2026 work", vec!["intent-001".to_string()])
        .unwrap();
    let epochs = r.list_epochs().unwrap();
    assert!(epochs.iter().any(|e| e.id == "q1-2026"));
}

#[test]
fn seal_epoch() {
    let r = repo();
    r.create_epoch("sprint-1", "Sprint 1", vec![]).unwrap();
    r.set(
        "main",
        "/sprint",
        &Object::int(1),
        opts(IntentCategory::Checkpoint, "sprint work"),
    )
    .unwrap();
    r.seal_epoch("sprint-1", "Sprint 1 complete").unwrap();

    let epoch = r.get_epoch("sprint-1").unwrap();
    assert!(
        epoch.sealed_at.is_some(),
        "sealed epoch should have sealed_at timestamp"
    );
}

// ---------------------------------------------------------------------------
// Error variants
// ---------------------------------------------------------------------------

#[test]
fn branch_not_found_error() {
    let r = repo();
    let err = r.get("nonexistent", "/x").unwrap_err();
    assert!(
        matches!(err, RepoError::BranchNotFound(_)),
        "expected BranchNotFound, got {:?}",
        err
    );
}

#[test]
fn ref_not_found_on_missing_path() {
    let r = repo();
    // Path that was never written — tree_get will return a TreeError which
    // becomes a RepoError::Tree variant. Confirm the error, whatever type.
    let result = r.get("main", "/does_not_exist");
    assert!(result.is_err(), "accessing a missing path should error");
}

#[test]
fn reserved_path_includes_the_path() {
    let r = repo();
    let err = r
        .set(
            "main",
            META_PATH_PREFIX,
            &Object::string("val"),
            opts(IntentCategory::Checkpoint, "bad write"),
        )
        .unwrap_err();
    if let RepoError::ReservedPath(path) = err {
        assert!(path.contains("_meta"));
    } else {
        panic!("expected ReservedPath, got {:?}", err);
    }
}
