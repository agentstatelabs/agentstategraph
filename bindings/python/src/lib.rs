//! Python bindings for StateGraph via PyO3.
//!
//! Usage:
//!   from stategraph import StateGraph
//!   sg = StateGraph()                    # in-memory
//!   sg = StateGraph("./state.db")        # SQLite
//!
//!   sg.set("/name", "my-cluster", category="Checkpoint", description="init")
//!   sg.get("/name")  # → "my-cluster"
//!   sg.branch("feature", "main")
//!   sg.diff("main", "feature")
//!   sg.merge("feature", "main", description="merge feature")

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

use agentstategraph::speculation::SpecHandle;
use agentstategraph::{CommitOptions, Repository};
use agentstategraph_core::{IntentCategory, Object};
use agentstategraph_storage::{MemoryStorage, SqliteStorage};

/// Convert a Python JSON-compatible value to a StateGraph Object.
fn py_to_object(py: Python<'_>, value: &Bound<'_, PyAny>) -> PyResult<Object> {
    if value.is_none() {
        Ok(Object::null())
    } else if let Ok(b) = value.extract::<bool>() {
        Ok(Object::bool(b))
    } else if let Ok(i) = value.extract::<i64>() {
        Ok(Object::int(i))
    } else if let Ok(f) = value.extract::<f64>() {
        Ok(Object::float(f))
    } else if let Ok(s) = value.extract::<String>() {
        Ok(Object::string(s))
    } else {
        // For complex types, serialize via JSON
        let json_mod = py.import("json")?;
        let json_str: String = json_mod.call_method1("dumps", (value,))?.extract()?;
        let json_val: serde_json::Value = serde_json::from_str(&json_str)
            .map_err(|e| PyRuntimeError::new_err(format!("JSON parse error: {}", e)))?;
        // Store as string representation for now
        Ok(Object::string(json_str))
    }
}

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
    agent: Option<String>,
    category: Option<String>,
    description: &str,
    reasoning: Option<String>,
    confidence: Option<f64>,
    tags: Option<Vec<String>>,
) -> CommitOptions {
    let agent_id = agent.unwrap_or_else(|| "python".to_string());
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

/// StateGraph — AI-native versioned state store.
///
/// Every write is an atomic commit with intent metadata.
/// Supports branching, merging, diffing, and speculative execution.
#[pyclass]
struct StateGraph {
    repo: Repository,
}

#[pymethods]
impl StateGraph {
    /// Create a new StateGraph.
    /// Pass a path for SQLite (durable), or None for in-memory (ephemeral).
    #[new]
    #[pyo3(signature = (path=None))]
    fn new(path: Option<String>) -> PyResult<Self> {
        let repo = match path {
            Some(p) => {
                let storage = SqliteStorage::open(&p)
                    .map_err(|e| PyRuntimeError::new_err(format!("storage error: {}", e)))?;
                Repository::new(Box::new(storage))
            }
            None => Repository::new(Box::new(MemoryStorage::new())),
        };
        repo.init()
            .map_err(|e| PyRuntimeError::new_err(format!("init error: {}", e)))?;
        Ok(Self { repo })
    }

    // -- State operations --

    /// Get a value at a path. Returns a JSON-compatible Python object.
    #[pyo3(signature = (path, r#ref="main"))]
    fn get(&self, py: Python<'_>, path: &str, r#ref: &str) -> PyResult<PyObject> {
        let json = self
            .repo
            .get_json(r#ref, path)
            .map_err(|e| PyRuntimeError::new_err(format!("{}", e)))?;
        json_to_py(py, &json)
    }

    /// Set a value at a path, creating a commit.
    #[pyo3(signature = (path, value, description, r#ref="main", category=None, agent=None, reasoning=None, confidence=None, tags=None))]
    fn set(
        &self,
        py: Python<'_>,
        path: &str,
        value: &Bound<'_, PyAny>,
        description: &str,
        r#ref: &str,
        category: Option<String>,
        agent: Option<String>,
        reasoning: Option<String>,
        confidence: Option<f64>,
        tags: Option<Vec<String>>,
    ) -> PyResult<String> {
        let obj = py_to_object(py, value)?;
        let opts = make_opts(agent, category, description, reasoning, confidence, tags);
        let commit_id = self
            .repo
            .set(r#ref, path, &obj, opts)
            .map_err(|e| PyRuntimeError::new_err(format!("{}", e)))?;
        Ok(commit_id.to_string())
    }

    /// Set a JSON value (pass a dict/list/etc).
    #[pyo3(signature = (path, value, description, r#ref="main", category=None, agent=None, reasoning=None, confidence=None, tags=None))]
    fn set_json(
        &self,
        py: Python<'_>,
        path: &str,
        value: &Bound<'_, PyAny>,
        description: &str,
        r#ref: &str,
        category: Option<String>,
        agent: Option<String>,
        reasoning: Option<String>,
        confidence: Option<f64>,
        tags: Option<Vec<String>>,
    ) -> PyResult<String> {
        let json_mod = py.import("json")?;
        let json_str: String = json_mod.call_method1("dumps", (value,))?.extract()?;
        let json_val: serde_json::Value = serde_json::from_str(&json_str)
            .map_err(|e| PyRuntimeError::new_err(format!("JSON error: {}", e)))?;

        let opts = make_opts(agent, category, description, reasoning, confidence, tags);
        let commit_id = self
            .repo
            .set_json(r#ref, path, &json_val, opts)
            .map_err(|e| PyRuntimeError::new_err(format!("{}", e)))?;
        Ok(commit_id.to_string())
    }

    /// Delete a value at a path.
    #[pyo3(signature = (path, description, r#ref="main", category=None))]
    fn delete(
        &self,
        path: &str,
        description: &str,
        r#ref: &str,
        category: Option<String>,
    ) -> PyResult<String> {
        let opts = make_opts(None, category, description, None, None, None);
        let commit_id = self
            .repo
            .delete(r#ref, path, opts)
            .map_err(|e| PyRuntimeError::new_err(format!("{}", e)))?;
        Ok(commit_id.to_string())
    }

    // -- Branch operations --

    /// Create a branch from a ref.
    #[pyo3(signature = (name, from="main"))]
    fn branch(&self, name: &str, from: &str) -> PyResult<String> {
        let id = self
            .repo
            .branch(name, from)
            .map_err(|e| PyRuntimeError::new_err(format!("{}", e)))?;
        Ok(id.to_string())
    }

    /// Delete a branch.
    fn delete_branch(&self, name: &str) -> PyResult<bool> {
        self.repo
            .delete_branch(name)
            .map_err(|e| PyRuntimeError::new_err(format!("{}", e)))
    }

    /// List branches.
    #[pyo3(signature = (prefix=None))]
    fn list_branches(&self, prefix: Option<&str>) -> PyResult<Vec<(String, String)>> {
        let branches = self
            .repo
            .list_branches(prefix)
            .map_err(|e| PyRuntimeError::new_err(format!("{}", e)))?;
        Ok(branches
            .into_iter()
            .map(|(name, id)| (name, id.short()))
            .collect())
    }

    // -- Merge --

    /// Merge source branch into target.
    #[pyo3(signature = (source, target="main", description="merge", reasoning=None))]
    fn merge(
        &self,
        source: &str,
        target: &str,
        description: &str,
        reasoning: Option<String>,
    ) -> PyResult<String> {
        let mut opts = CommitOptions::new("python", IntentCategory::Merge, description);
        if let Some(r) = reasoning {
            opts = opts.with_reasoning(r);
        }
        let commit_id = self
            .repo
            .merge(source, target, opts)
            .map_err(|e| PyRuntimeError::new_err(format!("{}", e)))?;
        Ok(commit_id.to_string())
    }

    // -- Diff --

    /// Structured diff between two refs. Returns list of change dicts.
    fn diff(&self, py: Python<'_>, ref_a: &str, ref_b: &str) -> PyResult<PyObject> {
        let ops = self
            .repo
            .diff(ref_a, ref_b)
            .map_err(|e| PyRuntimeError::new_err(format!("{}", e)))?;
        let json =
            serde_json::to_value(&ops).map_err(|e| PyRuntimeError::new_err(format!("{}", e)))?;
        json_to_py(py, &json)
    }

    // -- Log --

    /// Commit log from a ref. Returns list of commit dicts.
    #[pyo3(signature = (r#ref="main", limit=10))]
    fn log(&self, py: Python<'_>, r#ref: &str, limit: usize) -> PyResult<PyObject> {
        let commits = self
            .repo
            .log(r#ref, limit)
            .map_err(|e| PyRuntimeError::new_err(format!("{}", e)))?;
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
                    "parents": c.parents.len(),
                    "timestamp": c.timestamp.to_rfc3339(),
                })
            })
            .collect();
        let json = serde_json::to_value(&entries)
            .map_err(|e| PyRuntimeError::new_err(format!("{}", e)))?;
        json_to_py(py, &json)
    }

    // -- Speculation --

    /// Create a speculation from a ref. Returns handle ID.
    #[pyo3(signature = (from="main", label=None))]
    fn speculate(&self, from: &str, label: Option<String>) -> PyResult<u64> {
        let handle = self
            .repo
            .speculate(from, label)
            .map_err(|e| PyRuntimeError::new_err(format!("{}", e)))?;
        Ok(handle.id())
    }

    /// Get a value from a speculation.
    fn spec_get(&self, py: Python<'_>, handle_id: u64, path: &str) -> PyResult<PyObject> {
        let handle = SpecHandle::from_id(handle_id);
        let obj = self
            .repo
            .spec_get(handle, path)
            .map_err(|e| PyRuntimeError::new_err(format!("{}", e)))?;
        // Convert Object to Python via JSON
        let json = match &obj {
            Object::Atom(a) => match a {
                agentstategraph_core::Atom::Null => serde_json::Value::Null,
                agentstategraph_core::Atom::Bool(b) => serde_json::json!(b),
                agentstategraph_core::Atom::Int(i) => serde_json::json!(i),
                agentstategraph_core::Atom::Float(f) => serde_json::json!(f),
                agentstategraph_core::Atom::String(s) => serde_json::json!(s),
                agentstategraph_core::Atom::Bytes(b) => {
                    serde_json::json!(format!("bytes:{}", b.len()))
                }
            },
            _ => serde_json::json!(format!("{:?}", obj)),
        };
        json_to_py(py, &json)
    }

    /// Set a value within a speculation.
    fn spec_set(
        &self,
        py: Python<'_>,
        handle_id: u64,
        path: &str,
        value: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let handle = SpecHandle::from_id(handle_id);
        let obj = py_to_object(py, value)?;
        self.repo
            .spec_set(handle, path, &obj)
            .map_err(|e| PyRuntimeError::new_err(format!("{}", e)))
    }

    /// Commit a speculation to its base branch.
    #[pyo3(signature = (handle_id, description, category=None, reasoning=None, confidence=None))]
    fn commit_speculation(
        &self,
        handle_id: u64,
        description: &str,
        category: Option<String>,
        reasoning: Option<String>,
        confidence: Option<f64>,
    ) -> PyResult<String> {
        let handle = SpecHandle::from_id(handle_id);
        let opts = make_opts(None, category, description, reasoning, confidence, None);
        let commit_id = self
            .repo
            .commit_speculation(handle, opts)
            .map_err(|e| PyRuntimeError::new_err(format!("{}", e)))?;
        Ok(commit_id.to_string())
    }

    /// Discard a speculation.
    fn discard_speculation(&self, handle_id: u64) -> PyResult<()> {
        let handle = SpecHandle::from_id(handle_id);
        self.repo
            .discard_speculation(handle)
            .map_err(|e| PyRuntimeError::new_err(format!("{}", e)))
    }

    // -- Query --

    /// Query commits with composable filters. All filters are AND-combined.
    #[pyo3(signature = (r#ref="main", agent_id=None, intent_category=None, tags=None, reasoning_contains=None, confidence_min=None, confidence_max=None, has_deviations=None, limit=20))]
    fn query(
        &self,
        py: Python<'_>,
        r#ref: &str,
        agent_id: Option<String>,
        intent_category: Option<String>,
        tags: Option<Vec<String>>,
        reasoning_contains: Option<String>,
        confidence_min: Option<f64>,
        confidence_max: Option<f64>,
        has_deviations: Option<bool>,
        limit: usize,
    ) -> PyResult<PyObject> {
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
            .query_commits(r#ref, &filters, limit)
            .map_err(|e| PyRuntimeError::new_err(format!("{}", e)))?;
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
        let json = serde_json::to_value(&entries)
            .map_err(|e| PyRuntimeError::new_err(format!("{}", e)))?;
        json_to_py(py, &json)
    }

    /// Blame — who last modified a value at a path and why.
    #[pyo3(signature = (path, r#ref="main"))]
    fn blame(&self, py: Python<'_>, path: &str, r#ref: &str) -> PyResult<PyObject> {
        let entry = self
            .repo
            .blame(r#ref, path)
            .map_err(|e| PyRuntimeError::new_err(format!("{}", e)))?;
        let json =
            serde_json::to_value(&entry).map_err(|e| PyRuntimeError::new_err(format!("{}", e)))?;
        json_to_py(py, &json)
    }

    // -- Epochs --

    /// Create a new epoch to group related work.
    fn create_epoch(
        &self,
        id: &str,
        description: &str,
        root_intents: Vec<String>,
    ) -> PyResult<String> {
        self.repo
            .create_epoch(id, description, root_intents)
            .map(|e| format!("Epoch '{}' created", e.id))
            .map_err(|e| PyRuntimeError::new_err(format!("{}", e)))
    }

    /// Seal an epoch, making it immutable and tamper-evident.
    fn seal_epoch(&self, id: &str, summary: &str) -> PyResult<()> {
        self.repo
            .seal_epoch(id, summary)
            .map_err(|e| PyRuntimeError::new_err(format!("{}", e)))
    }

    /// List all epochs.
    fn list_epochs(&self, py: Python<'_>) -> PyResult<PyObject> {
        let entries = self
            .repo
            .list_epochs()
            .map_err(|e| PyRuntimeError::new_err(format!("{}", e)))?;
        let json: Vec<serde_json::Value> = entries
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
            .collect();
        let val =
            serde_json::to_value(&json).map_err(|e| PyRuntimeError::new_err(format!("{}", e)))?;
        json_to_py(py, &val)
    }

    // -- Watch --

    /// Subscribe to state changes matching a path pattern. Returns subscription ID.
    /// pattern_type: "exact", "prefix", or "all"
    #[pyo3(signature = (pattern_type="all", pattern=None))]
    fn watch(&self, pattern_type: &str, pattern: Option<String>) -> PyResult<u64> {
        let pat = match pattern_type {
            "exact" => agentstategraph::PathPattern::Exact(pattern.unwrap_or_default()),
            "prefix" => agentstategraph::PathPattern::Prefix(pattern.unwrap_or_default()),
            _ => agentstategraph::PathPattern::All,
        };
        let sub_id = self.repo.watches().subscribe(pat);
        // Return the raw inner value — SubscriptionId is opaque
        Ok(0) // placeholder — need to expose SubscriptionId
    }

    /// Get pending events for a watch subscription.
    fn watch_events(&self, py: Python<'_>, subscription_id: u64) -> PyResult<PyObject> {
        // Simplified: return empty for now until SubscriptionId is properly exposed
        json_to_py(py, &serde_json::json!([]))
    }
}

/// Convert serde_json::Value to a Python object.
fn json_to_py(py: Python<'_>, value: &serde_json::Value) -> PyResult<PyObject> {
    let json_mod = py.import("json")?;
    let json_str =
        serde_json::to_string(value).map_err(|e| PyRuntimeError::new_err(format!("{}", e)))?;
    let result = json_mod.call_method1("loads", (json_str,))?;
    Ok(result.into())
}

/// Python module definition.
#[pymodule]
fn agentstategraph_py(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<StateGraph>()?;
    Ok(())
}
