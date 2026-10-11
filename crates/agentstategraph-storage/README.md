# agentstategraph-storage

Pluggable storage backends for AgentStateGraph.

| Backend | Feature | Use |
|---|---|---|
| `SqliteStorage` | `sqlite` (default) | Durable, single file; the default for most deployments |
| `MemoryStorage` | `memory` | Fast and ephemeral, for tests and speculation |
| `PostgresStorage` | `postgres` | Multi-tenant and connection-pooled, for shared servers |
| `IndexedDbStorage` | `indexeddb` | Browser persistence, used by `agentstategraph-wasm` |

Custom backends implement the `Storage` trait, which is a blanket impl over
`ObjectStore + CommitStore + RefStore`.

Part of [AgentStateGraph](https://github.com/agentstatelabs/agentstategraph). Most applications depend on the
[`agentstategraph`](https://crates.io/crates/agentstategraph) crate, which
re-exports the core types and storage backends.

## License

Licensed under either of
[Apache License, Version 2.0](https://github.com/agentstatelabs/agentstategraph/blob/main/LICENSE-APACHE)
or [MIT license](https://github.com/agentstatelabs/agentstategraph/blob/main/LICENSE-MIT),
at your option.
