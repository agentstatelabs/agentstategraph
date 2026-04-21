//! TypeScript/Node.js bindings for AgentStateGraph via napi-rs.
//!
//! Usage:
//!   const { AgentStateGraph } = require('agentstategraph')
//!   const asg = new AgentStateGraph()           // in-memory
//!   const asg = new AgentStateGraph("state.db") // SQLite
//!
//!   asg.set("/name", "my-cluster", { category: "Checkpoint", description: "init" })
//!   asg.get("/name")  // → "my-cluster"

// Binding glue: exported napi functions mirror the JS-side call shape
// which has no natural way to collapse into fewer args. Allow the lint
// here rather than wrapping every export in its own allow attribute.
#![allow(clippy::too_many_arguments)]

#[macro_use]
extern crate napi_derive;

use std::sync::Arc;

// Session + SessionStatus moved to agentstategraph-core in 0.6.5;
// import from the canonical location rather than the facade re-export.
use agentstategraph::speculation::SpecHandle;
use agentstategraph::{CommitOptions, Repository};
use agentstategraph_core::{IntentCategory, Object};
use agentstategraph_core::{Session, SessionStatus};
use agentstategraph_policy::{
    ChangeProposal, Decision, Policy, PolicyStore as PolicyBackend, Situation,
};
use agentstategraph_storage::{MemoryStorage, SqliteStorage};
use agentstategraph_tasks::{
    NoopVerifier, OnCompleteHook, Plan, PlanStatus, Priority, Proof, ProofKind, Task, TaskId,
    TaskStatus, TaskStore as TasksBackend, TaskStoreError, Verifier, VerifyReport, VerifyResult,
};

fn parse_category(s: &str) -> IntentCategory {
    match s.to_lowercase().as_str() {
        "explore" => IntentCategory::Explore,
        "refine" => IntentCategory::Refine,
        "fix" => IntentCategory::Fix,
        "rollback" => IntentCategory::Rollback,
        "checkpoint" => IntentCategory::Checkpoint,
        "merge" => IntentCategory::Merge,
        "migrate" => IntentCategory::Migrate,
        other => IntentCategory::Custom(other.to_string()),
    }
}

fn make_opts(
    description: &str,
    category: Option<String>,
    agent: Option<String>,
    reasoning: Option<String>,
    confidence: Option<f64>,
    tags: Option<Vec<String>>,
) -> CommitOptions {
    let agent_id = agent.unwrap_or_else(|| "node".to_string());
    let cat = parse_category(&category.unwrap_or_else(|| "Checkpoint".to_string()));
    let mut opts = CommitOptions::new(agent_id, cat, description);
    if let Some(r) = reasoning {
        opts = opts.with_reasoning(r);
    }
    if let Some(c) = confidence {
        opts = opts.with_confidence(c);
    }
    if let Some(t) = tags {
        opts = opts.with_tags(t);
    }
    opts
}

fn js_to_object(value: &serde_json::Value) -> Object {
    match value {
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
    }
}

fn err(e: impl std::fmt::Display) -> napi::Error {
    napi::Error::from_reason(format!("{}", e))
}

/// AgentStateGraph — AI-native versioned state store.
#[napi]
pub struct AgentStateGraph {
    repo: Arc<Repository>,
}

#[napi]
impl AgentStateGraph {
    /// Create a new AgentStateGraph.
    /// Pass a path for SQLite (durable), or omit for in-memory (ephemeral).
    #[napi(constructor)]
    pub fn new(path: Option<String>) -> napi::Result<Self> {
        let repo = match path {
            Some(p) => {
                let storage = SqliteStorage::open(&p).map_err(err)?;
                Repository::new(Box::new(storage))
            }
            None => Repository::new(Box::new(MemoryStorage::new())),
        };
        repo.init().map_err(err)?;
        Ok(Self {
            repo: Arc::new(repo),
        })
    }

    // -- State operations --

    /// Get a value at a path. Returns a JSON-compatible value.
    #[napi]
    pub fn get(&self, path: String, reference: Option<String>) -> napi::Result<serde_json::Value> {
        let ref_name = reference.unwrap_or_else(|| "main".to_string());
        self.repo.get_json(&ref_name, &path).map_err(err)
    }

    /// Set a simple value at a path, creating a commit.
    #[napi]
    pub fn set(
        &self,
        path: String,
        value: serde_json::Value,
        description: String,
        reference: Option<String>,
        category: Option<String>,
        agent: Option<String>,
        reasoning: Option<String>,
        confidence: Option<f64>,
        tags: Option<Vec<String>>,
    ) -> napi::Result<String> {
        let ref_name = reference.unwrap_or_else(|| "main".to_string());
        let obj = js_to_object(&value);
        let opts = make_opts(&description, category, agent, reasoning, confidence, tags);
        let commit_id = self.repo.set(&ref_name, &path, &obj, opts).map_err(err)?;
        Ok(commit_id.to_string())
    }

    /// Set a JSON value (object/array) at a path, creating a commit.
    #[napi]
    pub fn set_json(
        &self,
        path: String,
        value: serde_json::Value,
        description: String,
        reference: Option<String>,
        category: Option<String>,
        agent: Option<String>,
        reasoning: Option<String>,
        confidence: Option<f64>,
        tags: Option<Vec<String>>,
    ) -> napi::Result<String> {
        let ref_name = reference.unwrap_or_else(|| "main".to_string());
        let opts = make_opts(&description, category, agent, reasoning, confidence, tags);
        let commit_id = self
            .repo
            .set_json(&ref_name, &path, &value, opts)
            .map_err(err)?;
        Ok(commit_id.to_string())
    }

    /// Delete a value at a path, creating a commit.
    #[napi]
    pub fn delete(
        &self,
        path: String,
        description: String,
        reference: Option<String>,
        category: Option<String>,
    ) -> napi::Result<String> {
        let ref_name = reference.unwrap_or_else(|| "main".to_string());
        let opts = make_opts(&description, category, None, None, None, None);
        let commit_id = self.repo.delete(&ref_name, &path, opts).map_err(err)?;
        Ok(commit_id.to_string())
    }

    // -- Branch operations --

    /// Create a branch from a ref.
    #[napi]
    pub fn branch(&self, name: String, from: Option<String>) -> napi::Result<String> {
        let from_ref = from.unwrap_or_else(|| "main".to_string());
        let id = self.repo.branch(&name, &from_ref).map_err(err)?;
        Ok(id.to_string())
    }

    /// Delete a branch.
    #[napi]
    pub fn delete_branch(&self, name: String) -> napi::Result<bool> {
        self.repo.delete_branch(&name).map_err(err)
    }

    /// List branches.
    #[napi]
    pub fn list_branches(&self, prefix: Option<String>) -> napi::Result<Vec<serde_json::Value>> {
        let branches = self.repo.list_branches(prefix.as_deref()).map_err(err)?;
        Ok(branches
            .into_iter()
            .map(|(name, id)| serde_json::json!({"name": name, "id": id.short()}))
            .collect())
    }

    // -- Merge --

    /// Merge source branch into target.
    #[napi]
    pub fn merge(
        &self,
        source: String,
        target: Option<String>,
        description: Option<String>,
        reasoning: Option<String>,
    ) -> napi::Result<String> {
        let target_ref = target.unwrap_or_else(|| "main".to_string());
        let desc = description.unwrap_or_else(|| "merge".to_string());
        let mut opts = CommitOptions::new("node", IntentCategory::Merge, &desc);
        if let Some(r) = reasoning {
            opts = opts.with_reasoning(r);
        }
        let commit_id = self.repo.merge(&source, &target_ref, opts).map_err(err)?;
        Ok(commit_id.to_string())
    }

    // -- Diff --

    /// Structured diff between two refs.
    #[napi]
    pub fn diff(&self, ref_a: String, ref_b: String) -> napi::Result<serde_json::Value> {
        let ops = self.repo.diff(&ref_a, &ref_b).map_err(err)?;
        serde_json::to_value(&ops).map_err(err)
    }

    // -- Log --

    /// Commit log from a ref.
    #[napi]
    pub fn log(
        &self,
        reference: Option<String>,
        limit: Option<u32>,
    ) -> napi::Result<Vec<serde_json::Value>> {
        let ref_name = reference.unwrap_or_else(|| "main".to_string());
        let max = limit.unwrap_or(10) as usize;
        let commits = self.repo.log(&ref_name, max).map_err(err)?;
        Ok(commits
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
                    "parents": c.parents.len(),
                    "timestamp": c.timestamp.to_rfc3339(),
                })
            })
            .collect())
    }

    // -- Speculation --

    /// Create a speculation. Returns handle ID.
    #[napi]
    pub fn speculate(&self, from: Option<String>, label: Option<String>) -> napi::Result<u32> {
        let from_ref = from.unwrap_or_else(|| "main".to_string());
        let handle = self.repo.speculate(&from_ref, label).map_err(err)?;
        Ok(handle.id() as u32)
    }

    /// Get a value from a speculation.
    #[napi]
    pub fn spec_get(&self, handle_id: u32, path: String) -> napi::Result<serde_json::Value> {
        let handle = SpecHandle::from_id(handle_id as u64);
        let obj = self.repo.spec_get(handle, &path).map_err(err)?;
        match &obj {
            Object::Atom(a) => match a {
                agentstategraph_core::Atom::Null => Ok(serde_json::Value::Null),
                agentstategraph_core::Atom::Bool(b) => Ok(serde_json::json!(b)),
                agentstategraph_core::Atom::Int(i) => Ok(serde_json::json!(i)),
                agentstategraph_core::Atom::Float(f) => Ok(serde_json::json!(f)),
                agentstategraph_core::Atom::String(s) => Ok(serde_json::json!(s)),
                agentstategraph_core::Atom::Bytes(b) => {
                    Ok(serde_json::json!(format!("bytes:{}", b.len())))
                }
            },
            _ => Ok(serde_json::json!(format!("{:?}", obj))),
        }
    }

    /// Set a value within a speculation.
    #[napi]
    pub fn spec_set(
        &self,
        handle_id: u32,
        path: String,
        value: serde_json::Value,
    ) -> napi::Result<()> {
        let handle = SpecHandle::from_id(handle_id as u64);
        let obj = js_to_object(&value);
        self.repo.spec_set(handle, &path, &obj).map_err(err)
    }

    /// Commit a speculation to its base branch.
    #[napi]
    pub fn commit_speculation(
        &self,
        handle_id: u32,
        description: String,
        category: Option<String>,
        reasoning: Option<String>,
        confidence: Option<f64>,
    ) -> napi::Result<String> {
        let handle = SpecHandle::from_id(handle_id as u64);
        let opts = make_opts(&description, category, None, reasoning, confidence, None);
        let commit_id = self.repo.commit_speculation(handle, opts).map_err(err)?;
        Ok(commit_id.to_string())
    }

    /// Discard a speculation.
    #[napi]
    pub fn discard_speculation(&self, handle_id: u32) -> napi::Result<()> {
        let handle = SpecHandle::from_id(handle_id as u64);
        self.repo.discard_speculation(handle).map_err(err)
    }

    // -- Query --

    /// Query commits with composable filters. All optional, AND-combined.
    #[napi]
    pub fn query(
        &self,
        reference: Option<String>,
        agent_id: Option<String>,
        intent_category: Option<String>,
        tags: Option<Vec<String>>,
        reasoning_contains: Option<String>,
        confidence_min: Option<f64>,
        confidence_max: Option<f64>,
        has_deviations: Option<bool>,
        limit: Option<u32>,
    ) -> napi::Result<Vec<serde_json::Value>> {
        let ref_name = reference.unwrap_or_else(|| "main".to_string());
        let max = limit.unwrap_or(20) as usize;
        let filters = agentstategraph_core::QueryFilters {
            agent_id,
            intent_category,
            tags,
            reasoning_contains,
            confidence_range: confidence_min.zip(confidence_max),
            has_deviations,
            ..Default::default()
        };
        let commits = self
            .repo
            .query_commits(&ref_name, &filters, max)
            .map_err(err)?;
        Ok(commits
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
            .collect())
    }

    /// Blame — who last modified a value at a path and why.
    #[napi]
    pub fn blame(
        &self,
        path: String,
        reference: Option<String>,
    ) -> napi::Result<serde_json::Value> {
        let ref_name = reference.unwrap_or_else(|| "main".to_string());
        let entry = self.repo.blame(&ref_name, &path).map_err(err)?;
        serde_json::to_value(&entry).map_err(err)
    }

    // -- Epochs --

    /// Create a new epoch.
    #[napi]
    pub fn create_epoch(
        &self,
        id: String,
        description: String,
        root_intents: Vec<String>,
    ) -> napi::Result<String> {
        self.repo
            .create_epoch(&id, &description, root_intents)
            .map(|e| format!("Epoch '{}' created", e.id))
            .map_err(err)
    }

    /// Seal an epoch.
    #[napi]
    pub fn seal_epoch(&self, id: String, summary: String) -> napi::Result<()> {
        self.repo.seal_epoch(&id, &summary).map_err(err)
    }

    /// List all epochs.
    #[napi]
    pub fn list_epochs(&self) -> napi::Result<Vec<serde_json::Value>> {
        let entries = self.repo.list_epochs().map_err(err)?;
        Ok(entries
            .iter()
            .map(|e| {
                serde_json::json!({
                    "id": e.id,
                    "description": e.description,
                    "status": format!("{:?}", e.status),
                    "commits": e.commit_count,
                    "agents": e.agents,
                    "tags": e.tags,
                })
            })
            .collect())
    }

    /// List active sessions (compact form — kept for backwards compat).
    #[napi]
    pub fn sessions(&self, agent_id: Option<String>) -> napi::Result<Vec<serde_json::Value>> {
        let sessions = self
            .repo
            .sessions()
            .list(agent_id.as_deref())
            .map_err(|e| napi::Error::from_reason(e.to_string()))?;
        Ok(sessions
            .iter()
            .map(|s| {
                serde_json::json!({
                    "id": s.id,
                    "agent": s.agent_id,
                    "branch": s.working_branch,
                    "parent_session": s.parent_session,
                    "path_scope": s.path_scope,
                })
            })
            .collect())
    }

    // -- Session full surface (post-0.6.5 audit) --

    /// Create a durable session record. `head` is resolved from the tip
    /// of `working_branch`.
    #[napi]
    pub fn create_session(
        &self,
        agent_id: String,
        working_branch: Option<String>,
        parent_session: Option<String>,
        delegated_intent: Option<String>,
        report_to: Option<String>,
        path_scope: Option<String>,
    ) -> napi::Result<serde_json::Value> {
        let branch = working_branch.unwrap_or_else(|| "main".to_string());
        let log = self.repo.log(&branch, 1).map_err(err)?;
        let head = log
            .into_iter()
            .next()
            .map(|c| c.id)
            .ok_or_else(|| napi::Error::from_reason(format!("ref {branch:?} empty")))?;
        let mgr = self.repo.sessions();
        let s = mgr
            .create(
                &agent_id,
                &branch,
                head,
                parent_session,
                delegated_intent,
                report_to,
                path_scope,
            )
            .map_err(err)?;
        Ok(session_to_json(&s))
    }

    /// Get a session by id. Returns null when not found.
    #[napi]
    pub fn get_session(&self, id: String) -> napi::Result<Option<serde_json::Value>> {
        let mgr = self.repo.sessions();
        Ok(mgr.get(&id).map_err(err)?.as_ref().map(session_to_json))
    }

    /// List sessions, optionally filtered by agent_id.
    #[napi]
    pub fn list_sessions(
        &self,
        agent_filter: Option<String>,
    ) -> napi::Result<Vec<serde_json::Value>> {
        let mgr = self.repo.sessions();
        let sessions = mgr.list(agent_filter.as_deref()).map_err(err)?;
        Ok(sessions.iter().map(session_to_json).collect())
    }

    /// End a session with `status` ∈ active | completed | abandoned.
    #[napi]
    pub fn end_session(&self, id: String, status: String) -> napi::Result<()> {
        let st = parse_session_status(&status)?;
        let mgr = self.repo.sessions();
        mgr.end(&id, st).map_err(err)
    }

    // -- Schema migration --

    /// Check the stored schema version against this binary's SCHEMA_VERSION.
    /// Returns an object with `status` ∈ up_to_date | upgrade_available | downgrade
    /// | unversioned | corrupt.
    #[napi]
    pub fn check_schema(
        &self,
        reference: Option<String>,
        target: Option<String>,
    ) -> napi::Result<serde_json::Value> {
        use agentstategraph_migrate::{CheckResult, Registry, binary_version, check};

        let ref_name = reference.unwrap_or_else(|| "main".to_string());
        let target = match target {
            Some(s) => semver::Version::parse(&s).map_err(err)?,
            None => binary_version(),
        };
        let registry = Registry::builtin();
        let result = check(&self.repo, &ref_name, &target, &registry).map_err(err)?;
        Ok(match result {
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
        })
    }

    /// Run migrations. `mode` is `"apply"` (default) or `"dry-run"`.
    #[napi]
    pub fn migrate(
        &self,
        reference: Option<String>,
        target: Option<String>,
        mode: Option<String>,
    ) -> napi::Result<serde_json::Value> {
        use agentstategraph_migrate::{Registry, RunMode, StepStatus, binary_version};

        let ref_name = reference.unwrap_or_else(|| "main".to_string());
        let target = match target {
            Some(s) => semver::Version::parse(&s).map_err(err)?,
            None => binary_version(),
        };
        let run_mode = match mode.as_deref().unwrap_or("apply") {
            "apply" => RunMode::Apply,
            "dry-run" | "dry_run" | "dryrun" => RunMode::DryRun,
            other => {
                return Err(napi::Error::from_reason(format!("invalid mode {other:?}")));
            }
        };
        let registry = Registry::builtin();
        let report = registry
            .run(&self.repo, &ref_name, &target, run_mode)
            .map_err(err)?;

        let steps: Vec<serde_json::Value> = report
            .steps
            .iter()
            .map(|s| {
                serde_json::json!({
                    "name": s.name,
                    "describe": s.describe,
                    "from": s.from.to_string(),
                    "to": s.to.to_string(),
                    "status": match s.status {
                        StepStatus::WouldApply => "would_apply",
                        StepStatus::WouldSkip => "would_skip",
                        StepStatus::Applied => "applied",
                        StepStatus::Skipped => "skipped",
                        StepStatus::Failed => "failed",
                    },
                    "commit_id": s.commit_id.as_ref().map(|c| c.to_string()),
                    "notes": s.notes,
                })
            })
            .collect();

        Ok(serde_json::json!({
            "from": report.from.to_string(),
            "target": report.target.to_string(),
            "final_version": report.final_version.to_string(),
            "mode": match report.mode {
                RunMode::Apply => "apply",
                RunMode::DryRun => "dry-run",
            },
            "steps": steps,
        }))
    }
}

/// Exit codes an app should use when surfacing `check_schema()` results.
#[napi]
pub fn exit_codes() -> serde_json::Value {
    use agentstategraph_migrate::exit;
    serde_json::json!({
        "OK": exit::OK,
        "DOWNGRADE_REFUSED": exit::DOWNGRADE_REFUSED,
        "CORRUPT_META": exit::CORRUPT_META,
        "MIGRATION_FAILED": exit::MIGRATION_FAILED,
        "UPGRADE_REQUIRED": exit::UPGRADE_REQUIRED,
    })
}

// =========================================================================
// TaskStore
// =========================================================================

fn task_err(e: TaskStoreError) -> napi::Error {
    napi::Error::from_reason(e.to_string())
}

fn parse_priority(s: &str) -> napi::Result<Priority> {
    Ok(match s.to_lowercase().as_str() {
        "low" => Priority::Low,
        "medium" => Priority::Medium,
        "high" => Priority::High,
        "critical" => Priority::Critical,
        other => {
            return Err(napi::Error::from_reason(format!(
                "invalid priority {other:?}"
            )));
        }
    })
}

fn priority_str(p: Priority) -> &'static str {
    match p {
        Priority::Low => "low",
        Priority::Medium => "medium",
        Priority::High => "high",
        Priority::Critical => "critical",
    }
}

fn status_str(s: TaskStatus) -> &'static str {
    match s {
        TaskStatus::Pending => "pending",
        TaskStatus::InProgress => "in_progress",
        TaskStatus::Done => "done",
        TaskStatus::Abandoned => "abandoned",
    }
}

fn plan_status_str(s: PlanStatus) -> &'static str {
    match s {
        PlanStatus::Active => "active",
        PlanStatus::Completed => "completed",
        PlanStatus::Archived => "archived",
    }
}

fn parse_plan_status(s: &str) -> napi::Result<PlanStatus> {
    Ok(match s.to_lowercase().as_str() {
        "active" => PlanStatus::Active,
        "completed" => PlanStatus::Completed,
        "archived" => PlanStatus::Archived,
        other => {
            return Err(napi::Error::from_reason(format!(
                "invalid plan status {other:?}"
            )));
        }
    })
}

fn parse_proof_kind(s: &str) -> napi::Result<ProofKind> {
    Ok(match s.to_lowercase().as_str() {
        "commit" => ProofKind::Commit,
        "file" => ProofKind::File,
        "test" => ProofKind::Test,
        "text" => ProofKind::Text,
        other => {
            return Err(napi::Error::from_reason(format!(
                "invalid proof kind {other:?}"
            )));
        }
    })
}

fn proof_kind_str(k: ProofKind) -> &'static str {
    match k {
        ProofKind::Commit => "commit",
        ProofKind::File => "file",
        ProofKind::Test => "test",
        ProofKind::Text => "text",
    }
}

fn plan_to_json(p: &Plan) -> serde_json::Value {
    serde_json::json!({
        "name": p.name,
        "description": p.description,
        "status": plan_status_str(p.status),
        "created_at": p.created_at.to_rfc3339(),
        "created_by": p.created_by,
        "archived_at": p.archived_at.map(|t| t.to_rfc3339()),
    })
}

fn task_to_json(t: &Task) -> serde_json::Value {
    serde_json::json!({
        "id": t.id.as_str(),
        "title": t.title,
        "status": status_str(t.status),
        "priority": priority_str(t.priority),
        "parent_id": t.parent_id.as_ref().map(|i| i.as_str().to_string()),
        "blocked_by": t.blocked_by.iter().map(|i| i.as_str().to_string()).collect::<Vec<_>>(),
        "created_at": t.created_at.to_rfc3339(),
        "created_by": t.created_by,
        "started_at": t.started_at.map(|x| x.to_rfc3339()),
        "started_by": t.started_by,
        "completed_at": t.completed_at.map(|x| x.to_rfc3339()),
        "completed_by": t.completed_by,
        "proof": t.proof.as_ref().map(|p| serde_json::json!({
            "kind": proof_kind_str(p.kind),
            "value": p.value,
            "note": p.note,
        })),
        "abandoned_at": t.abandoned_at.map(|x| x.to_rfc3339()),
        "abandoned_reason": t.abandoned_reason,
        "assigned_to": t.assigned_to,
        // Policy-fallback extension fields (POLICY_V1.md §22.4).
        "payload": t.payload,
        "parent_change": t.parent_change,
        "on_complete": t.on_complete.as_ref().map(|h| serde_json::to_value(h).unwrap_or(serde_json::Value::Null)),
    })
}

fn report_to_json(r: &VerifyReport) -> serde_json::Value {
    let entries: Vec<serde_json::Value> = r
        .results
        .iter()
        .map(|e| {
            let (status, msg) = match &e.result {
                VerifyResult::Verified { message } => ("verified", message.clone()),
                VerifyResult::Decayed { reason } => ("decayed", reason.clone()),
                VerifyResult::Unverifiable { reason } => ("unverifiable", reason.clone()),
            };
            serde_json::json!({
                "task_id": e.task_id.as_str(),
                "status": status,
                "message": msg,
            })
        })
        .collect();
    serde_json::json!({
        "plan": r.plan,
        "results": entries,
        "verified_count": r.verified_count(),
        "decayed_count": r.decayed_count(),
        "unverifiable_count": r.unverifiable_count(),
        "all_strongly_verified": r.all_strongly_verified(),
        "summary": r.summary(),
    })
}

struct KindMapVerifier {
    commit: bool,
    file: bool,
    test: bool,
    text: bool,
}

impl Verifier for KindMapVerifier {
    fn verify(&self, proof: &Proof) -> VerifyResult {
        let ok = match proof.kind {
            ProofKind::Commit => self.commit,
            ProofKind::File => self.file,
            ProofKind::Test => self.test,
            ProofKind::Text => self.text,
        };
        if ok {
            VerifyResult::Verified {
                message: format!("{} proof accepted by kind map", proof_kind_str(proof.kind)),
            }
        } else {
            VerifyResult::Unverifiable {
                reason: format!("{} proof not in kind map", proof_kind_str(proof.kind)),
            }
        }
    }
}

/// TaskStore — plans-and-tasks layer on top of an AgentStateGraph.
#[napi]
pub struct TaskStore {
    inner: TasksBackend,
}

#[napi]
impl TaskStore {
    #[napi(constructor)]
    pub fn new(asg: &AgentStateGraph, prefix: Option<String>, agent_id: Option<String>) -> Self {
        let prefix = prefix.unwrap_or_else(|| "/plans".to_string());
        let agent_id = agent_id.unwrap_or_else(|| "node".to_string());
        Self {
            inner: TasksBackend::new(Arc::clone(&asg.repo), prefix, agent_id),
        }
    }

    // --- Plan ---

    #[napi]
    pub fn create_plan(
        &self,
        ref_name: String,
        name: String,
        description: Option<String>,
    ) -> napi::Result<serde_json::Value> {
        let p = self
            .inner
            .create_plan(&ref_name, &name, description)
            .map_err(task_err)?;
        Ok(plan_to_json(&p))
    }

    #[napi]
    pub fn list_plans(&self, ref_name: String) -> napi::Result<Vec<serde_json::Value>> {
        let plans = self.inner.list_plans(&ref_name).map_err(task_err)?;
        Ok(plans.iter().map(plan_to_json).collect())
    }

    #[napi]
    pub fn list_plans_by_status(
        &self,
        ref_name: String,
        status: String,
    ) -> napi::Result<Vec<serde_json::Value>> {
        let s = parse_plan_status(&status)?;
        let plans = self
            .inner
            .list_plans_by_status(&ref_name, Some(s))
            .map_err(task_err)?;
        Ok(plans.iter().map(plan_to_json).collect())
    }

    #[napi]
    pub fn get_plan(&self, ref_name: String, name: String) -> napi::Result<serde_json::Value> {
        let p = self.inner.get_plan(&ref_name, &name).map_err(task_err)?;
        Ok(plan_to_json(&p))
    }

    #[napi]
    pub fn archive_plan(&self, ref_name: String, name: String) -> napi::Result<serde_json::Value> {
        let p = self
            .inner
            .archive_plan(&ref_name, &name)
            .map_err(task_err)?;
        Ok(plan_to_json(&p))
    }

    #[napi]
    pub fn delete_plan(&self, ref_name: String, name: String) -> napi::Result<()> {
        self.inner.delete_plan(&ref_name, &name).map_err(task_err)
    }

    // --- Task ---

    #[napi]
    pub fn add_task(
        &self,
        ref_name: String,
        plan: String,
        title: String,
        priority: Option<String>,
        parent_id: Option<String>,
        blocked_by: Option<Vec<String>>,
        assigned_to: Option<String>,
        payload: Option<serde_json::Value>,
        parent_change: Option<String>,
        on_complete: Option<serde_json::Value>,
    ) -> napi::Result<serde_json::Value> {
        let pri = parse_priority(&priority.unwrap_or_else(|| "medium".to_string()))?;
        let parent = parent_id.map(TaskId);
        let blockers: Vec<TaskId> = blocked_by
            .unwrap_or_default()
            .into_iter()
            .map(TaskId)
            .collect();
        let payload_val = payload.and_then(|v| if v.is_null() { None } else { Some(v) });
        let on_complete_val = match on_complete {
            None => None,
            Some(serde_json::Value::Null) => None,
            Some(v) => Some(
                serde_json::from_value::<OnCompleteHook>(v)
                    .map_err(|e| napi::Error::from_reason(format!("invalid on_complete: {e}")))?,
            ),
        };
        let task = self
            .inner
            .add_task_with_extensions(
                &ref_name,
                &plan,
                &title,
                pri,
                parent,
                blockers,
                assigned_to,
                payload_val,
                parent_change,
                on_complete_val,
            )
            .map_err(task_err)?;
        Ok(task_to_json(&task))
    }

    #[napi]
    pub fn list_tasks(
        &self,
        ref_name: String,
        plan: String,
    ) -> napi::Result<Vec<serde_json::Value>> {
        let tasks = self.inner.list_tasks(&ref_name, &plan).map_err(task_err)?;
        Ok(tasks.iter().map(task_to_json).collect())
    }

    #[napi]
    pub fn task_ids(&self, ref_name: String, plan: String) -> napi::Result<Vec<String>> {
        let ids = self.inner.task_ids(&ref_name, &plan).map_err(task_err)?;
        Ok(ids.into_iter().map(|i| i.0).collect())
    }

    #[napi]
    pub fn get_task(
        &self,
        ref_name: String,
        plan: String,
        id: String,
    ) -> napi::Result<serde_json::Value> {
        let t = self
            .inner
            .get_task(&ref_name, &plan, &TaskId(id))
            .map_err(task_err)?;
        Ok(task_to_json(&t))
    }

    #[napi]
    pub fn start_task(
        &self,
        ref_name: String,
        plan: String,
        id: String,
    ) -> napi::Result<serde_json::Value> {
        let t = self
            .inner
            .start_task(&ref_name, &plan, &TaskId(id))
            .map_err(task_err)?;
        Ok(task_to_json(&t))
    }

    #[napi]
    pub fn complete_task(
        &self,
        ref_name: String,
        plan: String,
        id: String,
        proof_kind: String,
        proof_value: String,
        proof_note: Option<String>,
    ) -> napi::Result<serde_json::Value> {
        let kind = parse_proof_kind(&proof_kind)?;
        let mut proof = Proof {
            kind,
            value: proof_value,
            note: None,
        };
        if let Some(n) = proof_note {
            proof = proof.with_note(n);
        }
        let t = self
            .inner
            .complete_task(&ref_name, &plan, &TaskId(id), proof)
            .map_err(task_err)?;
        Ok(task_to_json(&t))
    }

    #[napi]
    pub fn abandon_task(
        &self,
        ref_name: String,
        plan: String,
        id: String,
        reason: String,
    ) -> napi::Result<serde_json::Value> {
        let t = self
            .inner
            .abandon_task(&ref_name, &plan, &TaskId(id), &reason)
            .map_err(task_err)?;
        Ok(task_to_json(&t))
    }

    #[napi]
    pub fn set_priority(
        &self,
        ref_name: String,
        plan: String,
        id: String,
        priority: String,
    ) -> napi::Result<serde_json::Value> {
        let pri = parse_priority(&priority)?;
        let t = self
            .inner
            .set_priority(&ref_name, &plan, &TaskId(id), pri)
            .map_err(task_err)?;
        Ok(task_to_json(&t))
    }

    #[napi]
    pub fn set_blockers(
        &self,
        ref_name: String,
        plan: String,
        id: String,
        blockers: Vec<String>,
    ) -> napi::Result<serde_json::Value> {
        let b: Vec<TaskId> = blockers.into_iter().map(TaskId).collect();
        let t = self
            .inner
            .set_blockers(&ref_name, &plan, &TaskId(id), b)
            .map_err(task_err)?;
        Ok(task_to_json(&t))
    }

    #[napi]
    pub fn assign_task(
        &self,
        ref_name: String,
        plan: String,
        id: String,
        agent: String,
    ) -> napi::Result<serde_json::Value> {
        let t = self
            .inner
            .assign_task(&ref_name, &plan, &TaskId(id), &agent)
            .map_err(task_err)?;
        Ok(task_to_json(&t))
    }

    #[napi]
    pub fn unassign_task(
        &self,
        ref_name: String,
        plan: String,
        id: String,
    ) -> napi::Result<serde_json::Value> {
        let t = self
            .inner
            .unassign_task(&ref_name, &plan, &TaskId(id))
            .map_err(task_err)?;
        Ok(task_to_json(&t))
    }

    #[napi]
    pub fn next_task(
        &self,
        ref_name: String,
        plan: String,
    ) -> napi::Result<Option<serde_json::Value>> {
        Ok(self
            .inner
            .next_task(&ref_name, &plan)
            .map_err(task_err)?
            .as_ref()
            .map(task_to_json))
    }

    #[napi]
    pub fn next_task_for(
        &self,
        ref_name: String,
        plan: String,
        assigned_to: Option<String>,
        include_unassigned: Option<bool>,
    ) -> napi::Result<Option<serde_json::Value>> {
        Ok(self
            .inner
            .next_task_for(
                &ref_name,
                &plan,
                assigned_to.as_deref(),
                include_unassigned.unwrap_or(true),
            )
            .map_err(task_err)?
            .as_ref()
            .map(task_to_json))
    }

    #[napi]
    pub fn derived_status(
        &self,
        ref_name: String,
        plan: String,
        parent_id: String,
    ) -> napi::Result<String> {
        let s = self
            .inner
            .derived_status(&ref_name, &plan, &TaskId(parent_id))
            .map_err(task_err)?;
        Ok(status_str(s).to_string())
    }

    /// Run a canned verifier: proof kinds with `true` in `verify_by_kind`
    /// are reported as Verified; others as Unverifiable. Map keys: commit,
    /// file, test, text.
    #[napi]
    pub fn verify_plan_with_kinds(
        &self,
        ref_name: String,
        plan: String,
        verify_by_kind: std::collections::HashMap<String, bool>,
    ) -> napi::Result<serde_json::Value> {
        let v = KindMapVerifier {
            commit: *verify_by_kind.get("commit").unwrap_or(&false),
            file: *verify_by_kind.get("file").unwrap_or(&false),
            test: *verify_by_kind.get("test").unwrap_or(&false),
            text: *verify_by_kind.get("text").unwrap_or(&false),
        };
        let report = self
            .inner
            .verify_plan(&ref_name, &plan, &v)
            .map_err(task_err)?;
        Ok(report_to_json(&report))
    }

    #[napi]
    pub fn verify_plan_noop(
        &self,
        ref_name: String,
        plan: String,
    ) -> napi::Result<serde_json::Value> {
        let report = self
            .inner
            .verify_plan(&ref_name, &plan, &NoopVerifier)
            .map_err(task_err)?;
        Ok(report_to_json(&report))
    }
}

// =========================================================================
// Session helpers
// =========================================================================

fn session_status_str(s: &SessionStatus) -> &'static str {
    match s {
        SessionStatus::Active => "active",
        SessionStatus::Completed => "completed",
        SessionStatus::Abandoned => "abandoned",
    }
}

fn parse_session_status(s: &str) -> napi::Result<SessionStatus> {
    Ok(match s.to_lowercase().as_str() {
        "active" => SessionStatus::Active,
        "completed" => SessionStatus::Completed,
        "abandoned" => SessionStatus::Abandoned,
        other => {
            return Err(napi::Error::from_reason(format!(
                "invalid session status {other:?}; expected active|completed|abandoned"
            )));
        }
    })
}

fn session_to_json(s: &Session) -> serde_json::Value {
    serde_json::json!({
        "id": s.id,
        "agent_id": s.agent_id,
        "working_branch": s.working_branch,
        "head": s.head.to_string(),
        "parent_session": s.parent_session,
        "delegated_intent": s.delegated_intent,
        "report_to": s.report_to,
        "path_scope": s.path_scope,
        "status": session_status_str(&s.status),
        "created_at": s.created_at.to_rfc3339(),
        "ended_at": s.ended_at.map(|t| t.to_rfc3339()),
    })
}

// =========================================================================
// PolicyStore — wraps agentstategraph_policy::PolicyStore
// =========================================================================

fn policy_err(e: agentstategraph_policy::PolicyError) -> napi::Error {
    napi::Error::from_reason(e.to_string())
}

/// PolicyStore — situation-matching authorization + change-cost policies.
///
/// Wraps `agentstategraph_policy::PolicyStore`. Complex values (Policy,
/// Situation, ChangeProposal, Decision) pass as plain JS objects and are
/// round-tripped through serde_json — same idiom as the Python binding.
#[napi]
pub struct PolicyStore {
    inner: PolicyBackend,
}

#[napi]
impl PolicyStore {
    #[napi(constructor)]
    pub fn new(asg: &AgentStateGraph, prefix: Option<String>, agent_id: Option<String>) -> Self {
        let prefix = prefix.unwrap_or_else(|| "/policies".to_string());
        let agent_id = agent_id.unwrap_or_else(|| "node".to_string());
        Self {
            inner: PolicyBackend::new(Arc::clone(&asg.repo), prefix, agent_id),
        }
    }

    // --- Write ops ---

    /// Write a proposed (unratified) policy. Returns `"path@version"`.
    #[napi]
    pub fn propose(&self, ref_name: String, policy: serde_json::Value) -> napi::Result<String> {
        let p: Policy = serde_json::from_value(policy)
            .map_err(|e| napi::Error::from_reason(format!("invalid policy: {e}")))?;
        self.inner.propose(&ref_name, p).map_err(policy_err)
    }

    /// Ratify an unratified proposal at `path`.
    #[napi]
    pub fn ratify(
        &self,
        ref_name: String,
        path: String,
        ratifier: String,
        reasoning: String,
    ) -> napi::Result<()> {
        self.inner
            .ratify(&ref_name, &path, &ratifier, &reasoning)
            .map_err(policy_err)
    }

    /// Replace the active policy at `path`. Returns the new handle.
    #[napi]
    pub fn supersede(
        &self,
        ref_name: String,
        path: String,
        new_policy: serde_json::Value,
    ) -> napi::Result<String> {
        let p: Policy = serde_json::from_value(new_policy)
            .map_err(|e| napi::Error::from_reason(format!("invalid policy: {e}")))?;
        self.inner
            .supersede(&ref_name, &path, p)
            .map_err(policy_err)
    }

    // --- Read ops ---

    /// List policies whose path starts with `prefix_filter` (null = all).
    #[napi]
    pub fn list(
        &self,
        ref_name: String,
        prefix_filter: Option<String>,
    ) -> napi::Result<Vec<serde_json::Value>> {
        let ps = self
            .inner
            .list(&ref_name, prefix_filter.as_deref())
            .map_err(policy_err)?;
        Ok(ps.iter().map(policy_to_json).collect())
    }

    /// List currently-active (ratified + `active_from <= now`) policies.
    #[napi]
    pub fn active(
        &self,
        ref_name: String,
        prefix_filter: Option<String>,
    ) -> napi::Result<Vec<serde_json::Value>> {
        let ps = self
            .inner
            .active(&ref_name, prefix_filter.as_deref())
            .map_err(policy_err)?;
        Ok(ps.iter().map(policy_to_json).collect())
    }

    /// Fetch a policy at `path`. Pass `version` to get a pinned
    /// historical version; omit for the current active one.
    #[napi]
    pub fn get(
        &self,
        ref_name: String,
        path: String,
        version: Option<u32>,
    ) -> napi::Result<serde_json::Value> {
        let v = version.map(|x| x as u64);
        let p = self.inner.get(&ref_name, &path, v).map_err(policy_err)?;
        Ok(policy_to_json(&p))
    }

    /// Walk the supersedes chain, oldest first → current.
    #[napi]
    pub fn history(&self, ref_name: String, path: String) -> napi::Result<Vec<serde_json::Value>> {
        let ps = self.inner.history(&ref_name, &path).map_err(policy_err)?;
        Ok(ps.iter().map(policy_to_json).collect())
    }

    /// Authorization evaluation (POLICY_V1.md §5). `situation` is a
    /// flat {string: string} map.
    #[napi]
    pub fn evaluate(
        &self,
        ref_name: String,
        situation: serde_json::Value,
        action: String,
        agent_id: String,
    ) -> napi::Result<serde_json::Value> {
        let sit = situation_from_json(situation)?;
        let d = self
            .inner
            .evaluate(&ref_name, &sit, &action, &agent_id)
            .map_err(policy_err)?;
        Ok(decision_to_json(&d))
    }

    /// Change-proposal evaluation (POLICY_V1.md §22.2).
    #[napi]
    pub fn evaluate_change(
        &self,
        ref_name: String,
        proposal: serde_json::Value,
    ) -> napi::Result<serde_json::Value> {
        let prop: ChangeProposal = serde_json::from_value(proposal)
            .map_err(|e| napi::Error::from_reason(format!("invalid proposal: {e}")))?;
        let d = self
            .inner
            .evaluate_change(&ref_name, &prop)
            .map_err(policy_err)?;
        Ok(decision_to_json(&d))
    }

    /// List active policies whose `triggers` intersect `tokens`.
    /// Binding-level helper mirroring the internal filter used by
    /// `evaluate_change` (TODO: hoist into PolicyStore proper).
    #[napi]
    pub fn check_tokens(
        &self,
        ref_name: String,
        tokens: Vec<String>,
    ) -> napi::Result<Vec<serde_json::Value>> {
        let actives = self.inner.active(&ref_name, None).map_err(policy_err)?;
        let token_set: std::collections::HashSet<&str> =
            tokens.iter().map(|s| s.as_str()).collect();
        let matched: Vec<serde_json::Value> = actives
            .iter()
            .filter(|p| p.triggers.iter().any(|t| token_set.contains(t.as_str())))
            .map(policy_to_json)
            .collect();
        Ok(matched)
    }
}

fn policy_to_json(p: &Policy) -> serde_json::Value {
    serde_json::to_value(p).unwrap_or(serde_json::Value::Null)
}

fn decision_to_json(d: &Decision) -> serde_json::Value {
    serde_json::to_value(d).unwrap_or(serde_json::Value::Null)
}

fn situation_from_json(v: serde_json::Value) -> napi::Result<Situation> {
    // Accept either a flat {string: string} map (the transparent serde
    // form) or a {"facts": {...}} wrapper. Fall back to a full round-trip.
    if let serde_json::Value::Object(ref map) = v
        && map.values().all(|x| x.is_string())
    {
        let hm: std::collections::HashMap<String, String> = map
            .iter()
            .map(|(k, val)| (k.clone(), val.as_str().unwrap_or("").to_string()))
            .collect();
        return Ok(Situation::from(hm));
    }
    serde_json::from_value(v)
        .map_err(|e| napi::Error::from_reason(format!("invalid situation: {e}")))
}
