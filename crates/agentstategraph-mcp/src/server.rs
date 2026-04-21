//! AgentStateGraph MCP Server — exposes AgentStateGraph operations as MCP tools.

use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

use agentstategraph::speculation::SpecHandle;
use agentstategraph::{CommitOptions, Repository};
use agentstategraph_core::{DiffOp, IntentCategory, Object, QueryFilters};
use agentstategraph_policy::{
    ChangeProposal, Decision, Policy, PolicySignature, PolicyStore, SignatureVerifier, Situation,
};
use agentstategraph_policy_sign::{PolicySigner, canonicalize};
use agentstategraph_tasks::{Priority, Proof, TaskId, TaskStore};

/// Threshold above which a change is tagged `large` in token inference.
pub const LARGE_CHANGE_THRESHOLD: usize = 50;

/// The AgentStateGraph MCP server.
#[derive(Clone)]
pub struct AgentStateGraphServer {
    repo: Arc<Repository>,
    tasks: Arc<TaskStore>,
    policies: Arc<PolicyStore>,
    /// Fail-safe translation for `Decision::NoPolicyMatch`. "deny" (default)
    /// flips NoPolicyMatch to Deny at the MCP layer; "allow" passes through.
    /// Configured via `AgentStateGraphServer::with_fail_safe`.
    policy_fail_safe: String,
    /// Optional policy signer — when set, the `policy_sign` tool uses it
    /// to produce signatures for policies at rest. Defaults to `None`:
    /// `policy_sign` returns `{"error": "no signer registered"}`.
    signer: Option<Arc<dyn PolicySigner>>,
    /// Optional signature verifier wired through to the internal
    /// `PolicyStore`. When set, `policy_verify` dispatches to it and
    /// the store gates on `require_signed_policies`.
    verifier: Option<Arc<dyn SignatureVerifier>>,
    /// When true (and a verifier is registered), unsigned policies are
    /// filtered from `active()` in the store — `evaluate` /
    /// `evaluate_change` therefore treat them as not-currently-active.
    /// §2c of the 0.7.5 plan.
    require_signed_policies: bool,
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
    /// Optional fields attached to the implicit `ChangeProposal` that is
    /// evaluated against policy before the speculation is promoted.
    /// Caller uses these to satisfy `required_fields` on change-cost
    /// policies (POLICY_V1.md §22.2.1), e.g. `{"estimated_downtime":
    /// "5m", "rollback_plan": "T-007"}`.
    #[serde(default)]
    pub attached_fields: Option<std::collections::HashMap<String, String>>,
    /// Optional base-ref override. If omitted, the speculation's own
    /// base_ref (captured when `agentstategraph_speculate` was called)
    /// is used to compute the diff for token inference.
    #[serde(default)]
    pub base_ref: Option<String>,
    /// Optional sibling handle IDs passed through as the proposal's
    /// `alternatives`. Purely metadata for audit — does not affect the
    /// evaluation result.
    #[serde(default)]
    pub alternatives: Option<Vec<u64>>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DiscardParams {
    /// Speculation handle ID.
    pub handle_id: u64,
}

#[derive(Deserialize, JsonSchema)]
pub struct QueryParams {
    /// Branch to query (default: "main").
    pub r#ref: Option<String>,
    /// Filter by agent ID.
    pub agent_id: Option<String>,
    /// Filter by intent category.
    pub intent_category: Option<String>,
    /// Filter by tags (all must match).
    pub tags: Option<Vec<String>>,
    /// Filter by authority principal.
    pub authority_principal: Option<String>,
    /// Full-text search in reasoning traces.
    pub reasoning_contains: Option<String>,
    /// Minimum confidence (used with confidence_max for range).
    pub confidence_min: Option<f64>,
    /// Maximum confidence.
    pub confidence_max: Option<f64>,
    /// Only results with deviations from plan.
    pub has_deviations: Option<bool>,
    /// Max results (default: 20).
    pub limit: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
pub struct BlameParams {
    /// Branch (default: "main").
    pub r#ref: Option<String>,
    /// Path to blame (e.g., "/nodes/2/status").
    pub path: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct CreateEpochParams {
    /// Epoch ID (e.g., "2026-04-incident-node3").
    pub id: String,
    /// Description.
    pub description: String,
    /// Root intent IDs that define this epoch.
    pub root_intents: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct SealEpochParams {
    /// Epoch ID.
    pub id: String,
    /// Final summary of the epoch's work.
    pub summary: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct SessionListParams {
    /// Optional agent filter.
    pub agent_id: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct EnterEpochParams {
    /// Id of an existing epoch (create it first via create_epoch).
    /// Subsequent commits will land with commits.epoch_id set to this id
    /// until exit_epoch is called. A sealed epoch cannot be entered.
    pub epoch_id: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct EnterSessionParams {
    /// Id of an existing session. Subsequent commits will land with
    /// commits.session_id set to this id until exit_session is called.
    /// A session that has ended cannot be entered.
    pub session_id: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct ListPathsParams {
    /// Branch or ref (default: "main").
    #[serde(default = "default_ref")]
    pub r#ref: String,
    /// Path prefix to list under (default: "/").
    #[serde(default = "default_root")]
    pub prefix: String,
    /// Max tree depth to traverse (default: 50).
    pub max_depth: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
pub struct GetTreeParams {
    /// Branch or ref (default: "main").
    #[serde(default = "default_ref")]
    pub r#ref: String,
    /// Path prefix to get subtree for (default: "/").
    #[serde(default = "default_root")]
    pub prefix: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct SearchValuesParams {
    /// Branch or ref (default: "main").
    #[serde(default = "default_ref")]
    pub r#ref: String,
    /// Search query string (case-insensitive, matches values and key names).
    pub query: String,
    /// Max results (default: 50).
    pub max_results: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
pub struct StatsParams {
    /// Branch or ref (default: "main").
    #[serde(default = "default_ref")]
    pub r#ref: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct CommitGraphParams {
    /// Branch or ref (default: "main").
    #[serde(default = "default_ref")]
    pub r#ref: String,
    /// Max commits to include (default: 50).
    #[serde(default = "default_graph_depth")]
    pub depth: usize,
}

#[derive(Deserialize, JsonSchema)]
pub struct IntentTreeParams {
    /// Branch or ref (default: "main").
    #[serde(default = "default_ref")]
    pub r#ref: String,
    /// Optional root commit ID to start from.
    pub root_commit_id: Option<String>,
}

fn default_ref() -> String {
    "main".to_string()
}
fn default_root() -> String {
    "/".to_string()
}
fn default_limit() -> usize {
    10
}
fn default_graph_depth() -> usize {
    50
}

// -- Policy parameter types --

#[derive(Deserialize, JsonSchema)]
pub struct PolicyProposeParams {
    #[serde(default = "default_ref")]
    pub r#ref: String,
    /// Full `Policy` JSON — POLICY_V1.md §2.1 + §22.2.1.
    pub policy: serde_json::Value,
}

#[derive(Deserialize, JsonSchema)]
pub struct PolicyRatifyParams {
    #[serde(default = "default_ref")]
    pub r#ref: String,
    pub path: String,
    pub ratifier: String,
    pub reasoning: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct PolicySupersedeParams {
    #[serde(default = "default_ref")]
    pub r#ref: String,
    pub old_path: String,
    /// Full new `Policy` JSON.
    pub new_policy: serde_json::Value,
}

#[derive(Deserialize, JsonSchema)]
pub struct PolicyListParams {
    #[serde(default = "default_ref")]
    pub r#ref: String,
    pub prefix: Option<String>,
    /// One of `"active"`, `"proposed"`, `"all"`. Default: `"active"`.
    pub status: Option<String>,
    /// Optional tenant scope (0.7.5 §3b). `None` returns every policy
    /// regardless of `tenant_id`; `Some(tid)` returns only policies with
    /// `tenant_id == Some(tid)` or `tenant_id == None` (globals always
    /// apply).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_filter: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct PolicyShowParams {
    #[serde(default = "default_ref")]
    pub r#ref: String,
    pub path: String,
    pub version: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
pub struct PolicyHistoryParams {
    #[serde(default = "default_ref")]
    pub r#ref: String,
    pub path: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct PolicyEvaluateParams {
    #[serde(default = "default_ref")]
    pub r#ref: String,
    /// Situation facts as a flat string map.
    pub situation: std::collections::HashMap<String, String>,
    pub action: String,
    pub agent_id: String,
    /// Optional tenant scope (0.7.5 §3b). `None` is no filter;
    /// `Some(tid)` consults only policies with `tenant_id == Some(tid)`
    /// or `tenant_id == None` (globals always apply).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_filter: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct PolicyEvaluateChangeParams {
    #[serde(default = "default_ref")]
    pub r#ref: String,
    /// Full `ChangeProposal` JSON — POLICY_V1.md §22.2.2.
    pub proposal: serde_json::Value,
    /// Optional tenant scope (0.7.5 §3b). `None` is no filter;
    /// `Some(tid)` consults only policies with `tenant_id == Some(tid)`
    /// or `tenant_id == None` (globals always apply).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_filter: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct PolicySignParams {
    #[serde(default = "default_ref")]
    pub r#ref: String,
    pub path: String,
    /// Optional hint passed through to the signer. `Ed25519Signer` uses
    /// its configured `key_id` and ignores this; multi-key signers may
    /// use it to pick which key to sign with.
    pub signer_key_id: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct PolicyVerifyParams {
    #[serde(default = "default_ref")]
    pub r#ref: String,
    pub path: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct PolicyCheckTokensParams {
    #[serde(default = "default_ref")]
    pub r#ref: String,
    pub tokens: Vec<String>,
}

// -- Plan/Task parameter types --

#[derive(Deserialize, JsonSchema)]
pub struct CreatePlanParams {
    /// Branch (default: "main").
    #[serde(default = "default_ref")]
    pub r#ref: String,
    /// Plan name (e.g., "cluster-drift-reconciliation").
    pub name: String,
    /// Optional description.
    pub description: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ListPlansParams {
    #[serde(default = "default_ref")]
    pub r#ref: String,
    /// Optional status filter: Active, Completed, Archived.
    pub status: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct GetPlanParams {
    #[serde(default = "default_ref")]
    pub r#ref: String,
    pub name: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct AddTaskParams {
    #[serde(default = "default_ref")]
    pub r#ref: String,
    /// Plan name.
    pub plan: String,
    /// Task title.
    pub title: String,
    /// Priority: Low, Medium, High, Critical (default: Medium).
    pub priority: Option<String>,
    /// Parent task ID for subtasks (e.g., "t-001").
    pub parent_id: Option<String>,
    /// Task IDs this task is blocked by.
    pub blocked_by: Option<Vec<String>>,
    /// Agent this task is assigned to.
    pub assigned_to: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ListTasksParams {
    #[serde(default = "default_ref")]
    pub r#ref: String,
    pub plan: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct TaskActionParams {
    #[serde(default = "default_ref")]
    pub r#ref: String,
    pub plan: String,
    /// Task ID (e.g., "t-001").
    pub task_id: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct CompleteTaskParams {
    #[serde(default = "default_ref")]
    pub r#ref: String,
    pub plan: String,
    pub task_id: String,
    /// Proof kind: Commit, File, Test, or Text.
    pub proof_kind: String,
    /// Proof value (commit hash, file path, test name, or text).
    pub proof_value: String,
    /// Optional proof note.
    pub proof_note: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct AbandonTaskParams {
    #[serde(default = "default_ref")]
    pub r#ref: String,
    pub plan: String,
    pub task_id: String,
    /// Reason for abandoning.
    pub reason: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct AssignTaskParams {
    #[serde(default = "default_ref")]
    pub r#ref: String,
    pub plan: String,
    pub task_id: String,
    /// Agent to assign to.
    pub agent: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct NextTaskParams {
    #[serde(default = "default_ref")]
    pub r#ref: String,
    pub plan: String,
    /// Optional agent filter — get next task for a specific agent.
    pub agent: Option<String>,
}

// -- Tool implementations --

#[tool_router]
impl AgentStateGraphServer {
    pub fn new(repo: Arc<Repository>) -> Self {
        let tasks = Arc::new(TaskStore::new(repo.clone(), "/plans", "mcp-agent"));
        let policies = Arc::new(PolicyStore::new(repo.clone(), "/policies", "mcp-agent"));
        Self {
            repo,
            tasks,
            policies,
            policy_fail_safe: "deny".to_string(),
            signer: None,
            verifier: None,
            require_signed_policies: false,
            tool_router: Self::tool_router(),
        }
    }

    /// Install a `PolicySigner` used by the `policy_sign` tool. Without
    /// this, the tool returns `{"error": "no signer registered"}`.
    pub fn with_signer(mut self, signer: Arc<dyn PolicySigner>) -> Self {
        self.signer = Some(signer);
        self
    }

    /// Install a `SignatureVerifier` used by the `policy_verify` tool
    /// and wired through to the internal `PolicyStore` so `evaluate` /
    /// `evaluate_change` can gate on signature validity. The store is
    /// rebuilt with the verifier and the current `require_signed_policies`
    /// setting.
    pub fn with_policy_verifier(mut self, verifier: Arc<dyn SignatureVerifier>) -> Self {
        self.verifier = Some(verifier.clone());
        self.policies = Arc::new(
            PolicyStore::new(self.repo.clone(), "/policies", "mcp-agent")
                .with_verifier(verifier)
                .with_require_signed(self.require_signed_policies),
        );
        self
    }

    /// Toggle `require_signed_policies` on the internal `PolicyStore`.
    /// Only meaningful when a verifier is also registered; with no
    /// verifier the store is in pass-through mode.
    pub fn with_require_signed_policies(mut self, require: bool) -> Self {
        self.require_signed_policies = require;
        if let Some(verifier) = self.verifier.clone() {
            self.policies = Arc::new(
                PolicyStore::new(self.repo.clone(), "/policies", "mcp-agent")
                    .with_verifier(verifier)
                    .with_require_signed(require),
            );
        }
        self
    }

    /// Override the fail-safe translation applied to `Decision::NoPolicyMatch`
    /// in the `policy_evaluate` / `policy_evaluate_change` tools. Accepts
    /// `"deny"` (default) or `"allow"`. Unknown values are coerced to
    /// `"deny"`.
    pub fn with_fail_safe(mut self, mode: impl Into<String>) -> Self {
        let m = mode.into();
        self.policy_fail_safe = if m == "allow" {
            "allow".to_string()
        } else {
            "deny".to_string()
        };
        self
    }

    /// Read-only accessor for tests.
    pub fn policies(&self) -> &PolicyStore {
        &self.policies
    }

    #[tool(
        description = "Read a value from state at any branch, tag, or commit. Use JSON-path addressing (e.g., '/nodes/0/hostname'). Use '/' for entire state."
    )]
    async fn agentstategraph_get(&self, params: Parameters<GetParams>) -> String {
        let p = params.0;
        match self.repo.get_json(&p.r#ref, &p.path) {
            Ok(value) => {
                serde_json::to_string_pretty(&value).unwrap_or_else(|_| "null".to_string())
            }
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(
        description = "Write a value to state, creating a new commit. Every write is atomic. Requires intent metadata explaining why this change is being made."
    )]
    async fn agentstategraph_set(&self, params: Parameters<SetParams>) -> String {
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
    async fn agentstategraph_delete(&self, params: Parameters<DeleteParams>) -> String {
        let p = params.0;
        let category = parse_category(&p.intent_category);
        let opts = CommitOptions::new("mcp-agent", category, &p.intent_description);
        match self.repo.delete(&p.r#ref, &p.path, opts) {
            Ok(commit_id) => format!("Deleted and committed: {}", commit_id),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(
        description = "Create a new branch from any ref. Use namespaced names like 'agents/my-agent/workspace' or 'explore/approach-a'."
    )]
    async fn agentstategraph_branch(&self, params: Parameters<BranchParams>) -> String {
        let p = params.0;
        match self.repo.branch(&p.name, &p.from) {
            Ok(id) => format!("Branch '{}' created at {}", p.name, id.short()),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(description = "List all branches, optionally filtered by namespace prefix.")]
    async fn agentstategraph_list_branches(
        &self,
        params: Parameters<ListBranchesParams>,
    ) -> String {
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

    #[tool(
        description = "Merge source branch into target. Uses schema-aware merge. Returns conflicts if auto-resolution fails."
    )]
    async fn agentstategraph_merge(&self, params: Parameters<MergeParams>) -> String {
        let p = params.0;
        let mut opts =
            CommitOptions::new("mcp-agent", IntentCategory::Merge, &p.intent_description);
        if let Some(r) = p.reasoning {
            opts = opts.with_reasoning(r);
        }
        match self.repo.merge(&p.source, &p.target, opts) {
            Ok(commit_id) => format!("Merged '{}' into '{}': {}", p.source, p.target, commit_id),
            Err(agentstategraph::RepoError::MergeConflicts(conflicts)) => {
                format!(
                    "CONFLICTS ({}):\n{}",
                    conflicts.len(),
                    serde_json::to_string_pretty(&conflicts).unwrap_or_default()
                )
            }
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(
        description = "List commits with full intent, reasoning, and metadata. Use to understand history of state changes."
    )]
    async fn agentstategraph_log(&self, params: Parameters<LogParams>) -> String {
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

    #[tool(
        description = "Structured diff between two refs. Returns typed DiffOps (SetValue, AddKey, RemoveKey, etc.), not text diffs."
    )]
    async fn agentstategraph_diff(&self, params: Parameters<DiffParams>) -> String {
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

    #[tool(
        description = "Create a lightweight speculation from a ref. Returns a numeric handle_id (e.g. 1, 2, 3) that you MUST use with spec_modify, compare, commit_spec, and discard. Speculations are in-memory — do NOT use agentstategraph_set with a spec ref. Instead, use agentstategraph_spec_modify with the handle_id to make changes within the speculation."
    )]
    async fn agentstategraph_speculate(&self, params: Parameters<SpeculateParams>) -> String {
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

    #[tool(
        description = "Modify state within a speculation using its numeric handle_id (from agentstategraph_speculate). Pass operations as [{\"op\": \"set\", \"path\": \"/my/path\", \"value\": \"myvalue\"}]. Changes are isolated until you call commit_spec. Do NOT use agentstategraph_set for speculation changes — use this tool instead."
    )]
    async fn agentstategraph_spec_modify(&self, params: Parameters<SpecModifyParams>) -> String {
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

        format!(
            "Applied {} operations to speculation {}",
            p.operations.len(),
            p.handle_id
        )
    }

    #[tool(
        description = "Compare multiple speculations side-by-side using their numeric handle_ids (from agentstategraph_speculate). Returns, per handle, the diff from base plus the inferred change tokens (destructive, schema-change, ref-rewrite, large, reindex, migration) that commit_spec would evaluate against the policy engine. Use to pre-flight policy gates before promoting a winner."
    )]
    async fn agentstategraph_compare(&self, params: Parameters<CompareParams>) -> String {
        let p = params.0;
        let handles: Vec<SpecHandle> = p
            .handle_ids
            .iter()
            .map(|&id| SpecHandle::from_id(id))
            .collect();
        match self.repo.compare_speculations(&handles) {
            Ok(comparison) => {
                let entries: Vec<serde_json::Value> = comparison
                    .entries
                    .iter()
                    .map(|e| {
                        // Policy pre-flight: emit the same tokens that
                        // commit_spec would infer, so an agent can see
                        // which handles will hit which policies before
                        // it commits to a promotion.
                        let tokens = infer_tokens_from_diff(&e.diff_from_base);
                        serde_json::json!({
                            "handle": e.handle.id(),
                            "label": e.label,
                            "changes": e.diff_from_base.len(),
                            "tokens": tokens,
                            "diff": e.diff_from_base,
                        })
                    })
                    .collect();
                serde_json::to_string_pretty(&entries).unwrap_or_default()
            }
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(
        description = "Promote a speculation to a real commit on its base branch using its numeric handle_id. Before promoting, the speculation's diff is turned into a ChangeProposal (with inferred tokens: destructive, schema-change, ref-rewrite, large, reindex, migration) and evaluated against the policy engine. If the decision is Deny or RequireApproval the speculation is NOT promoted and the Decision JSON is returned so the caller can apply the fallback branch. The speculation is consumed only on Allow / NoPolicyMatch."
    )]
    async fn agentstategraph_commit_spec(&self, params: Parameters<CommitSpecParams>) -> String {
        let p = params.0;
        let handle = SpecHandle::from_id(p.handle_id);

        // Infer the ChangeProposal from the live speculation diff.
        let tokens = match infer_change_tokens(&self.repo, handle) {
            Ok(t) => t,
            Err(e) => return format!("Error: {}", e),
        };
        let intent = p.intent_description.clone();
        let mut proposal = ChangeProposal::new(
            "promote_speculation",
            "mcp-agent",
            &intent,
            p.handle_id.to_string(),
        );
        proposal.tokens = tokens;
        proposal.alternatives = p
            .alternatives
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(|id| id.to_string())
            .collect();
        if let Some(af) = p.attached_fields.clone() {
            proposal.attached_fields = af;
        }

        // Ref to evaluate policies against (same ref we will commit back to).
        let eval_ref = p.base_ref.clone().unwrap_or_else(|| "main".to_string());

        match self.policies.evaluate_change(&eval_ref, &proposal) {
            Ok(Decision::Allow { .. }) | Ok(Decision::NoPolicyMatch) => {
                // Allowed — run the existing promotion path.
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
            Ok(decision @ (Decision::Deny { .. } | Decision::RequireApproval { .. })) => {
                serde_json::to_string_pretty(&decision).unwrap_or_else(|_| "null".to_string())
            }
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(
        description = "Discard a speculation by its numeric handle_id. All changes freed immediately. Use this for the 'losers' after promoting a winner with commit_spec."
    )]
    async fn agentstategraph_discard(&self, params: Parameters<DiscardParams>) -> String {
        let p = params.0;
        let handle = SpecHandle::from_id(p.handle_id);
        match self.repo.discard_speculation(handle) {
            Ok(()) => format!("Speculation {} discarded", p.handle_id),
            Err(e) => format!("Error: {}", e),
        }
    }

    // -- Query tools --

    #[tool(
        description = "Query commits with composable filters. Filter by agent, intent category, tags, reasoning text, confidence range, date range, and more. All filters are AND-combined."
    )]
    async fn agentstategraph_query(&self, params: Parameters<QueryParams>) -> String {
        let p = params.0;
        let filters = QueryFilters {
            agent_id: p.agent_id,
            intent_category: p.intent_category,
            tags: p.tags,
            reasoning_contains: p.reasoning_contains,
            confidence_range: p.confidence_min.zip(p.confidence_max),
            authority_principal: p.authority_principal,
            has_deviations: p.has_deviations,
            ..Default::default()
        };
        let limit = p.limit.unwrap_or(20);
        let ref_name = p.r#ref.unwrap_or_else(|| "main".to_string());

        match self.repo.query_commits(&ref_name, &filters, limit) {
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
                            "timestamp": c.timestamp.to_rfc3339(),
                        })
                    })
                    .collect();
                serde_json::to_string_pretty(&entries).unwrap_or_default()
            }
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(
        description = "Blame — find which commit last modified a value at a path and why. Returns the agent, intent, reasoning, and timestamp."
    )]
    async fn agentstategraph_blame(&self, params: Parameters<BlameParams>) -> String {
        let p = params.0;
        let ref_name = p.r#ref.unwrap_or_else(|| "main".to_string());
        match self.repo.blame(&ref_name, &p.path) {
            Ok(entry) => serde_json::to_string_pretty(&entry).unwrap_or_default(),
            Err(e) => format!("Error: {}", e),
        }
    }

    // -- Epoch tools --

    #[tool(
        description = "Create a new epoch to group related work. Commits are associated by intent lineage."
    )]
    async fn agentstategraph_create_epoch(&self, params: Parameters<CreateEpochParams>) -> String {
        let p = params.0;
        match self
            .repo
            .create_epoch(&p.id, &p.description, p.root_intents)
        {
            Ok(epoch) => format!("Epoch '{}' created (status: {:?})", epoch.id, epoch.status),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(
        description = "Seal an epoch, making it read-only and tamper-evident. Cannot be undone."
    )]
    async fn agentstategraph_seal_epoch(&self, params: Parameters<SealEpochParams>) -> String {
        let p = params.0;
        match self.repo.seal_epoch(&p.id, &p.summary) {
            Ok(()) => format!("Epoch '{}' sealed", p.id),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(
        description = "Set the active epoch for this server. Subsequent commits through this process will be associated with this epoch via commits.epoch_id, enabling audit rollup by epoch. The epoch must already exist (call create_epoch first) and must not be sealed. Returns the previous active epoch id, if any."
    )]
    async fn agentstategraph_enter_epoch(&self, params: Parameters<EnterEpochParams>) -> String {
        let p = params.0;
        // Validate: epoch exists and is not sealed.
        let epoch = match self.repo.get_epoch(&p.epoch_id) {
            Ok(e) => e,
            Err(e) => return format!("Error: {}", e),
        };
        if matches!(
            epoch.status,
            agentstategraph_core::EpochStatus::Sealed | agentstategraph_core::EpochStatus::Archived
        ) {
            return format!("Error: epoch '{}' is sealed or archived", p.epoch_id);
        }
        let prev = match self.repo.active_epoch() {
            Ok(p) => p,
            Err(e) => return format!("Error: {}", e),
        };
        match self.repo.set_active_epoch(Some(p.epoch_id.clone())) {
            Ok(()) => serde_json::json!({
                "entered": p.epoch_id,
                "previous": prev,
            })
            .to_string(),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(
        description = "Clear the active epoch for this server. Subsequent commits will not be associated with any epoch (commits.epoch_id = NULL). Returns the epoch id that was active, if any."
    )]
    async fn agentstategraph_exit_epoch(&self) -> String {
        let prev = match self.repo.active_epoch() {
            Ok(p) => p,
            Err(e) => return format!("Error: {}", e),
        };
        match self.repo.set_active_epoch(None) {
            Ok(()) => serde_json::json!({ "exited": prev }).to_string(),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(
        description = "Set the active session for this server. Subsequent commits will be associated with this session via commits.session_id. The session must exist and must not have ended. Returns the previous active session id, if any."
    )]
    async fn agentstategraph_enter_session(
        &self,
        params: Parameters<EnterSessionParams>,
    ) -> String {
        let p = params.0;
        let session = match self.repo.sessions().get(&p.session_id) {
            Ok(opt) => match opt {
                Some(s) => s,
                None => return format!("Error: session '{}' not found", p.session_id),
            },
            Err(e) => return format!("Error: {}", e),
        };
        if !matches!(session.status, agentstategraph_core::SessionStatus::Active) {
            return format!(
                "Error: session '{}' is not Active (status: {:?})",
                p.session_id, session.status
            );
        }
        let prev = match self.repo.active_session() {
            Ok(p) => p,
            Err(e) => return format!("Error: {}", e),
        };
        match self.repo.set_active_session(Some(p.session_id.clone())) {
            Ok(()) => serde_json::json!({
                "entered": p.session_id,
                "previous": prev,
            })
            .to_string(),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(
        description = "Clear the active session for this server. Subsequent commits will not be associated with any session."
    )]
    async fn agentstategraph_exit_session(&self) -> String {
        let prev = match self.repo.active_session() {
            Ok(p) => p,
            Err(e) => return format!("Error: {}", e),
        };
        match self.repo.set_active_session(None) {
            Ok(()) => serde_json::json!({ "exited": prev }).to_string(),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(description = "List all epochs with their status, dates, and commit counts.")]
    async fn agentstategraph_list_epochs(&self) -> String {
        match self.repo.list_epochs() {
            Ok(entries) => {
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
                            "created": e.created_at.to_rfc3339(),
                            "sealed": e.sealed_at.map(|t| t.to_rfc3339()),
                        })
                    })
                    .collect();
                serde_json::to_string_pretty(&json).unwrap_or_default()
            }
            Err(e) => format!("Error: {}", e),
        }
    }

    // -- Session tools --

    #[tool(
        description = "List active agent sessions. Shows parent-child relationships and path scoping."
    )]
    async fn agentstategraph_sessions(&self, params: Parameters<SessionListParams>) -> String {
        let sessions = match self.repo.sessions().list(params.0.agent_id.as_deref()) {
            Ok(s) => s,
            Err(e) => return format!("Error: {}", e),
        };
        let json: Vec<serde_json::Value> = sessions
            .iter()
            .map(|s| {
                serde_json::json!({
                    "id": s.id,
                    "agent": s.agent_id,
                    "branch": s.working_branch,
                    "parent_session": s.parent_session,
                    "delegated_intent": s.delegated_intent,
                    "report_to": s.report_to,
                    "path_scope": s.path_scope,
                    "created": s.created_at.to_rfc3339(),
                })
            })
            .collect();
        serde_json::to_string_pretty(&json).unwrap_or_default()
    }

    // -- Explorer tools (0.4.0) --

    #[tool(
        description = "List all paths in the state tree under a prefix. Use to explore what data exists. Returns leaf paths (values, not intermediate maps)."
    )]
    async fn agentstategraph_list_paths(&self, params: Parameters<ListPathsParams>) -> String {
        let p = params.0;
        match self.repo.list_paths(&p.r#ref, &p.prefix, p.max_depth) {
            Ok(paths) => format!("{} paths:\n{}", paths.len(), paths.join("\n")),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(
        description = "Get an entire subtree as nested JSON. Efficient batch alternative to reading individual paths."
    )]
    async fn agentstategraph_get_tree(&self, params: Parameters<GetTreeParams>) -> String {
        let p = params.0;
        match self.repo.get_tree(&p.r#ref, &p.prefix) {
            Ok(json) => serde_json::to_string_pretty(&json).unwrap_or_default(),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(
        description = "Search state values and key names for a query string. Case-insensitive. Returns matching paths and values."
    )]
    async fn agentstategraph_search(&self, params: Parameters<SearchValuesParams>) -> String {
        let p = params.0;
        match self.repo.search_values(&p.r#ref, &p.query, p.max_results) {
            Ok(results) if results.is_empty() => "No matches found.".to_string(),
            Ok(results) => {
                let entries: Vec<serde_json::Value> = results
                    .iter()
                    .map(|(path, value)| serde_json::json!({"path": path, "value": value}))
                    .collect();
                serde_json::to_string_pretty(&entries).unwrap_or_default()
            }
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(
        description = "Get summary statistics: commit count, branch count, path count, epoch count, agent list, and latest commit."
    )]
    async fn agentstategraph_stats(&self, params: Parameters<StatsParams>) -> String {
        let p = params.0;
        match self.repo.stats(&p.r#ref) {
            Ok(json) => serde_json::to_string_pretty(&json).unwrap_or_default(),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(
        description = "Get the commit DAG for visualization. Returns nodes with parents, agent, category, and timestamps."
    )]
    async fn agentstategraph_commit_graph(&self, params: Parameters<CommitGraphParams>) -> String {
        let p = params.0;
        match self.repo.commit_graph(&p.r#ref, p.depth) {
            Ok(nodes) => serde_json::to_string_pretty(&nodes).unwrap_or_default(),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(
        description = "Get the intent decomposition tree. Shows how intents are broken down into sub-tasks across agents."
    )]
    async fn agentstategraph_intent_tree(&self, params: Parameters<IntentTreeParams>) -> String {
        let p = params.0;
        match self.repo.intent_tree(&p.r#ref, p.root_commit_id.as_deref()) {
            Ok(json) => serde_json::to_string_pretty(&json).unwrap_or_default(),
            Err(e) => format!("Error: {}", e),
        }
    }

    // -- Plan & Task tools --

    #[tool(
        description = "Create a new plan — a named container for tasks with a state machine. Plans track structured work: drift reconciliation, upgrades, incident response."
    )]
    async fn agentstategraph_create_plan(&self, params: Parameters<CreatePlanParams>) -> String {
        let p = params.0;
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

    #[tool(
        description = "List all plans, optionally filtered by status (Active, Completed, Archived)."
    )]
    async fn agentstategraph_list_plans(&self, params: Parameters<ListPlansParams>) -> String {
        let p = params.0;
        let status = p.status.map(|s| match s.to_lowercase().as_str() {
            "active" => agentstategraph_tasks::PlanStatus::Active,
            "completed" => agentstategraph_tasks::PlanStatus::Completed,
            "archived" => agentstategraph_tasks::PlanStatus::Archived,
            _ => agentstategraph_tasks::PlanStatus::Active,
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

    #[tool(description = "Get a plan's details including status and task summary.")]
    async fn agentstategraph_get_plan(&self, params: Parameters<GetPlanParams>) -> String {
        let p = params.0;
        match self.tasks.get_plan(&p.r#ref, &p.name) {
            Ok(plan) => {
                let tasks = self.tasks.list_tasks(&p.r#ref, &p.name).unwrap_or_default();
                let pending = tasks
                    .iter()
                    .filter(|t| matches!(t.status, agentstategraph_tasks::TaskStatus::Pending))
                    .count();
                let in_progress = tasks
                    .iter()
                    .filter(|t| matches!(t.status, agentstategraph_tasks::TaskStatus::InProgress))
                    .count();
                let done = tasks
                    .iter()
                    .filter(|t| matches!(t.status, agentstategraph_tasks::TaskStatus::Done))
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

    #[tool(
        description = "Add a task to a plan. Tasks have a strict state machine: pending → in_progress → done. Supports priority, blockers, parent tasks, and agent assignment."
    )]
    async fn agentstategraph_add_task(&self, params: Parameters<AddTaskParams>) -> String {
        let p = params.0;
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

    #[tool(
        description = "List all tasks in a plan with their status, priority, assignment, and proof."
    )]
    async fn agentstategraph_list_tasks(&self, params: Parameters<ListTasksParams>) -> String {
        let p = params.0;
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

    #[tool(
        description = "Start a task — transition from pending to in_progress. Validates that all blockers are resolved."
    )]
    async fn agentstategraph_start_task(&self, params: Parameters<TaskActionParams>) -> String {
        let p = params.0;
        match self.tasks.start_task(&p.r#ref, &p.plan, &TaskId(p.task_id)) {
            Ok(task) => format!(
                "Task {} started (was: Pending → now: InProgress)",
                task.id.as_str()
            ),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(
        description = "Complete a task with proof. Proof documents what was accomplished: a commit hash, file path, test name, or text description. Auto-completes the plan when the last task finishes."
    )]
    async fn agentstategraph_complete_task(
        &self,
        params: Parameters<CompleteTaskParams>,
    ) -> String {
        let p = params.0;
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

    #[tool(description = "Abandon a task with a reason. Terminal state — cannot be restarted.")]
    async fn agentstategraph_abandon_task(&self, params: Parameters<AbandonTaskParams>) -> String {
        let p = params.0;
        match self
            .tasks
            .abandon_task(&p.r#ref, &p.plan, &TaskId(p.task_id), &p.reason)
        {
            Ok(task) => format!("Task {} abandoned: {}", task.id.as_str(), p.reason),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(
        description = "Assign a task to an agent. The agent can then query for their next task."
    )]
    async fn agentstategraph_assign_task(&self, params: Parameters<AssignTaskParams>) -> String {
        let p = params.0;
        match self
            .tasks
            .assign_task(&p.r#ref, &p.plan, &TaskId(p.task_id), &p.agent)
        {
            Ok(task) => format!("Task {} assigned to {}", task.id.as_str(), p.agent),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(
        description = "Get the next pending task in a plan, optionally filtered by assigned agent. Returns the highest-priority unblocked task."
    )]
    async fn agentstategraph_next_task(&self, params: Parameters<NextTaskParams>) -> String {
        let p = params.0;
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

    // -- Policy tools (POLICY_V1.md §6 + §22.5) --

    #[tool(
        description = "Propose a new policy. Writes an unratified policy node at /policies/<path>. Body is a full Policy JSON (path, situation, situation_selector, allow/deny/require_approval, triggers, required_fields, severity, etc.). Returns path@version."
    )]
    async fn agentstategraph_policy_propose(
        &self,
        params: Parameters<PolicyProposeParams>,
    ) -> String {
        let p = params.0;
        let policy: Policy = match serde_json::from_value(p.policy) {
            Ok(p) => p,
            Err(e) => return format!("Error: invalid Policy JSON: {}", e),
        };
        match self.policies.propose(&p.r#ref, policy) {
            Ok(handle) => format!("Proposed {}", handle),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(
        description = "Ratify an unratified policy proposal. The ratifier string (human or agent id) and free-form reasoning are recorded on the policy node."
    )]
    async fn agentstategraph_policy_ratify(
        &self,
        params: Parameters<PolicyRatifyParams>,
    ) -> String {
        let p = params.0;
        match self
            .policies
            .ratify(&p.r#ref, &p.path, &p.ratifier, &p.reasoning)
        {
            Ok(()) => format!("Ratified {} by {}", p.path, p.ratifier),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(
        description = "Supersede an active policy with a new version. The prior version is moved to /policies/<path>/history/<n>; the new policy is written at the active path with version+1 and supersedes: <old>@<old_version>. Returns new path@version."
    )]
    async fn agentstategraph_policy_supersede(
        &self,
        params: Parameters<PolicySupersedeParams>,
    ) -> String {
        let p = params.0;
        let new_policy: Policy = match serde_json::from_value(p.new_policy) {
            Ok(p) => p,
            Err(e) => return format!("Error: invalid Policy JSON: {}", e),
        };
        match self.policies.supersede(&p.r#ref, &p.old_path, new_policy) {
            Ok(handle) => format!("Superseded → {}", handle),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(
        description = "List policies. Filter by path prefix and status (\"active\", \"proposed\", or \"all\" — default \"active\"). Optional `tenant_filter` (0.7.5 §3b): when set, only policies with tenant_id matching or tenant_id=None (globals) are returned; when omitted, all policies are visible."
    )]
    async fn agentstategraph_policy_list(&self, params: Parameters<PolicyListParams>) -> String {
        let p = params.0;
        let status = p.status.as_deref().unwrap_or("active").to_lowercase();
        let tenant = p.tenant_filter.as_deref();
        let result = match status.as_str() {
            "proposed" => self
                .policies
                .list_scoped(&p.r#ref, p.prefix.as_deref(), tenant)
                .map(|ps| ps.into_iter().filter(|p| !p.is_ratified()).collect()),
            "all" => self
                .policies
                .list_scoped(&p.r#ref, p.prefix.as_deref(), tenant),
            _ => self
                .policies
                .active_scoped(&p.r#ref, p.prefix.as_deref(), tenant),
        };
        match result {
            Ok(policies) => serde_json::to_string_pretty(&policies).unwrap_or_default(),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(
        description = "Read a policy. Returns the active version by default; pass `version` to pin a historical read."
    )]
    async fn agentstategraph_policy_show(&self, params: Parameters<PolicyShowParams>) -> String {
        let p = params.0;
        match self.policies.get(&p.r#ref, &p.path, p.version) {
            Ok(policy) => serde_json::to_string_pretty(&policy).unwrap_or_default(),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(
        description = "Walk the supersedes chain for a policy path. Returns the full version history oldest-first."
    )]
    async fn agentstategraph_policy_history(
        &self,
        params: Parameters<PolicyHistoryParams>,
    ) -> String {
        let p = params.0;
        match self.policies.history(&p.r#ref, &p.path) {
            Ok(chain) => serde_json::to_string_pretty(&chain).unwrap_or_default(),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(
        description = "Authorization evaluation. Given a situation (flat string map), a proposed action, and the agent id, returns a Decision (Allow / Deny / RequireApproval / NoPolicyMatch). NoPolicyMatch is translated per the server's fail-safe config (default: deny); the original kind is surfaced in the response. Optional `tenant_filter` (0.7.5 §3b): when set, only policies with tenant_id matching or tenant_id=None (globals) contribute to the decision."
    )]
    async fn agentstategraph_policy_evaluate(
        &self,
        params: Parameters<PolicyEvaluateParams>,
    ) -> String {
        let p = params.0;
        let situation = Situation(p.situation);
        match self.policies.evaluate_scoped(
            &p.r#ref,
            &situation,
            &p.action,
            &p.agent_id,
            p.tenant_filter.as_deref(),
        ) {
            Ok(decision) => render_decision_with_fail_safe(&decision, &self.policy_fail_safe),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(
        description = "Change-proposal evaluation. Takes a full ChangeProposal (action, agent_id, intent, preferred_option, alternatives, tokens, attached_fields) and returns a Decision with a fallback when RequireApproval. NoPolicyMatch is translated per the server's fail-safe config (default: deny). Optional `tenant_filter` (0.7.5 §3b): when set, only policies with tenant_id matching or tenant_id=None (globals) are consulted."
    )]
    async fn agentstategraph_policy_evaluate_change(
        &self,
        params: Parameters<PolicyEvaluateChangeParams>,
    ) -> String {
        let p = params.0;
        let proposal: ChangeProposal = match serde_json::from_value(p.proposal) {
            Ok(p) => p,
            Err(e) => return format!("Error: invalid ChangeProposal JSON: {}", e),
        };
        match self
            .policies
            .evaluate_change_scoped(&p.r#ref, &proposal, p.tenant_filter.as_deref())
        {
            Ok(decision) => render_decision_with_fail_safe(&decision, &self.policy_fail_safe),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(
        description = "Pre-flight: given a set of change tokens, list every active policy whose triggers would match. Lets agents surface which policies a change will hit before committing to a proposal."
    )]
    async fn agentstategraph_policy_check_tokens(
        &self,
        params: Parameters<PolicyCheckTokensParams>,
    ) -> String {
        let p = params.0;
        match self.policies.active(&p.r#ref, None) {
            Ok(policies) => {
                let token_set: std::collections::HashSet<&str> =
                    p.tokens.iter().map(String::as_str).collect();
                let matches: Vec<serde_json::Value> = policies
                    .iter()
                    .filter(|policy| {
                        policy
                            .triggers
                            .iter()
                            .any(|t| token_set.contains(t.as_str()))
                    })
                    .map(|policy| {
                        let hit: Vec<&String> = policy
                            .triggers
                            .iter()
                            .filter(|t| token_set.contains(t.as_str()))
                            .collect();
                        serde_json::json!({
                            "policy": policy.handle(),
                            "matched_triggers": hit,
                            "severity": policy.severity,
                            "required_fields": policy.required_fields,
                        })
                    })
                    .collect();
                serde_json::to_string_pretty(&matches).unwrap_or_default()
            }
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(
        description = "Sign the active policy at `path` using the server's registered PolicySigner. Canonicalizes the policy (excluding the `signature` field), signs the canonical bytes, and writes the policy back with the signature attached. Returns {\"ok\": true, \"signature\": {...}} on success or {\"error\": \"...\"} when no signer is registered or the policy doesn't exist."
    )]
    async fn agentstategraph_policy_sign(&self, params: Parameters<PolicySignParams>) -> String {
        let p = params.0;
        let Some(signer) = self.signer.as_ref() else {
            return serde_json::json!({ "error": "no signer registered" }).to_string();
        };
        let policy = match self.policies.get(&p.r#ref, &p.path, None) {
            Ok(pol) => pol,
            Err(e) => return serde_json::json!({ "error": e.to_string() }).to_string(),
        };
        let canonical = match canonicalize(&policy) {
            Ok(c) => c,
            Err(e) => {
                return serde_json::json!({ "error": format!("canonicalize: {}", e) }).to_string();
            }
        };
        let (key_id, sig_bytes) = match signer.sign(&canonical) {
            Ok(pair) => pair,
            Err(e) => return serde_json::json!({ "error": format!("sign: {}", e) }).to_string(),
        };
        // `signer_key_id` param is advisory — `Ed25519Signer` returns its
        // configured key_id. We surface the one the signer actually used.
        let _requested = p.signer_key_id;
        let signature = PolicySignature::Ed25519 {
            signer_key_id: key_id,
            signature_hex: hex::encode(&sig_bytes),
        };
        if let Err(e) = self
            .policies
            .set_signature(&p.r#ref, &p.path, signature.clone())
        {
            return serde_json::json!({ "error": e.to_string() }).to_string();
        }
        serde_json::json!({
            "ok": true,
            "signature": signature,
        })
        .to_string()
    }

    #[tool(
        description = "Verify the signature on the active policy at `path` using the server's registered SignatureVerifier. Returns {\"valid\": true} on success, {\"valid\": false, \"reason\": \"...\"} on rejection, or {\"valid\": null, \"reason\": \"no verifier registered\"} when no verifier is wired."
    )]
    async fn agentstategraph_policy_verify(
        &self,
        params: Parameters<PolicyVerifyParams>,
    ) -> String {
        let p = params.0;
        let Some(verifier) = self.verifier.as_ref() else {
            return serde_json::json!({
                "valid": serde_json::Value::Null,
                "reason": "no verifier registered",
            })
            .to_string();
        };
        let policy = match self.policies.get(&p.r#ref, &p.path, None) {
            Ok(pol) => pol,
            Err(e) => return serde_json::json!({ "error": e.to_string() }).to_string(),
        };
        match verifier.verify_policy(&policy) {
            Ok(()) => serde_json::json!({ "valid": true }).to_string(),
            Err(e) => serde_json::json!({
                "valid": false,
                "reason": e.to_string(),
            })
            .to_string(),
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for AgentStateGraphServer {}

// -- Helpers --

fn parse_category(s: &str) -> IntentCategory {
    match s.to_lowercase().as_str() {
        "explore" => IntentCategory::Explore,
        "refine" => IntentCategory::Refine,
        "fix" => IntentCategory::Fix,
        "rollback" => IntentCategory::Rollback,
        "checkpoint" => IntentCategory::Checkpoint,
        "merge" => IntentCategory::Merge,
        // SECURITY (threat model v2, finding C3): the MCP stdio transport
        // has no capability layer comparable to HTTP's `can_migrate`, so a
        // caller here must NOT be able to obtain `IntentCategory::Migrate`
        // by string. Map the literal "migrate" to a Custom category so
        // `/_meta/*` writes are rejected by the substrate's reserved-path
        // guard. Real migrations go through the `migrate` subcommand,
        // which constructs `IntentCategory::Migrate` directly in Rust.
        "migrate" => IntentCategory::Custom("Migrate-requested".into()),
        "plan" => IntentCategory::Plan,
        other => IntentCategory::Custom(other.to_string()),
    }
}

/// Translate `Decision::NoPolicyMatch` per the MCP fail-safe config.
///
/// The engine never rewrites its result — translation happens only at
/// this layer. The returned JSON always surfaces the original
/// `no_policy_match` kind alongside the translated decision so callers
/// can distinguish "authorized by an explicit allow" from "nothing
/// matched; default policy applied."
pub fn render_decision_with_fail_safe(decision: &Decision, fail_safe: &str) -> String {
    match decision {
        Decision::NoPolicyMatch => {
            let translated = if fail_safe == "allow" {
                serde_json::json!({
                    "kind": "allow",
                    "matched_policy": "<fail-safe:allow>",
                    "preconditions": [],
                })
            } else {
                serde_json::json!({
                    "kind": "deny",
                    "matched_policy": "<fail-safe:deny>",
                    "reason": "no policy matched; fail-safe deny applied at MCP layer",
                })
            };
            serde_json::to_string_pretty(&serde_json::json!({
                "original": { "kind": "no_policy_match" },
                "translated": translated,
                "fail_safe": fail_safe,
            }))
            .unwrap_or_else(|_| "null".to_string())
        }
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| "null".to_string()),
    }
}

/// Infer change tokens from the diff between a speculation and its base
/// ref. Implementation of POLICY_V1.md §22.2.2 + implementation plan
/// §4.3. Token rules (heuristics — documented as such):
///
/// - `destructive` — any `RemoveKey` / `RemoveElement` / `RemoveFromSet` op
/// - `schema-change` — any path touched under `/_meta/schema_version`
/// - `ref-rewrite` — any `ChangeType` op (a path's node shape was
///   rewritten; the closest proxy for a ref rename the engine currently
///   emits)
/// - `large` — total count of diff ops > `LARGE_CHANGE_THRESHOLD` (50)
/// - `reindex` — any path under `/index/` or containing a `"reindexed":
///   true` marker (heuristic)
/// - `migration` — any path under `/_meta/migrations/`
pub fn infer_change_tokens(
    repo: &Repository,
    handle: SpecHandle,
) -> Result<Vec<String>, agentstategraph::RepoError> {
    let comparison = repo.compare_speculations(&[handle])?;
    let diff = comparison
        .entries
        .into_iter()
        .next()
        .map(|e| e.diff_from_base)
        .unwrap_or_default();
    Ok(infer_tokens_from_diff(&diff))
}

/// Pure token-inference helper, extracted for testability. See
/// `infer_change_tokens` for the rule set.
pub fn infer_tokens_from_diff(diff: &[DiffOp]) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    let push = |t: &str, tokens: &mut Vec<String>| {
        if !tokens.iter().any(|x| x == t) {
            tokens.push(t.to_string());
        }
    };

    let mut destructive = false;
    let mut ref_rewrite = false;
    let mut schema_change = false;
    let mut migration = false;
    let mut reindex = false;

    for op in diff {
        let path_ref = diff_op_path(op);
        match op {
            DiffOp::RemoveKey { .. }
            | DiffOp::RemoveElement { .. }
            | DiffOp::RemoveFromSet { .. } => {
                destructive = true;
            }
            DiffOp::ChangeType { .. } => {
                ref_rewrite = true;
            }
            _ => {}
        }
        let path = path_ref.unwrap_or("");
        if path.starts_with("/_meta/schema_version") || path == "/_meta/schema_version" {
            schema_change = true;
        }
        if path.starts_with("/_meta/migrations/") || path == "/_meta/migrations" {
            migration = true;
        }
        if path.starts_with("/index/") || path == "/index" {
            reindex = true;
        }
        // Heuristic: a "reindexed": true marker anywhere in the diff.
        if let DiffOp::AddKey { key, value, .. } = op
            && key == "reindexed"
            && matches!(value, agentstategraph_core::DiffValue::Bool(true))
        {
            reindex = true;
        }
    }

    if destructive {
        push("destructive", &mut tokens);
    }
    if schema_change {
        push("schema-change", &mut tokens);
    }
    if ref_rewrite {
        push("ref-rewrite", &mut tokens);
    }
    if reindex {
        push("reindex", &mut tokens);
    }
    if migration {
        push("migration", &mut tokens);
    }
    if diff.len() > LARGE_CHANGE_THRESHOLD {
        push("large", &mut tokens);
    }
    tokens
}

fn diff_op_path(op: &DiffOp) -> Option<&str> {
    match op {
        DiffOp::SetValue { path, .. }
        | DiffOp::AddKey { path, .. }
        | DiffOp::RemoveKey { path, .. }
        | DiffOp::AddElement { path, .. }
        | DiffOp::RemoveElement { path, .. }
        | DiffOp::AddToSet { path, .. }
        | DiffOp::RemoveFromSet { path, .. }
        | DiffOp::ChangeType { path, .. } => Some(path.as_str()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use agentstategraph::{CommitOptions, RepoError, Repository};
    use agentstategraph_storage::MemoryStorage;

    #[test]
    fn parse_category_migrate_is_custom_not_migrate() {
        // Threat model v2, C3: the MCP stdio boundary must NOT let a string
        // caller obtain IntentCategory::Migrate — that would bypass the
        // reserved-path guard on `/_meta/*`.
        let cat = parse_category("migrate");
        assert!(
            !matches!(cat, IntentCategory::Migrate),
            "parse_category(\"migrate\") must not return Migrate on the MCP stdio boundary"
        );
        assert!(matches!(cat, IntentCategory::Custom(_)));
    }

    #[test]
    fn mcp_set_on_meta_with_migrate_string_is_rejected() {
        // A caller passing intent_category="migrate" must not be able to
        // write under /_meta/* — the substrate should reject it with
        // ReservedPath.
        let repo = Repository::new(Box::new(MemoryStorage::new()));
        repo.init().expect("init repo");
        let opts = CommitOptions::new(
            "test-agent",
            parse_category("migrate"),
            "attempted meta write",
        );
        let err = repo
            .set_json(
                "main",
                "/_meta/schema_version",
                &serde_json::json!("1"),
                opts,
            )
            .expect_err("/_meta/* write with string-derived Migrate must be rejected");
        assert!(
            matches!(err, RepoError::ReservedPath(_)),
            "expected ReservedPath, got {:?}",
            err
        );
    }

    #[test]
    fn direct_migrate_construction_still_works() {
        // The `agentstategraph-mcp migrate` subcommand constructs
        // IntentCategory::Migrate directly (not via parse_category) — that
        // path MUST continue to work, otherwise we've broken legitimate
        // migrations.
        let repo = Repository::new(Box::new(MemoryStorage::new()));
        repo.init().expect("init repo");
        let opts = CommitOptions::new("migrator", IntentCategory::Migrate, "legitimate migration");
        repo.set_json(
            "main",
            "/_meta/schema_version",
            &serde_json::json!("1"),
            opts,
        )
        .expect("direct IntentCategory::Migrate must still be allowed");
    }
}
