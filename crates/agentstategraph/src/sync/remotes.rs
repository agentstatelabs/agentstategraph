//! Named remotes: where a store's remotes are, kept in the store (DESIGN.md
//! Plan G t-008).
//!
//! A remote's name, URL and store are kept on the reserved ref
//! [`REMOTES_REF`], which stays local like the control ref's other
//! neighbours: no sync, export or ordinary write touches it. Its token
//! isn't kept at all: a remote names the environment variable that holds
//! it, read when the remote is used, so no credential sits in the store.
//! The MCP server, the HTTP API and the bindings use these to push, fetch
//! and pull by name (`agentstategraph_sync_http::named`).

use std::collections::BTreeMap;

use agentstategraph_core::{Authority, CommitBuilder, Intent, IntentCategory, Namespace, ObjectId};
use serde::{Deserialize, Serialize};

use crate::repo::{RepoError, Repository, TRACKING_PREFIX};
use crate::sync::remote::check_remote_name;
use crate::tree;

/// The ref the remotes' list hangs from, in the default namespace.
pub const REMOTES_REF: &str = "_control-remotes";

/// How many times a change rebuilds on a moved head.
const MAX_ATTEMPTS: usize = 64;

/// Where a remote is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteConfig {
    /// The host's base URL, `https://hub.example.com`; the store is served
    /// below it at `/sync/v1/<store>`.
    pub url: String,
    pub store: String,
    /// The environment variable holding the bearer token, if the remote
    /// needs one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_env: Option<String>,
}

impl RemoteConfig {
    fn check(&self) -> Result<(), RepoError> {
        let bad = |what: String| Err(RepoError::InvalidOperation(what));
        if !(self.url.starts_with("https://") || self.url.starts_with("http://")) {
            return bad(format!(
                "a remote's URL starts with https://: {:?}",
                self.url
            ));
        }
        let simple = |c: char| c.is_ascii_alphanumeric() || "-_.".contains(c);
        if self.store.is_empty() || !self.store.chars().all(simple) {
            return bad(format!("not a store name: {:?}", self.store));
        }
        if let Some(var) = &self.token_env {
            let env_char = |c: char| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_';
            if var.is_empty() || !var.chars().all(env_char) {
                return bad(format!("not an environment variable's name: {var:?}"));
            }
        }
        Ok(())
    }
}

fn malformed(what: impl std::fmt::Display) -> RepoError {
    RepoError::InvalidOperation(format!("malformed remotes list: {what}"))
}

impl Repository {
    /// Every named remote.
    pub fn remotes(&self) -> Result<BTreeMap<String, RemoteConfig>, RepoError> {
        let store = self.taint_storage();
        let Some(head) = store.get_ref(&Namespace::default_ns(), REMOTES_REF)? else {
            return Ok(BTreeMap::new());
        };
        let commit = store
            .get_commit(&head)?
            .ok_or_else(|| RepoError::CommitNotFound(head.to_string()))?;
        let object = store
            .get_object(&commit.state_root)?
            .ok_or_else(|| malformed("its tree isn't here"))?;
        serde_json::from_value(tree::tree_to_json(store, &object)?).map_err(malformed)
    }

    /// The remote named `name`, if there is one.
    pub fn remote_config(&self, name: &str) -> Result<Option<RemoteConfig>, RepoError> {
        Ok(self.remotes()?.remove(name))
    }

    /// Add the remote `name`, or change where it is.
    pub fn set_remote(&self, name: &str, config: &RemoteConfig) -> Result<(), RepoError> {
        check_remote_name(name).map_err(|e| RepoError::InvalidOperation(e.to_string()))?;
        config.check()?;
        self.change_remotes(&format!("set remote {name}"), |remotes| {
            remotes.insert(name.to_string(), config.clone());
        })
    }

    /// Remove the remote `name` and its remote-tracking refs, in every
    /// namespace. Returns whether there was one.
    pub fn remove_remote(&self, name: &str) -> Result<bool, RepoError> {
        check_remote_name(name).map_err(|e| RepoError::InvalidOperation(e.to_string()))?;
        let mut existed = false;
        self.change_remotes(&format!("remove remote {name}"), |remotes| {
            existed = remotes.remove(name).is_some();
        })?;
        let store = self.taint_storage();
        let prefix = format!("{TRACKING_PREFIX}{name}/");
        for ns in store.list_namespaces()? {
            for (tracking, target) in store.list_refs(&ns, &prefix)? {
                store.cas_delete_ref(&ns, &tracking, target)?;
                existed = true;
            }
        }
        Ok(existed)
    }

    /// Commit `change` to the remotes list, rebuilt when another writer
    /// moved it first. A change that leaves the list as it was commits
    /// nothing.
    fn change_remotes(
        &self,
        what: &str,
        mut change: impl FnMut(&mut BTreeMap<String, RemoteConfig>),
    ) -> Result<(), RepoError> {
        let store = self.taint_storage();
        let ns = Namespace::default_ns();
        for _ in 0..MAX_ATTEMPTS {
            let head: Option<ObjectId> = store.get_ref(&ns, REMOTES_REF)?;
            let mut remotes = self.remotes()?;
            let before = remotes.clone();
            change(&mut remotes);
            if remotes == before && head.is_some() {
                return Ok(());
            }
            let value = serde_json::to_value(&remotes).map_err(malformed)?;
            let root = tree::json_to_tree(store, &value)?;
            let commit = CommitBuilder::new(
                root,
                "system",
                Authority::simple("system"),
                Intent::new(IntentCategory::Checkpoint, what),
            )
            .parents(head.into_iter().collect())
            .build();
            store.put_commit(&commit)?;
            let landed = match head {
                Some(h) => store.cas_ref(&ns, REMOTES_REF, h, commit.id)?,
                None => store.create_ref(&ns, REMOTES_REF, commit.id)?,
            };
            if landed {
                return Ok(());
            }
        }
        Err(RepoError::WriteConflict)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::{InProcessRemote, Remote};
    use agentstategraph_storage::{MemoryStorage, Storage};
    use std::sync::Arc;

    fn repo() -> Repository {
        Repository::from_shared(Arc::new(MemoryStorage::new()) as Arc<dyn Storage + Send + Sync>)
    }

    fn hub_config() -> RemoteConfig {
        RemoteConfig {
            url: "https://hub.example.com".into(),
            store: "default".into(),
            token_env: Some("ASG_HUB_TOKEN".into()),
        }
    }

    #[test]
    fn remotes_are_kept_changed_and_removed() {
        let a = repo();
        assert!(a.remotes().unwrap().is_empty());
        a.set_remote("hub", &hub_config()).unwrap();
        assert_eq!(a.remote_config("hub").unwrap(), Some(hub_config()));
        let moved = RemoteConfig {
            url: "https://other.example.com".into(),
            ..hub_config()
        };
        a.set_remote("hub", &moved).unwrap();
        assert_eq!(a.remotes().unwrap().len(), 1);
        assert_eq!(a.remote_config("hub").unwrap().unwrap().url, moved.url);

        // Removing one takes its tracking refs with it.
        a.init().unwrap();
        let hub = repo();
        a.push("hub", &InProcessRemote::new(&hub), "main").unwrap();
        assert_eq!(a.remote_refs("hub").unwrap().len(), 1);
        assert!(a.remove_remote("hub").unwrap());
        assert!(a.remote_config("hub").unwrap().is_none());
        assert!(a.remote_refs("hub").unwrap().is_empty());
        assert!(!a.remove_remote("hub").unwrap());
    }

    #[test]
    fn a_bad_remote_is_refused_and_the_list_never_syncs() {
        let a = repo();
        for config in [
            RemoteConfig {
                url: "ftp://x".into(),
                ..hub_config()
            },
            RemoteConfig {
                store: "a/b".into(),
                ..hub_config()
            },
            RemoteConfig {
                token_env: Some("lower".into()),
                ..hub_config()
            },
        ] {
            assert!(a.set_remote("hub", &config).is_err(), "{config:?}");
        }
        assert!(a.set_remote("bad/name", &hub_config()).is_err());

        a.init().unwrap();
        a.set_remote("hub", &hub_config()).unwrap();
        assert_eq!(a.list_branches(None).unwrap().len(), 1);
        let offered = InProcessRemote::new(&a).list_refs().unwrap();
        assert!(offered.iter().all(|r| r.name != REMOTES_REF), "{offered:?}");
    }
}
