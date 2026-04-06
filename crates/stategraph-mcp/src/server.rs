//! StateGraph MCP Server — exposes StateGraph operations as MCP tools.

use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_handler, tool_router, ServerHandler};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use stategraph::speculation::SpecHandle;
use stategraph::{CommitOptions, Repository};
use stategraph_core::{IntentCategory, Object};

/// The StateGraph MCP server.
#[derive(Clone)]
pub struct StateGraphServer {
    repo: Arc<Repository>,
    tool_router: ToolRouter<Self>,
}

// -- Parameter types for each tool --

#[derive(Deserialize, JsonSchema)]
pub struct GetParams {
    /// Branch, tag, or commit ID (default: "main").
    #[serde(default = "default_ref")]
    pub r#ref: String,
    /// JSON path (e.g., "/nodes/0/status"). Use "/" for entire state.
    pub path: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct SetParams {
    /// Branch to commit to (default: "main").
    #[serde(default = "default_ref")]
    pub r#ref: String,
    /// JSON path to set.
    pub path: String,
    /// JSON value to write.
    pub value: serde_json::Value,
    /// Intent category: Explore, Refine, Fix, Rollback, Checkpoint, Merge, Migrate.
    pub intent_category: String,
    /// Why this change is being made.
    pub intent_description: String,
    /// Optional reasoning for this approach.
    pub reasoning: Option<String>,
    /// Optional confidence (0.0-1.0).
    pub confidence: Option<f64>,
    /// Optional queryable tags.
    pub tags: Option<Vec<String>>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DeleteParams {
    #[serde(default = "default_ref")]
    pub r#ref: String,
    pub path: String,
    pub intent_category: String,
    pub intent_description: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct BranchParams {
    /// Branch name (supports "/" namespacing).
    pub name: String,
    /// Ref to branch from (default: "main").
    #[serde(default = "default_ref")]
    pub from: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct ListBranchesParams {
    /// Optional namespace prefix filter.
    pub prefix: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct MergeParams {
    /// Branch with changes to merge from.
    pub source: String,
    /// Branch to merge into (default: "main").
    #[serde(default = "default_ref")]
    pub target: String,
    /// Why this merge is being done.
    pub intent_description: String,
    /// Optional reasoning.
    pub reasoning: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct LogParams {
    /// Branch or ref (default: "main").
    #[serde(default = "default_ref")]
    pub r#ref: String,
    /// Max commits to return (default: 10).
    #[serde(default = "default_limit")]
    pub limit: usize,
}

#[derive(Deserialize, JsonSchema)]
pub struct DiffParams {
    /// First ref.
    pub ref_a: String,
    /// Second ref to compare against.
    pub ref_b: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct SpeculateParams {
    /// Ref to speculate from (default: "main").
    #[serde(default = "default_ref")]
    pub from: String,
    /// Human-readable label.
    pub label: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct SpecModifyParams {
    /// Speculation handle ID.
    pub handle_id: u64,
    /// Operations: [{"op": "set", "path": "/x", "value": 42}]
    pub operations: Vec<SpecOp>,
}

#[derive(Deserialize, JsonSchema)]
pub struct SpecOp {
    /// "set" or "delete".
    pub op: String,
    /// Path to modify.
    pub path: String,
    /// Value (required for "set").
    pub value: Option<serde_json::Value>,
}

#[derive(Deserialize, JsonSchema)]
pub struct CompareParams {
    /// Speculation handle IDs to compare.
    pub handle_ids: Vec<u64>,
}

#[derive(Deserialize, JsonSchema)]
pub struct CommitSpecParams {
    /// Speculation handle ID.
    pub handle_id: u64,
    pub intent_category: String,
    pub intent_description: String,
    pub reasoning: Option<String>,
    pub confidence: Option<f64>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DiscardParams {
    /// Speculation handle ID.
    pub handle_id: u64,
}

fn default_ref() -> String {
    "main".to_string()
}
fn default_limit() -> usize {
    10
}

// -- Tool implementations --

#[tool_router]
impl StateGraphServer {
    pub fn new(repo: Arc<Repository>) -> Self {
        Self {
            repo,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "Read a value from state at any branch, tag, or commit. Use JSON-path addressing (e.g., '/nodes/0/hostname'). Use '/' for entire state.")]
    async fn stategraph_get(&self, params: Parameters<GetParams>) -> String {
        let p = params.0;
        match self.repo.get_json(&p.r#ref, &p.path) {
            Ok(value) => serde_json::to_string_pretty(&value).unwrap_or_else(|_| "null".to_string()),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(description = "Write a value to state, creating a new commit. Every write is atomic. Requires intent metadata explaining why this change is being made.")]
    async fn stategraph_set(&self, params: Parameters<SetParams>) -> String {
        let p = params.0;
        let category = parse_category(&p.intent_category);
        let mut opts = CommitOptions::new("mcp-agent", category, &p.intent_description);
        if let Some(r) = p.reasoning {
            opts = opts.with_reasoning(r);
        }
        if let Some(c) = p.confidence {
            opts = opts.with_confidence(c);
        }
        if let Some(t) = p.tags {
            opts = opts.with_tags(t);
        }

        match self.repo.set_json(&p.r#ref, &p.path, &p.value, opts) {
            Ok(commit_id) => format!("Committed: {}", commit_id),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(description = "Remove a value from state, creating a new commit.")]
    async fn stategraph_delete(&self, params: Parameters<DeleteParams>) -> String {
        let p = params.0;
        let category = parse_category(&p.intent_category);
        let opts = CommitOptions::new("mcp-agent", category, &p.intent_description);
        match self.repo.delete(&p.r#ref, &p.path, opts) {
            Ok(commit_id) => format!("Deleted and committed: {}", commit_id),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(description = "Create a new branch from any ref. Use namespaced names like 'agents/my-agent/workspace' or 'explore/approach-a'.")]
    async fn stategraph_branch(&self, params: Parameters<BranchParams>) -> String {
        let p = params.0;
        match self.repo.branch(&p.name, &p.from) {
            Ok(id) => format!("Branch '{}' created at {}", p.name, id.short()),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(description = "List all branches, optionally filtered by namespace prefix.")]
    async fn stategraph_list_branches(&self, params: Parameters<ListBranchesParams>) -> String {
        let p = params.0;
        match self.repo.list_branches(p.prefix.as_deref()) {
            Ok(branches) => {
                let lines: Vec<String> = branches
                    .iter()
                    .map(|(name, id)| format!("  {} -> {}", name, id.short()))
                    .collect();
                format!("{} branches:\n{}", branches.len(), lines.join("\n"))
            }
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(description = "Merge source branch into target. Uses schema-aware merge. Returns conflicts if auto-resolution fails.")]
    async fn stategraph_merge(&self, params: Parameters<MergeParams>) -> String {
        let p = params.0;
        let mut opts = CommitOptions::new("mcp-agent", IntentCategory::Merge, &p.intent_description);
        if let Some(r) = p.reasoning {
            opts = opts.with_reasoning(r);
        }
        match self.repo.merge(&p.source, &p.target, opts) {
            Ok(commit_id) => format!("Merged '{}' into '{}': {}", p.source, p.target, commit_id),
            Err(stategraph::RepoError::MergeConflicts(conflicts)) => {
                format!(
                    "CONFLICTS ({}):\n{}",
                    conflicts.len(),
                    serde_json::to_string_pretty(&conflicts).unwrap_or_default()
                )
            }
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(description = "List commits with full intent, reasoning, and metadata. Use to understand history of state changes.")]
    async fn stategraph_log(&self, params: Parameters<LogParams>) -> String {
        let p = params.0;
        match self.repo.log(&p.r#ref, p.limit) {
            Ok(commits) => {
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
                serde_json::to_string_pretty(&entries).unwrap_or_default()
            }
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(description = "Structured diff between two refs. Returns typed DiffOps (SetValue, AddKey, RemoveKey, etc.), not text diffs.")]
    async fn stategraph_diff(&self, params: Parameters<DiffParams>) -> String {
        let p = params.0;
        match self.repo.diff(&p.ref_a, &p.ref_b) {
            Ok(ops) if ops.is_empty() => "No differences.".to_string(),
            Ok(ops) => {
                format!(
                    "{} changes:\n{}",
                    ops.len(),
                    serde_json::to_string_pretty(&ops).unwrap_or_default()
                )
            }
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(description = "Create a lightweight speculation from a ref. O(1) creation. Use to explore approaches before committing.")]
    async fn stategraph_speculate(&self, params: Parameters<SpeculateParams>) -> String {
        let p = params.0;
        match self.repo.speculate(&p.from, p.label.clone()) {
            Ok(handle) => format!(
                "Speculation created: handle_id={} (from '{}', label: {:?})",
                handle.id(),
                p.from,
                p.label
            ),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(description = "Modify state within a speculation. Changes are isolated until committed.")]
    async fn stategraph_spec_modify(&self, params: Parameters<SpecModifyParams>) -> String {
        let p = params.0;
        let handle = SpecHandle::from_id(p.handle_id);

        for op in &p.operations {
            match op.op.as_str() {
                "set" => {
                    let value = match &op.value {
                        Some(v) => json_value_to_object(v),
                        None => return "Error: 'set' op requires a 'value'".to_string(),
                    };
                    if let Err(e) = self.repo.spec_set(handle, &op.path, &value) {
                        return format!("Error: {}", e);
                    }
                }
                "delete" => {
                    if let Err(e) = self.repo.spec_delete(handle, &op.path) {
                        return format!("Error: {}", e);
                    }
                }
                other => return format!("Error: unknown op '{}'", other),
            }
        }

        format!("Applied {} operations to speculation {}", p.operations.len(), p.handle_id)
    }

    #[tool(description = "Compare multiple speculations. Returns diffs showing how each diverges from base.")]
    async fn stategraph_compare(&self, params: Parameters<CompareParams>) -> String {
        let p = params.0;
        let handles: Vec<SpecHandle> = p.handle_ids.iter().map(|&id| SpecHandle::from_id(id)).collect();
        match self.repo.compare_speculations(&handles) {
            Ok(comparison) => {
                let entries: Vec<serde_json::Value> = comparison
                    .entries
                    .iter()
                    .map(|e| {
                        serde_json::json!({
                            "handle": e.handle.id(),
                            "label": e.label,
                            "changes": e.diff_from_base.len(),
                            "diff": e.diff_from_base,
                        })
                    })
                    .collect();
                serde_json::to_string_pretty(&entries).unwrap_or_default()
            }
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(description = "Promote a speculation to a real commit on its base branch. The speculation is consumed.")]
    async fn stategraph_commit_spec(&self, params: Parameters<CommitSpecParams>) -> String {
        let p = params.0;
        let handle = SpecHandle::from_id(p.handle_id);
        let category = parse_category(&p.intent_category);
        let mut opts = CommitOptions::new("mcp-agent", category, &p.intent_description);
        if let Some(r) = p.reasoning {
            opts = opts.with_reasoning(r);
        }
        if let Some(c) = p.confidence {
            opts = opts.with_confidence(c);
        }
        match self.repo.commit_speculation(handle, opts) {
            Ok(commit_id) => format!("Speculation committed: {}", commit_id),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(description = "Discard a speculation. All changes freed immediately.")]
    async fn stategraph_discard(&self, params: Parameters<DiscardParams>) -> String {
        let p = params.0;
        let handle = SpecHandle::from_id(p.handle_id);
        match self.repo.discard_speculation(handle) {
            Ok(()) => format!("Speculation {} discarded", p.handle_id),
            Err(e) => format!("Error: {}", e),
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for StateGraphServer {}

// -- Helpers --

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

fn json_value_to_object(value: &serde_json::Value) -> Object {
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
