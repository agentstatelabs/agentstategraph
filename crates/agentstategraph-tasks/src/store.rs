//! `TaskStore` — handle bound to a `Repository` + path prefix.
//!
//! All task and plan operations go through a `TaskStore`. Writes are
//! committed to the repository with `IntentCategory::Plan`, so plan
//! activity is natively filterable in log and blame queries. Multi-path
//! transitions (e.g. `complete_task` updating the task and the plan's
//! `_meta` together) use the speculation API so they land in a single
//! commit.

use std::sync::Arc;

use agentstategraph::{CommitOptions, Repository};
use agentstategraph_core::IntentCategory;
use chrono::Utc;
use once_cell::sync::Lazy;
use regex::Regex;

/// Canonical blocker id pattern: `t-` followed by 1..9 digits. Validated
/// at the substrate layer so malformed blocker ids (path-traversal
/// attempts, nonsense strings) never reach the tree walker.
static BLOCKER_ID_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^t-\d{1,9}$").expect("static blocker-id regex must compile"));

fn validate_blocker_ids(blockers: &[TaskId]) -> Result<(), TaskStoreError> {
    for b in blockers {
        if !BLOCKER_ID_RE.is_match(&b.0) {
            return Err(TaskStoreError::InvalidBlockerId(b.0.clone()));
        }
    }
    Ok(())
}

use crate::error::TaskStoreError;
use crate::paths;
use crate::state::{Transition, check_transition};
use crate::types::{Plan, PlanStatus, Priority, Proof, Task, TaskId, TaskStatus};
use crate::verifier::{Verifier, VerifyEntry, VerifyReport};

/// A handle bound to a `Repository` and a path prefix. All operations on
/// plans and tasks go through a `TaskStore`.
pub struct TaskStore {
    repo: Arc<Repository>,
    prefix: String,
    agent_id: String,
}

impl TaskStore {
    /// Create a new task store bound to a path prefix in the given
    /// repository. The prefix must NOT end with a slash.
    ///
    /// `agent_id` is used as the commit author for every task operation.
    pub fn new(
        repo: Arc<Repository>,
        prefix: impl Into<String>,
        agent_id: impl Into<String>,
    ) -> Self {
        let mut prefix = prefix.into();
        if prefix.ends_with('/') {
            prefix.pop();
        }
        Self {
            repo,
            prefix,
            agent_id: agent_id.into(),
        }
    }

    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    // -----------------------------------------------------------------------
    // Plan operations
    // -----------------------------------------------------------------------

    /// Create a new plan. Fails if a plan with the same name already exists.
    pub fn create_plan(
        &self,
        ref_name: &str,
        name: &str,
        description: Option<String>,
    ) -> Result<Plan, TaskStoreError> {
        if self.plan_exists(ref_name, name)? {
            return Err(TaskStoreError::PlanAlreadyExists(name.to_string()));
        }

        let plan = Plan {
            name: name.to_string(),
            description,
            status: PlanStatus::Active,
            created_at: Utc::now(),
            created_by: self.agent_id.clone(),
            archived_at: None,
        };

        let meta_path = paths::plan_meta(&self.prefix, name);
        let value = serde_json::to_value(&plan)?;
        self.repo.set_json(
            ref_name,
            &meta_path,
            &value,
            self.commit_opts(format!("Create plan {}", name)),
        )?;

        Ok(plan)
    }

    /// List every plan under the store's prefix. Returns plans in the
    /// order they are stored (alphabetical by name, since state trees
    /// use BTreeMap underneath).
    pub fn list_plans(&self, ref_name: &str) -> Result<Vec<Plan>, TaskStoreError> {
        let root = match self.repo.get_json(ref_name, &self.prefix) {
            Ok(v) => v,
            Err(e) if is_path_not_found(&e) => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };

        let serde_json::Value::Object(map) = root else {
            return Ok(Vec::new());
        };

        let mut plans = Vec::new();
        for name in map.keys() {
            let plan = self.get_plan(ref_name, name)?;
            plans.push(plan);
        }
        Ok(plans)
    }

    /// Fetch a single plan by name.
    pub fn get_plan(&self, ref_name: &str, name: &str) -> Result<Plan, TaskStoreError> {
        let meta_path = paths::plan_meta(&self.prefix, name);
        let value = self
            .repo
            .get_json(ref_name, &meta_path)
            .map_err(|e| map_not_found(e, || TaskStoreError::PlanNotFound(name.to_string())))?;
        Ok(serde_json::from_value(value)?)
    }

    /// Archive a plan — a soft, reversible transition that marks the
    /// plan as no longer active but leaves all task data intact.
    pub fn archive_plan(&self, ref_name: &str, name: &str) -> Result<Plan, TaskStoreError> {
        let mut plan = self.get_plan(ref_name, name)?;
        plan.status = PlanStatus::Archived;
        plan.archived_at = Some(Utc::now());

        let meta_path = paths::plan_meta(&self.prefix, name);
        let value = serde_json::to_value(&plan)?;
        self.repo.set_json(
            ref_name,
            &meta_path,
            &value,
            self.commit_opts(format!("Archive plan {}", name)),
        )?;
        Ok(plan)
    }

    /// Delete a plan and every task it contains. Destructive — use
    /// `archive_plan` if the history should remain discoverable.
    pub fn delete_plan(&self, ref_name: &str, name: &str) -> Result<(), TaskStoreError> {
        if !self.plan_exists(ref_name, name)? {
            return Err(TaskStoreError::PlanNotFound(name.to_string()));
        }
        let root = paths::plan_root(&self.prefix, name);
        self.repo.delete(
            ref_name,
            &root,
            self.commit_opts(format!("Delete plan {}", name)),
        )?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Task operations
    // -----------------------------------------------------------------------

    /// Add a new task to a plan. Task ids are assigned monotonically —
    /// the next available `t-NNN` is picked by scanning existing entries.
    /// `assigned_to` is optional — pass an agent id to record ownership.
    #[allow(clippy::too_many_arguments)]
    pub fn add_task(
        &self,
        ref_name: &str,
        plan: &str,
        title: &str,
        priority: Priority,
        parent_id: Option<TaskId>,
        blocked_by: Vec<TaskId>,
        assigned_to: Option<String>,
    ) -> Result<Task, TaskStoreError> {
        if !self.plan_exists(ref_name, plan)? {
            return Err(TaskStoreError::PlanNotFound(plan.to_string()));
        }

        if let Some(ref parent) = parent_id {
            let parent_task = self.get_task(ref_name, plan, parent)?;
            if parent_task.parent_id.is_some() {
                return Err(TaskStoreError::ParentIsSubtask(parent.clone()));
            }
        }

        validate_blocker_ids(&blocked_by)?;
        for blocker in &blocked_by {
            self.get_task(ref_name, plan, blocker)?;
        }

        let next_num = self.next_task_number(ref_name, plan)?;
        let id = TaskId::new(next_num);

        let task = Task {
            id: id.clone(),
            title: title.to_string(),
            status: TaskStatus::Pending,
            priority,
            parent_id,
            blocked_by,
            created_at: Utc::now(),
            created_by: self.agent_id.clone(),
            started_at: None,
            started_by: None,
            completed_at: None,
            completed_by: None,
            proof: None,
            abandoned_at: None,
            abandoned_reason: None,
            assigned_to,
        };

        let task_path = paths::task(&self.prefix, plan, &id);
        let value = serde_json::to_value(&task)?;
        self.repo.set_json(
            ref_name,
            &task_path,
            &value,
            self.commit_opts(format!("Add task {}/{}", plan, id)),
        )?;

        Ok(task)
    }

    /// List every task id in a plan, cheaply — this walks tree paths
    /// without deserializing task bodies. Use this when you only need
    /// to know which tasks exist (e.g. picking the next task number,
    /// checking blocker existence). Prefer `list_tasks` when you need
    /// task data.
    pub fn task_ids(&self, ref_name: &str, plan: &str) -> Result<Vec<TaskId>, TaskStoreError> {
        let root_path = paths::plan_root(&self.prefix, plan);
        let leaves = match self.repo.list_paths(ref_name, &root_path, None) {
            Ok(v) => v,
            Err(e) if is_path_not_found(&e) => {
                return Err(TaskStoreError::PlanNotFound(plan.to_string()));
            }
            Err(e) => return Err(e.into()),
        };

        let prefix_with_slash = format!("{}/", root_path);
        let mut ids: std::collections::BTreeSet<TaskId> = std::collections::BTreeSet::new();
        for leaf in leaves {
            let Some(suffix) = leaf.strip_prefix(&prefix_with_slash) else {
                continue;
            };
            let first = suffix.split('/').next().unwrap_or("");
            if first.is_empty() || first == paths::META_KEY {
                continue;
            }
            ids.insert(TaskId(first.to_string()));
        }
        Ok(ids.into_iter().collect())
    }

    /// List every task in a plan. Ordered by task id. This fully
    /// deserializes every task — O(n) in total field count. For id-only
    /// operations prefer `task_ids`.
    pub fn list_tasks(&self, ref_name: &str, plan: &str) -> Result<Vec<Task>, TaskStoreError> {
        if !self.plan_exists(ref_name, plan)? {
            return Err(TaskStoreError::PlanNotFound(plan.to_string()));
        }

        let root_path = paths::plan_root(&self.prefix, plan);
        let root = self.repo.get_json(ref_name, &root_path)?;

        let serde_json::Value::Object(map) = root else {
            return Ok(Vec::new());
        };

        let mut tasks = Vec::new();
        for (key, value) in map.iter() {
            if key == paths::META_KEY {
                continue;
            }
            let task: Task = serde_json::from_value(value.clone())?;
            tasks.push(task);
        }
        tasks.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(tasks)
    }

    pub fn get_task(
        &self,
        ref_name: &str,
        plan: &str,
        id: &TaskId,
    ) -> Result<Task, TaskStoreError> {
        let path = paths::task(&self.prefix, plan, id);
        let value = self.repo.get_json(ref_name, &path).map_err(|e| {
            map_not_found(e, || TaskStoreError::TaskNotFound {
                plan: plan.to_string(),
                id: id.clone(),
            })
        })?;
        Ok(serde_json::from_value(value)?)
    }

    /// Transition `pending → in_progress`. Fails with `Blocked` if any
    /// blocker is not `done`, or `BlockerNotFound` if any blocker id no
    /// longer exists in the plan (e.g. because the plan was rebuilt
    /// after a delete).
    pub fn start_task(
        &self,
        ref_name: &str,
        plan: &str,
        id: &TaskId,
    ) -> Result<Task, TaskStoreError> {
        let mut task = self.get_task(ref_name, plan, id)?;
        check_transition(task.status, Transition::Start)?;

        let BlockerCheck { missing, pending } =
            self.classify_blockers(ref_name, plan, &task.blocked_by)?;
        if !missing.is_empty() {
            return Err(TaskStoreError::BlockerNotFound { blockers: missing });
        }
        if !pending.is_empty() {
            return Err(TaskStoreError::Blocked { blockers: pending });
        }

        task.status = TaskStatus::InProgress;
        task.started_at = Some(Utc::now());
        task.started_by = Some(self.agent_id.clone());

        self.write_task(ref_name, plan, &task, format!("Start {}/{}", plan, id))?;
        Ok(task)
    }

    /// Transition `in_progress → done`. Requires a `Proof` — it's stored
    /// but NOT verified here (use `verify_plan` for that). If this is the
    /// last open task in the plan, the plan's `_meta` is also promoted to
    /// `Completed` in the same commit.
    pub fn complete_task(
        &self,
        ref_name: &str,
        plan: &str,
        id: &TaskId,
        proof: Proof,
    ) -> Result<Task, TaskStoreError> {
        let mut task = self.get_task(ref_name, plan, id)?;
        check_transition(task.status, Transition::Complete)?;

        task.status = TaskStatus::Done;
        task.proof = Some(proof);
        task.completed_at = Some(Utc::now());
        task.completed_by = Some(self.agent_id.clone());

        self.commit_terminal_transition(
            ref_name,
            plan,
            &task,
            format!("Complete {}/{}", plan, id),
        )?;
        Ok(task)
    }

    /// Shared back-end for `complete_task` and `abandon_task` — writes
    /// the task in its new terminal state and, if the plan's open-task
    /// queue is now empty, promotes the plan's `_meta` to `Completed`
    /// in the same commit.
    fn commit_terminal_transition(
        &self,
        ref_name: &str,
        plan: &str,
        task: &Task,
        desc: String,
    ) -> Result<(), TaskStoreError> {
        debug_assert!(task.status.is_terminal());

        let all_tasks = self.list_tasks(ref_name, plan)?;
        let all_terminal = all_tasks
            .iter()
            .filter(|t| t.id != task.id)
            .all(|t| t.status.is_terminal())
            && all_tasks.iter().any(|t| t.id == task.id);

        let mut plan_meta_update: Option<Plan> = None;
        if all_terminal {
            let mut plan_meta = self.get_plan(ref_name, plan)?;
            if plan_meta.status == PlanStatus::Active {
                plan_meta.status = PlanStatus::Completed;
                plan_meta_update = Some(plan_meta);
            }
        }

        let task_path = paths::task(&self.prefix, plan, &task.id);
        let task_value = serde_json::to_value(task)?;

        if let Some(plan_meta) = plan_meta_update {
            let meta_path = paths::plan_meta(&self.prefix, plan);
            let meta_value = serde_json::to_value(&plan_meta)?;

            let handle = self.repo.speculate(ref_name, Some(desc.clone()))?;
            self.repo.spec_set_json(handle, &task_path, &task_value)?;
            self.repo.spec_set_json(handle, &meta_path, &meta_value)?;
            self.repo
                .commit_speculation(handle, self.commit_opts(desc))?;
        } else {
            self.repo
                .set_json(ref_name, &task_path, &task_value, self.commit_opts(desc))?;
        }

        Ok(())
    }

    /// Transition to `abandoned`. Legal from both `pending` and
    /// `in_progress`. Reason is required. If this is the last open
    /// task in the plan, the plan's `_meta` is also promoted to
    /// `Completed` in the same commit — mirroring `complete_task` so
    /// the invariant "plan is `Completed` iff every task is terminal"
    /// always holds.
    pub fn abandon_task(
        &self,
        ref_name: &str,
        plan: &str,
        id: &TaskId,
        reason: &str,
    ) -> Result<Task, TaskStoreError> {
        if reason.trim().is_empty() {
            return Err(TaskStoreError::ReasonRequired);
        }

        let mut task = self.get_task(ref_name, plan, id)?;
        check_transition(task.status, Transition::Abandon)?;

        task.status = TaskStatus::Abandoned;
        task.abandoned_at = Some(Utc::now());
        task.abandoned_reason = Some(reason.to_string());

        self.commit_terminal_transition(ref_name, plan, &task, format!("Abandon {}/{}", plan, id))?;
        Ok(task)
    }

    pub fn set_priority(
        &self,
        ref_name: &str,
        plan: &str,
        id: &TaskId,
        priority: Priority,
    ) -> Result<Task, TaskStoreError> {
        let mut task = self.get_task(ref_name, plan, id)?;
        task.priority = priority;
        self.write_task(
            ref_name,
            plan,
            &task,
            format!("Set priority {:?} on {}/{}", priority, plan, id),
        )?;
        Ok(task)
    }

    pub fn set_blockers(
        &self,
        ref_name: &str,
        plan: &str,
        id: &TaskId,
        blockers: Vec<TaskId>,
    ) -> Result<Task, TaskStoreError> {
        validate_blocker_ids(&blockers)?;
        for blocker in &blockers {
            self.get_task(ref_name, plan, blocker)?;
        }
        let mut task = self.get_task(ref_name, plan, id)?;
        task.blocked_by = blockers;
        self.write_task(
            ref_name,
            plan,
            &task,
            format!("Update blockers on {}/{}", plan, id),
        )?;
        Ok(task)
    }

    /// Set `assigned_to` on a task.
    pub fn assign_task(
        &self,
        ref_name: &str,
        plan: &str,
        id: &TaskId,
        agent: &str,
    ) -> Result<Task, TaskStoreError> {
        let mut task = self.get_task(ref_name, plan, id)?;
        task.assigned_to = Some(agent.to_string());
        self.write_task(
            ref_name,
            plan,
            &task,
            format!("Assign {}/{} to {}", plan, id, agent),
        )?;
        Ok(task)
    }

    /// Clear `assigned_to` on a task.
    pub fn unassign_task(
        &self,
        ref_name: &str,
        plan: &str,
        id: &TaskId,
    ) -> Result<Task, TaskStoreError> {
        let mut task = self.get_task(ref_name, plan, id)?;
        task.assigned_to = None;
        self.write_task(ref_name, plan, &task, format!("Unassign {}/{}", plan, id))?;
        Ok(task)
    }

    // -----------------------------------------------------------------------
    // Query helpers
    // -----------------------------------------------------------------------

    /// Highest-priority `pending` task whose blockers are all `done`.
    /// Ties broken by ascending task id (insertion order).
    pub fn next_task(&self, ref_name: &str, plan: &str) -> Result<Option<Task>, TaskStoreError> {
        self.next_task_for(ref_name, plan, None, true)
    }

    /// Like `next_task`, but filter by assignment.
    ///
    /// - `assigned_to = None` → any pending unblocked task (same as
    ///   `next_task`).
    /// - `assigned_to = Some(agent)`, `include_unassigned = true` →
    ///   tasks assigned to `agent` OR unassigned.
    /// - `assigned_to = Some(agent)`, `include_unassigned = false` →
    ///   only tasks explicitly assigned to `agent`.
    pub fn next_task_for(
        &self,
        ref_name: &str,
        plan: &str,
        assigned_to: Option<&str>,
        include_unassigned: bool,
    ) -> Result<Option<Task>, TaskStoreError> {
        let tasks = self.list_tasks(ref_name, plan)?;
        let mut candidates: Vec<&Task> = tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Pending)
            .filter(|t| t.blocked_by.iter().all(|b| blocker_satisfied(&tasks, b)))
            .filter(|t| match assigned_to {
                None => true,
                Some(agent) => match &t.assigned_to {
                    Some(a) => a == agent,
                    None => include_unassigned,
                },
            })
            .collect();
        candidates.sort_by(|a, b| b.priority.cmp(&a.priority).then(a.id.cmp(&b.id)));
        Ok(candidates.first().map(|t| (*t).clone()))
    }

    /// List plans filtered by status. `None` returns all plans.
    pub fn list_plans_by_status(
        &self,
        ref_name: &str,
        status: Option<PlanStatus>,
    ) -> Result<Vec<Plan>, TaskStoreError> {
        let plans = self.list_plans(ref_name)?;
        match status {
            None => Ok(plans),
            Some(s) => Ok(plans.into_iter().filter(|p| p.status == s).collect()),
        }
    }

    /// Compute the rollup status of a parent task from its direct
    /// subtasks. Read-only — does not mutate storage.
    ///
    /// Rules:
    /// - `Done` if every non-abandoned subtask is `Done`.
    /// - `InProgress` if any subtask is `InProgress`.
    /// - `Pending` otherwise.
    ///
    /// If the parent has no subtasks, returns the parent's own status.
    pub fn derived_status(
        &self,
        ref_name: &str,
        plan: &str,
        parent_id: &TaskId,
    ) -> Result<TaskStatus, TaskStoreError> {
        let tasks = self.list_tasks(ref_name, plan)?;
        let parent = tasks.iter().find(|t| &t.id == parent_id).ok_or_else(|| {
            TaskStoreError::TaskNotFound {
                plan: plan.to_string(),
                id: parent_id.clone(),
            }
        })?;

        let subtasks: Vec<&Task> = tasks
            .iter()
            .filter(|t| t.parent_id.as_ref() == Some(parent_id))
            .collect();

        if subtasks.is_empty() {
            return Ok(parent.status);
        }

        if subtasks.iter().any(|t| t.status == TaskStatus::InProgress) {
            return Ok(TaskStatus::InProgress);
        }

        let non_abandoned: Vec<&&Task> = subtasks
            .iter()
            .filter(|t| t.status != TaskStatus::Abandoned)
            .collect();

        if !non_abandoned.is_empty() && non_abandoned.iter().all(|t| t.status == TaskStatus::Done) {
            return Ok(TaskStatus::Done);
        }

        Ok(TaskStatus::Pending)
    }

    /// Walk every `done` task and run the given verifier against each proof.
    /// Non-`done` tasks are skipped.
    pub fn verify_plan(
        &self,
        ref_name: &str,
        plan: &str,
        verifier: &dyn Verifier,
    ) -> Result<VerifyReport, TaskStoreError> {
        let tasks = self.list_tasks(ref_name, plan)?;
        let mut results = Vec::new();
        for task in tasks.iter().filter(|t| t.status == TaskStatus::Done) {
            let proof = task.proof.as_ref().ok_or(TaskStoreError::ProofRequired)?;
            let outcome = verifier.verify(proof);
            results.push(VerifyEntry {
                task_id: task.id.clone(),
                result: outcome,
            });
        }
        Ok(VerifyReport {
            plan: plan.to_string(),
            results,
        })
    }

    // -----------------------------------------------------------------------
    // Internals
    // -----------------------------------------------------------------------

    fn plan_exists(&self, ref_name: &str, name: &str) -> Result<bool, TaskStoreError> {
        match self.get_plan(ref_name, name) {
            Ok(_) => Ok(true),
            Err(TaskStoreError::PlanNotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    fn next_task_number(&self, ref_name: &str, plan: &str) -> Result<u32, TaskStoreError> {
        let ids = self.task_ids(ref_name, plan)?;
        let max = ids
            .iter()
            .filter_map(|id| id.number().ok())
            .max()
            .unwrap_or(0);
        Ok(max + 1)
    }

    fn classify_blockers(
        &self,
        ref_name: &str,
        plan: &str,
        blockers: &[TaskId],
    ) -> Result<BlockerCheck, TaskStoreError> {
        if blockers.is_empty() {
            return Ok(BlockerCheck::default());
        }
        let tasks = self.list_tasks(ref_name, plan)?;
        let mut missing = Vec::new();
        let mut pending = Vec::new();
        for b in blockers {
            match tasks.iter().find(|t| &t.id == b) {
                None => missing.push(b.clone()),
                Some(t) if t.status != TaskStatus::Done => pending.push(b.clone()),
                _ => {}
            }
        }
        Ok(BlockerCheck { missing, pending })
    }

    fn write_task(
        &self,
        ref_name: &str,
        plan: &str,
        task: &Task,
        description: String,
    ) -> Result<(), TaskStoreError> {
        let path = paths::task(&self.prefix, plan, &task.id);
        let value = serde_json::to_value(task)?;
        self.repo
            .set_json(ref_name, &path, &value, self.commit_opts(description))?;
        Ok(())
    }

    fn commit_opts(&self, description: impl Into<String>) -> CommitOptions {
        CommitOptions::new(&self.agent_id, IntentCategory::Plan, description)
    }
}

/// A blocker is satisfied iff it names a task in the same plan whose
/// status is `Done`. Abandoned blockers are NOT considered satisfied —
/// an agent who abandons a blocker must explicitly re-open the
/// blockers on the dependent task (via `set_blockers`) before starting
/// it. Missing blockers (the task no longer exists) are also not
/// satisfied and surface as `BlockerNotFound` at `start_task` time.
fn blocker_satisfied(tasks: &[Task], id: &TaskId) -> bool {
    tasks
        .iter()
        .any(|t| &t.id == id && t.status == TaskStatus::Done)
}

#[derive(Default, Debug)]
struct BlockerCheck {
    /// Blocker ids that are not currently `Done` but DO exist in the plan.
    pending: Vec<TaskId>,
    /// Blocker ids that no longer resolve to any task in the plan.
    missing: Vec<TaskId>,
}

fn is_path_not_found(e: &agentstategraph::RepoError) -> bool {
    matches!(e, agentstategraph::RepoError::Tree(_))
}

fn map_not_found<F>(e: agentstategraph::RepoError, make: F) -> TaskStoreError
where
    F: FnOnce() -> TaskStoreError,
{
    if is_path_not_found(&e) {
        make()
    } else {
        e.into()
    }
}
