//! Push, fetch and pull by a remote's name, as the store keeps it
//! ([`Repository::set_remote`]): what the MCP server, the HTTP API and the
//! bindings call. A remote's token comes from the environment variable it
//! names, read each time it's used.

use agentstategraph::sync::{
    FetchReport, PullReport, PushOptions, PushOverride, PushReport, RemoteConfig, SyncError,
};
use agentstategraph::{CommitOptions, Repository};

use crate::client::HttpRemote;

impl HttpRemote {
    /// The remote `config` describes, with its token if it names one.
    pub fn from_config(config: &RemoteConfig) -> Result<Self, SyncError> {
        let remote = HttpRemote::new(&config.url, &config.store);
        Ok(match &config.token_env {
            None => remote,
            Some(var) => match std::env::var(var) {
                Ok(token) if !token.is_empty() => remote.with_token(token),
                _ => {
                    return Err(SyncError::Remote(format!(
                        "set {var} to the token for {}",
                        config.url
                    )));
                }
            },
        })
    }
}

/// The remote `repo` calls `name`.
pub fn open(repo: &Repository, name: &str) -> Result<HttpRemote, SyncError> {
    let config = repo
        .remote_config(name)?
        .ok_or_else(|| SyncError::NoSuchRemote(name.to_string()))?;
    HttpRemote::from_config(&config)
}

/// [`Repository::fetch`] from the remote named `name`.
pub fn fetch(repo: &Repository, name: &str, refs: &[&str]) -> Result<FetchReport, SyncError> {
    repo.fetch(name, &open(repo, name)?, refs)
}

/// [`Repository::push_with`] to the remote named `name`, with the default
/// filters and `overrides`.
pub fn push(
    repo: &Repository,
    name: &str,
    ref_name: &str,
    overrides: Vec<PushOverride>,
) -> Result<PushReport, SyncError> {
    let options = PushOptions {
        overrides,
        ..PushOptions::default()
    };
    repo.push_with(name, &open(repo, name)?, ref_name, &options)
}

/// [`Repository::pull`] from the remote named `name`.
pub fn pull(
    repo: &Repository,
    name: &str,
    ref_name: &str,
    options: CommitOptions,
) -> Result<PullReport, SyncError> {
    repo.pull(name, &open(repo, name)?, ref_name, options)
}

/// [`Repository::join`] from the remote named `name`: a pull that also
/// joins unrelated histories.
pub fn join(
    repo: &Repository,
    name: &str,
    ref_name: &str,
    options: CommitOptions,
) -> Result<PullReport, SyncError> {
    repo.join(name, &open(repo, name)?, ref_name, options)
}
