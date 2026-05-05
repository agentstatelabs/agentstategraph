//! §6 integration tests — taint / quarantine / watch MCP tools
//! exercise the Repository integration via the server without
//! booting the MCP stdio transport.

use std::sync::Arc;

use agentstategraph::Repository;
use agentstategraph_mcp::server::{
    AgentStateGraphServer, parse_optional_rfc3339, parse_taint_effect, parse_taint_severity,
};
use agentstategraph_storage::SqliteStorage;
use agentstategraph_taint::{TaintEffect, TaintKind, TaintMetadata, TaintParams, TaintSeverity};

fn server() -> AgentStateGraphServer {
    let repo = Arc::new(Repository::new(Box::new(SqliteStorage::in_memory().expect("in-memory sqlite"))));
    repo.init().unwrap();
    AgentStateGraphServer::new(repo)
}

#[test]
fn parse_taint_effect_recognizes_all_variants() {
    assert_eq!(parse_taint_effect("warn"), Some(TaintEffect::Warn));
    assert_eq!(parse_taint_effect("BLOCK"), Some(TaintEffect::Block));
    assert_eq!(parse_taint_effect("review"), Some(TaintEffect::Review));
    assert_eq!(parse_taint_effect("isolate"), Some(TaintEffect::Isolate));
    assert_eq!(parse_taint_effect("advisory"), Some(TaintEffect::Advisory));
    assert_eq!(parse_taint_effect("unknown"), None);
}

#[test]
fn parse_taint_severity_defaults_to_medium() {
    assert_eq!(parse_taint_severity(None), TaintSeverity::Medium);
    assert_eq!(parse_taint_severity(Some("")), TaintSeverity::Medium);
    assert_eq!(
        parse_taint_severity(Some("critical")),
        TaintSeverity::Critical
    );
    assert_eq!(parse_taint_severity(Some("LOW")), TaintSeverity::Low);
    assert_eq!(parse_taint_severity(Some("bogus")), TaintSeverity::Medium);
}

#[test]
fn parse_rfc3339_round_trips() {
    assert!(parse_optional_rfc3339(None).is_none());
    assert!(parse_optional_rfc3339(Some("not-a-date")).is_none());
    let parsed = parse_optional_rfc3339(Some("2026-04-21T12:00:00Z")).unwrap();
    assert_eq!(parsed.to_rfc3339(), "2026-04-21T12:00:00+00:00");
}

#[test]
fn taint_round_trip_through_server_repo() {
    let s = server();
    let id = s
        .repo()
        .taint(
            "main",
            "/cluster",
            TaintParams {
                name: "unstable".into(),
                effect: TaintEffect::Warn,
                reason: "flaky".into(),
                severity: TaintSeverity::Medium,
                expires_at: None,
                propagate: true,
                metadata: TaintMetadata::new(),
                agent_id: "ops".into(),
            },
        )
        .unwrap();
    let listed = s
        .repo()
        .list_taints(None, Some(TaintKind::Taint), false)
        .unwrap();
    assert!(listed.iter().any(|t| t.id == id));
}

#[test]
fn check_taint_surfaces_status_through_server() {
    let s = server();
    s.repo()
        .taint(
            "main",
            "/secret",
            TaintParams {
                name: "review".into(),
                effect: TaintEffect::Review,
                reason: "review".into(),
                severity: TaintSeverity::Medium,
                expires_at: None,
                propagate: true,
                metadata: TaintMetadata::new(),
                agent_id: "ops".into(),
            },
        )
        .unwrap();
    let c = s.repo().check_taint("/secret/x", "agent-1", 0.5).unwrap();
    assert!(c.tainted);
    assert_eq!(c.required_confidence, 0.9);
    assert!(!c.can_write);
}
