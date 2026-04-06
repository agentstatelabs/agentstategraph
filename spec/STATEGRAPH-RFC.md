# StateGraph RFC-0001: AI-Native Versioned State Store

```
RFC:         0001
Title:       StateGraph — AI-Native Versioned State Store
Status:      Draft
Created:     2026-04-04
Authors:     Craig Brown
```

---

## Abstract

StateGraph is a content-addressed, versioned, branchable structured state store designed as an infrastructure primitive for intent-based systems. It provides AI agents and AI-native applications with a git-like state management layer that captures not just *what* changed, but *why*, *who authorized it*, *what alternatives were considered*, and *who was informed* — making every state transition auditable, reversible, and explainable.

StateGraph is implemented as an embeddable Rust library with language bindings for Python, TypeScript, and Go, and exposed as a Model Context Protocol (MCP) server for direct agent integration.

---

## 1. Motivation

### 1.1 The Four Architecture Eras

Software architecture has evolved through distinct eras, each defined by its fundamental unit of work and the infrastructure primitives that support it:

| Era | Unit of Work | Key Primitives |
|-----|-------------|----------------|
| **Monolithic** | Function call | OS, filesystem, local database |
| **Batch / Request-Response** | Request → Response | HTTP, REST, SQL, message queues |
| **Streaming** | Event | Kafka, Flink, event stores, CQRS |
| **Intent-based** | Intent → Outcome | *Largely undefined* |

We are entering the intent-based era. Users and systems increasingly express desired outcomes ("make the cluster resilient," "optimize this layout," "balance the budget") rather than imperative steps. AI agents decompose these intents into actions, explore approaches, and converge on solutions.

The infrastructure primitives for this era do not yet exist. Current agent tooling wraps existing tools (git, databases, REST APIs) behind AI-friendly interfaces. This is analogous to implementing streaming architectures on top of batch infrastructure — functional but fighting the abstraction.

### 1.2 The Provenance Gap

Intent-based systems have a fundamental trust problem from two directions:

**Humans are bad at provenance.** We flatten complex causal chains into blame. "Billy clicked the deploy button" obscures the state he was working from, the information he had, the alternatives he considered, and the authorization chain that allowed the action. Every post-mortem is an exercise in reconstructing context that was never captured.

**AI systems lack provenance.** When an agent reconfigures infrastructure or modifies application state, the reasoning is trapped in an ephemeral conversation context. "The AI did it" is not an acceptable audit trail. Without traceability, trust in AI systems requires faith — and faith does not survive the first incident.

### 1.3 The Shift from Single Agent to Orchestrator

Today's agent systems are largely single-agent: one agent receives a task, calls tools, and returns a result. Some systems support sub-agent calls, but the primary agent still does most of the work and the sub-agent relationship is transient — a function call, not a managed delegation.

This is changing fast. The emerging pattern is the **orchestrator model**: a lead agent decomposes a complex intent into sub-tasks, delegates to specialist agents, monitors progress, collects reports, resolves conflicts, and synthesizes results. The primary agent becomes less of a worker and more of a coordinator.

| Phase | Agent Architecture | State Needs |
|-------|-------------------|-------------|
| **Current** | Single agent, linear tool calls | Simple key-value or file-based persistence |
| **Emerging** | Orchestrator + specialist sub-agents | Scoped branches, parallel work, structured reporting, intent decomposition |
| **Future** | Agent networks across orgs and systems | Distributed state, cross-boundary authority delegation, federated merge |

Most current infrastructure (including MCP, agent frameworks, and git-based state) is designed for the single-agent phase. StateGraph is designed for the orchestrator phase and forward — its data model natively represents intent decomposition, agent hierarchies, scoped delegation, parallel sub-agent execution with safe merge, and structured reporting up the chain.

The fan-out problem is significant. An orchestrator delegates to sub-agents, each of which may call multiple tools, MCP servers, and even spawn their own sub-agents. A single user intent can spider out into dozens of agent sessions, hundreds of tool calls, and multiple MCP server interactions across layers:

```
User intent: "Set up the cluster for ML training"
  └─ Orchestrator agent
       ├─ Network sub-agent
       │    ├─ tool: kubectl apply (configure CNI)
       │    ├─ MCP: cloud-provider/vpc (verify subnets)
       │    └─ tool: stategraph_set (commit network state)
       ├─ Storage sub-agent
       │    ├─ MCP: cloud-provider/storage (provision volumes)
       │    ├─ tool: kubectl apply (deploy NFS server)
       │    └─ Sub-sub-agent: storage-validator
       │         ├─ tool: ssh (mount test)
       │         └─ MCP: monitoring/prometheus (verify IOPS)
       └─ GPU scheduling sub-agent
            ├─ MCP: nvidia/dcgm (query GPU health)
            ├─ tool: stategraph_set (commit schedule)
            └─ Sub-sub-agent: benchmarker
                 ├─ tool: kubectl apply (run benchmark pod)
                 └─ MCP: monitoring/prometheus (collect metrics)
```

Today, each branch of this tree is a separate conversation context that evaporates when it completes. The orchestrator gets back a text summary — maybe. There is no unified record of what happened, what was tried, what failed, who authorized what at each level, or how the decisions connect across the tree.

StateGraph captures the entire execution tree: intent decomposition, delegation chains, per-agent branches with scoped state changes, tool call provenance at every level, and structured resolutions reporting back up the chain. When something goes wrong three layers deep, you trace it from root intent through every decision to the specific tool call that failed.

Building for the orchestrator pattern now means teams adopting StateGraph are ready for the architecture that is already arriving, rather than retrofitting single-agent tooling when it breaks under multi-agent coordination.

### 1.4 What Exists Today and Why It Falls Short

| Existing Tool | What It Provides | What It Lacks |
|--------------|-----------------|---------------|
| **Git** | Content-addressed DAG, branching, merging | Text-oriented diffs; no structured data awareness; no intent/reasoning metadata; merge strategies don't understand domain schemas; not designed for programmatic-speed branching |
| **Databases** | Structured data, queries, transactions | No branching; no persistent alternatives; no history DAG; transactions are commit/rollback, not explore/compare/merge |
| **Event Sourcing** | Append-only history, event replay | No branching or speculative exploration; events are facts, not intents; no built-in merge semantics |
| **Redux / State Managers** | Time-travel debugging, undo/redo | Linear stack, not a DAG; framework-specific; no multi-agent support; no persistence; undo/redo is not branching |
| **CRDTs** | Conflict-free concurrent editing | No history; no intent tracking; designed for real-time collaboration, not agent exploration; limited to specific data types |
| **Kubernetes** | Declarative desired state, reconciliation | Domain-specific (containers); no reasoning trace; no branching exploration; no general-purpose state model |

### 1.5 What StateGraph Provides

StateGraph combines the missing pieces into a single primitive:

- **Content-addressed Merkle DAG** for immutable, deduplicated state history (from git)
- **Structured, typed state** with schema-aware operations (from databases)
- **Intent, reasoning, and authority as first-class metadata** on every state transition (new)
- **Intent lifecycle with resolution reporting** — authorize, execute, report back (new)
- **Schema-aware merge** with CRDT-inspired conflict resolution annotations (from CRDTs, applied to version control)
- **Speculative execution** as a first-class primitive — branch, explore, compare, merge or discard at programmatic speed (new)
- **Multi-agent coordination** with optimistic concurrency, sessions, and merge proposals (new)
- **Notification policy** as part of the provenance record (new)

---

## 2. Glossary

Precise definitions for all terms used in this specification. Agents and humans should reference this section when interpreting the spec.

| Term | Definition |
|------|-----------|
| **Atom** | A leaf value in the state tree: null, boolean, integer, float, string, or bytes. Atoms are immutable and content-addressed. |
| **Node** | A container value in the state tree: Map (string-keyed), List (ordered), or Set (unordered, unique). Nodes reference children by ObjectId. |
| **Object** | Either an Atom or a Node. The fundamental unit of state storage. |
| **ObjectId** | A BLAKE3 hash of an Object's canonical serialization. Serves as the object's unique identifier and content address. Two objects with identical content always produce the same ObjectId. |
| **State Tree** | A Merkle DAG of Objects rooted at a single Node. Represents the complete application state at a point in time. |
| **Commit** | An immutable record linking a state tree root to its parent commit(s), along with metadata: agent identity, authority, intent, reasoning, confidence, and timestamp. |
| **Intent** | A structured declaration of *why* a state change is being made. Includes a category, description, tags, lifecycle status, and optional resolution report. |
| **Authority** | A record of *who authorized* a state change, including the principal, scope of authorization, expiration, and delegation chain. |
| **Resolution** | A structured report filed when an intent is fulfilled, partially fulfilled, failed, or deferred. Includes a summary, deviations from plan, commits made, branches explored, outcome status, and confidence score. |
| **Deviation** | A record within a Resolution describing where and why the agent diverged from the original plan. |
| **NotificationPolicy** | A declaration of who should be informed about an intent's resolution, at what urgency, and in what format. Part of the provenance record. |
| **Ref** | A named pointer to a CommitId. Branches are mutable refs; tags are immutable refs. |
| **Branch** | A mutable ref that advances as new commits are made. Supports namespacing (e.g., `agents/planner/workspace`). |
| **Tag** | An immutable ref that permanently points to a specific commit. Used for marking significant states. |
| **HEAD** | The current commit a session is working from. Each agent session has its own HEAD, unlike git's single HEAD. |
| **Session** | A working context for an agent, consisting of an agent identity, a working branch, and a HEAD pointer. Multiple sessions can operate concurrently on the same state store. |
| **Speculation** | A lightweight, disposable branch used for speculative execution. Optimized for O(1) creation and instant discard via copy-on-write and arena allocation. |
| **MergeProposal** | A request to merge changes from one branch into another, subject to review and approval by an authorized principal. |
| **DiffOp** | A single structured change operation within a diff. Schema-aware and typed (not a text line diff). |
| **CAS** | Compare-and-swap. The atomic concurrency primitive: update a ref only if it still points to the expected commit. |
| **Principal** | An identity (human user, agent, team, or system policy) that can authorize actions or receive notifications. |
| **DelegationLink** | A single hop in a delegation chain: principal A authorized principal B, with scope and timestamp. |

---

## 3. Core Data Model

### 3.1 Objects

All state in StateGraph is composed of Objects. Every Object is individually content-addressed via BLAKE3 hash of its canonical serialization.

#### 3.1.1 Atoms (Leaf Values)

```
Atom = Null
     | Bool(bool)
     | Int(i64)
     | Float(f64)
     | String(UTF-8 string)
     | Bytes(byte array)
```

#### 3.1.2 Nodes (Containers)

```
Node = Map(BTreeMap<String, ObjectId>)
     | List(Vec<ObjectId>)
     | Set(BTreeSet<ObjectId>)
```

Nodes reference children by ObjectId, forming a Merkle DAG. Identical subtrees at any depth are automatically deduplicated — if two branches share the same sub-state, only one copy is stored.

#### 3.1.3 ObjectId

```
ObjectId = BLAKE3(canonical_serialize(Object))
```

The canonical serialization format is deterministic: map keys are sorted lexicographically, sets are sorted by ObjectId, and all values use a fixed-width encoding for numeric types. This ensures that identical logical state always produces the same ObjectId regardless of insertion order.

### 3.2 Commits

A Commit is an immutable record that links a state tree to its history and metadata.

```
Commit {
  id:           ObjectId          // BLAKE3 hash of all fields below
  state_root:   ObjectId          // root of the state tree at this commit
  parents:      Vec<ObjectId>     // 0 = initial commit, 1 = normal, 2+ = merge
  timestamp:    DateTime<Utc>

  // Identity: who performed this action
  agent_id:     String            // "user", "agent/planner-v2", "system/scheduler"

  // Authority: who authorized this action
  authority:    Authority

  // Intent: why this action was taken
  intent:       Intent

  // Reasoning: how the agent decided on this approach
  reasoning:    Option<String>    // agent's chain-of-thought or explanation
  confidence:   Option<f64>       // 0.0 to 1.0, agent's self-assessed confidence

  // Provenance: what tool calls produced this state change
  tool_calls:   Vec<ToolCall>
}
```

#### 3.2.1 Authority

```
Authority {
  principal:         String                // "user", "ops-team", "policy/auto-scale"
  scope:             AuthScope             // what was authorized
  granted_at:        DateTime<Utc>
  expires:           Option<DateTime<Utc>>
  delegation_chain:  Vec<DelegationLink>   // full authorization path
}

AuthScope = IntentScope(IntentId)          // authorized for a specific intent
          | BranchScope(BranchPattern)     // authorized for branches matching pattern
          | Wildcard                        // full access (e.g., admin)
          | Custom(String)                 // app-defined scope

DelegationLink {
  from:       String            // delegating principal
  to:         String            // receiving principal
  scope:      AuthScope         // what was delegated
  granted_at: DateTime<Utc>
  expires:    Option<DateTime<Utc>>
}
```

**Example delegation chain**: An admin policy authorizes the ops team; the ops team authorizes user user; user authorizes the planner agent for a specific intent:

```json
{
  "principal": "agent/planner-v2",
  "scope": { "intent": "intent-00a1" },
  "delegation_chain": [
    { "from": "policy/production-access", "to": "team/ops", "scope": "branch:production/*" },
    { "from": "team/ops", "to": "user", "scope": "branch:production/*" },
    { "from": "user", "to": "agent/planner-v2", "scope": { "intent": "intent-00a1" } }
  ]
}
```

#### 3.2.2 Intent

```
Intent {
  id:              IntentId           // unique identifier
  category:        IntentCategory
  description:     String             // human/agent-readable description
  tags:            Vec<String>        // queryable labels
  parent_intent:   Option<IntentId>   // threading: this intent was decomposed from a parent
  lifecycle:       IntentLifecycle
}

IntentCategory = Explore              // trying an approach to evaluate it
               | Refine              // improving on a previous state
               | Fix                 // correcting an error or regression
               | Rollback            // reverting to a prior state
               | Checkpoint          // saving a known-good state
               | Merge              // combining work from branches
               | Migrate            // schema or structural change
               | Custom(String)     // application-defined category
```

#### 3.2.3 Intent Lifecycle

The intent lifecycle tracks the full arc from proposal through resolution and notification.

```
IntentLifecycle {
  status:       IntentStatus
  assigned_to:  Vec<String>               // agent(s) working on this intent
  resolution:   Option<Resolution>
  notification: Option<NotificationPolicy>
}

IntentStatus = Proposed                   // intent has been declared
             | Authorized                 // authority has approved execution
             | InProgress                 // agent(s) are actively working
             | Completed                  // work is done, resolution filed
             | Failed                     // agent could not fulfill the intent
             | Blocked                    // waiting on external dependency
```

**State machine** (valid transitions):

```
Proposed ──→ Authorized ──→ InProgress ──→ Completed
                │                │    ──→ Failed
                │                └────→ Blocked ──→ InProgress
                └──→ (rejected, no commit)                  ──→ Failed
```

#### 3.2.4 Resolution

Filed when an intent reaches Completed, Failed, or a terminal state. This is the "report back."

```
Resolution {
  summary:             String             // concise description of what was accomplished
  deviations:          Vec<Deviation>     // where/why the agent diverged from plan
  commits:             Vec<ObjectId>      // commit IDs of state changes made
  branches_explored:   Vec<String>        // branches created during exploration
  outcome:             Outcome
  confidence:          f64                // 0.0 to 1.0
}

Outcome = Fulfilled                       // intent fully satisfied
        | PartiallyFulfilled             // some aspects completed, others remain
        | Failed                         // could not satisfy the intent
        | Deferred                       // punted to a follow-up intent

Deviation {
  description:    String                  // what was different from the plan
  reason:         String                  // why the deviation occurred
  impact:         Low | Medium | High     // severity of the deviation
  follow_up:      Option<IntentId>        // optional follow-up intent created
}
```

#### 3.2.5 Notification Policy

```
NotificationPolicy {
  urgency:      Urgency
  audience:     Vec<String>       // principals who should be informed
  format_hint:  FormatHint
}

Urgency    = Routine | Priority | Critical
FormatHint = Summary | Detailed | DiffOnly
```

StateGraph does not deliver notifications directly. The notification policy is stored as part of the provenance record and emitted as a structured event that integration layers (Slack bots, email systems, dashboards) can subscribe to and act on.

#### 3.2.6 ToolCall

```
ToolCall {
  tool_name:  String              // "kubectl_apply", "stategraph_set", etc.
  arguments:  Map<String, Value>  // input arguments
  result:     Option<String>      // summary of result (not full output)
  timestamp:  DateTime<Utc>
}
```

### 3.3 Refs

Refs are named pointers to commits.

```
Branch {
  name:    String       // e.g., "main", "agents/planner/workspace", "explore/ceph-vs-nfs"
  target:  ObjectId     // commit ID
}

Tag {
  name:    String       // e.g., "v1.0", "approved-march-budget", "pre-migration"
  target:  ObjectId     // commit ID (immutable once created)
}
```

**Namespace conventions**:
- `main` — the primary shared state
- `agents/{agent_id}/workspace` — per-agent working branches
- `explore/{description}` — speculative exploration branches
- `proposals/{id}` — branches backing merge proposals

### 3.4 State Addressing

State within a tree is addressed using JSON-path-like expressions:

```
/                          → root node
/nodes                     → the "nodes" key in the root map
/nodes/0                   → first element of the "nodes" list
/nodes/0/hostname          → "hostname" key in the first node object
/config/network/subnet     → nested map traversal
```

Path addressing works at any ref:

```
get("main", "/nodes/0/hostname")                    → "jetson-01"
get("agents/planner/workspace", "/nodes/0/hostname") → "jetson-01-renamed"
get("abc123", "/nodes/0/hostname")                   → value at specific commit
```

---

## 4. Operations

### 4.1 State Operations

All write operations are atomic commits. There is no staging area.

| Operation | Signature | Description |
|-----------|-----------|-------------|
| `get` | `(ref: Ref, path: Path) → Value` | Read a value from state at any ref |
| `set` | `(ref: Ref, path: Path, value: Value, intent: Intent) → CommitId` | Write a value atomically, creating a new commit |
| `delete` | `(ref: Ref, path: Path, intent: Intent) → CommitId` | Remove a value atomically |
| `query` | `(ref: Ref, expression: String) → Vec<Value>` | Query state using JSONPath expressions |
| `exists` | `(ref: Ref, path: Path) → bool` | Check if a path exists in state |
| `snapshot` | `(ref: Ref) → Value` | Return the full state tree as a JSON-like value |

**Example** — setting a value:

```json
// Request
{
  "operation": "set",
  "ref": "main",
  "path": "/nodes/0/status",
  "value": "healthy",
  "intent": {
    "category": "Fix",
    "description": "Mark node as healthy after health check passed",
    "tags": ["health-check", "node-0"]
  },
  "agent_id": "agent/health-monitor",
  "authority": {
    "principal": "policy/health-monitoring",
    "scope": "branch:main"
  }
}

// Response
{
  "commit_id": "sg_7f3a...",
  "ref": "main",
  "parent": "sg_4e2b...",
  "timestamp": "2026-04-04T14:30:00Z"
}
```

### 4.2 Branch Operations

| Operation | Signature | Description |
|-----------|-----------|-------------|
| `branch` | `(name: String, from: Ref) → Branch` | Create a new branch from any ref |
| `delete_branch` | `(name: String) → ()` | Delete a branch (commits remain in DAG) |
| `list_branches` | `(prefix: Option<String>) → Vec<Branch>` | List branches, optionally filtered by namespace |
| `merge` | `(source: Ref, target: Ref, strategy: MergeStrategy) → MergeResult` | Merge source into target |
| `rebase` | `(branch: Ref, onto: Ref) → RebaseResult` | Replay branch commits onto a new base |

**MergeResult**:

```
MergeResult = Success { commit_id: ObjectId }
            | Conflicts { conflicts: Vec<Conflict>, partial_commit: ObjectId }

Conflict {
  path:                  Path
  ours:                  Option<Value>
  theirs:                Option<Value>
  base:                  Option<Value>     // common ancestor value
  suggested_resolution:  Option<Value>     // AI-assisted suggestion
  resolution_strategy:   String            // which merge hint applied (or "manual")
}
```

### 4.3 Diff Operations

Diffs are structured and schema-aware. They operate on typed values, not text lines.

```
diff(ref_a: Ref, ref_b: Ref) → Vec<DiffOp>

DiffOp = SetValue    { path: Path, old: Value, new: Value }
       | AddKey      { path: Path, key: String, value: Value }
       | RemoveKey   { path: Path, key: String, old_value: Value }
       | AddElement  { path: Path, index: usize, value: Value }
       | RemoveElement { path: Path, index: usize, old_value: Value }
       | MoveElement { path: Path, from_index: usize, to_index: usize }
       | AddToSet    { path: Path, value: Value }
       | RemoveFromSet { path: Path, value: Value }
       | ChangeType  { path: Path, old_type: String, new_type: String }
```

**Example** — diff between two commits:

```json
[
  { "op": "SetValue", "path": "/nodes/0/status", "old": "unhealthy", "new": "healthy" },
  { "op": "AddKey", "path": "/nodes/0", "key": "last_health_check", "value": "2026-04-04T14:30:00Z" },
  { "op": "RemoveFromSet", "path": "/alerts/active", "value": "node-0-down" }
]
```

### 4.4 Unified Query Interface

StateGraph stores rich, multi-dimensional data: state values, commit history, intent metadata, authority chains, reasoning traces, agent sessions, and epoch records. Rather than requiring agents to learn many specialized query operations, StateGraph provides a **single unified query interface** that can answer questions across all dimensions.

#### 4.4.1 Design Principles

1. **One tool, not eight.** An agent shouldn't need to decide between `log_by_intent`, `log_by_agent`, `log_by_authority`, and `search_reasoning`. One `query` operation with composable filters handles all cases.
2. **Natural mapping.** The query parameters map directly to the questions agents and humans ask: "what changed?" (target: commits), "what's the current state?" (target: state), "what was decided?" (target: intents).
3. **Composable filters.** All filters are optional and combined with AND. Simple queries use one or two filters. Complex queries combine many.
4. **Temporal by default.** Any query can include a time dimension — "at this point in time" or "during this date range."

#### 4.4.2 Query Structure

```
query(
  // What to query — the primary dimension
  target:  "state" | "commits" | "intents" | "agents" | "epochs"

  // Context — where and when to look
  ref:     Option<Ref>                    // branch, tag, or commit (default: "main")
  at_time: Option<DateTime>              // temporal: state/commits as of this time
  epoch:   Option<String>                // scope query to a specific epoch

  // Filters — all optional, combined with AND
  filters: {
    path:                 Option<PathPattern>     // "/nodes/*", "/config/network/**"
    where:                Option<Expression>      // "status == 'unhealthy'" or "gpu_memory_mb > 8000"
    agent_id:             Option<String>          // commits/intents by this agent
    intent_category:      Option<IntentCategory>  // Explore, Fix, Refine, etc.
    tags:                 Option<Vec<String>>     // intent tags
    authority_principal:  Option<String>          // authorized by this principal
    reasoning_contains:   Option<String>          // full-text search in reasoning traces
    confidence_range:     Option<(f64, f64)>      // e.g., (0.0, 0.5) for low-confidence commits
    intent_status:        Option<IntentStatus>    // Completed, Failed, InProgress, etc.
    outcome:              Option<Outcome>         // Fulfilled, PartiallyFulfilled, Failed, Deferred
    date_range:           Option<(DateTime, DateTime)>
    has_deviations:       Option<bool>            // only results where agent deviated from plan
  }

  // Output control
  select:   Option<Vec<String>>     // which fields to return (e.g., ["path", "value", "intent.description"])
  order_by: Option<String>          // "timestamp", "confidence", "agent_id"
  limit:    Option<usize>           // max results (default: 20)
  offset:   Option<usize>           // pagination
)
```

#### 4.4.3 Query Examples

**State queries** — "What is the current state?"

```json
// Simple: get a value
{ "target": "state", "ref": "main", "filters": { "path": "/nodes/0/status" } }

// Search: find all unhealthy nodes
{ "target": "state", "ref": "main",
  "filters": { "path": "/nodes/*", "where": "status == 'unhealthy'" } }

// Temporal: what was the node status yesterday at 2pm?
{ "target": "state", "ref": "main",
  "at_time": "2026-04-03T14:00:00Z",
  "filters": { "path": "/nodes/*/status" } }

// Cross-branch: compare a value across branches
// (run two queries and diff, or use stategraph_diff)
```

**Commit queries** — "What happened?"

```json
// What did agent/planner do today?
{ "target": "commits",
  "filters": { "agent_id": "agent/planner",
               "date_range": ["2026-04-04T00:00:00Z", "2026-04-04T23:59:59Z"] },
  "select": ["intent.description", "reasoning", "confidence"] }

// Show me all low-confidence commits
{ "target": "commits",
  "filters": { "confidence_range": [0.0, 0.5] },
  "order_by": "confidence" }

// What commits mention "memory controller" in reasoning?
{ "target": "commits",
  "filters": { "reasoning_contains": "memory controller" } }

// What did user authorize this week?
{ "target": "commits",
  "filters": { "authority_principal": "user",
               "date_range": ["2026-03-29", "2026-04-04"] } }
```

**Intent queries** — "What was decided and why?"

```json
// All storage-related explorations
{ "target": "intents",
  "filters": { "intent_category": "Explore", "tags": ["storage"] } }

// All failed intents (what didn't work?)
{ "target": "intents",
  "filters": { "outcome": "Failed" },
  "select": ["description", "resolution.summary", "resolution.deviations"] }

// All intents with deviations (where did agents go off-plan?)
{ "target": "intents",
  "filters": { "has_deviations": true },
  "select": ["description", "agent_id", "resolution.deviations"] }

// What is agent/gpu-scheduler currently working on?
{ "target": "intents",
  "filters": { "agent_id": "agent/gpu-scheduler", "intent_status": "InProgress" } }
```

**Epoch queries** — "What bodies of work have been done?"

```json
// All epochs involving storage
{ "target": "epochs",
  "filters": { "tags": ["storage"] } }

// All sealed epochs from Q1 2026
{ "target": "epochs",
  "filters": { "intent_status": "Sealed",
               "date_range": ["2026-01-01", "2026-03-31"] } }
```

**Aggregate queries** — "Give me a summary"

```json
// How many commits per agent this week?
{ "target": "commits",
  "filters": { "date_range": ["2026-03-29", "2026-04-04"] },
  "group_by": "agent_id",
  "select": ["agent_id", "count"] }

// Intent outcomes by category (how often do explorations succeed?)
{ "target": "intents",
  "filters": { "intent_status": "Completed" },
  "group_by": "intent_category",
  "select": ["intent_category", "outcome", "count"] }
```

#### 4.4.4 Specialized Query Operations

Some queries require their own operations because they involve graph traversal or binary search rather than filtering:

| Operation | Signature | Description |
|-----------|-----------|-------------|
| `bisect` | `(good: Ref, bad: Ref, predicate: Predicate) → CommitId` | Binary search the commit DAG to find where a condition changed |
| `intent_tree` | `(intent_id: IntentId) → IntentTree` | Return the full decomposition tree of an intent and all sub-intents |
| `diff` | `(ref_a: Ref, ref_b: Ref) → Vec<DiffOp>` | Structured diff between two refs (defined in Section 4.3) |
| `blame` | `(ref: Ref, path: Path) → Vec<BlameEntry>` | For each field at the path, which commit last modified it and why |

**Predicate** for bisect:

```
Predicate = PathEquals(path: Path, value: Value)
          | PathExists(path: Path)
          | Expression(jsonpath: String, expected: Value)
          | Custom(fn(state: Value) -> bool)
```

**BlameEntry** — extends git blame with intent and reasoning:

```
BlameEntry {
  path:        Path
  value:       Value
  commit_id:   ObjectId
  agent_id:    String
  intent:      Intent          // why this value was set
  reasoning:   Option<String>  // the agent's reasoning at the time
  timestamp:   DateTime<Utc>
}
```

**Example** — blame on a field:

```json
blame(ref="main", path="/nodes/2/status")

// Returns:
{
  "path": "/nodes/2/status",
  "value": "draining",
  "commit_id": "sg_7f3a...",
  "agent_id": "agent/gpu-scheduler",
  "intent": {
    "category": "Fix",
    "description": "Drain node 2 due to detected GPU thermal throttling"
  },
  "reasoning": "Node 2 GPU temperature at 94C, exceeding 90C threshold. Draining workloads to prevent hardware damage.",
  "timestamp": "2026-04-04T15:30:00Z"
}

### 4.5 Speculative Execution

Speculation is a first-class primitive for the "try many approaches, compare, keep the winner" pattern.

| Operation | Signature | Description |
|-----------|-----------|-------------|
| `speculate` | `(from: Ref) → SpecHandle` | Create a lightweight speculation. O(1) via copy-on-write. |
| `spec_set` | `(handle: SpecHandle, path: Path, value: Value) → ()` | Modify state within a speculation (no commit yet) |
| `spec_get` | `(handle: SpecHandle, path: Path) → Value` | Read state within a speculation |
| `compare_speculations` | `(handles: Vec<SpecHandle>) → Comparison` | Compare state across multiple speculations |
| `commit_speculation` | `(handle: SpecHandle, intent: Intent) → CommitId` | Promote a speculation to a real commit |
| `discard` | `(handle: SpecHandle) → ()` | Discard a speculation. Instant cleanup via arena allocation. |

**Example** — agent explores two storage approaches:

```json
// Create two speculations from the same base
spec_a = speculate("main")           // try NFS
spec_b = speculate("main")           // try Ceph

// Agent modifies state in each
spec_set(spec_a, "/storage/type", "nfs")
spec_set(spec_a, "/storage/mount", "/shared/nfs")

spec_set(spec_b, "/storage/type", "ceph")
spec_set(spec_b, "/storage/replicas", 3)

// Compare
comparison = compare_speculations([spec_a, spec_b])
// Returns structured diff showing how spec_a and spec_b diverge

// Agent picks NFS (Ceph needs more nodes)
commit_id = commit_speculation(spec_a, {
  "category": "Refine",
  "description": "Selected NFS over Ceph — Ceph requires 3+ nodes, only 2 available",
  "tags": ["storage", "nfs"]
})

// Discard the losing speculation
discard(spec_b)
```

### 4.6 Watch / Subscribe

Reactive agents can subscribe to state changes by path pattern.

```
watch(pattern: PathPattern, callback: Fn(WatchEvent)) → Subscription

WatchEvent {
  commit_id:  ObjectId
  path:       Path
  old_value:  Option<Value>
  new_value:  Option<Value>
  intent:     Intent
  agent_id:   String
}

PathPattern = Exact("/nodes/0/status")
            | Prefix("/nodes/")
            | Glob("/nodes/*/status")
            | All
```

---

## 5. Multi-Agent Coordination

### 5.1 Concurrency Model

StateGraph uses **optimistic concurrency** with compare-and-swap (CAS) on refs as the atomic primitive.

```
cas_ref(branch: String, expected: ObjectId, new: ObjectId) → Result<bool>
```

A `cas_ref` succeeds only if the branch currently points to `expected`. If another agent has advanced the branch, the CAS fails and the agent must rebase and retry.

A convenience wrapper handles the retry loop:

```
update_with_retry(
  branch: String,
  update_fn: Fn(current_state: Value) → (new_state: Value, intent: Intent),
  max_retries: usize
) → Result<CommitId>
```

### 5.2 Branch-Per-Agent Pattern

The recommended multi-agent pattern:

1. Each agent operates on a namespaced branch: `agents/{agent_id}/workspace`
2. Agents periodically `sync()` from upstream (e.g., `main`) via fast-forward
3. When work is complete, agents create a `MergeProposal`
4. An authorized principal (human or lead agent) reviews and merges

```
MergeProposal {
  id:          String
  source:      String         // branch to merge from
  target:      String         // branch to merge into
  author:      String         // who proposed
  intent:      IntentId       // the intent this work fulfills
  status:      ProposalStatus
  resolution:  Resolution     // the "report back" — what was done, deviations
  created_at:  DateTime<Utc>
}

ProposalStatus = Open | Approved | Merged | Rejected | Withdrawn
```

### 5.3 Agent Sessions

A Session formalizes a working context.

```
Session {
  id:               String
  agent_id:         String
  working_branch:   String
  head:             ObjectId

  // Agent hierarchy
  parent_session:   Option<SessionId>     // who spawned this agent
  delegated_intent: Option<IntentId>      // what this agent was asked to do
  report_to:        Option<String>        // agent/principal to report resolution to

  // Operations
  sync()                                → Result         // fast-forward from upstream
  commit(changes, intent)               → CommitId       // commit to working branch
  propose_merge(target)                 → MergeProposal
  speculate()                           → SpecHandle
  delegate(sub_intent, agent_id)        → Session        // spawn a scoped sub-agent session
  collect_reports()                     → Vec<Resolution> // gather sub-agent resolutions
}
```

Multiple sessions can be active simultaneously. Each session has its own HEAD, so agents don't interfere with each other's working state. Sessions form a tree via `parent_session`, mirroring the agent orchestration hierarchy.

### 5.4 Sub-Agent Orchestration

StateGraph natively models the parent-child agent relationship. When a lead agent decomposes an intent into sub-tasks, the sub-agent hierarchy, branch isolation, authority scoping, and reporting all flow through StateGraph — not through external orchestration.

#### 5.4.1 Why This Matters

Consider a lead agent tasked with "set up a 5-node cluster for ML training." It decomposes into:
- Sub-agent A: configure networking
- Sub-agent B: configure storage
- Sub-agent C: configure GPU scheduling

With git, these three agents would be working on the same files, creating branches, merging, and generating conflicts constantly. Coordinating them requires external orchestration logic, and there is no structured way for sub-agents to report back or for the lead agent to understand what happened across all sub-tasks.

With StateGraph, the orchestration is built into the state store:

```
main
  └── agents/cluster-planner/workspace          (lead agent)
        ├── agents/network-agent/workspace       (sub-agent A)
        │     └── scoped to /config/network/**
        ├── agents/storage-agent/workspace       (sub-agent B)
        │     └── scoped to /config/storage/**
        └── agents/gpu-scheduler/workspace       (sub-agent C)
              └── scoped to /scheduling/**
```

#### 5.4.2 Delegation

A lead agent delegates work by creating a sub-intent and spawning a scoped session:

```
delegate(
  parent_intent:  IntentId,
  sub_intent: Intent {
    category: Refine,
    description: "Configure NFS storage for shared model weights",
    tags: ["storage", "nfs"]
  },
  agent_id:       "agent/storage-specialist",
  scope:          PathScope("/config/storage"),    // restrict what sub-agent can modify
  authority:      inherit_narrowed(parent_authority, PathScope("/config/storage"))
) → Session {
  id: "session-0042",
  agent_id: "agent/storage-specialist",
  working_branch: "agents/storage-specialist/intent-00a3",
  parent_session: "session-0039",         // lead agent's session
  delegated_intent: "intent-00a3",
  report_to: "agent/cluster-planner"
}
```

Key properties of delegation:
- **Branch isolation**: Sub-agent gets its own branch, cannot accidentally modify other sub-agents' work
- **Path scoping**: Sub-agent can only modify state under its assigned paths
- **Authority inheritance**: Sub-agent's authority is automatically narrowed from the parent's delegation chain
- **Intent threading**: Sub-intent links to parent intent via `parent_intent`, forming a queryable tree

#### 5.4.3 Parallel Sub-Agent Execution

Multiple sub-agents work simultaneously without interference:

```
                        main (shared state)
                          │
                    lead agent branch
                     ┌────┼────┐
                     │    │    │
                 net-agent │  gpu-agent
                 modifies: │  modifies:
                 /network  │  /scheduling
                           │
                     storage-agent
                     modifies:
                     /storage
```

Because each sub-agent is scoped to different paths, their work auto-merges without conflicts via the schema-aware merge engine. The lead agent collects all results:

```
// Lead agent gathers sub-agent reports
reports = collect_reports()

// Returns:
[
  Resolution {
    summary: "Configured NFS at /shared/models, 500GB allocated",
    deviations: [],
    outcome: Fulfilled,
    confidence: 0.95
  },
  Resolution {
    summary: "Network configured with 10GbE bonding across nodes 1-5",
    deviations: [],
    outcome: Fulfilled,
    confidence: 0.92
  },
  Resolution {
    summary: "GPU scheduling configured. Node 3 excluded — CUDA fault.",
    deviations: [{ description: "Node 3 excluded", reason: "Hardware fault", impact: Medium }],
    outcome: PartiallyFulfilled,
    confidence: 0.85
  }
]

// Lead agent merges all sub-agent branches into its workspace
merge("agents/storage-agent/intent-00a3", "agents/cluster-planner/workspace")
merge("agents/network-agent/intent-00a4", "agents/cluster-planner/workspace")
merge("agents/gpu-scheduler/intent-00a5", "agents/cluster-planner/workspace")

// All three merge cleanly because they modified different paths
// Lead agent then files its own resolution to report back to the human
```

#### 5.4.4 Intent Tree Queries

The full decomposition is queryable:

```
intent_tree("intent-00a1")

// Returns:
IntentTree {
  intent: "Optimize 5-node cluster for ML training",
  status: InProgress,
  children: [
    { intent: "Configure NFS storage", status: Completed, agent: "storage-specialist" },
    { intent: "Configure 10GbE network", status: Completed, agent: "network-agent" },
    { intent: "Configure GPU scheduling", status: Completed, agent: "gpu-scheduler",
      deviations: [{ "Node 3 excluded — CUDA fault" }],
      children: [
        { intent: "Investigate node 3 GPU fault", status: Proposed, agent: unassigned }
      ]
    }
  ]
}
```

This gives both humans and orchestrator agents a complete picture of the work hierarchy — what was asked, who is doing it, what is done, what deviated, and what follow-ups were generated.

#### 5.4.5 Why This is a Nightmare in Git

The same scenario in git requires:
- External orchestration to manage branch naming, scoping, and merging
- No built-in way to scope an agent to specific paths — any agent can modify anything
- Merge conflicts when agents touch adjacent lines in the same JSON/YAML file (common in config)
- No structured reporting — sub-agents would need a separate communication channel
- No intent threading — commit messages are flat text, not queryable trees
- No authority scoping — any agent with repo access can push anywhere
- Coordinating 3+ agents merging into a shared branch is fragile and error-prone

StateGraph makes this safe by design: scoped branches, schema-aware auto-merge, structured delegation and reporting, and a queryable intent tree.

### 5.5 Conflict Resolution

Structured data has a significant advantage over text for conflict resolution. Many concurrent changes can be auto-resolved based on schema annotations.

**Auto-resolvable** (no human/agent intervention needed):
- Different keys modified by different agents → union both changes
- Both agents add to a Set → union
- Both agents make identical changes → deduplicate
- Field annotated `sum` modified by both → add the deltas
- Field annotated `max` modified by both → take the higher value
- Field annotated `union-by-id` (array of records) modified by both → merge by record ID

**Requires resolution**:
- Same scalar field modified to different values by both agents
- Type conflict (one agent changed a field from string to integer)
- Delete-vs-modify (one agent deleted a key, another modified it)
- Custom merge strategy fields

When conflicts occur, the `Conflict` object includes a `suggested_resolution` field. An AI agent assigned to conflict resolution can inspect the intents and reasoning of both sides and propose a resolution.

---

## 6. Schema System

### 6.1 Overview

Schemas are optional. StateGraph functions without schemas (schema-free mode). When present, schemas provide:

- Validation of state changes before commit
- Merge hints that enable automatic conflict resolution
- Documentation of state structure for human and agent readers

### 6.2 Schema Format

Schemas use JSON Schema 2020-12 with StateGraph-specific extensions prefixed `x-stategraph-`.

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "stategraph://schema/cluster-state",
  "type": "object",
  "properties": {
    "nodes": {
      "type": "array",
      "items": { "$ref": "#/$defs/Node" },
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
  },
  "$defs": {
    "Node": {
      "type": "object",
      "properties": {
        "node_id": { "type": "string" },
        "hostname": { "type": "string" },
        "status": {
          "type": "string",
          "enum": ["healthy", "unhealthy", "draining", "offline"]
        },
        "gpu_memory_mb": {
          "type": "integer",
          "x-stategraph-merge": "last-writer-wins"
        }
      },
      "required": ["node_id", "hostname", "status"]
    }
  }
}
```

### 6.3 Merge Hints

| Hint | Behavior on Conflict | Applicable Types |
|------|---------------------|-----------------|
| `last-writer-wins` | Most recent commit's value wins | All types |
| `union-by-id` | Merge arrays of records by a key field | Arrays of objects |
| `union` | Union of both sets of values | Arrays, sets |
| `sum` | Add the deltas from both sides | Integers, floats |
| `max` | Take the higher value | Integers, floats, timestamps |
| `min` | Take the lower value | Integers, floats, timestamps |
| `concat` | Concatenate (source then target) | Strings, arrays |
| `manual` | Always flag as conflict | All types |
| `custom` | Invoke a named resolution function | All types |

### 6.4 Enforcement Modes

| Mode | Behavior |
|------|----------|
| `None` | No schema validation. Schema is documentation only. |
| `Warn` | Validate on commit, log warnings, but allow the commit. |
| `Enforce` | Reject commits that violate the schema. |
| `Migrate` | Apply automatic migrations when schema changes. |

### 6.5 Schema Evolution

Schema changes are themselves versioned as commits with intent category `Migrate`. The schema is stored as a special object in the state tree at `/__schema__`.

```json
{
  "operation": "set",
  "path": "/__schema__",
  "value": { "...updated schema..." },
  "intent": {
    "category": "Migrate",
    "description": "Add gpu_memory_mb field to Node schema",
    "tags": ["schema-migration", "v2"]
  }
}
```

---

## 7. MCP Interface

StateGraph exposes its operations as a Model Context Protocol server, allowing any MCP-compatible agent to interact with state stores directly.

### 7.1 Tools

#### 7.1.1 State Tools

**stategraph_get**
```json
{
  "name": "stategraph_get",
  "description": "Read a value from state at any branch, tag, or commit reference. Use JSON-path-style addressing to reach nested values.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "ref": { "type": "string", "description": "Branch name, tag name, or commit ID" },
      "path": { "type": "string", "description": "JSON path (e.g., '/nodes/0/hostname')" }
    },
    "required": ["ref", "path"]
  }
}
```

**stategraph_set**
```json
{
  "name": "stategraph_set",
  "description": "Write a value to state, creating a new commit. Every write is atomic. Requires an intent describing why this change is being made.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "ref": { "type": "string", "description": "Branch to commit to" },
      "path": { "type": "string", "description": "JSON path to set" },
      "value": { "description": "The value to write (any JSON type)" },
      "intent_category": { "type": "string", "enum": ["Explore", "Refine", "Fix", "Rollback", "Checkpoint", "Merge", "Migrate", "Custom"] },
      "intent_description": { "type": "string", "description": "Why this change is being made" },
      "intent_tags": { "type": "array", "items": { "type": "string" } },
      "reasoning": { "type": "string", "description": "Optional: your reasoning for this approach" },
      "confidence": { "type": "number", "minimum": 0, "maximum": 1, "description": "Optional: confidence in this change (0.0-1.0)" }
    },
    "required": ["ref", "path", "value", "intent_category", "intent_description"]
  }
}
```

**stategraph_delete**
```json
{
  "name": "stategraph_delete",
  "description": "Remove a value from state at the given path, creating a new commit.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "ref": { "type": "string" },
      "path": { "type": "string" },
      "intent_category": { "type": "string", "enum": ["Explore", "Refine", "Fix", "Rollback", "Checkpoint", "Merge", "Migrate", "Custom"] },
      "intent_description": { "type": "string" }
    },
    "required": ["ref", "path", "intent_category", "intent_description"]
  }
}
```

**stategraph_query**
```json
{
  "name": "stategraph_query",
  "description": "Query state using JSONPath expressions. Returns all matching values.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "ref": { "type": "string" },
      "expression": { "type": "string", "description": "JSONPath expression (e.g., '$.nodes[?(@.status==\"unhealthy\")]')" }
    },
    "required": ["ref", "expression"]
  }
}
```

#### 7.1.2 Branch Tools

**stategraph_branch**
```json
{
  "name": "stategraph_branch",
  "description": "Create a new branch from any ref. Use namespaced names like 'agents/my-agent/workspace' or 'explore/approach-a'.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "name": { "type": "string", "description": "Branch name (supports '/' namespacing)" },
      "from": { "type": "string", "description": "Ref to branch from (branch, tag, or commit ID)" }
    },
    "required": ["name", "from"]
  }
}
```

**stategraph_merge**
```json
{
  "name": "stategraph_merge",
  "description": "Merge changes from source ref into target branch. Uses schema-aware merge when schemas are defined. Returns conflicts if auto-resolution is not possible.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "source": { "type": "string" },
      "target": { "type": "string" },
      "strategy": { "type": "string", "enum": ["auto", "ours", "theirs", "manual"], "default": "auto" }
    },
    "required": ["source", "target"]
  }
}
```

**stategraph_list_branches**
```json
{
  "name": "stategraph_list_branches",
  "description": "List all branches, optionally filtered by namespace prefix.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "prefix": { "type": "string", "description": "Optional namespace filter (e.g., 'agents/' or 'explore/')" }
    }
  }
}
```

#### 7.1.3 Query and History Tools

**stategraph_query**
```json
{
  "name": "stategraph_query",
  "description": "Unified query interface for StateGraph. Query state values, commits, intents, agents, or epochs with composable filters. All filters are optional and combined with AND. This is the primary tool for asking questions about state, history, and metadata.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "target": {
        "type": "string",
        "enum": ["state", "commits", "intents", "agents", "epochs"],
        "description": "What to query: 'state' for current values, 'commits' for history, 'intents' for decisions, 'agents' for who did what, 'epochs' for bodies of work"
      },
      "ref": { "type": "string", "default": "main", "description": "Branch, tag, or commit to query against" },
      "at_time": { "type": "string", "format": "date-time", "description": "Temporal query: return results as of this point in time" },
      "epoch": { "type": "string", "description": "Scope query to a specific epoch" },
      "filters": {
        "type": "object",
        "properties": {
          "path": { "type": "string", "description": "Path pattern (e.g., '/nodes/*', '/config/network/**')" },
          "where": { "type": "string", "description": "Value filter expression (e.g., \"status == 'unhealthy'\", \"gpu_memory_mb > 8000\")" },
          "agent_id": { "type": "string" },
          "intent_category": { "type": "string", "enum": ["Explore", "Refine", "Fix", "Rollback", "Checkpoint", "Merge", "Migrate", "Custom"] },
          "tags": { "type": "array", "items": { "type": "string" } },
          "authority_principal": { "type": "string", "description": "Filter by who authorized the action" },
          "reasoning_contains": { "type": "string", "description": "Full-text search in reasoning traces" },
          "confidence_range": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2, "description": "[min, max] confidence range (e.g., [0.0, 0.5] for low-confidence)" },
          "intent_status": { "type": "string", "enum": ["Proposed", "Authorized", "InProgress", "Completed", "Failed", "Blocked"] },
          "outcome": { "type": "string", "enum": ["Fulfilled", "PartiallyFulfilled", "Failed", "Deferred"] },
          "date_range": { "type": "array", "items": { "type": "string" }, "minItems": 2, "maxItems": 2, "description": "[start, end] date range" },
          "has_deviations": { "type": "boolean", "description": "Only results where agent deviated from plan" }
        }
      },
      "select": { "type": "array", "items": { "type": "string" }, "description": "Fields to return (e.g., ['path', 'value', 'intent.description', 'agent_id'])" },
      "group_by": { "type": "string", "description": "Group results by a field for aggregation (e.g., 'agent_id', 'intent_category')" },
      "order_by": { "type": "string", "description": "Sort results (e.g., 'timestamp', 'confidence')" },
      "limit": { "type": "integer", "default": 20 },
      "offset": { "type": "integer", "default": 0, "description": "Pagination offset" }
    },
    "required": ["target"]
  }
}
```

**stategraph_diff**
```json
{
  "name": "stategraph_diff",
  "description": "Compute a structured diff between two refs. Returns typed DiffOps (SetValue, AddKey, RemoveKey, etc.), not text diffs. Use this to see exactly what changed between two branches, tags, or commits.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "ref_a": { "type": "string" },
      "ref_b": { "type": "string" }
    },
    "required": ["ref_a", "ref_b"]
  }
}
```

**stategraph_bisect**
```json
{
  "name": "stategraph_bisect",
  "description": "Binary search the commit DAG to find exactly where a condition changed. Provide a 'good' ref (where condition is true) and 'bad' ref (where it is false). StateGraph efficiently narrows down to the responsible commit.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "good": { "type": "string", "description": "Ref where the predicate is true" },
      "bad": { "type": "string", "description": "Ref where the predicate is false" },
      "predicate_path": { "type": "string", "description": "Path to check" },
      "predicate_value": { "description": "Expected value at the path" }
    },
    "required": ["good", "bad", "predicate_path", "predicate_value"]
  }
}
```

**stategraph_blame**
```json
{
  "name": "stategraph_blame",
  "description": "For each field at a path, show which commit last modified it, which agent, what intent, and what reasoning. Like git blame but with full provenance — not just who, but why.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "ref": { "type": "string", "default": "main" },
      "path": { "type": "string", "description": "Path to blame (e.g., '/nodes/2/status')" }
    },
    "required": ["path"]
  }
}
```

#### 7.1.4 Speculative Execution Tools

**stategraph_speculate**
```json
{
  "name": "stategraph_speculate",
  "description": "Create a lightweight speculation from a ref. Speculations are cheap (O(1) creation) and disposable. Use them to explore approaches before committing.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "from": { "type": "string", "description": "Ref to speculate from" },
      "label": { "type": "string", "description": "Optional human-readable label for this speculation" }
    },
    "required": ["from"]
  }
}
```

**stategraph_spec_modify**
```json
{
  "name": "stategraph_spec_modify",
  "description": "Modify state within a speculation. Changes are isolated until committed.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "handle": { "type": "string", "description": "Speculation handle from stategraph_speculate" },
      "operations": {
        "type": "array",
        "items": {
          "type": "object",
          "properties": {
            "op": { "type": "string", "enum": ["set", "delete"] },
            "path": { "type": "string" },
            "value": {}
          },
          "required": ["op", "path"]
        }
      }
    },
    "required": ["handle", "operations"]
  }
}
```

**stategraph_compare**
```json
{
  "name": "stategraph_compare",
  "description": "Compare state across multiple speculations. Returns structured diffs showing how each speculation diverges from the base and from each other.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "handles": { "type": "array", "items": { "type": "string" }, "minItems": 2 }
    },
    "required": ["handles"]
  }
}
```

**stategraph_commit_spec**
```json
{
  "name": "stategraph_commit_spec",
  "description": "Promote a speculation to a real commit on a branch. The speculation is consumed.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "handle": { "type": "string" },
      "target_branch": { "type": "string" },
      "intent_category": { "type": "string" },
      "intent_description": { "type": "string" },
      "reasoning": { "type": "string" }
    },
    "required": ["handle", "target_branch", "intent_category", "intent_description"]
  }
}
```

**stategraph_discard**
```json
{
  "name": "stategraph_discard",
  "description": "Discard a speculation. All associated state is freed immediately.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "handle": { "type": "string" }
    },
    "required": ["handle"]
  }
}
```

#### 7.1.5 Sub-Agent Orchestration Tools

**stategraph_delegate**
```json
{
  "name": "stategraph_delegate",
  "description": "Delegate a sub-intent to a sub-agent. Creates a scoped session with its own branch, narrowed authority, and path restrictions. The sub-agent reports back to the delegating agent when done.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "parent_intent_id": { "type": "string", "description": "The parent intent being decomposed" },
      "sub_intent": {
        "type": "object",
        "properties": {
          "category": { "type": "string", "enum": ["Explore", "Refine", "Fix", "Rollback", "Checkpoint", "Merge", "Migrate", "Custom"] },
          "description": { "type": "string" },
          "tags": { "type": "array", "items": { "type": "string" } }
        },
        "required": ["category", "description"]
      },
      "agent_id": { "type": "string", "description": "The sub-agent to assign (e.g., 'agent/storage-specialist')" },
      "path_scope": { "type": "string", "description": "Optional path prefix to restrict what the sub-agent can modify (e.g., '/config/storage')" }
    },
    "required": ["parent_intent_id", "sub_intent", "agent_id"]
  }
}
```

**stategraph_intent_tree**
```json
{
  "name": "stategraph_intent_tree",
  "description": "Return the full intent decomposition tree for a given intent. Shows all sub-intents, their status, assigned agents, resolutions, and deviations. Use this to understand the complete picture of a complex multi-agent task.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "intent_id": { "type": "string", "description": "Root intent ID to query the tree from" }
    },
    "required": ["intent_id"]
  }
}
```

**stategraph_collect_reports**
```json
{
  "name": "stategraph_collect_reports",
  "description": "Gather resolution reports from all sub-agents under the current session. Returns the summary, deviations, outcome, and confidence from each sub-agent's completed work.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "session_id": { "type": "string", "description": "The parent session to collect reports for" },
      "include_pending": { "type": "boolean", "default": false, "description": "Include sub-agents that haven't filed a resolution yet" }
    },
    "required": ["session_id"]
  }
}
```

#### 7.1.6 Collaboration Tools

**stategraph_propose_merge**
```json
{
  "name": "stategraph_propose_merge",
  "description": "Create a merge proposal for review. Includes intent resolution (summary of what was done, any deviations from plan).",
  "inputSchema": {
    "type": "object",
    "properties": {
      "source": { "type": "string", "description": "Branch with changes" },
      "target": { "type": "string", "description": "Branch to merge into" },
      "intent_id": { "type": "string", "description": "The intent this work fulfills" },
      "resolution": {
        "type": "object",
        "properties": {
          "summary": { "type": "string" },
          "deviations": { "type": "array", "items": { "type": "object" } },
          "outcome": { "type": "string", "enum": ["Fulfilled", "PartiallyFulfilled", "Failed", "Deferred"] },
          "confidence": { "type": "number" }
        },
        "required": ["summary", "outcome", "confidence"]
      },
      "notification": {
        "type": "object",
        "properties": {
          "urgency": { "type": "string", "enum": ["Routine", "Priority", "Critical"] },
          "audience": { "type": "array", "items": { "type": "string" } },
          "format_hint": { "type": "string", "enum": ["Summary", "Detailed", "DiffOnly"] }
        }
      }
    },
    "required": ["source", "target", "resolution"]
  }
}
```

**stategraph_review**
```json
{
  "name": "stategraph_review",
  "description": "Review and act on a merge proposal. Approve to merge, reject to decline.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "proposal_id": { "type": "string" },
      "action": { "type": "string", "enum": ["approve", "reject", "request_changes"] },
      "comment": { "type": "string" }
    },
    "required": ["proposal_id", "action"]
  }
}
```

**stategraph_sessions**
```json
{
  "name": "stategraph_sessions",
  "description": "List active agent sessions on this state store.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "agent_id": { "type": "string", "description": "Optional filter by agent" }
    }
  }
}
```

### 7.2 Resources

StateGraph exposes state as MCP resources with the `stategraph://` URI scheme.

| URI Pattern | Description |
|-------------|-------------|
| `stategraph://state/{ref}` | Full state tree at a ref |
| `stategraph://state/{ref}/{path}` | Value at a specific path |
| `stategraph://diff/{ref_a}..{ref_b}` | Structured diff between two refs |
| `stategraph://log/{ref}` | Commit log (supports query params for filtering) |
| `stategraph://branches` | List of all branches |
| `stategraph://tags` | List of all tags |
| `stategraph://schema` | Current schema (if defined) |
| `stategraph://proposals` | Open merge proposals |
| `stategraph://sessions` | Active agent sessions |

### 7.3 Events

StateGraph emits structured events that MCP clients can subscribe to:

| Event | Payload | Use Case |
|-------|---------|----------|
| `commit` | Commit object | Reactive agents watching for state changes |
| `intent.status_changed` | Intent + new status | Tracking intent lifecycle |
| `intent.resolved` | Intent + Resolution + NotificationPolicy | Triggering notifications |
| `proposal.created` | MergeProposal | Alerting reviewers |
| `proposal.status_changed` | MergeProposal + new status | Tracking review progress |
| `conflict.detected` | Vec\<Conflict\> | AI-assisted conflict resolution |

---

## 8. Architecture

### 8.1 Crate Structure

```
stategraph/
├── crates/
│   ├── stategraph-core/        # Object model, types, DAG, diff, merge, schema
│   │                           # Zero I/O dependencies. Pure logic.
│   │
│   ├── stategraph-storage/     # Storage traits + backend implementations
│   │   ├── traits.rs           # ObjectStore, RefStore, SessionStore
│   │   ├── memory.rs           # In-memory backend (default)
│   │   ├── sqlite.rs           # SQLite backend
│   │   └── file.rs             # File-based backend
│   │
│   ├── stategraph/             # High-level API
│   │   ├── repo.rs             # Repository handle
│   │   ├── session.rs          # Agent sessions
│   │   ├── speculation.rs      # Speculative execution
│   │   ├── watch.rs            # Watch/subscribe system
│   │   └── query.rs            # Intent queries, search, bisect
│   │
│   ├── stategraph-mcp/         # MCP server implementation
│   ├── stategraph-ffi/         # C ABI surface for language bindings
│   └── stategraph-wasm/        # wasm-bindgen for browser/Deno/Node
│
├── bindings/
│   ├── python/                 # PyO3 + maturin
│   ├── typescript/             # napi-rs
│   └── go/                     # CGo
│
├── spec/
│   ├── STATEGRAPH-RFC.md       # This document
│   └── schemas/                # JSON Schema definitions for all types
│
└── examples/
    ├── cluster-management/     # Multi-node cluster state (AgentStateLabs/Jetson)
    ├── multi-agent/            # Three agents coordinating on shared state
    └── creative-app/           # Human-AI collaborative state editing
```

### 8.2 Storage Traits

```rust
/// Content-addressed object storage
trait ObjectStore: Send + Sync {
    fn get(&self, id: &ObjectId) -> Result<Option<Object>>;
    fn put(&self, obj: &Object) -> Result<ObjectId>;
    fn exists(&self, id: &ObjectId) -> Result<bool>;
    fn batch_get(&self, ids: &[ObjectId]) -> Result<Vec<Option<Object>>>;
    fn batch_put(&self, objs: &[Object]) -> Result<Vec<ObjectId>>;
    fn gc(&self, reachable: &HashSet<ObjectId>) -> Result<usize>;
}

/// Named ref management with atomic CAS
trait RefStore: Send + Sync {
    fn get_ref(&self, name: &str) -> Result<Option<ObjectId>>;
    fn set_ref(&self, name: &str, target: ObjectId) -> Result<()>;
    fn cas_ref(&self, name: &str, expected: ObjectId, new: ObjectId) -> Result<bool>;
    fn list_refs(&self, prefix: &str) -> Result<Vec<(String, ObjectId)>>;
    fn delete_ref(&self, name: &str) -> Result<bool>;
}

/// Agent session management
trait SessionStore: Send + Sync {
    fn create_session(&self, session: &Session) -> Result<()>;
    fn get_session(&self, id: &str) -> Result<Option<Session>>;
    fn update_session(&self, session: &Session) -> Result<()>;
    fn list_sessions(&self) -> Result<Vec<Session>>;
    fn delete_session(&self, id: &str) -> Result<bool>;
}
```

### 8.3 Performance Design

| Aspect | Target | Technique |
|--------|--------|-----------|
| Branch creation | O(1) | Copy-on-write: new branch is just a ref pointing to existing commit |
| Hashing | >1 GB/s | BLAKE3 (parallelizable, SIMD-optimized) |
| Speculation create/discard | O(1) | Arena allocation: all speculation objects in a single arena, freed in one operation |
| State materialization | Lazy | Only load objects along the accessed path, not the entire tree |
| Deduplication | Automatic | Content-addressing: identical subtrees share storage |
| Batch operations | Amortized | batch_get/batch_put reduce I/O round trips |

### 8.4 Language Bindings

| Language | Technology | Distribution |
|----------|-----------|-------------|
| **Python** | PyO3 + maturin | `pip install stategraph` |
| **TypeScript/Node** | napi-rs | `npm install stategraph` |
| **Go** | CGo wrapping stategraph-ffi | `go get stategraph` |
| **Browser/Deno** | wasm-bindgen (stategraph-wasm) | npm or direct WASM import |
| **Any (C ABI)** | stategraph-ffi | Shared library (.so/.dylib/.dll) |

---

## 9. Human-Agent Collaboration

### 9.1 Shared Interface

Humans and agents use the same API. A human's identity is their `agent_id`. There is no separate "admin" interface — authority and intent metadata distinguish human actions from agent actions.

### 9.2 Approval Gates

The merge proposal system creates natural checkpoints where human review is required:

1. Agent works on `agents/planner/workspace`
2. Agent creates a MergeProposal targeting `main`
3. Agent files a Resolution: summary, deviations, outcome, confidence
4. Human reviews the structured diff (typed changes, not text patches)
5. Human approves, rejects, or requests changes
6. If approved, changes merge to `main` with full provenance

### 9.3 Transparency

Every state change is traceable through:
- **Who** performed the action (`agent_id`)
- **Who authorized it** (`authority` with delegation chain)
- **Why** (`intent` with category, description, tags)
- **How they decided** (`reasoning`, `confidence`)
- **What tools they used** (`tool_calls`)
- **What was accomplished** (`resolution` with deviations)
- **Who was notified** (`notification` policy)

This makes agent behavior auditable without requiring access to ephemeral conversation logs.

### 9.4 Web UI (Future)

A web interface powered by the WASM build would provide:
- **State explorer**: browse state at any ref, navigate the tree
- **DAG visualizer**: interactive commit graph with intent annotations
- **Intent timeline**: chronological view of intents with lifecycle status
- **Diff viewer**: structured, typed diffs between any two refs
- **Approval queue**: pending merge proposals with resolution summaries
- **Agent activity**: which agents are active, what they're working on, what they've done

### 9.5 Graduated Trust and Enterprise Adoption

Organizations adopting agentic AI face a fundamental tension: agents are most valuable when given autonomy, but autonomy without visibility is unacceptable to compliance teams, security teams, and leadership. The typical result is that organizations either don't adopt agents or adopt them without adequate oversight.

StateGraph resolves this by making **visibility scale with autonomy**. As agents are given more freedom, the provenance record becomes richer, not sparser. The authority and delegation model supports a graduated trust progression:

**Level 1 — Propose only (full guardrails)**
- Agent works on its own branch
- All changes require human review via MergeProposal
- Human merges to shared branches after reviewing structured diffs and resolution reports
- Authority scope: agent can only modify its own workspace branch

**Level 2 — Auto-merge to staging (loosening)**
- Agent merges to staging branches automatically
- Production changes still require human approval
- Authority scope: widened to include staging branches
- Notification policy: Priority alerts on staging changes, Critical on production proposals

**Level 3 — Scoped production autonomy (full autonomy, auditable)**
- Agent manages staging and production within its scoped paths
- Human reviews happen asynchronously via the audit trail, not as blocking gates
- Authority scope: specific production paths with expiring delegations
- Notification policy: routine summaries, critical alerts only on deviations or low-confidence actions

At every level, the same provenance record captures: what changed, why, who authorized it, what alternatives were considered, and who was notified. The audit trail for Level 3 is strictly richer than Level 1 — more intents, more delegation hops, more resolution reports — because more work is being done.

This means compliance and security teams can answer their core questions at any level:
- **"What did the AI do?"** → Query the intent tree, read resolutions and reasoning traces
- **"Who authorized this?"** → Follow the delegation chain from policy to agent
- **"Can we undo it?"** → Revert to any prior commit with full context on what will change
- **"Is it operating within bounds?"** → Authority scopes and path restrictions are enforced, not advisory

Organizations can start at Level 1 with zero risk and progress to Level 3 as trust builds — without changing tools, rewriting integrations, or losing audit capability.

---

## 10. Lifecycle Management: Epochs and the Registry

### 10.1 The Growth Problem

A long-lived StateGraph instance accumulates history fast. A production cluster managed by multiple agents over weeks generates thousands of commits, hundreds of intents, and dozens of agent sessions. Without lifecycle management, the store becomes:

- **Unwieldy to query** — searching all history for a specific incident means scanning everything
- **Expensive to load** — agents pulling context don't need last quarter's migration history
- **Hard to audit** — compliance needs bounded, self-contained units of work, not an infinite scroll
- **Risky to keep mutable** — historical records should be tamper-evident, not editable

### 10.2 Epochs

An **Epoch** is a bounded, sealable segment of work within a StateGraph instance. It groups related commits, intents, agent sessions, and resolutions into a coherent unit that can be managed as a whole.

```
Epoch {
  id:              String              // "2026-Q1-storage-migration"
  description:     String
  root_intents:    Vec<IntentId>       // the top-level intents that define this epoch
  status:          EpochStatus
  created_at:      DateTime<Utc>
  sealed_at:       Option<DateTime<Utc>>

  // Computed from the commit DAG
  commits:         Vec<ObjectId>       // all commits in this epoch
  agents:          Vec<String>         // all agents that participated
  branches:        Vec<String>         // all branches created during this epoch

  // Seal verification
  seal_hash:       Option<ObjectId>    // Merkle root of all epoch contents when sealed
}

EpochStatus = Active                   // work in progress, commits still being added
            | Sealed                   // work complete, read-only, hash-verified
            | Archived                 // sealed and moved to cold storage, queryable via index
```

#### 10.2.1 Creating an Epoch

An epoch is created when a significant body of work begins — typically tied to one or more root intents:

```
create_epoch(
  id: "2026-04-incident-node3",
  description: "Investigation and remediation of node 3 GPU fault",
  root_intents: ["intent-00a2"]    // the follow-up intent from GPU optimization
) → Epoch
```

Commits are associated with an epoch through their intent lineage: any commit whose intent (or ancestor intent) is a root intent of the epoch belongs to that epoch. This means sub-agent work is automatically included — no manual tagging.

#### 10.2.2 Sealing an Epoch

When work is complete, an epoch is **sealed**:

```
seal_epoch(
  id: "2026-04-incident-node3",
  summary: "Node 3 GPU fault diagnosed as memory controller failure. Node removed from scheduling. Hardware replacement ordered.",
  final_tag: "epoch/2026-04-incident-node3"   // immutable tag marking the final state
) → SealedEpoch
```

Sealing:
1. Marks the epoch as read-only — no new commits can be added
2. Creates an immutable tag at the final state
3. Computes a **seal hash** — the Merkle root of all commits, intents, resolutions, and objects in the epoch
4. Records the seal timestamp

Once sealed, the epoch is **tamper-evident**. Any modification to any commit, intent, or resolution within the epoch would change the seal hash. This is not a policy — it's a cryptographic property inherited from the content-addressed storage model.

#### 10.2.3 Exporting an Epoch

A sealed epoch can be exported as a self-contained **audit bundle**:

```
export_epoch(id: "2026-04-incident-node3", format: "bundle") → EpochBundle
```

An `EpochBundle` contains:
- All objects (state snapshots) referenced by commits in the epoch
- All commits with full metadata (intent, authority, reasoning, tool calls)
- All intent trees with resolutions and deviation reports
- The seal hash for independent verification
- A manifest listing all contents with their hashes

This bundle is independently verifiable: anyone with the bundle can recompute the Merkle tree and confirm it matches the seal hash. No access to the live StateGraph instance is needed.

Use cases for exported bundles:
- **Compliance audits**: hand the bundle to auditors as a tamper-evident record
- **Incident review**: share the full context of an incident across teams
- **Knowledge transfer**: onboard a new agent or team with the complete history of a body of work
- **Legal discovery**: produce a cryptographically verifiable record of AI-driven actions

#### 10.2.4 Archiving an Epoch

Sealed epochs can be **archived** — moved to cold storage while keeping a lightweight index entry:

```
archive_epoch(id: "2026-Q1-storage-migration", destination: "s3://audits/stategraph/")
```

Archiving:
1. Exports the epoch bundle to the specified destination
2. Removes the full objects from the active store (keeping only the index entry)
3. Retains the epoch's registry entry, seal hash, and cross-references
4. Epoch remains queryable via the registry ("what happened in Q1?") but full details require loading the archive

### 10.3 The Registry

The **Registry** is a lightweight master index stored at `/__registry__` in the state tree. It provides a navigable overview of all work in the StateGraph instance without requiring agents or humans to scan the full commit history.

```
Registry {
  epochs: Vec<EpochEntry>
  cross_references: Vec<CrossReference>
}

EpochEntry {
  id:              String
  description:     String
  status:          EpochStatus
  date_range:      (DateTime, Option<DateTime>)   // start, end (None if active)
  root_intents:    Vec<IntentId>
  agents:          Vec<String>
  commit_count:    usize
  seal_hash:       Option<ObjectId>
  storage:         Local | Archived(String)        // where the full data lives
  tags:            Vec<String>                     // queryable labels
}

CrossReference {
  from_epoch:     String
  to_epoch:       String
  relationship:   FollowUp | Dependency | Related | Reverts
  description:    String        // "Node 3 investigation spawned from GPU optimization epoch"
}
```

#### 10.3.1 Registry Queries

```
// List all epochs
list_epochs(filters: EpochFilters) → Vec<EpochEntry>

// Find epochs by content
search_epochs(query: "storage migration") → Vec<EpochEntry>

// Find epochs by agent
epochs_by_agent(agent_id: "agent/cluster-planner") → Vec<EpochEntry>

// Find epochs by date range
epochs_in_range(start: "2026-01-01", end: "2026-03-31") → Vec<EpochEntry>

// Get cross-references
epoch_references(id: "2026-04-incident-node3") → Vec<CrossReference>
// Returns: "Follow-up from epoch '2026-04-gpu-optimization', intent-00a2"
```

#### 10.3.2 Cross-Epoch Traceability

When a resolution in one epoch creates a follow-up intent that becomes the root of a new epoch, a cross-reference is automatically recorded. This creates a navigable chain across epochs:

```
Epoch: "2026-04-gpu-optimization"
  └─ Intent: "Optimize GPU scheduling"
       └─ Resolution: PartiallyFulfilled
            └─ Deviation: "Node 3 excluded — hardware fault"
                 └─ Follow-up intent: "Investigate node 3 GPU fault"
                      └─ Cross-reference → Epoch: "2026-04-incident-node3"
                           └─ Resolution: "Memory controller failure confirmed"
                                └─ Follow-up intent: "Replace node 3 hardware"
                                     └─ Cross-reference → Epoch: "2026-04-node3-replacement"
```

An auditor or agent can follow this chain from the original optimization request through the incident investigation to the hardware replacement — across three sealed, independently verifiable epochs.

### 10.4 MCP Tools for Lifecycle Management

**stategraph_create_epoch**
```json
{
  "name": "stategraph_create_epoch",
  "description": "Create a new epoch to group related work. Commits are automatically included based on intent lineage from the root intents.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "id": { "type": "string", "description": "Epoch identifier (e.g., '2026-04-incident-node3')" },
      "description": { "type": "string" },
      "root_intents": { "type": "array", "items": { "type": "string" }, "description": "Top-level intent IDs that define this epoch's scope" },
      "tags": { "type": "array", "items": { "type": "string" } }
    },
    "required": ["id", "description", "root_intents"]
  }
}
```

**stategraph_seal_epoch**
```json
{
  "name": "stategraph_seal_epoch",
  "description": "Seal an epoch, making it read-only and tamper-evident. Computes a Merkle root over all contents. Cannot be undone.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "id": { "type": "string" },
      "summary": { "type": "string", "description": "Final summary of the epoch's work and outcomes" },
      "final_tag": { "type": "string", "description": "Immutable tag name for the epoch's final state" }
    },
    "required": ["id", "summary"]
  }
}
```

**stategraph_export_epoch**
```json
{
  "name": "stategraph_export_epoch",
  "description": "Export a sealed epoch as a self-contained, independently verifiable audit bundle.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "id": { "type": "string" },
      "format": { "type": "string", "enum": ["bundle", "json", "cbor"], "default": "bundle" },
      "destination": { "type": "string", "description": "Optional export path or URI" }
    },
    "required": ["id"]
  }
}
```

**stategraph_list_epochs**
```json
{
  "name": "stategraph_list_epochs",
  "description": "List epochs in the registry with optional filters. Use this to understand the history of work in the StateGraph instance.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "status": { "type": "string", "enum": ["Active", "Sealed", "Archived", "All"], "default": "All" },
      "agent_id": { "type": "string", "description": "Filter to epochs involving this agent" },
      "tags": { "type": "array", "items": { "type": "string" } },
      "date_from": { "type": "string", "format": "date" },
      "date_to": { "type": "string", "format": "date" }
    }
  }
}
```

**stategraph_epoch_references**
```json
{
  "name": "stategraph_epoch_references",
  "description": "Get cross-references between epochs. Shows how work in one epoch led to follow-up work in other epochs.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "id": { "type": "string", "description": "Epoch ID to get references for" },
      "direction": { "type": "string", "enum": ["outgoing", "incoming", "both"], "default": "both" }
    },
    "required": ["id"]
  }
}
```

---

## 11. Reference Implementation

The specification is only valuable if it can be implemented and used easily — by humans reading the spec and by AI agents reading the MCP tool schemas. This section defines the two primary deliverables beyond the spec itself.

### 11.1 Principles

1. **Progressive complexity** — Start with the simplest useful subset. A developer or agent should be productive in minutes, not days.
2. **Layered adoption** — Each capability layer (branching, schema, speculation, sub-agents, epochs) is independently useful. You don't need epochs to use branching. You don't need sub-agent orchestration to use intents.
3. **Zero config to start** — The default configuration should work out of the box. SQLite storage (durable, single file), no schema, single agent. Add complexity only when needed.
4. **Spec-faithful** — The reference implementation must match the spec exactly. It is the canonical proof that the spec is implementable and coherent.

### 11.2 Rust Reference Library (`stategraph`)

The core library, implemented in Rust, provides the complete StateGraph API as described in this spec.

**Distribution:**
- Rust: `cargo add stategraph`
- Python: `pip install stategraph`
- TypeScript/Node: `npm install stategraph`
- Go: `go get stategraph`

**Implementation layers** (each layer builds on the previous and is independently shippable):

| Layer | What It Adds | Minimum Viable Use Case |
|-------|-------------|------------------------|
| **1. Core** | Objects, commits with intent, SQLite + in-memory stores, get/set/delete | Single agent managing structured state with durable history |
| **2. Branching** | Branches, structured diff, basic merge | Agent exploring alternatives, comparing outcomes |
| **3. Schema** | JSON Schema validation, merge hints, CRDT-inspired auto-merge | Multiple agents modifying shared state without conflicts |
| **4. Speculation** | Speculative execution, compare, commit/discard | Agent trying multiple approaches in parallel |
| **5. Authority** | Authority model, delegation chains, scoped sessions | Enterprise use with audit and access control |
| **6. Sub-Agent Orchestration** | Delegation, intent trees, scoped sessions, collect_reports | Orchestrator pattern with specialist sub-agents |
| **7. Epochs** | Lifecycle management, sealing, export, registry | Long-running production use with compliance needs |

Each layer has:
- Its own module in the crate
- Unit and integration tests
- A standalone example demonstrating the layer's capabilities
- Documentation that can be read independently

### 11.3 MCP Server (`stategraph-mcp`)

A standalone MCP server that any MCP-compatible agent (Claude, GPT, open-source agents) can connect to immediately.

**Distribution and startup:**
```bash
# npm (zero install)
npx stategraph-mcp

# pip
pip install stategraph-mcp
stategraph-mcp

# Binary (from releases)
./stategraph-mcp

# Docker
docker run -p 3000:3000 stategraph/mcp-server
```

**Configuration:**
```json
{
  "storage": "sqlite",
  "path": "./stategraph.db",
  "transport": "stdio"
}
```

That's the default — durable SQLite storage in a single file, zero external dependencies. All history, intents, and provenance survive process restarts. For ephemeral/testing use, set `"storage": "memory"`. Schema, authority, and epochs are enabled by adding their config sections — absent means off.

**For Claude Code / MCP client configuration:**
```json
{
  "mcpServers": {
    "stategraph": {
      "command": "npx",
      "args": ["stategraph-mcp"]
    }
  }
}
```

An agent connecting for the first time discovers all available tools via the MCP tool listing. The tool descriptions (Section 7) are written so that an agent reading them can understand what each tool does and how to use it without external documentation.

### 11.4 Getting Started Example

A complete example that demonstrates the core workflow, suitable for inclusion in the README and as an agent's first interaction with StateGraph:

```
# Agent connects to stategraph-mcp and runs:

# 1. Set initial state
stategraph_set(ref="main", path="/app/name", value="my-project",
  intent_category="Checkpoint", intent_description="Initialize project state")

# 2. Create a branch to try something
stategraph_branch(name="explore/new-feature", from="main")

# 3. Make changes on the branch
stategraph_set(ref="explore/new-feature", path="/app/feature_flags/dark_mode", value=true,
  intent_category="Explore", intent_description="Try enabling dark mode",
  reasoning="User requested dark mode support. Adding feature flag first to test.")

# 4. Compare with main
stategraph_diff(ref_a="main", ref_b="explore/new-feature")
# Returns: [{ op: "AddKey", path: "/app/feature_flags", key: "dark_mode", value: true }]

# 5. Merge if happy
stategraph_merge(source="explore/new-feature", target="main")

# 6. Check history
stategraph_log(ref="main", limit=5)
# Returns commits with full intent, reasoning, and metadata
```

This example works with zero configuration — SQLite storage (durable by default), no schema, single agent. Every concept introduced in the spec (branching, intents, structured diff, merge) is demonstrated in 6 tool calls. History survives restarts.

### 11.5 Implementation Test Suite

The reference implementation includes a comprehensive test suite organized by spec section:

| Test Suite | What It Validates |
|-----------|-------------------|
| `test_objects` | Content-addressing, deduplication, canonical serialization |
| `test_commits` | Commit creation, intent metadata, authority chains |
| `test_branches` | Branch create/delete/list, namespace conventions |
| `test_diff` | All DiffOp variants, schema-aware diffing |
| `test_merge` | Auto-resolution, conflict detection, all merge hint strategies |
| `test_speculation` | Create, modify, compare, commit, discard |
| `test_sessions` | Multi-session concurrency, CAS, sync |
| `test_delegation` | Sub-agent spawning, scoped authority, path restrictions |
| `test_intent_tree` | Intent decomposition, lifecycle transitions, resolution reporting |
| `test_epochs` | Create, seal, export, archive, registry queries, cross-references |
| `test_mcp` | All MCP tools against the spec, round-trip request/response validation |

A conformance test suite is also provided for third-party implementations: any implementation that passes the conformance suite is spec-compliant.

---

## 12. Open Questions

These questions are deferred for resolution during implementation or future RFCs.

1. **Hash function pluggability**: Should BLAKE3 be fixed, or should the hash function be configurable? Fixed simplifies interoperability; pluggable accommodates regulatory requirements (FIPS).

2. **Large value handling**: What is the inline size threshold for objects? Should large values (files, images, model weights) use a chunked storage model similar to git LFS?

3. **Commit signing**: Should commits support Ed25519 signatures for cryptographic agent identity verification? This would enable trustless verification of the delegation chain.

4. **Remote sync protocol**: How do distributed StateGraph instances synchronize? A protocol similar to git's pack-based transfer but operating on structured objects rather than files.

5. **Time-travel queries**: Should `query_at(ref, path, timestamp)` be supported natively? This requires temporal indexing and has storage cost implications.

6. **History compaction**: For long-running agents generating thousands of commits, should StateGraph support squashing or pruning history while preserving key checkpoints?

7. **Access control**: For multi-tenant deployments, should per-branch or per-path permissions be enforced at the storage layer?

8. **Event sourcing bridge**: Should StateGraph emit events compatible with existing event sourcing infrastructure (Kafka, EventStore), enabling integration with streaming architectures?

9. **Relationship to app-state systems**: StateGraph targets agent workflows. A companion project (exploratory-state-system, Swift) targets AI-native creative applications with simpler versioning semantics. Should there be a shared specification for the overlapping subset?

---

## Appendix A: Comparison with Git Object Model

| Concept | Git | StateGraph |
|---------|-----|-----------|
| Content addressing | SHA-1 (migrating to SHA-256) | BLAKE3 |
| Blob | Untyped byte sequence | Typed Atom (null, bool, int, float, string, bytes) |
| Tree | Directory listing (name → blob/tree) | Node (Map, List, Set of ObjectId references) |
| Commit | tree + parents + author + message | state_root + parents + agent_id + authority + intent + lifecycle + reasoning + confidence + tool_calls |
| Ref | Branch or tag → commit SHA | Branch or tag → ObjectId, with namespace conventions |
| Staging area | Index file | None — every write is an atomic commit |
| Merge | Text-based (3-way merge, recursive) | Schema-aware (CRDT-inspired merge hints per field) |
| Diff | Line-based text diff | Typed DiffOp (SetValue, AddKey, AddElement, etc.) |
| Hooks | Shell scripts triggered by events | MCP events + watch/subscribe system |

## Appendix B: Full Commit Example

```json
{
  "id": "sg_7f3a2b9c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a",
  "state_root": "sg_1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b",
  "parents": ["sg_4e2b8a1c3d5f7e9a2b4c6d8e0f1a3b5c7d9e1f3a"],
  "timestamp": "2026-04-04T14:30:00Z",

  "agent_id": "agent/cluster-planner-v2",

  "authority": {
    "principal": "agent/cluster-planner-v2",
    "scope": { "intent": "intent-00a1" },
    "granted_at": "2026-04-04T14:00:00Z",
    "expires": "2026-04-04T18:00:00Z",
    "delegation_chain": [
      {
        "from": "policy/cluster-management",
        "to": "team/infrastructure",
        "scope": "branch:main",
        "granted_at": "2026-01-01T00:00:00Z"
      },
      {
        "from": "team/infrastructure",
        "to": "user",
        "scope": "branch:main",
        "granted_at": "2026-03-01T00:00:00Z"
      },
      {
        "from": "user",
        "to": "agent/cluster-planner-v2",
        "scope": { "intent": "intent-00a1" },
        "granted_at": "2026-04-04T14:00:00Z",
        "expires": "2026-04-04T18:00:00Z"
      }
    ]
  },

  "intent": {
    "id": "intent-00a1",
    "category": "Refine",
    "description": "Optimize GPU workload distribution across cluster nodes",
    "tags": ["gpu", "scheduling", "optimization"],
    "parent_intent": "intent-0099",
    "lifecycle": {
      "status": "Completed",
      "assigned_to": ["agent/cluster-planner-v2"],
      "resolution": {
        "summary": "Redistributed GPU workloads across nodes 2 and 4. Achieved 40% improvement in GPU utilization. Node 3 excluded due to hardware fault.",
        "deviations": [
          {
            "description": "Excluded node 3 from GPU workload scheduling",
            "reason": "Hardware fault detected on GPU 1 — CUDA memory test failed",
            "impact": "Medium",
            "follow_up": "intent-00a2"
          }
        ],
        "commits": [
          "sg_7f3a2b9c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a"
        ],
        "branches_explored": [
          "explore/round-robin-scheduling",
          "explore/memory-aware-scheduling"
        ],
        "outcome": "PartiallyFulfilled",
        "confidence": 0.85
      },
      "notification": {
        "urgency": "Priority",
        "audience": ["user", "team/infrastructure"],
        "format_hint": "Summary"
      }
    }
  },

  "reasoning": "Analyzed GPU memory usage across all 5 nodes. Node 3 failed CUDA memory test — excluded from scheduling. Compared round-robin vs memory-aware placement on branches. Memory-aware scheduling showed 40% better utilization in simulation. Applied memory-aware schedule to nodes 2 and 4 (highest available VRAM). Created follow-up intent for node 3 hardware investigation.",

  "confidence": 0.85,

  "tool_calls": [
    {
      "tool_name": "nvidia_smi_query",
      "arguments": { "node": "all", "query": "memory.used,memory.total" },
      "result": "Node 1: 4GB/8GB, Node 2: 2GB/16GB, Node 3: ERROR, Node 4: 1GB/16GB, Node 5: 6GB/8GB",
      "timestamp": "2026-04-04T14:15:00Z"
    },
    {
      "tool_name": "stategraph_set",
      "arguments": { "ref": "main", "path": "/scheduling/gpu_policy", "value": "memory-aware" },
      "result": "committed",
      "timestamp": "2026-04-04T14:28:00Z"
    }
  ]
}
```

---

*StateGraph RFC-0001 — Draft, 2026-04-04*
