//! The repository on the control ref (DESIGN.md Plan G t-001, part 2).
//!
//! Every taint, reminder and epoch change still goes to its storage table
//! first, which enforces the table's rules (one unresolved taint per path,
//! name and kind; a reminder changed only if it's still what the caller
//! read; a sealed epoch takes no more commits) and keeps the per-write taint
//! check index-backed. It's then committed to the control ref, so it can
//! sync. A write marks itself pending before it touches the table and clears
//! the mark once its control commit lands, so a write cut short between the
//! two is taken from the table at the next reconcile rather than lost.
//!
//! [`Repository::reconcile_control`] brings the two together: the control
//! ref takes the rows it lacks (rows from before it, and pending writes),
//! by its merge rules; then the tables take every record whose control
//! version they don't match. It runs when a repository opens, and must run
//! after anything else moves the control ref (a fetch). A repository opened
//! on tables that already match the control ref does nothing but read three
//! refs.
//!
//! The control ref and the refs beside it start with `_control`, and no
//! ordinary write may move them.

use std::collections::HashSet;

use agentstategraph_core::{
    Authority, CommitBuilder, Epoch, EpochStatus, Intent, IntentCategory, Namespace, ObjectId,
};
use agentstategraph_reminders::{ReminderFilter, same_reminder};
use agentstategraph_storage::{Storage, StorageError};
use agentstategraph_taint::Taint;
use chrono::Utc;

use crate::control::{self, CONTROL_REF, ControlEdit, ControlState, ReminderEntry};
use crate::repo::{CommitOptions, RepoError, Repository};

/// Refs whose names start with this belong to the control ref.
pub const CONTROL_PREFIX: &str = "_control";

/// The control commit the tables last matched.
const INDEXED_REF: &str = "_control-indexed";

/// A write whose control commit hasn't landed: `<prefix><kind>/<id>/<nonce>`.
const PENDING_PREFIX: &str = "_control-pending/";

/// How many times a control commit rebuilds on a moved head.
const MAX_ATTEMPTS: usize = 64;

/// Whether `name` is the control ref or one of the refs beside it.
pub fn is_reserved_ref(name: &str) -> bool {
    name.starts_with(CONTROL_PREFIX)
}

/// The kinds of record a write can leave pending.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Kind {
    Taint,
    Reminder,
    Epoch,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Taint => "taint",
            Kind::Reminder => "reminder",
            Kind::Epoch => "epoch",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "taint" => Some(Kind::Taint),
            "reminder" => Some(Kind::Reminder),
            "epoch" => Some(Kind::Epoch),
            _ => None,
        }
    }
}

/// What to commit to the control ref once the table write is done.
pub(crate) enum Stage<'a> {
    /// The record as the tables now hold it, or its deletion.
    Record(Kind, &'a str),
    /// One more commit in an epoch.
    Member { epoch: &'a str, commit: ObjectId },
}

/// What [`Repository::reconcile_control`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ControlReport {
    /// Whether the control ref moved: it took rows from the tables.
    pub committed: bool,
    /// Records the tables took from the control ref.
    pub rebuilt: usize,
    /// Records the tables couldn't take, with why: a second unresolved
    /// taint with the same path, name and kind, say, which two stores can
    /// each create.
    pub skipped: Vec<String>,
}

fn reminder_err(e: agentstategraph_reminders::ReminderError) -> RepoError {
    RepoError::Storage(StorageError::Backend(e.to_string()))
}

/// The options a control commit is made with, for changes with no caller
/// identity (reminders, epochs, reconciling).
pub(crate) fn system_options(category: IntentCategory, description: String) -> CommitOptions {
    CommitOptions::new("system", category, description)
}

impl Repository {
    fn control_store(&self) -> &dyn Storage {
        self.taint_storage()
    }

    /// Commit a change to the control ref: `stage` edits its current tree,
    /// under a compare-and-swap, rebuilt when another writer moved the ref
    /// first. A change that leaves the tree as it was commits nothing.
    /// Returns the head it built on and the head after.
    fn control_commit(
        &self,
        options: &CommitOptions,
        stage: &dyn Fn(&mut ControlEdit<'_>) -> Result<(), RepoError>,
    ) -> Result<(Option<ObjectId>, ObjectId), RepoError> {
        let store = self.control_store();
        let ns = Namespace::default_ns();
        for _ in 0..MAX_ATTEMPTS {
            let head = store.get_ref(&ns, CONTROL_REF)?;
            let base = match head {
                Some(h) => Some(
                    store
                        .get_commit(&h)?
                        .ok_or_else(|| RepoError::CommitNotFound(h.to_string()))?
                        .state_root,
                ),
                None => None,
            };
            let mut edit = match &base {
                Some(root) => ControlEdit::open(store, root)?,
                None => ControlEdit::new(store),
            };
            stage(&mut edit)?;
            let root = edit.finish()?;
            if let (Some(h), Some(b)) = (head, base)
                && b == root
            {
                return Ok((head, h));
            }
            let commit = CommitBuilder::new(
                root,
                options.agent_id.clone(),
                options.authority.clone(),
                options.intent.clone(),
            )
            .parents(head.into_iter().collect())
            .build();
            store.put_commit(&commit)?;
            let landed = match head {
                Some(h) => store.cas_ref(&ns, CONTROL_REF, h, commit.id)?,
                None => store.create_ref(&ns, CONTROL_REF, commit.id)?,
            };
            if landed {
                return Ok((head, commit.id));
            }
        }
        Err(RepoError::WriteConflict)
    }

    /// Put what `stage` names into `edit`, from the tables.
    fn stage_from_tables(
        &self,
        edit: &mut ControlEdit<'_>,
        stage: &Stage<'_>,
    ) -> Result<(), RepoError> {
        let store = self.control_store();
        match stage {
            Stage::Record(Kind::Taint, id) => {
                if let Some(t) = store.get_taint(id)? {
                    edit.put_taint(&t)?;
                }
            }
            Stage::Record(Kind::Reminder, id) => match store.get(id).map_err(reminder_err)? {
                Some(r) => edit.put_reminder(&r, Utc::now())?,
                None => edit.delete_reminder(id, Utc::now())?,
            },
            Stage::Record(Kind::Epoch, id) => {
                if let Some(e) = store.get_epoch(id)? {
                    edit.put_epoch(&e)?;
                    if !e.commits.is_empty() {
                        edit.add_members(id, e.commits.iter().copied())?;
                    }
                }
            }
            Stage::Member { epoch, commit } => edit.add_members(epoch, [*commit])?,
        }
        Ok(())
    }

    /// Run `table`, a write to a taint, reminder or epoch table, and then
    /// commit what `stage` names to the control ref. A pending mark covers
    /// the gap; if the control commit fails, the mark stays and the next
    /// reconcile takes the table's version. The table write's result is
    /// returned either way, since it's what the operation did.
    pub(crate) fn write_through<T, E>(
        &self,
        stage: Stage<'_>,
        options: CommitOptions,
        table: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E>
    where
        E: From<RepoError>,
    {
        let store = self.control_store();
        let ns = Namespace::default_ns();
        let (kind, id) = match &stage {
            Stage::Record(kind, id) => (*kind, *id),
            Stage::Member { epoch, .. } => (Kind::Epoch, *epoch),
        };
        let mark = format!(
            "{PENDING_PREFIX}{}/{id}/{}",
            kind.as_str(),
            uuid::Uuid::new_v4().simple()
        );
        let marked = ObjectId::hash(mark.as_bytes());
        store
            .set_ref(&ns, &mark, marked)
            .map_err(|e| E::from(RepoError::from(e)))?;
        let out = match table() {
            Ok(out) => out,
            Err(e) => {
                // Nothing changed, so nothing is pending.
                let _ = store.cas_delete_ref(&ns, &mark, marked);
                return Err(e);
            }
        };
        match self.control_commit(&options, &|edit| self.stage_from_tables(edit, &stage)) {
            Ok((before, after)) => {
                // If the tables matched the head this built on, they match
                // the new one: this write changed both the same way.
                if let Some(before) = before
                    && before != after
                {
                    let _ = store.cas_ref(&ns, INDEXED_REF, before, after);
                }
                let _ = store.cas_delete_ref(&ns, &mark, marked);
            }
            Err(e) => tracing::warn!(
                error.type = e.code(),
                "couldn't commit {} {id} to the control ref; the next reconcile will: {e}",
                kind.as_str()
            ),
        }
        Ok(out)
    }

    /// Bring the control ref and the taint, reminder and epoch tables
    /// together, as the [module docs](crate::control_repo) describe.
    pub fn reconcile_control(&self) -> Result<ControlReport, RepoError> {
        for _ in 0..MAX_ATTEMPTS {
            if let Some(report) = self.try_reconcile_control()? {
                return Ok(report);
            }
        }
        Err(RepoError::WriteConflict)
    }

    /// `None` when another writer moved the control ref first.
    fn try_reconcile_control(&self) -> Result<Option<ControlReport>, RepoError> {
        let store = self.control_store();
        let ns = Namespace::default_ns();
        let head = store.get_ref(&ns, CONTROL_REF)?;
        let indexed = store.get_ref(&ns, INDEXED_REF)?;
        let pending = store.list_refs(&ns, PENDING_PREFIX)?;
        if head.is_some() && head == indexed && pending.is_empty() {
            return Ok(Some(ControlReport::default()));
        }
        let pending_records: HashSet<(Kind, String)> = pending
            .iter()
            .filter_map(|(name, _)| {
                let rest = name.strip_prefix(PENDING_PREFIX)?;
                let (kind, rest) = rest.split_once('/')?;
                let (id, _nonce) = rest.rsplit_once('/')?;
                Some((Kind::parse(kind)?, id.to_string()))
            })
            .collect();

        let base = match head {
            Some(h) => Some(
                store
                    .get_commit(&h)?
                    .ok_or_else(|| RepoError::CommitNotFound(h.to_string()))?
                    .state_root,
            ),
            None => None,
        };
        let control = match &base {
            Some(root) => control::read(store, root)?,
            None => ControlState::default(),
        };
        let taints = store.list_taints(None, None, true)?;
        let reminders = store
            .list(&ReminderFilter::default())
            .map_err(reminder_err)?;
        let mut epochs = Vec::new();
        for e in store.list_epochs()? {
            // `list_epochs` leaves membership out; `get_epoch` reads it.
            epochs.push(store.get_epoch(&e.id)?.unwrap_or(e));
        }
        if head.is_none()
            && taints.is_empty()
            && reminders.is_empty()
            && epochs.is_empty()
            && pending.is_empty()
        {
            return Ok(Some(ControlReport::default()));
        }

        // 1. The control ref takes, by its merge rules, the rows it lacks or
        //    holds differently. A reminder's rule needs an edit time the
        //    table doesn't keep, so only a reminder the ref lacks or a
        //    pending one is taken.
        let now = Utc::now();
        let mut candidate = ControlEdit::new(store);
        for t in &taints {
            if control.taints.get(&t.id) != Some(t) {
                candidate.put_taint(t)?;
            }
        }
        let table_reminders: HashSet<&str> = reminders.iter().map(|r| r.id.as_str()).collect();
        for r in &reminders {
            if !control.reminders.contains_key(&r.id)
                || pending_records.contains(&(Kind::Reminder, r.id.clone()))
            {
                candidate.put_reminder(r, now)?;
            }
        }
        for (kind, id) in &pending_records {
            if *kind == Kind::Reminder && !table_reminders.contains(id.as_str()) {
                candidate.delete_reminder(id, now)?;
            }
        }
        for e in &epochs {
            let mut record = e.clone();
            record.commits.clear();
            record.sealed_commits.sort();
            if control.epochs.get(&e.id) != Some(&record) {
                candidate.put_epoch(e)?;
            }
            let have = control.members.get(&e.id);
            if e.commits
                .iter()
                .any(|c| have.is_none_or(|m| !m.contains(c)))
            {
                candidate.add_members(&e.id, e.commits.iter().copied())?;
            }
        }
        let candidate = candidate.finish()?;
        let merged = match &base {
            Some(root) => control::merge(store, root, &candidate)?.root,
            None => candidate,
        };
        let (new_head, committed) = if Some(merged) == base {
            (head, false)
        } else {
            let commit = CommitBuilder::new(
                merged,
                "system",
                Authority::simple("system"),
                Intent::new(
                    IntentCategory::Migrate,
                    "take the taint, reminder and epoch tables into the control ref",
                ),
            )
            .parents(head.into_iter().collect())
            .build();
            store.put_commit(&commit)?;
            let landed = match head {
                Some(h) => store.cas_ref(&ns, CONTROL_REF, h, commit.id)?,
                None => store.create_ref(&ns, CONTROL_REF, commit.id)?,
            };
            if !landed {
                return Ok(None);
            }
            (Some(commit.id), true)
        };

        // 2. The tables take every record they don't match.
        let state = control::read(store, &merged)?;
        let mut report = ControlReport {
            committed,
            ..ControlReport::default()
        };
        self.rebuild_taints(&state, &taints, &mut report);
        self.rebuild_reminders(&state, &reminders, &mut report);
        self.rebuild_epochs(&state, &epochs, &mut report);

        // 3. Bookkeeping: the tables match `new_head`, and what was pending
        //    has been taken.
        if let Some(h) = new_head {
            store.set_ref(&ns, INDEXED_REF, h)?;
        }
        for (name, target) in pending {
            let _ = store.cas_delete_ref(&ns, &name, target);
        }
        Ok(Some(report))
    }

    fn rebuild_taints(&self, state: &ControlState, table: &[Taint], report: &mut ControlReport) {
        let store = self.control_store();
        for t in state.taints.values() {
            let have = table.iter().find(|x| x.id == t.id);
            let result: Result<bool, StorageError> = (|| {
                let mut changed = false;
                let have = match have {
                    Some(h) => h.clone(),
                    None => {
                        let mut fresh = t.clone();
                        fresh.resolved_at = None;
                        fresh.resolved_by = None;
                        fresh.resolved_reason = None;
                        fresh.resolved_proof = None;
                        store.create_taint(&fresh)?;
                        changed = true;
                        fresh
                    }
                };
                if have.commit_id != t.commit_id && !t.commit_id.is_empty() {
                    store.set_taint_commit_id(&t.id, &t.commit_id)?;
                    changed = true;
                }
                if let (None, Some(at)) = (have.resolved_at, t.resolved_at) {
                    store.resolve_taint(
                        &t.id,
                        t.resolved_by.as_deref().unwrap_or(""),
                        t.resolved_reason.as_deref().unwrap_or(""),
                        t.resolved_proof.as_deref(),
                        at,
                    )?;
                    changed = true;
                }
                Ok(changed)
            })();
            match result {
                Ok(true) => report.rebuilt += 1,
                Ok(false) => {}
                Err(e) => report.skipped.push(format!("taint {}: {e}", t.id)),
            }
        }
    }

    fn rebuild_reminders(
        &self,
        state: &ControlState,
        table: &[agentstategraph_reminders::Reminder],
        report: &mut ControlReport,
    ) {
        let store = self.control_store();
        for (id, entry) in &state.reminders {
            let have = table.iter().find(|r| &r.id == id);
            let result = match (entry, have) {
                (ReminderEntry::Live { reminder, .. }, None) => store.save(reminder).map(|_| true),
                (ReminderEntry::Live { reminder, .. }, Some(h)) if !same_reminder(reminder, h) => {
                    store.update(reminder).map(|_| true)
                }
                (ReminderEntry::Deleted { .. }, Some(_)) => store.delete(id),
                _ => Ok(false),
            };
            match result {
                Ok(true) => report.rebuilt += 1,
                Ok(false) => {}
                Err(e) => report.skipped.push(format!("reminder {id}: {e}")),
            }
        }
    }

    fn rebuild_epochs(&self, state: &ControlState, table: &[Epoch], report: &mut ControlReport) {
        let store = self.control_store();
        for e in state.epochs.values() {
            let have = table.iter().find(|x| x.id == e.id);
            let members = state.members.get(&e.id);
            let result: Result<bool, StorageError> = (|| {
                let mut changed = false;
                let status = match have {
                    Some(h) => h.status.clone(),
                    None => {
                        // In as active, so it can take its members before
                        // it's sealed.
                        store.create_epoch(&Epoch {
                            status: EpochStatus::Active,
                            sealed_at: None,
                            seal_summary: None,
                            seal_hash: None,
                            sealed_commits: Vec::new(),
                            commits: Vec::new(),
                            ..e.clone()
                        })?;
                        changed = true;
                        EpochStatus::Active
                    }
                };
                if status == EpochStatus::Active {
                    let had: HashSet<ObjectId> = have
                        .map(|h| h.commits.iter().copied().collect())
                        .unwrap_or_default();
                    for c in members.into_iter().flatten() {
                        // A commit this store hasn't got yet can't join.
                        if !had.contains(c) && store.has_commit(c)? {
                            store.set_commit_epoch(c, &e.id)?;
                            changed = true;
                        }
                    }
                }
                if status == EpochStatus::Active && e.status != EpochStatus::Active {
                    store.seal_epoch(
                        &e.id,
                        e.seal_summary.as_deref().unwrap_or(""),
                        e.sealed_at.unwrap_or_else(Utc::now),
                        &e.sealed_commits,
                        &e.seal_hash
                            .unwrap_or_else(|| ObjectId::hash(e.id.as_bytes())),
                    )?;
                    changed = true;
                }
                if e.status == EpochStatus::Archived && status != EpochStatus::Archived {
                    store.archive_epoch(&e.id)?;
                    changed = true;
                }
                if let (Some(ns), Some(h)) = (&e.namespace, have)
                    && h.namespace.is_none()
                    && store.assign_epoch_namespace(&e.id, ns)?
                {
                    changed = true;
                }
                Ok(changed)
            })();
            match result {
                Ok(true) => report.rebuilt += 1,
                Ok(false) => {}
                Err(e2) => report.skipped.push(format!("epoch {}: {e2}", e.id)),
            }
        }
    }
}
