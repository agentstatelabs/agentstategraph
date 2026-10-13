//! A commit links back to the trace of the request that made it, and the log
//! and blame show it (DESIGN.md Plan H t-009). One test, because it installs
//! the process's telemetry and trace source.

use std::net::SocketAddr;
use std::sync::Arc;

use agentstategraph::Repository;
use agentstategraph_mcp::auth::TenantManager;
use agentstategraph_mcp::http::{self, OriginPolicy, RequestBounds};
use agentstategraph_storage::SqliteStorage;
use oxtel::Value;
use oxtel::sdk::trace::{InMemorySpanExporter, SpanData};
use serde_json::json;

fn attr(span: &SpanData, key: &str) -> Option<Value> {
    span.attributes
        .iter()
        .find(|kv| kv.key == key)
        .map(|kv| kv.value.clone())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn commits_link_to_the_request_that_made_them() {
    let spans = InMemorySpanExporter::default();
    let _guard = oxtel::builder()
        .without_env_exporters()
        .without_local_logs()
        .with_traces({
            let spans = spans.clone();
            move |traces| traces.with_simple_exporter(spans)
        })
        .install()
        .expect("install telemetry");
    assert!(agentstategraph::link_commits_to_traces(
        agentstategraph_mcp::current_trace
    ));

    let repo = Arc::new(Repository::new(Box::new(
        SqliteStorage::in_memory().unwrap(),
    )));
    repo.init().unwrap();
    let mgr = TenantManager::single_tenant(Arc::clone(&repo));
    let app = http::with_server_layers(
        http::build_router_with(Arc::clone(&mgr), 0, None),
        mgr,
        &RequestBounds::default(),
        &OriginPolicy::for_bind("127.0.0.1", &[], &[]),
        None,
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let written = client
        .post(format!("{base}/api/state/main/set"))
        .header("x-request-id", "write-1")
        .json(&json!({
            "path": "/greeting",
            "value": "hello",
            "intent_category": "Refine",
            "intent_description": "say hello",
        }))
        .send()
        .await
        .unwrap();
    assert!(
        written.status().is_success(),
        "{}",
        written.text().await.unwrap()
    );

    let log: serde_json::Value = client
        .get(format!("{base}/api/log/main?limit=1"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let blame: serde_json::Value = client
        .get(format!("{base}/api/blame/main?path=/greeting"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let recorded = spans.finished_spans();
    let request = recorded
        .iter()
        .find(|s| attr(s, "http.request.header.x-request-id") == Some(Value::from("write-1")))
        .expect("the write's request span");
    let trace_id = request.context.trace_id.to_string();
    let commit = &log[0];
    assert_eq!(commit["trace_id"], trace_id, "{log}");
    let set = recorded
        .iter()
        .find(|s| s.name == "asg.repo.set")
        .expect("the write's library span");
    assert_eq!(set.context.trace_id, request.context.trace_id);
    assert_eq!(commit["span_id"], set.context.span_id.to_string());
    assert_eq!(
        attr(set, "asg.commit.id"),
        Some(Value::from(commit["full_id"].as_str().unwrap().to_string()))
    );
    assert_eq!(blame["trace_id"], trace_id, "{blame}");
}
