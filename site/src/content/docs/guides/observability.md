---
title: Observability
description: Traces, metrics and logs from the AgentStateGraph server, an operator kit to collect them, and every metric it records.
---

The AgentStateGraph server records traces, metrics and logs under OpenTelemetry's names, through [oxtel](https://github.com/agentstatelabs/oxtel). Nothing leaves the process until you send it somewhere. Any OTLP backend, or Prometheus, can take what it records without code changes.

The full contract, with every span, attribute and privacy rule, is [docs/OBSERVABILITY.md](https://github.com/agentstatelabs/agentstategraph/blob/main/docs/OBSERVABILITY.md). This page covers turning it on, the operator kit, and the metrics.

## Turning it on

| Setting | What it does |
|---|---|
| `OTEL_EXPORTER_OTLP_ENDPOINT=http://collector:4318` | sends traces, metrics and logs over OTLP/HTTP. Release binaries and the Docker image include OTLP; a source build needs `--features otlp` |
| `--metrics` | serves `/metrics` for Prometheus to scrape; with `--auth`, it needs an admin key |
| `ASG_LOG`, `ASG_LOG_FORMAT` | log level, and `text`, `json` or `ecs` lines on standard error |
| `ASG_SQL_SPANS=on` | a span for each SQL statement; off by default, as it costs a small read more than anything else |
| `ASG_TRACE_LINKS=off` | stops recording the trace each commit was made in |
| `ASG_COMMIT_EVENTS=on` | logs each commit as an `asg.commit` event, for a log backend or SIEM |
| `OTEL_SERVICE_NAME`, `OTEL_RESOURCE_ATTRIBUTES`, `OTEL_TRACES_SAMPLER`, ... | the standard OpenTelemetry settings |

A write's trace leads to its commit: its span carries `asg.commit.id`. A commit leads back to its trace: `log` and `blame` show its `trace_id`. Error responses carry the request's `trace_id`, and every HTTP response carries an `x-request-id`.

## The operator kit

`deploy/observability` holds:

- an OpenTelemetry Collector config;
- Prometheus with alert rules;
- Grafana with a dashboard and data sources for Prometheus and Jaeger.

The `observability` profile in `docker-compose.yml` runs all of them, with Jaeger:

```bash
ASG_OTLP_ENDPOINT=http://otel-collector:4318 docker compose --profile observability up -d
```

Then open:

- **Grafana** at `http://localhost:3000`, for the AgentStateGraph dashboard: requests, rejections, MCP tool calls, writes and conflicts, policy decisions, database statements and connections, store size, history lag and garbage collection;
- **Jaeger** at `http://localhost:16686`, for traces;
- **Prometheus** at `http://localhost:9090`, for metrics and alerts.

Everything is published on 127.0.0.1 only. A server running outside compose can send to the collector at `http://localhost:4318`.

The collector sends traces to Jaeger, serves metrics for Prometheus, and prints logs. To use your own backends, change its exporters; the server needs nothing else.

### Alerts

`deploy/observability/alerts.yml` alerts when:

- the store stops answering;
- the server stops reporting;
- more than 5% of HTTP requests fail;
- HTTP p99 latency goes over a second;
- requests are shed (over capacity or timed out), or rejected in bulk;
- a Postgres pool is over 90% used, or SQLite's connection is waited on for long;
- history extraction falls behind while the lag grows;
- a garbage collection sweep refuses to run;
- writes keep failing with write conflicts.

The thresholds are starting points.

## Metrics

Every metric the server records. Prometheus names follow OpenTelemetry's rules: dots become underscores, the unit is appended (`_seconds`, `_bytes`), and counters end in `_total`. So `asg.commit.duration` is `asg_commit_duration_seconds` and `asg.http.rejections` is `asg_http_rejections_total`. A test checks this table against the instruments the server registers.

| Metric | Instrument | Unit | Labels | Meaning |
|---|---|---|---|---|
| `http.server.request.duration` | histogram | `s` | `http.request.method`, `http.route`, `http.response.status_code`, `url.scheme`, `network.protocol.version`, `error.type` | HTTP requests, by route template |
| `http.server.active_requests` | up-down counter | | `http.request.method`, `url.scheme` | HTTP requests in flight |
| `asg.http.rejections` | counter | | `asg.rejection.reason` | requests turned away before a handler ran: `rate_limited`, `over_capacity`, `timed_out`, `body_too_large`, `unauthenticated`, `forbidden`, `host_refused` |
| `mcp.server.operation.duration` | histogram | `s` | `mcp.method.name`, `gen_ai.tool.name`, `gen_ai.operation.name`, `mcp.protocol.version`, `network.transport`, `error.type`, `rpc.response.status_code` | MCP requests, by tool; a failed tool call has `error.type` `tool_error` |
| `mcp.server.session.duration` | histogram | `s` | `mcp.protocol.version`, `network.transport` | MCP sessions, when they end |
| `asg.commit.duration` | histogram | `s` | `asg.operation.name`, `error.type` | writes that make a commit: `set`, `delete`, `merge`, `commit_speculation` |
| `asg.ref.cas_retries` | counter | | `asg.operation.name` | writes rebuilt because another writer moved the ref first |
| `asg.policy.decisions` | counter | | `asg.policy.decision` | policy evaluations, by decision |
| `asg.policy.evaluator.errors` | counter | | `asg.policy.evaluator`, `error.type` | external evaluators (Rego, Cedar) that failed, so their policy was skipped |
| `asg.gc.sweeps` | counter | | `asg.gc.outcome` | sweeps asked to delete: `swept`, or `refused` because history wasn't distilled |
| `db.client.operation.duration` | histogram | `s` | `db.system.name`, `db.query.summary`, `db.namespace`, `server.address`, `server.port`, `error.type`, `db.response.status_code` | SQL statements |
| `db.client.connection.count` | up-down counter | | `db.client.connection.pool.name`, `db.client.connection.state` | Postgres connections, `idle` and `used` |
| `db.client.connection.max` | up-down counter | | `db.client.connection.pool.name` | the Postgres pool's size |
| `db.client.connection.pending_requests` | up-down counter | | `db.client.connection.pool.name` | callers waiting for a connection; SQLite's one connection is the pool `sqlite` |
| `db.client.connection.wait_time` | histogram | `s` | `db.client.connection.pool.name` | time waiting for a connection |
| `db.client.connection.use_time` | histogram | `s` | `db.client.connection.pool.name` | time holding one |
| `db.client.connection.timeouts` | counter | | `db.client.connection.pool.name` | Postgres connection waits that timed out |
| `asg.store.available` | gauge | | | 1 if the store answered the last sample, else 0 |
| `asg.store.objects` | gauge | | | objects in the store (this tenant's, on Postgres) |
| `asg.store.commits` | gauge | | | commits in the store |
| `asg.store.size` | gauge | `By` | | the store's file on disk (SQLite) |
| `asg.history.undistilled_commits` | gauge | | | commits history extraction hasn't distilled yet: its lag |
| `asg.gc.last_sweep.time` | gauge | `s` | | when the last sweep this server ran finished, as Unix time |
| `asg.gc.last_sweep.objects_deleted` | gauge | | | what that sweep deleted |
| `asg.gc.last_vacuum.bytes_reclaimed` | gauge | `By` | | what the last vacuum returned to the operating system |

The store gauges are sampled every 60 seconds while a metrics pipeline exists (`ASG_STORE_GAUGES_INTERVAL` changes it, and `0` turns them off). The `db.client.connection.*` metrics are still in development in OpenTelemetry's conventions, so their names may change.

## What telemetry costs

On a busy laptop, with telemetry on and nothing exported, an HTTP read gains about 11 µs (6%) and an MCP call about 3% or less. Metrics cost nothing measurable. DESIGN.md Plan H has the measurements, and `cargo bench -p agentstategraph-mcp --features otlp --bench overhead` repeats them.
