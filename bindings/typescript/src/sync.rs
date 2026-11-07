//! Sync through named remotes (DESIGN.md Plan G t-008): `remoteAdd`,
//! `remotes`, `remoteRemove`, `fetch`, `push` and `pull`. A remote's token
//! comes from the environment variable it names. The network calls block
//! until they finish.

use agentstategraph::CommitOptions;
use agentstategraph::sync::{PushOverride, RemoteConfig};
use agentstategraph_core::IntentCategory;
use agentstategraph_sync_http::{named, report};
use napi_derive::napi;

use crate::{AgentStateGraph, err};

/// A push filter finding to let through, with why.
#[napi(object)]
pub struct SyncOverride {
    pub finding: String,
    pub reason: String,
}

#[napi]
impl AgentStateGraph {
    /// Add a sync remote, or change where it is. `tokenEnv` names the
    /// environment variable holding the hub's API key; the key itself is
    /// never stored.
    #[napi]
    pub fn remote_add(
        &self,
        name: String,
        url: String,
        store: Option<String>,
        token_env: Option<String>,
    ) -> napi::Result<()> {
        let config = RemoteConfig {
            url,
            store: store.unwrap_or_else(|| "default".to_string()),
            token_env,
        };
        self.repo.set_remote(&name, &config).map_err(err)
    }

    /// Every sync remote, with where its branches were at the last fetch
    /// or push.
    #[napi]
    pub fn remotes(&self) -> napi::Result<serde_json::Value> {
        report::remotes(&self.repo).map_err(err)
    }

    /// Remove a sync remote and where its branches were last seen.
    #[napi]
    pub fn remote_remove(&self, name: String) -> napi::Result<bool> {
        self.repo.remove_remote(&name).map_err(err)
    }

    /// Fetch branches from a remote (all of them when `refs` is omitted),
    /// recording where they are as `remotes/<remote>/<branch>`.
    #[napi]
    pub fn fetch(
        &self,
        remote: String,
        refs: Option<Vec<String>>,
    ) -> napi::Result<serde_json::Value> {
        let refs = refs.unwrap_or_default();
        let refs: Vec<&str> = refs.iter().map(String::as_str).collect();
        let fetched = named::fetch(&self.repo, &remote, &refs).map_err(err)?;
        Ok(report::fetch(&fetched))
    }

    /// Push a branch to a remote: only forward, and never secrets,
    /// local-only branches, or credential-like values unless `overrides`
    /// lets each finding through.
    #[napi]
    pub fn push(
        &self,
        remote: String,
        reference: Option<String>,
        overrides: Option<Vec<SyncOverride>>,
    ) -> napi::Result<serde_json::Value> {
        let overrides = overrides
            .unwrap_or_default()
            .into_iter()
            .map(|o| PushOverride {
                finding: o.finding,
                reason: o.reason,
            })
            .collect();
        let reference = reference.unwrap_or_else(|| "main".to_string());
        let pushed = named::push(&self.repo, &remote, &reference, overrides).map_err(err)?;
        Ok(report::push(&pushed))
    }

    /// Fetch a branch from a remote and merge it into the local one. With
    /// `allowUnrelated`, a branch whose history shares no commit with the
    /// remote's joins it, merged against an empty base.
    #[napi]
    pub fn pull(
        &self,
        remote: String,
        reference: Option<String>,
        description: Option<String>,
        agent: Option<String>,
        allow_unrelated: Option<bool>,
    ) -> napi::Result<serde_json::Value> {
        let options = CommitOptions::new(
            agent.unwrap_or_else(|| "typescript".to_string()),
            IntentCategory::Merge,
            description.unwrap_or_else(|| format!("pull from {remote}")),
        );
        let reference = reference.unwrap_or_else(|| "main".to_string());
        let pulled = if allow_unrelated.unwrap_or(false) {
            named::join(&self.repo, &remote, &reference, options)
        } else {
            named::pull(&self.repo, &remote, &reference, options)
        }
        .map_err(err)?;
        Ok(report::pull(&pulled))
    }
}
