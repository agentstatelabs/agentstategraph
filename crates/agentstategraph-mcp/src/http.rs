//! HTTP REST API for AgentStateGraph.
//!
//! Exposes the same operations as the MCP tools over HTTP.
//! Supports two modes:
//!   - Single-tenant (default): no auth, one repo
//!   - Multi-tenant: API key auth, per-tenant isolation
//!
//! Start with: agentstategraph-mcp --http --port 3001

use std::sync::{Arc, Mutex};

use axum::{
    Extension, Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    middleware,
    response::IntoResponse,
    routing::{get, post},
};
use governor::middleware::NoOpMiddleware;
use serde::Deserialize;
use tower_governor::GovernorLayer;
use tower_governor::governor::{GovernorConfig, GovernorConfigBuilder};
use tower_governor::key_extractor::PeerIpKeyExtractor;
use tower_http::cors::{Any, CorsLayer};
use tracing::{info, warn};

use agentstategraph::{CommitOptions, RepoError, Repository};
use agentstategraph_core::IntentCategory;

use crate::auth::{self, AuthContext, TenantManager};

/// Compiled config for per-peer-IP governor.
pub type PeerIpGovernorConfig = GovernorConfig<PeerIpKeyExtractor, NoOpMiddleware>;

/// Fully-pinned GovernorLayer type.
pub type PeerIpGovernorLayer = GovernorLayer<PeerIpKeyExtractor, NoOpMiddleware, axum::body::Body>;

/// Build a per-peer-IP rate limit layer enforcing `rpm` requests/minute.
/// Returns `None` when `rpm == 0` (disabled). The returned layer emits
/// axum-native `429 Too Many Requests` responses on its own.
pub fn build_governor_layer(rpm: u32) -> Option<PeerIpGovernorLayer> {
    if rpm == 0 {
        info!("ASG rate limiting disabled (rpm = 0)");
        return None;
    }
    let period_ms = (60_000 / rpm.max(1)) as u64;
    let burst = (rpm / 10).max(5);
    let config: PeerIpGovernorConfig = GovernorConfigBuilder::default()
        .period(std::time::Duration::from_millis(period_ms))
        .burst_size(burst)
        .finish()?;
    info!(rpm, period_ms, burst, "ASG rate limiter configured");
    Some(GovernorLayer::new(config))
}

/// Track per-key-prefix warning state so we only warn once per key when
/// it falls back to the body-supplied agent field.
static AGENT_FALLBACK_WARNED: Mutex<Option<std::collections::HashSet<String>>> = Mutex::new(None);

fn warn_agent_fallback_once(key_prefix: &Option<String>) {
    let prefix = key_prefix.clone().unwrap_or_else(|| "<anon>".to_string());
    let mut guard = AGENT_FALLBACK_WARNED
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let set = guard.get_or_insert_with(std::collections::HashSet::new);
    if set.insert(prefix.clone()) {
        warn!(
            key_prefix = %prefix,
            "commit agent_id not bound to API key; falling back to body-supplied `agent` field (set `commit_agent_id` on the key to enforce authenticated agent identity)"
        );
    }
}

pub type AppState = Arc<Repository>;

/// Create a single-tenant router (no auth, backward compatible).
#[allow(dead_code)]
pub fn router(repo: Arc<Repository>) -> Router {
    router_with_rate_limit(repo, 0)
}

/// Create a single-tenant router with an explicit rate limit (rpm).
pub fn router_with_rate_limit(repo: Arc<Repository>, rpm: u32) -> Router {
    let tenant_mgr = TenantManager::single_tenant(repo.clone());
    build_router(repo, tenant_mgr, rpm)
}

/// Create a multi-tenant router with API key authentication.
#[allow(dead_code)]
pub fn router_multi_tenant(repo: Arc<Repository>, keys_file: Option<&str>) -> Router {
    router_multi_tenant_with_rate_limit(repo, keys_file, 0)
}

/// Create a multi-tenant router with an explicit rate limit (rpm).
pub fn router_multi_tenant_with_rate_limit(
    repo: Arc<Repository>,
    keys_file: Option<&str>,
    rpm: u32,
) -> Router {
    let tenant_mgr = TenantManager::multi_tenant(repo.clone(), keys_file);
    build_router(repo, tenant_mgr, rpm)
}

/// Test hook: build the router with a pre-constructed `TenantManager`
/// so integration tests can seed specific API keys without spinning
/// them through the admin HTTP endpoints.
pub fn build_router_for_test(
    repo: Arc<Repository>,
    tenant_mgr: Arc<TenantManager>,
    rpm: u32,
) -> Router {
    build_router(repo, tenant_mgr, rpm)
}

fn build_router(repo: Arc<Repository>, tenant_mgr: Arc<TenantManager>, rpm: u32) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    // API routes that go through auth middleware
    let api_routes = Router::new()
        // State operations
        .route("/state/{ref_name}", get(get_state))
        .route("/state/{ref_name}/paths", get(list_paths))
        .route("/state/{ref_name}/search", get(search_values))
        .route("/state/{ref_name}/set", post(set_value))
        .route("/state/{ref_name}/delete", post(delete_value))
        // History
        .route("/log/{ref_name}", get(get_log))
        .route("/blame/{ref_name}", get(blame))
        .route("/diff", get(diff))
        .route("/query/{ref_name}", post(query_commits))
        .route("/graph/{ref_name}", get(commit_graph))
        // Branches
        .route("/branches", get(list_branches))
        .route("/branches", post(create_branch))
        .route("/merge", post(merge_branches))
        // Epochs
        .route("/epochs", get(list_epochs))
        .route("/epochs", post(create_epoch))
        .route("/epochs/seal", post(seal_epoch))
        // Stats & meta
        .route("/stats/{ref_name}", get(stats))
        .route("/intents/{ref_name}", get(intent_tree))
        .route_layer(middleware::from_fn_with_state(
            tenant_mgr.clone(),
            auth::auth_middleware,
        ))
        .with_state(repo);

    // Public health endpoint — no auth.
    let public_routes = Router::new()
        .route("/api/health", get(health_with_mgr))
        .with_state(tenant_mgr.clone());

    // Admin routes (key management) — gated behind admin_auth_middleware.
    // In single-tenant mode the middleware is a no-op (see auth.rs).
    let admin_routes = Router::new()
        .route("/api/admin/keys", get(list_keys))
        .route("/api/admin/keys", post(create_key))
        .route("/api/admin/keys/revoke", post(revoke_key))
        .route_layer(middleware::from_fn_with_state(
            tenant_mgr.clone(),
            auth::admin_auth_middleware,
        ))
        .with_state(tenant_mgr);

    let mut router = Router::new()
        .nest("/api", api_routes)
        .merge(public_routes)
        .merge(admin_routes)
        .layer(cors);

    // Apply the governor layer LAST so it runs FIRST in the request
    // lifecycle (tower layers execute in reverse insertion order).
    // tower_governor's GovernorLayer emits axum-native 429 responses on
    // its own — no HandleErrorLayer wrapping needed. When rpm=0, the
    // layer is absent.
    if let Some(layer) = build_governor_layer(rpm) {
        router = router.layer(layer);
    }

    router
}

// ─── Health ─────────────────────────────────────────────────

async fn health_with_mgr(State(mgr): State<Arc<TenantManager>>) -> Json<serde_json::Value> {
    // Get repo via manager (no auth needed for health)
    let repo = mgr.get_repo(None).unwrap_or_else(|_| {
        // If auth is enabled, health still works — just report status
        mgr.get_repo(None).unwrap_or_else(|_| {
            Arc::new(Repository::new(Box::new(
                agentstategraph_storage::MemoryStorage::new(),
            )))
        })
    });
    let branches = repo.list_branches(None).unwrap_or_default();
    Json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "branches": branches.len(),
    }))
}

// ─── Admin: Key Management ──────────────────────────────────

#[derive(Deserialize)]
struct CreateKeyRequest {
    tenant_id: String,
    name: String,
    plan: Option<String>,
    #[serde(default)]
    commit_agent_id: Option<String>,
    #[serde(default)]
    can_migrate: Option<bool>,
    #[serde(default)]
    is_admin: Option<bool>,
}

async fn list_keys(State(mgr): State<Arc<TenantManager>>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "keys": mgr.list_keys() }))
}

async fn create_key(
    State(mgr): State<Arc<TenantManager>>,
    Json(req): Json<CreateKeyRequest>,
) -> Json<serde_json::Value> {
    let plan = req.plan.unwrap_or_else(|| "free".to_string());
    let can_migrate = req.can_migrate.unwrap_or(false);
    let is_admin = req.is_admin.unwrap_or(false);
    let key = mgr.create_key_with(
        &req.tenant_id,
        &req.name,
        &plan,
        req.commit_agent_id,
        can_migrate,
        is_admin,
    );
    // Return the full key ONCE — it won't be shown again
    Json(serde_json::json!({
        "key": key.key,
        "tenant_id": key.tenant_id,
        "name": key.name,
        "plan": key.plan,
        "commit_agent_id": key.commit_agent_id,
        "can_migrate": key.can_migrate,
        "is_admin": key.is_admin,
        "message": "Save this key — it will not be shown again."
    }))
}

#[derive(Deserialize)]
struct RevokeKeyRequest {
    key_prefix: String,
}

async fn revoke_key(
    State(mgr): State<Arc<TenantManager>>,
    Json(req): Json<RevokeKeyRequest>,
) -> Json<serde_json::Value> {
    let revoked = mgr.revoke_key(&req.key_prefix);
    Json(serde_json::json!({ "revoked": revoked }))
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
    Ok(Json(
        serde_json::json!({ "count": paths.len(), "paths": paths }),
    ))
}

async fn search_values(
    State(repo): State<AppState>,
    Path(ref_name): Path<String>,
    Query(q): Query<PathQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let query = q.query.as_deref().unwrap_or("");
    if query.is_empty() {
        return Ok(Json(
            serde_json::json!({ "error": "query parameter required" }),
        ));
    }
    let results = repo.search_values(&ref_name, query, q.max_results)?;
    let entries: Vec<serde_json::Value> = results
        .iter()
        .map(|(path, value)| serde_json::json!({ "path": path, "value": value }))
        .collect();
    Ok(Json(
        serde_json::json!({ "count": entries.len(), "results": entries }),
    ))
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
    Extension(ctx): Extension<AuthContext>,
    Path(ref_name): Path<String>,
    Json(req): Json<SetRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let category = enforce_migrate_capability(parse_category(&req.intent_category), &ctx);
    let agent = resolve_agent(&ctx, req.agent);
    let mut opts = CommitOptions::new(agent, category, &req.intent_description);
    if let Some(r) = req.reasoning {
        opts = opts.with_reasoning(r);
    }
    if let Some(c) = req.confidence {
        opts = opts.with_confidence(c);
    }
    let commit_id = repo.set_json(&ref_name, &req.path, &req.value, opts)?;
    Ok(Json(
        serde_json::json!({ "commit_id": commit_id.to_string() }),
    ))
}

#[derive(Deserialize)]
struct DeleteRequest {
    path: String,
    intent_category: String,
    intent_description: String,
    #[serde(default)]
    agent: Option<String>,
}

async fn delete_value(
    State(repo): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(ref_name): Path<String>,
    Json(req): Json<DeleteRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let category = enforce_migrate_capability(parse_category(&req.intent_category), &ctx);
    let agent = resolve_agent(&ctx, req.agent);
    let opts = CommitOptions::new(agent, category, &req.intent_description);
    let commit_id = repo.delete(&ref_name, &req.path, opts)?;
    Ok(Json(
        serde_json::json!({ "commit_id": commit_id.to_string() }),
    ))
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
    Ok(Json(
        serde_json::json!({ "branch": req.name, "commit": id.short() }),
    ))
}

#[derive(Deserialize)]
struct MergeRequest {
    source: String,
    target: Option<String>,
    intent_description: String,
    reasoning: Option<String>,
    #[serde(default)]
    agent: Option<String>,
}

async fn merge_branches(
    State(repo): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Json(req): Json<MergeRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let target = req.target.as_deref().unwrap_or("main");
    let agent = resolve_agent(&ctx, req.agent);
    let mut opts = CommitOptions::new(agent, IntentCategory::Merge, &req.intent_description);
    if let Some(r) = req.reasoning {
        opts = opts.with_reasoning(r);
    }
    match repo.merge(&req.source, target, opts) {
        Ok(id) => Ok(Json(serde_json::json!({ "commit_id": id.to_string() }))),
        Err(agentstategraph::RepoError::MergeConflicts(conflicts)) => Ok(Json(
            serde_json::json!({ "conflicts": conflicts.len(), "details": serde_json::to_value(&conflicts).unwrap_or_default() }),
        )),
        Err(e) => Err(AppError::from(e)),
    }
}

// ─── Epochs ─────────────────────────────────────────────────

async fn list_epochs(State(repo): State<AppState>) -> Result<Json<serde_json::Value>, AppError> {
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
    Ok(Json(
        serde_json::json!({ "id": epoch.id, "status": format!("{:?}", epoch.status) }),
    ))
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

struct AppError {
    status: StatusCode,
    message: String,
}

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        (
            self.status,
            Json(serde_json::json!({ "error": self.message })),
        )
            .into_response()
    }
}

impl From<RepoError> for AppError {
    fn from(err: RepoError) -> Self {
        let status = match err {
            // Writes to `/_meta/*` without `IntentCategory::Migrate` are
            // a capability violation, not a server fault — surface 403.
            RepoError::ReservedPath(_) => StatusCode::FORBIDDEN,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        AppError {
            status,
            message: err.to_string(),
        }
    }
}

// Generic fallback for non-RepoError error types produced by handlers.
impl From<Box<dyn std::error::Error>> for AppError {
    fn from(err: Box<dyn std::error::Error>) -> Self {
        AppError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: err.to_string(),
        }
    }
}

// ─── Helpers ────────────────────────────────────────────────

/// Pick the `agent_id` for a commit: prefer the authenticated identity
/// bound to the API key; fall back to the body-supplied `agent` (logging
/// a one-time warning per key prefix); otherwise a safe default.
fn resolve_agent(ctx: &AuthContext, body_agent: Option<String>) -> String {
    if let Some(ref bound) = ctx.commit_agent_id {
        return bound.clone();
    }
    warn_agent_fallback_once(&ctx.key_prefix);
    body_agent.unwrap_or_else(|| "http".to_string())
}

/// Downgrade `IntentCategory::Migrate` to a custom category when the
/// request's API key lacks the `can_migrate` capability. This lets the
/// reserved-path guard in `Repository` naturally reject writes to
/// `/_meta/*` from unprivileged keys.
fn enforce_migrate_capability(category: IntentCategory, ctx: &AuthContext) -> IntentCategory {
    if matches!(category, IntentCategory::Migrate) && !ctx.can_migrate {
        warn!(
            key_prefix = ?ctx.key_prefix,
            "rejecting Migrate category from key without `can_migrate`; downgrading to Custom(\"Migrate-claimed\")"
        );
        return IntentCategory::Custom("Migrate-claimed".to_string());
    }
    category
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
        "plan" => IntentCategory::Plan,
        other => IntentCategory::Custom(other.to_string()),
    }
}
