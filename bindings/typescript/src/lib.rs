//! TypeScript/Node.js bindings for StateGraph via napi-rs.
//!
//! Usage:
//!   const { StateGraph } = require('stategraph')
//!   const sg = new StateGraph()           // in-memory
//!   const sg = new StateGraph("state.db") // SQLite
//!
//!   sg.set("/name", "my-cluster", { category: "Checkpoint", description: "init" })
//!   sg.get("/name")  // → "my-cluster"

#[macro_use]
extern crate napi_derive;

use stategraph::speculation::SpecHandle;
use stategraph::{CommitOptions, Repository};
use stategraph_core::{IntentCategory, Object};
use stategraph_storage::{MemoryStorage, SqliteStorage};

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

/// StateGraph — AI-native versioned state store.
#[napi]
pub struct StateGraph {
    repo: Repository,
}

#[napi]
impl StateGraph {
    /// Create a new StateGraph.
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
        Ok(Self { repo })
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
        let commit_id = self.repo.set_json(&ref_name, &path, &value, opts).map_err(err)?;
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
    pub fn log(&self, reference: Option<String>, limit: Option<u32>) -> napi::Result<Vec<serde_json::Value>> {
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
                stategraph_core::Atom::Null => Ok(serde_json::Value::Null),
                stategraph_core::Atom::Bool(b) => Ok(serde_json::json!(b)),
                stategraph_core::Atom::Int(i) => Ok(serde_json::json!(i)),
                stategraph_core::Atom::Float(f) => Ok(serde_json::json!(f)),
                stategraph_core::Atom::String(s) => Ok(serde_json::json!(s)),
                stategraph_core::Atom::Bytes(b) => Ok(serde_json::json!(format!("bytes:{}", b.len()))),
            },
            _ => Ok(serde_json::json!(format!("{:?}", obj))),
        }
    }

    /// Set a value within a speculation.
    #[napi]
    pub fn spec_set(&self, handle_id: u32, path: String, value: serde_json::Value) -> napi::Result<()> {
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
        let filters = stategraph_core::QueryFilters {
            agent_id,
            intent_category,
            tags,
            reasoning_contains,
            confidence_range: confidence_min.zip(confidence_max),
            has_deviations,
            ..Default::default()
        };
        let commits = self.repo.query_commits(&ref_name, &filters, max).map_err(err)?;
        Ok(commits.iter().map(|c| {
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
        }).collect())
    }

    /// Blame — who last modified a value at a path and why.
    #[napi]
    pub fn blame(&self, path: String, reference: Option<String>) -> napi::Result<serde_json::Value> {
        let ref_name = reference.unwrap_or_else(|| "main".to_string());
        let entry = self.repo.blame(&ref_name, &path).map_err(err)?;
        serde_json::to_value(&entry).map_err(err)
    }

    // -- Epochs --

    /// Create a new epoch.
    #[napi]
    pub fn create_epoch(&self, id: String, description: String, root_intents: Vec<String>) -> napi::Result<String> {
        self.repo.create_epoch(&id, &description, root_intents)
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
        Ok(entries.iter().map(|e| {
            serde_json::json!({
                "id": e.id,
                "description": e.description,
                "status": format!("{:?}", e.status),
                "commits": e.commit_count,
                "agents": e.agents,
                "tags": e.tags,
            })
        }).collect())
    }

    /// List active sessions.
    #[napi]
    pub fn sessions(&self, agent_id: Option<String>) -> napi::Result<Vec<serde_json::Value>> {
        let sessions = self.repo.sessions().list(agent_id.as_deref());
        Ok(sessions.iter().map(|s| {
            serde_json::json!({
                "id": s.id,
                "agent": s.agent_id,
                "branch": s.working_branch,
                "parent_session": s.parent_session,
                "path_scope": s.path_scope,
            })
        }).collect())
    }
}
