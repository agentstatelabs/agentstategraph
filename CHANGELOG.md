# Changelog

All notable changes to AgentStateGraph are documented here.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
Versioning: [Semantic Versioning](https://semver.org/spec/v2.0.0.html)

## [0.3.0-beta.1] — 2026-04-09

### Status
**Beta** — Specification complete, all features implemented and tested. Not yet published to crates.io / PyPI / npm. ThreadWeaver chat app uses it as the reference implementation. Awaiting community feedback.

### Specification
- Complete RFC at `spec/STATEGRAPH-RFC.md` (~2200 lines, 12 sections)
- Sections: Core Data Model, Intent Lifecycle, Authority/Delegation, Resolution Reporting, Sub-Agent Orchestration, Schema System, Epochs/Registry, MCP Interface, Architecture, Reference Implementation, Open Questions

### Implementation (137 tests passing)

#### Core (`agentstategraph-core`)
- Content-addressed objects (Atom, Node) with BLAKE3 hashing
- Commit type with full provenance: agent_id, authority, intent, reasoning, confidence, tool_calls
- Intent system: category (Explore/Refine/Fix/Rollback/Checkpoint/Merge/Migrate), description, tags, lifecycle
- Authority and delegation chains
- Resolution reporting with deviations and outcomes
- Notification policy
- Path addressing (JSON-path style)
- Structured diff engine (typed DiffOps, not text)
- Three-way merge engine with conflict detection
- Schema system with x-stategraph-merge hints (CRDT-inspired)
- Intent lifecycle state machine
- Composable query interface
- Blame operation (who changed what and why)
- Epochs (sealable, tamper-evident audit bundles)

#### Storage (`agentstategraph-storage`)
- ObjectStore, CommitStore, RefStore traits
- In-memory backend
- SQLite backend (durable, single file)
- IndexedDB backend (browser, via WASM)
- Pluggable design — add custom backends

#### High-Level API (`agentstategraph`)
- Repository handle ties core + storage
- Get/set/delete by JSON path
- Branch create/delete/list with namespacing
- Three-way merge (CAS-based concurrency)
- Speculative execution (O(1) branching, instant discard)
- Sub-agent sessions with parent-child hierarchy and path scoping
- Watch/subscribe system for reactive agents
- 9 reference implementation examples

#### MCP Server (`agentstategraph-mcp`)
- 20 MCP tools exposing the full API over stdio
- Tools: get, set, delete, branch, merge, diff, log, blame, query, speculate, compare, commit_speculation, discard_speculation, create_epoch, seal_epoch, list_epochs, sessions, etc.
- Connect from any MCP-compatible agent (Claude, GPT, etc.)
- CLI: `stategraph-mcp` binary

#### Language Bindings
- **Rust**: native crate (137 tests)
- **Python** (`agentstategraph_py`): PyO3 bindings via maturin
- **TypeScript/Node** (`agentstategraph`): napi-rs bindings
- **Go**: CGo bindings via stategraph-ffi
- **C ABI** (`agentstategraph-ffi`): cdylib + staticlib
- **WASM** (`agentstategraph-wasm`): wasm-bindgen for browser/Deno/Node

### Documentation
- Live site at agentstategraph.dev (Astro Starlight)
- 13 documentation pages: Introduction, Quick Start, Core Concepts, MCP Server guide, Python guide, TypeScript guide, Go guide, WASM/Browser guide, MCP Tools reference, RFC, blog
- Blog post: "The Missing Primitive for AI Agent Infrastructure"
- README, CONTRIBUTING.md, PUBLISHING.md
- Reference implementations in `examples/`

### CI/CD
- GitHub Actions: tests, clippy, fmt, WASM build, Go tests
- Site auto-deploys to GitHub Pages on push
- All checks green

### Known Limitations
- Not yet published to crates.io / PyPI / npm
- No conformance test suite for third-party implementations yet
- Schema merge engine doesn't yet apply CRDT hints automatically (annotations parsed but not enforced)
- No remote sync protocol (single-instance only)
- No commit signing yet
- Time-travel queries deferred

## [0.1.0] — 2026-04-04

### Added
- Initial RFC specification
- Core implementation in Rust
- Basic MCP server
- Initial bindings for Python and TypeScript

## Upcoming (0.4.0)

- Publish to crates.io, PyPI, npm
- Schema merge hint enforcement
- Conformance test suite
- Bisect operation completion
- intent_tree() traversal
- Watch/subscribe MCP integration
