---
title: MCP Tools Reference
description: Complete reference for all 27 AgentStateGraph MCP tools with parameters and examples.
---

> **59 tools** — 29 core (state, branching, speculation, query/audit, epochs, sessions, explorer) + 10 tasks + 11 policy + 9 taint. Also available as [22 HTTP REST endpoints](/guides/mcp-server/#http-rest-api) via `--http` mode. The `agentstategraph-mcp` binary additionally offers a [`migrate` subcommand](/guides/mcp-server/) for schema upgrades — it's a one-shot CLI, not an MCP tool.

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

## Sessions

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
| `plan_id` | string | yes | | Unique plan identifier |
| `title` | string | yes | | Plan title |
| `description` | string | no | | Plan description |

**Example input:**
```json
{
  "plan_id": "deploy-v2",
  "title": "Deploy version 2.0",
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
| `status` | string | no | | Filter by status: `active`, `completed`, or `all` (default) |

**Example input:**
```json
{ "status": "active" }
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
| `plan_id` | string | yes | | Plan identifier |

**Example input:**
```json
{ "plan_id": "deploy-v2" }
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

Add a task to a plan.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `plan_id` | string | yes | | Plan to add the task to |
| `task_id` | string | yes | | Unique task identifier within the plan |
| `title` | string | yes | | Task title |
| `description` | string | no | | Task description |
| `priority` | string | no | `"medium"` | `"low"`, `"medium"`, or `"high"` |
| `blocked_by` | string[] | no | | Task IDs that must complete first |

**Example input:**
```json
{
  "plan_id": "deploy-v2",
  "task_id": "t-004",
  "title": "Update DNS records",
  "priority": "high",
  "blocked_by": ["t-003"]
}
```

**Example output:**
```
Task 't-004' added to plan 'deploy-v2'
```

---

### agentstategraph_list_tasks

List tasks in a plan, optionally filtered by status or assigned agent.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `plan_id` | string | yes | | Plan identifier |
| `status` | string | no | | Filter by `pending`, `in_progress`, or `done` |
| `assigned_to` | string | no | | Filter by assigned agent |

**Example input:**
```json
{ "plan_id": "deploy-v2", "status": "pending" }
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
| `plan_id` | string | yes | | Plan identifier |
| `task_id` | string | yes | | Task identifier |
| `agent_id` | string | no | | Agent taking ownership |

**Example input:**
```json
{ "plan_id": "deploy-v2", "task_id": "t-002", "agent_id": "agent/ops" }
```

**Example output:**
```
Task 't-002' started
```

---

### agentstategraph_complete_task

Transition a task from `in_progress` to `done`. Optionally attach a proof of completion.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `plan_id` | string | yes | | Plan identifier |
| `task_id` | string | yes | | Task identifier |
| `proof` | string | no | | Evidence of completion (commit ID, URL, description) |
| `notes` | string | no | | Completion notes |

**Example input:**
```json
{
  "plan_id": "deploy-v2",
  "task_id": "t-002",
  "proof": "sg_f5b2c39e...",
  "notes": "Node-1 upgraded and passing health checks"
}
```

**Example output:**
```
Task 't-002' completed
```

---

### agentstategraph_abandon_task

Abandon a task, returning it to `pending` with a reason.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `plan_id` | string | yes | | Plan identifier |
| `task_id` | string | yes | | Task identifier |
| `reason` | string | yes | | Why the task is being abandoned |

**Example input:**
```json
{ "plan_id": "deploy-v2", "task_id": "t-002", "reason": "Node-1 failed pre-flight checks" }
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
| `plan_id` | string | yes | | Plan identifier |
| `task_id` | string | yes | | Task identifier |
| `agent_id` | string | yes | | Agent to assign the task to |

**Example input:**
```json
{ "plan_id": "deploy-v2", "task_id": "t-003", "agent_id": "agent/verifier" }
```

**Example output:**
```
Task 't-003' assigned to 'agent/verifier'
```

---

### agentstategraph_next_task

Get the next unblocked, unassigned pending task for an agent (or any agent if omitted).

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `plan_id` | string | yes | | Plan identifier |
| `agent_id` | string | no | | Prefer tasks assigned to this agent |

**Example input:**
```json
{ "plan_id": "deploy-v2", "agent_id": "agent/ops" }
```

**Example output:**
```json
{ "id": "t-003", "title": "Verify node-1", "priority": "medium", "assigned_to": null }
```

---

## Policy

Policies express authorization rules and cost-of-change thresholds. A policy is proposed, ratified by one or more signers (Ed25519), and then active. Changes can be evaluated against the active policy before they are committed. Evaluation produces a `Decision` (`Allow`, `Deny`, or `Audit`) with a fail-safe fallback.

### agentstategraph_policy_propose

Propose a new policy document.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `policy_id` | string | yes | | Unique policy identifier |
| `title` | string | yes | | Policy title |
| `body` | string | yes | | Policy body (plaintext or structured rules) |
| `proposed_by` | string | yes | | Agent or principal proposing the policy |

**Example input:**
```json
{
  "policy_id": "p-001",
  "title": "Cluster write gate",
  "body": "Deny writes to /cluster/* when confidence < 0.7",
  "proposed_by": "agent/compliance"
}
```

**Example output:**
```
Policy 'p-001' proposed (status: Proposed)
```

---

### agentstategraph_policy_ratify

Ratify a proposed policy, advancing it to `Active` status.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `policy_id` | string | yes | | Policy to ratify |
| `ratified_by` | string | yes | | Agent or principal ratifying |
| `signature` | string | no | | Ed25519 signature (hex) if signed ratification |

**Example input:**
```json
{ "policy_id": "p-001", "ratified_by": "agent/lead" }
```

**Example output:**
```
Policy 'p-001' ratified (status: Active)
```

---

### agentstategraph_policy_supersede

Supersede an active policy with a newer one, archiving the old.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `old_policy_id` | string | yes | | Policy being superseded |
| `new_policy_id` | string | yes | | Replacement policy ID |
| `reason` | string | yes | | Why this policy is being superseded |

**Example input:**
```json
{
  "old_policy_id": "p-001",
  "new_policy_id": "p-002",
  "reason": "Raising confidence threshold to 0.8"
}
```

**Example output:**
```
Policy 'p-001' superseded by 'p-002'
```

---

### agentstategraph_policy_list

List all policies with their status.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `status` | string | no | | Filter by `Proposed`, `Active`, `Superseded`, or `Archived` |

**Example output:**
```json
[
  { "id": "p-001", "title": "Cluster write gate", "status": "Superseded" },
  { "id": "p-002", "title": "Cluster write gate v2", "status": "Active" }
]
```

---

### agentstategraph_policy_show

Show the full details of a specific policy.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `policy_id` | string | yes | | Policy identifier |

**Example output:**
```json
{
  "id": "p-002",
  "title": "Cluster write gate v2",
  "status": "Active",
  "body": "Deny writes to /cluster/* when confidence < 0.8",
  "proposed_by": "agent/compliance",
  "ratified_by": "agent/lead",
  "created": "2026-04-10T10:00:00Z"
}
```

---

### agentstategraph_policy_history

Show the version history of a policy, including superseded predecessors.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `policy_id` | string | yes | | Policy identifier (any version in the chain) |

**Example output:**
```json
[
  { "id": "p-001", "title": "Cluster write gate", "status": "Superseded", "superseded_by": "p-002" },
  { "id": "p-002", "title": "Cluster write gate v2", "status": "Active" }
]
```

---

### agentstategraph_policy_evaluate

Evaluate a free-form situation description against the active policy.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `policy_id` | string | yes | | Policy to evaluate against |
| `situation` | string | yes | | Description of the situation to evaluate |
| `agent_id` | string | no | | Agent requesting the evaluation |

**Example input:**
```json
{
  "policy_id": "p-002",
  "situation": "Writing /cluster/replicas = 5, confidence = 0.65",
  "agent_id": "agent/scaler"
}
```

**Example output:**
```
Decision: Deny
Reason: Confidence 0.65 is below required threshold 0.8 for /cluster/* writes
```

---

### agentstategraph_policy_evaluate_change

Evaluate a specific proposed state change against the active policy before committing.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `policy_id` | string | yes | | Policy to evaluate against |
| `ref` | string | no | `"main"` | Branch the change would be applied to |
| `path` | string | yes | | Path being modified |
| `value` | any | yes | | Proposed new value |
| `agent_id` | string | no | | Agent proposing the change |
| `confidence` | number | no | | Agent's confidence in the change |

**Example input:**
```json
{
  "policy_id": "p-002",
  "path": "/cluster/replicas",
  "value": 5,
  "agent_id": "agent/scaler",
  "confidence": 0.65
}
```

**Example output:**
```
Decision: Deny
Reason: Confidence 0.65 is below required threshold 0.8 for /cluster/* writes
```

---

### agentstategraph_policy_check_tokens

Check token/cost budget constraints in a policy for a proposed change.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `policy_id` | string | yes | | Policy to check |
| `estimated_tokens` | number | yes | | Estimated token cost of the proposed action |
| `agent_id` | string | no | | Agent requesting the check |

**Example input:**
```json
{ "policy_id": "p-002", "estimated_tokens": 4500, "agent_id": "agent/planner" }
```

**Example output:**
```
Decision: Allow
Remaining budget: 5500 tokens
```

---

### agentstategraph_policy_sign

Sign a policy with an Ed25519 key, producing a verifiable signature.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `policy_id` | string | yes | | Policy to sign |
| `private_key_hex` | string | yes | | Ed25519 private key (hex, 64 bytes) |
| `signer_id` | string | yes | | Identity of the signer |

**Example input:**
```json
{
  "policy_id": "p-002",
  "private_key_hex": "a1b2c3...",
  "signer_id": "agent/lead"
}
```

**Example output:**
```
Signature: 3f8a2b... (Ed25519, signer: agent/lead)
```

---

### agentstategraph_policy_verify

Verify an Ed25519 signature on a policy.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `policy_id` | string | yes | | Policy to verify |
| `signature_hex` | string | yes | | Signature to verify (hex) |
| `public_key_hex` | string | yes | | Ed25519 public key (hex, 32 bytes) |

**Example input:**
```json
{
  "policy_id": "p-002",
  "signature_hex": "3f8a2b...",
  "public_key_hex": "e4d5f6..."
}
```

**Example output:**
```
Signature valid: policy 'p-002' verified against key e4d5f6...
```

---

## Taint

Taint marks paths as sensitive, suspicious, or under watch. Quarantined paths require policy evaluation before changes are allowed. Watch marks trigger audit notifications. All taint operations are audit-committed with `IntentCategory::Taint` (or the corresponding variant) so they appear in `log` and `blame`.

### agentstategraph_taint

Apply a taint mark to a path.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch |
| `path` | string | yes | | Path to taint |
| `effect` | string | yes | | `"Quarantine"` or `"Watch"` |
| `severity` | string | no | `"Medium"` | `"Low"`, `"Medium"`, `"High"`, or `"Critical"` |
| `reason` | string | yes | | Why this path is being tainted |
| `expires_at` | string | no | | ISO 8601 expiry timestamp; taint auto-clears after this |

**Example input:**
```json
{
  "path": "/cluster/credentials",
  "effect": "Quarantine",
  "severity": "High",
  "reason": "Potential credential exposure detected"
}
```

**Example output:**
```
Taint applied: /cluster/credentials (Quarantine/High)
```

---

### agentstategraph_untaint

Remove a taint mark from a path.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch |
| `path` | string | yes | | Path to untaint |
| `reason` | string | yes | | Why the taint is being lifted |

**Example input:**
```json
{ "path": "/cluster/credentials", "reason": "Credentials rotated and verified clean" }
```

**Example output:**
```
Taint removed: /cluster/credentials
```

---

### agentstategraph_quarantine

Shorthand to apply a `Quarantine` taint effect to a path.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch |
| `path` | string | yes | | Path to quarantine |
| `severity` | string | no | `"Medium"` | Taint severity |
| `reason` | string | yes | | Why |

**Example input:**
```json
{ "path": "/cluster/node-3", "severity": "Critical", "reason": "Node-3 compromised" }
```

**Example output:**
```
Quarantine applied: /cluster/node-3 (Critical)
```

---

### agentstategraph_unquarantine

Lift a quarantine from a path.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch |
| `path` | string | yes | | Path to unquarantine |
| `reason` | string | yes | | Why quarantine is being lifted |

**Example input:**
```json
{ "path": "/cluster/node-3", "reason": "Node-3 reimaged and verified" }
```

**Example output:**
```
Quarantine lifted: /cluster/node-3
```

---

### agentstategraph_watch

Apply a `Watch` taint to a path (audit notifications without blocking changes).

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch |
| `path` | string | yes | | Path to watch |
| `reason` | string | yes | | Why |

**Example input:**
```json
{ "path": "/cluster/network", "reason": "Monitoring for unexpected topology changes" }
```

**Example output:**
```
Watch applied: /cluster/network
```

---

### agentstategraph_unwatch

Remove a watch from a path.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch |
| `path` | string | yes | | Path to unwatch |
| `reason` | string | yes | | Why |

**Example input:**
```json
{ "path": "/cluster/network", "reason": "Monitoring period complete" }
```

**Example output:**
```
Watch removed: /cluster/network
```

---

### agentstategraph_list_taints

List all active taint marks on a ref.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch |
| `effect` | string | no | | Filter by `Quarantine` or `Watch` |

**Example output:**
```json
[
  {
    "path": "/cluster/credentials",
    "effect": "Quarantine",
    "severity": "High",
    "reason": "Potential credential exposure detected",
    "tainted_by": "agent/security",
    "tainted_at": "2026-04-10T14:00:00Z"
  },
  {
    "path": "/cluster/network",
    "effect": "Watch",
    "severity": "Low",
    "reason": "Monitoring for unexpected topology changes",
    "tainted_by": "agent/monitor",
    "tainted_at": "2026-04-10T15:00:00Z"
  }
]
```

---

### agentstategraph_check_taint

Check whether a specific path is tainted (and what effect applies).

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch |
| `path` | string | yes | | Path to check |

**Example input:**
```json
{ "path": "/cluster/credentials" }
```

**Example output:**
```json
{
  "tainted": true,
  "effect": "Quarantine",
  "severity": "High",
  "reason": "Potential credential exposure detected"
}
```

---

### agentstategraph_policy_evaluate_change_with_taints

Evaluate a proposed change against the active policy and any taint marks on the target path. Combines policy and taint evaluation in a single call.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `policy_id` | string | yes | | Policy to evaluate against |
| `ref` | string | no | `"main"` | Branch |
| `path` | string | yes | | Path being modified |
| `value` | any | yes | | Proposed new value |
| `agent_id` | string | no | | Agent proposing the change |
| `confidence` | number | no | | Agent's confidence |

**Example input:**
```json
{
  "policy_id": "p-002",
  "path": "/cluster/credentials",
  "value": { "token": "new-token" },
  "agent_id": "agent/ops",
  "confidence": 0.9
}
```

**Example output:**
```
Decision: Deny
Reason: Path '/cluster/credentials' is under Quarantine (High severity) — policy evaluation blocked
```
