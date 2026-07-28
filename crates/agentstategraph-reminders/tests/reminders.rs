//! Integration tests for the reminders crate.
//!
//! Uses `MemoryReminderStore` + `ReminderManager` as the production
//! code path. Every public behaviour is covered here.

use std::sync::Arc;

use chrono::{Duration, NaiveTime, Utc, Weekday};

use agentstategraph_reminders::{
    CreateReminder, ExecutionRecord, ExecutionResult, MemoryReminderStore, Priority, RefKind,
    ReminderFilter, ReminderManager, ReminderRef, ReminderStatus, Schedule,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn manager() -> ReminderManager {
    ReminderManager::new(Arc::new(MemoryReminderStore::new()))
}

fn due_in(secs: i64) -> chrono::DateTime<Utc> {
    Utc::now() + Duration::seconds(secs)
}

fn past(secs: i64) -> chrono::DateTime<Utc> {
    Utc::now() - Duration::seconds(secs)
}

fn success_record(agent: &str) -> ExecutionRecord {
    ExecutionRecord {
        started_at: Utc::now(),
        completed_at: None,
        agent_id: agent.to_string(),
        approved_by: None,
        result: ExecutionResult::Success,
        notes: Vec::new(),
        task_id: None,
    }
}

// ---------------------------------------------------------------------------
// CRUD
// ---------------------------------------------------------------------------

#[test]
fn create_and_get_roundtrip() {
    let mgr = manager();
    let r = mgr
        .create(CreateReminder::new(
            "Check dev server",
            "See if it's still running",
            due_in(3600),
            "agent/test",
        ))
        .unwrap();
    assert_eq!(r.status, ReminderStatus::Pending);
    let fetched = mgr.get(&r.id).unwrap();
    assert_eq!(fetched.title, "Check dev server");
}

#[test]
fn get_nonexistent_returns_not_found() {
    let mgr = manager();
    assert!(matches!(
        mgr.get("no-such-id"),
        Err(agentstategraph_reminders::ReminderError::NotFound(_))
    ));
}

#[test]
fn cancel_moves_to_cancelled() {
    let mgr = manager();
    let r = mgr
        .create(CreateReminder::new("t", "i", due_in(60), "a"))
        .unwrap();
    let cancelled = mgr.cancel(&r.id).unwrap();
    assert_eq!(cancelled.status, ReminderStatus::Cancelled);
}

#[test]
fn cancel_completed_reminder_returns_error() {
    let mgr = manager();
    let r = mgr
        .create(CreateReminder::new("t", "i", past(1), "a"))
        .unwrap();
    // promote to Due then record success → Completed
    mgr.remind_me().unwrap();
    mgr.record_execution(&r.id, success_record("a")).unwrap();
    let result = mgr.cancel(&r.id);
    assert!(matches!(
        result,
        Err(agentstategraph_reminders::ReminderError::AlreadyTerminal(
            ..
        ))
    ));
}

// ---------------------------------------------------------------------------
// remind_me — lazy promotion + ordering
// ---------------------------------------------------------------------------

#[test]
fn remind_me_promotes_pending_past_due() {
    let mgr = manager();
    let r = mgr
        .create(CreateReminder::new("overdue", "i", past(10), "a"))
        .unwrap();
    assert_eq!(mgr.get(&r.id).unwrap().status, ReminderStatus::Pending);

    let due = mgr.remind_me().unwrap();
    assert!(due.iter().any(|x| x.id == r.id));
    assert_eq!(mgr.get(&r.id).unwrap().status, ReminderStatus::Due);
}

#[test]
fn remind_me_does_not_promote_future_reminders() {
    let mgr = manager();
    let r = mgr
        .create(CreateReminder::new("future", "i", due_in(3600), "a"))
        .unwrap();
    mgr.remind_me().unwrap();
    assert_eq!(mgr.get(&r.id).unwrap().status, ReminderStatus::Pending);
}

#[test]
fn remind_me_orders_by_priority_then_due_at() {
    let mgr = manager();
    let _low = mgr
        .create(CreateReminder::new("low", "i", past(5), "a").with_priority(Priority::Low))
        .unwrap();
    let _crit = mgr
        .create(
            CreateReminder::new("critical", "i", past(3), "a").with_priority(Priority::Critical),
        )
        .unwrap();
    let _high = mgr
        .create(CreateReminder::new("high", "i", past(1), "a").with_priority(Priority::High))
        .unwrap();

    let due = mgr.remind_me().unwrap();
    assert_eq!(due[0].title, "critical");
    assert_eq!(due[1].title, "high");
    assert_eq!(due[2].title, "low");
}

#[test]
fn remind_me_gates_non_autonomous_to_awaiting_permission() {
    let mgr = manager();
    // Autonomous + past due → promoted straight to Due (pre-approved).
    let auto = mgr
        .create(CreateReminder::new("auto", "i", past(2), "a").with_autonomous(true))
        .unwrap();
    // Non-autonomous + past due → held at AwaitingPermission until approved.
    let manual = mgr
        .create(CreateReminder::new("manual", "i", past(1), "a").with_autonomous(false))
        .unwrap();

    let actionable = mgr.remind_me().unwrap();
    // Both surface as actionable, but with different statuses.
    assert!(actionable.iter().any(|x| x.id == auto.id));
    assert!(actionable.iter().any(|x| x.id == manual.id));
    assert_eq!(mgr.get(&auto.id).unwrap().status, ReminderStatus::Due);
    assert_eq!(
        mgr.get(&manual.id).unwrap().status,
        ReminderStatus::AwaitingPermission,
        "non-autonomous reminders must not auto-promote to Due"
    );
}

// ---------------------------------------------------------------------------
// Snooze
// ---------------------------------------------------------------------------

#[test]
fn snooze_sets_snoozed_status_and_until() {
    let mgr = manager();
    let r = mgr
        .create(CreateReminder::new("t", "i", past(1), "a"))
        .unwrap();
    mgr.remind_me().unwrap();
    let wake = due_in(3600);
    let snoozed = mgr.snooze(&r.id, wake).unwrap();
    assert_eq!(snoozed.status, ReminderStatus::Snoozed);
    assert_eq!(snoozed.snoozed_until, Some(wake));
}

#[test]
fn snoozed_reminder_wakes_on_remind_me() {
    let mgr = manager();
    let r = mgr
        .create(CreateReminder::new("t", "i", past(100), "a"))
        .unwrap();
    mgr.remind_me().unwrap();
    // Snooze until the past (expired snooze)
    mgr.snooze(&r.id, past(1)).unwrap();
    assert_eq!(mgr.get(&r.id).unwrap().status, ReminderStatus::Snoozed);

    mgr.remind_me().unwrap();
    assert_eq!(mgr.get(&r.id).unwrap().status, ReminderStatus::Due);
}

#[test]
fn snooze_terminal_reminder_returns_error() {
    let mgr = manager();
    let r = mgr
        .create(CreateReminder::new("t", "i", past(1), "a"))
        .unwrap();
    mgr.cancel(&r.id).unwrap();
    assert!(mgr.snooze(&r.id, due_in(60)).is_err());
}

// ---------------------------------------------------------------------------
// Approve (non-autonomous flow)
// ---------------------------------------------------------------------------

#[test]
fn approve_awaiting_permission_moves_to_due() {
    let mgr = manager();
    // Create non-autonomous, past-due reminder
    let r = mgr
        .create(CreateReminder::new("t", "i", past(1), "a").with_autonomous(false))
        .unwrap();
    // remind_me holds a non-autonomous past-due reminder at AwaitingPermission;
    // approve then transitions it to Due.
    mgr.remind_me().unwrap();
    assert_eq!(
        mgr.get(&r.id).unwrap().status,
        ReminderStatus::AwaitingPermission
    );
    let approved = mgr.approve(&r.id, "human/alice").unwrap();
    assert_eq!(approved.status, ReminderStatus::Due);
}

#[test]
fn approve_terminal_reminder_returns_error() {
    let mgr = manager();
    let r = mgr
        .create(CreateReminder::new("t", "i", past(1), "a"))
        .unwrap();
    mgr.cancel(&r.id).unwrap();
    assert!(mgr.approve(&r.id, "human/alice").is_err());
}

// ---------------------------------------------------------------------------
// start + record_execution
// ---------------------------------------------------------------------------

#[test]
fn start_and_complete_marks_completed() {
    let mgr = manager();
    let r = mgr
        .create(CreateReminder::new("t", "i", past(1), "a"))
        .unwrap();
    mgr.remind_me().unwrap();
    mgr.start(&r.id, "agent/worker").unwrap();
    assert_eq!(mgr.get(&r.id).unwrap().status, ReminderStatus::InProgress);

    let done = mgr
        .record_execution(&r.id, success_record("agent/worker"))
        .unwrap();
    assert_eq!(done.status, ReminderStatus::Completed);
    assert_eq!(done.executions.len(), 1);
    assert!(done.executions[0].completed_at.is_some());
}

#[test]
fn record_execution_without_start_also_works() {
    let mgr = manager();
    let r = mgr
        .create(CreateReminder::new("t", "i", past(1), "a"))
        .unwrap();
    mgr.remind_me().unwrap();
    let done = mgr
        .record_execution(&r.id, success_record("agent/a"))
        .unwrap();
    assert_eq!(done.status, ReminderStatus::Completed);
    assert_eq!(done.executions.len(), 1);
}

#[test]
fn failed_execution_returns_to_due() {
    let mgr = manager();
    let r = mgr
        .create(CreateReminder::new("t", "i", past(1), "a"))
        .unwrap();
    mgr.remind_me().unwrap();
    mgr.start(&r.id, "agent/a").unwrap();
    let after = mgr
        .record_execution(
            &r.id,
            ExecutionRecord {
                started_at: Utc::now(),
                completed_at: None,
                agent_id: "agent/a".into(),
                approved_by: None,
                result: ExecutionResult::Failed,
                notes: vec!["disk full".into()],
                task_id: None,
            },
        )
        .unwrap();
    assert_eq!(after.status, ReminderStatus::Due);
    assert!(!after.executions[0].notes.is_empty());
}

#[test]
fn execution_records_task_id() {
    let mgr = manager();
    let r = mgr
        .create(CreateReminder::new("t", "i", past(1), "a"))
        .unwrap();
    mgr.remind_me().unwrap();
    let done = mgr
        .record_execution(
            &r.id,
            ExecutionRecord {
                started_at: Utc::now(),
                completed_at: None,
                agent_id: "agent/a".into(),
                approved_by: None,
                result: ExecutionResult::Success,
                notes: Vec::new(),
                task_id: Some("task-0042".into()),
            },
        )
        .unwrap();
    assert_eq!(done.executions[0].task_id.as_deref(), Some("task-0042"));
}

// ---------------------------------------------------------------------------
// Repeating schedule advancement
// ---------------------------------------------------------------------------

#[test]
fn interval_schedule_resets_to_pending_after_success() {
    let mgr = manager();
    let r = mgr
        .create(
            CreateReminder::new("recurring", "i", past(1), "a").with_schedule(Schedule::Interval {
                every_seconds: 3600,
            }),
        )
        .unwrap();
    mgr.remind_me().unwrap();
    let after = mgr
        .record_execution(&r.id, success_record("agent/a"))
        .unwrap();
    assert_eq!(after.status, ReminderStatus::Pending);
    assert!(after.due_at > Utc::now());
    // Should be roughly 1 hour from now
    let diff = (after.due_at - Utc::now()).num_seconds();
    assert!(diff >= 3550 && diff <= 3650, "expected ~3600s, got {diff}");
}

#[test]
fn non_autonomous_repeating_resets_to_awaiting_permission() {
    let mgr = manager();
    let r = mgr
        .create(
            CreateReminder::new("weekly-report", "i", past(1), "a")
                .with_autonomous(false)
                .with_schedule(Schedule::Weekly {
                    day: Weekday::Mon,
                    time: NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
                }),
        )
        .unwrap();
    mgr.remind_me().unwrap();
    let after = mgr
        .record_execution(&r.id, success_record("agent/a"))
        .unwrap();
    assert_eq!(after.status, ReminderStatus::AwaitingPermission);
    assert!(after.due_at > Utc::now());
}

#[test]
fn once_schedule_marks_completed_not_rescheduled() {
    let mgr = manager();
    let r = mgr
        .create(CreateReminder::new("one-time", "i", past(1), "a"))
        .unwrap();
    mgr.remind_me().unwrap();
    let after = mgr
        .record_execution(&r.id, success_record("agent/a"))
        .unwrap();
    assert_eq!(after.status, ReminderStatus::Completed);
}

#[test]
fn repeating_reminder_accumulates_execution_history() {
    let mgr = manager();
    let r = mgr
        .create(
            CreateReminder::new("daily-check", "i", past(1), "a")
                .with_schedule(Schedule::Interval { every_seconds: 60 }),
        )
        .unwrap();

    for _ in 0..3 {
        mgr.remind_me().unwrap();
        // Force due for next iteration
        let mut reminder = mgr.get(&r.id).unwrap();
        reminder.due_at = past(1);
        reminder.status = ReminderStatus::Due;
        // Directly update via list workaround: just call record_execution
        mgr.record_execution(&r.id, success_record("agent/a"))
            .unwrap();
    }

    let final_r = mgr.get(&r.id).unwrap();
    assert_eq!(final_r.executions.len(), 3);
}

// ---------------------------------------------------------------------------
// Soft refs — stale marking
// ---------------------------------------------------------------------------

#[test]
fn mark_ref_stale_updates_matching_ref() {
    let mgr = manager();
    let r = mgr
        .create(
            CreateReminder::new("t", "i", due_in(60), "a").with_refs(vec![
                ReminderRef::branch("feature/web-demo", "Web demo branch"),
                ReminderRef::task("t-042", "Deploy task"),
            ]),
        )
        .unwrap();

    let count = mgr.mark_ref_stale(&r.id, "feature/web-demo").unwrap();
    assert_eq!(count, 1);

    let updated = mgr.get(&r.id).unwrap();
    let branch_ref = updated
        .refs
        .iter()
        .find(|rf| rf.id == "feature/web-demo")
        .unwrap();
    assert!(branch_ref.stale);
    let task_ref = updated.refs.iter().find(|rf| rf.id == "t-042").unwrap();
    assert!(!task_ref.stale);
}

#[test]
fn mark_ref_stale_idempotent() {
    let mgr = manager();
    let r = mgr
        .create(
            CreateReminder::new("t", "i", due_in(60), "a")
                .with_refs(vec![ReminderRef::branch("main", "main")]),
        )
        .unwrap();
    assert_eq!(mgr.mark_ref_stale(&r.id, "main").unwrap(), 1);
    assert_eq!(mgr.mark_ref_stale(&r.id, "main").unwrap(), 0); // already stale
}

#[test]
fn mark_ref_stale_nonexistent_ref_returns_zero() {
    let mgr = manager();
    let r = mgr
        .create(CreateReminder::new("t", "i", due_in(60), "a"))
        .unwrap();
    assert_eq!(mgr.mark_ref_stale(&r.id, "no-such-ref").unwrap(), 0);
}

#[test]
fn stale_ref_label_preserved() {
    let mgr = manager();
    let r = mgr
        .create(
            CreateReminder::new("t", "i", due_in(60), "a").with_refs(vec![ReminderRef::branch(
                "feature/x",
                "Feature X — web server cleanup",
            )]),
        )
        .unwrap();
    mgr.mark_ref_stale(&r.id, "feature/x").unwrap();
    let updated = mgr.get(&r.id).unwrap();
    let rf = &updated.refs[0];
    assert!(rf.stale);
    assert_eq!(rf.label.as_deref(), Some("Feature X — web server cleanup"));
}

// ---------------------------------------------------------------------------
// List filtering
// ---------------------------------------------------------------------------

#[test]
fn list_by_tags() {
    let mgr = manager();
    mgr.create(
        CreateReminder::new("infra", "i", due_in(60), "a")
            .with_tags(vec!["infra".into(), "cleanup".into()]),
    )
    .unwrap();
    mgr.create(CreateReminder::new("other", "i", due_in(60), "a").with_tags(vec!["other".into()]))
        .unwrap();

    let results = mgr
        .list(&ReminderFilter {
            tags: vec!["cleanup".into()],
            ..Default::default()
        })
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].title, "infra");
}

#[test]
fn list_by_ref_id() {
    let mgr = manager();
    mgr.create(
        CreateReminder::new("with-ref", "i", due_in(60), "a")
            .with_refs(vec![ReminderRef::plan("plan-99", "Q4 rollout")]),
    )
    .unwrap();
    mgr.create(CreateReminder::new("no-ref", "i", due_in(60), "a"))
        .unwrap();

    let results = mgr
        .list(&ReminderFilter {
            ref_id: Some("plan-99".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].title, "with-ref");
}

#[test]
fn list_by_created_by() {
    let mgr = manager();
    mgr.create(CreateReminder::new("mine", "i", due_in(60), "agent/alice"))
        .unwrap();
    mgr.create(CreateReminder::new("theirs", "i", due_in(60), "agent/bob"))
        .unwrap();

    let results = mgr
        .list(&ReminderFilter {
            created_by: Some("agent/alice".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].title, "mine");
}

// ---------------------------------------------------------------------------
// RefKind coverage
// ---------------------------------------------------------------------------

#[test]
fn all_ref_kinds_roundtrip_json() {
    let refs = vec![
        ReminderRef::branch("b", "branch"),
        ReminderRef::memory("mem-key"),
        ReminderRef::plan("plan-1", "My plan"),
        ReminderRef::task("t-01", "Task 1"),
        ReminderRef::state_path("/_sessions/srv"),
        ReminderRef::external("https://example.com", "https"),
    ];
    for r in refs {
        let j = serde_json::to_value(&r).unwrap();
        let back: ReminderRef = serde_json::from_value(j).unwrap();
        assert_eq!(r.kind, back.kind);
        assert_eq!(r.id, back.id);
    }
}

#[test]
fn external_ref_kind_carries_scheme() {
    let r = ReminderRef::external("https://grafana.internal/d/latency", "https");
    if let RefKind::External { scheme } = &r.kind {
        assert_eq!(scheme, "https");
    } else {
        panic!("expected External kind");
    }
}
