//! Health, readiness and metrics (DESIGN.md Plan E t-007). `/api/health` is
//! liveness and touches nothing; `/api/ready` asks the store; `--metrics`
//! serves Prometheus text, admin-only when auth is on.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

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
    let base = serve(http::with_observability(
        http::build_router_with(Arc::clone(&mgr), 0, None),
        mgr,
        false,
    ))
    .await;

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
    let base = serve(http::with_observability(
        http::build_router_with(Arc::clone(&mgr), 0, None),
        mgr,
        true,
    ))
    .await;
    get(&base, "/api/state/main?path=/", None).await;
    get(&base, "/api/state/main?path=/", None).await;
    get(&base, "/api/state/no-such-branch?path=/", None).await;
    assert_eq!(get(&base, "/no/such/route", None).await.0, 404);

    let (status, text) = get(&base, "/metrics", None).await;
    assert_eq!(status, 200);
    for line in [
        r#"asg_build_info{version="#,
        r#"asg_http_requests_total{method="GET",route="/api/state/{ref_name}",status="200"} 2"#,
        r#"asg_http_requests_total{method="GET",route="unmatched",status="404"} 1"#,
        r#"asg_http_request_duration_seconds_bucket{method="GET",route="/api/state/{ref_name}",le="+Inf"} 3"#,
        r#"asg_http_request_duration_seconds_count{method="GET",route="/api/state/{ref_name}"} 3"#,
        "# TYPE asg_http_request_duration_seconds histogram",
    ] {
        assert!(text.contains(line), "missing {line}:\n{text}");
    }
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
    let base = serve(http::with_observability(
        http::build_router_with(Arc::clone(&mgr), 0, None),
        Arc::clone(&mgr),
        true,
    ))
    .await;
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

    let off = serve(http::with_observability(
        http::build_router_with(Arc::clone(&mgr), 0, None),
        mgr,
        false,
    ))
    .await;
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
