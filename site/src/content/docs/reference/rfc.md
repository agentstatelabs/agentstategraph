---
title: RFC Specification
description: Overview of the AgentStateGraph RFC-0001 specification and what each section covers.
---

The full specification lives at [`spec/STATEGRAPH-RFC.md`](https://github.com/agentstatelabs/AgentStateGraph/blob/main/spec/STATEGRAPH-RFC.md) in the repository.

**Status:** Draft
**Authors:** Craig Brown
**Created:** 2026-04-04

---

## Section Overview

### 1. Motivation

Why AgentStateGraph exists. Covers the four architecture eras (monolithic, batch, streaming, intent-based), the provenance gap in AI systems, the shift from single-agent to orchestrator patterns, what existing tools lack, and what StateGraph provides.

### 2. Glossary

Precise definitions for every term in the spec: Object, ObjectId, Commit, Intent, Authority, Resolution, Deviation, NotificationPolicy, Ref, Branch, Tag, HEAD, Session, Speculation, MergeProposal, DiffOp, CAS, Principal, DelegationLink.

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

How StateGraph exposes itself as an MCP server.

- **7.1 Tools** -- All 20 tools with full parameter schemas, descriptions, and example inputs/outputs.
- **7.2 Resources** -- MCP resource endpoints for state at paths.
- **7.3 Events** -- MCP event notifications for state changes.

### 8. Architecture

Implementation structure.

- **8.1 Crate Structure** -- `agentstategraph-core` (types, diff, merge), `agentstategraph-storage` (pluggable backends), `stategraph` (high-level API), `agentstategraph-mcp` (MCP server).
- **8.2 Storage Traits** -- The `Storage` trait interface for pluggable backends (Memory, SQLite, IndexedDB).
- **8.3 Performance Design** -- Content-addressed deduplication, O(1) branch creation, copy-on-write speculation.
- **8.4 Language Bindings** -- Python (PyO3), TypeScript (napi-rs), Go (C FFI), WASM (wasm-bindgen).

### 9. Human-Agent Collaboration

How humans and agents share the same state store.

- **9.1 Shared Interface** -- Both use the same API; no separate "admin" interface.
- **9.2 Approval Gates** -- MergeProposals that require human approval before merging.
- **9.3 Transparency** -- Every agent action is auditable via log, query, and blame.
- **9.4 Web UI** -- Future plans for visual state exploration, diff review, and approval workflows.
- **9.5 Graduated Trust** -- Enterprise adoption path from full-approval to autonomous operation.

### 10. Lifecycle Management: Epochs and the Registry

Managing state growth over time.

- **10.1 The Growth Problem** -- Unbounded history accumulation and how to manage it.
- **10.2 Epochs** -- Bounded, sealable segments of work. Open/Sealed/Archived lifecycle. Merkle root hash for tamper evidence. Exportable as self-contained audit bundles.
- **10.3 The Registry** -- Metadata catalog for discovering and navigating state stores, epochs, and schemas.
- **10.4 MCP Tools** -- Epoch and registry management tools.

### 11. Reference Implementation

Implementation details and test coverage.

- **11.1 Principles** -- Correctness over performance, content-addressed everything, zero unsafe.
- **11.2 Rust Reference Library** -- The `stategraph` crate with Repository API.
- **11.3 MCP Server** -- The `agentstategraph-mcp` crate with all 20 tools.
- **11.4 Getting Started Example** -- End-to-end code walkthrough.
- **11.5 Implementation Test Suite** -- 137 tests covering all operations.

### 12. Open Questions

Active design discussions: distributed federation, real-time sync, schema registry, and more.
