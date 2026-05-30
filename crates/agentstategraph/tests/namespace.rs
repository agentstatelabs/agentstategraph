//! Integration tests for namespace isolation and Repository namespace API.

use agentstategraph::session::CreateSessionParams;
use agentstategraph::{CommitOptions, RepoError, Repository};
use agentstategraph_core::{IntentCategory, Namespace, Object};
use agentstategraph_storage::SqliteStorage;

fn repo_in_ns(ns: &str) -> Repository {
    let storage = SqliteStorage::in_memory().expect("in-memory sqlite");
    let namespace = Namespace::new(ns).expect("valid namespace");
    let repo = Repository::new(Box::new(storage)).with_namespace(namespace);
    // create_namespace ensures the namespace row exists before init() writes refs
    repo.create_namespace(ns).expect("create namespace");
    repo.init().expect("init");
    repo
}

fn default_repo() -> Repository {
    let storage = SqliteStorage::in_memory().expect("in-memory sqlite");
    let repo = Repository::new(Box::new(storage));
    repo.init().expect("init");
    repo
}

fn checkpoint(desc: &str) -> CommitOptions {
    CommitOptions::new("agent/test", IntentCategory::Checkpoint, desc)
}

// ---------------------------------------------------------------------------
// Namespace creation and listing
// ---------------------------------------------------------------------------

#[test]
fn create_and_list_namespaces() {
    let repo = default_repo();
    repo.create_namespace("alpha").unwrap();
    repo.create_namespace("beta").unwrap();

    let names: Vec<String> = repo
        .list_namespaces()
        .unwrap()
        .into_iter()
        .map(|n| n.as_str().to_string())
        .collect();

    assert!(names.contains(&"default".to_string()));
    assert!(names.contains(&"alpha".to_string()));
    assert!(names.contains(&"beta".to_string()));
}

#[test]
fn create_namespace_is_idempotent() {
    let repo = default_repo();
    repo.create_namespace("gamma").unwrap();
    repo.create_namespace("gamma").unwrap(); // should not error
    let names = repo.list_namespaces().unwrap();
    let count = names.iter().filter(|n| n.as_str() == "gamma").count();
    assert_eq!(count, 1, "gamma should appear exactly once");
}

#[test]
fn create_namespace_rejects_invalid_name() {
    let repo = default_repo();
    let err = repo.create_namespace("has space").unwrap_err();
    assert!(
        matches!(err, RepoError::InvalidOperation(_)),
        "expected InvalidOperation, got {:?}",
        err
    );
}

// ---------------------------------------------------------------------------
// Ref isolation between namespaces
// ---------------------------------------------------------------------------

#[test]
fn refs_are_isolated_between_namespaces() {
    let repo_a = repo_in_ns("ns-a");
    let repo_b = repo_in_ns("ns-b");

    // Write to ns-a
    repo_a
        .set(
            "main",
            "/color",
            &Object::Atom(agentstategraph_core::Atom::String("red".into())),
            checkpoint("set color in ns-a"),
        )
        .unwrap();

    // ns-b's "main" is untouched — it only has the init commit
    let val_b = repo_b.get("main", "/color");
    // Should fail because the path does not exist yet in ns-b
    assert!(
        val_b.is_err(),
        "ns-b should not see ns-a's writes; got {:?}",
        val_b
    );
}

#[test]
fn branch_in_one_namespace_invisible_to_another() {
    let repo_a = repo_in_ns("ns-c");
    let repo_b = repo_in_ns("ns-d");

    repo_a.branch("feature", "main").unwrap();

    // ns-d should not have "feature"
    let branches_b = repo_b.list_branches(None).unwrap();
    let names: Vec<&str> = branches_b.iter().map(|(n, _)| n.as_str()).collect();
    assert!(
        !names.contains(&"feature"),
        "feature branch from ns-c leaked into ns-d: {:?}",
        names
    );
}

// ---------------------------------------------------------------------------
// Repository::namespace() and with_namespace()
// ---------------------------------------------------------------------------

#[test]
fn repository_namespace_reflects_configured_value() {
    let storage = SqliteStorage::in_memory().expect("in-memory sqlite");
    let ns = Namespace::new("my-project").unwrap();
    let repo = Repository::new(Box::new(storage)).with_namespace(ns.clone());
    assert_eq!(repo.namespace(), &ns);
}

#[test]
fn default_repository_uses_default_namespace() {
    let storage = SqliteStorage::in_memory().expect("in-memory sqlite");
    let repo = Repository::new(Box::new(storage));
    assert_eq!(repo.namespace().as_str(), "default");
}

// ---------------------------------------------------------------------------
// Same-namespace cross_namespace_merge is a plain merge
// ---------------------------------------------------------------------------

#[test]
fn cross_namespace_merge_same_namespace_succeeds() {
    let repo = repo_in_ns("same-ns");

    repo.branch("feature", "main").unwrap();
    repo.set(
        "feature",
        "/x",
        &Object::Atom(agentstategraph_core::Atom::String("hello".into())),
        checkpoint("write on feature"),
    )
    .unwrap();

    // same source namespace → plain merge
    let result = repo.cross_namespace_merge(
        "same-ns",
        "feature",
        "main",
        checkpoint("cross-ns merge (same)"),
    );
    assert!(
        result.is_ok(),
        "same-ns cross_namespace_merge failed: {:?}",
        result
    );

    let val = repo.get("main", "/x").unwrap();
    assert_eq!(
        val,
        Object::Atom(agentstategraph_core::Atom::String("hello".into()))
    );
}

// ---------------------------------------------------------------------------
// Different-namespace cross_namespace_merge is denied (no PolicyStore)
// ---------------------------------------------------------------------------

#[test]
fn cross_namespace_merge_across_namespaces_denied_without_policy() {
    let repo_src = repo_in_ns("src-ns");
    let repo_dst = repo_in_ns("dst-ns");

    repo_src
        .set(
            "main",
            "/msg",
            &Object::Atom(agentstategraph_core::Atom::String("secret".into())),
            checkpoint("write in src-ns"),
        )
        .unwrap();

    // dst-ns repo tries to merge from a different namespace
    let result = repo_dst.cross_namespace_merge(
        "src-ns", // different namespace
        "main",
        "main",
        checkpoint("cross-ns attempt"),
    );
    assert!(
        matches!(result, Err(RepoError::CrossNamespaceAccessDenied)),
        "expected CrossNamespaceAccessDenied, got {:?}",
        result
    );
}

// ---------------------------------------------------------------------------
// Session scope_namespace overrides repository namespace
// ---------------------------------------------------------------------------

#[test]
fn session_scope_namespace_overrides_repo_namespace() {
    let storage = SqliteStorage::in_memory().expect("in-memory sqlite");
    let repo = Repository::new(Box::new(storage));

    // create both namespaces
    repo.create_namespace("default").unwrap();
    repo.create_namespace("project-x").unwrap();

    repo.init().unwrap();

    // create a session with scope_namespace set
    let mgr = repo.sessions();
    let head = repo.head("main").unwrap();
    let session = mgr
        .create(
            "agent/test",
            "main",
            head,
            CreateSessionParams {
                scope_namespace: Some(Namespace::new("project-x").unwrap()),
                ..Default::default()
            },
        )
        .unwrap();

    // The session's scope_namespace should be the Namespace type.
    assert_eq!(
        session.scope_namespace.as_ref().map(|n| n.as_str()),
        Some("project-x")
    );

    // Verify that list_namespaces surfaces both namespaces.
    let namespaces = repo.list_namespaces().unwrap();
    let names: Vec<&str> = namespaces.iter().map(|n| n.as_str()).collect();
    assert!(names.contains(&"project-x"), "project-x should be listed");
}

// ---------------------------------------------------------------------------
// Repository::init() auto-creates its namespace
// ---------------------------------------------------------------------------

#[test]
fn init_auto_creates_namespace() {
    // Previously would fail with NamespaceNotFound for namespaces other than "default".
    let storage = SqliteStorage::in_memory().expect("in-memory sqlite");
    let ns = Namespace::new("fresh-ns").unwrap();
    let repo = Repository::new(Box::new(storage)).with_namespace(ns);
    // No explicit create_namespace call — init() should handle it.
    repo.init()
        .expect("init() should auto-create the namespace");
}

// ---------------------------------------------------------------------------
// delete_namespace
// ---------------------------------------------------------------------------

#[test]
fn delete_namespace_removes_refs_and_namespace() {
    let repo = default_repo();
    repo.create_namespace("to-delete").unwrap();

    // Create a ref in the namespace by using a repo scoped to it.
    let storage2 = SqliteStorage::in_memory().expect("in-memory sqlite");
    let ns = Namespace::new("to-delete").unwrap();
    let repo2 = Repository::new(Box::new(storage2)).with_namespace(ns);
    repo2.init().unwrap();

    // Deleting a non-default namespace that exists should return true.
    assert!(repo.delete_namespace("to-delete").unwrap());
    // Deleting it again should return false.
    assert!(!repo.delete_namespace("to-delete").unwrap());

    // It should no longer appear in list_namespaces.
    let names: Vec<String> = repo
        .list_namespaces()
        .unwrap()
        .into_iter()
        .map(|n| n.as_str().to_string())
        .collect();
    assert!(
        !names.contains(&"to-delete".to_string()),
        "deleted namespace must not appear in listing"
    );
}

#[test]
fn delete_default_namespace_is_rejected() {
    let repo = default_repo();
    let err = repo.delete_namespace("default").unwrap_err();
    assert!(
        matches!(err, RepoError::Storage(_)),
        "expected Storage error wrapping InvalidOperation, got {:?}",
        err
    );
}
