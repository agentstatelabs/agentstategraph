# DESIGN.md — AgentStateGraph plans

Plan-level design discussion for the AgentStateGraph (ASG) truth-store
engine — the content-addressed Merkle-DAG state store that backs
AgentStateDeveloper and other consumers via `.asd-state.db`.

Format mirrors AgentStateDeveloper/DESIGN.md: each plan is
`## Plan X — title` with **Motivation**, a **Task table** (with Wave),
**Acceptance**, **What's NOT in this plan**, and **Done when**.

These first two plans were scoped from a field DB:
`SessionDrift-ios/.asd-state.db`
(2026-08-10, 22 GB, 512,453 commits, 14,790,044 objects — full history
from the repo's first day, 2026-07-09).

---

## Plan A — Project-history enrichment (distill the commit chain into queryable metrics)

### Motivation

The ASG commit chain IS a project's history, and it is strictly richer
than `git log`: every commit is an authored, intent-tagged, timestamped
state snapshot carrying `agent_id`, `authority` (principal + scope +
delegation chain), `intent` (category + description + lifecycle), and —
by schema — `reasoning` / `confidence` / `tool_calls`. Nobody but ASG
can reconstruct this: it's the provenance of every decision an agent
made, in order.

Today that history is **latent**. Answering "how did this project
evolve?" means walking the full commit chain and dereferencing the
object DAG every time. The signal is drowned by volume. The
SessionDrift-ios field DB proves it:

- **511,210 / 512,453 commits are `Refine`** (automated ledger
  decisions, "ledger decision for sym_…"); only **1,243 are
  `Checkpoint`**. The human-meaningful milestones are a rounding error
  buried in machine churn.
- **Authorship** (uncomputed): `asd-cli-user` 492k, `Craig Brown` 18k,
  `asd-task-close` 1.8k, `codex` 176.
- **Velocity is wildly bursty**: <2.7k commits/day through 2026-07-24,
  then 35k → 114k/day 2026-07-27–08-01 (bulk ledger runs), then back
  down. Nothing surfaces this shape.
- **`reasoning` and `confidence` are populated 0 / 512,453 times, and
  `tool_calls` is empty on every commit** — the setters exist on
  `CommitBuilder` / `CommitOptions` (`with_reasoning`,
  `with_confidence`, `tool_calls`) but no production caller ever invokes
  them (`repo.rs` hardcodes `reasoning: None, confidence: None`, and
  `CommitOptions` doesn't even expose `tool_calls`). The history's
  richest fields are reserved-but-unwired.

This plan **distills** the commit chain into materialized, incremental
metric tables so history is queryable in milliseconds — and so the
distilled facts survive when Plan B prunes the raw snapshots. **You
cannot safely garbage-collect history you have not yet distilled.**
Plan A is the prerequisite for Plan B.

Wiring up the dead provenance fields themselves (reasoning, confidence,
tool_calls, authority, parent_intent) is **Plan C** — it makes *future*
commits richer; Plan A materializes whatever the commit chain captures,
old and new. A and C are independent and can proceed in parallel.

### What to materialize (the metric set)

Decided from the field distribution, not guesswork:

1. **Commit-history rollup** — per (day, namespace, agent, intent
   category): commit count, epoch/session span, first/last timestamp.
   Collapses velocity + authorship + intent-mix into one indexed table
   instead of a 512k-row scan.
2. **Milestone timeline** — every `Checkpoint` commit and every epoch
   seal / session boundary, with description and state_root: the
   human-readable spine (the 1,243 needles pulled from the 511k
   haystack).
3. **Decision history** — the `Refine` stream keyed by target symbol:
   per-symbol decision count + timeline (the churn signal Plan B must
   reason about, and the dominant volume).
4. **Authority / provenance ledger** — distinct principals, scopes, and
   delegation depth over time (schema carries `authority`; nothing
   surfaces it).
5. **Store-shape metrics** — object/commit counts, dbstat bytes per
   table, DAG fan-out, path-copy amplification (nodes created per
   commit). Both a history metric AND the input Plan B's retention
   policy keys off.

### Task table

| Task | Description | Wave |
|------|-------------|------|
| t-001 | **Metric schema + incremental extractor**: new `asg_history_*` tables in the storage layer; a walker that reads the commit chain forward from a watermark cursor (in `asd_index_meta` / a store-meta table) so it's incremental, not a full re-scan. Idempotent, resumable, bounded memory over 512k+ commits. — **LANDED**: `asg_history_meta` (cursor), `asg_history_commit_rollup` (day/namespace/agent/category), `asg_history_milestone` (Checkpoint timeline); `SqliteStorage::history_extract_batch` folds commits by monotonic `commits.rowid` in transactional batches; `Repository::extract_history`. Rollup metric (#1) + Checkpoint milestones (#2) populated; decision/provenance/store-shape metrics + epoch/session-boundary milestones are follow-on. | 1 |
| t-002 | **`asg history` command + storage API**: query the materialized tables — velocity (`--by day\|week`), intent mix, authorship, milestone timeline, per-symbol decision history. Human table + `--json` contract (smoke-tested from a non-source checkout — the same CWD/JSON-contract discipline ASD's CLAUDE.md mandates). — **LANDED**: `Repository::history_report(namespace, by, milestone_limit, refresh)` aggregates the rollup into velocity (day/week), intent mix, authorship, namespaces, totals + milestone timeline as JSON; exposed via MCP `agentstategraph_history`, HTTP `GET /history`, and FFI `explore.history` (ASG ships no CLI — these are its command surfaces; JSON contract holds). Per-symbol decision history rides the decision-history table (later task). | 1 |
| t-003 | **Store-shape + amplification report** (`asg history --store`): objects, commits, bytes/table via dbstat, avg nodes-created-per-commit, hottest-churn symbols. The evidence surface for Plan B retention thresholds. — **LANDED**: `SqliteStorage::history_store_shape` (object/commit counts, total bytes via page_count×page_size, per-table bytes via `dbstat` with graceful fallback when the build lacks it); `Repository::history_store_shape` adds path-copy amplification (objects/commit); surfaced via the `store` flag on `agentstategraph_history` / `GET /history?store=1` / `explore.history`. Hottest-churn symbols ride the deferred decision-history table. | 1 |
| t-004 | **Backfill + reconcile**: run the extractor against SessionDrift-ios (512k commits) and PulseLab (golden benchmark); record wall-clock; verify rollup counts reconcile EXACTLY against `SELECT COUNT(*)` ground truth (reconcile against real distributions, not synthetic — the ASD calibration-inversion lesson). — **LANDED**: ignored reconcile test `tests/history_backfill_field_db.rs` (env `ASG_HISTORY_BACKFILL_DB` → a writable copy; on APFS use `cp -c` for an instant CoW clone). **SessionDrift-ios field DB (2026-08-13): 512,453 commits / 14,790,044 objects / 23.82 GB backfilled in 21.4s (~24k commits/s); rollup = 89 buckets reconciling EXACTLY to COUNT(\*); path-copy amplification 28.9 objects/commit; `dbstat` present, objects table = 22.5 GB.** Re-run is a no-op (idempotent). Note: `history_store_shape`'s `dbstat` scan is O(all pages) (~160s on 22 GB) — kept behind the opt-in `store` flag; caching is a follow-on. | 2 |
| t-005 | **Retention hooks for Plan B**: each metric row records the `state_root`(s) and commit-id range it summarizes, so GC can prove "this raw snapshot's signal is already captured" before pruning. The contract seam between A and B. — **LANDED**: `asg_history_milestone.state_root` (schema + migration + extractor populates it); `Repository::history_is_distilled(commit_id)` = the "already captured" prune predicate (commit rowid ≤ extractor cursor — the cursor is the commit-id range boundary); `Repository::history_retained_state_roots()` = milestone snapshots GC must keep reachable. Directly feeds **Plan B t-003** (GC gate) and **t-006** (already-captured predicate). state_root also surfaced on the milestone timeline JSON. | 2 |

### Acceptance

On the 512k-commit field DB, "how did this project evolve — velocity,
who authored what, when the milestones landed, which symbols churned
most" is answered from the materialized tables in well under a second;
counts reconcile exactly against a full commit scan; and every metric
row names the commit range it distilled.

### What's NOT in this plan

- Pruning anything (that is Plan B; A only wires, reads, and
  materializes).
- Web/Lens visualization of the history (follow-up once tables exist;
  keep the tables UI-neutral).
- Re-deriving history from `git` — the ASG commit chain is the richer
  source of truth.

### Done when

A project's full history is a set of small, indexed, incrementally
maintained tables that answer evolution questions instantly, and each
metric row carries enough provenance for the GC to trust it.

---

## Plan B — State-store garbage collection & compaction (keep the DB trim)

### Motivation

The field DB is **22 GB on disk**, and **20.95 GB is the `objects`
table** (14,790,044 rows, avg 1,450 bytes) plus 0.64 GB of its
autoindex. `commits` is 0.49 GB; everything else combined is under
60 MB. The store is a **path-copying persistent Merkle DAG**: every
commit writes a new `state_root` and copies every node on the mutation
path up to it. 512k commits — 511k of them single-symbol `Refine`
ledger decisions — means the object count is dominated by **historical
intermediate roots and interior nodes no live ref will ever reach
again**. `freelist_count` is 0 and there is no `auto_vacuum`, so
nothing is reclaimable today without an explicit compaction pass.

There is **no GC anywhere in ASG** — no sweep, prune, vacuum, or
reachability collector in `agentstategraph-storage` or
`agentstategraph`. The store only grows, superlinearly in agent
activity (the 2026-07-27–08-01 bulk runs added ~500k commits and the
bulk of the 21 GB in five days). On a long-lived repo this is a scaling
wall — and it's the exact class of problem the ASD product exists to
catch in other people's code.

**Ordering constraint: Plan B runs after Plan A.** GC must not delete a
snapshot whose history-signal hasn't been distilled. Plan A t-006 gives
B the "already captured" predicate; B's job is to reclaim the raw bytes
safely once that predicate holds.

### Design shape

Two independent reclamation mechanisms, both needed:

1. **Mark-and-sweep** over the DAG. Roots = the `state_root` of every
   commit reachable from a live `refs` entry, plus retained
   checkpoints / epoch seals. Mark transitively; sweep unreferenced
   `objects`. Reclaims nodes orphaned by ref rewinds, abandoned
   branches, and superseded ledger churn. Must respect the epoch-seal
   invariant — but note the hard guard is currently dead in production
   (only the soft `[WARN]` path runs; see Plan C t-005). GC must enforce
   "never orphan a sealed commit" itself, and should land alongside
   Plan C engaging the strict guard.
2. **Checkpoint-and-prune retention** for commit history. The 511k
   `Refine` roots are not individually valuable once Plan A has
   distilled them. Policy (configurable): keep every commit newer than
   `--keep-recent`; keep all `Checkpoint` / epoch / session-boundary
   commits forever; for older `Refine` runs keep only periodic
   checkpoints and drop the interior roots, unreferencing their
   path-copied nodes for the sweep to reclaim. Never prune a commit
   whose signal Plan A hasn't recorded (t-006 predicate).

Then **`VACUUM`** (or incremental-vacuum) to return freed pages to the
OS — mark-sweep only unlinks rows; disk shrinks only after vacuum.

### Task table

| Task | Description | Wave |
|------|-------------|------|
| t-001 | **Reachability marker**: from `refs` + retained checkpoints, walk each `state_root`'s node closure and mark live objects. Streaming/batched to run in bounded memory over 14.8M nodes (cannot hold the mark-set naively — disk-backed or bitmap-over-rowid). Report live vs. total object counts (the reclaimable estimate). — **LANDED**: `SqliteStorage::history_gc_reachability(roots, batch)` — **disk-backed** mark set (a TEMP `gc_mark` table with an `expanded` frontier index), draining unexpanded nodes in `batch` chunks so the closure never materializes in RAM. `Repository::gc_default_roots` (live ref tips' `state_root` + retained milestone roots), `gc_reachability_from(roots)` / `gc_reachability_report()` → `{ total, live, reclaimable, reclaimable_pct, roots_walked }`. Invariant `live + reclaimable == total`; keeping only the tip leaves superseded path-copy churn reclaimable. **Perf**: the frontier drain is set-based — one JOIN fetch of ids+data per batch, then chunked multi-row `INSERT`/`IN`-clause `UPDATE` (≤400 params) — so a 14.8M-object walk is ~tens-of-thousands of statements, not ~30M per-node round-trips. | 1 |
| t-002 | **`asg gc --dry-run`**: reports what would be swept, bytes/pages reclaimable, and estimated post-vacuum size WITHOUT mutating — the safe default. Loud about anything it would drop that Plan A has NOT yet distilled (refuses; never silently skips). — **LANDED**: `Repository::gc_dry_run()` → `{ dry_run, mutates:false, objects{total,live,reclaimable,reclaimable_pct}, commits, bytes{current_total, estimated_reclaimable, estimated_post_vacuum}, roots_walked, safe, safety{undistilled_commits, reason} }`. Byte estimate = reclaimable object-fraction × objects-table bytes (t-003 store-shape). **Safety gate**: `safe=false` with a loud REFUSING reason when `history_undistilled_commit_count() > 0` (extractor behind → pruning would lose not-yet-captured history). Exposed via MCP `agentstategraph_gc_dry_run`, HTTP `GET /gc/dry-run`, FFI `gc.dry_run`. | 1 |
| t-003 | **Sweep + retention engine**: apply mark-sweep and the checkpoint-and-prune rules (`--keep-recent`, `--checkpoint-every`, keep-all-milestones); transactional, resumable, gated on the Plan A t-006 predicate and the epoch-seal invariant. `--dry-run` is default; mutation requires an explicit flag. — **LANDED**: `RetentionPolicy { keep_recent, checkpoint_every, keep_milestones }`; `Repository::gc_keep_roots(policy)` resolves the keep-set (always ref tips + sealed-epoch commits [epoch-seal invariant]; + milestones + recent/every-Kth). `SqliteStorage::history_gc_sweep(keep, batch)` marks the closure then DELETEs unreachable objects — transactional, **resumable** (materializes a `gc_dead` set, deletes in bounded batches). `Repository::gc_sweep(policy, mutate)`: `mutate=false` (default) previews; `mutate=true` refuses (`refused:true`, no delete) unless every commit is distilled (Plan A gate). Commits are kept (bloat is objects); pruned commits' state becomes unmaterializable but their signal is distilled. Surfaced: MCP `agentstategraph_gc_sweep`, HTTP `POST /gc/sweep`, FFI `gc.sweep`. Test: churn is reclaimed, current state stays fully materializable + tip closure intact, dry-run mutates nothing, gate refuses pre-distillation. | 2 |
| t-004 | **Vacuum / page reclaim**: `VACUUM` or `PRAGMA incremental_vacuum` after sweep; evaluate enabling `auto_vacuum=INCREMENTAL` going forward so freelist pages return without a full rewrite. Measure before/after on the field DB. — **LANDED**: `SqliteStorage::open` sets `PRAGMA auto_vacuum=INCREMENTAL` before tables exist, so **new** DBs return freed pages to the freelist (existing DBs adopt it on their next full VACUUM). `SqliteStorage::history_vacuum` runs a full `VACUUM` (universal — shrinks the file and converts the DB to incremental mode) reporting bytes before/after/reclaimed. `Repository::gc_vacuum()` + a `vacuum` flag on `gc_sweep` (VACUUM after a mutating sweep). Surfaces: MCP `agentstategraph_gc_vacuum` + `gc_sweep{vacuum}`, HTTP `POST /gc/vacuum` + `/gc/sweep{vacuum}`, FFI `gc.vacuum`. Unit test: sweep+vacuum never grows the file. End-to-end field before/after rides the ignored `gc_sweep_vacuum_on_field_db` test / the t-006 field test. | 2 |
| t-005 | **Integrity gates**: after GC, every live ref still derefs to a fully-present node closure (reuse the existing "object reachable from state root is missing" check in `repo.rs`), no sealed commit is orphaned, and Plan A's metrics still reconcile. Add a store-bloat dimension (objects, bytes, path-copy amplification, days-since-gc) to a health/status surface. | 2 |
| t-006 | **Field test on SessionDrift-ios**: dry-run → capture reclaimable estimate → run GC on a COPY → verify 22 GB drops substantially, all refs resolve, `asg history` output is unchanged, PulseLab (golden) round-trips identically. Record the numbers here. | 3 |
| t-007 | **Scheduling guidance / hook**: document (and optionally hook) when GC should run — after bulk ledger runs, or on a size/commit-count threshold surfaced by health. No silent auto-deletion; the user/agent triggers it knowingly. | 3 |

### Acceptance

`asg gc --dry-run` reports an accurate reclaimable estimate on the 22 GB
field DB; a real run drops the file substantially; every live ref still
resolves to a complete node closure; no sealed commit is orphaned; and
Plan A's history metrics are byte-for-byte unchanged. No commit whose
signal wasn't distilled is ever dropped.

### What's NOT in this plan

- Changing the underlying storage format (still content-addressed
  path-copying; GC works *with* it, doesn't replace it).
- Cross-repo / federated GC (per-store here).
- Automatic unattended deletion — GC is explicit and defaults to
  dry-run.

### Done when

A long-lived ASG store stops growing without bound: after a bulk agent
run, one command (dry-run first) reclaims the historical churn safely,
the disk footprint shrinks, and nothing the project needs to
remember — history, milestones, live state — is lost.

---

## Plan C — Provenance-layer wiring (make commits capture what the schema promises)

### Motivation

An ASG commit's schema promises a rich provenance model: **who**
authorized the change and through what delegation chain (`authority`),
**why** (`reasoning`, `confidence`), **what tools** did it
(`tool_calls`), and **where it sits** in the intent hierarchy
(`parent_intent`). Almost none of it is populated. An audit of the
engine (2026-08-12) found the model was **designed rich and wired
shallow** — the types, fields, and builder setters exist, but no
production caller invokes them, so every commit persists constant
defaults. On the SessionDrift-ios field DB that's 512,453 commits, each
storing `reasoning: null`, `confidence: null`, `tool_calls: []`, a
constant `Authority { principal: "default", scope: Wildcard,
delegation_chain: [] }`, and `parent_intent: null`.

This is both a **capability gap** (the provenance features ASD markets —
accountability, delegation, decision rationale — are inert at the store
layer) and a **storage tax** (constant/empty blobs on every commit,
directly feeding the Plan B bloat). Plan C wires the plumbing so
*future* commits capture real provenance. It's independent of Plan A
(which materializes whatever exists) and complementary: richer commits
make richer history.

### Findings this plan closes

Verified against the engine, 2026-08-12:

1. **`reasoning` / `confidence` / `tool_calls` never set.** Setters
   exist on `CommitBuilder` and (for reasoning/confidence) on
   `CommitOptions` via `with_reasoning` / `with_confidence`; `tool_calls`
   isn't even exposed on `CommitOptions`. Every ASD caller uses bare
   `CommitOptions::new(...)`. Populated 0 / 512,453.
2. **`CommitOptions::with_authority` is dead** (`repo.rs:183`, zero
   references anywhere). Every commit hardcodes
   `Authority::simple("default")` (`repo.rs:175`), and no MCP/HTTP/FFI/
   WASM surface exposes authority at all — so no caller *can* set it.
3. **The scoped/delegated-authority model is unreachable.**
   `Authority::for_intent` (`intent.rs:338`) has zero callers;
   `AuthScope::{Branch,Path,Custom,Intent}` and `DelegationLink` are
   never constructed anywhere; only `AuthScope::Wildcard` is ever built;
   `delegation_chain` is always `Vec::new()`.
4. **`Intent::with_parent` / `parent_intent` unwired, with a lying
   doc.** The setter is never called; `Intent::new` always sets
   `parent_intent: None`. `Repository::intent_tree` (`repo.rs:1706`) is
   documented as walking `parent_intent` but actually walks
   `commit.parents` (the commit DAG). The field is never read in
   production.
5. **`Repository::with_epoch_seal_strict` is dead in production**
   (`repo.rs:422`; only a unit test at `repo.rs:3184` sets it, no
   shipped surface exposes it). The hard `EpochSealViolated` guard never
   fires; production always takes the soft `[WARN]` path. **This one is
   a latent correctness gap Plan B's GC depends on** — GC must not
   orphan sealed-epoch commits, and the enforcement that should prevent
   it is currently unengageable.

### Task table

| Task | Description | Wave |
|------|-------------|------|
| t-001 | **Thread reasoning / confidence / tool_calls end to end**: expose `tool_calls` on `CommitOptions`; carry all three from the caller (ASD's ledger/trace/mcp commit paths, and the MCP/HTTP/FFI surfaces) through `Repo::commit` so real values persist. All optional, backward compatible (default to today's None/empty). — **ASG side landed** (MR !21: `tool_calls` on `CommitOptions` + MCP/HTTP/FFI surfaces; reasoning/confidence already flowed). **ASD reasoning/confidence landed** (agentstatedeveloper!10: ledger decisions carry body/confidence). ASD `tool_calls` wiring is deferred → **t-007**. | 1 |
| t-002 | **Wire authority**: expose authority on the commit surfaces (MCP/HTTP/FFI/CLI) and route the caller's real principal through instead of the hardcoded `Authority::simple("default")`. Decide the minimum viable capture (principal at least) vs. deferring full delegation chains to t-004. | 1 |
| t-003 | **Fix the `parent_intent` lie**: either wire `with_parent` + make `intent_tree` actually consult `parent_intent`, OR (if the commit-DAG walk is the intended behavior) delete `parent_intent` / `with_parent` and correct the doc. Do not ship a field documented to do something it doesn't. | 1 |
| t-004 | **Scoped / delegated authority** (the `AuthScope` + `DelegationLink` model): either wire `for_intent` and the non-Wildcard scopes into a real authorization path, or prune the dead variants/struct if the model is premature. Explicit decision, not drift — dead public enum variants are a maintenance tax. | 2 |
| t-005 | **Engage epoch-seal-strict**: expose `with_epoch_seal_strict` (or make strict the default) on a shipped surface so the `EpochSealViolated` hard guard is reachable in production. Coordinate with Plan B — GC relies on sealed commits not being orphanable. Add a test that the hard path fires through a real surface, not just a unit constructor. | 2 |
| t-006 | **Storage-tax check**: confirm that after wiring, commits with no provenance still serialize compactly (skip-if-none / skip-if-empty already holds for most fields — verify authority doesn't bloat the common case). Provenance should cost bytes only when actually present. | 2 |
| t-007 | **ASD `tool_calls` wiring — BLOCKED on ASG release.** Follow-on to t-001. `with_tool_calls` exists only in unreleased ASG (MR !21); AgentStateDeveloper pins `agentstategraph` to GitHub tag **v0.9.10**, which lacks it, so ASD cannot compile a `tool_calls` call yet. **When ready:** (1) ASG cuts a release containing t-001 and mirrors it to the GitHub release face; (2) bump AgentStateDeveloper's `agentstategraph*` dep tag from `v0.9.10` to that release; (3) thread `tool_calls` into `AsgLedgerStore::append_entry` (alongside the reasoning/confidence already wired in agentstatedeveloper!10) and any ASD commit caller that has real tool-call context. Backward compatible; empty by default. | 3 |

### Acceptance

A commit created through ASD (or any surface) can carry real reasoning,
confidence, tool_calls, and authority principal, and they round-trip
into the store and out through `log` / `blame` / Plan A's history
tables. `intent_tree` behaves exactly as its doc says. The
epoch-seal-strict hard guard is reachable and tested through a shipped
surface. No dead authority variant or unwired setter remains without an
explicit keep/prune decision.

### What's NOT in this plan

- Deciding ASD's UX for *supplying* reasoning/authority (that's an ASD
  product question; Plan C only guarantees the store can carry it).
- Retroactively backfilling provenance onto the 512k existing commits
  (impossible — the data was never captured; Plan A distills what
  exists).
- Full delegation-chain semantics if t-004 decides to prune rather than
  wire.

### Done when

The provenance the commit schema promises is provenance the store
actually captures: no field is defined-but-dead without a deliberate
decision, `intent_tree`'s doc matches its behavior, and the
epoch-seal safety guard Plan B depends on is live.

---

## Plan D — Postgres as a first-class backend

Plans D and E were scoped on 2026-10-05 from a review of v1.2.7 for
shared (team / enterprise) deployments.

### Motivation

Postgres is the only backend that partitions by tenant (`tenant_id` is
in every primary key). The README and `agentstategraph-storage/src/lib.rs`
present it as the backend for shared deployments, and downstream
servers build on it. It is also the least exercised
backend. Its tests skip unless `TEST_DATABASE_URL` is set, and neither CI
sets it. It was ported from SQLite before the transactions, history, GC
and search work that has landed since. The review found it broken in the
paths a shared deployment depends on.

Plan D holds Postgres to the same behavior as SQLite, proven by the same
tests.

### Findings this plan closes

Verified against v1.2.7 by reading the code. Items marked *(confirm)*
still need a run against a live database.

1. **`get_epoch` queries a column that doesn't exist.** It loads an
   epoch's commits with `ORDER BY created_at` (`postgres.rs:1201-1203`),
   but `commits` has `timestamp`, not `created_at` (`postgres.rs:100-106`).
   So `get_epoch` fails, and so do `seal_epoch` and `export_epoch`, which
   call it.
2. **Epochs lose their namespace and seal.** The `epochs` table
   (`postgres.rs:126-140`) has no `namespace`, `scope` or `seal_hash`
   column.
   - `create_epoch` doesn't write them, and `row_to_epoch` returns `None`
     for namespace and seal hash.
   - `assign_epoch_namespace` updates a column that isn't there.
   - As a result, the per-namespace epoch binding from v1.1.x and seal
     enforcement don't work on Postgres.
3. **The server binary drops its async runtime before using Postgres.**
   *Confirmed in t-002:* the binary crashes at startup with `--storage
   postgres` in both HTTP and MCP modes ("there is no reactor running"), so
   it has never worked with Postgres.
   - `main.rs:413-422` builds a `tokio` runtime inside the storage match
     arm, connects, and drops the runtime when the arm ends.
   - `repo.init()` (`main.rs:431`) then calls `PostgresStorage::block_on`,
     which runs `block_in_place(|| Handle::current().block_on(..))`
     (`postgres.rs:247-252`) with no runtime present.
   - MCP stdio mode runs on a `current_thread` runtime, where
     `block_in_place` panics.
   - Embedders that connect inside their own `#[tokio::main]` don't hit
     this, which is likely why it went unnoticed.
4. **No transactions.** `postgres.rs` never calls `BEGIN` or
   `transaction()`.
   - Multi-statement operations run as separate autocommits on separate
     pooled connections. Examples: `delete_namespace`; and a commit plus
     its epoch/session association and `COUNT(*)` recount.
   - `batch_put_objects` isn't overridden. The merge path assumes one
     atomic batch but gets N independent inserts.
5. **Check-then-act races.** Epoch seal and session end read the status
   and then write it, with no lock and no conditional update. Reminder
   `update_if_unchanged` falls back to the trait's non-atomic default.
6. **Missing hooks.** SQLite implements the history-rollup, GC, leaf-index
   (search) and `list_commits_dag` hooks. Postgres overrides none of them.
   - `/api/history` and GC silently do nothing.
   - Search falls back to a full tree walk.
   - Every history walk costs one round trip per commit.
7. **No TLS.** Connections use `NoTls` (`postgres.rs:15, 67`).
8. **No migration versioning.** The schema DDL (`CREATE ... IF NOT
   EXISTS` / `ALTER ... ADD COLUMN IF NOT EXISTS`) runs on every connect.
   Concurrent startups race, and nothing records which schema version a
   database is at.
9. **Untested.** `tests/postgres_tenant_isolation.rs` and
   `tests/postgres_taint.rs` skip without `TEST_DATABASE_URL`, and neither
   GitLab nor GitHub CI provides a database. There is no Postgres
   concurrency test.

**Found by the conformance suite (t-001, 2026-10-06), confirmed by running
it against Postgres 16:**

10. **No epoch can be created on Postgres.** `create_epoch` binds
    `commit_count` as an `i64` into an `INTEGER` column, so every call fails
    with "error serializing parameter 10". Finding 1 is never even reached.
    Epochs have not worked on Postgres at all.
11. **The in-memory backend drops an epoch's seal hash.**
    `MemoryStorage::seal_epoch` ignores its `seal_hash` argument.
12. **Reminder compare-and-swap isn't atomic in the in-memory backend
    either.** `MemoryStorage` doesn't override `update_if_unchanged` (the
    trait docs mean the reminders crate's `MemoryReminderStore`). Racing
    updates from one snapshot let several writers win: 3 of 6 in memory,
    6 of 6 on Postgres.
13. **Finding 8 is a real deadlock, not just a theoretical race.** Two
    connects at once deadlocked in Postgres (`deadlock detected`). That made
    `postgres_taint.rs` flaky as soon as CI set `TEST_DATABASE_URL`; its
    connects are now serialized until t-006 lands.

14. **Racing seals all won on SQLite** (found while fixing t-003). Separate
    processes sealing one epoch each passed a status check and then wrote,
    so all 6 of 6 racers "won", each overwriting the previous seal's commits
    and digest. Fixed in t-003 with a single conditional UPDATE on SQLite and
    Postgres. This also covers the seal half of t-005.

15. **More races and a startup deadlock** (found while fixing t-004 and
    t-005). On SQLite, separate processes could all end one session. In
    memory, linking a commit twice listed it twice in its epoch or session.
    Once linking ran in a transaction, it deadlocked with the schema DDL that
    every connect re-ran, because `ALTER TABLE ... IF NOT EXISTS` takes an
    exclusive lock even when it changes nothing. Fixed in t-004/t-005; the
    DDL now runs once per schema version (t-006).

16. **Search could keep an overwritten value** (found while doing t-008, on
    SQLite; Postgres had the same exposure). When writers raced on one
    branch, their search-index updates could apply out of order, and the
    index then held the older value until that path was written again:
    about 1 run in 200 of a 4-writer test. Fixed in t-008. Each indexed set
    records the state root it reflects, an update applies only from the
    tree it was computed against, and a search catches the set up first.

17. **A SQLite sweep can take a write that's in flight** (found while doing
    t-009). Every write stores its new tree, then its commit, then moves
    the ref. A sweep that takes the write lock after the first step finds
    the tree unreferenced and deletes it; the write then commits, and the
    ref lands on a tree with objects missing. The writer lock doesn't help,
    because the objects were written before the sweep took it.
    `sweep_spares_a_write_in_flight` in `backend_conformance.rs` shows it
    and lists it as a known SQLite failure. Postgres doesn't have it (t-009
    spares recent writes instead of locking writers out). **Fix in review
    (MR !65, stacked on !64):** SQLite now does the same. Each object has a
    write time, a sweep spares unreachable objects written within a grace
    period (10 minutes by default), and storing an object again refreshes
    its time. Existing objects count as old, so existing stores sweep as
    before.

What passed on all three backends: objects, commits and DAG walks, refs and
namespaces (including `create_ref` and `cas_ref` races across separate
handles), sessions, taints, and repository write/branch/merge/history. That
includes the v1.2.5 lost-update scenario, with separate repositories writing
concurrently.

### Task table

| Task | Description | Wave |
|------|-------------|------|
| t-001 | **Backend conformance suite, with Postgres in CI.** Turn the storage behavior tests into one suite that runs against memory, SQLite and Postgres, so parity is tested instead of checked by eye. Add a Postgres service to `.gitlab-ci.yml`, whose merge-request pipeline is the pre-merge gate, and to GitHub Actions `ci.yml`. Add `scripts/test-postgres.sh` (Docker) for local runs. Include a two-process concurrent-writer test modeled on the v1.2.5 lost-update test. Expect this suite to surface failures beyond the list above; that's its job. — **In review (MR !58).** The local runner falls back to a local `initdb` when Docker isn't running. It found findings 10–13. | 1 |
| t-002 | **Make `PostgresStorage` independent of the caller's runtime, and fix the server's wiring.** — **In review (MR !60, stacked on !59 and !58).** It also serializes schema setup with `pg_advisory_xact_lock`, the lock half of t-006, because the binary smoke tests start two servers at once. Products embed ASG and call storage from both plain sync code and tokio servers. So `PostgresStorage` should own its own runtime or worker, instead of `block_in_place` on whatever `Handle::current()` happens to be. That fixes the binary's dropped-runtime crash and the `current_thread` MCP panic as one class of bug. Add a smoke test that starts the binary in HTTP and in MCP mode against Postgres and does a set → get → log round trip. Add an embedding test that calls the storage from inside a `#[tokio::main]` host and from plain sync code. | 1 |
| t-003 | **Fix epochs.** — **In review (MR !59, stacked on !58).** Bind `commit_count` with the column's type (finding 10). Add `namespace`, `scope` and `seal_hash` columns to `epochs`, and write and read them. Make `MemoryStorage::seal_epoch` keep `seal_hash` (finding 11). Fix `get_epoch` to order by `timestamp`. If t-006 is ready, land this as a migration; otherwise use `ADD COLUMN IF NOT EXISTS` and convert it later. Add conformance tests for epoch namespace binding, seal and export. | 1 |
| t-004 | **Transactions where atomicity is assumed.** Add a transaction helper. Implement `batch_put_objects` as one multi-row `INSERT ... ON CONFLICT DO NOTHING` inside a transaction, which also removes N round trips. Make `delete_namespace` and commit association atomic. Replace the per-commit `COUNT(*)` recount with an increment. — **In review (MR !62, stacked on !61).** `delete_namespace` and commit linking each run in one transaction. Objects are written 1,000 per `INSERT` and read in one query. | 2 |
| t-005 | **Close the check-then-act races.** Seal is done in t-003 (finding 14). Do session end as a conditional `UPDATE ... WHERE status = 'Active'`, or `SELECT ... FOR UPDATE` inside a transaction. Make reminder `update_if_unchanged` atomic on Postgres and in `MemoryStorage` (finding 12). — **In review (MR !62, with t-004).** Session end is a conditional `UPDATE` on Postgres and SQLite. Reminder CAS locks the row on Postgres, and memory uses its reminder store's CAS. Linking a commit holds the epoch or session row, so it can't join one being sealed or ended. The conformance suite has no known failures left. | 2 |
| t-006 | **Versioned migrations.** Add a `schema_migrations` table and ordered migrations. (The advisory lock that makes concurrent startups safe landed with t-002.) Existing databases are adopted as version 1 with no data changes. — **Mostly in review (MR !62).** The database records its schema version in `asg_schema`, and the idempotent DDL runs only when that is behind the build, under the advisory lock, with a retry on deadlock. Existing databases are brought up to version 1 on their first connect. A test upgrades an older database once and shows an up-to-date one isn't touched. Still open: ordered migrations, which the first change that can't be written as `IF NOT EXISTS` will need. | 2 |
| t-007 | **TLS.** Use a rustls connector and honor `sslmode` from the connection URL. Document `require` as the expected setting for non-local hosts. — **In review (MR !61, stacked on !60).** It's behind an opt-in `postgres-tls` feature, per the network-crate guard rail; release binaries and the Docker image enable it. TLS is always verified (`require`, `verify-ca` and `verify-full` all check the chain and host name), with `sslrootcert` / `PGSSLROOTCERT` for provider CAs. Tests run against a local server that refuses plaintext. | 2 |
| t-008 | **History and search parity.** Override `list_commits_dag` with a recursive CTE or a `commit_parents` table, so a walk is one query instead of N. Add a Postgres leaf index for search using `pg_trgm`. Add history-rollup tables equivalent to SQLite's. — **In review (MR !63, stacked on !62).** Walks follow parent ids in the commit JSON through a hash index, so commits written by older binaries are walkable too; 200,000 commits walk in about a second. The history extractor reads commits in transaction-id order, and only from transactions older than every one still running, so a commit that becomes visible late can't be skipped. Search uses `pg_trgm` when it can be installed and a scan otherwise. It also fixed finding 16 on every indexed backend. Postgres now needs version 13+. `history_store_shape` moves to t-009 with the rest of the GC evidence. | 3 |
| t-009 | **GC on Postgres.** Port the SQLite design (Plan B) and run it through the t-001 suite: mark and sweep scoped to `tenant_id`, writer exclusion through an advisory lock, and pinned speculation roots. — **In review (MR !64, stacked on !63).** It doesn't lock writers out. A sweep spares objects written within a grace period (10 minutes by default) or since it started, and storing an object again refreshes its write time. That keeps a write in flight safe, which SQLite's lock doesn't (finding 17), and writers never wait. The advisory lock allows one sweep per tenant at a time. Every statement is short, so a sweep doesn't hold back history extraction for other tenants. Also includes `history_store_shape`, moved from t-008. | 3 |

### Acceptance

- The conformance suite passes identically on memory, SQLite and Postgres in CI.
- `agentstategraph-mcp --storage postgres` starts and serves both HTTP and MCP.
- Two server processes sharing one database finish the concurrent-writer test with zero lost updates.
- Epochs bind to a namespace, seal and export on Postgres.
- Connections can require TLS.
- Startup migrations are versioned and safe to run from several processes at once.

### What's NOT in this plan

- Serving more than one tenant per process, and Postgres Row-Level
  Security. Tenant routing is a separate plan, not yet scoped.
- Making the `Storage` trait async. One `block_in_place` per call caps
  throughput, but changing the trait touches every backend and binding.
  Measure after t-008 first.
- Backup, restore and replication. Operators get these from Postgres.
- IndexedDB parity.

### Done when

Postgres can be recommended for a shared deployment without caveats. It
behaves the same as SQLite, and the same tests prove it on every merge.

---

## Plan E — Server hardening (safe defaults for `agentstategraph-mcp`)

### Motivation

`agentstategraph-mcp` began as a local tool: one user, a loopback
address, trusted callers. It is now also what the Docker image and the
README's HTTP quick start ship, and the first thing anyone evaluating ASG
for a team will run. Its defaults, and a handful of bugs, still reflect
the local-only origin.

Plan E makes the server safe by default and accurate about what it
enforces, without changing the local developer experience.

### Findings this plan closes

Verified against v1.2.7 by reading the code. Items marked *(confirm)*
still need a test.

1. **CORS allows any origin.** `http.rs:118-122` allows any origin,
   method and header, while auth is off by default. Loopback binds have no
   `Host` header check.
2. **API keys are stored in plaintext.**
   - `--keys-file` holds the raw keys. `save_keys` in `auth.rs` writes it
     with a non-atomic `fs::write` at default permissions.
   - Load errors are ignored. If the file fails to parse and
     `--initial-admin-key` is set, the file is rewritten with only the
     bootstrap key, and every other key is lost.
3. **Revoke and rotate match keys by prefix** and take the first hash-map
   hit. `list_keys` shows only 8 characters, so prefixes are ambiguous.
4. **The ASD proxy routes skip auth and rate limiting.** `/api/code/*` is
   merged after those layers (around `main.rs:510-514`).
5. **Per-key scoping is stored but not enforced.**
   - Keys carry `tenant_id` and `namespace_id`.
   - `get_repo` discards the tenant and returns the default repo for every
     key (`auth.rs:405-410`).
   - No HTTP handler reads `namespace_id`. Handlers take `State(repo)`, not
     the repo the auth middleware injects.
6. **The Docker quick start can't connect.** The image's CMD has no
   `--bind` (`Dockerfile:40`) and the default bind is `127.0.0.1`
   (`main.rs:50-61`), so the published port reaches nothing. The compose
   healthcheck calls `curl`, which the runtime image doesn't install.
7. **Warnings never print.** The code calls `tracing::warn!` (for example,
   the warning about a self-asserted agent id, around `http.rs:804-810`),
   but no tracing subscriber is installed anywhere. There is no request
   logging and no metrics.
8. **`/api/health` isn't a readiness probe.** With auth on, it builds a
   throwaway in-memory repo instead of touching the real store.
9. **No request bounds.**
   - Only axum's default body limit applies; there are no timeout or
     concurrency layers.
   - Rate limiting is per peer IP, so behind a load balancer every client
     shares one bucket.
10. **Namespaced speculations are unreachable** (confirmed by a test in t-005).
    `agentstategraph_speculate` with a `namespace` creates the speculation
    on a temporary forked repo (around `server/mod.rs:1458-1463`).
    `spec_modify` and `commit_spec` look on `self.repo` and don't find it.
11. **MCP reminders don't survive a restart.** The MCP server uses
    `MemoryReminderStore` (`server/mod.rs:1021-1024`), even though
    `Repository` implements a durable `ReminderStore`.
12. **The docs claim more than ships.**
    - README: "Multi-tenant & multi-agent by design", and Postgres
      "(multi-tenant)".
    - `storage/src/lib.rs:6`: Postgres "for SaaS and enterprise".
    - Site compare page: governance "enforced at commit time". Taint gates
      are enforced at commit time; policy is advisory.
    - `docs/TAINT_GUIDE.md:188` still calls Postgres taint storage a stub,
      but `postgres.rs:1680` implements it.

### Task table

| Task | Description | Wave |
|------|-------------|------|
| t-001 | **CORS allow-list and Host validation.** Allow only loopback origins by default, plus the hosted explorer if it needs access. Make the list configurable by flag or env var. On loopback binds, reject unexpected `Host` headers. Record the behavior change in CHANGELOG. — **In review (MR !66, based on main).** Pages from loopback origins are allowed by default; `--cors-origin` (or `ASG_CORS_ORIGINS`) adds others, and `*` allows any. On a loopback bind, a request for any other `Host` gets 403; `--allowed-host` (or `ASG_ALLOWED_HOSTS`) adds names for a reverse proxy on the same machine. The policy wraps the whole app, the ASD proxy routes included. The hosted explorer uses demo data and makes no API calls, so it needs no entry. | 1 |
| t-002 | **Key storage.** Store SHA-256 hashes instead of keys. Write the file atomically (temp file + rename) with 0600 permissions. Fail loudly on a parse error. Revoke and rotate by key id, not prefix. Migrate existing plaintext files when they're loaded. — **In review (MR !67, stacked on !66).** The file now holds SHA-256 hashes, stable ids and previews. Changes take effect only once saved, so a failed save is a 500 and nothing changes. An unreadable file stops the server and is left alone; this was finding 2's data loss. Revoke and rotate take `key_id` or the full key, and `key_prefix` gets a 400. | 1 |
| t-003 | **Put the proxy routes behind auth and rate limiting.** — **In review (MR !68, stacked on !67).** The proxy is built with `http::build_router_with`, which puts extra routes behind the API's auth and inside its rate limit. The proxy also refuses paths with `.` or `..` segments or backslashes; URL parsing collapsed those and reached ASD outside `/api/v1`. It forwards no headers, so the caller's key never reaches ASD. CI runs its test with the `providers` feature. | 1 |
| t-004 | **Docker.** Set `ENV ASG_BIND=0.0.0.0` in the image; unlike a CMD flag, it survives CMD overrides such as the README's `--http`. Make the healthcheck work, either with an `agentstategraph-mcp healthcheck` subcommand or by installing curl. Note in the README and compose file that a container listening on all interfaces should run with `--auth`. Test the README commands exactly as written. — **In review (MR !69, stacked on !68).** The image sets `ASG_BIND=0.0.0.0` and `ASG_PORT=3001` as environment variables. A new `agentstategraph-mcp healthcheck` subcommand needs no curl, and compose uses it. The README's `docker run` and the compose file publish on `127.0.0.1`, and the README shows `--auth` for serving other machines. The README commands were run as written against an image built from the branch, with only the host port changed. | 1 |
| t-005 | **Fix namespaced speculations and make MCP reminders durable.** Keep a speculation on the long-lived repo, or key forks by namespace and reuse them. Have the MCP server use the repo's `ReminderStore`. — **In review (MR !70, stacked on !69).** Both findings were confirmed by tests that failed first. The server keeps the namespace fork that holds each speculation until it is committed or discarded, and every speculation tool finds the speculation where it lives. Reminders are saved in the store. | 1 |
| t-006 | **Enforce per-key scope, or stop carrying it.** HTTP handlers use the repo the auth middleware injects, bound to the key's `namespace_id`. A namespace-bound key may use branch names but not raw commit ids or prefixes, because those resolve across namespaces today (`repo.rs:3308`). Until per-request tenant routing exists, reject a key whose `tenant_id` doesn't match the process's tenant instead of silently ignoring it. — **In review (MR !71, stacked on !70).** A non-admin key for another tenant gets 403, while admin keys serve the whole process. A namespace-bound key works in a fork of its namespace made with the new `Repository::with_branch_refs_only`, so commit ids and prefixes get 403. GC and history refuse it, since they cover the whole store. Handlers use the injected repository, and the admin API can create namespace-bound keys. | 2 |
| t-007 | **Observability baseline.** Install `tracing-subscriber` with an env filter. Add request logging with `tower-http`'s `TraceLayer`; log the key id, never the key. Split health into liveness (`/api/health`) and readiness (`/api/ready`, which touches storage). Add an optional metrics endpoint behind a flag. — **In review (MR !72, stacked on !71).** Logs go to stderr, filtered by `ASG_LOG`, with JSON available. Each request is logged with its key id, never the key, and the old key-prefix logging is gone. `/api/health` is liveness; `/api/ready` checks the store and returns 503 if it doesn't answer; the container health check uses `/api/ready`. `--metrics` serves Prometheus `/metrics`, admin-only with auth, labelled by route template. | 2 |
| t-008 | **Request bounds.** Set an explicit body limit and add timeout and concurrency layers. Rate-limit by API key when auth is on, and by IP otherwise. — **In review (MR !73, stacked on !72).** Bodies over the limit get 413 (default 4 MiB). Requests past the timeout get 503 (default 30s). Past the concurrency cap (default 256) requests get 503 with Retry-After instead of queueing, though `/api/health` is never refused. The rate limit buckets requests by API key, and requests without a valid key share their IP's bucket. Handlers now run storage calls on the blocking pool; before, a few slow calls could freeze the whole server, health checks included, and no timeout could cut them off. | 2 |
| t-009 | **Docs accuracy pass.** After Plans D and E, align the README, `storage/src/lib.rs`, the site pages and `TAINT_GUIDE.md` with what ships. Describe namespaces as a branch-level partition, not an isolation boundary. Describe policy as advisory except where a specific gate is named. — **In review (MR !74, stacked on !73; merge after both the Plan D and Plan E stacks).** The Namespaces guide is rewritten: what a namespace doesn't separate (shared commits, commit ids, the MCP server trusting its client, store-wide operations), how a namespace-bound key confines a client, and tenants. Cross-namespace merge is documented as always refused. Policy is documented as advisory, with MCP `commit_spec` as the built-in gate. The GC and history MCP tools are documented, and tool counts are corrected to 77. The `create_namespace` and `cross_namespace_merge` tool descriptions drop the same claims. It also surfaced that the `policy_evaluate` tools turn NoPolicyMatch into a deny by default while `commit_spec` promoted on it. MR !75 (stacked on !74) resolves that: `commit_spec` applies the fail-safe to changes carrying tokens. Changes without tokens are exempt, because no policy can match them, and `--policy-fail-safe` sets the fail-safe from the CLI. | 3 |

### Acceptance

- A fresh install, local or Docker, accepts browser requests only from allowed origins.
- No plaintext keys are kept on disk.
- Every route sits behind the same auth and rate-limit layers.
- The README's Docker commands work as written.
- A namespace-bound key can't read or write outside its namespace over HTTP.
- Server warnings and requests appear in the logs.
- Namespaced speculations and MCP reminders last as long as they're meant to.
- Public docs describe isolation and enforcement as they actually behave.

### What's NOT in this plan

- New authentication schemes (OIDC, SAML, JWT), roles, or per-request
  tenant routing.
- Remote MCP over HTTP. MCP is stdio-only today.
- Binding commit identity to the authenticated principal. That is Plan C
  t-002 / t-004 territory.
- Namespace-scoped commit resolution for every caller. t-006 only fences
  namespace-bound keys. The general fix needs a decision on how commits map
  to namespaces.
- Making the active session, epoch and namespace per caller instead of
  per process (`repo.rs:1849`). It isn't reachable over HTTP today, but it
  blocks serving many callers from one process.

### Done when

Running the server for more than one person is safe by default, and the
docs promise no isolation or enforcement that the code doesn't provide.

---

## Plan F — Embedding API for multi-user hosts

Scoped on 2026-10-05.

### Motivation

Products embed ASG as a library, and some of them now serve many users,
agents and stores from one process: team servers, and Plan G's sync hub.
The library still assumes one trusted user per `Repository`:

- **The active session, epoch and namespace are process-wide.** They're
  fields on the shared `Repository` (`repo.rs:151-173`). One caller's
  `set_active_session` changes the effective namespace for every other
  caller (`repo.rs:1849`, `:1883`).
- **Provenance is whatever the caller passes.** `CommitOptions` carries
  `agent_id` and `authority`, but nothing ties them to an identity the
  host has verified. The delegation model (`AuthScope`, `DelegationLink`)
  is still unwired; that's Plan C t-004.
- **Guards are applied method by method.** The `/_meta` write and read
  guards and the taint check are each called from specific methods, so
  coverage depends on every method remembering to call them. Policy is
  advisory (`docs/POLICY_GUIDE.md`), and reads are never authorized.
- **One storage instance is one store.** Postgres opens a pool of up to
  32 connections per `connect_tenant`, so a process serving N stores
  needs N pools.
- **Namespaces don't isolate reads.** Commit ids and prefixes resolve
  across every namespace in the store (`repo.rs:3308`).

**The principle: ASG doesn't authenticate; the host does.** ASG's job is
to carry the identity the host has verified, enforce access the same way
on every operation, record what happened, and make it hard for host
code to skip any of that.

### Design shape

```rust
// The host builds this from what it has verified.
pub struct CallContext {
    pub namespace: Namespace,
    pub principal: Principal,              // who is authorized (a person or service)
    pub agent_id: AgentId,                 // who is acting
    pub delegation: Vec<DelegationLink>,   // principal → … → agent
    pub session: Option<SessionId>,
    pub epoch: Option<String>,
    pub request_id: Option<String>,        // correlates audit events
}

let repo = stores.open(&store_id)?;      // F t-003: cheap, shares one pool
let scoped = repo.scoped(ctx);           // F t-001: per-call handle
scoped.set("main", "/x", &value, intent)?;   // provenance from ctx; authorized; audited
```

- **`ScopedRepo`** exposes the `Repository` operations. Every one of
  them:
  - fills provenance from the context (principal, agent, delegation,
    session, epoch);
  - calls the `Authorizer`;
  - emits to the `AuditSink`.
- **`Authorizer::check(ctx, action, target) -> Decision`.**
  - `action` is read, write, branch, merge, delete-ref, epoch,
    taint/reminder or admin (GC, namespaces).
  - `target` is namespace, ref and optional path.
  - `Decision` is `Allow`, `Deny(reason)`, or `AllowExcept(path
    prefixes)` so tree reads can be redacted rather than refused.
  - The built-in reserved-path rules (`/_meta`, `/_meta/_secret`) become
    the first authorizer rules, applied at one choke point.
- **`AuditSink::record(event)`** receives every decision and every
  mutation (context, action, target, decision, resulting commit, time).
- **Enforcement mode.** With an `Authorizer` configured, unscoped
  `Repository` methods return `ContextRequired`; only `scoped()` works.
  With no `Authorizer`, unscoped methods behave exactly as today, so
  local single-user use and the language bindings don't change.

### Task table

| Task | Description | Wave |
|------|-------------|------|
| t-001 | **`CallContext` and `ScopedRepo`.** Add the per-call handle. Move the active session, epoch and namespace from `Repository` fields into the context. The existing setters stay as the default context for unscoped single-user use. Build `CommitOptions` from the context, so every commit records the host-verified principal, the acting agent and the delegation chain; this resolves Plan C t-004 by wiring delegation. Scope speculation handles to the principal that created them. Key them by namespace, so a namespaced speculation is reachable (the bug class in Plan E t-005). | 1 |
| t-002 | **`Authorizer` and `AuditSink`, behind one choke point.** Define both traits. The defaults allow everything and record nothing. Route every `ScopedRepo` operation through a single internal enforcement path, and re-express the reserved-path guards as built-in rules there. Add the enforcement mode (`ContextRequired`). **Coverage test:** with a deny-all authorizer, every public operation must fail, enumerated so that a new method without a check fails the test. Decide whether an audit failure fails the operation (best-effort vs. required) and make it configurable. | 1 |
| t-003 | **Several stores per process.** Add a `StoreProvider` trait, `open(store_id) -> Arc<Repository>`, with two implementations. **Postgres:** one shared pool; each store is a cheap view keyed by the existing `tenant_id` column; each transaction sets `SET LOCAL app.store_id`; Row-Level Security policies on every table, under an app role without `BYPASSRLS`, so a query that forgets the filter returns nothing. **SQLite:** one file per store under a directory. Bound per-store in-memory state (watchers, speculations) with eviction. Builds on Plan D t-004 (transactions). Plan G's reference hub uses it. | 1 |
| t-004 | **Policy-backed `Authorizer`.** An `Authorizer` implementation backed by the policy crate, with Cedar evaluated in-process through the `cedar-policy` crate instead of the `cedar` CLI (today one subprocess per evaluation, `agentstategraph-policy-cedar/src/lib.rs:169`). Pass real principal attributes and groups from the context instead of an empty `Agent::"<id>"`. Make `/policies` a protected path that only policy APIs can write. | 2 |
| t-005 | **Namespace read isolation.** Commit-id and prefix resolution only sees commits reachable from the context's namespace, through a commit → namespace index maintained on ref moves; measure it against a reachability walk first. Filter sessions, history rollups and control-ref records (Plan G t-001) by namespace. Make cross-namespace reads an explicit authorizer action. | 2 |
| t-006 | **Host-friendly performance.** Add an async facade (`AsyncRepository`) that runs operations on a blocking pool, so tokio hosts don't stall reactor threads. Plan D t-002 already frees storage from the caller's runtime. Bound the per-write epoch-seal check: index sealed commits and check only the commits a ref move would orphan, instead of scanning every epoch and walking the reachable graph (`repo.rs:3085-3129`). Add generation numbers to the commit graph so merge-base and bounded walks stop early. | 2 |
| t-007 | **Context through the surfaces.** The MCP server builds a context per call (namespace from the argument, not `fork_namespace`), and so does the HTTP server (principal from the authenticated key, Plan E t-006). Expose `scoped` in the FFI and language bindings once the Rust API settles. ASG's own server stays a single-store reference server, but it uses the same API. | 2 |
| t-008 | **Store lifecycle.** Export and import a whole store (reusing Plan G's bundle format); purge a store (delete every row for its id, which is clean because dedup is per store); history retention with sealed epochs exempt. Erasing *part* of a store (one person's data) conflicts with content-addressed, sealed history. Choose between crypto-shredding with per-subject keys and redaction commits plus GC before building it. | 3 |
| t-009 | **Rolling-upgrade schema contract.** Migrations follow expand-then-contract, so two versions of an embedder can share a database during a deploy. At startup, refuse a schema newer than the binary understands. Extends Plan D t-006. | 3 |

### Acceptance

- With an `Authorizer` configured, every public operation goes through
  it. The deny-all coverage test proves this, and unscoped calls fail
  with `ContextRequired`.
- Two callers in one process, in different namespaces and sessions,
  don't affect each other's reads, writes or speculations.
- Commits record the host-verified principal, the acting agent and the
  delegation chain from the context.
- One process serves many stores over one Postgres pool. A raw query run
  under the app role without the store setting returns no rows.
- A context bound to one namespace can't read another namespace's
  commits by id or prefix.
- Local single-user use (no authorizer, unscoped calls) behaves exactly
  as before, and the bindings keep working unchanged.
- A store exports, imports and purges cleanly.

### What's NOT in this plan

- **Authentication** (API keys, OIDC, SAML, device login), and role
  definitions or role management. Hosts own these and express them
  through an `Authorizer`.
- **Accounts, billing, or mapping accounts to stores.**
- **Remote MCP transport.** That belongs to the host.
- **Sync.** That's Plan G. Plan F's store provider and context are what
  a hub needs to serve many stores safely.

### Done when

An embedder can serve many users, agents and stores from one process by
passing a verified context, and ASG enforces and records it the same
way on every operation, with no per-method gaps and no change for local
single-user use.

---

## Plan G — Sync: push and pull between ASG stores

Scoped on 2026-10-05. Plan G closes RFC open question 3, "Remote sync
protocol" (`spec/AGENTSTATEGRAPH-RFC.md:2973`).

### Motivation

ASG stores are local today: one SQLite file per app or repo. Products
built on ASG are local-first, and their users want the same state on
every machine and shared with their team. ASD wants judgment (ledger,
conclusions) to follow a developer and their teammates. CTXone wants
memory to follow its user. ASD currently shares judgment by
committing a JSONL sidecar to git. That sidecar is a lossy copy with a
known merge hazard on stale branches.

ASG's history is content-addressed and branchable, which is git's model
applied to structured state. So sync can work the way git does: push and
pull per ref, fast-forward on the receiving side, merge on the client.
Plan G builds that once, in the library, so every embedder gets it. It
also ships a basic single-account **reference hub** so anyone can
self-host one.

### Decisions this plan builds on

1. **What syncs: anything hard or impossible to reproduce.** Regenerable
   data (code indexes, call graphs, search tables, history rollups)
   stays local and is rebuilt on each machine. Each embedder keeps
   irreplaceable data on refs it syncs, and regenerable data on refs it
   doesn't. The sync unit is a ref, and a ref ships its whole tree.
2. **Side tables move into a control ref.**
   - Today, taints, reminders and epochs live in storage tables outside
     the commit graph. A taint's commit is only a one-line marker; the
     record itself is in the table (`taint.rs:58`).
   - Which commits belong to which epoch is a side column on `commits`
     (`epoch_id`, `session_id`), not part of the commit.
   - These move into a reserved per-store **control ref** as ordinary
     tree state. The tables become local indexes rebuilt from it, the
     same way the leaf index is.
   - Sync then moves exactly one kind of thing, commit history. ASG's
     merge handles edits made on two machines, and every taint, reminder
     and epoch change gets full provenance.
   - Policies (`/policies`) and plans (`/plans`) already live in the
     tree this way.
   - ASG sessions (`SessionStore`) stay local in v1; no current embedder
     uses them.
3. **Secrets never sync.** A push fails if the ref's tree contains
   `/_meta/_secret`, and the error names the path. Secrets belong in a
   keychain, not in synced state.
4. **The receiving side never merges.** A push is a compare-and-swap
   from the client's expected old head to its new head. If the remote
   has moved, the client fetches, merges locally and pushes again. This
   keeps a hub simple and leaves conflicts with the party that
   understands them.
5. **The receiver never trusts client hashes.** Every commit and object
   id is recomputed (BLAKE3) on receipt; one mismatch rejects the whole
   bundle. Otherwise a client could plant content under another
   object's id.

### Design shape

- **Remote-tracking refs.** A fetch records the remote's heads locally,
  as `remotes/<remote>/<ref>`. A later push can then compute exactly
  what the remote lacks without asking per object.
- **Missing-set computation.** Walk the new commits back to the known
  remote head, and diff each new state tree against what the remote
  already has. Unchanged subtrees are skipped by hash. This is git's
  "thin pack" idea.
- **Bundle.** A versioned header, then length-prefixed commits and
  objects, compressed with zstd. Bundles have explicit size limits.
- **Shallow history.** A first push can start from a chosen commit; the
  boundary is recorded, so a large store can start syncing without
  uploading its whole past.
- **`Remote` trait.** `list_refs`, `fetch(wants, haves)`,
  `push(ref, expected_old, new, bundle)`. There's an in-process
  implementation for tests and an HTTP implementation for real use.
- **HTTP endpoints:** `GET /sync/v1/{store}/refs`,
  `POST /sync/v1/{store}/fetch`, `POST /sync/v1/{store}/push`.
- **Push log.** The receiving side appends a record for every accepted
  push: authenticated key, ref, old → new, time. Agent ids inside
  commits are claims made on the client; the push log is the receiver's
  own record of who delivered what.

### Task table

| Task | Description | Wave |
|------|-------------|------|
| t-001 | **Control ref.** Add a reserved per-store ref holding taints, reminders and epochs (with each epoch's commit membership) as tree state at fixed paths. Rebuild the tables from it on open and after every fetch, and keep them current on local writes so the per-write taint check stays index-backed. Writes to the control ref go only through the taint, reminder and epoch APIs, guarded like `/_meta`. Define merge rules per record type: a resolved taint stays resolved; reminder status changes are ordered, and otherwise the newest edit wins; a sealed epoch is immutable. Migrate existing rows on open, idempotently. Settle the ref's name. | 1 |
| t-002 | **Bundle format and verification.** Serialize and deserialize commits and objects with a versioned header, length prefixes and zstd. Recompute every id on read and reject the whole bundle on any mismatch. Enforce size limits. Library only, no network. | 1 |
| t-003 | **Missing-set computation.** Given a local head and the last known remote head (or none), produce the commits and objects the remote lacks by walking commits and diffing trees. Support a shallow boundary. Test against `list_commits_dag`, including merge commits and second parents. | 1 |
| t-004 | **`Remote` trait and library push / fetch / pull.** In-process remote for tests. Remote-tracking refs. Fetch applies a verified bundle and moves `remotes/<remote>/<ref>`. Push is fast-forward-only, a CAS on the expected old head; on rejection it returns a clear "fetch and merge" error. `pull` = fetch + `merge`, with the caller supplying `CommitOptions`. | 2 |
| t-005 | **Push guards.** Refuse a push when the ref's tree contains `/_meta/_secret`. Refuse refs the store marks local-only. Both errors name what was refused. | 2 |
| t-006 | **HTTP transport and reference hub.** HTTP `Remote` client. `agentstategraph-mcp --hub` serves `/sync/v1/*` for one account on SQLite or Postgres, behind Plan E's hashed API keys, and keeps the push log. Version is negotiated in the URL and the bundle header. | 2 |
| t-007 | **Shallow push and fetch.** Record boundary commits; make history walks, blame and merge-base stop cleanly at a boundary instead of erroring. | 3 |
| t-008 | **Surfaces.** MCP tools (`agentstategraph_remote_add`, `_fetch`, `_push`, `_pull`) and the matching HTTP routes. Expose the same calls in the FFI and language bindings after the Rust API settles. | 3 |
| t-009 | **End-to-end sync suite.** Two stores and a hub. Cover: concurrent pushes, and the fetch → merge → push retry; a taint applied on store A blocks a write on store B after B pulls; reminder and epoch round-trips; secrets refusal; a tampered bundle rejected; and the same suite against a Postgres hub through Plan D's harness. | 3 |
| t-010 | **Merge-safe task ids.** `TaskStore` mints sequential `t-NNN` ids, so two stores adding tasks offline mint the same id. On pull, renumber local tasks that haven't been pushed and whose ids the remote already used, moving them past the remote's highest id. Rewrite `blocked_by`, parent ids and plan links in the same commit. Nothing already pushed is ever renumbered (git-rebase semantics). The same pass detects same-name plans created on both sides and reports them as conflicts. | 2 |
| t-011 | **Pluggable push filter.** A `PushFilter` hook runs over a bundle's new content before it leaves the store. It can block with findings (path + rule) or pass. Ship a credential-pattern filter (provider token and key shapes, private keys, password assignments) that embedders can extend, with an explicit per-finding override recorded in the push log. t-005's `/_meta/_secret` rule becomes one built-in filter. | 2 |

### Acceptance

- Two machines can push and pull a ref through a hub, with no lost updates under concurrent pushes.
- A diverged push is rejected, and fetch → merge → push resolves it.
- Taints, reminders and epochs made on one machine are present and enforced on the other after a pull.
- A bundle with any wrong id is rejected whole.
- A branch containing `/_meta/_secret` can't be pushed.
- A credential-shaped string in new content blocks the push unless explicitly overridden.
- Tasks added offline on two stores to the same plan land with distinct ids after a pull, and their references stay intact.
- The reference hub runs from the public binary on SQLite or Postgres, and keeps a push log.
- A large store can start syncing from a shallow boundary.

### What's NOT in this plan

- **Embedder changes.** Moving each embedder's irreplaceable data onto a
  synced ref is the embedder's job. For ASD: judgment onto its own ref,
  the index on a local-only ref, and `asd remote` / `push` / `pull`.
  For CTXone: memory sync, and whether session transcripts sync.
- **Accounts, teams, billing, or a multi-account hosted hub.** The
  reference hub serves one account.
- **Commit signing.** Per-device Ed25519 signatures would let a hub
  verify authorship rather than only the pusher. That's a follow-on
  using the existing signing infrastructure.
- **Partial clone** (fetching a subtree of a ref) and real-time push
  notifications.
- **Syncing ASG sessions.**

### Done when

Any ASG embedder can keep a store in sync across machines and teammates
through a hub, including its taints, reminders and epochs, with the same
integrity guarantees as local history, and anyone can run that hub from
the public binary.

---

## Plan H — Observability: OpenTelemetry traces, metrics and logs

Scoped on 2026-10-08, against the Plan D and Plan E stacks (MRs !58–!75).

### Motivation

ASG is becoming a server that teams run (Plans D–G), and it is the
substrate inside ASD and CTXone. Operators can't see into it:

- Plan E t-007 logs and counts HTTP requests, but nothing below the HTTP
  layer is visible. A slow commit, a burst of ref retries, a saturated
  Postgres pool, a refused GC and history extraction falling behind all
  look the same from outside.
- MCP tool calls are the main way agents use ASG, and they produce no
  telemetry at all.
- Nothing connects a request to the commit it produced.

ASG already records what changed and why: that is its provenance. Plan H
adds how the work ran, using OpenTelemetry, the vendor-neutral standard
that every major backend ingests. It also links the two: a trace leads to
the commits it produced, and a commit leads back to its trace.

### Findings this plan closes

Verified by reading the code on the Plan E stack.

1. **The libraries log nothing.**
   - Only the MCP binary depends on `tracing` and installs a subscriber
     (`agentstategraph-mcp/Cargo.toml:49-50`, `main.rs:826-843`).
   - The one runtime log line in the library is an `eprintln!`
     (`repo.rs:3180`).
   - There is no `#[instrument]` anywhere.
2. **MCP tool calls are invisible.**
   - The 77 tools run with no span, timing or metric.
   - Their errors are `"Error: …"` strings.
   - In stdio mode the default log level is `warn`.
3. **Hot paths are silent.**
   - The ref compare-and-swap loops retry up to `MAX_REF_CAS_ATTEMPTS`
     (256, `repo.rs:188`), then fail with `WriteConflict`. Nothing records
     how often they retried.
   - Commit, merge, speculation, history extraction, GC, the taint check
     and policy evaluation have no spans.
   - The Rego and Cedar evaluators run as subprocesses with no timeout, and
     their errors are swallowed (`run_externals` in
     `agentstategraph-policy/src/store.rs`).
4. **Storage pressure can't be seen.**
   - deadpool tracks the Postgres pool's size, available connections and
     waiters, but only `pool_max_size()` is exposed (`postgres.rs:749` on
     the Plan D stack).
   - SQLite serializes every call on one `Mutex<Connection>`
     (`sqlite.rs:31`), and nothing measures the wait.
5. **Store health exists only on demand.**
   - The numbers exist: `stats()`, `history_store_shape`, `gc_dry_run` and
     the undistilled-commit count.
   - None is a metric, so no alert can watch history lag or store growth.
   - Plan B t-007 expected GC to be prompted by a size threshold "surfaced
     by health". That surface was never built.
6. **Nothing correlates.**
   - There is no `traceparent`, request id or trace id anywhere.
   - A commit can't be tied to the request that made it.
7. **Errors have no stable codes.**
   - `RepoError` and `StorageError` carry message strings only.
   - HTTP maps two variants to 403 and every other error to 500
     (`http.rs:1535-1566`).
8. **Plan E's log and metric names are its own.**
   - Log fields: `method`, `path`, `status`, `latency_ms`, `key_id`.
   - Metrics: `asg_http_requests_total`,
     `asg_http_request_duration_seconds` with `route` and `status`.
   - OpenTelemetry has names for all of these. **None of Plan E has been
     released**, so renaming costs nothing until the release that ships it.
9. **Requests the Host check refuses aren't logged or counted.** They are
   turned away (`http.rs:730`) outside the observability layer.

### Design shape

1. **The libraries emit `tracing` only.**
   - The library crates gain spans and events through `tracing`: core,
     storage, `agentstategraph`, policy, taint, reminders and tasks.
     `tracing` costs almost nothing when nothing listens.
   - They don't depend on OpenTelemetry. Its Rust crates are pre-1.0 and
     break on minor releases, and every embedder would have to match ASG's
     version.
   - Metrics come from those spans and events, through a layer in the
     telemetry crate, plus gauges read from repository and storage APIs.
2. **One telemetry crate for every binary.** `agentstategraph-telemetry`
   owns setup, built on oxtel (decision 3):
   - the log subscriber;
   - OpenTelemetry's tracer and meter providers;
   - trace-context propagation;
   - `/metrics`;
   - OTLP export.

   The ASG server, ASD and CTXone each call one `init`, and get the same
   names and configuration.
3. **Export uses OTLP; Prometheus scrapes; the library is oxtel.**
   - Every major backend accepts OTLP, and Prometheus ingests it natively.
   - Export goes through **oxtel**
     (`github.com/agentstatelabs/oxtel`, Apache-2.0). It is an
     OpenTelemetry-compatible library that AgentStateLabs is building. It
     reads `tracing` and `metrics` directly and writes OTLP and
     Prometheus/OpenMetrics.
   - We chose it over the official crates after reviewing them on
     2026-10-08:
     - all of them are pre-1.0, and each release breaks compatibility;
     - traces are still Beta;
     - the bridge from `tracing` has effectively one maintainer;
     - pull readers sit behind experimental features;
     - there is no declarative config and no OpAMP.
   - (The official Prometheus exporter is not discontinued, as an earlier
     draft said. It was revived in 0.32 and is Beta.)
   - ASG's instrumentation stays on `tracing`, so nothing outside the
     telemetry crate depends on that choice.
   - OTLP needs network crates, so it goes behind an opt-in `otlp` feature,
     per the guard rail in `agentstategraph-mcp/Cargo.toml`. Release binaries
     and the Docker image turn it on.
4. **Nothing leaves the process unless an operator sends it.**
   - Export stays off until an endpoint is configured.
   - Nothing reports usage to us. CTXone promises zero telemetry, and that
     stays true.
5. **Standard configuration.**
   - The standard `OTEL_*` variables: service name, resource attributes,
     endpoint, protocol, headers, sampler and `OTEL_SDK_DISABLED`.
   - `ASG_LOG`, `ASG_LOG_FORMAT` and `--metrics` stay.
6. **OpenTelemetry semantic conventions, pinned.**
   - HTTP and database telemetry follow the stable conventions.
   - MCP tool calls follow the MCP conventions, which are still in
     development:
     - a span named `tools/call {tool}`;
     - `mcp.method.name` and `gen_ai.tool.name`;
     - `error.type`, which is `tool_error` when a tool returns an error;
     - the `mcp.server.operation.duration` histogram;
     - trace context read from `params._meta`.
   - ASG's own names use `asg.*`.
   - The conventions version is pinned in one place, so a development
     convention that changes is updated there.
7. **Cardinality and privacy rules.**
   - Metric attributes are bounded: methods, route templates, tool names,
     outcomes and error codes.
   - Metrics never carry paths, commit ids, agent ids, key ids or branch
     names. Spans may carry ids.
   - A process serves one tenant, so the tenant is a resource attribute, not
     a label.
   - Telemetry never carries state values, tool arguments or results, keys,
     or SQL with literals.
   - State paths are recorded only when an operator opts in.
8. **Logs carry the trace.**
   - JSON log lines use the conventions' field names and include
     `trace_id` and `span_id`.
   - An `ecs` format writes Elastic Common Schema, for pipelines that expect
     it.
   - With the `otlp` feature, logs export over OTLP too.
9. **Commit-to-trace links stay outside commit content.**
   - The commit's span records the commit id, which links trace to commit at
     no cost.
   - The reverse link, commit to trace, is a side table like the session
     link (`set_commit_session`), not a commit field. That keeps commit ids
     and sync bundles independent of telemetry.

**Sequencing:** t-002 should land before the release that ships Plan E, so
that no released log or metric format changes.

### Task table

| Task | Description | Wave |
|------|-------------|------|
| t-001 | **Conventions.** Write the telemetry contract in `docs/OBSERVABILITY.md`: the pinned semantic-conventions version; span names; metric names, units and instrument types; the `asg.*` attribute registry; the cardinality and privacy rules; and the configuration. Later tasks implement against it. | 1 |
| t-002 | **Align Plan E's names before they ship.** JSON logs flatten their fields and use the conventions' names (`http.request.method`, `url.path`, `http.route`, `http.response.status_code`). Metrics follow the OpenTelemetry-to-Prometheus naming: `http_server_request_duration_seconds` with `http_request_method`, `http_route` and `http_response_status_code`. Requests the Host check refuses are logged and counted. — **In review (MR !76, stacked on !75).** The JSON format is now one flat object per event, with span and event fields side by side. The key id is `asg.api_key.id`, and unknown methods are `_OTHER`. The metrics are `http_server_request_duration_seconds` (with the conventions' buckets, and `error_type` for 5xx; its `_count` replaces the request counter), `http_server_active_requests` and `target_info`. To count refused requests without leaving `/metrics` outside the origin policy, the metrics route is now added separately, inside the policy, and `with_server_layers` fixes the order for `main` and the tests. | 1 |
| t-003 | **`agentstategraph-telemetry`, on oxtel.** One `init` that returns a flush guard, built on oxtel's Phase 1 release. It sets up the log subscriber (text, JSON or ECS); tracer and meter providers whose resource is read from the environment (`service.name`, `service.version`, `service.instance.id`, `asg.tenant.id`); W3C trace-context propagation; the layer that derives metrics from spans; and `/metrics` rendered from the meter provider. OTLP export of traces, metrics and logs sits behind the `otlp` feature and runs only when an endpoint is set. The server switches to the crate. Without features, `cargo tree` shows no network crates. | 1 |
| t-004 | **Stable error codes.** `RepoError` and `StorageError` get a stable, low-cardinality `code()`, such as `write_conflict`, `not_found`, `commit_ref_not_allowed`, `taint_blocked` or `storage_unavailable`. HTTP error bodies include the code, and statuses follow from it instead of defaulting to 500. MCP tool errors carry it too. It becomes `error.type` on spans and metrics. | 1 |
| t-005 | **HTTP traces.** Read `traceparent` and `tracestate`. Give each request one server span, named `{method} {route}`, with the HTTP conventions' attributes. The key id goes on the span, never on a metric. Every log line and error body carries the trace id, and `x-request-id` is accepted, or generated, and echoed. A rejection counter is broken down by reason: rate limited, over capacity, timed out, body too large, unauthenticated, forbidden or refused host. | 2 |
| t-006 | **MCP traces.** One span per tool call, per the MCP conventions, parented on `params._meta.traceparent` when the client sends one. Record `mcp.server.operation.duration`. `error.type` comes from t-004's codes, and is `tool_error` for refusals such as `commit_spec`'s policy gate. Covers stdio. | 2 |
| t-007 | **Library spans and events.** Spans for repository writes (set, delete, merge, `commit_speculation`), the speculation lifecycle, history extraction, GC (dry run, sweep, vacuum), policy evaluation (local and external) and the taint check. Events for compare-and-swap retries and write conflicts, taint rejections, policy decisions (recording whether the fail-safe applied) and external-evaluator errors, which are swallowed today. External evaluators get a timeout. Reads get spans at debug level only. Metrics are derived from these, such as `asg.commit.duration`, `asg.ref.cas_retries` and `asg.policy.decisions`. | 2 |
| t-008 | **Storage metrics.** Operation duration per backend, per the database conventions (`db.client.operation.duration`, `db.system.name`). Postgres pool usage, pending requests and wait time (`db.client.connection.*`). Time spent waiting for SQLite's connection lock. Store gauges, read on a slow interval: objects, commits, size, undistilled commits (history lag), and the last GC sweep and what it reclaimed. Measure what the gauges cost on a large store before choosing the interval. | 2 |
| t-009 | **Commit-to-trace link.** Record the active trace id for each commit in a side table on all four backends. Show it in `log`, `blame` and the HTTP commit output. It can be turned off. | 3 |
| t-010 | **Provenance events.** Optionally emit each commit as an OpenTelemetry log event (`asg.commit`). The event carries the agent, intent category, confidence, namespace, commit id and trace id, and never values. A log backend or SIEM can then index state changes beside traces. Off by default. | 3 |
| t-011 | **Overhead budget.** Benchmark HTTP reads and writes and MCP tool calls in three modes: telemetry off, on without export, and exporting to a local collector. Proposed budget with telemetry on but not exporting: no more than 3% added at p50 and 5% at p99. Record the results in this plan. | 3 |
| t-012 | **Operator kit and docs.** An example OpenTelemetry Collector config. An `observability` compose profile with a collector, Prometheus, Jaeger and Grafana. A Grafana dashboard and Prometheus alert rules as files, covering: not ready, 5xx ratio, p99 latency, rejections, pool saturation, growing history lag, refused GC and write conflicts. A site Observability page with the metric catalogue, and a test that checks the catalogue against the registered instruments. An end-to-end test with an in-memory exporter: one HTTP write makes one trace from request to storage, its log lines carry the trace id, and the commit links back to it. | 3 |

### Rollout to ASD and CTXone

Each product gets its own plan in its own repo. They are listed here so the
order is visible.

- **Now, independent of ASG:**
  - Both log requests through tower-http's `TraceLayer`, whose events
    default to DEBUG under an `info` filter, so neither logs any requests.
    Fix that.
  - Neither has a readiness check. Add one.
- **After an ASG release that includes t-003:**
  - Bump the ASG pin (ASD is on v1.2.3, CTXone on v1.2.4).
  - Call `init` in each binary.
  - Both then inherit ASG's library spans and storage metrics.
- **ASD:**
  - Spans and durations for the index pipeline's phases. Nothing times them
    today.
  - MCP tool spans.
  - Its audit chain stays separate: it is an integrity record, not
    telemetry. ASD Plan I t-037 (SIEM export) is its counterpart.
- **CTXone:**
  - Spans for MCP over HTTP, with trace context from `_meta`.
  - Token usage from `record_llm_usage` as the GenAI usage metrics the
    conventions define (input, output and cache tokens, by model and
    provider).
  - No prompt or response content in telemetry.
  - Export is opt-in, so its zero-telemetry promise holds.

### Acceptance

- **Export:** with an OTLP endpoint configured, a collector receives traces,
  metrics and logs from the server.
- **One write, one trace:** a single HTTP write makes one trace, from the
  request through the commit to storage. Its log lines carry the trace id,
  and the commit links to the trace.
- **MCP:** a client that sends `traceparent` in `_meta` sees ASG's tool span
  as a child.
- **Coverage:** `/metrics` covers HTTP, MCP, the repository, policy, storage,
  GC and history lag, using the catalogue's names.
- **Off by default:** without configuration, nothing is exported, and the
  default build has no network crates.
- **Privacy:** state values, keys, tool arguments and SQL literals never
  appear in telemetry, and a test checks this.
- **Overhead:** stays within the budget.
- **One call:** ASD and CTXone set up the same telemetry with one call.

### What's NOT in this plan

- **A hosted telemetry backend**, or usage reporting to us.
- **Process and runtime metrics** (CPU, memory, tokio internals). Node
  exporters and the collector's host metrics cover them.
- **Profiling.** OpenTelemetry's profiles signal isn't stable.
- **Browser telemetry** for the explorer or the Lens.
- **Audit logging.** Commits are ASG's audit record, and ASD's audit chain is
  its own.
- **Dashboards for every vendor.** One Grafana dashboard ships, and OTLP
  carries the data to any backend.

### Done when

An operator can point any OTLP backend, or Prometheus, at ASG, ASD or
CTXone without code changes. They see requests, tool calls, state changes
and storage health, with traces tied to logs and commits. Nothing leaves
the process unless they configure it.

---

## First-parent surfaces — which are deliberate, which are accidents

`Repository::log` follows `parents[0]`. That is a legitimate thing to offer —
git's own `log --first-parent` exists — but until `list_commits_dag` landed it
was the *only* walk available, so several surfaces inherited it without ever
choosing it. This records which is which, measured rather than assumed.

**Measurement.** A fixture with a three-commit side branch merged into `main`:
`log` returns **4** commits, `log_dag` returns **7**. All three side-branch
commits are invisible to `log`. On the real asd store the same gap is 4,268
reachable by `log` versus 5,896 in the store.

Note what is *not* missed: a merge commit is itself on the first-parent line,
so its `parents` — including the merged-in tip — are still visible to anything
that dereferences them. What disappears is the branch's **interior**.

| surface | uses | verdict |
|---|---|---|
| `extract_history` / the `asg_history_*` rollup | `FROM commits ORDER BY rowid` — no walk at all | **Correct by design.** Sees every commit including unreachable ones. This is why asd's rollup counted 5,896 while `log` reached 4,268. |
| `log` | `parents[0]` | **Deliberate.** Documented, and left alone on purpose — widening it would change output for every existing caller. |
| `intent_tree` | `log` for the population, then threads by `parent_intent` | **Mostly deliberate.** Its doc is explicit that it is not a lineage walk. But its *population* is still first-parent, so intents recorded on a merged-in branch never appear. |
| `detect_timestamp_anomalies` | `log(ref, 100_000)` | **Accident, and the one that matters.** A security surface (threat model V4). It compares each commit against its parents, so `merge` vs the merged-in tip *is* checked — but pairs wholly inside a merged-in branch are never examined, because those commits never appear as the outer `commit`. Clock-rewind evidence on a side branch is invisible. |
| `blame` | `log(ref, 1000)`, then `parents.first()` internally | **Accident.** A change made on a side branch is attributed to the merge commit rather than to the commit that made it. |
| `commit_graph` | `log(ref, depth)` | **Accident, and self-contradictory.** It emits nodes carrying a `parents` array, so with a merge in range it produces edges pointing at nodes absent from its own node set — 4 nodes describing a 7-node graph. |
| `stats` | `log(ref, 10000)` | **Accident.** Undercounts by whatever the branches hold. |
| MCP `GET /log` | `Repository::log` | **Deliberate**, in that it mirrors `log`. Worth a `?dag=1` option rather than a change of meaning. |

Fixing the four accidents is deliberately **not** bundled with the walk that
makes fixing them possible: `blame` and `detect_timestamp_anomalies` change
security- and attribution-relevant output, and that is a decision to take
explicitly rather than as a side effect of a storage change.
