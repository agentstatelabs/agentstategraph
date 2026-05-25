---
title: Taint & Quarantine
description: Mark-and-sweep markers that turn observation into enforcement at commit time.
---

Policy says *what is allowed*. Taint says *what has gone wrong*. Taint is AgentStateGraph's runtime marking system: an agent (or a human, or a monitor) marks a path as suspect, and a pre-commit hook enforces that mark on every subsequent write — blocking, requiring confidence, hiding, or simply warning.

Every taint operation is itself a first-class commit with a native intent category (`Taint`, `Untaint`, `Quarantine`, `Unquarantine`, `Watch`, `Unwatch`), so the full lifecycle of who marked what, when, and why is visible in `log` and `blame`.

## Three kinds of mark

| Kind | Meaning |
|------|---------|
| **Taint** | A path is suspect. The attached **effect** decides what happens on write. |
| **Quarantine** | A path is restricted to an explicit list of authorized agents. |
| **Watch** | An advisory marker; optionally auto-escalates to a taint when a numeric threshold is crossed. |

## Effects

A taint carries a `TaintEffect` that the pre-commit hook consults on every `set`, `delete`, and `merge`:

- **`Warn`** — allow the write, surface a warning.
- **`Block`** — reject the write (`Blocked`).
- **`Review`** — allow only if the commit's confidence is `>= 0.9`, otherwise reject (`InsufficientConfidence`).
- **`Isolate`** — allow the write but hide the path from `search` and `query`.
- **`Advisory`** — watch-only, no enforcement.

Quarantines block any agent not in `authorized_agents` (`NotAuthorized`). When marks overlap, precedence is **Quarantine > Block > Review > Isolate > Warn/Advisory**.

Taints can `propagate` to descendant paths, so quarantining `/cluster/credentials` can cover everything beneath it. Watches with a numeric threshold and direction auto-escalate to a `Warn` taint the moment a write crosses the line — turning passive monitoring into active enforcement without a human in the loop. Taint-lifecycle commits themselves bypass the hook, so you can always lift a mark.

## Composing with policy

Taint and [policy](/guides/policy/) are independent layers that combine in a single call: `agentstategraph_policy_evaluate_change_with_taints` returns `{ decision, taint_status, can_proceed }`, where `can_proceed` is true only when the policy decision is not `deny` **and** every affected path is writable under its taints. This is the call to make before committing a sensitive change.

## Audit trail

Every taint records `agent_id`, `created_at`, and the `commit_id` that applied it, plus `resolved_at` / `resolved_by` / `resolved_reason` / `resolved_proof` when lifted. Read access over MCP is through `check_taint` (the aggregated status for a path, including inherited marks from ancestors) and `list_taints` (filterable).

## MCP tools

| Tool | Purpose |
|------|---------|
| `agentstategraph_taint` | Apply a taint (warn / block / review / isolate) to a path |
| `agentstategraph_untaint` | Remove a taint by name (reason required) |
| `agentstategraph_quarantine` | Restrict a path to an authorized-agents list |
| `agentstategraph_unquarantine` | Release a quarantine (with proof) |
| `agentstategraph_watch` | Apply an advisory watch (optional threshold auto-escalation) |
| `agentstategraph_unwatch` | Remove a watch |
| `agentstategraph_list_taints` | List active taints / quarantines / watches |
| `agentstategraph_check_taint` | Full taint status for a path, including ancestors |

See the [MCP Tools reference](/reference/mcp-tools/#taint) for parameters and examples.
