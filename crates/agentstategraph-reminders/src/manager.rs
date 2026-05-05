//! `ReminderManager` — business logic layer over a `ReminderStore`.
//!
//! All state transitions, schedule advancement, and permission checks
//! live here so the store layer stays a plain CRUD surface.

use std::sync::Arc;

use chrono::Utc;

use crate::error::ReminderError;
use crate::store::ReminderStore;
use crate::types::{
    CreateReminder, ExecutionRecord, ExecutionResult, Reminder, ReminderFilter, ReminderStatus,
};

pub struct ReminderManager {
    store: Arc<dyn ReminderStore>,
}

impl ReminderManager {
    pub fn new(store: Arc<dyn ReminderStore>) -> Self {
        Self { store }
    }

    // -----------------------------------------------------------------------
    // Create / read
    // -----------------------------------------------------------------------

    /// Create and persist a new reminder. Returns the fully-populated record.
    pub fn create(&self, input: CreateReminder) -> Result<Reminder, ReminderError> {
        let reminder = input.into_reminder();
        self.store.save(&reminder)?;
        Ok(reminder)
    }

    /// Retrieve a reminder by ID.
    pub fn get(&self, id: &str) -> Result<Reminder, ReminderError> {
        self.store
            .get(id)?
            .ok_or_else(|| ReminderError::NotFound(id.to_string()))
    }

    // -----------------------------------------------------------------------
    // List / remind_me
    // -----------------------------------------------------------------------

    /// Return all reminders matching `filter`.
    pub fn list(&self, filter: &ReminderFilter) -> Result<Vec<Reminder>, ReminderError> {
        self.store.list(filter)
    }

    /// Return all reminders that are currently actionable: status `Due` or
    /// `AwaitingPermission`, ordered by priority then `due_at`.
    ///
    /// Also promotes `Pending` reminders whose `due_at <= now` to `Due`
    /// (lazy promotion — avoids the need for a background timer).
    pub fn remind_me(&self) -> Result<Vec<Reminder>, ReminderError> {
        let now = Utc::now();

        // Promote any pending reminders whose due_at has passed.
        let pending = self.store.list(&ReminderFilter {
            status: Some(ReminderStatus::Pending),
            ..Default::default()
        })?;
        for mut r in pending {
            if r.due_at <= now {
                r.status = ReminderStatus::Due;
                self.store.update(&r)?;
            }
        }

        // Also promote snoozed reminders whose snooze has expired.
        let snoozed = self.store.list(&ReminderFilter {
            status: Some(ReminderStatus::Snoozed),
            ..Default::default()
        })?;
        for mut r in snoozed {
            let wake = r.snoozed_until.unwrap_or(r.due_at);
            if wake <= now {
                r.status = ReminderStatus::Due;
                r.snoozed_until = None;
                self.store.update(&r)?;
            }
        }

        // Now return all actionable reminders.
        let mut due = self.store.list(&ReminderFilter {
            status: Some(ReminderStatus::Due),
            ..Default::default()
        })?;
        let mut awaiting = self.store.list(&ReminderFilter {
            status: Some(ReminderStatus::AwaitingPermission),
            ..Default::default()
        })?;
        due.append(&mut awaiting);
        due.sort_by(|a, b| a.priority.cmp(&b.priority).then(a.due_at.cmp(&b.due_at)));
        Ok(due)
    }

    // -----------------------------------------------------------------------
    // Lifecycle mutations
    // -----------------------------------------------------------------------

    /// Snooze a reminder until `until`. Valid from `Due`, `AwaitingPermission`,
    /// or `InProgress` states.
    pub fn snooze(
        &self,
        id: &str,
        until: chrono::DateTime<Utc>,
    ) -> Result<Reminder, ReminderError> {
        let mut r = self.get(id)?;
        if r.status.is_terminal() {
            return Err(ReminderError::AlreadyTerminal(id.to_string(), r.status));
        }
        r.status = ReminderStatus::Snoozed;
        r.snoozed_until = Some(until);
        self.store.update(&r)?;
        Ok(r)
    }

    /// Cancel a reminder. Idempotent if already cancelled.
    pub fn cancel(&self, id: &str) -> Result<Reminder, ReminderError> {
        let mut r = self.get(id)?;
        if r.status == ReminderStatus::Completed {
            return Err(ReminderError::AlreadyTerminal(id.to_string(), r.status));
        }
        r.status = ReminderStatus::Cancelled;
        self.store.update(&r)?;
        Ok(r)
    }

    /// Approve a non-autonomous reminder for execution, recording who approved.
    /// Transitions `AwaitingPermission` → `Due`.
    pub fn approve(&self, id: &str, approved_by: &str) -> Result<Reminder, ReminderError> {
        let mut r = self.get(id)?;
        match r.status {
            ReminderStatus::AwaitingPermission => {}
            ReminderStatus::Due => {
                // Already approved / promoted; record the approver but don't error.
            }
            s if s.is_terminal() => {
                return Err(ReminderError::AlreadyTerminal(id.to_string(), s));
            }
            s => {
                return Err(ReminderError::InvalidTransition {
                    id: id.to_string(),
                    from: s,
                    to: ReminderStatus::Due,
                });
            }
        }
        // Stamp the most recent pending execution record with the approver,
        // or leave that to record_execution if none exists yet.
        r.status = ReminderStatus::Due;
        // Store the approved_by on the reminder directly so record_execution
        // can pick it up without requiring the caller to pass it again.
        // We use a sentinel in the first partial execution record if present.
        if let Some(rec) = r.executions.last_mut() {
            if rec.completed_at.is_none() {
                rec.approved_by = Some(approved_by.to_string());
            }
        }
        self.store.update(&r)?;
        Ok(r)
    }

    /// Mark a reminder as in-progress. Transitions `Due` → `InProgress`.
    /// Non-autonomous reminders must be approved first.
    pub fn start(&self, id: &str, agent_id: &str) -> Result<Reminder, ReminderError> {
        let mut r = self.get(id)?;
        match r.status {
            ReminderStatus::Due => {}
            ReminderStatus::AwaitingPermission => {
                if !r.autonomous {
                    return Err(ReminderError::RequiresApproval(id.to_string()));
                }
            }
            s if s.is_terminal() => {
                return Err(ReminderError::AlreadyTerminal(id.to_string(), s));
            }
            s => {
                return Err(ReminderError::InvalidTransition {
                    id: id.to_string(),
                    from: s,
                    to: ReminderStatus::InProgress,
                });
            }
        }
        // Open a partial execution record.
        r.executions.push(crate::types::ExecutionRecord {
            started_at: Utc::now(),
            completed_at: None,
            agent_id: agent_id.to_string(),
            approved_by: None,
            result: ExecutionResult::Deferred, // placeholder until record_execution
            notes: Vec::new(),
            task_id: None,
        });
        r.status = ReminderStatus::InProgress;
        self.store.update(&r)?;
        Ok(r)
    }

    /// Complete an execution attempt, appending the `ExecutionRecord`.
    ///
    /// On `Success`:
    /// - If the reminder has a repeating schedule, compute `next_due` and
    ///   reset to `Pending`.
    /// - Otherwise mark `Completed`.
    ///
    /// On non-autonomous reminders that have not yet been approved, this
    /// method transitions to `AwaitingPermission` instead of `Due` after
    /// schedule advancement.
    pub fn record_execution(
        &self,
        id: &str,
        mut record: ExecutionRecord,
    ) -> Result<Reminder, ReminderError> {
        let mut r = self.get(id)?;
        if r.status.is_terminal() {
            return Err(ReminderError::AlreadyTerminal(id.to_string(), r.status));
        }

        let completed_at = Utc::now();
        record.completed_at = Some(completed_at);

        // If start() opened a partial record, update it in-place; otherwise push.
        if let Some(last) = r.executions.last_mut() {
            if last.completed_at.is_none() {
                last.completed_at = record.completed_at;
                last.result = record.result.clone();
                last.notes = record.notes.clone();
                last.task_id = record.task_id.clone();
                if record.approved_by.is_some() {
                    last.approved_by = record.approved_by.clone();
                }
            } else {
                r.executions.push(record.clone());
            }
        } else {
            r.executions.push(record.clone());
        }

        match &record.result {
            ExecutionResult::Success => {
                match r.schedule.as_ref().and_then(|s| s.next_due(completed_at)) {
                    Some(next) => {
                        r.due_at = next;
                        r.snoozed_until = None;
                        r.status = if r.autonomous {
                            ReminderStatus::Pending
                        } else {
                            ReminderStatus::AwaitingPermission
                        };
                    }
                    None => {
                        r.status = ReminderStatus::Completed;
                    }
                }
            }
            ExecutionResult::Snoozed => {
                // Snooze was set separately via snooze(); just record.
                if r.status != ReminderStatus::Snoozed {
                    r.status = ReminderStatus::Snoozed;
                }
            }
            ExecutionResult::Cancelled => {
                r.status = ReminderStatus::Cancelled;
            }
            ExecutionResult::Failed | ExecutionResult::Deferred => {
                // Leave status as-is (InProgress or Due); caller decides next step.
                if r.status == ReminderStatus::InProgress {
                    r.status = ReminderStatus::Due;
                }
            }
        }

        self.store.update(&r)?;
        Ok(r)
    }

    /// Mark a ref as stale by `ref_id`. Returns the number of refs updated.
    pub fn mark_ref_stale(&self, id: &str, ref_id: &str) -> Result<usize, ReminderError> {
        let mut r = self.get(id)?;
        let mut count = 0;
        for rf in r.refs.iter_mut() {
            if rf.id == ref_id && !rf.stale {
                rf.stale = true;
                count += 1;
            }
        }
        if count > 0 {
            self.store.update(&r)?;
        }
        Ok(count)
    }
}
