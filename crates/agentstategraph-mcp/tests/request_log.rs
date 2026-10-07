//! Request logs name the API key by id, never by the key (DESIGN.md Plan E
//! t-007). Its own test binary, because it installs the process's global
//! log subscriber to capture what the server logs.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use agentstategraph::Repository;
use agentstategraph_mcp::auth::{ApiKey, KeyRef, TenantManager};
use agentstategraph_mcp::http;
use agentstategraph_storage::SqliteStorage;

/// A log sink the test can read back.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
    type Writer = Captured;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

const KEY: &str = "asg_request_log_secret_key_00000000000";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_logs_carry_the_key_id_never_the_key() {
    let captured = Captured::default();
    tracing_subscriber::fmt()
        .with_writer(captured.clone())
        .with_ansi(false)
        .init();

    let repo = Arc::new(Repository::new(Box::new(
        SqliteStorage::in_memory().expect("in-memory sqlite"),
    )));
    repo.init().unwrap();
    let mgr = TenantManager::multi_tenant(Arc::clone(&repo), None).unwrap();
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
    let id = mgr.key_id(KeyRef::Key(KEY)).unwrap();
    let app = http::with_observability(
        http::build_router_with(Arc::clone(&mgr), 0, None),
        mgr,
        false,
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

    let status = reqwest::Client::new()
        .get(format!("http://{addr}/api/state/main?path=/&secret=query"))
        .bearer_auth(KEY)
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status.as_u16(), 200);
    // The log line is written as the response finishes.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let log = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    let line = log
        .lines()
        .find(|l| l.contains("path=/api/state/main"))
        .unwrap_or_else(|| panic!("no request line in:\n{log}"));
    assert!(line.contains(&id), "{line}");
    assert!(line.contains("status=200"), "{line}");
    assert!(!log.contains(KEY), "the key reached the log:\n{log}");
    assert!(
        !log.contains("secret=query"),
        "the query reached the log:\n{log}"
    );
}
