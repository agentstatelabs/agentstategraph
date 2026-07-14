# The Missing Primitive for AI Agent Infrastructure

*Why git, databases, and event sourcing aren't enough — and what we built instead.*

---

Software architecture has evolved through distinct eras, each defined by its fundamental unit of work:

| Era | Unit of Work | Key Primitives |
|-----|-------------|----------------|
| Monolithic | Function call | OS, filesystem, local DB |
| Batch / Request-Response | Request → Response | HTTP, REST, SQL, queues |
| Streaming | Event | Kafka, Flink, event stores, CQRS |
| **Intent-based** | **Intent → Outcome** | **???** |

We're entering the intent-based era. Users and systems increasingly express desired outcomes — "make the cluster resilient," "optimize the training pipeline," "balance the budget" — rather than imperative steps. AI agents decompose these intents into actions, explore approaches, and converge on solutions.

**The infrastructure for this era doesn't exist yet.**

Current agent tooling wraps existing tools behind AI-friendly interfaces. Agents call REST APIs, read files, use git. That works, like implementing streaming on top of batch infrastructure. But you're fighting the abstraction.

## What Agents Actually Need

Agents don't execute linear scripts. They explore state spaces. An agent asked to "set up a cluster for ML training" doesn't run 12 commands in sequence. It:

1. **Branches** to try different storage approaches (NFS vs. Ceph vs. local SSD)
2. **Compares** the outcomes of each approach
3. **Picks the winner** based on reasoning (Ceph needs 3+ nodes, only 2 available)
4. **Reports back** with what was done, what was tried, what deviated from plan
5. **Records** the full reasoning chain for anyone who asks "why?"

None of the existing tools support this workflow natively:

- **Git**: Text-oriented diffs. No intent metadata. Merge strategies don't understand structured data. Not designed for programmatic-speed branching.
- **Databases**: No branching. No persistent alternatives. Transactions are commit/rollback, not explore/compare/merge.
- **Event sourcing**: Append-only. No branching exploration. Events are facts, not intents.
- **Redux / state managers**: Linear undo stack, not a DAG. Framework-specific. No multi-agent support.

## The Trust Problem

There's a deeper issue. Intent-based systems have a fundamental trust problem from two directions:

**Humans are bad at provenance.** We flatten complex causal chains into blame. "Billy clicked the deploy button" obscures the state he was working from, the alternatives he considered, and the authorization chain that allowed the action.

**AI systems lack provenance.** When an agent reconfigures infrastructure, the reasoning is trapped in an ephemeral conversation context. "The AI did it" is not an acceptable audit trail.

Without provenance, trust in AI systems requires faith. Faith doesn't survive the first incident.

## What We Built: AgentStateGraph

AgentStateGraph is a content-addressed, versioned, branchable structured state store designed as an infrastructure primitive for intent-based systems.

Every state change captures the **full provenance chain**:

| Field | Question it answers |
|-------|-------------------|
| `state_root` | What changed? |
| `intent` | Why? (structured, queryable) |
| `reasoning` | How did the agent decide? |
| `confidence` | How sure was it? |
| `agent_id` | Who did it? |
| `authority` | Who authorized it? (with delegation chain) |
| `resolution` | What was accomplished? Any deviations? |
| `notification` | Who was informed? |

### Built for the Orchestrator Pattern

Today's agent systems are mostly single-agent. But the architecture is shifting. The emerging pattern is the **orchestrator model**: a lead agent decomposes a complex intent into sub-tasks, delegates to specialist agents, monitors progress, and synthesizes results.

A single user intent can spider out into dozens of agent sessions, hundreds of tool calls, and multiple layers of delegation. Without something like AgentStateGraph, that entire execution tree evaporates when the conversation ends.

AgentStateGraph captures the tree: intent decomposition, delegation chains, per-agent branches with scoped state changes, and structured resolutions reporting back up the chain.

### Schema-Aware Merge

When multiple agents work on the same state concurrently, conflicts are inevitable in text-based systems. AgentStateGraph uses schema annotations to auto-resolve most conflicts:

- Both agents add nodes? → `union-by-id` merges by record key
- Both increment a counter? → `sum` adds the deltas
- Both modify different keys? → union both changes
- Same scalar, different values? → conflict (with suggested resolution)

### Speculative Execution

Agents explore by branching. AgentStateGraph makes this a first-class primitive:

```
spec_a = speculate("main")    // try NFS
spec_b = speculate("main")    // try Ceph

// Modify each independently...

compare([spec_a, spec_b])      // structured side-by-side diff
commit(spec_a)                 // pick the winner
discard(spec_b)                // instant cleanup
```

Branch creation is O(1). Discard is instant. This enables agents to explore hundreds of approaches without performance concerns.

### Epochs: Immutable Audit Bundles

For enterprise adoption, work can be grouped into **epochs** — bounded, sealable units with a Merkle root hash. A sealed epoch is cryptographically tamper-evident and exportable as a self-contained audit bundle. Compliance teams can independently verify the record.

## Try It Now

AgentStateGraph is source-available under [BSL 1.1](https://github.com/agentstatelabs/agentstategraph/blob/main/LICENSE) (free for all production use; converts to Apache 2.0 after four years) and available today:

**As an MCP server** (connect to Claude, GPT, or any MCP agent):
```bash
cargo run -p agentstategraph-mcp
```

**As a Rust library**:
```rust
let repo = Repository::new(Box::new(SqliteStorage::open("state.db")?));
repo.init()?;
repo.set("main", "/cluster/name", &Object::string("prod"),
    CommitOptions::new("agent/setup", IntentCategory::Checkpoint, "init"));
```

**From Python**:
```python
from agentstategraph_py import AgentStateGraph
sg = AgentStateGraph("state.db")
sg.set("/name", "prod", "init", category="Checkpoint")
```

**From TypeScript, Go, or WASM** — all supported.

20 MCP tools. 137 tests. 6 reference implementations. Full RFC specification.

**GitHub**: https://github.com/agentstatelabs/agentstategraph

## What's Next

- **AgentStateGraph Chat**: An LLM-agnostic chat app with branchable conversations, built on AgentStateGraph. The visual proof-of-concept.
- **Schema merge in the engine**: Wire the CRDT-inspired merge hints into the merge engine for fully automatic conflict resolution.
- **Conformance test suite**: Any implementation that passes the suite is spec-compliant.

AgentStateGraph is one of the missing infrastructure primitives for the intent-based era. If you're building with AI agents and feel the gap, come build with us.

---

*Craig Brown — [GitHub](https://github.com/agentstatelabs/agentstategraph)*
