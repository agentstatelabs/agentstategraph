//! The ASD proxy (`--asd-url`, `providers` feature) against a stand-in ASD
//! (DESIGN.md Plan E t-003): it needs a key when auth is on, never forwards
//! the key, and can't be steered outside ASD's `/api/v1`.
//!
//! Run with: cargo test -p agentstategraph-mcp --features providers --test asd_proxy

#![cfg(feature = "providers")]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use agentstategraph::Repository;
use agentstategraph_mcp::auth::{ApiKey, TenantManager};
use agentstategraph_mcp::http;
use agentstategraph_storage::SqliteStorage;
use axum::{Router, extract::Request, routing::any};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// What the stand-in ASD received: path and query, and whether it saw a
/// credential.
type Seen = Arc<Mutex<Vec<(String, bool)>>>;

async fn serve(app: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    addr
}

async fn fake_asd(seen: Seen) -> SocketAddr {
    serve(Router::new().fallback(any(move |req: Request| {
        let seen = Arc::clone(&seen);
        async move {
            let credential = req.headers().contains_key("authorization")
                || req.headers().contains_key("x-api-key");
            let path = req.uri().path_and_query().unwrap().to_string();
            seen.lock().unwrap().push((path, credential));
            r#"{"from": "asd"}"#
        }
    })))
    .await
}

const KEY: &str = "asg_asd_proxy_test_key_000000000000";

async fn asg_with_proxy(asd: SocketAddr) -> SocketAddr {
    let repo = Arc::new(Repository::new(Box::new(
        SqliteStorage::in_memory().expect("in-memory sqlite"),
    )));
    repo.init().unwrap();
    let mgr = TenantManager::multi_tenant(repo.clone(), None).unwrap();
    mgr.register_key(ApiKey {
        id: String::new(),
        key: KEY.into(),
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
    })
    .unwrap();
    serve(http::build_router_with(
        mgr,
        0,
        Some(http::asd_proxy_router(format!("http://{asd}"))),
    ))
    .await
}

/// Send `path` exactly as written (an HTTP client would normalize `..`) and
/// return the status code.
async fn raw_get(addr: SocketAddr, path: &str) -> u16 {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(
            format!(
                "GET {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {KEY}\r\n\
                 Connection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    response
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_proxy_needs_a_key_and_never_forwards_it() {
    let seen: Seen = Arc::default();
    let asd = fake_asd(Arc::clone(&seen)).await;
    let asg = asg_with_proxy(asd).await;
    let client = reqwest::Client::new();

    let anonymous = client
        .get(format!("http://{asg}/api/code/symbols?q=1"))
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status().as_u16(), 401);
    assert!(
        seen.lock().unwrap().is_empty(),
        "ASD was reached without a key"
    );

    let proxied = client
        .get(format!("http://{asg}/api/code/symbols?q=1"))
        .bearer_auth(KEY)
        .header("x-api-key", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(proxied.status().as_u16(), 200);
    assert_eq!(proxied.text().await.unwrap(), r#"{"from": "asd"}"#);
    assert_eq!(
        *seen.lock().unwrap(),
        vec![("/api/v1/symbols?q=1".to_string(), false)],
        "ASD got the request without the key"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_proxy_stays_under_asd_api_v1() {
    let seen: Seen = Arc::default();
    let asd = fake_asd(Arc::clone(&seen)).await;
    let asg = asg_with_proxy(asd).await;

    for path in [
        "/api/code/../../admin",
        "/api/code/%2e%2e/%2e%2e/admin",
        "/api/code/..\\..\\admin",
    ] {
        assert_eq!(raw_get(asg, path).await, 400, "{path}");
    }
    assert!(
        seen.lock().unwrap().is_empty(),
        "{:?}",
        seen.lock().unwrap()
    );
    assert_eq!(raw_get(asg, "/api/code/symbols").await, 200);
}
