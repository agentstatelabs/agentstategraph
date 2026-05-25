---
title: MCP Tools Reference
description: Complete reference for all AgentStateGraph MCP tools with parameters and examples.
---

> **73 tools** — 4 core state · 7 branching/speculation · 10 query/blame/explore · 4 namespaces · 7 epochs · 4 sessions · 10 plans/tasks · 12 policy · 8 taint · 7 reminders. Also available as HTTP REST endpoints via `--http` mode. The `agentstategraph-mcp` binary additionally offers a [`migrate` subcommand](/guides/mcp-server/) for schema upgrades — it's a one-shot CLI, not an MCP tool.
>
> **Namespace override:** the 17 ref-touching tools (`get`, `set`, `delete`, `branch`, `list_branches`, `merge`, `log`, `diff`, `speculate`, `query`, `blame`, `list_paths`, `get_tree`, `search`, `stats`, `commit_graph`, `intent_tree`) also accept an optional `namespace` field that overrides the server's configured namespace for that call. See [Namespaces](/guides/namespaces/).

## State Operations

### agentstategraph_get

Read a value from state at any branch, tag, or commit.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch, tag, or commit ID |
| `path` | string | yes | | JSON path (e.g., `/nodes/0/status`). Use `/` for entire state. |

**Example input:**
```json
{ "ref": "main", "path": "/cluster/name" }
```

**Example output:**
```json
"prod"
```

---

### agentstategraph_set

Write a value to state, creating a new atomic commit with intent metadata.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch to commit to |
| `path` | string | yes | | JSON path to set |
| `value` | any | yes | | JSON value to write |
| `intent_category` | string | yes | | `Explore`, `Refine`, `Fix`, `Rollback`, `Checkpoint`, `Merge`, `Migrate`, `Plan`, `Taint`, `Untaint`, `Quarantine`, `Unquarantine`, `Watch`, `Unwatch`, `PolicyPropose`, `PolicyRatify`, `PolicySupersede`, `PolicySign`, or `Custom:<value>` |
| `intent_description` | string | yes | | Why this change is being made |
| `reasoning` | string | no | | Agent's chain-of-thought |
| `confidence` | number | no | | Self-assessed confidence (0.0-1.0) |
| `tags` | string[] | no | | Queryable tags |

**Example input:**
```json
{
  "path": "/cluster/replicas",
  "value": 3,
  "intent_category": "Refine",
  "intent_description": "Scale to 3 replicas",
  "reasoning": "Traffic increased 40% over last hour",
  "confidence": 0.85,
  "tags": ["scaling", "auto"]
}
```

**Example output:**
```
Committed: a1b2c3d4
```

---

### agentstategraph_delete

Remove a value from state, creating a new commit.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch |
| `path` | string | yes | | JSON path to delete |
| `intent_category` | string | yes | | Intent category |
| `intent_description` | string | yes | | Why this deletion |

**Example input:**
```json
{
  "path": "/cluster/deprecated_config",
  "intent_category": "Fix",
  "intent_description": "Remove deprecated config field"
}
```

**Example output:**
```
Deleted and committed: e5f6g7h8
```

---

## Branch Operations

### agentstategraph_branch

Create a new branch from any ref. Supports namespaced names.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `name` | string | yes | | Branch name (supports `/` namespacing) |
| `from` | string | no | `"main"` | Ref to branch from |

**Example input:**
```json
{ "name": "agents/planner/workspace", "from": "main" }
```

**Example output:**
```
Branch 'agents/planner/workspace' created at a1b2c3d4
```

---

### agentstategraph_list_branches

List all branches, optionally filtered by namespace prefix.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `prefix` | string | no | | Namespace prefix filter |

**Example input:**
```json
{ "prefix": "agents/" }
```

**Example output:**
```
2 branches:
  agents/planner/workspace -> a1b2c3d4
  agents/executor/workspace -> e5f6g7h8
```

---

### agentstategraph_merge

Merge source branch into target. Uses schema-aware merge. Returns conflicts if auto-resolution fails.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `source` | string | yes | | Branch to merge from |
| `target` | string | no | `"main"` | Branch to merge into |
| `intent_description` | string | yes | | Why this merge |
| `reasoning` | string | no | | Reasoning for merge |

**Example input:**
```json
{
  "source": "feature/new-network",
  "target": "main",
  "intent_description": "Adopt flannel network config",
  "reasoning": "Lower overhead than calico in benchmarks"
}
```

**Example output (success):**
```
Merged 'feature/new-network' into 'main': i9j0k1l2
```

**Example output (conflict):**
```json
CONFLICTS (1):
[
  {
    "path": "/cluster/network/dns",
    "base": "8.8.8.8",
    "ours": "1.1.1.1",
    "theirs": "9.9.9.9"
  }
]
```

---

### agentstategraph_diff

Structured diff between two refs. Returns typed DiffOps, not text diffs.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref_a` | string | yes | | First ref |
| `ref_b` | string | yes | | Second ref |

**Example input:**
```json
{ "ref_a": "main", "ref_b": "feature/v2" }
```

**Example output:**
```json
2 changes:
[
  { "op": "SetValue", "path": "/app/version", "value": "2.0" },
  { "op": "AddKey", "path": "/app/features/dark-mode", "value": true }
]
```

---

## Namespaces

Namespaces are ref-layer isolation boundaries. Branches in different namespaces are invisible to each other and can share names. See the [Namespaces guide](/guides/namespaces/) for the full model.

### agentstategraph_create_namespace

Create a namespace (idempotent).

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `name` | string | yes | | Namespace name (alphanumeric + `-_`, max 64 chars) |

**Example output:**
```
Namespace 'acme' created
```

---

### agentstategraph_list_namespaces

List all namespaces in the repository.

**Parameters:** None.

**Example output:**
```json
["default", "acme", "globex"]
```

---

### agentstategraph_delete_namespace

Delete a namespace and all of its refs. Cannot delete `default`. Irreversible.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `name` | string | yes | | Namespace to delete |

**Example output:**
```
Namespace 'globex' deleted (12 refs removed)
```

---

### agentstategraph_cross_namespace_merge

Merge a branch from another namespace into a branch in the active namespace. Policy-gated and audited — **denied by default** when no `PolicyStore` is configured.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `source_namespace` | string | yes | | Namespace to merge from |
| `source_branch` | string | yes | | Branch in the source namespace |
| `target_branch` | string | no | `"main"` | Branch in the active namespace to merge into |
| `intent_description` | string | yes | | Why this cross-namespace merge |

**Example output (denied):**
```
Denied: cross-namespace merge requires an active PolicyStore with a matching grant
```

---

## Speculation

### agentstategraph_speculate

Create a lightweight speculation from a ref. O(1) creation.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `from` | string | no | `"main"` | Ref to speculate from |
| `label` | string | no | | Human-readable label |

**Example input:**
```json
{ "from": "main", "label": "try-ceph-storage" }
```

**Example output:**
```
Speculation created: handle_id=1 (from 'main', label: "try-ceph-storage")
```

---

### agentstategraph_spec_modify

Modify state within a speculation. Changes are isolated until committed.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `handle_id` | number | yes | | Speculation handle ID |
| `operations` | array | yes | | Array of `{op, path, value?}` |

Each operation has:
- `op`: `"set"` or `"delete"`
- `path`: JSON path
- `value`: required for `"set"`

**Example input:**
```json
{
  "handle_id": 1,
  "operations": [
    { "op": "set", "path": "/storage/type", "value": "ceph" },
    { "op": "set", "path": "/storage/replicas", "value": 3 },
    { "op": "delete", "path": "/storage/legacy" }
  ]
}
```

**Example output:**
```
Applied 3 operations to speculation 1
```

---

### agentstategraph_compare

Compare multiple speculations. Returns diffs showing how each diverges from base.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `handle_ids` | number[] | yes | | Speculation handle IDs to compare |

**Example input:**
```json
{ "handle_ids": [1, 2] }
```

**Example output:**
```json
[
  {
    "handle": 1,
    "label": "try-ceph",
    "changes": 2,
    "diff": [
      { "op": "SetValue", "path": "/storage/type", "value": "ceph" },
      { "op": "SetValue", "path": "/storage/replicas", "value": 3 }
    ]
  },
  {
    "handle": 2,
    "label": "try-nfs",
    "changes": 1,
    "diff": [
      { "op": "SetValue", "path": "/storage/type", "value": "nfs" }
    ]
  }
]
```

---

### agentstategraph_commit_spec

Promote a speculation to a real commit on its base branch. The speculation is consumed.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `handle_id` | number | yes | | Speculation handle ID |
| `intent_category` | string | yes | | Intent category |
| `intent_description` | string | yes | | Why this approach was chosen |
| `reasoning` | string | no | | Reasoning |
| `confidence` | number | no | | Confidence (0.0-1.0) |

**Example input:**
```json
{
  "handle_id": 2,
  "intent_category": "Checkpoint",
  "intent_description": "Use NFS storage",
  "reasoning": "Only 2 nodes available, Ceph needs 3+",
  "confidence": 0.9
}
```

**Example output:**
```
Speculation committed: m3n4o5p6
```

---

### agentstategraph_discard

Discard a speculation. All changes freed immediately.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `handle_id` | number | yes | | Speculation handle ID |

**Example input:**
```json
{ "handle_id": 1 }
```

**Example output:**
```
Speculation 1 discarded
```

---

## Query and Audit

### agentstategraph_log

List commits with full intent, reasoning, and metadata.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch or ref |
| `limit` | number | no | `10` | Max commits to return |

**Example input:**
```json
{ "ref": "main", "limit": 3 }
```

**Example output:**
```json
[
  {
    "id": "a1b2c3d4",
    "agent": "mcp-agent",
    "intent": {
      "category": "Refine",
      "description": "Scale to 3 replicas",
      "tags": ["scaling"]
    },
    "reasoning": "Traffic increased 40%",
    "confidence": 0.85,
    "parents": 1,
    "timestamp": "2026-04-06T12:00:00Z"
  }
]
```

---

### agentstategraph_query

Query commits with composable filters. All filters are AND-combined.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch to query |
| `agent_id` | string | no | | Filter by agent |
| `intent_category` | string | no | | Filter by category |
| `tags` | string[] | no | | Filter by tags (all must match) |
| `authority_principal` | string | no | | Filter by authority |
| `reasoning_contains` | string | no | | Full-text search in reasoning |
| `confidence_min` | number | no | | Minimum confidence |
| `confidence_max` | number | no | | Maximum confidence |
| `has_deviations` | boolean | no | | Only results with deviations |
| `limit` | number | no | `20` | Max results |

**Example input:**
```json
{
  "agent_id": "agent/scaler",
  "intent_category": "Refine",
  "confidence_min": 0.8,
  "limit": 5
}
```

**Example output:**
```json
[
  {
    "id": "a1b2c3d4",
    "agent": "agent/scaler",
    "intent": {
      "category": "Refine",
      "description": "Scale to 3 replicas",
      "tags": ["scaling"]
    },
    "reasoning": "Traffic increased 40%",
    "confidence": 0.85,
    "timestamp": "2026-04-06T12:00:00Z"
  }
]
```

---

### agentstategraph_blame

Find which commit last modified a value at a path and why.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch |
| `path` | string | yes | | Path to blame |

**Example input:**
```json
{ "path": "/cluster/replicas" }
```

**Example output:**
```json
{
  "commit_id": "a1b2c3d4",
  "agent": "agent/scaler",
  "intent": {
    "category": "Refine",
    "description": "Scale to 3 replicas"
  },
  "reasoning": "Traffic increased 40%",
  "confidence": 0.85,
  "timestamp": "2026-04-06T12:00:00Z"
}
```

---

## Epochs

### agentstategraph_create_epoch

Create a new epoch to group related work.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `id` | string | yes | | Epoch ID (e.g., `"2026-04-incident-node3"`) |
| `description` | string | yes | | Description |
| `root_intents` | string[] | yes | | Root intent IDs that define this epoch |

**Example input:**
```json
{
  "id": "2026-04-incident-node3",
  "description": "Node3 failure recovery",
  "root_intents": ["intent-001", "intent-002"]
}
```

**Example output:**
```
Epoch '2026-04-incident-node3' created (status: Open)
```

---

### agentstategraph_seal_epoch

Seal an epoch, making it read-only and tamper-evident. Cannot be undone.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `id` | string | yes | | Epoch ID |
| `summary` | string | yes | | Final summary |

**Example input:**
```json
{
  "id": "2026-04-incident-node3",
  "summary": "Node3 recovered. Replicas restored to 3. No data loss."
}
```

**Example output:**
```
Epoch '2026-04-incident-node3' sealed
```

---

### agentstategraph_list_epochs

List all epochs with their status, dates, and commit counts.

**Parameters:** None.

**Example output:**
```json
[
  {
    "id": "2026-04-incident-node3",
    "description": "Node3 failure recovery",
    "status": "Sealed",
    "commits": 12,
    "agents": ["agent/monitor", "agent/recovery"],
    "tags": ["incident", "node3"],
    "created": "2026-04-06T10:00:00Z",
    "sealed": "2026-04-06T11:30:00Z"
  }
]
```

---

### agentstategraph_archive_epoch

Transition a sealed epoch to `Archived` (cold storage; still queryable).

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `id` | string | yes | | Epoch ID (must be sealed) |

**Example output:**
```
Epoch '2026-04-incident-node3' archived
```

---

### agentstategraph_export_epoch

Export a sealed or archived epoch as a self-contained JSON audit bundle (the epoch plus full commit records). Active epochs cannot be exported.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `id` | string | yes | | Epoch ID (sealed or archived) |

**Example output:**
```json
{
  "agentstategraph_export_version": 1,
  "epoch": { "id": "2026-04-incident-node3", "status": "Sealed", "seal_hash": "..." },
  "commits": [ /* full Commit records */ ],
  "exported_at": "2026-05-25T10:00:00Z"
}
```

---

### agentstategraph_enter_epoch

Set the active epoch for this server. Subsequent commits are associated with it.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `id` | string | yes | | Epoch ID to enter |

---

### agentstategraph_exit_epoch

Clear the active epoch.

**Parameters:** None.

---

## Sessions

### agentstategraph_create_session

Create a new agent session, optionally scoped to a namespace and/or a path subtree.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `agent_id` | string | yes | | Agent identity for the session |
| `branch` | string | no | `"main"` | Working branch |
| `parent_session` | string | no | | Parent session id (for sub-agent orchestration) |
| `delegated_intent` | string | no | | Intent this session was delegated |
| `path_scope` | string | no | | Restrict the session to a path subtree |
| `namespace_id` | string | no | | Namespace to scope the session to |

**Example output:**
```
Session 'session-002' created for 'agent/executor'
```

---

### agentstategraph_enter_session

Set the active session for this server.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `id` | string | yes | | Session id to enter |

---

### agentstategraph_exit_session

Clear the active session.

**Parameters:** None.

---

### agentstategraph_sessions

List active agent sessions with parent-child relationships and path scoping.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `agent_id` | string | no | | Filter by agent |

**Example input:**
```json
{ "agent_id": "agent/planner" }
```

**Example output:**
```json
[
  {
    "id": "session-001",
    "agent": "agent/planner",
    "branch": "agents/planner/workspace",
    "parent_session": null,
    "delegated_intent": "intent-001",
    "report_to": "user",
    "path_scope": "/cluster",
    "created": "2026-04-06T12:00:00Z"
  }
]
```

## Explorer & Viewer Tools

### agentstategraph_list_paths

List all leaf paths in the state tree under a prefix. Use to explore what data exists.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch or ref |
| `prefix` | string | no | `"/"` | Path prefix to list under |
| `max_depth` | number | no | `50` | Max tree depth to traverse |

**Example input:**
```json
{ "ref": "main", "prefix": "/cluster" }
```

**Example output:**
```
6 paths:
/cluster/name
/cluster/region
/cluster/nodes/0/hostname
/cluster/nodes/0/status
/cluster/network/topology
/cluster/config/log_level
```

---

### agentstategraph_get_tree

Get an entire subtree as nested JSON. Efficient batch alternative to reading individual paths.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch or ref |
| `prefix` | string | no | `"/"` | Path prefix to get subtree for |

**Example input:**
```json
{ "ref": "main", "prefix": "/cluster/network" }
```

**Example output:**
```json
{
  "topology": "mesh",
  "subnet": "10.0.0.0/24",
  "dns": "1.1.1.1"
}
```

---

### agentstategraph_search

Search state values and key names for a query string. Case-insensitive.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch or ref |
| `query` | string | yes | | Search query (matches values and key names) |
| `max_results` | number | no | `50` | Max results to return |

**Example input:**
```json
{ "query": "mesh" }
```

**Example output:**
```json
[
  { "path": "/cluster/network/topology", "value": "mesh" }
]
```

---

### agentstategraph_stats

Get summary statistics for a ref. Useful for dashboard displays.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch or ref |

**Example output:**
```json
{
  "commit_count": 47,
  "branch_count": 5,
  "path_count": 23,
  "epoch_count": 2,
  "agents": ["agent/monitor", "agent/planner", "agent/setup"],
  "categories": ["Checkpoint", "Explore", "Fix", "Merge", "Refine"],
  "latest_commit": {
    "id": "sg_f5b2..17",
    "agent": "agent/compliance",
    "intent": "Seal Q1 epoch",
    "timestamp": "2026-04-10T14:33:00Z"
  }
}
```

---

### agentstategraph_commit_graph

Get the commit DAG for visualization. Returns nodes with parents, agent, category, and timestamps.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch or ref |
| `depth` | number | no | `50` | Max commits to include |

**Example output:**
```json
[
  {
    "id": "sg_f5b2..17",
    "full_id": "sg_f5b2c39e...",
    "parents": ["sg_d1e8..9a"],
    "agent": "agent/compliance",
    "category": "Checkpoint",
    "description": "Seal Q1 epoch",
    "confidence": 0.99,
    "timestamp": "2026-04-10T14:33:00Z",
    "is_merge": false
  }
]
```

---

### agentstategraph_intent_tree

Get the intent decomposition tree. Shows how intents are broken down into sub-tasks across agents.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch or ref |
| `root_commit_id` | string | no | | Optional root commit ID to start from |

**Example output:**
```json
{
  "roots": [
    {
      "id": "sg_6c0d..78",
      "agent": "agent/setup",
      "category": "Checkpoint",
      "description": "Initialize cluster",
      "confidence": 0.99,
      "children": [
        {
          "id": "sg_7d1c..56",
          "agent": "agent/setup",
          "category": "Checkpoint",
          "description": "Add node-1 as worker",
          "children": []
        }
      ]
    }
  ],
  "total_commits": 47
}
```

---

## Tasks

Plans and tasks are stored in AgentStateGraph as structured state. Each plan is a collection of tasks with a strict state machine (`pending → in_progress → done`). Tasks can be assigned to agents, have blockers, carry `Proof` of completion, and are filterable by `IntentCategory::Plan` in log and blame queries.

### agentstategraph_create_plan

Create a new plan.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch |
| `name` | string | yes | | Plan name (e.g., `"deploy-v2"`) |
| `description` | string | no | | Plan description |

**Example input:**
```json
{
  "name": "deploy-v2",
  "description": "Rolling upgrade of all cluster nodes to v2.0"
}
```

**Example output:**
```
Plan 'deploy-v2' created
```

---

### agentstategraph_list_plans

List all plans, optionally filtered by status.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch |
| `status` | string | no | | Filter by status: `Active`, `Completed`, or `Archived` |

**Example input:**
```json
{ "status": "Active" }
```

**Example output:**
```json
[
  {
    "id": "deploy-v2",
    "title": "Deploy version 2.0",
    "task_count": 3,
    "completed": 1,
    "created": "2026-04-10T09:00:00Z"
  }
]
```

---

### agentstategraph_get_plan

Get a plan with all its tasks.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch |
| `name` | string | yes | | Plan name |

**Example input:**
```json
{ "name": "deploy-v2" }
```

**Example output:**
```json
{
  "id": "deploy-v2",
  "title": "Deploy version 2.0",
  "tasks": [
    { "id": "t-001", "title": "Drain node-1", "status": "done", "assigned_to": "agent/ops" },
    { "id": "t-002", "title": "Upgrade node-1", "status": "in_progress", "assigned_to": "agent/ops" },
    { "id": "t-003", "title": "Verify node-1", "status": "pending", "assigned_to": null }
  ]
}
```

---

### agentstategraph_add_task

Add a task to a plan. The task ID (`t-NNN`) is auto-assigned and returned.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch |
| `plan` | string | yes | | Plan name to add the task to |
| `title` | string | yes | | Task title |
| `priority` | string | no | `"Medium"` | `Low`, `Medium`, `High`, or `Critical` |
| `parent_id` | string | no | | Parent task ID for subtasks (e.g., `"t-001"`) |
| `blocked_by` | string[] | no | | Task IDs that must complete first |
| `assigned_to` | string | no | | Agent to assign the task to |

**Example input:**
```json
{
  "plan": "deploy-v2",
  "title": "Update DNS records",
  "priority": "High",
  "blocked_by": ["t-003"]
}
```

**Example output:**
```
Task 't-004' added to plan 'deploy-v2'
```

---

### agentstategraph_list_tasks

List all tasks in a plan.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch |
| `plan` | string | yes | | Plan name |

**Example input:**
```json
{ "plan": "deploy-v2" }
```

**Example output:**
```json
[
  { "id": "t-003", "title": "Verify node-1", "status": "pending", "priority": "medium" },
  { "id": "t-004", "title": "Update DNS records", "status": "pending", "priority": "high", "blocked_by": ["t-003"] }
]
```

---

### agentstategraph_start_task

Transition a task from `pending` to `in_progress`.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch |
| `plan` | string | yes | | Plan name |
| `task_id` | string | yes | | Task identifier (e.g., `"t-002"`) |

**Example input:**
```json
{ "plan": "deploy-v2", "task_id": "t-002" }
```

**Example output:**
```
Task 't-002' started
```

---

### agentstategraph_complete_task

Transition a task from `in_progress` to `done`. A typed proof is required.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch |
| `plan` | string | yes | | Plan name |
| `task_id` | string | yes | | Task identifier |
| `proof_kind` | string | yes | | `Commit`, `File`, `Test`, or `Text` |
| `proof_value` | string | yes | | Commit hash, file path, test name, or text |
| `proof_note` | string | no | | Optional note |

**Example input:**
```json
{
  "plan": "deploy-v2",
  "task_id": "t-002",
  "proof_kind": "Commit",
  "proof_value": "sg_f5b2c39e...",
  "proof_note": "Node-1 upgraded and passing health checks"
}
```

**Example output:**
```
Task 't-002' completed
```

---

### agentstategraph_abandon_task

Abandon a task with a reason. `abandoned` is terminal.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch |
| `plan` | string | yes | | Plan name |
| `task_id` | string | yes | | Task identifier |
| `reason` | string | yes | | Why the task is being abandoned |

**Example input:**
```json
{ "plan": "deploy-v2", "task_id": "t-002", "reason": "Node-1 failed pre-flight checks" }
```

**Example output:**
```
Task 't-002' abandoned
```

---

### agentstategraph_assign_task

Assign a task to an agent.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch |
| `plan` | string | yes | | Plan name |
| `task_id` | string | yes | | Task identifier |
| `agent` | string | yes | | Agent to assign the task to |

**Example input:**
```json
{ "plan": "deploy-v2", "task_id": "t-003", "agent": "agent/verifier" }
```

**Example output:**
```
Task 't-003' assigned to 'agent/verifier'
```

---

### agentstategraph_next_task

Get the next highest-priority unblocked task (optionally for a specific agent).

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch |
| `plan` | string | yes | | Plan name |
| `agent` | string | no | | Prefer tasks assigned to this agent |

**Example input:**
```json
{ "plan": "deploy-v2", "agent": "agent/ops" }
```

**Example output:**
```json
{ "id": "t-003", "title": "Verify node-1", "priority": "medium", "assigned_to": null }
```

---

## Policy

Policies express authorization rules and cost-of-change thresholds. A policy is proposed, ratified, optionally signed (Ed25519), and then active. Changes can be evaluated against the active policy before they are committed, optionally through a pluggable Cedar / Rego / WASM evaluator. Evaluation produces a `Decision` — `Allow`, `Deny`, `RequireApproval` (with a fallback action), or `NoPolicyMatch` — with a fail-safe deny applied by the server when nothing matches. Precedence is `deny > require_approval > allow`. See the [Policy guide](/guides/policy/) for the full model.

### agentstategraph_policy_propose

Propose a new policy. The `policy` is a full Policy JSON document (it carries
its own `path`). Proposed policies are unratified and ignored by the evaluator
until ratified.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch |
| `policy` | object | yes | | Full Policy JSON (`path`, `description`, `rules`, …) |

**Example input:**
```json
{
  "policy": {
    "path": "/cluster",
    "description": "High-confidence gate for cluster writes",
    "rules": [
      { "match": { "path_prefix": "/cluster/" }, "min_confidence": 0.8, "effect": "require_approval" }
    ]
  }
}
```

**Example output:**
```
Policy proposed at /cluster (version 1, unratified)
```

---

### agentstategraph_policy_ratify

Ratify an unratified policy at `path`, making it active.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch |
| `path` | string | yes | | Policy path to ratify |
| `ratifier` | string | yes | | Agent or principal ratifying |
| `reasoning` | string | yes | | Why this policy is being ratified |

**Example input:**
```json
{ "path": "/cluster", "ratifier": "agent/lead", "reasoning": "Reviewed and approved" }
```

**Example output:**
```
Policy /cluster ratified
```

---

### agentstategraph_policy_supersede

Replace the active policy at `old_path` with a new version. Returns the new
`path@version` handle.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch |
| `old_path` | string | yes | | Path of the policy being superseded |
| `new_policy` | object | yes | | Full new Policy JSON |

**Example input:**
```json
{
  "old_path": "/cluster",
  "new_policy": {
    "path": "/cluster",
    "description": "Raise confidence threshold to 0.9",
    "rules": [ { "match": { "path_prefix": "/cluster/" }, "min_confidence": 0.9, "effect": "require_approval" } ]
  }
}
```

**Example output:**
```
Policy /cluster superseded → /cluster@2
```

---

### agentstategraph_policy_list

List policies, optionally filtered by path prefix, status, and tenant.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch |
| `prefix` | string | no | | Path prefix filter |
| `status` | string | no | `"active"` | `"active"`, `"proposed"`, or `"all"` |
| `tenant_filter` | string | no | | Restrict to a tenant (globals always apply) |

**Example output:**
```json
[
  { "path": "/cluster", "version": 2, "status": "active", "description": "Cluster write gate v2" }
]
```

---

### agentstategraph_policy_show

Show a policy at a path (active version, or a pinned version).

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch |
| `path` | string | yes | | Policy path |
| `version` | number | no | | Pin a specific version (default: active) |

**Example output:**
```json
{
  "path": "/cluster",
  "version": 2,
  "status": "active",
  "description": "Cluster write gate v2",
  "ratified_by": "agent/lead",
  "rules": [ { "match": { "path_prefix": "/cluster/" }, "min_confidence": 0.8, "effect": "require_approval" } ]
}
```

---

### agentstategraph_policy_history

Walk a policy's supersedes chain at a path.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch |
| `path` | string | yes | | Policy path |

**Example output:**
```json
[
  { "path": "/cluster", "version": 1, "status": "superseded", "supersedes": null },
  { "path": "/cluster", "version": 2, "status": "active", "supersedes": "/cluster@1" }
]
```

---

### agentstategraph_policy_evaluate

Evaluate an authorization request against active policies. The `situation` is a
flat string map of facts.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch |
| `situation` | object | yes | | Flat `{ string: string }` map of situation facts |
| `action` | string | yes | | The action being requested |
| `agent_id` | string | yes | | Agent requesting the evaluation |
| `tenant_filter` | string | no | | Restrict to a tenant (globals always apply) |

**Example input:**
```json
{
  "situation": { "path": "/cluster/replicas", "confidence": "0.65" },
  "action": "write",
  "agent_id": "agent/scaler"
}
```

**Example output:**
```
Decision: RequireApproval
Reason: Confidence 0.65 is below required threshold 0.8 for /cluster/* writes
```

---

### agentstategraph_policy_evaluate_change

Evaluate a full `ChangeProposal` against active policies before committing.
Returns a `Decision` with a fallback action.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch the change would be applied to |
| `proposal` | object | yes | | Full ChangeProposal JSON (action, agent_id, intent, preferred_option, alternatives, tokens, attached_fields) |
| `tenant_filter` | string | no | | Restrict to a tenant (globals always apply) |

**Example input:**
```json
{
  "proposal": {
    "action": "write",
    "agent_id": "agent/scaler",
    "preferred_option": { "path": "/cluster/replicas", "value": 5 },
    "confidence": 0.65
  }
}
```

**Example output:**
```
Decision: RequireApproval (fallback: KeepCurrentState)
Reason: Confidence 0.65 is below required threshold 0.8 for /cluster/* writes
```

---

### agentstategraph_policy_check_tokens

Pre-flight check: list the active policies whose triggers match the given
tokens. Use it to discover which policies would fire before proposing a change.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch |
| `tokens` | string[] | yes | | Trigger tokens to match against |

**Example input:**
```json
{ "tokens": ["destructive", "/cluster/"] }
```

**Example output:**
```json
[ { "path": "/cluster", "version": 2, "matched": ["destructive"] } ]
```

---

### agentstategraph_policy_sign

Sign the active policy at a path with the server's registered `PolicySigner`.
The signing key is configured on the server, not passed in the call.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch |
| `path` | string | yes | | Policy path to sign |
| `signer_key_id` | string | no | | Hint for multi-key signers; `Ed25519Signer` ignores it |

**Example input:**
```json
{ "path": "/cluster" }
```

**Example output:**
```json
{ "algorithm": "ed25519", "signer_key_id": "lead-key-1", "signature_hex": "3f8a2b..." }
```

---

### agentstategraph_policy_verify

Verify the signature on the active policy at a path, using the verifying key
looked up by `signer_key_id` in the server's key registry.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch |
| `path` | string | yes | | Policy path to verify |

**Example input:**
```json
{ "path": "/cluster" }
```

**Example output:**
```json
{ "valid": true, "algorithm": "ed25519", "signer_key_id": "lead-key-1" }
```

---

## Taint

Taint marks paths as sensitive, suspicious, or under watch. Quarantined paths require policy evaluation before changes are allowed. Watch marks trigger audit notifications. All taint operations are audit-committed with `IntentCategory::Taint` (or the corresponding variant) so they appear in `log` and `blame`.

### agentstategraph_taint

Apply a named taint to a path. The `effect` controls what the pre-commit hook
does on subsequent writes.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch |
| `path` | string | yes | | Path to taint |
| `name` | string | yes | | Taint name (used to resolve it later) |
| `effect` | string | yes | | `warn`, `block`, `review`, or `isolate` |
| `reason` | string | yes | | Why this path is being tainted |
| `severity` | string | no | `"medium"` | `low`, `medium`, `high`, or `critical` |
| `expires` | string | no | | RFC3339 expiry; null = permanent |
| `propagate` | bool | no | `true` | Cascade to descendant paths |
| `agent_id` | string | yes | | Agent applying the taint |

**Example input:**
```json
{
  "path": "/cluster/credentials",
  "name": "cred-exposure-2026-05",
  "effect": "block",
  "severity": "high",
  "reason": "Potential credential exposure detected",
  "agent_id": "agent/security"
}
```

**Example output:**
```
Taint 'cred-exposure-2026-05' applied: /cluster/credentials (block/high)
```

---

### agentstategraph_untaint

Resolve a named taint on a path. The taint is resolved (not deleted) to
preserve the audit trail.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch |
| `path` | string | yes | | Path to untaint |
| `name` | string | yes | | Name of the taint to resolve |
| `reason` | string | yes | | Why the taint is being lifted |
| `proof` | string | no | | Evidence the issue is resolved |
| `agent_id` | string | yes | | Agent resolving the taint |

**Example input:**
```json
{
  "path": "/cluster/credentials",
  "name": "cred-exposure-2026-05",
  "reason": "Credentials rotated and verified clean",
  "agent_id": "agent/security"
}
```

**Example output:**
```
Taint 'cred-exposure-2026-05' removed: /cluster/credentials
```

---

### agentstategraph_quarantine

Quarantine a path, restricting writes to an explicit list of authorized agents.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch |
| `path` | string | yes | | Path to quarantine |
| `name` | string | yes | | Quarantine name |
| `reason` | string | yes | | Why |
| `authorized_agents` | string[] | yes | | Agents permitted to write under the path |
| `severity` | string | no | `"high"` | `low`, `medium`, `high`, or `critical` |
| `expires` | string | no | | RFC3339 expiry; null = permanent |
| `propagate` | bool | no | `true` | Cascade to descendant paths |
| `agent_id` | string | yes | | Agent applying the quarantine |

**Example input:**
```json
{
  "path": "/cluster/node-3",
  "name": "node-3-compromise",
  "severity": "critical",
  "reason": "Node-3 compromised",
  "authorized_agents": ["agent/security"],
  "agent_id": "agent/security"
}
```

**Example output:**
```
Quarantine 'node-3-compromise' applied: /cluster/node-3 (critical)
```

---

### agentstategraph_unquarantine

Lift a named quarantine from a path (resolved, not deleted).

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch |
| `path` | string | yes | | Path to unquarantine |
| `name` | string | yes | | Name of the quarantine to lift |
| `reason` | string | yes | | Why quarantine is being lifted |
| `proof` | string | no | | Evidence the path is safe |
| `agent_id` | string | yes | | Agent lifting the quarantine |

**Example input:**
```json
{
  "path": "/cluster/node-3",
  "name": "node-3-compromise",
  "reason": "Node-3 reimaged and verified",
  "agent_id": "agent/security"
}
```

**Example output:**
```
Quarantine 'node-3-compromise' lifted: /cluster/node-3
```

---

### agentstategraph_watch

Apply an advisory watch to a path. With a `metric`/`threshold`/`direction` it
auto-escalates to a `warn` taint when a write crosses the threshold.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch |
| `path` | string | yes | | Path to watch |
| `name` | string | yes | | Watch name |
| `reason` | string | yes | | Why |
| `metric` | string | no | | Numeric field to monitor for escalation |
| `threshold` | number | no | | Escalation threshold |
| `direction` | string | no | `"above"` | `above` or `below` |
| `check_interval_secs` | number | no | | Re-check cadence |
| `expires` | string | no | | RFC3339 expiry |
| `severity` | string | no | | Severity if escalated |
| `propagate` | bool | no | `true` | Cascade to descendant paths |
| `agent_id` | string | yes | | Agent applying the watch |

**Example input:**
```json
{
  "path": "/cluster/network",
  "name": "topology-watch",
  "reason": "Monitoring for unexpected topology changes",
  "agent_id": "agent/monitor"
}
```

**Example output:**
```
Watch 'topology-watch' applied: /cluster/network
```

---

### agentstategraph_unwatch

Remove a named watch from a path.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch |
| `path` | string | yes | | Path to unwatch |
| `name` | string | yes | | Name of the watch to remove |
| `reason` | string | no | | Why |
| `agent_id` | string | yes | | Agent removing the watch |

**Example input:**
```json
{ "path": "/cluster/network", "name": "topology-watch", "agent_id": "agent/monitor" }
```

**Example output:**
```
Watch 'topology-watch' removed: /cluster/network
```

---

### agentstategraph_list_taints

List taints, quarantines, and watches, with optional filters.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `path` | string | no | | Filter to a path |
| `kind` | string | no | | `taint`, `quarantine`, or `watch` (default: all) |
| `effect` | string | no | | `warn`, `block`, `review`, or `isolate` (client-side filter) |
| `include_expired` | bool | no | `false` | Include expired marks |

**Example output:**
```json
[
  {
    "path": "/cluster/credentials",
    "name": "cred-exposure-2026-05",
    "kind": "quarantine",
    "severity": "high",
    "reason": "Potential credential exposure detected",
    "agent_id": "agent/security",
    "created_at": "2026-04-10T14:00:00Z"
  }
]
```

---

### agentstategraph_check_taint

Check the full taint status for a path, including marks inherited from ancestor
paths. Optionally evaluate write access for a specific agent and confidence.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `path` | string | yes | | Path to check |
| `agent_id` | string | no | | Agent to evaluate access for |
| `confidence` | number | no | | Commit confidence (for the `review` effect gate) |

**Example input:**
```json
{ "path": "/cluster/credentials", "agent_id": "agent/ops", "confidence": 0.95 }
```

**Example output:**
```json
{
  "tainted": true,
  "kind": "quarantine",
  "effect": "block",
  "severity": "high",
  "can_write": false,
  "reason": "Potential credential exposure detected"
}
```

---

### agentstategraph_policy_evaluate_change_with_taints

Evaluate a change proposal against active policies **and** the taints on each
affected path in one call. Returns `{ decision, taint_status, can_proceed }`,
where `can_proceed` is true only when the decision is not `deny` and every path
is writable.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch |
| `proposal` | object | yes | | Full ChangeProposal JSON |
| `affected_paths` | string[] | no | | Paths the change would touch (each is taint-checked) |
| `agent_id` | string | no | | Agent for the taint-check pass (falls back to `proposal.agent_id`) |
| `confidence` | number | no | `1.0` | Confidence for the `review`-effect gate |
| `tenant_filter` | string | no | | Restrict policies to a tenant |

**Example input:**
```json
{
  "proposal": {
    "action": "write",
    "agent_id": "agent/ops",
    "preferred_option": { "path": "/cluster/credentials", "value": { "token": "new-token" } }
  },
  "affected_paths": ["/cluster/credentials"],
  "confidence": 0.95
}
```

**Example output:**
```json
{ "decision": "Allow", "taint_status": "Quarantine", "can_proceed": false }
```

---

## Reminders (7 tools)

Pull-based reminders let agents and users schedule future work with priority, repeating schedules, soft object references, and an optional autonomous execution flag. Agents call `remind_me` at checkpoints (session start, task transitions, branch switches) to receive all currently due items.

**Schedule string format:** `"once"` | `"interval:<seconds>"` | `"daily:HH:MM"` | `"weekly:Weekday:HH:MM"` (e.g. `"weekly:Monday:09:00"`).

**Priority values:** `"critical"` | `"high"` | `"medium"` (default) | `"low"` | `"minimal"`.

**Ref kind values:** `"branch"` | `"memory"` | `"plan"` | `"task"` | `"state_path"` | `"external:<scheme>"`.

---

### agentstategraph_reminder_create

Create a new reminder.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `title` | string | yes | | Short human-readable title |
| `instructions` | string | yes | | What to do when due |
| `due_at` | string | yes | | ISO 8601 due timestamp |
| `schedule` | string | no | `"once"` | Schedule string (see above) |
| `priority` | string | no | `"medium"` | Priority (see above) |
| `autonomous` | bool | no | `false` | Execute without asking permission |
| `created_by` | string | no | | Agent or user id creating this |
| `commands` | string[] | no | | CLI commands to run at execution time |
| `tags` | string[] | no | | Arbitrary tags for filtering |
| `refs` | object[] | no | | Soft refs: `[{"kind":"branch","id":"main","label":"main branch"}]` |

**Example input:**
```json
{
  "title": "Stop local web server",
  "instructions": "The dev server started for PR review may still be running. Check and terminate it.",
  "due_at": "2026-05-03T09:00:00Z",
  "priority": "high",
  "autonomous": false,
  "created_by": "agent/dev",
  "tags": ["cleanup", "server"]
}
```

**Example output:**
```json
{ "id": "rem-0193a4f2-...", "status": "Pending" }
```

---

### agentstategraph_reminder_list

List reminders with optional filtering.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `status` | string | no | | Filter by status: `Pending`, `Due`, `AwaitingPermission`, `InProgress`, `Completed`, `Snoozed`, `Cancelled` |
| `priority_at_most` | string | no | | Only return items at this priority or higher (e.g. `"high"` returns Critical + High) |
| `created_by` | string | no | | Filter by creator id |
| `due_before` | string | no | | Only items due before this ISO 8601 timestamp |
| `ref_id` | string | no | | Only items that reference this object id |
| `tags` | string[] | no | | Items that have ALL of these tags |

**Example input:**
```json
{ "status": "Due", "priority_at_most": "high" }
```

---

### agentstategraph_reminder_remind_me

The core pull-based query. Lazily promotes past-due `Pending` items and expired `Snoozed` items to `Due`, then returns all `Due` and `AwaitingPermission` items ordered by priority then due date. Call this at checkpoints.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `created_by` | string | no | | Scope to a specific agent or user |

**Example output:**
```json
[
  {
    "id": "rem-0193a4f2-...",
    "title": "Stop local web server",
    "status": "Due",
    "priority": "High",
    "due_at": "2026-05-03T09:00:00Z",
    "autonomous": false
  }
]
```

---

### agentstategraph_reminder_snooze

Defer a `Due` or `Pending` reminder until a later time.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `id` | string | yes | | Reminder id |
| `until` | string | yes | | ISO 8601 timestamp to snooze until |

---

### agentstategraph_reminder_approve

Approve a reminder that is in `AwaitingPermission` status (created with `autonomous: false`). Transitions it to `Due` so the agent can proceed.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `id` | string | yes | | Reminder id |
| `approved_by` | string | yes | | User or agent id granting approval |

---

### agentstategraph_reminder_cancel

Cancel a reminder (terminal state; cannot be undone).

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `id` | string | yes | | Reminder id |
| `reason` | string | no | | Why it is being cancelled |

---

### agentstategraph_reminder_record_execution

Record the result of executing a reminder. If the reminder has a repeating schedule and the result is `success`, the next due time is computed and the reminder resets to `Pending` (or `AwaitingPermission` for non-autonomous). A `once` reminder transitions to `Completed` on success.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `id` | string | yes | | Reminder id |
| `agent_id` | string | yes | | Agent that executed the reminder |
| `result` | string | yes | | `success`, `failed`, or `cancelled` |
| `started_at` | string | yes | | ISO 8601 execution start time |
| `completed_at` | string | yes | | ISO 8601 execution end time |
| `approved_by` | string | no | | Who approved execution (for non-autonomous) |
| `notes` | string | no | | Free-form execution notes |
| `task_id` | string | no | | Id of the task created for this execution |

**Example input:**
```json
{
  "id": "rem-0193a4f2-...",
  "agent_id": "agent/dev",
  "result": "success",
  "started_at": "2026-05-03T09:01:00Z",
  "completed_at": "2026-05-03T09:01:05Z",
  "notes": "Server process was not running; nothing to terminate."
}
```
