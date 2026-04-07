---
title: MCP Server
description: Connect StateGraph to Claude Code, GPT, or any MCP-compatible agent.
---

## Build and Run

```bash
git clone https://github.com/agentstatelabs/AgentStateGraph.git
cd AgentStateGraph
cargo build --release -p agentstategraph-mcp
cargo run --release -p agentstategraph-mcp
# Creates ./stategraph.db, listens on stdio
```

## Connect to Claude Code

Add to `~/.claude.json` or your project's `.mcp.json`:

```json
{
  "mcpServers": {
    "stategraph": {
      "command": "/path/to/AgentStateGraph/target/release/agentstategraph-mcp"
    }
  }
}
```

Or run from source:

```json
{
  "mcpServers": {
    "stategraph": {
      "command": "cargo",
      "args": ["run", "--release", "-p", "agentstategraph-mcp", "--manifest-path", "/path/to/AgentStateGraph/Cargo.toml"]
    }
  }
}
```

Restart Claude Code. The 20 StateGraph tools appear automatically.

## Connect to Other MCP Clients

Any MCP client that supports stdio transport works. Point it at the `agentstategraph-mcp` binary.

## Configuration

The server creates `stategraph.db` in the current working directory. To change the path, set the working directory when launching the binary.

## Available Tools (20)

### State Operations

| Tool | Description |
|------|-------------|
| `stategraph_get` | Read a value at any branch/path. Use `/` for entire state. |
| `stategraph_set` | Write a value with intent metadata (category, description, reasoning, confidence). |
| `stategraph_delete` | Remove a value, creating a commit with intent. |

### Branching

| Tool | Description |
|------|-------------|
| `stategraph_branch` | Create a branch from any ref. Supports namespaced names. |
| `stategraph_list_branches` | List all branches, optionally filtered by namespace prefix. |
| `stategraph_merge` | Schema-aware three-way merge. Returns conflicts if auto-resolution fails. |
| `stategraph_diff` | Structured typed diff between two refs (not text diffs). |

### Speculation

| Tool | Description |
|------|-------------|
| `stategraph_speculate` | Create a lightweight O(1) speculation from a ref. |
| `stategraph_spec_modify` | Apply set/delete operations within a speculation. |
| `stategraph_compare` | Compare multiple speculations side-by-side. |
| `stategraph_commit_spec` | Promote a speculation to a real commit on its base branch. |
| `stategraph_discard` | Discard a speculation. All changes freed immediately. |

### Query and Audit

| Tool | Description |
|------|-------------|
| `stategraph_log` | Commit history with full intent, reasoning, and metadata. |
| `stategraph_query` | Composable filters: agent, category, tags, reasoning text, confidence range. |
| `stategraph_blame` | Find which commit last modified a path and why. |

### Epochs

| Tool | Description |
|------|-------------|
| `stategraph_create_epoch` | Create an epoch to group related work. |
| `stategraph_seal_epoch` | Seal an epoch (immutable, tamper-evident). Cannot be undone. |
| `stategraph_list_epochs` | List all epochs with status, dates, and commit counts. |

### Sessions

| Tool | Description |
|------|-------------|
| `stategraph_sessions` | List active agent sessions with parent-child relationships. |

## Example Conversation

```
You: Store the cluster config under /cluster
Agent: [calls stategraph_set with path="/cluster/name", value="prod", ...]

You: Try two network approaches and compare them
Agent: [calls stategraph_speculate twice, stategraph_spec_modify on each,
        stategraph_compare to diff them, stategraph_commit_spec on winner]

You: Who changed the network config and why?
Agent: [calls stategraph_blame with path="/cluster/network"]
```

See the full [MCP Tools Reference](/reference/mcp-tools/) for parameters and example payloads.
