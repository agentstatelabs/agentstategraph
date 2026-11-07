//! Sync through named remotes (DESIGN.md Plan G t-008): `remote_add`,
//! `remotes`, `remote_remove`, `fetch`, `push` and `pull`. A remote's token
//! comes from the environment variable it names. Network calls release the
//! GIL.

use agentstategraph::CommitOptions;
use agentstategraph::sync::{PushOverride, RemoteConfig};
use agentstategraph_core::IntentCategory;
use agentstategraph_sync_http::{named, report};
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

use crate::{AgentStateGraph, json_to_py};

fn err(e: impl std::fmt::Display) -> PyErr {
    PyRuntimeError::new_err(e.to_string())
}

#[pymethods]
impl AgentStateGraph {
    /// Add a sync remote, or change where it is. `token_env` names the
    /// environment variable holding the hub's API key; the key itself is
    /// never stored.
    #[pyo3(signature = (name, url, store="default", token_env=None))]
    fn remote_add(
        &self,
        name: &str,
        url: &str,
        store: &str,
        token_env: Option<String>,
    ) -> PyResult<()> {
        let config = RemoteConfig {
            url: url.to_string(),
            store: store.to_string(),
            token_env,
        };
        self.repo.set_remote(name, &config).map_err(err)
    }

    /// Every sync remote, with where its branches were at the last fetch
    /// or push.
    fn remotes(&self, py: Python<'_>) -> PyResult<PyObject> {
        json_to_py(py, &report::remotes(&self.repo).map_err(err)?)
    }

    /// Remove a sync remote and where its branches were last seen.
    fn remote_remove(&self, name: &str) -> PyResult<bool> {
        self.repo.remove_remote(name).map_err(err)
    }

    /// Fetch branches from a remote (all of them when `refs` is None),
    /// recording where they are as `remotes/<remote>/<branch>`.
    #[pyo3(signature = (remote, refs=None))]
    fn fetch(&self, py: Python<'_>, remote: &str, refs: Option<Vec<String>>) -> PyResult<PyObject> {
        let repo = self.repo.clone();
        let refs = refs.unwrap_or_default();
        let done = py.allow_threads(move || {
            let refs: Vec<&str> = refs.iter().map(String::as_str).collect();
            named::fetch(&repo, remote, &refs).map(|r| report::fetch(&r))
        });
        json_to_py(py, &done.map_err(err)?)
    }

    /// Push a branch to a remote: only forward, and never secrets,
    /// local-only branches, or credential-like values unless `overrides`
    /// (pairs of a finding's id and a reason) lets each finding through.
    #[pyo3(signature = (remote, r#ref="main", overrides=None))]
    fn push(
        &self,
        py: Python<'_>,
        remote: &str,
        r#ref: &str,
        overrides: Option<Vec<(String, String)>>,
    ) -> PyResult<PyObject> {
        let repo = self.repo.clone();
        let overrides = overrides
            .unwrap_or_default()
            .into_iter()
            .map(|(finding, reason)| PushOverride { finding, reason })
            .collect();
        let done = py.allow_threads(move || {
            named::push(&repo, remote, r#ref, overrides).map(|r| report::push(&r))
        });
        json_to_py(py, &done.map_err(err)?)
    }

    /// Fetch a branch from a remote and merge it into the local one. With
    /// `allow_unrelated`, a branch whose history shares no commit with the
    /// remote's joins it, merged against an empty base.
    #[pyo3(signature = (remote, r#ref="main", description=None, agent=None, allow_unrelated=false))]
    fn pull(
        &self,
        py: Python<'_>,
        remote: &str,
        r#ref: &str,
        description: Option<String>,
        agent: Option<String>,
        allow_unrelated: bool,
    ) -> PyResult<PyObject> {
        let repo = self.repo.clone();
        let options = CommitOptions::new(
            agent.unwrap_or_else(|| "python".to_string()),
            IntentCategory::Merge,
            description.unwrap_or_else(|| format!("pull from {remote}")),
        );
        let done = py.allow_threads(move || {
            if allow_unrelated {
                named::join(&repo, remote, r#ref, options)
            } else {
                named::pull(&repo, remote, r#ref, options)
            }
            .map(|r| report::pull(&r))
        });
        json_to_py(py, &done.map_err(err)?)
    }
}
