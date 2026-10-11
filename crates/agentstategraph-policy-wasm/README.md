# agentstategraph-policy-wasm

A host-side `ExternalEvaluator` for
[`agentstategraph-policy`](https://crates.io/crates/agentstategraph-policy)
that runs WASM policy modules with `wasmtime`. It is not compiled to WASM
itself; it runs a WASM module inside the host process.

The module exports a linear memory named `memory` and the functions
`asg_alloc`, `asg_free` and `asg_evaluate`, and exchanges JSON with the host.
The full ABI is documented in the crate docs and in
[POLICY-EVALUATOR-ABI.md](https://github.com/agentstatelabs/agentstategraph/blob/main/docs/POLICY-EVALUATOR-ABI.md).

Part of [AgentStateGraph](https://github.com/agentstatelabs/agentstategraph). Most applications depend on the
[`agentstategraph`](https://crates.io/crates/agentstategraph) crate, which
re-exports the core types and storage backends.

## License

Licensed under either of
[Apache License, Version 2.0](https://github.com/agentstatelabs/agentstategraph/blob/main/LICENSE-APACHE)
or [MIT license](https://github.com/agentstatelabs/agentstategraph/blob/main/LICENSE-MIT),
at your option.
