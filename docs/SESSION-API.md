# Session API — fallible since 0.6.5-beta.1

_For consumers of `agentstategraph::SessionManager`. Written during the
0.6.75 audit sweep (§4 of the milestone plan)._

## What changed

In 0.6.0-beta.1 and earlier, `SessionManager::create` / `list` / `get` /
`end` were infallible — the session registry was an in-memory
`RwLock<HashMap>` with no failure modes a caller could observe.

In **0.6.5-beta.1** the session registry moved to storage (SQLite is
durable; Postgres and IndexedDB followed in 0.6.75-beta.1). Sessions
now survive process restart. That required the methods to become
fallible so storage errors can surface:

```rust
// Before (0.6.0 and earlier):
let session = repo.sessions().create(...);              // returns Session

// After (0.6.5+):
let session = repo.sessions().create(...)?;             // returns Result<Session, SessionError>
```

## Impact on callers

### Inside this repo

Three call sites were patched as part of 0.6.5 commit `05b82af`:

- `crates/agentstategraph-mcp/src/server.rs` — the `sessions` tool and
  a handful of others now propagate errors as string envelopes
- `bindings/typescript/src/lib.rs` — the napi wrapper maps errors to
  a JS-side exception
- `crates/agentstategraph/examples/multi_agent.rs` — switched to
  `.unwrap()` since the example doesn't need sophisticated error
  handling

Every in-repo call site that touches `.sessions()` now uses `?`,
`.unwrap()`, or a `match` arm.

### Outside this repo

Consumers that pinned on 0.6.0-beta.1 or earlier will see compile
errors when they bump to 0.6.5-beta.1 or later. The fix is mechanical:

- For each `SessionManager` method call, add `?` (if the function
  returns a suitable error type) or `.unwrap()` (if not).
- If the return type of the outer function is not already fallible,
  either:
  - Make it return `Result<_, Box<dyn Error>>` (or similar), or
  - Handle the error explicitly with `match` / `if let Err`.

The error type is `agentstategraph::SessionError`, which converts
from `agentstategraph_storage::StorageError` via `#[from]`.

### Bindings (Python / TypeScript / Go / WASM / C FFI)

As of 0.6.75-beta.1 the bindings do **not** expose `SessionManager`
directly. Session visibility through bindings is scheduled for the
0.7.0-beta.1 "bring all existing bindings current" milestone.

When that work lands, each binding will surface a `Session` wrapper
and propagate `SessionError` as the language's native exception type:

- Python: `SessionException` (inherits from `Exception`)
- TypeScript: a typed `Error` subclass with `code` property
- Go: standard `error` return on every method
- WASM: JS `Error` with a `code` property
- C FFI: returns a `char*` error string (non-NULL means error), owned
  by the caller and freed via `agentstategraph_free_string`

## Error semantics

`SessionError` wraps `StorageError` and adds session-specific variants:

| Variant | Meaning |
|---|---|
| `Storage(StorageError)` | Any underlying storage error (DB connection lost, corruption, etc.) |
| `NotFound(id)` | The session id does not exist |
| `OutOfScope { path, scope }` | A path write was attempted outside the session's `path_scope` |
| `EndedAlready(id)` | `end` was called on a session that was already not-Active |

`StorageError` further surfaces:

| Variant | Meaning |
|---|---|
| `SessionEnded { id }` | Returned by `set_commit_session` if the session is already ended — the commit association silently drops; callers should clear `active_session` first |
| `Backend(msg)` | A backend-specific error (Postgres connection, SQLite locked, IndexedDB transaction aborted) |
| `Serialization(msg)` | Failed to (de)serialize a session row |

## Migration checklist for an external consumer

1. Bump `agentstategraph` and `agentstategraph-storage` to 0.6.5-beta.1
   or later in `Cargo.toml`.
2. `cargo build` — compile errors will point at each call site that
   needs updating.
3. For each error, add `?` or `.unwrap()` (favour `?`).
4. Re-run tests.

Typical diff:

```diff
-let session = repo.sessions().create(...);
-let list    = repo.sessions().list(None);
+let session = repo.sessions().create(...)?;
+let list    = repo.sessions().list(None)?;
```

## Rationale

The old signatures would eventually have had to change anyway to
support Postgres / IndexedDB backends that CAN fail. Making the
signatures fallible up front is less churn than shipping a broken API
and revising it once errors become observable.

## Related

- `spec/PERSISTENCE_SPEC.md` — why sessions moved to storage
- `crates/agentstategraph/src/session.rs` — the `SessionManager`
  impl
- `crates/agentstategraph-storage/src/traits.rs` — the
  `SessionStore` trait
