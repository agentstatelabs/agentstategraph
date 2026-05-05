//! `ReminderStore` trait — durable storage contract for reminders.
//!
//! All methods have default no-op implementations that return
//! `ReminderError::Store("reminder storage not supported")`.
//! Backends that do not need reminder support satisfy the trait without
//! extra boilerplate; backends that do support reminders override every
//! method.

use crate::error::ReminderError;
use crate::types::{Reminder, ReminderFilter};

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
