//! One HTTP write, end to end (DESIGN.md Plan H t-012): it makes one trace,
//! from the request through the repository to each SQL statement; its log
//! line carries that trace; and its commit links back to it, in the log and
//! blame (t-009). One test, because it installs the process's telemetry and
//! trace source.

use std::net::SocketAddr;
use std::sync::Arc;

use agentstategraph::Repository;
use agentstategraph_mcp::auth::TenantManager;
use agentstategraph_mcp::http::{self, OriginPolicy, RequestBounds};
use agentstategraph_storage::SqliteStorage;
use oxtel::Value;
use oxtel::sdk::logs::InMemoryLogExporter;
use oxtel::sdk::trace::{InMemorySpanExporter, SpanData};
use serde_json::json;

fn attr(span: &SpanData, key: &str) -> Option<Value> {
    span.attributes
        .iter()
        .find(|kv| kv.key == key)
        .map(|kv| kv.value.clone())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_write_makes_one_trace_its_logs_carry_and_its_commit_links_to() {
    let spans = InMemorySpanExporter::default();
    let records = InMemoryLogExporter::default();
    // As the server's telemetry, with SQL statement spans on (ASG_SQL_SPANS).
    let _guard = oxtel::builder()
        .without_env_exporters()
        .without_local_logs()
        .with_traces_filter("info")
        .with_traces({
            let spans = spans.clone();
            move |traces| traces.with_simple_exporter(spans)
        })
        .with_logs_filter("info")
        .with_logs({
            let records = records.clone();
            move |logs| logs.with_simple_exporter(records)
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

    // One trace, from the request down to each SQL statement.
    let in_trace: Vec<&SpanData> = recorded
        .iter()
        .filter(|s| s.context.trace_id == request.context.trace_id)
        .collect();
    let parent_of = |span: &SpanData| {
        in_trace
            .iter()
            .find(|s| Some(s.context.span_id) == span.parent_span_id)
            .copied()
    };
    let descends_from = |span: &SpanData, ancestor: &SpanData| {
        let mut at = span;
        while let Some(parent) = parent_of(at) {
            if parent.context.span_id == ancestor.context.span_id {
                return true;
            }
            at = parent;
        }
        false
    };
    assert!(
        descends_from(set, request),
        "the write's span is under the request's"
    );
    let statements: Vec<&&SpanData> = in_trace
        .iter()
        .filter(|s| attr(s, "db.system.name") == Some(Value::from("sqlite")))
        .collect();
    assert!(
        statements
            .iter()
            .any(|s| s.name.starts_with("INSERT commits")),
        "{:?}",
        statements.iter().map(|s| &s.name).collect::<Vec<_>>()
    );
    for statement in &statements {
        assert!(
            descends_from(statement, request),
            "{} isn't under the request",
            statement.name
        );
    }
    assert!(
        recorded
            .iter()
            .filter(|s| s.context.trace_id == request.context.trace_id)
            .all(|s| s.parent_span_id.is_some() || s.context.span_id == request.context.span_id),
        "the request's span is the trace's only root"
    );

    // The request's log line carries the trace.
    let logged = records.records();
    let line = logged
        .iter()
        .find(|r| {
            r.trace_id == Some(request.context.trace_id)
                && r.body
                    .as_ref()
                    .is_some_and(|b| format!("{b:?}").contains("request"))
        })
        .unwrap_or_else(|| panic!("no request log line in the write's trace: {logged:?}"));
    assert_eq!(line.span_id, Some(request.context.span_id));
}
