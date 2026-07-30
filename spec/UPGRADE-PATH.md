# AgentStateGraph Upgrade Path

**Status:** Implemented. See `crates/agentstategraph-migrate/` and `agentstategraph-mcp migrate`. Consumer integration snippet in that crate's README.
**Scope:** Schema evolution for `.db` files + consumer upgrade ergonomics.
**Context:** Workspace at `0.9.2`. The first shipped migration walks a legacy `/plan_assignments` sidecar → native `Task::assigned_to`.

---

## 1. Schema versioning — where the version lives

**Decision:** a single well-known state path, `/_meta/schema_version`, written on first `init()` and bumped atomically by migration commits on `main`.

- Path: `/_meta/schema_version` — string value, SemVer (e.g. `"0.4.0"`).
- The underscore prefix signals "reserved/internal"; consumer writes to `/_meta/*` should be rejected at the Repository layer (new guard). Parallel slot `/_meta/app_version` reserved for consumer metadata.
- **Secret sub-prefix:** `/_meta/_secret/*` is further reserved for secret-bearing metadata. BOTH reads AND writes under this sub-tree are gated to `IntentCategory::Migrate`; the broader `/_meta/*` prefix only gates writes. Use `Repository::get_with_intent` / `get_json_with_intent` with a Migrate intent to access these values. `list_paths` and `search_values` silently filter out entries under `/_meta/_secret/*` so a broad `/` or `/_meta` walk cannot leak secret path names.
- Written during `Repository::init()` at [crates/agentstategraph/src/repo.rs:119](../crates/agentstategraph/src/repo.rs). On an already-initialized repo that lacks `/_meta/schema_version` (i.e. pre-0.4), we treat the absence as an **implicit version**, not an error — see §3.
- Bumped only by migrations, in the same commit as the migration's effects. Atomicity comes from the existing multi-path commit plumbing used by `agentstategraph-tasks` (`spec_set_json`).

**Why this key and not a dedicated ref or a storage-level column?**
- Storage-level (a SQLite column) leaks into every backend (memory, IndexedDB, future Postgres). The state tree is the one universal plane.
- A dedicated ref (`_meta`) is invisible to log/blame tooling. Putting it inside the state graph means `asg log --intent Migrate` already shows the upgrade history for free.
- Content-addressed history means a downgrade = checking out the pre-migration parent commit. See §6.

**Implicit "version 0":** any `.db` file without `/_meta/schema_version` is treated as `0.3.x` (pre-tasks-crate). The very first migration's job is to stamp the version on arrival.

---

## 2. Migration registry — core or sibling crate?

**Decision:** new sibling crate `agentstategraph-migrate`.

The [PLAN_ROT_ENGINE_HANDOFF.md](../PLAN_ROT_ENGINE_HANDOFF.md) doctrine (validated by `agentstategraph-tasks`): opinionated-but-shared concerns live in siblings, core stays minimal. Migration logic aggregates knowledge of *multiple* consumer-layer schemas (task schemas from `agentstategraph-tasks`, possibly future ones), which should not bleed into `agentstategraph-core` or `agentstategraph`. Core's concern is the substrate; migrate's concern is named transformations on top of it.

**Shape:**

```rust
pub trait Migration: Send + Sync {
    fn from_version(&self) -> &semver::VersionReq; // "^0.3"
    fn to_version(&self) -> &semver::Version;      // "0.4.0"
    fn name(&self) -> &str;                        // "plan_assignments_sidecar_to_native"
    fn describe(&self) -> &str;                    // human summary for dry-run
    fn applies_to(&self, repo: &Repository, ref_name: &str) -> Result<bool>;
    fn migrate(&self, repo: &Repository, ref_name: &str) -> Result<MigrationOutcome>;
}

pub struct Registry { /* Vec<Box<dyn Migration>> */ }

impl Registry {
    pub fn builtin() -> Self;                     // all shipped migrations
    pub fn register(&mut self, m: Box<dyn Migration>);
    pub fn plan(&self, current: &Version, target: &Version) -> Vec<&dyn Migration>;
    pub fn run(&self, repo: &Repository, ref_name: &str, target: &Version, mode: RunMode) -> Result<Report>;
}

pub enum RunMode { DryRun, Apply }
```

- `migrate()` produces the migration commit itself, using `IntentCategory::Migrate` ([crates/agentstategraph-core/src/intent.rs:63](../crates/agentstategraph-core/src/intent.rs)). It bumps `/_meta/schema_version` in the same commit as its data changes. One migration = one commit.
- Idempotency via `applies_to()` — re-running a completed migration is a no-op, not an error.
- Migrations ordered by `to_version` then registration order. Strictly linear SemVer chain; no graph.
- Third-party crates (future siblings, app-specific schemas) can `registry.register()` their own migrations, keeping consumer migrations discoverable next to the schema they evolve.

**Why not `agentstategraph`?** That crate is the integration shell, already pulling core + storage. Putting migration there means every consumer (FFI, WASM, Python bindings) pays compile cost for upgrade logic most don't run.

---

## 3. Consumer-side upgrade

**API (in `agentstategraph-migrate`):**

```rust
pub fn check(repo: &Repository, target: &Version) -> CheckResult;

pub enum CheckResult {
    UpToDate,
    UpgradeAvailable { from: Version, to: Version, migrations: Vec<&'static str> },
    Downgrade { db: Version, binary: Version },
    Unversioned,         // implicit 0.3 — stamp needed
    Corrupt(String),
}
```

Consumers call `check()` at startup before handing the `Repository` to application code.

**Recommended consumer policy (document, don't enforce):**

| State | Default | Override |
|---|---|---|
| `UpToDate` | continue | — |
| `Unversioned` / `UpgradeAvailable` (patch/minor) | auto-migrate with log line | `ASG_MIGRATE=prompt\|never` env |
| `UpgradeAvailable` (major) | refuse to start, print `asg-migrate` command | `ASG_MIGRATE=auto` |
| `Downgrade` | refuse, non-zero exit | none — data-loss risk |
| `Corrupt` | refuse, non-zero exit | none |

**Exit codes (matter for ops / systemd / healthchecks):**

- `0` — ok
- `64` — downgrade refused (`EX_USAGE` spirit)
- `65` — corrupt `/_meta` (`EX_DATAERR`)
- `75` — upgrade required but `ASG_MIGRATE=never` (`EX_TEMPFAIL`)

Codes live as constants in `agentstategraph-migrate::exit`. Consumers surface them through their own CLI.

**Consumer integration:** one call at boot, before the server opens its listener. If policy is "prompt," the consumer prints the diff summary on stderr and reads y/n from stdin — non-interactive deployments set `ASG_MIGRATE=auto` explicitly.

---

## 4. First worked example — `plan_assignments` sidecar migration

Lives in `agentstategraph-migrate/src/migrations/v0_4_0_plan_assignments.rs`.

**Probe (`applies_to`)**: `repo.get("main", "/plan_assignments")` returns a non-empty map.

**Migration body:**

1. Read `/plan_assignments` (map: `plan_name → map: task_id → agent_id`).
2. For each `(plan, task_id, agent)`: invoke `TaskStore::assign_task` from `agentstategraph-tasks` to stamp `assigned_to`.
3. Delete `/plan_assignments` entirely.
4. Bump `/_meta/schema_version` to `0.4.0`.
5. Commit all of the above in a **single atomic commit** via the same multi-path commit plumbing used by `TaskStore::create_plan` ([crates/agentstategraph-tasks/src/lib.rs](../crates/agentstategraph-tasks/src/lib.rs)). Intent:
   ```
   IntentCategory::Migrate
   description: "migrate plan_assignments sidecar → Task.assigned_to (0.3 → 0.4.0)"
   reasoning: "native assigned_to supersedes sidecar; spec/UPGRADE-PATH.md §4"
   ```

**Crate dependency:** `agentstategraph-migrate` depends on `agentstategraph-tasks` because writing `assigned_to` correctly requires the `Task` type. `tasks` has no reverse dependency, and consumers that skip `tasks` also have nothing to migrate here.

**Testing:** fixture `.db` seeded with `/plan_assignments`, round-trip through `migrate`, assert (a) `/plan_assignments` gone (b) `Task::assigned_to` populated (c) `/_meta/schema_version == "0.4.0"` (d) exactly one new `IntentCategory::Migrate` commit (e) re-running is a no-op (commit count unchanged).

---

## 5. CLI — `agentstategraph-mcp migrate`

Add a `migrate` subcommand to the existing binary at [crates/agentstategraph-mcp/src/main.rs](../crates/agentstategraph-mcp/src/main.rs). This binary is already the operator-facing surface; no third binary needed. The hand-rolled arg loop branches on subcommand before entering server parsing.

```
agentstategraph-mcp migrate --db ./state.db [--to 0.4.0] [--dry-run] [--yes]
                           [--ref main] [--storage sqlite|memory]
```

- No `--to` → latest known version from builtin registry.
- `--dry-run` → print plan, exit `0`.
- Default (no `--dry-run`, no `--yes`) → print plan, prompt on stdin.
- `--yes` → apply without prompt.
- Refuses on a non-clean ref (pending speculations): abort, tell user to resolve first.
- Output: per-migration status line (`ok | skip | fail`), final version, commit IDs of each migration commit (so operators can `asg log` or branch from them).
- Non-zero exit on any failure; partial application is safe because each migration is its own commit.

The subcommand must not start the MCP/HTTP server — it is a one-shot maintenance mode. Server mode unchanged.

---

## 6. Downgrade / rollback

We get most of this for free from content-addressing.

**What works natively:**
- Every migration produces a commit. `asg log --intent Migrate` lists them.
- To roll back, the operator creates a branch at the pre-migration parent: `asg branch create pre-0.4 <parent-commit>`. The old binary can check out that branch and operate normally — data on `main` is preserved.
- Safer than `git reset --hard`: `main` still advances; downgrade is a *new ref*, not a rewrite.

**What does not work:**
- Running an *older* binary against a `main` that has advanced past its schema version. The `Downgrade` check in §3 catches this — exit 64. Fix: point the old binary at the pre-migration branch, or upgrade the binary.

**What we explicitly don't ship:**
- Inverse migrations (`down()`). Bad complexity-to-value ratio in a content-addressed system where the old state is already durable. Operators who need to continue from the old state branch from the parent commit — standard Git-shaped workflow.
- Automatic rollback on partial failure. Each migration is one commit; a failure mid-registry leaves a valid intermediate version. Operator re-runs or branches from the last good commit.

**Multi-ref caveat:** migration runs against a single ref (default `main`). Consumers with long-lived branches migrate each independently. Document this in CLI help; do not silently auto-migrate every ref.

---

## Summary of new artifacts

| Artifact | Location | New? |
|---|---|---|
| `/_meta/schema_version` state convention | runtime | yes |
| Reject consumer writes to `/_meta/*` | `agentstategraph/src/repo.rs` | yes (guard) |
| Version stamp in `Repository::init()` | [repo.rs:119](../crates/agentstategraph/src/repo.rs) | yes |
| `agentstategraph-migrate` crate | `crates/agentstategraph-migrate` | yes |
| First migration `v0_4_0_plan_assignments` | inside above | yes |
| `migrate` subcommand | [mcp/src/main.rs](../crates/agentstategraph-mcp/src/main.rs) | yes |
| Consumer `check()` doc + exit codes | README / RFC appendix | doc only |

---

## Decisions from review

1. **No engine-level migration on day one.** Ship the framework, the `/_meta/schema_version` path, the guard on `/_meta/*` writes, and the `plan_assignments` migration. Engine types (Commit, Intent, Object, state-tree shape) are stable and don't need a v0→v1 migration yet. First engine migration lands when an actual breaking engine change does.

2. **Fresh graphs get the version stamped by `init()`, not by a migration commit.** The "less confusing" choice: a brand-new `.db` has `/_meta/schema_version` written as part of the initial Checkpoint commit that `Repository::init()` already produces, not as a separate `IntentCategory::Migrate` commit. Migrate commits appear only when real transformation work happens (or when stamping a pre-0.4 graph that lacked the key — the Unversioned → current stamp is a migration commit, because it marks a real transition).

3. **Sequential migration application, ordered by `to_version`.** No parallelism, no interleaving across hypothetical owners. If we ever move to per-crate versioning (see §7.2 below), the rule stays: pick the owner, drain its pending migrations in order, then move on.

4. **Enforce the `/_meta/*` namespace at the Repository layer.** Writes to paths starting with `/_meta/` are rejected unless the caller opts in explicitly:

   ```rust
   impl CommitOptions {
       /// Permit writes to reserved paths (`/_meta/*`). Reserved for
       /// migrations and `Repository::init()`. Ordinary agent commits
       /// should never set this.
       pub fn allow_reserved(mut self) -> Self {
           self.allow_reserved = true;
           self
       }
   }
   ```

   `Repository::{set, set_json, spec_set, spec_set_json, delete}` check the flag before touching a `/_meta/*` path; normal callers get a new `RepoError::ReservedPath` error. The migrate crate's `Runner` and `Repository::init()` both build their `CommitOptions` with `.allow_reserved()`. Cross-owner reuse of the namespace (e.g., a future `/_meta/workflows_schema_version`) inherits the same guard for free.

## Confirmed

5. **`/_meta/schema_version` advances only when migrations run.** The key holds the *last schema-affecting version*, not the current binary's SemVer. Binary 0.4.2 with `schema_version = "0.4.0"` and no 0.4.2 migration registered is compatible without touching the key. The binary defines its `SCHEMA_VERSION` constant and the check is `db_version` is recognised by the registry — not strict equality.

6. **Single substrate-wide version.** One `/_meta/schema_version` covers engine + siblings. Moving a single crate to its own `/_meta/<name>_schema_version` stays on the table as an additive future change; nothing shipped today assumes it.

7. **Multi-tenant: `migrate` iterates, one report row per tenant.** Uses a storage-level `list_tenants()` where present (Postgres); falls back to a single pass on backends without tenancy (memory, SQLite single-db).

---

## 8. Implementation plan (post sign-off)

Rough order, ~1–2 days:

1. **`/_meta/*` guard in `agentstategraph`** ([crates/agentstategraph/src/repo.rs](../crates/agentstategraph/src/repo.rs))
   - Add `allow_reserved` bool to `CommitOptions`, defaulting false.
   - Add `RepoError::ReservedPath(String)`.
   - Gate writes in `set`, `set_json`, `spec_set`, `spec_set_json`, `delete` on paths starting with `/_meta/`.
   - Update `Repository::init()` to write `/_meta/schema_version` in its initial commit with `.allow_reserved()`.
   - Tests: ordinary write rejected; allowed-reserved write accepted; fresh init stamps version.

2. **`agentstategraph-migrate` crate** (~400 lines + tests)
   - `Migration` trait, `Registry`, `check()`, `run()`, `DryRun`/`Apply` modes.
   - Version read/write helpers using `.allow_reserved()`.
   - Forward-compat guard (refuse if `db_version > binary.max_known`).
   - Exit-code constants (`EX_USAGE = 64`, `EX_DATAERR = 65`, `EX_TEMPFAIL = 75`).
   - Tests: empty registry no-op, pending listed, applied and re-applied idempotently, downgrade refused.

3. **First migration: `v0_4_0_plan_assignments`**
   - Lives in `agentstategraph-migrate/src/migrations/v0_4_0_plan_assignments.rs`.
   - Depends on `agentstategraph-tasks` for the `assign_task` call.
   - Tests per §4 (fixture `.db`, round-trip, assertions, idempotency).

4. **`agentstategraph-mcp migrate` subcommand** ([crates/agentstategraph-mcp/src/main.rs](../crates/agentstategraph-mcp/src/main.rs))
   - Flags per §5: `--db`, `--to`, `--dry-run`, `--yes`, `--ref`, `--storage`.
   - Prompt on stdin when neither `--dry-run` nor `--yes`.
   - Per-migration status lines, final version, commit IDs.
   - Non-server path; no listener.

5. **Consumer integration**
   - Call `agentstategraph_migrate::check()` at the start of the consumer's `main()` before the server binds its listener.
   - The consumer preserves its own `server/src/migrations.rs` for app-schema concerns (it owns its own `/<app>/schema_version`); the new crate handles the ASG-schema concerns.
   - Document the `ASG_MIGRATE=auto|prompt|never` env var in the README.

6. **Changelog + doc updates**
   - Workspace `CHANGELOG.md`: new crate, new `init()` semantics, new `RepoError::ReservedPath`, new `CommitOptions::allow_reserved()`.
   - The consumer's `CHANGELOG.md`: startup now runs ASG-level `check()` in addition to its own migrations.
   - `agentstategraph-migrate/README.md`: carries the doctrine block and the code example from §2.
