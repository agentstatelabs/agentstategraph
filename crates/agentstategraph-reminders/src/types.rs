//! Core data types for the reminders substrate.

use chrono::{DateTime, Datelike, NaiveTime, Utc, Weekday};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Priority
// ---------------------------------------------------------------------------

/// Reminder urgency. Lower number = higher urgency (1 is most urgent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Critical = 1,
    High = 2,
    Medium = 3,
    Low = 4,
    Minimal = 5,
}

impl Default for Priority {
    fn default() -> Self {
        Self::Medium
    }
}

impl Priority {
    pub fn as_u8(self) -> u8 {
        self as u8
    }
}

// ---------------------------------------------------------------------------
// ReminderStatus
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReminderStatus {
    /// Scheduled but not yet due.
    Pending,
    /// `due_at` has passed; ready to be acted on.
    Due,
    /// Not autonomous — waiting for explicit user approval before execution.
    AwaitingPermission,
    /// Agent has begun working on this reminder.
    InProgress,
    /// Successfully executed (or acknowledged).
    Completed,
    /// Deferred to a later time via `snooze()`.
    Snoozed,
    /// Explicitly cancelled; will never fire again.
    Cancelled,
}

impl ReminderStatus {
    /// Returns `true` if the reminder is in a terminal state.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled)
    }

    /// Returns `true` if the reminder is actionable right now.
    pub fn is_actionable(self) -> bool {
        matches!(
            self,
            Self::Due | Self::AwaitingPermission | Self::InProgress
        )
    }
}

// ---------------------------------------------------------------------------
// Schedule
// ---------------------------------------------------------------------------

/// Recurrence schedule for a repeating reminder.
///
/// When `record_execution` completes, the manager computes the next `due_at`
/// from the schedule and resets status to `Pending`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Schedule {
    /// Fire once (no recurrence). This is the default when no schedule is set.
    Once,
    /// Repeat every N seconds after each execution.
    Interval { every_seconds: u64 },
    /// Repeat daily at a fixed UTC time.
    Daily { time: NaiveTime },
    /// Repeat weekly on a specific day at a fixed UTC time.
    Weekly { day: Weekday, time: NaiveTime },
}

impl Schedule {
    /// Compute the next due time given the time the current execution
    /// completed. Returns `None` for `Once` (no recurrence).
    pub fn next_due(&self, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
        use chrono::Duration;
        match self {
            Schedule::Once => None,
            Schedule::Interval { every_seconds } => {
                Some(after + Duration::seconds(*every_seconds as i64))
            }
            Schedule::Daily { time } => {
                let candidate = after.date_naive().and_time(*time).and_utc();
                Some(if candidate > after {
                    candidate
                } else {
                    (after.date_naive() + chrono::Days::new(1))
                        .and_time(*time)
                        .and_utc()
                })
            }
            Schedule::Weekly { day, time } => {
                // Walk forward day-by-day until we hit the target weekday.
                let mut date = after.date_naive() + chrono::Days::new(1);
                for _ in 0..7 {
                    if date.weekday() == *day {
                        let candidate = date.and_time(*time).and_utc();
                        return Some(candidate);
                    }
                    date = date + chrono::Days::new(1);
                }
                None // unreachable
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Soft references
// ---------------------------------------------------------------------------

/// The kind of object a `ReminderRef` points to.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RefKind {
    /// A git-style branch in the stategraph.
    Branch,
    /// A memory entry (key in the memories system).
    Memory,
    /// A plan record.
    Plan,
    /// A task record.
    Task,
    /// Any `/_path` in the stategraph state tree.
    StatePath,
    /// An external resource (URL, file path, etc.).
    External { scheme: String },
}

/// A soft, advisory reference to another object.
///
/// Refs are contextual metadata — a stale ref does not invalidate the
/// reminder. The `label` is captured at creation so the agent retains
/// a human-readable hint even if the underlying object is renamed or
/// deleted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReminderRef {
    pub kind: RefKind,
    /// Stable identifier (branch name, memory key, task ID, path, URL…).
    pub id: String,
    /// Human-readable label captured at creation time. Survives renames.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Set lazily when the referenced object can no longer be resolved.
    #[serde(default)]
    pub stale: bool,
}

impl ReminderRef {
    pub fn branch(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            kind: RefKind::Branch,
            id: id.into(),
            label: Some(label.into()),
            stale: false,
        }
    }

    pub fn memory(id: impl Into<String>) -> Self {
        Self {
            kind: RefKind::Memory,
            id: id.into(),
            label: None,
            stale: false,
        }
    }

    pub fn plan(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            kind: RefKind::Plan,
            id: id.into(),
            label: Some(label.into()),
            stale: false,
        }
    }

    pub fn task(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            kind: RefKind::Task,
            id: id.into(),
            label: Some(label.into()),
            stale: false,
        }
    }

    pub fn state_path(path: impl Into<String>) -> Self {
        Self {
            kind: RefKind::StatePath,
            id: path.into(),
            label: None,
            stale: false,
        }
    }

    pub fn external(url: impl Into<String>, scheme: impl Into<String>) -> Self {
        Self {
            kind: RefKind::External {
                scheme: scheme.into(),
            },
            id: url.into(),
            label: None,
            stale: false,
        }
    }
}

// ---------------------------------------------------------------------------
// ExecutionRecord
// ---------------------------------------------------------------------------

/// Outcome of a single reminder execution attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionResult {
    /// Reminder was fully acted on.
    Success,
    /// Execution failed; see `notes`.
    Failed,
    /// Agent decided to defer without a specific new time.
    Deferred,
    /// Agent snoozed during execution (new `due_at` was set).
    Snoozed,
    /// Reminder was cancelled during execution.
    Cancelled,
}

/// Audit record of one execution of a reminder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionRecord {
    pub started_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
    pub agent_id: String,
    /// Set when `autonomous: false` and user approved execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_by: Option<String>,
    pub result: ExecutionResult,
    #[serde(default)]
    pub notes: Vec<String>,
    /// Task ID created for this execution, if the agent created one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Reminder
// ---------------------------------------------------------------------------

/// A scheduled reminder for an agent or user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reminder {
    pub id: String,
    pub title: String,
    /// Full instructions for the agent at execution time.
    pub instructions: String,
    /// Optional specific commands/tool calls to run (agent interprets these).
    #[serde(default)]
    pub commands: Vec<String>,
    /// Soft references to contextual objects (branches, memories, plans, tasks).
    #[serde(default)]
    pub refs: Vec<ReminderRef>,
    pub priority: Priority,
    pub due_at: DateTime<Utc>,
    /// Recurrence schedule. `None` means fire once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<Schedule>,
    /// `true`: agent may execute without asking the user first.
    /// `false`: agent must surface to user and call `approve()` before acting.
    pub autonomous: bool,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub status: ReminderStatus,
    /// Set when the reminder was snoozed; the original `due_at` is preserved
    /// as the first entry in `executions` if snooze happened mid-execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snoozed_until: Option<DateTime<Utc>>,
    /// Full audit trail of every execution attempt.
    #[serde(default)]
    pub executions: Vec<ExecutionRecord>,
    #[serde(default)]
    pub tags: Vec<String>,
}

// ---------------------------------------------------------------------------
// Input / filter types
// ---------------------------------------------------------------------------

/// Input for creating a new reminder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateReminder {
    pub title: String,
    pub instructions: String,
    #[serde(default)]
    pub commands: Vec<String>,
    #[serde(default)]
    pub refs: Vec<ReminderRef>,
    #[serde(default)]
    pub priority: Priority,
    pub due_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<Schedule>,
    #[serde(default = "default_true")]
    pub autonomous: bool,
    pub created_by: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

fn default_true() -> bool {
    true
}

impl CreateReminder {
    pub fn new(
        title: impl Into<String>,
        instructions: impl Into<String>,
        due_at: DateTime<Utc>,
        created_by: impl Into<String>,
    ) -> Self {
        Self {
            title: title.into(),
            instructions: instructions.into(),
            commands: Vec::new(),
            refs: Vec::new(),
            priority: Priority::default(),
            due_at,
            schedule: None,
            autonomous: true,
            created_by: created_by.into(),
            tags: Vec::new(),
        }
    }

    pub fn with_priority(mut self, p: Priority) -> Self {
        self.priority = p;
        self
    }
    pub fn with_schedule(mut self, s: Schedule) -> Self {
        self.schedule = Some(s);
        self
    }
    pub fn with_autonomous(mut self, a: bool) -> Self {
        self.autonomous = a;
        self
    }
    pub fn with_commands(mut self, cmds: Vec<String>) -> Self {
        self.commands = cmds;
        self
    }
    pub fn with_refs(mut self, refs: Vec<ReminderRef>) -> Self {
        self.refs = refs;
        self
    }
    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }

    /// Build a `Reminder` from this input, generating a new ID.
    pub fn into_reminder(self) -> Reminder {
        let now = Utc::now();
        Reminder {
            id: Uuid::new_v4().to_string(),
            title: self.title,
            instructions: self.instructions,
            commands: self.commands,
            refs: self.refs,
            priority: self.priority,
            due_at: self.due_at,
            schedule: self.schedule,
            autonomous: self.autonomous,
            created_by: self.created_by,
            created_at: now,
            status: ReminderStatus::Pending,
            snoozed_until: None,
            executions: Vec::new(),
            tags: self.tags,
        }
    }
}

/// Filters for `ReminderStore::list`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReminderFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ReminderStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority_at_most: Option<Priority>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    /// Return only reminders due at or before this time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_before: Option<DateTime<Utc>>,
    /// Return only reminders that have a ref with this `id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

impl ReminderFilter {
    pub fn matches(&self, r: &Reminder) -> bool {
        if let Some(s) = self.status {
            if r.status != s {
                return false;
            }
        }
        if let Some(p) = self.priority_at_most {
            if r.priority > p {
                return false;
            }
        }
        if let Some(ref cb) = self.created_by {
            if &r.created_by != cb {
                return false;
            }
        }
        if let Some(due) = self.due_before {
            if r.due_at > due {
                return false;
            }
        }
        if let Some(ref rid) = self.ref_id {
            if !r.refs.iter().any(|rf| &rf.id == rid) {
                return false;
            }
        }
        for tag in &self.tags {
            if !r.tags.contains(tag) {
                return false;
            }
        }
        true
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn due_in(secs: i64) -> DateTime<Utc> {
        Utc::now() + Duration::seconds(secs)
    }

    fn past(secs: i64) -> DateTime<Utc> {
        Utc::now() - Duration::seconds(secs)
    }

    // --- Priority ---

    #[test]
    fn priority_ordering() {
        assert!(Priority::Critical < Priority::High);
        assert!(Priority::High < Priority::Medium);
        assert!(Priority::Medium < Priority::Low);
        assert!(Priority::Low < Priority::Minimal);
    }

    #[test]
    fn priority_default_is_medium() {
        assert_eq!(Priority::default(), Priority::Medium);
    }

    #[test]
    fn priority_roundtrips_json() {
        for p in [
            Priority::Critical,
            Priority::High,
            Priority::Medium,
            Priority::Low,
            Priority::Minimal,
        ] {
            let j = serde_json::to_value(p).unwrap();
            let back: Priority = serde_json::from_value(j).unwrap();
            assert_eq!(p, back);
        }
    }

    // --- ReminderStatus ---

    #[test]
    fn terminal_statuses() {
        assert!(ReminderStatus::Completed.is_terminal());
        assert!(ReminderStatus::Cancelled.is_terminal());
        assert!(!ReminderStatus::Pending.is_terminal());
        assert!(!ReminderStatus::Due.is_terminal());
    }

    #[test]
    fn actionable_statuses() {
        assert!(ReminderStatus::Due.is_actionable());
        assert!(ReminderStatus::AwaitingPermission.is_actionable());
        assert!(ReminderStatus::InProgress.is_actionable());
        assert!(!ReminderStatus::Pending.is_actionable());
        assert!(!ReminderStatus::Snoozed.is_actionable());
    }

    // --- Schedule::next_due ---

    #[test]
    fn schedule_once_returns_none() {
        let now = Utc::now();
        assert!(Schedule::Once.next_due(now).is_none());
    }

    #[test]
    fn schedule_interval_advances_by_given_seconds() {
        let now = Utc::now();
        let next = Schedule::Interval {
            every_seconds: 3600,
        }
        .next_due(now)
        .unwrap();
        let diff = (next - now).num_seconds();
        assert_eq!(diff, 3600);
    }

    #[test]
    fn schedule_daily_advances_to_next_occurrence() {
        let time = NaiveTime::from_hms_opt(9, 0, 0).unwrap();
        let now = Utc::now();
        let next = Schedule::Daily { time }.next_due(now).unwrap();
        assert!(next > now);
        assert!(next <= now + Duration::days(2));
        assert_eq!(next.time(), time);
    }

    #[test]
    fn schedule_weekly_lands_on_correct_day() {
        let time = NaiveTime::from_hms_opt(10, 0, 0).unwrap();
        let now = Utc::now();
        let next = Schedule::Weekly {
            day: Weekday::Mon,
            time,
        }
        .next_due(now)
        .unwrap();
        assert!(next > now);
        assert_eq!(next.weekday(), Weekday::Mon);
    }

    // --- ReminderRef builders ---

    #[test]
    fn ref_builders_set_kind_and_stale_false() {
        let b = ReminderRef::branch("feature/x", "Feature X cleanup");
        assert_eq!(b.kind, RefKind::Branch);
        assert!(!b.stale);
        assert_eq!(b.label.as_deref(), Some("Feature X cleanup"));

        let t = ReminderRef::task("t-042", "Deploy task");
        assert_eq!(t.kind, RefKind::Task);
        assert!(!t.stale);

        let sp = ReminderRef::state_path("/_sessions/dev-server");
        assert_eq!(sp.kind, RefKind::StatePath);
        assert!(sp.label.is_none());
    }

    #[test]
    fn ref_stale_can_be_set() {
        let mut r = ReminderRef::memory("mem-001");
        assert!(!r.stale);
        r.stale = true;
        assert!(r.stale);
    }

    #[test]
    fn ref_roundtrips_json() {
        let r = ReminderRef::branch("main", "main branch");
        let j = serde_json::to_value(&r).unwrap();
        let back: ReminderRef = serde_json::from_value(j).unwrap();
        assert_eq!(r, back);
    }

    // --- CreateReminder / into_reminder ---

    #[test]
    fn create_reminder_builder_defaults() {
        let cr = CreateReminder::new("title", "do the thing", due_in(3600), "agent/test");
        assert!(cr.autonomous);
        assert_eq!(cr.priority, Priority::Medium);
        assert!(cr.schedule.is_none());
        assert!(cr.refs.is_empty());
        assert!(cr.tags.is_empty());
    }

    #[test]
    fn into_reminder_generates_id_and_pending_status() {
        let due = due_in(3600);
        let r = CreateReminder::new("test", "instructions", due, "agent/a").into_reminder();
        assert!(!r.id.is_empty());
        assert_eq!(r.status, ReminderStatus::Pending);
        assert_eq!(r.due_at, due);
        assert!(r.executions.is_empty());
    }

    #[test]
    fn create_reminder_builder_chain() {
        let due = due_in(100);
        let r = CreateReminder::new("t", "i", due, "a")
            .with_priority(Priority::Critical)
            .with_autonomous(false)
            .with_tags(vec!["cleanup".into()])
            .with_schedule(Schedule::Interval {
                every_seconds: 86400,
            })
            .with_refs(vec![ReminderRef::branch("feat/x", "Feature X")])
            .into_reminder();

        assert_eq!(r.priority, Priority::Critical);
        assert!(!r.autonomous);
        assert_eq!(r.tags, vec!["cleanup"]);
        assert!(matches!(r.schedule, Some(Schedule::Interval { .. })));
        assert_eq!(r.refs.len(), 1);
    }

    // --- ReminderFilter::matches ---

    fn make_reminder(
        status: ReminderStatus,
        priority: Priority,
        due_at: DateTime<Utc>,
    ) -> Reminder {
        let mut r = CreateReminder::new("t", "i", due_at, "agent/a")
            .with_priority(priority)
            .into_reminder();
        r.status = status;
        r
    }

    #[test]
    fn filter_by_status() {
        let r = make_reminder(ReminderStatus::Due, Priority::Medium, past(1));
        assert!(
            ReminderFilter {
                status: Some(ReminderStatus::Due),
                ..Default::default()
            }
            .matches(&r)
        );
        assert!(
            !ReminderFilter {
                status: Some(ReminderStatus::Pending),
                ..Default::default()
            }
            .matches(&r)
        );
    }

    #[test]
    fn filter_by_priority_at_most() {
        let r = make_reminder(ReminderStatus::Due, Priority::High, past(1));
        // High (2) <= Medium (3) → matches
        assert!(
            ReminderFilter {
                priority_at_most: Some(Priority::Medium),
                ..Default::default()
            }
            .matches(&r)
        );
        // High (2) > Critical (1) → no match
        assert!(
            !ReminderFilter {
                priority_at_most: Some(Priority::Critical),
                ..Default::default()
            }
            .matches(&r)
        );
    }

    #[test]
    fn filter_by_due_before() {
        let now = Utc::now();
        let r = make_reminder(
            ReminderStatus::Pending,
            Priority::Medium,
            now + Duration::hours(2),
        );
        assert!(
            !ReminderFilter {
                due_before: Some(now + Duration::hours(1)),
                ..Default::default()
            }
            .matches(&r)
        );
        assert!(
            ReminderFilter {
                due_before: Some(now + Duration::hours(3)),
                ..Default::default()
            }
            .matches(&r)
        );
    }

    #[test]
    fn filter_by_ref_id() {
        let mut r = make_reminder(ReminderStatus::Pending, Priority::Medium, due_in(100));
        r.refs
            .push(ReminderRef::branch("feature/web-demo", "web demo"));
        assert!(
            ReminderFilter {
                ref_id: Some("feature/web-demo".into()),
                ..Default::default()
            }
            .matches(&r)
        );
        assert!(
            !ReminderFilter {
                ref_id: Some("other-branch".into()),
                ..Default::default()
            }
            .matches(&r)
        );
    }

    #[test]
    fn filter_by_tags_all_must_match() {
        let mut r = make_reminder(ReminderStatus::Pending, Priority::Medium, due_in(100));
        r.tags = vec!["cleanup".into(), "infra".into()];
        assert!(
            ReminderFilter {
                tags: vec!["cleanup".into()],
                ..Default::default()
            }
            .matches(&r)
        );
        assert!(
            ReminderFilter {
                tags: vec!["cleanup".into(), "infra".into()],
                ..Default::default()
            }
            .matches(&r)
        );
        assert!(
            !ReminderFilter {
                tags: vec!["cleanup".into(), "missing".into()],
                ..Default::default()
            }
            .matches(&r)
        );
    }

    #[test]
    fn empty_filter_matches_everything() {
        let r = make_reminder(ReminderStatus::Due, Priority::Critical, past(1));
        assert!(ReminderFilter::default().matches(&r));
    }
}
