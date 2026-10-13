//! Policy evaluation records its spans, events and metrics as
//! docs/OBSERVABILITY.md lists them, and an external evaluator that fails
//! is logged and counted rather than skipped silently.

use std::sync::Arc;

use agentstategraph::Repository;
use agentstategraph_policy::{
    AuthorizedAction, Decision, EvaluatorSource, ExternalError, ExternalEvaluator,
    ExternalEvaluatorRef, ExternalEvaluatorRegistry, Policy, PolicyStore, Selector, Severity,
    Situation,
};
use agentstategraph_storage::SqliteStorage;
use chrono::Utc;
use oxtel_sdk::metrics::{MeterProvider, PointValue};
use oxtel_sdk::trace::{InMemorySpanExporter, Sampler, SpanData, Status, TracerProvider};
use oxtel_sdk::{Resource, Value};
use tracing_subscriber::layer::SubscriberExt;

const REF: &str = "main";
/// What the failing evaluator puts in its message, which must not be recorded.
const ECHOED_INPUT: &str = "echoed-input-5678";

/// Fails every evaluation, the way a broken Rego runner would.
struct Broken;

impl ExternalEvaluator for Broken {
    fn kind(&self) -> &'static str {
        "rego"
    }

    fn evaluate(
        &self,
        _source: &EvaluatorSource,
        _situation: &Situation,
        _action: &str,
        _agent_id: &str,
    ) -> Result<Decision, ExternalError> {
        Err(ExternalError::Execution(format!(
            "opa eval exited 1: {ECHOED_INPUT}"
        )))
    }
}

fn policy(path: &str) -> Policy {
    Policy {
        path: path.into(),
        version: 0,
        situation: "test".into(),
        situation_selector: Selector::Always,
        allow: vec![],
        deny: vec![],
        require_approval: vec![],
        procedure: None,
        triggers: vec![],
        required_fields: vec![],
        severity: Severity::Low,
        proposed_by: String::new(),
        proposed_at: Utc::now(),
        ratified_by: None,
        ratified_at: None,
        ratification_reasoning: None,
        active_from: Utc::now(),
        expires_at: None,
        supersedes: None,
        signature: None,
        tenant_id: None,
        external_evaluator: None,
    }
}

fn attr(attributes: &[oxtel_sdk::KeyValue], key: &str) -> Option<Value> {
    attributes
        .iter()
        .find(|kv| kv.key == key)
        .map(|kv| kv.value.clone())
}

fn text(value: &str) -> Option<Value> {
    Some(Value::from(value.to_string()))
}

fn counter(meters: &MeterProvider, name: &str, labels: &[(&str, &str)]) -> u64 {
    meters
        .collect()
        .into_iter()
        .filter(|f| f.name == name)
        .flat_map(|f| f.points)
        .filter(|p| {
            labels
                .iter()
                .all(|(k, v)| p.attributes.contains(&((*k).into(), (*v).into())))
        })
        .map(|p| match p.value {
            PointValue::Counter(n) => n,
            other => panic!("{name} isn't a counter: {other:?}"),
        })
        .sum()
}

#[test]
fn evaluations_record_their_decisions_and_failures() {
    let spans = InMemorySpanExporter::default();
    let traces = TracerProvider::builder()
        .with_resource(Resource::builder().without_env().build())
        .with_sampler(Sampler::AlwaysOn)
        .with_span_events(true)
        .with_simple_exporter(spans.clone())
        .build();
    let meters = MeterProvider::builder().build();
    let recorder: &'static _ = Box::leak(Box::new(meters.recorder()));
    let _guards = (
        tracing::subscriber::set_default(tracing_subscriber::registry().with(traces.layer())),
        metrics::set_default_local_recorder(recorder),
    );

    let repo = Arc::new(Repository::new(Box::new(
        SqliteStorage::in_memory().unwrap(),
    )));
    repo.init().unwrap();
    let mut registry = ExternalEvaluatorRegistry::new();
    registry.register(Arc::new(Broken));
    let store =
        PolicyStore::new(repo, "/policies", "agent").with_external_evaluators(Arc::new(registry));

    // Nothing matches yet.
    let decision = store
        .evaluate(REF, &Situation::new(), "deploy", "agent-1")
        .unwrap();
    assert_eq!(decision, Decision::NoPolicyMatch);

    // A local allow, and a Rego policy whose evaluator fails: the allow
    // stands, and the failure is logged and counted.
    let mut local = policy("local/allow");
    local.allow = vec![AuthorizedAction {
        action: "deploy".into(),
        condition: None,
        preconditions: vec![],
    }];
    store.propose(REF, local).unwrap();
    store.ratify(REF, "local/allow", "alice", "ok").unwrap();
    let mut external = policy("external/rego");
    external.external_evaluator = Some(ExternalEvaluatorRef::Rego {
        source: EvaluatorSource::Inline {
            body: "package policy".into(),
        },
    });
    store.propose(REF, external).unwrap();
    store.ratify(REF, "external/rego", "alice", "ok").unwrap();
    let decision = store
        .evaluate(REF, &Situation::new(), "deploy", "agent-1")
        .unwrap();
    assert_eq!(decision.kind(), "allow");

    let recorded = spans.finished_spans();
    let evaluations: Vec<&SpanData> = recorded
        .iter()
        .filter(|s| s.name == "asg.policy.evaluate")
        .collect();
    assert_eq!(evaluations.len(), 2);
    assert_eq!(
        attr(&evaluations[0].attributes, "asg.policy.decision"),
        text("no_policy_match")
    );
    assert_eq!(
        attr(&evaluations[1].attributes, "asg.policy.decision"),
        text("allow")
    );
    let external = recorded
        .iter()
        .find(|s| s.name == "asg.policy.external")
        .expect("the external evaluation's span");
    assert_eq!(
        external.parent_span_id,
        Some(evaluations[1].context.span_id)
    );
    assert_eq!(
        attr(&external.attributes, "asg.policy.evaluator"),
        text("rego")
    );
    assert_eq!(
        attr(&external.attributes, "error.type"),
        text("evaluator_failed")
    );
    assert!(matches!(external.status, Status::Error { .. }));
    let event = external
        .events
        .iter()
        .find(|e| e.name == "an external policy evaluator failed, so its policy was skipped")
        .expect("the failure's event");
    assert_eq!(
        attr(&event.attributes, "asg.policy.id"),
        text("external/rego@1")
    );
    for kv in recorded.iter().flat_map(|s| {
        s.attributes
            .iter()
            .chain(s.events.iter().flat_map(|e| &e.attributes))
    }) {
        assert!(
            !format!("{:?}", kv.value).contains(ECHOED_INPUT),
            "{} carries the evaluator's message",
            kv.key
        );
    }

    assert_eq!(
        counter(
            &meters,
            "asg.policy.decisions",
            &[("asg.policy.decision", "no_policy_match")]
        ),
        1
    );
    assert_eq!(
        counter(
            &meters,
            "asg.policy.decisions",
            &[("asg.policy.decision", "allow")]
        ),
        1
    );
    assert_eq!(
        counter(
            &meters,
            "asg.policy.evaluator.errors",
            &[
                ("asg.policy.evaluator", "rego"),
                ("error.type", "evaluator_failed")
            ]
        ),
        1
    );
}
