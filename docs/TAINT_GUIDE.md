# Taint / Quarantine / Watch Guide

_User-facing guide to the taint substrate. See
`spec/TAINT_SPEC.md` for the design rationale._

## What is it for?

Policy tells an agent *what is allowed*. Taints tell an agent
*what has gone wrong*. They bridge passive observation ("this node
is under disk pressure") into enforcement ("deny writes to this
node until it's cleared").

Three kinds in one substrate:

| Kind | Severity | Use case |
|---|---|---|
| **Taint** | middle | "This path is degraded — proceed with caution." |
| **Quarantine** | heavy | "This path is suspect — do not touch without authorization." |
| **Watch** | advisory | "This path needs attention — monitor closely." |

All three are first-class commits. Every apply / resolve writes an
intent commit with the matching `IntentCategory` (`Taint` /
`Untaint` / `Quarantine` / `Unquarantine` / `Watch` / `Unwatch`) so
the full lifecycle is auditable, blameable, and queryable through
`log` / `blame`.

## Effects (the "what happens when I write" table)

Taints carry an **effect** that the pre-commit hook consults on
every `set` / `set_json` / `delete` / `merge`:

| Effect | Pre-commit behavior |
|---|---|
| `warn` | Allow the write. Attach a structured warning to the commit metadata. |
| `block` | Reject the write with `TaintError::Blocked`. Reads still work. |
| `review` | Require `CommitOptions.confidence >= 0.9`. Lower-confidence writes are rejected with `TaintError::InsufficientConfidence`. |
| `isolate` | Allow the write. Flag the path for search / query / `get_tree` filtering (callers opt in via `include_tainted: true`). |
| `advisory` | Watch-only; never blocks. |

Quarantines always behave as `block` against unauthorized agents
(an agent not in the quarantine's `authorized_agents` allowlist).

## The pre-commit hook

Wired into the Repository surface since 0.7.75:

```rust
let err = repo.set_json("main", "/cluster/nodes/picoup2/state", &json!({...}),
    CommitOptions::new("agent-1", IntentCategory::Refine, "update")).unwrap_err();

match err {
    RepoError::Taint { source: TaintError::Blocked { .. }, .. } => {
        // path was blocked — show the operator why
    }
    RepoError::Taint { source: TaintError::InsufficientConfidence { required, got, .. }, .. } => {
        // review gate — the commit needs higher confidence
    }
    RepoError::Taint { source: TaintError::NotAuthorized { .. }, .. } => {
        // quarantine — this agent isn't on the allowlist
    }
    _ => {}
}
```

Taint-lifecycle commits (`IntentCategory::Taint` etc.) bypass the
hook so the substrate doesn't deadlock creating / resolving the
very taints it gates on.

## Propagation

Every taint carries `propagate: bool` (default `true`). When set, a
taint on `/cluster` applies to `/cluster/nodes/a` and
`/cluster/nodes/a/state` but NOT to `/cluster-staging` — path
boundaries are respected (the substrate matches on `path + '/'`
prefixes, not raw string prefixes).

Set `propagate: false` when you want a leaf-only taint — e.g. a
block on `/config/debug_mode` that doesn't cascade to
`/config/debug_mode/history`.

## Watches and auto-escalation

Watches are purely advisory until you attach a numeric threshold.
Then the magic happens:

```rust
repo.watch_path("main", "/cluster/disk", WatchParams {
    name: "disk-80".into(),
    reason: "running out of headroom".into(),
    metric: Some("disk_used_pct".into()),
    threshold: Some(80.0),
    direction: WatchDirection::Above,
    // ...
})?;
```

When a subsequent `set_json` writes a value that crosses 80%, the
substrate auto-creates a **Warn-effect taint** named
`watch-threshold-exceeded-disk-80` on the watch's path. The
auto-taint metadata cites `source_watch_id`, `metric`,
`threshold`, and `observed` so blame is unambiguous.

Auto-escalation is **idempotent** — crossing twice in a row creates
one taint, not N. The watch → taint chain is visible in
`agentstategraph_query intent_category=Taint` for audit.

Metric extraction supports three shapes:
- top-level number — `json!(82.5)`
- stringified number — `json!("82.5")`
- flat object lookup — `json!({"disk_used_pct": 82.5})` with
  `metric: "disk_used_pct"`

## Policy composition

Taints don't replace the policy primitive — they compose with it.
Use `agentstategraph_policy_evaluate_change_with_taints` to get
both verdicts at once:

```json
{
  "ok": true,
  "decision": { "kind": "allow", "matched_policy": "ops/reindex@1", ... },
  "taint_status": [
    { "path": "/cluster/shards", "check": { "tainted": true, "can_write": false, ... } }
  ],
  "can_proceed": false
}
```

`can_proceed` is the conjunction of `decision.kind != deny` and
every affected path's `check_taint.can_write`.

## MCP tools (60 → 61 total for 0.7.75 including `_with_taints`)

Eight taint tools plus the policy-composition tool, all under
`agentstategraph_*`:

- `taint` / `untaint`
- `quarantine` / `unquarantine`
- `watch` / `unwatch`
- `list_taints` — filter by path prefix, kind, effect,
  include_expired
- `check_taint` — aggregated status for a path
- `policy_evaluate_change_with_taints` — conjunction response

See `agentstategraph-mcp` README for the parameter shapes.

## FFI externs (56 → 64)

Eight new extern C functions; every one takes a JSON params
envelope so the binding boundary stays narrow as the substrate
evolves. See `bindings/go/agentstategraph.h` for the full
declarations.

## Storage

Both `MemoryStorage` and `SqliteStorage` implement `TaintStore`:

```sql
CREATE TABLE IF NOT EXISTS taints (
    id              TEXT PRIMARY KEY,
    path            TEXT NOT NULL,
    name            TEXT NOT NULL,
    kind            TEXT NOT NULL,
    effect          TEXT NOT NULL,
    severity        TEXT NOT NULL DEFAULT 'medium',
    reason          TEXT NOT NULL,
    agent_id        TEXT NOT NULL,
    commit_id       TEXT NOT NULL DEFAULT '',
    created_at      TEXT NOT NULL,
    expires_at      TEXT,
    resolved_at     TEXT,
    resolved_by     TEXT,
    resolved_reason TEXT,
    resolved_proof  TEXT,
    propagate       INTEGER NOT NULL DEFAULT 1,
    metadata        TEXT NOT NULL DEFAULT '{}'
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_taints_unique_active
    ON taints(path, name, kind) WHERE resolved_at IS NULL;
```

The partial unique index allows historical (resolved) rows to
coexist with new active rows on the same `(path, name, kind)`
triple — useful for repeated taint-fix-taint cycles where the
audit trail matters.

`PostgresStorage` ships a stub that returns `Backend` on mutating
calls; `IndexedDbStorage` delegates to an inner `MemoryStorage`
(in-session taints; durable snapshotting lands in a later
milestone).

## Soft enforcement — read this before marketing

AgentStateGraph cannot physically stop a misbehaving agent. A taint
is a machine-readable boundary; pair with OPA / Cedar / IAM for hard
enforcement at the infrastructure layer.

The value:
1. **Clarity** — the agent always knows which paths are degraded.
2. **Audit trail** — every taint apply / resolve is a commit.
3. **Composition** — taints × policies × approval tasks compose
   cleanly; agents can plan around all three.

## Related docs

- `spec/TAINT_SPEC.md` — design spec
- `docs/POLICY_GUIDE.md` — policy primitive
- `crates/agentstategraph-taint/README.md` — crate API reference
