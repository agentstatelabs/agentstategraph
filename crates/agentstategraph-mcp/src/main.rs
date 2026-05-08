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
use agentstategraph_storage::SqliteStorage;
use rmcp::ServiceExt;

/// All resolved server configuration, parsed from CLI args + environment.
pub struct ServeArgs {
    pub storage_type: String,
    pub db_path: String,
    pub database_url: String,
    pub tenant_id: String,
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
    let mut http_mode = false;
    let mut http_port: u16 = 3001;
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
                    "      --database-url <URL>  Postgres connection URL (required for --storage postgres)"
                );
                eprintln!(
                    "      --tenant <ID>     Tenant ID for multi-tenant Postgres (default: \"default\")"
                );
                eprintln!("      --port <PORT>     HTTP port (default: 3001, requires --http)");
                eprintln!(
                    "      --bind <ADDR>     Bind address (default: 127.0.0.1; pass 0.0.0.0 for LAN; env ASG_BIND)"
                );
                eprintln!(
                    "      --rate-limit-rpm <N>  Per-IP requests/minute (default: 600, 0 disables; env ASG_RATE_LIMIT_RPM)"
                );
                eprintln!(
                    "      --pg-pool-size <N>  Max Postgres connections (default: 32; env ASG_PG_POOL_SIZE)"
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
                eprintln!("  -h, --help            Print help");
                eprintln!();
                eprintln!("HTTP API ENDPOINTS:");
                eprintln!("  GET  /api/health                  Health check");
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
    }
}

/// Build the repository and run either the HTTP or MCP server.
pub fn cmd_serve(args: ServeArgs) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("AgentStateGraph Server v{}", env!("CARGO_PKG_VERSION"));

    let repo: Arc<Repository> = match args.storage_type.as_str() {
        "memory" => {
            eprintln!("Storage: in-memory (ephemeral)");
            Arc::new(Repository::new(Box::new(
                SqliteStorage::in_memory().expect("in-memory sqlite"),
            )))
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
            let rt = tokio::runtime::Runtime::new()?;
            let storage = rt.block_on(async {
                agentstategraph_storage::PostgresStorage::connect_tenant_with_pool_size(
                    &args.database_url,
                    &args.tenant_id,
                    args.pg_pool_size,
                )
                .await
            })?;
            Arc::new(Repository::new(Box::new(storage)))
        }
        _ => {
            eprintln!("Storage: {}", args.db_path);
            let storage = SqliteStorage::open(&args.db_path)?;
            Arc::new(Repository::new(Box::new(storage)))
        }
    };

    repo.init()?;

    if args.http_mode {
        eprintln!(
            "Rate limit: {} requests/minute per peer IP{}",
            args.rate_limit_rpm,
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
        eprintln!("Try: curl http://localhost:{}/api/health", args.http_port);

        #[cfg(feature = "providers")]
        if let Some(ref url) = args.asd_url {
            eprintln!("ASD proxy: /api/code/* → {}/api/v1/*", url);
        }
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?
            .block_on(async {
                let app = if args.auth_enabled {
                    let kf = if args.keys_file.is_empty() {
                        None
                    } else {
                        Some(args.keys_file.as_str())
                    };
                    let tenant_mgr = TenantManager::multi_tenant(repo.clone(), kf);

                    let provided = args.initial_admin_key.clone();
                    if let Some(k) = provided {
                        if tenant_mgr.register_admin_key(k, "bootstrap-admin") {
                            eprintln!("Auth: registered bootstrap admin key");
                        }
                    } else if !tenant_mgr.has_admin_key() {
                        eprintln!(
                            "Error: --auth / --keys-file enabled but no admin key present. \
                             Provide --initial-admin-key <KEY> or ASG_INITIAL_ADMIN_KEY to \
                             bootstrap, or add an is_admin=true entry to the keys file."
                        );
                        std::process::exit(2);
                    }

                    http::build_router_for_test(repo, tenant_mgr, args.rate_limit_rpm)
                } else {
                    http::router_with_rate_limit(repo, args.rate_limit_rpm)
                };
                #[cfg(feature = "providers")]
                let app = if let Some(asd_url) = args.asd_url {
                    app.merge(http::asd_proxy_router(asd_url))
                } else {
                    app
                };
                let addr = format!("{}:{}", args.bind_addr, args.http_port);
                let listener = tokio::net::TcpListener::bind(&addr).await?;
                axum::serve(
                    listener,
                    app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
                )
                .await?;
                Ok::<(), Box<dyn std::error::Error>>(())
            })?;
    } else {
        eprintln!("MCP server waiting for client on stdio...");

        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(async {
                let mut srv = server::AgentStateGraphServer::new(repo);
                srv = apply_external_evaluators(srv, &args.external_evaluators);
                let service = srv
                    .serve(rmcp::transport::stdio())
                    .await
                    .map_err(|e| format!("MCP server error: {}", e))?;

                service.waiting().await?;
                Ok::<(), Box<dyn std::error::Error>>(())
            })?;
    }

    eprintln!("Server shut down.");
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();

    if args.get(1).map(String::as_str) == Some("migrate") {
        let rest: Vec<String> = args.iter().skip(2).cloned().collect();
        std::process::exit(migrate::run(&rest));
    }

    cmd_serve(parse_args(&args[1..]))
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
    fn parse_args_defaults() {
        let parsed = super::parse_args(&[]);
        assert_eq!(parsed.storage_type, "sqlite");
        assert_eq!(parsed.http_port, 3001);
        assert!(!parsed.http_mode);
        assert!(!parsed.auth_enabled);
        assert_eq!(parsed.tenant_id, "default");
    }

    #[test]
    fn parse_args_http_port() {
        let args: Vec<String> = vec!["--http".into(), "--port".into(), "8080".into()];
        let parsed = super::parse_args(&args);
        assert!(parsed.http_mode);
        assert_eq!(parsed.http_port, 8080);
    }

    #[test]
    fn parse_args_external_evaluators() {
        let args: Vec<String> =
            vec!["--external-evaluator".into(), "wasm".into(), "--external-evaluator".into(), "cedar".into()];
        let parsed = super::parse_args(&args);
        assert_eq!(parsed.external_evaluators, vec!["wasm", "cedar"]);
    }
}
