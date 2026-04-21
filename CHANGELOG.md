# Changelog

All notable changes to AgentStateGraph are documented here.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
Versioning: [Semantic Versioning](https://semver.org/spec/v2.0.0.html)

## [0.7.25-beta.1] — 2026-04-21

Theme: **C# / .NET binding.** Brand-new language binding joining
Python / TypeScript / Go / WASM / C FFI. Everything lives under
`bindings/dotnet/`; zero changes to the Rust workspace or any
other binding. Six engineering sections plus a release commit.

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

## [0.7.0-beta.1] — 2026-04-21

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

## [0.6.75-beta.1] — 2026-04-21

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

## [0.6.5-beta.1] — 2026-04-21

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

## [0.6.0-beta.1] — 2026-04-21

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
- AgentStateConsole / CTXone / Lens UI surfaces for proposal review and
  the approval task queue.
- `ctx policy` CLI subcommand.
- External Rego / Cedar file references as an escape hatch for complex
  rules.
- Time-based activation (`active_from` scheduled go-live).
- Multi-tenant namespace isolation for policies.
- Cryptographic signing of policies.

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
