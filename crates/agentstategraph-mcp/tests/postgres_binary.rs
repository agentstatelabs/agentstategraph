//! The shipped binary against Postgres, in both transports (DESIGN.md Plan D
//! t-002).
//!
//! The binary used to connect on a throwaway runtime and drop it, then run
//! `init()` with no runtime at all, and MCP stdio mode ran on a
//! single-threaded runtime where the storage's `block_in_place` panics. This
//! starts the real binary each way and does a write and a read through it.
//!
//! Skipped unless `TEST_DATABASE_URL` is set; `ASG_REQUIRE_POSTGRES` turns the
//! skip into a failure.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

fn url() -> Option<String> {
    match std::env::var("TEST_DATABASE_URL") {
        Ok(u) if !u.is_empty() => Some(u),
        _ => {
            let required =
                std::env::var("ASG_REQUIRE_POSTGRES").is_ok_and(|v| !v.is_empty() && v != "0");
            assert!(
                !required,
                "ASG_REQUIRE_POSTGRES is set, but TEST_DATABASE_URL isn't"
            );
            eprintln!("skip: TEST_DATABASE_URL not set");
            None
        }
    }
}

fn tenant(tag: &str) -> String {
    format!("binary-{tag}-{}", uuid::Uuid::new_v4().simple())
}

/// Kills the server when the test ends, pass or fail.
struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Server {
    fn start(url: &str, tenant: &str, extra: &[&str]) -> Server {
        let child = Command::new(env!("CARGO_BIN_EXE_agentstategraph-mcp"))
            .args([
                "--storage",
                "postgres",
                "--database-url",
                url,
                "--tenant",
                tenant,
            ])
            .args(extra)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start agentstategraph-mcp");
        Server(child)
    }

    /// The server's stderr, for a failure message once it has exited.
    fn stderr(&mut self) -> String {
        let _ = self.0.kill();
        let mut out = String::new();
        if let Some(mut e) = self.0.stderr.take() {
            let _ = e.read_to_string(&mut out);
        }
        out
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[tokio::test(flavor = "multi_thread")]
async fn http_mode_reads_and_writes_postgres() {
    let Some(url) = url() else { return };
    let port = free_port();
    let mut server = Server::start(
        &url,
        &tenant("http"),
        &["--http", "--bind", "127.0.0.1", "--port", &port.to_string()],
    );
    let base = format!("http://127.0.0.1:{port}/api");
    let client = reqwest::Client::new();

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(Some(status)) = server.0.try_wait() {
            panic!(
                "server exited ({status}) before serving:\n{}",
                server.stderr()
            );
        }
        if client.get(format!("{base}/health")).send().await.is_ok() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "server never answered:\n{}",
            server.stderr()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let set = client
        .post(format!("{base}/state/main/set"))
        .json(&json!({
            "path": "/smoke",
            "value": { "ok": true },
            "intent_category": "Checkpoint",
            "intent_description": "postgres smoke",
        }))
        .send()
        .await
        .unwrap();
    assert!(
        set.status().is_success(),
        "set: {}",
        set.text().await.unwrap()
    );

    let got: Value = client
        .get(format!("{base}/state/main?path=/smoke"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(got, json!({ "ok": true }));

    let log = client
        .get(format!("{base}/log/main"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(log.contains("postgres smoke"), "log: {log}");
}

#[test]
fn mcp_stdio_mode_reads_and_writes_postgres() {
    let Some(url) = url() else { return };
    let mut server = Server::start(&url, &tenant("mcp"), &[]);
    let mut stdin = server.0.stdin.take().unwrap();
    let stdout = server.0.stdout.take().unwrap();

    // Read responses on a thread so a hung server fails the test, not the run.
    let (tx, rx) = mpsc::channel::<Value>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(msg) = serde_json::from_str::<Value>(&line) {
                let _ = tx.send(msg);
            }
        }
    });
    let mut send = |msg: Value| {
        writeln!(stdin, "{msg}").unwrap();
        stdin.flush().unwrap();
    };
    let mut response = |id: u64| -> Value {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(msg) if msg["id"] == json!(id) => return msg,
                Ok(_) => continue,
                Err(_) => panic!("no response to request {id}:\n{}", server.stderr()),
            }
        }
    };

    send(json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "postgres-smoke", "version": "0" },
        },
    }));
    assert!(response(1).get("result").is_some(), "initialize failed");
    send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));

    send(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {
            "name": "agentstategraph_set",
            "arguments": {
                "path": "/smoke",
                "value": { "ok": true },
                "intent_category": "Checkpoint",
                "intent_description": "postgres smoke",
            },
        },
    }));
    let set = response(2);
    assert_eq!(set["result"]["isError"], json!(false), "set: {set}");
    let text = set["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(text.starts_with("Committed"), "set: {set}");

    send(json!({
        "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": { "name": "agentstategraph_get", "arguments": { "path": "/smoke" } },
    }));
    let got = response(3);
    let text = got["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    let value: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    assert_eq!(value, json!({ "ok": true }), "get: {got}");
}
