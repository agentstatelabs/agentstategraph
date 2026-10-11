//! Request bounds (DESIGN.md Plan E t-008): a body limit, a timeout, a cap
//! on requests in flight, and a rate limit per API key.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use agentstategraph::Repository;
use agentstategraph_mcp::auth::{ApiKey, TenantManager};
use agentstategraph_mcp::http::{self, RequestBounds};
use agentstategraph_storage::SqliteStorage;
use axum::{Router, routing::get};
use serde_json::json;

fn scratch_db() -> PathBuf {
    std::env::temp_dir().join(format!("asg-bounds-{}.db", uuid::Uuid::new_v4().simple()))
}

fn remove_db(path: &std::path::Path) {
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

fn single_tenant(path: &std::path::Path) -> Arc<TenantManager> {
    let repo = Arc::new(Repository::new(Box::new(
        SqliteStorage::open(path).expect("open sqlite"),
    )));
    repo.init().unwrap();
    TenantManager::single_tenant(repo)
}

/// A slow route beside the API: async sleep, or blocking work.
fn slow_routes() -> Router {
    Router::new()
        .route(
            "/api/code/sleep",
            get(|| async {
                tokio::time::sleep(Duration::from_millis(600)).await;
                "slept"
            }),
        )
        .route(
            "/api/code/block",
            get(|| async {
                tokio::task::spawn_blocking(|| std::thread::sleep(Duration::from_secs(2)))
                    .await
                    .unwrap();
                "blocked"
            }),
        )
}

async fn serve(mgr: Arc<TenantManager>, rpm: u32, bounds: &RequestBounds) -> String {
    let app = http::build_router_with(mgr, rpm, Some(slow_routes()));
    let app = http::with_request_bounds(app, bounds);
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

fn write_body(value: serde_json::Value) -> serde_json::Value {
    json!({
        "path": "/v",
        "value": value,
        "intent_category": "Refine",
        "intent_description": "bounded",
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_body_over_the_limit_gets_413() {
    let path = scratch_db();
    let bounds = RequestBounds {
        max_body_bytes: 1024,
        ..RequestBounds::default()
    };
    let base = serve(single_tenant(&path), 0, &bounds).await;
    let client = reqwest::Client::new();
    let post = |value: serde_json::Value| {
        client
            .post(format!("{base}/api/state/main/set"))
            .json(&write_body(value))
            .send()
    };
    assert_eq!(
        post(json!("x".repeat(2000)))
            .await
            .unwrap()
            .status()
            .as_u16(),
        413
    );
    assert_eq!(post(json!("small")).await.unwrap().status().as_u16(), 200);
    remove_db(&path);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn requests_past_the_cap_get_503_at_once_but_liveness_answers() {
    let path = scratch_db();
    let bounds = RequestBounds {
        max_concurrent: 1,
        ..RequestBounds::default()
    };
    let base = serve(single_tenant(&path), 0, &bounds).await;
    let client = reqwest::Client::new();
    let slow = tokio::spawn({
        let (client, base) = (client.clone(), base.clone());
        async move {
            client
                .get(format!("{base}/api/code/sleep"))
                .send()
                .await
                .unwrap()
                .status()
                .as_u16()
        }
    });
    tokio::time::sleep(Duration::from_millis(150)).await;

    let started = Instant::now();
    let busy = client
        .get(format!("{base}/api/state/main?path=/"))
        .send()
        .await
        .unwrap();
    assert_eq!(busy.status().as_u16(), 503);
    assert_eq!(busy.headers()["retry-after"], "1");
    assert!(started.elapsed() < Duration::from_millis(400), "it queued");
    let health = client
        .get(format!("{base}/api/health"))
        .send()
        .await
        .unwrap();
    assert_eq!(health.status().as_u16(), 200, "liveness is never shed");

    assert_eq!(slow.await.unwrap(), 200);
    let after = client
        .get(format!("{base}/api/state/main?path=/"))
        .send()
        .await
        .unwrap();
    assert_eq!(after.status().as_u16(), 200);
    remove_db(&path);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_past_the_timeout_gets_503() {
    let path = scratch_db();
    let bounds = RequestBounds {
        timeout: Some(Duration::from_millis(300)),
        ..RequestBounds::default()
    };
    let base = serve(single_tenant(&path), 0, &bounds).await;
    let started = Instant::now();
    let resp = reqwest::get(format!("{base}/api/code/block"))
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 503);
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(
        resp.text()
            .await
            .unwrap()
            .contains("may still have completed")
    );
    assert_eq!(
        reqwest::get(format!("{base}/api/state/main?path=/"))
            .await
            .unwrap()
            .status()
            .as_u16(),
        200
    );
    remove_db(&path);
}

/// Storage work runs off the async workers. With two workers, two writes
/// stuck on a locked SQLite file used to occupy both, and the whole server,
/// health checks included, stopped answering until they gave up. Now liveness
/// answers at once, and nothing waits past the timeout. (A read can still
/// wait on the stuck writes: the SQLite backend shares one connection.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn writes_stuck_in_storage_neither_stall_the_server_nor_outlast_the_timeout() {
    let path = scratch_db();
    let bounds = RequestBounds {
        timeout: Some(Duration::from_secs(1)),
        ..RequestBounds::default()
    };
    let base = serve(single_tenant(&path), 0, &bounds).await;

    // Another process holds the write lock.
    let locker = rusqlite::Connection::open(&path).unwrap();
    locker.execute_batch("BEGIN EXCLUSIVE;").unwrap();

    let client = reqwest::Client::new();
    let writes: Vec<_> = (0..3)
        .map(|i| {
            let (client, base) = (client.clone(), base.clone());
            tokio::spawn(async move {
                let started = Instant::now();
                let status = client
                    .post(format!("{base}/api/state/main/set"))
                    .json(&write_body(json!(i)))
                    .send()
                    .await
                    .unwrap()
                    .status()
                    .as_u16();
                (status, started.elapsed())
            })
        })
        .collect();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let started = Instant::now();
    let health = client
        .get(format!("{base}/api/health"))
        .send()
        .await
        .unwrap();
    assert_eq!(health.status().as_u16(), 200);
    assert!(
        started.elapsed() < Duration::from_millis(300),
        "liveness waited on storage"
    );
    let started = Instant::now();
    client
        .get(format!("{base}/api/state/main?path=/"))
        .send()
        .await
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "a read outlasted the timeout"
    );

    for write in writes {
        let (status, took) = write.await.unwrap();
        assert_eq!(status, 503, "a write stuck in storage wasn't cut off");
        assert!(took < Duration::from_secs(3), "{took:?}");
    }
    locker.execute_batch("ROLLBACK;").unwrap();
    remove_db(&path);
}

fn key(raw: &str) -> ApiKey {
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
        is_admin: false,
        expires_at: None,
        last_used_at: None,
    }
}

/// Every client of one machine (or behind one load balancer) shares an IP,
/// so the limit is per key: one busy key doesn't use up another's. A
/// request without a live key, made-up keys included, counts against its
/// IP.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_rate_limit_is_per_key_with_auth() {
    let path = scratch_db();
    let repo = Arc::new(Repository::new(Box::new(
        SqliteStorage::open(&path).expect("open sqlite"),
    )));
    repo.init().unwrap();
    let mgr = TenantManager::multi_tenant(repo, None).unwrap();
    mgr.register_key(key("asg_rate_key_a_000000000000000000000"))
        .unwrap();
    mgr.register_key(key("asg_rate_key_b_000000000000000000000"))
        .unwrap();
    // One a minute, with the floor burst of five.
    let base = serve(mgr, 1, &RequestBounds::default()).await;
    let client = reqwest::Client::new();
    let burst = |key: Option<&'static str>, n: usize| {
        let (client, base) = (client.clone(), base.clone());
        async move {
            let mut statuses = Vec::new();
            for _ in 0..n {
                let mut req = client.get(format!("{base}/api/state/main?path=/"));
                if let Some(key) = key {
                    req = req.bearer_auth(key);
                }
                statuses.push(req.send().await.unwrap().status().as_u16());
            }
            statuses
        }
    };

    assert_eq!(
        burst(Some("asg_rate_key_a_000000000000000000000"), 6).await,
        vec![200, 200, 200, 200, 200, 429]
    );
    assert_eq!(
        burst(Some("asg_rate_key_b_000000000000000000000"), 5).await,
        vec![200; 5],
        "key a's use spent key b's limit"
    );
    assert_eq!(burst(None, 6).await, vec![401, 401, 401, 401, 401, 429]);
    assert_eq!(
        burst(Some("asg_made_up_key_0000000000000000000"), 1).await,
        vec![429],
        "a made-up key got a bucket of its own"
    );
    remove_db(&path);
}
