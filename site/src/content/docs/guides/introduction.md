---
title: Introduction
description: What AgentStateGraph is and why it exists.
---

AgentStateGraph is a content-addressed, versioned, branchable structured state store designed as an infrastructure primitive for intent-based systems.

## The Problem

AI agents don't execute linear scripts — they explore state spaces. An agent asked to "set up a cluster for ML training" tries different approaches, compares outcomes, and picks winners. It needs to:

- **Branch** to try approaches without risk
- **Compare** outcomes side-by-side
- **Merge** the winner back
- **Report** what was done, what deviated, and why
- **Record** the full reasoning chain for audit

No existing tool supports this natively. Git is text-oriented. Databases lack branching. Event sourcing is append-only.

## What AgentStateGraph Provides

Every state change in AgentStateGraph captures the **full provenance chain**:

| Field | Question |
|-------|----------|
| `state_root` | What changed? |
| `intent` | Why? |
| `reasoning` | How did the agent decide? |
| `confidence` | How sure was it? |
| `agent_id` | Who did it? |
| `authority` | Who authorized it? |
| `resolution` | What was accomplished? Deviations? |

## Key Features

- **Content-addressed Merkle DAG** — immutable, deduplicated history
- **Schema-aware merge** — CRDT-inspired conflict resolution
- **Speculative execution** — O(1) branching, instant discard
- **Multi-agent orchestration** — scoped sessions, delegation, intent trees
- **Epochs** — sealable, tamper-evident audit bundles
- **26 MCP tools** — any agent can connect immediately
- **HTTP REST API** — 19 endpoints with CORS, run with `--http`
- **Browser explorer** — interactive data viewer at [agentstategraph.dev/explorer/](https://agentstategraph.dev/explorer/)
- **6 language bindings** — Rust, Python, TypeScript, Go, WASM, C FFI
