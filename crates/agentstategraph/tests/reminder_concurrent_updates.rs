//! Reminder lifecycle methods are read-modify-writes of a whole record. Two
//! concurrent updates to one reminder must both survive (data-loss audit,
//! 2026-10). Runs over a SQLite-backed `Repository`, as consumers use it.

use std::sync::{Arc, Barrier};
use std::thread;

use agentstategraph::Repository;
use agentstategraph_reminders::{
    CreateReminder, RefKind, ReminderManager, ReminderRef, ReminderStatus,
};
use agentstategraph_storage::SqliteStorage;
use chrono::{Duration, Utc};

#[test]
fn concurrent_snooze_and_mark_ref_stale_both_survive() {
    let path = std::env::temp_dir().join(format!(
        "asg-reminder-race-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let repo = Arc::new(Repository::new(Box::new(
        SqliteStorage::open(&path).unwrap(),
    )));
    repo.init().unwrap();
    let mgr = Arc::new(ReminderManager::new(repo));

    let mut lost = Vec::new();
    for i in 0..100 {
        let r = mgr
            .create(CreateReminder {
                title: format!("r{i}"),
                instructions: "check the branch".into(),
                commands: vec![],
                refs: vec![ReminderRef {
                    kind: RefKind::Branch,
                    id: "feature".into(),
                    label: None,
                    stale: false,
                }],
                priority: Default::default(),
                due_at: Utc::now() + Duration::hours(1),
                schedule: None,
                autonomous: true,
                created_by: "tester".into(),
                tags: vec![],
            })
            .unwrap();

        let barrier = Arc::new(Barrier::new(2));
        let (m1, b1, id1) = (Arc::clone(&mgr), Arc::clone(&barrier), r.id.clone());
        let snoozer = thread::spawn(move || {
            b1.wait();
            m1.snooze(&id1, Utc::now() + Duration::hours(2)).map(|_| ())
        });
        let (m2, b2, id2) = (Arc::clone(&mgr), Arc::clone(&barrier), r.id.clone());
        let marker = thread::spawn(move || {
            b2.wait();
            m2.mark_ref_stale(&id2, "feature").map(|_| ())
        });
        snoozer.join().unwrap().expect("snooze failed");
        marker.join().unwrap().expect("mark_ref_stale failed");

        let after = mgr.get(&r.id).unwrap();
        if after.status != ReminderStatus::Snoozed || !after.refs[0].stale {
            lost.push(format!(
                "r{i}: status={:?} stale={}",
                after.status, after.refs[0].stale
            ));
        }
    }
    assert!(
        lost.is_empty(),
        "{} of 100 lost an update: {lost:?}",
        lost.len()
    );
}

/// A store that, the first time a write arrives, lets a competing writer
/// change the same reminder first — the interleaving a race produces only
/// occasionally, made certain.
struct InterleavingStore {
    inner: Arc<dyn agentstategraph_reminders::ReminderStore>,
    competing: std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl InterleavingStore {
    fn interleave(&self) {
        if let Some(f) = self.competing.lock().unwrap().take() {
            f();
        }
    }
}

impl agentstategraph_reminders::ReminderStore for InterleavingStore {
    fn save(
        &self,
        r: &agentstategraph_reminders::Reminder,
    ) -> Result<(), agentstategraph_reminders::ReminderError> {
        self.inner.save(r)
    }
    fn get(
        &self,
        id: &str,
    ) -> Result<Option<agentstategraph_reminders::Reminder>, agentstategraph_reminders::ReminderError>
    {
        self.inner.get(id)
    }
    fn update(
        &self,
        r: &agentstategraph_reminders::Reminder,
    ) -> Result<(), agentstategraph_reminders::ReminderError> {
        self.interleave();
        self.inner.update(r)
    }
    fn update_if_unchanged(
        &self,
        expected: &agentstategraph_reminders::Reminder,
        new: &agentstategraph_reminders::Reminder,
    ) -> Result<bool, agentstategraph_reminders::ReminderError> {
        self.interleave();
        self.inner.update_if_unchanged(expected, new)
    }
    fn delete(&self, id: &str) -> Result<bool, agentstategraph_reminders::ReminderError> {
        self.inner.delete(id)
    }
    fn list(
        &self,
        f: &agentstategraph_reminders::ReminderFilter,
    ) -> Result<Vec<agentstategraph_reminders::Reminder>, agentstategraph_reminders::ReminderError>
    {
        self.inner.list(f)
    }
}

#[test]
fn a_write_between_read_and_write_is_not_reverted() {
    let path = std::env::temp_dir().join(format!(
        "asg-reminder-interleave-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let repo = Arc::new(Repository::new(Box::new(
        SqliteStorage::open(&path).unwrap(),
    )));
    repo.init().unwrap();
    let direct = Arc::new(ReminderManager::new(repo.clone()));
    let r = direct
        .create(CreateReminder {
            title: "r".into(),
            instructions: "check the branch".into(),
            commands: vec![],
            refs: vec![ReminderRef {
                kind: RefKind::Branch,
                id: "feature".into(),
                label: None,
                stale: false,
            }],
            priority: Default::default(),
            due_at: Utc::now() + Duration::hours(1),
            schedule: None,
            autonomous: true,
            created_by: "tester".into(),
            tags: vec![],
        })
        .unwrap();

    let (other, id) = (Arc::clone(&direct), r.id.clone());
    let store = Arc::new(InterleavingStore {
        inner: repo,
        competing: std::sync::Mutex::new(Some(Box::new(move || {
            other.mark_ref_stale(&id, "feature").unwrap();
        }))),
    });
    // Snooze reads the reminder; the competing write lands before its write.
    ReminderManager::new(store)
        .snooze(&r.id, Utc::now() + Duration::hours(2))
        .unwrap();

    let after = direct.get(&r.id).unwrap();
    assert_eq!(after.status, ReminderStatus::Snoozed);
    assert!(
        after.refs[0].stale,
        "the competing write (ref marked stale) was reverted by the snooze"
    );
}
