# AgentStateGraph RFC-0001: AI-Native Versioned State Store

```
RFC:         0001
Title:       AgentStateGraph — AI-Native Versioned State Store
Status:      Stable
Version:     0.8.0
Created:     2026-04-04
Updated:     2026-05-02
Authors:     Craig Brown
```

---

## Abstract

AgentStateGraph is a content-addressed, versioned, branchable structured state store designed as an infrastructure primitive for intent-based systems. It provides AI agents and AI-native applications with a git-like state management layer that captures not just *what* changed, but *why*, *who authorized it*, *what alternatives were considered*, and *who was informed* — making every state transition auditable, reversible, and explainable.

AgentStateGraph is implemented as an embeddable Rust library with language bindings for Python, TypeScript, Go, and .NET, and exposed as a Model Context Protocol (MCP) server with 73 tools for direct agent integration. As of v0.8.0, the system also provides declarative authorization policy with multi-engine evaluation and Ed25519 signing, taint/quarantine/watch safety controls, pull-based reminders, and a five-level priority task system — all integrated into the same provenance-bearing commit model.

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

Most current infrastructure (including MCP, agent frameworks, and git-based state) is designed for the single-agent phase. AgentStateGraph is designed for the orchestrator phase and forward — its data model natively represents intent decomposition, agent hierarchies, scoped delegation, parallel sub-agent execution with safe merge, and structured reporting up the chain.

The fan-out problem is significant. An orchestrator delegates to sub-agents, each of which may call multiple tools, MCP servers, and even spawn their own sub-agents. A single user intent can spider out into dozens of agent sessions, hundreds of tool calls, and multiple MCP server interactions across layers:

```
User intent: "Set up the cluster for ML training"
  └─ Orchestrator agent
       ├─ Network sub-agent
       │    ├─ tool: kubectl apply (configure CNI)
       │    ├─ MCP: cloud-provider/vpc (verify subnets)
       │    └─ tool: agentstategraph_set (commit network state)
       ├─ Storage sub-agent
       │    ├─ MCP: cloud-provider/storage (provision volumes)
       │    ├─ tool: kubectl apply (deploy NFS server)
       │    └─ Sub-sub-agent: storage-validator
       │         ├─ tool: ssh (mount test)
       │         └─ MCP: monitoring/prometheus (verify IOPS)
       └─ GPU scheduling sub-agent
            ├─ MCP: nvidia/dcgm (query GPU health)
            ├─ tool: agentstategraph_set (commit schedule)
            └─ Sub-sub-agent: benchmarker
                 ├─ tool: kubectl apply (run benchmark pod)
                 └─ MCP: monitoring/prometheus (collect metrics)
```

Today, each branch of this tree is a separate conversation context that evaporates when it completes. The orchestrator gets back a text summary — maybe. There is no unified record of what happened, what was tried, what failed, who authorized what at each level, or how the decisions connect across the tree.

AgentStateGraph captures the entire execution tree: intent decomposition, delegation chains, per-agent branches with scoped state changes, tool call provenance at every level, and structured resolutions reporting back up the chain. When something goes wrong three layers deep, you trace it from root intent through every decision to the specific tool call that failed.

Building for the orchestrator pattern now means teams adopting AgentStateGraph are ready for the architecture that is already arriving, rather than retrofitting single-agent tooling when it breaks under multi-agent coordination.

### 1.4 What Exists Today and Why It Falls Short

| Existing Tool | What It Provides | What It Lacks |
|--------------|-----------------|---------------|
| **Git** | Content-addressed DAG, branching, merging | Text-oriented diffs; no structured data awareness; no intent/reasoning metadata; merge strategies don't understand domain schemas; not designed for programmatic-speed branching |
| **Databases** | Structured data, queries, transactions | No branching; no persistent alternatives; no history DAG; transactions are commit/rollback, not explore/compare/merge |
| **Event Sourcing** | Append-only history, event replay | No branching or speculative exploration; events are facts, not intents; no built-in merge semantics |
| **Redux / State Managers** | Time-travel debugging, undo/redo | Linear stack, not a DAG; framework-specific; no multi-agent support; no persistence; undo/redo is not branching |
| **CRDTs** | Conflict-free concurrent editing | No history; no intent tracking; designed for real-time collaboration, not agent exploration; limited to specific data types |
| **Kubernetes** | Declarative desired state, reconciliation | Domain-specific (containers); no reasoning trace; no branching exploration; no general-purpose state model |

### 1.5 What AgentStateGraph Provides

AgentStateGraph combines the missing pieces into a single primitive:

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
| **Epoch** | A bounded, named segment of work. Epochs are Open while active and Sealed when closed. Sealing stamps a tamper-evident Merkle root hash and prevents further commits from being associated. Sealed epochs can be exported as self-contained audit bundles. |
| **EpochStatus** | Open or Sealed. Attempting to associate a commit with a Sealed epoch returns `EpochAlreadySealed`. |
| **Task** | A discrete unit of work within a Plan. Carries a status state machine (Pending → InProgress → Completed / Abandoned), optional assignment, priority, and proof-of-work. |
| **Plan** | A named collection of Tasks. Plans are committed to the state tree under a configurable path prefix. |
| **Taint** | A safety mark applied to a state path. Three kinds: `Taint` (suspicious data), `Quarantine` (blocks modification until reviewed), `Watch` (audit notifications). Each carries a path, name, kind, effect, severity, reason, agent\_id, expiry, and propagation flag. Taints are resolved (not deleted) to preserve the audit trail. |
| **TaintKind** | `Taint`, `Quarantine`, or `Watch`. Determines what effect is applied when the path is accessed. |
| **TaintEffect** | `Warn`, `Block`, `Review`, `Isolate`, or `Advisory`. The action taken when a tainted path is accessed or modified. |
| **TaintSeverity** | `Low`, `Medium`, `High`, or `Critical`. Informational; consumers may use it to prioritize resolution. |
| **TaintStore** | The storage sub-trait responsible for persisting taint records. Provides default no-op implementations so backends can opt in gradually. |
| **Policy** | A named, versioned access-control or cost-of-change rule set. Policies are proposed, ratified, and optionally signed before becoming Active. Each policy carries an engine kind (builtin, Rego, WASM, Cedar) and its rule payload. |
| **PolicyStatus** | `Proposed`, `Active`, or `Superseded`. Only Active policies are evaluated. Superseding an Active policy atomically transitions it to Superseded and activates the replacement. |
| **PolicyEngine** | The evaluation backend for a policy: `builtin` (native rule engine), `rego` (OPA), `wasm` (any language compiled to WASM), or `cedar` (Amazon Cedar). |
| **Ratification** | The formal act of approving a Proposed policy, transitioning it to Active. Ratification is recorded as an audit commit with `IntentCategory::PolicyRatify`. |
| **Ed25519Signature** | A cryptographic signature over the canonical (sorted-key JSON, signature field excluded) policy body. Policies can require N-of-M signatures before ratification is permitted. |
| **Reminder** | A scheduled future work item for an agent or user. Carries a title, instructions, priority, due time, repeating schedule, autonomous flag, soft refs, and full execution audit trail. |
| **ReminderStatus** | `Pending`, `Due`, `AwaitingPermission`, `InProgress`, `Completed`, `Snoozed`, or `Cancelled`. `Due` and `AwaitingPermission` items are returned by `remind_me()`. |
| **Priority** | A five-level urgency scale: `Critical` (1), `High` (2), `Medium` (3), `Low` (4), `Minimal` (5). Lower number = higher urgency. `remind_me()` returns items ordered by priority then due date. |
| **Schedule** | The recurrence rule for a repeating Reminder: `Once` (no recurrence), `Interval` (every N seconds), `Daily` (HH:MM UTC), or `Weekly` (Weekday + HH:MM UTC). After a successful execution the next due time is computed and the Reminder resets. |
| **ReminderRef** | A soft, advisory reference from a Reminder to another object (branch, memory, plan, task, state path, or external URL). The `label` is captured at creation time and survives renames. A stale ref does not invalidate the Reminder. |
| **RefKind** | The type of object a ReminderRef points to: `Branch`, `Memory`, `Plan`, `Task`, `StatePath`, or `External { scheme }`. |
| **ExecutionRecord** | An audit entry for one execution of a Reminder. Records start/end time, agent id, result (success/failed/cancelled), notes, and optional task id. Execution history accumulates; the Reminder record is never overwritten. |
| **ReminderStore** | The storage sub-trait responsible for persisting Reminder records. Provides default no-op implementations so backends can opt in gradually. |

---

## 3. Core Data Model

### 3.1 Objects

All state in AgentStateGraph is composed of Objects. Every Object is individually content-addressed via BLAKE3 hash of its canonical serialization.

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

AgentStateGraph does not deliver notifications directly. The notification policy is stored as part of the provenance record and emitted as a structured event that integration layers (Slack bots, email systems, dashboards) can subscribe to and act on.

#### 3.2.6 ToolCall

```
ToolCall {
  tool_name:  String              // "kubectl_apply", "agentstategraph_set", etc.
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

AgentStateGraph stores rich, multi-dimensional data: state values, commit history, intent metadata, authority chains, reasoning traces, agent sessions, and epoch records. Rather than requiring agents to learn many specialized query operations, AgentStateGraph provides a **single unified query interface** that can answer questions across all dimensions.

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
// (run two queries and diff, or use agentstategraph_diff)
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

AgentStateGraph uses **optimistic concurrency** with compare-and-swap (CAS) on refs as the atomic primitive.

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

AgentStateGraph natively models the parent-child agent relationship. When a lead agent decomposes an intent into sub-tasks, the sub-agent hierarchy, branch isolation, authority scoping, and reporting all flow through AgentStateGraph — not through external orchestration.

#### 5.4.1 Why This Matters

Consider a lead agent tasked with "set up a 5-node cluster for ML training." It decomposes into:
- Sub-agent A: configure networking
- Sub-agent B: configure storage
- Sub-agent C: configure GPU scheduling

With git, these three agents would be working on the same files, creating branches, merging, and generating conflicts constantly. Coordinating them requires external orchestration logic, and there is no structured way for sub-agents to report back or for the lead agent to understand what happened across all sub-tasks.

With AgentStateGraph, the orchestration is built into the state store:

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

AgentStateGraph makes this safe by design: scoped branches, schema-aware auto-merge, structured delegation and reporting, and a queryable intent tree.

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

Schemas are optional. AgentStateGraph functions without schemas (schema-free mode). When present, schemas provide:

- Validation of state changes before commit
- Merge hints that enable automatic conflict resolution
- Documentation of state structure for human and agent readers

### 6.2 Schema Format

Schemas use JSON Schema 2020-12 with AgentStateGraph-specific extensions prefixed `x-agentstategraph-`.

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "agentstategraph://schema/cluster-state",
  "type": "object",
  "properties": {
    "nodes": {
      "type": "array",
      "items": { "$ref": "#/$defs/Node" },
      "x-agentstategraph-merge": "union-by-id",
      "x-agentstategraph-id-field": "node_id"
    },
    "request_count": {
      "type": "integer",
      "x-agentstategraph-merge": "sum"
    },
    "config": {
      "type": "object",
      "x-agentstategraph-merge": "last-writer-wins"
    },
    "active_alerts": {
      "type": "array",
      "uniqueItems": true,
      "x-agentstategraph-merge": "union"
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
          "x-agentstategraph-merge": "last-writer-wins"
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

AgentStateGraph exposes its operations as a Model Context Protocol server, allowing any MCP-compatible agent to interact with state stores directly.

The MCP server exposes **73 tools** across five groups: 36 core (state, branching, speculation, query/audit, namespaces, epochs, sessions, explorer), 10 task management, 12 policy, 8 taint, and 7 reminders.

The `intent_category` parameter accepted by write tools has been expanded to include plan/task lifecycle events, taint operations, and policy lifecycle events:

```
"Explore" | "Refine" | "Fix" | "Rollback" | "Checkpoint" | "Merge" | "Migrate"
| "Plan"
| "Taint" | "Untaint" | "Quarantine" | "Unquarantine" | "Watch" | "Unwatch"
| "PolicyPropose" | "PolicyRatify" | "PolicySupersede" | "PolicySign"
| "Custom:<value>"
```

### 7.1 Tools

#### 7.1.1 State Tools

**agentstategraph_get**
```json
{
  "name": "agentstategraph_get",
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

**agentstategraph_set**
```json
{
  "name": "agentstategraph_set",
  "description": "Write a value to state, creating a new commit. Every write is atomic. Requires an intent describing why this change is being made.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "ref": { "type": "string", "description": "Branch to commit to" },
      "path": { "type": "string", "description": "JSON path to set" },
      "value": { "description": "The value to write (any JSON type)" },
      "intent_category": { "type": "string", "description": "See §7.1 preamble for full enum" },
      "intent_description": { "type": "string", "description": "Why this change is being made" },
      "intent_tags": { "type": "array", "items": { "type": "string" } },
      "reasoning": { "type": "string", "description": "Optional: your reasoning for this approach" },
      "confidence": { "type": "number", "minimum": 0, "maximum": 1, "description": "Optional: confidence in this change (0.0-1.0)" }
    },
    "required": ["ref", "path", "value", "intent_category", "intent_description"]
  }
}
```

**agentstategraph_delete**
```json
{
  "name": "agentstategraph_delete",
  "description": "Remove a value from state at the given path, creating a new commit.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "ref": { "type": "string" },
      "path": { "type": "string" },
      "intent_category": { "type": "string", "description": "See §7.1 preamble for full enum" },
      "intent_description": { "type": "string" }
    },
    "required": ["ref", "path", "intent_category", "intent_description"]
  }
}
```

**agentstategraph_query**
```json
{
  "name": "agentstategraph_query",
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

**agentstategraph_branch**
```json
{
  "name": "agentstategraph_branch",
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

**agentstategraph_merge**
```json
{
  "name": "agentstategraph_merge",
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

**agentstategraph_list_branches**
```json
{
  "name": "agentstategraph_list_branches",
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

**agentstategraph_query**
```json
{
  "name": "agentstategraph_query",
  "description": "Unified query interface for AgentStateGraph. Query state values, commits, intents, agents, or epochs with composable filters. All filters are optional and combined with AND. This is the primary tool for asking questions about state, history, and metadata.",
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
          "intent_category": { "type": "string", "description": "See §7.1 preamble for full enum" },
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

**agentstategraph_diff**
```json
{
  "name": "agentstategraph_diff",
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

**agentstategraph_bisect**
```json
{
  "name": "agentstategraph_bisect",
  "description": "Binary search the commit DAG to find exactly where a condition changed. Provide a 'good' ref (where condition is true) and 'bad' ref (where it is false). AgentStateGraph efficiently narrows down to the responsible commit.",
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

**agentstategraph_blame**
```json
{
  "name": "agentstategraph_blame",
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

**agentstategraph_speculate**
```json
{
  "name": "agentstategraph_speculate",
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

**agentstategraph_spec_modify**
```json
{
  "name": "agentstategraph_spec_modify",
  "description": "Modify state within a speculation. Changes are isolated until committed.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "handle": { "type": "string", "description": "Speculation handle from agentstategraph_speculate" },
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

**agentstategraph_compare**
```json
{
  "name": "agentstategraph_compare",
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

**agentstategraph_commit_spec**
```json
{
  "name": "agentstategraph_commit_spec",
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

**agentstategraph_discard**
```json
{
  "name": "agentstategraph_discard",
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

**agentstategraph_delegate**
```json
{
  "name": "agentstategraph_delegate",
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

**agentstategraph_intent_tree**
```json
{
  "name": "agentstategraph_intent_tree",
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

**agentstategraph_collect_reports**
```json
{
  "name": "agentstategraph_collect_reports",
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

**agentstategraph_propose_merge**
```json
{
  "name": "agentstategraph_propose_merge",
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

**agentstategraph_review**
```json
{
  "name": "agentstategraph_review",
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

**agentstategraph_sessions**
```json
{
  "name": "agentstategraph_sessions",
  "description": "List active agent sessions on this state store.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "agent_id": { "type": "string", "description": "Optional filter by agent" }
    }
  }
}
```

#### 7.1.7 Task Management Tools

Plans and tasks are persisted in the state tree. All task mutations produce audit commits.

**agentstategraph_create_plan**
```json
{
  "name": "agentstategraph_create_plan",
  "description": "Create a new Plan (a named collection of Tasks).",
  "inputSchema": {
    "type": "object",
    "properties": {
      "ref":         { "type": "string", "default": "main" },
      "title":       { "type": "string" },
      "description": { "type": "string" },
      "tags":        { "type": "array", "items": { "type": "string" } }
    },
    "required": ["title"]
  }
}
```

**agentstategraph_add_task**
```json
{
  "name": "agentstategraph_add_task",
  "description": "Add a Task to an existing Plan.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "ref":         { "type": "string", "default": "main" },
      "plan_id":     { "type": "string" },
      "title":       { "type": "string" },
      "description": { "type": "string" },
      "priority":    { "type": "integer", "minimum": 1, "maximum": 5, "default": 3 },
      "assigned_to": { "type": "string" },
      "tags":        { "type": "array", "items": { "type": "string" } }
    },
    "required": ["plan_id", "title"]
  }
}
```

**agentstategraph_start_task** / **agentstategraph_complete_task** / **agentstategraph_abandon_task**
```json
{
  "name": "agentstategraph_start_task",
  "description": "Transition a Task from Pending to InProgress.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "ref":     { "type": "string", "default": "main" },
      "task_id": { "type": "string" },
      "agent_id":{ "type": "string" }
    },
    "required": ["task_id"]
  }
}
```

*(complete_task and abandon_task share the same schema; complete_task accepts an optional `proof` string; abandon_task accepts an optional `reason` string.)*

**agentstategraph_assign_task**
```json
{
  "name": "agentstategraph_assign_task",
  "description": "Assign a Task to an agent or user.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "ref":         { "type": "string", "default": "main" },
      "task_id":     { "type": "string" },
      "assigned_to": { "type": "string" }
    },
    "required": ["task_id", "assigned_to"]
  }
}
```

**agentstategraph_list_tasks** / **agentstategraph_next_task** / **agentstategraph_get_plan** / **agentstategraph_list_plans**

These tools list and query tasks and plans. `next_task` returns the highest-priority Pending task optionally filtered by assigned agent. `list_tasks` accepts status, priority, assigned\_to, and plan\_id filters. `get_plan` returns a plan and all its tasks. `list_plans` accepts tag and status filters.

---

#### 7.1.8 Policy Tools

All policy mutations are audit-committed with the appropriate `PolicyPropose` / `PolicyRatify` / etc. intent category.

**agentstategraph_policy_propose**
```json
{
  "name": "agentstategraph_policy_propose",
  "description": "Propose a new policy. Status is Proposed until ratified.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "ref":         { "type": "string", "default": "main" },
      "name":        { "type": "string" },
      "description": { "type": "string" },
      "engine":      { "type": "string", "enum": ["builtin", "rego", "wasm", "cedar"] },
      "rules":       { "type": "string", "description": "Policy rules payload (engine-dependent format)" },
      "proposed_by": { "type": "string" }
    },
    "required": ["name", "engine", "rules", "proposed_by"]
  }
}
```

**agentstategraph_policy_ratify**
```json
{
  "name": "agentstategraph_policy_ratify",
  "description": "Ratify a Proposed policy, making it Active. Fails if required signatures are missing.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "ref":       { "type": "string", "default": "main" },
      "policy_id": { "type": "string" },
      "ratified_by": { "type": "string" }
    },
    "required": ["policy_id", "ratified_by"]
  }
}
```

**agentstategraph_policy_sign**
```json
{
  "name": "agentstategraph_policy_sign",
  "description": "Sign a policy with an Ed25519 private key. The signature covers the canonical (sorted-key JSON, signature field excluded) policy body.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "policy_id":   { "type": "string" },
      "signer_id":   { "type": "string" },
      "private_key": { "type": "string", "description": "Base64-encoded Ed25519 private key" }
    },
    "required": ["policy_id", "signer_id", "private_key"]
  }
}
```

**agentstategraph_policy_verify**
```json
{
  "name": "agentstategraph_policy_verify",
  "description": "Verify all Ed25519 signatures on a policy. Returns per-signer pass/fail.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "policy_id": { "type": "string" }
    },
    "required": ["policy_id"]
  }
}
```

**agentstategraph_policy_supersede**
```json
{
  "name": "agentstategraph_policy_supersede",
  "description": "Atomically supersede an Active policy with a new one. The old policy transitions to Superseded.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "old_policy_id": { "type": "string" },
      "new_policy_id": { "type": "string" },
      "superseded_by": { "type": "string" }
    },
    "required": ["old_policy_id", "new_policy_id", "superseded_by"]
  }
}
```

**agentstategraph_policy_evaluate**
```json
{
  "name": "agentstategraph_policy_evaluate",
  "description": "Evaluate a request against a specific policy. Returns Allow/Deny/Escalate/RequestApproval.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "policy_id": { "type": "string" },
      "request":   { "type": "object", "description": "The access request context (agent_id, path, action, value, confidence, token_cost, etc.)" }
    },
    "required": ["policy_id", "request"]
  }
}
```

**agentstategraph_policy_evaluate_change** / **agentstategraph_policy_check_tokens** / **agentstategraph_policy_show** / **agentstategraph_policy_list** / **agentstategraph_policy_history**

`policy_evaluate_change` combines policy + taint evaluation for a proposed state change in a single call. `policy_check_tokens` evaluates a token-cost budget against the active policy. `policy_show` returns a single policy record with its full rule payload and signature list. `policy_list` returns all policies with optional status filter. `policy_history` returns the audit commit trail for a given policy id.

---

#### 7.1.9 Taint, Quarantine, and Watch Tools

**agentstategraph_taint**
```json
{
  "name": "agentstategraph_taint",
  "description": "Apply a Taint mark to a path. Audit-committed with IntentCategory::Taint.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "ref":        { "type": "string", "default": "main" },
      "path":       { "type": "string" },
      "name":       { "type": "string", "description": "Short label (e.g., 'credential-exposure')" },
      "effect":     { "type": "string", "enum": ["warn", "block", "review", "isolate", "advisory"] },
      "severity":   { "type": "string", "enum": ["low", "medium", "high", "critical"], "default": "medium" },
      "reason":     { "type": "string" },
      "agent_id":   { "type": "string" },
      "expires_at": { "type": "string", "format": "date-time" },
      "propagate":  { "type": "boolean", "default": true, "description": "Apply to descendant paths" }
    },
    "required": ["path", "name", "effect", "reason", "agent_id"]
  }
}
```

**agentstategraph_untaint**
```json
{
  "name": "agentstategraph_untaint",
  "description": "Resolve a Taint mark. The record is preserved for audit; resolved_at is stamped.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "ref":         { "type": "string", "default": "main" },
      "path":        { "type": "string" },
      "name":        { "type": "string" },
      "resolved_by": { "type": "string" },
      "reason":      { "type": "string" },
      "proof":       { "type": "string", "description": "Optional evidence URL or commit id" }
    },
    "required": ["path", "name", "resolved_by", "reason"]
  }
}
```

**agentstategraph_quarantine** / **agentstategraph_unquarantine**

Shorthand to apply / resolve a `Quarantine` taint effect. Share the same schema as `taint` / `untaint` but default `effect` to `"block"`.

**agentstategraph_watch** / **agentstategraph_unwatch**

Shorthand for a `Watch` taint (effect: `"advisory"`). Triggers audit notifications when the watched path is accessed.

**agentstategraph_list_taints**
```json
{
  "name": "agentstategraph_list_taints",
  "description": "List taint marks, optionally filtered by path prefix, kind, and resolved status.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "ref":              { "type": "string", "default": "main" },
      "path_prefix":      { "type": "string" },
      "kind":             { "type": "string", "enum": ["taint", "quarantine", "watch"] },
      "include_resolved": { "type": "boolean", "default": false }
    }
  }
}
```

**agentstategraph_check_taint**
```json
{
  "name": "agentstategraph_check_taint",
  "description": "Check whether a path is tainted (exact match or propagating ancestor). Returns all applicable active taints.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "ref":  { "type": "string", "default": "main" },
      "path": { "type": "string" }
    },
    "required": ["path"]
  }
}
```

---

#### 7.1.10 Reminder Tools

**agentstategraph_reminder_create**
```json
{
  "name": "agentstategraph_reminder_create",
  "description": "Create a pull-based reminder for future work. Agents poll remind_me() at checkpoints to receive due items.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "title":        { "type": "string" },
      "instructions": { "type": "string", "description": "Full instructions for execution time" },
      "due_at":       { "type": "string", "format": "date-time" },
      "schedule":     { "type": "string", "description": "'once' | 'interval:<secs>' | 'daily:HH:MM' | 'weekly:Weekday:HH:MM'" },
      "priority":     { "type": "string", "enum": ["critical","high","medium","low","minimal"], "default": "medium" },
      "autonomous":   { "type": "boolean", "default": false, "description": "true = execute without permission; false = AwaitingPermission until approve() called" },
      "created_by":   { "type": "string" },
      "commands":     { "type": "array", "items": { "type": "string" } },
      "tags":         { "type": "array", "items": { "type": "string" } },
      "refs": {
        "type": "array",
        "items": {
          "type": "object",
          "properties": {
            "kind":  { "type": "string", "enum": ["branch","memory","plan","task","state_path","external"] },
            "id":    { "type": "string" },
            "label": { "type": "string" }
          },
          "required": ["kind", "id"]
        }
      }
    },
    "required": ["title", "instructions", "due_at"]
  }
}
```

**agentstategraph_reminder_remind_me**
```json
{
  "name": "agentstategraph_reminder_remind_me",
  "description": "The core pull query. Lazily promotes past-due Pending items and expired Snoozed items to Due, then returns all Due and AwaitingPermission items ordered by priority then due date. Call at session start, task transitions, and branch switches.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "created_by": { "type": "string", "description": "Scope to a specific agent or user" }
    }
  }
}
```

**agentstategraph_reminder_snooze**
```json
{
  "name": "agentstategraph_reminder_snooze",
  "description": "Defer a Due or Pending reminder until a later time.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "id":    { "type": "string" },
      "until": { "type": "string", "format": "date-time" }
    },
    "required": ["id", "until"]
  }
}
```

**agentstategraph_reminder_approve**
```json
{
  "name": "agentstategraph_reminder_approve",
  "description": "Approve a reminder in AwaitingPermission status. Transitions it to Due so execution can proceed.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "id":          { "type": "string" },
      "approved_by": { "type": "string" }
    },
    "required": ["id", "approved_by"]
  }
}
```

**agentstategraph_reminder_cancel**
```json
{
  "name": "agentstategraph_reminder_cancel",
  "description": "Cancel a reminder (terminal; cannot be undone).",
  "inputSchema": {
    "type": "object",
    "properties": {
      "id":     { "type": "string" },
      "reason": { "type": "string" }
    },
    "required": ["id"]
  }
}
```

**agentstategraph_reminder_record_execution**
```json
{
  "name": "agentstategraph_reminder_record_execution",
  "description": "Record the result of executing a reminder. On success, if the reminder has a repeating schedule the next due time is computed and status resets to Pending (or AwaitingPermission for non-autonomous). A Once reminder transitions to Completed on success.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "id":           { "type": "string" },
      "agent_id":     { "type": "string" },
      "result":       { "type": "string", "enum": ["success","failed","cancelled"] },
      "started_at":   { "type": "string", "format": "date-time" },
      "completed_at": { "type": "string", "format": "date-time" },
      "approved_by":  { "type": "string" },
      "notes":        { "type": "string" },
      "task_id":      { "type": "string" }
    },
    "required": ["id", "agent_id", "result", "started_at", "completed_at"]
  }
}
```

**agentstategraph_reminder_list**
```json
{
  "name": "agentstategraph_reminder_list",
  "description": "List reminders with optional filtering.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "status":           { "type": "string", "enum": ["Pending","Due","AwaitingPermission","InProgress","Completed","Snoozed","Cancelled"] },
      "priority_at_most": { "type": "string", "enum": ["critical","high","medium","low","minimal"] },
      "created_by":       { "type": "string" },
      "due_before":       { "type": "string", "format": "date-time" },
      "ref_id":           { "type": "string" },
      "tags":             { "type": "array", "items": { "type": "string" } }
    }
  }
}
```

---

### 7.2 Resources

AgentStateGraph exposes state as MCP resources with the `agentstategraph://` URI scheme.

| URI Pattern | Description |
|-------------|-------------|
| `agentstategraph://state/{ref}` | Full state tree at a ref |
| `agentstategraph://state/{ref}/{path}` | Value at a specific path |
| `agentstategraph://diff/{ref_a}..{ref_b}` | Structured diff between two refs |
| `agentstategraph://log/{ref}` | Commit log (supports query params for filtering) |
| `agentstategraph://branches` | List of all branches |
| `agentstategraph://tags` | List of all tags |
| `agentstategraph://schema` | Current schema (if defined) |
| `agentstategraph://proposals` | Open merge proposals |
| `agentstategraph://sessions` | Active agent sessions |

### 7.3 Events

AgentStateGraph emits structured events that MCP clients can subscribe to:

| Event | Payload | Use Case |
|-------|---------|----------|
| `commit` | Commit object | Reactive agents watching for state changes |
| `intent.status_changed` | Intent + new status | Tracking intent lifecycle |
| `intent.resolved` | Intent + Resolution + NotificationPolicy | Triggering notifications |
| `proposal.created` | MergeProposal | Alerting reviewers |
| `proposal.status_changed` | MergeProposal + new status | Tracking review progress |
| `conflict.detected` | Vec\<Conflict\> | AI-assisted conflict resolution |

---

## 8. Taint, Quarantine, and Watch

Mark-and-sweep safety controls that let agents and operators flag suspicious or sensitive state paths without blocking all access.

### 8.1 Taint Marks

A `TaintMark` attaches to a state path and carries metadata about why the path is flagged:

| Field | Type | Description |
|-------|------|-------------|
| `id` | `String` | Unique identifier for the mark |
| `path` | `String` | JSON-path the mark applies to (e.g., `/cluster/nodes/0/credentials`) |
| `kind` | `TaintKind` | `Taint` \| `Quarantine` \| `Watch` |
| `severity` | `TaintSeverity` | `Low` \| `Medium` \| `High` \| `Critical` |
| `effect` | `TaintEffect` | `Warn` \| `Block` \| `Audit` |
| `reason` | `String` | Human/agent-readable explanation |
| `created_by` | `String` | Agent or user who applied the mark |
| `created_at` | `DateTime` | Timestamp |
| `expires_at` | `Option<DateTime>` | Optional automatic expiry |
| `propagate` | `bool` | Whether descendants inherit the mark (default: `true`) |
| `resolved` | `Option<TaintResolution>` | Set when the mark is resolved; never deleted |

**TaintKind semantics:**

| Kind | Meaning | Typical Use |
|------|---------|-------------|
| `Taint` | Data at this path is suspicious or unverified | Flagging externally-sourced data before review |
| `Quarantine` | Changes to this path are blocked until resolved | Locking a path after a security incident |
| `Watch` | All access to this path triggers an audit notification | Monitoring sensitive fields (credentials, PII) |

### 8.2 Propagation

Taints propagate to descendant paths by default (`propagate = true`). `check_taint` returns all applicable taints for a request path, including ancestor matches.

```
path: /cluster/nodes/0/credentials  → TaintKind::Quarantine
check_taint(path="/cluster/nodes/0/credentials/token")
  → returns the ancestor Quarantine mark (propagated)
```

This means a single mark on a parent path protects the entire subtree without enumerating every child.

### 8.3 Resolution

Taints are **resolved**, not deleted. This preserves the audit trail. A `TaintResolution` stamps the mark with:

```rust
struct TaintResolution {
    resolved_by: String,
    reason:      String,
    proof:       Option<String>,  // e.g., ticket URL, commit hash
    resolved_at: DateTime,
}
```

Resolved taints remain visible in `list_taints` and `check_taint` responses (with `resolved` set), so auditors can see the full history of what was flagged and when.

### 8.4 Policy Integration

`policy_evaluate_change_with_taints` combines policy and taint evaluation in a single call:

1. If any path in the request is `Quarantine`-marked, the call returns `Denied` immediately — policy evaluation is skipped entirely.
2. Otherwise, policy evaluation proceeds normally. Active `Watch` marks generate audit notifications as a side effect.
3. Active `Taint` marks may increase the cost-of-change score for AI token-budget policies.

This layering means quarantine is a hard gate that even a permissive policy cannot bypass.

### 8.5 MCP Tools

9 tools: `taint_apply`, `taint_resolve`, `quarantine_apply`, `quarantine_resolve`, `watch_apply`, `watch_resolve`, `taint_list`, `taint_check`, `taint_evaluate_change_with_taints`.

`quarantine_apply` and `watch_apply` are convenience shorthands for `taint_apply` with `kind = Quarantine` and `kind = Watch` respectively.

---

## 9. Authorization Policy

Declarative, multi-engine access control with tamper-evident ratification. Policies cannot be silently modified — every transition is an audit commit.

### 9.1 Policy Lifecycle

```
Proposed ──ratify()──▶ Active ──supersede()──▶ Superseded
                              │
                              └── policy_evaluate() queries Active policies only
```

| Status | Description |
|--------|-------------|
| `Proposed` | Draft submitted for review. Not yet in effect. |
| `Active` | Ratified and enforcing. |
| `Superseded` | Replaced by a newer policy. Retained for audit. |

Every status transition creates an immutable audit commit with the agent identity, timestamp, and optional rationale.

### 9.2 Evaluation Engines

The engine is specified per-policy at proposal time and stored with the policy record:

| Engine | Kind string | Description |
|--------|-------------|-------------|
| Built-in rule engine | `"builtin"` | JSON-expressed allow/deny rules; no external dependencies |
| Rego (OPA) | `"rego"` | Open Policy Agent evaluation via embedded OPA |
| WASM | `"wasm"` | Any language compiled to WASM; evaluated in a sandboxed WASM host |
| Cedar | `"cedar"` | Amazon Cedar; expressive policies with explicit deny |

The `agentstategraph-policy` crate dispatches to the appropriate engine at evaluation time. Adding a new engine is a crate-level extension with no changes to the storage layer or MCP tools.

### 9.3 Ed25519 Signing

Policies can be signed with Ed25519 keys before ratification. `policy_sign` canonicalizes the policy (sorted JSON, `signature` field excluded) before signing. `policy_verify` checks all signatures.

Ratification requires valid signatures from all required signers as declared in the policy's `required_signers` list. Attempting to ratify without all signatures returns an error — the policy stays in `Proposed`.

```json
{
  "id": "policy-001",
  "engine": "builtin",
  "rules": [...],
  "required_signers": ["agent:orchestrator", "user:admin"],
  "signatures": {
    "agent:orchestrator": "<base64-ed25519-sig>",
    "user:admin": "<base64-ed25519-sig>"
  }
}
```

### 9.4 Cost-of-Change Gating

Policies can encode token-cost thresholds for AI agents operating within budget constraints:

```json
{
  "cost_of_change": {
    "threshold_tokens": 1000,
    "fallback_action": "Escalate"
  }
}
```

When `policy_check_tokens` is called with an estimated token cost, the policy evaluates whether the cost exceeds the threshold and returns the appropriate `fallback_action`:

| Fallback action | Meaning |
|-----------------|---------|
| `Allow` | Proceed regardless of cost |
| `Deny` | Block the operation |
| `Escalate` | Route to a higher-authority agent or human |
| `RequestApproval` | Pause and require explicit approval before proceeding |

This integrates naturally with the MergeProposal approval gate pattern (§12.2).

### 9.5 MCP Tools

12 tools: `policy_propose`, `policy_ratify`, `policy_sign`, `policy_verify`, `policy_supersede`, `policy_show`, `policy_list`, `policy_history`, `policy_evaluate`, `policy_evaluate_change`, `policy_check_tokens`, `policy_evaluate_change_with_taints`.

---

## 10. Reminders

Pull-based scheduled work for agents and users. Reminders are explicit checkpoints — agents must call `remind_me()` to receive due reminders. There are no background timers or push delivery.

### 10.1 Pull Model

```
Agent starts session
  │
  └─▶ reminder_remind_me(agent_id=..., branch=...) ──▶ due reminders returned
        │
        (agent acts on reminders)
        │
        └─▶ reminder_record_execution(id=..., result=Success, notes=...)
```

**Lazy status promotion:** `remind_me()` scans `Pending` items and promotes past-due ones to `Due` on each call. There is no scheduler process — status is computed lazily at poll time.

### 10.2 Priority and Scheduling

Five priority levels control which reminders are surfaced first:

| Priority | Value | Use |
|----------|-------|-----|
| `Critical` | 0 | Must not be missed; escalated if skipped |
| `High` | 1 | Important; surface at every checkpoint |
| `Medium` | 2 | Default priority |
| `Low` | 3 | Background; surface when idle |
| `Minimal` | 4 | Nice-to-have; surface rarely |

Four schedule kinds:

| Kind | Description | Example |
|------|-------------|---------|
| `Once` | Fire once at a specific `DateTime` | One-time migration reminder |
| `Interval` | Fire every N seconds after last execution | Every 3600s health check |
| `Daily` | Fire at HH:MM UTC every day | Daily summary at 09:00 |
| `Weekly` | Fire at Weekday + HH:MM UTC | Weekly triage every Monday 08:00 |

After a successful execution, the next due time is computed from the schedule and status resets to `Pending` automatically.

### 10.3 Autonomous Flag

Each reminder carries an `autonomous: bool` flag:

- `true` — Agent executes the reminder without asking. Suitable for routine, low-risk tasks.
- `false` — Reminder transitions to `AwaitingPermission` status. Execution is blocked until `reminder_approve()` is called by a human or higher-authority agent.

This integrates with the graduated trust model (§12.5): an agent in a restricted authorization tier may have all reminders in non-autonomous mode until promoted.

### 10.4 Soft References

A reminder can carry advisory refs to related objects:

```rust
struct ReminderRef {
    kind:       RefKind,   // Branch | Memory | Plan | Task | StatePath | ExternalUrl
    target:     String,    // The ref target (branch name, plan id, URL, etc.)
    label:      String,    // Captured at creation time; survives renames
    stale:      bool,      // Set lazily if target can no longer be resolved
}
```

The `stale` flag is set lazily at `remind_me()` time if the target cannot be resolved. A stale ref does not invalidate the reminder — it is informational, prompting the agent to update or clean up the dead reference.

### 10.5 Execution Audit

Every execution attempt is recorded in an append-only `executions` list on the reminder:

```rust
struct ExecutionRecord {
    started_at:  DateTime,
    finished_at: DateTime,
    agent_id:    String,
    result:      ExecutionResult,  // Success | Failed | Cancelled
    notes:       Option<String>,
    task_id:     Option<String>,   // Links to a Task if execution created one
}
```

The reminder is never overwritten. History accumulates indefinitely, giving auditors a complete record of when the reminder was acted on and by whom.

### 10.6 MCP Tools

7 tools: `reminder_create`, `reminder_list`, `reminder_remind_me`, `reminder_snooze`, `reminder_approve`, `reminder_cancel`, `reminder_record_execution`.

---

## 11. Architecture

### 11.1 Crate Structure

The workspace contains 12 Rust crates organized by responsibility:

| Crate | Responsibility |
|-------|---------------|
| `agentstategraph-core` | Object model, types, content-addressed DAG, diff, merge, BLAKE3. Zero I/O. |
| `agentstategraph-storage` | 7-trait `Storage` supertrait + four backends: Memory, SQLite, Postgres, IndexedDB |
| `agentstategraph` | High-level Repository API: repo, session, speculation, watch, query |
| `agentstategraph-mcp` | MCP server (73 tools), HTTP REST API, `migrate` CLI subcommand |
| `agentstategraph-tasks` | Shared Plan/Task state machine, proofs, assignment |
| `agentstategraph-migrate` | Schema-evolution framework and migration registry |
| `agentstategraph-policy` | Authorization engine: built-in rules, Rego, WASM, Cedar; cost-of-change gating |
| `agentstategraph-policy-sign` | Ed25519 key generation, signing, and verification for policy ratification |
| `agentstategraph-policy-wasm` | WASM host runner for WASM-compiled policy evaluation |
| `agentstategraph-policy-cedar` | Amazon Cedar policy engine integration |
| `agentstategraph-taint` | Taint/Quarantine/Watch mark-and-sweep safety primitives + TaintStore trait |
| `agentstategraph-reminders` | Pull-based reminders: priority, schedules, soft refs, autonomous flag, audit |
| `agentstategraph-ffi` | C ABI surface shared by Go and .NET bindings |
| `agentstategraph-wasm` | wasm-bindgen bindings for browser/Deno/Node |

```
AgentStateGraph/
├── crates/
│   ├── agentstategraph-core/        # Types, DAG, diff, merge, BLAKE3. Zero I/O.
│   │
│   ├── agentstategraph-storage/     # Storage supertrait + backends
│   │   ├── traits.rs           # Storage supertrait (7 sub-traits combined)
│   │   ├── memory.rs           # In-memory backend (full supertrait)
│   │   ├── sqlite.rs           # SQLite backend (full supertrait)
│   │   ├── postgres.rs         # Postgres backend (full supertrait, tenant-isolated)
│   │   └── indexeddb.rs        # IndexedDB backend (WASM, delegates to memory)
│   │
│   ├── agentstategraph/        # High-level API
│   │   ├── repo.rs             # Repository handle
│   │   ├── session.rs          # Agent sessions
│   │   ├── speculation.rs      # Speculative execution
│   │   ├── watch.rs            # Watch/subscribe system
│   │   └── query.rs            # Intent queries, search, blame
│   │
│   ├── agentstategraph-mcp/         # MCP server: 73 tools + HTTP REST + migrate CLI
│   ├── agentstategraph-tasks/       # Plan/Task state machine, proofs, assignment
│   ├── agentstategraph-migrate/     # Schema-evolution framework
│   ├── agentstategraph-policy/      # Authorization + cost-of-change gating
│   ├── agentstategraph-policy-sign/ # Ed25519 signing for policy ratification
│   ├── agentstategraph-policy-wasm/ # WASM host runner for policy evaluation
│   ├── agentstategraph-policy-cedar/# Amazon Cedar engine integration
│   ├── agentstategraph-taint/       # Taint/Quarantine/Watch + TaintStore
│   ├── agentstategraph-reminders/   # Pull-based reminders + ReminderStore
│   ├── agentstategraph-ffi/         # C ABI surface (Go, .NET)
│   └── agentstategraph-wasm/        # wasm-bindgen (browser/Deno/Node)
│
├── bindings/
│   ├── python/                 # PyO3 + maturin
│   ├── typescript/             # napi-rs
│   ├── go/                     # CGo wrapping agentstategraph-ffi
│   └── dotnet/                 # C FFI wrapping agentstategraph-ffi
│
├── spec/
│   └── AGENTSTATEGRAPH-RFC.md  # This document
│
└── examples/
    ├── cluster-management/     # Multi-node cluster state (multi-node edge cluster)
    ├── multi-agent/            # Three agents coordinating on shared state
    └── creative-app/           # Human-AI collaborative state editing
```

### 11.2 Storage Traits

The `Storage` supertrait combines 7 sub-traits. Every backend must satisfy all 7. `TaintStore` and `ReminderStore` provide default no-op implementations so custom backends can opt in gradually.

```rust
/// The combined Storage supertrait — satisfied by all four backends.
pub trait Storage:
    ObjectStore + CommitStore + RefStore + EpochStore
    + SessionStore + TaintStore + ReminderStore
    + Send + Sync + 'static
{}

/// Content-addressed object storage
trait ObjectStore: Send + Sync {
    fn get(&self, id: &ObjectId) -> Result<Option<Object>>;
    fn put(&self, obj: &Object) -> Result<ObjectId>;
    fn exists(&self, id: &ObjectId) -> Result<bool>;
    fn batch_get(&self, ids: &[ObjectId]) -> Result<Vec<Option<Object>>>;
    fn batch_put(&self, objs: &[Object]) -> Result<Vec<ObjectId>>;
    fn gc(&self, reachable: &HashSet<ObjectId>) -> Result<usize>;
}

/// Commit storage
trait CommitStore: Send + Sync {
    fn get_commit(&self, id: &ObjectId) -> Result<Option<Commit>>;
    fn put_commit(&self, commit: &Commit) -> Result<ObjectId>;
    fn list_commits(&self, branch: &str, limit: usize) -> Result<Vec<Commit>>;
}

/// Named ref management with atomic CAS
trait RefStore: Send + Sync {
    fn get_ref(&self, name: &str) -> Result<Option<ObjectId>>;
    fn set_ref(&self, name: &str, target: ObjectId) -> Result<()>;
    fn cas_ref(&self, name: &str, expected: ObjectId, new: ObjectId) -> Result<bool>;
    fn list_refs(&self, prefix: &str) -> Result<Vec<(String, ObjectId)>>;
    fn delete_ref(&self, name: &str) -> Result<bool>;
}

/// Epoch lifecycle management
trait EpochStore: Send + Sync {
    fn create_epoch(&self, epoch: &Epoch) -> Result<()>;
    fn get_epoch(&self, id: &str) -> Result<Option<Epoch>>;
    fn seal_epoch(&self, id: &str) -> Result<()>;
    fn list_epochs(&self) -> Result<Vec<Epoch>>;
}

/// Agent session management
trait SessionStore: Send + Sync {
    fn create_session(&self, session: &Session) -> Result<()>;
    fn get_session(&self, id: &str) -> Result<Option<Session>>;
    fn update_session(&self, session: &Session) -> Result<()>;
    fn list_sessions(&self) -> Result<Vec<Session>>;
    fn delete_session(&self, id: &str) -> Result<bool>;
}

/// Taint/Quarantine/Watch mark-and-sweep (default: no-op)
trait TaintStore: Send + Sync {
    fn save_taint(&self, taint: &TaintMark) -> Result<(), TaintError> { Ok(()) }
    fn get_taint(&self, id: &str) -> Result<Option<TaintMark>, TaintError> { Ok(None) }
    fn list_taints(&self, filter: &TaintFilter) -> Result<Vec<TaintMark>, TaintError> { Ok(vec![]) }
    fn resolve_taint(&self, id: &str, resolution: TaintResolution) -> Result<(), TaintError> { Ok(()) }
}

/// Pull-based reminders (default: no-op)
trait ReminderStore: Send + Sync {
    fn save(&self, reminder: &Reminder) -> Result<(), ReminderError> { Ok(()) }
    fn get(&self, id: &str) -> Result<Option<Reminder>, ReminderError> { Ok(None) }
    fn update(&self, reminder: &Reminder) -> Result<(), ReminderError> { Ok(()) }
    fn delete(&self, id: &str) -> Result<bool, ReminderError> { Ok(false) }
    fn list(&self, filter: &ReminderFilter) -> Result<Vec<Reminder>, ReminderError> { Ok(vec![]) }
}
```

All four backends (Memory, SQLite, Postgres, IndexedDB) fully implement all 7 sub-traits. The Postgres backend is tenant-isolated: every query includes a `tenant_id` column so multiple logical stores can share a single database instance.

### 11.3 Performance Design

| Aspect | Target | Technique |
|--------|--------|-----------|
| Branch creation | O(1) | Copy-on-write: new branch is just a ref pointing to existing commit |
| Hashing | >1 GB/s | BLAKE3 (parallelizable, SIMD-optimized) |
| Speculation create/discard | O(1) | Arena allocation: all speculation objects in a single arena, freed in one operation |
| State materialization | Lazy | Only load objects along the accessed path, not the entire tree |
| Deduplication | Automatic | Content-addressing: identical subtrees share storage |
| Batch operations | Amortized | batch_get/batch_put reduce I/O round trips |

### 11.4 Language Bindings

| Language | Technology | Distribution |
|----------|-----------|-------------|
| **Python** | PyO3 + maturin | `pip install agentstategraph` |
| **TypeScript/Node** | napi-rs | `npm install agentstategraph` |
| **Go** | CGo wrapping `agentstategraph-ffi` | `go get github.com/agentstatelabs/agentstategraph/bindings/go` |
| **.NET** | C FFI wrapping `agentstategraph-ffi` | NuGet package `AgentStateGraph` |
| **Browser/Deno** | wasm-bindgen (`agentstategraph-wasm`) | npm or direct WASM import |
| **Any (C ABI)** | `agentstategraph-ffi` | Shared library (`.so`/`.dylib`/`.dll`) |

---

## 12. Human-Agent Collaboration

### 12.1 Shared Interface

Humans and agents use the same API. A human's identity is their `agent_id`. There is no separate "admin" interface — authority and intent metadata distinguish human actions from agent actions.

### 12.2 Approval Gates

The merge proposal system creates natural checkpoints where human review is required:

1. Agent works on `agents/planner/workspace`
2. Agent creates a MergeProposal targeting `main`
3. Agent files a Resolution: summary, deviations, outcome, confidence
4. Human reviews the structured diff (typed changes, not text patches)
5. Human approves, rejects, or requests changes
6. If approved, changes merge to `main` with full provenance

### 12.3 Transparency

Every state change is traceable through:
- **Who** performed the action (`agent_id`)
- **Who authorized it** (`authority` with delegation chain)
- **Why** (`intent` with category, description, tags)
- **How they decided** (`reasoning`, `confidence`)
- **What tools they used** (`tool_calls`)
- **What was accomplished** (`resolution` with deviations)
- **Who was notified** (`notification` policy)

This makes agent behavior auditable without requiring access to ephemeral conversation logs.

### 12.4 Web UI (Future)

A web interface powered by the WASM build would provide:
- **State explorer**: browse state at any ref, navigate the tree
- **DAG visualizer**: interactive commit graph with intent annotations
- **Intent timeline**: chronological view of intents with lifecycle status
- **Diff viewer**: structured, typed diffs between any two refs
- **Approval queue**: pending merge proposals with resolution summaries
- **Agent activity**: which agents are active, what they're working on, what they've done

### 12.5 Graduated Trust and Enterprise Adoption

Organizations adopting agentic AI face a fundamental tension: agents are most valuable when given autonomy, but autonomy without visibility is unacceptable to compliance teams, security teams, and leadership. The typical result is that organizations either don't adopt agents or adopt them without adequate oversight.

AgentStateGraph resolves this by making **visibility scale with autonomy**. As agents are given more freedom, the provenance record becomes richer, not sparser. The authority and delegation model supports a graduated trust progression:

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

## 13. Lifecycle Management: Epochs and the Registry

### 13.1 The Growth Problem

A long-lived AgentStateGraph instance accumulates history fast. A production cluster managed by multiple agents over weeks generates thousands of commits, hundreds of intents, and dozens of agent sessions. Without lifecycle management, the store becomes:

- **Unwieldy to query** — searching all history for a specific incident means scanning everything
- **Expensive to load** — agents pulling context don't need last quarter's migration history
- **Hard to audit** — compliance needs bounded, self-contained units of work, not an infinite scroll
- **Risky to keep mutable** — historical records should be tamper-evident, not editable

### 13.2 Epochs

An **Epoch** is a bounded, sealable segment of work within a AgentStateGraph instance. It groups related commits, intents, agent sessions, and resolutions into a coherent unit that can be managed as a whole.

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

This bundle is independently verifiable: anyone with the bundle can recompute the Merkle tree and confirm it matches the seal hash. No access to the live AgentStateGraph instance is needed.

Use cases for exported bundles:
- **Compliance audits**: hand the bundle to auditors as a tamper-evident record
- **Incident review**: share the full context of an incident across teams
- **Knowledge transfer**: onboard a new agent or team with the complete history of a body of work
- **Legal discovery**: produce a cryptographically verifiable record of AI-driven actions

#### 10.2.4 Archiving an Epoch

Sealed epochs can be **archived** — moved to cold storage while keeping a lightweight index entry:

```
archive_epoch(id: "2026-Q1-storage-migration", destination: "s3://audits/agentstategraph/")
```

Archiving:
1. Exports the epoch bundle to the specified destination
2. Removes the full objects from the active store (keeping only the index entry)
3. Retains the epoch's registry entry, seal hash, and cross-references
4. Epoch remains queryable via the registry ("what happened in Q1?") but full details require loading the archive

### 13.3 The Registry

The **Registry** is a lightweight master index stored at `/__registry__` in the state tree. It provides a navigable overview of all work in the AgentStateGraph instance without requiring agents or humans to scan the full commit history.

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

### 13.4 MCP Tools for Lifecycle Management

**agentstategraph_create_epoch**
```json
{
  "name": "agentstategraph_create_epoch",
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

**agentstategraph_seal_epoch**
```json
{
  "name": "agentstategraph_seal_epoch",
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

**agentstategraph_export_epoch**
```json
{
  "name": "agentstategraph_export_epoch",
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

**agentstategraph_list_epochs**
```json
{
  "name": "agentstategraph_list_epochs",
  "description": "List epochs in the registry with optional filters. Use this to understand the history of work in the AgentStateGraph instance.",
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

**agentstategraph_epoch_references**
```json
{
  "name": "agentstategraph_epoch_references",
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

## 14. Reference Implementation

The specification is only valuable if it can be implemented and used easily — by humans reading the spec and by AI agents reading the MCP tool schemas. This section defines the two primary deliverables beyond the spec itself.

### 14.1 Principles

1. **Progressive complexity** — Start with the simplest useful subset. A developer or agent should be productive in minutes, not days.
2. **Layered adoption** — Each capability layer (branching, schema, speculation, sub-agents, epochs) is independently useful. You don't need epochs to use branching. You don't need sub-agent orchestration to use intents.
3. **Zero config to start** — The default configuration should work out of the box. SQLite storage (durable, single file), no schema, single agent. Add complexity only when needed.
4. **Spec-faithful** — The reference implementation must match the spec exactly. It is the canonical proof that the spec is implementable and coherent.

### 14.2 Rust Reference Library (`agentstategraph`)

The core library, implemented in Rust, provides the complete AgentStateGraph API as described in this spec.

**Distribution:**
- Rust: `cargo add agentstategraph`
- Python: `pip install agentstategraph`
- TypeScript/Node: `npm install agentstategraph`
- Go: `go get github.com/agentstatelabs/agentstategraph/bindings/go`

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

### 14.3 MCP Server (`agentstategraph-mcp`)

A standalone MCP server that any MCP-compatible agent (Claude, GPT, open-source agents) can connect to immediately.

**Distribution and startup:**
```bash
# npm (zero install)
npx agentstategraph-mcp

# pip
pip install agentstategraph-mcp
agentstategraph-mcp

# Binary (from releases)
./agentstategraph-mcp

# Docker
docker run -p 3000:3000 agentstategraph/mcp-server
```

**Configuration:**
```json
{
  "storage": "sqlite",
  "path": "./agentstategraph.db",
  "transport": "stdio"
}
```

That's the default — durable SQLite storage in a single file, zero external dependencies. All history, intents, and provenance survive process restarts. For ephemeral/testing use, set `"storage": "memory"`. Schema, authority, and epochs are enabled by adding their config sections — absent means off.

**For Claude Code / MCP client configuration:**
```json
{
  "mcpServers": {
    "agentstategraph": {
      "command": "npx",
      "args": ["agentstategraph-mcp"]
    }
  }
}
```

An agent connecting for the first time discovers all 66 available tools via the MCP tool listing. The tool descriptions (§7) are written so that an agent reading them can understand what each tool does and how to use it without external documentation.

The server also ships a `migrate` CLI subcommand for applying storage schema migrations:

```bash
agentstategraph-mcp migrate --storage sqlite --path ./agentstategraph.db
```

### 14.4 Getting Started Example

A complete example that demonstrates the core workflow, suitable for inclusion in the README and as an agent's first interaction with AgentStateGraph:

```
# Agent connects to agentstategraph-mcp and runs:

# 1. Set initial state
agentstategraph_set(ref="main", path="/app/name", value="my-project",
  intent_category="Checkpoint", intent_description="Initialize project state")

# 2. Create a branch to try something
agentstategraph_branch(name="explore/new-feature", from="main")

# 3. Make changes on the branch
agentstategraph_set(ref="explore/new-feature", path="/app/feature_flags/dark_mode", value=true,
  intent_category="Explore", intent_description="Try enabling dark mode",
  reasoning="User requested dark mode support. Adding feature flag first to test.")

# 4. Compare with main
agentstategraph_diff(ref_a="main", ref_b="explore/new-feature")
# Returns: [{ op: "AddKey", path: "/app/feature_flags", key: "dark_mode", value: true }]

# 5. Merge if happy
agentstategraph_merge(source="explore/new-feature", target="main")

# 6. Check history
agentstategraph_log(ref="main", limit=5)
# Returns commits with full intent, reasoning, and metadata
```

This example works with zero configuration — SQLite storage (durable by default), no schema, single agent. Every concept introduced in the spec (branching, intents, structured diff, merge) is demonstrated in 6 tool calls. History survives restarts.

### 14.5 Implementation Test Suite

The reference implementation includes 848+ tests across 12 crates:

| Test Suite | Crate | What It Validates |
|-----------|-------|-------------------|
| `test_objects` | `agentstategraph-core` | Content-addressing, deduplication, canonical serialization |
| `test_commits` | `agentstategraph-core` | Commit creation, intent metadata, authority chains |
| `test_branches` | `agentstategraph` | Branch create/delete/list, namespace conventions |
| `test_diff` | `agentstategraph-core` | All DiffOp variants, schema-aware diffing |
| `test_merge` | `agentstategraph-core` | Auto-resolution, conflict detection, all merge hint strategies |
| `test_speculation` | `agentstategraph` | Create, modify, compare, commit, discard |
| `test_sessions` | `agentstategraph` | Multi-session concurrency, CAS, sync |
| `test_delegation` | `agentstategraph` | Sub-agent spawning, scoped authority, path restrictions |
| `test_intent_tree` | `agentstategraph` | Intent decomposition, lifecycle transitions, resolution reporting |
| `test_epochs` | `agentstategraph-storage` | Create, seal, export, archive, registry queries, cross-references |
| `test_storage_backends` | `agentstategraph-storage` | Memory, SQLite, Postgres — full supertrait coverage per backend |
| `test_tasks` | `agentstategraph-tasks` | Plan/Task state machine, proofs, assignment, next-task selection |
| `test_policy` | `agentstategraph-policy` | Propose/ratify/supersede lifecycle, built-in + Rego + WASM + Cedar engines |
| `test_policy_sign` | `agentstategraph-policy-sign` | Ed25519 key generation, signing, verification, ratification gating |
| `test_taint` | `agentstategraph-taint` | Apply/resolve marks, propagation, quarantine blocking, watch audit |
| `test_reminders` | `agentstategraph-reminders` | Create/list/snooze/approve, priority filtering, schedule next-due computation |
| `test_mcp` | `agentstategraph-mcp` | All 73 MCP tools against the spec, round-trip request/response validation |

A conformance test suite is also provided for third-party implementations: any implementation that passes the conformance suite is spec-compliant.

---

## 15. Open Questions

These questions are deferred for resolution during implementation or future RFCs.

**Resolved in v0.8.0:**

- ~~**Commit signing** (#3)~~ — Resolved via `agentstategraph-policy-sign`: Ed25519 signing is applied to *policies* (which govern the entire delegation chain) rather than individual commits. Policy ratification requires valid signatures from all required signers; the ratification event itself is an audit commit. This provides cryptographic agent identity verification without per-commit signing overhead.

- ~~**Access control** (#7)~~ — Resolved via `agentstategraph-policy`: declarative, multi-engine authorization (built-in, Rego, WASM, Cedar) with tamper-evident ratification and cost-of-change gating. Per-path quarantine via `agentstategraph-taint` provides a hard gate beyond policy evaluation.

**Active:**

1. **Hash function pluggability**: Should BLAKE3 be fixed, or should the hash function be configurable? Fixed simplifies interoperability; pluggable accommodates regulatory requirements (FIPS).

2. **Large value handling**: What is the inline size threshold for objects? Should large values (files, images, model weights) use a chunked storage model similar to git LFS?

3. **Remote sync protocol**: How do distributed AgentStateGraph instances synchronize? A protocol similar to git's pack-based transfer but operating on structured objects rather than files.

4. **Time-travel queries**: Should `query_at(ref, path, timestamp)` be supported natively? This requires temporal indexing and has storage cost implications.

5. **History compaction**: For long-running agents generating thousands of commits, should AgentStateGraph support squashing or pruning history while preserving key checkpoints?

6. **Event sourcing bridge**: Should AgentStateGraph emit events compatible with existing event sourcing infrastructure (Kafka, EventStore), enabling integration with streaming architectures?

7. **Relationship to app-state systems**: AgentStateGraph targets agent workflows. A companion project (exploratory-state-system, Swift) targets AI-native creative applications with simpler versioning semantics. Should there be a shared specification for the overlapping subset?

---

## Appendix A: Comparison with Git Object Model

| Concept | Git | AgentStateGraph |
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
      "tool_name": "agentstategraph_set",
      "arguments": { "ref": "main", "path": "/scheduling/gpu_policy", "value": "memory-aware" },
      "result": "committed",
      "timestamp": "2026-04-04T14:28:00Z"
    }
  ]
}
```

---

*AgentStateGraph RFC-0001 — Stable, updated 2026-05-02*
