---
title: "AgentStateGraph vs. Stategraph vs. LangGraph's StateGraph"
description: A side-by-side disambiguation of three projects whose names are easy to confuse.
---

> **AgentStateGraph is to agent state what Git was to source code — a content-addressed, branchable, blameable state primitive, designed from the ground up for AI agents as the primary actor.**

If you landed here searching for "stategraph" and weren't sure which project you meant, this page is for you. Three different things share some or all of the letters, and they target very different audiences. Here is how they compare — no combat, just clarity.

## The three projects at a glance

- **[AgentStateGraph](https://agentstategraph.dev)** *(this project)* — A content-addressed, branchable, blameable state primitive for agent-driven operations. Think "Git for agent-era state."
- **[Stategraph](https://stategraph.com)** *(by Terrateam)* — A commercial Terraform/OpenTofu backend replacement that stores state in a PostgreSQL graph for parallelism, SQL-queryable history, and drift detection. Think "a better database under Terraform."
- **[LangGraph's `StateGraph`](https://langchain-ai.github.io/langgraph/)** *(by LangChain)* — A Python class in the LangGraph library that defines an agent's execution graph as nodes and edges with a typed state dict. Think "a state machine inside your agent."

They are not competitors to each other. They occupy different layers of the stack and address different actor models. The confusion is purely lexical.

## Side-by-side

| Dimension | Terrateam Stategraph | LangGraph `StateGraph` | AgentStateGraph |
|---|---|---|---|
| **Primary purpose** | Replacement state backend for Terraform/OpenTofu | In-memory state container for a single agent's LangGraph workflow | Content-addressed, branchable, blameable state primitive for agent-driven operations |
| **Actor model** | Humans running `terraform plan`/`apply` via CI/CD | An agent executing a single defined graph in-process | Any agent or human, with explicit authority chains, delegation, and multi-agent sessions |
| **Data model** | PostgreSQL graph of Terraform resources | Python dict with reducer functions, per-run | Content-addressed Merkle DAG with structured intent metadata |
| **Branching** | Subgraph isolation for parallelism; not a user-facing branch model | None — linear execution | O(1) branches, speculations, merges; first-class |
| **Intent / reasoning** | No structured field | None | Per-commit: category, description, reasoning, confidence, authority, alternatives |
| **Blame** | SQL queries over state history | None | Per-path; surfaces who, why, when, and at what confidence |
| **Audit surface** | Encrypted state at rest, RBAC, history | Optional thread checkpoints | Tamper-evident sealed epochs |
| **Language / bindings** | Rust + Go, backs the Terraform CLI | Python only | Rust core with Python, TypeScript, Go, WASM, and C FFI bindings |
| **Primary interface** | `terraform` / `opentofu` CLI | Python library imports | MCP server (20 tools), library calls, CLI |
| **Storage backends** | PostgreSQL | Process memory (+ optional checkpointer) | Memory, SQLite, IndexedDB (browser) |
| **Closest analogy** | A better database under Terraform | A state machine inside your agent | Git for agent-era state |

## Why the differences matter

The rows that do the most work here are **actor model**, **data model**, and **branching** — because those are where the projects diverge most sharply and where misreading the name leads to the wrong mental model.

### Actor model

- **Terrateam Stategraph** assumes humans. Humans write HCL, open PRs, run `terraform plan`, and approve `terraform apply`. The accountability model is social (PR review, audit logs, GitHub approvals), and Stategraph's job is to make the *storage* under that human-driven workflow faster, queryable, and drift-aware. It is a great answer to a real problem that humans doing IaC actually have.

- **LangGraph's `StateGraph`** assumes a single agent execution. You define nodes (Python functions) and edges (transitions), and the framework runs the graph with a typed state dict that nodes read and update. The actor is one agent following one graph for one run.

- **AgentStateGraph** assumes any agent or human with **explicit authority chains** and **delegation**. An orchestrator agent can spawn sub-agents, scope them to a subtree of the state, delegate an intent, and receive back a resolution report — all with full provenance. The accountability model is mechanical: every change carries a structured `intent`, a `confidence`, a principal in the `authority` chain, and a delegation history, so correctness is verifiable from the data model rather than from GitHub comments or Slack context.

### Data model

- **Terrateam Stategraph** stores Terraform resources as a graph in PostgreSQL. Fast, queryable with SQL, great for drift detection — but shaped around Terraform's resource/provider model.
- **LangGraph's `StateGraph`** holds a Python dict with reducer functions that merge updates from each node. It lives in process memory for the duration of a run, with optional thread checkpoints for persistence.
- **AgentStateGraph** stores a content-addressed Merkle DAG where every node is hashed by BLAKE3, every commit carries structured intent metadata (category, description, tags, reasoning, confidence, authority, alternatives, resolution), and history is immutable and deduplicated by construction.

### Branching

- **Terrateam Stategraph** uses subgraph isolation internally for parallelism, but branching is not a user-facing primitive the way it is in Git.
- **LangGraph's `StateGraph`** executes a defined graph linearly (or with conditional edges); there is no notion of branching a state snapshot, exploring alternatives in parallel, and merging the winner back.
- **AgentStateGraph** makes **O(1) branching** first-class. Agents can create **speculations** to explore alternatives, `compare` them side-by-side, `commit_speculation` the winner, and `discard_speculation` the rest. This is the primitive that makes "try three approaches, pick the best one" a cheap operation instead of an expensive rollback dance.

## Which one do you want?

- If you're replacing a Terraform state backend for a team of humans running CI/CD pipelines: you want **[Terrateam's Stategraph](https://stategraph.com)**.
- If you're building a single agent workflow in Python using LangChain/LangGraph and need an in-process state machine: you want **[LangGraph's `StateGraph`](https://langchain-ai.github.io/langgraph/)**.
- If you're building the substrate for agent-driven operations — where agents take actions, need to be held mechanically accountable, and must branch, compare, merge, and audit state across runs, across agents, and across time: you're in the right place. Welcome to **AgentStateGraph**.

## A note on short forms

You will not see AgentStateGraph abbreviated to "ASG" anywhere in this project, and we ask that you don't abbreviate it that way either. "ASG" collides verbally with AWS Auto Scaling Groups, which is ubiquitous in infrastructure conversations. If the full name feels heavy, use it confidently rather than abbreviate it into another collision. We have learned this lesson the hard way — twice.
