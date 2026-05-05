//! Plan & task tool implementations.

use agentstategraph_tasks::{PlanStatus, Priority, Proof, TaskId, TaskStatus};

use super::{
    AbandonTaskParams, AddTaskParams, AgentStateGraphServer, AssignTaskParams, CompleteTaskParams,
    CreatePlanParams, GetPlanParams, ListPlansParams, ListTasksParams, NextTaskParams,
    TaskActionParams,
};

impl AgentStateGraphServer {
    pub(super) fn impl_create_plan(&self, p: CreatePlanParams) -> String {
        match self.tasks.create_plan(&p.r#ref, &p.name, p.description) {
            Ok(plan) => serde_json::to_string_pretty(&serde_json::json!({
                "name": plan.name,
                "status": format!("{:?}", plan.status),
                "created_at": plan.created_at.to_rfc3339(),
                "created_by": plan.created_by,
            }))
            .unwrap_or_default(),
            Err(e) => format!("Error: {}", e),
        }
    }

    pub(super) fn impl_list_plans(&self, p: ListPlansParams) -> String {
        let status = p.status.map(|s| match s.to_lowercase().as_str() {
            "active" => PlanStatus::Active,
            "completed" => PlanStatus::Completed,
            "archived" => PlanStatus::Archived,
            _ => PlanStatus::Active,
        });
        match self.tasks.list_plans_by_status(&p.r#ref, status) {
            Ok(plans) => {
                let json: Vec<serde_json::Value> = plans
                    .iter()
                    .map(|p| {
                        serde_json::json!({
                            "name": p.name,
                            "description": p.description,
                            "status": format!("{:?}", p.status),
                            "created_at": p.created_at.to_rfc3339(),
                        })
                    })
                    .collect();
                format!(
                    "{} plans:\n{}",
                    json.len(),
                    serde_json::to_string_pretty(&json).unwrap_or_default()
                )
            }
            Err(e) => format!("Error: {}", e),
        }
    }

    pub(super) fn impl_get_plan(&self, p: GetPlanParams) -> String {
        match self.tasks.get_plan(&p.r#ref, &p.name) {
            Ok(plan) => {
                let tasks = self.tasks.list_tasks(&p.r#ref, &p.name).unwrap_or_default();
                let pending = tasks
                    .iter()
                    .filter(|t| matches!(t.status, TaskStatus::Pending))
                    .count();
                let in_progress = tasks
                    .iter()
                    .filter(|t| matches!(t.status, TaskStatus::InProgress))
                    .count();
                let done = tasks
                    .iter()
                    .filter(|t| matches!(t.status, TaskStatus::Done))
                    .count();
                serde_json::to_string_pretty(&serde_json::json!({
                    "name": plan.name,
                    "description": plan.description,
                    "status": format!("{:?}", plan.status),
                    "created_at": plan.created_at.to_rfc3339(),
                    "task_count": tasks.len(),
                    "pending": pending,
                    "in_progress": in_progress,
                    "done": done,
                }))
                .unwrap_or_default()
            }
            Err(e) => format!("Error: {}", e),
        }
    }

    pub(super) fn impl_add_task(&self, p: AddTaskParams) -> String {
        let priority = match p
            .priority
            .as_deref()
            .unwrap_or("Medium")
            .to_lowercase()
            .as_str()
        {
            "low" => Priority::Low,
            "high" => Priority::High,
            "critical" => Priority::Critical,
            _ => Priority::Medium,
        };
        let parent_id = p.parent_id.map(TaskId);
        let blocked_by = p
            .blocked_by
            .unwrap_or_default()
            .into_iter()
            .map(TaskId)
            .collect();
        match self.tasks.add_task(
            &p.r#ref,
            &p.plan,
            &p.title,
            priority,
            parent_id,
            blocked_by,
            p.assigned_to,
        ) {
            Ok(task) => serde_json::to_string_pretty(&serde_json::json!({
                "id": task.id.as_str(),
                "title": task.title,
                "status": format!("{:?}", task.status),
                "priority": format!("{:?}", task.priority),
                "assigned_to": task.assigned_to,
            }))
            .unwrap_or_default(),
            Err(e) => format!("Error: {}", e),
        }
    }

    pub(super) fn impl_list_tasks(&self, p: ListTasksParams) -> String {
        match self.tasks.list_tasks(&p.r#ref, &p.plan) {
            Ok(tasks) => {
                let json: Vec<serde_json::Value> = tasks.iter().map(|t| {
                    let mut v = serde_json::json!({
                        "id": t.id.as_str(),
                        "title": t.title,
                        "status": format!("{:?}", t.status),
                        "priority": format!("{:?}", t.priority),
                        "assigned_to": t.assigned_to,
                        "blocked_by": t.blocked_by.iter().map(|b| b.as_str().to_string()).collect::<Vec<_>>(),
                    });
                    if let Some(ref proof) = t.proof {
                        v["proof"] = serde_json::json!({
                            "kind": format!("{:?}", proof.kind),
                            "value": proof.value,
                            "note": proof.note,
                        });
                    }
                    v
                }).collect();
                format!(
                    "{} tasks:\n{}",
                    json.len(),
                    serde_json::to_string_pretty(&json).unwrap_or_default()
                )
            }
            Err(e) => format!("Error: {}", e),
        }
    }

    pub(super) fn impl_start_task(&self, p: TaskActionParams) -> String {
        match self.tasks.start_task(&p.r#ref, &p.plan, &TaskId(p.task_id)) {
            Ok(task) => format!(
                "Task {} started (was: Pending → now: InProgress)",
                task.id.as_str()
            ),
            Err(e) => format!("Error: {}", e),
        }
    }

    pub(super) fn impl_complete_task(&self, p: CompleteTaskParams) -> String {
        let proof = match p.proof_kind.to_lowercase().as_str() {
            "commit" => Proof::commit(p.proof_value),
            "file" => Proof::file(p.proof_value),
            "test" => Proof::test(p.proof_value),
            _ => Proof::text(p.proof_value),
        };
        let proof = if let Some(note) = p.proof_note {
            proof.with_note(note)
        } else {
            proof
        };
        match self
            .tasks
            .complete_task(&p.r#ref, &p.plan, &TaskId(p.task_id), proof)
        {
            Ok(task) => format!("Task {} completed (InProgress → Done)", task.id.as_str()),
            Err(e) => format!("Error: {}", e),
        }
    }

    pub(super) fn impl_abandon_task(&self, p: AbandonTaskParams) -> String {
        match self
            .tasks
            .abandon_task(&p.r#ref, &p.plan, &TaskId(p.task_id), &p.reason)
        {
            Ok(task) => format!("Task {} abandoned: {}", task.id.as_str(), p.reason),
            Err(e) => format!("Error: {}", e),
        }
    }

    pub(super) fn impl_assign_task(&self, p: AssignTaskParams) -> String {
        match self
            .tasks
            .assign_task(&p.r#ref, &p.plan, &TaskId(p.task_id), &p.agent)
        {
            Ok(task) => format!("Task {} assigned to {}", task.id.as_str(), p.agent),
            Err(e) => format!("Error: {}", e),
        }
    }

    pub(super) fn impl_next_task(&self, p: NextTaskParams) -> String {
        let result = if let Some(agent) = p.agent {
            self.tasks
                .next_task_for(&p.r#ref, &p.plan, Some(&agent), true)
        } else {
            self.tasks.next_task(&p.r#ref, &p.plan)
        };
        match result {
            Ok(Some(task)) => serde_json::to_string_pretty(&serde_json::json!({
                "id": task.id.as_str(),
                "title": task.title,
                "status": format!("{:?}", task.status),
                "priority": format!("{:?}", task.priority),
                "assigned_to": task.assigned_to,
                "blocked_by": task.blocked_by.iter().map(|b| b.as_str().to_string()).collect::<Vec<_>>(),
            })).unwrap_or_default(),
            Ok(None) => "No pending tasks".to_string(),
            Err(e) => format!("Error: {}", e),
        }
    }
}
