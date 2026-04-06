# StateGraph

**AI-native versioned state store for intent-based systems.**

StateGraph is a content-addressed, versioned, branchable structured state store designed as an infrastructure primitive for the next era of computing. It captures not just *what* changed, but *why*, *who authorized it*, *what alternatives were considered*, and *who was informed*.

## Why StateGraph?

| Era | Unit of Work | Key Primitives |
|-----|-------------|----------------|
| Monolithic | Function call | OS, filesystem, local DB |
| Batch / Request-Response | Request → Response | HTTP, REST, SQL, queues |
| Streaming | Event | Kafka, Flink, event stores, CQRS |
| **Intent-based** | **Intent → Outcome** | **StateGraph** |

Agents don't execute linear scripts — they explore state spaces. They need a primitive that supports speculative branching, comparison, and merge with full reasoning history. Git is text-oriented. Databases lack branching. Event sourcing is append-only. StateGraph fills the gap.

## Quick Start

### As an MCP Server (connect to Claude Code, GPT, any MCP agent)

```bash
# Clone and build
git clone <repo-url> && cd stategraph
cargo build -p stategraph-mcp

# Run the MCP server
cargo run -p stategraph-mcp
# → Creates ./stategraph.db, waits for MCP client connection
```

Add to your Claude Code MCP config (`~/.claude.json` or project `.mcp.json`):
```json
{
  "mcpServers": {
    "stategraph": {
      "command": "cargo",
      "args": ["run", "-p", "stategraph-mcp", "--manifest-path", "/path/to/stategraph/Cargo.toml"]
    }
  }
}
```

Then in Claude Code, you can:
```
> Set the cluster name to "prod" with intent "Checkpoint"
> Branch to "explore/new-config" and try a different network setup
> Compare the two branches
> Merge the winner back to main
```

### As a Rust Library

```rust
use stategraph::{Repository, CommitOptions};
use stategraph_storage::SqliteStorage;
use stategraph_core::{IntentCategory, Object};

// Create a repo with durable SQLite storage
let storage = SqliteStorage::open("./my-state.db").unwrap();
let repo = Repository::new(Box::new(storage));
repo.init().unwrap();

// Set state — every write is an atomic commit with intent
repo.set(
    "main",
    "/cluster/name",
    &Object::string("prod"),
    CommitOptions::new("agent/setup", IntentCategory::Checkpoint, "Initialize cluster name"),
).unwrap();

// Branch to explore alternatives
repo.branch("explore/new-network", "main").unwrap();
repo.set_json(
    "explore/new-network",
    "/cluster/network",
    &serde_json::json!({"subnet": "192.168.0.0/16", "dns": "1.1.1.1"}),
    CommitOptions::new("agent/planner", IntentCategory::Explore, "Try new subnet layout")
        .with_reasoning("Current /24 is too small for planned expansion")
        .with_confidence(0.8),
).unwrap();

// Diff branches
let changes = repo.diff("main", "explore/new-network").unwrap();
println!("{} changes", changes.len());

// Merge when ready
repo.merge(
    "explore/new-network",
    "main",
    CommitOptions::new("agent/planner", IntentCategory::Merge, "Adopt new network layout"),
).unwrap();

// Full audit trail
let log = repo.log("main", 10).unwrap();
for commit in &log {
    println!("{}: {} (by {}, confidence: {:?})",
        commit.id.short(),
        commit.intent.description,
        commit.agent_id,
        commit.confidence,
    );
}
```

## MCP Tools

13 tools available when connected as an MCP server:

| Tool | Description |
|------|-------------|
| `stategraph_get` | Read state at any branch/path |
| `stategraph_set` | Write with intent, reasoning, confidence |
| `stategraph_delete` | Remove with intent |
| `stategraph_branch` | Create namespaced branches |
| `stategraph_list_branches` | List branches by namespace |
| `stategraph_merge` | Schema-aware three-way merge |
| `stategraph_log` | Commit history with full provenance |
| `stategraph_diff` | Structured typed diffs (not text) |
| `stategraph_speculate` | Create lightweight speculation |
| `stategraph_spec_modify` | Modify within speculation |
| `stategraph_compare` | Compare speculations side-by-side |
| `stategraph_commit_spec` | Commit winning speculation |
| `stategraph_discard` | Discard losing speculation |

## What Makes StateGraph Different

Every commit captures the **full provenance chain**:

| Field | Question it answers |
|-------|-------------------|
| `state_root` | What changed? |
| `intent` | Why? (structured, queryable category + description + tags) |
| `reasoning` | How did the agent decide? |
| `confidence` | How sure was it? (0.0-1.0) |
| `agent_id` | Who did it? |
| `authority` | Who authorized it? (with delegation chain) |
| `tool_calls` | What actions produced this? |

## Architecture

```
stategraph/
├── stategraph-core        # Types, diff, merge — zero I/O deps
├── stategraph-storage     # Pluggable backends (memory, SQLite)
├── stategraph             # High-level Repository API
└── stategraph-mcp         # MCP server (13 tools over stdio)
```

## Specification

See [spec/STATEGRAPH-RFC.md](spec/STATEGRAPH-RFC.md) for the complete RFC covering:
- Core data model with content-addressed Merkle DAG
- Intent lifecycle (Proposed → Authorized → InProgress → Completed/Failed)
- Authority and delegation chains
- Resolution reporting (the "report back" to whoever authorized the work)
- Sub-agent orchestration
- Schema-aware merge with CRDT-inspired annotations
- Epoch-based lifecycle management
- Unified query interface

## License

MIT OR Apache-2.0
