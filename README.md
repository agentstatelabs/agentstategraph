# AgentStateGraph

> **AgentStateGraph is to agent state what Git was to source code — a content-addressed, branchable, blameable state primitive, designed from the ground up for AI agents as the primary actor.**

**Website:** [agentstategraph.dev](https://agentstategraph.dev)
**Demo app:** [ThreadWeaver](https://github.com/agentstatelabs/ThreadWeaver) — AI chat with branchable conversations, powered by AgentStateGraph
**Disambiguation:** [AgentStateGraph vs. Stategraph vs. LangGraph's StateGraph](site/src/content/docs/compare.md)

## What AgentStateGraph is (and isn't)

AgentStateGraph is **not** a Terraform replacement. The Terraform-replacement space is crowded with evolutionary players, and the actor model is wrong — Terraform assumes humans writing HCL and opening PRs, while AgentStateGraph assumes agents making low-confidence decisions at scale and needing to be held mechanically accountable. AgentStateGraph is **not** a LangGraph helper. LangGraph's `StateGraph` is an in-process Python dict used inside a single agent's execution; AgentStateGraph is a persistent, content-addressed substrate used *between and above* agents. AgentStateGraph is a **state primitive** — the layer on which a next-generation IaC tool, a next-generation GitOps tool, and agent-native ops tooling can all be built. Every change it records carries *why*, *who authorized it*, *what alternatives existed*, and *what the agent expected vs. observed*, across every branch, forever.

This is what the substrate has to look like when the primary actor touching production systems is no longer a human who can be governed socially (via PRs, code review, Slack threads) but a fleet of agents that must be governed mechanically.

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
    "agentstategraph": {
      "command": "/path/to/AgentStateGraph/target/release/agentstategraph-mcp"
    }
  }
}
```

### As an HTTP REST API

```bash
cargo run --release -p agentstategraph-mcp -- --http --port 3001
```

```bash
curl http://localhost:3001/api/health
curl http://localhost:3001/api/stats/main
curl http://localhost:3001/api/state/main?path=/
curl "http://localhost:3001/api/blame/main?path=/cluster/name"
```

19 REST endpoints with CORS enabled — connect from browsers, scripts, or any HTTP client. See `--help` for the full endpoint list.

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
from agentstategraph_py import AgentStateGraph

asg = AgentStateGraph("state.db")
asg.set("/name", "prod", "init", category="Checkpoint")
asg.branch("feature")
asg.merge("feature", description="Adopt feature")
asg.blame("/name")  # who changed it and why
```

### From TypeScript, Go, or WASM — all supported.

## Features

- **26 MCP tools** — any agent can connect immediately
- **19 HTTP REST endpoints** — `--http` mode with CORS for browsers and scripts
- **Browser explorer** — interactive data viewer at [agentstategraph.dev/explorer/](https://agentstategraph.dev/explorer/)
- **6 language bindings** — Rust, Python, TypeScript, Go, WASM, C FFI
- **3 storage backends** — Memory, SQLite, IndexedDB (browser)
- **131 tests** across 6 crates
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
| `confidence` | How sure was it? (0.0–1.0) |
| `agent_id` | Who did it? |
| `authority` | Who authorized it? (with delegation chain) |
| `resolution` | What was accomplished? Any deviations? |
| `notification` | Who was informed? |
| `tool_calls` | What actions produced this? |

## Architecture Eras

AgentStateGraph sits at the bottom of a new architecture era. Prior eras had their own primitives; this one needs its own too.

| Era | Unit of Work | Key Primitives |
|-----|-------------|----------------|
| Monolithic | Function call | OS, filesystem, local DB |
| Batch / Request-Response | Request → Response | HTTP, REST, SQL, queues |
| Streaming | Event | Kafka, Flink, event stores, CQRS |
| **Intent-based** | **Intent → Outcome** | **AgentStateGraph** |

Agents don't execute linear scripts — they explore state spaces. They need a primitive that supports speculative branching, comparison, and merge with full reasoning history. Git is text-oriented. Databases lack branching. Event sourcing is append-only. AgentStateGraph fills the gap.

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
│   └── AGENTSTATEGRAPH-RFC.md    # Full specification (~2300 lines)
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

See [spec/AGENTSTATEGRAPH-RFC.md](spec/AGENTSTATEGRAPH-RFC.md) for the complete RFC covering core data model, intent lifecycle, authority/delegation, resolution reporting, sub-agent orchestration, schema system, epochs/registry, MCP interface, and architecture.

## Links

- **Website**: [agentstategraph.dev](https://agentstategraph.dev)
- **Explorer**: [agentstategraph.dev/explorer/](https://agentstategraph.dev/explorer/) — interactive data viewer
- **Disambiguation**: [AgentStateGraph vs. Stategraph vs. LangGraph's StateGraph](https://agentstategraph.dev/compare/)
- **Demo app**: [ThreadWeaver](https://github.com/agentstatelabs/ThreadWeaver) — branchable AI chat
- **RFC Spec**: [AGENTSTATEGRAPH-RFC.md](spec/AGENTSTATEGRAPH-RFC.md)

## License — Why BSL 1.1?

AgentStateGraph is a state primitive designed to become infrastructure. Infrastructure primitives are strip-mining targets: cloud providers offer them as managed services, capture the value, and contribute nothing back. BSL 1.1 closes that gap.

**What this means in practice:**

- **Individuals, startups, and enterprises** using AgentStateGraph internally — including in production — are **unaffected**.
- The restriction covers **one specific case**: offering AgentStateGraph as a hosted or managed service to third parties without a commercial agreement.
- After **four years**, each version converts to **Apache 2.0 permanently**, with no conditions. This is a binding commitment, not a marketing promise.

**Why not MIT from day one?** Because the project wouldn't survive it. An MIT-licensed infrastructure primitive that gains traction gets absorbed by a hyperscaler within 18 months. BSL 1.1 lets the project grow, stay independent, and convert to a fully permissive license once it's established enough that strip-mining is no longer an existential threat.

This is the same reasoning MongoDB, Elastic, and MariaDB used — with one difference: we committed to the conversion date upfront.

See [LICENSE](LICENSE) and [LICENSING.md](LICENSING.md) for the full terms and plain-English FAQ.
