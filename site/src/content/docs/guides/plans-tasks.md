---
title: Plans & Tasks
description: A shared, process-safe task primitive that lives in the state graph itself.
---

`agentstategraph-tasks` gives a fleet of agents a shared, durable to-do system that lives *inside* the state graph rather than alongside it. A **plan** is a named container of tasks; a **task** has a strict state machine, can be assigned to an agent, can be blocked on other tasks, and carries proof of completion. Because tasks are stored as state, every task transition is a blameable commit.

## The model

A `Plan` (`Active` / `Completed` / `Archived`) holds tasks. A `Task` carries:

- a `TaskId` (`t-NNN`) and a `priority`,
- an optional `parent_id` for subtasks,
- `blocked_by` — a list of task ids that must finish first,
- `assigned_to` — the owning agent,
- `proof` — evidence attached on completion.

The state machine is strict:

```
pending ──▶ in_progress ──▶ done        (done requires a Proof)
   │              │
   └──────────────┴────────▶ abandoned  (with a reason)
```

`done` and `abandoned` are terminal — there is no reopen, so history stays honest. A `Proof` carries a kind (`Commit`, `File`, `Test`, or `Text`) so "done" always means something verifiable.

`next_task` returns the highest-priority unblocked task, which is how an agent picks up the next piece of work without coordination.

## Process-safe by construction

Tasks are stored as JSON in the state tree under a path prefix on an `Arc<Repository>`, so they work on **any** backend. Writes use a compare-and-swap retry loop, which makes concurrent task creation and updates **safe across multiple processes** — two agents racing to claim or complete tasks won't clobber each other. Plan/task commits use the native `IntentCategory::Plan` variant, so you can filter them out of (or into) `log` and `blame`.

## MCP tools

| Tool | Purpose |
|------|---------|
| `agentstategraph_create_plan` | Create a plan |
| `agentstategraph_list_plans` | List plans (optionally by status) |
| `agentstategraph_get_plan` | Get a plan and its task summary |
| `agentstategraph_add_task` | Add a task to a plan |
| `agentstategraph_list_tasks` | List tasks in a plan |
| `agentstategraph_start_task` | `pending` → `in_progress` |
| `agentstategraph_complete_task` | Complete a task with proof |
| `agentstategraph_abandon_task` | Abandon a task with a reason |
| `agentstategraph_assign_task` | Assign a task to an agent |
| `agentstategraph_next_task` | Next highest-priority unblocked task |

See the [MCP Tools reference](/reference/mcp-tools/#tasks) for parameters and examples.
