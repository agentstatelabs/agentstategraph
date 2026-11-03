//! The control ref (DESIGN.md Plan G t-001).
//!
//! Taints, reminders and epochs live in storage tables outside the commit
//! graph, so syncing commit history would leave them behind. The control ref,
//! `_control`, holds them as ordinary tree state instead: every change to
//! them is a commit with provenance, sync moves only commit history, and the
//! tables become indexes rebuilt from the ref.
//!
//! This module is the ref's model: its layout, reading and editing it, and
//! the merge that combines two versions of it. The merge needs no common
//! ancestor and raises no conflicts. Each record type has a rule that picks
//! one version, or for epoch membership takes the union, so merging is
//! commutative, associative and idempotent, and two stores that merge each
//! other's control refs end up with the same tree.
//!
//! - **Taints.** A resolved taint stays resolved: a resolved version beats an
//!   unresolved one, and of two resolutions the earlier wins. Of two
//!   unresolved versions, the one carrying its intent commit wins.
//! - **Reminders.** A terminal status (completed, cancelled) beats a
//!   non-terminal one, and a deletion beats both. Otherwise the newest edit
//!   wins, by the edit time the control ref records with each version.
//!   Non-terminal statuses aren't ordered among themselves, since a
//!   recurring reminder goes back to pending after each run.
//! - **Epochs.** A sealed epoch is immutable: sealed beats active, archived
//!   beats sealed, and of two different seals the earlier wins. Two seals
//!   that differ mean two stores sealed one epoch differently, so the merge
//!   reports them. Of two active versions, one with a namespace assigned
//!   wins (assigning one is final).
//! - **Epoch membership** is a set of commits per epoch, merged by union.
//!
//! Versions that differ yet rank the same fall to the larger object id, so
//! the result never depends on which side is which.
//!
//! # Layout
//!
//! ```text
//! /format              1
//! /taints/<id>         the taint, as a JSON tree
//! /reminders/<id>      {reminder: <JSON tree>, edited_at} or {deleted_at}
//! /epochs/<id>         {record: <the epoch as a JSON tree, without its
//!                       commit lists>, sealed: <id list>}
//! /members/<epoch id>  <id list>: the commits in the epoch
//! ```
//!
//! An id list is a set of ids, sorted, as a list of string atoms each
//! holding up to 256 ids in hex. A chunk ends after an id whose last byte is
//! below 4 (about one id in 64), so where chunks end depends only on the ids
//! in them: adding or removing one id changes one chunk, and sets that mostly
//! overlap share most chunks. That matters for seals, since each covers every
//! commit reachable from `main` at the time and successive seals mostly
//! repeat each other. A real CTXone store's 364 sealed sets, 118 MB as its
//! epochs table keeps them, take 7.2 MB this way (and 68 MB in walk order).

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};

use agentstategraph_core::{Atom, Epoch, EpochStatus, Node, Object, ObjectId};
use agentstategraph_reminders::Reminder;
use agentstategraph_storage::{ObjectStore, StorageError};
use agentstategraph_taint::Taint;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::repo::RepoError;
use crate::tree::{json_to_tree, tree_to_json};

/// The control ref's name, in the default namespace. One per store.
pub const CONTROL_REF: &str = "_control";

/// The layout version this build reads and writes.
pub const CONTROL_FORMAT: i64 = 1;

const FORMAT: &str = "format";
const TAINTS: &str = "taints";
const REMINDERS: &str = "reminders";
const EPOCHS: &str = "epochs";
const MEMBERS: &str = "members";
const SECTIONS: [&str; 4] = [TAINTS, REMINDERS, EPOCHS, MEMBERS];

/// An id list's chunk ends after an id whose last byte is below this. The
/// last byte, not the first: a sorted list is ordered by the first, so cuts
/// on it would all fall at the start.
const CHUNK_CUT: u8 = 4;
/// And after this many ids, whatever they are.
const CHUNK_MAX: usize = 256;

/// A reminder as the control ref keeps it: the record and when it was last
/// edited, or that it was deleted, which no later edit undoes.
#[derive(Debug, Clone)]
pub enum ReminderEntry {
    Live {
        reminder: Box<Reminder>,
        edited_at: DateTime<Utc>,
    },
    Deleted {
        deleted_at: DateTime<Utc>,
    },
}

impl ReminderEntry {
    /// The reminder, unless it was deleted.
    pub fn live(&self) -> Option<&Reminder> {
        match self {
            ReminderEntry::Live { reminder, .. } => Some(reminder.as_ref()),
            ReminderEntry::Deleted { .. } => None,
        }
    }
}

/// Everything the control ref holds, decoded.
#[derive(Debug, Clone, Default)]
pub struct ControlState {
    pub taints: BTreeMap<String, Taint>,
    pub reminders: BTreeMap<String, ReminderEntry>,
    /// Each epoch with its `sealed_commits`, sorted; its `commits` are in
    /// `members`.
    pub epochs: BTreeMap<String, Epoch>,
    /// The commits in each epoch.
    pub members: BTreeMap<String, BTreeSet<ObjectId>>,
}

/// Two stores sealed one epoch differently. The merge keeps the earlier seal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpochConflict {
    pub epoch: String,
    pub kept_seal: Option<ObjectId>,
    pub dropped_seal: Option<ObjectId>,
}

/// The result of [`merge`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Merged {
    pub root: ObjectId,
    pub conflicts: Vec<EpochConflict>,
}

fn malformed(what: impl std::fmt::Display) -> RepoError {
    RepoError::InvalidOperation(format!("malformed control ref: {what}"))
}

fn load(store: &dyn ObjectStore, id: &ObjectId) -> Result<Object, RepoError> {
    store
        .get_object(id)?
        .ok_or_else(|| RepoError::Storage(StorageError::ObjectNotFound(id.to_string())))
}

fn load_map(
    store: &dyn ObjectStore,
    id: &ObjectId,
) -> Result<BTreeMap<String, ObjectId>, RepoError> {
    match load(store, id)? {
        Object::Node(Node::Map(entries)) => Ok(entries),
        _ => Err(malformed(format!("{id} isn't a map"))),
    }
}

fn put_map(
    store: &dyn ObjectStore,
    entries: BTreeMap<String, ObjectId>,
) -> Result<ObjectId, RepoError> {
    Ok(store.put_object(&Object::map(entries))?)
}

fn put_string(store: &dyn ObjectStore, s: String) -> Result<ObjectId, RepoError> {
    Ok(store.put_object(&Object::string(s))?)
}

fn load_string(store: &dyn ObjectStore, id: &ObjectId) -> Result<String, RepoError> {
    match load(store, id)? {
        Object::Atom(Atom::String(s)) => Ok(s),
        _ => Err(malformed(format!("{id} isn't a string"))),
    }
}

fn put_json<T: Serialize>(store: &dyn ObjectStore, value: &T) -> Result<ObjectId, RepoError> {
    let value = serde_json::to_value(value).map_err(malformed)?;
    Ok(json_to_tree(store, &value)?)
}

fn load_json<T: DeserializeOwned>(store: &dyn ObjectStore, id: &ObjectId) -> Result<T, RepoError> {
    let value = tree_to_json(store, &load(store, id)?)?;
    serde_json::from_value(value).map_err(malformed)
}

fn put_time(store: &dyn ObjectStore, at: &DateTime<Utc>) -> Result<ObjectId, RepoError> {
    put_string(store, at.to_rfc3339())
}

fn load_time(store: &dyn ObjectStore, id: &ObjectId) -> Result<DateTime<Utc>, RepoError> {
    DateTime::parse_from_rfc3339(&load_string(store, id)?)
        .map(|t| t.with_timezone(&Utc))
        .map_err(malformed)
}

/// Store `ids`, which must be sorted and distinct, as an id list.
fn put_ids<'a>(
    store: &dyn ObjectStore,
    ids: impl IntoIterator<Item = &'a ObjectId>,
) -> Result<ObjectId, RepoError> {
    let mut chunks = Vec::new();
    let mut chunk = String::new();
    let mut in_chunk = 0;
    for id in ids {
        chunk.push_str(&id.to_hex());
        in_chunk += 1;
        if id.as_bytes()[31] < CHUNK_CUT || in_chunk == CHUNK_MAX {
            chunks.push(Object::string(std::mem::take(&mut chunk)));
            in_chunk = 0;
        }
    }
    if !chunk.is_empty() {
        chunks.push(Object::string(chunk));
    }
    let list = Object::Node(Node::List(chunks.iter().map(Object::id).collect()));
    let id = list.id();
    chunks.push(list);
    store.batch_put_objects(&chunks)?;
    Ok(id)
}

fn load_ids(store: &dyn ObjectStore, id: &ObjectId) -> Result<Vec<ObjectId>, RepoError> {
    let Object::Node(Node::List(chunk_ids)) = load(store, id)? else {
        return Err(malformed(format!("{id} isn't an id list")));
    };
    let mut ids = Vec::new();
    for (chunk_id, chunk) in chunk_ids.iter().zip(store.batch_get_objects(&chunk_ids)?) {
        let Some(Object::Atom(Atom::String(hex))) = chunk else {
            return Err(malformed(format!("id list chunk {chunk_id}")));
        };
        if hex.len() % 64 != 0 {
            return Err(malformed(format!("id list chunk {chunk_id}")));
        }
        for i in (0..hex.len()).step_by(64) {
            ids.push(
                ObjectId::from_hex(&hex[i..i + 64])
                    .ok_or_else(|| malformed(format!("id list chunk {chunk_id}")))?,
            );
        }
    }
    Ok(ids)
}

fn put_taint(store: &dyn ObjectStore, taint: &Taint) -> Result<ObjectId, RepoError> {
    put_json(store, taint)
}

fn load_taint(store: &dyn ObjectStore, id: &ObjectId) -> Result<Taint, RepoError> {
    load_json(store, id)
}

fn put_reminder(store: &dyn ObjectStore, entry: &ReminderEntry) -> Result<ObjectId, RepoError> {
    let mut doc = BTreeMap::new();
    match entry {
        ReminderEntry::Live {
            reminder,
            edited_at,
        } => {
            doc.insert("reminder".to_string(), put_json(store, reminder)?);
            doc.insert("edited_at".to_string(), put_time(store, edited_at)?);
        }
        ReminderEntry::Deleted { deleted_at } => {
            doc.insert("deleted_at".to_string(), put_time(store, deleted_at)?);
        }
    }
    put_map(store, doc)
}

fn load_reminder(store: &dyn ObjectStore, id: &ObjectId) -> Result<ReminderEntry, RepoError> {
    let doc = load_map(store, id)?;
    if let Some(at) = doc.get("deleted_at") {
        return Ok(ReminderEntry::Deleted {
            deleted_at: load_time(store, at)?,
        });
    }
    let field = |name: &str| {
        doc.get(name)
            .ok_or_else(|| malformed(format!("reminder {id} has no {name}")))
    };
    Ok(ReminderEntry::Live {
        reminder: Box::new(load_json(store, field("reminder")?)?),
        edited_at: load_time(store, field("edited_at")?)?,
    })
}

fn put_epoch(store: &dyn ObjectStore, epoch: &Epoch) -> Result<ObjectId, RepoError> {
    let record = Epoch {
        commits: Vec::new(),
        sealed_commits: Vec::new(),
        ..epoch.clone()
    };
    let sealed: BTreeSet<ObjectId> = epoch.sealed_commits.iter().copied().collect();
    let mut doc = BTreeMap::new();
    doc.insert("record".to_string(), put_json(store, &record)?);
    doc.insert("sealed".to_string(), put_ids(store, &sealed)?);
    put_map(store, doc)
}

fn load_epoch(store: &dyn ObjectStore, id: &ObjectId) -> Result<Epoch, RepoError> {
    let doc = load_map(store, id)?;
    let field = |name: &str| {
        doc.get(name)
            .ok_or_else(|| malformed(format!("epoch {id} has no {name}")))
    };
    let mut epoch: Epoch = load_json(store, field("record")?)?;
    epoch.sealed_commits = load_ids(store, field("sealed")?)?;
    Ok(epoch)
}

/// The control tree's sections, as maps from record id to the record's tree.
struct Sections {
    maps: BTreeMap<&'static str, BTreeMap<String, ObjectId>>,
}

impl Sections {
    fn empty() -> Self {
        Self {
            maps: SECTIONS.iter().map(|s| (*s, BTreeMap::new())).collect(),
        }
    }

    fn load(store: &dyn ObjectStore, root: &ObjectId) -> Result<Self, RepoError> {
        let top = load_map(store, root)?;
        let format = match top.get(FORMAT).map(|id| load(store, id)).transpose()? {
            Some(Object::Atom(Atom::Int(n))) => n,
            _ => return Err(malformed("no format")),
        };
        if format != CONTROL_FORMAT {
            return Err(RepoError::InvalidOperation(format!(
                "the control ref is in format {format}; this build reads format {CONTROL_FORMAT}. Upgrade AgentStateGraph"
            )));
        }
        let mut maps = BTreeMap::new();
        for section in SECTIONS {
            let id = top
                .get(section)
                .ok_or_else(|| malformed(format!("no {section}")))?;
            maps.insert(section, load_map(store, id)?);
        }
        Ok(Self { maps })
    }

    fn map(&mut self, section: &'static str) -> &mut BTreeMap<String, ObjectId> {
        self.maps
            .get_mut(section)
            .expect("every section is present")
    }

    fn write(self, store: &dyn ObjectStore) -> Result<ObjectId, RepoError> {
        let mut top = BTreeMap::new();
        top.insert(
            FORMAT.to_string(),
            store.put_object(&Object::int(CONTROL_FORMAT))?,
        );
        for (section, entries) in self.maps {
            top.insert(section.to_string(), put_map(store, entries)?);
        }
        put_map(store, top)
    }
}

/// Read the whole control tree at `root`.
pub fn read(store: &dyn ObjectStore, root: &ObjectId) -> Result<ControlState, RepoError> {
    let sections = Sections::load(store, root)?;
    let mut state = ControlState::default();
    for (id, tree) in &sections.maps[TAINTS] {
        state.taints.insert(id.clone(), load_taint(store, tree)?);
    }
    for (id, tree) in &sections.maps[REMINDERS] {
        state
            .reminders
            .insert(id.clone(), load_reminder(store, tree)?);
    }
    for (id, tree) in &sections.maps[EPOCHS] {
        state.epochs.insert(id.clone(), load_epoch(store, tree)?);
    }
    for (id, tree) in &sections.maps[MEMBERS] {
        state
            .members
            .insert(id.clone(), load_ids(store, tree)?.into_iter().collect());
    }
    Ok(state)
}

/// Write `state` as a control tree, returning its root.
pub fn write(store: &dyn ObjectStore, state: &ControlState) -> Result<ObjectId, RepoError> {
    let mut edit = ControlEdit::new(store);
    for taint in state.taints.values() {
        edit.put_taint(taint)?;
    }
    for (id, entry) in &state.reminders {
        edit.put_reminder_entry(id, entry)?;
    }
    for epoch in state.epochs.values() {
        edit.put_epoch(epoch)?;
    }
    for (epoch, commits) in &state.members {
        edit.add_members(epoch, commits.iter().copied())?;
    }
    edit.finish()
}

/// Changes to a control tree, a record at a time: only the records changed
/// are encoded, and the rest of the tree is shared with the one it started
/// from.
pub struct ControlEdit<'s> {
    store: &'s dyn ObjectStore,
    sections: Sections,
}

impl<'s> ControlEdit<'s> {
    /// Start from an empty control tree.
    pub fn new(store: &'s dyn ObjectStore) -> Self {
        Self {
            store,
            sections: Sections::empty(),
        }
    }

    /// Start from the control tree at `root`.
    pub fn open(store: &'s dyn ObjectStore, root: &ObjectId) -> Result<Self, RepoError> {
        Ok(Self {
            store,
            sections: Sections::load(store, root)?,
        })
    }

    /// Add or replace a taint, keyed by its id.
    pub fn put_taint(&mut self, taint: &Taint) -> Result<(), RepoError> {
        let tree = put_taint(self.store, taint)?;
        self.sections.map(TAINTS).insert(taint.id.clone(), tree);
        Ok(())
    }

    /// Add or replace a reminder, edited at `edited_at`.
    pub fn put_reminder(
        &mut self,
        reminder: &Reminder,
        edited_at: DateTime<Utc>,
    ) -> Result<(), RepoError> {
        let entry = ReminderEntry::Live {
            reminder: Box::new(reminder.clone()),
            edited_at,
        };
        self.put_reminder_entry(&reminder.id, &entry)
    }

    /// Record that the reminder `id` was deleted.
    pub fn delete_reminder(
        &mut self,
        id: &str,
        deleted_at: DateTime<Utc>,
    ) -> Result<(), RepoError> {
        self.put_reminder_entry(id, &ReminderEntry::Deleted { deleted_at })
    }

    fn put_reminder_entry(&mut self, id: &str, entry: &ReminderEntry) -> Result<(), RepoError> {
        let tree = put_reminder(self.store, entry)?;
        self.sections.map(REMINDERS).insert(id.to_string(), tree);
        Ok(())
    }

    /// Add or replace an epoch, keyed by its id. Its `sealed_commits` go
    /// with it, as a set (they read back sorted); its `commits` don't (see
    /// [`add_members`](Self::add_members)).
    pub fn put_epoch(&mut self, epoch: &Epoch) -> Result<(), RepoError> {
        let tree = put_epoch(self.store, epoch)?;
        self.sections.map(EPOCHS).insert(epoch.id.clone(), tree);
        Ok(())
    }

    /// Add commits to an epoch's membership.
    pub fn add_members(
        &mut self,
        epoch: &str,
        commits: impl IntoIterator<Item = ObjectId>,
    ) -> Result<(), RepoError> {
        let mut members: BTreeSet<ObjectId> = match self.sections.map(MEMBERS).get(epoch) {
            Some(tree) => load_ids(self.store, tree)?.into_iter().collect(),
            None => BTreeSet::new(),
        };
        let before = members.len();
        members.extend(commits);
        if members.len() == before && self.sections.map(MEMBERS).contains_key(epoch) {
            return Ok(());
        }
        let tree = put_ids(self.store, &members)?;
        self.sections.map(MEMBERS).insert(epoch.to_string(), tree);
        Ok(())
    }

    /// Write the edited tree, returning its root.
    pub fn finish(self) -> Result<ObjectId, RepoError> {
        self.sections.write(self.store)
    }
}

/// How a taint version ranks: a resolution beats none, an earlier
/// resolution a later one, and a version carrying its intent commit one
/// without.
fn taint_rank(t: &Taint) -> impl Ord {
    (
        t.resolved_at.is_some(),
        Reverse(t.resolved_at),
        !t.commit_id.is_empty(),
    )
}

/// How a reminder version ranks: deleted, then terminal, then by edit time.
fn reminder_rank(r: &ReminderEntry) -> impl Ord {
    match r {
        ReminderEntry::Deleted { deleted_at } => (2u8, *deleted_at),
        ReminderEntry::Live {
            reminder,
            edited_at,
        } => (u8::from(reminder.status.is_terminal()), *edited_at),
    }
}

/// How an epoch version ranks: archived, then sealed, then active; an
/// earlier seal beats a later one, and an assigned namespace beats none.
fn epoch_rank(e: &Epoch) -> impl Ord {
    let status = match e.status {
        EpochStatus::Active => 0u8,
        EpochStatus::Sealed => 1,
        EpochStatus::Archived => 2,
    };
    (status, Reverse(e.sealed_at), e.namespace.is_some())
}

/// Pick between two versions of a record: the higher rank, or on a tie the
/// larger tree id. Returns `(winner, loser)` trees.
fn pick<R: Ord>(ours: (ObjectId, R), theirs: (ObjectId, R)) -> (ObjectId, ObjectId) {
    if (&theirs.1, theirs.0) > (&ours.1, ours.0) {
        (theirs.0, ours.0)
    } else {
        (ours.0, theirs.0)
    }
}

/// Merge two control trees by the rules in the [module docs](self). The
/// result is the same whichever side is `ours`.
pub fn merge(
    store: &dyn ObjectStore,
    ours: &ObjectId,
    theirs: &ObjectId,
) -> Result<Merged, RepoError> {
    if ours == theirs {
        return Ok(Merged {
            root: *ours,
            conflicts: Vec::new(),
        });
    }
    let mut a = Sections::load(store, ours)?;
    let b = Sections::load(store, theirs)?;
    let mut conflicts = Vec::new();
    for (section, theirs_map) in b.maps {
        let ours_map = a.map(section);
        for (id, their_tree) in theirs_map {
            let Some(&our_tree) = ours_map.get(&id) else {
                ours_map.insert(id, their_tree);
                continue;
            };
            if our_tree == their_tree {
                continue;
            }
            let winner = match section {
                TAINTS => {
                    let (o, t) = (
                        load_taint(store, &our_tree)?,
                        load_taint(store, &their_tree)?,
                    );
                    pick((our_tree, taint_rank(&o)), (their_tree, taint_rank(&t))).0
                }
                REMINDERS => {
                    let o = load_reminder(store, &our_tree)?;
                    let t = load_reminder(store, &their_tree)?;
                    pick(
                        (our_tree, reminder_rank(&o)),
                        (their_tree, reminder_rank(&t)),
                    )
                    .0
                }
                EPOCHS => {
                    let (o, t) = (
                        load_epoch(store, &our_tree)?,
                        load_epoch(store, &their_tree)?,
                    );
                    let (winner, _) =
                        pick((our_tree, epoch_rank(&o)), (their_tree, epoch_rank(&t)));
                    if let (Some(x), Some(y)) = (o.seal_hash, t.seal_hash)
                        && x != y
                    {
                        let (kept, dropped) = if winner == our_tree { (x, y) } else { (y, x) };
                        conflicts.push(EpochConflict {
                            epoch: id.clone(),
                            kept_seal: Some(kept),
                            dropped_seal: Some(dropped),
                        });
                    }
                    winner
                }
                MEMBERS => {
                    let mut union: BTreeSet<ObjectId> =
                        load_ids(store, &our_tree)?.into_iter().collect();
                    union.extend(load_ids(store, &their_tree)?);
                    put_ids(store, &union)?
                }
                _ => unreachable!("every section is handled"),
            };
            ours_map.insert(id, winner);
        }
    }
    Ok(Merged {
        root: a.write(store)?,
        conflicts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentstategraph_reminders::types::{Priority, ReminderStatus};
    use agentstategraph_storage::MemoryStorage;
    use agentstategraph_taint::{TaintEffect, TaintKind, TaintMetadata, TaintSeverity};

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + secs, 0).unwrap()
    }

    fn taint(id: &str) -> Taint {
        Taint {
            id: id.into(),
            path: "/a/b".into(),
            name: "pii".into(),
            kind: TaintKind::Taint,
            effect: TaintEffect::Block,
            severity: TaintSeverity::High,
            reason: "because".into(),
            agent_id: "agent/x".into(),
            commit_id: String::new(),
            created_at: at(0),
            expires_at: None,
            resolved_at: None,
            resolved_by: None,
            resolved_reason: None,
            resolved_proof: None,
            propagate: true,
            metadata: TaintMetadata::new(),
        }
    }

    fn resolved(mut t: Taint, secs: i64, by: &str) -> Taint {
        t.resolved_at = Some(at(secs));
        t.resolved_by = Some(by.into());
        t.resolved_reason = Some("fixed".into());
        t
    }

    fn reminder(id: &str, status: ReminderStatus) -> Reminder {
        Reminder {
            id: id.into(),
            title: "check".into(),
            instructions: "look".into(),
            commands: vec![],
            refs: vec![],
            priority: Priority::default(),
            due_at: at(100),
            schedule: None,
            autonomous: false,
            created_by: "agent/x".into(),
            created_at: at(0),
            status,
            snoozed_until: None,
            executions: vec![],
            tags: vec!["t".into()],
        }
    }

    fn epoch(id: &str) -> Epoch {
        Epoch::new(id, "work", vec![]).with_namespace("default")
    }

    fn sealed(mut e: Epoch, secs: i64, commits: &[ObjectId]) -> Epoch {
        e.status = EpochStatus::Sealed;
        e.sealed_at = Some(at(secs));
        e.seal_summary = Some("done".into());
        e.sealed_commits = commits.to_vec();
        e.sealed_commits.sort();
        e.seal_hash = Some(ObjectId::hash(format!("{secs}{commits:?}").as_bytes()));
        e
    }

    fn ids(n: usize, salt: &str) -> Vec<ObjectId> {
        (0..n)
            .map(|i| ObjectId::hash(format!("{salt}{i}").as_bytes()))
            .collect()
    }

    /// Records compared by their JSON, since `Reminder` has no `PartialEq`.
    fn json(state: &ControlState) -> serde_json::Value {
        let reminders: BTreeMap<&String, serde_json::Value> = state
            .reminders
            .iter()
            .map(|(id, e)| {
                let v = match e {
                    ReminderEntry::Live {
                        reminder,
                        edited_at,
                    } => serde_json::json!({"live": reminder, "at": edited_at}),
                    ReminderEntry::Deleted { deleted_at } => {
                        serde_json::json!({"deleted": deleted_at})
                    }
                };
                (id, v)
            })
            .collect();
        serde_json::json!({
            "taints": state.taints,
            "reminders": reminders,
            "epochs": state.epochs,
            "members": state.members,
        })
    }

    fn sample() -> ControlState {
        let mut s = ControlState::default();
        s.taints.insert("t1".into(), taint("t1"));
        s.taints
            .insert("t2".into(), resolved(taint("t2"), 5, "person/a"));
        s.reminders.insert(
            "r1".into(),
            ReminderEntry::Live {
                reminder: Box::new(reminder("r1", ReminderStatus::Pending)),
                edited_at: at(1),
            },
        );
        s.reminders
            .insert("r2".into(), ReminderEntry::Deleted { deleted_at: at(2) });
        s.epochs.insert("e1".into(), epoch("e1"));
        let mut main = ids(600, "main");
        main.sort();
        s.epochs.insert("e2".into(), sealed(epoch("e2"), 9, &main));
        s.members
            .insert("e1".into(), ids(300, "e1").into_iter().collect());
        s
    }

    #[test]
    fn a_control_tree_round_trips() {
        let store = MemoryStorage::new();
        let state = sample();
        let root = write(&store, &state).unwrap();
        assert_eq!(json(&read(&store, &root).unwrap()), json(&state));
        let mut sorted = ids(600, "main");
        sorted.sort();
        assert_eq!(
            read(&store, &root).unwrap().epochs["e2"].sealed_commits,
            sorted,
            "a sealed set reads back sorted"
        );
        // The same state writes the same tree.
        assert_eq!(write(&store, &state).unwrap(), root);
    }

    #[test]
    fn an_empty_control_tree_round_trips() {
        let store = MemoryStorage::new();
        let root = ControlEdit::new(&store).finish().unwrap();
        assert_eq!(
            json(&read(&store, &root).unwrap()),
            json(&ControlState::default())
        );
    }

    #[test]
    fn an_edit_touches_only_its_record() {
        let store = MemoryStorage::new();
        let root = write(&store, &sample()).unwrap();
        let before = Sections::load(&store, &root).unwrap();

        let mut edit = ControlEdit::open(&store, &root).unwrap();
        edit.put_taint(&resolved(taint("t1"), 7, "person/b"))
            .unwrap();
        let after = Sections::load(&store, &edit.finish().unwrap()).unwrap();

        for section in [REMINDERS, EPOCHS, MEMBERS] {
            assert_eq!(before.maps[section], after.maps[section], "{section}");
        }
        assert_eq!(before.maps[TAINTS]["t2"], after.maps[TAINTS]["t2"]);
        assert_ne!(before.maps[TAINTS]["t1"], after.maps[TAINTS]["t1"]);
    }

    #[test]
    fn id_lists_change_locally_and_share_runs() {
        let store = MemoryStorage::new();
        let chunks = |list: &ObjectId| -> Vec<ObjectId> {
            match store.get_object(list).unwrap().unwrap() {
                Object::Node(Node::List(c)) => c,
                _ => panic!("a list"),
            }
        };
        let mut sorted: Vec<ObjectId> = ids(5_000, "x");
        sorted.sort();
        let a = put_ids(&store, &sorted).unwrap();
        assert_eq!(load_ids(&store, &a).unwrap(), sorted);
        assert!(chunks(&a).len() > 20, "{} chunks", chunks(&a).len());

        // One more id changes one chunk.
        let mut more = sorted.clone();
        more.push(ObjectId::hash(b"one more"));
        more.sort();
        let b = put_ids(&store, &more).unwrap();
        let (ca, cb): (BTreeSet<_>, BTreeSet<_>) = (
            chunks(&a).into_iter().collect(),
            chunks(&b).into_iter().collect(),
        );
        assert!(
            cb.difference(&ca).count() <= 2,
            "{} new chunks",
            cb.difference(&ca).count()
        );

        // A later seal: the earlier seal's commits and 50 more.
        let earlier: BTreeSet<ObjectId> = ids(5_000, "main").into_iter().collect();
        let later: BTreeSet<ObjectId> = earlier.iter().copied().chain(ids(50, "new")).collect();
        let (e, l) = (
            put_ids(&store, &earlier).unwrap(),
            put_ids(&store, &later).unwrap(),
        );
        let (ce, cl): (BTreeSet<_>, BTreeSet<_>) = (
            chunks(&e).into_iter().collect(),
            chunks(&l).into_iter().collect(),
        );
        assert!(
            ce.intersection(&cl).count() * 10 >= ce.len() * 6,
            "{} of {} shared",
            ce.intersection(&cl).count(),
            ce.len()
        );

        let empty = put_ids(&store, &[]).unwrap();
        assert!(load_ids(&store, &empty).unwrap().is_empty());
    }

    fn state_root(store: &MemoryStorage, state: &ControlState) -> ObjectId {
        write(store, state).unwrap()
    }

    fn merged(store: &MemoryStorage, a: &ControlState, b: &ControlState) -> ControlState {
        let (x, y) = (state_root(store, a), state_root(store, b));
        let ab = merge(store, &x, &y).unwrap();
        let ba = merge(store, &y, &x).unwrap();
        assert_eq!(ab, ba, "merging is commutative");
        read(store, &ab.root).unwrap()
    }

    fn one_taint(t: Taint) -> ControlState {
        let mut s = ControlState::default();
        s.taints.insert(t.id.clone(), t);
        s
    }

    #[test]
    fn a_resolved_taint_stays_resolved() {
        let store = MemoryStorage::new();
        let open = one_taint(taint("t"));
        let mut backpatched = taint("t");
        backpatched.commit_id = "sg_intent".into();
        let early = one_taint(resolved(taint("t"), 3, "person/early"));
        let late = one_taint(resolved(taint("t"), 8, "person/late"));

        let m = merged(&store, &open, &early);
        assert_eq!(m.taints["t"].resolved_by.as_deref(), Some("person/early"));
        let m = merged(&store, &early, &late);
        assert_eq!(m.taints["t"].resolved_by.as_deref(), Some("person/early"));
        let m = merged(&store, &open, &one_taint(backpatched.clone()));
        assert_eq!(m.taints["t"].commit_id, "sg_intent");
        let m = merged(&store, &one_taint(backpatched), &late);
        assert!(m.taints["t"].resolved_at.is_some());
    }

    fn one_reminder(id: &str, entry: ReminderEntry) -> ControlState {
        let mut s = ControlState::default();
        s.reminders.insert(id.into(), entry);
        s
    }

    fn live(status: ReminderStatus, edited: i64) -> ReminderEntry {
        ReminderEntry::Live {
            reminder: Box::new(reminder("r", status)),
            edited_at: at(edited),
        }
    }

    fn status(state: &ControlState) -> Option<ReminderStatus> {
        state.reminders["r"].live().map(|r| r.status)
    }

    #[test]
    fn reminders_end_where_either_side_ended_them_and_otherwise_take_the_newest_edit() {
        let store = MemoryStorage::new();
        let s = |e| one_reminder("r", e);
        // Newest edit wins among non-terminal statuses, either way round.
        let m = merged(
            &store,
            &s(live(ReminderStatus::Snoozed, 5)),
            &s(live(ReminderStatus::Due, 9)),
        );
        assert_eq!(status(&m), Some(ReminderStatus::Due));
        let m = merged(
            &store,
            &s(live(ReminderStatus::Due, 5)),
            &s(live(ReminderStatus::Pending, 9)),
        );
        assert_eq!(
            status(&m),
            Some(ReminderStatus::Pending),
            "a recurring reminder's next run"
        );
        // A terminal status beats a newer non-terminal edit.
        let m = merged(
            &store,
            &s(live(ReminderStatus::Cancelled, 1)),
            &s(live(ReminderStatus::InProgress, 9)),
        );
        assert_eq!(status(&m), Some(ReminderStatus::Cancelled));
        // A deletion beats everything.
        let m = merged(
            &store,
            &s(ReminderEntry::Deleted { deleted_at: at(0) }),
            &s(live(ReminderStatus::Completed, 9)),
        );
        assert_eq!(status(&m), None);
    }

    fn one_epoch(e: Epoch) -> ControlState {
        let mut s = ControlState::default();
        s.epochs.insert(e.id.clone(), e);
        s
    }

    #[test]
    fn a_sealed_epoch_is_immutable() {
        let store = MemoryStorage::new();
        let active = one_epoch(epoch("e"));
        let first = sealed(epoch("e"), 3, &ids(10, "a"));
        let second = sealed(epoch("e"), 7, &ids(12, "b"));

        let m = merged(&store, &active, &one_epoch(first.clone()));
        assert_eq!(m.epochs["e"], first);

        // Two different seals: the earlier stays, and it's reported.
        let (x, y) = (
            state_root(&store, &one_epoch(first.clone())),
            state_root(&store, &one_epoch(second.clone())),
        );
        let result = merge(&store, &y, &x).unwrap();
        assert_eq!(read(&store, &result.root).unwrap().epochs["e"], first);
        assert_eq!(
            result.conflicts,
            vec![EpochConflict {
                epoch: "e".into(),
                kept_seal: first.seal_hash,
                dropped_seal: second.seal_hash,
            }]
        );

        // Archiving beats sealing; assigning a namespace beats none.
        let mut archived = first.clone();
        archived.status = EpochStatus::Archived;
        let m = merged(
            &store,
            &one_epoch(first.clone()),
            &one_epoch(archived.clone()),
        );
        assert_eq!(m.epochs["e"], archived);
        let mut unowned = epoch("e");
        unowned.namespace = None;
        let m = merged(&store, &one_epoch(unowned), &active);
        assert_eq!(m.epochs["e"].namespace.as_deref(), Some("default"));
    }

    #[test]
    fn epoch_membership_is_the_union() {
        let store = MemoryStorage::new();
        let (a, b) = (ids(5, "a"), ids(5, "b"));
        let mut x = ControlState::default();
        x.members.insert("e".into(), a.iter().copied().collect());
        let mut y = ControlState::default();
        y.members
            .insert("e".into(), b.iter().copied().chain([a[0]]).collect());
        let m = merged(&store, &x, &y);
        let want: BTreeSet<ObjectId> = a.into_iter().chain(b).collect();
        assert_eq!(m.members["e"], want);
    }

    #[test]
    fn records_on_one_side_only_are_kept_and_merging_is_idempotent_and_associative() {
        let store = MemoryStorage::new();
        let mut a = sample();
        a.taints.insert("only-a".into(), taint("only-a"));
        let mut b = sample();
        b.taints
            .insert("t1".into(), resolved(taint("t1"), 4, "person/b"));
        b.reminders
            .insert("r1".into(), live(ReminderStatus::Due, 50));
        b.members.get_mut("e1").unwrap().extend(ids(10, "more"));
        let mut c = ControlState::default();
        c.epochs
            .insert("e1".into(), sealed(epoch("e1"), 20, &ids(3, "c")));
        c.reminders
            .insert("r3".into(), live(ReminderStatus::Pending, 1));

        let (ra, rb, rc) = (
            state_root(&store, &a),
            state_root(&store, &b),
            state_root(&store, &c),
        );
        let ab = merge(&store, &ra, &rb).unwrap().root;
        assert_eq!(merge(&store, &ab, &ab).unwrap().root, ab, "idempotent");
        assert_eq!(merge(&store, &ab, &ra).unwrap().root, ab, "absorbs a side");
        let ab_c = merge(&store, &ab, &rc).unwrap().root;
        let bc = merge(&store, &rb, &rc).unwrap().root;
        let a_bc = merge(&store, &ra, &bc).unwrap().root;
        assert_eq!(ab_c, a_bc, "associative");

        let m = read(&store, &ab_c).unwrap();
        assert!(m.taints.contains_key("only-a"));
        assert!(m.taints["t1"].resolved_at.is_some());
        assert_eq!(
            m.reminders["r1"].live().unwrap().status,
            ReminderStatus::Due
        );
        assert!(m.reminders.contains_key("r3"));
        assert_eq!(m.epochs["e1"].status, EpochStatus::Sealed);
        assert_eq!(m.members["e1"].len(), 310);
    }

    #[test]
    fn a_newer_format_is_refused() {
        let store = MemoryStorage::new();
        let root = ControlEdit::new(&store).finish().unwrap();
        let mut top = load_map(&store, &root).unwrap();
        top.insert(FORMAT.into(), store.put_object(&Object::int(2)).unwrap());
        let newer = put_map(&store, top).unwrap();
        let err = read(&store, &newer).unwrap_err();
        assert!(err.to_string().contains("format 2"), "{err}");
        assert!(ControlEdit::open(&store, &newer).is_err());
    }
}
