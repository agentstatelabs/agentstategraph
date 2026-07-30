# agentstategraph-tasks

Shared task-store primitives built on [AgentStateGraph](https://github.com/agentstatelabs/agentstategraph).

This crate provides `Plan`, `Task`, `Proof`, and `TaskStore` types so that
multiple ASG consumers share a single set
of types, a single state machine, and a single verification surface instead of
reimplementing them independently.

## Pattern

This crate establishes a pattern for opinionated-but-shared primitives in the
ASG workspace:

> If a capability is opinionated but shared across multiple ASG consumers,
> it goes in a sibling crate under `crates/agentstategraph-<name>`.
> ASG core stays primitive. Consumers that don't need the capability don't
> depend on the sibling crate.

## Example

```rust
use agentstategraph::Repository;
use agentstategraph_storage::MemoryStorage;
use agentstategraph_tasks::{Priority, Proof, TaskStore};
use std::sync::Arc;

let repo = Arc::new(Repository::new(Box::new(MemoryStorage::new())));
repo.init().unwrap();

let store = TaskStore::new(repo, "/plans", "claude-code");

store.create_plan("main", "website-v2", Some("Brand pivot".into())).unwrap();

let task = store.add_task(
    "main", "website-v2", "Rewrite hero",
    Priority::High, None, vec![], Some("claude-code".into()),
).unwrap();

store.start_task("main", "website-v2", &task.id).unwrap();

store.complete_task(
    "main", "website-v2", &task.id,
    Proof::commit("abc123"),
).unwrap();
```

## State machine

```text
pending ──start──> in_progress ──complete(proof)──> done          (terminal)
   │                    │
   └───abandon(reason)──┴──> abandoned                            (terminal)
```

Terminal states cannot be re-opened. Create a new task to redo work.

## Proof verification

The crate defines the `Verifier` trait. Consumers implement it against their
environment:

- A **code/CI consumer** ships a `GitFileTestVerifier` (checks commit
  reachability, file existence, test suite membership).
- **A chat app** ships `ChatVerifier` (validates text proofs against chat
  log content).
- A `NoopVerifier` is included for tests and fallback.

Call `store.verify_plan(ref, plan, &verifier)` to walk every `done` task and
produce a `VerifyReport` with per-task results (`Verified`, `Decayed`, or
`Unverifiable`).

## Storage layout

All data lives under the prefix bound at `TaskStore::new` time:

```
<prefix>/
  <plan-name>/
    _meta       -> Plan JSON
    t-001       -> Task JSON
    t-002       -> Task JSON
```

## Watch integration

Subscribe to plan changes with the existing ASG watch API:

```rust
use agentstategraph::PathPattern;

let sub = repo.watches().subscribe(
    PathPattern::Prefix("/plans/website-v2/".to_string())
);
```

## Links

- [AgentStateGraph](https://github.com/agentstatelabs/agentstategraph)
