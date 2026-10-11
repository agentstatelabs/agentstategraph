# agentstategraph-core

Core types and algorithms for AgentStateGraph, with no I/O dependencies:

- Objects (atoms and nodes), content-addressed with BLAKE3
- Commits carrying intent, authority and provenance metadata
- Path addressing for navigating state trees
- Diff and schema-aware merge
- Error types

All types are serializable and designed for both agent and human use.

Part of [AgentStateGraph](https://github.com/agentstatelabs/agentstategraph). Most applications depend on the
[`agentstategraph`](https://crates.io/crates/agentstategraph) crate, which
re-exports the core types and storage backends.

## License

Licensed under either of
[Apache License, Version 2.0](https://github.com/agentstatelabs/agentstategraph/blob/main/LICENSE-APACHE)
or [MIT license](https://github.com/agentstatelabs/agentstategraph/blob/main/LICENSE-MIT),
at your option.
