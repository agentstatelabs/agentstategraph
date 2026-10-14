//! The site's metric catalogue matches what the server records (DESIGN.md
//! Plan H t-012). A workload over HTTP and MCP makes the server record as
//! many of its metrics as it can. Every metric it records must be in the
//! catalogue in `site/src/content/docs/guides/observability.md`, as the
//! instrument and unit given there. Every metric in the catalogue must be
//! recorded, or be listed below with the reason this workload can't make it.
//! One test, because it installs the process's telemetry.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use agentstategraph::Repository;
use agentstategraph_mcp::auth::TenantManager;
use agentstategraph_mcp::http::{self, OriginPolicy, RequestBounds};
use agentstategraph_mcp::server::AgentStateGraphServer;
use agentstategraph_storage::SqliteStorage;
use oxtel::sdk::metrics::MetricKind;
use rmcp::model::CallToolRequestParams;
use rmcp::service::ServiceExt;
use serde_json::json;

/// Catalogue entries this workload can't produce, and why.
const NOT_PRODUCED: &[(&str, &str)] = &[
    ("asg.ref.cas_retries", "only when writers race on a ref"),
    (
        "asg.policy.evaluator.errors",
        "only with an external evaluator configured",
    ),
    ("db.client.connection.count", "Postgres pools only"),
    ("db.client.connection.max", "Postgres pools only"),
    ("db.client.connection.timeouts", "Postgres pools only"),
];

/// A catalogue row: the metric's instrument and unit.
struct Entry {
    instrument: String,
    unit: String,
}

/// The rows of the catalogue's table: `| \`name\` | instrument | unit | ...`.
fn catalogue() -> BTreeMap<String, Entry> {
    let page = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../site/src/content/docs/guides/observability.md"),
    )
    .expect("the site's observability page");
    let metrics = page
        .split("\n## Metrics\n")
        .nth(1)
        .expect("a Metrics section");
    metrics
        .lines()
        .filter(|line| line.starts_with("| `"))
        .map(|line| {
            let cells: Vec<&str> = line.split('|').map(str::trim).collect();
            (
                cells[1].trim_matches('`').to_string(),
                Entry {
                    instrument: cells[2].to_string(),
                    unit: cells[3].trim_matches('`').to_string(),
                },
            )
        })
        .collect()
}

fn instrument(kind: MetricKind) -> &'static [&'static str] {
    match kind {
        MetricKind::Counter => &["counter"],
        // The `metrics` facade records an up-down counter as a gauge.
        MetricKind::Gauge => &["gauge", "up-down counter"],
        MetricKind::Histogram => &["histogram"],
    }
}

async fn serve(app: axum::Router) -> String {
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
    format!("http://{addr}")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_catalogue_matches_what_the_server_records() {
    let guard = oxtel::builder()
        .without_env_exporters()
        .without_local_logs()
        .with_pull_metrics()
        .install()
        .expect("install telemetry");
    let meters = guard.meter_provider().unwrap().clone();
    agentstategraph_mcp::store_gauges::describe();

    let path = std::env::temp_dir().join(format!(
        "asg-catalogue-{}.db",
        uuid::Uuid::new_v4().simple()
    ));
    let repo = Arc::new(Repository::new(Box::new(
        SqliteStorage::open(&path).unwrap(),
    )));
    repo.init().unwrap();
    let mgr = TenantManager::single_tenant(Arc::clone(&repo));
    let base = serve(http::with_server_layers(
        http::build_router_with(Arc::clone(&mgr), 0, None),
        mgr,
        &RequestBounds::default(),
        &OriginPolicy::for_bind("127.0.0.1", &[], &[]),
        Some(meters.clone()),
    ))
    .await;
    let client = reqwest::Client::new();

    // HTTP: writes, reads, a miss and a rejection.
    for i in 0..3 {
        let written = client
            .post(format!("{base}/api/state/main/set"))
            .json(&json!({
                "path": format!("/catalogue/k{i}"),
                "value": i,
                "intent_category": "Refine",
                "intent_description": "catalogue",
            }))
            .send()
            .await
            .unwrap();
        assert!(written.status().is_success());
    }
    client
        .get(format!("{base}/api/state/main?path=/catalogue"))
        .send()
        .await
        .unwrap();
    client
        .get(format!("{base}/api/state/no-such-branch?path=/"))
        .send()
        .await
        .unwrap();
    let refused = client
        .get(format!("{base}/api/state/main?path=/"))
        .header("host", "rebound.example")
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status().as_u16(), 403);

    // Garbage collection: refused while history isn't distilled, then a
    // sweep and a vacuum once it is.
    let sweep = |vacuum: bool| {
        client
            .post(format!("{base}/api/gc/sweep"))
            .json(&json!({ "mutate": true, "vacuum": vacuum }))
            .send()
    };
    let refused: serde_json::Value = sweep(false).await.unwrap().json().await.unwrap();
    assert_eq!(refused["refused"], true, "{refused}");
    client
        .get(format!("{base}/api/history?refresh=true"))
        .send()
        .await
        .unwrap();
    let swept: serde_json::Value = sweep(true).await.unwrap().json().await.unwrap();
    assert_eq!(swept["mutated"], true, "{swept}");

    // The store gauges, as the sampler takes them.
    assert!(agentstategraph_mcp::store_gauges::sample(&repo));

    // MCP: a read, a failed read and a policy evaluation, then the session
    // ends.
    let (server_io, client_io) = tokio::io::duplex(1 << 20);
    let mcp_server = tokio::spawn({
        let repo = Arc::clone(&repo);
        async move {
            let running = oxtel_rmcp::Instrumented::new(AgentStateGraphServer::new(repo))
                .with_transport("pipe")
                .serve(server_io)
                .await
                .expect("server starts");
            let _ = running.waiting().await;
        }
    });
    let mcp = ().serve(client_io).await.expect("client connects");
    let call = |tool: &'static str, arguments: serde_json::Value| {
        let serde_json::Value::Object(arguments) = arguments else {
            unreachable!()
        };
        CallToolRequestParams::new(tool).with_arguments(arguments)
    };
    mcp.call_tool(call(
        "agentstategraph_get",
        json!({ "path": "/catalogue/k1" }),
    ))
    .await
    .unwrap();
    mcp.call_tool(call("agentstategraph_get", json!({ "path": "/nowhere" })))
        .await
        .unwrap();
    mcp.call_tool(call(
        "agentstategraph_policy_evaluate",
        json!({ "situation": {}, "action": "deploy", "agent_id": "agent-1" }),
    ))
    .await
    .unwrap();
    mcp.cancel().await.unwrap();
    mcp_server.await.unwrap();

    let catalogue = catalogue();
    let mut problems = Vec::new();
    let mut recorded = Vec::new();
    for family in meters.collect() {
        let name = family.name.clone();
        recorded.push(name.clone());
        let Some(entry) = catalogue.get(&name) else {
            problems.push(format!("{name} is recorded but isn't in the catalogue"));
            continue;
        };
        if !instrument(family.kind).contains(&entry.instrument.as_str()) {
            problems.push(format!(
                "{name} is a {:?}, but the catalogue says {}",
                family.kind, entry.instrument
            ));
        }
        let unit = family.unit.clone().unwrap_or_default();
        if unit != entry.unit {
            problems.push(format!(
                "{name}'s unit is {unit:?}, but the catalogue says {:?}",
                entry.unit
            ));
        }
    }
    for name in catalogue.keys() {
        let excused = NOT_PRODUCED.iter().any(|(n, _)| n == name);
        let produced = recorded.contains(name);
        if !produced && !excused {
            problems.push(format!(
                "{name} is in the catalogue but wasn't recorded; record it, or list it in NOT_PRODUCED"
            ));
        }
        if produced && excused {
            problems.push(format!("{name} was recorded; take it out of NOT_PRODUCED"));
        }
    }
    for (name, _) in NOT_PRODUCED {
        assert!(
            catalogue.contains_key(*name),
            "{name} isn't in the catalogue"
        );
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));

    drop(guard);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}
