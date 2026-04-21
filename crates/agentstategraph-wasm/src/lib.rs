//! WASM bindings for AgentStateGraph — runs in browsers, Deno, and Node.
//!
//! Usage (browser/Deno):
//!   import init, { WasmAgentStateGraph } from './agentstategraph_wasm.js'
//!   await init()
//!   const asg = new WasmAgentStateGraph()
//!   asg.set("/name", "my-cluster", "Checkpoint", "init")
//!   asg.get("/name")  // → '"my-cluster"'

// Binding glue: exported wasm-bindgen functions mirror the JS-side
// call shape which has no natural way to collapse into fewer args.
#![allow(clippy::too_many_arguments)]

use std::sync::Arc;

use wasm_bindgen::prelude::*;

use agentstategraph::session::SessionStatus;
use agentstategraph::speculation::SpecHandle;
use agentstategraph::{CommitOptions, Repository, SCHEMA_VERSION};
use agentstategraph_core::{IntentCategory, Object};
use agentstategraph_migrate::{CheckResult, Registry, RunMode, StepStatus};
use agentstategraph_policy::{ChangeProposal, Policy, PolicyStore as PolicyBackend, Situation};
use agentstategraph_storage::IndexedDbStorage;
use agentstategraph_tasks::{OnCompleteHook, Priority, Proof, ProofKind, TaskId, TaskStore};
use semver::Version;

fn parse_category(s: &str) -> IntentCategory {
    match s.to_lowercase().as_str() {
        "explore" => IntentCategory::Explore,
        "refine" => IntentCategory::Refine,
        "fix" => IntentCategory::Fix,
        "rollback" => IntentCategory::Rollback,
        "checkpoint" => IntentCategory::Checkpoint,
        "merge" => IntentCategory::Merge,
        // SECURITY (threat model v2, finding C3): the WASM boundary runs
        // untrusted browser/Deno code with no capability check. Map
        // "migrate" to a Custom category so `/_meta/*` writes are rejected
        // by the substrate's reserved-path guard. Migration tooling lives
        // outside WASM.
        "migrate" => IntentCategory::Custom("Migrate-requested".into()),
        "plan" => IntentCategory::Plan,
        other => IntentCategory::Custom(other.to_string()),
    }
}

fn make_opts(
    description: &str,
    category: &str,
    reasoning: Option<String>,
    confidence: Option<f64>,
) -> CommitOptions {
    let cat = parse_category(category);
    let mut opts = CommitOptions::new("wasm", cat, description);
    if let Some(r) = reasoning {
        opts = opts.with_reasoning(r);
    }
    if let Some(c) = confidence {
        opts = opts.with_confidence(c);
    }
    opts
}

/// AgentStateGraph for WASM — uses IndexedDB for persistent browser storage.
///
/// Architecture: in-memory cache with write-through to IndexedDB.
/// - All reads are instant (from memory)
/// - All writes queue changes for IndexedDB flush
/// - Call `drain_pending()` from JS to get queued writes, then persist to IndexedDB
/// - Call `load_data()` on startup to hydrate from IndexedDB
#[wasm_bindgen]
pub struct WasmAgentStateGraph {
    repo: Arc<Repository>,
    storage: std::sync::Arc<IndexedDbStorage>,
}

#[wasm_bindgen]
impl WasmAgentStateGraph {
    /// Create a new AgentStateGraph with IndexedDB-backed storage.
    /// After construction, call `load_data()` with data from IndexedDB to restore state.
    #[wasm_bindgen(constructor)]
    pub fn new(db_name: Option<String>) -> Result<WasmAgentStateGraph, JsValue> {
        let name = db_name.unwrap_or_else(|| "agentstategraph".to_string());
        let _storage = std::sync::Arc::new(IndexedDbStorage::new(&name));
        let repo = Repository::new(Box::new(IndexedDbStorage::new(&name)));
        repo.init()
            .map_err(|e| JsValue::from_str(&format!("{}", e)))?;

        // Re-create with shared storage so we can access pending writes
        let storage2 = std::sync::Arc::new(IndexedDbStorage::new(&name));
        let repo2 = Repository::new(Box::new(IndexedDbStorage::new(&name)));
        repo2
            .init()
            .map_err(|e| JsValue::from_str(&format!("{}", e)))?;

        Ok(Self {
            repo: Arc::new(repo2),
            storage: storage2,
        })
    }

    /// Load objects from IndexedDB dump. Call on startup.
    /// Pass a JSON string: [["hex_id", "json"], ...]
    pub fn load_objects(&self, json_pairs: &str) -> Result<(), JsValue> {
        let pairs: Vec<(String, String)> = serde_json::from_str(json_pairs)
            .map_err(|e| JsValue::from_str(&format!("parse error: {}", e)))?;
        self.storage
            .load_objects(&pairs)
            .map_err(|e| JsValue::from_str(&format!("{}", e)))
    }

    /// Load commits from IndexedDB dump.
    pub fn load_commits(&self, json_pairs: &str) -> Result<(), JsValue> {
        let pairs: Vec<(String, String)> = serde_json::from_str(json_pairs)
            .map_err(|e| JsValue::from_str(&format!("parse error: {}", e)))?;
        self.storage
            .load_commits(&pairs)
            .map_err(|e| JsValue::from_str(&format!("{}", e)))
    }

    /// Load refs from IndexedDB dump.
    pub fn load_refs(&self, json_pairs: &str) -> Result<(), JsValue> {
        let pairs: Vec<(String, String)> = serde_json::from_str(json_pairs)
            .map_err(|e| JsValue::from_str(&format!("parse error: {}", e)))?;
        self.storage
            .load_refs(&pairs)
            .map_err(|e| JsValue::from_str(&format!("{}", e)))
    }

    /// Get pending object writes for flushing to IndexedDB. Returns JSON.
    pub fn drain_pending_objects(&self) -> String {
        let pending = self.storage.drain_pending_objects();
        serde_json::to_string(&pending).unwrap_or_else(|_| "[]".to_string())
    }

    /// Get pending commit writes.
    pub fn drain_pending_commits(&self) -> String {
        let pending = self.storage.drain_pending_commits();
        serde_json::to_string(&pending).unwrap_or_else(|_| "[]".to_string())
    }

    /// Get pending ref writes.
    pub fn drain_pending_refs(&self) -> String {
        let pending = self.storage.drain_pending_refs();
        serde_json::to_string(&pending).unwrap_or_else(|_| "[]".to_string())
    }

    /// Get the IndexedDB database name.
    pub fn db_name(&self) -> String {
        self.storage.db_name().to_string()
    }

    /// Get a JSON value at a path.
    pub fn get(&self, path: &str, reference: Option<String>) -> Result<String, JsValue> {
        let ref_name = reference.unwrap_or_else(|| "main".to_string());
        let val = self
            .repo
            .get_json(&ref_name, path)
            .map_err(|e| JsValue::from_str(&format!("{}", e)))?;
        Ok(serde_json::to_string(&val).unwrap_or_default())
    }

    /// Set a JSON value at a path.
    pub fn set(
        &self,
        path: &str,
        json_value: &str,
        category: &str,
        description: &str,
        reference: Option<String>,
        reasoning: Option<String>,
        confidence: Option<f64>,
    ) -> Result<String, JsValue> {
        let ref_name = reference.unwrap_or_else(|| "main".to_string());
        let value: serde_json::Value = serde_json::from_str(json_value)
            .map_err(|e| JsValue::from_str(&format!("JSON error: {}", e)))?;
        let opts = make_opts(description, category, reasoning, confidence);
        let id = self
            .repo
            .set_json(&ref_name, path, &value, opts)
            .map_err(|e| JsValue::from_str(&format!("{}", e)))?;
        Ok(id.to_string())
    }

    /// Delete a value at a path.
    pub fn delete(
        &self,
        path: &str,
        category: &str,
        description: &str,
        reference: Option<String>,
    ) -> Result<String, JsValue> {
        let ref_name = reference.unwrap_or_else(|| "main".to_string());
        let opts = make_opts(description, category, None, None);
        let id = self
            .repo
            .delete(&ref_name, path, opts)
            .map_err(|e| JsValue::from_str(&format!("{}", e)))?;
        Ok(id.to_string())
    }

    /// Create a branch.
    pub fn branch(&self, name: &str, from: Option<String>) -> Result<String, JsValue> {
        let from_ref = from.unwrap_or_else(|| "main".to_string());
        let id = self
            .repo
            .branch(name, &from_ref)
            .map_err(|e| JsValue::from_str(&format!("{}", e)))?;
        Ok(id.to_string())
    }

    /// Merge source into target.
    pub fn merge(
        &self,
        source: &str,
        target: Option<String>,
        description: Option<String>,
    ) -> Result<String, JsValue> {
        let target_ref = target.unwrap_or_else(|| "main".to_string());
        let desc = description.unwrap_or_else(|| "merge".to_string());
        let opts = make_opts(&desc, "Merge", None, None);
        let id = self
            .repo
            .merge(source, &target_ref, opts)
            .map_err(|e| JsValue::from_str(&format!("{}", e)))?;
        Ok(id.to_string())
    }

    /// Structured diff between two refs. Returns JSON.
    pub fn diff(&self, ref_a: &str, ref_b: &str) -> Result<String, JsValue> {
        let ops = self
            .repo
            .diff(ref_a, ref_b)
            .map_err(|e| JsValue::from_str(&format!("{}", e)))?;
        Ok(serde_json::to_string(&ops).unwrap_or_default())
    }

    /// Commit log. Returns JSON.
    pub fn log(&self, reference: Option<String>, limit: Option<u32>) -> Result<String, JsValue> {
        let ref_name = reference.unwrap_or_else(|| "main".to_string());
        let max = limit.unwrap_or(10) as usize;
        let commits = self
            .repo
            .log(&ref_name, max)
            .map_err(|e| JsValue::from_str(&format!("{}", e)))?;
        let entries: Vec<serde_json::Value> = commits
            .iter()
            .map(|c| {
                serde_json::json!({
                    "id": c.id.short(),
                    "agent": c.agent_id,
                    "intent": {
                        "category": format!("{:?}", c.intent.category),
                        "description": c.intent.description,
                        "tags": c.intent.tags,
                    },
                    "reasoning": c.reasoning,
                    "confidence": c.confidence,
                    "timestamp": c.timestamp.to_rfc3339(),
                })
            })
            .collect();
        Ok(serde_json::to_string(&entries).unwrap_or_default())
    }

    /// Blame — who modified a path and why.
    pub fn blame(&self, path: &str, reference: Option<String>) -> Result<String, JsValue> {
        let ref_name = reference.unwrap_or_else(|| "main".to_string());
        let entry = self
            .repo
            .blame(&ref_name, path)
            .map_err(|e| JsValue::from_str(&format!("{}", e)))?;
        Ok(serde_json::to_string(&entry).unwrap_or_default())
    }

    /// Create a speculation. Returns handle ID.
    pub fn speculate(&self, from: Option<String>, label: Option<String>) -> Result<u32, JsValue> {
        let from_ref = from.unwrap_or_else(|| "main".to_string());
        let handle = self
            .repo
            .speculate(&from_ref, label)
            .map_err(|e| JsValue::from_str(&format!("{}", e)))?;
        Ok(handle.id() as u32)
    }

    /// Get from a speculation.
    pub fn spec_get(&self, handle_id: u32, path: &str) -> Result<String, JsValue> {
        let handle = SpecHandle::from_id(handle_id as u64);
        let obj = self
            .repo
            .spec_get(handle, path)
            .map_err(|e| JsValue::from_str(&format!("{}", e)))?;
        let val = match &obj {
            Object::Atom(a) => match a {
                agentstategraph_core::Atom::Null => serde_json::Value::Null,
                agentstategraph_core::Atom::Bool(b) => serde_json::json!(b),
                agentstategraph_core::Atom::Int(i) => serde_json::json!(i),
                agentstategraph_core::Atom::Float(f) => serde_json::json!(f),
                agentstategraph_core::Atom::String(s) => serde_json::json!(s),
                _ => serde_json::json!(format!("{:?}", obj)),
            },
            _ => serde_json::json!(format!("{:?}", obj)),
        };
        Ok(serde_json::to_string(&val).unwrap_or_default())
    }

    /// Set in a speculation.
    pub fn spec_set(&self, handle_id: u32, path: &str, json_value: &str) -> Result<(), JsValue> {
        let handle = SpecHandle::from_id(handle_id as u64);
        let value: serde_json::Value = serde_json::from_str(json_value)
            .map_err(|e| JsValue::from_str(&format!("JSON: {}", e)))?;
        let obj = match &value {
            serde_json::Value::Null => Object::null(),
            serde_json::Value::Bool(b) => Object::bool(*b),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Object::int(i)
                } else {
                    Object::float(n.as_f64().unwrap_or(0.0))
                }
            }
            serde_json::Value::String(s) => Object::string(s.clone()),
            _ => Object::string(value.to_string()),
        };
        self.repo
            .spec_set(handle, path, &obj)
            .map_err(|e| JsValue::from_str(&format!("{}", e)))
    }

    /// Commit a speculation.
    pub fn commit_speculation(
        &self,
        handle_id: u32,
        category: &str,
        description: &str,
        reasoning: Option<String>,
        confidence: Option<f64>,
    ) -> Result<String, JsValue> {
        let handle = SpecHandle::from_id(handle_id as u64);
        let opts = make_opts(description, category, reasoning, confidence);
        let id = self
            .repo
            .commit_speculation(handle, opts)
            .map_err(|e| JsValue::from_str(&format!("{}", e)))?;
        Ok(id.to_string())
    }

    /// Discard a speculation.
    pub fn discard_speculation(&self, handle_id: u32) -> Result<(), JsValue> {
        let handle = SpecHandle::from_id(handle_id as u64);
        self.repo
            .discard_speculation(handle)
            .map_err(|e| JsValue::from_str(&format!("{}", e)))
    }

    /// Create an epoch.
    pub fn create_epoch(&self, id: &str, description: &str) -> Result<String, JsValue> {
        self.repo
            .create_epoch(id, description, vec![])
            .map(|e| format!("Epoch '{}' created", e.id))
            .map_err(|e| JsValue::from_str(&format!("{}", e)))
    }

    /// Seal an epoch.
    pub fn seal_epoch(&self, id: &str, summary: &str) -> Result<(), JsValue> {
        self.repo
            .seal_epoch(id, summary)
            .map_err(|e| JsValue::from_str(&format!("{}", e)))
    }

    // -----------------------------------------------------------------
    // TaskStore surface
    //
    // Plans/tasks cross the boundary as JSON. Consumers deserialize on
    // the JS side. See `agentstategraph-tasks` for schemas.
    // -----------------------------------------------------------------

    /// Create a plan. Returns the Plan as JSON.
    #[wasm_bindgen(js_name = tasksCreatePlan)]
    pub fn tasks_create_plan(
        &self,
        prefix: &str,
        agent_id: &str,
        ref_name: &str,
        name: &str,
        description: Option<String>,
    ) -> Result<String, JsValue> {
        let store = TaskStore::new(self.repo.clone(), prefix, agent_id);
        let plan = store
            .create_plan(ref_name, name, description)
            .map_err(js_err)?;
        serde_json::to_string(&plan).map_err(js_err)
    }

    #[wasm_bindgen(js_name = tasksListPlans)]
    pub fn tasks_list_plans(
        &self,
        prefix: &str,
        agent_id: &str,
        ref_name: &str,
    ) -> Result<String, JsValue> {
        let store = TaskStore::new(self.repo.clone(), prefix, agent_id);
        let plans = store.list_plans(ref_name).map_err(js_err)?;
        serde_json::to_string(&plans).map_err(js_err)
    }

    #[wasm_bindgen(js_name = tasksListPlansByStatus)]
    pub fn tasks_list_plans_by_status(
        &self,
        prefix: &str,
        agent_id: &str,
        ref_name: &str,
        status: Option<String>,
    ) -> Result<String, JsValue> {
        let store = TaskStore::new(self.repo.clone(), prefix, agent_id);
        let parsed = status.as_deref().and_then(parse_plan_status);
        let plans = store
            .list_plans_by_status(ref_name, parsed)
            .map_err(js_err)?;
        serde_json::to_string(&plans).map_err(js_err)
    }

    #[wasm_bindgen(js_name = tasksGetPlan)]
    pub fn tasks_get_plan(
        &self,
        prefix: &str,
        agent_id: &str,
        ref_name: &str,
        name: &str,
    ) -> Result<String, JsValue> {
        let store = TaskStore::new(self.repo.clone(), prefix, agent_id);
        let plan = store.get_plan(ref_name, name).map_err(js_err)?;
        serde_json::to_string(&plan).map_err(js_err)
    }

    #[wasm_bindgen(js_name = tasksArchivePlan)]
    pub fn tasks_archive_plan(
        &self,
        prefix: &str,
        agent_id: &str,
        ref_name: &str,
        name: &str,
    ) -> Result<String, JsValue> {
        let store = TaskStore::new(self.repo.clone(), prefix, agent_id);
        let plan = store.archive_plan(ref_name, name).map_err(js_err)?;
        serde_json::to_string(&plan).map_err(js_err)
    }

    #[wasm_bindgen(js_name = tasksDeletePlan)]
    pub fn tasks_delete_plan(
        &self,
        prefix: &str,
        agent_id: &str,
        ref_name: &str,
        name: &str,
    ) -> Result<(), JsValue> {
        let store = TaskStore::new(self.repo.clone(), prefix, agent_id);
        store.delete_plan(ref_name, name).map_err(js_err)
    }

    /// Add a task. `priority` is "low"|"medium"|"high"|"critical".
    /// `blockers_json` is a JSON array of task id strings (or null).
    #[wasm_bindgen(js_name = tasksAddTask)]
    #[allow(clippy::too_many_arguments)]
    pub fn tasks_add_task(
        &self,
        prefix: &str,
        agent_id: &str,
        ref_name: &str,
        plan: &str,
        title: &str,
        priority: &str,
        parent_id: Option<String>,
        blockers_json: Option<String>,
        assigned_to: Option<String>,
    ) -> Result<String, JsValue> {
        let store = TaskStore::new(self.repo.clone(), prefix, agent_id);
        let prio = parse_priority(priority);
        let parent = parent_id.filter(|s| !s.is_empty()).map(TaskId);
        let blockers: Vec<TaskId> = match blockers_json {
            None => Vec::new(),
            Some(s) if s.is_empty() => Vec::new(),
            Some(s) => serde_json::from_str::<Vec<String>>(&s)
                .map_err(js_err)?
                .into_iter()
                .map(TaskId)
                .collect(),
        };
        let task = store
            .add_task(ref_name, plan, title, prio, parent, blockers, assigned_to)
            .map_err(js_err)?;
        serde_json::to_string(&task).map_err(js_err)
    }

    /// Add a task with the 0.6.0 extension fields (`payload`,
    /// `parent_change`, `on_complete`). All three are JSON strings; pass
    /// `None` / null for unset. `payload` is an arbitrary JSON value,
    /// `parent_change` a `"spec-id@version"` handle, `on_complete` a
    /// JSON-serialized `OnCompleteHook`.
    #[wasm_bindgen(js_name = tasksAddTaskWithExtensions)]
    #[allow(clippy::too_many_arguments)]
    pub fn tasks_add_task_with_extensions(
        &self,
        prefix: &str,
        agent_id: &str,
        ref_name: &str,
        plan: &str,
        title: &str,
        priority: &str,
        parent_id: Option<String>,
        blockers_json: Option<String>,
        assigned_to: Option<String>,
        payload_json: Option<String>,
        parent_change: Option<String>,
        on_complete_json: Option<String>,
    ) -> Result<String, JsValue> {
        let store = TaskStore::new(self.repo.clone(), prefix, agent_id);
        let prio = parse_priority(priority);
        let parent = parent_id.filter(|s| !s.is_empty()).map(TaskId);
        let blockers: Vec<TaskId> = match blockers_json {
            None => Vec::new(),
            Some(s) if s.is_empty() => Vec::new(),
            Some(s) => serde_json::from_str::<Vec<String>>(&s)
                .map_err(js_err)?
                .into_iter()
                .map(TaskId)
                .collect(),
        };
        let payload: Option<serde_json::Value> = match payload_json {
            None => None,
            Some(s) if s.is_empty() => None,
            Some(s) => Some(serde_json::from_str(&s).map_err(js_err)?),
        };
        let on_complete: Option<OnCompleteHook> = match on_complete_json {
            None => None,
            Some(s) if s.is_empty() => None,
            Some(s) => Some(serde_json::from_str(&s).map_err(js_err)?),
        };
        let task = store
            .add_task_with_extensions(
                ref_name,
                plan,
                title,
                prio,
                parent,
                blockers,
                assigned_to,
                payload,
                parent_change,
                on_complete,
            )
            .map_err(js_err)?;
        serde_json::to_string(&task).map_err(js_err)
    }

    #[wasm_bindgen(js_name = tasksListTasks)]
    pub fn tasks_list_tasks(
        &self,
        prefix: &str,
        agent_id: &str,
        ref_name: &str,
        plan: &str,
    ) -> Result<String, JsValue> {
        let store = TaskStore::new(self.repo.clone(), prefix, agent_id);
        let tasks = store.list_tasks(ref_name, plan).map_err(js_err)?;
        serde_json::to_string(&tasks).map_err(js_err)
    }

    #[wasm_bindgen(js_name = tasksTaskIds)]
    pub fn tasks_task_ids(
        &self,
        prefix: &str,
        agent_id: &str,
        ref_name: &str,
        plan: &str,
    ) -> Result<String, JsValue> {
        let store = TaskStore::new(self.repo.clone(), prefix, agent_id);
        let ids: Vec<String> = store
            .task_ids(ref_name, plan)
            .map_err(js_err)?
            .into_iter()
            .map(|i| i.0)
            .collect();
        serde_json::to_string(&ids).map_err(js_err)
    }

    #[wasm_bindgen(js_name = tasksGetTask)]
    pub fn tasks_get_task(
        &self,
        prefix: &str,
        agent_id: &str,
        ref_name: &str,
        plan: &str,
        task_id: &str,
    ) -> Result<String, JsValue> {
        let store = TaskStore::new(self.repo.clone(), prefix, agent_id);
        let task = store
            .get_task(ref_name, plan, &TaskId(task_id.to_string()))
            .map_err(js_err)?;
        serde_json::to_string(&task).map_err(js_err)
    }

    #[wasm_bindgen(js_name = tasksStartTask)]
    pub fn tasks_start_task(
        &self,
        prefix: &str,
        agent_id: &str,
        ref_name: &str,
        plan: &str,
        task_id: &str,
    ) -> Result<String, JsValue> {
        let store = TaskStore::new(self.repo.clone(), prefix, agent_id);
        let task = store
            .start_task(ref_name, plan, &TaskId(task_id.to_string()))
            .map_err(js_err)?;
        serde_json::to_string(&task).map_err(js_err)
    }

    /// Complete a task. `proof_kind` is "commit"|"file"|"test"|"text".
    #[wasm_bindgen(js_name = tasksCompleteTask)]
    #[allow(clippy::too_many_arguments)]
    pub fn tasks_complete_task(
        &self,
        prefix: &str,
        agent_id: &str,
        ref_name: &str,
        plan: &str,
        task_id: &str,
        proof_kind: &str,
        proof_value: &str,
        proof_note: Option<String>,
    ) -> Result<String, JsValue> {
        let store = TaskStore::new(self.repo.clone(), prefix, agent_id);
        let kind = parse_proof_kind(proof_kind)
            .ok_or_else(|| JsValue::from_str(&format!("invalid proof kind: {proof_kind}")))?;
        let proof = Proof {
            kind,
            value: proof_value.to_string(),
            note: proof_note,
        };
        let task = store
            .complete_task(ref_name, plan, &TaskId(task_id.to_string()), proof)
            .map_err(js_err)?;
        serde_json::to_string(&task).map_err(js_err)
    }

    #[wasm_bindgen(js_name = tasksAbandonTask)]
    pub fn tasks_abandon_task(
        &self,
        prefix: &str,
        agent_id: &str,
        ref_name: &str,
        plan: &str,
        task_id: &str,
        reason: &str,
    ) -> Result<String, JsValue> {
        let store = TaskStore::new(self.repo.clone(), prefix, agent_id);
        let task = store
            .abandon_task(ref_name, plan, &TaskId(task_id.to_string()), reason)
            .map_err(js_err)?;
        serde_json::to_string(&task).map_err(js_err)
    }

    #[wasm_bindgen(js_name = tasksSetPriority)]
    pub fn tasks_set_priority(
        &self,
        prefix: &str,
        agent_id: &str,
        ref_name: &str,
        plan: &str,
        task_id: &str,
        priority: &str,
    ) -> Result<String, JsValue> {
        let store = TaskStore::new(self.repo.clone(), prefix, agent_id);
        let task = store
            .set_priority(
                ref_name,
                plan,
                &TaskId(task_id.to_string()),
                parse_priority(priority),
            )
            .map_err(js_err)?;
        serde_json::to_string(&task).map_err(js_err)
    }

    #[wasm_bindgen(js_name = tasksSetBlockers)]
    pub fn tasks_set_blockers(
        &self,
        prefix: &str,
        agent_id: &str,
        ref_name: &str,
        plan: &str,
        task_id: &str,
        blockers_json: &str,
    ) -> Result<String, JsValue> {
        let store = TaskStore::new(self.repo.clone(), prefix, agent_id);
        let blockers: Vec<TaskId> = if blockers_json.is_empty() {
            Vec::new()
        } else {
            serde_json::from_str::<Vec<String>>(blockers_json)
                .map_err(js_err)?
                .into_iter()
                .map(TaskId)
                .collect()
        };
        let task = store
            .set_blockers(ref_name, plan, &TaskId(task_id.to_string()), blockers)
            .map_err(js_err)?;
        serde_json::to_string(&task).map_err(js_err)
    }

    #[wasm_bindgen(js_name = tasksAssignTask)]
    pub fn tasks_assign_task(
        &self,
        prefix: &str,
        agent_id: &str,
        ref_name: &str,
        plan: &str,
        task_id: &str,
        agent: &str,
    ) -> Result<String, JsValue> {
        let store = TaskStore::new(self.repo.clone(), prefix, agent_id);
        let task = store
            .assign_task(ref_name, plan, &TaskId(task_id.to_string()), agent)
            .map_err(js_err)?;
        serde_json::to_string(&task).map_err(js_err)
    }

    #[wasm_bindgen(js_name = tasksUnassignTask)]
    pub fn tasks_unassign_task(
        &self,
        prefix: &str,
        agent_id: &str,
        ref_name: &str,
        plan: &str,
        task_id: &str,
    ) -> Result<String, JsValue> {
        let store = TaskStore::new(self.repo.clone(), prefix, agent_id);
        let task = store
            .unassign_task(ref_name, plan, &TaskId(task_id.to_string()))
            .map_err(js_err)?;
        serde_json::to_string(&task).map_err(js_err)
    }

    #[wasm_bindgen(js_name = tasksNextTask)]
    pub fn tasks_next_task(
        &self,
        prefix: &str,
        agent_id: &str,
        ref_name: &str,
        plan: &str,
    ) -> Result<String, JsValue> {
        let store = TaskStore::new(self.repo.clone(), prefix, agent_id);
        let task = store.next_task(ref_name, plan).map_err(js_err)?;
        serde_json::to_string(&task).map_err(js_err)
    }

    #[wasm_bindgen(js_name = tasksNextTaskFor)]
    pub fn tasks_next_task_for(
        &self,
        prefix: &str,
        agent_id: &str,
        ref_name: &str,
        plan: &str,
        agent: Option<String>,
        include_unassigned: bool,
    ) -> Result<String, JsValue> {
        let store = TaskStore::new(self.repo.clone(), prefix, agent_id);
        let task = store
            .next_task_for(ref_name, plan, agent.as_deref(), include_unassigned)
            .map_err(js_err)?;
        serde_json::to_string(&task).map_err(js_err)
    }

    #[wasm_bindgen(js_name = tasksDerivedStatus)]
    pub fn tasks_derived_status(
        &self,
        prefix: &str,
        agent_id: &str,
        ref_name: &str,
        plan: &str,
        parent_id: &str,
    ) -> Result<String, JsValue> {
        let store = TaskStore::new(self.repo.clone(), prefix, agent_id);
        let status = store
            .derived_status(ref_name, plan, &TaskId(parent_id.to_string()))
            .map_err(js_err)?;
        serde_json::to_string(&status).map_err(js_err)
    }

    // -----------------------------------------------------------------
    // Migration surface
    // -----------------------------------------------------------------

    /// Check migration status. Returns a JSON report (see `CheckResult`).
    #[wasm_bindgen(js_name = migrateCheck)]
    pub fn migrate_check(&self, ref_name: &str, target: Option<String>) -> Result<String, JsValue> {
        let target_str = target.unwrap_or_else(|| SCHEMA_VERSION.to_string());
        let target_version = Version::parse(&target_str).map_err(js_err)?;
        let registry = Registry::builtin();
        let r = agentstategraph_migrate::check(&self.repo, ref_name, &target_version, &registry)
            .map_err(js_err)?;
        Ok(check_result_json(&r))
    }

    /// Run migrations. `mode` is `"apply"` or `"dry-run"`.
    #[wasm_bindgen(js_name = migrateRun)]
    pub fn migrate_run(
        &self,
        ref_name: &str,
        target: Option<String>,
        mode: &str,
    ) -> Result<String, JsValue> {
        let target_str = target.unwrap_or_else(|| SCHEMA_VERSION.to_string());
        let target_version = Version::parse(&target_str).map_err(js_err)?;
        let run_mode = match mode.to_lowercase().as_str() {
            "apply" => RunMode::Apply,
            "dry-run" | "dryrun" | "dry_run" => RunMode::DryRun,
            other => return Err(JsValue::from_str(&format!("invalid mode: {other}"))),
        };
        let registry = Registry::builtin();
        let r = registry
            .run(&self.repo, ref_name, &target_version, run_mode)
            .map_err(js_err)?;
        Ok(report_json(&r))
    }

    // -----------------------------------------------------------------
    // Session surface (audit per §6: Session / SessionStatus moved to
    // agentstategraph-core in 0.6.5). The existing `agentstategraph`
    // facade re-exports both via `agentstategraph::session::{Session,
    // SessionStatus}`, and `Repository::sessions()` still returns a
    // `SessionManager`. We surface the same shape the Python binding
    // does so JS consumers can round-trip Session records.
    // -----------------------------------------------------------------

    /// Create a durable session record. Returns the Session as JSON.
    #[wasm_bindgen(js_name = createSession)]
    #[allow(clippy::too_many_arguments)]
    pub fn create_session(
        &self,
        agent_id: &str,
        working_branch: Option<String>,
        parent_session: Option<String>,
        delegated_intent: Option<String>,
        report_to: Option<String>,
        path_scope: Option<String>,
    ) -> Result<String, JsValue> {
        let branch = working_branch.unwrap_or_else(|| "main".to_string());
        let log = self.repo.log(&branch, 1).map_err(js_err)?;
        let head = log
            .into_iter()
            .next()
            .map(|c| c.id)
            .ok_or_else(|| JsValue::from_str(&format!("ref {branch:?} empty")))?;
        let mgr = self.repo.sessions();
        let s = mgr
            .create(
                agent_id,
                &branch,
                head,
                parent_session,
                delegated_intent,
                report_to,
                path_scope,
            )
            .map_err(js_err)?;
        serde_json::to_string(&s).map_err(js_err)
    }

    /// Fetch a session by id. Returns JSON, or "null" if not found.
    #[wasm_bindgen(js_name = getSession)]
    pub fn get_session(&self, id: &str) -> Result<String, JsValue> {
        let mgr = self.repo.sessions();
        let s = mgr.get(id).map_err(js_err)?;
        serde_json::to_string(&s).map_err(js_err)
    }

    /// List sessions, optionally filtered by agent_id.
    #[wasm_bindgen(js_name = listSessions)]
    pub fn list_sessions(&self, agent_filter: Option<String>) -> Result<String, JsValue> {
        let mgr = self.repo.sessions();
        let sessions = mgr.list(agent_filter.as_deref()).map_err(js_err)?;
        serde_json::to_string(&sessions).map_err(js_err)
    }

    /// End a session. `status` is "active" | "completed" | "abandoned".
    #[wasm_bindgen(js_name = endSession)]
    pub fn end_session(&self, id: &str, status: &str) -> Result<(), JsValue> {
        let st = parse_session_status(status)
            .ok_or_else(|| JsValue::from_str(&format!("invalid session status: {status}")))?;
        let mgr = self.repo.sessions();
        mgr.end(id, st).map_err(js_err)
    }

    /// List epochs. Returns JSON.
    pub fn list_epochs(&self) -> Result<String, JsValue> {
        let entries = self
            .repo
            .list_epochs()
            .map_err(|e| JsValue::from_str(&format!("{}", e)))?;
        let json: Vec<serde_json::Value> = entries
            .iter()
            .map(|e| {
                serde_json::json!({
                    "id": e.id,
                    "description": e.description,
                    "status": format!("{:?}", e.status),
                    "commits": e.commit_count,
                })
            })
            .collect();
        Ok(serde_json::to_string(&json).unwrap_or_default())
    }
}

// ---------------------------------------------------------------------------
// PolicyStore surface
//
// Mirrors the Python (§2) / TS (§3) idiom: complex types (Policy,
// Decision, ChangeProposal, Situation, Selector, ...) cross the
// boundary as JSON strings. Consumers `JSON.parse` on the JS side.
// See `agentstategraph-policy` for schemas.
// ---------------------------------------------------------------------------

/// PolicyStore — situation-matching authorization + change-cost
/// policies. Wraps `agentstategraph_policy::PolicyStore`.
///
/// Construct from a `WasmAgentStateGraph`, a path prefix (e.g.
/// `/policies`), and an agent id.
#[wasm_bindgen]
pub struct WasmPolicyStore {
    inner: PolicyBackend,
}

impl WasmPolicyStore {
    /// Construct directly from a `Repository` handle. Used by the
    /// integration tests that run under wasm-bindgen-test and need a
    /// MemoryStorage-backed store rather than the IndexedDB-backed
    /// `WasmAgentStateGraph`. Not exported to JS (plain Rust `impl`).
    #[doc(hidden)]
    pub fn from_repo(repo: Arc<Repository>, prefix: &str, agent_id: &str) -> Self {
        Self {
            inner: PolicyBackend::new(repo, prefix, agent_id),
        }
    }
}

#[wasm_bindgen]
impl WasmPolicyStore {
    /// Create a PolicyStore bound to an AgentStateGraph instance.
    #[wasm_bindgen(constructor)]
    pub fn new(asg: &WasmAgentStateGraph, prefix: &str, agent_id: &str) -> WasmPolicyStore {
        WasmPolicyStore {
            inner: PolicyBackend::new(asg.repo.clone(), prefix, agent_id),
        }
    }

    /// Write a proposed (unratified) policy. Returns `"path@version"`.
    /// `policy_json` is the serialized Policy struct.
    pub fn propose(&self, ref_name: &str, policy_json: &str) -> Result<String, JsValue> {
        let p: Policy = serde_json::from_str(policy_json).map_err(js_err)?;
        self.inner.propose(ref_name, p).map_err(js_err)
    }

    /// Ratify an unratified proposal at `path`.
    pub fn ratify(
        &self,
        ref_name: &str,
        path: &str,
        ratifier: &str,
        reasoning: &str,
    ) -> Result<(), JsValue> {
        self.inner
            .ratify(ref_name, path, ratifier, reasoning)
            .map_err(js_err)
    }

    /// Replace the active policy at `path` with `new_policy_json`.
    /// Returns the new `"path@version"` handle.
    pub fn supersede(
        &self,
        ref_name: &str,
        path: &str,
        new_policy_json: &str,
    ) -> Result<String, JsValue> {
        let p: Policy = serde_json::from_str(new_policy_json).map_err(js_err)?;
        self.inner.supersede(ref_name, path, p).map_err(js_err)
    }

    /// List every policy (active versions, ratified or not). Returns a
    /// JSON array.
    pub fn list(&self, ref_name: &str, prefix_filter: Option<String>) -> Result<String, JsValue> {
        let policies = self
            .inner
            .list(ref_name, prefix_filter.as_deref())
            .map_err(js_err)?;
        serde_json::to_string(&policies).map_err(js_err)
    }

    /// List currently-active policies (ratified AND `active_from <= now`).
    pub fn active(&self, ref_name: &str, prefix_filter: Option<String>) -> Result<String, JsValue> {
        let policies = self
            .inner
            .active(ref_name, prefix_filter.as_deref())
            .map_err(js_err)?;
        serde_json::to_string(&policies).map_err(js_err)
    }

    /// Fetch a policy at `path`. If `version` is provided, returns the
    /// pinned historical version; otherwise the current active version.
    pub fn get(&self, ref_name: &str, path: &str, version: Option<u64>) -> Result<String, JsValue> {
        let policy = self.inner.get(ref_name, path, version).map_err(js_err)?;
        serde_json::to_string(&policy).map_err(js_err)
    }

    /// Walk the supersedes chain (oldest first → current).
    pub fn history(&self, ref_name: &str, path: &str) -> Result<String, JsValue> {
        let policies = self.inner.history(ref_name, path).map_err(js_err)?;
        serde_json::to_string(&policies).map_err(js_err)
    }

    /// Authorization evaluation (POLICY_V1.md §5). Returns a Decision
    /// JSON object.
    pub fn evaluate(
        &self,
        ref_name: &str,
        situation_json: &str,
        action: &str,
        agent_id: &str,
    ) -> Result<String, JsValue> {
        let sit: Situation = serde_json::from_str(situation_json).map_err(js_err)?;
        let decision = self
            .inner
            .evaluate(ref_name, &sit, action, agent_id)
            .map_err(js_err)?;
        serde_json::to_string(&decision).map_err(js_err)
    }

    /// Change-proposal evaluation (POLICY_V1.md §22.2). Returns a
    /// Decision JSON object.
    #[wasm_bindgen(js_name = evaluateChange)]
    pub fn evaluate_change(&self, ref_name: &str, proposal_json: &str) -> Result<String, JsValue> {
        let prop: ChangeProposal = serde_json::from_str(proposal_json).map_err(js_err)?;
        let decision = self
            .inner
            .evaluate_change(ref_name, &prop)
            .map_err(js_err)?;
        serde_json::to_string(&decision).map_err(js_err)
    }

    /// List ratified policies whose `triggers` intersect `tokens_json`
    /// (a JSON array of strings). Convenience wrapper exposing the
    /// filter `evaluate_change` uses internally.
    #[wasm_bindgen(js_name = checkTokens)]
    pub fn check_tokens(&self, ref_name: &str, tokens_json: &str) -> Result<String, JsValue> {
        let tokens: Vec<String> = serde_json::from_str(tokens_json).map_err(js_err)?;
        let token_set: std::collections::HashSet<&str> =
            tokens.iter().map(|s| s.as_str()).collect();
        let actives = self.inner.active(ref_name, None).map_err(js_err)?;
        let matched: Vec<Policy> = actives
            .into_iter()
            .filter(|p| p.triggers.iter().any(|t| token_set.contains(t.as_str())))
            .collect();
        serde_json::to_string(&matched).map_err(js_err)
    }
}

// ---------------------------------------------------------------------------
// Helpers (non-exported)
// ---------------------------------------------------------------------------

fn js_err<E: std::fmt::Display>(e: E) -> JsValue {
    JsValue::from_str(&format!("{}", e))
}

fn parse_priority(s: &str) -> Priority {
    match s.to_lowercase().as_str() {
        "low" => Priority::Low,
        "high" => Priority::High,
        "critical" => Priority::Critical,
        _ => Priority::Medium,
    }
}

fn parse_plan_status(s: &str) -> Option<agentstategraph_tasks::PlanStatus> {
    match s.to_lowercase().as_str() {
        "active" => Some(agentstategraph_tasks::PlanStatus::Active),
        "completed" => Some(agentstategraph_tasks::PlanStatus::Completed),
        "archived" => Some(agentstategraph_tasks::PlanStatus::Archived),
        _ => None,
    }
}

fn parse_session_status(s: &str) -> Option<SessionStatus> {
    match s.to_lowercase().as_str() {
        "active" => Some(SessionStatus::Active),
        "completed" => Some(SessionStatus::Completed),
        "abandoned" => Some(SessionStatus::Abandoned),
        _ => None,
    }
}

fn parse_proof_kind(s: &str) -> Option<ProofKind> {
    match s.to_lowercase().as_str() {
        "commit" => Some(ProofKind::Commit),
        "file" => Some(ProofKind::File),
        "test" => Some(ProofKind::Test),
        "text" => Some(ProofKind::Text),
        _ => None,
    }
}

fn check_result_json(r: &CheckResult) -> String {
    let v = match r {
        CheckResult::UpToDate { version } => serde_json::json!({
            "status": "up_to_date",
            "version": version.to_string(),
        }),
        CheckResult::UpgradeAvailable {
            from,
            to,
            migrations,
        } => serde_json::json!({
            "status": "upgrade_available",
            "from": from.to_string(),
            "to": to.to_string(),
            "migrations": migrations,
        }),
        CheckResult::Downgrade { db, binary } => serde_json::json!({
            "status": "downgrade",
            "db": db.to_string(),
            "binary": binary.to_string(),
        }),
        CheckResult::Unversioned { implicit } => serde_json::json!({
            "status": "unversioned",
            "implicit": implicit.to_string(),
        }),
        CheckResult::Corrupt(msg) => serde_json::json!({
            "status": "corrupt",
            "message": msg,
        }),
    };
    serde_json::to_string(&v).unwrap_or_default()
}

fn report_json(r: &agentstategraph_migrate::Report) -> String {
    let mode = match r.mode {
        RunMode::DryRun => "dry-run",
        RunMode::Apply => "apply",
    };
    let steps: Vec<serde_json::Value> = r
        .steps
        .iter()
        .map(|s| {
            serde_json::json!({
                "name": s.name,
                "describe": s.describe,
                "from": s.from.to_string(),
                "to": s.to.to_string(),
                "status": step_status_str(s.status),
                "commit_id": s.commit_id.as_ref().map(|c| c.to_string()),
                "notes": s.notes,
            })
        })
        .collect();
    let v = serde_json::json!({
        "from": r.from.to_string(),
        "target": r.target.to_string(),
        "final_version": r.final_version.to_string(),
        "mode": mode,
        "steps": steps,
    });
    serde_json::to_string(&v).unwrap_or_default()
}

fn step_status_str(s: StepStatus) -> &'static str {
    match s {
        StepStatus::WouldApply => "would-apply",
        StepStatus::WouldSkip => "would-skip",
        StepStatus::Applied => "applied",
        StepStatus::Skipped => "skipped",
        StepStatus::Failed => "failed",
    }
}
