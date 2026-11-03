//! Missing sets (DESIGN.md Plan G t-003): the commits and objects a remote
//! lacks, so a push or fetch sends those and little else.
//!
//! The commits are those reachable from the heads being sent and not from
//! any commit the remote is known to have. The walk runs from both sides at
//! once, newest commit first, as git's does. A commit reached from one the
//! remote has is marked as one the remote has, and the mark spreads to its
//! parents. The walk stops once nothing left to walk could be new, and once
//! every commit the remote has that's as new as the oldest new commit has
//! spread its mark. Marks only spread from commits the remote has, so a clock
//! that's wrong (a commit dated before its parent) can only make the walk
//! stop early and list a commit the remote already has; it never leaves out
//! one the remote lacks. With timestamps that never go backwards along a
//! parent edge, the commits are exactly those the remote lacks.
//!
//! The objects come from diffing each new commit's tree against its parents'
//! trees, node by node. A child the parent's node at the same path also has
//! is skipped whole, since the remote has the parent's tree or is about to
//! get it: git's thin pack. So the objects are never fewer than the remote
//! needs, but may include a few it has, such as a subtree moved to a new
//! path.
//!
//! Two assumptions: the remote has the whole tree of every commit it has
//! (sync never sends less), and a commit a remote has comes with all its
//! ancestors, except below a shallow boundary.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};

use agentstategraph_core::{Node, Object, ObjectId};
use agentstategraph_storage::{CommitStore, ObjectStore, StorageError};
use chrono::{DateTime, Utc};

/// How many objects one storage read asks for.
const READ_BATCH: usize = 1024;

/// What a remote lacks to take some heads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MissingSet {
    /// The commits the remote lacks, each after its parents.
    pub commits: Vec<ObjectId>,
    /// The objects their trees need that the remote may lack, each after
    /// the node that refers to it.
    pub objects: Vec<ObjectId>,
    /// Of `commits`, those sent without their parents: the shallow
    /// boundaries asked for, and any commit a parent of which this store no
    /// longer has (pruned). The receiver needs to accept them as shallow.
    pub shallow: Vec<ObjectId>,
}

impl MissingSet {
    pub fn is_empty(&self) -> bool {
        self.commits.is_empty()
    }
}

/// What the walk keeps of each commit it reads.
struct Walked {
    timestamp: DateTime<Utc>,
    parents: Vec<ObjectId>,
    state_root: ObjectId,
}

#[derive(Default, Clone, Copy)]
struct Mark {
    /// Reachable from a commit the remote has.
    remote_has: bool,
    /// The remote-has mark has been passed to the parents.
    spread: bool,
    /// Queued from a head's side.
    queued: bool,
    /// Walked from a head's side.
    walked: bool,
}

/// The walk's state: the commits read so far and their marks.
struct Walk<'s, S: ?Sized> {
    store: &'s S,
    commits: HashMap<ObjectId, Walked>,
    marks: HashMap<ObjectId, Mark>,
    queue: BinaryHeap<(DateTime<Utc>, ObjectId)>,
    /// Queued commits that may still be new.
    pending: HashSet<ObjectId>,
}

impl<S: CommitStore + ?Sized> Walk<'_, S> {
    /// Read a commit into the walk; `false` if the store hasn't got it.
    fn load(&mut self, id: &ObjectId) -> Result<bool, StorageError> {
        if self.commits.contains_key(id) {
            return Ok(true);
        }
        let Some(commit) = self.store.get_commit(id)? else {
            return Ok(false);
        };
        self.commits.insert(
            *id,
            Walked {
                timestamp: commit.timestamp,
                parents: commit.parents,
                state_root: commit.state_root,
            },
        );
        Ok(true)
    }

    fn mark(&mut self, id: &ObjectId) -> &mut Mark {
        self.marks.entry(*id).or_default()
    }

    fn remote_has(&self, id: &ObjectId) -> bool {
        self.marks.get(id).is_some_and(|m| m.remote_has)
    }

    fn queue_new(&mut self, id: ObjectId) {
        let m = self.mark(&id);
        if m.remote_has || m.queued {
            return;
        }
        m.queued = true;
        self.pending.insert(id);
        self.queue.push((self.commits[&id].timestamp, id));
    }

    fn mark_remote_has(&mut self, id: ObjectId) {
        let m = self.mark(&id);
        if m.remote_has {
            return;
        }
        m.remote_has = true;
        self.pending.remove(&id);
        self.queue.push((self.commits[&id].timestamp, id));
    }

    fn spread(&mut self, id: &ObjectId) -> Result<(), StorageError> {
        let m = self.mark(id);
        if m.spread {
            return Ok(());
        }
        m.spread = true;
        for parent in self.commits[id].parents.clone() {
            if self.load(&parent)? {
                self.mark_remote_has(parent);
            }
        }
        Ok(())
    }
}

/// The commits and objects a remote lacks to take `heads`, given commits it
/// has (`haves`, typically the remote-tracking heads from the last fetch;
/// empty for a remote that has nothing) and commits to send without their
/// parents (`shallow`, to start a remote's history at a chosen commit).
///
/// A have this store hasn't got is ignored. A head it hasn't got is
/// [`StorageError::CommitNotFound`], and an object a new commit's tree needs
/// that it hasn't got is [`StorageError::ObjectNotFound`]: the store can't
/// send what it lacks.
pub fn missing_set<S>(
    store: &S,
    heads: &[ObjectId],
    haves: &[ObjectId],
    shallow: &[ObjectId],
) -> Result<MissingSet, StorageError>
where
    S: CommitStore + ObjectStore + ?Sized,
{
    let shallow: HashSet<ObjectId> = shallow.iter().copied().collect();
    let mut walk = Walk {
        store,
        commits: HashMap::new(),
        marks: HashMap::new(),
        queue: BinaryHeap::new(),
        pending: HashSet::new(),
    };
    for head in heads {
        if !walk.load(head)? {
            return Err(StorageError::CommitNotFound(head.to_string()));
        }
        walk.queue_new(*head);
    }
    for have in haves {
        if walk.load(have)? {
            walk.mark_remote_has(*have);
        }
    }

    let mut walked: Vec<ObjectId> = Vec::new();
    let mut lacks_parent: HashSet<ObjectId> = HashSet::new();
    let mut oldest_new: Option<DateTime<Utc>> = None;
    while let Some(&(timestamp, _)) = walk.queue.peek() {
        // Done once nothing queued could be new, and every commit the remote
        // has that's as new as the oldest new commit has spread its mark
        // (which settles commits with equal timestamps).
        if walk.pending.is_empty() && oldest_new.is_none_or(|oldest| timestamp < oldest) {
            break;
        }
        let (timestamp, id) = walk.queue.pop().expect("peeked");
        walk.pending.remove(&id);
        let mark = walk.marks[&id];
        if mark.remote_has {
            walk.spread(&id)?;
            continue;
        }
        if mark.walked {
            continue;
        }
        walk.mark(&id).walked = true;
        walked.push(id);
        oldest_new = Some(oldest_new.map_or(timestamp, |t| t.min(timestamp)));
        if shallow.contains(&id) {
            continue;
        }
        for parent in walk.commits[&id].parents.clone() {
            if walk.load(&parent)? {
                walk.queue_new(parent);
            } else {
                lacks_parent.insert(id);
            }
        }
    }

    let new: HashSet<ObjectId> = walked
        .into_iter()
        .filter(|id| !walk.remote_has(id))
        .collect();
    let commits = parents_first(&walk.commits, &new);
    let shallow_out: Vec<ObjectId> = commits
        .iter()
        .filter(|id| shallow.contains(id) || lacks_parent.contains(id))
        .copied()
        .collect();

    let mut sent: HashSet<ObjectId> = HashSet::new();
    let mut objects = Vec::new();
    for id in &commits {
        let commit = &walk.commits[id];
        // Trees the remote has, or gets with this send.
        let bases: Vec<ObjectId> = commit
            .parents
            .iter()
            .filter(|p| new.contains(p) || walk.remote_has(p))
            .filter_map(|p| walk.commits.get(p).map(|c| c.state_root))
            .collect();
        diff_tree(store, commit.state_root, bases, &mut sent, &mut objects)?;
    }

    Ok(MissingSet {
        commits,
        objects,
        shallow: shallow_out,
    })
}

/// `new` ordered so each commit comes after its parents in `new`; among
/// commits that are ready together, oldest first.
fn parents_first(commits: &HashMap<ObjectId, Walked>, new: &HashSet<ObjectId>) -> Vec<ObjectId> {
    let mut waiting: HashMap<ObjectId, usize> = HashMap::new();
    let mut children: HashMap<ObjectId, Vec<ObjectId>> = HashMap::new();
    for id in new {
        let parents: HashSet<&ObjectId> = commits[id]
            .parents
            .iter()
            .filter(|p| new.contains(p))
            .collect();
        waiting.insert(*id, parents.len());
        for p in parents {
            children.entry(*p).or_default().push(*id);
        }
    }
    let mut ready: BinaryHeap<Reverse<(DateTime<Utc>, ObjectId)>> = waiting
        .iter()
        .filter(|(_, n)| **n == 0)
        .map(|(id, _)| Reverse((commits[id].timestamp, *id)))
        .collect();
    let mut order = Vec::with_capacity(new.len());
    while let Some(Reverse((_, id))) = ready.pop() {
        order.push(id);
        for child in children.remove(&id).unwrap_or_default() {
            let n = waiting.get_mut(&child).expect("a new commit");
            *n -= 1;
            if *n == 0 {
                ready.push(Reverse((commits[&child].timestamp, child)));
            }
        }
    }
    order
}

/// Where a child sits in its parent node, to find the node at the same path
/// in a base tree.
enum Slot<'a> {
    Key(&'a str),
    Index(usize),
    /// A set's members have no path.
    Member,
}

fn children(object: &Object) -> Vec<(Slot<'_>, ObjectId)> {
    match object {
        Object::Atom(_) => Vec::new(),
        Object::Node(Node::Map(entries)) => entries
            .iter()
            .map(|(k, id)| (Slot::Key(k.as_str()), *id))
            .collect(),
        Object::Node(Node::List(items)) => items
            .iter()
            .enumerate()
            .map(|(i, id)| (Slot::Index(i), *id))
            .collect(),
        Object::Node(Node::Set(items)) => items.iter().map(|id| (Slot::Member, *id)).collect(),
    }
}

fn child_at(object: &Object, slot: &Slot<'_>) -> Option<ObjectId> {
    match (object, slot) {
        (Object::Node(Node::Map(entries)), Slot::Key(k)) => entries.get(*k).copied(),
        (Object::Node(Node::List(items)), Slot::Index(i)) => items.get(*i).copied(),
        _ => None,
    }
}

/// Append to `out` the objects of the tree at `root` that neither `sent`
/// nor the trees at `bases` account for, a level at a time. Everything in
/// `sent` is already on its way with its whole subtree.
fn diff_tree<S: ObjectStore + ?Sized>(
    store: &S,
    root: ObjectId,
    bases: Vec<ObjectId>,
    sent: &mut HashSet<ObjectId>,
    out: &mut Vec<ObjectId>,
) -> Result<(), StorageError> {
    if sent.contains(&root) || bases.contains(&root) {
        return Ok(());
    }
    sent.insert(root);
    let mut level: Vec<(ObjectId, Vec<ObjectId>)> = vec![(root, bases)];
    while !level.is_empty() {
        let mut want = Vec::new();
        let mut asked = HashSet::new();
        for (id, bases) in &level {
            for x in std::iter::once(id).chain(bases) {
                if asked.insert(*x) {
                    want.push(*x);
                }
            }
        }
        let mut loaded: HashMap<ObjectId, Object> = HashMap::with_capacity(want.len());
        for chunk in want.chunks(READ_BATCH) {
            for (id, object) in chunk.iter().zip(store.batch_get_objects(chunk)?) {
                if let Some(object) = object {
                    loaded.insert(*id, object);
                }
            }
        }

        let mut next = Vec::new();
        for (id, bases) in &level {
            let object = loaded
                .get(id)
                .ok_or_else(|| StorageError::ObjectNotFound(id.to_string()))?;
            out.push(*id);
            // A base missing here only means less is skipped.
            let bases: Vec<&Object> = bases.iter().filter_map(|b| loaded.get(b)).collect();
            let known: HashSet<ObjectId> = bases
                .iter()
                .flat_map(|b| children(b).into_iter().map(|(_, c)| c))
                .collect();
            for (slot, child) in children(object) {
                if known.contains(&child) || !sent.insert(child) {
                    continue;
                }
                let child_bases = bases.iter().filter_map(|b| child_at(b, &slot)).collect();
                next.push((child, child_bases));
            }
        }
        level = next;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentstategraph_core::{Authority, Commit, CommitBuilder, Intent, IntentCategory};
    use agentstategraph_storage::MemoryStorage;
    use std::collections::BTreeMap;

    /// Builds trees and commits straight into a store, with chosen times.
    struct History {
        store: MemoryStorage,
        clock: i64,
    }

    impl History {
        fn new() -> Self {
            Self {
                store: MemoryStorage::new(),
                clock: 1_700_000_000,
            }
        }

        fn put(&self, object: Object) -> ObjectId {
            self.store.put_object(&object).unwrap()
        }

        /// A tree `{shared: {...}, own: <tag>}`: `shared` is the same big
        /// subtree in every commit, so a thin diff skips it.
        fn tree(&self, tag: &str) -> ObjectId {
            let shared = self.shared();
            let own = self.put(Object::string(tag));
            let mut root = BTreeMap::new();
            root.insert("shared".to_string(), shared);
            root.insert("own".to_string(), own);
            self.put(Object::map(root))
        }

        fn shared(&self) -> ObjectId {
            let mut entries = BTreeMap::new();
            for i in 0..20 {
                entries.insert(format!("k{i}"), self.put(Object::int(i)));
            }
            self.put(Object::map(entries))
        }

        /// A commit one second after the last, unless `at` says otherwise.
        fn commit(&mut self, tag: &str, parents: &[ObjectId], at: Option<i64>) -> ObjectId {
            self.clock += 1;
            let mut c: Commit = CommitBuilder::new(
                self.tree(tag),
                "agent/missing",
                Authority::simple("t"),
                Intent::new(IntentCategory::Refine, tag),
            )
            .parents(parents.to_vec())
            .build();
            c.timestamp = DateTime::from_timestamp(at.unwrap_or(self.clock), 0).unwrap();
            c.id = c.compute_id();
            self.store.put_commit(&c).unwrap();
            c.id
        }

        fn ancestors(&self, heads: &[ObjectId]) -> HashSet<ObjectId> {
            heads
                .iter()
                .flat_map(|h| {
                    self.store
                        .list_commits_dag(h, usize::MAX)
                        .unwrap()
                        .commits
                        .into_iter()
                        .map(|c| c.id)
                })
                .collect()
        }

        fn missing(&self, heads: &[ObjectId], haves: &[ObjectId]) -> MissingSet {
            missing_set(&self.store, heads, haves, &[]).unwrap()
        }

        /// What `missing` should list: reachable from `heads`, not from
        /// `haves`, per `list_commits_dag`.
        fn expected(&self, heads: &[ObjectId], haves: &[ObjectId]) -> HashSet<ObjectId> {
            let has = self.ancestors(haves);
            self.ancestors(heads)
                .into_iter()
                .filter(|c| !has.contains(c))
                .collect()
        }

        /// A remote holding the commits reachable from `haves`, with whole
        /// trees.
        fn remote_with(&self, haves: &[ObjectId]) -> MemoryStorage {
            let remote = MemoryStorage::new();
            for id in self.ancestors(haves) {
                let c = self.store.get_commit(&id).unwrap().unwrap();
                copy_tree(&self.store, &remote, c.state_root);
                remote.put_commit(&c).unwrap();
            }
            remote
        }

        /// Send `set` to `remote`, then check it has every commit reachable
        /// from `heads` (down to `shallow`) with its whole tree.
        fn send_and_check(&self, set: &MissingSet, remote: &MemoryStorage, heads: &[ObjectId]) {
            for id in &set.objects {
                remote
                    .put_object(&self.store.get_object(id).unwrap().unwrap())
                    .unwrap();
            }
            for id in &set.commits {
                remote
                    .put_commit(&self.store.get_commit(id).unwrap().unwrap())
                    .unwrap();
            }
            let boundary: HashSet<ObjectId> = set.shallow.iter().copied().collect();
            let mut todo: Vec<ObjectId> = heads.to_vec();
            let mut seen = HashSet::new();
            while let Some(id) = todo.pop() {
                if !seen.insert(id) {
                    continue;
                }
                let c = remote
                    .get_commit(&id)
                    .unwrap()
                    .unwrap_or_else(|| panic!("remote lacks commit {id}"));
                assert_whole_tree(remote, c.state_root);
                if !boundary.contains(&id) {
                    todo.extend(c.parents);
                }
            }
        }
    }

    fn copy_tree(from: &MemoryStorage, to: &MemoryStorage, root: ObjectId) {
        let mut todo = vec![root];
        while let Some(id) = todo.pop() {
            let o = from.get_object(&id).unwrap().unwrap();
            todo.extend(children(&o).into_iter().map(|(_, c)| c));
            to.put_object(&o).unwrap();
        }
    }

    fn assert_whole_tree(store: &MemoryStorage, root: ObjectId) {
        let mut todo = vec![root];
        while let Some(id) = todo.pop() {
            let o = store
                .get_object(&id)
                .unwrap()
                .unwrap_or_else(|| panic!("remote lacks object {id}"));
            todo.extend(children(&o).into_iter().map(|(_, c)| c));
        }
    }

    fn set(ids: &[ObjectId]) -> HashSet<ObjectId> {
        ids.iter().copied().collect()
    }

    #[test]
    fn a_linear_push_sends_the_new_commits_parents_first_and_only_what_changed() {
        let mut h = History::new();
        let c1 = h.commit("1", &[], None);
        let c2 = h.commit("2", &[c1], None);
        let c3 = h.commit("3", &[c2], None);
        let c4 = h.commit("4", &[c3], None);

        let m = h.missing(&[c4], &[c2]);
        assert_eq!(m.commits, vec![c3, c4]);
        assert!(m.shallow.is_empty());
        // Each new commit changes its root and one leaf; the shared subtree
        // and its 20 leaves stay behind.
        assert_eq!(m.objects.len(), 4, "{:?}", m.objects);
        h.send_and_check(&m, &h.remote_with(&[c2]), &[c4]);
    }

    #[test]
    fn a_remote_with_nothing_gets_every_commit_and_object() {
        let mut h = History::new();
        let c1 = h.commit("1", &[], None);
        let c2 = h.commit("2", &[c1], None);
        let m = h.missing(&[c2], &[]);
        assert_eq!(m.commits, vec![c1, c2]);
        // 2 roots, 2 own leaves, the shared map and its 20 leaves.
        assert_eq!(m.objects.len(), 25);
        assert_eq!(set(&m.objects).len(), 25, "each object once");
        h.send_and_check(&m, &MemoryStorage::new(), &[c2]);
    }

    #[test]
    fn nothing_is_missing_when_the_remote_has_the_head() {
        let mut h = History::new();
        let c1 = h.commit("1", &[], None);
        let c2 = h.commit("2", &[c1], None);
        assert!(h.missing(&[c2], &[c2]).is_empty());
        assert!(h.missing(&[c1], &[c2]).is_empty(), "an ancestor of a have");
        assert!(h.missing(&[c1], &[c2]).objects.is_empty());
    }

    #[test]
    fn merges_send_second_parents_and_stop_at_the_remotes_history() {
        // base - m1 - m2 ------- merge
        //    \                  /
        //     f1 - f2 ---------
        // The remote has m2. f1 and f2 branch from below it, so the walk
        // must stop at base, which the remote has through m2.
        let mut h = History::new();
        let base = h.commit("base", &[], None);
        let f1 = h.commit("f1", &[base], None);
        let m1 = h.commit("m1", &[base], None);
        let f2 = h.commit("f2", &[f1], None);
        let m2 = h.commit("m2", &[m1], None);
        let merge = h.commit("merge", &[m2, f2], None);

        let m = h.missing(&[merge], &[m2]);
        assert_eq!(set(&m.commits), h.expected(&[merge], &[m2]));
        assert_eq!(set(&m.commits), set(&[f1, f2, merge]));
        let pos = |id| m.commits.iter().position(|c| *c == id).unwrap();
        assert!(pos(f1) < pos(f2) && pos(f2) < pos(merge));
        h.send_and_check(&m, &h.remote_with(&[m2]), &[merge]);

        // From the other side: the remote has f2 and lacks the main line.
        let m = h.missing(&[merge], &[f2]);
        assert_eq!(set(&m.commits), set(&[m1, m2, merge]));
        h.send_and_check(&m, &h.remote_with(&[f2]), &[merge]);
    }

    #[test]
    fn several_heads_and_haves() {
        let mut h = History::new();
        let base = h.commit("base", &[], None);
        let a1 = h.commit("a1", &[base], None);
        let b1 = h.commit("b1", &[base], None);
        let a2 = h.commit("a2", &[a1], None);
        let b2 = h.commit("b2", &[b1], None);
        let m = h.missing(&[a2, b2], &[a1, b1]);
        assert_eq!(set(&m.commits), set(&[a2, b2]));
        h.send_and_check(&m, &h.remote_with(&[a1, b1]), &[a2, b2]);
    }

    #[test]
    fn commits_with_equal_timestamps_are_settled_exactly() {
        let mut h = History::new();
        let c1 = h.commit("1", &[], Some(100));
        let c2 = h.commit("2", &[c1], Some(100));
        let side = h.commit("side", &[c1], Some(100));
        let c3 = h.commit("3", &[c2, side], Some(100));
        let m = h.missing(&[c3], &[c2]);
        assert_eq!(set(&m.commits), h.expected(&[c3], &[c2]));
    }

    #[test]
    fn a_wrong_clock_can_over_send_but_never_under_send() {
        // Every timestamp is backwards: each child is older than its parent.
        let mut h = History::new();
        let base = h.commit("base", &[], Some(1_000));
        let m1 = h.commit("m1", &[base], Some(900));
        let f1 = h.commit("f1", &[base], Some(800));
        let m2 = h.commit("m2", &[m1], Some(700));
        let f2 = h.commit("f2", &[f1], Some(600));
        let merge = h.commit("merge", &[m2, f2], Some(500));
        for (head, have) in [(merge, m2), (merge, f2), (merge, base), (m2, m1)] {
            let m = h.missing(&[head], &[have]);
            let got = set(&m.commits);
            let want = h.expected(&[head], &[have]);
            assert!(got.is_superset(&want), "{head} over {have}: lost commits");
            assert!(
                got.is_subset(&h.ancestors(&[head])),
                "{head} over {have}: a commit not reachable from the head"
            );
            h.send_and_check(&m, &h.remote_with(&[have]), &[head]);
        }
    }

    #[test]
    fn a_shallow_boundary_sends_the_boundary_whole_and_none_of_its_parents() {
        let mut h = History::new();
        let c1 = h.commit("1", &[], None);
        let c2 = h.commit("2", &[c1], None);
        let c3 = h.commit("3", &[c2], None);
        let c4 = h.commit("4", &[c3], None);
        let m = missing_set(&h.store, &[c4], &[], &[c3]).unwrap();
        assert_eq!(m.commits, vec![c3, c4]);
        assert_eq!(m.shallow, vec![c3]);
        // c3's whole tree (23 objects), then c4's root and own leaf.
        assert_eq!(m.objects.len(), 25);
        h.send_and_check(&m, &MemoryStorage::new(), &[c4]);
    }

    #[test]
    fn a_commit_whose_parent_was_pruned_goes_as_shallow() {
        let mut h = History::new();
        let c1 = h.commit("1", &[], None);
        let c2 = h.commit("2", &[c1], None);
        let c3 = h.commit("3", &[c2], None);
        // A store that has c2 and c3 but not c1, as after a prune.
        let pruned = MemoryStorage::new();
        for id in [c2, c3] {
            let c = h.store.get_commit(&id).unwrap().unwrap();
            copy_tree(&h.store, &pruned, c.state_root);
            pruned.put_commit(&c).unwrap();
        }
        let m = missing_set(&pruned, &[c3], &[], &[]).unwrap();
        assert_eq!(m.commits, vec![c2, c3]);
        assert_eq!(m.shallow, vec![c2]);
    }

    #[test]
    fn an_unknown_have_is_ignored_and_an_unknown_head_fails() {
        let mut h = History::new();
        let c1 = h.commit("1", &[], None);
        let stranger = ObjectId::hash(b"not here");
        assert_eq!(h.missing(&[c1], &[stranger]).commits, vec![c1]);
        assert!(matches!(
            missing_set(&h.store, &[stranger], &[], &[]),
            Err(StorageError::CommitNotFound(_))
        ));
    }

    #[test]
    fn a_new_commit_missing_an_object_fails() {
        let mut h = History::new();
        let c1 = h.commit("1", &[], None);
        let mut c: Commit = h.store.get_commit(&c1).unwrap().unwrap();
        c.state_root = ObjectId::hash(b"absent");
        c.parents = vec![c1];
        c.id = c.compute_id();
        h.store.put_commit(&c).unwrap();
        assert!(matches!(
            missing_set(&h.store, &[c.id], &[c1], &[]),
            Err(StorageError::ObjectNotFound(_))
        ));
    }

    #[test]
    fn random_histories_match_the_dag_exactly() {
        // A seeded generator, so a failure reproduces.
        let mut seed: u64 = 0x5eed_cafe;
        let mut next = move |n: usize| {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (seed >> 33) as usize % n
        };
        for _ in 0..20 {
            let mut h = History::new();
            let mut ids: Vec<ObjectId> = Vec::new();
            for i in 0..40 {
                let parents: Vec<ObjectId> = match (ids.len(), next(4)) {
                    (0, _) => vec![],
                    (n, 0) if n > 1 => {
                        let a = ids[next(n)];
                        let b = ids[next(n)];
                        if a == b { vec![a] } else { vec![a, b] }
                    }
                    (n, _) => vec![ids[n - 1 - next(n.min(5))]],
                };
                // Timestamps never go backwards, and often repeat.
                let at = 1_000 + (i / 3) as i64;
                ids.push(h.commit(&format!("c{i}"), &parents, Some(at)));
            }
            for _ in 0..5 {
                let heads = vec![ids[next(ids.len())], ids[next(ids.len())]];
                let haves: Vec<ObjectId> = (0..next(3)).map(|_| ids[next(ids.len())]).collect();
                let m = h.missing(&heads, &haves);
                assert_eq!(set(&m.commits), h.expected(&heads, &haves));
                assert_eq!(m.commits.len(), set(&m.commits).len());
                h.send_and_check(&m, &h.remote_with(&haves), &heads);
            }
        }
    }

    #[test]
    fn lists_and_sets_are_diffed_too() {
        let h = History::new();
        let items: Vec<ObjectId> = (0..10).map(|i| h.put(Object::int(100 + i))).collect();
        let list_a = h.put(Object::Node(Node::List(items.clone())));
        let mut more = items.clone();
        more.push(h.put(Object::string("appended")));
        let list_b = h.put(Object::Node(Node::List(more.clone())));
        let set_b = h.put(Object::Node(Node::Set(more)));

        let mut sent = HashSet::new();
        let mut out = Vec::new();
        diff_tree(&h.store, list_b, vec![list_a], &mut sent, &mut out).unwrap();
        assert_eq!(out.len(), 2, "the new list and its one new item");

        let (mut sent, mut out) = (HashSet::new(), Vec::new());
        diff_tree(&h.store, set_b, vec![list_a], &mut sent, &mut out).unwrap();
        assert_eq!(out.len(), 2, "a set's members are matched by id");
    }
}
