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

Restart Claude Code. The 20 AgentStateGraph tools appear automatically.

## Connect to Other MCP Clients

Any MCP client that supports stdio transport works. Point it at the `agentstategraph-mcp` binary.

## Configuration

The server creates `agentstategraph.db` in the current working directory. To change the path, set the working directory when launching the binary.

## Available Tools (20)

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
