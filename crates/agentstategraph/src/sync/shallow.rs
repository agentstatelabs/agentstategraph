//! Shallow history (DESIGN.md Plan G t-007).
//!
//! A store can start syncing from a chosen commit: a push with
//! [`PushOptions::shallow`](crate::sync::PushOptions::shallow) sends those
//! commits without their parents, and a fetch with a
//! [`FetchOptions::depth`] takes only that many commits along each line from
//! the heads. The receiving store records each commit it holds without its
//! parents as a boundary, a reserved ref `_control-shallow/<commit>`, so it
//! can tell a history cut on purpose from a damaged one. A boundary goes
//! once its parents arrive, as with a fetch with
//! [`FetchOptions::unshallow`].
//!
//! History walks stop at a boundary: `log`, `log_dag` and the GC's
//! reachability walk end there, `merge_base` treats the boundary as a root,
//! and `blame` names the boundary for a value it already held. Two stores
//! whose histories meet only past a boundary have no merge base, as with a
//! shallow git clone.

use std::collections::{HashMap, HashSet, VecDeque};

use agentstategraph_core::{Namespace, ObjectId};
use agentstategraph_storage::Storage;

use crate::repo::{RepoError, Repository};

/// Refs recording a boundary: `<prefix><commit hex>`, pointing at the
/// commit, in the default namespace.
pub const SHALLOW_PREFIX: &str = "_control-shallow/";

/// How a fetch is limited.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FetchOptions {
    /// Take at most this many commits along each line from a fetched ref's
    /// head (1 is the head alone), and record where the history stops. A
    /// commit this store already has stops the walk sooner, as always.
    pub depth: Option<u32>,
    /// Take the remote's whole history again, telling it nothing of what
    /// this store has, so the boundaries whose parents it holds go. A
    /// plain fetch never deepens a shallow history: what this store has
    /// tells the remote it holds everything below.
    pub unshallow: bool,
}

/// The commits at `depth` along each line from `heads`, which a fetch of
/// that depth sends without their parents. Commits nearer a head by any
/// line aren't boundaries, and ones that have no parents aren't either.
pub(crate) fn depth_boundary(
    store: &dyn Storage,
    heads: &[ObjectId],
    depth: u32,
) -> Result<Vec<ObjectId>, RepoError> {
    if depth == 0 {
        return Err(RepoError::InvalidOperation(
            "a fetch's depth is at least 1, the head itself".into(),
        ));
    }
    let mut level: HashMap<ObjectId, u32> = HashMap::new();
    let mut queue: VecDeque<ObjectId> = VecDeque::new();
    for head in heads {
        if level.insert(*head, 1).is_none() {
            queue.push_back(*head);
        }
    }
    let mut boundary = Vec::new();
    while let Some(id) = queue.pop_front() {
        let at = level[&id];
        let Some(commit) = store.get_commit(&id)? else {
            continue;
        };
        if at == depth {
            if !commit.parents.is_empty() {
                boundary.push(id);
            }
            continue;
        }
        for parent in commit.parents {
            if let std::collections::hash_map::Entry::Vacant(e) = level.entry(parent) {
                e.insert(at + 1);
                queue.push_back(parent);
            }
        }
    }
    boundary.sort();
    Ok(boundary)
}

impl Repository {
    /// The commits this store holds without their parents, where a shallow
    /// push, fetch or import started its history.
    pub fn shallow_commits(&self) -> Result<Vec<ObjectId>, RepoError> {
        let store = self.taint_storage();
        Ok(store
            .list_refs(&Namespace::default_ns(), SHALLOW_PREFIX)?
            .into_iter()
            .map(|(_, target)| target)
            .collect())
    }

    /// Record as boundaries the commits in `shallow` whose parents aren't
    /// all here, and drop the boundaries whose parents now are.
    pub(crate) fn record_shallow(
        &self,
        shallow: impl IntoIterator<Item = ObjectId>,
    ) -> Result<(), RepoError> {
        let store = self.taint_storage();
        let ns = Namespace::default_ns();
        let mut seen: HashSet<ObjectId> = HashSet::new();
        for id in shallow {
            if !seen.insert(id) {
                continue;
            }
            if !self.has_all_parents(&id)? {
                store.set_ref(&ns, &format!("{SHALLOW_PREFIX}{}", id.to_hex()), id)?;
            }
        }
        for (name, id) in store.list_refs(&ns, SHALLOW_PREFIX)? {
            if self.has_all_parents(&id)? {
                store.cas_delete_ref(&ns, &name, id)?;
            }
        }
        Ok(())
    }

    /// Whether this store has `commit` and every one of its parents.
    fn has_all_parents(&self, commit: &ObjectId) -> Result<bool, RepoError> {
        let store = self.taint_storage();
        let Some(c) = store.get_commit(commit)? else {
            return Ok(false);
        };
        for parent in &c.parents {
            if !store.has_commit(parent)? {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CommitOptions;
    use crate::sync::{InProcessRemote, PushOptions, SyncError};
    use agentstategraph_core::IntentCategory;
    use agentstategraph_storage::MemoryStorage;
    use serde_json::json;
    use std::sync::Arc;

    fn repo() -> Repository {
        Repository::from_shared(Arc::new(MemoryStorage::new()) as Arc<dyn Storage + Send + Sync>)
    }

    fn opts(what: &str) -> CommitOptions {
        CommitOptions::new("agent/shallow", IntentCategory::Refine, what)
    }

    /// A store that set `/keep` once, then `/v` five times: the ids of the
    /// five, oldest first.
    fn five() -> (Repository, Vec<ObjectId>) {
        let a = repo();
        a.init().unwrap();
        a.set_json("main", "/keep", &json!("old"), opts("keep"))
            .unwrap();
        let ids = (0..5)
            .map(|i| a.set_json("main", "/v", &json!(i), opts("v")).unwrap())
            .collect();
        (a, ids)
    }

    fn shallow_hub(a: &Repository, from: ObjectId) -> Repository {
        let hub = repo();
        let options = PushOptions::default().shallow_from(from);
        a.push_with("hub", &InProcessRemote::new(&hub), "main", &options)
            .unwrap();
        hub
    }

    #[test]
    fn a_shallow_push_starts_the_remotes_history_at_the_boundary() {
        let (a, ids) = five();
        let hub = shallow_hub(&a, ids[2]);
        assert_eq!(hub.get_json("main", "/v").unwrap(), json!(4));
        assert_eq!(hub.get_json("main", "/keep").unwrap(), json!("old"));
        assert_eq!(hub.shallow_commits().unwrap(), vec![ids[2]]);
        assert_eq!(hub.log("main", 100).unwrap().len(), 3);
        let walk = hub.log_dag("main", 100).unwrap();
        assert_eq!(walk.commits.len(), 3);
        assert!(!walk.truncated);
        assert_eq!(hub.blame("main", "/v").unwrap().commit_id, ids[4].short());
        assert_eq!(
            hub.blame("main", "/keep").unwrap().commit_id,
            ids[2].short(),
            "a value from before the boundary is blamed on the boundary"
        );

        // Later pushes are ordinary.
        a.set_json("main", "/v", &json!(5), opts("v")).unwrap();
        let report = a.push("hub", &InProcessRemote::new(&hub), "main").unwrap();
        assert_eq!(report.commits, 1, "{report:?}");
        assert_eq!(hub.shallow_commits().unwrap(), vec![ids[2]]);
    }

    #[test]
    fn a_store_fetching_from_a_shallow_one_is_shallow_too_until_the_rest_arrives() {
        let (a, ids) = five();
        let hub = shallow_hub(&a, ids[2]);
        let b = repo();
        b.fetch("hub", &InProcessRemote::new(&hub), &[]).unwrap();
        assert_eq!(b.shallow_commits().unwrap(), vec![ids[2]]);
        assert_eq!(b.get_json("remotes/hub/main", "/v").unwrap(), json!(4));

        b.fetch("a", &InProcessRemote::new(&a), &[]).unwrap();
        assert_eq!(
            b.shallow_commits().unwrap(),
            vec![ids[2]],
            "a plain fetch doesn't deepen"
        );
        let unshallow = FetchOptions {
            unshallow: true,
            ..FetchOptions::default()
        };
        b.fetch_with("a", &InProcessRemote::new(&a), &[], &unshallow)
            .unwrap();
        assert!(
            b.shallow_commits().unwrap().is_empty(),
            "the boundary's parents arrived"
        );
        assert_eq!(b.log("remotes/a/main", 100).unwrap().len(), 7);
    }

    #[test]
    fn a_fetch_with_a_depth_takes_that_much_history() {
        let (a, ids) = five();
        let b = repo();
        let options = FetchOptions {
            depth: Some(2),
            ..FetchOptions::default()
        };
        let fetched = b
            .fetch_with("a", &InProcessRemote::new(&a), &["main"], &options)
            .unwrap();
        assert_eq!(fetched.commits, 2, "{fetched:?}");
        assert_eq!(b.shallow_commits().unwrap(), vec![ids[3]]);
        assert_eq!(b.head("remotes/a/main").unwrap(), ids[4]);
        assert_eq!(b.log("remotes/a/main", 100).unwrap().len(), 2);

        let none = FetchOptions {
            depth: Some(0),
            ..FetchOptions::default()
        };
        assert!(
            repo()
                .fetch_with("a", &InProcessRemote::new(&a), &["main"], &none)
                .is_err()
        );
    }

    #[test]
    fn a_shallow_push_checks_the_whole_tree_it_sends() {
        let a = repo();
        a.init().unwrap();
        let token = ["gh", "p_", &"c3".repeat(18)].concat();
        a.set_json("main", "/old", &json!(token), opts("old"))
            .unwrap();
        let from = a.set_json("main", "/v", &json!(1), opts("v")).unwrap();
        let hub = repo();
        let options = PushOptions::default().shallow_from(from);
        match a.push_with("hub", &InProcessRemote::new(&hub), "main", &options) {
            Err(SyncError::Blocked { findings, .. }) => assert_eq!(findings[0].path, "/old"),
            other => panic!("expected a blocked push, got {other:?}"),
        }
    }
}
