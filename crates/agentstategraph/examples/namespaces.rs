//! Namespaces — a separate set of branches per project in one store.
//!
//! Two projects ("acme" and "globex") share one repository and one database,
//! each with its own branches. They share the store's commits, so a commit id
//! reaches across; a repository limited to branch refs keeps a caller in one.
//!
//! Run: cargo run --example namespaces -p agentstategraph

use agentstategraph::{CommitOptions, RepoError, Repository};
use agentstategraph_core::{IntentCategory, Namespace, Object};
use agentstategraph_storage::SqliteStorage;

fn main() {
    // ─── 1. Create a repository and two namespaces ────────────────
    let repo = Repository::new(Box::new(
        SqliteStorage::in_memory().expect("in-memory sqlite"),
    ));
    repo.init().unwrap();

    repo.create_namespace("acme").unwrap();
    repo.create_namespace("globex").unwrap();
    println!("✓ Namespaces: {:?}\n", repo.list_namespaces().unwrap());

    // ─── 2. Each project works in its own namespace ───────────────
    // fork_namespace() returns a lightweight sibling sharing the same
    // storage but operating in a different namespace. init() bootstraps
    // the `main` branch within each namespace.
    let acme = repo.fork_namespace(Namespace::new("acme").unwrap());
    let globex = repo.fork_namespace(Namespace::new("globex").unwrap());
    acme.init().unwrap();
    globex.init().unwrap();

    let acme_commit = acme
        .set(
            "main",
            "/billing/plan",
            &Object::string("enterprise"),
            CommitOptions::new("agent/acme", IntentCategory::Checkpoint, "Acme plan"),
        )
        .unwrap();

    globex
        .set(
            "main",
            "/billing/plan",
            &Object::string("starter"),
            CommitOptions::new("agent/globex", IntentCategory::Checkpoint, "Globex plan"),
        )
        .unwrap();

    // ─── 3. Same branch + path, separate values ───────────────────
    let acme_plan = acme.get_json("main", "/billing/plan").unwrap();
    let globex_plan = globex.get_json("main", "/billing/plan").unwrap();
    println!("  acme   main:/billing/plan = {}", acme_plan);
    println!("  globex main:/billing/plan = {}", globex_plan);
    assert_ne!(acme_plan, globex_plan);
    println!("  ✓ Same branch name, separate branches\n");

    // ─── 4. Commit ids reach across namespaces ────────────────────
    // Namespaces share the store's commits: globex can read Acme's state
    // by naming Acme's commit.
    let by_id = globex
        .get_json(&acme_commit.to_hex(), "/billing/plan")
        .unwrap();
    assert_eq!(by_id, acme_plan);
    println!("  globex reads acme's commit by id: {}", by_id);

    // A repository limited to branch refs refuses commit ids, which keeps
    // its caller in one namespace (an HTTP key bound to a namespace gets
    // this limit).
    let confined = repo
        .fork_namespace(Namespace::new("globex").unwrap())
        .with_branch_refs_only();
    match confined.get_json(&acme_commit.to_hex(), "/billing/plan") {
        Err(RepoError::CommitRefNotAllowed(_)) => {
            println!("  ✓ Branch-refs-only repository refuses the commit id\n");
        }
        other => panic!("expected CommitRefNotAllowed, got {:?}", other),
    }

    // ─── 5. Cross-namespace merge is refused ──────────────────────
    let result = globex.cross_namespace_merge(
        "acme",
        "main",
        "main",
        CommitOptions::new("agent/globex", IntentCategory::Merge, "Pull Acme's main"),
    );
    match result {
        Err(RepoError::CrossNamespaceAccessDenied) => {
            println!("  ✓ Cross-namespace merge refused (not built yet)");
        }
        other => panic!("expected a refusal, got {:?}", other),
    }

    println!("\n=== Namespaces example complete! ===");
}
