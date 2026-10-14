//! agentstategraph-mcp — MCP + HTTP server for AgentStateGraph.
//!
//! Run as MCP server (stdio):  cargo run -p agentstategraph-mcp
//! Run as HTTP server:         cargo run -p agentstategraph-mcp -- --http
//! Both:                       cargo run -p agentstategraph-mcp -- --http --port 3001
//! Options:                    cargo run -p agentstategraph-mcp -- --storage memory
//!                             cargo run -p agentstategraph-mcp -- --path /data/state.db

use agentstategraph_mcp::auth::TenantManager;
use agentstategraph_mcp::http;

mod migrate;

use agentstategraph_mcp::server;

use std::sync::Arc;

use agentstategraph::Repository;
use agentstategraph_core::Namespace;
use agentstategraph_storage::SqliteStorage;
use rmcp::ServiceExt;

/// All resolved server configuration, parsed from CLI args + environment.
pub struct ServeArgs {
    pub storage_type: String,
    pub db_path: String,
    pub database_url: String,
    pub tenant_id: String,
    /// Default namespace for ref operations. Can be overridden per-session
    /// via `scope_namespace`. Maps to `Repository::with_namespace(...)`.
    /// Env: `ASG_NAMESPACE`. CLI: `--namespace <ID>`. Default: `"default"`.
    pub default_namespace: String,
    pub http_mode: bool,
    pub http_port: u16,
    pub bind_addr: String,
    pub auth_enabled: bool,
    pub keys_file: String,
    pub rate_limit_rpm: u32,
    pub pg_pool_size: usize,
    pub initial_admin_key: Option<String>,
    pub external_evaluators: Vec<String>,
    /// When set, proxy `GET /api/code/*` to ASD at this URL (`/api/v1/*`).
    pub asd_url: Option<String>,
    /// Browser origins allowed besides loopback ones (`*` for any). CLI:
    /// `--cors-origin` (repeatable). Env: `ASG_CORS_ORIGINS`, comma-separated.
    pub cors_origins: Vec<String>,
    /// `Host` names served besides loopback ones (`*` for any). CLI:
    /// `--allowed-host` (repeatable). Env: `ASG_ALLOWED_HOSTS`, comma-separated.
    pub allowed_hosts: Vec<String>,
    /// Serve Prometheus metrics at `/metrics`. CLI: `--metrics`. Env:
    /// `ASG_METRICS=1`.
    pub metrics: bool,
    /// Per-request limits. CLI: `--max-body-bytes`, `--request-timeout-secs`
    /// (0 for none), `--max-concurrent-requests`. Env: `ASG_MAX_BODY_BYTES`,
    /// `ASG_REQUEST_TIMEOUT_SECS`, `ASG_MAX_CONCURRENT_REQUESTS`.
    pub bounds: http::RequestBounds,
    /// What MCP policy tools and `commit_spec` do when no policy matches:
    /// `deny` (default) or `allow`. `commit_spec` applies it only to changes
    /// that carry tokens. CLI: `--policy-fail-safe`. Env: `ASG_POLICY_FAIL_SAFE`.
    pub policy_fail_safe: String,
}

/// A number from the environment, if set and valid.
fn env_number<T: std::str::FromStr>(name: &str) -> Option<T> {
    std::env::var(name).ok()?.trim().parse().ok()
}

/// A comma-separated list from the environment.
fn env_list(name: &str) -> Vec<String> {
    std::env::var(name)
        .ok()
        .map(|v| {
            v.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// The HTTP port from `ASG_PORT`, if set and valid.
fn env_port() -> Option<u16> {
    std::env::var("ASG_PORT").ok()?.trim().parse().ok()
}

/// `agentstategraph-mcp healthcheck [--port N] [--bind ADDR]`: exit 0 if the
/// HTTP server on this machine answers `GET /api/ready` with 200 (it is
/// serving and its store answers), and 1 otherwise. It finds the server the way `--http` picks its address
/// (`--port` or `ASG_PORT`, `--bind` or `ASG_BIND`), so a container health
/// check needs neither curl nor the server's arguments.
pub fn healthcheck(args: &[String]) -> i32 {
    let port = args
        .iter()
        .position(|a| a == "--port")
        .and_then(|i| args.get(i + 1))
        .and_then(|p| p.parse().ok())
        .or_else(env_port)
        .unwrap_or(3001);
    let bind = resolve_bind_addr(args, std::env::var("ASG_BIND").ok());
    let host = healthcheck_host(&bind);
    match probe_health(&host, port) {
        Ok(200) => {
            println!("healthy: {host}:{port}");
            0
        }
        Ok(status) => {
            eprintln!("unhealthy: {host}:{port}/api/ready answered {status}");
            1
        }
        Err(e) => {
            eprintln!("unhealthy: {host}:{port}: {e}");
            1
        }
    }
}

/// Where to reach a server bound to `bind` from the same machine.
pub fn healthcheck_host(bind: &str) -> String {
    match bind.trim_start_matches('[').trim_end_matches(']') {
        "0.0.0.0" | "localhost" | "" => "127.0.0.1".to_string(),
        "::" => "::1".to_string(),
        other => other.to_string(),
    }
}

/// `GET /api/ready` with plain HTTP/1.1 and a 5-second budget per step;
/// the status code on success.
fn probe_health(host: &str, port: u16) -> std::io::Result<u16> {
    use std::io::{Read, Write};
    use std::net::ToSocketAddrs;
    // Longer than the server's own 3-second readiness timeout, so a slow
    // store reads as "not ready" rather than as no answer.
    let timeout = std::time::Duration::from_secs(5);
    let addr = (host, port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no address for host"))?;
    let mut stream = std::net::TcpStream::connect_timeout(&addr, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    // `localhost` passes the Host check on any bind.
    stream.write_all(b"GET /api/ready HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")?;
    let mut head = [0u8; 64];
    let n = stream.read(&mut head)?;
    std::str::from_utf8(&head[..n])
        .ok()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "not an HTTP response"))
}

/// Parse the bind address from CLI args + env.
///
/// Precedence: `--bind <ADDR>` > `ASG_BIND` env > default `127.0.0.1`.
/// Exposed as a free function so unit tests can assert the default.
pub fn resolve_bind_addr(args: &[String], env_bind: Option<String>) -> String {
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--bind" && i + 1 < args.len() {
            return args[i + 1].clone();
        }
        i += 1;
    }
    env_bind
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "127.0.0.1".to_string())
}

/// Return a TLS advice string for operators when the server binds to a
/// non-loopback address without TLS in front of it (v3-V7). Returns
/// `None` when the bind is loopback or TLS is configured.
pub fn tls_advice(bind_addr: &str, has_tls: bool) -> Option<String> {
    if has_tls {
        return None;
    }
    let loopback = bind_addr == "127.0.0.1"
        || bind_addr == "localhost"
        || bind_addr == "::1"
        || bind_addr.starts_with("127.");
    if loopback {
        return None;
    }
    Some(
        "binding to non-loopback without TLS — put a TLS-terminating proxy \
         in front, or run only on a trusted network."
            .to_string(),
    )
}

/// Apply `--external-evaluator <kind>` flags to a freshly-constructed
/// `AgentStateGraphServer`. Unknown kinds are logged and skipped;
/// kinds whose feature isn't compiled in are logged and skipped.
/// Returns the (possibly modified) server so callers can chain.
fn apply_external_evaluators(
    #[cfg_attr(
        not(any(
            feature = "policy-wasm",
            feature = "policy-rego",
            feature = "policy-cedar"
        )),
        allow(unused_mut)
    )]
    mut srv: server::AgentStateGraphServer,
    kinds: &[String],
) -> server::AgentStateGraphServer {
    for kind in kinds {
        match kind.as_str() {
            "wasm" => {
                #[cfg(feature = "policy-wasm")]
                {
                    eprintln!("Policy: registering external evaluator 'wasm'");
                    srv = srv.with_wasm_evaluator();
                }
                #[cfg(not(feature = "policy-wasm"))]
                eprintln!(
                    "Warning: --external-evaluator wasm requested but the \
                     'policy-wasm' feature is not enabled; skipping."
                );
            }
            "rego" => {
                #[cfg(feature = "policy-rego")]
                {
                    eprintln!("Policy: registering external evaluator 'rego'");
                    srv = srv.with_rego_evaluator();
                }
                #[cfg(not(feature = "policy-rego"))]
                eprintln!(
                    "Warning: --external-evaluator rego requested but the \
                     'policy-rego' feature is not enabled; skipping."
                );
            }
            "cedar" => {
                #[cfg(feature = "policy-cedar")]
                {
                    eprintln!("Policy: registering external evaluator 'cedar'");
                    srv = srv.with_cedar_evaluator();
                }
                #[cfg(not(feature = "policy-cedar"))]
                eprintln!(
                    "Warning: --external-evaluator cedar requested but the \
                     'policy-cedar' feature is not enabled; skipping."
                );
            }
            other => eprintln!(
                "Warning: --external-evaluator '{}' is not a recognized kind \
                 (expected wasm|rego|cedar); skipping.",
                other
            ),
        }
    }
    srv
}

/// Parse CLI args (everything after the program name) together with
/// relevant environment variables into a fully resolved `ServeArgs`.
pub fn parse_args(args: &[String]) -> ServeArgs {
    let mut storage_type = "sqlite".to_string();
    let mut db_path = "./agentstategraph.db".to_string();
    let mut database_url = String::new();
    let mut tenant_id = "default".to_string();
    let mut default_namespace: String = std::env::var("ASG_NAMESPACE")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "default".to_string());
    let mut http_mode = false;
    let mut http_port: u16 = env_port().unwrap_or(3001);
    let mut auth_enabled = false;
    let mut keys_file = String::new();
    let mut rate_limit_rpm: u32 = std::env::var("ASG_RATE_LIMIT_RPM")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(600);
    let mut rate_limit_rpm_cli: Option<u32> = None;
    let bind_addr = resolve_bind_addr(args, std::env::var("ASG_BIND").ok());
    let mut pg_pool_size: usize = std::env::var("ASG_PG_POOL_SIZE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(32);
    let initial_admin_key_env = std::env::var("ASG_INITIAL_ADMIN_KEY").ok();
    let mut initial_admin_key_cli: Option<String> = None;
    let mut external_evaluators: Vec<String> = Vec::new();
    let mut asd_url: Option<String> = std::env::var("ASG_ASD_URL").ok().filter(|s| !s.is_empty());
    let mut metrics = std::env::var("ASG_METRICS").is_ok_and(|v| !v.is_empty() && v != "0");
    let defaults = http::RequestBounds::default();
    let mut max_body_bytes: usize =
        env_number("ASG_MAX_BODY_BYTES").unwrap_or(defaults.max_body_bytes);
    let mut timeout_secs: Option<u64> = env_number("ASG_REQUEST_TIMEOUT_SECS");
    let mut max_concurrent: usize =
        env_number("ASG_MAX_CONCURRENT_REQUESTS").unwrap_or(defaults.max_concurrent);
    let mut cors_origins_cli: Vec<String> = Vec::new();
    let mut allowed_hosts_cli: Vec<String> = Vec::new();
    let mut policy_fail_safe = std::env::var("ASG_POLICY_FAIL_SAFE")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "deny".to_string());

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--storage" | "-s" => {
                i += 1;
                if i < args.len() {
                    storage_type = match args[i].as_str() {
                        "memory" => "memory",
                        "postgres" | "pg" => "postgres",
                        _ => "sqlite",
                    }
                    .to_string();
                }
            }
            "--path" | "-p" => {
                i += 1;
                if i < args.len() {
                    db_path = args[i].clone();
                }
            }
            "--database-url" => {
                i += 1;
                if i < args.len() {
                    database_url = args[i].clone();
                }
            }
            "--tenant" => {
                i += 1;
                if i < args.len() {
                    tenant_id = args[i].clone();
                }
            }
            "--namespace" => {
                i += 1;
                if i < args.len() {
                    default_namespace = args[i].clone();
                }
            }
            "--http" => {
                http_mode = true;
            }
            "--auth" => {
                auth_enabled = true;
            }
            "--keys-file" => {
                i += 1;
                if i < args.len() {
                    keys_file = args[i].clone();
                    auth_enabled = true;
                }
            }
            "--port" => {
                i += 1;
                if i < args.len() {
                    http_port = args[i].parse().unwrap_or(3001);
                }
            }
            "--rate-limit-rpm" => {
                i += 1;
                if i < args.len()
                    && let Ok(v) = args[i].parse()
                {
                    rate_limit_rpm_cli = Some(v);
                }
            }
            "--bind" => {
                // Already consumed by resolve_bind_addr; skip the value.
                i += 1;
            }
            "--pg-pool-size" => {
                i += 1;
                if i < args.len()
                    && let Ok(v) = args[i].parse::<usize>()
                {
                    pg_pool_size = v.max(1);
                }
            }
            "--initial-admin-key" => {
                i += 1;
                if i < args.len() {
                    initial_admin_key_cli = Some(args[i].clone());
                }
            }
            "--external-evaluator" => {
                i += 1;
                if i < args.len() {
                    external_evaluators.push(args[i].clone());
                }
            }
            "--asd-url" => {
                i += 1;
                if i < args.len() {
                    asd_url = Some(args[i].clone());
                }
            }
            "--cors-origin" => {
                i += 1;
                if i < args.len() {
                    cors_origins_cli.push(args[i].clone());
                }
            }
            "--allowed-host" => {
                i += 1;
                if i < args.len() {
                    allowed_hosts_cli.push(args[i].clone());
                }
            }
            "--metrics" => {
                metrics = true;
            }
            "--max-body-bytes" => {
                i += 1;
                if let Some(v) = args.get(i).and_then(|v| v.parse().ok()) {
                    max_body_bytes = v;
                }
            }
            "--request-timeout-secs" => {
                i += 1;
                if let Some(v) = args.get(i).and_then(|v| v.parse().ok()) {
                    timeout_secs = Some(v);
                }
            }
            "--max-concurrent-requests" => {
                i += 1;
                if let Some(v) = args.get(i).and_then(|v| v.parse().ok()) {
                    max_concurrent = v;
                }
            }
            "--policy-fail-safe" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    policy_fail_safe = v.clone();
                }
            }
            "--help" | "-h" => {
                eprintln!("AgentStateGraph Server v{}", env!("CARGO_PKG_VERSION"));
                eprintln!();
                eprintln!("USAGE:");
                eprintln!("  agentstategraph-mcp [OPTIONS]");
                eprintln!();
                eprintln!("MODES:");
                eprintln!("  (default)             MCP server over stdio");
                eprintln!("  --http                HTTP REST API server");
                eprintln!(
                    "  healthcheck           Exit 0 if this machine's HTTP server is healthy (for container health checks)"
                );
                eprintln!(
                    "  migrate [...]         One-shot schema migration (see `migrate --help`)"
                );
                eprintln!();
                eprintln!("OPTIONS:");
                eprintln!(
                    "  -s, --storage <TYPE>  Storage backend: sqlite (default), memory, or postgres"
                );
                eprintln!(
                    "  -p, --path <PATH>     SQLite database path (default: ./agentstategraph.db)"
                );
                eprintln!(
                    "      --database-url <URL>  Postgres connection URL (required for --storage postgres).\n                            TLS: add ?sslmode=verify-full (and &sslrootcert=<ca.pem> for a\n                            private CA); needs a build with the `postgres-tls` feature"
                );
                eprintln!(
                    "      --tenant <ID>     Tenant this server serves: its Postgres partition, and the tenant non-admin API keys must belong to (default: \"default\")"
                );
                eprintln!(
                    "      --namespace <ID>  Default namespace for ref isolation (default: \"default\"; env ASG_NAMESPACE)"
                );
                eprintln!(
                    "      --port <PORT>     HTTP port (default: 3001, requires --http; env ASG_PORT)"
                );
                eprintln!(
                    "      --bind <ADDR>     Bind address (default: 127.0.0.1; pass 0.0.0.0 for LAN; env ASG_BIND)"
                );
                eprintln!(
                    "      --rate-limit-rpm <N>  Requests/minute per API key with --auth, else per client IP (default: 600, 0 disables; env ASG_RATE_LIMIT_RPM)"
                );
                eprintln!(
                    "      --pg-pool-size <N>  Max Postgres connections (default: 32; env ASG_PG_POOL_SIZE)"
                );
                eprintln!("      --auth            Require an API key on every request");
                eprintln!(
                    "      --keys-file <PATH>  Where API keys are kept, as hashes in an owner-only file (implies --auth)"
                );
                eprintln!(
                    "      --initial-admin-key <KEY>  Bootstrap admin key (multi-tenant; env ASG_INITIAL_ADMIN_KEY)"
                );
                eprintln!(
                    "      --external-evaluator <KIND>  Register stock external policy runner (wasm|rego|cedar); repeatable"
                );
                eprintln!(
                    "      --asd-url <URL>  Proxy GET /api/code/* to ASD at URL/api/v1/* (env ASG_ASD_URL)"
                );
                eprintln!(
                    "      --cors-origin <ORIGIN>  Also let pages from ORIGIN call the API; loopback pages always can; * for any (repeatable; env ASG_CORS_ORIGINS)"
                );
                eprintln!(
                    "      --allowed-host <HOST>  Also answer to HOST; on a loopback bind only loopback names are served by default; * for any (repeatable; env ASG_ALLOWED_HOSTS)"
                );
                eprintln!(
                    "      --metrics         Serve Prometheus metrics at /metrics (admin key needed with --auth; env ASG_METRICS=1)"
                );
                eprintln!(
                    "      --max-body-bytes <N>  Largest request body (default: 4194304; env ASG_MAX_BODY_BYTES)"
                );
                eprintln!(
                    "      --request-timeout-secs <N>  Answer 503 to a request still running after N seconds (default: 30, 0 for none; env ASG_REQUEST_TIMEOUT_SECS)"
                );
                eprintln!(
                    "      --max-concurrent-requests <N>  Requests handled at once; more get 503 (default: 256; env ASG_MAX_CONCURRENT_REQUESTS)"
                );
                eprintln!(
                    "      --policy-fail-safe <deny|allow>  When no policy matches: MCP policy tools report it, and commit_spec refuses a change with tokens (destructive, large, ...) under deny (default: deny; env ASG_POLICY_FAIL_SAFE)"
                );
                eprintln!();
                eprintln!("LOGGING (stderr):");
                eprintln!(
                    "  ASG_LOG=<filter>      Level or filter, e.g. debug or agentstategraph_mcp=debug (else RUST_LOG; default info, or warn for MCP stdio)"
                );
                eprintln!(
                    "  ASG_LOG_FORMAT=<fmt>  auto (text on a terminal, JSON otherwise), text, json or ecs"
                );
                eprintln!(
                    "  OTEL_EXPORTER_OTLP_ENDPOINT=<url>  Send traces, metrics and logs over OTLP (built with --features otlp)"
                );
                eprintln!(
                    "  ASG_TRACE_LINKS=off   Don't record the trace each commit was made in (default: on)"
                );
                eprintln!("  -h, --help            Print help");
                eprintln!();
                eprintln!("HTTP API ENDPOINTS:");
                eprintln!("  GET  /api/health                  Liveness: the process is serving");
                eprintln!(
                    "  GET  /api/ready                   Readiness: the store answers (503 if not)"
                );
                eprintln!("  GET  /api/stats/:ref              Summary statistics");
                eprintln!("  GET  /api/state/:ref?path=/x      Read state value");
                eprintln!("  GET  /api/state/:ref/paths        List all paths");
                eprintln!("  GET  /api/state/:ref/search?query=x  Search values");
                eprintln!("  POST /api/state/:ref/set          Write value (with intent)");
                eprintln!("  GET  /api/log/:ref                Commit log");
                eprintln!("  GET  /api/blame/:ref?path=/x      Blame a path");
                eprintln!("  GET  /api/diff?ref_a=x&ref_b=y    Diff two refs");
                eprintln!("  GET  /api/graph/:ref              Commit DAG");
                eprintln!("  GET  /api/branches                List branches");
                eprintln!("  POST /api/branches                Create branch");
                eprintln!("  POST /api/merge                   Merge branches");
                eprintln!("  GET  /api/epochs                  List epochs");
                eprintln!("  POST /api/epochs                  Create epoch");
                eprintln!("  POST /api/epochs/seal             Seal epoch");
                eprintln!("  GET  /api/intents/:ref            Intent tree");
                std::process::exit(0);
            }
            _ => {}
        }
        i += 1;
    }

    // CLI flag wins over env for rate_limit_rpm and initial_admin_key.
    if let Some(cli_rpm) = rate_limit_rpm_cli {
        rate_limit_rpm = cli_rpm;
    }

    // Check for DATABASE_URL env var as fallback for postgres.
    if database_url.is_empty()
        && let Ok(url) = std::env::var("DATABASE_URL")
    {
        database_url = url;
        if storage_type == "sqlite" {
            storage_type = "postgres".to_string();
        }
    }

    ServeArgs {
        storage_type,
        db_path,
        database_url,
        tenant_id,
        default_namespace,
        http_mode,
        http_port,
        bind_addr,
        auth_enabled,
        keys_file,
        rate_limit_rpm,
        pg_pool_size,
        initial_admin_key: initial_admin_key_cli.or(initial_admin_key_env),
        external_evaluators,
        asd_url,
        // CLI values replace the environment's.
        cors_origins: if cors_origins_cli.is_empty() {
            env_list("ASG_CORS_ORIGINS")
        } else {
            cors_origins_cli
        },
        allowed_hosts: if allowed_hosts_cli.is_empty() {
            env_list("ASG_ALLOWED_HOSTS")
        } else {
            allowed_hosts_cli
        },
        metrics,
        bounds: http::RequestBounds {
            max_body_bytes,
            timeout: match timeout_secs {
                None => defaults.timeout,
                Some(0) => None,
                Some(secs) => Some(std::time::Duration::from_secs(secs)),
            },
            max_concurrent: max_concurrent.max(1),
        },
        policy_fail_safe,
    }
}

/// Build the repository and run either the HTTP or MCP server.
pub fn cmd_serve(args: ServeArgs) -> Result<(), Box<dyn std::error::Error>> {
    cmd_serve_with(args, None)
}

/// [`cmd_serve`], serving `/metrics` (with `--metrics`) from `meters`, the
/// meter provider of the process's telemetry.
pub fn cmd_serve_with(
    args: ServeArgs,
    meters: Option<oxtel::sdk::metrics::MeterProvider>,
) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("AgentStateGraph Server v{}", env!("CARGO_PKG_VERSION"));
    // A typo mustn't quietly pick a side.
    if !matches!(args.policy_fail_safe.as_str(), "deny" | "allow") {
        return Err(format!(
            "--policy-fail-safe must be deny or allow, not '{}'",
            args.policy_fail_safe
        )
        .into());
    }

    let repo_namespace = Namespace::new(&args.default_namespace).unwrap_or_else(|_| {
        eprintln!(
            "Warning: invalid namespace '{}', falling back to 'default'",
            args.default_namespace
        );
        Namespace::default_ns()
    });

    let repo: Arc<Repository> = match args.storage_type.as_str() {
        "memory" => {
            eprintln!("Storage: in-memory (ephemeral)");
            Arc::new(
                Repository::new(Box::new(
                    SqliteStorage::in_memory().expect("in-memory sqlite"),
                ))
                .with_namespace(repo_namespace),
            )
        }
        "postgres" => {
            if args.database_url.is_empty() {
                eprintln!("Error: --database-url or DATABASE_URL required for postgres storage");
                std::process::exit(1);
            }
            eprintln!(
                "Storage: postgres (tenant: {}, pool: {})",
                args.tenant_id, args.pg_pool_size
            );
            // The storage drives its connections on its own runtime, so it can
            // connect from here and then serve either transport. A throwaway
            // runtime used to connect, and was dropped with the connection's
            // driver still on it.
            let storage = agentstategraph_storage::PostgresStorage::connect_tenant_blocking(
                &args.database_url,
                &args.tenant_id,
                args.pg_pool_size,
            )?;
            Arc::new(Repository::new(Box::new(storage)).with_namespace(repo_namespace))
        }
        _ => {
            eprintln!("Storage: {}", args.db_path);
            let storage = SqliteStorage::open(&args.db_path)?;
            Arc::new(Repository::new(Box::new(storage)).with_namespace(repo_namespace))
        }
    };

    repo.init()?;
    // The store gauges, when there's a metrics pipeline to take them.
    if meters.is_some() {
        if let Some(interval) = agentstategraph_mcp::store_gauges::interval_from_env() {
            agentstategraph_mcp::store_gauges::spawn(&repo, interval);
        }
    }

    if args.http_mode {
        eprintln!(
            "Rate limit: {} requests/minute per {}{}",
            args.rate_limit_rpm,
            if args.auth_enabled {
                "API key (per client IP without one)"
            } else {
                "client IP"
            },
            if args.rate_limit_rpm == 0 {
                " (DISABLED)"
            } else {
                ""
            }
        );
        if args.auth_enabled {
            eprintln!(
                "Auth: enabled (keys file: {})",
                if args.keys_file.is_empty() {
                    "none"
                } else {
                    &args.keys_file
                }
            );
        } else {
            eprintln!("Auth: disabled (single-tenant mode)");
        }
        eprintln!(
            "HTTP API listening on http://{}:{} (bind: {})",
            if args.bind_addr == "0.0.0.0" {
                "0.0.0.0"
            } else {
                args.bind_addr.as_str()
            },
            args.http_port,
            args.bind_addr
        );
        if args.bind_addr == "127.0.0.1" || args.bind_addr == "localhost" {
            eprintln!(
                "Note: default bind is loopback-only. Pass --bind 0.0.0.0 to expose on the LAN."
            );
        }
        if let Some(msg) = tls_advice(&args.bind_addr, false) {
            eprintln!("WARNING: {}", msg);
        }
        let origin_policy =
            http::OriginPolicy::for_bind(&args.bind_addr, &args.cors_origins, &args.allowed_hosts);
        eprintln!("{}", origin_policy.describe());
        eprintln!("{}", args.bounds.describe());
        if args.metrics && meters.is_none() {
            eprintln!(
                "Warning: --metrics is off: another telemetry setup is installed in this process"
            );
        } else if args.metrics {
            eprintln!(
                "Metrics: /metrics{}",
                if args.auth_enabled {
                    " (admin key)"
                } else {
                    ""
                }
            );
        }
        if origin_policy.allows_any_origin() && !args.auth_enabled {
            eprintln!(
                "WARNING: any web page the user visits can read and write this store \
                 (--cors-origin * without --auth)."
            );
        }
        eprintln!("Try: curl http://localhost:{}/api/health", args.http_port);

        #[cfg(feature = "providers")]
        if let Some(ref url) = args.asd_url {
            eprintln!("ASD proxy: /api/code/* → {}/api/v1/*", url);
        }
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?
            .block_on(async {
                let tenant_mgr = if args.auth_enabled {
                    let kf = if args.keys_file.is_empty() {
                        None
                    } else {
                        Some(args.keys_file.as_str())
                    };
                    let tenant_mgr =
                        match TenantManager::multi_tenant_for(repo.clone(), kf, &args.tenant_id) {
                            Ok(mgr) => mgr,
                            Err(e) => {
                                eprintln!("Error: {e}");
                                std::process::exit(2);
                            }
                        };

                    let provided = args.initial_admin_key.clone();
                    if let Some(k) = provided {
                        if k.len() < 32 {
                            eprintln!(
                                "WARNING: the bootstrap admin key is short. Keys are stored as \
                                 SHA-256 hashes, which protect long random keys; use one like \
                                 those the server generates (asg_ and 64 hex characters)."
                            );
                        }
                        match tenant_mgr.register_admin_key(k, "bootstrap-admin") {
                            Ok(true) => eprintln!("Auth: registered bootstrap admin key"),
                            Ok(false) => {}
                            Err(e) => {
                                eprintln!("Error: can't register the bootstrap admin key: {e}");
                                std::process::exit(2);
                            }
                        }
                    } else if !tenant_mgr.has_admin_key() {
                        eprintln!(
                            "Error: --auth / --keys-file enabled but no admin key present. \
                             Provide --initial-admin-key <KEY> or ASG_INITIAL_ADMIN_KEY to \
                             bootstrap, or add an is_admin=true entry to the keys file."
                        );
                        std::process::exit(2);
                    }
                    tenant_mgr
                } else {
                    TenantManager::single_tenant(repo.clone())
                };
                // The ASD proxy goes behind the same auth and rate limit as
                // the API.
                #[cfg(feature = "providers")]
                let extra = args.asd_url.map(http::asd_proxy_router);
                #[cfg(not(feature = "providers"))]
                let extra = None;
                let app =
                    http::build_router_with(Arc::clone(&tenant_mgr), args.rate_limit_rpm, extra);
                let app = http::with_server_layers(
                    app,
                    tenant_mgr,
                    &args.bounds,
                    &origin_policy,
                    if args.metrics { meters } else { None },
                );
                let addr = format!("{}:{}", args.bind_addr, args.http_port);
                let listener = tokio::net::TcpListener::bind(&addr).await?;
                axum::serve(
                    listener,
                    app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
                )
                .with_graceful_shutdown(shutdown_signal())
                .await?;
                Ok::<(), Box<dyn std::error::Error>>(())
            })?;
    } else {
        eprintln!("MCP server waiting for client on stdio...");

        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(async {
                let mut srv = server::AgentStateGraphServer::new(repo)
                    .with_fail_safe(args.policy_fail_safe.clone());
                srv = apply_external_evaluators(srv, &args.external_evaluators);
                let service = oxtel_rmcp::Instrumented::new(srv)
                    .with_transport("pipe")
                    .serve(rmcp::transport::stdio())
                    .await
                    .map_err(|e| format!("MCP server error: {}", e))?;

                tokio::select! {
                    done = service.waiting() => {
                        done?;
                    }
                    () = shutdown_signal() => {}
                }
                Ok::<(), Box<dyn std::error::Error>>(())
            })?;
    }

    eprintln!("Server shut down.");
    Ok(())
}

/// Set up logs, traces and metrics as docs/OBSERVABILITY.md describes. Log
/// lines go to stderr (stdout is the MCP stdio channel) at `ASG_LOG`, else
/// `RUST_LOG`, else `default_level`, in the form `ASG_LOG_FORMAT` names.
/// With the `otlp` feature, everything is also sent over OTLP once
/// `OTEL_EXPORTER_OTLP_ENDPOINT` is set. Serving HTTP keeps metrics in
/// memory for `/metrics`.
///
/// `None` when an embedding host has installed its own subscriber, which
/// stays in charge. Keep the guard until `main` returns: dropping it sends
/// what's pending.
fn init_telemetry(
    default_level: &str,
    serving_http: bool,
) -> Result<Option<oxtel::Guard>, oxtel::Error> {
    use oxtel::sdk::logs::LocalFormat;
    let level = std::env::var("ASG_LOG")
        .or_else(|_| std::env::var("RUST_LOG"))
        .ok()
        .filter(|l| !l.trim().is_empty())
        .unwrap_or_else(|| default_level.to_string());
    let mut builder = oxtel::builder()
        .with_default_service_name("agentstategraph")
        .with_resource_attribute(oxtel::KeyValue::new(
            "service.version",
            env!("CARGO_PKG_VERSION"),
        ))
        .with_logs_filter(level);
    match std::env::var("ASG_LOG_FORMAT")
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "" | "auto" => {}
        "text" => builder = builder.with_local_logs(LocalFormat::Text),
        "json" => builder = builder.with_local_logs(LocalFormat::Json),
        "ecs" => builder = builder.with_local_logs(LocalFormat::Ecs),
        other => {
            eprintln!("Warning: ASG_LOG_FORMAT={other} isn't auto, text, json or ecs; using auto")
        }
    }
    if serving_http {
        builder = builder.with_pull_metrics();
    }
    match builder.install() {
        Ok(guard) => Ok(Some(guard)),
        Err(oxtel::Error::SubscriberAlreadySet | oxtel::Error::RecorderAlreadySet) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Resolves on SIGINT (Ctrl-C) or, on Unix, SIGTERM, which container
/// runtimes send to stop a server. The server then finishes what's in
/// flight and `main` returns, so pending telemetry is sent.
async fn shutdown_signal() {
    let interrupt = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => {
                terminate.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = interrupt => {}
        () = terminate => {}
    }
    tracing::info!("shutting down");
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    // HTTP serving logs each request at info; an MCP stdio server stays
    // quiet unless something is wrong.
    let serving_http = args.iter().any(|a| a == "--http");
    let telemetry = init_telemetry(if serving_http { "info" } else { "warn" }, serving_http)?;
    if agentstategraph_mcp::trace_links_from_env() {
        agentstategraph::link_commits_to_traces(agentstategraph_mcp::current_trace);
    }

    if args.get(1).map(String::as_str) == Some("migrate") {
        let rest: Vec<String> = args.iter().skip(2).cloned().collect();
        let code = migrate::run(&rest);
        drop(telemetry);
        std::process::exit(code);
    }
    if args.get(1).map(String::as_str) == Some("healthcheck") {
        let code = healthcheck(&args[2..]);
        drop(telemetry);
        std::process::exit(code);
    }

    let meters = telemetry.as_ref().and_then(|t| t.meter_provider().cloned());
    cmd_serve_with(parse_args(&args[1..]), meters)
}

#[cfg(test)]
mod tests {
    use super::{resolve_bind_addr, tls_advice};

    #[test]
    fn tls_advice_warns_on_non_loopback_without_tls() {
        let msg = tls_advice("0.0.0.0", false).expect("expected warn msg");
        assert!(msg.contains("TLS"));
    }

    #[test]
    fn tls_advice_silent_on_loopback() {
        assert!(tls_advice("127.0.0.1", false).is_none());
        assert!(tls_advice("localhost", false).is_none());
        assert!(tls_advice("::1", false).is_none());
    }

    #[test]
    fn tls_advice_silent_when_tls_configured() {
        assert!(tls_advice("0.0.0.0", true).is_none());
        assert!(tls_advice("10.0.0.5", true).is_none());
    }

    #[test]
    fn tls_advice_warns_on_lan_ip() {
        assert!(tls_advice("10.0.0.5", false).is_some());
        assert!(tls_advice("192.168.1.2", false).is_some());
    }

    #[test]
    fn default_bind_is_loopback() {
        let args: Vec<String> = vec!["--http".into()];
        assert_eq!(resolve_bind_addr(&args, None), "127.0.0.1");
    }

    #[test]
    fn cli_bind_overrides_default_and_env() {
        let args: Vec<String> = vec!["--bind".into(), "0.0.0.0".into()];
        assert_eq!(resolve_bind_addr(&args, Some("10.0.0.5".into())), "0.0.0.0");
    }

    #[test]
    fn env_bind_used_when_no_cli() {
        let args: Vec<String> = vec![];
        assert_eq!(
            resolve_bind_addr(&args, Some("192.168.1.2".into())),
            "192.168.1.2"
        );
    }

    #[test]
    fn empty_env_bind_falls_back_to_default() {
        let args: Vec<String> = vec![];
        assert_eq!(resolve_bind_addr(&args, Some(String::new())), "127.0.0.1");
    }

    #[test]
    fn healthcheck_reaches_the_server_from_this_machine() {
        assert_eq!(super::healthcheck_host("0.0.0.0"), "127.0.0.1");
        assert_eq!(super::healthcheck_host("localhost"), "127.0.0.1");
        assert_eq!(super::healthcheck_host("::"), "::1");
        assert_eq!(super::healthcheck_host("[::]"), "::1");
        assert_eq!(super::healthcheck_host("127.0.0.1"), "127.0.0.1");
        assert_eq!(super::healthcheck_host("10.0.0.5"), "10.0.0.5");
    }

    /// A one-shot server answering every connection with `status`.
    fn answer_with(status: &'static str) -> u16 {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut request = [0u8; 512];
                let _ = stream.read(&mut request);
                let _ = stream.write_all(
                    format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\n\r\n").as_bytes(),
                );
            }
        });
        port
    }

    #[test]
    fn healthcheck_exit_codes() {
        let args = |port: u16| vec!["--port".to_string(), port.to_string()];
        assert_eq!(super::healthcheck(&args(answer_with("200 OK"))), 0);
        assert_eq!(
            super::healthcheck(&args(answer_with("503 Service Unavailable"))),
            1
        );
        // Nothing listening.
        let closed = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        assert_eq!(super::healthcheck(&args(closed)), 1);
    }

    #[test]
    fn cors_origins_and_allowed_hosts_repeat() {
        let args: Vec<String> = [
            "--http",
            "--cors-origin",
            "https://a.example",
            "--allowed-host",
            "asg.internal",
            "--cors-origin",
            "https://b.example",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let parsed = super::parse_args(&args);
        assert_eq!(
            parsed.cors_origins,
            vec!["https://a.example", "https://b.example"]
        );
        assert_eq!(parsed.allowed_hosts, vec!["asg.internal"]);
    }

    #[test]
    fn parse_args_defaults() {
        let parsed = super::parse_args(&[]);
        assert_eq!(parsed.storage_type, "sqlite");
        assert_eq!(parsed.http_port, 3001);
        assert!(!parsed.http_mode);
        assert!(!parsed.auth_enabled);
        assert_eq!(parsed.tenant_id, "default");
        assert_eq!(parsed.default_namespace, "default");
    }

    #[test]
    fn parse_args_namespace_flag() {
        let args: Vec<String> = vec!["--namespace".into(), "project-alpha".into()];
        let parsed = super::parse_args(&args);
        assert_eq!(parsed.default_namespace, "project-alpha");
    }

    #[test]
    fn parse_args_http_port() {
        let args: Vec<String> = vec!["--http".into(), "--port".into(), "8080".into()];
        let parsed = super::parse_args(&args);
        assert!(parsed.http_mode);
        assert_eq!(parsed.http_port, 8080);
    }

    #[test]
    fn parse_args_policy_fail_safe() {
        assert_eq!(super::parse_args(&[]).policy_fail_safe, "deny");
        let args: Vec<String> = vec!["--policy-fail-safe".into(), "allow".into()];
        assert_eq!(super::parse_args(&args).policy_fail_safe, "allow");
    }

    #[test]
    fn a_misspelt_policy_fail_safe_stops_the_server() {
        let args: Vec<String> = vec![
            "--storage".into(),
            "memory".into(),
            "--policy-fail-safe".into(),
            "alow".into(),
        ];
        let err = super::cmd_serve(super::parse_args(&args)).expect_err("started anyway");
        assert!(err.to_string().contains("must be deny or allow"), "{err}");
    }

    #[test]
    fn parse_args_external_evaluators() {
        let args: Vec<String> = vec![
            "--external-evaluator".into(),
            "wasm".into(),
            "--external-evaluator".into(),
            "cedar".into(),
        ];
        let parsed = super::parse_args(&args);
        assert_eq!(parsed.external_evaluators, vec!["wasm", "cedar"]);
    }
}
