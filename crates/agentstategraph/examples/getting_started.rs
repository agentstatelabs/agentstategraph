//! Getting Started — your first 5 minutes with StateGraph.
//!
//! This is the canonical "hello world" for StateGraph. Copy this
//! as your starting point.
//!
//! Run: cargo run --example getting_started -p stategraph

use agentstategraph::{CommitOptions, Repository};
use agentstategraph_core::{IntentCategory, Object};
use agentstategraph_storage::MemoryStorage;

fn main() {
    // ─── 1. Create a repository ───────────────────────────────────
    // MemoryStorage for quick start. Use SqliteStorage("path.db") for persistence.
    let repo = Repository::new(Box::new(MemoryStorage::new()));
    repo.init().unwrap();
    println!("✓ Repository initialized\n");

    // ─── 2. Set state ─────────────────────────────────────────────
    // Every write is an atomic commit with intent metadata.
    // No staging area — set() creates a commit immediately.
    repo.set(
        "main",
        "/app/name",
        &Object::string("my-project"),
        CommitOptions::new("developer", IntentCategory::Checkpoint, "Initialize project"),
    )
    .unwrap();

    // set_json for complex values
    repo.set_json(
        "main",
        "/app/config",
        &serde_json::json!({
            "debug": false,
            "max_retries": 3,
            "features": ["auth", "logging"]
        }),
        CommitOptions::new("developer", IntentCategory::Checkpoint, "Set default config"),
    )
    .unwrap();
    println!("✓ State set on main\n");

    // ─── 3. Read state ────────────────────────────────────────────
    let name = repo.get_json("main", "/app/name").unwrap();
    let debug = repo.get_json("main", "/app/config/debug").unwrap();
    println!("  app.name:         {}", name);
    println!("  app.config.debug: {}", debug);

    // ─── 4. Branch ────────────────────────────────────────────────
    // Branches are instant (just a pointer). Use them freely.
    repo.branch("feature/dark-mode", "main").unwrap();

    repo.set(
        "feature/dark-mode",
        "/app/config/theme",
        &Object::string("dark"),
        CommitOptions::new("developer", IntentCategory::Explore, "Try dark mode theme"),
    )
    .unwrap();
    println!("\n✓ Branch 'feature/dark-mode' created and modified\n");

    // main is untouched
    assert!(repo.get("main", "/app/config/theme").is_err()); // doesn't exist on main
    let branch_theme = repo.get_json("feature/dark-mode", "/app/config/theme").unwrap();
    println!("  main: no theme (correct — branch isolation)");
    println!("  feature/dark-mode: theme = {}", branch_theme);

    // ─── 5. Diff ──────────────────────────────────────────────────
    let changes = repo.diff("main", "feature/dark-mode").unwrap();
    println!("\n✓ Diff main → feature/dark-mode ({} changes):", changes.len());
    for op in &changes {
        println!("  {:?}", op);
    }

    // ─── 6. Merge ─────────────────────────────────────────────────
    repo.merge(
        "feature/dark-mode",
        "main",
        CommitOptions::new("developer", IntentCategory::Merge, "Adopt dark mode"),
    )
    .unwrap();

    let merged_theme = repo.get_json("main", "/app/config/theme").unwrap();
    println!("\n✓ Merged! main now has theme = {}", merged_theme);

    // ─── 7. History ───────────────────────────────────────────────
    println!("\n✓ Commit log:");
    let log = repo.log("main", 10).unwrap();
    for commit in log.iter().rev() {
        println!(
            "  {} [{:?}] {}",
            commit.id.short(),
            commit.intent.category,
            commit.intent.description
        );
    }

    // ─── 8. Delete ────────────────────────────────────────────────
    repo.delete(
        "main",
        "/app/config/debug",
        CommitOptions::new("developer", IntentCategory::Refine, "Remove debug flag for production"),
    )
    .unwrap();
    assert!(repo.get("main", "/app/config/debug").is_err());
    println!("\n✓ Deleted /app/config/debug");

    println!("\n=== Getting Started complete! ===");
    println!("Total commits: {}", repo.log("main", 100).unwrap().len());
}
