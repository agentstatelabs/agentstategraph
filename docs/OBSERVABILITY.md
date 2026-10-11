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

Each finished request also logs `request` at `info`, or at `error` for a 5xx, with the status and `http.server.request.duration` in seconds.

| Metric | Instrument and unit | Labels | Prometheus name |
|---|---|---|---|
| `http.server.request.duration` | histogram, `s` | `http.request.method`, `url.scheme`, `network.protocol.version`, `http.route`, `http.response.status_code`, `error.type` | `http_server_request_duration_seconds` |
| `http.server.active_requests` | up-down counter, `{request}` | `http.request.method`, `url.scheme` | `http_server_active_requests` |

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

### Log lines

Each line carries `timestamp`, `level`, `target` and `message`. Inside a span it also carries `trace_id` and `span_id`, then the fields of the enclosing spans and of the event, under their OpenTelemetry names. In text, a span appears by its name, such as `GET /state/{ref_name}`. The `ecs` format writes the Elastic Common Schema. With OTLP, the same events are exported as log records that carry their span's context.

### Planned

DESIGN.md Plan H adds, under the rules below:

- spans for repository writes, merges, speculations, history extraction, garbage collection, policy evaluation and taint checks;
- events for write conflicts, policy decisions and taint rejections;
- metrics such as `asg.commit.duration`, `asg.ref.cas_retries` and `asg.policy.decisions`;
- database client metrics (`db.client.operation.duration`, pool usage) and store gauges;
- stable error codes as `error.type`;
- links from commits to their traces.

Each is added to this page when it lands.

## `asg.*` attributes

Attributes of our own use the `asg.` prefix, lowercase and dot-separated, and are listed here before the code uses them.

| Attribute | Type | Where | Meaning |
|---|---|---|---|
| `asg.api_key.id` | string | spans, log lines | the id of the API key a request used; never the key |
| `asg.tenant.id` | string | resource only | the tenant a process serves, when it serves one |

## Cardinality

A metric label must have a small, bounded set of values. Never use as a label:

- paths: `url.path`, state paths;
- ids: commit, agent, session, API key, request or trace ids;
- branch, ref or namespace names;
- free text: error messages, user agents.

Allowed: methods, route templates, status codes, `error.type` codes, schemes, protocol versions, MCP method names and tool names, which the server defines. A tenant is a resource attribute, never a label: one process per tenant, or none. High-cardinality detail belongs on spans and log lines, where it costs nothing to aggregate.

## Privacy

Never recorded, anywhere:

- the values stored in the graph;
- MCP tool arguments and results;
- API keys and other secrets: only a key's id;
- request and response bodies, and query strings;
- SQL text with literals.

Recorded only on spans and log lines, never on metrics: branch names in `url.path`, and `asg.api_key.id`. State paths aren't recorded. When later tasks add them to spans, they will be off unless the operator turns them on.

Exporting is the operator's choice: nothing leaves the process unless the `otlp` feature is built in and an endpoint is set, or `/metrics` is turned on and scraped.

## Shutdown

Telemetry is sent in batches. On SIGINT or SIGTERM, the HTTP server stops taking requests, finishes those in flight and returns from `main`, so what's pending is sent within 5 seconds. Killing the process with SIGKILL loses up to the last few seconds.
