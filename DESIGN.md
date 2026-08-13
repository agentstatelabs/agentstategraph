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
| t-001 | **Metric schema + incremental extractor**: new `asg_history_*` tables in the storage layer; a walker that reads the commit chain forward from a watermark cursor (in `asd_index_meta` / a store-meta table) so it's incremental, not a full re-scan. Idempotent, resumable, bounded memory over 512k+ commits. | 1 |
| t-002 | **`asg history` command + storage API**: query the materialized tables — velocity (`--by day\|week`), intent mix, authorship, milestone timeline, per-symbol decision history. Human table + `--json` contract (smoke-tested from a non-source checkout — the same CWD/JSON-contract discipline ASD's CLAUDE.md mandates). | 1 |
| t-003 | **Store-shape + amplification report** (`asg history --store`): objects, commits, bytes/table via dbstat, avg nodes-created-per-commit, hottest-churn symbols. The evidence surface for Plan B retention thresholds. | 1 |
| t-004 | **Backfill + reconcile**: run the extractor against SessionDrift-ios (512k commits) and PulseLab (golden benchmark); record wall-clock; verify rollup counts reconcile EXACTLY against `SELECT COUNT(*)` ground truth (reconcile against real distributions, not synthetic — the ASD calibration-inversion lesson). | 2 |
| t-005 | **Retention hooks for Plan B**: each metric row records the `state_root`(s) and commit-id range it summarizes, so GC can prove "this raw snapshot's signal is already captured" before pruning. The contract seam between A and B. | 2 |

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
| t-001 | **Reachability marker**: from `refs` + retained checkpoints, walk each `state_root`'s node closure and mark live objects. Streaming/batched to run in bounded memory over 14.8M nodes (cannot hold the mark-set naively — disk-backed or bitmap-over-rowid). Report live vs. total object counts (the reclaimable estimate). | 1 |
| t-002 | **`asg gc --dry-run`**: reports what would be swept, bytes/pages reclaimable, and estimated post-vacuum size WITHOUT mutating — the safe default. Loud about anything it would drop that Plan A has NOT yet distilled (refuses; never silently skips). | 1 |
| t-003 | **Sweep + retention engine**: apply mark-sweep and the checkpoint-and-prune rules (`--keep-recent`, `--checkpoint-every`, keep-all-milestones); transactional, resumable, gated on the Plan A t-006 predicate and the epoch-seal invariant. `--dry-run` is default; mutation requires an explicit flag. | 2 |
| t-004 | **Vacuum / page reclaim**: `VACUUM` or `PRAGMA incremental_vacuum` after sweep; evaluate enabling `auto_vacuum=INCREMENTAL` going forward so freelist pages return without a full rewrite. Measure before/after on the field DB. | 2 |
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
| t-001 | **Thread reasoning / confidence / tool_calls end to end**: expose `tool_calls` on `CommitOptions`; carry all three from the caller (ASD's ledger/trace/mcp commit paths, and the MCP/HTTP/FFI surfaces) through `Repo::commit` so real values persist. All optional, backward compatible (default to today's None/empty). | 1 |
| t-002 | **Wire authority**: expose authority on the commit surfaces (MCP/HTTP/FFI/CLI) and route the caller's real principal through instead of the hardcoded `Authority::simple("default")`. Decide the minimum viable capture (principal at least) vs. deferring full delegation chains to t-004. | 1 |
| t-003 | **Fix the `parent_intent` lie**: either wire `with_parent` + make `intent_tree` actually consult `parent_intent`, OR (if the commit-DAG walk is the intended behavior) delete `parent_intent` / `with_parent` and correct the doc. Do not ship a field documented to do something it doesn't. | 1 |
| t-004 | **Scoped / delegated authority** (the `AuthScope` + `DelegationLink` model): either wire `for_intent` and the non-Wildcard scopes into a real authorization path, or prune the dead variants/struct if the model is premature. Explicit decision, not drift — dead public enum variants are a maintenance tax. | 2 |
| t-005 | **Engage epoch-seal-strict**: expose `with_epoch_seal_strict` (or make strict the default) on a shipped surface so the `EpochSealViolated` hard guard is reachable in production. Coordinate with Plan B — GC relies on sealed commits not being orphanable. Add a test that the hard path fires through a real surface, not just a unit constructor. | 2 |
| t-006 | **Storage-tax check**: confirm that after wiring, commits with no provenance still serialize compactly (skip-if-none / skip-if-empty already holds for most fields — verify authority doesn't bloat the common case). Provenance should cost bytes only when actually present. | 2 |

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
