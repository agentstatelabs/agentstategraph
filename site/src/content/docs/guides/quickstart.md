---
title: Quick Start
description: Get AgentStateGraph running in under 5 minutes.
---

## Option 1: MCP Server (connect to Claude, GPT, any agent)

```bash
git clone https://github.com/agentstatelabs/AgentStateGraph.git
cd AgentStateGraph
cargo build --release -p agentstategraph-mcp
cargo run --release -p agentstategraph-mcp
```

Add to Claude Code config:
```json
{
  "mcpServers": {
    "stategraph": {
      "command": "/path/to/AgentStateGraph/target/release/agentstategraph-mcp"
    }
  }
}
```

Then ask your agent to use AgentStateGraph tools.

## Option 2: Rust Library

```bash
cargo add stategraph agentstategraph-core agentstategraph-storage
```

```rust
use stategraph::{Repository, CommitOptions};
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

## Option 3: Python

```bash
cd bindings/python
python3 -m venv .venv && source .venv/bin/activate
pip install maturin && maturin develop --release
```

```python
from agentstategraph_py import StateGraph

sg = StateGraph("state.db")  # SQLite, or StateGraph() for in-memory
sg.set("/name", "my-project", "Init", category="Checkpoint")
sg.branch("feature")
sg.set("/version", "2.0", "Try v2", ref="feature", category="Explore")
sg.merge("feature", description="Adopt v2")
```

## Option 4: TypeScript

```bash
cd bindings/typescript
npm install && npm run build
```

```typescript
const { StateGraph } = require('stategraph')
const sg = new StateGraph()  // or new StateGraph("state.db")
sg.set("/name", "my-project", "Init", undefined, "Checkpoint")
sg.branch("feature")
sg.set("/version", "2.0", "Try v2", "feature", "Explore")
sg.merge("feature", undefined, "Adopt v2")
```

## Run an Example

```bash
cargo run --example getting_started -p stategraph
cargo run --example agent_workflow -p stategraph
cargo run --example multi_agent -p stategraph
```
