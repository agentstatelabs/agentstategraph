//! Error responses carry a stable code beside the message, and the status
//! follows from the code (DESIGN.md Plan H t-004). Before, everything but
//! two 403s was a 500.

use std::net::SocketAddr;
use std::sync::Arc;

use agentstategraph::Repository;
use agentstategraph_mcp::http;
use agentstategraph_storage::SqliteStorage;
use axum::Router;
use serde_json::{Value, json};

fn repo() -> Arc<Repository> {
    let repo = Arc::new(Repository::new(Box::new(
        SqliteStorage::in_memory().expect("in-memory sqlite"),
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

async fn expect(response: reqwest::Response, status: u16, code: &str) {
    assert_eq!(response.status().as_u16(), status, "for {code}");
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["code"], code, "{body}");
    assert!(
        body["error"].as_str().is_some_and(|m| !m.is_empty()),
        "{body}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn errors_carry_a_code_and_a_status_from_it() {
    let base = serve(http::router(repo())).await;
    let client = reqwest::Client::new();

    let missing_path = client
        .get(format!("{base}/api/state/main?path=/nothing/here"))
        .send()
        .await
        .unwrap();
    expect(missing_path, 404, "path_not_found").await;

    let missing_branch = client
        .get(format!("{base}/api/state/nope?path=/"))
        .send()
        .await
        .unwrap();
    assert_eq!(missing_branch.status().as_u16(), 404);

    let again = client
        .post(format!("{base}/api/branches"))
        .json(&json!({"name": "main"}))
        .send()
        .await
        .unwrap();
    expect(again, 409, "branch_already_exists").await;

    let reserved = client
        .post(format!("{base}/api/state/main/set"))
        .json(&json!({
            "path": "/_meta/schema",
            "value": 1,
            "intent_category": "Refine",
            "intent_description": "touch a reserved path",
        }))
        .send()
        .await
        .unwrap();
    expect(reserved, 403, "reserved_path").await;
}

#[test]
fn codes_are_short_snake_case() {
    use agentstategraph::RepoError;
    use agentstategraph_storage::StorageError;
    let codes = [
        RepoError::WriteConflict.code(),
        RepoError::BranchNotFound("x".into()).code(),
        RepoError::Storage(StorageError::Backend("disk full".into())).code(),
        StorageError::CasConflict {
            name: "main".into(),
            expected: "a".into(),
            actual: "b".into(),
        }
        .code(),
    ];
    assert_eq!(
        codes,
        [
            "write_conflict",
            "branch_not_found",
            "storage_error",
            "write_conflict"
        ]
    );
    for code in codes {
        assert!(
            code.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
            "{code}"
        );
    }
}
