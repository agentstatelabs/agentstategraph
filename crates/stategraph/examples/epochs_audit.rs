//! Epochs & Audit — lifecycle management and compliance demo.
//!
//! Shows how to group work into epochs, seal them for immutability,
//! and use them as tamper-evident audit bundles.
//!
//! Run: cargo run --example epochs_audit -p stategraph

use stategraph::{CommitOptions, Repository};
use stategraph_core::{IntentCategory, Object};
use stategraph_storage::MemoryStorage;

fn main() {
    let repo = Repository::new(Box::new(MemoryStorage::new()));
    repo.init().unwrap();

    println!("=== Epochs & Audit Trail Demo ===\n");

    // ─── 1. Create an epoch for a body of work ───────────────────
    repo.create_epoch(
        "2026-Q2-cluster-setup",
        "Initial cluster setup for ML training",
        vec!["intent-setup".to_string()],
    )
    .unwrap();
    println!("✓ Epoch 'cluster-setup' created (Active)\n");

    // ─── 2. Do work within the epoch ──────────────────────────────
    println!("--- Work within epoch ---\n");

    repo.set_json(
        "main",
        "/cluster",
        &serde_json::json!({
            "name": "ml-prod",
            "nodes": 5,
            "storage": "nfs",
        }),
        CommitOptions::new("agent/setup", IntentCategory::Checkpoint, "Initialize cluster")
            .with_reasoning("Setting up 5-node cluster for distributed ML training")
            .with_confidence(0.95),
    )
    .unwrap();
    println!("  ✓ Cluster initialized");

    repo.set(
        "main",
        "/cluster/network",
        &Object::string("10GbE-bonded"),
        CommitOptions::new("agent/network", IntentCategory::Refine, "Configure network bonding")
            .with_reasoning("10GbE bonding for inter-node communication during training"),
    )
    .unwrap();
    println!("  ✓ Network configured");

    repo.set(
        "main",
        "/cluster/gpu_scheduling",
        &Object::string("memory-aware"),
        CommitOptions::new("agent/gpu", IntentCategory::Refine, "Set GPU scheduling policy")
            .with_reasoning("Memory-aware scheduling allocates workloads based on available VRAM"),
    )
    .unwrap();
    println!("  ✓ GPU scheduling configured");

    // ─── 3. Show audit trail before sealing ───────────────────────
    println!("\n--- Audit trail ---\n");

    let log = repo.log("main", 10).unwrap();
    for commit in log.iter().rev() {
        println!("  {} [{:?}]", commit.id.short(), commit.intent.category);
        println!("    by: {}", commit.agent_id);
        println!("    why: {}", commit.intent.description);
        if let Some(ref r) = commit.reasoning {
            println!("    reasoning: {}", r);
        }
        if let Some(c) = commit.confidence {
            println!("    confidence: {:.0}%", c * 100.0);
        }
        println!();
    }

    // ─── 4. Seal the epoch ────────────────────────────────────────
    repo.seal_epoch(
        "2026-Q2-cluster-setup",
        "Cluster setup complete. 5 nodes configured with NFS storage, 10GbE bonding, and memory-aware GPU scheduling.",
    )
    .unwrap();
    println!("✓ Epoch sealed — now immutable and tamper-evident\n");

    // ─── 5. Show epoch details ────────────────────────────────────
    let epochs = repo.list_epochs().unwrap();
    println!("--- Epochs ---\n");
    for epoch in &epochs {
        println!("  ID:          {}", epoch.id);
        println!("  Description: {}", epoch.description);
        println!("  Status:      {:?}", epoch.status);
        println!("  Agents:      {:?}", epoch.agents);
        if let Some(sealed_at) = epoch.sealed_at {
            println!("  Sealed at:   {}", sealed_at.format("%Y-%m-%d %H:%M:%S UTC"));
        }
        if let Some(ref hash) = epoch.seal_hash {
            println!("  Seal hash:   {}", hash.short());
        }
        println!();
    }

    // ─── 6. Start a new epoch for the next body of work ───────────
    repo.create_epoch(
        "2026-Q2-incident-node3",
        "Investigation of node-3 GPU fault",
        vec!["intent-incident".to_string()],
    )
    .unwrap();

    repo.set(
        "main",
        "/cluster/incidents/node3",
        &Object::string("GPU memory test failed — investigating"),
        CommitOptions::new("agent/health-monitor", IntentCategory::Fix, "Node-3 GPU fault detected")
            .with_reasoning("CUDA memory test failed on node-3 GPU. Initiating investigation.")
            .with_confidence(0.7),
    )
    .unwrap();
    println!("✓ New epoch 'incident-node3' started");
    println!("  (Previous epoch is sealed and can't be modified)\n");

    // ─── 7. Show both epochs ──────────────────────────────────────
    let all_epochs = repo.list_epochs().unwrap();
    println!("--- All epochs ---\n");
    for epoch in &all_epochs {
        println!("  {} [{}] — {}", epoch.id, format!("{:?}", epoch.status), epoch.description);
    }

    // ─── 8. Blame: trace any value to its origin ──────────────────
    println!("\n--- Blame (who changed what and why) ---\n");

    let fields = vec![
        "/cluster/storage",
        "/cluster/network",
        "/cluster/gpu_scheduling",
    ];

    for field in fields {
        match repo.blame("main", field) {
            Ok(entry) => {
                println!("  {}:", field);
                println!("    by: {}", entry.agent_id);
                println!("    why: {}", entry.intent_description);
                println!();
            }
            Err(e) => println!("  {}: {}", field, e),
        }
    }

    // ─── 9. Query: find specific commits ──────────────────────────
    println!("--- Query: all Fix intents ---\n");

    let fixes = repo
        .query_commits(
            "main",
            &stategraph_core::QueryFilters {
                intent_category: Some("Fix".to_string()),
                ..Default::default()
            },
            10,
        )
        .unwrap();

    for commit in &fixes {
        println!("  {} — {}", commit.id.short(), commit.intent.description);
        if let Some(ref r) = commit.reasoning {
            println!("    reasoning: {}", r);
        }
    }

    println!("\n=== Epochs & Audit complete ===");
    println!("Epochs: {}", repo.list_epochs().unwrap().len());
    println!("Total commits: {}", repo.log("main", 100).unwrap().len());
}
