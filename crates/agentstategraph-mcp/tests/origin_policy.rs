//! Which web pages may call the HTTP API, and which `Host` names it answers
//! to (DESIGN.md Plan E t-001). Runs the real router behind the policy on a
//! loopback port and sends requests with the `Origin` and `Host` a browser
//! would.

use std::net::SocketAddr;
use std::sync::Arc;

use agentstategraph::Repository;
use agentstategraph_mcp::http::{self, OriginPolicy};
use agentstategraph_storage::SqliteStorage;
use reqwest::header::{ACCESS_CONTROL_ALLOW_ORIGIN, HOST, ORIGIN};

/// Serve the API under `policy` on an ephemeral loopback port.
async fn boot(policy: OriginPolicy) -> SocketAddr {
    let repo = Arc::new(Repository::new(Box::new(
        SqliteStorage::in_memory().expect("in-memory sqlite"),
    )));
    repo.init().expect("init repo");
    let app = http::with_origin_policy(http::router(repo), &policy);
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

fn policy(bind: &str, origins: &[&str], hosts: &[&str]) -> OriginPolicy {
    let owned = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    OriginPolicy::for_bind(bind, &owned(origins), &owned(hosts))
}

/// The `Access-Control-Allow-Origin` a page at `origin` gets back.
async fn allowed_origin(addr: SocketAddr, origin: &str) -> Option<String> {
    reqwest::Client::new()
        .get(format!("http://{addr}/api/health"))
        .header(ORIGIN, origin)
        .send()
        .await
        .unwrap()
        .headers()
        .get(ACCESS_CONTROL_ALLOW_ORIGIN)
        .map(|v| v.to_str().unwrap().to_string())
}

/// The status of a request to `path` addressed to `host`.
async fn status_for_host(addr: SocketAddr, host: &str) -> u16 {
    reqwest::Client::new()
        .get(format!("http://{addr}/api/health"))
        .header(HOST, host)
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

#[tokio::test]
async fn loopback_pages_may_call_the_api_and_others_may_not() {
    let addr = boot(policy("127.0.0.1", &[], &[])).await;
    for origin in [
        "http://localhost:5173",
        "http://127.0.0.1:8080",
        "https://app.localhost",
        "http://[::1]:3000",
    ] {
        assert_eq!(
            allowed_origin(addr, origin).await.as_deref(),
            Some(origin),
            "{origin}"
        );
    }
    for origin in [
        "https://evil.example",
        "http://localhost.evil.example",
        "http://127.0.0.1.evil.example",
        "null",
    ] {
        assert_eq!(allowed_origin(addr, origin).await, None, "{origin}");
    }

    // A preflight for a write gets no permission either.
    let preflight = |origin: &'static str| async move {
        reqwest::Client::new()
            .request(
                reqwest::Method::OPTIONS,
                format!("http://{addr}/api/state/main/set"),
            )
            .header(ORIGIN, origin)
            .header("access-control-request-method", "POST")
            .header("access-control-request-headers", "content-type")
            .send()
            .await
            .unwrap()
            .headers()
            .contains_key(ACCESS_CONTROL_ALLOW_ORIGIN)
    };
    assert!(preflight("http://localhost:5173").await);
    assert!(!preflight("https://evil.example").await);
}

#[tokio::test]
async fn listed_origins_are_allowed_and_star_allows_any() {
    let addr = boot(policy("127.0.0.1", &["https://App.Example.com/"], &[])).await;
    assert_eq!(
        allowed_origin(addr, "https://app.example.com")
            .await
            .as_deref(),
        Some("https://app.example.com")
    );
    assert_eq!(allowed_origin(addr, "https://other.example").await, None);

    let any = boot(policy("127.0.0.1", &["*"], &[])).await;
    assert_eq!(
        allowed_origin(any, "https://other.example")
            .await
            .as_deref(),
        Some("*")
    );
}

/// DNS rebinding: a page points a domain it controls at 127.0.0.1, and its
/// requests reach a loopback-bound server with that domain as `Host`.
#[tokio::test]
async fn a_loopback_bind_answers_only_to_loopback_names() {
    let addr = boot(policy("127.0.0.1", &[], &[])).await;
    let port = addr.port();
    for host in [
        format!("localhost:{port}"),
        format!("127.0.0.1:{port}"),
        format!("[::1]:{port}"),
        "LOCALHOST".to_string(),
    ] {
        assert_eq!(status_for_host(addr, &host).await, 200, "{host}");
    }
    assert_eq!(status_for_host(addr, "evil.example").await, 403);
    assert_eq!(
        status_for_host(addr, &format!("evil.example:{port}")).await,
        403
    );

    // A refused write changes nothing; the same write addressed to a
    // loopback name goes through.
    let client = reqwest::Client::new();
    let write = |host: Option<&'static str>| {
        let mut req = client
            .post(format!("http://{addr}/api/state/main/set"))
            .json(&serde_json::json!({
                "path": "/written",
                "value": "x",
                "intent_category": "Refine",
                "intent_description": "rebinding",
            }));
        if let Some(host) = host {
            req = req.header(HOST, host);
        }
        req.send()
    };
    let read = || {
        client
            .get(format!("http://{addr}/api/state/main?path=/written"))
            .send()
    };
    let refused = write(Some("evil.example")).await.unwrap();
    assert_eq!(refused.status().as_u16(), 403);
    assert!(
        refused
            .text()
            .await
            .unwrap()
            .contains("--allowed-host evil.example"),
        "the refusal says how to allow the name"
    );
    assert!(
        !read().await.unwrap().status().is_success(),
        "the refused write landed"
    );
    assert!(write(None).await.unwrap().status().is_success());
    assert!(read().await.unwrap().status().is_success());
}

#[tokio::test]
async fn listed_hosts_are_served() {
    let addr = boot(policy("127.0.0.1", &[], &["asg.internal:8443"])).await;
    assert_eq!(status_for_host(addr, "asg.internal").await, 200);
    assert_eq!(status_for_host(addr, "asg.internal:443").await, 200);
    assert_eq!(status_for_host(addr, "evil.example").await, 403);

    let off = boot(policy("127.0.0.1", &[], &["*"])).await;
    assert_eq!(status_for_host(off, "evil.example").await, 200);
}

#[tokio::test]
async fn other_binds_answer_any_host_unless_hosts_are_listed() {
    let open = boot(policy("0.0.0.0", &[], &[])).await;
    assert_eq!(status_for_host(open, "asg.example.com").await, 200);

    let listed = boot(policy("0.0.0.0", &[], &["asg.example.com"])).await;
    assert_eq!(status_for_host(listed, "asg.example.com").await, 200);
    assert_eq!(
        status_for_host(listed, "localhost").await,
        200,
        "a health check on the machine itself still works"
    );
    assert_eq!(status_for_host(listed, "evil.example").await, 403);
}

#[test]
fn loopback_binds() {
    for bind in ["127.0.0.1", "127.0.1.1", "::1", "[::1]", "localhost"] {
        assert!(http::is_loopback_bind(bind), "{bind}");
    }
    for bind in ["0.0.0.0", "::", "10.0.0.5", "192.168.1.2"] {
        assert!(!http::is_loopback_bind(bind), "{bind}");
    }
}
