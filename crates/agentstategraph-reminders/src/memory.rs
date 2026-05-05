//! In-process memory store — for tests and lightweight consumers that
//! do not need durable reminder storage.

use std::collections::HashMap;
use std::sync::RwLock;

use crate::error::ReminderError;
use crate::store::ReminderStore;
use crate::types::{Reminder, ReminderFilter};

pub struct MemoryReminderStore {
    reminders: RwLock<HashMap<String, Reminder>>,
}

impl Default for MemoryReminderStore {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryReminderStore {
    pub fn new() -> Self {
        Self {
            reminders: RwLock::new(HashMap::new()),
        }
    }
}

impl ReminderStore for MemoryReminderStore {
    fn save(&self, reminder: &Reminder) -> Result<(), ReminderError> {
        self.reminders
            .write()
            .expect("MemoryReminderStore lock poisoned")
            .insert(reminder.id.clone(), reminder.clone());
        Ok(())
    }

    fn get(&self, id: &str) -> Result<Option<Reminder>, ReminderError> {
        Ok(self
            .reminders
            .read()
            .expect("MemoryReminderStore lock poisoned")
            .get(id)
            .cloned())
    }

    fn update(&self, reminder: &Reminder) -> Result<(), ReminderError> {
        let mut map = self
            .reminders
            .write()
            .expect("MemoryReminderStore lock poisoned");
        if map.contains_key(&reminder.id) {
            map.insert(reminder.id.clone(), reminder.clone());
            Ok(())
        } else {
            Err(ReminderError::NotFound(reminder.id.clone()))
        }
    }

    fn delete(&self, id: &str) -> Result<bool, ReminderError> {
        Ok(self
            .reminders
            .write()
            .expect("MemoryReminderStore lock poisoned")
            .remove(id)
            .is_some())
    }

    fn list(&self, filter: &ReminderFilter) -> Result<Vec<Reminder>, ReminderError> {
        let map = self
            .reminders
            .read()
            .expect("MemoryReminderStore lock poisoned");
        let mut results: Vec<Reminder> = map
            .values()
            .filter(|r| filter.matches(r))
            .cloned()
            .collect();
        // Sort by priority (ascending = most urgent first), then due_at ascending.
        results.sort_by(|a, b| a.priority.cmp(&b.priority).then(a.due_at.cmp(&b.due_at)));
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CreateReminder, Priority, ReminderStatus};
    use chrono::{Duration, Utc};

    fn make(title: &str, priority: Priority, due_secs: i64) -> Reminder {
        CreateReminder::new(
            title,
            "instructions",
            Utc::now() + Duration::seconds(due_secs),
            "agent/test",
        )
        .with_priority(priority)
        .into_reminder()
    }

    #[test]
    fn save_and_get_roundtrip() {
        let store = MemoryReminderStore::new();
        let r = make("test", Priority::Medium, 60);
        store.save(&r).unwrap();
        let fetched = store.get(&r.id).unwrap().unwrap();
        assert_eq!(fetched.id, r.id);
        assert_eq!(fetched.title, "test");
    }

    #[test]
    fn get_nonexistent_returns_none() {
        let store = MemoryReminderStore::new();
        assert!(store.get("no-such-id").unwrap().is_none());
    }

    #[test]
    fn update_mutates_record() {
        let store = MemoryReminderStore::new();
        let mut r = make("original", Priority::Low, 60);
        store.save(&r).unwrap();
        r.title = "updated".into();
        store.update(&r).unwrap();
        assert_eq!(store.get(&r.id).unwrap().unwrap().title, "updated");
    }

    #[test]
    fn update_nonexistent_returns_not_found() {
        let store = MemoryReminderStore::new();
        let r = make("x", Priority::Low, 60);
        assert!(matches!(store.update(&r), Err(ReminderError::NotFound(_))));
    }

    #[test]
    fn delete_returns_true_on_success_false_when_missing() {
        let store = MemoryReminderStore::new();
        let r = make("x", Priority::Low, 60);
        store.save(&r).unwrap();
        assert!(store.delete(&r.id).unwrap());
        assert!(!store.delete(&r.id).unwrap());
    }

    #[test]
    fn list_ordered_by_priority_then_due_at() {
        let store = MemoryReminderStore::new();
        let a = make("low-later", Priority::Low, 120);
        let b = make("high-later", Priority::High, 120);
        let c = make("high-sooner", Priority::High, 60);
        for r in [&a, &b, &c] {
            store.save(r).unwrap();
        }

        let results = store.list(&ReminderFilter::default()).unwrap();
        assert_eq!(results[0].title, "high-sooner");
        assert_eq!(results[1].title, "high-later");
        assert_eq!(results[2].title, "low-later");
    }

    #[test]
    fn list_filters_by_status() {
        let store = MemoryReminderStore::new();
        let mut r1 = make("due", Priority::Medium, -10);
        r1.status = ReminderStatus::Due;
        let r2 = make("pending", Priority::Medium, 60);
        store.save(&r1).unwrap();
        store.save(&r2).unwrap();

        let results = store
            .list(&ReminderFilter {
                status: Some(ReminderStatus::Due),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "due");
    }
}
