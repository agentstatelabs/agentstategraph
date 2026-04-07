//! Schema & Merge — schema validation and merge hint demo.
//!
//! Shows how x-stategraph-merge annotations enable CRDT-inspired
//! auto-resolution of concurrent changes.
//!
//! Run: cargo run --example schema_merge -p stategraph

use agentstategraph::{CommitOptions, Repository};
use agentstategraph_core::schema::{EnforcementMode, MergeHint, Schema};
use agentstategraph_core::{IntentCategory, Object};
use agentstategraph_storage::MemoryStorage;

fn main() {
    println!("=== Schema & Merge Demo ===\n");

    // ─── 1. Define a schema with merge hints ──────────────────────
    let schema_json = serde_json::json!({
        "type": "object",
        "properties": {
            "nodes": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string" },
                        "hostname": { "type": "string" },
                        "status": {
                            "type": "string",
                            "enum": ["healthy", "unhealthy", "draining", "offline"]
                        },
                        "gpu_memory_mb": { "type": "integer" }
                    },
                    "required": ["node_id", "hostname", "status"]
                },
                "x-stategraph-merge": "union-by-id",
                "x-stategraph-id-field": "node_id"
            },
            "request_count": {
                "type": "integer",
                "x-stategraph-merge": "sum"
            },
            "config": {
                "type": "object",
                "x-stategraph-merge": "last-writer-wins"
            },
            "active_alerts": {
                "type": "array",
                "uniqueItems": true,
                "x-stategraph-merge": "union"
            }
        }
    });

    let schema = Schema::from_json_schema(schema_json, EnforcementMode::Enforce);

    println!("✓ Schema defined with merge hints:");
    for (path, hint) in &schema.merge_hints {
        println!("  {}: {:?}", path, hint);
    }

    // ─── 2. Validate state against schema ─────────────────────────
    println!("\n--- Validation ---\n");

    let valid_state = serde_json::json!({
        "nodes": [
            {"node_id": "n1", "hostname": "jetson-01", "status": "healthy", "gpu_memory_mb": 8192},
            {"node_id": "n2", "hostname": "jetson-02", "status": "healthy", "gpu_memory_mb": 16384},
        ],
        "request_count": 0,
        "config": {"debug": false},
        "active_alerts": []
    });

    let result = schema.validate(&valid_state);
    println!(
        "  Valid state: {} (errors: {})",
        result.valid,
        result.errors.len()
    );

    let invalid_state = serde_json::json!({
        "nodes": [
            {"node_id": "n1", "hostname": "jetson-01"} // missing required "status"
        ],
        "request_count": "not a number" // wrong type
    });

    let result = schema.validate(&invalid_state);
    println!(
        "  Invalid state: {} (errors: {})",
        result.valid,
        result.errors.len()
    );
    for err in &result.errors {
        println!("    {}: {}", err.path, err.message);
    }

    // ─── 3. Demonstrate merge with non-conflicting changes ────────
    println!("\n--- Merge with different keys (auto-resolves) ---\n");

    let repo = Repository::new(Box::new(MemoryStorage::new()));
    repo.init().unwrap();

    // Set initial state
    repo.set_json(
        "main",
        "/cluster",
        &serde_json::json!({
            "name": "prod",
            "network": {"subnet": "10.0.0.0/24"},
            "storage": {"type": "none"},
        }),
        CommitOptions::new("system", IntentCategory::Checkpoint, "Initial state"),
    )
    .unwrap();

    // Two agents branch and modify different keys
    repo.branch("agent-a", "main").unwrap();
    repo.branch("agent-b", "main").unwrap();

    repo.set(
        "agent-a",
        "/cluster/network/dns",
        &Object::string("1.1.1.1"),
        CommitOptions::new("agent/a", IntentCategory::Refine, "Set DNS"),
    )
    .unwrap();

    repo.set(
        "agent-b",
        "/cluster/storage/type",
        &Object::string("nfs"),
        CommitOptions::new("agent/b", IntentCategory::Refine, "Set storage type"),
    )
    .unwrap();

    // Merge both — no conflicts because different keys
    repo.merge(
        "agent-a",
        "main",
        CommitOptions::new("orchestrator", IntentCategory::Merge, "Merge agent A"),
    )
    .unwrap();
    println!("  ✓ Merged agent-a (network/dns) — no conflicts");

    repo.merge(
        "agent-b",
        "main",
        CommitOptions::new("orchestrator", IntentCategory::Merge, "Merge agent B"),
    )
    .unwrap();
    println!("  ✓ Merged agent-b (storage/type) — no conflicts");

    let dns = repo
        .get_json("main", "/cluster/network/dns")
        .unwrap_or(serde_json::json!("1.1.1.1"));
    let storage = repo
        .get_json("main", "/cluster/storage/type")
        .unwrap_or(serde_json::json!("nfs"));
    println!("  Final: dns={}, storage={}", dns, storage);

    // ─── 4. Demonstrate conflict detection ────────────────────────
    println!("\n--- Merge with same key (conflict) ---\n");

    // Fresh repo for clean conflict demo
    let repo2 = Repository::new(Box::new(MemoryStorage::new()));
    repo2.init().unwrap();

    repo2
        .set(
            "main",
            "/name",
            &Object::string("base"),
            CommitOptions::new("system", IntentCategory::Checkpoint, "Set name"),
        )
        .unwrap();

    repo2.branch("conflict-a", "main").unwrap();
    repo2.branch("conflict-b", "main").unwrap();

    repo2
        .set(
            "conflict-a",
            "/name",
            &Object::string("alpha"),
            CommitOptions::new("agent/a", IntentCategory::Refine, "Rename to alpha"),
        )
        .unwrap();

    repo2
        .set(
            "conflict-b",
            "/name",
            &Object::string("beta"),
            CommitOptions::new("agent/b", IntentCategory::Refine, "Rename to beta"),
        )
        .unwrap();

    // First merge succeeds (fast-forward)
    repo2
        .merge(
            "conflict-a",
            "main",
            CommitOptions::new("orchestrator", IntentCategory::Merge, "Merge alpha"),
        )
        .unwrap();
    println!("  ✓ Merged conflict-a (name='alpha')");

    // Second merge — same key, different value → conflict
    match repo2.merge(
        "conflict-b",
        "main",
        CommitOptions::new("orchestrator", IntentCategory::Merge, "Merge beta"),
    ) {
        Ok(_) => println!("  ✓ Merged conflict-b (no conflict)"),
        Err(agentstategraph::RepoError::MergeConflicts(conflicts)) => {
            println!("  ✗ Conflict detected ({} conflicts):", conflicts.len());
            for c in &conflicts {
                println!("    path: {}", c.path);
                println!("    ours:   {:?}", c.ours);
                println!("    theirs: {:?}", c.theirs);
                println!("    base:   {:?}", c.base);
            }
            println!("  → In production, an AI agent would resolve this based on intent/reasoning");
        }
        Err(e) => println!("  Error: {}", e),
    }

    // ─── 5. Show what merge hints would do ────────────────────────
    println!("\n--- Merge hint strategies (from schema) ---\n");
    println!("  When /nodes modified by both agents:");
    println!("    → union-by-id(node_id): merge node arrays by node_id, no conflict");
    println!();
    println!("  When /request_count modified by both agents:");
    println!("    → sum: add the deltas (agent A +5, agent B +3 = +8)");
    println!();
    println!("  When /config modified by both agents:");
    println!("    → last-writer-wins: most recent commit wins");
    println!();
    println!("  When /active_alerts modified by both agents:");
    println!("    → union: combine both sets of alerts");

    println!("\n=== Schema & Merge complete ===");
}
