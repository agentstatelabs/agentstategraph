//! With commit events on, the server's log takes each `asg.commit` event
//! whatever level the rest of the log runs at (DESIGN.md Plan H t-010). One
//! test, because it installs the process's telemetry.

use std::sync::Arc;

use agentstategraph::{CommitOptions, Repository};
use agentstategraph_core::IntentCategory;
use agentstategraph_storage::SqliteStorage;
use oxtel::sdk::logs::InMemoryLogExporter;

#[test]
fn commit_events_pass_a_quiet_log() {
    let records = InMemoryLogExporter::default();
    // As `main` sets up an MCP stdio server with ASG_COMMIT_EVENTS=on.
    let _guard = oxtel::builder()
        .without_env_exporters()
        .without_local_logs()
        .with_logs_filter(format!(
            "warn,{}",
            agentstategraph_mcp::COMMIT_EVENTS_DIRECTIVE
        ))
        .with_logs({
            let records = records.clone();
            move |logs| logs.with_simple_exporter(records)
        })
        .install()
        .expect("install telemetry");
    agentstategraph::emit_commit_events(true);

    let repo = Arc::new(Repository::new(Box::new(
        SqliteStorage::in_memory().unwrap(),
    )));
    repo.init().unwrap();
    let id = repo
        .set_json(
            "main",
            "/greeting",
            &serde_json::json!("hello"),
            CommitOptions::new("agent-1", IntentCategory::Refine, "say hello"),
        )
        .unwrap();
    tracing::info!("an ordinary info line, which a warn log leaves out");

    let records = records.records();
    let events: Vec<_> = records
        .iter()
        .filter(|r| r.event_name.as_deref() == Some("asg.commit"))
        .collect();
    assert_eq!(events.len(), 1, "{records:?}");
    assert!(
        events[0]
            .attributes
            .iter()
            .any(|kv| kv.key == "asg.commit.id" && kv.value == id.to_string().into())
    );
    assert_eq!(
        records.len(),
        1,
        "only the commit event got through: {records:?}"
    );
}
