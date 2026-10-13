//! HTTP REST API for AgentStateGraph.
//!
//! Exposes the same operations as the MCP tools over HTTP.
//! Supports two modes:
//!   - Single-tenant (default): no auth, one repo
//!   - Multi-tenant: API key auth, per-tenant isolation
//!
//! Start with: agentstategraph-mcp --http --port 3001

use std::sync::{Arc, Mutex};

#[cfg(feature = "providers")]
use axum::extract::OriginalUri;
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
use tower_http::cors::{AllowOrigin, Any, CorsLayer};
use tracing::{info, warn};

use agentstategraph::{CommitOptions, RepoError, Repository, RetentionPolicy};
use agentstategraph_core::IntentCategory;

use crate::auth::{self, AuthContext, CreateKeyOptions, KeyRef, KeyStoreError, TenantManager};
use oxtel::sdk::metrics::MeterProvider;

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

/// Track per-key warning state so we only warn once per key when it falls
/// back to the body-supplied agent field.
static AGENT_FALLBACK_WARNED: Mutex<Option<std::collections::HashSet<String>>> = Mutex::new(None);

fn warn_agent_fallback_once(key_id: &Option<String>) {
    let key = key_id.clone().unwrap_or_else(|| "<anon>".to_string());
    let mut guard = AGENT_FALLBACK_WARNED
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let set = guard.get_or_insert_with(std::collections::HashSet::new);
    if set.insert(key.clone()) {
        warn!(
            key_id = %key,
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
    build_router_with(TenantManager::single_tenant(repo), rpm, None)
}

/// Create a multi-tenant router with API key authentication. Fails if the
/// keys file can't be read.
#[allow(dead_code)]
pub fn router_multi_tenant(
    repo: Arc<Repository>,
    keys_file: Option<&str>,
) -> Result<Router, KeyStoreError> {
    router_multi_tenant_with_rate_limit(repo, keys_file, 0)
}

/// Create a multi-tenant router with an explicit rate limit (rpm). Fails if
/// the keys file can't be read.
pub fn router_multi_tenant_with_rate_limit(
    repo: Arc<Repository>,
    keys_file: Option<&str>,
    rpm: u32,
) -> Result<Router, KeyStoreError> {
    let tenant_mgr = TenantManager::multi_tenant(repo, keys_file)?;
    Ok(build_router_with(tenant_mgr, rpm, None))
}

/// Test hook: build the router with a pre-constructed `TenantManager`
/// so integration tests can seed specific API keys without spinning
/// them through the admin HTTP endpoints. `_repo` is unused: requests work
/// in the manager's repository, scoped to each request's key.
pub fn build_router_for_test(
    _repo: Arc<Repository>,
    tenant_mgr: Arc<TenantManager>,
    rpm: u32,
) -> Router {
    build_router_with(tenant_mgr, rpm, None)
}

/// The API router, plus `extra` routes (the ASD proxy) served behind the
/// same auth and rate limit as the API. Routes merged onto the router
/// afterwards would get neither. Each request works in the repository the
/// manager gives its key: the default one, or a namespace-bound one.
pub fn build_router_with(
    tenant_mgr: Arc<TenantManager>,
    rpm: u32,
    extra: Option<Router>,
) -> Router {
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
        .route("/history", get(history))
        .route("/gc/dry-run", get(gc_dry_run))
        .route("/gc/sweep", post(gc_sweep))
        .route("/gc/vacuum", post(gc_vacuum))
        .route_layer(middleware::from_fn_with_state(
            tenant_mgr.clone(),
            auth::auth_middleware,
        ));

    // Public health endpoint — no auth.
    let public_routes = Router::new()
        .route("/api/health", get(health))
        .route("/api/ready", get(ready))
        .with_state(tenant_mgr.clone());

    // Admin routes (key management) — gated behind admin_auth_middleware.
    // In single-tenant mode the middleware is a no-op (see auth.rs).
    let admin_routes = Router::new()
        .route("/api/admin/keys", get(list_keys))
        .route("/api/admin/keys", post(create_key))
        .route("/api/admin/keys/revoke", post(revoke_key))
        .route("/api/admin/keys/rotate", post(rotate_key))
        .route_layer(middleware::from_fn_with_state(
            tenant_mgr.clone(),
            auth::admin_auth_middleware,
        ))
        .with_state(tenant_mgr.clone());

    let mut router = Router::new()
        .nest("/api", api_routes)
        .merge(public_routes)
        .merge(admin_routes);
    if let Some(extra) = extra {
        router = router.merge(extra.route_layer(middleware::from_fn_with_state(
            tenant_mgr.clone(),
            auth::auth_middleware,
        )));
    }

    // Apply the governor layer LAST so it runs FIRST in the request
    // lifecycle (tower layers execute in reverse insertion order).
    // tower_governor's GovernorLayer emits axum-native 429 responses on
    // its own — no HandleErrorLayer wrapping needed. When rpm=0, the
    // layer is absent.
    if let Some(layer) = build_rate_limit_layer(rpm, tenant_mgr) {
        router = router.layer(layer);
    }

    router
}

/// Which bucket a request's rate limit comes from: its API key, when it
/// carries a live one and auth is on, else the peer IP. Behind a load
/// balancer every client shares the balancer's IP, so per-key buckets are
/// what keeps one client from using up everyone's limit. A key that isn't
/// live falls back to the IP, so inventing keys earns no extra buckets.
#[derive(Clone)]
pub struct KeyOrPeerIp {
    tenant_mgr: Arc<TenantManager>,
}

/// A rate-limit bucket: an API key's id, or a peer IP.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum RateKey {
    Key(String),
    Ip(std::net::IpAddr),
}

impl tower_governor::key_extractor::KeyExtractor for KeyOrPeerIp {
    type Key = RateKey;

    fn extract<T>(
        &self,
        req: &axum::http::Request<T>,
    ) -> Result<Self::Key, tower_governor::GovernorError> {
        if self.tenant_mgr.auth_enabled()
            && let Some(raw) = auth::extract_api_key(req.headers())
            && let Some(id) = self.tenant_mgr.live_key_id(&raw)
        {
            return Ok(RateKey::Key(id));
        }
        req.extensions()
            .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
            .map(|info| RateKey::Ip(info.0.ip()))
            .ok_or(tower_governor::GovernorError::UnableToExtractKey)
    }
}

/// The rate limit layer the API uses: `rpm` requests a minute per bucket
/// (see [`KeyOrPeerIp`]), or `None` when `rpm == 0`. Buckets idle long
/// enough to be full again are dropped every minute, so a stream of new
/// client IPs can't grow the table without bound.
pub fn build_rate_limit_layer(
    rpm: u32,
    tenant_mgr: Arc<TenantManager>,
) -> Option<GovernorLayer<KeyOrPeerIp, NoOpMiddleware, axum::body::Body>> {
    if rpm == 0 {
        info!("ASG rate limiting disabled (rpm = 0)");
        return None;
    }
    let period_ms = (60_000 / rpm.max(1)) as u64;
    let burst = (rpm / 10).max(5);
    let config = GovernorConfigBuilder::default()
        .key_extractor(KeyOrPeerIp { tenant_mgr })
        .period(std::time::Duration::from_millis(period_ms))
        .burst_size(burst)
        .finish()?;
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        let limiter = Arc::downgrade(config.limiter());
        runtime.spawn(async move {
            let mut every_minute = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                every_minute.tick().await;
                let Some(limiter) = limiter.upgrade() else {
                    return;
                };
                limiter.retain_recent();
            }
        });
    }
    info!(rpm, period_ms, burst, "ASG rate limiter configured");
    Some(GovernorLayer::new(config))
}

/// Whether a proxied path could leave ASD's `/api/v1`: URL parsing collapses
/// `.` and `..` segments (also when percent-encoded) and reads `\` as `/`.
pub fn escapes_proxy_root(path: &str) -> bool {
    path.contains('\\')
        || path.split('/').any(|segment| {
            let segment = segment.to_ascii_lowercase().replace("%2e", ".");
            segment == "." || segment == ".."
        })
}

// ─── Request bounds ─────────────────────────────────────────

/// Limits on each HTTP request (DESIGN.md Plan E t-008).
#[derive(Debug, Clone)]
pub struct RequestBounds {
    /// The largest request body accepted, in bytes; a larger one gets 413.
    pub max_body_bytes: usize,
    /// How long a request may take before it gets 503. `None` for no limit.
    /// Storage work already started still finishes, so a write may complete
    /// after its caller was told it timed out.
    pub timeout: Option<std::time::Duration>,
    /// Requests handled at once. More get 503 straight away rather than
    /// queueing behind them. `/api/health` is exempt, so a busy server isn't
    /// mistaken for a dead one.
    pub max_concurrent: usize,
}

impl Default for RequestBounds {
    fn default() -> Self {
        Self {
            max_body_bytes: 4 * 1024 * 1024,
            timeout: Some(std::time::Duration::from_secs(30)),
            max_concurrent: 256,
        }
    }
}

impl RequestBounds {
    /// A one-line summary for the startup log.
    pub fn describe(&self) -> String {
        format!(
            "Limits: body {} bytes, timeout {}, {} requests at once",
            self.max_body_bytes,
            self.timeout
                .map_or_else(|| "none".to_string(), |t| format!("{}s", t.as_secs())),
            self.max_concurrent
        )
    }
}

/// Apply `bounds` to every route of `app`.
pub fn with_request_bounds(app: Router, bounds: &RequestBounds) -> Router {
    let state = Arc::new(BoundsState {
        permits: Arc::new(tokio::sync::Semaphore::new(bounds.max_concurrent.max(1))),
        max_concurrent: bounds.max_concurrent.max(1),
        timeout: bounds.timeout,
    });
    app.layer(middleware::from_fn_with_state(state, bound_request))
        .layer(axum::extract::DefaultBodyLimit::max(bounds.max_body_bytes))
}

struct BoundsState {
    permits: Arc<tokio::sync::Semaphore>,
    max_concurrent: usize,
    timeout: Option<std::time::Duration>,
}

async fn bound_request(
    State(bounds): State<Arc<BoundsState>>,
    req: axum::extract::Request,
    next: middleware::Next,
) -> axum::response::Response {
    let liveness = req
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .is_some_and(|m| m.as_str() == "/api/health");
    if liveness {
        return next.run(req).await;
    }
    let Ok(_permit) = Arc::clone(&bounds.permits).try_acquire_owned() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(axum::http::header::RETRY_AFTER, "1")],
            Json(serde_json::json!({
                "error": format!(
                    "the server is handling its limit of {} requests; retry shortly",
                    bounds.max_concurrent
                )
            })),
        )
            .into_response();
    };
    let Some(limit) = bounds.timeout else {
        return next.run(req).await;
    };
    match tokio::time::timeout(limit, next.run(req)).await {
        Ok(res) => res,
        Err(_) => {
            warn!(timeout_secs = limit.as_secs_f64(), "request timed out");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({
                    "error": format!(
                        "the request took longer than {}s; a write may still have completed",
                        limit.as_secs_f64()
                    )
                })),
            )
                .into_response()
        }
    }
}

// ─── Observability ──────────────────────────────────────────
//
// Spans, request logs and metrics come from oxtel-tower, named by the
// OpenTelemetry semantic conventions for HTTP servers; docs/OBSERVABILITY.md
// lists them.

/// A span, a log line and metrics for every request, as
/// docs/OBSERVABILITY.md describes.
///
/// Apply it last, outside [`with_origin_policy`], so that requests the
/// policy refuses are logged and counted too, and serve the metrics with
/// [`with_metrics_endpoint`] before the policy, so the policy covers it.
/// [`with_server_layers`] does both in that order.
///
/// Each request gets a server span named `{method} {route}` that continues
/// the caller's W3C trace, and logs `request` when it finishes, at `info`,
/// or at `error` for a 5xx, with its status and duration. The span carries
/// `asg.api_key.id`, the id of the API key used, recorded by the auth
/// middleware, never the key. Metrics go to the process's `metrics`
/// recorder, which `main` installs.
pub fn with_observability(app: Router) -> Router {
    app.layer(
        oxtel_tower::HttpServerLayer::new().with_span(|info| {
            oxtel_tower::server_span!(info, asg.api_key.id = tracing::field::Empty)
        }),
    )
}

/// Everything the server puts around its router, in the order that matters:
/// request bounds; the metrics route, when `meters` is set; the origin
/// policy, which then covers that route; and logging and metrics outermost,
/// so they see requests the policy refuses.
pub fn with_server_layers(
    app: Router,
    tenant_mgr: Arc<TenantManager>,
    bounds: &RequestBounds,
    policy: &OriginPolicy,
    meters: Option<MeterProvider>,
) -> Router {
    let app = with_request_bounds(app, bounds);
    let app = match meters {
        Some(meters) => with_metrics_endpoint(app, tenant_mgr, meters),
        None => app,
    };
    let app = with_origin_policy(app, policy);
    with_observability(app)
}

/// Serve the metrics `meters` holds at `GET /metrics`, as OpenMetrics or
/// Prometheus text, whichever the scraper asks for. When auth is on, it
/// needs an admin key. Add it before [`with_origin_policy`], which then
/// covers it.
///
/// Names follow OpenTelemetry's rules for Prometheus:
/// `http_server_request_duration_seconds`, `http_server_active_requests`,
/// and `target_info` with the service's name and version. Routes are
/// templates (`/api/state/{ref_name}`), so label sets stay bounded; a
/// request that matched no route has no `http_route`.
pub fn with_metrics_endpoint(
    app: Router,
    tenant_mgr: Arc<TenantManager>,
    meters: MeterProvider,
) -> Router {
    let resource = meters.resource().clone();
    let exporter = oxtel::prometheus::PrometheusExporter::new(meters, resource);
    app.merge(
        Router::new()
            .route_service("/metrics", oxtel_tower::MetricsService::new(exporter))
            .route_layer(middleware::from_fn_with_state(
                tenant_mgr,
                auth::admin_auth_middleware,
            )),
    )
}

// ─── Origins and Host names ─────────────────────────────────

/// Which web pages may call the API from a browser (CORS), and which `Host`
/// names the server answers to. Apply it with [`with_origin_policy`]; the
/// router constructors add neither, so a router served without it sends no
/// CORS headers and browsers keep other origins out.
#[derive(Debug, Clone, Default)]
pub struct OriginPolicy {
    /// Origins allowed besides loopback ones, lowercased and without a
    /// trailing slash. Holds `"*"` when any origin is allowed.
    cors_origins: Vec<String>,
    /// `Host` names accepted besides loopback ones, or `None` for any.
    allowed_hosts: Option<Vec<String>>,
}

impl OriginPolicy {
    /// The policy for a server bound to `bind_addr`.
    ///
    /// CORS allows pages served from loopback (`localhost`, `*.localhost`,
    /// `127.0.0.0/8`, `[::1]`, any port or scheme) plus `extra_origins`,
    /// such as `https://app.example.com`. `*` allows any origin.
    ///
    /// On a loopback bind, only requests addressed to a loopback name are
    /// served, plus `extra_hosts`. A web page can point a domain it controls
    /// at 127.0.0.1 (DNS rebinding); its requests then carry that domain as
    /// `Host`, and are refused. A reverse proxy on the same machine that
    /// forwards the public name needs that name in `extra_hosts`. On other
    /// binds any `Host` is served unless `extra_hosts` names some. `*` turns
    /// the check off.
    pub fn for_bind(bind_addr: &str, extra_origins: &[String], extra_hosts: &[String]) -> Self {
        let normalize = |v: &String| v.trim().trim_end_matches('/').to_ascii_lowercase();
        let cors_origins: Vec<String> = extra_origins
            .iter()
            .map(normalize)
            .filter(|o| !o.is_empty())
            .collect();
        let hosts: Vec<String> = extra_hosts
            .iter()
            .map(|h| host_name(h.trim()).to_ascii_lowercase())
            .filter(|h| !h.is_empty())
            .collect();
        let allowed_hosts = if hosts.iter().any(|h| h == "*") {
            None
        } else if is_loopback_bind(bind_addr) || !hosts.is_empty() {
            Some(hosts)
        } else {
            None
        };
        Self {
            cors_origins,
            allowed_hosts,
        }
    }

    /// Whether any origin may call the API from a browser.
    pub fn allows_any_origin(&self) -> bool {
        self.cors_origins.iter().any(|o| o == "*")
    }

    /// Whether a browser page served from `origin` may call the API.
    pub fn allows_origin(&self, origin: &str) -> bool {
        let origin = origin.trim_end_matches('/').to_ascii_lowercase();
        self.allows_any_origin()
            || is_loopback_origin(&origin)
            || self.cors_origins.contains(&origin)
    }

    /// Whether a request whose `Host` header is `host` is served.
    pub fn allows_host(&self, host: &str) -> bool {
        let Some(allowed) = &self.allowed_hosts else {
            return true;
        };
        let name = host_name(host).to_ascii_lowercase();
        is_loopback_host(&name) || allowed.contains(&name)
    }

    /// A one-line summary for the startup log.
    pub fn describe(&self) -> String {
        let origins = if self.allows_any_origin() {
            "CORS: any origin".to_string()
        } else if self.cors_origins.is_empty() {
            "CORS: loopback origins only".to_string()
        } else {
            format!("CORS: loopback origins + {}", self.cors_origins.join(", "))
        };
        let hosts = match &self.allowed_hosts {
            None => "Host check: off".to_string(),
            Some(names) if names.is_empty() => "Host check: loopback names only".to_string(),
            Some(names) => format!("Host check: loopback names + {}", names.join(", ")),
        };
        format!("{origins}; {hosts}")
    }
}

/// Serve `app` under `policy`: CORS for the allowed origins, and a 403 for a
/// request addressed to a `Host` the policy doesn't serve.
pub fn with_origin_policy(app: Router, policy: &OriginPolicy) -> Router {
    let allow_origin = if policy.allows_any_origin() {
        AllowOrigin::from(Any)
    } else {
        let policy = policy.clone();
        AllowOrigin::predicate(move |origin, _| {
            origin.to_str().is_ok_and(|o| policy.allows_origin(o))
        })
    };
    let cors = CorsLayer::new()
        .allow_origin(allow_origin)
        .allow_methods(Any)
        .allow_headers(Any);
    let app = app.layer(cors);
    if policy.allowed_hosts.is_none() {
        return app;
    }
    // Outermost, so a refused request goes no further.
    app.layer(middleware::from_fn_with_state(
        Arc::new(policy.clone()),
        check_host,
    ))
}

async fn check_host(
    State(policy): State<Arc<OriginPolicy>>,
    req: axum::extract::Request,
    next: middleware::Next,
) -> axum::response::Response {
    let host = req
        .headers()
        .get(axum::http::header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(str::to_string)
        .or_else(|| req.uri().authority().map(|a| a.to_string()));
    match host {
        // No browser sends a request without a Host.
        None => next.run(req).await,
        Some(host) if policy.allows_host(&host) => next.run(req).await,
        Some(host) => {
            warn!(host = %host, "refused a request for a Host this server doesn't serve");
            (
                StatusCode::FORBIDDEN,
                format!(
                    "this server doesn't answer to Host '{host}'. Behind a reverse proxy, \
                     pass --allowed-host {} (or set ASG_ALLOWED_HOSTS).",
                    host_name(&host)
                ),
            )
                .into_response()
        }
    }
}

/// The host part of a `host[:port]` authority, with an IPv6 literal kept in
/// brackets.
fn host_name(authority: &str) -> &str {
    if authority.starts_with('[') {
        return authority
            .find(']')
            .map_or(authority, |end| &authority[..=end]);
    }
    authority.split(':').next().unwrap_or(authority)
}

fn is_loopback_host(host: &str) -> bool {
    let host = host.trim_end_matches('.');
    host == "localhost"
        || host.ends_with(".localhost")
        || host == "[::1]"
        || host
            .parse::<std::net::Ipv4Addr>()
            .is_ok_and(|ip| ip.is_loopback())
}

fn is_loopback_origin(origin: &str) -> bool {
    origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
        .is_some_and(|authority| is_loopback_host(host_name(authority)))
}

/// Whether the server listens only on a loopback address.
pub fn is_loopback_bind(bind_addr: &str) -> bool {
    let addr = bind_addr.trim_start_matches('[').trim_end_matches(']');
    addr == "localhost"
        || addr
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

// ─── Health ─────────────────────────────────────────────────

/// Liveness: the process is up and serving. Touches nothing else, so a slow
/// or unreachable store doesn't get a healthy process restarted; that's
/// what `/api/ready` reports.
async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

/// How long `/api/ready` waits for the store.
const READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Readiness: the store answers. 503 when it errors or takes longer than
/// [`READY_TIMEOUT`]. The endpoint is public, so the reason is logged, not
/// returned (a storage error can name hosts).
async fn ready(State(mgr): State<Arc<TenantManager>>) -> impl IntoResponse {
    let repo = Arc::clone(mgr.default_repo());
    let check = tokio::task::spawn_blocking(move || repo.list_namespaces().map(|_| ()));
    let failure = match tokio::time::timeout(READY_TIMEOUT, check).await {
        Ok(Ok(Ok(()))) => {
            return (
                StatusCode::OK,
                Json(serde_json::json!({ "status": "ready" })),
            );
        }
        Ok(Ok(Err(e))) => e.to_string(),
        Ok(Err(e)) => format!("check panicked: {e}"),
        Err(_) => format!("no answer within {}s", READY_TIMEOUT.as_secs()),
    };
    warn!(reason = %failure, "not ready: the store isn't answering");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({ "status": "not ready" })),
    )
}

// ─── Admin: Key Management ──────────────────────────────────

#[derive(Deserialize)]
struct CreateKeyRequest {
    /// Defaults to the tenant this server serves.
    #[serde(default)]
    tenant_id: Option<String>,
    name: String,
    /// Bind the key to a namespace: it then works only there, by branch name.
    #[serde(default)]
    namespace_id: Option<String>,
    plan: Option<String>,
    #[serde(default)]
    commit_agent_id: Option<String>,
    #[serde(default)]
    can_migrate: Option<bool>,
    #[serde(default)]
    is_admin: Option<bool>,
    /// Optional expiry in days from now (v3-V5). `None` = never expires.
    #[serde(default)]
    expires_in_days: Option<u32>,
}

async fn list_keys(State(mgr): State<Arc<TenantManager>>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "keys": mgr.list_keys() }))
}

/// A key change the keys file couldn't record: the change didn't happen.
fn key_store_error(e: KeyStoreError) -> AppError {
    AppError::new("key_store_failed", e.to_string())
}

/// The JSON for a newly issued key, the only time its raw value is shown.
fn issued_key_json(key: &auth::ApiKey, message: &str) -> serde_json::Value {
    serde_json::json!({
        "id": key.id,
        "key": key.key,
        "tenant_id": key.tenant_id,
        "name": key.name,
        "plan": key.plan,
        "commit_agent_id": key.commit_agent_id,
        "namespace_id": key.namespace_id,
        "can_migrate": key.can_migrate,
        "is_admin": key.is_admin,
        "expires_at": key.expires_at.map(|t| t.to_rfc3339()),
        "message": message,
    })
}

async fn create_key(
    State(mgr): State<Arc<TenantManager>>,
    Json(req): Json<CreateKeyRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let plan = req.plan.unwrap_or_else(|| "free".to_string());
    let can_migrate = req.can_migrate.unwrap_or(false);
    let is_admin = req.is_admin.unwrap_or(false);
    let expires_at = req
        .expires_in_days
        .map(|d| chrono::Utc::now() + chrono::Duration::days(d as i64));
    if let Some(ns) = &req.namespace_id
        && let Err(e) = agentstategraph_core::Namespace::new(ns)
    {
        return Err(AppError::new(
            "invalid_request",
            format!("invalid namespace_id '{ns}': {e}"),
        ));
    }
    let tenant_id = req.tenant_id.unwrap_or_else(|| mgr.tenant_id().to_string());
    let key = mgr
        .create_key_with(
            &tenant_id,
            &req.name,
            &plan,
            CreateKeyOptions {
                commit_agent_id: req.commit_agent_id,
                namespace_id: req.namespace_id,
                can_migrate,
                is_admin,
                expires_at,
            },
        )
        .map_err(key_store_error)?;
    Ok(Json(issued_key_json(
        &key,
        "Save this key — it will not be shown again.",
    )))
}

/// Which key a revoke or rotate means: `key_id` (from `GET /api/admin/keys`)
/// or the full `key`. A `key_prefix`, which this API used to take, is
/// refused: a prefix can match more than one key.
#[derive(Deserialize)]
struct KeySelector {
    #[serde(default)]
    key_id: Option<String>,
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    key_prefix: Option<String>,
}

impl KeySelector {
    fn resolve(&self, mgr: &TenantManager) -> Result<String, AppError> {
        let key_ref = match (&self.key_id, &self.key, &self.key_prefix) {
            (Some(id), _, _) => KeyRef::Id(id),
            (None, Some(key), _) => KeyRef::Key(key),
            (None, None, Some(_)) => {
                return Err(AppError::new(
                    "invalid_request",
                    "key_prefix is no longer accepted: a prefix can match more than one \
                     key. Pass key_id (from GET /api/admin/keys) or the full key.",
                ));
            }
            (None, None, None) => {
                return Err(AppError::new(
                    "invalid_request",
                    "pass key_id (from GET /api/admin/keys) or the full key",
                ));
            }
        };
        mgr.key_id(key_ref)
            .ok_or_else(|| AppError::new("key_not_found", "no such key"))
    }
}

async fn rotate_key(
    State(mgr): State<Arc<TenantManager>>,
    Json(req): Json<KeySelector>,
) -> Result<Json<serde_json::Value>, AppError> {
    let id = req.resolve(&mgr)?;
    match mgr.rotate_key(&id).map_err(key_store_error)? {
        Some(k) => Ok(Json(issued_key_json(
            &k,
            "Save this key — it will not be shown again. The old key has been revoked.",
        ))),
        None => Err(AppError::new("key_not_found", "no such key")),
    }
}

async fn revoke_key(
    State(mgr): State<Arc<TenantManager>>,
    Json(req): Json<KeySelector>,
) -> Result<Json<serde_json::Value>, AppError> {
    let id = req.resolve(&mgr)?;
    let revoked = mgr.revoke_key(&id).map_err(key_store_error)?;
    Ok(Json(serde_json::json!({ "revoked": revoked, "id": id })))
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
    Extension(repo): Extension<AppState>,
    Path(ref_name): Path<String>,
    Query(q): Query<PathQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    blocking(move || {
        let path = q.path.as_deref().unwrap_or("/");
        let value = repo.get_json(&ref_name, path)?;
        Ok(Json(value))
    })
    .await
}

async fn list_paths(
    Extension(repo): Extension<AppState>,
    Path(ref_name): Path<String>,
    Query(q): Query<PathQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    blocking(move || {
        let prefix = q.prefix.as_deref().unwrap_or("/");
        let paths = repo.list_paths(&ref_name, prefix, q.max_depth)?;
        Ok(Json(
            serde_json::json!({ "count": paths.len(), "paths": paths }),
        ))
    })
    .await
}

async fn search_values(
    Extension(repo): Extension<AppState>,
    Path(ref_name): Path<String>,
    Query(q): Query<PathQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    blocking(move || {
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
    })
    .await
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
    #[serde(default)]
    tool_calls: Option<Vec<crate::server::ToolCallInput>>,
}

async fn set_value(
    Extension(repo): Extension<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(ref_name): Path<String>,
    Json(req): Json<SetRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    blocking(move || {
        let category = enforce_migrate_capability(parse_category(&req.intent_category), &ctx);
        let agent = resolve_agent(&ctx, req.agent);
        let mut opts = CommitOptions::new(agent, category, &req.intent_description);
        if let Some(r) = req.reasoning {
            opts = opts.with_reasoning(r);
        }
        if let Some(c) = req.confidence {
            opts = opts.with_confidence(c);
        }
        let tool_calls = crate::server::tool_calls_from(req.tool_calls);
        if !tool_calls.is_empty() {
            opts = opts.with_tool_calls(tool_calls);
        }
        let commit_id = repo.set_json(&ref_name, &req.path, &req.value, opts)?;
        Ok(Json(
            serde_json::json!({ "commit_id": commit_id.to_string() }),
        ))
    })
    .await
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
    Extension(repo): Extension<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(ref_name): Path<String>,
    Json(req): Json<DeleteRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    blocking(move || {
        let category = enforce_migrate_capability(parse_category(&req.intent_category), &ctx);
        let agent = resolve_agent(&ctx, req.agent);
        let opts = CommitOptions::new(agent, category, &req.intent_description);
        let commit_id = repo.delete(&ref_name, &req.path, opts)?;
        Ok(Json(
            serde_json::json!({ "commit_id": commit_id.to_string() }),
        ))
    })
    .await
}

// ─── History ────────────────────────────────────────────────

#[derive(Deserialize)]
struct LogQuery {
    limit: Option<usize>,
}

async fn get_log(
    Extension(repo): Extension<AppState>,
    Path(ref_name): Path<String>,
    Query(q): Query<LogQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    blocking(move || {
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
    })
    .await
}

#[derive(Deserialize)]
struct BlameQuery {
    path: String,
}

async fn blame(
    Extension(repo): Extension<AppState>,
    Path(ref_name): Path<String>,
    Query(q): Query<BlameQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    blocking(move || {
        let entry = repo.blame(&ref_name, &q.path)?;
        Ok(Json(serde_json::to_value(&entry).unwrap_or_default()))
    })
    .await
}

#[derive(Deserialize)]
struct DiffQuery {
    ref_a: String,
    ref_b: String,
}

async fn diff(
    Extension(repo): Extension<AppState>,
    Query(q): Query<DiffQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    blocking(move || {
        let ops = repo.diff(&q.ref_a, &q.ref_b)?;
        Ok(Json(serde_json::to_value(&ops).unwrap_or_default()))
    })
    .await
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
    Extension(repo): Extension<AppState>,
    Path(ref_name): Path<String>,
    Json(req): Json<QueryRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    blocking(move || {
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
    })
    .await
}

#[derive(Deserialize)]
struct GraphQuery {
    depth: Option<usize>,
}

async fn commit_graph(
    Extension(repo): Extension<AppState>,
    Path(ref_name): Path<String>,
    Query(q): Query<GraphQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    blocking(move || {
        let depth = q.depth.unwrap_or(50);
        let nodes = repo.commit_graph(&ref_name, depth)?;
        Ok(Json(serde_json::json!(nodes)))
    })
    .await
}

#[derive(Deserialize)]
struct HistoryQuery {
    by: Option<String>,
    milestones: Option<usize>,
    refresh: Option<bool>,
    store: Option<bool>,
    namespace: Option<String>,
}

async fn history(
    Extension(repo): Extension<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Query(q): Query<HistoryQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    blocking(move || {
        store_wide(&ctx)?;
        let report = repo.history_report(
            q.namespace.as_deref(),
            q.by.as_deref().unwrap_or("day"),
            q.milestones.unwrap_or(50),
            q.refresh.unwrap_or(true),
            q.store.unwrap_or(false),
        )?;
        Ok(Json(report))
    })
    .await
}

async fn gc_dry_run(
    Extension(repo): Extension<AppState>,
    Extension(ctx): Extension<AuthContext>,
) -> Result<Json<serde_json::Value>, AppError> {
    blocking(move || {
        store_wide(&ctx)?;
        Ok(Json(repo.gc_dry_run()?))
    })
    .await
}

#[derive(Deserialize)]
struct SweepRequest {
    keep_recent: Option<usize>,
    checkpoint_every: Option<usize>,
    keep_milestones: Option<bool>,
    /// Must be explicitly true to delete; omitted/false previews (dry-run).
    mutate: Option<bool>,
    /// Run VACUUM after a mutating sweep to shrink the file (default false).
    vacuum: Option<bool>,
}

async fn gc_sweep(
    Extension(repo): Extension<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Json(req): Json<SweepRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    blocking(move || {
        store_wide(&ctx)?;
        let default = RetentionPolicy::default();
        let policy = RetentionPolicy {
            keep_recent: req.keep_recent.unwrap_or(default.keep_recent),
            checkpoint_every: req.checkpoint_every.unwrap_or(default.checkpoint_every),
            keep_milestones: req.keep_milestones.unwrap_or(default.keep_milestones),
        };
        let report = repo.gc_sweep(
            policy,
            req.mutate.unwrap_or(false),
            req.vacuum.unwrap_or(false),
        )?;
        crate::store_gauges::record_sweep(&report);
        Ok(Json(report))
    })
    .await
}

async fn gc_vacuum(
    Extension(repo): Extension<AppState>,
    Extension(ctx): Extension<AuthContext>,
) -> Result<Json<serde_json::Value>, AppError> {
    blocking(move || {
        store_wide(&ctx)?;
        let report = repo.gc_vacuum()?;
        crate::store_gauges::record_vacuum(&report);
        Ok(Json(report))
    })
    .await
}

/// Refuse a store-wide operation (GC, the history report) to a key bound to
/// a namespace: they read or change every namespace.
fn store_wide(ctx: &AuthContext) -> Result<(), AppError> {
    match &ctx.namespace_id {
        None => Ok(()),
        Some(ns) => Err(AppError::new(
            "key_bound_to_namespace",
            format!("this covers the whole store, and this key is bound to namespace '{ns}'"),
        )),
    }
}

// ─── Branches ───────────────────────────────────────────────

#[derive(Deserialize)]
struct BranchQuery {
    prefix: Option<String>,
}

async fn list_branches(
    Extension(repo): Extension<AppState>,
    Query(q): Query<BranchQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    blocking(move || {
        let branches = repo.list_branches(q.prefix.as_deref())?;
        let entries: Vec<serde_json::Value> = branches
            .iter()
            .map(|(name, id)| serde_json::json!({ "name": name, "commit": id.short() }))
            .collect();
        Ok(Json(serde_json::json!(entries)))
    })
    .await
}

#[derive(Deserialize)]
struct CreateBranchRequest {
    name: String,
    from: Option<String>,
}

async fn create_branch(
    Extension(repo): Extension<AppState>,
    Json(req): Json<CreateBranchRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    blocking(move || {
        let from = req.from.as_deref().unwrap_or("main");
        let id = repo.branch(&req.name, from)?;
        Ok(Json(
            serde_json::json!({ "branch": req.name, "commit": id.short() }),
        ))
    })
    .await
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
    Extension(repo): Extension<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Json(req): Json<MergeRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    blocking(move || {
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
    })
    .await
}

// ─── Epochs ─────────────────────────────────────────────────

async fn list_epochs(
    Extension(repo): Extension<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    blocking(move || {
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
    })
    .await
}

#[derive(Deserialize)]
struct CreateEpochRequest {
    id: String,
    description: String,
}

async fn create_epoch(
    Extension(repo): Extension<AppState>,
    Json(req): Json<CreateEpochRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    blocking(move || {
        let epoch = repo.create_epoch(&req.id, &req.description, vec![])?;
        Ok(Json(
            serde_json::json!({ "id": epoch.id, "status": format!("{:?}", epoch.status) }),
        ))
    })
    .await
}

#[derive(Deserialize)]
struct SealEpochRequest {
    id: String,
    summary: String,
}

async fn seal_epoch(
    Extension(repo): Extension<AppState>,
    Json(req): Json<SealEpochRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    blocking(move || {
        repo.seal_epoch(&req.id, &req.summary)?;
        Ok(Json(serde_json::json!({ "id": req.id, "sealed": true })))
    })
    .await
}

// ─── Stats & Intents ────────────────────────────────────────

async fn stats(
    Extension(repo): Extension<AppState>,
    Path(ref_name): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    blocking(move || {
        let s = repo.stats(&ref_name)?;
        Ok(Json(s))
    })
    .await
}

#[derive(Deserialize)]
struct IntentQuery {
    root_commit_id: Option<String>,
}

async fn intent_tree(
    Extension(repo): Extension<AppState>,
    Path(ref_name): Path<String>,
    Query(q): Query<IntentQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    blocking(move || {
        let tree = repo.intent_tree(&ref_name, q.root_commit_id.as_deref())?;
        Ok(Json(tree))
    })
    .await
}

/// Run a handler's storage work on the blocking pool, in the request's log
/// span. Storage calls block; on an async worker they would hold up every
/// other request on that worker (health checks included), and the request
/// timeout couldn't fire until they finished. Work still running when a
/// request times out finishes here, so a write may complete after its
/// caller was told it timed out.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, AppError> + Send + 'static,
) -> Result<T, AppError> {
    let span = tracing::Span::current();
    tokio::task::spawn_blocking(move || span.in_scope(work))
        .await
        .map_err(|e| AppError::new("internal", format!("the request failed: {e}")))?
}

// ─── Error handling ─────────────────────────────────────────

/// An error response: `{"error": message, "code": code}`. The code is
/// stable (DESIGN.md Plan H t-004) and the status follows from it; clients
/// can branch on the code, while messages may change.
struct AppError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl AppError {
    /// An error with `code`, at the status that code calls for.
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        AppError {
            status: status_for(code),
            code,
            message: message.into(),
        }
    }
}

/// The HTTP status for an error code: 404 for what doesn't exist, 409 for
/// what conflicts with the store's state, 400 for a request that can't be
/// carried out as asked, 403 for what the key or a policy doesn't allow,
/// 429 and 503 for what may work later, and 500 for the server's own
/// failures.
fn status_for(code: &str) -> StatusCode {
    match code {
        "branch_not_found"
        | "ref_not_found"
        | "commit_not_found"
        | "namespace_not_found"
        | "object_not_found"
        | "path_not_found"
        | "speculation_not_found"
        | "key_not_found" => StatusCode::NOT_FOUND,
        "branch_already_exists"
        | "namespace_already_exists"
        | "ref_already_exists"
        | "write_conflict"
        | "merge_conflict"
        | "merge_would_delete"
        | "epoch_sealed"
        | "session_ended" => StatusCode::CONFLICT,
        "invalid_request"
        | "invalid_operation"
        | "invalid_json"
        | "ambiguous_commit_prefix"
        | "type_mismatch"
        | "not_a_container"
        | "index_out_of_bounds" => StatusCode::BAD_REQUEST,
        // `/_meta/*` without `IntentCategory::Migrate`; a namespace-bound
        // key naming a commit, or asking for the whole store; a policy or a
        // taint saying no.
        "reserved_path"
        | "commit_ref_not_allowed"
        | "key_bound_to_namespace"
        | "cross_namespace_denied"
        | "taint_blocked" => StatusCode::FORBIDDEN,
        "too_many_speculations" => StatusCode::TOO_MANY_REQUESTS,
        "not_initialized" => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        (
            self.status,
            Json(serde_json::json!({ "error": self.message, "code": self.code })),
        )
            .into_response()
    }
}

impl From<RepoError> for AppError {
    fn from(err: RepoError) -> Self {
        AppError::new(err.code(), err.to_string())
    }
}

// Generic fallback for non-RepoError error types produced by handlers.
impl From<Box<dyn std::error::Error>> for AppError {
    fn from(err: Box<dyn std::error::Error>) -> Self {
        AppError::new("internal", err.to_string())
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
    warn_agent_fallback_once(&ctx.key_id);
    body_agent.unwrap_or_else(|| "http".to_string())
}

/// Downgrade `IntentCategory::Migrate` to a custom category when the
/// request's API key lacks the `can_migrate` capability. This lets the
/// reserved-path guard in `Repository` naturally reject writes to
/// `/_meta/*` from unprivileged keys.
fn enforce_migrate_capability(category: IntentCategory, ctx: &AuthContext) -> IntentCategory {
    if matches!(category, IntentCategory::Migrate) && !ctx.can_migrate {
        warn!(
            key_id = ?ctx.key_id,
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

// ─── ASD Code Proxy ──────────────────────────────────────────

/// Build a router that proxies `GET /api/code/*` to ASD's `<base>/api/v1/*`,
/// so the embedded Lens can reach ASD from the same origin (e.g.
/// `your-hub --lens --asd-url http://localhost:8787`). Pass it to
/// [`build_router_with`], which puts it behind the API's auth and rate
/// limit. No request header is forwarded, so an API key never reaches ASD.
#[cfg(feature = "providers")]
pub fn asd_proxy_router(asd_base_url: String) -> Router {
    Router::new()
        .route("/api/code/{*path}", get(asd_proxy))
        .with_state(std::sync::Arc::new(asd_base_url))
}

#[cfg(feature = "providers")]
async fn asd_proxy(
    State(base_url): State<std::sync::Arc<String>>,
    OriginalUri(uri): OriginalUri,
) -> impl IntoResponse {
    let client = reqwest::Client::new();
    let path_and_query = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/");
    let stripped = path_and_query
        .strip_prefix("/api/code")
        .unwrap_or(path_and_query);
    if escapes_proxy_root(uri.path()) {
        return (
            StatusCode::BAD_REQUEST,
            "the proxied path may not contain . or .. segments or backslashes",
        )
            .into_response();
    }
    let target = format!("{}/api/v1{}", base_url.trim_end_matches('/'), stripped);
    match client.get(&target).send().await {
        Ok(resp) => {
            let status =
                StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let ct = resp
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("application/json")
                .to_string();
            match resp.bytes().await {
                Ok(body) => {
                    (status, [(axum::http::header::CONTENT_TYPE, ct)], body).into_response()
                }
                Err(_) => StatusCode::BAD_GATEWAY.into_response(),
            }
        }
        Err(_) => StatusCode::BAD_GATEWAY.into_response(),
    }
}
