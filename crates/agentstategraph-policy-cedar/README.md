# agentstategraph-policy-cedar

A Cedar `ExternalEvaluator` for
[`agentstategraph-policy`](https://crates.io/crates/agentstategraph-policy).
`CedarEvaluator` runs the `cedar` binary from `$PATH` (or an explicit path),
synthesizes the entities and request for each call, and maps
`cedar authorize`'s decision to `Allow`, `Deny` or `NoPolicyMatch`.

Requires the `cedar` CLI at runtime.

Part of [AgentStateGraph](https://github.com/agentstatelabs/agentstategraph). Most applications depend on the
[`agentstategraph`](https://crates.io/crates/agentstategraph) crate, which
re-exports the core types and storage backends.

## License

Licensed under either of
[Apache License, Version 2.0](https://github.com/agentstatelabs/agentstategraph/blob/main/LICENSE-APACHE)
or [MIT license](https://github.com/agentstatelabs/agentstategraph/blob/main/LICENSE-MIT),
at your option.
