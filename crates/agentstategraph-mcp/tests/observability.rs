//! Health, readiness and metrics (DESIGN.md Plan E t-007, names from Plan H
//! t-002). `/api/health` is
//! liveness and touches nothing; `/api/ready` asks the store; `--metrics`
//! serves Prometheus text, admin-only when auth is on.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use agentstategraph::Repository;
use agentstategraph_mcp::auth::{ApiKey, TenantManager};
use agentstategraph_mcp::http;
use agentstategraph_storage::SqliteStorage;
use axum::Router;

fn scratch_db() -> PathBuf {
    std::env::temp_dir().join(format!(
        "asg-observability-{}.db",
        uuid::Uuid::new_v4().simple()
    ))
}

fn repo_at(path: &PathBuf) -> Arc<Repository> {
    let repo = Arc::new(Repository::new(Box::new(
        SqliteStorage::open(path).expect("open sqlite"),
    )));
    repo.init().unwrap();
    repo
}

/// The process's telemetry, as `main` installs it with `--http`: metrics
/// kept in memory for `/metrics`, nothing exported and no log lines. Tests
/// share it, as requests share the `metrics` recorder, so each test counts
/// label sets of its own.
fn meters() -> oxtel::sdk::metrics::MeterProvider {
    static TELEMETRY: OnceLock<oxtel::Guard> = OnceLock::new();
    TELEMETRY
        .get_or_init(|| {
            oxtel::builder()
                .with_default_service_name("agentstategraph")
                .with_resource_attribute(oxtel::KeyValue::new(
                    "service.version",
                    env!("CARGO_PKG_VERSION"),
                ))
                .without_env_exporters()
                .without_local_logs()
                .with_pull_metrics()
                .install()
                .expect("install telemetry")
        })
        .meter_provider()
        .expect("a meter provider for /metrics")
        .clone()
}

/// The app as `main` assembles it.
fn observed(mgr: &Arc<TenantManager>, metrics: bool, policy: &http::OriginPolicy) -> Router {
    // Before the layers, which describe their metrics to the recorder.
    let meters = meters();
    http::with_server_layers(
        http::build_router_with(Arc::clone(mgr), 0, None),
        Arc::clone(mgr),
        &http::RequestBounds::default(),
        policy,
        metrics.then_some(meters),
    )
}

fn loopback() -> http::OriginPolicy {
    http::OriginPolicy::for_bind("127.0.0.1", &[], &[])
}

async fn serve(app: Router) -> String {
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

async fn get(base: &str, path: &str, key: Option<&str>) -> (u16, String) {
    let mut req = reqwest::Client::new().get(format!("{base}{path}"));
    if let Some(key) = key {
        req = req.bearer_auth(key);
    }
    let resp = req.send().await.unwrap();
    (resp.status().as_u16(), resp.text().await.unwrap())
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn liveness_stays_up_when_the_store_doesnt_answer() {
    let path = scratch_db();
    let mgr = TenantManager::single_tenant(repo_at(&path));
    let base = serve(observed(&mgr, false, &loopback())).await;

    let (status, body) = get(&base, "/api/health", None).await;
    let health: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(status, 200);
    assert_eq!(health["status"], "ok");
    assert!(health.get("branches").is_none(), "liveness read the store");
    assert_eq!(
        get(&base, "/api/ready", None).await,
        (200, r#"{"status":"ready"}"#.to_string())
    );

    // Break the store under the running server.
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch("DROP TABLE namespaces;")
        .unwrap();
    let (status, body) = get(&base, "/api/ready", None).await;
    assert_eq!(status, 503);
    assert_eq!(
        body, r#"{"status":"not ready"}"#,
        "the reason stays in the log"
    );
    assert_eq!(get(&base, "/api/health", None).await.0, 200);

    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn metrics_count_requests_by_route_template() {
    let path = scratch_db();
    let mgr = TenantManager::single_tenant(repo_at(&path));
    let base = serve(observed(&mgr, true, &loopback())).await;
    get(&base, "/api/state/main?path=/", None).await;
    get(&base, "/api/state/main?path=/", None).await;
    get(&base, "/api/state/no-such-branch?path=/", None).await;
    // A POST, so its label set is this test's alone: the tests share one
    // recorder, and another one GETs a missing route.
    let missing = reqwest::Client::new()
        .post(format!("{base}/no/such/route"))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status().as_u16(), 404);

    let (status, text) = get(&base, "/metrics", None).await;
    assert_eq!(status, 200);
    for line in [
        r#"http_server_request_duration_seconds_count{http_request_method="GET",http_response_status_code="200",http_route="/api/state/{ref_name}",network_protocol_version="1.1",url_scheme="http",otel_scope_name="oxtel_tower"} 2"#,
        r#"http_server_request_duration_seconds_bucket{http_request_method="GET",http_response_status_code="200",http_route="/api/state/{ref_name}",network_protocol_version="1.1",url_scheme="http",otel_scope_name="oxtel_tower",le="+Inf"} 2"#,
        // A request that matched no route has no route label.
        r#"http_server_request_duration_seconds_count{http_request_method="POST",http_response_status_code="404",network_protocol_version="1.1",url_scheme="http",otel_scope_name="oxtel_tower"} 1"#,
        "# TYPE http_server_request_duration_seconds histogram",
        // Other tests' requests may be in flight too.
        r#"http_server_active_requests{http_request_method="GET",url_scheme="http",otel_scope_name="oxtel_tower"} "#,
    ] {
        assert!(text.contains(line), "missing {line}:\n{text}");
    }
    let target = text
        .lines()
        .find(|l| l.starts_with("target_info{"))
        .unwrap_or_else(|| panic!("no target_info:\n{text}"));
    assert!(
        target.contains(r#"service_name="agentstategraph""#)
            && target.contains(&format!(
                r#"service_version="{}""#,
                env!("CARGO_PKG_VERSION")
            )),
        "{target}"
    );
    assert!(
        !text.contains("no-such-branch"),
        "a path became a label:\n{text}"
    );

    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn metrics_are_admin_only_with_auth_and_absent_without_the_flag() {
    let path = scratch_db();
    let repo = repo_at(&path);
    let mgr = TenantManager::multi_tenant(Arc::clone(&repo), None).unwrap();
    mgr.register_key(key("asg_observability_admin_000000000000", true))
        .unwrap();
    mgr.register_key(key("asg_observability_user_0000000000000", false))
        .unwrap();
    let base = serve(observed(&mgr, true, &loopback())).await;
    assert_eq!(get(&base, "/metrics", None).await.0, 401);
    assert_eq!(
        get(
            &base,
            "/metrics",
            Some("asg_observability_user_0000000000000")
        )
        .await
        .0,
        403
    );
    assert_eq!(
        get(
            &base,
            "/metrics",
            Some("asg_observability_admin_000000000000")
        )
        .await
        .0,
        200
    );
    // Health and readiness stay public.
    assert_eq!(get(&base, "/api/ready", None).await.0, 200);

    let off = serve(observed(&mgr, false, &loopback())).await;
    assert_eq!(
        get(
            &off,
            "/metrics",
            Some("asg_observability_admin_000000000000")
        )
        .await
        .0,
        404
    );

    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

/// Requests the Host check refuses are logged and counted, and the check
/// covers `/metrics` (DESIGN.md Plan H t-002). The check used to sit outside
/// the observability layer, so refused requests left no trace.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refused_hosts_are_counted_and_metrics_stay_behind_the_host_check() {
    let path = scratch_db();
    let mgr = TenantManager::single_tenant(repo_at(&path));
    let base = serve(observed(&mgr, true, &loopback())).await;
    let client = reqwest::Client::new();
    let as_host = |path: &str, host: &str| {
        client
            .get(format!("{base}{path}"))
            .header("host", host)
            .send()
    };

    let refused = as_host("/api/state/main?path=/", "rebound.example")
        .await
        .unwrap();
    assert_eq!(refused.status().as_u16(), 403);
    let metrics_elsewhere = as_host("/metrics", "rebound.example").await.unwrap();
    assert_eq!(
        metrics_elsewhere.status().as_u16(),
        403,
        "/metrics answered a Host the server doesn't serve"
    );

    let (status, text) = get(&base, "/metrics", None).await;
    assert_eq!(status, 200);
    let line = r#"http_server_request_duration_seconds_count{http_request_method="GET",http_response_status_code="403",http_route="/api/state/{ref_name}",network_protocol_version="1.1",url_scheme="http",otel_scope_name="oxtel_tower"} 1"#;
    assert!(text.contains(line), "missing {line}:\n{text}");

    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}
