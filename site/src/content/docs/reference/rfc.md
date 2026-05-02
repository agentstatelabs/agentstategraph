---
title: RFC Specification
description: Overview of the AgentStateGraph RFC-0001 specification and what each section covers.
---

The full specification lives at [`spec/STATEGRAPH-RFC.md`](https://github.com/agentstatelabs/AgentStateGraph/blob/main/spec/STATEGRAPH-RFC.md) in the repository.

**Status:** Stable
**Authors:** Craig Brown
**Created:** 2026-04-04
**Updated:** 2026-05-02
**Version:** 0.8.0

---

## Section Overview

### 1. Motivation

Why AgentStateGraph exists. Covers the four architecture eras (monolithic, batch, streaming, intent-based), the provenance gap in AI systems, the shift from single-agent to orchestrator patterns, what existing tools lack, and what AgentStateGraph provides.

### 2. Glossary

Precise definitions for every term in the spec: Object, ObjectId, Commit, Intent, Authority, Resolution, Deviation, NotificationPolicy, Ref, Branch, Tag, HEAD, Session, Speculation, MergeProposal, DiffOp, CAS, Principal, DelegationLink, Epoch, Taint, Quarantine, Watch, TaintStore, Policy, PolicyProposal, Ratification, Reminder, Schedule, Priority, ReminderRef, ExecutionRecord, ReminderStore.

### 3. Core Data Model

The foundational types.

- **3.1 Objects** -- Atoms (null, bool, int, float, string, bytes) and Nodes (Map, List, Set). All content-addressed via BLAKE3.
- **3.2 Commits** -- Immutable records linking state trees to history. Includes agent identity, authority with delegation chains, structured intent with lifecycle, reasoning, confidence, and tool call provenance.
- **3.3 Refs** -- Named pointers to commits. Mutable branches, immutable tags, and per-session HEAD.
- **3.4 State Addressing** -- JSON-path addressing for nested state (`/cluster/nodes/0/hostname`).

### 4. Operations

All state operations with formal semantics.

- **4.1 State Operations** -- get, set, delete, set_json, get_json. Every write is an atomic commit.
- **4.2 Branch Operations** -- create, delete, list (with namespace prefix filtering).
- **4.3 Diff Operations** -- Structured, typed diffs (SetValue, AddKey, RemoveKey, ChangeType, AddListItem, RemoveListItem).
- **4.4 Unified Query Interface** -- Composable filters on agent, intent category, tags, reasoning text, confidence range, authority principal, date range, path, and deviation status.
- **4.5 Speculative Execution** -- O(1) branch creation, isolated modification, side-by-side comparison, commit or discard.
- **4.6 Watch / Subscribe** -- Path-based subscriptions for reactive state observation.

### 5. Multi-Agent Coordination

How multiple agents work on the same state concurrently.

- **5.1 Concurrency Model** -- Optimistic concurrency with compare-and-swap (CAS) on refs.
- **5.2 Branch-Per-Agent Pattern** -- Isolation via namespaced branches.
- **5.3 Agent Sessions** -- Working contexts with agent identity, branch, HEAD, path scope, and parent-child relationships.
- **5.4 Sub-Agent Orchestration** -- Intent decomposition, delegation with authority chains, scoped sub-agent sessions, and structured resolution reporting.
- **5.5 Conflict Resolution** -- Three-way merge with schema-aware auto-resolution and manual conflict reporting.

### 6. Schema System

Optional schema annotations for validation and merge behavior.

- **6.1 Overview** -- Schemas are advisory by default, enforceable when needed.
- **6.2 Schema Format** -- Type annotations, required fields, defaults, enums, ranges.
- **6.3 Merge Hints** -- CRDT-inspired annotations: `union-by-id`, `sum`, `max`, `min`, `last-writer-wins`, `set-union`.
- **6.4 Enforcement Modes** -- Advisory (log warnings), strict (reject invalid writes), migration (old values tolerated).
- **6.5 Schema Evolution** -- Versioned schemas with migration paths.

### 7. MCP Interface

How AgentStateGraph exposes itself as an MCP server.

- **7.1 Tools** -- All 66 tools across 7 groups: 29 core (state, branching, speculation, query/audit, epochs, sessions, explorer), 10 tasks, 11 policy, 9 taint, 7 reminders. Full parameter schemas, descriptions, and example inputs/outputs documented in the [MCP Tools Reference](/reference/mcp-tools).
- **7.2 Resources** -- MCP resource endpoints for state at paths.
- **7.3 Events** -- MCP event notifications for state changes.

### 8. Taint, Quarantine, and Watch

Mark-and-sweep safety controls for agent state.

- **8.1 Taint Marks** -- Three kinds: `Taint` (suspicious data), `Quarantine` (blocks changes until reviewed), `Watch` (audit notifications). Each carries a path, severity, effect, reason, and optional expiry.
- **8.2 Propagation** -- Taints propagate to descendant paths by default (`propagate = true`). `check_taint` returns all applicable taints for a request path, including ancestor matches.
- **8.3 Resolution** -- Taints are resolved (not deleted) to preserve the audit trail. Resolved taints are stamped with resolved\_by, reason, and optional proof.
- **8.4 Policy Integration** -- `policy_evaluate_change_with_taints` combines policy and taint evaluation in one call; a Quarantined path blocks policy evaluation entirely.

### 9. Authorization Policy

Declarative, multi-engine access control with tamper-evident ratification.

- **9.1 Policy Lifecycle** -- Propose → Ratify → Active (or Supersede). Every transition is an audit commit. Policies cannot be silently modified.
- **9.2 Evaluation Engines** -- Pluggable evaluators: built-in rule engine, Rego (OPA), WASM (any language compiled to WASM), Cedar (Amazon). Engine kind is stored with the policy.
- **9.3 Ed25519 Signing** -- Policies can be signed with Ed25519 keys. `policy_sign` canonicalizes the policy (sorted JSON, signature field excluded) before signing. `policy_verify` checks all signatures. Ratification requires valid signatures from all required signers.
- **9.4 Cost-of-Change Gating** -- Policies can encode token-cost thresholds and fallback actions (Allow, Deny, Escalate, RequestApproval) for AI agents operating within budget constraints.
- **9.5 MCP Tools** -- 11 tools: policy_propose, policy_ratify, policy_sign, policy_verify, policy_supersede, policy_show, policy_list, policy_history, policy_evaluate, policy_evaluate_change, policy_check_tokens.

### 10. Reminders

Pull-based scheduled work for agents and users.

- **10.1 Pull Model** -- Agents call `remind_me()` at checkpoints (session start, task transitions, branch switches). No background timers or push delivery. Lazy status promotion: `remind_me()` scans Pending items and promotes past-due ones to Due on each call.
- **10.2 Priority and Scheduling** -- Five priority levels (Critical → Minimal). Four schedule kinds: `Once`, `Interval` (every N seconds), `Daily` (HH:MM UTC), `Weekly` (Weekday + HH:MM UTC). After a successful execution the next due time is computed and status resets automatically.
- **10.3 Autonomous Flag** -- `true`: agent executes without asking. `false`: reminder transitions to `AwaitingPermission`; requires an explicit `approve()` call before execution proceeds.
- **10.4 Soft References** -- Advisory refs to branches, memories, plans, tasks, state paths, or external URLs. Label is captured at creation time and survives renames. `stale` flag is set lazily if the target can no longer be resolved; does not invalidate the reminder.
- **10.5 Execution Audit** -- Every run is recorded with start/end time, agent id, result (success/failed/cancelled), notes, and optional task id. History accumulates; the reminder is never overwritten.
- **10.6 MCP Tools** -- 7 tools: reminder_create, reminder_list, reminder_remind_me, reminder_snooze, reminder_approve, reminder_cancel, reminder_record_execution.

### 11. Architecture

Implementation structure.

- **11.1 Crate Structure** -- 12 Rust crates:
  - `agentstategraph-core` — types, diff, merge, BLAKE3 content addressing
  - `agentstategraph-storage` — pluggable storage backends (Memory, SQLite, Postgres, IndexedDB); 7-trait `Storage` supertrait
  - `agentstategraph` — high-level Repository API
  - `agentstategraph-mcp` — MCP server (66 tools) + HTTP REST API + `migrate` CLI
  - `agentstategraph-tasks` — shared Plan/Task state machine, proofs, assignment
  - `agentstategraph-migrate` — schema-evolution framework and migration registry
  - `agentstategraph-policy` — authorization + cost-of-change gating with fallback actions
  - `agentstategraph-policy-sign` — Ed25519 signing for policy ratification
  - `agentstategraph-policy-wasm` — WASM host runner for policy evaluation
  - `agentstategraph-policy-cedar` — Cedar engine integration
  - `agentstategraph-taint` — taint/quarantine/watch mark-and-sweep primitive
  - `agentstategraph-reminders` — pull-based reminders: priority, schedules, soft refs, autonomous flag
- **11.2 Storage Traits** -- The `Storage` supertrait combines 7 sub-traits: `ObjectStore`, `CommitStore`, `RefStore`, `EpochStore`, `SessionStore`, `TaintStore`, `ReminderStore`. All four backends (Memory, SQLite, Postgres, IndexedDB) fully satisfy the supertrait. `TaintStore` and `ReminderStore` provide default no-op implementations so custom backends can opt in gradually.
- **11.3 Performance Design** -- Content-addressed deduplication, O(1) branch creation, copy-on-write speculation.
- **11.4 Language Bindings** -- Python (PyO3), TypeScript (napi-rs), Go (C FFI / CGo), .NET (C FFI), WASM (wasm-bindgen).

### 12. Human-Agent Collaboration

How humans and agents share the same state store.

- **12.1 Shared Interface** -- Both use the same API; no separate "admin" interface.
- **12.2 Approval Gates** -- MergeProposals that require human approval before merging.
- **12.3 Transparency** -- Every agent action is auditable via log, query, and blame.
- **12.4 Web UI** -- Future plans for visual state exploration, diff review, and approval workflows.
- **12.5 Graduated Trust** -- Enterprise adoption path from full-approval to autonomous operation.

### 13. Lifecycle Management: Epochs and the Registry

Managing state growth over time.

- **13.1 The Growth Problem** -- Unbounded history accumulation and how to manage it.
- **13.2 Epochs** -- Bounded, sealable segments of work. Open/Sealed/Archived lifecycle. Merkle root hash for tamper evidence. Exportable as self-contained audit bundles.
- **13.3 The Registry** -- Metadata catalog for discovering and navigating state stores, epochs, and schemas.
- **13.4 MCP Tools** -- Epoch and registry management tools.

### 14. Reference Implementation

Implementation details and test coverage.

- **14.1 Principles** -- Correctness over performance, content-addressed everything, zero unsafe.
- **14.2 Rust Reference Library** -- The `agentstategraph` crate with Repository API.
- **14.3 MCP Server** -- The `agentstategraph-mcp` crate with all 66 tools plus the `migrate` subcommand.
- **14.4 Getting Started Example** -- End-to-end code walkthrough.
- **14.5 Implementation Test Suite** -- 848+ tests across 12 crates covering all operations, storage backends, policy engines, taint propagation, and reminder lifecycle.

### 15. Open Questions

Active design discussions: distributed federation, real-time sync, schema registry, and more.
