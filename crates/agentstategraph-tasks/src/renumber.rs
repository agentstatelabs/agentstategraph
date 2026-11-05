//! Merge-safe task ids (DESIGN.md Plan G t-010).
//!
//! A plan mints `t-NNN` ids one past the highest it holds, so two stores
//! that add tasks to one plan while apart mint the same ids. Before merging
//! the other side in, [`TaskStore::renumber_before_merge`] moves this side's
//! colliding tasks past every id either side uses, and rewrites the
//! `blocked_by` and `parent_id` references to them, in one commit per plan.
//!
//! Like a rebase, it only ever moves work the other side hasn't seen: a task
//! already in the two sides' merge base was pushed or pulled, and keeps its
//! id. A task both sides hold with the same creation (time and author) is
//! the same task, not a collision. A plan both sides created apart, under
//! one name, is reported as a conflict and left alone.
//!
//! Pulling with renumbering, for a store whose tasks live in `tasks`:
//!
//! ```ignore
//! repo.fetch("hub", &remote, &["main"])?;
//! let renumbered = tasks.renumber_before_merge("main", &tracking_ref("hub", "main"))?;
//! repo.merge_fetched("hub", "main", options)?;
//! ```
//!
//! An embedder that refers to tasks from elsewhere (links from sessions,
//! say) rewrites those from [`Renumbering::renamed`].

use std::collections::{BTreeMap, BTreeSet};

use agentstategraph::RepoError;
use agentstategraph_core::ObjectId;
use serde_json::{Map, Value};

use crate::error::TaskStoreError;
use crate::paths;
use crate::store::TaskStore;
use crate::types::TaskId;

/// What [`TaskStore::renumber_before_merge`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Renumbering {
    /// Per plan, each moved task's old id and new one.
    pub renamed: BTreeMap<String, Vec<(TaskId, TaskId)>>,
    /// Plans both sides created apart under the same name. They're left
    /// as they are; merging them conflicts.
    pub plan_conflicts: Vec<String>,
    /// The commits that moved tasks, one per plan.
    pub commits: Vec<ObjectId>,
}

impl Renumbering {
    pub fn is_empty(&self) -> bool {
        self.renamed.is_empty() && self.plan_conflicts.is_empty()
    }

    /// The id `old` in `plan` now has, if it moved.
    pub fn new_id(&self, plan: &str, old: &TaskId) -> Option<&TaskId> {
        self.renamed
            .get(plan)?
            .iter()
            .find(|(from, _)| from == old)
            .map(|(_, to)| to)
    }
}

/// A plan's entries other than its metadata: its tasks, by id.
fn task_entries(plan: &Value) -> impl Iterator<Item = (&String, &Value)> {
    plan.as_object()
        .into_iter()
        .flatten()
        .filter(|(key, _)| key.as_str() != paths::META_KEY)
}

/// Whether two versions of a task are one task: created at the same time
/// by the same author.
fn same_task(a: &Value, b: &Value) -> bool {
    a.get("created_at") == b.get("created_at") && a.get("created_by") == b.get("created_by")
}

fn number(id: &str) -> Option<u32> {
    TaskId(id.to_string()).number().ok()
}

/// What to move in one plan: this side's version of it, the other side's,
/// and their merge base's (`None` if the plan wasn't there).
fn plan_moves(ours: &Value, theirs: &Value, base: Option<&Value>) -> Vec<(TaskId, TaskId)> {
    let base_ids: BTreeSet<&str> = base
        .map(|b| task_entries(b).map(|(k, _)| k.as_str()).collect())
        .unwrap_or_default();
    let theirs_tasks: BTreeMap<&str, &Value> =
        task_entries(theirs).map(|(k, v)| (k.as_str(), v)).collect();
    let mut colliding: Vec<&str> = task_entries(ours)
        .filter(|(id, _)| !base_ids.contains(id.as_str()))
        .filter(|(id, task)| {
            theirs_tasks
                .get(id.as_str())
                .is_some_and(|their| !same_task(task, their))
        })
        .map(|(id, _)| id.as_str())
        .collect();
    if colliding.is_empty() {
        return Vec::new();
    }
    colliding.sort_by_key(|id| (number(id), id.to_string()));
    let highest = task_entries(ours)
        .chain(task_entries(theirs))
        .filter_map(|(id, _)| number(id))
        .max()
        .unwrap_or(0);
    colliding
        .into_iter()
        .enumerate()
        .map(|(i, id)| (TaskId(id.to_string()), TaskId::new(highest + 1 + i as u32)))
        .collect()
}

/// `plan` with each task in `moves` under its new id, and every reference
/// to a moved task rewritten.
fn apply_moves(plan: &Value, moves: &[(TaskId, TaskId)]) -> Value {
    let rename = |id: &str| -> Option<String> {
        moves
            .iter()
            .find(|(from, _)| from.as_str() == id)
            .map(|(_, to)| to.as_str().to_string())
    };
    let mut out = Map::new();
    for (key, value) in plan.as_object().into_iter().flatten() {
        if key == paths::META_KEY {
            out.insert(key.clone(), value.clone());
            continue;
        }
        let mut task = value.clone();
        if let Some(fields) = task.as_object_mut() {
            if let Some(Value::String(parent)) = fields.get("parent_id")
                && let Some(to) = rename(parent)
            {
                fields.insert("parent_id".into(), Value::String(to));
            }
            if let Some(Value::Array(blockers)) = fields.get_mut("blocked_by") {
                for b in blockers.iter_mut() {
                    if let Some(to) = b.as_str().and_then(rename) {
                        *b = Value::String(to);
                    }
                }
            }
        }
        let new_key = rename(key).unwrap_or_else(|| key.clone());
        if let Some(fields) = task.as_object_mut()
            && new_key != *key
        {
            fields.insert("id".into(), Value::String(new_key.clone()));
        }
        out.insert(new_key, task);
    }
    Value::Object(out)
}

fn read(store: &TaskStore, ref_name: &str, path: &str) -> Result<Option<Value>, TaskStoreError> {
    match store.repo().get_json(ref_name, path) {
        Ok(v) => Ok(Some(v)),
        Err(RepoError::Tree(agentstategraph::tree::TreeError::PathNotFound(_))) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

impl TaskStore {
    /// Before merging `theirs` into `ref_name`, renumber the tasks this
    /// side added that collide with ones `theirs` added, as the
    /// [module docs](crate::renumber) describe. `theirs` is typically a
    /// remote-tracking ref after a fetch. Does nothing when nothing
    /// collides.
    pub fn renumber_before_merge(
        &self,
        ref_name: &str,
        theirs: &str,
    ) -> Result<Renumbering, TaskStoreError> {
        let mut report = Renumbering::default();
        let Some(our_plans) = read(self, ref_name, self.prefix())? else {
            return Ok(report);
        };
        let Some(their_plans) = read(self, theirs, self.prefix())? else {
            return Ok(report);
        };
        let names: Vec<String> = our_plans
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(name, _)| their_plans.get(name.as_str()).is_some())
            .map(|(name, _)| name.clone())
            .collect();

        for name in names {
            let root = paths::plan_root(self.prefix(), &name);
            for attempt in 0..Self::MAX_CAS_RETRIES {
                let head = self.repo().head(ref_name)?;
                let (Some(ours), Some(theirs_plan)) =
                    (read(self, ref_name, &root)?, read(self, theirs, &root)?)
                else {
                    break;
                };
                let base = self
                    .repo()
                    .get_json_at_merge_base(theirs, ref_name, &root)?;
                if base.is_none() && ours.get(paths::META_KEY) != theirs_plan.get(paths::META_KEY) {
                    report.plan_conflicts.push(name.clone());
                    break;
                }
                let moves = plan_moves(&ours, &theirs_plan, base.as_ref());
                if moves.is_empty() {
                    break;
                }
                let listed: Vec<String> = moves.iter().map(|(a, b)| format!("{a}→{b}")).collect();
                let renumbered = apply_moves(&ours, &moves);
                match self.repo().set_json_cas(
                    ref_name,
                    head,
                    &root,
                    &renumbered,
                    self.commit_opts(format!(
                        "Renumber tasks in {name} before merging {theirs}: {}",
                        listed.join(", ")
                    )),
                ) {
                    Ok(commit) => {
                        report.commits.push(commit);
                        report.renamed.insert(name.clone(), moves);
                        break;
                    }
                    Err(RepoError::WriteConflict) if attempt + 1 < Self::MAX_CAS_RETRIES => {
                        continue;
                    }
                    Err(RepoError::WriteConflict) => return Err(TaskStoreError::WriteConflict),
                    Err(e) => return Err(e.into()),
                }
            }
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn task(id: &str, at: &str, blocked_by: &[&str], parent: Option<&str>) -> Value {
        json!({
            "id": id, "title": id, "created_at": at, "created_by": "a",
            "blocked_by": blocked_by, "parent_id": parent,
        })
    }

    #[test]
    fn only_new_colliding_tasks_move_past_both_sides() {
        let base = json!({"_meta": {}, "t-001": task("t-001", "0", &[], None)});
        let ours = json!({
            "_meta": {},
            "t-001": task("t-001", "0", &["t-002"], None),
            "t-002": task("t-002", "ours", &[], None),
            "t-003": task("t-003", "ours2", &["t-002"], Some("t-002")),
        });
        let theirs = json!({
            "_meta": {},
            "t-001": task("t-001", "0", &[], None),
            "t-002": task("t-002", "theirs", &[], None),
        });
        let moves = plan_moves(&ours, &theirs, Some(&base));
        assert_eq!(moves, vec![(TaskId::new(2), TaskId::new(4))]);
        let out = apply_moves(&ours, &moves);
        assert!(out.get("t-002").is_none());
        assert_eq!(out["t-004"]["id"], "t-004");
        assert_eq!(out["t-003"]["blocked_by"], json!(["t-004"]));
        assert_eq!(out["t-003"]["parent_id"], "t-004");
        assert_eq!(
            out["t-001"]["blocked_by"],
            json!(["t-004"]),
            "a base task's reference to it too"
        );
    }

    #[test]
    fn a_task_both_sides_hold_is_not_a_collision() {
        let same = task("t-002", "x", &[], None);
        let ours = json!({"_meta": {}, "t-002": same.clone()});
        let theirs = json!({"_meta": {}, "t-002": same});
        assert!(plan_moves(&ours, &theirs, None).is_empty());
    }
}
