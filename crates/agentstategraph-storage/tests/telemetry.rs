//! The backends record each query as a database span and a
//! `db.client.operation.duration` measurement, through oxtel-sql, and the
//! Postgres pool its `db.client.connection.*` metrics. One test, because it
//! installs a global subscriber and recorder: the Postgres backend runs its
//! queries on a runtime of its own. The Postgres half needs
//! `TEST_DATABASE_URL`.

use agentstategraph_core::Object;
use agentstategraph_storage::{CommitStore, ObjectStore, SqliteStorage};
use oxtel_sdk::Resource;
use oxtel_sdk::metrics::MeterProvider;
use oxtel_sdk::trace::{InMemorySpanExporter, Sampler, SpanData, TracerProvider};
use tracing_subscriber::layer::SubscriberExt;

fn attr(span: &SpanData, key: &str) -> Option<String> {
    span.attributes
        .iter()
        .find(|kv| kv.key == key)
        .map(|kv| match &kv.value {
            oxtel_sdk::Value::String(s) => s.to_string(),
            other => format!("{other:?}"),
        })
}

fn has_point(meters: &MeterProvider, metric: &str, label: (&str, &str)) -> bool {
    meters
        .collect()
        .into_iter()
        .filter(|f| f.name == metric)
        .flat_map(|f| f.points)
        .any(|p| p.attributes.contains(&(label.0.into(), label.1.into())))
}

#[test]
fn backends_record_their_queries() {
    let spans = InMemorySpanExporter::default();
    let traces = TracerProvider::builder()
        .with_resource(Resource::builder().without_env().build())
        .with_sampler(Sampler::AlwaysOn)
        .with_span_events(false)
        .with_simple_exporter(spans.clone())
        .build();
    let meters = MeterProvider::builder().build();
    tracing::subscriber::set_global_default(tracing_subscriber::registry().with(traces.layer()))
        .unwrap();
    metrics::set_global_recorder(meters.recorder()).unwrap();

    let sqlite = SqliteStorage::in_memory().unwrap();
    sqlite.put_object(&Object::string("hello")).unwrap();
    let recorded = spans.finished_spans();
    let insert = recorded
        .iter()
        .find(|s| {
            attr(s, "db.system.name").as_deref() == Some("sqlite") && s.name.starts_with("INSERT")
        })
        .unwrap_or_else(|| {
            panic!(
                "no SQLite insert span in {:?}",
                recorded.iter().map(|s| &s.name).collect::<Vec<_>>()
            )
        });
    let text = attr(insert, "db.query.text").expect("the query's text");
    assert!(!text.contains("hello"), "a value reached telemetry: {text}");
    assert!(has_point(
        &meters,
        "db.client.operation.duration",
        ("db.system.name", "sqlite")
    ));
    // SQLite's single connection, recorded as a pool of one.
    let pool = ("db.client.connection.pool.name", "sqlite");
    assert!(has_point(&meters, "db.client.connection.wait_time", pool));
    assert!(has_point(&meters, "db.client.connection.use_time", pool));
    let totals = sqlite.store_totals().unwrap().expect("SQLite counts");
    assert_eq!(totals.objects, 1);
    assert_eq!(totals.bytes, None, "an in-memory store has no file");

    #[cfg(feature = "postgres")]
    postgres(&spans, &meters);
}

#[cfg(feature = "postgres")]
fn postgres(spans: &InMemorySpanExporter, meters: &MeterProvider) {
    let Some(url) = std::env::var("TEST_DATABASE_URL")
        .ok()
        .filter(|u| !u.is_empty())
    else {
        let required =
            std::env::var("ASG_REQUIRE_POSTGRES").is_ok_and(|v| !v.is_empty() && v != "0");
        assert!(
            !required,
            "ASG_REQUIRE_POSTGRES is set, but TEST_DATABASE_URL isn't"
        );
        eprintln!("skip the Postgres half: TEST_DATABASE_URL not set");
        return;
    };
    let tenant = format!("telemetry-{}", uuid::Uuid::new_v4().simple());
    let postgres =
        agentstategraph_storage::PostgresStorage::connect_tenant_blocking(&url, &tenant, 2)
            .unwrap();
    postgres.put_object(&Object::string("hello")).unwrap();
    assert!(postgres.has_object(&Object::string("hello").id()).unwrap());
    let totals = postgres.store_totals().unwrap().expect("Postgres counts");
    assert_eq!(
        (totals.objects, totals.bytes),
        (1, None),
        "this tenant's one object"
    );
    let recorded = spans.finished_spans();
    let postgres_spans: Vec<&SpanData> = recorded
        .iter()
        .filter(|s| attr(s, "db.system.name").as_deref() == Some("postgresql"))
        .collect();
    assert!(
        postgres_spans
            .iter()
            .any(|s| s.name.starts_with("INSERT objects")),
        "{:?}",
        postgres_spans.iter().map(|s| &s.name).collect::<Vec<_>>()
    );
    for span in &postgres_spans {
        if let Some(text) = attr(span, "db.query.text") {
            assert!(
                !text.contains(&tenant),
                "the tenant id reached telemetry: {text}"
            );
        }
    }
    assert!(has_point(
        meters,
        "db.client.operation.duration",
        ("db.system.name", "postgresql")
    ));
    assert!(has_point(
        meters,
        "db.client.connection.count",
        ("db.client.connection.state", "idle")
    ));
}
