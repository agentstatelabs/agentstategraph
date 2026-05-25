//! Namespaces — ref-layer isolation for multi-project / multi-tenant deployments.
//!
//! Two tenants ("acme" and "globex") share one repository and one database, but
//! their refs are completely isolated. Crossing the boundary is deny-by-default.
//!
//! Run: cargo run --example namespaces -p agentstategraph

use agentstategraph::{CommitOptions, Repository, RepoError};
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

    // ─── 2. Each tenant works in its own namespace ────────────────
    // fork_namespace() returns a lightweight sibling sharing the same
    // storage but operating in a different namespace. init() bootstraps
    // the `main` branch within each namespace.
    let acme = repo.fork_namespace(Namespace::new("acme").unwrap());
    let globex = repo.fork_namespace(Namespace::new("globex").unwrap());
    acme.init().unwrap();
    globex.init().unwrap();

    acme.set(
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

    // ─── 3. Same branch + path, isolated values ───────────────────
    let acme_plan = acme.get_json("main", "/billing/plan").unwrap();
    let globex_plan = globex.get_json("main", "/billing/plan").unwrap();
    println!("  acme   main:/billing/plan = {}", acme_plan);
    println!("  globex main:/billing/plan = {}", globex_plan);
    assert_ne!(acme_plan, globex_plan);
    println!("  ✓ Same branch name, fully isolated\n");

    // ─── 4. Cross-namespace merge is denied by default ────────────
    let result = globex.cross_namespace_merge(
        "acme",
        "main",
        "main",
        CommitOptions::new("agent/globex", IntentCategory::Merge, "Pull Acme's main"),
    );
    match result {
        Err(RepoError::CrossNamespaceAccessDenied) => {
            println!("  ✓ Cross-namespace merge denied (no PolicyStore + grant)");
        }
        other => panic!("expected deny-by-default, got {:?}", other),
    }

    println!("\n=== Namespaces example complete! ===");
}
