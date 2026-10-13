# Observability

AgentStateGraph's logs, traces and metrics follow OpenTelemetry, through [oxtel](https://github.com/agentstatelabs/oxtel). This page is the contract: what is recorded, under which names and units, and what never is. Change it before changing the code.

## Getting it

- **Logs** go to standard error. Standard output stays free for MCP over stdio. `ASG_LOG` sets the level, for example `info` or `debug,agentstategraph_core=trace`; `RUST_LOG` is the fallback. The default is `info` when serving HTTP and `warn` as an MCP stdio server.
- **Log format.** `ASG_LOG_FORMAT` is `auto` (the default: text on a terminal, one JSON object per line otherwise), `text`, `json` or `ecs`. `OXTEL_LOG_FORMAT` does the same and wins if both are set.
- **Metrics for Prometheus.** `--metrics` (or `ASG_METRICS=1`) serves `/metrics`. With `--auth`, it needs an admin key.
- **Traces, metrics and logs over OTLP.** Build with the `otlp` feature and set `OTEL_EXPORTER_OTLP_ENDPOINT`, for example `http://collector:4318`. Without the feature, the server contains no OTLP code. With it, nothing is sent until an endpoint is set. The standard `OTEL_*` variables apply: `OTEL_SERVICE_NAME`, `OTEL_RESOURCE_ATTRIBUTES`, `OTEL_TRACES_SAMPLER` and the rest.
- At startup, at `info`, one line from the target `oxtel` says where log lines go and where each signal is sent. When sending fails, a warning says where, why and what to check.

## Pinned conventions

| Conventions | Version |
|---|---|
| OpenTelemetry semantic conventions | v1.44.0 (schema URL `https://opentelemetry.io/schemas/1.44.0`) |
| MCP and GenAI conventions | semantic-conventions-genai at commit `6fd0d763`, Development status: names may change |

Both come from `oxtel-semconv`; moving to a newer release is a deliberate change to this page.

## What is recorded

### Resource

Every signal carries `service.name` (`agentstategraph` unless `OTEL_SERVICE_NAME` says otherwise), `service.version`, `service.instance.id`, and what oxtel detects about the host, process and container. A process that serves a single tenant can name it with `OTEL_RESOURCE_ATTRIBUTES=asg.tenant.id=<id>`.

### HTTP requests

Each request gets a server span named `{method} {route}`, such as `GET /state/{ref_name}`, continuing the caller's trace when it sends W3C `traceparent`.

| Attribute | Notes |
|---|---|
| `http.request.method` | `_OTHER` for a nonstandard method, with the method in `http.request.method_original` |
| `http.route` | the route template, never the filled-in path |
| `url.path` | the path, which holds branch names but never state paths or values; no query string |
| `url.scheme`, `server.address`, `server.port`, `network.protocol.version`, `user_agent.original` | as the HTTP conventions define them |
| `http.response.status_code` | |
| `error.type` | for 5xx responses |
| `asg.api_key.id` | the id of the API key used, recorded by the auth layer; never the key |
| `http.request.header.x-request-id` | the request's id: the `x-request-id` it sent, if that's 1 to 128 visible ASCII characters, or a new UUID. Every response echoes it |
| `asg.error.code` | an error response's stable code, such as `branch_not_found` or `rate_limited`; `error.type` stays the status, as the HTTP conventions have it |
| `asg.rejection.reason` | why the server turned the request away before a handler ran, when it did |

Each finished request also logs `request` at `info`, or at `error` for a 5xx, with the status and `http.server.request.duration` in seconds.

Error bodies carry the request's `trace_id` beside `error` and `code`, so a client can point at the trace and its log lines.

A rejection is a request turned away before a handler ran. Its response's `code` is the reason, and it's counted by that reason:

| Reason | Status |
|---|---|
| `rate_limited` | 429, over the rate limit |
| `over_capacity` | 503, the server is handling its limit of requests |
| `timed_out` | 503, past the request timeout |
| `body_too_large` | 413, over the body limit |
| `unauthenticated` | 401, no API key or an invalid one |
| `forbidden` | 403, a key that can't use the endpoint |
| `host_refused` | 403, a Host the server doesn't answer to |

| Metric | Instrument and unit | Labels | Prometheus name |
|---|---|---|---|
| `http.server.request.duration` | histogram, `s` | `http.request.method`, `url.scheme`, `network.protocol.version`, `http.route`, `http.response.status_code`, `error.type` | `http_server_request_duration_seconds` |
| `http.server.active_requests` | up-down counter, `{request}` | `http.request.method`, `url.scheme` | `http_server_active_requests` |
| `asg.http.rejections` | counter | `asg.rejection.reason` | `asg_http_rejections_total` |

Requests the Host check refuses are logged and counted too: the observability layer sits outside the origin policy.

### MCP

Each request and notification gets a span named `{mcp.method.name} {target}`, such as `tools/call commit_spec`. It continues the client's trace when `params._meta` carries `traceparent`, over stdio as well as HTTP.

| Attribute | Notes |
|---|---|
| `mcp.method.name`, `mcp.protocol.version`, `jsonrpc.request.id` | |
| `gen_ai.tool.name`, with `gen_ai.operation.name` = `execute_tool` | for tool calls |
| `mcp.resource.uri`, `gen_ai.prompt.name` | for resources and prompts |
| `network.transport` | `pipe` over stdio |
| `error.type` | `tool_error` when a tool reports an error, such as a policy refusal; a JSON-RPC code when the server failed |
| `rpc.response.status_code` | the JSON-RPC error code, when there is one |

| Metric | Instrument and unit | Labels |
|---|---|---|
| `mcp.server.operation.duration` | histogram, `s` | `mcp.method.name`, `gen_ai.tool.name`, `gen_ai.operation.name`, `gen_ai.prompt.name`, `mcp.protocol.version`, `network.transport`, `error.type`, `rpc.response.status_code` |
| `mcp.server.session.duration` | histogram, `s` | `mcp.protocol.version`, `network.transport` |

Tool arguments and results are never recorded.

### Repository and policy

The library records its own work as spans inside the request or tool call that asked for it. Writes, speculations, history extraction, garbage collection, the taint check and policy evaluation get spans at `info`. Reads (`asg.repo.get`, `asg.repo.log`, `asg.repo.diff`, `asg.repo.blame`) get spans at `debug` only, so they're recorded only when the trace filter asks for them.

| Span | For | Attributes |
|---|---|---|
| `asg.repo.set`, `asg.repo.delete`, `asg.repo.merge` | a write, including `set_json_cas` | `asg.ref.name`, `gen_ai.agent.id`, `asg.intent.category`, `asg.commit.id`, `asg.ref.cas_attempts` |
| `asg.speculation.create`, `asg.speculation.commit`, `asg.speculation.discard` | a speculation's life | `asg.speculation.id`; committing also records the write's attributes |
| `asg.history.extract` | folding commits into the history tables | `asg.history.commits_processed` |
| `asg.gc.dry_run`, `asg.gc.sweep`, `asg.gc.vacuum` | garbage collection | `asg.gc.mutate`, `asg.gc.refused`, `asg.gc.objects_deleted`, `asg.gc.bytes_reclaimed` |
| `asg.taint.check` | the check before each write | |
| `asg.policy.evaluate`, `asg.policy.evaluate_change` | a policy decision | `asg.policy.decision` |
| `asg.policy.external` | one external evaluator's part in it | `asg.policy.evaluator`, `asg.policy.decision` |

A span that fails has an error status and `error.type`, the error's stable code, such as `write_conflict` or `taint_blocked`. Error messages aren't recorded: they can name state paths.

Events:

- `debug`: the ref moved under a write, which rebuilds its commit on the new head (`asg.ref.cas_attempts`);
- `warn`: a write gave up because the ref kept moving (`asg.operation.name`, `error.type` = `write_conflict`);
- `info`: a taint refused a write (`asg.taint.id`, `asg.taint.effect`);
- `info`: no policy matched, so the server's fail-safe decided (`asg.policy.fail_safe`);
- `warn`: an external evaluator failed, so its policy was skipped (`asg.policy.evaluator`, `asg.policy.id`, `error.type`). Its message isn't recorded, since an evaluator's output can echo its input.

External evaluators that run as processes, Rego and Cedar, are killed after 10 seconds, or the limit their runner sets with `with_timeout`. A timeout's `error.type` is `evaluator_timeout`.

| Metric | Instrument and unit | Labels | Prometheus name |
|---|---|---|---|
| `asg.commit.duration` | histogram, `s` | `asg.operation.name`, `error.type` | `asg_commit_duration_seconds` |
| `asg.ref.cas_retries` | counter | `asg.operation.name` | `asg_ref_cas_retries_total` |
| `asg.policy.decisions` | counter | `asg.policy.decision` | `asg_policy_decisions_total` |
| `asg.policy.evaluator.errors` | counter | `asg.policy.evaluator`, `error.type` | `asg_policy_evaluator_errors_total` |

`asg.policy.decisions` counts what the policies decided, before the fail-safe: `no_policy_match` is counted as such.

### Database

Each statement the storage backend runs gets a client span, through oxtel-sql, named by its query summary: the operations and tables it touches, such as `SELECT objects` or `INSERT refs`. On Postgres, a transaction's `BEGIN`, `COMMIT` and `ROLLBACK` are spans too.

| Attribute | Notes |
|---|---|
| `db.system.name` | `sqlite` or `postgresql` |
| `db.query.summary` | the operations and tables, as the conventions define it |
| `db.query.text` | the statement with every literal replaced by `?`; values are bound parameters, so none reach telemetry |
| `db.namespace`, `server.address`, `server.port` | Postgres: the database and server, from the connection string |
| `db.response.status_code`, `error.type` | Postgres: the SQLSTATE of a failed statement, such as `23505`. SQLite's trace hooks don't report failures |
| `db.response.returned_rows` | Postgres: the rows a query returned |

| Metric | Instrument and unit | Labels |
|---|---|---|
| `db.client.operation.duration` | histogram, `s` | `db.system.name`, `db.query.summary`, `db.namespace`, `server.address`, `server.port`, and on failure `error.type` and `db.response.status_code` |
| `db.client.connection.count` | gauge | Postgres pool: `db.client.connection.pool.name`, `db.client.connection.state` (`idle` or `used`) |
| `db.client.connection.max`, `db.client.connection.pending_requests` | gauge | `db.client.connection.pool.name` |
| `db.client.connection.wait_time`, `db.client.connection.use_time` | histogram, `s` | `db.client.connection.pool.name` |
| `db.client.connection.timeouts` | counter | `db.client.connection.pool.name` |

On Postgres the pool's name is `host:port/database`. SQLite's single connection is recorded as a pool of one named `sqlite`: `db.client.connection.wait_time` is time spent waiting for it, `use_time` time holding it, and `pending_requests` callers waiting. The `db.client.connection.*` metrics are still in development in the conventions, so their names may change.

### Store

Gauges of what's in the store, sampled every 60 seconds by default while a metrics pipeline exists (`--metrics`, or OTLP). `ASG_STORE_GAUGES_INTERVAL` sets the interval in seconds; `0` turns sampling off. Counting reads every row's key: on a 3.2 GB SQLite store (212,000 objects, 1.1 million commits) a sample took about 220 ms with a cold cache and 13 ms warm, holding SQLite's connection meanwhile. On Postgres it grows with the tenant's rows; lengthen the interval for a large one.

| Metric | Instrument and unit | Meaning |
|---|---|---|
| `asg.store.objects` | gauge | objects in the store; on Postgres, this tenant's |
| `asg.store.commits` | gauge | commits in the store |
| `asg.store.size` | gauge, `By` | the store's file on disk; SQLite only, since Postgres tables are shared by tenants |
| `asg.history.undistilled_commits` | gauge | commits the history extractor hasn't distilled yet: its lag |
| `asg.gc.last_sweep.time` | gauge, `s` | when the last garbage collection sweep this server ran finished, as Unix time |
| `asg.gc.last_sweep.objects_deleted` | gauge | what that sweep deleted |
| `asg.gc.last_vacuum.bytes_reclaimed` | gauge, `By` | what the last vacuum returned to the operating system |

The garbage collection gauges cover sweeps and vacuums run through this server's `/gc/sweep` and `/gc/vacuum`, not those run from the command line. None of these metrics has labels.

### Log lines

Each line carries `timestamp`, `level`, `target` and `message`. Inside a span it also carries `trace_id` and `span_id`, then the fields of the enclosing spans and of the event, under their OpenTelemetry names. In text, a span appears by its name, such as `GET /state/{ref_name}`. The `ecs` format writes the Elastic Common Schema. With OTLP, the same events are exported as log records that carry their span's context.

### Commits and their traces

Each commit records the trace it was made in, and the span that made it: its write's span, such as `asg.repo.set`. A trace leads to its commits through `asg.commit.id` on that span, and a commit leads back through these ids:

- `log` and `blame`, over HTTP and as MCP tools, show `trace_id` and `span_id` for each commit that has them;
- `Repository::commit_trace` and `Repository::blame_with_trace` read them in code.

They're kept beside the commit, in the store's `commit_traces` table, never in the commit itself, so commit ids and sync bundles don't depend on telemetry, and a fetched commit carries no trace. Log lines carry the same trace id, so a commit's log lines can be found even when its trace wasn't exported or was sampled out. `ASG_TRACE_LINKS=off` turns linking off. A program embedding the library turns it on by passing a source of the current trace to `agentstategraph::link_commits_to_traces`, once, at startup. The browser build keeps no links: it has no traces.

## `asg.*` attributes

Attributes of our own use the `asg.` prefix, lowercase and dot-separated, and are listed here before the code uses them.

| Attribute | Type | Where | Meaning |
|---|---|---|---|
| `asg.api_key.id` | string | spans, log lines | the id of the API key a request used; never the key |
| `asg.commit.id` | string | spans | the commit a write made, which ties the trace to the commit |
| `asg.error.code` | string | spans, log lines | an HTTP error response's stable code, from the codes in the MCP server guide |
| `asg.gc.bytes_reclaimed` | int | spans | the bytes a vacuum returned to the operating system |
| `asg.gc.mutate` | boolean | spans | whether a sweep deletes, or only previews |
| `asg.gc.objects_deleted` | int | spans | the objects a sweep deleted |
| `asg.gc.refused` | boolean | spans | whether a sweep refused, because history isn't distilled yet |
| `asg.history.commits_processed` | int | spans | the commits an extraction folded into the history tables |
| `asg.intent.category` | string | spans | a write's intent category, such as `Checkpoint` |
| `asg.operation.name` | string | metrics, spans, log lines | the kind of write: `set`, `delete`, `merge`, `commit_speculation` or `taint` |
| `asg.policy.decision` | string | metrics, spans, log lines | `allow`, `deny`, `require_approval` or `no_policy_match` |
| `asg.policy.evaluator` | string | metrics, spans, log lines | an external evaluator's kind, such as `rego` or `cedar` |
| `asg.policy.fail_safe` | string | log lines | what the fail-safe decided when no policy matched: `deny` or `allow` |
| `asg.policy.id` | string | log lines | a policy's `path@version` |
| `asg.ref.cas_attempts` | int | spans, log lines | how many times a write built its commit, recorded when the ref moved under it |
| `asg.ref.name` | string | spans | the branch or ref a write or read used |
| `asg.rejection.reason` | string | metrics, spans, log lines | why the server turned a request away: `rate_limited`, `over_capacity`, `timed_out`, `body_too_large`, `unauthenticated`, `forbidden` or `host_refused` |
| `asg.speculation.id` | int | spans | a speculation's handle |
| `asg.taint.effect` | string | log lines | the effect that refused a write: `block`, `review` or `quarantine` |
| `asg.taint.id` | string | log lines | the taint that refused a write |
| `asg.tenant.id` | string | resource only | the tenant a process serves, when it serves one |

## Cardinality

A metric label must have a small, bounded set of values. Never use as a label:

- paths: `url.path`, state paths;
- ids: commit, agent, session, API key, request or trace ids;
- branch, ref or namespace names;
- free text: error messages, user agents.

Allowed: methods, route templates, status codes, `error.type` codes, schemes, protocol versions, MCP method names and tool names, which the server defines; query summaries, which name only the operations and tables in the server's own statements; and the database system, its name and the pool's. A tenant is a resource attribute, never a label: one process per tenant, or none. High-cardinality detail belongs on spans and log lines, where it costs nothing to aggregate.

## Privacy

Never recorded, anywhere:

- the values stored in the graph;
- MCP tool arguments and results;
- API keys and other secrets: only a key's id;
- request and response bodies, and query strings;
- SQL text with literals.

Recorded only on spans and log lines, never on metrics: branch names in `url.path` and `asg.ref.name`, agent ids in `gen_ai.agent.id`, commit ids, and `asg.api_key.id`. State paths aren't recorded. When later tasks add them to spans, they will be off unless the operator turns them on.

Exporting is the operator's choice: nothing leaves the process unless the `otlp` feature is built in and an endpoint is set, or `/metrics` is turned on and scraped.

## Shutdown

Telemetry is sent in batches. On SIGINT or SIGTERM, the HTTP server stops taking requests, finishes those in flight and returns from `main`, so what's pending is sent within 5 seconds. Killing the process with SIGKILL loses up to the last few seconds.
