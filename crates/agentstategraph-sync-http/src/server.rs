//! The `/sync/v1` router a host mounts (DESIGN.md Plan G t-006 and t-012).
//!
//! The host supplies a [`StoreResolver`]: given the store a request names
//! and the request itself, it authenticates the caller and hands back the
//! store's repository and the name the push log records the caller under,
//! or says why not. A host with one account resolves every request to one
//! store; one with many accounts resolves each to the account's own, behind
//! its own tokens.
//!
//! | Route | Body | Answer |
//! |---|---|---|
//! | `GET /sync/v1/{store}/refs` | | JSON list of [`WireRef`] |
//! | `POST /sync/v1/{store}/fetch` | JSON [`WireFetch`] | a bundle |
//! | `POST /sync/v1/{store}/push` | [`encode_push`](crate::wire::encode_push) | JSON [`WireOutcome`] |
//!
//! An error is a JSON [`WireError`] with its status. Storage work runs on
//! Tokio's blocking pool.

use std::future::Future;
use std::sync::Arc;

use agentstategraph::Repository;
use agentstategraph::sync::{BundleLimits, SyncError};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::{Path, Request, State};
use axum::http::{StatusCode, header, request::Parts};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};

use crate::wire::{
    API_PREFIX, BUNDLE_MEDIA_TYPE, WireError, WireFetch, WireOutcome, WireRef, decode_push,
    status_for,
};

/// The most a fetch's request may take: its ref names and haves.
const MAX_FETCH_REQUEST_BYTES: usize = 16 << 20;

/// Room in a push's body beyond its bundle, for the update ahead of it.
const PUSH_OVERHEAD_BYTES: usize = 2 << 20;

/// What a host gives the router for each request.
pub struct Resolved {
    pub repo: Arc<Repository>,
    /// Who the caller is, as the host knows them: the push log records
    /// pushes under it.
    pub pusher: String,
}

/// Why a host won't serve a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    /// No credentials, or ones that aren't good: 401.
    Unauthorized(String),
    /// Good credentials that don't reach this store: 403.
    Forbidden(String),
    /// No such store: 404.
    NotFound(String),
}

impl IntoResponse for Rejection {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Rejection::Unauthorized(m) => (StatusCode::UNAUTHORIZED, "unauthorized", m),
            Rejection::Forbidden(m) => (StatusCode::FORBIDDEN, "forbidden", m),
            Rejection::NotFound(m) => (StatusCode::NOT_FOUND, "no_such_store", m),
        };
        error(status, code, message)
    }
}

/// How a host finds the store a request is for, and who's asking.
pub trait StoreResolver: Send + Sync + 'static {
    fn resolve(
        &self,
        store: &str,
        request: &Parts,
    ) -> impl Future<Output = Result<Resolved, Rejection>> + Send;
}

struct Shared<R> {
    resolver: Arc<R>,
    limits: BundleLimits,
}

/// The router, with the default bundle limits for pushes.
pub fn router<R: StoreResolver>(resolver: Arc<R>) -> Router {
    router_with_limits(resolver, BundleLimits::default())
}

/// The router, with `limits` on what a push may send. The router reads a
/// push's body up to the limit's `max_bytes`, plus room for its update, and
/// a fetch's request up to 16 MiB, whatever `DefaultBodyLimit` the host sets
/// for its own routes. A hard limit a host layers over everything, such as
/// tower-http's `RequestBodyLimitLayer`, still applies.
pub fn router_with_limits<R: StoreResolver>(resolver: Arc<R>, limits: BundleLimits) -> Router {
    let shared = Arc::new(Shared { resolver, limits });
    Router::new()
        .route(&format!("{API_PREFIX}/{{store}}/refs"), get(refs::<R>))
        .route(&format!("{API_PREFIX}/{{store}}/fetch"), post(fetch::<R>))
        .route(&format!("{API_PREFIX}/{{store}}/push"), post(push::<R>))
        .with_state(shared)
}

fn error(status: StatusCode, code: &str, message: impl Into<String>) -> Response {
    let body = WireError {
        code: code.to_string(),
        message: message.into(),
    };
    (status, axum::Json(body)).into_response()
}

fn sync_error(e: SyncError) -> Response {
    let status = StatusCode::from_u16(status_for(&e)).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    error(status, e.code(), e.to_string())
}

fn bad_request(message: impl Into<String>) -> Response {
    error(StatusCode::BAD_REQUEST, "bad_request", message)
}

/// Run `work` on the blocking pool.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, SyncError> + Send + 'static,
) -> Result<T, Response> {
    match tokio::task::spawn_blocking(work).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(sync_error(e)),
        Err(e) => Err(error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            format!("the request's work stopped: {e}"),
        )),
    }
}

/// Resolve the request's store, and read its body up to `limit` bytes.
async fn open<R: StoreResolver>(
    shared: &Shared<R>,
    store: &str,
    request: Request,
    limit: usize,
) -> Result<(Resolved, axum::body::Bytes), Response> {
    let (parts, body) = request.into_parts();
    let resolved = shared
        .resolver
        .resolve(store, &parts)
        .await
        .map_err(IntoResponse::into_response)?;
    let bytes = to_bytes(body, limit).await.map_err(|_| {
        error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "body_too_large",
            format!("the request's body is over this route's limit of {limit} bytes"),
        )
    })?;
    Ok((resolved, bytes))
}

async fn refs<R: StoreResolver>(
    State(shared): State<Arc<Shared<R>>>,
    Path(store): Path<String>,
    request: Request,
) -> Response {
    let (resolved, _) = match open(&shared, &store, request, 0).await {
        Ok(v) => v,
        Err(response) => return response,
    };
    let repo = resolved.repo;
    match blocking(move || repo.sync_refs()).await {
        Ok(refs) => axum::Json(refs.iter().map(WireRef::from).collect::<Vec<_>>()).into_response(),
        Err(response) => response,
    }
}

async fn fetch<R: StoreResolver>(
    State(shared): State<Arc<Shared<R>>>,
    Path(store): Path<String>,
    request: Request,
) -> Response {
    let (resolved, body) = match open(&shared, &store, request, MAX_FETCH_REQUEST_BYTES).await {
        Ok(v) => v,
        Err(response) => return response,
    };
    let wanted = match serde_json::from_slice::<WireFetch>(&body)
        .map_err(|e| e.to_string())
        .and_then(WireFetch::into_request)
    {
        Ok(r) => r,
        Err(e) => return bad_request(format!("a fetch's request: {e}")),
    };
    let repo = resolved.repo;
    match blocking(move || repo.sync_fetch(&wanted, Vec::new())).await {
        Ok(bundle) => (
            [(header::CONTENT_TYPE, BUNDLE_MEDIA_TYPE)],
            Body::from(bundle),
        )
            .into_response(),
        Err(response) => response,
    }
}

async fn push<R: StoreResolver>(
    State(shared): State<Arc<Shared<R>>>,
    Path(store): Path<String>,
    request: Request,
) -> Response {
    let limit = usize::try_from(shared.limits.max_bytes)
        .unwrap_or(usize::MAX)
        .saturating_add(PUSH_OVERHEAD_BYTES);
    let (resolved, body) = match open(&shared, &store, request, limit).await {
        Ok(v) => v,
        Err(response) => return response,
    };
    let (update, bundle_at) = match decode_push(&body) {
        Ok((update, bundle)) => (update, body.len() - bundle.len()),
        Err(e) => return bad_request(e),
    };
    let update = match update.into_update() {
        Ok(u) => u,
        Err(e) => return bad_request(format!("a push's update: {e}")),
    };
    let (repo, pusher, limits) = (resolved.repo, resolved.pusher, shared.limits.clone());
    match blocking(move || repo.sync_receive(&update, &body[bundle_at..], limits, &pusher)).await {
        Ok(outcome) => axum::Json(WireOutcome::from(&outcome)).into_response(),
        Err(response) => response,
    }
}
