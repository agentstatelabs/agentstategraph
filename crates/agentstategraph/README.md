# agentstategraph

A versioned state store for AI agents: content-addressed, branchable, and
blameable. Every commit records structured intent — who made the change, on
whose authority, why, and with what confidence — so agent work can be audited
and governed mechanically.

This is the high-level `Repository` API. It ties together
[`agentstategraph-core`](https://crates.io/crates/agentstategraph-core) (types
and algorithms) and
[`agentstategraph-storage`](https://crates.io/crates/agentstategraph-storage)
(pluggable backends), and re-exports both as `agentstategraph::core` and
`agentstategraph::storage`.

```sh
cargo add agentstategraph
```

## Quick start

```rust,no_run
use agentstategraph::core::{IntentCategory, Object};
use agentstategraph::storage::SqliteStorage;
use agentstategraph::{CommitOptions, Repository};

let repo = Repository::new(Box::new(SqliteStorage::open("state.db")?));
repo.init()?;

// Every write is an atomic commit carrying intent metadata.
repo.set(
    "main",
    "/app/name",
    &Object::string("my-project"),
    CommitOptions::new("developer", IntentCategory::Checkpoint, "Initialize project"),
)?;

// Branches are instant. Explore on one, then merge it back.
repo.branch("feature/dark-mode", "main")?;
repo.set(
    "feature/dark-mode",
    "/app/theme",
    &Object::string("dark"),
    CommitOptions::new("developer", IntentCategory::Explore, "Try dark mode"),
)?;
let changes = repo.diff("main", "feature/dark-mode")?;
repo.merge(
    "feature/dark-mode",
    "main",
    CommitOptions::new("developer", IntentCategory::Merge, "Adopt dark mode"),
)?;

for commit in repo.log("main", 10)? {
    println!("{} {}", commit.id.short(), commit.intent.description);
}
```

## Features

| Feature | Default | What it enables |
|---|---|---|
| `sqlite` | yes | The SQLite backend (`SqliteStorage`, bundled SQLite) |

The in-memory and Postgres backends are features of
[`agentstategraph-storage`](https://crates.io/crates/agentstategraph-storage).

## The crate family

| Crate | Purpose |
|---|---|
| [`agentstategraph-core`](https://crates.io/crates/agentstategraph-core) | Objects, commits, paths and merge — no I/O |
| [`agentstategraph-storage`](https://crates.io/crates/agentstategraph-storage) | Storage traits and the memory, SQLite and Postgres backends |
| [`agentstategraph-tasks`](https://crates.io/crates/agentstategraph-tasks) | Shared plans and tasks with proofs |
| [`agentstategraph-policy`](https://crates.io/crates/agentstategraph-policy) | Authorization and cost-of-change gating |
| [`agentstategraph-taint`](https://crates.io/crates/agentstategraph-taint) | Taint, quarantine and watch markers |
| [`agentstategraph-reminders`](https://crates.io/crates/agentstategraph-reminders) | Pull-based agent reminders |
| [`agentstategraph-migrate`](https://crates.io/crates/agentstategraph-migrate) | Schema migrations for existing stores |
| [`agentstategraph-mcp`](https://crates.io/crates/agentstategraph-mcp) | MCP server and HTTP API (`cargo install agentstategraph-mcp`) |
| [`agentstategraph-ffi`](https://crates.io/crates/agentstategraph-ffi) | C ABI for other languages |
| [`agentstategraph-wasm`](https://crates.io/crates/agentstategraph-wasm) | WASM bindings for browsers, Deno and Node |

## More

- [Project README](https://github.com/agentstatelabs/agentstategraph#readme)
- [RFC](https://github.com/agentstatelabs/agentstategraph/blob/main/spec/AGENTSTATEGRAPH-RFC.md)
  — data model, intent lifecycle, authority and delegation
- [Examples](https://github.com/agentstatelabs/agentstategraph/tree/main/crates/agentstategraph/examples)

## License

Licensed under either of
[Apache License, Version 2.0](https://github.com/agentstatelabs/agentstategraph/blob/main/LICENSE-APACHE)
or [MIT license](https://github.com/agentstatelabs/agentstategraph/blob/main/LICENSE-MIT),
at your option.
