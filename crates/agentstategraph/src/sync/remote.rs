//! Push, fetch and pull through a [`Remote`] (DESIGN.md Plan G t-004).
//!
//! A store keeps where each ref pointed on a remote at the last fetch or
//! push as a remote-tracking ref, `remotes/<remote>/<ref>`, in the ref's own
//! namespace. Only fetch and push move one; ordinary writes are refused
//! ([`RepoError::ReservedRef`]), and [`Repository::list_branches`] leaves
//! them out.
//!
//! - **Fetch** asks the remote for refs by name, so the remote resolves each
//!   one when it answers, and sends the commits it has that this store's
//!   heads don't reach. The bundle is verified, stored, and checked whole
//!   before any tracking ref moves. A bundle that fails leaves nothing
//!   reachable.
//! - **Push** is a compare-and-swap on the remote, from the tracking ref's
//!   commit to the local head, and only a fast-forward. A remote that has
//!   moved rejects it; fetch and merge, then push again. The receiving side
//!   never merges.
//! - **Pull** is a fetch, then [`Repository::merge`] of the tracking ref,
//!   with the caller's [`CommitOptions`].
//!
//! The control ref, `_control`, holding taints, reminders and epochs, goes
//! with every fetch and push. A fetched one is merged into the local one by
//! its own rules ([`crate::control::merge`], which needs no common ancestor),
//! and the tables are rebuilt from the result.
//!
//! The receiving side is [`Repository::sync_refs`],
//! [`Repository::sync_fetch`] and [`Repository::sync_receive`]: an
//! [`InProcessRemote`] calls them directly, and a transport (t-006) carries
//! the same calls.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{Cursor, Read, Write};

use agentstategraph_core::{CommitBuilder, IntentCategory, Namespace, Node, Object, ObjectId};
use agentstategraph_storage::{Storage, StorageError};

use crate::control::{self, CONTROL_REF, EpochConflict};
use crate::control_repo::{ControlReport, is_reserved_ref, system_options};
use crate::repo::{CommitOptions, RepoError, Repository, TRACKING_PREFIX, is_tracking_ref};
use crate::sync::bundle::{BundleError, BundleLimits, BundleReader, BundleRef};
use crate::sync::filter::{
    self, Finding, PushOptions, PushOverride, tree_holds_secrets, tree_is_local_only,
};
use crate::sync::missing::missing_set;
use crate::sync::push_log::PushLogEntry;
use crate::sync::shallow::{FetchOptions, depth_boundary};
use crate::sync::store::{Received, TransferError, store_records, write_set};

/// How many times folding in a fetched control ref rebuilds on a moved head.
const MAX_ATTEMPTS: usize = 64;

/// How far a commit's clock may run behind its parent's before an ancestry
/// walk stops looking past it. Only ever makes a walk answer "no" when the
/// answer is "yes", which refuses a fast-forward; it never accepts one that
/// isn't.
const CLOCK_SKEW_DAYS: i64 = 7;

/// Why a fetch, push or pull failed.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error(transparent)]
    Transfer(#[from] TransferError),

    /// The remote's ref isn't where this store last saw it.
    #[error(
        "{name} has moved on the remote (it's at {}, not {} as last fetched): fetch and merge, then push again",
        show(current),
        show(expected)
    )]
    Rejected {
        name: String,
        expected: Option<ObjectId>,
        current: Option<ObjectId>,
    },

    /// The push would move the remote's ref to a commit that doesn't
    /// descend from where it is.
    #[error("{name} isn't a fast-forward of the remote's: fetch and merge, then push again")]
    NotFastForward { name: String },

    #[error("the remote has no ref {0}")]
    NoSuchRef(String),

    /// No remote of this name is set ([`Repository::set_remote`]).
    #[error("no remote is named {0}")]
    NoSuchRemote(String),

    /// A push filter found something in what the push would send
    /// ([`crate::sync::filter`]).
    #[error("{name} wasn't pushed: {}", filter::describe(findings))]
    Blocked {
        name: String,
        findings: Vec<Finding>,
    },

    /// The ref is marked local-only ([`Repository::set_local_only`]).
    #[error("{0} is marked local-only, so it isn't pushed")]
    LocalOnly(String),

    /// The receiving side won't move this ref.
    #[error("refused: {0}")]
    Refused(String),

    /// The transport failed: a [`Remote`] implementation's own error.
    #[error("remote: {0}")]
    Remote(String),
}

fn show(id: &Option<ObjectId>) -> String {
    id.map_or_else(|| "nothing".to_string(), |id| id.short())
}

impl From<RepoError> for SyncError {
    fn from(e: RepoError) -> Self {
        SyncError::Transfer(e.into())
    }
}

impl From<BundleError> for SyncError {
    fn from(e: BundleError) -> Self {
        SyncError::Transfer(e.into())
    }
}

impl From<StorageError> for SyncError {
    fn from(e: StorageError) -> Self {
        SyncError::Transfer(e.into())
    }
}

impl SyncError {
    /// A stable code for this kind of error.
    pub fn code(&self) -> &'static str {
        match self {
            SyncError::Transfer(e) => e.code(),
            SyncError::Rejected { .. } => "push_rejected",
            SyncError::NotFastForward { .. } => "not_fast_forward",
            SyncError::NoSuchRef(_) => "no_such_ref",
            SyncError::NoSuchRemote(_) => "no_such_remote",
            SyncError::Blocked { .. } => "push_blocked",
            SyncError::LocalOnly(_) => "local_only_ref",
            SyncError::Refused(_) => "push_refused",
            SyncError::Remote(_) => "remote_error",
        }
    }
}

/// What a fetch asks a remote for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchRequest {
    /// Refs by name; the remote resolves each when it answers.
    pub refs: Vec<(Namespace, String)>,
    /// Commits the fetching store has, so the remote can leave out what
    /// they reach. A remote ignores the ones it hasn't got.
    pub haves: Vec<ObjectId>,
    /// Send at most this many commits along each line from a ref's head
    /// ([`crate::sync::shallow`]).
    pub depth: Option<u32>,
}

/// What a push asks a remote to do: move `name` in `namespace` from `old`
/// (absent: create it) to `new`. `overrides` are the push filter findings
/// the pusher let through, for the remote's push log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefUpdate {
    pub namespace: Namespace,
    pub name: String,
    pub old: Option<ObjectId>,
    pub new: ObjectId,
    pub overrides: Vec<PushOverride>,
}

/// How a remote answered a push.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushOutcome {
    Updated,
    /// The ref isn't at the update's `old`; it's at `current`.
    Stale {
        current: Option<ObjectId>,
    },
    /// The update's `new` doesn't descend from its `old`.
    NotFastForward,
}

/// Another store to sync with. The calls carry bundles
/// ([`crate::sync::bundle`]); a receiving side implements them with
/// [`Repository::sync_refs`], [`Repository::sync_fetch`] and
/// [`Repository::sync_receive`].
pub trait Remote {
    /// Every ref the remote offers, in every namespace: its branches and
    /// its control ref.
    fn list_refs(&self) -> Result<Vec<BundleRef>, SyncError>;

    /// A bundle holding `request.refs` as the remote has them now, and
    /// the commits and objects they reach that `request.haves` don't.
    fn fetch(&self, request: &FetchRequest) -> Result<Box<dyn Read + Send + '_>, SyncError>;

    /// Apply `update`, with `bundle` carrying what the remote lacks.
    fn push(&self, update: &RefUpdate, bundle: &[u8]) -> Result<PushOutcome, SyncError>;
}

/// A [`Remote`] that's another repository in this process: for tests, and
/// for syncing two stores one program has open.
pub struct InProcessRemote<'a> {
    repo: &'a Repository,
    limits: BundleLimits,
    pusher: String,
}

impl<'a> InProcessRemote<'a> {
    pub fn new(repo: &'a Repository) -> Self {
        Self {
            repo,
            limits: BundleLimits::default(),
            pusher: "in-process".to_string(),
        }
    }

    /// Bound what a push to this remote may send.
    pub fn with_limits(mut self, limits: BundleLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Who the remote's push log says delivered this remote's pushes
    /// (default `in-process`).
    pub fn as_pusher(mut self, pusher: impl Into<String>) -> Self {
        self.pusher = pusher.into();
        self
    }
}

impl Remote for InProcessRemote<'_> {
    fn list_refs(&self) -> Result<Vec<BundleRef>, SyncError> {
        self.repo.sync_refs()
    }

    fn fetch(&self, request: &FetchRequest) -> Result<Box<dyn Read + Send + '_>, SyncError> {
        let bytes = self.repo.sync_fetch(request, Vec::new())?;
        Ok(Box::new(Cursor::new(bytes)))
    }

    fn push(&self, update: &RefUpdate, bundle: &[u8]) -> Result<PushOutcome, SyncError> {
        self.repo
            .sync_receive(update, bundle, self.limits.clone(), &self.pusher)
    }
}

/// A ref that moved: a remote-tracking ref after a fetch, or a remote's ref
/// after a push, named by the remote's ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefChange {
    pub namespace: Namespace,
    pub name: String,
    pub old: Option<ObjectId>,
    pub new: ObjectId,
}

/// What [`Repository::fetch`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FetchReport {
    /// Remote-tracking refs that moved.
    pub updated: Vec<RefChange>,
    pub commits: usize,
    pub objects: usize,
    /// What rebuilding the taint, reminder and epoch tables did, when the
    /// fetched control ref changed the local one.
    pub control: Option<ControlReport>,
    /// Epochs the two stores sealed differently. The earlier seal is kept.
    pub epoch_conflicts: Vec<EpochConflict>,
}

/// What [`Repository::push`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PushReport {
    /// The remote's refs that moved: none when it was up to date.
    pub updated: Vec<RefChange>,
    pub commits: usize,
    pub objects: usize,
    /// Push filter findings that went through, with the override that let
    /// each one.
    pub overridden: Vec<(Finding, PushOverride)>,
}

/// How [`Repository::pull`] left the local ref.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullOutcome {
    /// It already had everything the remote's ref holds.
    UpToDate,
    /// It didn't exist, or held only the empty first commit `init` makes,
    /// and now points where the remote's does.
    Created(ObjectId),
    FastForward(ObjectId),
    /// A merge commit.
    Merged(ObjectId),
}

/// What [`Repository::pull`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullReport {
    pub fetch: FetchReport,
    pub outcome: PullOutcome,
}

/// The remote-tracking ref for `name` on `remote`.
pub fn tracking_ref(remote: &str, name: &str) -> String {
    format!("{TRACKING_PREFIX}{remote}/{name}")
}

pub(crate) fn check_remote_name(remote: &str) -> Result<(), SyncError> {
    if remote.is_empty()
        || remote.starts_with('_')
        || remote.contains('/')
        || remote.chars().any(char::is_whitespace)
    {
        return Err(RepoError::InvalidOperation(format!("not a remote name: {remote:?}")).into());
    }
    Ok(())
}

/// Whether a store offers `name` in `ns` to a remote: a branch, or the
/// control ref.
fn is_synced_ref(ns: &Namespace, name: &str) -> bool {
    if name == CONTROL_REF {
        return *ns == Namespace::default_ns();
    }
    !name.is_empty() && !is_reserved_ref(name) && !is_tracking_ref(name)
}

/// A ref, with a namespace that doesn't exist counting as no ref.
fn get_ref(
    store: &dyn Storage,
    ns: &Namespace,
    name: &str,
) -> Result<Option<ObjectId>, StorageError> {
    match store.get_ref(ns, name) {
        Err(StorageError::NamespaceNotFound(_)) => Ok(None),
        other => other,
    }
}

fn list_refs(
    store: &dyn Storage,
    ns: &Namespace,
    prefix: &str,
) -> Result<Vec<(String, ObjectId)>, StorageError> {
    match store.list_refs(ns, prefix) {
        Err(StorageError::NamespaceNotFound(_)) => Ok(Vec::new()),
        other => other,
    }
}

/// Whether `ancestor` is `descendant` or reachable from it. The walk goes
/// breadth first, so a near ancestor is found quickly, and doesn't look
/// past a commit more than [`CLOCK_SKEW_DAYS`] older than `ancestor`.
fn is_ancestor(
    store: &dyn Storage,
    ancestor: &ObjectId,
    descendant: &ObjectId,
) -> Result<bool, StorageError> {
    if ancestor == descendant {
        return Ok(true);
    }
    let Some(target) = store.get_commit(ancestor)? else {
        return Ok(false);
    };
    let floor = target.timestamp - chrono::Duration::days(CLOCK_SKEW_DAYS);
    let mut seen = HashSet::from([*descendant]);
    let mut queue = VecDeque::from([*descendant]);
    while let Some(id) = queue.pop_front() {
        let Some(commit) = store.get_commit(&id)? else {
            continue;
        };
        if commit.timestamp < floor {
            continue;
        }
        for parent in commit.parents {
            if parent == *ancestor {
                return Ok(true);
            }
            if seen.insert(parent) {
                queue.push_back(parent);
            }
        }
    }
    Ok(false)
}

impl Repository {
    // -- The receiving side ------------------------------------------------

    /// Every ref this store offers a remote, in every namespace: its
    /// branches and its control ref.
    pub fn sync_refs(&self) -> Result<Vec<BundleRef>, SyncError> {
        let store = self.taint_storage();
        let mut refs = Vec::new();
        for ns in store.list_namespaces()? {
            for (name, target) in list_refs(store, &ns, "")? {
                if is_synced_ref(&ns, &name) && !self.commit_is_local_only(&target)? {
                    refs.push(BundleRef {
                        namespace: ns.clone(),
                        name,
                        target,
                    });
                }
            }
        }
        Ok(refs)
    }

    /// Answer a fetch: write to `out` a bundle holding the refs `request`
    /// names, as they are now, and what they reach that its haves don't.
    /// A ref this store doesn't offer is [`SyncError::NoSuchRef`].
    pub fn sync_fetch<W: Write>(&self, request: &FetchRequest, out: W) -> Result<W, SyncError> {
        let store = self.taint_storage();
        let mut refs = Vec::with_capacity(request.refs.len());
        for (ns, name) in &request.refs {
            let target = match is_synced_ref(ns, name) {
                true => get_ref(store, ns, name)?,
                false => None,
            };
            let target = match target {
                Some(t) if self.commit_is_local_only(&t)? => None,
                other => other,
            };
            let target = target.ok_or_else(|| SyncError::NoSuchRef(format!("{ns}/{name}")))?;
            refs.push(BundleRef {
                namespace: ns.clone(),
                name: name.clone(),
                target,
            });
        }
        let mut heads: Vec<ObjectId> = refs.iter().map(|r| r.target).collect();
        heads.sort();
        heads.dedup();
        let shallow = match request.depth {
            Some(depth) => depth_boundary(store, &heads, depth)?,
            None => Vec::new(),
        };
        let set = missing_set(store, &heads, &request.haves, &shallow)?;
        Ok(write_set(store, &set, &refs, out)?)
    }

    /// Answer a push from `pusher` (as the caller authenticated them):
    /// store and check `bundle`, then move the ref, only from where the
    /// update says it is and only forward. A push to the control ref
    /// rebuilds the taint, reminder and epoch tables. The ref moves only
    /// once the whole bundle has been read and checked, and each push that
    /// moves it is recorded in the push log ([`Repository::push_log`]).
    pub fn sync_receive<R: Read>(
        &self,
        update: &RefUpdate,
        bundle: R,
        limits: BundleLimits,
        pusher: &str,
    ) -> Result<PushOutcome, SyncError> {
        if !is_synced_ref(&update.namespace, &update.name) {
            return Err(SyncError::Refused(format!(
                "{}/{} isn't a ref a push can move",
                update.namespace, update.name
            )));
        }
        let store = self.taint_storage();
        let current = get_ref(store, &update.namespace, &update.name)?;
        if current != update.old {
            return Ok(PushOutcome::Stale { current });
        }
        if update.old == Some(update.new) {
            return Ok(PushOutcome::Updated);
        }
        let received = store_records(store, BundleReader::new(bundle, limits)?)?;
        self.check_received(&received)?;
        self.check_target(&update.new)?;
        // Secrets never sync, and a local-only ref stays where it is.
        for (id, root, _) in &received.commits {
            if store.has_object(root)? && tree_holds_secrets(store, root)? {
                return Err(SyncError::Refused(format!(
                    "commit {id} holds /_meta/_secret, which never syncs"
                )));
            }
        }
        if update.name != CONTROL_REF && self.commit_is_local_only(&update.new)? {
            return Err(SyncError::Refused(format!(
                "{}/{} is marked local-only",
                update.namespace, update.name
            )));
        }
        // Only forward, except from the empty first commit `init` makes,
        // which holds nothing to lose.
        if let Some(old) = update.old
            && !is_ancestor(store, &old, &update.new)?
            && !self.is_fresh_root(&old)?
        {
            return Ok(PushOutcome::NotFastForward);
        }

        let landed = if update.name == CONTROL_REF {
            let ns = Namespace::default_ns();
            let landed = match update.old {
                Some(old) => store.cas_ref(&ns, CONTROL_REF, old, update.new)?,
                None => store.create_ref(&ns, CONTROL_REF, update.new)?,
            };
            if landed && let Err(e) = self.reconcile_control() {
                // The ref is right; the next reconcile catches the tables up.
                tracing::warn!(
                    error.type = e.code(),
                    "couldn't rebuild the tables from a pushed control ref: {e}"
                );
            }
            landed
        } else {
            if update.namespace != Namespace::default_ns() {
                self.create_namespace(update.namespace.as_str())?;
            }
            match update.old {
                Some(old) => {
                    self.fast_forward_ref(&update.namespace, &update.name, old, update.new)?
                }
                None => store.create_ref(&update.namespace, &update.name, update.new)?,
            }
        };
        if landed {
            self.record_shallow(received.shallow.iter().copied())?;
            let entry = PushLogEntry {
                at: chrono::Utc::now(),
                pusher: pusher.to_string(),
                namespace: update.namespace.to_string(),
                name: update.name.clone(),
                old: update.old.map(|id| id.to_hex()),
                new: update.new.to_hex(),
                overrides: update.overrides.clone(),
            };
            if let Err(e) = self.append_push_log(&entry) {
                // The ref has moved; failing the push now would only make
                // the pusher retry one that landed.
                tracing::warn!(
                    error.type = e.code(),
                    "couldn't record a push of {}/{} in the push log: {e}",
                    update.namespace,
                    update.name
                );
            }
        }
        Ok(if landed {
            PushOutcome::Updated
        } else {
            PushOutcome::Stale {
                current: get_ref(store, &update.namespace, &update.name)?,
            }
        })
    }

    /// Check that what [`store_records`] stored can be used: each commit's
    /// parents are here, unless it came shallow, and each tree is whole,
    /// unless its sender no longer had it (a retention sweep took it). A
    /// tree is checked against its first parent's, which is whole if it's
    /// a ref's target here or a commit checked before it, skipping what the
    /// two share; otherwise in full.
    fn check_received(&self, received: &Received) -> Result<(), SyncError> {
        let store = self.taint_storage();
        let mut targets: HashSet<ObjectId> = HashSet::new();
        for ns in store.list_namespaces()? {
            targets.extend(list_refs(store, &ns, "")?.into_iter().map(|(_, t)| t));
        }
        let mut whole: HashMap<ObjectId, ObjectId> = HashMap::new();
        for (id, root, parents) in &received.commits {
            if !received.shallow.contains(id) {
                for parent in parents {
                    if !store.has_commit(parent)? {
                        return Err(TransferError::Incomplete(format!(
                            "commit {id} has a parent, {parent}, that isn't here"
                        ))
                        .into());
                    }
                }
            }
            if !store.has_object(root)? {
                continue;
            }
            let base = match parents.first() {
                Some(p) if whole.contains_key(p) => whole.get(p).copied(),
                Some(p) if targets.contains(p) => store.get_commit(p)?.map(|c| c.state_root),
                _ => None,
            };
            let missing = match base {
                Some(base) => self.first_missing_new(&base, root)?,
                None => self.first_missing_reachable(root)?,
            };
            if let Some(missing) = missing {
                return Err(TransferError::Incomplete(format!(
                    "commit {id}'s tree lacks {missing}"
                ))
                .into());
            }
            whole.insert(*id, *root);
        }
        Ok(())
    }

    /// Check that a ref can point at `target`: it's here, with its tree.
    fn check_target(&self, target: &ObjectId) -> Result<(), SyncError> {
        let store = self.taint_storage();
        let commit = store
            .get_commit(target)?
            .ok_or_else(|| TransferError::Incomplete(format!("{target} isn't here")))?;
        if !store.has_object(&commit.state_root)? {
            return Err(TransferError::Incomplete(format!("{target}'s tree isn't here")).into());
        }
        Ok(())
    }

    // -- The fetching and pushing side --------------------------------------

    /// Where each of `remote`'s branches pointed at the last fetch or push,
    /// in this repository's namespace. The control ref's tracking ref isn't
    /// a branch, so it's left out.
    pub fn remote_refs(&self, remote: &str) -> Result<Vec<(String, ObjectId)>, SyncError> {
        check_remote_name(remote)?;
        let prefix = tracking_ref(remote, "");
        let ns = self.active_namespace()?;
        Ok(list_refs(self.taint_storage(), &ns, &prefix)?
            .into_iter()
            .filter_map(|(name, t)| Some((name.strip_prefix(&prefix)?.to_string(), t)))
            .filter(|(name, _)| name != CONTROL_REF)
            .collect())
    }

    /// Fetch `refs` (every branch the remote has in this repository's
    /// namespace, when empty) from `remote`, which this store calls
    /// `remote_name`, moving their remote-tracking refs. The remote's
    /// control ref comes too and is merged into this store's, and the
    /// taint, reminder and epoch tables are rebuilt from the result.
    pub fn fetch(
        &self,
        remote_name: &str,
        remote: &dyn Remote,
        refs: &[&str],
    ) -> Result<FetchReport, SyncError> {
        self.fetch_with(remote_name, remote, refs, &FetchOptions::default())
    }

    /// [`Repository::fetch`], limited as `options` say: a `depth` takes only
    /// that much history and records where it stops
    /// ([`crate::sync::shallow`]).
    pub fn fetch_with(
        &self,
        remote_name: &str,
        remote: &dyn Remote,
        refs: &[&str],
        options: &FetchOptions,
    ) -> Result<FetchReport, SyncError> {
        check_remote_name(remote_name)?;
        let store = self.taint_storage();
        let ns = self.active_namespace()?;
        let default_ns = Namespace::default_ns();
        let offered = remote.list_refs()?;

        let mut wanted: Vec<BundleRef> = Vec::new();
        for &name in refs {
            if name == CONTROL_REF || !is_synced_ref(&ns, name) {
                return Err(RepoError::ReservedRef(name.to_string()).into());
            }
            let r = offered
                .iter()
                .find(|r| r.namespace == ns && r.name == name)
                .ok_or_else(|| SyncError::NoSuchRef(name.to_string()))?;
            wanted.push(r.clone());
        }
        if refs.is_empty() {
            wanted.extend(
                offered
                    .iter()
                    .filter(|r| r.namespace == ns && r.name != CONTROL_REF)
                    .filter(|r| is_synced_ref(&r.namespace, &r.name))
                    .cloned(),
            );
        }
        if let Some(r) = offered
            .iter()
            .find(|r| r.namespace == default_ns && r.name == CONTROL_REF)
        {
            wanted.push(r.clone());
        }

        let mut report = FetchReport::default();
        let mut request = FetchRequest {
            refs: Vec::new(),
            haves: if options.unshallow {
                Vec::new()
            } else {
                self.fetch_haves(remote_name, &ns)?
            },
            depth: options.depth,
        };
        for r in &wanted {
            if options.unshallow || !self.has_whole(&r.target)? {
                request.refs.push((r.namespace.clone(), r.name.clone()));
            }
        }
        if !request.refs.is_empty() {
            let input = remote.fetch(&request)?;
            let received =
                store_records(store, BundleReader::new(input, BundleLimits::default())?)?;
            self.check_received(&received)?;
            // The bundle says where each ref was when the remote answered.
            for r in &received.refs {
                if let Some(w) = wanted
                    .iter_mut()
                    .find(|w| w.namespace == r.namespace && w.name == r.name)
                {
                    w.target = r.target;
                }
            }
            report.commits = received.commits.len();
            report.objects = received.objects;
            self.record_shallow(received.shallow.iter().copied())?;
        }
        for r in &wanted {
            self.check_target(&r.target)?;
        }

        // Tracking refs move only once everything above checked out.
        let mut control = None;
        for r in &wanted {
            let tracking = tracking_ref(remote_name, &r.name);
            let old = get_ref(store, &r.namespace, &tracking)?;
            if r.name == CONTROL_REF {
                control = Some(r.target);
            }
            if old == Some(r.target) {
                continue;
            }
            store.set_ref(&r.namespace, &tracking, r.target)?;
            report.updated.push(RefChange {
                namespace: r.namespace.clone(),
                name: r.name.clone(),
                old,
                new: r.target,
            });
        }
        if let Some(theirs) = control {
            let (moved, conflicts) = self.merge_control(remote_name, theirs)?;
            report.epoch_conflicts = conflicts;
            if moved {
                report.control = Some(self.reconcile_control()?);
            }
        }
        Ok(report)
    }

    /// Push `ref_name` from this repository's namespace to `remote`, which
    /// this store calls `remote_name`: a fast-forward of the remote's ref
    /// from where it was at the last fetch. The control ref goes first.
    /// A remote that has moved is [`SyncError::Rejected`]: fetch and merge,
    /// then push again. What the push sends goes through the built-in
    /// credential filter first ([`PushOptions::default`]); a finding is
    /// [`SyncError::Blocked`].
    pub fn push(
        &self,
        remote_name: &str,
        remote: &dyn Remote,
        ref_name: &str,
    ) -> Result<PushReport, SyncError> {
        self.push_with(remote_name, remote, ref_name, &PushOptions::default())
    }

    /// [`Repository::push`] with the filters and overrides `options` give.
    /// Anything under `/_meta/_secret` and any ref marked local-only are
    /// refused whatever they are.
    pub fn push_with(
        &self,
        remote_name: &str,
        remote: &dyn Remote,
        ref_name: &str,
        options: &PushOptions,
    ) -> Result<PushReport, SyncError> {
        check_remote_name(remote_name)?;
        let store = self.taint_storage();
        let ns = self.active_namespace()?;
        if ref_name == CONTROL_REF || !is_synced_ref(&ns, ref_name) {
            return Err(RepoError::ReservedRef(ref_name.to_string()).into());
        }
        let head = get_ref(store, &ns, ref_name)?
            .ok_or_else(|| RepoError::BranchNotFound(ref_name.to_string()))?;
        if self.commit_is_local_only(&head)? {
            return Err(SyncError::LocalOnly(ref_name.to_string()));
        }
        // So the control ref carries every taint, reminder and epoch write.
        self.reconcile_control()?;
        let mut report = PushReport::default();
        let default_ns = Namespace::default_ns();
        if let Some(control) = get_ref(store, &default_ns, CONTROL_REF)? {
            self.push_one(
                remote_name,
                remote,
                &default_ns,
                CONTROL_REF,
                control,
                options,
                &mut report,
            )?;
        }
        self.push_one(
            remote_name,
            remote,
            &ns,
            ref_name,
            head,
            options,
            &mut report,
        )?;
        Ok(report)
    }

    /// Fetch `ref_name` from `remote` and merge it into the local one.
    /// `options` describe a merge commit, if one is made.
    ///
    /// A ref holding only the empty first commit [`Repository::init`] makes
    /// counts as empty, on either side: two stores that each ran `init`
    /// would otherwise have unrelated histories, which [`Repository::merge`]
    /// refuses. A local ref that's absent or holds only that takes the
    /// remote's head; a remote ref that holds only that has nothing to
    /// take, and a push may replace it.
    pub fn pull(
        &self,
        remote_name: &str,
        remote: &dyn Remote,
        ref_name: &str,
        options: CommitOptions,
    ) -> Result<PullReport, SyncError> {
        let fetch = self.fetch(remote_name, remote, &[ref_name])?;
        let outcome = self.merge_fetched(remote_name, ref_name, options)?;
        Ok(PullReport { fetch, outcome })
    }

    /// The second half of [`Repository::pull`]: merge `ref_name` as the
    /// last fetch from `remote_name` left it into the local one. A caller
    /// that prepares the merge first fetches, prepares, then calls this:
    /// `agentstategraph_tasks::TaskStore::renumber_before_merge` renumbers
    /// colliding task ids this way.
    pub fn merge_fetched(
        &self,
        remote_name: &str,
        ref_name: &str,
        options: CommitOptions,
    ) -> Result<PullOutcome, SyncError> {
        check_remote_name(remote_name)?;
        let store = self.taint_storage();
        let ns = self.active_namespace()?;
        let tracking = tracking_ref(remote_name, ref_name);
        let theirs = get_ref(store, &ns, &tracking)?
            .ok_or_else(|| SyncError::NoSuchRef(ref_name.to_string()))?;
        let outcome = match get_ref(store, &ns, ref_name)? {
            None if store.create_ref(&ns, ref_name, theirs)? => PullOutcome::Created(theirs),
            Some(ours) if is_ancestor(store, &theirs, &ours)? => PullOutcome::UpToDate,
            // The remote's ref holds only `init`'s empty first commit: there's
            // nothing to take, and the next push replaces it.
            Some(_) if self.is_fresh_root(&theirs)? => PullOutcome::UpToDate,
            Some(ours)
                if self.is_fresh_root(&ours)?
                    && self.fast_forward_ref(&ns, ref_name, ours, theirs)? =>
            {
                PullOutcome::Created(theirs)
            }
            _ => {
                let id = self.merge(&tracking, ref_name, options)?;
                if id == theirs {
                    PullOutcome::FastForward(id)
                } else {
                    PullOutcome::Merged(id)
                }
            }
        };
        Ok(outcome)
    }

    /// Push one ref: `name` in `ns`, at `head` here.
    #[allow(clippy::too_many_arguments)]
    fn push_one(
        &self,
        remote_name: &str,
        remote: &dyn Remote,
        ns: &Namespace,
        name: &str,
        head: ObjectId,
        options: &PushOptions,
        report: &mut PushReport,
    ) -> Result<(), SyncError> {
        let store = self.taint_storage();
        let tracking = tracking_ref(remote_name, name);
        let old = get_ref(store, ns, &tracking)?;
        if old == Some(head) {
            return Ok(());
        }
        if let Some(old) = old
            && !is_ancestor(store, &old, &head)?
            && !self.is_fresh_root(&old)?
        {
            return Err(SyncError::NotFastForward {
                name: name.to_string(),
            });
        }
        let haves = self.tracking_targets(remote_name, ns)?;
        let set = missing_set(store, &[head], &haves, &options.shallow)?;
        let findings = filter::scan(store, &set, &options.filters)?;
        let (allowed, blocking) = filter::apply_overrides(findings, &options.overrides);
        if !blocking.is_empty() {
            return Err(SyncError::Blocked {
                name: name.to_string(),
                findings: blocking,
            });
        }
        let bundle = write_set(store, &set, &[], Vec::new())?;
        let update = RefUpdate {
            namespace: ns.clone(),
            name: name.to_string(),
            old,
            new: head,
            overrides: allowed.iter().map(|(_, o)| o.clone()).collect(),
        };
        match remote.push(&update, &bundle)? {
            PushOutcome::Updated => {
                store.set_ref(ns, &tracking, head)?;
                report.commits += set.commits.len();
                report.objects += set.objects.len();
                report.overridden.extend(allowed);
                report.updated.push(RefChange {
                    namespace: ns.clone(),
                    name: name.to_string(),
                    old,
                    new: head,
                });
                Ok(())
            }
            PushOutcome::Stale { current } => Err(SyncError::Rejected {
                name: name.to_string(),
                expected: old,
                current,
            }),
            PushOutcome::NotFastForward => Err(SyncError::NotFastForward {
                name: name.to_string(),
            }),
        }
    }

    /// The commits `remote` is known to have, from the tracking refs in
    /// `ns` and the control ref's: what a push can leave out.
    fn tracking_targets(&self, remote: &str, ns: &Namespace) -> Result<Vec<ObjectId>, SyncError> {
        let store = self.taint_storage();
        let prefix = tracking_ref(remote, "");
        let mut haves: Vec<ObjectId> = list_refs(store, ns, &prefix)?
            .into_iter()
            .map(|(_, t)| t)
            .collect();
        haves.extend(get_ref(
            store,
            &Namespace::default_ns(),
            &tracking_ref(remote, CONTROL_REF),
        )?);
        haves.sort();
        haves.dedup();
        Ok(haves)
    }

    /// The commits a fetch from `remote` can tell it this store has: the
    /// tracking refs, and the local branches and control ref, which the
    /// remote may well have too.
    fn fetch_haves(&self, remote: &str, ns: &Namespace) -> Result<Vec<ObjectId>, SyncError> {
        let store = self.taint_storage();
        let mut haves = self.tracking_targets(remote, ns)?;
        haves.extend(
            list_refs(store, ns, "")?
                .into_iter()
                .filter(|(name, _)| is_synced_ref(ns, name))
                .map(|(_, t)| t),
        );
        haves.extend(get_ref(store, &Namespace::default_ns(), CONTROL_REF)?);
        haves.sort();
        haves.dedup();
        Ok(haves)
    }

    /// Whether `commit` is a first commit holding nothing outside `/_meta`,
    /// as [`Repository::init`] makes: replacing it loses nothing.
    fn is_fresh_root(&self, commit: &ObjectId) -> Result<bool, SyncError> {
        let store = self.taint_storage();
        let Some(commit) = store.get_commit(commit)? else {
            return Ok(false);
        };
        if !commit.parents.is_empty() {
            return Ok(false);
        }
        Ok(match store.get_object(&commit.state_root)? {
            Some(Object::Node(Node::Map(entries))) => entries.keys().all(|k| k == "_meta"),
            _ => false,
        })
    }

    /// Whether `commit`'s tree marks its ref local-only. A commit that isn't
    /// here, or lacks its tree, isn't marked.
    fn commit_is_local_only(&self, commit: &ObjectId) -> Result<bool, SyncError> {
        let store = self.taint_storage();
        Ok(match store.get_commit(commit)? {
            Some(c) if store.has_object(&c.state_root)? => {
                tree_is_local_only(store, &c.state_root)?
            }
            _ => false,
        })
    }

    /// Whether this store has `commit` and its tree.
    fn has_whole(&self, commit: &ObjectId) -> Result<bool, SyncError> {
        let store = self.taint_storage();
        Ok(match store.get_commit(commit)? {
            Some(c) => store.has_object(&c.state_root)?,
            None => false,
        })
    }

    /// Bring `theirs`, a control commit fetched from `remote`, into the
    /// local control ref: take it if the local one is absent or behind,
    /// otherwise merge the two trees in a commit with both as parents.
    /// Returns whether the local ref moved, and the epochs the two sealed
    /// differently.
    fn merge_control(
        &self,
        remote: &str,
        theirs: ObjectId,
    ) -> Result<(bool, Vec<EpochConflict>), SyncError> {
        let store = self.taint_storage();
        let ns = Namespace::default_ns();
        for _ in 0..MAX_ATTEMPTS {
            let landed = match get_ref(store, &ns, CONTROL_REF)? {
                None => store.create_ref(&ns, CONTROL_REF, theirs)?,
                Some(ours) if is_ancestor(store, &theirs, &ours)? => {
                    return Ok((false, Vec::new()));
                }
                Some(ours) if is_ancestor(store, &ours, &theirs)? => {
                    store.cas_ref(&ns, CONTROL_REF, ours, theirs)?
                }
                Some(ours) => {
                    let root = |id: &ObjectId| -> Result<ObjectId, SyncError> {
                        Ok(store
                            .get_commit(id)?
                            .ok_or_else(|| RepoError::CommitNotFound(id.to_string()))?
                            .state_root)
                    };
                    let merged = control::merge(store, &root(&ours)?, &root(&theirs)?)?;
                    let options = system_options(
                        IntentCategory::Merge,
                        format!("merge the control ref from {remote}"),
                    );
                    let commit = CommitBuilder::new(
                        merged.root,
                        options.agent_id,
                        options.authority,
                        options.intent,
                    )
                    .parents(vec![ours, theirs])
                    .build();
                    store.put_commit(&commit)?;
                    if store.cas_ref(&ns, CONTROL_REF, ours, commit.id)? {
                        return Ok((true, merged.conflicts));
                    }
                    false
                }
            };
            if landed {
                return Ok((true, Vec::new()));
            }
        }
        Err(RepoError::WriteConflict.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::bundle::Bundle;
    use agentstategraph_storage::MemoryStorage;
    use serde_json::json;
    use std::sync::Arc;

    fn repo() -> Repository {
        Repository::from_shared(Arc::new(MemoryStorage::new()) as Arc<dyn Storage + Send + Sync>)
    }

    fn opts(what: &str) -> CommitOptions {
        CommitOptions::new("agent/sync", IntentCategory::Refine, what)
    }

    fn merge_opts() -> CommitOptions {
        CommitOptions::new("agent/sync", IntentCategory::Merge, "pull")
    }

    fn taint_params(name: &str) -> agentstategraph_taint::TaintParams {
        agentstategraph_taint::TaintParams {
            name: name.into(),
            effect: agentstategraph_taint::TaintEffect::Block,
            reason: "holds pii".into(),
            severity: agentstategraph_taint::TaintSeverity::High,
            expires_at: None,
            propagate: true,
            metadata: agentstategraph_taint::TaintMetadata::new(),
            agent_id: "agent/sync".into(),
        }
    }

    /// A hub, and a store `a` that has pushed its `main` to it.
    fn hub_and_a() -> (Repository, Repository) {
        let (hub, a) = (repo(), repo());
        a.init().unwrap();
        a.set_json("main", "/x", &json!(1), opts("x")).unwrap();
        a.push("hub", &InProcessRemote::new(&hub), "main").unwrap();
        (hub, a)
    }

    #[test]
    fn push_then_pull_moves_a_ref_between_stores() {
        let (hub, a) = hub_and_a();
        assert_eq!(hub.head("main").unwrap(), a.head("main").unwrap());
        assert_eq!(
            a.remote_refs("hub").unwrap(),
            vec![("main".to_string(), a.head("main").unwrap())]
        );

        let b = repo();
        let pulled = b
            .pull("hub", &InProcessRemote::new(&hub), "main", merge_opts())
            .unwrap();
        assert_eq!(
            pulled.outcome,
            PullOutcome::Created(a.head("main").unwrap())
        );
        assert_eq!(b.get_json("main", "/x").unwrap(), json!(1));

        // Pulling again changes nothing.
        let again = b
            .pull("hub", &InProcessRemote::new(&hub), "main", merge_opts())
            .unwrap();
        assert_eq!(again.outcome, PullOutcome::UpToDate);
        assert!(again.fetch.updated.is_empty(), "{:?}", again.fetch);
        assert_eq!(again.fetch.commits, 0);
    }

    #[test]
    fn a_store_that_ran_init_takes_the_remotes_history_on_its_first_pull() {
        let (hub, a) = hub_and_a();
        let b = repo();
        b.init().unwrap();
        let pulled = b
            .pull("hub", &InProcessRemote::new(&hub), "main", merge_opts())
            .unwrap();
        assert_eq!(
            pulled.outcome,
            PullOutcome::Created(a.head("main").unwrap())
        );
        assert_eq!(b.get_json("main", "/x").unwrap(), json!(1));

        // A store with its own work on an unrelated history isn't replaced.
        let c = repo();
        c.init().unwrap();
        c.set_json("main", "/mine", &json!(true), opts("mine"))
            .unwrap();
        let before = c.head("main").unwrap();
        assert!(
            c.pull("hub", &InProcessRemote::new(&hub), "main", merge_opts())
                .is_err()
        );
        assert_eq!(c.head("main").unwrap(), before);
    }

    #[test]
    fn a_hub_that_ran_init_takes_a_stores_history_after_one_pull() {
        let hub = repo();
        hub.init().unwrap();
        let a = repo();
        a.init().unwrap();
        a.set_json("main", "/x", &json!(1), opts("x")).unwrap();
        assert!(
            matches!(
                a.push("hub", &InProcessRemote::new(&hub), "main"),
                Err(SyncError::Rejected { .. })
            ),
            "the hub's main exists, and a hasn't seen it"
        );
        let pulled = a
            .pull("hub", &InProcessRemote::new(&hub), "main", merge_opts())
            .unwrap();
        assert_eq!(pulled.outcome, PullOutcome::UpToDate, "nothing to take");
        a.push("hub", &InProcessRemote::new(&hub), "main").unwrap();
        assert_eq!(hub.head("main").unwrap(), a.head("main").unwrap());

        // Only init's empty commit gives way: a hub ref with work doesn't.
        let b = repo();
        b.init().unwrap();
        b.set_json("main", "/y", &json!(2), opts("y")).unwrap();
        b.fetch("hub", &InProcessRemote::new(&hub), &[]).unwrap();
        assert!(matches!(
            b.push("hub", &InProcessRemote::new(&hub), "main"),
            Err(SyncError::NotFastForward { .. })
        ));
    }

    #[test]
    fn a_second_push_sends_only_what_the_remote_lacks() {
        let (hub, a) = hub_and_a();
        a.set_json("main", "/y", &json!({"deep": {"er": 2}}), opts("y"))
            .unwrap();
        let report = a.push("hub", &InProcessRemote::new(&hub), "main").unwrap();
        assert_eq!(report.commits, 1, "{report:?}");
        // The new leaf, the maps above it, and the new root; /x is shared.
        assert!(report.objects <= 4, "{report:?}");
        assert_eq!(hub.get_json("main", "/y/deep/er").unwrap(), json!(2));

        // Nothing new: nothing sent, nothing moved.
        let again = a.push("hub", &InProcessRemote::new(&hub), "main").unwrap();
        assert!(again.updated.is_empty(), "{again:?}");
    }

    #[test]
    fn a_diverged_push_is_rejected_and_fetch_merge_push_resolves_it() {
        let (hub, a) = hub_and_a();
        let b = repo();
        b.pull("hub", &InProcessRemote::new(&hub), "main", merge_opts())
            .unwrap();

        a.set_json("main", "/from_a", &json!("a"), opts("a"))
            .unwrap();
        a.push("hub", &InProcessRemote::new(&hub), "main").unwrap();
        b.set_json("main", "/from_b", &json!("b"), opts("b"))
            .unwrap();
        let before = hub.head("main").unwrap();
        match b.push("hub", &InProcessRemote::new(&hub), "main") {
            Err(e @ SyncError::Rejected { .. }) => {
                assert_eq!(e.code(), "push_rejected");
                assert!(e.to_string().contains("fetch and merge"), "{e}");
            }
            other => panic!("expected a rejection, got {other:?}"),
        }
        assert_eq!(hub.head("main").unwrap(), before, "the hub didn't move");

        let pulled = b
            .pull("hub", &InProcessRemote::new(&hub), "main", merge_opts())
            .unwrap();
        assert!(
            matches!(pulled.outcome, PullOutcome::Merged(_)),
            "{pulled:?}"
        );
        b.push("hub", &InProcessRemote::new(&hub), "main").unwrap();
        assert_eq!(hub.get_json("main", "/from_a").unwrap(), json!("a"));
        assert_eq!(hub.get_json("main", "/from_b").unwrap(), json!("b"));

        let fast = a
            .pull("hub", &InProcessRemote::new(&hub), "main", merge_opts())
            .unwrap();
        assert!(
            matches!(fast.outcome, PullOutcome::FastForward(_)),
            "{fast:?}"
        );
        assert_eq!(a.head("main").unwrap(), hub.head("main").unwrap());
    }

    #[test]
    fn a_push_that_isnt_a_fast_forward_is_refused_by_the_receiver() {
        let (hub, a) = hub_and_a();
        let old = hub.head("main").unwrap();
        // A commit on a different line of history.
        let other = repo();
        other.init().unwrap();
        other.set_json("main", "/z", &json!(1), opts("z")).unwrap();
        let head = other.head("main").unwrap();
        let store = other.taint_storage();
        let set = missing_set(store, &[head], &[], &[]).unwrap();
        let bundle = write_set(store, &set, &[], Vec::new()).unwrap();
        let update = RefUpdate {
            namespace: Namespace::default_ns(),
            name: "main".into(),
            old: Some(old),
            new: head,
            overrides: Vec::new(),
        };
        assert_eq!(
            hub.sync_receive(&update, &bundle[..], BundleLimits::default(), "test")
                .unwrap(),
            PushOutcome::NotFastForward
        );
        assert_eq!(hub.head("main").unwrap(), old);
        drop(a);
    }

    #[test]
    fn the_receiver_refuses_reserved_and_tracking_refs() {
        let hub = repo();
        hub.init().unwrap();
        for name in ["_control-indexed", "remotes/x/main", ""] {
            let update = RefUpdate {
                namespace: Namespace::default_ns(),
                name: name.into(),
                old: None,
                new: hub.head("main").unwrap(),
                overrides: Vec::new(),
            };
            assert!(
                matches!(
                    hub.sync_receive(&update, &[][..], BundleLimits::default(), "test"),
                    Err(SyncError::Refused(_))
                ),
                "{name:?}"
            );
        }
    }

    #[test]
    fn a_tampered_or_incomplete_push_moves_nothing() {
        let (hub, a) = hub_and_a();
        a.set_json("main", "/y", &json!([1, 2, 3]), opts("y"))
            .unwrap();
        let head = a.head("main").unwrap();
        let old = hub.head("main").unwrap();
        let store = a.taint_storage();
        let set = missing_set(store, &[head], &[old], &[]).unwrap();
        let update = RefUpdate {
            namespace: Namespace::default_ns(),
            name: "main".into(),
            old: Some(old),
            new: head,
            overrides: Vec::new(),
        };

        // A bundle with one byte flipped near the end fails to verify.
        let mut bytes = write_set(store, &set, &[], Vec::new()).unwrap();
        let at = bytes.len() - 3;
        bytes[at] ^= 0xff;
        assert!(
            hub.sync_receive(&update, &bytes[..], BundleLimits::default(), "test")
                .is_err()
        );
        assert_eq!(hub.head("main").unwrap(), old);

        // A bundle that leaves out an object the new tree needs is incomplete.
        let mut bundle = Bundle::from_bytes(
            &write_set(store, &set, &[], Vec::new()).unwrap(),
            &BundleLimits::default(),
        )
        .unwrap();
        bundle.objects.retain(|o| !matches!(o, Object::Atom(_)));
        match hub.sync_receive(
            &update,
            &bundle.to_bytes().unwrap()[..],
            BundleLimits::default(),
            "test",
        ) {
            Err(e) => assert_eq!(e.code(), "bundle_incomplete", "{e}"),
            other => panic!("expected an incomplete bundle, got {other:?}"),
        }
        assert_eq!(hub.head("main").unwrap(), old);
    }

    #[test]
    fn tracking_refs_are_reserved_and_hidden() {
        let (_hub, a) = hub_and_a();
        assert_eq!(
            a.list_branches(None).unwrap(),
            vec![("main".to_string(), a.head("main").unwrap())]
        );
        assert!(matches!(
            a.set_json("remotes/hub/main", "/x", &json!(2), opts("x")),
            Err(RepoError::ReservedRef(_))
        ));
        assert!(matches!(
            a.branch("remotes/hub/other", "main"),
            Err(RepoError::ReservedRef(_))
        ));
        // Read through it, though: a branch can start from one.
        a.branch("copy", "remotes/hub/main").unwrap();
    }

    #[test]
    fn a_taint_made_on_one_store_holds_on_another_after_a_pull() {
        let (hub, a) = hub_and_a();
        let b = repo();
        b.pull("hub", &InProcessRemote::new(&hub), "main", merge_opts())
            .unwrap();

        let taint = a.taint("main", "/x", taint_params("pii")).unwrap();
        a.push("hub", &InProcessRemote::new(&hub), "main").unwrap();
        assert!(
            hub.get_taint(&taint).unwrap().is_some(),
            "the hub's table is rebuilt from the pushed control ref"
        );

        let pulled = b
            .pull("hub", &InProcessRemote::new(&hub), "main", merge_opts())
            .unwrap();
        assert!(pulled.fetch.control.is_some(), "{pulled:?}");
        assert!(b.get_taint(&taint).unwrap().is_some());
        assert_eq!(b.reconcile_control().unwrap(), ControlReport::default());
    }

    #[test]
    fn control_refs_changed_on_both_sides_merge() {
        let (hub, a) = hub_and_a();
        let b = repo();
        b.pull("hub", &InProcessRemote::new(&hub), "main", merge_opts())
            .unwrap();
        a.create_epoch("from-a", "a's work", vec![]).unwrap();
        a.push("hub", &InProcessRemote::new(&hub), "main").unwrap();
        b.create_epoch("from-b", "b's work", vec![]).unwrap();

        // b's control ref and the hub's have diverged: the push is rejected
        // before main is sent.
        match b.push("hub", &InProcessRemote::new(&hub), "main") {
            Err(SyncError::Rejected { name, .. }) => assert_eq!(name, CONTROL_REF),
            other => panic!("expected a rejection, got {other:?}"),
        }
        let fetched = b.fetch("hub", &InProcessRemote::new(&hub), &[]).unwrap();
        assert!(fetched.control.is_some(), "{fetched:?}");
        assert!(b.get_epoch("from-a").is_ok());
        assert!(b.get_epoch("from-b").is_ok());
        b.push("hub", &InProcessRemote::new(&hub), "main").unwrap();
        assert!(hub.get_epoch("from-b").is_ok());

        a.fetch("hub", &InProcessRemote::new(&hub), &[]).unwrap();
        assert!(a.get_epoch("from-b").is_ok());
        let store = a.taint_storage();
        let ns = Namespace::default_ns();
        assert_eq!(
            store.get_ref(&ns, CONTROL_REF).unwrap(),
            hub.taint_storage().get_ref(&ns, CONTROL_REF).unwrap(),
            "a fast-forwards to the merged control ref"
        );
    }

    #[test]
    fn refs_in_another_namespace_sync_there() {
        let (hub, a) = hub_and_a();
        a.create_namespace("team").unwrap();
        let team = a.fork_namespace(Namespace::new("team").unwrap());
        team.init().unwrap();
        team.set_json("main", "/t", &json!("team"), opts("t"))
            .unwrap();
        team.push("hub", &InProcessRemote::new(&hub), "main")
            .unwrap();
        let hub_team = hub.fork_namespace(Namespace::new("team").unwrap());
        assert_eq!(hub_team.get_json("main", "/t").unwrap(), json!("team"));
        assert_eq!(
            hub.get_json("main", "/x").unwrap(),
            json!(1),
            "default/main untouched"
        );

        let b = repo();
        b.create_namespace("team").unwrap();
        let b_team = b.fork_namespace(Namespace::new("team").unwrap());
        let fetched = b_team
            .fetch("hub", &InProcessRemote::new(&hub), &[])
            .unwrap();
        let names: Vec<&str> = fetched.updated.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"main"), "{names:?}");
        assert_eq!(
            b_team.remote_refs("hub").unwrap(),
            vec![("main".to_string(), team.head("main").unwrap())]
        );
    }

    /// Credential-shaped text, put together at run time so no source file
    /// holds one.
    fn fake_token() -> String {
        ["gh", "p_", &"a1".repeat(18)].concat()
    }

    fn migrate(what: &str) -> CommitOptions {
        CommitOptions::new("agent/sync", IntentCategory::Migrate, what)
    }

    #[test]
    fn the_receiver_logs_each_push_it_takes() {
        let (hub, a) = hub_and_a();
        let first = hub.push_log(10).unwrap();
        assert_eq!(first.len(), 1, "{first:?}");
        assert_eq!(first[0].pusher, "in-process");
        assert_eq!(first[0].name, "main");
        assert_eq!(first[0].old, None);
        assert_eq!(first[0].new, a.head("main").unwrap().to_hex());

        let old = a.head("main").unwrap();
        a.taint("main", "/x", taint_params("pii")).unwrap();
        a.set_json("main", "/y", &json!(2), opts("y")).unwrap();
        let remote = InProcessRemote::new(&hub).as_pusher("key/alice");
        a.push("hub", &remote, "main").unwrap();
        let log = hub.push_log(10).unwrap();
        let names: Vec<(&str, &str)> = log
            .iter()
            .map(|e| (e.name.as_str(), e.pusher.as_str()))
            .collect();
        assert_eq!(
            names,
            vec![
                ("main", "key/alice"),
                ("_control", "key/alice"),
                ("main", "in-process")
            ],
            "newest first: the control ref went before main"
        );
        assert_eq!(log[0].old, Some(old.to_hex()));
        assert_eq!(hub.push_log(1).unwrap().len(), 1);
        assert!(
            hub.list_branches(None)
                .unwrap()
                .iter()
                .all(|(n, _)| n == "main"),
            "the push log isn't a branch"
        );
        let offered = InProcessRemote::new(&hub).list_refs().unwrap();
        assert!(
            offered
                .iter()
                .all(|r| r.name != crate::sync::push_log::PUSH_LOG_REF)
        );
    }

    /// A remote that remembers the updates pushed to it.
    struct Recording<'a> {
        inner: InProcessRemote<'a>,
        updates: std::sync::Mutex<Vec<RefUpdate>>,
    }

    impl Remote for Recording<'_> {
        fn list_refs(&self) -> Result<Vec<BundleRef>, SyncError> {
            self.inner.list_refs()
        }
        fn fetch(&self, request: &FetchRequest) -> Result<Box<dyn Read + Send + '_>, SyncError> {
            self.inner.fetch(request)
        }
        fn push(&self, update: &RefUpdate, bundle: &[u8]) -> Result<PushOutcome, SyncError> {
            self.updates.lock().unwrap().push(update.clone());
            self.inner.push(update, bundle)
        }
    }

    #[test]
    fn a_credential_blocks_the_push_until_it_is_overridden() {
        let (hub, a) = hub_and_a();
        let before = hub.head("main").unwrap();
        a.set_json(
            "main",
            "/notes/n1",
            &json!(format!("use {}", fake_token())),
            opts("n1"),
        )
        .unwrap();
        let finding = match a.push("hub", &InProcessRemote::new(&hub), "main") {
            Err(e @ SyncError::Blocked { .. }) => {
                assert_eq!(e.code(), "push_blocked");
                assert!(
                    !e.to_string().contains(&fake_token()),
                    "the error never shows the value: {e}"
                );
                let SyncError::Blocked { name, findings } = e else {
                    unreachable!()
                };
                assert_eq!(name, "main");
                assert_eq!(findings.len(), 1, "{findings:?}");
                assert_eq!(findings[0].rule, "github-token");
                assert_eq!(findings[0].path, "/notes/n1");
                assert!(findings[0].overridable);
                findings[0].clone()
            }
            other => panic!("expected a blocked push, got {other:?}"),
        };
        assert_eq!(hub.head("main").unwrap(), before, "nothing was sent");

        let remote = Recording {
            inner: InProcessRemote::new(&hub),
            updates: Default::default(),
        };
        let options = PushOptions::default().allow(finding.id.clone(), "a revoked test token");
        let report = a.push_with("hub", &remote, "main", &options).unwrap();
        assert_eq!(report.overridden.len(), 1);
        assert_eq!(report.overridden[0].0, finding);
        assert_eq!(hub.head("main").unwrap(), a.head("main").unwrap());
        let updates = remote.updates.lock().unwrap();
        let main = updates.iter().find(|u| u.name == "main").unwrap();
        assert_eq!(
            main.overrides,
            vec![PushOverride {
                finding: finding.id.clone(),
                reason: "a revoked test token".into(),
            }]
        );
        drop(updates);
        let logged = hub.push_log(1).unwrap();
        assert_eq!(logged[0].name, "main");
        assert_eq!(logged[0].overrides.len(), 1);
        assert_eq!(logged[0].overrides[0].finding, finding.id);

        // Once the remote has the value, later pushes don't send it again
        // and aren't blocked by it.
        a.set_json("main", "/other", &json!(2), opts("other"))
            .unwrap();
        a.push("hub", &InProcessRemote::new(&hub), "main").unwrap();
    }

    #[test]
    fn a_credential_added_and_removed_before_the_push_still_blocks_it() {
        let (hub, a) = hub_and_a();
        a.set_json("main", "/notes/n", &json!(fake_token()), opts("add"))
            .unwrap();
        a.delete("main", "/notes/n", opts("remove")).unwrap();
        assert!(
            matches!(
                a.push("hub", &InProcessRemote::new(&hub), "main"),
                Err(SyncError::Blocked { .. })
            ),
            "the history ships, so the value would too"
        );
        // Without filters, only the rules that always apply are checked.
        a.push_with(
            "hub",
            &InProcessRemote::new(&hub),
            "main",
            &PushOptions::unfiltered(),
        )
        .unwrap();
    }

    #[test]
    fn secrets_never_sync_even_overridden() {
        let (hub, a) = hub_and_a();
        a.set_json("main", "/_meta/_secret/key", &json!("k"), migrate("secret"))
            .unwrap();
        let finding = match a.push("hub", &InProcessRemote::new(&hub), "main") {
            Err(SyncError::Blocked { findings, .. }) => {
                assert_eq!(findings.len(), 1, "{findings:?}");
                assert_eq!(findings[0].filter, "secrets");
                assert!(!findings[0].overridable);
                findings[0].clone()
            }
            other => panic!("expected a blocked push, got {other:?}"),
        };
        let options = PushOptions::unfiltered().allow(finding.id, "please");
        assert!(matches!(
            a.push_with("hub", &InProcessRemote::new(&hub), "main", &options),
            Err(SyncError::Blocked { .. })
        ));
    }

    #[test]
    fn the_receiver_refuses_secrets_however_they_arrive() {
        let (hub, a) = hub_and_a();
        let old = hub.head("main").unwrap();
        a.set_json("main", "/_meta/_secret/key", &json!("k"), migrate("secret"))
            .unwrap();
        let head = a.head("main").unwrap();
        let store = a.taint_storage();
        let set = missing_set(store, &[head], &[old], &[]).unwrap();
        let bundle = write_set(store, &set, &[], Vec::new()).unwrap();
        let update = RefUpdate {
            namespace: Namespace::default_ns(),
            name: "main".into(),
            old: Some(old),
            new: head,
            overrides: Vec::new(),
        };
        match hub.sync_receive(&update, &bundle[..], BundleLimits::default(), "test") {
            Err(e @ SyncError::Refused(_)) => assert!(e.to_string().contains("_secret"), "{e}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert_eq!(hub.head("main").unwrap(), old);
    }

    #[test]
    fn a_local_only_ref_is_never_pushed_or_offered() {
        let (hub, a) = hub_and_a();
        a.branch("index", "main").unwrap();
        a.set_local_only("index", true).unwrap();
        assert!(a.is_local_only("index").unwrap());
        assert!(!a.is_local_only("main").unwrap());
        match a.push("hub", &InProcessRemote::new(&hub), "index") {
            Err(e @ SyncError::LocalOnly(_)) => assert_eq!(e.code(), "local_only_ref"),
            other => panic!("expected a local-only refusal, got {other:?}"),
        }

        // A hub's own local-only ref isn't offered or fetchable.
        hub.branch("hub-index", "main").unwrap();
        hub.set_local_only("hub-index", true).unwrap();
        let offered = InProcessRemote::new(&hub).list_refs().unwrap();
        assert!(offered.iter().all(|r| r.name != "hub-index"), "{offered:?}");
        assert!(matches!(
            a.fetch("hub", &InProcessRemote::new(&hub), &["hub-index"]),
            Err(SyncError::NoSuchRef(_))
        ));

        // Cleared, it pushes.
        a.set_local_only("index", false).unwrap();
        a.push("hub", &InProcessRemote::new(&hub), "index").unwrap();
        assert!(hub.head("index").is_ok());
    }

    #[test]
    fn an_embedders_filter_applies() {
        struct NoInternalIds;
        impl crate::sync::PushFilter for NoInternalIds {
            fn name(&self) -> &str {
                "internal"
            }
            fn check(&self, _path: &str, value: &str) -> Vec<String> {
                if value.contains("INTERNAL-") {
                    vec!["internal-id".into()]
                } else {
                    Vec::new()
                }
            }
        }
        let (hub, a) = hub_and_a();
        a.set_json("main", "/n", &json!("ticket INTERNAL-42"), opts("n"))
            .unwrap();
        let options = PushOptions::default().with_filter(std::sync::Arc::new(NoInternalIds));
        match a.push_with("hub", &InProcessRemote::new(&hub), "main", &options) {
            Err(SyncError::Blocked { findings, .. }) => {
                assert_eq!(findings[0].filter, "internal");
                assert_eq!(findings[0].rule, "internal-id");
            }
            other => panic!("expected a blocked push, got {other:?}"),
        }
    }

    #[test]
    fn the_merge_base_is_readable_with_branch_refs_only() {
        let limited = Repository::from_shared(
            Arc::new(MemoryStorage::new()) as Arc<dyn Storage + Send + Sync>
        )
        .with_branch_refs_only();
        limited.init().unwrap();
        limited
            .set_json("main", "/v", &json!(1), opts("v1"))
            .unwrap();
        limited.branch("side", "main").unwrap();
        limited
            .set_json("main", "/v", &json!(2), opts("v2"))
            .unwrap();
        limited
            .set_json("side", "/v", &json!(3), opts("v3"))
            .unwrap();
        assert_eq!(
            limited
                .get_json_at_merge_base("side", "main", "/v")
                .unwrap(),
            Some(json!(1))
        );
        assert_eq!(
            limited
                .get_json_at_merge_base("side", "main", "/nope")
                .unwrap(),
            None
        );

        // Unrelated histories have no merge base.
        let (x, y) = (repo(), repo());
        x.init().unwrap();
        x.set_json("main", "/v", &json!("x"), opts("x")).unwrap();
        y.init().unwrap();
        y.fetch("x", &InProcessRemote::new(&x), &[]).unwrap();
        assert_eq!(
            y.get_json_at_merge_base("remotes/x/main", "main", "/v")
                .unwrap(),
            None
        );
    }

    #[test]
    fn fetching_a_ref_the_remote_lacks_is_an_error() {
        let (hub, a) = hub_and_a();
        match a.fetch("hub", &InProcessRemote::new(&hub), &["nope"]) {
            Err(e @ SyncError::NoSuchRef(_)) => assert_eq!(e.code(), "no_such_ref"),
            other => panic!("expected no such ref, got {other:?}"),
        }
        assert!(matches!(
            a.fetch("bad/name", &InProcessRemote::new(&hub), &[]),
            Err(SyncError::Transfer(_))
        ));
        assert!(matches!(
            a.fetch("hub", &InProcessRemote::new(&hub), &["_control"]),
            Err(SyncError::Transfer(TransferError::Repo(
                RepoError::ReservedRef(_)
            )))
        ));
    }
}
