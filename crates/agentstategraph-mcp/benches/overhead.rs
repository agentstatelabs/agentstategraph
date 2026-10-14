//! What telemetry costs a request (DESIGN.md Plan H t-011).
//!
//! Starts the server binary in three modes and times HTTP reads, HTTP writes
//! and MCP tool calls (a read and a write, over stdio) from a client:
//!
//! - **off**: `OTEL_SDK_DISABLED=true` and `ASG_LOG=off`, as an operator turns
//!   telemetry off;
//! - **on**: the defaults, with nothing exported: spans, metrics, log lines
//!   (to a discarded stderr) and commit trace links;
//! - **exporting**: the defaults, sending everything over OTLP to a collector
//!   stub in this process, which accepts and discards it.
//!
//! Each round runs every mode once, in an order that rotates, so background
//! load on the machine spreads across the modes. Each mode starts on a new
//! database. The report gives p50 and p99 per mode, pooled over the rounds,
//! and what `on` and `exporting` add to `off`.
//!
//! ```text
//! cargo bench -p agentstategraph-mcp --features otlp --bench overhead
//! ```
//!
//! Without `--features otlp` the server can't export, and the report says so.
//! `OVERHEAD_ROUNDS` (default 5) and `OVERHEAD_REQUESTS` (default 1000, per
//! workload per round) size the run. `OVERHEAD_ENV`, such as
//! `ASG_TRACE_LINKS=off;ASG_LOG=off`, sets more variables for `on` and
//! `exporting`, to find what costs what.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const SERVER: &str = env!("CARGO_BIN_EXE_agentstategraph-mcp");
const KEYS: usize = 100;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Off,
    On,
    Exporting,
}

impl Mode {
    const ALL: [Mode; 3] = [Mode::Off, Mode::On, Mode::Exporting];

    fn name(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::On => "on",
            Mode::Exporting => "exporting",
        }
    }

    /// The server's command, its environment cleared of telemetry settings
    /// and set for this mode.
    fn command(self, collector: &str) -> Command {
        let mut command = Command::new(SERVER);
        for (key, _) in std::env::vars() {
            if key.starts_with("OTEL_") || key.starts_with("ASG_") || key == "RUST_LOG" {
                command.env_remove(key);
            }
        }
        match self {
            Mode::Off => {
                command
                    .env("OTEL_SDK_DISABLED", "true")
                    .env("ASG_LOG", "off");
            }
            Mode::On => {}
            Mode::Exporting => {
                command.env("OTEL_EXPORTER_OTLP_ENDPOINT", collector);
            }
        }
        if self != Mode::Off {
            let extra = std::env::var("OVERHEAD_ENV").unwrap_or_default();
            for setting in extra.split(';').filter(|s| !s.trim().is_empty()) {
                let (key, value) = setting
                    .split_once('=')
                    .expect("OVERHEAD_ENV is KEY=VALUE;...");
                command.env(key.trim(), value.trim());
            }
        }
        command.stdout(Stdio::null()).stderr(Stdio::null());
        command
    }
}

const WORKLOADS: [&str; 4] = ["HTTP read", "HTTP write", "MCP get", "MCP set"];

/// Latencies by mode and workload, pooled over the rounds.
struct Samples(Vec<Vec<Vec<Duration>>>);

impl Samples {
    fn new() -> Self {
        Samples(vec![vec![Vec::new(); WORKLOADS.len()]; Mode::ALL.len()])
    }

    fn add(&mut self, mode: Mode, workload: usize, mut latencies: Vec<Duration>) {
        let index = Mode::ALL.iter().position(|m| *m == mode).unwrap();
        self.0[index][workload].append(&mut latencies);
    }

    fn percentile(&self, mode: usize, workload: usize, p: f64) -> Duration {
        let mut sorted = self.0[mode][workload].clone();
        sorted.sort();
        let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
        sorted[rank.clamp(1, sorted.len()) - 1]
    }
}

fn scratch_db(mode: Mode, round: usize) -> PathBuf {
    std::env::temp_dir().join(format!(
        "asg-overhead-{}-{}-{round}.db",
        std::process::id(),
        mode.name()
    ))
}

fn remove_db(path: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A minimal HTTP/1.1 client on one kept-alive connection, so the client
/// adds as little and as steady a cost as it can.
struct Http {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    host: String,
}

impl Http {
    fn connect(port: u16) -> Option<Http> {
        let stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
        stream.set_nodelay(true).ok()?;
        Some(Http {
            reader: BufReader::new(stream.try_clone().ok()?),
            writer: stream,
            host: format!("127.0.0.1:{port}"),
        })
    }

    fn request(&mut self, method: &str, path: &str, body: Option<&str>) -> u16 {
        let body = body.unwrap_or("");
        write!(
            self.writer,
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            self.host,
            body.len()
        )
        .unwrap();
        let mut line = String::new();
        self.reader.read_line(&mut line).unwrap();
        let status: u16 = line.split(' ').nth(1).unwrap().parse().unwrap();
        let mut length = 0usize;
        loop {
            line.clear();
            self.reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                length = value.trim().parse().unwrap();
            }
        }
        let mut sink = vec![0; length];
        self.reader.read_exact(&mut sink).unwrap();
        status
    }
}

fn write_body(path: &str, value: usize) -> String {
    format!(
        r#"{{"path":"{path}","value":{value},"intent_category":"Refine","intent_description":"bench"}}"#
    )
}

/// Time `count` calls of `call`, each given its index.
fn timed(count: usize, mut call: impl FnMut(usize)) -> Vec<Duration> {
    (0..count)
        .map(|i| {
            let started = Instant::now();
            call(i);
            started.elapsed()
        })
        .collect()
}

fn http_round(mode: Mode, round: usize, requests: usize, collector: &str, samples: &mut Samples) {
    let db = scratch_db(mode, round);
    let port = free_port();
    let mut server = mode
        .command(collector)
        .args([
            "--http",
            "--port",
            &port.to_string(),
            "--rate-limit-rpm",
            "0",
        ])
        .arg("--path")
        .arg(&db)
        .spawn()
        .expect("start the server");
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut http = loop {
        if let Some(mut http) = Http::connect(port)
            && http.request("GET", "/api/health", None) == 200
        {
            break http;
        }
        assert!(Instant::now() < deadline, "the server didn't start");
        std::thread::sleep(Duration::from_millis(50));
    };
    // Seed the keys the reads use, which warms up the server too.
    for k in 0..KEYS {
        let status = http.request(
            "POST",
            "/api/state/main/set",
            Some(&write_body(&format!("/bench/k{k}"), k)),
        );
        assert_eq!(status, 200, "seeding");
    }
    for i in 0..KEYS {
        http.request("GET", &format!("/api/state/main?path=/bench/k{i}"), None);
    }
    let reads = timed(requests, |i| {
        let status = http.request(
            "GET",
            &format!("/api/state/main?path=/bench/k{}", i % KEYS),
            None,
        );
        assert_eq!(status, 200);
    });
    let writes = timed(requests, |i| {
        let status = http.request(
            "POST",
            "/api/state/main/set",
            Some(&write_body(&format!("/bench/w{}", i % KEYS), i)),
        );
        assert_eq!(status, 200);
    });
    samples.add(mode, 0, reads);
    samples.add(mode, 1, writes);
    stop(&mut server);
    remove_db(&db);
}

/// An MCP client over the server's stdin and stdout.
struct Mcp {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Mcp {
    fn call(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        self.next_id += 1;
        let id = self.next_id;
        let request =
            serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        writeln!(self.stdin, "{request}").unwrap();
        let mut line = String::new();
        loop {
            line.clear();
            assert!(
                self.stdout.read_line(&mut line).unwrap() > 0,
                "the server hung up"
            );
            let message: serde_json::Value = serde_json::from_str(&line).unwrap();
            if message["id"] == id {
                return message;
            }
        }
    }

    fn tool(&mut self, name: &str, arguments: serde_json::Value) {
        let response = self.call(
            "tools/call",
            serde_json::json!({ "name": name, "arguments": arguments }),
        );
        assert!(
            response["result"]["isError"] != true,
            "{name} failed: {response}"
        );
    }
}

fn mcp_round(mode: Mode, round: usize, requests: usize, collector: &str, samples: &mut Samples) {
    let db = scratch_db(mode, round);
    let mut server = mode
        .command(collector)
        .arg("--path")
        .arg(&db)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("start the server");
    let mut mcp = Mcp {
        stdin: server.stdin.take().unwrap(),
        stdout: BufReader::new(server.stdout.take().unwrap()),
        next_id: 0,
    };
    mcp.call(
        "initialize",
        serde_json::json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "overhead", "version": "0" },
        }),
    );
    writeln!(
        mcp.stdin,
        r#"{{"jsonrpc":"2.0","method":"notifications/initialized"}}"#
    )
    .unwrap();
    let set = |mcp: &mut Mcp, path: String, value: usize| {
        mcp.tool(
            "agentstategraph_set",
            serde_json::json!({
                "path": path,
                "value": value,
                "intent_category": "Refine",
                "intent_description": "bench",
            }),
        )
    };
    for k in 0..KEYS {
        set(&mut mcp, format!("/bench/k{k}"), k);
    }
    let gets = timed(requests, |i| {
        mcp.tool(
            "agentstategraph_get",
            serde_json::json!({ "path": format!("/bench/k{}", i % KEYS) }),
        )
    });
    let sets = timed(requests, |i| {
        set(&mut mcp, format!("/bench/w{}", i % KEYS), i)
    });
    samples.add(mode, 2, gets);
    samples.add(mode, 3, sets);
    drop(mcp);
    stop(&mut server);
    remove_db(&db);
}

fn stop(server: &mut Child) {
    let _ = server.kill();
    let _ = server.wait();
}

/// An OTLP endpoint that accepts everything and keeps nothing but a count of
/// the bytes it was sent.
fn collector(received: Arc<AtomicU64>) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let app = axum::Router::new().fallback(move |body: axum::body::Bytes| {
                received.fetch_add(body.len() as u64, Ordering::Relaxed);
                async { axum::http::StatusCode::OK }
            });
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            axum::serve(listener, app).await.unwrap();
        });
    });
    address
}

fn env_number(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

fn micros(d: Duration) -> String {
    format!("{:.0} µs", d.as_secs_f64() * 1e6)
}

fn added(value: Duration, base: Duration) -> String {
    format!(
        "{:+.1}%",
        (value.as_secs_f64() / base.as_secs_f64() - 1.0) * 100.0
    )
}

fn main() {
    let rounds = env_number("OVERHEAD_ROUNDS", 5);
    let requests = env_number("OVERHEAD_REQUESTS", 1000);
    let received = Arc::new(AtomicU64::new(0));
    let collector = collector(Arc::clone(&received));
    let mut samples = Samples::new();
    let started = Instant::now();
    for round in 0..rounds {
        for step in 0..Mode::ALL.len() {
            let mode = Mode::ALL[(round + step) % Mode::ALL.len()];
            http_round(mode, round, requests, &collector, &mut samples);
            mcp_round(mode, round, requests, &collector, &mut samples);
        }
        eprintln!("round {} of {rounds} done", round + 1);
    }

    println!(
        "{rounds} rounds of {requests} requests per workload per mode, in {:.0} s; the collector received {} bytes",
        started.elapsed().as_secs_f64(),
        received.load(Ordering::Relaxed)
    );
    if received.load(Ordering::Relaxed) == 0 {
        println!("Nothing was exported: build with --features otlp to measure exporting.");
    }
    println!();
    println!("| Workload | Mode | p50 | p99 | p50 added | p99 added |");
    println!("|---|---|---|---|---|---|");
    for (w, workload) in WORKLOADS.iter().enumerate() {
        let (base50, base99) = (
            samples.percentile(0, w, 50.0),
            samples.percentile(0, w, 99.0),
        );
        for (m, mode) in Mode::ALL.iter().enumerate() {
            let (p50, p99) = (
                samples.percentile(m, w, 50.0),
                samples.percentile(m, w, 99.0),
            );
            let (more50, more99) = if *mode == Mode::Off {
                ("".to_string(), "".to_string())
            } else {
                (added(p50, base50), added(p99, base99))
            };
            println!(
                "| {workload} | {} | {} | {} | {more50} | {more99} |",
                mode.name(),
                micros(p50),
                micros(p99)
            );
        }
    }
}
