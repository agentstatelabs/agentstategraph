# Contributing to AgentStateGraph

Thank you for your interest in AgentStateGraph! This project is building a new infrastructure primitive for the AI-native era, and contributions are welcome.

## Getting Started

1. **Fork and clone** the repository
2. **Install Rust**: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
3. **Run tests**: `cargo test`
4. **Run an example**: `cargo run --example getting_started -p agentstategraph`

## Project Structure

```
AgentStateGraph/
├── spec/AGENTSTATEGRAPH-RFC.md     # The specification (read this first!)
├── crates/
│   ├── agentstategraph-core/       # Types, diff, merge, schema — zero I/O
│   ├── agentstategraph-storage/    # Pluggable backends (memory, SQLite, IndexedDB)
│   ├── agentstategraph/            # High-level Repository API
│   ├── agentstategraph-mcp/        # MCP server (20 tools)
│   ├── agentstategraph-ffi/        # C ABI for language bindings
│   └── agentstategraph-wasm/       # Browser/Deno WASM build
├── bindings/
│   ├── python/                     # PyO3 bindings
│   ├── typescript/                 # napi-rs bindings
│   └── go/                         # CGo bindings
└── examples/                       # Reference implementations
```

## How to Contribute

### Good First Issues

Look for issues labeled `good-first-issue`. These are designed to be approachable for new contributors:

- **Add a new example**: Write a reference implementation for a specific use case
- **Improve error messages**: Make error types more descriptive
- **Add tests**: Increase coverage for edge cases in diff, merge, or query
- **Documentation**: Improve doc comments or add usage examples to doc tests

### Medium Issues

- **Schema merge hints in merge engine**: The schema system defines merge hints (`sum`, `max`, `union-by-id`, etc.) but the merge engine doesn't use them yet. Wire them together.
- **Bisect operation**: Implement binary search over the commit DAG to find where a condition changed (spec section 4.4.4).
- **Intent tree traversal**: Build the `intent_tree()` operation that returns the full decomposition tree of parent/child intents.

### Larger Contributions

- **New storage backend**: Implement `ObjectStore + CommitStore + RefStore` for a new backend (Redis, DynamoDB, etc.)
- **New language binding**: Add bindings for Ruby, Java, C#, or another language using the FFI crate
- **MCP resources**: Add MCP resource endpoints (`agentstategraph://state/{ref}/{path}`, etc.)

## Development Workflow

1. Create a branch: `git checkout -b feature/my-change`
2. Make changes
3. Run tests: `cargo test`
4. Run formatter: `cargo fmt`
5. Run clippy: `cargo clippy`
6. Commit with a clear message describing what and why
7. Open a pull request

## Code Style

- Follow standard Rust conventions
- Write doc comments for all public items
- Add tests for new functionality
- Keep agentstategraph-core free of I/O dependencies

## Architecture Principles

- **Intent metadata is mandatory**: Every state change must have intent. Don't add write operations that skip intent.
- **Provenance is permanent**: Don't add operations that destroy history without explicit epoch/archive semantics.
- **Storage is pluggable**: Don't hard-code storage backends. All storage goes through traits.
- **Schema is optional**: AgentStateGraph must work without schemas. Schema features are additive.

## Naming

When writing prose or new docs, always use the full **AgentStateGraph**, never the short form "StateGraph" alone. The short form collides with both LangGraph's `StateGraph` class and Terrateam's Stategraph Terraform backend, and those collisions are actively harmful for our target audience. See `site/src/content/docs/compare.md` for the disambiguation page. Do not adopt "ASG" as an abbreviation — it collides with AWS Auto Scaling Groups.

## Questions?

Open an issue or start a discussion. We're building the infrastructure primitive for intent-based systems — your perspective matters.
