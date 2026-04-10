//! Complete agent workflow example: speculate, compare, pick winner.
//!
//! This demonstrates the core AgentStateGraph value proposition:
//! an agent exploring multiple approaches to a problem, comparing
//! outcomes, and committing the winner with full provenance.
//!
//! Run with: cargo run --example agent_workflow

use agentstategraph::{CommitOptions, Repository};
use agentstategraph_core::{IntentCategory, Object};
use agentstategraph_storage::MemoryStorage;

fn main() {
    println!("=== AgentStateGraph Agent Workflow Demo ===\n");

    // 1. Create a repository
    let repo = Repository::new(Box::new(MemoryStorage::new()));
    repo.init().unwrap();

    // 2. Set initial cluster state
    repo.set_json(
        "main",
        "/cluster",
        &serde_json::json!({
            "name": "production",
            "nodes": [
                {"id": "node-1", "hostname": "jetson-01", "status": "healthy", "gpu_mb": 8192},
                {"id": "node-2", "hostname": "jetson-02", "status": "healthy", "gpu_mb": 16384},
                {"id": "node-3", "hostname": "jetson-03", "status": "unhealthy", "gpu_mb": 16384}
            ],
            "storage": {"type": "none"},
            "network": {"subnet": "10.0.0.0/24"}
        }),
        CommitOptions::new(
            "system",
            IntentCategory::Checkpoint,
            "Initialize cluster state",
        ),
    )
    .unwrap();

    println!("Initial state committed to 'main'.\n");

    // 3. Agent creates three speculations for storage approach
    println!("--- Agent: Exploring storage options ---\n");

    let nfs = repo
        .speculate("main", Some("NFS shared storage".to_string()))
        .unwrap();
    let ceph = repo
        .speculate("main", Some("Ceph distributed storage".to_string()))
        .unwrap();
    let local = repo
        .speculate("main", Some("Local NVMe SSDs".to_string()))
        .unwrap();

    // NFS approach
    repo.spec_set(nfs, "/cluster/storage/type", &Object::string("nfs"))
        .unwrap();
    repo.spec_set(nfs, "/cluster/storage/server", &Object::string("jetson-01"))
        .unwrap();
    repo.spec_set(
        nfs,
        "/cluster/storage/mount",
        &Object::string("/shared/models"),
    )
    .unwrap();

    // Ceph approach
    repo.spec_set(ceph, "/cluster/storage/type", &Object::string("ceph"))
        .unwrap();
    repo.spec_set(ceph, "/cluster/storage/replicas", &Object::int(3))
        .unwrap();
    repo.spec_set(ceph, "/cluster/storage/pool", &Object::string("ml-data"))
        .unwrap();

    // Local SSD approach
    repo.spec_set(
        local,
        "/cluster/storage/type",
        &Object::string("local-nvme"),
    )
    .unwrap();
    repo.spec_set(
        local,
        "/cluster/storage/path",
        &Object::string("/dev/nvme0n1"),
    )
    .unwrap();

    // 4. Compare all three
    println!("--- Comparing speculations ---\n");
    let comparison = repo.compare_speculations(&[nfs, ceph, local]).unwrap();
    for entry in &comparison.entries {
        println!(
            "  {:?}: {} changes from base",
            entry.label,
            entry.diff_from_base.len()
        );
        for op in &entry.diff_from_base {
            println!("    {:?}", op);
        }
        println!();
    }

    // 5. Agent picks NFS (Ceph needs 3+ healthy nodes, local isn't shared)
    println!("--- Agent: Selecting NFS ---");
    println!("  Reasoning: NFS provides shared storage with minimal node requirements.");
    println!("  Ceph rejected: requires 3 healthy nodes, only 2 available.");
    println!("  Local rejected: not shared across nodes.\n");

    repo.commit_speculation(
        nfs,
        CommitOptions::new(
            "agent/storage-planner",
            IntentCategory::Refine,
            "Selected NFS — Ceph requires 3+ healthy nodes, local not shared",
        )
        .with_reasoning(
            "NFS provides shared storage with minimal node requirements. \
             Ceph requires 3 healthy replicas but node-3 is unhealthy. \
             Local NVMe is fast but not shared across nodes for distributed training.",
        )
        .with_confidence(0.85)
        .with_tags(vec!["storage".to_string(), "nfs".to_string()]),
    )
    .unwrap();

    // Discard losers
    repo.discard_speculation(ceph).unwrap();
    repo.discard_speculation(local).unwrap();

    // 6. Verify final state
    println!("--- Final state ---\n");
    let storage_type = repo.get_json("main", "/cluster/storage/type").unwrap();
    let storage_mount = repo.get_json("main", "/cluster/storage/mount").unwrap();
    println!("  storage.type:  {}", storage_type);
    println!("  storage.mount: {}", storage_mount);

    // 7. Show commit log with full provenance
    println!("\n--- Commit log ---\n");
    let log = repo.log("main", 10).unwrap();
    for commit in log.iter().rev() {
        println!(
            "  {} [{}]",
            commit.id.short(),
            format!("{:?}", commit.intent.category)
        );
        println!("    agent: {}", commit.agent_id);
        println!("    intent: {}", commit.intent.description);
        if !commit.intent.tags.is_empty() {
            println!("    tags: {:?}", commit.intent.tags);
        }
        if let Some(ref r) = commit.reasoning {
            let short = if r.len() > 80 { &r[..80] } else { r };
            println!("    reasoning: {}...", short);
        }
        if let Some(c) = commit.confidence {
            println!("    confidence: {}", c);
        }
        println!();
    }

    // 8. Diff between branches
    println!("--- Branch workflow ---\n");
    repo.branch("hotfix/node3", "main").unwrap();
    repo.set(
        "hotfix/node3",
        "/cluster/nodes/2/status",
        &Object::string("draining"),
        CommitOptions::new(
            "agent/health-monitor",
            IntentCategory::Fix,
            "Drain node-3 due to GPU fault",
        )
        .with_reasoning("Node-3 GPU health check failed — CUDA memory test error")
        .with_confidence(0.95),
    )
    .unwrap();

    let diff = repo.diff("main", "hotfix/node3").unwrap();
    println!("  Diff main → hotfix/node3:");
    for op in &diff {
        println!("    {:?}", op);
    }

    // Merge the hotfix
    repo.merge(
        "hotfix/node3",
        "main",
        CommitOptions::new(
            "agent/health-monitor",
            IntentCategory::Fix,
            "Apply node-3 drain hotfix",
        ),
    )
    .unwrap();

    let node3_status = repo.get_json("main", "/cluster/nodes/2/status").unwrap();
    println!("\n  After merge, node-3 status: {}", node3_status);

    println!("\n=== Demo complete ===");
    println!("Total commits: {}", repo.log("main", 100).unwrap().len());
}
