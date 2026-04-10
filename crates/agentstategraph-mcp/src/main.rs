//! agentstategraph-mcp — MCP server for AgentStateGraph.
//!
//! Run with: cargo run -p agentstategraph-mcp
//! Or with options: cargo run -p agentstategraph-mcp -- --storage memory
//!                  cargo run -p agentstategraph-mcp -- --path /data/my-state.db

mod server;

use std::sync::Arc;

use agentstategraph::Repository;
use agentstategraph_storage::{MemoryStorage, SqliteStorage};
use rmcp::ServiceExt;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();

    let mut storage_type = "sqlite";
    let mut db_path = "./agentstategraph.db".to_string();

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--storage" | "-s" => {
                i += 1;
                if i < args.len() {
                    storage_type = if args[i] == "memory" {
                        "memory"
                    } else {
                        "sqlite"
                    };
                }
            }
            "--path" | "-p" => {
                i += 1;
                if i < args.len() {
                    db_path = args[i].clone();
                }
            }
            "--help" | "-h" => {
                eprintln!("AgentStateGraph MCP Server v{}", env!("CARGO_PKG_VERSION"));
                eprintln!();
                eprintln!("USAGE:");
                eprintln!("  agentstategraph-mcp [OPTIONS]");
                eprintln!();
                eprintln!("OPTIONS:");
                eprintln!("  -s, --storage <TYPE>  Storage backend: sqlite (default) or memory");
                eprintln!(
                    "  -p, --path <PATH>     SQLite database path (default: ./agentstategraph.db)"
                );
                eprintln!("  -h, --help            Print help");
                std::process::exit(0);
            }
            _ => {}
        }
        i += 1;
    }

    eprintln!("AgentStateGraph MCP Server v{}", env!("CARGO_PKG_VERSION"));

    let repo: Arc<Repository> = match storage_type {
        "memory" => {
            eprintln!("Storage: in-memory (ephemeral)");
            Arc::new(Repository::new(Box::new(MemoryStorage::new())))
        }
        _ => {
            eprintln!("Storage: {}", db_path);
            let storage = SqliteStorage::open(&db_path)?;
            Arc::new(Repository::new(Box::new(storage)))
        }
    };

    repo.init()?;
    eprintln!("Repository initialized. Waiting for MCP client...");

    // Build and run the async runtime
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

    eprintln!("MCP server shut down.");
    Ok(())
}
