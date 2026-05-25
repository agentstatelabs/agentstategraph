# AgentStateGraph

> **AgentStateGraph is to agent state what Git was to source code — a content-addressed, branchable, blameable state primitive, designed from the ground up for AI agents as the primary actor.**

**Website:** [agentstategraph.dev](https://agentstategraph.dev)
**Demo app:** [ThreadWeaver](https://github.com/agentstatelabs/ThreadWeaver) — AI chat with branchable conversations, powered by AgentStateGraph
**Disambiguation:** [AgentStateGraph vs. Stategraph vs. LangGraph's StateGraph](site/src/content/docs/compare.md)

## What AgentStateGraph is (and isn't)

AgentStateGraph is **not** a Terraform replacement. The Terraform-replacement space is crowded with evolutionary players, and the actor model is wrong — Terraform assumes humans writing HCL and opening PRs, while AgentStateGraph assumes agents making low-confidence decisions at scale and needing to be held mechanically accountable. AgentStateGraph is **not** a LangGraph helper. LangGraph's `StateGraph` is an in-process Python dict used inside a single agent's execution; AgentStateGraph is a persistent, content-addressed substrate used *between and above* agents. AgentStateGraph is a **state primitive** — the layer on which a next-generation IaC tool, a next-generation GitOps tool, and agent-native ops tooling can all be built. Every change it records carries *why*, *who authorized it*, *what alternatives existed*, and *what the agent expected vs. observed*, across every branch, forever.

This is what the substrate has to look like when the primary actor touching production systems is no longer a human who can be governed socially (via PRs, code review, Slack threads) but a fleet of agents that must be governed mechanically.

## Quick Start

### One-line install (Mac / Linux)

```bash
curl -sSL https://agentstategraph.dev/install.sh | sh
```

Detects your platform, downloads the prebuilt binary, installs to `~/.local/bin`. Then:

```bash
agentstategraph-mcp --http --port 3001
curl http://localhost:3001/api/health
```

### Docker

```bash
docker run -p 3001:3001 ghcr.io/agentstatelabs/agentstategraph --http
# Or with persistent storage:
docker compose up -d
```

### As an MCP Server (connect to Claude Code, GPT, any MCP agent)

```bash
# If installed via the install script:
agentstategraph-mcp

# Or build from source:
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

22 REST endpoints with CORS enabled — connect from browsers, scripts, or any HTTP client. See `--help` for the full endpoint list.

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

- **73 MCP tools** — any agent can connect immediately
- **HTTP REST API** — `--http` mode with CORS for browsers and scripts
- **Browser explorer** — interactive data viewer at [agentstategraph.dev/explorer/](https://agentstategraph.dev/explorer/)
- **6 language bindings** — Rust, Python, TypeScript, Go, WASM, C FFI
- **4 storage backends** — Memory, SQLite, Postgres (multi-tenant), IndexedDB (browser)
- **14 crates** — modular core, storage, MCP, policy, taint, tasks, reminders, and bindings
- **Namespaces** — ref-layer isolation for multi-project / multi-tenant deployments
- **Reminders** — pull-based scheduling with priority, recurrence, and approval gating
- **Taint & quarantine** — `agentstategraph-taint` mark-and-sweep enforced at commit time
- **Content-addressed Merkle DAG** — immutable, deduplicated history
- **Structured intent metadata** — category, description, tags, reasoning, confidence
- **19 intent categories** — Explore, Refine, Fix, Rollback, Checkpoint, Merge, Migrate, Plan, Taint, Untaint, Quarantine, Unquarantine, Watch, Unwatch, PolicyPropose, PolicyRatify, PolicySupersede, PolicySign, plus Custom
- **Authority & delegation chains** — who authorized what, with full chain
- **Schema-aware merge** — CRDT-inspired conflict resolution (sum, max, union-by-id)
- **Speculative execution** — O(1) branching, instant discard
- **Multi-agent orchestration** — scoped sessions, delegation, intent trees
- **Plans & tasks** — shared `agentstategraph-tasks` primitive with state machine, proofs, blockers, agent assignment
- **Policy** — `agentstategraph-policy` primitive for authorization + cost-of-change gating with fallback actions (soft enforcement + audit trail)
- **Schema-evolution framework** — `/_meta/schema_version` guard + `agentstategraph-migrate` crate + `agentstategraph-mcp migrate` CLI
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
│   ├── agentstategraph-mcp/      # MCP server (73 tools over stdio) + HTTP + migrate CLI
│   ├── agentstategraph-tasks/    # Shared Plan/Task store — state machine, proofs, assignment
│   ├── agentstategraph-policy/   # Authorization + cost-of-change gating with fallback actions
│   ├── agentstategraph-policy-sign/ # Ed25519 signing for policy ratification
│   ├── agentstategraph-policy-wasm/ # WASM host runner for policy evaluation (stub)
│   ├── agentstategraph-taint/    # Taint/quarantine/watch mark-and-sweep primitive
│   ├── agentstategraph-migrate/  # Schema-evolution framework + migration registry
│   ├── agentstategraph-ffi/      # C ABI for language bindings
│   └── agentstategraph-wasm/     # Browser/Deno WASM build
├── bindings/
│   ├── python/                   # PyO3 + maturin
│   ├── typescript/               # napi-rs
│   └── go/                       # CGo via FFI
├── spec/
│   ├── AGENTSTATEGRAPH-RFC.md    # Full specification (~2300 lines)
│   ├── UPGRADE-PATH.md           # Schema versioning + migration design
│   └── SECURITY-THREAT-MODEL.md
├── examples/                     # reference implementations + feature walkthroughs
└── site/                         # agentstategraph.dev (Astro Starlight)
```

## Plans & Tasks

`agentstategraph-tasks` is an opinionated sibling crate that layers a shared plan / task model on top of the raw state graph so multiple consumers (CTXone, ThreadWeaver, future apps) don't each reimplement `Task` independently.

- `Plan` → `Task[]` with a strict state machine: `pending → in_progress → done`.
- `Task::assigned_to` for agent assignment, plus `TaskStore::assign_task`, `unassign_task`, `next_task_for(agent)`.
- `list_plans_by_status(status)` for native status filtering.
- `Proof` and `Verifier` trait — completion of a task must produce verifiable evidence before it can transition to `done`.
- `Repository::spec_set_json` on the high-level API supports atomic multi-path plan/task commits.
- Plan-related writes are natively filterable via `IntentCategory::Plan` in log and blame queries.

The crate is optional — ignore it if your agents don't need a shared plan primitive. If they do, `use agentstategraph_tasks::*;` and you inherit the state machine.

## Upgrade path

ASG databases have a schema version that lives in-band at `/_meta/schema_version`. Migrations are regular commits on `main` with `IntentCategory::Migrate`, so upgrade history shows up in `log` and `blame` for free.

- **Guarded reserved path:** `/_meta/*` writes require `IntentCategory::Migrate` — accidental overwrites from app code fail with `RepoError::ReservedPath`.
- **`/_meta/_secret/*` sub-prefix** is additionally gated on reads via `Repository::get_with_intent`, and silently filtered out of `list_paths` / `search_values`.
- **`Repository::init()` stamps the current schema version** on a new database. An older database missing the key is treated as implicit "version 0" and upgraded by the first migration that runs.
- **`agentstategraph-migrate` crate** provides a `Migration` trait, a `Registry`, a `check()` function for startup introspection (`UpToDate` / `UpgradeAvailable` / `Downgrade` / `Unversioned` / `Corrupt`), and a `Runner` with `DryRun` and `Apply` modes.
- **`agentstategraph-mcp migrate` CLI** is a one-shot maintenance command that refuses to start the MCP/HTTP surface — run it, let it report `DryRun` output, then run it again with `--yes`. Exit codes follow the `sysexits.h` spirit (64 / 65 / 70 / 75) so ops tooling can react programmatically.

```bash
# Dry-run against a production database
agentstategraph-mcp migrate --db ./prod.db --dry-run

# Apply
agentstategraph-mcp migrate --db ./prod.db --yes

# Check a specific ref or target version
agentstategraph-mcp migrate --db ./prod.db --ref main --to 0.4.0 --dry-run
```

Full design discussion: [spec/UPGRADE-PATH.md](spec/UPGRADE-PATH.md) — versioning model, migration registry, consumer-side upgrade flow, downgrade / rollback semantics, and the first shipped migration (CTXone's `plan_assignments` sidecar → native `Task.assigned_to`) as a worked example.

## Reference Implementations

```bash
cargo run --example getting_started -p agentstategraph    # Basic ops
cargo run --example agent_workflow -p agentstategraph     # Speculate, compare, pick winner
cargo run --example multi_agent -p agentstategraph        # Orchestrator + sub-agents
cargo run --example schema_merge -p agentstategraph       # Schema validation + merge
cargo run --example epochs_audit -p agentstategraph       # Epochs, blame, query
cargo run --example namespaces -p agentstategraph         # Multi-tenant namespace isolation
python3 examples/python_agent.py                          # Python workflow
node examples/typescript_agent.ts                         # TypeScript workflow
```

MCP tool-call walkthroughs for the newer capabilities (work with any MCP-connected agent):

- [`examples/namespaces-multitenant.md`](examples/namespaces-multitenant.md) — multi-tenant isolation + deny-by-default cross-namespace merge
- [`examples/governance-policy-taint.md`](examples/governance-policy-taint.md) — policy + taint gating a sensitive change
- [`examples/reminders-and-tasks.md`](examples/reminders-and-tasks.md) — pull-based reminders driving a task plan

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
