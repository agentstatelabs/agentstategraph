//! A tool call that fails comes back as an error result (`isError`), so MCP
//! clients and telemetry see the failure: the tool's span gets `error.type`
//! `tool_error`, as the MCP conventions have it (DESIGN.md Plan H t-006).
//! That covers `commit_spec` refused by policy. The specific code stays in
//! the text, and on the library spans inside the call.

use std::sync::Arc;

use agentstategraph::{CommitOptions, Repository};
use agentstategraph_core::IntentCategory;
use agentstategraph_mcp::server::AgentStateGraphServer;
use agentstategraph_storage::SqliteStorage;
use oxtel::Value;
use oxtel::sdk::trace::{InMemorySpanExporter, Sampler, SpanData, TracerProvider};
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::{RoleClient, RunningService, ServiceExt};
use tracing_subscriber::layer::SubscriberExt;

fn call(tool: &'static str, arguments: serde_json::Value) -> CallToolRequestParams {
    let serde_json::Value::Object(arguments) = arguments else {
        panic!("arguments are an object")
    };
    CallToolRequestParams::new(tool).with_arguments(arguments)
}

fn text(result: &CallToolResult) -> &str {
    result
        .content
        .first()
        .and_then(|content| content.as_text())
        .map(|text| text.text.as_str())
        .unwrap_or("")
}

fn tool_span<'a>(spans: &'a [SpanData], name: &str) -> Vec<&'a SpanData> {
    spans
        .iter()
        .filter(|s| s.name == format!("tools/call {name}"))
        .collect()
}

fn error_type(span: &SpanData) -> Option<Value> {
    span.attributes
        .iter()
        .find(|kv| kv.key == "error.type")
        .map(|kv| kv.value.clone())
}

/// Serve `repo` as `main` does, instrumented, to a client over a pipe.
async fn connect(
    repo: Arc<Repository>,
) -> (RunningService<RoleClient, ()>, tokio::task::JoinHandle<()>) {
    let (server_io, client_io) = tokio::io::duplex(1 << 20);
    let server = tokio::spawn(async move {
        let running = oxtel_rmcp::Instrumented::new(AgentStateGraphServer::new(repo))
            .with_transport("pipe")
            .serve(server_io)
            .await
            .expect("server starts");
        let _ = running.waiting().await;
    });
    let client = ().serve(client_io).await.expect("client connects");
    (client, server)
}

#[tokio::test(flavor = "current_thread")]
async fn failed_tool_calls_are_error_results() {
    let spans = InMemorySpanExporter::default();
    let traces = TracerProvider::builder()
        .with_sampler(Sampler::AlwaysOn)
        .with_simple_exporter(spans.clone())
        .build();
    let _guard =
        tracing::subscriber::set_default(tracing_subscriber::registry().with(traces.layer()));

    let repo = Arc::new(Repository::new(Box::new(
        SqliteStorage::in_memory().unwrap(),
    )));
    repo.init().unwrap();
    repo.set_json(
        "main",
        "/victim",
        &serde_json::json!("still here"),
        CommitOptions::new("seed", IntentCategory::Checkpoint, "seed"),
    )
    .unwrap();
    // A destructive speculation, which the default fail-safe refuses.
    let handle = repo.speculate("main", None).unwrap();
    repo.spec_delete(handle, "/victim").unwrap();
    let (client, server) = connect(Arc::clone(&repo)).await;

    let found = client
        .call_tool(call(
            "agentstategraph_get",
            serde_json::json!({ "path": "/victim" }),
        ))
        .await
        .unwrap();
    assert_ne!(found.is_error, Some(true), "{}", text(&found));
    let missing = client
        .call_tool(call(
            "agentstategraph_get",
            serde_json::json!({ "path": "/nowhere" }),
        ))
        .await
        .unwrap();
    assert_eq!(missing.is_error, Some(true));
    assert!(text(&missing).starts_with("Error ["), "{}", text(&missing));
    let refused = client
        .call_tool(call(
            "agentstategraph_commit_spec",
            serde_json::json!({
                "handle_id": handle.id(),
                "intent_category": "Refine",
                "intent_description": "delete the victim",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(refused.is_error, Some(true));
    let decision: serde_json::Value = serde_json::from_str(text(&refused)).unwrap();
    assert_eq!(decision["translated"]["kind"], "deny");
    assert_eq!(
        repo.get_json("main", "/victim").unwrap(),
        serde_json::json!("still here")
    );
    client.cancel().await.unwrap();
    server.await.unwrap();

    let recorded = spans.finished_spans();
    let gets = tool_span(&recorded, "agentstategraph_get");
    assert_eq!(gets.len(), 2);
    assert_eq!(error_type(gets[0]), None);
    assert_eq!(error_type(gets[1]), Some(Value::from("tool_error")));
    let commit = tool_span(&recorded, "agentstategraph_commit_spec");
    assert_eq!(error_type(commit[0]), Some(Value::from("tool_error")));
}

#[test]
fn error_text_is_recognised() {
    use agentstategraph_mcp::server::reports_error;
    assert!(reports_error("Error [write_conflict]: the ref moved"));
    assert!(reports_error("Error: unknown weekday 'Funday'"));
    assert!(reports_error(r#"{"error":"taint not found"}"#));
    assert!(!reports_error("Speculation committed: sg_0123"));
    assert!(!reports_error(r#""Error: a stored string""#));
    assert!(!reports_error(
        r#"{"kind":"deny","matched_policy":"p@1","reason":"no"}"#
    ));
}
