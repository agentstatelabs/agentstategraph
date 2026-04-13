//! agentstategraph-mcp — MCP + HTTP server for AgentStateGraph.
//!
//! Run as MCP server (stdio):  cargo run -p agentstategraph-mcp
//! Run as HTTP server:         cargo run -p agentstategraph-mcp -- --http
//! Both:                       cargo run -p agentstategraph-mcp -- --http --port 3001
//! Options:                    cargo run -p agentstategraph-mcp -- --storage memory
//!                             cargo run -p agentstategraph-mcp -- --path /data/state.db

mod auth;
mod http;
mod server;

use std::sync::Arc;

use agentstategraph::Repository;
use agentstategraph_storage::{MemoryStorage, SqliteStorage};
use rmcp::ServiceExt;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();

    let mut storage_type = "sqlite";
    let mut db_path = "./agentstategraph.db".to_string();
    let mut database_url = String::new();
    let mut tenant_id = "default".to_string();
    let mut http_mode = false;
    let mut http_port: u16 = 3001;
    let mut auth_enabled = false;
    let mut keys_file = String::new();

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--storage" | "-s" => {
                i += 1;
                if i < args.len() {
                    storage_type = match args[i].as_str() {
                        "memory" => "memory",
                        "postgres" | "pg" => "postgres",
                        _ => "sqlite",
                    };
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
            "--help" | "-h" => {
                eprintln!("AgentStateGraph Server v{}", env!("CARGO_PKG_VERSION"));
                eprintln!();
                eprintln!("USAGE:");
                eprintln!("  agentstategraph-mcp [OPTIONS]");
                eprintln!();
                eprintln!("MODES:");
                eprintln!("  (default)             MCP server over stdio");
                eprintln!("  --http                HTTP REST API server");
                eprintln!();
                eprintln!("OPTIONS:");
                eprintln!("  -s, --storage <TYPE>  Storage backend: sqlite (default), memory, or postgres");
                eprintln!(
                    "  -p, --path <PATH>     SQLite database path (default: ./agentstategraph.db)"
                );
                eprintln!("      --database-url <URL>  Postgres connection URL (required for --storage postgres)");
                eprintln!("      --tenant <ID>     Tenant ID for multi-tenant Postgres (default: \"default\")");
                eprintln!("      --port <PORT>     HTTP port (default: 3001, requires --http)");
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

    eprintln!("AgentStateGraph Server v{}", env!("CARGO_PKG_VERSION"));

    // Check for DATABASE_URL env var as fallback for postgres
    if database_url.is_empty() {
        if let Ok(url) = std::env::var("DATABASE_URL") {
            database_url = url;
            if storage_type == "sqlite" {
                storage_type = "postgres";
            }
        }
    }

    let repo: Arc<Repository> = match storage_type {
        "memory" => {
            eprintln!("Storage: in-memory (ephemeral)");
            Arc::new(Repository::new(Box::new(MemoryStorage::new())))
        }
        "postgres" => {
            if database_url.is_empty() {
                eprintln!("Error: --database-url or DATABASE_URL required for postgres storage");
                std::process::exit(1);
            }
            eprintln!("Storage: postgres (tenant: {})", tenant_id);
            let rt = tokio::runtime::Runtime::new()?;
            let storage = rt.block_on(async {
                agentstategraph_storage::PostgresStorage::connect_tenant(&database_url, &tenant_id).await
            })?;
            Arc::new(Repository::new(Box::new(storage)))
        }
        _ => {
            eprintln!("Storage: {}", db_path);
            let storage = SqliteStorage::open(&db_path)?;
            Arc::new(Repository::new(Box::new(storage)))
        }
    };

    repo.init()?;

    if http_mode {
        if auth_enabled {
            eprintln!("Auth: enabled (keys file: {})", if keys_file.is_empty() { "none" } else { &keys_file });
        } else {
            eprintln!("Auth: disabled (single-tenant mode)");
        }
        eprintln!("HTTP API listening on http://0.0.0.0:{}", http_port);
        eprintln!("Try: curl http://localhost:{}/api/health", http_port);

        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?
            .block_on(async {
                let app = if auth_enabled {
                    let kf = if keys_file.is_empty() { None } else { Some(keys_file.as_str()) };
                    http::router_multi_tenant(repo, kf)
                } else {
                    http::router(repo)
                };
                let addr = format!("0.0.0.0:{}", http_port);
                let listener = tokio::net::TcpListener::bind(&addr).await?;
                axum::serve(listener, app).await?;
                Ok::<(), Box<dyn std::error::Error>>(())
            })?;
    } else {
        eprintln!("MCP server waiting for client on stdio...");

        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(async {
                let service = server::AgentStateGraphServer::new(repo)
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
