# WASM Policy Evaluator ABI

_Contract for third-party WASM policy evaluators targeting the
`agentstategraph-policy-wasm` host. Shipped in 0.7.5-beta.1._

The `WasmEvaluator` host embeds [wasmtime](https://wasmtime.dev/) and
loads a policy module for every `Policy` whose `external_evaluator`
field is `Some(Wasm { source })`. The source is fetched from inline
bytes, a file path, or a state-tree commit-ref depending on the
`EvaluatorSource` variant, compiled to a `Module`, instantiated, and
invoked via this ABI.

## Scope

The ABI is deliberately minimal — one entry point, two allocator
helpers, JSON on both sides. It's stable for the 0.7.x line.

## Exports

A conforming module MUST export exactly the following WASM functions:

### `asg_alloc(len: i32) -> i32`

Allocate `len` bytes of linear memory and return a pointer (non-zero).
The host uses this to write the input JSON into the module. Returning
`0` is an error (out of memory).

### `asg_free(ptr: i32, len: i32)`

Free a region previously returned by `asg_alloc`. The host calls this
once after reading the evaluation result.

### `asg_evaluate(ptr: i32, len: i32) -> i64`

Evaluate the JSON request buffer at `[ptr, ptr+len)` and return a
packed i64 that encodes the result buffer's `(ptr, len)`:

```
result_i64 = ((result_ptr as i64) << 32) | (result_len as i64)
```

The low 32 bits are the byte length; the high 32 bits are the pointer.
The host reads `result_len` bytes from `result_ptr`, parses them as
UTF-8 JSON, then calls `asg_free(result_ptr, result_len)`.

Returning `0` is interpreted as "evaluator error"; the host treats the
policy as skipped (the same as POLICY_V1.md §11 soft-model behavior).

## Request shape

The host writes this JSON into the module's memory before calling
`asg_evaluate`:

```json
{
  "situation": { "<key>": "<value>", ... },
  "action": "<action-name>",
  "agent_id": "<agent-identifier>",
  "source": {
    "kind": "inline" | "file_path" | "commit_ref",
    "body": "<body when kind=inline>",
    "path": "<path when kind=file_path | commit_ref>"
  }
}
```

`situation` is a flat string-to-string map (POLICY_V1.md §2). The
`source` block is informational — the module already has its code
loaded; this is just context for modules that multiplex multiple
policies through a single binary.

## Response shape

The module writes a `Decision` JSON buffer using the main crate's
serde encoding:

```json
{ "kind": "allow",             "matched_policy": "...", "preconditions": [] }
{ "kind": "deny",              "matched_policy": "...", "reason": "..." }
{ "kind": "require_approval",  "matched_policy": "...", "approvers": [...],
  "timeout": <ms|null>, "fallback": {"kind": "block"|...}, "approval_task_path": null }
{ "kind": "no_policy_match" }
```

A malformed response (invalid UTF-8, not valid JSON, or not a
recognized `kind`) is treated as "evaluator error" and the policy is
skipped.

## Memory model

- The module owns its linear memory; the host only interacts through
  `asg_alloc` / `asg_free`.
- Pointers are 32-bit; the ABI targets `wasm32-unknown-unknown`.
- The host grants no imports beyond the module's own memory — no WASI,
  no system access. Evaluators must be pure functions of their input.

## Error convention

Errors are communicated by returning `0` from `asg_evaluate`. The
module is expected to handle its own panics; a trapped module has
its policy skipped and the error surfaced through the host's
tracing (§4c).

## Minimal reference module

A bump-allocator fixture in WAT lives at
`crates/agentstategraph-policy-wasm/tests/fixtures/bump_allocator.wat`
and documents the smallest module that implements this ABI.

## Versioning

The ABI is versioned through the sibling crate's semver. 0.7.x
modules will continue to load on 0.7.x hosts without modification.
Breaking changes (e.g. adopting a richer request shape) will bump
the minor version and carry a migration note in CHANGELOG.
