# Changelog

All notable changes to AgentStateGraph are documented here.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
Versioning: [Semantic Versioning](https://semver.org/spec/v2.0.0.html)

## [Unreleased]

## [v0.9.21] — 2026-08-05

### Added

- Swift and C binding support for namespaces, expected-head CAS writes,
  commit queries, merge base and preview, safe merges, repository exploration,
  atomic speculation, durable sessions, and epochs.
- A versioned advanced repository ABI with runtime capability discovery.
- A cross-language capability manifest and GitLab/GitHub release gates that
  require every binding to be reviewed for each Core release.

### Fixed

- Swift intent categories `Correction`, `Refinement`, and `Exploration` now
  map to their native Core variants instead of custom categories.

## [v0.9.20] — 2026-08-05

## [v0.9.19] — 2026-08-05

## [v0.9.18] — 2026-08-05

## [v0.9.17] — 2026-08-05

### Changed
- **SwiftPM releases are now checksum-correct and consumable from the repository root.** GitLab dispatches a pre-tag GitHub macOS build for the exact mirrored source SHA, stages the immutable XCFramework in GitLab, generates the root binary-target manifest, publishes those exact bytes to GitHub Releases, and verifies a clean remote consumer. Apple builds now pin macOS 11 and iOS 14 deployment targets instead of inheriting runner defaults.

## [v0.9.16] — 2026-08-04

### Added
- **Swift binding for macOS and iOS.** A complete Swift Package
  (`bindings/swift`) over the `agentstategraph-ffi` C ABI, at parity with the
  Go/Python/TypeScript bindings: repository (`get`/`set`/`delete`, branches,
  `diff`/`merge`, `log`, `blame`), `TaskStore`, `PolicyStore`, taint /
  quarantine / watch, and migrate — with `Codable` models and a throwing API.
  It builds two ways (selected by `AGENTSTATEGRAPH_SWIFT_LOCAL`): the default
  XCFramework mode links a device-capable fat framework built by
  `scripts/build-swift-xcframework.sh` (macOS arm64+x86_64, iOS device arm64,
  iOS simulator arm64+x86_64), and a local-dylib mode links `target/release`
  for fast macOS/simulator development. A `swift-xcframework` release job
  publishes the XCFramework as a GitHub release asset, and a `swift` CI job
  runs the XCTest suite on macOS.

## [v0.9.15] — 2026-08-03

## [v0.9.14] — 2026-07-29

### Fixed
- **`publish-npm` now installs Node 20.** `@napi-rs/cli@3` requires Node >= 20,
  but the CI image's apt ships Node 18, so the addon build crashed on a missing
  `node:util` export. The job now installs Node from the official tarball.

## [v0.9.13] — 2026-07-29

### Added
- **npm publishing for the TypeScript (napi-rs) binding.** A `publish-npm` CI
  job builds the native addon and publishes the Node package to the GitLab npm
  registry on release tags (Linux x64, matching the wheel's single-platform
  scope). A cross-platform matrix remains a follow-up.

## [v0.9.12] — 2026-07-29

### Fixed
- **Release artifacts no longer drift from the tag.** The Python wheel was
  hardcoded to `0.9.8` in `pyproject.toml`, so every release rebuilt a `0.9.8`
  wheel and the upload failed on the GitLab PyPI registry with a duplicate
  `400 Bad Request`. The wheel version is now derived from the Cargo workspace
  version (`dynamic`), the TypeScript `package.json` is kept in sync, internal
  crate deps inherit from `[workspace.dependencies]` (no more stale `0.9.6`
  pins), and a `version-guard` CI job fails any tag whose versions disagree
  before it can publish. `scripts/release.sh` bumps everything in one command.

## [v0.9.11] — 2026-07-29

### Fixed
- **`TaskStore::archive_plan` no longer loses concurrent archivals.** Archive
  used a plain `set_json` (an unconditional ref advance), so two archives racing
  on the same branch both branched off the pre-archive head and the second
  silently overwrote the first — the "archived" plan reappeared as active. It
  now uses the same CAS retry loop as `add_task` (`set_json_cas` on a snapshotted
  head, retrying on `WriteConflict`, surfacing `TaskStoreError::WriteConflict`
  once `MAX_CAS_RETRIES` is exhausted). Archive remains valid for any status
  (active, completed, empty) and never touches tasks, proofs, or abandonment
  reasons. New `archive_plan` test suite covers active/empty archival and the
  concurrent no-lost-update guarantee.

### Changed
- **Relicensed to `MIT OR Apache-2.0`** (from BUSL-1.1), returning the project
  to a fully permissive dual-license in preparation for publishing to
  crates.io. The grant applies to all prior released versions as well, so
  every published version is available under MIT OR Apache-2.0. See
  [`LICENSING.md`](LICENSING.md).

## [v0.9.6] — 2026-07-23

### Added
- `Repository::merge_base(source, target)` exposes the lowest-common-ancestor commit of two refs, so callers can reason about what each side changed relative to the branch point — required for correct domain-level merge policies (e.g. distinguishing a genuine `done→pending` regression from a source branch that simply predates the target's completion).

## [v0.9.5] — 2026-07-23

Theme: **merge data-loss fix and ref-spec resolution.**

### Fixed
- **Merge no longer erases nested maps.** When a merge combined a nested map that both branches had touched (e.g. `/plans`), the engine fabricated a new intermediate node but never persisted it — only its id was embedded in the parent — so the committed tree dangled with `ObjectNotFound` on readback, presenting as an erased subtree. `three_way_merge` now has a collecting variant (`three_way_merge_collect`) that returns every fabricated composite, and `Repository::merge` persists them all before advancing the ref. The dead `store_object_tree` helper is removed.
- **Correct merge base.** `find_common_ancestor` walked only the first parent and fell back to the root commit, so once merge commits existed it could pick too old a base and treat the target's own keys as deletions. It now computes a true DAG-aware lowest common ancestor and errors on genuinely disjoint histories instead of guessing.

### Added
- **Ref-spec resolution.** `Repository::resolve_ref` (and therefore `branch --from`, `merge`, `diff`, `read`, …) now accepts a branch name, a full commit hash (with or without the `sg_` prefix), or a unique `sg_`/hex commit-id prefix, including orphaned commits from deleted branches. New `RepoError::CommitNotFound` and `RepoError::AmbiguousCommitPrefix` distinguish the failure modes; `ObjectId::from_hex` / `to_hex` / `normalize_hex_prefix` and `CommitStore::all_commit_ids` support it.
- **Merge deletion guard and dry-run.** `Repository::merge_checked(source, target, opts, allow_deletions)` refuses to advance the target ref when the merge would remove a top-level entry unless `allow_deletions` is set (`RepoError::MergeWouldDelete`). `Repository::preview_merge` returns a `MergePreview` (added / changed / removed top-level keys and conflicts) without mutating anything.

## [v0.9.2] — 2026-07-13

Theme: **first public release.** Repository, license, and documentation prepared for open publication. No functional API changes from 0.9.1.

### Changed
- Canonical repository is now `github.com/agentstatelabs/agentstategraph`; all package metadata, install scripts, and docs point there.
- License finalized as **BUSL-1.1** (valid SPDX identifier) with a concrete Change Date of **2030-07-13**, converting to Apache 2.0.
- Rust toolchain pinned to 1.96 across the Dockerfile and CI.

### Fixed
- Documentation counts reconciled to the shipped implementation across README, website, and RFC: 73 MCP tools, 15 crates, 7 language bindings.

### Removed
- Internal planning/roadmap documents and the draft security threat model removed from the public tree.

## [v0.9.1] — 2026-05-12

Theme: **per-call namespace override** on all ref-touching operations.

### Added

- **`Repository::fork_namespace(&self, ns: Namespace) -> Repository`** —
  creates a lightweight sibling repository sharing the same `Arc` storage
  but operating in a different namespace. Inherits epoch/session context
  and `epoch_seal_strict`; starts with fresh in-memory speculation and
  watch state. Enables per-request namespace isolation without holding a
  separate storage connection per namespace.
- **`Repository` storage changed to `Arc<dyn Storage + Send + Sync>`** —
  enables shared-storage forks without data duplication.
- **`namespace: Option<String>` on 17 MCP tools** — `get`, `set`,
  `delete`, `branch`, `list_branches`, `merge`, `log`, `diff`,
  `speculate`, `query`, `blame`, `list_paths`, `get_tree`, `search`,
  `stats`, `commit_graph`, `intent_tree` all accept an optional
  `namespace` field. When provided, the tool operates against that
  namespace; when omitted, the server's configured namespace is used.
- **`namespace: Option<String>` on 9 WASM methods** — `get`, `set`,
  `delete`, `branch`, `merge`, `diff`, `log`, `blame`, `speculate`.
  Same semantics as the MCP param: pass `null` / `undefined` to use the
  repository's configured namespace.

## [v0.9.0] — 2026-05-12

Theme: **namespace primitive** — ref-layer isolation boundary for
multi-project and multi-tenant deployments.

### Added

- **`Namespace` newtype** (`agentstategraph-core`) — validated
  alphanumeric + `-_` identifier (max 64 chars) with a `"default"`
  constant. Enforced at the ref-store layer via a composite
  `(namespace, name)` primary key on the refs table across all
  four backends (SQLite, Postgres, in-memory, IndexedDB).
- **`Repository::with_namespace(ns) -> Self`** — configures a
  repository-level default namespace at construction time. The
  `--namespace` / `ASG_NAMESPACE` flag on `agentstategraph-mcp`
  threads this through to the server.
- **`Repository::create_namespace` / `list_namespaces` /
  `delete_namespace`** — full CRUD for namespace lifecycle.
  `delete_namespace` cascades to all refs in that namespace and
  protects `"default"` from deletion.
- **`Repository::cross_namespace_merge`** — merges a branch from
  a source namespace into a target branch in the current namespace.
  Same-namespace merges are plain merges; cross-namespace merges
  are denied by default (no `PolicyStore` attached) with a new
  `RepoError::CrossNamespaceAccessDenied` variant.
- **`Session::scope_namespace: Option<Namespace>`** — upgraded
  from `Option<String>` to the typed `Namespace` newtype. Wire
  format is unchanged; SQLite and Postgres `row_to_session` do the
  conversion at read time. `CreateSessionParams::scope_namespace`
  allows callers to set it at session-creation time.
- **`active_session_namespace` cache on `Repository`** — the
  session's namespace is eagerly cached in a `RwLock` when
  `set_active_session()` is called, eliminating a `get_session()`
  storage round-trip on every ref operation.
- **MCP tools** — `agentstategraph_create_namespace`,
  `agentstategraph_list_namespaces`, `agentstategraph_delete_namespace`,
  `agentstategraph_cross_namespace_merge`; `create_session` gains
  an optional `namespace_id` field.
- **`StorageError::InvalidOperation(String)`** — new error variant
  for operations rejected by the storage layer (e.g. deleting the
  default namespace).
- **13 integration tests** in `crates/agentstategraph/tests/namespace.rs`
  covering namespace isolation, ref/branch visibility, CRUD, session
  scope override, cross-namespace merge, and startup auto-creation.

### Changed

- **`Repository::init()` auto-creates its configured namespace** —
  fixes startup failures when `--namespace` points to a namespace
  that doesn't yet exist in the database.
- **Auth `ApiKey` gains `namespace_id: Option<String>`** — keys
  can be scoped to a specific namespace; the auth middleware
  propagates this into `AuthContext`.

### Verified locally

- `cargo test --workspace` — all tests pass (13 new namespace
  integration tests included).
- Full build clean; the pre-existing PyO3 dyld failure on the
  Python bindings is unchanged (unrelated to this release).

## [v0.7.75-beta.3] — 2026-04-24

Theme: **remaining stub closures** flagged in the 0.7.75-beta.2
audit — FFI Postgres constructor + WASM sign/verify parity.

### Added

- **FFI `agentstategraph_new_postgres(url, tenant_id)`** —
  feature-gated constructor behind `--features postgres` on the
  FFI crate. Opens a multi-tenant Postgres-backed repository
  through the C ABI; required for commercial Postgres-first FFI
  consumers. Runtime is leaked for the process
  lifetime (matches `tokio::main` semantics); shutdown is by
  process exit. Symbol absent from the default `sqlite`-only
  build — consumers targeting both modes should `dlsym`-probe.
- **WASM `WasmPolicyStore::sign` / `verify`** — full Ed25519
  round-trip mirroring the TypeScript wiring from 0.7.5-beta.2.
  Caller supplies a 32-byte hex seed for `sign` and a 32-byte hex
  public key for `verify`. Returns `{algorithm, signer_key_id,
  signature_hex}` on sign; `{valid, algorithm, signer_key_id}` on
  verify success; `{valid: false, reason}` on mismatch or
  `"unsigned"` when the policy carries no signature.
- **WASM taint tests extended** — 3 new wasm_bindgen_test
  cases: sign/verify round-trip with deterministic seed,
  unsigned-policy verify returns `reason: "unsigned"`,
  `set_external_evaluator` still returns the documented stub
  envelope (by plan §4c).

### Changed

- **`set_external_evaluator` on every binding is now documented
  as "by design, not a gap"** — the five bindings' stubs match
  plan §4c: the FFI dispatcher is intentionally thin; register
  runners via the MCP server builders instead.

### Verified locally

- `cargo build --release -p agentstategraph-ffi --features postgres`
  produces a `.a`/`.dylib` with the new extern exported.
- `wasm-pack test --node crates/agentstategraph-wasm` — 22 policy
  tests (was 20 in -beta.2), all pass.
- All other 0.7.75-beta.2 suites unchanged and green.

## [v0.7.75-beta.2] — 2026-04-24

Theme: **real Postgres `TaintStore`** — unblocks any Postgres-first
consumer from the 0.7.75-beta.1 taint substrate.

### Added

- **`PostgresStorage` taint schema** — multi-tenant `taints`
  table mirroring the SQLite shape, composite PK
  `(tenant_id, id)`, partial unique index
  `(tenant_id, path, name, kind) WHERE resolved_at IS NULL`.
  Per-tenant indexes on `(tenant_id, path)` and
  `(tenant_id, kind)`.
- **6 real `TaintStore` impls on `PostgresStorage`** replacing
  the 0.7.75-beta.1 stubs. SQL translations of the SQLite
  versions, every WHERE scoped by `tenant_id = $1`. Ancestor
  propagation uses `$N LIKE path || '/%'` — same semantics as
  SQLite, same path-boundary safety.
- **New integration test** `tests/postgres_taint.rs` — 2 tests
  exercised against a live Postgres:
  1. Full 12-assertion conformance (CRUD, duplicate rejection,
     ancestor propagation, path-boundary safety, list filters,
     resolve + double-resolve rejection, include_resolved
     history, expired-taint filtering).
  2. Cross-tenant isolation — tenant A's taints are invisible to
     tenant B on `get_taint`, `list_taints`, `check_taint`; each
     tenant can independently create the same
     `(path, name, kind)` triple (partial unique index is
     tenant-scoped).
  Skipped unless `TEST_DATABASE_URL` is set (matches existing
  `postgres_tenant_isolation.rs` convention).

### Fixed

- **Pre-existing `rt-multi-thread` build bug on `PostgresStorage`.**
  The crate's `tokio` feature set was `["rt"]` only, but
  `block_in_place` requires `rt-multi-thread`. Fixed Cargo.toml
  to `["rt", "rt-multi-thread"]`. The taint-stub impls didn't
  call `block_on`, so the bug was latent; real SQL surfaced it.

### Verified locally

- Homebrew `postgresql@16` + `asg_test` database.
- Both new Postgres tests pass with `--test-threads=1` (the
  pre-existing DDL race on parallel init_tables is documented in
  `spec/POST-PRODUCTION-NOTES.md`).
- All 0.7.75-beta.1 test suites continue to pass unchanged.

## [v0.7.75-beta.1] — 2026-04-21

Theme: **taint / quarantine / watch** — dynamic runtime markers
that bridge passive observation into enforcement. Every taint is
a first-class commit (auditable, blameable, with full intent
metadata) and the pre-commit hook consults the aggregated
decision on every `set` / `set_json` / `delete` / `merge`.

### Added

- **New sibling crate `agentstategraph-taint`** — core types
  (`Taint`, `TaintKind`, `TaintEffect`, `TaintSeverity`,
  `TaintMetadata`, `TaintCheck`), parameter bundles
  (`TaintParams`, `QuarantineParams`, `WatchParams`,
  `UntaintParams`, `UnwatchParams`), `TaintError` enum, and the
  pure `evaluate_access(path, agent_id, confidence, candidates,
  now)` precedence algorithm (Quarantine > Block > Review >
  Isolate > Warn > Advisory).
- **Six new `IntentCategory` variants**: `Taint`, `Untaint`,
  `Quarantine`, `Unquarantine`, `Watch`, `Unwatch` — additive,
  preserving round-trip distinctness from `Custom("Taint")` etc.
- **`TaintStore` trait** on `agentstategraph-storage` with 6
  methods; `SqliteStorage` + `MemoryStorage` implement in full;
  `PostgresStorage` returns `Backend` errors (post-production
  wiring); `IndexedDbStorage` delegates to an inner
  `MemoryStorage` for in-session taints. SQLite gains a `taints`
  table with a partial unique index
  (`WHERE resolved_at IS NULL`) so historical rows coexist with
  fresh active ones on the same `(path, name, kind)` triple.
- **`Repository` taint surface** — `taint`, `untaint`,
  `quarantine`, `unquarantine`, `watch_path`, `unwatch`,
  `list_taints`, `check_taint`. Each persists the row AND writes
  an intent commit with the matching `IntentCategory`; the
  commit id is patched back onto the storage row.
- **Pre-commit taint hook** wired into `set` / `set_json` /
  `delete`. `CommitOptions.confidence` drives the review-effect
  gate (threshold 0.9, inclusive; default 1.0 keeps pre-0.7.75
  call sites silent). Taint-lifecycle commits bypass the hook to
  avoid self-deadlock.
- **Watch auto-escalation** — a watch with a numeric
  `threshold` + `direction` (above/below) auto-creates a
  Warn-effect taint when `set_json` crosses the threshold. The
  auto-taint's metadata cites `source_watch_id`, `metric`,
  `threshold`, `observed` for blame. Idempotent: repeated
  crossings do not re-fire.
- **8 new MCP tools** (count 52 → 60): `agentstategraph_taint`,
  `_untaint`, `_quarantine`, `_unquarantine`, `_watch`,
  `_unwatch`, `_list_taints`, `_check_taint`.
- **Policy × taint composition tool**
  `agentstategraph_policy_evaluate_change_with_taints` (60 → 61)
  returns `{decision, taint_status, can_proceed}` — `can_proceed`
  is the conjunction of `decision.kind != deny` and every
  affected path's `check_taint.can_write`.
- **8 new FFI externs** (count 56 → 64):
  `agentstategraph_taint_apply` / `_remove`,
  `_quarantine_apply` / `_release`, `_watch_apply` / `_remove`,
  `_list_taints`, `_check_taint`. All accept JSON params
  envelopes so the C ABI stays narrow as the substrate evolves.
- **All 5 bindings** (Py / TS / Go / WASM / C#) expose the full
  8-method taint surface mirroring the Rust Repository signature.
  Python binding renames pre-existing stub `ag.watch` →
  `ag.subscribe_watch` (breaking for any consumer of the stub,
  which unconditionally returned 0).
- **`docs/TAINT_GUIDE.md`** — end-user guide covering effects,
  propagation semantics, auto-escalation, policy composition,
  storage schema.
- **`crates/agentstategraph-taint/README.md`** — crate primer.
- **`spec/0.7.75-PLAN.md`** — implementation plan.
- **Parity fixture extension** — new `taint_cases` +
  `quarantine_case` blocks exercised by a Rust reference runner
  in `agentstategraph/tests/taint_parity.rs`.

### Verified locally

| Surface | Count | Status |
|---|---|---|
| Rust workspace | all | pass (clippy -D warnings) |
| `agentstategraph-taint` unit tests | 18 | pass |
| Storage conformance (Memory + SQLite) | 26 | pass |
| Repository integration | 12 | pass |
| Watch auto-escalation | 5 | pass |
| MCP taint tools | 5 | pass |
| MCP policy×taint | 3 | pass |
| FFI smoke | 6 | pass |
| Parity reference | 2 | pass |
| Python pytest | 44 | 43 pass, 1 skipped (pre-existing) |
| TypeScript node:test | 38 | pass |
| Go go test | 39 | pass |
| WASM wasm-pack --node | 28 | pass |
| .NET | — | experimental; committed on trust |

Counts: MCP tools 52 → 61. FFI externs 56 → 64. IntentCategory
variants +6.

## [v0.7.5-beta.2] — 2026-04-21

Theme: **follow-up polish on 0.7.5-beta.1.** Small patch release
that closes the post-ship caveats — no new primitives, but the
advanced-policy surface is now fully exercisable across every
toolchain we have locally.

### Added

- **Cedar runner promoted from stub to real.** `agentstategraph-policy-cedar`
  now shells out to `cedar authorize` with tempfile-backed policies
  / entities / request JSON. Decision mapping `Allow` → Allow,
  `Deny` → Deny, else NoPolicyMatch. Skip-when-missing-binary test
  pattern mirrors the Rego runner.
- **Scoped FFI externs.** `agentstategraph_policy_evaluate_scoped`
  and `_evaluate_change_scoped` added so non-Rust consumers can
  filter candidates *before* evaluation (FFI count 54 → 56). Fixes
  a client-side post-hoc filter bug where Go couldn't redirect to
  a global fallback when a tenant-scoped policy was first-match.
- **Python `PolicyStore.sign()`** now calls real `set_signature`
  (was a stub envelope in -beta.1). Accepts a pre-computed
  Ed25519 signature hex and commits it under
  `IntentCategory::Custom("policy-sign")`.
- **TypeScript `PolicyStore.sign()` / `verify()`** now do full
  end-to-end Ed25519 via the `agentstategraph-policy-sign` crate
  (added as a napi dep). No stubs remain on the TS signing surface.
- **`crates/agentstategraph-policy/README.md`** — long-promised
  crate reference covering core types, workflow, 0.7.5 advanced
  features, API quick-reference, storage layout, cross-crate map.
- **`spec/POST-PRODUCTION-NOTES.md`** — explicit register of items
  deferred past 0.7.5: key rotation + CRL, commit/speculation
  signing, FFI dispatcher first-class wiring, Cedar entity-graph
  enrichment, runner error telemetry, and the .NET test-execution
  gap.

### Fixed

- **TypeScript `index.js`** pre-existing bug (since the binding
  was added) — only re-exported `AgentStateGraph`, so
  `PolicyStore` / `TaskStore` / `exitCodes` tests couldn't even
  import. Now re-exports all `#[napi]`-annotated symbols.
- **Python `test_propose_creates_unratified_policy`** asserted
  `fetched["ratified_by"] is None` against a dict that omits the
  key via `skip_serializing_if`. Fixed to `fetched.get(...)`.
- **Go `EvaluateScoped` global-fallback bug** — the 0.7.5-beta.1
  client-side post-hoc filter couldn't redirect when the
  first-match policy failed the filter. Fixed by routing through
  the new scoped FFI externs.
- **WASM §5d tests** used hallucinated PolicySignature /
  ExternalEvaluatorRef shapes. Corrected to real tagged-union
  shapes and dropped `run_in_browser` so `wasm-pack test --node`
  runs the suite.
- **Parity fixture §6** — extended with `tenant_evaluate` +
  `external_evaluate` + `extra_policies` + `ratify_extra` blocks,
  wired into all 7 runners (Rust reference + FFI + WASM + Py + TS
  + Go + .NET).

### Verified locally

| Surface | Count | Status |
|---|---|---|
| Rust workspace | all | pass |
| Python pytest | 38 | 37 pass, 1 documented skip |
| TypeScript node:test | 31 | pass |
| Go go test | 33 | pass |
| WASM (wasm-pack --node) | 21 | pass |
| Cedar real subprocess | 8 | pass |
| .NET | — | untested; experimental per 0.7.25 decision |

FFI extern count 54 → 56. No MCP tool count change.

## [v0.7.5-beta.1] — 2026-04-18

Theme: **advanced policy — signing + multi-tenant + external
evaluators.** Biggest single milestone so far: ~14 section commits
covering three features that each stand alone but compose into the
"AI agent that knows when to act, when to ask, and what to do while
it waits" substrate (POLICY_V1.md §22.1). Per ROADMAP D2 / D3 / D4:
Ed25519 signing, tenant-id namespace discrimination, external-
evaluator dispatch to Rego / Cedar / WASM.

### Added

- **Policy.expires_at enforcement** (§1) — deferred cleanup from
  0.7.0-beta.1. `Policy::is_currently_active(now)` now enforces the
  full activation window (ratified + active_from + expires_at).
- **Policy signing** (§2) — new sibling crate
  `agentstategraph-policy-sign` with canonical-JSON + Ed25519 signer
  / verifier / pluggable `KeyRegistry`. `Policy.signature:
  Option<PolicySignature>` round-trips on every policy without forcing
  the crypto dep. `PolicyStore::with_verifier(...)`,
  `with_require_signed(true)`. MCP tools `policy_sign` / `policy_verify`
  (50 → 52). FFI externs `agentstategraph_policy_sign` /
  `_verify` (51 → 53).
- **Multi-tenant** (§3) — `Policy.tenant_id: Option<String>` +
  `Session.scope_tenant: Option<String>`. New `_scoped` evaluator
  methods (`active_scoped`, `evaluate_scoped`, `evaluate_change_scoped`,
  `list_scoped`, `policies_for_situation_scoped`) taking
  `tenant_filter: Option<&str>` with global-fallback semantics
  (`None` tenant_id = applies to all tenants). Zero-arg variants
  remain as back-compat wrappers.
- **External evaluators** (§4) — `Policy.external_evaluator:
  Option<ExternalEvaluatorRef>` (`Rego` / `Cedar` / `Wasm` variants,
  each carrying an `EvaluatorSource` — `Inline` / `FilePath` /
  `CommitRef`). `ExternalEvaluator` trait + `ExternalEvaluatorRegistry`.
  Three new opt-in runner crates: `agentstategraph-policy-wasm`
  (wasmtime host, documented ABI), `-rego` (subprocess `opa eval`),
  `-cedar` (stub). Server builders `with_external_evaluator(...)`,
  `with_wasm_evaluator()`, `with_rego_evaluator()`,
  `with_cedar_evaluator()` (feature-gated). FFI extern
  `agentstategraph_policy_set_external_evaluator` (stub, 53 → 54).
- **Bindings** (§5a–§5e) — Python, TypeScript, Go, WASM, and C#
  all gain the three new Policy fields, `Session.scope_tenant`,
  `tenantFilter` / `tenant_filter` params on scoped evaluator
  methods (client-side filter in Go / C#; Rust-side `_scoped`
  routing in Py / TS / WASM), and `sign` / `verify` /
  `setExternalEvaluator` stub methods returning documented error
  envelopes until the MCP/FFI surface is threaded through.
- **Parity fixture** (§6) — `spec/policy_parity_fixture.json`
  extended with `extra_policies`, `ratify_extra`, `tenant_evaluate`,
  and `external_evaluate` sections. Rust reference runner
  exercises all three new blocks via `evaluate_scoped`. Older
  binding runners ignore the new keys (back-compat).
- **Docs** (§7) — new `docs/POLICY-EVALUATOR-ABI.md` documents the
  WASM evaluator ABI (asg_alloc / asg_free / asg_evaluate exports,
  request / response JSON shapes, memory model, error convention).
  `docs/POLICY_GUIDE.md` gains a "0.7.5 — Advanced policy" section.

### Changed

- Workspace version `0.7.25-beta.2` → `0.7.5-beta.1`. Bindings
  (Py / TS / Go / WASM / C#) track the workspace version.
- MCP tool count 50 → 52 (+ `policy_sign`, `policy_verify`).
- FFI extern count 51 → 54 (+ 3 signing / external-evaluator
  externs).

### Rationale

Per ROADMAP defaults accepted by the principal:
- **D2** — Ed25519 as the only signature algorithm shipped.
- **D3** — `tenant_id` as a cheap string-keyed namespace
  discriminator rather than a full multi-tenant storage split.
- **D4** — External evaluators as an escape-hatch dispatcher
  rather than re-implementing Rego / Cedar inside ASG.

Key rotation + CRL semantics, authoring UX for Rego / Cedar, and
first-class FFI dispatcher wiring are scheduled for pre-GA.

## [v0.7.25-beta.2] — 2026-04-21

Theme: **close the three FFI gaps + fix Decision polymorphism.**
Small patch release on top of 0.7.25-beta.1 that promotes the
C# binding from experimental to first-class.

### Added

- **3 new extern C functions** in `agentstategraph-ffi`
  (DllImport count 48 → 51):
  - `agentstategraph_list_branches(repo, prefix)` — JSON array
    of `{name, target}`; prefix `NULL` for no filter
  - `agentstategraph_delete_branch(repo, name)` —
    `{"deleted": true|false}` or `{"error": "..."}`
  - `agentstategraph_taskstore_add_task_ex(...)` — extended
    `add_task` threading the 0.6.0 Task extension fields
    (`payload`, `parent_change`, `on_complete`). Routes to the
    Rust `TaskStore::add_task_with_extensions` method that's
    existed since 0.6.0 but was unreachable over the FFI.
- **Go binding**: `AgentStateGraph.ListBranches(prefix) ->
  []BranchEntry`, `AgentStateGraph.DeleteBranch(name) -> bool`,
  `TaskStore.AddTaskWithExtensions(ref, plan, title, priority,
  opts *AddTaskExtOptions) -> (*Task, error)`. `Task` struct
  gains `Payload` / `ParentChange` / `OnComplete`. 3 new tests.
- **C# binding**: `Repository.ListBranches(prefix?)`,
  `Repository.DeleteBranch(name)`,
  `TaskStore.AddTaskWithExtensions(...)` — all real, no more
  stubs. New `BranchEntry` record. 6 new / 4 flipped tests.

### Fixed

- **C# `Decision` polymorphism collision**: the derived records
  had a `Kind` property that collided with
  `[JsonPolymorphic(TypeDiscriminatorPropertyName = "kind")]`.
  Every serialization threw `InvalidOperationException`.
  Fix: removed the redundant `Kind` property from variant
  records on `Decision` and `FallbackAction` (the discriminator
  carries the tag). Added a computed `KindTag` read-only
  property on each abstract polymorphic base (Decision,
  FallbackAction, Selector, OnCompleteHook) for consumers that
  want the string tag without type-matching. Dead
  `DecisionKind` enum removed. The ~8 xUnit tests + parity
  runner that were failing in 0.7.25-beta.1 now pass.

### Changed

- **C# binding promoted from experimental.** `bindings/dotnet/
  README.md` drops the experimental banner and "Known issues"
  section. `.github/workflows/ci.yml` dotnet job drops
  `continue-on-error: true` and the `(experimental)` label.
  NuGet auto-publish is still manual-only — flip after one full
  green CI pass.
- Workspace bumped `0.7.25-beta.1` → `0.7.25-beta.2`. Python,
  TypeScript, and C# binding versions aligned.

### Security / design notes

- Every FFI extension is purely additive — existing extern
  declarations are unchanged, so every consumer compiling against
  0.7.25-beta.1 continues to compile.

## [v0.7.25-beta.1] — 2026-04-21

Theme: **C# / .NET binding (experimental).** Brand-new language
binding joining Python / TypeScript / Go / WASM / C FFI.
Everything lives under `bindings/dotnet/`; zero changes to the
Rust workspace or any other binding. Six engineering sections
plus a release commit.

**Shipped as experimental / community-maintained.** The full
surface compiles cleanly on Ubuntu / macOS / Windows, but a
known System.Text.Json polymorphism collision on the `Decision`
discriminator causes ~8 xUnit tests + the parity runner to
fail at runtime. Fix is specific, documented in
`bindings/dotnet/README.md` "Known issues"; PR welcome. The
.NET CI job is set to `continue-on-error: true` so the rest of
the workspace isn't gated on .NET fixes. NuGet publish is held
until the test suite goes green.

### Added

- **`bindings/dotnet/`** — new NuGet-ready C# binding targeting
  **.NET 10 LTS** (current) with a **.NET 8 LTS** floor. Package
  id `agentstatelabs.AgentStateGraph`. Windows / macOS / Linux via
  P/Invoke over the stable `agentstategraph_ffi` C ABI.

- **Project skeleton**: `AgentStateGraph.sln`,
  `AgentStateGraph.csproj` (multi-target net10.0 + net8.0),
  xUnit test project, `.gitignore`, `README.md`. A
  `NativeLibrary.cs` `ModuleInitializer` registers a
  `DllImportResolver` that searches `AGENTSTATEGRAPH_FFI_PATH`
  (env override), the NuGet `runtimes/<rid>/native/`
  convention, and the cargo target dir (dev walk). Resolver
  short-circuits for any library other than
  `agentstategraph_ffi`.

- **P/Invoke interop layer**: `Interop/{Interop.cs, Handles.cs,
  Strings.cs}` — all **48** `[DllImport]` declarations matching
  the public `agentstategraph.h` surface (12 Repository + 22
  TaskStore + 12 PolicyStore + 2 Migrate). `SafeRepoHandle` /
  `SafeTaskStoreHandle` / `SafePolicyStoreHandle` wrap opaque
  pointers for GC-safe cleanup. UTF-8 marshalling + the
  `agentstategraph_free_string` free convention are hidden
  behind `Strings.ConsumeUtf8`.

- **Idiomatic C# surface**: `Repository` / `TaskStore` /
  `PolicyStore` as `IDisposable` classes, `Commit` / `Decision`
  / `FallbackAction` / `ChangeProposal` / `Policy` / `Task` /
  `Plan` / `OnCompleteHook` / `Epoch` / `Session` /
  `AuthorizedAction` / `ApprovalRule` / `ProcedureStep` /
  `Selector` as records. Decision / FallbackAction / Selector /
  OnCompleteHook use `System.Text.Json`'s polymorphic
  discriminator (`[JsonPolymorphic(TypeDiscriminatorPropertyName
  = "kind")]`) to match the Rust serde-tagged enums.
  `Situation` is `IReadOnlyDictionary<string, string>` — mirrors
  the Rust `#[serde(transparent)]` shape. PolicyStore exposes
  the 10 methods: `Propose`, `Ratify`, `Supersede`, `List`,
  `Active`, `Get`, `History`, `Evaluate`, `EvaluateChange`,
  `CheckTokens`. `CheckTokens` is binding-level (filters
  `Active()` by trigger intersection) — same pattern as every
  other binding. Errors from the native layer surface as
  `AgentStateGraphException`.

- **56 xUnit tests** across seven files (PolicyStore, TaskStore,
  Repository, Decision polymorphism, lifetime / SafeHandle,
  NativeLibrary loader, parity). Mirrors the 14 Python scenarios
  one-to-one plus C#-native IDisposable and SafeHandle checks.

- **Seventh parity runner** on
  `spec/policy_parity_fixture.json`. Joins the existing six
  (Rust reference + Python + TS + Go + WASM + C FFI); all seven
  produce identical `Decision.Kind` + `matched_policy` for the
  same inputs.

- **CI `dotnet` job** on `ubuntu-latest`, `macos-latest`,
  `windows-latest` with `fail-fast: false`. Builds the FFI in
  release mode, then `dotnet restore / build / test`.
  `AGENTSTATEGRAPH_FFI_PATH` points at the cargo target dir.
  Both net10.0 and net8.0 exercised per target framework.

### Changed

- Workspace bumped `0.7.0-beta.1` → `0.7.25-beta.1` per the
  0.0.25 cadence. Python + TypeScript binding `Cargo.toml` /
  `package.json` + `AgentStateGraph.csproj` `<Version>` aligned.

### Known limitations (pre-existing FFI gaps; affect Go binding too)

- **`Repository.ListBranches` / `DeleteBranch`** are not
  declared in the 48 FFI extern functions. The C# binding has
  create-only branch support; list + delete are stubbed in the
  plan but not surfaced. Closing these gaps is a pre-0.7.5 FFI
  extension.
- **`TaskStore.AddTaskWithExtensions`** is a stable-named stub
  that forwards to `AddTask` — the FFI's `add_task` doesn't
  accept the 0.6.0 Task extension fields (`payload`,
  `parent_change`, `on_complete`). Python / TS / WASM bypass
  the FFI and surface these directly; closing the Go + C# gap
  is tracked for the same FFI extension.

### Deferred to follow-ups (all scheduled per ROADMAP.md)

- NuGet auto-publish (currently manual approval only) — when
  the FFI gaps above close.
- Advanced policy features (signing, multi-tenant, external
  evaluator) — 0.7.5-beta.1. Will be automatically available
  through C# via the same FFI since C# rides on the C ABI.
- Watch API exposure in C# — 0.7.75-beta.1.

## [v0.7.0-beta.1] — 2026-04-21

Theme: **bring all existing bindings current.** The policy primitive
(0.6.0) and the Task extensions (0.6.0) and the Session relocation
(0.6.5) are now surfaced through every language binding except C#
(its own milestone at 0.7.25). Nine engineering sections plus a
release commit, each on its own commit per the ROADMAP.md
section-granular rule.

### Added

- **`Policy.active_from` enforced in the evaluator.** A ratified
  policy with `active_from > now` is treated as not-yet-active —
  `evaluate()` and `evaluate_change()` skip it the same way they
  skip an unratified proposal. Enables scheduled go-live per
  POLICY_V1.md §17. New helper `Policy::is_currently_active(now)`.
  `expires_at` remains advisory metadata; expiry enforcement is
  scheduled for 0.7.5.

- **Python binding**: PolicyStore class with 10 methods (propose /
  ratify / supersede / list / active / get / history / evaluate /
  evaluate_change / check_tokens). Complex types (Policy, Decision,
  FallbackAction, Severity, ChangeProposal, Situation, Selector,
  AuthorizedAction, ApprovalRule, ProcedureStep, OnCompleteHook)
  pass as serde-JSON dicts. Session + SessionStatus round-trip via
  new `AgentStateGraph.{create,get,list,end}_session` methods.
  `Task.payload` / `parent_change` / `on_complete` round-trip via
  `TaskStore.add_task` kwargs. 14 new Python tests.

- **TypeScript binding**: napi-rs wrappers mirroring Python's
  surface. PolicyStore class, same 10 methods, JS-object idiom via
  serde round-trip. `createSession` / `getSession` / `listSessions`
  / `endSession` methods. `TaskStore.add_task` extended with three
  trailing-optional args. 11 new node:test cases.

- **C FFI binding**: 12 new extern C functions
  (`agentstategraph_policy_store_{new,free}` plus the 10 operations)
  and the `SgPolicyStore` opaque handle. JSON round-trip on every
  non-primitive field. Declarations added to the shared header at
  `bindings/go/agentstategraph.h`. 10 new native Rust FFI tests.

- **Go binding**: cgo wrappers over the C FFI. `*PolicyStore` with
  10 methods returning typed Go structs (Policy, Decision,
  ChangeProposal, etc.). Variant-tagged sub-documents exposed as
  `json.RawMessage` so Go callers unmarshal into the variant they
  need. 13 new Go tests.

- **WASM binding**: wasm-bindgen wrappers. PolicyStore with 10
  methods, JSON-string boundary idiom (matches existing TaskStore
  surface in this crate). `createSession` / `getSession` /
  `listSessions` / `endSession` wrappers on `WasmAgentStateGraph`.
  `tasksAddTaskWithExtensions` for the Task extension fields. 13
  new wasm-bindgen-test cases, gated behind `#[cfg(target_arch =
  "wasm32")]`.

- **Cross-binding parity tests**: shared scenario fixture at
  `spec/policy_parity_fixture.json` exercised from six runners —
  Rust reference (`agentstategraph-policy`) plus Python, TS, Go,
  WASM, and C FFI. All six produce identical `Decision.kind` and
  `matched_policy` for the same inputs. Scenario is the POLICY_V1.md
  §22.7 OpenSearch flow with an extra policy ratified-but-future-
  active to guard §1 regressions.

### Changed

- **Session imports unified**. Every binding now imports `Session`
  and `SessionStatus` from `agentstategraph_core` (the canonical
  location since 0.6.5) rather than the `agentstategraph::session`
  facade re-export. No functional change; readability audit.

- Workspace bumped `0.6.75-beta.1` → `0.7.0-beta.1` per the 0.0.25
  cadence. Python + TypeScript binding `Cargo.toml` / `package.json`
  aligned.

- **Clippy `-D warnings` baseline holds** (the 0.6.75 §5 guardrail).
  The `unnecessary_sort_by` pattern from clippy 1.95, caught on CI,
  was fixed in 0.6.75-beta.1 post-release; this milestone adds a
  `collapsible_if` fix in the TypeScript binding's situation helper.

### Deferred to follow-ups (all scheduled per ROADMAP.md)

- **C# / .NET binding** — 0.7.25-beta.1
- **`expires_at` scheduled deactivation** — 0.7.5-beta.1
- **Advanced policy**: cryptographic signing, multi-tenant
  namespace, external Rego/Cedar/WASM evaluator escape hatch —
  0.7.5-beta.1
- **Watch API** — 0.7.75-beta.1
- **AgentStateConsole Phase 1 support** — 0.8.0-beta.1

### Security / design notes

- `check_tokens` is binding-level logic in all five bindings plus
  the MCP server (filters `active()` policies by trigger
  intersection). Candidate for hoisting into `PolicyStore` proper
  in a follow-up so the behaviour lives in one place.

## [v0.6.75-beta.1] — 2026-04-21

Theme: **complete the 0.6 line.** Every deferral flagged in 0.6.0 +
0.6.5 is now closed. Six engineering sections plus a release commit,
each on its own commit per the ROADMAP.md section-granular rule.

### Added

- **Postgres `EpochStore` + `SessionStore`** implementations replace
  the 0.6.5-beta.1 stubs. Schema mirrors SQLite exactly (JSON-as-TEXT);
  tenant isolation via the existing `tenant_id` column pattern; four
  new indexes (`idx_epochs_tenant_status`,
  `idx_sessions_tenant_agent`, `idx_commits_tenant_epoch`,
  `idx_commits_tenant_session`); `ADD COLUMN IF NOT EXISTS` for
  migration safety on `commits.epoch_id` / `session_id`. Seal
  enforcement + session-ended guards identical to SQLite.

- **IndexedDB `EpochStore` + `SessionStore`** implementations
  replace the 0.6.5-beta.1 stubs with the write-through-queue
  pattern. Four new pending queues (`pending_epochs`,
  `pending_sessions`, `pending_commit_epochs`,
  `pending_commit_sessions`) plus four new JS-side load methods
  (`load_epochs`, `load_sessions`, `load_commit_epochs`,
  `load_commit_sessions`). Migration: onupgradeneeded bump from
  IDB v1 → v2 for the four new object stores. 6 new wasm-native
  unit tests.

- **Four new MCP tools**: `enter_epoch`, `exit_epoch`,
  `enter_session`, `exit_session`. Tool count 44 → 48. Thin
  wrappers over the `Repository::{set_,}active_epoch` plumbing
  shipped in 0.6.5 (`05b82af`). `enter_epoch` refuses sealed /
  archived epochs; `enter_session` refuses non-Active sessions.
  Each returns the previous active id for restore-on-scope-exit
  patterns. Typical flow documented in
  `crates/agentstategraph-mcp/README.md` §"Epoch + session
  scoping".

- **`docs/SESSION-API.md`**: audit outcome + migration guide for
  the 0.6.5 `SessionManager::{create, list, get, end}` →
  `Result` break. Contract for downstream bindings when the
  0.7.0 "bring all bindings current" milestone exposes Session
  through Python / TS / Go / WASM / C FFI.

- **`examples/policy_demo.rs`** in `agentstategraph-policy` — a
  runnable walkthrough of the POLICY_V1.md §22.7 OpenSearch
  scenario. Seeds the `/change-control/high-cost-change` policy,
  evaluates three candidate proposals (scores 3/7/9), prints
  the full fallback + approval-task playbook, closes with the
  thesis line and a first-class commit log. Run with
  `cargo run --example policy_demo -p agentstategraph-policy`.

- **`compare` MCP tool now emits `tokens` per handle.** Uses the
  same `infer_tokens_from_diff` helper that `commit_spec` uses
  internally. Agents pre-flighting a promotion can see which
  handles will trip which policies *before* calling `commit_spec`.
  New test `test_compare_tokens_match_commit_spec_inference`
  guards the parity contract.

### Changed

- **Clippy baseline restored: `-D warnings` across the workspace.**
  Pre-existing debt in `agentstategraph-core` (merge / diff /
  object), `agentstategraph-storage` (indexeddb `is_multiple_of`),
  `agentstategraph` (speculation unused bindings, repo test
  binding), and every example fixed. Binding glue crates
  (`bindings/typescript`, `agentstategraph-wasm`) carry a
  crate-level `#![allow(clippy::too_many_arguments)]` with an
  inline comment — exported napi/wasm-bindgen functions mirror the
  JS call shape and can't collapse args. CI `clippy` job loses its
  `continue-on-error` and stops excluding `agentstategraph-wasm` /
  `agentstategraph-napi`. From this release on, `cargo clippy
  --workspace --exclude agentstategraph-python --all-targets --
  -D warnings` is mandatory in CI.

- Workspace bumped `0.6.5-beta.1` → `0.6.75-beta.1` per the
  **0.0.25 cadence** (amended from 0.0.5 on 2026-04-21; see
  `spec/ROADMAP.md`). Python + TypeScript binding `Cargo.toml` /
  `package.json` aligned.

### Fixed

- None of the listed clippy fixes altered behaviour; test counts
  unchanged from 0.6.5-beta.1 modulo the new tests added in this
  release.

### Deferred to follow-ups (all scheduled)

- Policy surface on Py / TS / Go / WASM / C FFI — 0.7.0-beta.1
- C# / .NET binding — 0.7.25-beta.1
- Advanced policy (signing, multi-tenant, external evaluator) —
  0.7.5-beta.1
- Watch API — 0.7.75-beta.1
- AgentStateConsole Phase 1 support — 0.8.0-beta.1

## [v0.6.5-beta.1] — 2026-04-21

### Added

- **Persistent epochs and sessions.** Epochs previously lived in a
  `RwLock<Vec<Epoch>>` on `Repository` and sessions in a
  `RwLock<HashMap>` on `SessionManager`; both vanished on MCP process
  restart. A sealed epoch that doesn't survive a restart defeats the
  compliance story — that's fixed in this release.
- **New storage traits** `EpochStore` and `SessionStore` in
  `agentstategraph-storage` with methods for
  `create` / `seal` / `list` / `get` / `set_commit_epoch` /
  `set_commit_session` / `end_session`. `SqliteStorage` and
  `MemoryStorage` implement both.
- **`commits.epoch_id` and `commits.session_id`** — new nullable
  columns on the commits table so an auditor can roll commits up to
  the epoch or session they landed in. Added via migration-safe
  `ALTER TABLE ADD COLUMN` using a `PRAGMA table_info` check-and-add
  pattern; existing databases open unchanged.
- **Persisted seal enforcement.** `epochs.sealed_commits` is now
  stored (JSON blob). The V8 "no ref mutation to sealed commits"
  guard keeps working across restart.
- **`Repository::active_epoch` / `active_session`** — new
  `RwLock<Option<String>>` knobs with getter + setter. On commit
  finalisation, if an active id is set, the new `set_commit_epoch`
  / `set_commit_session` calls wire the association. MCP tool-level
  `enter_epoch` / `exit_epoch` helpers are a follow-up.
- **`Session` and `SessionStatus` moved into `agentstategraph-core`**
  so the storage crate can reference the types without depending on
  the repo crate. `Epoch` was already there.
- **14 new tests** across four files covering round-trip, restart
  survival, seal enforcement, and migration safety on both SQLite
  and in-memory backends. Workspace now at 357 passing tests.

### Changed

- Workspace bumped from `0.6.0-beta.1` to `0.6.5-beta.1` per the
  0.0.5-increment cadence.
- Python + TypeScript binding `Cargo.toml` / `package.json` aligned
  to `0.6.5-beta.1`.
- **`SessionManager::{create, list, get, end}` now return `Result`**
  so callers can surface `StorageError`. Internal callers (MCP
  server, TypeScript binding, `multi_agent` example) were updated.
  External consumers will need to add `?` / `.unwrap()` on session
  calls — call sites outside the repo haven't been audited.

### Deferred

- **Postgres** and **IndexedDB** backends have stub `EpochStore` +
  `SessionStore` implementations that return
  `StorageError::Backend("not yet implemented")`. Both compile
  cleanly; actual persistence slated for a later milestone (browser
  WASM and Postgres deployments rarely need epoch/session audit
  today).
- MCP tools `enter_epoch` / `exit_epoch` / `enter_session` /
  `exit_session` for setting the active ids from a client. The
  plumbing exists; the tool wrappers are follow-up.
- Auto-rolled dashboards (Stack Viewer, Lens) consuming the new
  columns — server-side readiness is here; front-end work lives in
  AgentStateConsole.

### Security / design notes

- `PERSISTENCE_SPEC.md` and `PERSISTENCE-IMPLEMENTATION-PLAN.md` both
  live in `spec/`. The first is the canonical design; the second
  records what we actually built.
- Speculations stay in-memory by design — they're ephemeral handles
  that become real commits on `commit_spec` promotion.
- Plans and policies were already persisted via the state tree at
  `/plans/*` and `/policies/*`. No change.

## [v0.6.0-beta.1] — 2026-04-21

### Added

- **New crate `agentstategraph-policy`** — the fourth primitive in the family
  (alongside memory, tasks, and migrate). Implements the authorization model
  specified in `strategy/POLICY_V1.md` v1.1: `Policy` with `allow` /
  `deny` / `require_approval` rules over (situation, action, agent_id)
  triples; situation selectors (`Selector::{All, Any, Not, Eq, Ne, Matches,
  Exists, Gt, Gte, Lt, Lte}`); `Decision::{Allow, Deny, RequireApproval,
  NoPolicyMatch}` with `deny > require_approval > allow` precedence;
  proposal → ratify → supersede lifecycle with chained versions; policies
  stored at `/policies/<domain>/<subdomain>/<slug>` as peer to `/plans/`.
  **Proposals are never consulted by the evaluator** — only ratified
  policies are active. 54 tests (17 unit + 36 integration + 1 doctest).

- **Cost-of-change dimension on `Policy`** — new fields
  `triggers: Vec<String>`, `required_fields: Vec<String>`, `severity: Severity`
  per `POLICY_V1.md` §22.2. A `ChangeProposal` carries tokens (e.g.
  `destructive`, `schema-change`, `reindex`, `migration`, `ref-rewrite`,
  `large`) that match against a policy's `triggers`. Missing required
  fields short-circuit to `RequireApproval`.

- **`FallbackAction` enum** (`Block`, `PickAlternative`,
  `LowestRiskAlternative`, `KeepCurrentState`, `DelegateTo`) on every
  `ApprovalRule` per §22.3. This is the "what to do while it waits"
  primitive — policies that require approval can now prescribe a safe
  fallback the agent applies immediately, so operations keep running
  while the approval gate is open.

- **`Task` extensions** (`agentstategraph-tasks`): `payload`,
  `parent_change`, `on_complete: Option<OnCompleteHook>` (variants
  `PromoteChange` / `Named`). Enables the fallback workflow to create
  approval tasks that carry the deferred `ChangeProposal` payload and a
  parent-change back-reference. All fields are `#[serde(default,
  skip_serializing_if = "Option::is_none")]` so legacy tasks deserialize
  unchanged and tasks that don't use them serialize byte-identically to
  the prior shape. 7 new tests including a hardcoded legacy-JSON fixture.

- **9 new MCP tools** in `agentstategraph-mcp`: `policy_propose`,
  `policy_ratify`, `policy_supersede`, `policy_list`, `policy_show`,
  `policy_history`, `policy_evaluate`, `policy_evaluate_change`,
  `policy_check_tokens`. Brings the tool count from 35 to **44**.

- **`commit_spec` gate on policy evaluation** — the speculation promotion
  tool now builds a `ChangeProposal` from the spec handle (with inferred
  tokens from the diff), calls `PolicyStore::evaluate_change`, and only
  promotes on `Allow` or `NoPolicyMatch`. `Deny` and `RequireApproval`
  short-circuit the promotion and return the `Decision` JSON so callers
  can apply the fallback branch. Token inference helpers
  (`infer_change_tokens`, `infer_tokens_from_diff`) are exposed for
  testing and are application logic that lives in the MCP crate, not the
  policy engine.

- **Fail-safe translation at the MCP layer** — new `with_fail_safe(..)`
  server config (default `"deny"`). The engine returns `NoPolicyMatch`
  verbatim; the MCP layer translates per config before returning, while
  still surfacing the original `no_policy_match` kind so callers can
  distinguish "authorized by an explicit allow" from "default policy
  applied."

- **Docs**: `spec/POLICY-IMPLEMENTATION-PLAN.md` (execution plan);
  `docs/POLICY_GUIDE.md` user-facing guide covering authoring,
  ratification, the fallback pattern, composition with speculation, and
  the soft-enforcement model.

### Changed

- Workspace bumped from `0.5.0-beta.1` to `0.6.0-beta.1`. New primitive
  warrants a minor bump.
- Python + TypeScript binding `Cargo.toml` and `package.json` aligned
  to `0.6.0-beta.1`.
- Root `README.md` lists Policy alongside Memory / Tasks / Migrate as
  an engine-level primitive.

### Security / design notes

- **Soft enforcement only** (`POLICY_V1.md` §11). ASG cannot physically
  stop a misbehaving agent — the evaluator tells the agent what's
  allowed; the agent still has to respect the decision. The value is
  clarity of boundary, blame trail, and composition with hard
  enforcement (OPA / Cedar / IAM) at the infrastructure layer. Do not
  market this as "stops rogue agents."
- The thesis line (`POLICY_V1.md` §22.1): _an AI agent that knows when
  to act, when to ask, and what to do while it waits — and all of it
  recorded, auditable, transparent, and sealed for export._

### Deferred to follow-ups

- Bindings for policy types across Py / TS / Go / WASM / C FFI (same
  pattern as the `-tasks` roll-out).
- Console / review UI surfaces for proposal review and the approval
  task queue.
- `ctx policy` CLI subcommand.
- External Rego / Cedar file references as an escape hatch for complex
  rules.
- Time-based activation (`active_from` scheduled go-live).
- Multi-tenant namespace isolation for policies.
- Cryptographic signing of policies.

## [v0.5.0-beta.1] — 2026-04-17

### Changed
- Minor version bump from `0.4.0-beta.3` to `0.5.0-beta.1`. The migration framework, `/_meta/*` guard, and upgrade CLI landed in the 0.4 series but are significant enough to reflect as a minor bump in the release line. No API changes from `0.4.0-beta.3` — same code, larger version number.
- All bindings (Python, TypeScript) aligned to `0.5.0-beta.1`.

## [v0.4.0-beta.3] — 2026-04-17

### Changed
- Aligned Python and TypeScript bindings (both Cargo and package files) to the workspace version `0.4.0-beta.3`. Bindings had been stranded at `0.3.5-beta.2` through the `0.4.0-beta.1` and `0.4.0-beta.2` workspace bumps.

## [v0.4.0-beta.2] — 2026-04-17

### Added
- **New crate `agentstategraph-migrate`** — schema-evolution framework for ASG databases. Provides `Migration` trait, `Registry`, `check()` for startup introspection (returns `UpToDate` / `UpgradeAvailable` / `Downgrade` / `Unversioned` / `Corrupt`), and a `Runner` with `DryRun` and `Apply` modes. First shipped migration (`plan_assignments_sidecar_to_native`) walks legacy `/plan_assignments` entries onto native `Task.assigned_to` and bumps `/_meta/schema_version` atomically. Exit-code constants follow `sysexits.h` spirit (64 / 65 / 70 / 75) for ops tooling.
- **`/_meta/*` reserved path guard** on `Repository::{set, set_json, spec_set, spec_set_json, delete}` and `commit_speculation`. Writes under `/_meta/*` now require `IntentCategory::Migrate` — a new `RepoError::ReservedPath` is returned otherwise. Protects schema metadata from accidental overwrites.
- **`Repository::init()` stamps `/_meta/schema_version`** in its initial commit using a decoupled `SCHEMA_VERSION` constant. The schema version advances only when a migration runs, independent of the crate's release version.
- **`agentstategraph-mcp migrate` subcommand** — one-shot maintenance CLI. Flags: `--db`, `--storage`, `--ref`, `--to`, `--dry-run`, `--yes`, `-h`. Refuses to start the MCP/HTTP surface. Prints per-step status, commit IDs, and final version.
- **Upgrade-path design doc** at `spec/UPGRADE-PATH.md` covering versioning model, migration registry, consumer-side upgrade flow, the sidecar migration as worked example, CLI, and downgrade/rollback semantics.

### Changed
- Workspace version bumped from `0.4.0-beta.1` to `0.4.0-beta.2`.
- `SCHEMA_VERSION` constant in `agentstategraph` is now a literal `"0.4.0"`, decoupled from `env!("CARGO_PKG_VERSION")`. Bump only when a migration advances the on-disk shape.

## [v0.4.0-beta.1] — 2026-04-15

### Added
- **New crate `agentstategraph-tasks`** — shared task-store primitives for plan-rot prevention. Provides `Plan`, `Task`, `TaskStore`, `Proof`, `Verifier` trait, and a full state machine (`pending → in_progress → done`, with proof and blocker enforcement). Multiple ASG consumers share a single implementation instead of reimplementing task types independently. Establishes the pattern for opinionated-but-shared sibling crates in the workspace.
- **`IntentCategory::Plan`** variant added to `agentstategraph-core` — plan/task operations are natively filterable in log and blame queries. Recognized in MCP, HTTP, FFI, and WASM parsers. **Consumer caveat**: the new native variant serializes as `"Plan"`; pre-existing data written as `{"Custom":"Plan"}` still deserializes as the `Custom` variant, so a filter on `IntentCategory::Plan` will NOT match legacy `Custom("Plan")` commits. Normalise at read time if you need unified filtering across the upgrade boundary.
- **`Repository::spec_set_json`** convenience method on the high-level API — mirrors `set_json` for the speculation path, used by `agentstategraph-tasks` for atomic multi-path commits.
- **`Task::assigned_to`** — optional agent-assignment field on `Task`, eliminating the need for consumer-side sidecar storage. New `TaskStore::assign_task`, `unassign_task`, and `next_task_for` methods support assignment-aware task selection. `list_plans_by_status` adds native status filtering.

### Changed
- Workspace version bumped from `0.3.5-beta.2` to `0.4.0-beta.1` (new public crate).

## [v0.3.5-beta.2] — 2026-04-09

### Changed
- **Naming hygiene pass.** Every standalone "StateGraph" reference replaced with the full "AgentStateGraph" across prose, identifiers, symbols, and packages. Eliminates collision surface with LangGraph's `StateGraph` class (the primary primitive LangChain developers use) and Terrateam's Stategraph Terraform backend.
  - Rust: `StateGraphServer` → `AgentStateGraphServer`; MCP tool method names `stategraph_*` → `agentstategraph_*` (all 20 tools); `WasmStateGraph` → `WasmAgentStateGraph`
  - C FFI: all 12 extern symbols renamed (`agentstategraph_new_memory`, `agentstategraph_get`, etc.)
  - Python: class `StateGraph` → `AgentStateGraph`
  - TypeScript: class `StateGraph` → `AgentStateGraph`; npm package `stategraph` → `agentstategraph`
  - Go: package `stategraph` → `agentstategraph`; struct and files renamed; module path `github.com/agentstatelabs/agentstategraph/bindings/go`
  - JSON Schema extensions: `x-stategraph-*` → `x-agentstategraph-*`
  - URI scheme: `stategraph://` → `agentstategraph://` (spec)
  - MCP server key in config examples: `"stategraph"` → `"agentstategraph"`
  - Default SQLite path: `./stategraph.db` → `./agentstategraph.db`
  - Default WASM IndexedDB name: `"stategraph"` → `"agentstategraph"`
  - Spec file: `spec/STATEGRAPH-RFC.md` → `spec/AGENTSTATEGRAPH-RFC.md`
  - Repository URL in `Cargo.toml` points to `github.com/agentstatelabs/agentstategraph`

### Added
- **Sharpened positioning.** README and landing page now lead with the one-sentence Git-analogy framing: *"AgentStateGraph is to agent state what Git was to source code — a content-addressed, branchable, blameable state primitive, designed from the ground up for AI agents as the primary actor."* Followed by an explicit "what it is not" paragraph: not a Terraform replacement (wrong actor model), not a LangGraph helper (different layer of the stack), but a state primitive on which next-generation IaC, GitOps, and agent-native ops tooling can all be built.
- **Disambiguation page** at `site/src/content/docs/compare.md` — "AgentStateGraph vs. Stategraph vs. LangGraph's StateGraph." Three-column comparison table covering actor model, data model, branching, intent/reasoning, blame, audit surface, language bindings, primary interface, storage backends, and closest analogy. Linked from site nav (Getting Started) and the README.
- **Landing page rework:** hero tagline carries the verbatim Git-analogy framing; new "The vision" and "What it is not" sections above the cards; compare link in the hero actions.
- **CONTRIBUTING.md Naming section** stating the hygiene rule for future contributors, including the no-ASG convention (collides with AWS Auto Scaling Groups).

### Fixed
- **Previously leaked short forms** in: MCP server key shown in README config example (visible on-screen as `mcp__stategraph__…` during recorded demos), Rust struct names, crate descriptions, doc comments across all six crates, spec file, Python/TypeScript/Go/Rust/WASM examples, browser demo, blog post, and all site guides.

## [v0.3.0-beta.1] — 2026-04-09

### Status
**Beta** — Specification complete, all features implemented and tested. Not yet published to crates.io / PyPI / npm. Awaiting community feedback.

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

## [v0.1.0] — 2026-04-04

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
