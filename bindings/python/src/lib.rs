//! Python bindings for AgentStateGraph via PyO3.
//!
//! Usage:
//!   from agentstategraph_py import AgentStateGraph
//!   asg = AgentStateGraph()                    # in-memory
//!   asg = AgentStateGraph("./state.db")        # SQLite
//!
//!   asg.set("/name", "my-cluster", category="Checkpoint", description="init")
//!   asg.get("/name")  # → "my-cluster"
//!   asg.branch("feature", "main")
//!   asg.diff("main", "feature")
//!   asg.merge("feature", "main", description="merge feature")

// Binding glue: exported PyO3 methods mirror the Python-side call
// shape which has no natural way to collapse into fewer args (kwargs
// are distinct parameters at the Rust boundary). Allow crate-wide
// rather than wrapping every export.
#![allow(clippy::too_many_arguments)]

use std::sync::Arc;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

// Session + SessionStatus moved to agentstategraph-core in 0.6.5;
// import from the canonical location rather than the facade re-export.
use agentstategraph::speculation::SpecHandle;
use agentstategraph::{CommitOptions, CreateSessionParams, Repository};
use agentstategraph_core::{IntentCategory, Object};
use agentstategraph_core::{Session, SessionStatus};
use agentstategraph_policy::{
    ChangeProposal, Decision, Policy, PolicySignature, PolicyStore as PolicyBackend, Situation,
};
use agentstategraph_storage::SqliteStorage;
use agentstategraph_taint::{
    QuarantineParams, Taint, TaintEffect, TaintKind, TaintMetadata, TaintParams, TaintSeverity,
    UntaintParams, UnwatchParams, WatchDirection, WatchParams,
};
use agentstategraph_tasks::{
    AddTaskOptions, NoopVerifier, OnCompleteHook, Plan, PlanStatus, Priority, Proof, ProofKind,
    Task, TaskId, TaskStatus, TaskStore as TasksBackend, TaskStoreError, Verifier, VerifyEntry,
    VerifyReport, VerifyResult,
};
use chrono::{DateTime, Utc};

/// Convert a Python JSON-compatible value to a AgentStateGraph Object.
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
        let _json_val: serde_json::Value = serde_json::from_str(&json_str)
            .map_err(|e| PyRuntimeError::new_err(format!("JSON parse error: {}", e)))?;
        // Store as string representation for now (TODO: convert complex types)
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

/// AgentStateGraph — AI-native versioned state store.
///
/// Every write is an atomic commit with intent metadata.
/// Supports branching, merging, diffing, and speculative execution.
#[pyclass]
struct AgentStateGraph {
    repo: Arc<Repository>,
}

#[pymethods]
impl AgentStateGraph {
    /// Create a new AgentStateGraph.
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
            None => {
                Repository::new(Box::new(SqliteStorage::in_memory().map_err(|e| {
                    PyRuntimeError::new_err(format!("storage error: {}", e))
                })?))
            }
        };
        repo.init()
            .map_err(|e| PyRuntimeError::new_err(format!("init error: {}", e)))?;
        Ok(Self {
            repo: Arc::new(repo),
        })
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
    #[pyo3(name = "subscribe_watch", signature = (pattern_type="all", pattern=None))]
    fn subscribe_watch(&self, pattern_type: &str, pattern: Option<String>) -> PyResult<u64> {
        let pat = match pattern_type {
            "exact" => agentstategraph::PathPattern::Exact(pattern.unwrap_or_default()),
            "prefix" => agentstategraph::PathPattern::Prefix(pattern.unwrap_or_default()),
            _ => agentstategraph::PathPattern::All,
        };
        let _sub_id = self.repo.watches().subscribe(pat);
        // Return the raw inner value — SubscriptionId is opaque
        Ok(0) // placeholder — need to expose SubscriptionId
    }

    /// Get pending events for a watch subscription.
    fn watch_events(&self, py: Python<'_>, _subscription_id: u64) -> PyResult<PyObject> {
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

/// Check the stored schema version against this binary's `SCHEMA_VERSION`.
///
/// Returns a dict with a `status` key — one of `"up_to_date"`,
/// `"upgrade_available"`, `"downgrade"`, `"unversioned"`, `"corrupt"` —
/// plus context fields. The `migrations` list (when present) names the
/// shipped migrations that would run; apply them via the
/// `agentstategraph-mcp migrate` CLI.
///
/// ```python
/// r = asg.check_schema()
/// if r["status"] == "downgrade":
///     sys.exit(64)   # EX_USAGE — db newer than binary
/// elif r["status"] == "upgrade_available":
///     # migrate before opening listeners
///     ...
/// ```
#[pymethods]
impl AgentStateGraph {
    #[pyo3(signature = (r#ref="main", target=None))]
    fn check_schema(
        &self,
        py: Python<'_>,
        r#ref: &str,
        target: Option<String>,
    ) -> PyResult<PyObject> {
        use agentstategraph_migrate::{CheckResult, Registry, binary_version, check};

        let target = match target {
            Some(s) => semver::Version::parse(&s).map_err(|e| {
                PyRuntimeError::new_err(format!("invalid target version {s:?}: {e}"))
            })?,
            None => binary_version(),
        };
        let registry = Registry::builtin();

        let result = check(&self.repo, r#ref, &target, &registry)
            .map_err(|e| PyRuntimeError::new_err(format!("check failed: {e}")))?;

        let dict = pyo3::types::PyDict::new(py);
        match result {
            CheckResult::UpToDate { version } => {
                dict.set_item("status", "up_to_date")?;
                dict.set_item("version", version.to_string())?;
            }
            CheckResult::UpgradeAvailable {
                from,
                to,
                migrations,
            } => {
                dict.set_item("status", "upgrade_available")?;
                dict.set_item("from", from.to_string())?;
                dict.set_item("to", to.to_string())?;
                dict.set_item("migrations", migrations)?;
            }
            CheckResult::Downgrade { db, binary } => {
                dict.set_item("status", "downgrade")?;
                dict.set_item("db", db.to_string())?;
                dict.set_item("binary", binary.to_string())?;
            }
            CheckResult::Unversioned { implicit } => {
                dict.set_item("status", "unversioned")?;
                dict.set_item("implicit", implicit.to_string())?;
            }
            CheckResult::Corrupt(msg) => {
                dict.set_item("status", "corrupt")?;
                dict.set_item("message", msg)?;
            }
        }
        Ok(dict.into())
    }
}

/// Exit codes an app should use when surfacing `check_schema()` results.
/// Mirrors `agentstategraph-migrate::exit` and `sysexits.h` conventions.
#[pyfunction]
fn exit_codes(py: Python<'_>) -> PyResult<PyObject> {
    use agentstategraph_migrate::exit;
    let d = pyo3::types::PyDict::new(py);
    d.set_item("OK", exit::OK)?;
    d.set_item("DOWNGRADE_REFUSED", exit::DOWNGRADE_REFUSED)?;
    d.set_item("CORRUPT_META", exit::CORRUPT_META)?;
    d.set_item("MIGRATION_FAILED", exit::MIGRATION_FAILED)?;
    d.set_item("UPGRADE_REQUIRED", exit::UPGRADE_REQUIRED)?;
    Ok(d.into())
}

/// Run migrations on a ref. Returns a dict summarizing the Report.
///
/// `mode` must be either `"apply"` (default) or `"dry-run"`.
#[pymethods]
impl AgentStateGraph {
    #[pyo3(signature = (r#ref="main", target=None, mode="apply"))]
    fn migrate(
        &self,
        py: Python<'_>,
        r#ref: &str,
        target: Option<String>,
        mode: &str,
    ) -> PyResult<PyObject> {
        use agentstategraph_migrate::{Registry, RunMode, binary_version};

        let target = match target {
            Some(s) => semver::Version::parse(&s)
                .map_err(|e| PyRuntimeError::new_err(format!("invalid target version: {e}")))?,
            None => binary_version(),
        };
        let run_mode = match mode {
            "apply" => RunMode::Apply,
            "dry-run" | "dry_run" | "dryrun" => RunMode::DryRun,
            other => {
                return Err(PyRuntimeError::new_err(format!(
                    "invalid mode {other:?}; expected 'apply' or 'dry-run'"
                )));
            }
        };
        let registry = Registry::builtin();
        let report = registry
            .run(&self.repo, r#ref, &target, run_mode)
            .map_err(|e| PyRuntimeError::new_err(format!("migrate failed: {e}")))?;

        let dict = pyo3::types::PyDict::new(py);
        dict.set_item("from", report.from.to_string())?;
        dict.set_item("target", report.target.to_string())?;
        dict.set_item("final_version", report.final_version.to_string())?;
        dict.set_item(
            "mode",
            match report.mode {
                RunMode::Apply => "apply",
                RunMode::DryRun => "dry-run",
            },
        )?;
        let steps = pyo3::types::PyList::empty(py);
        for step in &report.steps {
            let sd = pyo3::types::PyDict::new(py);
            sd.set_item("name", &step.name)?;
            sd.set_item("describe", &step.describe)?;
            sd.set_item("from", step.from.to_string())?;
            sd.set_item("to", step.to.to_string())?;
            use agentstategraph_migrate::StepStatus;
            sd.set_item(
                "status",
                match step.status {
                    StepStatus::WouldApply => "would_apply",
                    StepStatus::WouldSkip => "would_skip",
                    StepStatus::Applied => "applied",
                    StepStatus::Skipped => "skipped",
                    StepStatus::Failed => "failed",
                },
            )?;
            sd.set_item("commit_id", step.commit_id.as_ref().map(|c| c.to_string()))?;
            sd.set_item("notes", step.notes.clone())?;
            steps.append(sd)?;
        }
        dict.set_item("steps", steps)?;
        Ok(dict.into())
    }
}

// =========================================================================
// TaskStore — wraps agentstategraph_tasks::TaskStore
// =========================================================================

fn task_err(e: TaskStoreError) -> PyErr {
    PyRuntimeError::new_err(e.to_string())
}

fn parse_priority(s: &str) -> PyResult<Priority> {
    match s.to_lowercase().as_str() {
        "low" => Ok(Priority::Low),
        "medium" => Ok(Priority::Medium),
        "high" => Ok(Priority::High),
        "critical" => Ok(Priority::Critical),
        other => Err(PyRuntimeError::new_err(format!(
            "invalid priority {other:?}; expected low|medium|high|critical"
        ))),
    }
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

fn parse_plan_status(s: &str) -> PyResult<PlanStatus> {
    match s.to_lowercase().as_str() {
        "active" => Ok(PlanStatus::Active),
        "completed" => Ok(PlanStatus::Completed),
        "archived" => Ok(PlanStatus::Archived),
        other => Err(PyRuntimeError::new_err(format!(
            "invalid plan status {other:?}"
        ))),
    }
}

fn parse_proof_kind(s: &str) -> PyResult<ProofKind> {
    match s.to_lowercase().as_str() {
        "commit" => Ok(ProofKind::Commit),
        "file" => Ok(ProofKind::File),
        "test" => Ok(ProofKind::Test),
        "text" => Ok(ProofKind::Text),
        other => Err(PyRuntimeError::new_err(format!(
            "invalid proof kind {other:?}; expected commit|file|test|text"
        ))),
    }
}

fn proof_kind_str(k: ProofKind) -> &'static str {
    match k {
        ProofKind::Commit => "commit",
        ProofKind::File => "file",
        ProofKind::Test => "test",
        ProofKind::Text => "text",
    }
}

fn plan_to_dict(py: Python<'_>, p: &Plan) -> PyResult<PyObject> {
    let d = pyo3::types::PyDict::new(py);
    d.set_item("name", &p.name)?;
    d.set_item("description", p.description.clone())?;
    d.set_item("status", plan_status_str(p.status))?;
    d.set_item("created_at", p.created_at.to_rfc3339())?;
    d.set_item("created_by", &p.created_by)?;
    d.set_item("archived_at", p.archived_at.map(|t| t.to_rfc3339()))?;
    Ok(d.into())
}

fn task_to_dict(py: Python<'_>, t: &Task) -> PyResult<PyObject> {
    let d = pyo3::types::PyDict::new(py);
    d.set_item("id", t.id.as_str())?;
    d.set_item("title", &t.title)?;
    d.set_item("status", status_str(t.status))?;
    d.set_item("priority", priority_str(t.priority))?;
    d.set_item(
        "parent_id",
        t.parent_id.as_ref().map(|i| i.as_str().to_string()),
    )?;
    d.set_item(
        "blocked_by",
        t.blocked_by
            .iter()
            .map(|i| i.as_str().to_string())
            .collect::<Vec<_>>(),
    )?;
    d.set_item("created_at", t.created_at.to_rfc3339())?;
    d.set_item("created_by", &t.created_by)?;
    d.set_item("started_at", t.started_at.map(|x| x.to_rfc3339()))?;
    d.set_item("started_by", t.started_by.clone())?;
    d.set_item("completed_at", t.completed_at.map(|x| x.to_rfc3339()))?;
    d.set_item("completed_by", t.completed_by.clone())?;
    if let Some(proof) = &t.proof {
        let pd = pyo3::types::PyDict::new(py);
        pd.set_item("kind", proof_kind_str(proof.kind))?;
        pd.set_item("value", &proof.value)?;
        pd.set_item("note", proof.note.clone())?;
        d.set_item("proof", pd)?;
    } else {
        d.set_item("proof", py.None())?;
    }
    d.set_item("abandoned_at", t.abandoned_at.map(|x| x.to_rfc3339()))?;
    d.set_item("abandoned_reason", t.abandoned_reason.clone())?;
    d.set_item("assigned_to", t.assigned_to.clone())?;
    // Policy-fallback extension fields (POLICY_V1.md §22.4).
    if let Some(payload) = &t.payload {
        d.set_item("payload", json_to_py(py, payload)?)?;
    } else {
        d.set_item("payload", py.None())?;
    }
    d.set_item("parent_change", t.parent_change.clone())?;
    if let Some(hook) = &t.on_complete {
        let v = serde_json::to_value(hook)
            .map_err(|e| PyRuntimeError::new_err(format!("on_complete serialize: {e}")))?;
        d.set_item("on_complete", json_to_py(py, &v)?)?;
    } else {
        d.set_item("on_complete", py.None())?;
    }
    Ok(d.into())
}

fn report_to_dict(py: Python<'_>, r: &VerifyReport) -> PyResult<PyObject> {
    let d = pyo3::types::PyDict::new(py);
    d.set_item("plan", &r.plan)?;
    let entries = pyo3::types::PyList::empty(py);
    for e in &r.results {
        let ed = pyo3::types::PyDict::new(py);
        ed.set_item("task_id", e.task_id.as_str())?;
        let (status, msg) = match &e.result {
            VerifyResult::Verified { message } => ("verified", message.clone()),
            VerifyResult::Decayed { reason } => ("decayed", reason.clone()),
            VerifyResult::Unverifiable { reason } => ("unverifiable", reason.clone()),
        };
        ed.set_item("status", status)?;
        ed.set_item("message", msg)?;
        entries.append(ed)?;
    }
    d.set_item("results", entries)?;
    d.set_item("verified_count", r.verified_count())?;
    d.set_item("decayed_count", r.decayed_count())?;
    d.set_item("unverifiable_count", r.unverifiable_count())?;
    d.set_item("all_strongly_verified", r.all_strongly_verified())?;
    d.set_item("summary", r.summary())?;
    Ok(d.into())
}

/// A canned verifier that returns `Verified` for certain proof kinds
/// (per a user-supplied `ProofKind -> bool` map) and `Unverifiable`
/// otherwise. Exposed via `TaskStore.verify_plan_with_kinds`.
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

/// TaskStore — plans and tasks layered on an AgentStateGraph.
///
/// Wraps `agentstategraph_tasks::TaskStore`. Construct from an
/// `AgentStateGraph` and a path prefix (e.g. `/plans`).
#[pyclass]
struct TaskStore {
    inner: TasksBackend,
}

#[pymethods]
impl TaskStore {
    #[new]
    #[pyo3(signature = (asg, prefix="/plans", agent_id="python"))]
    fn new(asg: &AgentStateGraph, prefix: &str, agent_id: &str) -> Self {
        Self {
            inner: TasksBackend::new(Arc::clone(&asg.repo), prefix, agent_id),
        }
    }

    // --- Plan ops ---

    fn create_plan(
        &self,
        py: Python<'_>,
        ref_name: &str,
        name: &str,
        description: Option<String>,
    ) -> PyResult<PyObject> {
        let plan = self
            .inner
            .create_plan(ref_name, name, description)
            .map_err(task_err)?;
        plan_to_dict(py, &plan)
    }

    fn list_plans(&self, py: Python<'_>, ref_name: &str) -> PyResult<PyObject> {
        let plans = self.inner.list_plans(ref_name).map_err(task_err)?;
        let list = pyo3::types::PyList::empty(py);
        for p in &plans {
            list.append(plan_to_dict(py, p)?)?;
        }
        Ok(list.into())
    }

    fn list_plans_by_status(
        &self,
        py: Python<'_>,
        ref_name: &str,
        status: &str,
    ) -> PyResult<PyObject> {
        let s = parse_plan_status(status)?;
        let plans = self
            .inner
            .list_plans_by_status(ref_name, Some(s))
            .map_err(task_err)?;
        let list = pyo3::types::PyList::empty(py);
        for p in &plans {
            list.append(plan_to_dict(py, p)?)?;
        }
        Ok(list.into())
    }

    fn get_plan(&self, py: Python<'_>, ref_name: &str, name: &str) -> PyResult<PyObject> {
        let p = self.inner.get_plan(ref_name, name).map_err(task_err)?;
        plan_to_dict(py, &p)
    }

    fn archive_plan(&self, py: Python<'_>, ref_name: &str, name: &str) -> PyResult<PyObject> {
        let p = self.inner.archive_plan(ref_name, name).map_err(task_err)?;
        plan_to_dict(py, &p)
    }

    fn delete_plan(&self, ref_name: &str, name: &str) -> PyResult<()> {
        self.inner.delete_plan(ref_name, name).map_err(task_err)
    }

    // --- Task ops ---

    #[pyo3(signature = (ref_name, plan, title, priority="medium", parent_id=None, blocked_by=None, assigned_to=None, payload=None, parent_change=None, on_complete=None))]
    #[allow(clippy::too_many_arguments)]
    fn add_task(
        &self,
        py: Python<'_>,
        ref_name: &str,
        plan: &str,
        title: &str,
        priority: &str,
        parent_id: Option<String>,
        blocked_by: Option<Vec<String>>,
        assigned_to: Option<String>,
        payload: Option<&Bound<'_, PyAny>>,
        parent_change: Option<String>,
        on_complete: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<PyObject> {
        let pri = parse_priority(priority)?;
        let parent = parent_id.map(TaskId);
        let blockers: Vec<TaskId> = blocked_by
            .unwrap_or_default()
            .into_iter()
            .map(TaskId)
            .collect();
        let payload_val = match payload {
            None => None,
            Some(v) if v.is_none() => None,
            Some(v) => Some(py_any_to_json(py, v)?),
        };
        let on_complete_val = match on_complete {
            None => None,
            Some(v) if v.is_none() => None,
            Some(v) => {
                let json = py_any_to_json(py, v)?;
                let hook: OnCompleteHook = serde_json::from_value(json)
                    .map_err(|e| PyRuntimeError::new_err(format!("invalid on_complete: {e}")))?;
                Some(hook)
            }
        };
        let task = self
            .inner
            .add_task_with_extensions(
                ref_name,
                plan,
                title,
                pri,
                parent,
                blockers,
                assigned_to,
                AddTaskOptions { payload: payload_val, parent_change, on_complete: on_complete_val },
            )
            .map_err(task_err)?;
        task_to_dict(py, &task)
    }

    fn list_tasks(&self, py: Python<'_>, ref_name: &str, plan: &str) -> PyResult<PyObject> {
        let tasks = self.inner.list_tasks(ref_name, plan).map_err(task_err)?;
        let list = pyo3::types::PyList::empty(py);
        for t in &tasks {
            list.append(task_to_dict(py, t)?)?;
        }
        Ok(list.into())
    }

    fn task_ids(&self, ref_name: &str, plan: &str) -> PyResult<Vec<String>> {
        let ids = self.inner.task_ids(ref_name, plan).map_err(task_err)?;
        Ok(ids.into_iter().map(|i| i.0).collect())
    }

    fn get_task(&self, py: Python<'_>, ref_name: &str, plan: &str, id: &str) -> PyResult<PyObject> {
        let t = self
            .inner
            .get_task(ref_name, plan, &TaskId(id.to_string()))
            .map_err(task_err)?;
        task_to_dict(py, &t)
    }

    fn start_task(
        &self,
        py: Python<'_>,
        ref_name: &str,
        plan: &str,
        id: &str,
    ) -> PyResult<PyObject> {
        let t = self
            .inner
            .start_task(ref_name, plan, &TaskId(id.to_string()))
            .map_err(task_err)?;
        task_to_dict(py, &t)
    }

    #[pyo3(signature = (ref_name, plan, id, proof_kind, proof_value, proof_note=None))]
    fn complete_task(
        &self,
        py: Python<'_>,
        ref_name: &str,
        plan: &str,
        id: &str,
        proof_kind: &str,
        proof_value: &str,
        proof_note: Option<String>,
    ) -> PyResult<PyObject> {
        let kind = parse_proof_kind(proof_kind)?;
        let mut proof = Proof {
            kind,
            value: proof_value.to_string(),
            note: None,
        };
        if let Some(n) = proof_note {
            proof = proof.with_note(n);
        }
        let t = self
            .inner
            .complete_task(ref_name, plan, &TaskId(id.to_string()), proof)
            .map_err(task_err)?;
        task_to_dict(py, &t)
    }

    fn abandon_task(
        &self,
        py: Python<'_>,
        ref_name: &str,
        plan: &str,
        id: &str,
        reason: &str,
    ) -> PyResult<PyObject> {
        let t = self
            .inner
            .abandon_task(ref_name, plan, &TaskId(id.to_string()), reason)
            .map_err(task_err)?;
        task_to_dict(py, &t)
    }

    fn set_priority(
        &self,
        py: Python<'_>,
        ref_name: &str,
        plan: &str,
        id: &str,
        priority: &str,
    ) -> PyResult<PyObject> {
        let pri = parse_priority(priority)?;
        let t = self
            .inner
            .set_priority(ref_name, plan, &TaskId(id.to_string()), pri)
            .map_err(task_err)?;
        task_to_dict(py, &t)
    }

    fn set_blockers(
        &self,
        py: Python<'_>,
        ref_name: &str,
        plan: &str,
        id: &str,
        blockers: Vec<String>,
    ) -> PyResult<PyObject> {
        let b: Vec<TaskId> = blockers.into_iter().map(TaskId).collect();
        let t = self
            .inner
            .set_blockers(ref_name, plan, &TaskId(id.to_string()), b)
            .map_err(task_err)?;
        task_to_dict(py, &t)
    }

    fn assign_task(
        &self,
        py: Python<'_>,
        ref_name: &str,
        plan: &str,
        id: &str,
        agent: &str,
    ) -> PyResult<PyObject> {
        let t = self
            .inner
            .assign_task(ref_name, plan, &TaskId(id.to_string()), agent)
            .map_err(task_err)?;
        task_to_dict(py, &t)
    }

    fn unassign_task(
        &self,
        py: Python<'_>,
        ref_name: &str,
        plan: &str,
        id: &str,
    ) -> PyResult<PyObject> {
        let t = self
            .inner
            .unassign_task(ref_name, plan, &TaskId(id.to_string()))
            .map_err(task_err)?;
        task_to_dict(py, &t)
    }

    fn next_task(&self, py: Python<'_>, ref_name: &str, plan: &str) -> PyResult<PyObject> {
        match self.inner.next_task(ref_name, plan).map_err(task_err)? {
            Some(t) => task_to_dict(py, &t),
            None => Ok(py.None()),
        }
    }

    #[pyo3(signature = (ref_name, plan, assigned_to=None, include_unassigned=true))]
    fn next_task_for(
        &self,
        py: Python<'_>,
        ref_name: &str,
        plan: &str,
        assigned_to: Option<String>,
        include_unassigned: bool,
    ) -> PyResult<PyObject> {
        match self
            .inner
            .next_task_for(ref_name, plan, assigned_to.as_deref(), include_unassigned)
            .map_err(task_err)?
        {
            Some(t) => task_to_dict(py, &t),
            None => Ok(py.None()),
        }
    }

    fn derived_status(&self, ref_name: &str, plan: &str, parent_id: &str) -> PyResult<String> {
        let s = self
            .inner
            .derived_status(ref_name, plan, &TaskId(parent_id.to_string()))
            .map_err(task_err)?;
        Ok(status_str(s).to_string())
    }

    /// Run a canned verifier: every proof whose kind is keyed `True`
    /// in `verify_by_kind` is reported as `Verified`; others are
    /// reported as `Unverifiable`.
    fn verify_plan_with_kinds(
        &self,
        py: Python<'_>,
        ref_name: &str,
        plan: &str,
        verify_by_kind: std::collections::HashMap<String, bool>,
    ) -> PyResult<PyObject> {
        let v = KindMapVerifier {
            commit: *verify_by_kind.get("commit").unwrap_or(&false),
            file: *verify_by_kind.get("file").unwrap_or(&false),
            test: *verify_by_kind.get("test").unwrap_or(&false),
            text: *verify_by_kind.get("text").unwrap_or(&false),
        };
        let report = self
            .inner
            .verify_plan(ref_name, plan, &v)
            .map_err(task_err)?;
        report_to_dict(py, &report)
    }

    /// Run the noop verifier — every `done` task yields `Unverifiable`.
    fn verify_plan_noop(&self, py: Python<'_>, ref_name: &str, plan: &str) -> PyResult<PyObject> {
        let report = self
            .inner
            .verify_plan(ref_name, plan, &NoopVerifier)
            .map_err(task_err)?;
        report_to_dict(py, &report)
    }
}

// Silence unused-import warnings for types only referenced via traits.
#[allow(dead_code)]
fn _touch_unused(_: &VerifyEntry) {}

// =========================================================================
// JSON bridge helpers (shared by PolicyStore + Session + Task extensions)
// =========================================================================

/// Convert any Python value to `serde_json::Value` via `json.dumps`.
fn py_any_to_json(py: Python<'_>, value: &Bound<'_, PyAny>) -> PyResult<serde_json::Value> {
    let json_mod = py.import("json")?;
    let s: String = json_mod.call_method1("dumps", (value,))?.extract()?;
    serde_json::from_str(&s).map_err(|e| PyRuntimeError::new_err(format!("JSON parse: {e}")))
}

fn policy_err(e: agentstategraph_policy::PolicyError) -> PyErr {
    PyRuntimeError::new_err(e.to_string())
}

fn session_err(e: agentstategraph::SessionError) -> PyErr {
    PyRuntimeError::new_err(e.to_string())
}

// =========================================================================
// PolicyStore — wraps agentstategraph_policy::PolicyStore
// =========================================================================

/// PolicyStore — situation-matching authorization + change-cost policies.
///
/// Wraps `agentstategraph_policy::PolicyStore`. Construct from an
/// `AgentStateGraph`, a path prefix (e.g. `/policies`), and an agent_id.
///
/// All complex values (Policy, Situation, ChangeProposal, Decision) are
/// passed as plain Python dicts; the wrapper serializes them through
/// serde_json round-trips to keep the Python surface small.
#[pyclass]
struct PolicyStore {
    inner: PolicyBackend,
}

#[pymethods]
impl PolicyStore {
    #[new]
    #[pyo3(signature = (asg, prefix="/policies", agent_id="python"))]
    fn new(asg: &AgentStateGraph, prefix: &str, agent_id: &str) -> Self {
        Self {
            inner: PolicyBackend::new(Arc::clone(&asg.repo), prefix, agent_id),
        }
    }

    // --- Write ops ---

    /// Write a proposed (unratified) policy. Returns the "path@version"
    /// handle on success. Fails if a policy already exists at that path.
    fn propose(
        &self,
        py: Python<'_>,
        ref_name: &str,
        policy: &Bound<'_, PyAny>,
    ) -> PyResult<String> {
        let json = py_any_to_json(py, policy)?;
        let p: Policy = serde_json::from_value(json)
            .map_err(|e| PyRuntimeError::new_err(format!("invalid policy dict: {e}")))?;
        self.inner.propose(ref_name, p).map_err(policy_err)
    }

    /// Ratify an unratified proposal at `path`.
    fn ratify(&self, ref_name: &str, path: &str, ratifier: &str, reasoning: &str) -> PyResult<()> {
        self.inner
            .ratify(ref_name, path, ratifier, reasoning)
            .map_err(policy_err)
    }

    /// Replace the active policy at `path` with `new_policy`. Returns
    /// the new "path@version" handle.
    fn supersede(
        &self,
        py: Python<'_>,
        ref_name: &str,
        path: &str,
        new_policy: &Bound<'_, PyAny>,
    ) -> PyResult<String> {
        let json = py_any_to_json(py, new_policy)?;
        let p: Policy = serde_json::from_value(json)
            .map_err(|e| PyRuntimeError::new_err(format!("invalid policy dict: {e}")))?;
        self.inner.supersede(ref_name, path, p).map_err(policy_err)
    }

    // --- Read ops ---

    /// List every policy (active versions, ratified or not) whose path
    /// starts with `prefix_filter`. `None` lists everything.
    ///
    /// `tenant_filter` (0.7.5 §3b): `None` keeps back-compat (all
    /// policies pass); `Some(tid)` keeps only policies with
    /// `tenant_id == Some(tid)` or `tenant_id == None`.
    #[pyo3(signature = (ref_name, prefix_filter=None, tenant_filter=None))]
    fn list(
        &self,
        py: Python<'_>,
        ref_name: &str,
        prefix_filter: Option<&str>,
        tenant_filter: Option<&str>,
    ) -> PyResult<PyObject> {
        let policies = self
            .inner
            .list_scoped(ref_name, prefix_filter, tenant_filter)
            .map_err(policy_err)?;
        policies_to_pylist(py, &policies)
    }

    /// List currently-active policies (ratified AND `active_from <= now`).
    ///
    /// `tenant_filter` (0.7.5 §3b) matches [`Self::list`] semantics.
    #[pyo3(signature = (ref_name, prefix_filter=None, tenant_filter=None))]
    fn active(
        &self,
        py: Python<'_>,
        ref_name: &str,
        prefix_filter: Option<&str>,
        tenant_filter: Option<&str>,
    ) -> PyResult<PyObject> {
        let policies = self
            .inner
            .active_scoped(ref_name, prefix_filter, tenant_filter)
            .map_err(policy_err)?;
        policies_to_pylist(py, &policies)
    }

    /// Fetch a policy at `path`. If `version` is provided, returns the
    /// pinned historical version; otherwise the current active version.
    #[pyo3(signature = (ref_name, path, version=None))]
    fn get(
        &self,
        py: Python<'_>,
        ref_name: &str,
        path: &str,
        version: Option<u64>,
    ) -> PyResult<PyObject> {
        let p = self
            .inner
            .get(ref_name, path, version)
            .map_err(policy_err)?;
        policy_to_py(py, &p)
    }

    /// Walk the supersedes chain (oldest first → current).
    fn history(&self, py: Python<'_>, ref_name: &str, path: &str) -> PyResult<PyObject> {
        let policies = self.inner.history(ref_name, path).map_err(policy_err)?;
        policies_to_pylist(py, &policies)
    }

    /// Authorization evaluation (POLICY_V1.md §5). Returns a Decision
    /// dict with a "kind" field.
    ///
    /// `tenant_filter` (0.7.5 §3b) routes through the Rust
    /// `evaluate_scoped` variant: `None` considers every policy,
    /// `Some(tid)` restricts the candidate set to policies whose
    /// `tenant_id == Some(tid)` or `tenant_id == None`.
    #[pyo3(signature = (ref_name, situation, action, agent_id, tenant_filter=None))]
    fn evaluate(
        &self,
        py: Python<'_>,
        ref_name: &str,
        situation: &Bound<'_, PyAny>,
        action: &str,
        agent_id: &str,
        tenant_filter: Option<&str>,
    ) -> PyResult<PyObject> {
        let sit = situation_from_py(py, situation)?;
        let decision = self
            .inner
            .evaluate_scoped(ref_name, &sit, action, agent_id, tenant_filter)
            .map_err(policy_err)?;
        decision_to_py(py, &decision)
    }

    /// Change-proposal evaluation (POLICY_V1.md §22.2). Returns a
    /// Decision dict.
    ///
    /// `tenant_filter` (0.7.5 §3b) matches [`Self::evaluate`] semantics.
    #[pyo3(signature = (ref_name, proposal, tenant_filter=None))]
    fn evaluate_change(
        &self,
        py: Python<'_>,
        ref_name: &str,
        proposal: &Bound<'_, PyAny>,
        tenant_filter: Option<&str>,
    ) -> PyResult<PyObject> {
        let json = py_any_to_json(py, proposal)?;
        let prop: ChangeProposal = serde_json::from_value(json)
            .map_err(|e| PyRuntimeError::new_err(format!("invalid proposal: {e}")))?;
        let decision = self
            .inner
            .evaluate_change_scoped(ref_name, &prop, tenant_filter)
            .map_err(policy_err)?;
        decision_to_py(py, &decision)
    }

    // ---- 0.7.5 §5a: sign / verify / set_external_evaluator ----
    //
    // `sign()` wires through to the real `PolicyStore::set_signature`
    // Rust API. Because the Python binding's Cargo.toml does NOT
    // currently depend on `agentstategraph-policy-sign` (+ ed25519-dalek
    // / hex) — see "plumbing gaps" below — we can't *produce* an Ed25519
    // signature locally from a private key. Instead, `sign()` accepts a
    // pre-computed `signature_hex` and writes it to the policy via
    // `set_signature`, which is the real Rust write path (it commits
    // under IntentCategory::Custom("policy-sign")). Callers who want
    // full end-to-end Python signing can produce the 64-byte Ed25519
    // signature over the canonical-JSON bytes via their preferred Python
    // crypto library and pass the hex in here.
    //
    // `verify()` returns a structured {"valid": false, "reason": ...}
    // error because PolicyStore.new() in the binding does not install a
    // `SignatureVerifier` — the Rust API takes the verifier at
    // construction time via `with_verifier`. Wiring a verifier through
    // PyO3 requires the crypto deps above plus a public-key-registry
    // wrapper class.
    //
    // `set_external_evaluator` remains a stub envelope — the Rust-side
    // `PolicyStore` does not expose a runtime mutator for the
    // per-policy `external_evaluator` field after propose/supersede.

    /// Write a signature onto the active policy at `path`.
    ///
    /// Real wiring of `PolicyStore::set_signature`. The signature bytes
    /// must be pre-computed Ed25519 over the canonical-JSON form of the
    /// policy (with the `signature` field omitted); see POLICY_V1.md
    /// §5a. Returns `{"ok": true, "handle": "<path>@<version>"}` on
    /// success, or `{"error": "..."}` on failure.
    ///
    /// Arguments:
    /// - `signer_key_id` — opaque key id the verifier will look up.
    /// - `signature_hex` — 128-char lowercase hex of 64-byte signature.
    ///
    /// The `private_key_hex` kwarg is accepted but currently rejected
    /// with a structured error: local signing requires the
    /// `agentstategraph-policy-sign` crate which is not yet a binding
    /// dependency. Supply `signature_hex` instead.
    #[pyo3(signature = (ref_name, path, signer_key_id, signature_hex=None, private_key_hex=None))]
    fn sign(
        &self,
        py: Python<'_>,
        ref_name: &str,
        path: &str,
        signer_key_id: &str,
        signature_hex: Option<&str>,
        private_key_hex: Option<&str>,
    ) -> PyResult<PyObject> {
        if private_key_hex.is_some() && signature_hex.is_none() {
            let env = serde_json::json!({
                "error": "local signing not available",
                "reason": "binding lacks agentstategraph-policy-sign dep",
                "hint": "compute the 64-byte Ed25519 signature over canonical JSON and pass signature_hex",
            });
            return json_to_py(py, &env);
        }
        let Some(hex) = signature_hex else {
            let env = serde_json::json!({
                "error": "signature_hex required",
                "hint": "pass the 128-char hex-encoded 64-byte Ed25519 signature",
            });
            return json_to_py(py, &env);
        };
        let sig = PolicySignature::Ed25519 {
            signer_key_id: signer_key_id.to_string(),
            signature_hex: hex.to_string(),
        };
        match self.inner.set_signature(ref_name, path, sig) {
            Ok(()) => {
                // Re-fetch to return the resulting handle.
                let p = self.inner.get(ref_name, path, None).map_err(policy_err)?;
                let env = serde_json::json!({
                    "ok": true,
                    "handle": p.handle(),
                    "signer_key_id": signer_key_id,
                });
                json_to_py(py, &env)
            }
            Err(e) => {
                let env = serde_json::json!({
                    "error": e.to_string(),
                });
                json_to_py(py, &env)
            }
        }
    }

    /// Verify the signature on the active policy at `path`.
    ///
    /// Returns `{"valid": true}` on a successful check, or
    /// `{"valid": false, "reason": "..."}` otherwise.
    ///
    /// Currently returns a structured "no verifier registered" error:
    /// `PolicyStore::new` in the binding does not install a
    /// `SignatureVerifier` (the Rust API takes one via `with_verifier`
    /// at construction time). Wiring a PyO3 verifier requires the
    /// `agentstategraph-policy-sign` crate + ed25519-dalek + hex as
    /// binding dependencies and a public-key-registry wrapper class.
    fn verify(&self, py: Python<'_>, ref_name: &str, path: &str) -> PyResult<PyObject> {
        // Confirm the policy exists so we return a real NotFound error
        // when the path is wrong (not a misleading "no verifier"
        // envelope).
        let policy = match self.inner.get(ref_name, path, None) {
            Ok(p) => p,
            Err(e) => {
                let env = serde_json::json!({ "valid": false, "reason": e.to_string() });
                return json_to_py(py, &env);
            }
        };
        if policy.signature.is_none() {
            let env = serde_json::json!({
                "valid": false,
                "reason": "policy has no signature",
            });
            return json_to_py(py, &env);
        }
        let env = serde_json::json!({
            "valid": false,
            "reason": "no verifier registered",
            "hint": "PolicyStore.new does not accept a verifier; needs agentstategraph-policy-sign wired into bindings/python/Cargo.toml + PyO3 key-registry wrapper",
        });
        json_to_py(py, &env)
    }

    /// Attach or update the external evaluator reference on the
    /// policy at `path` (stub). Returns the same envelope as `sign` /
    /// `verify`. Until the runtime-side mutator lands, callers can set
    /// `external_evaluator` on the policy dict at propose/supersede
    /// time — the field is preserved by serde round-trip.
    #[pyo3(signature = (ref_name, path, config=None))]
    #[allow(unused_variables)]
    fn set_external_evaluator(
        &self,
        py: Python<'_>,
        ref_name: &str,
        path: &str,
        config: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<PyObject> {
        let envelope = serde_json::json!({
            "error": "not yet wired",
            "hint": "set policy['external_evaluator'] before propose/supersede",
        });
        json_to_py(py, &envelope)
    }

    /// List ratified policies whose `triggers` intersect the given token
    /// set. Convenience helper exposing the same filter
    /// `evaluate_change` uses internally.
    fn check_tokens(
        &self,
        py: Python<'_>,
        ref_name: &str,
        tokens: Vec<String>,
    ) -> PyResult<PyObject> {
        let actives = self.inner.active(ref_name, None).map_err(policy_err)?;
        let token_set: std::collections::HashSet<&str> =
            tokens.iter().map(|s| s.as_str()).collect();
        let matched: Vec<Policy> = actives
            .into_iter()
            .filter(|p| p.triggers.iter().any(|t| token_set.contains(t.as_str())))
            .collect();
        policies_to_pylist(py, &matched)
    }
}

fn policy_to_py(py: Python<'_>, p: &Policy) -> PyResult<PyObject> {
    let v = serde_json::to_value(p)
        .map_err(|e| PyRuntimeError::new_err(format!("policy serialize: {e}")))?;
    json_to_py(py, &v)
}

fn policies_to_pylist(py: Python<'_>, policies: &[Policy]) -> PyResult<PyObject> {
    let list = pyo3::types::PyList::empty(py);
    for p in policies {
        list.append(policy_to_py(py, p)?)?;
    }
    Ok(list.into())
}

fn decision_to_py(py: Python<'_>, d: &Decision) -> PyResult<PyObject> {
    let v = serde_json::to_value(d)
        .map_err(|e| PyRuntimeError::new_err(format!("decision serialize: {e}")))?;
    json_to_py(py, &v)
}

fn situation_from_py(py: Python<'_>, situation: &Bound<'_, PyAny>) -> PyResult<Situation> {
    // Accept either a dict[str,str] (the transparent serde form) or a
    // {"facts": {...}} wrapper for future-proofing. Fall back to a full
    // JSON round-trip for anything else.
    if let Ok(map) = situation.extract::<std::collections::HashMap<String, String>>() {
        return Ok(Situation::from(map));
    }
    let json = py_any_to_json(py, situation)?;
    serde_json::from_value(json)
        .map_err(|e| PyRuntimeError::new_err(format!("invalid situation: {e}")))
}

// =========================================================================
// Session — wraps agentstategraph_core::{Session, SessionStatus}
// =========================================================================

fn session_status_str(s: &SessionStatus) -> &'static str {
    match s {
        SessionStatus::Active => "active",
        SessionStatus::Completed => "completed",
        SessionStatus::Abandoned => "abandoned",
    }
}

fn parse_session_status(s: &str) -> PyResult<SessionStatus> {
    match s.to_lowercase().as_str() {
        "active" => Ok(SessionStatus::Active),
        "completed" => Ok(SessionStatus::Completed),
        "abandoned" => Ok(SessionStatus::Abandoned),
        other => Err(PyRuntimeError::new_err(format!(
            "invalid session status {other:?}; expected active|completed|abandoned"
        ))),
    }
}

fn session_to_dict(py: Python<'_>, s: &Session) -> PyResult<PyObject> {
    let d = pyo3::types::PyDict::new(py);
    d.set_item("id", &s.id)?;
    d.set_item("agent_id", &s.agent_id)?;
    d.set_item("working_branch", &s.working_branch)?;
    d.set_item("head", s.head.to_string())?;
    d.set_item("parent_session", s.parent_session.clone())?;
    d.set_item("delegated_intent", s.delegated_intent.clone())?;
    d.set_item("report_to", s.report_to.clone())?;
    d.set_item("path_scope", s.path_scope.clone())?;
    // 0.7.5 §3a — tenant scope on the session record. Always surfaced
    // (as None when unset) so Python callers can rely on the key.
    d.set_item("scope_tenant", s.scope_tenant.clone())?;
    d.set_item("status", session_status_str(&s.status))?;
    d.set_item("created_at", s.created_at.to_rfc3339())?;
    d.set_item("ended_at", s.ended_at.map(|t| t.to_rfc3339()))?;
    Ok(d.into())
}

/// Session operations — sub-agent orchestration. Exposed via
/// `AgentStateGraph.sessions()`-style methods (but attached directly to
/// AgentStateGraph for API simplicity).
#[pymethods]
impl AgentStateGraph {
    /// Create a durable session record. `head` defaults to the tip of
    /// `working_branch` if omitted.
    #[pyo3(signature = (agent_id, working_branch="main", parent_session=None, delegated_intent=None, report_to=None, path_scope=None))]
    #[allow(clippy::too_many_arguments)]
    fn create_session(
        &self,
        py: Python<'_>,
        agent_id: &str,
        working_branch: &str,
        parent_session: Option<String>,
        delegated_intent: Option<String>,
        report_to: Option<String>,
        path_scope: Option<String>,
    ) -> PyResult<PyObject> {
        // Resolve head via a 1-commit log read on the working branch.
        let head = {
            let log = self
                .repo
                .log(working_branch, 1)
                .map_err(|e| PyRuntimeError::new_err(format!("{e}")))?;
            log.into_iter()
                .next()
                .map(|c| c.id)
                .ok_or_else(|| PyRuntimeError::new_err(format!("ref {working_branch:?} empty")))?
        };
        let mgr = self.repo.sessions();
        let s = mgr
            .create(
                agent_id,
                working_branch,
                head,
                CreateSessionParams { parent_session, delegated_intent, report_to, path_scope },
            )
            .map_err(session_err)?;
        session_to_dict(py, &s)
    }

    /// Get a session by id.
    fn get_session(&self, py: Python<'_>, id: &str) -> PyResult<PyObject> {
        let mgr = self.repo.sessions();
        match mgr.get(id).map_err(session_err)? {
            Some(s) => session_to_dict(py, &s),
            None => Ok(py.None()),
        }
    }

    /// List sessions, optionally filtered by agent_id.
    #[pyo3(signature = (agent_filter=None))]
    fn list_sessions(&self, py: Python<'_>, agent_filter: Option<&str>) -> PyResult<PyObject> {
        let mgr = self.repo.sessions();
        let sessions = mgr.list(agent_filter).map_err(session_err)?;
        let list = pyo3::types::PyList::empty(py);
        for s in &sessions {
            list.append(session_to_dict(py, s)?)?;
        }
        Ok(list.into())
    }

    /// End a session with a given status ("active" | "completed" |
    /// "abandoned"). `active` is legal but rarely useful — typical use
    /// is "completed" or "abandoned".
    fn end_session(&self, id: &str, status: &str) -> PyResult<()> {
        let st = parse_session_status(status)?;
        let mgr = self.repo.sessions();
        mgr.end(id, st).map_err(session_err)
    }
}

// =========================================================================
// Taint / Quarantine / Watch — wraps agentstategraph_taint::*
// =========================================================================

fn parse_taint_effect(s: &str) -> PyResult<TaintEffect> {
    match s.to_lowercase().as_str() {
        "warn" => Ok(TaintEffect::Warn),
        "block" => Ok(TaintEffect::Block),
        "review" => Ok(TaintEffect::Review),
        "isolate" => Ok(TaintEffect::Isolate),
        "advisory" => Ok(TaintEffect::Advisory),
        other => Err(PyRuntimeError::new_err(format!(
            "invalid taint effect {other:?}; expected warn|block|review|isolate|advisory"
        ))),
    }
}

fn parse_taint_severity(s: &str) -> PyResult<TaintSeverity> {
    match s.to_lowercase().as_str() {
        "low" => Ok(TaintSeverity::Low),
        "medium" => Ok(TaintSeverity::Medium),
        "high" => Ok(TaintSeverity::High),
        "critical" => Ok(TaintSeverity::Critical),
        other => Err(PyRuntimeError::new_err(format!(
            "invalid severity {other:?}; expected low|medium|high|critical"
        ))),
    }
}

fn parse_taint_kind(s: &str) -> PyResult<TaintKind> {
    match s.to_lowercase().as_str() {
        "taint" => Ok(TaintKind::Taint),
        "quarantine" => Ok(TaintKind::Quarantine),
        "watch" => Ok(TaintKind::Watch),
        other => Err(PyRuntimeError::new_err(format!(
            "invalid taint kind {other:?}; expected taint|quarantine|watch"
        ))),
    }
}

fn parse_watch_direction(s: &str) -> PyResult<WatchDirection> {
    match s.to_lowercase().as_str() {
        "above" => Ok(WatchDirection::Above),
        "below" => Ok(WatchDirection::Below),
        other => Err(PyRuntimeError::new_err(format!(
            "invalid watch direction {other:?}; expected above|below"
        ))),
    }
}

fn parse_expires(s: Option<&str>) -> PyResult<Option<DateTime<Utc>>> {
    match s {
        None => Ok(None),
        Some(v) => DateTime::parse_from_rfc3339(v)
            .map(|dt| Some(dt.with_timezone(&Utc)))
            .map_err(|e| PyRuntimeError::new_err(format!("invalid expires (rfc3339): {e}"))),
    }
}

fn taint_to_py(py: Python<'_>, t: &Taint) -> PyResult<PyObject> {
    let v = serde_json::to_value(t)
        .map_err(|e| PyRuntimeError::new_err(format!("taint serialize: {e}")))?;
    json_to_py(py, &v)
}

fn dict_get_str<'py>(d: &Bound<'py, pyo3::types::PyDict>, key: &str) -> PyResult<Option<String>> {
    match d.get_item(key)? {
        Some(v) if !v.is_none() => Ok(Some(v.extract::<String>()?)),
        _ => Ok(None),
    }
}

fn dict_get_bool(d: &Bound<'_, pyo3::types::PyDict>, key: &str) -> PyResult<Option<bool>> {
    match d.get_item(key)? {
        Some(v) if !v.is_none() => Ok(Some(v.extract::<bool>()?)),
        _ => Ok(None),
    }
}

fn dict_get_f64(d: &Bound<'_, pyo3::types::PyDict>, key: &str) -> PyResult<Option<f64>> {
    match d.get_item(key)? {
        Some(v) if !v.is_none() => Ok(Some(v.extract::<f64>()?)),
        _ => Ok(None),
    }
}

fn dict_get_u64(d: &Bound<'_, pyo3::types::PyDict>, key: &str) -> PyResult<Option<u64>> {
    match d.get_item(key)? {
        Some(v) if !v.is_none() => Ok(Some(v.extract::<u64>()?)),
        _ => Ok(None),
    }
}

fn dict_get_vec_str(
    d: &Bound<'_, pyo3::types::PyDict>,
    key: &str,
) -> PyResult<Option<Vec<String>>> {
    match d.get_item(key)? {
        Some(v) if !v.is_none() => Ok(Some(v.extract::<Vec<String>>()?)),
        _ => Ok(None),
    }
}

#[pymethods]
impl AgentStateGraph {
    /// Apply a taint to `path`. `params` dict keys: name, effect, reason,
    /// severity (default "medium"), expires (RFC3339 | None),
    /// propagate (default True), agent_id.
    fn taint(
        &self,
        ref_name: &str,
        path: &str,
        params: &Bound<'_, pyo3::types::PyDict>,
    ) -> PyResult<String> {
        let name = dict_get_str(params, "name")?
            .ok_or_else(|| PyRuntimeError::new_err("taint params: missing 'name'"))?;
        let effect_s = dict_get_str(params, "effect")?
            .ok_or_else(|| PyRuntimeError::new_err("taint params: missing 'effect'"))?;
        let reason = dict_get_str(params, "reason")?.unwrap_or_default();
        let severity = parse_taint_severity(
            dict_get_str(params, "severity")?
                .as_deref()
                .unwrap_or("medium"),
        )?;
        let expires = parse_expires(dict_get_str(params, "expires")?.as_deref())?;
        let propagate = dict_get_bool(params, "propagate")?.unwrap_or(true);
        let agent_id = dict_get_str(params, "agent_id")?.unwrap_or_else(|| "python".to_string());
        let tp = TaintParams {
            name,
            effect: parse_taint_effect(&effect_s)?,
            reason,
            severity,
            expires_at: expires,
            propagate,
            metadata: TaintMetadata::new(),
            agent_id,
        };
        self.repo
            .taint(ref_name, path, tp)
            .map_err(|e| PyRuntimeError::new_err(format!("{e}")))
    }

    /// Resolve a taint by name at `path`.
    fn untaint(
        &self,
        ref_name: &str,
        path: &str,
        name: &str,
        params: &Bound<'_, pyo3::types::PyDict>,
    ) -> PyResult<()> {
        let reason = dict_get_str(params, "reason")?.unwrap_or_default();
        let proof = dict_get_str(params, "proof")?;
        let agent_id = dict_get_str(params, "agent_id")?.unwrap_or_else(|| "python".to_string());
        self.repo
            .untaint(
                ref_name,
                path,
                name,
                UntaintParams {
                    reason,
                    proof,
                    agent_id,
                },
            )
            .map_err(|e| PyRuntimeError::new_err(format!("{e}")))
    }

    /// Apply a quarantine to `path`.
    fn quarantine(
        &self,
        ref_name: &str,
        path: &str,
        params: &Bound<'_, pyo3::types::PyDict>,
    ) -> PyResult<String> {
        let name = dict_get_str(params, "name")?
            .ok_or_else(|| PyRuntimeError::new_err("quarantine params: missing 'name'"))?;
        let reason = dict_get_str(params, "reason")?.unwrap_or_default();
        let severity = parse_taint_severity(
            dict_get_str(params, "severity")?
                .as_deref()
                .unwrap_or("high"),
        )?;
        let authorized_agents = dict_get_vec_str(params, "authorized_agents")?.unwrap_or_default();
        let expires = parse_expires(dict_get_str(params, "expires")?.as_deref())?;
        let propagate = dict_get_bool(params, "propagate")?.unwrap_or(true);
        let agent_id = dict_get_str(params, "agent_id")?.unwrap_or_else(|| "python".to_string());
        let qp = QuarantineParams {
            name,
            reason,
            severity,
            authorized_agents,
            expires_at: expires,
            propagate,
            agent_id,
        };
        self.repo
            .quarantine(ref_name, path, qp)
            .map_err(|e| PyRuntimeError::new_err(format!("{e}")))
    }

    /// Release a quarantine.
    fn unquarantine(
        &self,
        ref_name: &str,
        path: &str,
        name: &str,
        params: &Bound<'_, pyo3::types::PyDict>,
    ) -> PyResult<()> {
        let reason = dict_get_str(params, "reason")?.unwrap_or_default();
        let proof = dict_get_str(params, "proof")?;
        let agent_id = dict_get_str(params, "agent_id")?.unwrap_or_else(|| "python".to_string());
        self.repo
            .unquarantine(
                ref_name,
                path,
                name,
                UntaintParams {
                    reason,
                    proof,
                    agent_id,
                },
            )
            .map_err(|e| PyRuntimeError::new_err(format!("{e}")))
    }

    /// Apply an advisory watch to `path`.
    #[pyo3(name = "watch")]
    fn watch_taint(
        &self,
        ref_name: &str,
        path: &str,
        params: &Bound<'_, pyo3::types::PyDict>,
    ) -> PyResult<String> {
        let name = dict_get_str(params, "name")?
            .ok_or_else(|| PyRuntimeError::new_err("watch params: missing 'name'"))?;
        let reason = dict_get_str(params, "reason")?.unwrap_or_default();
        let metric = dict_get_str(params, "metric")?;
        let threshold = dict_get_f64(params, "threshold")?;
        let direction = parse_watch_direction(
            dict_get_str(params, "direction")?
                .as_deref()
                .unwrap_or("above"),
        )?;
        let check_interval_secs = dict_get_u64(params, "check_interval_secs")?;
        let expires = parse_expires(dict_get_str(params, "expires")?.as_deref())?;
        let severity = parse_taint_severity(
            dict_get_str(params, "severity")?
                .as_deref()
                .unwrap_or("low"),
        )?;
        let propagate = dict_get_bool(params, "propagate")?.unwrap_or(true);
        let agent_id = dict_get_str(params, "agent_id")?.unwrap_or_else(|| "python".to_string());
        let wp = WatchParams {
            name,
            reason,
            metric,
            threshold,
            direction,
            check_interval_secs,
            expires_at: expires,
            severity,
            propagate,
            agent_id,
        };
        self.repo
            .watch_path(ref_name, path, wp)
            .map_err(|e| PyRuntimeError::new_err(format!("{e}")))
    }

    /// Resolve a watch by name at `path`.
    #[pyo3(name = "unwatch")]
    fn unwatch_taint(
        &self,
        ref_name: &str,
        path: &str,
        name: &str,
        params: &Bound<'_, pyo3::types::PyDict>,
    ) -> PyResult<()> {
        let reason = dict_get_str(params, "reason")?;
        let agent_id = dict_get_str(params, "agent_id")?.unwrap_or_else(|| "python".to_string());
        self.repo
            .unwatch(ref_name, path, name, UnwatchParams { reason, agent_id })
            .map_err(|e| PyRuntimeError::new_err(format!("{e}")))
    }

    /// List taints, filtered by path prefix / kind / resolved state.
    #[pyo3(signature = (path=None, kind=None, include_resolved=false))]
    fn list_taints(
        &self,
        py: Python<'_>,
        path: Option<&str>,
        kind: Option<&str>,
        include_resolved: bool,
    ) -> PyResult<PyObject> {
        let k = match kind {
            Some(s) => Some(parse_taint_kind(s)?),
            None => None,
        };
        let taints = self
            .repo
            .list_taints(path, k, include_resolved)
            .map_err(|e| PyRuntimeError::new_err(format!("{e}")))?;
        let list = pyo3::types::PyList::empty(py);
        for t in &taints {
            list.append(taint_to_py(py, t)?)?;
        }
        Ok(list.into())
    }

    /// Aggregated taint / quarantine / watch check for `path`.
    #[pyo3(signature = (path, agent_id="", confidence=1.0))]
    fn check_taint(
        &self,
        py: Python<'_>,
        path: &str,
        agent_id: &str,
        confidence: f64,
    ) -> PyResult<PyObject> {
        let check = self
            .repo
            .check_taint(path, agent_id, confidence)
            .map_err(|e| PyRuntimeError::new_err(format!("{e}")))?;
        let v = serde_json::to_value(&check)
            .map_err(|e| PyRuntimeError::new_err(format!("check serialize: {e}")))?;
        json_to_py(py, &v)
    }
}

/// Python module definition.
#[pymodule]
fn agentstategraph_py(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<AgentStateGraph>()?;
    m.add_class::<TaskStore>()?;
    m.add_class::<PolicyStore>()?;
    m.add_function(wrap_pyfunction!(exit_codes, m)?)?;
    Ok(())
}
