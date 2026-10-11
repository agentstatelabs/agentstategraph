# agentstategraph-policy-rego

An OPA/Rego `ExternalEvaluator` for
[`agentstategraph-policy`](https://crates.io/crates/agentstategraph-policy).
`RegoEvaluator` runs the `opa` binary from `$PATH` (or an explicit path),
streams the evaluation input on stdin, and reads the result from
`data.policy.decision`.

The Rego module must declare `package policy` and define a `decision` rule
that evaluates to a `Decision` object. Requires `opa` at runtime.

Part of [AgentStateGraph](https://github.com/agentstatelabs/agentstategraph). Most applications depend on the
[`agentstategraph`](https://crates.io/crates/agentstategraph) crate, which
re-exports the core types and storage backends.

## License

Licensed under either of
[Apache License, Version 2.0](https://github.com/agentstatelabs/agentstategraph/blob/main/LICENSE-APACHE)
or [MIT license](https://github.com/agentstatelabs/agentstategraph/blob/main/LICENSE-MIT),
at your option.
