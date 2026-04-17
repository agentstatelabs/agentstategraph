# Changelog

All notable changes to AgentStateGraph are documented here.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
Versioning: [Semantic Versioning](https://semver.org/spec/v2.0.0.html)

## [0.5.0-beta.1] — 2026-04-17

### Changed
- Minor version bump from `0.4.0-beta.3` to `0.5.0-beta.1`. The migration framework, `/_meta/*` guard, and upgrade CLI landed in the 0.4 series but are significant enough to reflect as a minor bump in the release line. No API changes from `0.4.0-beta.3` — same code, larger version number.
- All bindings (Python, TypeScript) aligned to `0.5.0-beta.1`.

## [0.4.0-beta.3] — 2026-04-17

### Changed
- Aligned Python and TypeScript bindings (both Cargo and package files) to the workspace version `0.4.0-beta.3`. Bindings had been stranded at `0.3.5-beta.2` through the `0.4.0-beta.1` and `0.4.0-beta.2` workspace bumps.

## [0.4.0-beta.2] — 2026-04-17

### Added
- **New crate `agentstategraph-migrate`** — schema-evolution framework for ASG databases. Provides `Migration` trait, `Registry`, `check()` for startup introspection (returns `UpToDate` / `UpgradeAvailable` / `Downgrade` / `Unversioned` / `Corrupt`), and a `Runner` with `DryRun` and `Apply` modes. First shipped migration (`plan_assignments_sidecar_to_native`) walks legacy CTXone `/plan_assignments` entries onto native `Task.assigned_to` and bumps `/_meta/schema_version` atomically. Exit-code constants follow `sysexits.h` spirit (64 / 65 / 70 / 75) for ops tooling.
- **`/_meta/*` reserved path guard** on `Repository::{set, set_json, spec_set, spec_set_json, delete}` and `commit_speculation`. Writes under `/_meta/*` now require `IntentCategory::Migrate` — a new `RepoError::ReservedPath` is returned otherwise. Protects schema metadata from accidental overwrites.
- **`Repository::init()` stamps `/_meta/schema_version`** in its initial commit using a decoupled `SCHEMA_VERSION` constant. The schema version advances only when a migration runs, independent of the crate's release version.
- **`agentstategraph-mcp migrate` subcommand** — one-shot maintenance CLI. Flags: `--db`, `--storage`, `--ref`, `--to`, `--dry-run`, `--yes`, `-h`. Refuses to start the MCP/HTTP surface. Prints per-step status, commit IDs, and final version.
- **Upgrade-path design doc** at `spec/UPGRADE-PATH.md` covering versioning model, migration registry, consumer-side upgrade flow, the sidecar migration as worked example, CLI, and downgrade/rollback semantics.

### Changed
- Workspace version bumped from `0.4.0-beta.1` to `0.4.0-beta.2`.
- `SCHEMA_VERSION` constant in `agentstategraph` is now a literal `"0.4.0"`, decoupled from `env!("CARGO_PKG_VERSION")`. Bump only when a migration advances the on-disk shape.

## [0.4.0-beta.1] — 2026-04-15

### Added
- **New crate `agentstategraph-tasks`** — shared task-store primitives for plan-rot prevention. Provides `Plan`, `Task`, `TaskStore`, `Proof`, `Verifier` trait, and a full state machine (`pending → in_progress → done`, with proof and blocker enforcement). Multiple ASG consumers (CTXone, ThreadWeaver, future apps) share a single implementation instead of reimplementing task types independently. Establishes the pattern for opinionated-but-shared sibling crates in the workspace.
- **`IntentCategory::Plan`** variant added to `agentstategraph-core` — plan/task operations are natively filterable in log and blame queries. Recognized in MCP, HTTP, FFI, and WASM parsers. **Consumer caveat**: the new native variant serializes as `"Plan"`; pre-existing data written as `{"Custom":"Plan"}` still deserializes as the `Custom` variant, so a filter on `IntentCategory::Plan` will NOT match legacy `Custom("Plan")` commits. Normalise at read time if you need unified filtering across the upgrade boundary.
- **`Repository::spec_set_json`** convenience method on the high-level API — mirrors `set_json` for the speculation path, used by `agentstategraph-tasks` for atomic multi-path commits.
- **`Task::assigned_to`** — optional agent-assignment field on `Task`, eliminating the need for consumer-side sidecar storage. New `TaskStore::assign_task`, `unassign_task`, and `next_task_for` methods support assignment-aware task selection. `list_plans_by_status` adds native status filtering.

### Changed
- Workspace version bumped from `0.3.5-beta.2` to `0.4.0-beta.1` (new public crate).

## [0.3.5-beta.2] — 2026-04-09

### Changed
- **Naming hygiene pass.** Every standalone "StateGraph" reference replaced with the full "AgentStateGraph" across prose, identifiers, symbols, and packages. Eliminates collision surface with LangGraph's `StateGraph` class (the primary primitive LangChain developers use) and Terrateam's Stategraph Terraform backend.
  - Rust: `StateGraphServer` → `AgentStateGraphServer`; MCP tool method names `stategraph_*` → `agentstategraph_*` (all 20 tools); `WasmStateGraph` → `WasmAgentStateGraph`
  - C FFI: all 12 extern symbols renamed (`agentstategraph_new_memory`, `agentstategraph_get`, etc.)
  - Python: class `StateGraph` → `AgentStateGraph`
  - TypeScript: class `StateGraph` → `AgentStateGraph`; npm package `stategraph` → `agentstategraph`
  - Go: package `stategraph` → `agentstategraph`; struct and files renamed; module path `github.com/agentstatelabs/AgentStateGraph/bindings/go`
  - JSON Schema extensions: `x-stategraph-*` → `x-agentstategraph-*`
  - URI scheme: `stategraph://` → `agentstategraph://` (spec)
  - MCP server key in config examples: `"stategraph"` → `"agentstategraph"`
  - Default SQLite path: `./stategraph.db` → `./agentstategraph.db`
  - Default WASM IndexedDB name: `"stategraph"` → `"agentstategraph"`
  - Spec file: `spec/STATEGRAPH-RFC.md` → `spec/AGENTSTATEGRAPH-RFC.md`
  - Repository URL in `Cargo.toml` points to `github.com/agentstatelabs/AgentStateGraph`

### Added
- **Sharpened positioning.** README and landing page now lead with the one-sentence Git-analogy framing: *"AgentStateGraph is to agent state what Git was to source code — a content-addressed, branchable, blameable state primitive, designed from the ground up for AI agents as the primary actor."* Followed by an explicit "what it is not" paragraph: not a Terraform replacement (wrong actor model), not a LangGraph helper (different layer of the stack), but a state primitive on which next-generation IaC, GitOps, and agent-native ops tooling can all be built.
- **Disambiguation page** at `site/src/content/docs/compare.md` — "AgentStateGraph vs. Stategraph vs. LangGraph's StateGraph." Three-column comparison table covering actor model, data model, branching, intent/reasoning, blame, audit surface, language bindings, primary interface, storage backends, and closest analogy. Linked from site nav (Getting Started) and the README.
- **Landing page rework:** hero tagline carries the verbatim Git-analogy framing; new "The vision" and "What it is not" sections above the cards; compare link in the hero actions.
- **CONTRIBUTING.md Naming section** stating the hygiene rule for future contributors, including the no-ASG convention (collides with AWS Auto Scaling Groups).

### Fixed
- **Previously leaked short forms** in: MCP server key shown in README config example (visible on-screen as `mcp__stategraph__…` during recorded demos), Rust struct names, crate descriptions, doc comments across all six crates, spec file, Python/TypeScript/Go/Rust/WASM examples, browser demo, blog post, and all site guides.

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
- Schema system with x-agentstategraph-merge hints (CRDT-inspired)
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
- CLI: `agentstategraph-mcp` binary

#### Language Bindings
- **Rust**: native crate (137 tests)
- **Python** (`agentstategraph_py`): PyO3 bindings via maturin
- **TypeScript/Node** (`agentstategraph`): napi-rs bindings
- **Go**: CGo bindings via agentstategraph-ffi
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
