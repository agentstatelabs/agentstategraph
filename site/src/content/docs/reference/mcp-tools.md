---
title: MCP Tools Reference
description: Complete reference for all 20 AgentStateGraph MCP tools with parameters and examples.
---

## State Operations

### stategraph_get

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

### stategraph_set

Write a value to state, creating a new atomic commit with intent metadata.

**Parameters:**

| Name | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `ref` | string | no | `"main"` | Branch to commit to |
| `path` | string | yes | | JSON path to set |
| `value` | any | yes | | JSON value to write |
| `intent_category` | string | yes | | `Explore`, `Refine`, `Fix`, `Rollback`, `Checkpoint`, `Merge`, `Migrate` |
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

### stategraph_delete

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

### stategraph_branch

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

### stategraph_list_branches

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

### stategraph_merge

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

### stategraph_diff

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

### stategraph_speculate

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

### stategraph_spec_modify

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

### stategraph_compare

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

### stategraph_commit_spec

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

### stategraph_discard

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

### stategraph_log

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

### stategraph_query

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

### stategraph_blame

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

### stategraph_create_epoch

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

### stategraph_seal_epoch

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

### stategraph_list_epochs

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

### stategraph_sessions

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
