//! Whenever the fail-safe decides, it logs what it decided. A test binary
//! of its own, with a global subscriber: with a thread-local one, another
//! test reaching the same event first could leave it cached as disabled.

use agentstategraph_mcp::server::render_decision_with_fail_safe;
use agentstategraph_policy::Decision;
use oxtel::sdk::trace::{InMemorySpanExporter, Sampler, TracerProvider};
use tracing_subscriber::layer::SubscriberExt;

#[test]
fn the_fail_safe_logs_its_decision() {
    let spans = InMemorySpanExporter::default();
    let traces = TracerProvider::builder()
        .with_sampler(Sampler::AlwaysOn)
        .with_span_events(true)
        .with_simple_exporter(spans.clone())
        .build();
    tracing::subscriber::set_global_default(tracing_subscriber::registry().with(traces.layer()))
        .unwrap();
    tracing::info_span!("call").in_scope(|| {
        render_decision_with_fail_safe(&Decision::NoPolicyMatch, "deny");
        let allowed = Decision::Allow {
            matched_policy: "x@1".into(),
            preconditions: Vec::new(),
        };
        render_decision_with_fail_safe(&allowed, "deny");
    });

    let call = &spans.finished_spans()[0];
    let decided: Vec<_> = call
        .events
        .iter()
        .filter(|e| e.name == "no policy matched, so the fail-safe decided")
        .collect();
    assert_eq!(decided.len(), 1, "only the unmatched decision");
    assert!(
        decided[0]
            .attributes
            .iter()
            .any(|kv| kv.key == "asg.policy.fail_safe" && kv.value == "deny".into())
    );
}
