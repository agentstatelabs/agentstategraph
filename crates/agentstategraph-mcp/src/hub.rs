//! `--hub`: the single-account reference sync hub (DESIGN.md Plan G t-006).
//!
//! It mounts `agentstategraph_sync_http`'s `/sync/v1` router beside the API,
//! behind the same API keys and rate limit, so stores can push to this
//! server's store and pull from it at `/sync/v1/<tenant>/…`. The push log
//! records each push under its key's id.
//!
//! A key bound to a namespace is refused: sync moves the whole store, every
//! namespace and the control ref, so only a key that reaches all of it may.

use std::sync::Arc;

use agentstategraph::Repository;
use agentstategraph::sync::BundleLimits;
use agentstategraph_sync_http::{Rejection, Resolved, StoreResolver};
use axum::Router;
use axum::http::request::Parts;

use crate::auth::AuthContext;

/// Resolves every sync request to this server's store, named `store`.
pub struct HubResolver {
    store: String,
}

impl StoreResolver for HubResolver {
    async fn resolve(&self, store: &str, request: &Parts) -> Result<Resolved, Rejection> {
        if store != self.store {
            return Err(Rejection::NotFound(format!(
                "this hub serves the store {:?}, not {store:?}",
                self.store
            )));
        }
        // The API's auth middleware ran first and left these.
        let (Some(ctx), Some(repo)) = (
            request.extensions.get::<AuthContext>(),
            request.extensions.get::<Arc<Repository>>(),
        ) else {
            return Err(Rejection::Unauthorized("no API key was checked".into()));
        };
        if let Some(ns) = &ctx.namespace_id {
            return Err(Rejection::Forbidden(format!(
                "this key is bound to the namespace {ns:?}, and sync moves the whole store"
            )));
        }
        let pusher = match (&ctx.key_id, &ctx.commit_agent_id) {
            (Some(id), _) => format!("key/{id}"),
            (None, Some(agent)) => agent.clone(),
            (None, None) => "unauthenticated".to_string(),
        };
        Ok(Resolved {
            repo: Arc::clone(repo),
            pusher,
        })
    }
}

/// The hub's routes, serving this server's store as `store` (its tenant
/// id), with `limits` on what a push may send. Mount them as the extra
/// routes of [`crate::http::build_router_with`], so the API's auth runs
/// first.
pub fn router(store: impl Into<String>, limits: BundleLimits) -> Router {
    agentstategraph_sync_http::router_with_limits(
        Arc::new(HubResolver {
            store: store.into(),
        }),
        limits,
    )
}
