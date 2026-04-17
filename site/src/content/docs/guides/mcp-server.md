---
title: MCP Server
description: Connect AgentStateGraph to Claude Code, GPT, or any MCP-compatible agent.
---

## Build and Run

```bash
git clone https://github.com/agentstatelabs/AgentStateGraph.git
cd AgentStateGraph
cargo build --release -p agentstategraph-mcp
cargo run --release -p agentstategraph-mcp
# Creates ./agentstategraph.db, listens on stdio
```

## Connect to Claude Code

Add to `~/.claude.json` or your project's `.mcp.json`:

```json
{
  "mcpServers": {
    "agentstategraph": {
      "command": "/path/to/AgentStateGraph/target/release/agentstategraph-mcp"
    }
  }
}
```

Or run from source:

```json
{
  "mcpServers": {
    "agentstategraph": {
      "command": "cargo",
      "args": ["run", "--release", "-p", "agentstategraph-mcp", "--manifest-path", "/path/to/AgentStateGraph/Cargo.toml"]
    }
  }
}
```

Restart Claude Code. The 27 AgentStateGraph tools appear automatically.

## HTTP REST API

The same binary also supports HTTP mode — 22 REST endpoints with CORS enabled:

```bash
cargo run --release -p agentstategraph-mcp -- --http --port 3001
```

```bash
curl http://localhost:3001/api/health
curl http://localhost:3001/api/stats/main
curl http://localhost:3001/api/state/main?path=/cluster/name
curl "http://localhost:3001/api/blame/main?path=/cluster/name"
curl "http://localhost:3001/api/state/main/search?query=mesh"
```

Full endpoint list:

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/health` | Health check |
| GET | `/api/stats/:ref` | Summary statistics |
| GET | `/api/state/:ref?path=/x` | Read state value |
| GET | `/api/state/:ref/paths` | List all paths |
| GET | `/api/state/:ref/search?query=x` | Full-text search values |
| POST | `/api/state/:ref/set` | Write value with intent |
| POST | `/api/state/:ref/delete` | Delete value with intent |
| GET | `/api/log/:ref` | Commit log |
| GET | `/api/blame/:ref?path=/x` | Blame a path |
| GET | `/api/diff?ref_a=x&ref_b=y` | Diff two refs |
| POST | `/api/query/:ref` | Query with composable filters |
| GET | `/api/graph/:ref` | Commit DAG |
| GET | `/api/branches` | List branches |
| POST | `/api/branches` | Create branch |
| POST | `/api/merge` | Merge branches |
| GET | `/api/epochs` | List epochs |
| POST | `/api/epochs` | Create epoch |
| POST | `/api/epochs/seal` | Seal epoch |
| GET | `/api/intents/:ref` | Intent decomposition tree |

## Connect to Other MCP Clients

Any MCP client that supports stdio transport works. Point it at the `agentstategraph-mcp` binary.

## Configuration

```bash
agentstategraph-mcp [OPTIONS]

OPTIONS:
  -s, --storage <TYPE>  Storage backend: sqlite (default) or memory
  -p, --path <PATH>     SQLite database path (default: ./agentstategraph.db)
      --http            Run as HTTP REST API instead of MCP stdio
      --port <PORT>     HTTP port (default: 3001, requires --http)
  -h, --help            Print help with full endpoint list
```

## Available Tools (26)

### State Operations

| Tool | Description |
|------|-------------|
| `agentstategraph_get` | Read a value at any branch/path. Use `/` for entire state. |
| `agentstategraph_set` | Write a value with intent metadata (category, description, reasoning, confidence). |
| `agentstategraph_delete` | Remove a value, creating a commit with intent. |

### Branching

| Tool | Description |
|------|-------------|
| `agentstategraph_branch` | Create a branch from any ref. Supports namespaced names. |
| `agentstategraph_list_branches` | List all branches, optionally filtered by namespace prefix. |
| `agentstategraph_merge` | Schema-aware three-way merge. Returns conflicts if auto-resolution fails. |
| `agentstategraph_diff` | Structured typed diff between two refs (not text diffs). |

### Speculation

| Tool | Description |
|------|-------------|
| `agentstategraph_speculate` | Create a lightweight O(1) speculation from a ref. |
| `agentstategraph_spec_modify` | Apply set/delete operations within a speculation. |
| `agentstategraph_compare` | Compare multiple speculations side-by-side. |
| `agentstategraph_commit_spec` | Promote a speculation to a real commit on its base branch. |
| `agentstategraph_discard` | Discard a speculation. All changes freed immediately. |

### Query and Audit

| Tool | Description |
|------|-------------|
| `agentstategraph_log` | Commit history with full intent, reasoning, and metadata. |
| `agentstategraph_query` | Composable filters: agent, category, tags, reasoning text, confidence range. |
| `agentstategraph_blame` | Find which commit last modified a path and why. |

### Epochs

| Tool | Description |
|------|-------------|
| `agentstategraph_create_epoch` | Create an epoch to group related work. |
| `agentstategraph_seal_epoch` | Seal an epoch (immutable, tamper-evident). Cannot be undone. |
| `agentstategraph_list_epochs` | List all epochs with status, dates, and commit counts. |

### Sessions

| Tool | Description |
|------|-------------|
| `agentstategraph_sessions` | List active agent sessions with parent-child relationships. |

### Explorer & Viewer

| Tool | Description |
|------|-------------|
| `agentstategraph_list_paths` | List all leaf paths in the state tree under a prefix. |
| `agentstategraph_get_tree` | Get entire subtree as nested JSON (batch read). |
| `agentstategraph_search` | Full-text search across state values and key names. |
| `agentstategraph_stats` | Summary statistics: commits, branches, paths, epochs, agents. |
| `agentstategraph_commit_graph` | Commit DAG with parents, agents, categories for visualization. |
| `agentstategraph_intent_tree` | Intent decomposition hierarchy across agents. |

## Example Conversation

```
You: Store the cluster config under /cluster
Agent: [calls agentstategraph_set with path="/cluster/name", value="prod", ...]

You: Try two network approaches and compare them
Agent: [calls agentstategraph_speculate twice, agentstategraph_spec_modify on each,
        agentstategraph_compare to diff them, agentstategraph_commit_spec on winner]

You: Who changed the network config and why?
Agent: [calls agentstategraph_blame with path="/cluster/network"]
```

See the full [MCP Tools Reference](/reference/mcp-tools/) for parameters and example payloads.
