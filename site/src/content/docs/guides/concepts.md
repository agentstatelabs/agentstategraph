---
title: Core Concepts
description: The building blocks of AgentStateGraph.
---

## Objects

All state is composed of **Objects** — either atoms (null, bool, int, float, string, bytes) or nodes (Map, List, Set). Every object is content-addressed via BLAKE3 hash. Two objects with identical content always produce the same ObjectId.

## Commits

A **Commit** links a state tree to its history and provenance. Beyond git's tree + parents + message, AgentStateGraph commits carry:

- **agent_id** — who performed the action
- **authority** — who authorized it, with delegation chain
- **intent** — structured "why" with category, description, tags
- **reasoning** — the agent's chain-of-thought
- **confidence** — self-assessed certainty (0.0-1.0)
- **tool_calls** — what actions produced this state change

## Intent Categories

| Category | Meaning |
|----------|---------|
| `Explore` | Trying an approach to evaluate it |
| `Refine` | Improving on a previous state |
| `Fix` | Correcting an error |
| `Rollback` | Reverting to a prior state |
| `Checkpoint` | Saving a known-good state |
| `Merge` | Combining work from branches |
| `Migrate` | Schema or structural change |
| `Plan` | Plan/task lifecycle commits |
| `Taint` | Marking a path as tainted |
| `Untaint` | Removing a taint mark |
| `Quarantine` | Isolating tainted state |
| `Unquarantine` | Lifting quarantine from a path |
| `Watch` | Placing a watch on a path |
| `Unwatch` | Removing a watch from a path |
| `PolicyPropose` | Proposing a new policy |
| `PolicyRatify` | Ratifying a proposed policy |
| `PolicySupersede` | Superseding an existing policy |
| `PolicySign` | Signing a policy with Ed25519 |
| `Custom(<value>)` | Application-defined category |

## Branches

Branches are named pointers to commits. Creation is O(1). Namespace conventions:

- `main` — primary shared state
- `agents/{id}/workspace` — per-agent working branches
- `explore/{description}` — speculative exploration
- `proposals/{id}` — merge proposals

## Speculation

A lightweight, disposable branch optimized for the "try many approaches, pick the winner" pattern. Create is O(1) (just a pointer), discard is instant.

## Epochs

Bounded segments of work that can be sealed (made immutable) and exported as tamper-evident audit bundles. The Merkle root hash makes sealed epochs cryptographically verifiable.

## Sessions

Working contexts for sub-agent orchestration. Each session has an agent identity, working branch, parent session, delegated intent, and optional path scope restriction.

## Reminders

Pull-based reminders let agents and users schedule future work without background timers. An agent calls `remind_me()` at checkpoints (branch switches, session starts, task transitions) and receives all items that are currently due. Key properties:

- **Priority** — Critical (1) through Minimal (5); `remind_me()` returns items ordered by priority then due date
- **Autonomous flag** — `true` means the agent executes immediately; `false` transitions the reminder to `AwaitingPermission` and requires an explicit `approve()` call before execution
- **Soft refs** — reminders can hold advisory references to branches, memories, plans, tasks, state paths, or external resources. The `label` is captured at creation time so the reminder stays meaningful even if the target is renamed or deleted
- **Repeating schedules** — `Once`, `Interval`, `Daily`, or `Weekly`; after a successful execution the next due time is computed and the reminder resets automatically
- **Execution history** — every run is recorded with start/end times, agent id, result, and an optional task id linking to the created task

Status lifecycle: `Pending` → `Due` (promoted lazily by `remind_me()`) → `InProgress` → `Completed` (or back to `Due` / `AwaitingPermission` for repeating reminders).
