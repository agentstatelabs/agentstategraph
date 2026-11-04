//! The sync calls the MCP tools and the HTTP API share (DESIGN.md Plan G
//! t-008): named remotes, and fetch, push and pull through them.
//!
//! Reaching a remote takes an HTTP client, which a default build leaves out
//! (see the `providers` note in `Cargo.toml`): without the `sync-client`
//! feature, fetch, push and pull answer `sync_unavailable`, while remotes
//! can still be listed, added and removed. A pull renumbers the task ids in
//! `/plans` that collide with the remote's before it merges.

use std::sync::Arc;

use agentstategraph::sync::{PushOverride, RemoteConfig, SyncError};
use agentstategraph::{CommitOptions, RepoError, Repository};
use serde_json::{Value, json};

/// A failed sync call: a stable code, a message, and for a blocked push its
/// findings (never their values).
#[derive(Debug)]
pub struct SyncFailure {
    pub code: &'static str,
    pub message: String,
    pub details: Option<Value>,
}

impl From<SyncError> for SyncFailure {
    fn from(e: SyncError) -> Self {
        let details = agentstategraph_sync_http::report::error(&e)
            .get("findings")
            .cloned();
        SyncFailure {
            code: e.code(),
            message: e.to_string(),
            details,
        }
    }
}

impl From<RepoError> for SyncFailure {
    fn from(e: RepoError) -> Self {
        SyncFailure {
            code: e.code(),
            message: e.to_string(),
            details: None,
        }
    }
}

impl SyncFailure {
    /// As an MCP tool's answer.
    pub fn to_tool_text(&self) -> String {
        match &self.details {
            Some(d) => format!(
                "Error [{}]: {}\n{}",
                self.code,
                self.message,
                serde_json::to_string_pretty(d).unwrap_or_default()
            ),
            None => format!("Error [{}]: {}", self.code, self.message),
        }
    }
}

#[cfg(not(feature = "sync-client"))]
fn unavailable() -> SyncFailure {
    SyncFailure {
        code: "sync_unavailable",
        message: "this build can't reach a remote: build agentstategraph-mcp with \
                  --features sync-client"
            .into(),
        details: None,
    }
}

/// Every named remote, with where each of its branches was last seen.
pub fn remotes(repo: &Repository) -> Result<Value, SyncFailure> {
    Ok(agentstategraph_sync_http::report::remotes(repo)?)
}

pub fn add_remote(
    repo: &Repository,
    name: &str,
    url: &str,
    store: &str,
    token_env: Option<String>,
) -> Result<Value, SyncFailure> {
    let config = RemoteConfig {
        url: url.to_string(),
        store: store.to_string(),
        token_env,
    };
    repo.set_remote(name, &config)?;
    Ok(
        json!({ "name": name, "url": config.url, "store": config.store, "token_env": config.token_env }),
    )
}

pub fn remove_remote(repo: &Repository, name: &str) -> Result<Value, SyncFailure> {
    Ok(json!({ "name": name, "removed": repo.remove_remote(name)? }))
}

#[cfg(feature = "sync-client")]
pub fn fetch(repo: &Repository, remote: &str, refs: &[String]) -> Result<Value, SyncFailure> {
    let refs: Vec<&str> = refs.iter().map(String::as_str).collect();
    let report = agentstategraph_sync_http::named::fetch(repo, remote, &refs)?;
    Ok(agentstategraph_sync_http::report::fetch(&report))
}

#[cfg(not(feature = "sync-client"))]
pub fn fetch(_repo: &Repository, _remote: &str, _refs: &[String]) -> Result<Value, SyncFailure> {
    Err(unavailable())
}

#[cfg(feature = "sync-client")]
pub fn push(
    repo: &Repository,
    remote: &str,
    ref_name: &str,
    overrides: Vec<PushOverride>,
) -> Result<Value, SyncFailure> {
    let report = agentstategraph_sync_http::named::push(repo, remote, ref_name, overrides)?;
    Ok(agentstategraph_sync_http::report::push(&report))
}

#[cfg(not(feature = "sync-client"))]
pub fn push(
    _repo: &Repository,
    _remote: &str,
    _ref_name: &str,
    _overrides: Vec<PushOverride>,
) -> Result<Value, SyncFailure> {
    Err(unavailable())
}

/// Fetch `ref_name`, renumber colliding tasks in `/plans`, then merge.
#[cfg(feature = "sync-client")]
pub fn pull(
    repo: Arc<Repository>,
    remote: &str,
    ref_name: &str,
    options: CommitOptions,
) -> Result<Value, SyncFailure> {
    use agentstategraph::sync::tracking_ref;
    let fetched = agentstategraph_sync_http::named::fetch(&repo, remote, &[ref_name])?;
    let tasks = agentstategraph_tasks::TaskStore::new(
        Arc::clone(&repo),
        "/plans",
        options.agent_id.clone(),
    );
    let renumbered = tasks
        .renumber_before_merge(ref_name, &tracking_ref(remote, ref_name))
        .map_err(|e| SyncFailure {
            code: "renumber_failed",
            message: e.to_string(),
            details: None,
        })?;
    let outcome = repo.merge_fetched(remote, ref_name, options)?;
    let mut out = agentstategraph_sync_http::report::pull_outcome(&outcome);
    out["fetch"] = agentstategraph_sync_http::report::fetch(&fetched);
    out["renumbered"] = renumbered
        .renamed
        .iter()
        .map(|(plan, moves)| {
            let moves: Vec<Value> = moves
                .iter()
                .map(|(a, b)| json!([a.to_string(), b.to_string()]))
                .collect();
            (plan.clone(), Value::Array(moves))
        })
        .collect::<serde_json::Map<_, _>>()
        .into();
    out["plan_conflicts"] = json!(renumbered.plan_conflicts);
    Ok(out)
}

#[cfg(not(feature = "sync-client"))]
pub fn pull(
    _repo: Arc<Repository>,
    _remote: &str,
    _ref_name: &str,
    _options: CommitOptions,
) -> Result<Value, SyncFailure> {
    Err(unavailable())
}
