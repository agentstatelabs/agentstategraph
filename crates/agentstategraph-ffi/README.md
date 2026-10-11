# agentstategraph-ffi

C ABI for AgentStateGraph: an opaque, handle-based API over C strings, built as
a shared library (`.so` / `.dylib` / `.dll`) and a static library (`.a`). The
Go, .NET and other non-Rust bindings link against it.

Strings returned by the library are owned by the caller and must be released
with `agentstategraph_free_string`.

| Feature | What it enables |
|---|---|
| `postgres` | Postgres-backed storage, via the `agentstategraph_new_postgres` extern |

Part of [AgentStateGraph](https://github.com/agentstatelabs/agentstategraph). Most applications depend on the
[`agentstategraph`](https://crates.io/crates/agentstategraph) crate, which
re-exports the core types and storage backends.

## License

Licensed under either of
[Apache License, Version 2.0](https://github.com/agentstatelabs/agentstategraph/blob/main/LICENSE-APACHE)
or [MIT license](https://github.com/agentstatelabs/agentstategraph/blob/main/LICENSE-MIT),
at your option.
