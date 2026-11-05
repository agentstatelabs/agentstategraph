//! The receiving side's record of the pushes it accepted (DESIGN.md Plan G
//! t-006).
//!
//! Agent ids inside commits are the pusher's own claims. The push log is the
//! receiver's record of who delivered what: for each accepted push, the
//! pusher as the receiver authenticated it (an API key's id, say), the ref,
//! its old and new commits, when, and the push filter findings the pusher
//! overrode.
//!
//! It's a chain of commits on [`PUSH_LOG_REF`], one per push, each holding
//! that push's entry, so appending writes one small tree and reading the
//! newest entries walks back from the head. The ref is reserved like the
//! control ref beside it: no ordinary write moves it, `list_branches` and
//! sync leave it out, and a store export doesn't carry it.

use agentstategraph_core::{Authority, CommitBuilder, Intent, IntentCategory, Namespace, ObjectId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::repo::{RepoError, Repository};
use crate::sync::filter::PushOverride;
use crate::tree;

/// The ref the push log's commits hang from, in the default namespace.
pub const PUSH_LOG_REF: &str = "_control-pushlog";

/// How many times an append rebuilds on a moved head.
const MAX_ATTEMPTS: usize = 64;

/// One accepted push.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushLogEntry {
    pub at: DateTime<Utc>,
    /// Who delivered it, as the receiver knows them.
    pub pusher: String,
    pub namespace: String,
    pub name: String,
    /// The ref's commit before, as hex; absent for a ref the push created.
    pub old: Option<String>,
    pub new: String,
    /// Findings the pusher let through, each with its reason.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overrides: Vec<PushOverride>,
}

fn malformed(what: impl std::fmt::Display) -> RepoError {
    RepoError::InvalidOperation(format!("malformed push log: {what}"))
}

impl Repository {
    /// Append `entry` to the push log.
    pub(crate) fn append_push_log(&self, entry: &PushLogEntry) -> Result<ObjectId, RepoError> {
        let store = self.taint_storage();
        let ns = Namespace::default_ns();
        let value = serde_json::to_value(entry).map_err(malformed)?;
        let root = tree::json_to_tree(store, &value)?;
        for _ in 0..MAX_ATTEMPTS {
            let head = store.get_ref(&ns, PUSH_LOG_REF)?;
            let commit = CommitBuilder::new(
                root,
                entry.pusher.clone(),
                Authority::simple(entry.pusher.clone()),
                Intent::new(
                    IntentCategory::Checkpoint,
                    format!("push {}/{}", entry.namespace, entry.name),
                ),
            )
            .parents(head.into_iter().collect())
            .build();
            store.put_commit(&commit)?;
            let landed = match head {
                Some(h) => store.cas_ref(&ns, PUSH_LOG_REF, h, commit.id)?,
                None => store.create_ref(&ns, PUSH_LOG_REF, commit.id)?,
            };
            if landed {
                return Ok(commit.id);
            }
        }
        Err(RepoError::WriteConflict)
    }

    /// The pushes this store accepted, newest first, at most `limit`.
    pub fn push_log(&self, limit: usize) -> Result<Vec<PushLogEntry>, RepoError> {
        let store = self.taint_storage();
        let mut next = store.get_ref(&Namespace::default_ns(), PUSH_LOG_REF)?;
        let mut entries = Vec::new();
        while let Some(id) = next {
            if entries.len() >= limit {
                break;
            }
            let commit = store
                .get_commit(&id)?
                .ok_or_else(|| RepoError::CommitNotFound(id.to_string()))?;
            let object = store
                .get_object(&commit.state_root)?
                .ok_or_else(|| malformed(format!("{id}'s entry isn't here")))?;
            let value = tree::tree_to_json(store, &object)?;
            entries.push(serde_json::from_value(value).map_err(malformed)?);
            next = commit.parents.first().copied();
        }
        Ok(entries)
    }
}
