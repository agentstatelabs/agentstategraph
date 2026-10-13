//! Request ids, trace ids in error bodies, and rejections counted by reason
//! (DESIGN.md Plan H t-005). One test, because it installs the process's
//! telemetry and counts exactly what it sent.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use agentstategraph::Repository;
use agentstategraph_mcp::auth::{ApiKey, TenantManager};
use agentstategraph_mcp::http::{self, OriginPolicy, RequestBounds};
use agentstategraph_storage::SqliteStorage;
use axum::{Router, routing::get};
use oxtel::sdk::metrics::{MeterProvider, PointValue};
use oxtel::sdk::trace::{InMemorySpanExporter, SpanData};
use oxtel::{Guard, Value};
use serde_json::json;
use tokio::sync::Notify;

fn scratch_db() -> PathBuf {
    std::env::temp_dir().join(format!(
        "asg-rejections-{}.db",
        uuid::Uuid::new_v4().simple()
    ))
}

fn repo_at(path: &PathBuf) -> Arc<Repository> {
    let repo = Arc::new(Repository::new(Box::new(
        SqliteStorage::open(path).unwrap(),
    )));
    repo.init().unwrap();
    repo
}

fn key(raw: &str, admin: bool) -> ApiKey {
    ApiKey {
        id: String::new(),
        key: raw.into(),
        tenant_id: "default".into(),
        name: "test".into(),
        plan: "free".into(),
        enabled: true,
        created_at: chrono::Utc::now().to_rfc3339(),
        namespace_id: None,
        commit_agent_id: None,
        can_migrate: false,
        is_admin: admin,
        expires_at: None,
        last_used_at: None,
    }
}

/// A route that says when it has started, then waits until the test
/// releases it.
fn held(started: Arc<Notify>, release: Arc<Notify>) -> Router {
    Router::new().route(
        "/api/code/hold",
        get(move || {
            let (started, release) = (Arc::clone(&started), Arc::clone(&release));
            async move {
                started.notify_one();
                release.notified().await;
                "released"
            }
        }),
    )
}

/// The app as `main` assembles it, served on a free port.
async fn serve(
    mgr: &Arc<TenantManager>,
    rpm: u32,
    extra: Option<Router>,
    bounds: &RequestBounds,
    meters: &MeterProvider,
) -> String {
    let app = http::with_server_layers(
        http::build_router_with(Arc::clone(mgr), rpm, extra),
        Arc::clone(mgr),
        bounds,
        &OriginPolicy::for_bind("127.0.0.1", &[], &[]),
        Some(meters.clone()),
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
    format!("http://{addr}")
}

fn attr(span: &SpanData, key: &str) -> Option<Value> {
    span.attributes
        .iter()
        .find(|kv| kv.key == key)
        .map(|kv| kv.value.clone())
}

fn rejections(meters: &MeterProvider, reason: &str) -> u64 {
    meters
        .collect()
        .into_iter()
        .filter(|f| f.name == "asg.http.rejections")
        .flat_map(|f| f.points)
        .filter(|p| {
            p.attributes
                .contains(&("asg.rejection.reason".into(), reason.into()))
        })
        .map(|p| match p.value {
            PointValue::Counter(n) => n,
            other => panic!("not a counter: {other:?}"),
        })
        .sum()
}

async fn body(resp: reqwest::Response) -> serde_json::Value {
    resp.json().await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn requests_carry_ids_and_rejections_are_counted_by_reason() {
    let spans = InMemorySpanExporter::default();
    let guard: Guard = oxtel::builder()
        .without_env_exporters()
        .without_local_logs()
        .with_pull_metrics()
        .with_traces({
            let spans = spans.clone();
            move |traces| traces.with_simple_exporter(spans)
        })
        .install()
        .expect("install telemetry");
    let meters = guard.meter_provider().unwrap().clone();
    let client = reqwest::Client::new();
    let path = scratch_db();
    let repo = repo_at(&path);
    let open = TenantManager::single_tenant(Arc::clone(&repo));
    let small = RequestBounds {
        max_body_bytes: 1024,
        ..RequestBounds::default()
    };
    let base = serve(&open, 0, None, &small, &meters).await;

    // An id for every response: the client's, or a new one.
    let missing = client
        .get(format!("{base}/api/state/no-such-branch?path=/"))
        .header("x-request-id", "client-req-42")
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status().as_u16(), 404);
    assert_eq!(missing.headers()["x-request-id"], "client-req-42");
    let error = body(missing).await;
    assert!(
        error["code"].as_str().unwrap().ends_with("not_found"),
        "{error}"
    );
    let trace_id = error["trace_id"].as_str().expect("a trace id").to_string();
    assert_eq!(trace_id.len(), 32);
    for sent in [None, Some("x".repeat(200)), Some("has space".to_string())] {
        let mut req = client.get(format!("{base}/api/health"));
        if let Some(id) = &sent {
            req = req.header("x-request-id", id.as_str());
        }
        let resp = req.send().await.unwrap();
        let id = resp.headers()["x-request-id"].to_str().unwrap();
        assert!(uuid::Uuid::parse_str(id).is_ok(), "{sent:?} got {id}");
    }

    // Each kind of rejection answers with its reason as the code.
    let refused = client
        .get(format!("{base}/api/state/main?path=/"))
        .header("host", "rebound.example")
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status().as_u16(), 403);
    assert!(refused.headers().contains_key("x-request-id"));
    let refusal = body(refused).await;
    assert_eq!(refusal["code"], "host_refused");
    assert!(
        refusal["error"]
            .as_str()
            .unwrap()
            .contains("--allowed-host")
    );

    let too_big = client
        .post(format!("{base}/api/state/main/set"))
        .json(&json!({
            "path": "/v",
            "value": "x".repeat(2000),
            "intent_category": "Refine",
            "intent_description": "too big",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(too_big.status().as_u16(), 413);
    assert_eq!(body(too_big).await["code"], "body_too_large");

    let guarded = TenantManager::multi_tenant(Arc::clone(&repo), None).unwrap();
    guarded
        .register_key(key("asg_rejections_user_00000000000000", false))
        .unwrap();
    let auth = serve(&guarded, 0, None, &RequestBounds::default(), &meters).await;
    let anonymous = client
        .get(format!("{auth}/api/state/main?path=/"))
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status().as_u16(), 401);
    assert_eq!(body(anonymous).await["code"], "unauthenticated");
    let not_admin = client
        .get(format!("{auth}/metrics"))
        .bearer_auth("asg_rejections_user_00000000000000")
        .send()
        .await
        .unwrap();
    assert_eq!(not_admin.status().as_u16(), 403);
    assert_eq!(body(not_admin).await["code"], "forbidden");

    // Over capacity: one request held, then the next turned away.
    let (started, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    let one = RequestBounds {
        max_concurrent: 1,
        timeout: None,
        ..RequestBounds::default()
    };
    let routes = held(Arc::clone(&started), Arc::clone(&release));
    let busy = serve(&open, 0, Some(routes), &one, &meters).await;
    let holding = tokio::spawn({
        let (client, busy) = (client.clone(), busy.clone());
        async move { client.get(format!("{busy}/api/code/hold")).send().await }
    });
    tokio::time::timeout(Duration::from_secs(10), started.notified())
        .await
        .expect("the held request started");
    let shed = client
        .get(format!("{busy}/api/state/main?path=/"))
        .send()
        .await
        .unwrap();
    assert_eq!(shed.status().as_u16(), 503);
    assert_eq!(shed.headers()["retry-after"], "1");
    assert_eq!(body(shed).await["code"], "over_capacity");
    release.notify_one();
    assert_eq!(holding.await.unwrap().unwrap().status().as_u16(), 200);

    // Timed out: a request held past the limit.
    let quick = RequestBounds {
        timeout: Some(Duration::from_millis(100)),
        ..RequestBounds::default()
    };
    let never = held(Arc::new(Notify::new()), Arc::new(Notify::new()));
    let slow = serve(&open, 0, Some(never), &quick, &meters).await;
    let late = client
        .get(format!("{slow}/api/code/hold"))
        .send()
        .await
        .unwrap();
    assert_eq!(late.status().as_u16(), 503);
    assert_eq!(body(late).await["code"], "timed_out");

    // Rate limited: one request a minute, with a burst of five.
    let limited = serve(&open, 1, None, &RequestBounds::default(), &meters).await;
    let mut limited_count = 0;
    for _ in 0..8 {
        let resp = client
            .get(format!("{limited}/api/state/main?path=/"))
            .send()
            .await
            .unwrap();
        if resp.status().as_u16() == 429 {
            assert!(resp.headers().contains_key("retry-after"));
            assert_eq!(body(resp).await["code"], "rate_limited");
            limited_count += 1;
        }
    }
    assert!(
        limited_count > 0,
        "eight requests never hit a limit of five"
    );

    for (reason, expected) in [
        ("host_refused", 1),
        ("body_too_large", 1),
        ("unauthenticated", 1),
        ("forbidden", 1),
        ("over_capacity", 1),
        ("timed_out", 1),
        ("rate_limited", limited_count),
    ] {
        assert_eq!(rejections(&meters, reason), expected, "{reason}");
    }

    // The spans: the request id, and the reason for a rejection.
    let recorded = spans.finished_spans();
    let tagged = recorded
        .iter()
        .find(|s| attr(s, "http.request.header.x-request-id") == Some(Value::from("client-req-42")))
        .expect("the request id on its span");
    assert_eq!(tagged.context.trace_id.to_string(), trace_id);
    assert!(attr(tagged, "asg.rejection.reason").is_none());
    assert!(
        recorded
            .iter()
            .any(|s| attr(s, "asg.rejection.reason") == Some(Value::from("host_refused")))
    );

    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}
