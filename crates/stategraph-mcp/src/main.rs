//! stategraph-mcp — MCP server for StateGraph.
//!
//! Run with: cargo run -p stategraph-mcp
//! Or install and run: stategraph-mcp
//!
//! Connects to any MCP-compatible agent over stdio.
//! Default storage: SQLite at ./stategraph.db

mod server;

use std::sync::Arc;

use rmcp::ServiceExt;
use stategraph::Repository;
use stategraph_storage::SqliteStorage;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Log to stderr (stdout is for MCP protocol)
    eprintln!("StateGraph MCP Server v{}", env!("CARGO_PKG_VERSION"));
    eprintln!("Storage: ./stategraph.db");

    // Initialize storage and repository
    let storage = SqliteStorage::open("./stategraph.db")?;
    let repo = Arc::new(Repository::new(Box::new(storage)));
    repo.init()?;

    eprintln!("Repository initialized. Waiting for MCP client connection...");

    // Create the MCP server and serve over stdio
    let service = server::StateGraphServer::new(repo)
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|e| format!("Failed to start MCP server: {}", e))?;

    // Wait for the connection to close
    service.waiting().await?;

    eprintln!("MCP server shutting down.");
    Ok(())
}
