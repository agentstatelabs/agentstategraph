# Persistence Spec: Epochs & Sessions in SqliteStorage

**For**: Agent implementing persistence in the `agentstategraph-storage` crate
**Context**: The demo and enterprise validation use case require all state to survive MCP process restarts. Currently epochs and sessions are in-memory only.
**Priority**: Epochs are critical (compliance feature). Sessions are important (multi-agent orchestration).

---

## Current state

### What's persisted (working correctly)
```
SQLite tables:
  objects  — content-addressed state tree blobs (1063+ rows)
  commits  — full commit history with intent/reasoning/confidence (144+ rows)
  refs     — branch heads (2+ rows: main, intent/scaleout-plan, fix/* branches)
```

Plans and policies are stored as regular state within the objects/commits system at `/plans/*` and `/policies/*` — they persist correctly.

### What's NOT persisted (the problem)

**Epochs** — `create_epoch`, `seal_epoch`, `list_epochs` all operate on an in-memory `Vec<Epoch>` or `HashMap`. When the MCP process exits, all epochs vanish. A sealed epoch that disappears defeats the entire compliance story.

**Sessions** — `sessions` (agent session tracking with parent-child relationships and path scoping) are in-memory. When the process restarts, the session registry is empty.

**Speculations** — These are intentionally ephemeral (in-memory handles for O(1) branching). When committed via `commit_spec`, they become real commits. This is correct by design and should NOT be persisted.

---

## What to implement

### 1. Epochs table

```sql
CREATE TABLE IF NOT EXISTS epochs (
    id          TEXT PRIMARY KEY,           -- e.g., "2026-Q2-infrastructure-review"
    description TEXT NOT NULL DEFAULT '',
    status      TEXT NOT NULL DEFAULT 'Active',  -- 'Active' or 'Sealed'
    created_at  TEXT NOT NULL,              -- ISO 8601 timestamp
    sealed_at   TEXT,                       -- NULL if Active, ISO 8601 if Sealed
    summary     TEXT,                       -- NULL if Active, set on seal
    root_intents TEXT NOT NULL DEFAULT '[]', -- JSON array of commit IDs
    agents      TEXT NOT NULL DEFAULT '[]', -- JSON array of agent IDs involved
    tags        TEXT NOT NULL DEFAULT '[]', -- JSON array of tags
    commit_count INTEGER NOT NULL DEFAULT 0 -- count of commits in this epoch
);
```

#### Operations to make durable

| Operation | Current behavior | Required behavior |
|---|---|---|
| `create_epoch(id, description, root_intents)` | Pushes to in-memory vec | INSERT into `epochs` table |
| `seal_epoch(id, summary)` | Mutates in-memory struct | UPDATE `epochs` SET status='Sealed', sealed_at=NOW(), summary=? |
| `list_epochs()` | Returns in-memory vec | SELECT * FROM epochs ORDER BY created_at DESC |
| `get_epoch(id)` | Finds in in-memory vec | SELECT * FROM epochs WHERE id=? |

#### Seal enforcement (already partially implemented)

When an epoch is sealed, no new commits should be added to it. The current in-memory implementation has this check. The persisted version must:

1. On `seal_epoch`: UPDATE status to 'Sealed', record `sealed_at` timestamp
2. On any commit that would fall within a sealed epoch's scope: reject with `EpochSealedError`
3. The `sealed_at` timestamp + `summary` together form the tamper-evident record

#### Epoch-commit association

Epochs need to know which commits belong to them. Two approaches:

**Option A (recommended)**: Add an `epoch_id` column to the `commits` table:
```sql
ALTER TABLE commits ADD COLUMN epoch_id TEXT REFERENCES epochs(id);
```
When a commit is created and an epoch is active, set `epoch_id` to the active epoch's ID. `commit_count` on the epoch is derived from `SELECT COUNT(*) FROM commits WHERE epoch_id=?`.

**Option B**: Store commit IDs in the epoch row as a JSON array. Gets unwieldy with many commits.

Go with Option A.

### 2. Sessions table

```sql
CREATE TABLE IF NOT EXISTS sessions (
    id          TEXT PRIMARY KEY,           -- UUID or agent-scoped ID
    agent_id    TEXT NOT NULL,              -- e.g., "agent/perf-tuner"
    parent_id   TEXT REFERENCES sessions(id), -- NULL for root sessions
    scope_path  TEXT,                       -- path prefix this session can write to
    scope_branch TEXT,                      -- branch this session operates on
    status      TEXT NOT NULL DEFAULT 'Active', -- 'Active', 'Completed', 'Abandoned'
    created_at  TEXT NOT NULL,
    ended_at    TEXT,
    metadata    TEXT NOT NULL DEFAULT '{}', -- JSON blob for extensible metadata
    commit_count INTEGER NOT NULL DEFAULT 0
);
```

#### Operations to make durable

| Operation | Current behavior | Required behavior |
|---|---|---|
| `create_session(agent_id, parent_id, scope)` | In-memory struct | INSERT into `sessions` table |
| `end_session(id, status)` | Mutates in-memory | UPDATE sessions SET status=?, ended_at=NOW() |
| `list_sessions(filter)` | Filters in-memory vec | SELECT with WHERE clauses |
| `get_session(id)` | Finds in memory | SELECT * FROM sessions WHERE id=? |

#### Session-commit association

Similar to epochs, add a `session_id` column to commits:
```sql
ALTER TABLE commits ADD COLUMN session_id TEXT REFERENCES sessions(id);
```

---

## Integration points

### SqliteStorage crate (`crates/agentstategraph-storage/src/sqlite.rs`)

This is where the tables are created and queried. The pattern is already established for `objects`, `commits`, and `refs`. Follow the same pattern:

1. Add `CREATE TABLE IF NOT EXISTS` to the `init()` method
2. Add methods: `create_epoch()`, `seal_epoch()`, `list_epochs()`, `get_epoch()`
3. Add methods: `create_session()`, `end_session()`, `list_sessions()`, `get_session()`
4. Add the `epoch_id` and `session_id` columns to commits (migration-safe: use `ALTER TABLE ADD COLUMN IF NOT EXISTS` or check column existence first)

### Repository crate (`crates/agentstategraph/src/repo.rs`)

The `Repository` struct delegates to the storage backend. Epoch and session methods currently operate on in-memory fields. Change them to call through to storage:

```rust
// Before (in-memory):
pub fn create_epoch(&self, id: &str, desc: &str, root_intents: Vec<String>) -> Result<Epoch> {
    let mut epochs = self.epochs.lock().unwrap();
    // ... push to vec
}

// After (persisted):
pub fn create_epoch(&self, id: &str, desc: &str, root_intents: Vec<String>) -> Result<Epoch> {
    let epoch = Epoch { id, desc, status: Active, ... };
    self.storage.create_epoch(&epoch)?;
    Ok(epoch)
}
```

### MCP server (`crates/agentstategraph-mcp/src/server.rs`)

No changes needed — the MCP tools already call `self.repo.create_epoch()` etc. Once the repo delegates to storage, persistence is automatic.

### MemoryStorage (`crates/agentstategraph-storage/src/memory.rs`)

The in-memory backend should also implement the new storage trait methods, keeping epochs and sessions in `Vec`s as they do today. This keeps tests working.

---

## Migration safety

The `commits` table already exists with data. Adding columns must be non-destructive:

```sql
-- Safe: SQLite supports ADD COLUMN on existing tables
ALTER TABLE commits ADD COLUMN epoch_id TEXT;
ALTER TABLE commits ADD COLUMN session_id TEXT;
```

These columns will be NULL for all existing commits, which is correct — they predate the epoch/session system.

Use the existing `agentstategraph-migrate` framework if a formal migration is preferred, but `ALTER TABLE ADD COLUMN` is safe enough for this case since the columns are nullable with no constraints.

---

## Storage trait changes

The `Storage` trait (in `crates/agentstategraph-storage/src/lib.rs` or similar) needs new methods:

```rust
// Epoch persistence
fn create_epoch(&self, epoch: &Epoch) -> Result<()>;
fn seal_epoch(&self, id: &str, summary: &str, sealed_at: DateTime<Utc>) -> Result<()>;
fn list_epochs(&self) -> Result<Vec<Epoch>>;
fn get_epoch(&self, id: &str) -> Result<Option<Epoch>>;

// Session persistence
fn create_session(&self, session: &Session) -> Result<()>;
fn end_session(&self, id: &str, status: SessionStatus, ended_at: DateTime<Utc>) -> Result<()>;
fn list_sessions(&self, agent_id: Option<&str>) -> Result<Vec<Session>>;
fn get_session(&self, id: &str) -> Result<Option<Session>>;

// Commit association
fn set_commit_epoch(&self, commit_id: &[u8], epoch_id: &str) -> Result<()>;
fn set_commit_session(&self, commit_id: &[u8], session_id: &str) -> Result<()>;
```

Both `SqliteStorage` and `MemoryStorage` must implement these. `PostgresStorage` (if it exists) should also be updated but can be deferred.

---

## Testing

Add tests for:

1. **Epoch round-trip**: create → list → seal → list (verify sealed_at and summary)
2. **Epoch persistence**: create epoch, drop the storage, reopen, verify epoch still exists
3. **Seal enforcement**: create epoch, seal it, attempt to add a commit with that epoch_id → should fail
4. **Session round-trip**: create → list → end → list (verify ended_at and status)
5. **Session persistence**: same pattern as epoch
6. **Commit association**: create commit with epoch_id, query commits by epoch, verify association
7. **Migration safety**: open a DB with existing commits (no epoch_id column), run init, verify column added and existing commits unaffected

---

## What NOT to change

- **Speculations**: Keep in-memory. They're ephemeral by design (O(1) create, O(1) discard).
- **Plans/Tasks**: Already persisted correctly via the state tree (commits at `/plans/*`). Don't move them to a separate table.
- **Policies**: Same — stored as state at `/policies/*`. Don't create a separate table.
- **The commit format**: Don't change the commit blob format. Just add nullable columns to the table.

---

## Acceptance criteria

1. `list_epochs()` returns epochs that were created in a previous MCP session
2. A sealed epoch survives process restart with its summary and sealed_at intact
3. `list_sessions()` returns sessions from previous MCP sessions
4. Existing databases (without the new columns) open without error
5. All existing tests still pass
6. The Stack Viewer's epoch panel shows epochs after page refresh (currently shows nothing)

---

## Files to modify

| File | Changes |
|---|---|
| `crates/agentstategraph-storage/src/lib.rs` | Add epoch/session methods to Storage trait |
| `crates/agentstategraph-storage/src/sqlite.rs` | Implement epoch/session tables + queries |
| `crates/agentstategraph-storage/src/memory.rs` | Implement in-memory epoch/session storage |
| `crates/agentstategraph/src/repo.rs` | Delegate epoch/session calls to storage instead of in-memory vecs |
| `crates/agentstategraph/src/types.rs` (or equivalent) | Ensure Epoch/Session types are defined and serializable |

The MCP server (`server.rs`) should NOT need changes — it already calls `self.repo.create_epoch()` etc.
