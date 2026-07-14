---
title: Quick Start
description: Get AgentStateGraph running in under 5 minutes.
---

## Option 0: One-line install (Mac / Linux)

```bash
curl -sSL https://agentstategraph.dev/install.sh | sh
```

Detects your platform, downloads the prebuilt binary, installs to `~/.local/bin`. Then:

```bash
agentstategraph-mcp
```

## Option 1: Build from source (MCP Server)

```bash
git clone https://github.com/agentstatelabs/agentstategraph.git
cd AgentStateGraph
cargo build --release -p agentstategraph-mcp
cargo run --release -p agentstategraph-mcp
```

Add to Claude Code config:
```json
{
  "mcpServers": {
    "agentstategraph": {
      "command": "/path/to/AgentStateGraph/target/release/agentstategraph-mcp"
    }
  }
}
```

Then ask your agent to use AgentStateGraph tools.

## Option 2: HTTP REST API

Same binary, add `--http`:

```bash
cargo run --release -p agentstategraph-mcp -- --http --port 3001
```

```bash
# Health check
curl http://localhost:3001/api/health

# Set a value with intent
curl -X POST http://localhost:3001/api/state/main/set \
  -H "Content-Type: application/json" \
  -d '{"path":"/app/name","value":"my-project","intent_category":"Checkpoint","intent_description":"Init"}'

# Read it back
curl http://localhost:3001/api/state/main?path=/app/name

# Blame — who changed it and why
curl "http://localhost:3001/api/blame/main?path=/app/name"

# Search across all values
curl "http://localhost:3001/api/state/main/search?query=project"

# Stats dashboard
curl http://localhost:3001/api/stats/main
```

A full REST API with CORS enabled — see the [MCP Server guide](/guides/mcp-server/#http-rest-api) for the full list.

## Option 3: Rust Library (embed in your own app)

```bash
cargo add agentstategraph agentstategraph-core agentstategraph-storage
```

```rust
use agentstategraph::{Repository, CommitOptions};
use agentstategraph_storage::SqliteStorage;
use agentstategraph_core::{IntentCategory, Object};

let storage = SqliteStorage::open("./state.db").unwrap();
let repo = Repository::new(Box::new(storage));
repo.init().unwrap();

// Set state — every write is an atomic commit with intent
repo.set("main", "/app/name", &Object::string("my-project"),
    CommitOptions::new("developer", IntentCategory::Checkpoint, "Init"));

// Read it back
let name = repo.get_json("main", "/app/name").unwrap();

// Branch, modify, diff, merge
repo.branch("feature", "main").unwrap();
repo.set("feature", "/app/version", &Object::string("2.0"),
    CommitOptions::new("developer", IntentCategory::Explore, "Try v2"));
let diff = repo.diff("main", "feature").unwrap();
repo.merge("feature", "main",
    CommitOptions::new("developer", IntentCategory::Merge, "Adopt v2"));
```

## Option 4: Python

```bash
cd bindings/python
python3 -m venv .venv && source .venv/bin/activate
pip install maturin && maturin develop --release
```

```python
from agentstategraph_py import AgentStateGraph

sg = AgentStateGraph("state.db")  # SQLite, or AgentStateGraph() for in-memory
sg.set("/name", "my-project", "Init", category="Checkpoint")
sg.branch("feature")
sg.set("/version", "2.0", "Try v2", ref="feature", category="Explore")
sg.merge("feature", description="Adopt v2")
```

## Option 5: TypeScript

```bash
cd bindings/typescript
npm install && npm run build
```

```typescript
const { AgentStateGraph } = require('agentstategraph')
const sg = new AgentStateGraph()  // or new AgentStateGraph("state.db")
sg.set("/name", "my-project", "Init", undefined, "Checkpoint")
sg.branch("feature")
sg.set("/version", "2.0", "Try v2", "feature", "Explore")
sg.merge("feature", undefined, "Adopt v2")
```

## Run an Example

```bash
cargo run --example getting_started -p agentstategraph
cargo run --example agent_workflow -p agentstategraph
cargo run --example multi_agent -p agentstategraph
```
