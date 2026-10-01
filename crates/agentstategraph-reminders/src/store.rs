//! `ReminderStore` trait — durable storage contract for reminders.
//!
//! All methods have default no-op implementations that return
//! `ReminderError::Store("reminder storage not supported")`.
//! Backends that do not need reminder support satisfy the trait without
//! extra boilerplate; backends that do support reminders override every
//! method.

use crate::error::ReminderError;
use crate::types::{Reminder, ReminderFilter};

/// Whether two reminder records are identical, field for field. `Reminder`
/// has no `PartialEq`; serialized forms compare every field, and a record read
/// back from a store serializes exactly as it was written.
pub fn same_reminder(a: &Reminder, b: &Reminder) -> bool {
    match (serde_json::to_value(a), serde_json::to_value(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

pub trait ReminderStore: Send + Sync {
    /// Persist a new reminder. The `id` field is already populated by
    /// `CreateReminder::into_reminder()`.
    fn save(&self, _reminder: &Reminder) -> Result<(), ReminderError> {
        Err(ReminderError::Store(
            "reminder storage not supported".into(),
        ))
    }

    /// Retrieve a reminder by ID.
    fn get(&self, _id: &str) -> Result<Option<Reminder>, ReminderError> {
        Err(ReminderError::Store(
            "reminder storage not supported".into(),
        ))
    }

    /// Overwrite the full reminder record (used after in-place mutations).
    fn update(&self, _reminder: &Reminder) -> Result<(), ReminderError> {
        Err(ReminderError::Store(
            "reminder storage not supported".into(),
        ))
    }

    /// Write `new` only if the stored reminder still equals `expected` (what
    /// the caller read). Returns `Ok(false)` when it changed in between; the
    /// caller re-reads and re-applies. Every lifecycle method is a
    /// read-modify-write of the whole record, and an unconditional `update`
    /// silently reverted a concurrent change (a cancel, snooze, or execution
    /// record) while both calls returned Ok.
    ///
    /// This default is NOT atomic (get, compare, update) — backends that can
    /// do better override it, as the SQLite and in-memory stores do.
    fn update_if_unchanged(
        &self,
        expected: &Reminder,
        new: &Reminder,
    ) -> Result<bool, ReminderError> {
        match self.get(&expected.id)? {
            Some(current) if same_reminder(&current, expected) => {
                self.update(new)?;
                Ok(true)
            }
            Some(_) => Ok(false),
            None => Err(ReminderError::NotFound(expected.id.clone())),
        }
    }

    /// Delete a reminder permanently (hard delete, for admin use only;
    /// normal cancellation uses `update` with `status: Cancelled`).
    fn delete(&self, _id: &str) -> Result<bool, ReminderError> {
        Err(ReminderError::Store(
            "reminder storage not supported".into(),
        ))
    }

    /// Return all reminders matching `filter`, ordered by priority then
    /// `due_at` ascending.
    fn list(&self, _filter: &ReminderFilter) -> Result<Vec<Reminder>, ReminderError> {
        Err(ReminderError::Store(
            "reminder storage not supported".into(),
        ))
    }
}
