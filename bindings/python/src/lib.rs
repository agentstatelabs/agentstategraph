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

use pyo3::prelude::*;
use pyo3::exceptions::PyRuntimeError;

use stategraph::speculation::SpecHandle;
use stategraph::{CommitOptions, Repository};
use stategraph_core::{IntentCategory, Object};
use stategraph_storage::{MemoryStorage, SqliteStorage};

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
        let json = self.repo.get_json(r#ref, path)
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
        let commit_id = self.repo.set(r#ref, path, &obj, opts)
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
        let commit_id = self.repo.set_json(r#ref, path, &json_val, opts)
            .map_err(|e| PyRuntimeError::new_err(format!("{}", e)))?;
        Ok(commit_id.to_string())
    }

    /// Delete a value at a path.
    #[pyo3(signature = (path, description, r#ref="main", category=None))]
    fn delete(&self, path: &str, description: &str, r#ref: &str, category: Option<String>) -> PyResult<String> {
        let opts = make_opts(None, category, description, None, None, None);
        let commit_id = self.repo.delete(r#ref, path, opts)
            .map_err(|e| PyRuntimeError::new_err(format!("{}", e)))?;
        Ok(commit_id.to_string())
    }

    // -- Branch operations --

    /// Create a branch from a ref.
    #[pyo3(signature = (name, from="main"))]
    fn branch(&self, name: &str, from: &str) -> PyResult<String> {
        let id = self.repo.branch(name, from)
            .map_err(|e| PyRuntimeError::new_err(format!("{}", e)))?;
        Ok(id.to_string())
    }

    /// Delete a branch.
    fn delete_branch(&self, name: &str) -> PyResult<bool> {
        self.repo.delete_branch(name)
            .map_err(|e| PyRuntimeError::new_err(format!("{}", e)))
    }

    /// List branches.
    #[pyo3(signature = (prefix=None))]
    fn list_branches(&self, prefix: Option<&str>) -> PyResult<Vec<(String, String)>> {
        let branches = self.repo.list_branches(prefix)
            .map_err(|e| PyRuntimeError::new_err(format!("{}", e)))?;
        Ok(branches.into_iter().map(|(name, id)| (name, id.short())).collect())
    }

    // -- Merge --

    /// Merge source branch into target.
    #[pyo3(signature = (source, target="main", description="merge", reasoning=None))]
    fn merge(&self, source: &str, target: &str, description: &str, reasoning: Option<String>) -> PyResult<String> {
        let mut opts = CommitOptions::new("python", IntentCategory::Merge, description);
        if let Some(r) = reasoning {
            opts = opts.with_reasoning(r);
        }
        let commit_id = self.repo.merge(source, target, opts)
            .map_err(|e| PyRuntimeError::new_err(format!("{}", e)))?;
        Ok(commit_id.to_string())
    }

    // -- Diff --

    /// Structured diff between two refs. Returns list of change dicts.
    fn diff(&self, py: Python<'_>, ref_a: &str, ref_b: &str) -> PyResult<PyObject> {
        let ops = self.repo.diff(ref_a, ref_b)
            .map_err(|e| PyRuntimeError::new_err(format!("{}", e)))?;
        let json = serde_json::to_value(&ops)
            .map_err(|e| PyRuntimeError::new_err(format!("{}", e)))?;
        json_to_py(py, &json)
    }

    // -- Log --

    /// Commit log from a ref. Returns list of commit dicts.
    #[pyo3(signature = (r#ref="main", limit=10))]
    fn log(&self, py: Python<'_>, r#ref: &str, limit: usize) -> PyResult<PyObject> {
        let commits = self.repo.log(r#ref, limit)
            .map_err(|e| PyRuntimeError::new_err(format!("{}", e)))?;
        let entries: Vec<serde_json::Value> = commits.iter().map(|c| {
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
        }).collect();
        let json = serde_json::to_value(&entries)
            .map_err(|e| PyRuntimeError::new_err(format!("{}", e)))?;
        json_to_py(py, &json)
    }

    // -- Speculation --

    /// Create a speculation from a ref. Returns handle ID.
    #[pyo3(signature = (from="main", label=None))]
    fn speculate(&self, from: &str, label: Option<String>) -> PyResult<u64> {
        let handle = self.repo.speculate(from, label)
            .map_err(|e| PyRuntimeError::new_err(format!("{}", e)))?;
        Ok(handle.id())
    }

    /// Get a value from a speculation.
    fn spec_get(&self, py: Python<'_>, handle_id: u64, path: &str) -> PyResult<PyObject> {
        let handle = SpecHandle::from_id(handle_id);
        let obj = self.repo.spec_get(handle, path)
            .map_err(|e| PyRuntimeError::new_err(format!("{}", e)))?;
        // Convert Object to Python via JSON
        let json = match &obj {
            Object::Atom(a) => match a {
                stategraph_core::Atom::Null => serde_json::Value::Null,
                stategraph_core::Atom::Bool(b) => serde_json::json!(b),
                stategraph_core::Atom::Int(i) => serde_json::json!(i),
                stategraph_core::Atom::Float(f) => serde_json::json!(f),
                stategraph_core::Atom::String(s) => serde_json::json!(s),
                stategraph_core::Atom::Bytes(b) => serde_json::json!(format!("bytes:{}", b.len())),
            },
            _ => serde_json::json!(format!("{:?}", obj)),
        };
        json_to_py(py, &json)
    }

    /// Set a value within a speculation.
    fn spec_set(&self, py: Python<'_>, handle_id: u64, path: &str, value: &Bound<'_, PyAny>) -> PyResult<()> {
        let handle = SpecHandle::from_id(handle_id);
        let obj = py_to_object(py, value)?;
        self.repo.spec_set(handle, path, &obj)
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
        let commit_id = self.repo.commit_speculation(handle, opts)
            .map_err(|e| PyRuntimeError::new_err(format!("{}", e)))?;
        Ok(commit_id.to_string())
    }

    /// Discard a speculation.
    fn discard_speculation(&self, handle_id: u64) -> PyResult<()> {
        let handle = SpecHandle::from_id(handle_id);
        self.repo.discard_speculation(handle)
            .map_err(|e| PyRuntimeError::new_err(format!("{}", e)))
    }
}

/// Convert serde_json::Value to a Python object.
fn json_to_py(py: Python<'_>, value: &serde_json::Value) -> PyResult<PyObject> {
    let json_mod = py.import("json")?;
    let json_str = serde_json::to_string(value)
        .map_err(|e| PyRuntimeError::new_err(format!("{}", e)))?;
    let result = json_mod.call_method1("loads", (json_str,))?;
    Ok(result.into())
}

/// Python module definition.
#[pymodule]
fn stategraph_py(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<StateGraph>()?;
    Ok(())
}
