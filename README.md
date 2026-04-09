# AgentStateGraph

**AI-native versioned state store for intent-based systems.**

AgentStateGraph is a content-addressed, versioned, branchable structured state store designed as an infrastructure primitive for the next era of computing. It captures not just *what* changed, but *why*, *who authorized it*, *what alternatives were considered*, and *who was informed*.

**Website:** [agentstategraph.dev](https://agentstategraph.dev)
**Demo app:** [ThreadWeaver](https://github.com/agentstatelabs/ThreadWeaver) — AI chat with branchable conversations, powered by AgentStateGraph

## Why AgentStateGraph?

| Era | Unit of Work | Key Primitives |
|-----|-------------|----------------|
| Monolithic | Function call | OS, filesystem, local DB |
| Batch / Request-Response | Request → Response | HTTP, REST, SQL, queues |
| Streaming | Event | Kafka, Flink, event stores, CQRS |
| **Intent-based** | **Intent → Outcome** | **AgentStateGraph** |

Agents don't execute linear scripts — they explore state spaces. They need a primitive that supports speculative branching, comparison, and merge with full reasoning history. Git is text-oriented. Databases lack branching. Event sourcing is append-only. AgentStateGraph fills the gap.

## Quick Start

### As an MCP Server (connect to Claude Code, GPT, any MCP agent)

```bash
git clone https://github.com/agentstatelabs/AgentStateGraph.git
cd AgentStateGraph
cargo build --release -p agentstategraph-mcp
cargo run --release -p agentstategraph-mcp
```

Add to your Claude Code MCP config:
```json
{
  "mcpServers": {
    "stategraph": {
      "command": "/path/to/AgentStateGraph/target/release/agentstategraph-mcp"
    }
  }
}
```

### As a Rust Library

```rust
use agentstategraph::{Repository, CommitOptions};
use agentstategraph_storage::SqliteStorage;
use agentstategraph_core::{IntentCategory, Object};

let storage = SqliteStorage::open("./state.db").unwrap();
let repo = Repository::new(Box::new(storage));
repo.init().unwrap();

// Every write is an atomic commit with intent
repo.set("main", "/cluster/name", &Object::string("prod"),
    CommitOptions::new("agent/setup", IntentCategory::Checkpoint, "init")
        .with_reasoning("Production cluster for ML training")
        .with_confidence(0.95));

// Branch, explore, merge
repo.branch("explore/new-network", "main").unwrap();
repo.diff("main", "explore/new-network").unwrap();
repo.merge("explore/new-network", "main",
    CommitOptions::new("agent/planner", IntentCategory::Merge, "Adopt new layout")).unwrap();

// Full audit trail
repo.log("main", 10).unwrap();
repo.blame("main", "/cluster/name").unwrap();
```

### From Python

```python
from agentstategraph_py import StateGraph

sg = StateGraph("state.db")
sg.set("/name", "prod", "init", category="Checkpoint")
sg.branch("feature")
sg.merge("feature", description="Adopt feature")
sg.blame("/name")  # who changed it and why
```

### From TypeScript, Go, or WASM — all supported.

## Features

- **20 MCP tools** — any agent can connect immediately
- **6 language bindings** — Rust, Python, TypeScript, Go, WASM, C FFI
- **3 storage backends** — Memory, SQLite, IndexedDB (browser)
- **137 tests** across 6 crates
- **Content-addressed Merkle DAG** — immutable, deduplicated history
- **Structured intent metadata** — category, description, tags, reasoning, confidence
- **Authority & delegation chains** — who authorized what, with full chain
- **Schema-aware merge** — CRDT-inspired conflict resolution (sum, max, union-by-id)
- **Speculative execution** — O(1) branching, instant discard
- **Multi-agent orchestration** — scoped sessions, delegation, intent trees
- **Epochs** — sealable, tamper-evident audit bundles
- **Unified query** — composable filters across commits, intents, agents
- **Blame** — who changed what, when, and why
- **Watch/subscribe** — reactive notifications on state changes

## What Makes Every Commit Different from Git

| Field | Question it answers |
|-------|-------------------|
| `state_root` | What changed? |
| `intent` | Why? (structured, queryable) |
| `reasoning` | How did the agent decide? |
| `confidence` | How sure was it? (0.0-1.0) |
| `agent_id` | Who did it? |
| `authority` | Who authorized it? (with delegation chain) |
| `resolution` | What was accomplished? Any deviations? |
| `notification` | Who was informed? |
| `tool_calls` | What actions produced this? |

## Architecture

```
AgentStateGraph/
├── crates/
│   ├── agentstategraph-core/     # Types, diff, merge, schema — zero I/O
│   ├── agentstategraph-storage/  # Pluggable backends (memory, SQLite, IndexedDB)
│   ├── agentstategraph/          # High-level Repository API
│   ├── agentstategraph-mcp/      # MCP server (20 tools over stdio)
│   ├── agentstategraph-ffi/      # C ABI for language bindings
│   └── agentstategraph-wasm/     # Browser/Deno WASM build
├── bindings/
│   ├── python/                   # PyO3 + maturin
│   ├── typescript/               # napi-rs
│   └── go/                       # CGo via FFI
├── spec/
│   └── STATEGRAPH-RFC.md         # Full specification (~2200 lines)
├── examples/                     # 9 reference implementations
└── site/                         # agentstategraph.dev (Astro Starlight)
```

## Reference Implementations

```bash
cargo run --example getting_started -p agentstategraph    # Basic ops
cargo run --example agent_workflow -p agentstategraph     # Speculate, compare, pick winner
cargo run --example multi_agent -p agentstategraph        # Orchestrator + sub-agents
cargo run --example schema_merge -p agentstategraph       # Schema validation + merge
cargo run --example epochs_audit -p agentstategraph       # Epochs, blame, query
python3 examples/python_agent.py                          # Python workflow
node examples/typescript_agent.ts                         # TypeScript workflow
```

## Specification

See [spec/STATEGRAPH-RFC.md](spec/STATEGRAPH-RFC.md) for the complete RFC covering core data model, intent lifecycle, authority/delegation, resolution reporting, sub-agent orchestration, schema system, epochs/registry, MCP interface, and architecture.

## Links

- **Website**: [agentstategraph.dev](https://agentstategraph.dev)
- **Demo app**: [ThreadWeaver](https://github.com/agentstatelabs/ThreadWeaver) — branchable AI chat
- **RFC Spec**: [STATEGRAPH-RFC.md](spec/STATEGRAPH-RFC.md)

## License

MIT OR Apache-2.0
