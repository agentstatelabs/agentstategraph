---
title: MCP Server
description: Connect AgentStateGraph to Claude Code, GPT, or any MCP-compatible agent.
---

## Build and Run

```bash
git clone https://github.com/agentstatelabs/agentstategraph.git
cd AgentStateGraph
cargo build --release -p agentstategraph-mcp
cargo run --release -p agentstategraph-mcp
# Creates ./agentstategraph.db, listens on stdio
```

## Connect to Claude Code

Add to `~/.claude.json` or your project's `.mcp.json`:

```json
{
  "mcpServers": {
    "agentstategraph": {
      "command": "/path/to/AgentStateGraph/target/release/agentstategraph-mcp"
    }
  }
}
```

Or run from source:

```json
{
  "mcpServers": {
    "agentstategraph": {
      "command": "cargo",
      "args": ["run", "--release", "-p", "agentstategraph-mcp", "--manifest-path", "/path/to/AgentStateGraph/Cargo.toml"]
    }
  }
}
```

Restart Claude Code. The 77 AgentStateGraph tools appear automatically.

## HTTP REST API

The same binary also supports HTTP mode, a REST API:

```bash
cargo run --release -p agentstategraph-mcp -- --http --port 3001
```

```bash
curl http://localhost:3001/api/health
curl http://localhost:3001/api/stats/main
curl http://localhost:3001/api/state/main?path=/cluster/name
curl "http://localhost:3001/api/blame/main?path=/cluster/name"
curl "http://localhost:3001/api/state/main/search?query=mesh"
```

### Logs, health and metrics

The server logs to stderr. In HTTP mode it logs each request at `info`. Fields use [OpenTelemetry's names](https://opentelemetry.io/docs/specs/semconv/http/):

- `http.request.method`;
- `url.path` (no query);
- `http.route`, the route template;
- `http.response.status_code`;
- `http.server.request.duration` (seconds);
- `asg.api_key.id`, the API key's id, never the key itself.

Set `ASG_LOG` to change the level (`debug`, or a filter like `agentstategraph_mcp=debug`). `ASG_LOG_FORMAT` chooses the form: `auto` (the default: text on a terminal, one flat JSON object per line otherwise, as in a container), `text`, `json` or `ecs` (Elastic Common Schema). A JSON line:

```json
{"asg.api_key.id":"key_3f9c2a71d04e8b65","http.request.method":"GET","http.response.status_code":200,"http.route":"/api/state/{ref_name}","http.server.request.duration":0.000855667,"level":"INFO","message":"request","network.protocol.version":"1.1","server.address":"localhost","server.port":3001,"span_id":"69a6763a47ef0951","target":"oxtel_tower::server","timestamp":"2026-10-08T15:57:08.711714Z","trace_id":"1b2e39eb15704924f95003629fbcd488","url.path":"/api/state/main","url.scheme":"http"}
```

`/api/health` reports that the process is serving (liveness). `/api/ready` also checks that the store answers, and returns `503` if it doesn't (readiness). `agentstategraph-mcp healthcheck` checks `/api/ready` from the same machine, for container health checks.

`--metrics` serves Prometheus metrics at `/metrics`, named as OpenTelemetry's HTTP metrics are for Prometheus:

- `http_server_request_duration_seconds` is a histogram by `http_request_method`, `http_route` (the template, so ref names never become labels), `http_response_status_code` and, for server errors, `error_type`. Its `_count` is the request count.
- `http_server_active_requests` counts requests in progress.
- `target_info` carries the version.

With `--auth`, scraping it needs an admin key.

With the `otlp` feature built in, set `OTEL_EXPORTER_OTLP_ENDPOINT` to send traces, metrics and these logs to an OpenTelemetry Collector or any OTLP backend. [Observability](https://github.com/agentstatelabs/agentstategraph/blob/main/docs/OBSERVABILITY.md) lists every name.

### Errors

An error response is JSON with a message, a stable code, and the id of the request's trace:

```json
{"error": "branch already exists: main", "code": "branch_already_exists", "trace_id": "4bf92f3577b34da6a3ce929d0e0e4736"}
```

Branch on the code; the message is for people and may change. Quote the trace id, or the `x-request-id` header every response carries, when reporting a problem: the server's log lines carry both. A request may send its own `x-request-id` (up to 128 visible ASCII characters), and the response echoes it; otherwise the server makes one. The status follows from the code:

| Status | Codes |
|---|---|
| `404` | `branch_not_found`, `ref_not_found`, `commit_not_found`, `namespace_not_found`, `object_not_found`, `path_not_found`, `speculation_not_found`, `key_not_found` |
| `409` | `write_conflict`, `merge_conflict`, `merge_would_delete`, `branch_already_exists`, `namespace_already_exists`, `ref_already_exists`, `epoch_sealed`, `session_ended` |
| `400` | `invalid_request`, `invalid_operation`, `invalid_json`, `ambiguous_commit_prefix`, `type_mismatch`, `not_a_container`, `index_out_of_bounds` |
| `403` | `reserved_path`, `commit_ref_not_allowed`, `key_bound_to_namespace`, `cross_namespace_denied`, `taint_blocked` |
| `429` | `too_many_speculations` |
| `503` | `not_initialized` |
| `500` | `storage_error`, `serialization_error`, `integrity_violation`, `key_store_failed`, `internal` |

A `write_conflict` means another writer moved the branch first; it's safe to retry. MCP tools report the same codes at the start of their error text, `Error [write_conflict]: …`, and mark the result as an error (`isError`). So does `agentstategraph_commit_spec` when policy refuses a promotion; its text is then the Decision JSON.

The limits, authentication and host check below answer before a request reaches the API, in the same form. Their codes are the reasons the server counts them by, in `asg.http.rejections`:

| Status | Code | Why |
|---|---|---|
| `401` | `unauthenticated` | no API key, or one that isn't valid |
| `403` | `forbidden` | the key can't use this endpoint, such as a non-admin key on an admin route |
| `403` | `host_refused` | the request named a host this server doesn't answer to |
| `413` | `body_too_large` | the body is over the limit |
| `429` | `rate_limited` | over the rate limit; `Retry-After` says when to try again |
| `503` | `over_capacity` | the server is handling its limit of requests; `Retry-After: 1` |
| `503` | `timed_out` | the request ran past the timeout; a write may still have completed |

### Limits

Each request body may be up to 4 MiB (`--max-body-bytes`); a larger one gets `413`. A request still running after 30 seconds (`--request-timeout-secs`, `0` for none) gets `503`; a write may still complete after that, so check before retrying it. At most 256 requests are handled at once (`--max-concurrent-requests`); more get `503` with `Retry-After: 1` rather than queueing. `/api/health` is never turned away.

The rate limit (`--rate-limit-rpm`, default 600 a minute) applies per API key when `--auth` is on, and per client IP otherwise. Requests without a valid key count against their IP. Every setting has an environment variable too; see `--help`.

### Browsers and host names

Browser pages served from localhost (any port) can call the API. To let a page from another origin call it, pass `--cors-origin https://app.example.com` (repeatable, or `ASG_CORS_ORIGINS` as a comma-separated list). `--cors-origin '*'` allows any origin; only do that with `--auth`.

Bound to loopback, as it is by default, the server only answers requests addressed to `localhost`, `127.0.0.1` or `[::1]`. That stops another website from reaching it through DNS rebinding, where the site points a domain it controls at your machine. Behind a reverse proxy on the same machine that forwards its own host name, allow that name with `--allowed-host asg.example.com` (or `ASG_ALLOWED_HOSTS`). Bound to another address, the server answers any host name unless you list some.

Full endpoint list:

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/health` | Liveness: the process is serving |
| GET | `/api/ready` | Readiness: the store answers (`503` if not) |
| GET | `/api/stats/:ref` | Summary statistics |
| GET | `/api/state/:ref?path=/x` | Read state value |
| GET | `/api/state/:ref/paths` | List all paths |
| GET | `/api/state/:ref/search?query=x` | Full-text search values |
| POST | `/api/state/:ref/set` | Write value with intent |
| POST | `/api/state/:ref/delete` | Delete value with intent |
| GET | `/api/log/:ref` | Commit log |
| GET | `/api/blame/:ref?path=/x` | Blame a path |
| GET | `/api/diff?ref_a=x&ref_b=y` | Diff two refs |
| POST | `/api/query/:ref` | Query with composable filters |
| GET | `/api/graph/:ref` | Commit DAG |
| GET | `/api/branches` | List branches |
| POST | `/api/branches` | Create branch |
| POST | `/api/merge` | Merge branches |
| GET | `/api/epochs` | List epochs |
| POST | `/api/epochs` | Create epoch |
| POST | `/api/epochs/seal` | Seal epoch |
| GET | `/api/intents/:ref` | Intent decomposition tree |

## Connect to Other MCP Clients

Any MCP client that supports stdio transport works. Point it at the `agentstategraph-mcp` binary.

## Configuration

```bash
agentstategraph-mcp [OPTIONS]

OPTIONS:
  -s, --storage <TYPE>  Storage backend: sqlite (default) or memory
  -p, --path <PATH>     SQLite database path (default: ./agentstategraph.db)
      --http            Run as HTTP REST API instead of MCP stdio
      --port <PORT>     HTTP port (default: 3001, requires --http)
      --policy-fail-safe <deny|allow>
                        When no policy matches: deny (default) or allow
  -h, --help            Print help with full endpoint list
```

Each commit records the trace it was made in, shown as `trace_id` and `span_id` in `log` and `blame`; set `ASG_TRACE_LINKS=off` to stop recording them. See [Observability](https://github.com/agentstatelabs/agentstategraph/blob/main/docs/OBSERVABILITY.md).

## Available Tools (77)

The most-used tools are listed below by category. See the full [MCP Tools Reference](/reference/mcp-tools/) for every tool with parameters and example payloads.

### State Operations

| Tool | Description |
|------|-------------|
| `agentstategraph_get` | Read a value at any branch/path. Use `/` for entire state. |
| `agentstategraph_set` | Write a value with intent metadata (category, description, reasoning, confidence). |
| `agentstategraph_delete` | Remove a value, creating a commit with intent. |

### Branching

| Tool | Description |
|------|-------------|
| `agentstategraph_branch` | Create a branch from any ref. Supports namespaced names. |
| `agentstategraph_list_branches` | List all branches, optionally filtered by namespace prefix. |
| `agentstategraph_merge` | Schema-aware three-way merge. Returns conflicts if auto-resolution fails. |
| `agentstategraph_diff` | Structured typed diff between two refs (not text diffs). |

### Speculation

| Tool | Description |
|------|-------------|
| `agentstategraph_speculate` | Create a lightweight O(1) speculation from a ref. |
| `agentstategraph_spec_modify` | Apply set/delete operations within a speculation. |
| `agentstategraph_compare` | Compare multiple speculations side-by-side. |
| `agentstategraph_commit_spec` | Promote a speculation to a real commit on its base branch, if policy allows it. |
| `agentstategraph_discard` | Discard a speculation. All changes freed immediately. |

### Query and Audit

| Tool | Description |
|------|-------------|
| `agentstategraph_log` | Commit history with full intent, reasoning, and metadata. |
| `agentstategraph_query` | Composable filters: agent, category, tags, reasoning text, confidence range. |
| `agentstategraph_blame` | Find which commit last modified a path and why. |

### Namespaces

| Tool | Description |
|------|-------------|
| `agentstategraph_create_namespace` | Create a namespace: a separate set of branches (idempotent). |
| `agentstategraph_list_namespaces` | List all namespaces. |
| `agentstategraph_delete_namespace` | Delete a namespace and its refs (not `default`). |
| `agentstategraph_cross_namespace_merge` | Merge across namespaces (always refused for now). |

### Epochs

| Tool | Description |
|------|-------------|
| `agentstategraph_create_epoch` | Create an epoch to group related work. |
| `agentstategraph_seal_epoch` | Seal an epoch (immutable, tamper-evident). Cannot be undone. |
| `agentstategraph_archive_epoch` | Move a sealed epoch to archived (cold storage). |
| `agentstategraph_export_epoch` | Export a sealed/archived epoch as a JSON audit bundle. |
| `agentstategraph_enter_epoch` / `agentstategraph_exit_epoch` | Set / clear the active epoch. |
| `agentstategraph_list_epochs` | List all epochs with status, dates, and commit counts. |

### Sessions

| Tool | Description |
|------|-------------|
| `agentstategraph_create_session` | Create an agent session (optionally namespace- and path-scoped). |
| `agentstategraph_enter_session` / `agentstategraph_exit_session` | Set / clear the active session. |
| `agentstategraph_sessions` | List active agent sessions with parent-child relationships. |

### Explorer & Viewer

| Tool | Description |
|------|-------------|
| `agentstategraph_list_paths` | List all leaf paths in the state tree under a prefix. |
| `agentstategraph_get_tree` | Get entire subtree as nested JSON (batch read). |
| `agentstategraph_search` | Full-text search across state values and key names. |
| `agentstategraph_stats` | Summary statistics: commits, branches, paths, epochs, agents. |
| `agentstategraph_commit_graph` | Commit DAG with parents, agents, categories for visualization. |
| `agentstategraph_intent_tree` | Intent decomposition hierarchy across agents. |

### Maintenance

| Tool | Description |
|------|-------------|
| `agentstategraph_history` | Project-history metrics: velocity, intent mix, authorship, milestones. |
| `agentstategraph_gc_dry_run` | Report what garbage collection would reclaim, changing nothing. |
| `agentstategraph_gc_sweep` | Sweep under a retention policy; deletes only with `mutate: true`. |
| `agentstategraph_gc_vacuum` | Compact the store after a sweep. |

### Plans & Tasks

| Tool | Description |
|------|-------------|
| `agentstategraph_create_plan` / `agentstategraph_list_plans` / `agentstategraph_get_plan` | Manage plans. |
| `agentstategraph_add_task` / `agentstategraph_list_tasks` | Add and list tasks. |
| `agentstategraph_start_task` / `agentstategraph_complete_task` / `agentstategraph_abandon_task` | Drive the task state machine. |
| `agentstategraph_assign_task` / `agentstategraph_next_task` | Assign work; pull the next unblocked task. |

See the [Plans & Tasks guide](/guides/plans-tasks/).

### Policy

| Tool | Description |
|------|-------------|
| `agentstategraph_policy_propose` / `agentstategraph_policy_ratify` / `agentstategraph_policy_supersede` | Policy lifecycle. |
| `agentstategraph_policy_list` / `agentstategraph_policy_show` / `agentstategraph_policy_history` | Inspect policies. |
| `agentstategraph_policy_evaluate` / `agentstategraph_policy_evaluate_change` / `agentstategraph_policy_check_tokens` | Evaluate authorization and cost-of-change. |
| `agentstategraph_policy_sign` / `agentstategraph_policy_verify` | Ed25519 signing and verification. |
| `agentstategraph_policy_evaluate_change_with_taints` | Compose policy + taint into one decision. |

See the [Policy guide](/guides/policy/).

### Taint & Quarantine

| Tool | Description |
|------|-------------|
| `agentstategraph_taint` / `agentstategraph_untaint` | Apply / remove a taint (warn, block, review, isolate). |
| `agentstategraph_quarantine` / `agentstategraph_unquarantine` | Restrict / release a path to authorized agents. |
| `agentstategraph_watch` / `agentstategraph_unwatch` | Advisory watch with optional threshold escalation. |
| `agentstategraph_list_taints` / `agentstategraph_check_taint` | List marks; check a path including ancestors. |

See the [Taint & Quarantine guide](/guides/taint/).

### Reminders

| Tool | Description |
|------|-------------|
| `agentstategraph_reminder_create` / `agentstategraph_reminder_list` | Create and list pull-based reminders. |
| `agentstategraph_reminder_remind_me` | Get all currently due reminders, by priority. |
| `agentstategraph_reminder_snooze` / `agentstategraph_reminder_approve` / `agentstategraph_reminder_cancel` | Manage reminder state. |
| `agentstategraph_reminder_record_execution` | Record an execution result; advances repeating schedules. |

See the [Reminders guide](/guides/reminders/).

## Example Conversation

```
You: Store the cluster config under /cluster
Agent: [calls agentstategraph_set with path="/cluster/name", value="prod", ...]

You: Try two network approaches and compare them
Agent: [calls agentstategraph_speculate twice, agentstategraph_spec_modify on each,
        agentstategraph_compare to diff them, agentstategraph_commit_spec on winner]

You: Who changed the network config and why?
Agent: [calls agentstategraph_blame with path="/cluster/network"]
```

See the full [MCP Tools Reference](/reference/mcp-tools/) for parameters and example payloads.
