# Taint, Quarantine, and Watch Spec for AgentStateGraph

**Context**: During live agent testing against a 7-node cluster, a gap was identified between passive observations (recording a problem) and static policies (enforcing rules). Taints are dynamic runtime markers that bridge observation into enforcement — marking components as degraded, suspect, or restricted with effects that change how agents interact with them.

**Related concepts implemented**: The Kubernetes taint/toleration model, Terraform's taint-for-rebuild, and security quarantine patterns all inform this design. ASG's unique contribution is that taints are commits (auditable, blameable, with full intent metadata) and taint checks integrate with the existing policy evaluation pipeline.

---

## Core concept: Three severity levels

| Command | What it means | Use case |
|---|---|---|
| **Taint** | "This component is degraded — proceed with caution" | Node under disk pressure, service crash-looping, config drift detected |
| **Quarantine** | "This component is suspect — do not touch without authorization" | Security incident, data corruption suspected, failed compliance check |
| **Watch** | "This component needs attention — monitor closely" | Performance trending down, approaching thresholds, recently changed |

Taint is the middle ground. Watch is lighter (advisory). Quarantine is heavier (restrictive).

---

## MCP Tools

### agentstategraph_taint

Mark a path as tainted with an effect that changes how agents interact with it.

```
Parameters:
  path: String          — path to taint (e.g., "/cluster/nodes/picoup2")
  taint: String         — taint name (e.g., "disk-pressure", "unstable", "drift")
  effect: String        — "warn" | "block" | "review" | "isolate"
  reason: String        — why this taint is being applied
  severity: String      — "low" | "medium" | "high" | "critical"
  expires: Option<String> — ISO 8601 timestamp for auto-expiry (null = permanent until untainted)
  propagate: Option<bool> — if true, taint applies to all child paths (default: true)

Returns:
  Taint ID, affected path, effect, and commit ID of the taint commit

Intent metadata:
  category: "Taint"  (new IntentCategory)
  All standard fields: reasoning, confidence, tags, authority
```

**Effects:**

| Effect | Behavior |
|---|---|
| `warn` | Agents see a warning when reading/writing the tainted path. Does not block. Warning included in get/set responses. |
| `block` | Writes to the tainted path are rejected with TaintedPathError. Reads still work (you need to see what's there to fix it). |
| `review` | Writes require confidence ≥ 0.9 and reasoning that explicitly addresses the taint. Lower-confidence writes are rejected. |
| `isolate` | The tainted subtree is excluded from query results, search, and get_tree unless the caller explicitly passes `include_tainted: true`. |

**Propagation:**

When `propagate: true` (default), a taint on `/cluster/nodes/picoup2` applies to all paths under it:
- `/cluster/nodes/picoup2/services/spark-silver/restarts` — tainted
- `/cluster/nodes/picoup2/disk_used_pct` — tainted
- `/cluster/nodes/picoup0/...` — NOT tainted

### agentstategraph_untaint

Remove a taint from a path. Requires a reason (auditable).

```
Parameters:
  path: String          — path to untaint
  taint: String         — taint name to remove (must match an active taint)
  reason: String        — why the taint is being removed
  proof: Option<String> — optional commit ID or evidence that the issue is resolved

Returns:
  Confirmation with the untaint commit ID

Intent metadata:
  category: "Untaint"  (new IntentCategory)
```

### agentstategraph_quarantine

Stronger than taint — marks a path as suspect with mandatory authorization for any interaction.

```
Parameters:
  path: String
  reason: String
  severity: String      — "high" | "critical" (quarantine is always serious)
  authorized_agents: Vec<String> — only these agents can read/write (e.g., ["agent/security", "human/sre-lead"])
  expires: Option<String>
  propagate: Option<bool>  — default true

Returns:
  Quarantine ID, commit ID

Effects:
  - Reads by unauthorized agents return QuarantinedPathError with the quarantine reason
  - Writes by unauthorized agents are rejected
  - The path is excluded from search/query/get_tree for unauthorized agents
  - Authorized agents see a quarantine banner but can proceed
  - All access attempts (including rejected ones) are logged as commits

Intent metadata:
  category: "Quarantine"
```

### agentstategraph_unquarantine

Release a quarantine. Requires authorization (must be one of the `authorized_agents`).

```
Parameters:
  path: String
  reason: String
  proof: Option<String>   — evidence the issue is resolved
  
Returns:
  Confirmation with commit ID
```

### agentstategraph_watch

Lighter than taint — advisory marker that draws attention without restricting access.

```
Parameters:
  path: String
  watch_type: String    — "performance" | "threshold" | "recently-changed" | "compliance" | custom
  reason: String
  metric: Option<String>  — what to watch (e.g., "disk_used_pct", "restarts", "latency_p95")
  threshold: Option<f64>  — alert if metric crosses this value
  check_interval_secs: Option<u64> — suggested polling interval
  expires: Option<String>

Returns:
  Watch ID, commit ID

Effects:
  - No access restrictions — purely advisory
  - Shows up in list_watches and in the viewer as a visual indicator
  - If threshold is set and a subsequent commit changes the watched metric past the threshold,
    the watch auto-escalates to a taint with effect "warn"
  - Watches are included in stats output: "active_watches: 3"
```

### agentstategraph_unwatch

Remove a watch.

```
Parameters:
  path: String
  watch_type: String
  reason: Option<String>  — optional, watches are lightweight
```

### agentstategraph_list_taints

List all active taints, quarantines, and watches.

```
Parameters:
  path: Option<String>   — filter by path prefix
  effect: Option<String> — filter by effect type
  include_expired: Option<bool> — include expired taints (default: false)

Returns:
  Array of active taints/quarantines/watches with:
  - path, taint/quarantine/watch name, effect, severity
  - reason, applied_by (agent), applied_at (timestamp)
  - expires (if set), expired (bool)
  - commit_id (the taint commit for blame)
```

### agentstategraph_check_taint

Check if a specific path is tainted, quarantined, or watched before writing. Agents should call this proactively before making changes to critical paths.

```
Parameters:
  path: String

Returns:
  {
    "tainted": bool,
    "quarantined": bool,
    "watched": bool,
    "taints": [...],      — active taints on this path or ancestors
    "quarantines": [...], — active quarantines
    "watches": [...],     — active watches
    "can_write": bool,    — given current taints/quarantines, is writing allowed?
    "required_confidence": f64  — minimum confidence needed (elevated if "review" effect)
  }
```

---

## Storage

### Taints table

```sql
CREATE TABLE IF NOT EXISTS taints (
    id          TEXT PRIMARY KEY,       -- UUID
    path        TEXT NOT NULL,          -- affected path
    name        TEXT NOT NULL,          -- taint name (e.g., "disk-pressure")
    kind        TEXT NOT NULL,          -- "taint" | "quarantine" | "watch"
    effect      TEXT NOT NULL,          -- "warn" | "block" | "review" | "isolate" | "advisory"
    severity    TEXT NOT NULL DEFAULT 'medium',
    reason      TEXT NOT NULL,
    agent_id    TEXT NOT NULL,          -- who applied it
    commit_id   TEXT NOT NULL,          -- the commit that created this taint
    created_at  TEXT NOT NULL,
    expires_at  TEXT,                   -- NULL = permanent
    resolved_at TEXT,                   -- NULL = still active
    resolved_by TEXT,                   -- agent who untainted
    resolved_reason TEXT,              -- why it was resolved
    resolved_proof TEXT,               -- commit ID proving resolution
    propagate   BOOLEAN NOT NULL DEFAULT 1,
    metadata    TEXT NOT NULL DEFAULT '{}', -- JSON: authorized_agents, threshold, metric, etc.
    
    UNIQUE(path, name, kind)           -- one taint of each name per path
);

CREATE INDEX IF NOT EXISTS idx_taints_path ON taints(path);
CREATE INDEX IF NOT EXISTS idx_taints_active ON taints(resolved_at) WHERE resolved_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_taints_kind ON taints(kind);
```

Both `SqliteStorage` and `MemoryStorage` must implement. Add to the `Storage` trait:

```rust
fn create_taint(&self, taint: &Taint) -> Result<()>;
fn resolve_taint(&self, id: &str, resolved_by: &str, reason: &str, proof: Option<&str>) -> Result<()>;
fn list_taints(&self, path_prefix: Option<&str>, kind: Option<&str>, include_resolved: bool) -> Result<Vec<Taint>>;
fn check_taint(&self, path: &str) -> Result<TaintCheck>;
fn get_taint(&self, id: &str) -> Result<Option<Taint>>;
```

---

## Integration with existing systems

### Pre-commit hook

When a commit is about to be written via `set`, `merge`, or `commit_spec`, check for taints on the affected paths:

```
1. Resolve the paths being modified
2. For each path, call check_taint (including ancestor paths if propagate=true)
3. If effect = "block" → reject with TaintedPathError
4. If effect = "review" → check confidence ≥ 0.9, reject if too low
5. If effect = "warn" → allow but include warning in response
6. If effect = "isolate" → allow writes but flag for visibility filtering
7. If quarantined → check if agent is in authorized_agents list
```

### Policy integration

Taints should be checkable from policy evaluation:

```
policy_evaluate_change should include taint status:
{
  "path": "/cluster/nodes/picoup2/services/spark-silver",
  "taints": ["unstable"],
  "quarantines": [],
  "watches": ["restart-count"],
  "taint_effects": ["review"],
  "policy_result": "allowed_with_elevated_confidence"
}
```

### Watch auto-escalation

When a watch has a `threshold` and a commit changes the watched path's value past the threshold:

```
1. On commit, check if any watches apply to the modified path
2. If watch has threshold and new value exceeds it:
   a. Auto-create a taint with effect "warn" and reason "Watch threshold exceeded: {metric} = {value} > {threshold}"
   b. The auto-taint commit references the watch as its parent intent
3. This creates an audit trail: watch → threshold exceeded → auto-taint → agent warning
```

### Query filtering

When `isolate` effect is active:
- `agentstategraph_search` excludes isolated paths unless `include_tainted: true`
- `agentstategraph_get_tree` excludes isolated subtrees unless `include_tainted: true`
- `agentstategraph_query` includes a `taint_status` field on each result

### Viewer integration

The Stack Viewer should show taints visually:
- **Tainted nodes**: amber border glow on the topology tile
- **Quarantined nodes**: red border glow with lock icon
- **Watched nodes**: blue subtle indicator
- **Taint panel**: new section in the left panel showing active taints/quarantines/watches with drill-down

---

## New IntentCategories

Add to the `IntentCategory` enum:

```rust
Taint,       // Marking a path as degraded
Untaint,     // Removing a taint with reason/proof
Quarantine,  // Restricting access to a suspect path
Unquarantine,// Releasing a quarantine
Watch,       // Setting an advisory watch
Unwatch,     // Removing a watch
```

These are all queryable — you can `agentstategraph_query intent_category="Taint"` to find all taint events.

---

## Demo scenarios

### Scenario 1: Crash loop → taint → restricted deployment

```
Agent discovers spark-silver at 6800 restarts
  → taint /cluster/nodes/picoup2 "unstable" effect=review severity=critical
  → reason: "spark-silver crash-looping, node under resource pressure"

Later, another agent tries to deploy a new service to picoup2
  → check_taint returns: tainted, effect=review, required_confidence=0.9
  → agent must provide high-confidence reasoning to proceed
  → or pick a different node
```

### Scenario 2: Security incident → quarantine

```
Agent detects unauthorized SSH access pattern on ci-runner
  → quarantine /cluster/nodes/ci-runner severity=critical
  → authorized_agents: ["agent/security", "human/sre-lead"]
  → all other agents locked out of reading/writing ci-runner state

Security agent investigates, finds false alarm
  → unquarantine with proof (investigation commit ID)
  → full audit trail of who was locked out, for how long, and why
```

### Scenario 3: Watch → auto-escalation

```
Agent sets watch on picoup2 disk:
  → watch /cluster/nodes/picoup2/disk_used_pct metric="disk_used_pct" threshold=80

Disk grows from 76% to 82%:
  → auto-taint fires: "Watch threshold exceeded: disk_used_pct = 82 > 80"
  → effect=warn, agents see warning on any picoup2 interaction
  → the watch→taint chain is visible in intent_tree
```

---

## Acceptance criteria

1. `taint` creates a taint commit and persists to the taints table
2. `untaint` resolves the taint with reason/proof and persists
3. `quarantine` restricts access to authorized agents only
4. `unquarantine` releases with audit trail
5. `watch` creates an advisory marker that persists
6. `list_taints` returns active taints across restarts (persistence)
7. `check_taint` returns the full taint status for a path including ancestor taints
8. Pre-commit hook rejects writes to blocked/quarantined paths
9. Review-effect taints require elevated confidence (≥0.9)
10. Watch auto-escalation creates a taint when threshold exceeded
11. All taint/quarantine/watch operations are commits with full intent metadata
12. Existing tests still pass (taints are additive, not breaking)

---

## Files to modify

| File | Changes |
|---|---|
| `crates/agentstategraph-core/src/intent.rs` | Add Taint/Untaint/Quarantine/Watch IntentCategories |
| `crates/agentstategraph-storage/src/lib.rs` | Add taint methods to Storage trait |
| `crates/agentstategraph-storage/src/sqlite.rs` | Implement taints table + queries |
| `crates/agentstategraph-storage/src/memory.rs` | Implement in-memory taint storage |
| `crates/agentstategraph/src/repo.rs` | Add taint/quarantine/watch methods, pre-commit hook |
| `crates/agentstategraph-mcp/src/server.rs` | Add 8 new MCP tools (taint, untaint, quarantine, unquarantine, watch, unwatch, list_taints, check_taint) |

---

## Estimated tool count after implementation

Current: 50 MCP tools
New: +8 tools
Total: 58 MCP tools
