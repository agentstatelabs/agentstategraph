//! AgentStateGraph MCP Server — exposes AgentStateGraph operations as MCP tools.

mod policy;
mod reminders;
mod taint;
mod tasks;

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
    ChangeProposal, Decision, ExternalEvaluator, ExternalEvaluatorRegistry,
    PolicyStore, SignatureVerifier,
};
use agentstategraph_policy_sign::PolicySigner;
use agentstategraph_reminders::ReminderManager;
use agentstategraph_tasks::TaskStore;

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
    /// Optional external-evaluator registry wired through to the internal
    /// `PolicyStore` (0.7.5 §4c). When set, policies carrying an
    /// `external_evaluator` whose kind matches a registered runner are
    /// dispatched through it. `None` means no external runners — every
    /// policy goes through the local evaluator.
    external_evaluators: Option<Arc<ExternalEvaluatorRegistry>>,
    reminders: Arc<ReminderManager>,
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
pub struct ArchiveEpochParams {
    /// ID of the sealed epoch to archive.
    pub id: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct ExportEpochParams {
    /// ID of the sealed or archived epoch to export.
    pub id: String,
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
pub struct PolicyEvaluateChangeWithTaintsParams {
    #[serde(default = "default_ref")]
    pub r#ref: String,
    pub proposal: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_filter: Option<String>,
    /// Paths that the proposal would affect. Each is passed through
    /// `check_taint` and the aggregated status is returned alongside
    /// the policy decision.
    #[serde(default)]
    pub affected_paths: Vec<String>,
    /// Agent id used for the taint-check authorization pass.
    /// Falls back to `proposal.agent_id` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// Commit confidence for the review-effect gate. Default 1.0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
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

// -- Taint / Quarantine / Watch parameter types (0.7.75 §6) --

#[derive(Deserialize, JsonSchema)]
pub struct TaintApplyParams {
    #[serde(default = "default_ref")]
    pub r#ref: String,
    pub path: String,
    pub name: String,
    /// "warn" | "block" | "review" | "isolate"
    pub effect: String,
    pub reason: String,
    /// "low" | "medium" | "high" | "critical". Default: "medium".
    #[serde(default)]
    pub severity: Option<String>,
    /// RFC3339; null = permanent.
    #[serde(default)]
    pub expires: Option<String>,
    /// Default: true.
    #[serde(default)]
    pub propagate: Option<bool>,
    pub agent_id: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct TaintRemoveParams {
    #[serde(default = "default_ref")]
    pub r#ref: String,
    pub path: String,
    pub name: String,
    pub reason: String,
    #[serde(default)]
    pub proof: Option<String>,
    pub agent_id: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct QuarantineApplyParams {
    #[serde(default = "default_ref")]
    pub r#ref: String,
    pub path: String,
    pub name: String,
    pub reason: String,
    /// Default: "high".
    #[serde(default)]
    pub severity: Option<String>,
    pub authorized_agents: Vec<String>,
    #[serde(default)]
    pub expires: Option<String>,
    #[serde(default)]
    pub propagate: Option<bool>,
    pub agent_id: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct WatchApplyParams {
    #[serde(default = "default_ref")]
    pub r#ref: String,
    pub path: String,
    pub name: String,
    pub reason: String,
    #[serde(default)]
    pub metric: Option<String>,
    #[serde(default)]
    pub threshold: Option<f64>,
    /// "above" | "below". Default: "above".
    #[serde(default)]
    pub direction: Option<String>,
    #[serde(default)]
    pub check_interval_secs: Option<u64>,
    #[serde(default)]
    pub expires: Option<String>,
    #[serde(default)]
    pub severity: Option<String>,
    #[serde(default)]
    pub propagate: Option<bool>,
    pub agent_id: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct WatchRemoveParams {
    #[serde(default = "default_ref")]
    pub r#ref: String,
    pub path: String,
    pub name: String,
    #[serde(default)]
    pub reason: Option<String>,
    pub agent_id: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct ListTaintsParams {
    #[serde(default)]
    pub path: Option<String>,
    /// "taint" | "quarantine" | "watch". Default: all.
    #[serde(default)]
    pub kind: Option<String>,
    /// "warn" | "block" | "review" | "isolate". Informational filter
    /// applied client-side on the result list.
    #[serde(default)]
    pub effect: Option<String>,
    #[serde(default)]
    pub include_expired: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct CheckTaintParams {
    pub path: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub confidence: Option<f64>,
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

// -- Reminder parameter types --

/// Input ref for a soft reminder reference.
#[derive(Deserialize, JsonSchema)]
pub struct ReminderRefInput {
    /// Kind: branch, memory, plan, task, state_path, or any scheme for external.
    pub kind: String,
    /// Stable identifier (branch name, task ID, path, URL…).
    pub id: String,
    /// Optional human-readable label captured at creation time.
    pub label: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ReminderCreateParams {
    /// Short descriptive title.
    pub title: String,
    /// Full instructions for the agent at execution time.
    pub instructions: String,
    /// RFC3339 datetime when the reminder should fire.
    pub due_at: String,
    /// Who is creating this reminder (agent ID or "user").
    pub created_by: String,
    /// Priority: critical, high, medium (default), low, minimal.
    pub priority: Option<String>,
    /// Schedule: "once" (default), "interval:<secs>", "daily:HH:MM", "weekly:Weekday:HH:MM".
    pub schedule: Option<String>,
    /// If false, agent must call reminder_approve before executing. Default: true.
    pub autonomous: Option<bool>,
    /// Optional shell/tool commands to run at execution time.
    pub commands: Option<Vec<String>>,
    /// Soft references to branches, memories, plans, tasks, or external resources.
    pub refs: Option<Vec<ReminderRefInput>>,
    /// Queryable tags.
    pub tags: Option<Vec<String>>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ReminderListParams {
    /// Filter by status: pending, due, awaiting_permission, in_progress, completed, snoozed, cancelled.
    pub status: Option<String>,
    /// Filter by creator agent ID.
    pub created_by: Option<String>,
    /// Return only reminders that reference this object ID.
    pub ref_id: Option<String>,
    /// Tags that must all be present.
    pub tags: Option<Vec<String>>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ReminderSnoozeParams {
    /// Reminder ID.
    pub id: String,
    /// RFC3339 datetime to wake the reminder.
    pub until: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct ReminderApproveParams {
    /// Reminder ID.
    pub id: String,
    /// Who is approving (e.g., "human/alice").
    pub approved_by: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct ReminderCancelParams {
    /// Reminder ID.
    pub id: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct ReminderRecordParams {
    /// Reminder ID.
    pub id: String,
    /// Agent that executed the reminder.
    pub agent_id: String,
    /// Execution result: success, failed, deferred, snoozed, cancelled.
    pub result: String,
    /// Who approved (if non-autonomous).
    pub approved_by: Option<String>,
    /// Free-form notes about what happened.
    pub notes: Option<Vec<String>>,
    /// Task ID created during this execution, if any.
    pub task_id: Option<String>,
}

// -- Tool implementations --

#[tool_router]
impl AgentStateGraphServer {
    pub fn new(repo: Arc<Repository>) -> Self {
        use agentstategraph_reminders::MemoryReminderStore;
        let tasks = Arc::new(TaskStore::new(repo.clone(), "/plans", "mcp-agent"));
        let policies = Arc::new(PolicyStore::new(repo.clone(), "/policies", "mcp-agent"));
        let reminders = Arc::new(ReminderManager::new(Arc::new(MemoryReminderStore::new())));
        Self {
            repo,
            tasks,
            policies,
            reminders,
            policy_fail_safe: "deny".to_string(),
            signer: None,
            verifier: None,
            require_signed_policies: false,
            external_evaluators: None,
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
        self.verifier = Some(verifier);
        self.rebuild_policy_store();
        self
    }

    /// Toggle `require_signed_policies` on the internal `PolicyStore`.
    /// Only meaningful when a verifier is also registered; with no
    /// verifier the store is in pass-through mode.
    pub fn with_require_signed_policies(mut self, require: bool) -> Self {
        self.require_signed_policies = require;
        self.rebuild_policy_store();
        self
    }

    /// Register an externally-constructed runner (0.7.5 §4c). May be
    /// called multiple times to register multiple runners; the last
    /// registration for a given `kind()` wins. The internal
    /// `PolicyStore` is rebuilt with a registry that contains every
    /// runner registered so far plus the current verifier /
    /// `require_signed_policies` state.
    pub fn with_external_evaluator(mut self, eval: Arc<dyn ExternalEvaluator>) -> Self {
        let mut registry = match self.external_evaluators.take() {
            Some(arc) => Arc::try_unwrap(arc).unwrap_or_else(|shared| {
                // Another holder exists (shouldn't happen during the
                // builder chain, but be defensive): clone its entries
                // into a fresh registry.
                let mut copy = ExternalEvaluatorRegistry::new();
                for kind in shared.kinds() {
                    if let Some(runner) = shared.get(kind) {
                        copy.register(runner.clone());
                    }
                }
                copy
            }),
            None => ExternalEvaluatorRegistry::new(),
        };
        registry.register(eval);
        self.external_evaluators = Some(Arc::new(registry));
        self.rebuild_policy_store();
        self
    }

    /// Convenience: construct + register the stock WASM runner
    /// (`agentstategraph-policy-wasm`). Equivalent to
    /// `self.with_external_evaluator(Arc::new(WasmEvaluator::default()))`.
    /// Gated on the `policy-wasm` feature.
    #[cfg(feature = "policy-wasm")]
    pub fn with_wasm_evaluator(self) -> Self {
        self.with_external_evaluator(Arc::new(
            agentstategraph_policy_wasm::WasmEvaluator::default(),
        ))
    }

    /// Convenience: construct + register the stock Rego runner
    /// (`agentstategraph-policy-rego`). The runner shells out to an
    /// `opa` binary on `$PATH` at evaluation time. Gated on the
    /// `policy-rego` feature.
    #[cfg(feature = "policy-rego")]
    pub fn with_rego_evaluator(self) -> Self {
        self.with_external_evaluator(Arc::new(
            agentstategraph_policy_rego::RegoEvaluator::default(),
        ))
    }

    /// Convenience: construct + register the stock Cedar runner
    /// (`agentstategraph-policy-cedar`). The runner shells out to a
    /// `cedar` binary on `$PATH` at evaluation time. Gated on the
    /// `policy-cedar` feature.
    #[cfg(feature = "policy-cedar")]
    pub fn with_cedar_evaluator(self) -> Self {
        self.with_external_evaluator(Arc::new(
            agentstategraph_policy_cedar::CedarEvaluator::default(),
        ))
    }

    /// Rebuild `self.policies` to reflect the current verifier +
    /// `require_signed_policies` + external-evaluator state. Called by
    /// every builder that mutates one of those three inputs so the
    /// store always observes the full configuration regardless of the
    /// order the builders were chained in.
    fn rebuild_policy_store(&mut self) {
        let mut store = PolicyStore::new(self.repo.clone(), "/policies", "mcp-agent");
        if let Some(verifier) = self.verifier.clone() {
            store = store
                .with_verifier(verifier)
                .with_require_signed(self.require_signed_policies);
        }
        if let Some(registry) = self.external_evaluators.clone() {
            store = store.with_external_evaluators(registry);
        }
        self.policies = Arc::new(store);
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

    /// Read-only accessor for tests and taint-surface callers that
    /// need to exercise the Repository methods directly.
    pub fn repo(&self) -> &Repository {
        &self.repo
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
        description = "Archive a sealed epoch, transitioning it from Sealed to Archived. An archived epoch is considered in cold storage: it remains queryable but signals that its state is finalized and no longer actively referenced. Only Sealed epochs can be archived."
    )]
    async fn agentstategraph_archive_epoch(
        &self,
        params: Parameters<ArchiveEpochParams>,
    ) -> String {
        let p = params.0;
        match self.repo.archive_epoch(&p.id) {
            Ok(()) => format!("Epoch '{}' archived", p.id),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(
        description = "Export a sealed or archived epoch as a self-contained JSON audit bundle. The bundle contains the epoch metadata and the full Commit records for every commit associated with the epoch, making it independently verifiable without access to the live store."
    )]
    async fn agentstategraph_export_epoch(
        &self,
        params: Parameters<ExportEpochParams>,
    ) -> String {
        let p = params.0;
        match self.repo.export_epoch(&p.id) {
            Ok(bundle) => bundle.to_string(),
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
        self.impl_create_plan(params.0)
    }

    #[tool(
        description = "List all plans, optionally filtered by status (Active, Completed, Archived)."
    )]
    async fn agentstategraph_list_plans(&self, params: Parameters<ListPlansParams>) -> String {
        self.impl_list_plans(params.0)
    }

    #[tool(description = "Get a plan's details including status and task summary.")]
    async fn agentstategraph_get_plan(&self, params: Parameters<GetPlanParams>) -> String {
        self.impl_get_plan(params.0)
    }

    #[tool(
        description = "Add a task to a plan. Tasks have a strict state machine: pending → in_progress → done. Supports priority, blockers, parent tasks, and agent assignment."
    )]
    async fn agentstategraph_add_task(&self, params: Parameters<AddTaskParams>) -> String {
        self.impl_add_task(params.0)
    }

    #[tool(
        description = "List all tasks in a plan with their status, priority, assignment, and proof."
    )]
    async fn agentstategraph_list_tasks(&self, params: Parameters<ListTasksParams>) -> String {
        self.impl_list_tasks(params.0)
    }

    #[tool(
        description = "Start a task — transition from pending to in_progress. Validates that all blockers are resolved."
    )]
    async fn agentstategraph_start_task(&self, params: Parameters<TaskActionParams>) -> String {
        self.impl_start_task(params.0)
    }

    #[tool(
        description = "Complete a task with proof. Proof documents what was accomplished: a commit hash, file path, test name, or text description. Auto-completes the plan when the last task finishes."
    )]
    async fn agentstategraph_complete_task(
        &self,
        params: Parameters<CompleteTaskParams>,
    ) -> String {
        self.impl_complete_task(params.0)
    }

    #[tool(description = "Abandon a task with a reason. Terminal state — cannot be restarted.")]
    async fn agentstategraph_abandon_task(&self, params: Parameters<AbandonTaskParams>) -> String {
        self.impl_abandon_task(params.0)
    }

    #[tool(
        description = "Assign a task to an agent. The agent can then query for their next task."
    )]
    async fn agentstategraph_assign_task(&self, params: Parameters<AssignTaskParams>) -> String {
        self.impl_assign_task(params.0)
    }

    #[tool(
        description = "Get the next pending task in a plan, optionally filtered by assigned agent. Returns the highest-priority unblocked task."
    )]
    async fn agentstategraph_next_task(&self, params: Parameters<NextTaskParams>) -> String {
        self.impl_next_task(params.0)
    }

    // -- Policy tools (POLICY_V1.md §6 + §22.5) --

    #[tool(
        description = "Propose a new policy. Writes an unratified policy node at /policies/<path>. Body is a full Policy JSON (path, situation, situation_selector, allow/deny/require_approval, triggers, required_fields, severity, etc.). Returns path@version."
    )]
    async fn agentstategraph_policy_propose(
        &self,
        params: Parameters<PolicyProposeParams>,
    ) -> String {
        self.impl_policy_propose(params.0)
    }

    #[tool(
        description = "Ratify an unratified policy proposal. The ratifier string (human or agent id) and free-form reasoning are recorded on the policy node."
    )]
    async fn agentstategraph_policy_ratify(
        &self,
        params: Parameters<PolicyRatifyParams>,
    ) -> String {
        self.impl_policy_ratify(params.0)
    }

    #[tool(
        description = "Supersede an active policy with a new version. The prior version is moved to /policies/<path>/history/<n>; the new policy is written at the active path with version+1 and supersedes: <old>@<old_version>. Returns new path@version."
    )]
    async fn agentstategraph_policy_supersede(
        &self,
        params: Parameters<PolicySupersedeParams>,
    ) -> String {
        self.impl_policy_supersede(params.0)
    }

    #[tool(
        description = "List policies. Filter by path prefix and status (\"active\", \"proposed\", or \"all\" — default \"active\"). Optional `tenant_filter` (0.7.5 §3b): when set, only policies with tenant_id matching or tenant_id=None (globals) are returned; when omitted, all policies are visible."
    )]
    async fn agentstategraph_policy_list(&self, params: Parameters<PolicyListParams>) -> String {
        self.impl_policy_list(params.0)
    }

    #[tool(
        description = "Read a policy. Returns the active version by default; pass `version` to pin a historical read."
    )]
    async fn agentstategraph_policy_show(&self, params: Parameters<PolicyShowParams>) -> String {
        self.impl_policy_show(params.0)
    }

    #[tool(
        description = "Walk the supersedes chain for a policy path. Returns the full version history oldest-first."
    )]
    async fn agentstategraph_policy_history(
        &self,
        params: Parameters<PolicyHistoryParams>,
    ) -> String {
        self.impl_policy_history(params.0)
    }

    #[tool(
        description = "Authorization evaluation. Given a situation (flat string map), a proposed action, and the agent id, returns a Decision (Allow / Deny / RequireApproval / NoPolicyMatch). NoPolicyMatch is translated per the server's fail-safe config (default: deny); the original kind is surfaced in the response. Optional `tenant_filter` (0.7.5 §3b): when set, only policies with tenant_id matching or tenant_id=None (globals) contribute to the decision."
    )]
    async fn agentstategraph_policy_evaluate(
        &self,
        params: Parameters<PolicyEvaluateParams>,
    ) -> String {
        self.impl_policy_evaluate(params.0)
    }

    #[tool(
        description = "Change-proposal evaluation. Takes a full ChangeProposal (action, agent_id, intent, preferred_option, alternatives, tokens, attached_fields) and returns a Decision with a fallback when RequireApproval. NoPolicyMatch is translated per the server's fail-safe config (default: deny). Optional `tenant_filter` (0.7.5 §3b): when set, only policies with tenant_id matching or tenant_id=None (globals) are consulted."
    )]
    async fn agentstategraph_policy_evaluate_change(
        &self,
        params: Parameters<PolicyEvaluateChangeParams>,
    ) -> String {
        self.impl_policy_evaluate_change(params.0)
    }

    #[tool(
        description = "Pre-flight: given a set of change tokens, list every active policy whose triggers would match. Lets agents surface which policies a change will hit before committing to a proposal."
    )]
    async fn agentstategraph_policy_check_tokens(
        &self,
        params: Parameters<PolicyCheckTokensParams>,
    ) -> String {
        self.impl_policy_check_tokens(params.0)
    }

    #[tool(
        description = "Sign the active policy at `path` using the server's registered PolicySigner. Canonicalizes the policy (excluding the `signature` field), signs the canonical bytes, and writes the policy back with the signature attached. Returns {\"ok\": true, \"signature\": {...}} on success or {\"error\": \"...\"} when no signer is registered or the policy doesn't exist."
    )]
    async fn agentstategraph_policy_sign(&self, params: Parameters<PolicySignParams>) -> String {
        self.impl_policy_sign(params.0)
    }

    #[tool(
        description = "Verify the signature on the active policy at `path` using the server's registered SignatureVerifier. Returns {\"valid\": true} on success, {\"valid\": false, \"reason\": \"...\"} on rejection, or {\"valid\": null, \"reason\": \"no verifier registered\"} when no verifier is wired."
    )]
    async fn agentstategraph_policy_verify(
        &self,
        params: Parameters<PolicyVerifyParams>,
    ) -> String {
        self.impl_policy_verify(params.0)
    }

    // -- Taint / Quarantine / Watch tools (0.7.75 §6) --

    #[tool(
        description = "Apply a taint to `path` with an effect that changes how agents interact with it. Effects: 'warn' (advisory), 'block' (rejects writes), 'review' (requires confidence >= 0.9), 'isolate' (excludes from query/search)."
    )]
    async fn agentstategraph_taint(&self, params: Parameters<TaintApplyParams>) -> String {
        self.impl_taint(params.0)
    }

    #[tool(
        description = "Remove a taint by name from `path`. Requires a reason; optional proof (commit id) for audit."
    )]
    async fn agentstategraph_untaint(&self, params: Parameters<TaintRemoveParams>) -> String {
        self.impl_untaint(params.0)
    }

    #[tool(
        description = "Quarantine `path` — restricts reads and writes to the supplied `authorized_agents` list. Stronger than taint; all rejected access attempts are logged as commits."
    )]
    async fn agentstategraph_quarantine(
        &self,
        params: Parameters<QuarantineApplyParams>,
    ) -> String {
        self.impl_quarantine(params.0)
    }

    #[tool(description = "Release a quarantine. Caller should supply evidence the issue is resolved via the `proof` field.")]
    async fn agentstategraph_unquarantine(&self, params: Parameters<TaintRemoveParams>) -> String {
        self.impl_unquarantine(params.0)
    }

    #[tool(
        description = "Apply an advisory watch to `path`. Lighter than taint — purely advisory, does not restrict access. Watches with a numeric `threshold` auto-escalate to a Warn-effect taint when a subsequent set_json crosses the threshold."
    )]
    async fn agentstategraph_watch(&self, params: Parameters<WatchApplyParams>) -> String {
        self.impl_watch(params.0)
    }

    #[tool(description = "Remove a watch by name.")]
    async fn agentstategraph_unwatch(&self, params: Parameters<WatchRemoveParams>) -> String {
        self.impl_unwatch(params.0)
    }

    #[tool(
        description = "List active taints / quarantines / watches. Optional filters: `path` prefix, `kind` (taint|quarantine|watch), `effect`, `include_expired`."
    )]
    async fn agentstategraph_list_taints(&self, params: Parameters<ListTaintsParams>) -> String {
        self.impl_list_taints(params.0)
    }

    #[tool(
        description = "Check the full taint status for `path`, including ancestor taints. Returns whether a write is allowed for the given agent at the given confidence, plus aggregated taint / quarantine / watch lists."
    )]
    async fn agentstategraph_check_taint(&self, params: Parameters<CheckTaintParams>) -> String {
        self.impl_check_taint(params.0)
    }

    #[tool(
        description = "Policy × Taint composition (0.7.75 §8). Evaluates a ChangeProposal against the policy store AND checks each `affected_paths` entry for taints / quarantines. Returns `{decision: {...}, taint_status: [...], can_proceed: bool}` where `can_proceed` is the conjunction of `decision.kind != deny` and all taint checks' `can_write`."
    )]
    async fn agentstategraph_policy_evaluate_change_with_taints(
        &self,
        params: Parameters<PolicyEvaluateChangeWithTaintsParams>,
    ) -> String {
        self.impl_policy_evaluate_change_with_taints(params.0)
    }

    // -- Reminder tools --

    #[tool(
        description = "Create a new reminder. Reminders are pull-based: agents call reminder_remind_me at checkpoints to retrieve due items. Use `schedule` for repeating reminders (once/interval:<secs>/daily:HH:MM/weekly:Weekday:HH:MM). Set `autonomous: false` to require user approval before execution."
    )]
    async fn agentstategraph_reminder_create(
        &self,
        params: Parameters<ReminderCreateParams>,
    ) -> String {
        self.impl_reminder_create(params.0)
    }

    #[tool(
        description = "List reminders with optional filters. Filter by status (pending/due/awaiting_permission/in_progress/completed/snoozed/cancelled), creator, ref_id (returns reminders referencing that object), or tags."
    )]
    async fn agentstategraph_reminder_list(
        &self,
        params: Parameters<ReminderListParams>,
    ) -> String {
        self.impl_reminder_list(params.0)
    }

    #[tool(
        description = "Get all currently due reminders ordered by priority. Automatically promotes past-due pending reminders and wakes expired snoozed reminders. Call this at the start of each session or after completing major tasks."
    )]
    async fn agentstategraph_reminder_remind_me(&self) -> String {
        self.impl_reminder_remind_me()
    }

    #[tool(
        description = "Snooze a reminder until a later time (RFC3339). The reminder will re-appear in remind_me after that time."
    )]
    async fn agentstategraph_reminder_snooze(
        &self,
        params: Parameters<ReminderSnoozeParams>,
    ) -> String {
        self.impl_reminder_snooze(params.0)
    }

    #[tool(
        description = "Approve a non-autonomous reminder for execution. Required for reminders created with autonomous=false. Records who approved."
    )]
    async fn agentstategraph_reminder_approve(
        &self,
        params: Parameters<ReminderApproveParams>,
    ) -> String {
        self.impl_reminder_approve(params.0)
    }

    #[tool(
        description = "Cancel a reminder permanently. Use this when a reminder is no longer relevant (e.g., the task it referenced is already done)."
    )]
    async fn agentstategraph_reminder_cancel(
        &self,
        params: Parameters<ReminderCancelParams>,
    ) -> String {
        self.impl_reminder_cancel(params.0)
    }

    #[tool(
        description = "Record the result of executing a reminder. Result must be one of: success, failed, deferred, snoozed, cancelled. On success, repeating reminders are automatically rescheduled. Optionally attach the task_id created for this execution."
    )]
    async fn agentstategraph_reminder_record_execution(
        &self,
        params: Parameters<ReminderRecordParams>,
    ) -> String {
        self.impl_reminder_record(params.0)
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

pub fn parse_taint_effect(s: &str) -> Option<agentstategraph_taint::TaintEffect> {
    match s.to_lowercase().as_str() {
        "warn" => Some(agentstategraph_taint::TaintEffect::Warn),
        "block" => Some(agentstategraph_taint::TaintEffect::Block),
        "review" => Some(agentstategraph_taint::TaintEffect::Review),
        "isolate" => Some(agentstategraph_taint::TaintEffect::Isolate),
        "advisory" => Some(agentstategraph_taint::TaintEffect::Advisory),
        _ => None,
    }
}

pub fn parse_taint_severity(s: Option<&str>) -> agentstategraph_taint::TaintSeverity {
    match s.unwrap_or("medium").to_lowercase().as_str() {
        "low" => agentstategraph_taint::TaintSeverity::Low,
        "high" => agentstategraph_taint::TaintSeverity::High,
        "critical" => agentstategraph_taint::TaintSeverity::Critical,
        _ => agentstategraph_taint::TaintSeverity::Medium,
    }
}

pub fn parse_optional_rfc3339(s: Option<&str>) -> Option<chrono::DateTime<chrono::Utc>> {
    s.and_then(|raw| {
        chrono::DateTime::parse_from_rfc3339(raw)
            .ok()
            .map(|dt| dt.with_timezone(&chrono::Utc))
    })
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
