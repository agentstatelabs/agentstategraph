# agentstategraph-taint

Taint / quarantine / watch substrate for AgentStateGraph. Dynamic
runtime markers that bridge passive observation into enforcement.

See `docs/TAINT_GUIDE.md` for the end-user overview and
`spec/TAINT_SPEC.md` for the design spec.

## Core types

| Type | Purpose |
|---|---|
| `Taint` | One record: `{id, path, name, kind, effect, severity, …}` |
| `TaintKind` | Discriminator — `Taint` / `Quarantine` / `Watch` |
| `TaintEffect` | `Warn` / `Block` / `Review` / `Isolate` / `Advisory` |
| `TaintSeverity` | Advisory — `Low` / `Medium` / `High` / `Critical` |
| `TaintMetadata` | Flat `String → serde_json::Value` map |
| `TaintCheck` | Aggregated access decision for a `(path, agent, confidence)` triple |
| `TaintParams` / `QuarantineParams` / `WatchParams` / `UntaintParams` / `UnwatchParams` | Repository API parameter bundles |

## The check algorithm

`evaluate_access(path, agent_id, confidence, candidates, now)`
returns a `TaintCheck` that answers "can this write proceed?"
Precedence (strongest first):

1. Active `Quarantine` not authorizing `agent_id` → `can_write = false`
2. Active `Taint` with effect `Block` → `can_write = false`
3. Active `Taint` with effect `Review` → `required_confidence = 0.9`;
   `can_write = confidence ≥ required`
4. Active `Taint` with effect `Isolate` → advisory; `isolated = true`
5. `Warn` / `Advisory` / `Watch` → advisory only

Ancestor match respects path boundaries: a taint on `/cluster` does
NOT match `/cluster-staging`.

## Workflow

Types-only: storage lives in `agentstategraph-storage`, commit-pipeline
wiring in the main `agentstategraph` crate. Direct use looks like:

```rust
use agentstategraph_taint::{Taint, TaintEffect, TaintKind, evaluate_access};
use chrono::Utc;

let candidates = vec![/* fetched from Storage::check_taint */];
let check = evaluate_access("/cluster/nodes/a", "agent-1", 0.95,
                            &candidates, Utc::now());
if !check.can_write {
    // block the write
}
```

Most consumers don't interact with this crate directly — they go
through `Repository::taint()` / `check_taint()` / etc. This crate
is the shared shape so bindings (Py / TS / Go / WASM / C#) can
round-trip the types via serde.

## Composability

- **With policy**: `PolicyStore::evaluate_change` returns a
  `Decision`; combine with `Repository::check_taint` via
  `agentstategraph_policy_evaluate_change_with_taints` for a
  conjunction answer (`can_proceed = !deny && every can_write`).
- **With intent metadata**: every taint / untaint / quarantine /
  watch operation writes a commit with the matching
  `IntentCategory`; filter them via
  `agentstategraph_query intent_category=Taint`.
- **With watch auto-escalation**: threshold-carrying watches fire
  `Warn`-effect taints on threshold cross; the auto-taint's
  metadata cites `source_watch_id` for blame.

## Soft enforcement

The same caveat applies here as to the policy primitive: ASG
cannot physically stop a misbehaving agent. A `Block`-effect taint
is a machine-readable boundary, not a syscall interceptor. Pair
with OPA / Cedar / cloud IAM for hard enforcement at the
infrastructure layer.

## License

BSL-1.1.
