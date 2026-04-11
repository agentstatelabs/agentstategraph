//! HTTP REST API for AgentStateGraph.
//!
//! Exposes the same operations as the MCP tools over HTTP.
//! Start with: agentstategraph-mcp --http --port 3001

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use serde::Deserialize;
use tower_http::cors::{Any, CorsLayer};

use agentstategraph::{CommitOptions, Repository};
use agentstategraph_core::IntentCategory;

pub type AppState = Arc<Repository>;

pub fn router(repo: Arc<Repository>) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        // State operations
        .route("/api/state/{ref_name}", get(get_state))
        .route("/api/state/{ref_name}/paths", get(list_paths))
        .route("/api/state/{ref_name}/search", get(search_values))
        .route("/api/state/{ref_name}/set", post(set_value))
        .route("/api/state/{ref_name}/delete", post(delete_value))
        // History
        .route("/api/log/{ref_name}", get(get_log))
        .route("/api/blame/{ref_name}", get(blame))
        .route("/api/diff", get(diff))
        .route("/api/query/{ref_name}", post(query_commits))
        .route("/api/graph/{ref_name}", get(commit_graph))
        // Branches
        .route("/api/branches", get(list_branches))
        .route("/api/branches", post(create_branch))
        .route("/api/merge", post(merge_branches))
        // Epochs
        .route("/api/epochs", get(list_epochs))
        .route("/api/epochs", post(create_epoch))
        .route("/api/epochs/seal", post(seal_epoch))
        // Stats & meta
        .route("/api/stats/{ref_name}", get(stats))
        .route("/api/intents/{ref_name}", get(intent_tree))
        // Health
        .route("/api/health", get(health))
        .with_state(repo)
        .layer(cors)
}

// ─── Health ─────────────────────────────────────────────────

async fn health(State(repo): State<AppState>) -> Json<serde_json::Value> {
    let branches = repo.list_branches(None).unwrap_or_default();
    Json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "branches": branches.len(),
    }))
}

// ─── State operations ───────────────────────────────────────

#[derive(Deserialize)]
struct PathQuery {
    path: Option<String>,
    prefix: Option<String>,
    max_depth: Option<usize>,
    query: Option<String>,
    max_results: Option<usize>,
}

async fn get_state(
    State(repo): State<AppState>,
    Path(ref_name): Path<String>,
    Query(q): Query<PathQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let path = q.path.as_deref().unwrap_or("/");
    let value = repo.get_json(&ref_name, path)?;
    Ok(Json(value))
}

async fn list_paths(
    State(repo): State<AppState>,
    Path(ref_name): Path<String>,
    Query(q): Query<PathQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let prefix = q.prefix.as_deref().unwrap_or("/");
    let paths = repo.list_paths(&ref_name, prefix, q.max_depth)?;
    Ok(Json(serde_json::json!({ "count": paths.len(), "paths": paths })))
}

async fn search_values(
    State(repo): State<AppState>,
    Path(ref_name): Path<String>,
    Query(q): Query<PathQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let query = q.query.as_deref().unwrap_or("");
    if query.is_empty() {
        return Ok(Json(serde_json::json!({ "error": "query parameter required" })));
    }
    let results = repo.search_values(&ref_name, query, q.max_results)?;
    let entries: Vec<serde_json::Value> = results
        .iter()
        .map(|(path, value)| serde_json::json!({ "path": path, "value": value }))
        .collect();
    Ok(Json(serde_json::json!({ "count": entries.len(), "results": entries })))
}

#[derive(Deserialize)]
struct SetRequest {
    path: String,
    value: serde_json::Value,
    intent_category: String,
    intent_description: String,
    reasoning: Option<String>,
    confidence: Option<f64>,
    agent: Option<String>,
}

async fn set_value(
    State(repo): State<AppState>,
    Path(ref_name): Path<String>,
    Json(req): Json<SetRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let category = parse_category(&req.intent_category);
    let agent = req.agent.unwrap_or_else(|| "http".to_string());
    let mut opts = CommitOptions::new(agent, category, &req.intent_description);
    if let Some(r) = req.reasoning {
        opts = opts.with_reasoning(r);
    }
    if let Some(c) = req.confidence {
        opts = opts.with_confidence(c);
    }
    let commit_id = repo.set_json(&ref_name, &req.path, &req.value, opts)?;
    Ok(Json(serde_json::json!({ "commit_id": commit_id.to_string() })))
}

#[derive(Deserialize)]
struct DeleteRequest {
    path: String,
    intent_category: String,
    intent_description: String,
}

async fn delete_value(
    State(repo): State<AppState>,
    Path(ref_name): Path<String>,
    Json(req): Json<DeleteRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let category = parse_category(&req.intent_category);
    let opts = CommitOptions::new("http", category, &req.intent_description);
    let commit_id = repo.delete(&ref_name, &req.path, opts)?;
    Ok(Json(serde_json::json!({ "commit_id": commit_id.to_string() })))
}

// ─── History ────────────────────────────────────────────────

#[derive(Deserialize)]
struct LogQuery {
    limit: Option<usize>,
}

async fn get_log(
    State(repo): State<AppState>,
    Path(ref_name): Path<String>,
    Query(q): Query<LogQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let limit = q.limit.unwrap_or(20);
    let commits = repo.log(&ref_name, limit)?;
    let entries: Vec<serde_json::Value> = commits
        .iter()
        .map(|c| {
            serde_json::json!({
                "id": c.id.short(),
                "full_id": c.id.to_string(),
                "agent": c.agent_id,
                "intent": {
                    "category": format!("{:?}", c.intent.category),
                    "description": c.intent.description,
                    "tags": c.intent.tags,
                },
                "reasoning": c.reasoning,
                "confidence": c.confidence,
                "parents": c.parents.iter().map(|p| p.short()).collect::<Vec<_>>(),
                "timestamp": c.timestamp.to_rfc3339(),
            })
        })
        .collect();
    Ok(Json(serde_json::json!(entries)))
}

#[derive(Deserialize)]
struct BlameQuery {
    path: String,
}

async fn blame(
    State(repo): State<AppState>,
    Path(ref_name): Path<String>,
    Query(q): Query<BlameQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let entry = repo.blame(&ref_name, &q.path)?;
    Ok(Json(serde_json::to_value(&entry).unwrap_or_default()))
}

#[derive(Deserialize)]
struct DiffQuery {
    ref_a: String,
    ref_b: String,
}

async fn diff(
    State(repo): State<AppState>,
    Query(q): Query<DiffQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let ops = repo.diff(&q.ref_a, &q.ref_b)?;
    Ok(Json(serde_json::to_value(&ops).unwrap_or_default()))
}

#[derive(Deserialize)]
struct QueryRequest {
    agent_id: Option<String>,
    intent_category: Option<String>,
    tags: Option<Vec<String>>,
    reasoning_contains: Option<String>,
    confidence_min: Option<f64>,
    confidence_max: Option<f64>,
    has_deviations: Option<bool>,
    limit: Option<usize>,
    offset: Option<usize>,
}

async fn query_commits(
    State(repo): State<AppState>,
    Path(ref_name): Path<String>,
    Json(req): Json<QueryRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let filters = agentstategraph_core::QueryFilters {
        agent_id: req.agent_id,
        intent_category: req.intent_category,
        tags: req.tags,
        reasoning_contains: req.reasoning_contains,
        confidence_range: req.confidence_min.zip(req.confidence_max),
        has_deviations: req.has_deviations,
        ..Default::default()
    };
    let limit = req.limit.unwrap_or(20);
    let offset = req.offset.unwrap_or(0);
    let commits = repo.query_commits_paged(&ref_name, &filters, limit, offset)?;
    let entries: Vec<serde_json::Value> = commits
        .iter()
        .map(|c| {
            serde_json::json!({
                "id": c.id.short(),
                "agent": c.agent_id,
                "intent": {
                    "category": format!("{:?}", c.intent.category),
                    "description": c.intent.description,
                },
                "reasoning": c.reasoning,
                "confidence": c.confidence,
                "timestamp": c.timestamp.to_rfc3339(),
            })
        })
        .collect();
    Ok(Json(serde_json::json!(entries)))
}

#[derive(Deserialize)]
struct GraphQuery {
    depth: Option<usize>,
}

async fn commit_graph(
    State(repo): State<AppState>,
    Path(ref_name): Path<String>,
    Query(q): Query<GraphQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let depth = q.depth.unwrap_or(50);
    let nodes = repo.commit_graph(&ref_name, depth)?;
    Ok(Json(serde_json::json!(nodes)))
}

// ─── Branches ───────────────────────────────────────────────

#[derive(Deserialize)]
struct BranchQuery {
    prefix: Option<String>,
}

async fn list_branches(
    State(repo): State<AppState>,
    Query(q): Query<BranchQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let branches = repo.list_branches(q.prefix.as_deref())?;
    let entries: Vec<serde_json::Value> = branches
        .iter()
        .map(|(name, id)| serde_json::json!({ "name": name, "commit": id.short() }))
        .collect();
    Ok(Json(serde_json::json!(entries)))
}

#[derive(Deserialize)]
struct CreateBranchRequest {
    name: String,
    from: Option<String>,
}

async fn create_branch(
    State(repo): State<AppState>,
    Json(req): Json<CreateBranchRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let from = req.from.as_deref().unwrap_or("main");
    let id = repo.branch(&req.name, from)?;
    Ok(Json(serde_json::json!({ "branch": req.name, "commit": id.short() })))
}

#[derive(Deserialize)]
struct MergeRequest {
    source: String,
    target: Option<String>,
    intent_description: String,
    reasoning: Option<String>,
}

async fn merge_branches(
    State(repo): State<AppState>,
    Json(req): Json<MergeRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let target = req.target.as_deref().unwrap_or("main");
    let mut opts = CommitOptions::new("http", IntentCategory::Merge, &req.intent_description);
    if let Some(r) = req.reasoning {
        opts = opts.with_reasoning(r);
    }
    match repo.merge(&req.source, target, opts) {
        Ok(id) => Ok(Json(serde_json::json!({ "commit_id": id.to_string() }))),
        Err(agentstategraph::RepoError::MergeConflicts(conflicts)) => {
            Ok(Json(serde_json::json!({ "conflicts": conflicts.len(), "details": serde_json::to_value(&conflicts).unwrap_or_default() })))
        }
        Err(e) => Err(AppError(e.into())),
    }
}

// ─── Epochs ─────────────────────────────────────────────────

async fn list_epochs(
    State(repo): State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    let entries = repo.list_epochs()?;
    let json: Vec<serde_json::Value> = entries
        .iter()
        .map(|e| {
            serde_json::json!({
                "id": e.id,
                "description": e.description,
                "status": format!("{:?}", e.status),
                "commits": e.commit_count,
                "agents": e.agents,
                "created": e.created_at.to_rfc3339(),
                "sealed": e.sealed_at.map(|t| t.to_rfc3339()),
            })
        })
        .collect();
    Ok(Json(serde_json::json!(json)))
}

#[derive(Deserialize)]
struct CreateEpochRequest {
    id: String,
    description: String,
}

async fn create_epoch(
    State(repo): State<AppState>,
    Json(req): Json<CreateEpochRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let epoch = repo.create_epoch(&req.id, &req.description, vec![])?;
    Ok(Json(serde_json::json!({ "id": epoch.id, "status": format!("{:?}", epoch.status) })))
}

#[derive(Deserialize)]
struct SealEpochRequest {
    id: String,
    summary: String,
}

async fn seal_epoch(
    State(repo): State<AppState>,
    Json(req): Json<SealEpochRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    repo.seal_epoch(&req.id, &req.summary)?;
    Ok(Json(serde_json::json!({ "id": req.id, "sealed": true })))
}

// ─── Stats & Intents ────────────────────────────────────────

async fn stats(
    State(repo): State<AppState>,
    Path(ref_name): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let s = repo.stats(&ref_name)?;
    Ok(Json(s))
}

#[derive(Deserialize)]
struct IntentQuery {
    root_commit_id: Option<String>,
}

async fn intent_tree(
    State(repo): State<AppState>,
    Path(ref_name): Path<String>,
    Query(q): Query<IntentQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let tree = repo.intent_tree(&ref_name, q.root_commit_id.as_deref())?;
    Ok(Json(tree))
}

// ─── Error handling ─────────────────────────────────────────

struct AppError(Box<dyn std::error::Error>);

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": self.0.to_string() }))).into_response()
    }
}

impl<E: std::error::Error + 'static> From<E> for AppError {
    fn from(err: E) -> Self {
        AppError(Box::new(err))
    }
}

// ─── Helpers ────────────────────────────────────────────────

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
