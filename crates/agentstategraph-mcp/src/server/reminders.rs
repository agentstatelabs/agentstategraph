//! Reminder tool implementations.

use agentstategraph_reminders::{
    CreateReminder, ExecutionRecord, ExecutionResult, Priority, ReminderFilter, ReminderRef,
    ReminderStatus, Schedule,
};
use chrono::{NaiveTime, Utc, Weekday};

use super::{
    AgentStateGraphServer, ReminderApproveParams, ReminderCancelParams, ReminderCreateParams,
    ReminderListParams, ReminderRecordParams, ReminderSnoozeParams,
};

impl AgentStateGraphServer {
    pub(super) fn impl_reminder_create(&self, p: ReminderCreateParams) -> String {
        let due_at = match chrono::DateTime::parse_from_rfc3339(&p.due_at) {
            Ok(dt) => dt.with_timezone(&Utc),
            Err(e) => return format!("Error: invalid due_at format (expected RFC3339): {e}"),
        };

        let schedule = match p.schedule.as_deref() {
            None | Some("once") => None,
            Some(s) if s.starts_with("interval:") => {
                let secs: u64 = match s.trim_start_matches("interval:").parse() {
                    Ok(n) => n,
                    Err(_) => return "Error: interval schedule must be 'interval:<seconds>'".to_string(),
                };
                Some(Schedule::Interval { every_seconds: secs })
            }
            Some(s) if s.starts_with("daily:") => {
                let time_str = s.trim_start_matches("daily:");
                match NaiveTime::parse_from_str(time_str, "%H:%M") {
                    Ok(t) => Some(Schedule::Daily { time: t }),
                    Err(_) => return format!("Error: daily schedule must be 'daily:HH:MM', got '{time_str}'"),
                }
            }
            Some(s) if s.starts_with("weekly:") => {
                // Format: weekly:Mon:09:00
                let parts: Vec<&str> = s.trim_start_matches("weekly:").splitn(2, ':').collect();
                if parts.len() != 2 {
                    return "Error: weekly schedule must be 'weekly:Weekday:HH:MM'".to_string();
                }
                let day = match parts[0].to_lowercase().as_str() {
                    "mon" | "monday" => Weekday::Mon,
                    "tue" | "tuesday" => Weekday::Tue,
                    "wed" | "wednesday" => Weekday::Wed,
                    "thu" | "thursday" => Weekday::Thu,
                    "fri" | "friday" => Weekday::Fri,
                    "sat" | "saturday" => Weekday::Sat,
                    "sun" | "sunday" => Weekday::Sun,
                    d => return format!("Error: unknown weekday '{d}'"),
                };
                match NaiveTime::parse_from_str(parts[1], "%H:%M") {
                    Ok(t) => Some(Schedule::Weekly { day, time: t }),
                    Err(_) => return format!("Error: time must be HH:MM, got '{}'", parts[1]),
                }
            }
            Some(other) => return format!("Error: unknown schedule format '{other}'. Use 'once', 'interval:<secs>', 'daily:HH:MM', or 'weekly:Weekday:HH:MM'"),
        };

        let priority = match p.priority.as_deref().unwrap_or("medium") {
            "critical" | "1" => Priority::Critical,
            "high" | "2" => Priority::High,
            "medium" | "3" => Priority::Medium,
            "low" | "4" => Priority::Low,
            "minimal" | "5" => Priority::Minimal,
            other => return format!("Error: unknown priority '{other}'. Use critical/high/medium/low/minimal"),
        };

        let refs: Vec<ReminderRef> = p.refs.unwrap_or_default().into_iter().map(|r| {
            match r.kind.to_lowercase().as_str() {
                "branch" => ReminderRef::branch(&r.id, r.label.as_deref().unwrap_or(&r.id)),
                "memory" => ReminderRef::memory(&r.id),
                "plan" => ReminderRef::plan(&r.id, r.label.as_deref().unwrap_or(&r.id)),
                "task" => ReminderRef::task(&r.id, r.label.as_deref().unwrap_or(&r.id)),
                "state_path" | "statepath" => ReminderRef::state_path(&r.id),
                _ => ReminderRef::external(&r.id, &r.kind),
            }
        }).collect();

        let input = CreateReminder::new(&p.title, &p.instructions, due_at, &p.created_by)
            .with_priority(priority)
            .with_autonomous(p.autonomous.unwrap_or(true))
            .with_commands(p.commands.unwrap_or_default())
            .with_refs(refs)
            .with_tags(p.tags.unwrap_or_default());

        let input = if let Some(s) = schedule { input.with_schedule(s) } else { input };

        match self.reminders.create(input) {
            Ok(r) => serde_json::to_string_pretty(&serde_json::json!({
                "id": r.id,
                "title": r.title,
                "status": format!("{:?}", r.status),
                "due_at": r.due_at.to_rfc3339(),
                "priority": format!("{:?}", r.priority),
                "autonomous": r.autonomous,
                "refs": r.refs.len(),
            })).unwrap_or_default(),
            Err(e) => format!("Error: {e}"),
        }
    }

    pub(super) fn impl_reminder_list(&self, p: ReminderListParams) -> String {
        let status = p.status.and_then(|s| match s.to_lowercase().as_str() {
            "pending" => Some(ReminderStatus::Pending),
            "due" => Some(ReminderStatus::Due),
            "awaiting_permission" | "awaiting-permission" => Some(ReminderStatus::AwaitingPermission),
            "in_progress" | "inprogress" => Some(ReminderStatus::InProgress),
            "completed" => Some(ReminderStatus::Completed),
            "snoozed" => Some(ReminderStatus::Snoozed),
            "cancelled" => Some(ReminderStatus::Cancelled),
            _ => None,
        });

        let filter = ReminderFilter {
            status,
            created_by: p.created_by,
            tags: p.tags.unwrap_or_default(),
            ref_id: p.ref_id,
            ..Default::default()
        };

        match self.reminders.list(&filter) {
            Ok(reminders) => {
                let json: Vec<serde_json::Value> = reminders.iter().map(|r| serde_json::json!({
                    "id": r.id,
                    "title": r.title,
                    "status": format!("{:?}", r.status),
                    "priority": format!("{:?}", r.priority),
                    "due_at": r.due_at.to_rfc3339(),
                    "autonomous": r.autonomous,
                    "tags": r.tags,
                    "refs": r.refs.iter().map(|rf| serde_json::json!({
                        "kind": format!("{:?}", rf.kind),
                        "id": rf.id,
                        "label": rf.label,
                        "stale": rf.stale,
                    })).collect::<Vec<_>>(),
                    "executions": r.executions.len(),
                })).collect();
                format!("{} reminders:\n{}", json.len(),
                    serde_json::to_string_pretty(&json).unwrap_or_default())
            }
            Err(e) => format!("Error: {e}"),
        }
    }

    pub(super) fn impl_reminder_remind_me(&self) -> String {
        match self.reminders.remind_me() {
            Ok(reminders) if reminders.is_empty() => "No reminders due.".to_string(),
            Ok(reminders) => {
                let json: Vec<serde_json::Value> = reminders.iter().map(|r| serde_json::json!({
                    "id": r.id,
                    "title": r.title,
                    "priority": format!("{:?}", r.priority),
                    "due_at": r.due_at.to_rfc3339(),
                    "instructions": r.instructions,
                    "commands": r.commands,
                    "autonomous": r.autonomous,
                    "status": format!("{:?}", r.status),
                    "refs": r.refs.iter().map(|rf| serde_json::json!({
                        "kind": format!("{:?}", rf.kind),
                        "id": rf.id,
                        "label": rf.label,
                        "stale": rf.stale,
                    })).collect::<Vec<_>>(),
                    "tags": r.tags,
                })).collect();
                format!("{} reminder(s) due:\n{}",
                    reminders.len(),
                    serde_json::to_string_pretty(&json).unwrap_or_default())
            }
            Err(e) => format!("Error: {e}"),
        }
    }

    pub(super) fn impl_reminder_snooze(&self, p: ReminderSnoozeParams) -> String {
        let until = match chrono::DateTime::parse_from_rfc3339(&p.until) {
            Ok(dt) => dt.with_timezone(&Utc),
            Err(e) => return format!("Error: invalid until format (expected RFC3339): {e}"),
        };
        match self.reminders.snooze(&p.id, until) {
            Ok(r) => format!("Reminder '{}' snoozed until {}", r.title, until.to_rfc3339()),
            Err(e) => format!("Error: {e}"),
        }
    }

    pub(super) fn impl_reminder_approve(&self, p: ReminderApproveParams) -> String {
        match self.reminders.approve(&p.id, &p.approved_by) {
            Ok(r) => format!("Reminder '{}' approved by {} — status: {:?}", r.title, p.approved_by, r.status),
            Err(e) => format!("Error: {e}"),
        }
    }

    pub(super) fn impl_reminder_cancel(&self, p: ReminderCancelParams) -> String {
        match self.reminders.cancel(&p.id) {
            Ok(r) => format!("Reminder '{}' cancelled.", r.title),
            Err(e) => format!("Error: {e}"),
        }
    }

    pub(super) fn impl_reminder_record(&self, p: ReminderRecordParams) -> String {
        let result = match p.result.to_lowercase().as_str() {
            "success" => ExecutionResult::Success,
            "failed" | "failure" => ExecutionResult::Failed,
            "deferred" => ExecutionResult::Deferred,
            "snoozed" => ExecutionResult::Snoozed,
            "cancelled" => ExecutionResult::Cancelled,
            other => return format!("Error: unknown result '{other}'. Use success/failed/deferred/snoozed/cancelled"),
        };

        let record = ExecutionRecord {
            started_at: Utc::now(),
            completed_at: None,
            agent_id: p.agent_id,
            approved_by: p.approved_by,
            result,
            notes: p.notes.unwrap_or_default(),
            task_id: p.task_id,
        };

        match self.reminders.record_execution(&p.id, record) {
            Ok(r) => serde_json::to_string_pretty(&serde_json::json!({
                "id": r.id,
                "title": r.title,
                "status": format!("{:?}", r.status),
                "executions": r.executions.len(),
                "next_due": r.due_at.to_rfc3339(),
            })).unwrap_or_default(),
            Err(e) => format!("Error: {e}"),
        }
    }
}
