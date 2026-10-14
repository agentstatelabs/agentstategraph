//! The repository records its operations as the spans, events and metrics
//! docs/OBSERVABILITY.md lists. One test, because it installs a global
//! subscriber and recorder: the compare-and-swap check writes from several
//! threads. The trace layer records `info` and above, as the server's does
//! by default.

use std::sync::{Arc, Barrier};

use agentstategraph::{CommitOptions, CommitTrace, RepoError, Repository, RetentionPolicy};
use agentstategraph_core::IntentCategory;
use agentstategraph_storage::SqliteStorage;
use agentstategraph_taint::{TaintEffect, TaintMetadata, TaintParams, TaintSeverity};
use oxtel_sdk::logs::{InMemoryLogExporter, LogRecord, LoggerProvider};
use oxtel_sdk::metrics::{MeterProvider, PointValue};
use oxtel_sdk::trace::{InMemorySpanExporter, Sampler, SpanData, Status, TracerProvider};
use oxtel_sdk::{Resource, Value};
use serde_json::json;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::SubscriberExt;

/// A value and a path that must never reach telemetry.
const SECRET: &str = "secret-value-1234";
const PATH: &str = "/private/name";

fn attr(span: &SpanData, key: &str) -> Option<Value> {
    span.attributes
        .iter()
        .find(|kv| kv.key == key)
        .map(|kv| kv.value.clone())
}

fn text(value: &str) -> Option<Value> {
    Some(Value::from(value.to_string()))
}

fn named<'a>(spans: &'a [SpanData], name: &str) -> Vec<&'a SpanData> {
    spans.iter().filter(|s| s.name == name).collect()
}

fn point(meters: &MeterProvider, name: &str, labels: &[(&str, &str)]) -> Option<PointValue> {
    meters
        .collect()
        .into_iter()
        .find(|f| f.name == name)?
        .points
        .into_iter()
        .find(|p| {
            labels
                .iter()
                .all(|(k, v)| p.attributes.contains(&((*k).into(), (*v).into())))
        })
        .map(|p| p.value)
}

/// The `asg.commit` events logged so far.
fn commit_events(records: &InMemoryLogExporter) -> Vec<LogRecord> {
    records
        .records()
        .into_iter()
        .filter(|r| r.event_name.as_deref() == Some("asg.commit"))
        .collect()
}

fn options(agent: &str) -> CommitOptions {
    CommitOptions::new(agent, IntentCategory::Checkpoint, "record the name")
}

#[test]
fn the_repository_records_its_operations() {
    let spans = InMemorySpanExporter::default();
    let traces = TracerProvider::builder()
        .with_resource(Resource::builder().without_env().build())
        .with_sampler(Sampler::AlwaysOn)
        .with_span_events(true)
        .with_simple_exporter(spans.clone())
        .build();
    let meters = MeterProvider::builder().build();
    let records = InMemoryLogExporter::default();
    let logs = LoggerProvider::builder()
        .with_simple_exporter(records.clone())
        .build();
    tracing::subscriber::set_global_default(
        tracing_subscriber::registry()
            .with(traces.layer().with_filter(LevelFilter::INFO))
            .with(logs.layer().with_filter(LevelFilter::INFO)),
    )
    .unwrap();
    agentstategraph::emit_commit_events(true);
    metrics::set_global_recorder(meters.recorder()).unwrap();
    assert!(agentstategraph::link_commits_to_traces(|| {
        oxtel_sdk::trace::current_span_context()
            .filter(|context| context.is_valid())
            .map(|context| CommitTrace {
                trace_id: context.trace_id.to_string(),
                span_id: context.span_id.to_string(),
            })
    }));

    let repo = Arc::new(Repository::new(Box::new(
        SqliteStorage::in_memory().unwrap(),
    )));
    repo.init().unwrap();

    // A write: its span carries the commit it made.
    let id = repo
        .set_json("main", PATH, &json!(SECRET), options("agent-7"))
        .unwrap();
    let recorded = spans.finished_spans();
    let set = named(&recorded, "asg.repo.set")[0];
    assert_eq!(attr(set, "asg.commit.id"), text(&id.to_string()));
    assert_eq!(attr(set, "asg.ref.name"), text("main"));
    assert_eq!(attr(set, "gen_ai.agent.id"), text("agent-7"));
    assert_eq!(attr(set, "asg.intent.category"), text("Checkpoint"));
    assert_eq!(set.status, Status::Unset);
    // Its provenance event, in the write's trace (Plan H t-010).
    let landed = commit_events(&records);
    assert_eq!(landed.len(), 1);
    let event = &landed[0];
    let field = |key: &str| {
        event
            .attributes
            .iter()
            .find(|kv| kv.key == key)
            .map(|kv| kv.value.clone())
    };
    assert_eq!(field("asg.commit.id"), text(&id.to_string()));
    assert_eq!(field("asg.ref.name"), text("main"));
    assert_eq!(field("asg.namespace"), text("default"));
    assert_eq!(field("gen_ai.agent.id"), text("agent-7"));
    assert_eq!(field("asg.intent.category"), text("Checkpoint"));
    assert_eq!(event.trace_id, Some(set.context.trace_id));
    // The commit links back to the span that made it (Plan H t-009).
    assert_eq!(
        repo.commit_trace(&id).unwrap(),
        Some(CommitTrace {
            trace_id: set.context.trace_id.to_string(),
            span_id: set.context.span_id.to_string(),
        })
    );
    let check = named(&recorded, "asg.taint.check")[0];
    assert_eq!(check.parent_span_id, Some(set.context.span_id));
    let Some(PointValue::Histogram(durations)) = point(
        &meters,
        "asg.commit.duration",
        &[("asg.operation.name", "set")],
    ) else {
        panic!("no asg.commit.duration for set")
    };
    assert_eq!(durations.count(), 1);
    let unit = meters
        .collect()
        .into_iter()
        .find(|f| f.name == "asg.commit.duration")
        .and_then(|f| f.unit);
    assert_eq!(unit.as_deref(), Some("s"));

    // A write that fails records its error's code.
    let other = repo
        .set_json_cas("main", id, PATH, &json!("other"), options("agent-7"))
        .unwrap();
    let error = repo
        .set_json_cas("main", id, PATH, &json!("late"), options("agent-7"))
        .unwrap_err();
    assert!(matches!(error, RepoError::WriteConflict));
    assert_eq!(
        commit_events(&records).len(),
        2,
        "the conflict landed nothing"
    );
    let (blamed, trace) = repo.blame_with_trace("main", PATH).unwrap();
    assert_eq!(blamed.commit_id, other.short());
    assert!(trace.is_some());
    assert_eq!(trace, repo.commit_trace(&other).unwrap());
    let recorded = spans.finished_spans();
    let conflict = named(&recorded, "asg.repo.set")
        .into_iter()
        .find(|s| attr(s, "error.type").is_some())
        .expect("a failed set");
    assert_eq!(attr(conflict, "error.type"), text("write_conflict"));
    assert!(matches!(conflict.status, Status::Error { .. }));
    assert!(
        point(
            &meters,
            "asg.commit.duration",
            &[
                ("asg.operation.name", "set"),
                ("error.type", "write_conflict")
            ],
        )
        .is_some()
    );

    // A taint that refuses a write says so on the check's span.
    repo.taint(
        "main",
        "/private",
        TaintParams {
            name: "frozen".into(),
            effect: TaintEffect::Block,
            reason: "under review".into(),
            severity: TaintSeverity::Medium,
            expires_at: None,
            propagate: true,
            metadata: TaintMetadata::new(),
            agent_id: "ops".into(),
        },
    )
    .unwrap();
    let refused = repo.set_json("main", PATH, &json!(SECRET), options("agent-7"));
    assert!(matches!(refused, Err(RepoError::Taint { .. })));
    let recorded = spans.finished_spans();
    let event = named(&recorded, "asg.taint.check")
        .into_iter()
        .flat_map(|s| &s.events)
        .find(|e| e.name == "a taint refused the write")
        .expect("the refusal");
    let field = |key: &str| {
        event
            .attributes
            .iter()
            .find(|kv| kv.key == key)
            .map(|kv| kv.value.clone())
    };
    assert_eq!(field("asg.taint.effect"), text("block"));
    assert!(field("asg.taint.id").is_some());

    // A speculation's life, and a merge.
    let spec = repo.speculate("main", None).unwrap();
    repo.spec_set_json(spec, "/public/a", &json!(1)).unwrap();
    let committed = repo.commit_speculation(spec, options("agent-8")).unwrap();
    let discarded = repo.speculate("main", None).unwrap();
    repo.discard_speculation(discarded).unwrap();
    repo.branch("feature", "main").unwrap();
    repo.set_json("feature", "/public/b", &json!(2), options("agent-8"))
        .unwrap();
    let merged = repo.merge("feature", "main", options("agent-8")).unwrap();
    let recorded = spans.finished_spans();
    let create = named(&recorded, "asg.speculation.create")[0];
    assert_eq!(
        attr(create, "asg.speculation.id"),
        Some(Value::I64(spec.id() as i64))
    );
    let commit = named(&recorded, "asg.speculation.commit")[0];
    assert_eq!(attr(commit, "asg.ref.name"), text("main"));
    assert_eq!(attr(commit, "asg.commit.id"), text(&committed.to_string()));
    assert_eq!(named(&recorded, "asg.speculation.discard").len(), 1);
    let merge = named(&recorded, "asg.repo.merge")[0];
    assert_eq!(attr(merge, "asg.ref.name"), text("main"));
    assert_eq!(attr(merge, "asg.commit.id"), text(&merged.to_string()));

    // History extraction and garbage collection.
    let extracted = repo.extract_history(100).unwrap();
    repo.gc_dry_run().unwrap();
    repo.gc_sweep(RetentionPolicy::default(), false, false)
        .unwrap();
    repo.gc_vacuum().unwrap();
    let recorded = spans.finished_spans();
    let extract = named(&recorded, "asg.history.extract")[0];
    assert_eq!(
        attr(extract, "asg.history.commits_processed"),
        Some(Value::I64(extracted.commits_processed as i64))
    );
    assert_eq!(named(&recorded, "asg.gc.dry_run").len(), 1);
    let sweep = named(&recorded, "asg.gc.sweep")[0];
    assert_eq!(attr(sweep, "asg.gc.mutate"), Some(Value::Bool(false)));
    assert!(
        attr(
            named(&recorded, "asg.gc.vacuum")[0],
            "asg.gc.bytes_reclaimed"
        )
        .is_some()
    );

    // Reads are at `debug`, below this layer's filter.
    repo.get_json("main", "/public").unwrap();
    repo.log("main", 10).unwrap();
    let recorded = spans.finished_spans();
    assert!(named(&recorded, "asg.repo.get").is_empty());
    assert!(named(&recorded, "asg.repo.log").is_empty());

    // Writers racing on one ref: each rebuild is counted, and recorded on
    // the write it held up. Racing isn't certain, so race until it happens.
    spans.reset();
    let landed_before = commit_events(&records).len();
    let mut writes = 0;
    let retries = || match point(
        &meters,
        "asg.ref.cas_retries",
        &[("asg.operation.name", "set")],
    ) {
        Some(PointValue::Counter(n)) => n,
        _ => 0,
    };
    for round in 0..20 {
        let writers = 8;
        let barrier = Arc::new(Barrier::new(writers));
        let threads: Vec<_> = (0..writers)
            .map(|w| {
                let repo = Arc::clone(&repo);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    for i in 0..10 {
                        let path = format!("/public/race/r{round}/w{w}/i{i}");
                        repo.set_json("main", &path, &json!(i), options("racer"))
                            .unwrap();
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        writes += writers * 10;
        if retries() > 0 {
            break;
        }
    }
    assert!(retries() > 0, "eight writers never raced");
    let rebuilt: i64 = spans
        .finished_spans()
        .iter()
        .filter(|s| s.name == "asg.repo.set")
        .filter_map(|s| match attr(s, "asg.ref.cas_attempts") {
            Some(Value::I64(attempts)) => Some(attempts - 1),
            _ => None,
        })
        .sum();
    assert_eq!(rebuilt as u64, retries());
    // One event per write that landed: none for the commits that lost.
    assert_eq!(commit_events(&records).len() - landed_before, writes);

    // Nothing recorded carries a value or a state path, and every name of
    // our own is in the contract.
    let contract = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/OBSERVABILITY.md"),
    )
    .ok();
    let documented = |name: &str| {
        if let Some(doc) = &contract {
            assert!(
                doc.contains(&format!("`{name}`")),
                "{name} isn't in docs/OBSERVABILITY.md"
            );
        }
    };
    for span in spans.finished_spans().iter().chain(recorded.iter()) {
        if span.name.starts_with("asg.") {
            documented(&span.name);
        }
        let fields = span
            .attributes
            .iter()
            .chain(span.events.iter().flat_map(|e| &e.attributes));
        for kv in fields {
            let value = format!("{:?}", kv.value);
            assert!(!value.contains(SECRET), "{} carries a value", kv.key);
            assert!(!value.contains(PATH), "{} carries a state path", kv.key);
            if kv.key.starts_with("asg.") {
                documented(&kv.key);
            }
        }
    }
    for record in records.records() {
        for kv in &record.attributes {
            let value = format!("{:?}", kv.value);
            assert!(!value.contains(SECRET), "{} carries a value", kv.key);
            assert!(!value.contains(PATH), "{} carries a state path", kv.key);
            if kv.key.starts_with("asg.") {
                documented(&kv.key);
            }
        }
    }
    for family in meters.collect() {
        if family.name.starts_with("asg.") {
            documented(&family.name);
        }
        for p in &family.points {
            for (key, _) in &p.attributes {
                if key.starts_with("asg.") {
                    documented(key);
                }
            }
        }
    }
}
