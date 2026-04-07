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
