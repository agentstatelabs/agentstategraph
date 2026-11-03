//! Whole-store export and import (DESIGN.md Plan F t-008), as bundles.
//!
//! An export carries every ref in every namespace, the commits they reach
//! and the objects those commits' trees need, so it carries taints,
//! reminders and epochs too, on the control ref. It leaves out what each
//! store keeps for itself: the control ref's bookkeeping refs, sessions, the
//! search index and the history rollups, all of which an import rebuilds or
//! starts afresh.
//!
//! An import stores objects and commits as the bundle reader verifies them,
//! and moves no ref until the whole bundle has been read and checked: each
//! ref's commit is there with its whole tree, and every commit's parents are
//! there or it's marked shallow. A bundle that fails leaves nothing
//! reachable, for a GC sweep to take. Then the taint, reminder and epoch
//! tables are rebuilt from the imported control ref.
//!
//! Retention, the third part of a store's lifecycle, is
//! [`Repository::gc_sweep`] with a [`crate::RetentionPolicy`], which always
//! keeps sealed epochs' commits. An export of a swept store carries the
//! commits whose trees a sweep took, without the trees.

use std::collections::{BTreeSet, HashSet};
use std::io::{Read, Write};

use agentstategraph_core::{Namespace, Object, ObjectId};
use agentstategraph_storage::StorageError;

use crate::control::CONTROL_REF;
use crate::control_repo::{ControlReport, is_reserved_ref};
use crate::repo::{RepoError, Repository};
use crate::sync::bundle::{
    BundleError, BundleLimits, BundleReader, BundleRef, BundleWriter, Record,
};
use crate::sync::missing::missing_set;

/// How many objects one storage read or write handles.
const BATCH: usize = 1024;

/// Why an export or import failed.
#[derive(Debug, thiserror::Error)]
pub enum TransferError {
    #[error(transparent)]
    Bundle(#[from] BundleError),

    #[error(transparent)]
    Repo(#[from] RepoError),

    /// The bundle verified, but it doesn't hold everything its refs need.
    #[error("the bundle is incomplete: {0}")]
    Incomplete(String),

    /// Refs the bundle carries already point elsewhere in this store.
    #[error("refs already point elsewhere in this store: {0:?}")]
    RefConflict(Vec<String>),
}

impl From<StorageError> for TransferError {
    fn from(e: StorageError) -> Self {
        TransferError::Repo(RepoError::Storage(e))
    }
}

impl TransferError {
    /// A stable code for this kind of error.
    pub fn code(&self) -> &'static str {
        match self {
            TransferError::Bundle(e) => e.code(),
            TransferError::Repo(e) => e.code(),
            TransferError::Incomplete(_) => "bundle_incomplete",
            TransferError::RefConflict(_) => "ref_conflict",
        }
    }
}

/// What [`Repository::export_store`] wrote.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExportReport {
    pub refs: usize,
    pub commits: usize,
    pub objects: usize,
    /// Commits written without their parents, which this store no longer has.
    pub shallow: usize,
    /// Commits written without their trees, which a retention sweep took.
    pub pruned: usize,
}

/// What [`Repository::import_store`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub refs: usize,
    pub commits: usize,
    pub objects: usize,
    pub shallow: usize,
    /// Commits that came without their trees.
    pub pruned: usize,
    /// What rebuilding the taint, reminder and epoch tables did.
    pub control: ControlReport,
}

impl Repository {
    /// Write the whole store to `out` as a bundle, returning `out`.
    pub fn export_store<W: Write>(&self, out: W) -> Result<(W, ExportReport), TransferError> {
        // So the control ref carries every taint, reminder and epoch write.
        self.reconcile_control()?;
        let store = self.taint_storage();
        let mut refs = Vec::new();
        for ns in store.list_namespaces()? {
            for (name, target) in store.list_refs(&ns, "")? {
                if is_reserved_ref(&name) && name != CONTROL_REF {
                    continue;
                }
                refs.push(BundleRef {
                    namespace: ns.clone(),
                    name,
                    target,
                });
            }
        }
        let heads: Vec<ObjectId> = refs
            .iter()
            .map(|r| r.target)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let set = missing_set(store, &heads, &[], &[])?;

        let mut writer = BundleWriter::new(out)?;
        for chunk in set.objects.chunks(BATCH) {
            for (id, object) in chunk.iter().zip(store.batch_get_objects(chunk)?) {
                let object = object.ok_or_else(|| StorageError::ObjectNotFound(id.to_string()))?;
                writer.write_object(&object)?;
            }
        }
        for id in &set.commits {
            let commit = store
                .get_commit(id)?
                .ok_or_else(|| StorageError::CommitNotFound(id.to_string()))?;
            writer.write_commit(&commit)?;
        }
        for id in &set.shallow {
            writer.write_shallow(id)?;
        }
        for r in &refs {
            writer.write_ref(r)?;
        }
        let out = writer.finish()?;
        Ok((
            out,
            ExportReport {
                refs: refs.len(),
                commits: set.commits.len(),
                objects: set.objects.len(),
                shallow: set.shallow.len(),
                pruned: set.pruned.len(),
            },
        ))
    }

    /// Read a bundle from [`export_store`](Self::export_store) into this
    /// store, as the [module docs](crate::sync::store) describe. Refs the
    /// bundle carries must be absent here or already point where the bundle
    /// says; a store that has run [`Repository::init`] already has a `main`.
    /// `limits` bound what's read: [`BundleLimits::unlimited`] suits a
    /// trusted backup.
    pub fn import_store<R: Read>(
        &self,
        input: R,
        limits: BundleLimits,
    ) -> Result<ImportReport, TransferError> {
        let store = self.taint_storage();
        let mut reader = BundleReader::new(input, limits)?;
        let mut report = ImportReport::default();
        let mut refs = Vec::new();
        let mut shallow = HashSet::new();
        let mut commits: Vec<(ObjectId, ObjectId, Vec<ObjectId>)> = Vec::new();
        let mut pending: Vec<Object> = Vec::with_capacity(BATCH);
        while let Some(record) = reader.next_record()? {
            match record {
                Record::Object(_, object) => {
                    pending.push(object);
                    if pending.len() == BATCH {
                        store.batch_put_objects(&pending)?;
                        report.objects += pending.len();
                        pending.clear();
                    }
                }
                Record::Commit(commit) => {
                    store.put_commit(&commit)?;
                    commits.push((commit.id, commit.state_root, commit.parents.clone()));
                }
                Record::Shallow(id) => {
                    shallow.insert(id);
                }
                Record::Ref(r) => refs.push(r),
            }
        }
        store.batch_put_objects(&pending)?;
        report.objects += pending.len();
        report.commits = commits.len();
        report.shallow = shallow.len();

        // Everything each ref needs is here.
        for (id, root, parents) in &commits {
            if !shallow.contains(id) {
                for parent in parents {
                    if !store.has_commit(parent)? {
                        return Err(TransferError::Incomplete(format!(
                            "commit {id} has a parent, {parent}, that isn't here"
                        )));
                    }
                }
            }
            if !store.has_object(root)? {
                report.pruned += 1;
            }
        }
        for r in &refs {
            if !store.has_commit(&r.target)? {
                return Err(TransferError::Incomplete(format!(
                    "{}/{} points at {}, which isn't here",
                    r.namespace, r.name, r.target
                )));
            }
            if let Some(missing) = self.first_missing_object(&r.target)? {
                return Err(TransferError::Incomplete(format!(
                    "{}/{}'s tree lacks {missing}",
                    r.namespace, r.name
                )));
            }
        }

        // Refs move only once nothing above failed.
        let mut conflicts = Vec::new();
        for r in &refs {
            let here = match store.get_ref(&r.namespace, &r.name) {
                Ok(here) => here,
                Err(StorageError::NamespaceNotFound(_)) => None,
                Err(e) => return Err(e.into()),
            };
            if here.is_some_and(|t| t != r.target) {
                conflicts.push(format!("{}/{}", r.namespace, r.name));
            }
        }
        if !conflicts.is_empty() {
            return Err(TransferError::RefConflict(conflicts));
        }
        for r in &refs {
            if r.namespace != Namespace::default_ns() {
                self.create_namespace(r.namespace.as_str())?;
            }
            store.set_ref(&r.namespace, &r.name, r.target)?;
        }
        report.refs = refs.len();
        report.control = self.reconcile_control()?;
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CommitOptions;
    use agentstategraph_core::IntentCategory;
    use agentstategraph_storage::{MemoryStorage, Storage};
    use std::sync::Arc;

    fn repo() -> Repository {
        Repository::from_shared(Arc::new(MemoryStorage::new()) as Arc<dyn Storage + Send + Sync>)
    }

    fn opts(what: &str) -> CommitOptions {
        CommitOptions::new("agent/export", IntentCategory::Refine, what)
    }

    fn export(repo: &Repository) -> (Vec<u8>, ExportReport) {
        repo.export_store(Vec::new()).unwrap()
    }

    #[test]
    fn a_store_round_trips_through_an_export() {
        let a = repo();
        a.init().unwrap();
        a.set_json("main", "/x", &serde_json::json!({"n": 1}), opts("x"))
            .unwrap();
        a.branch("feature", "main").unwrap();
        a.set_json("feature", "/y", &serde_json::json!([1, 2]), opts("y"))
            .unwrap();
        a.create_namespace("team").unwrap();
        let team = a.fork_namespace(Namespace::new("team").unwrap());
        team.init().unwrap();
        team.set_json("main", "/t", &serde_json::json!("team"), opts("t"))
            .unwrap();
        a.create_epoch("e", "work", vec![]).unwrap();
        a.seal_epoch("e", "done").unwrap();

        let (bytes, exported) = export(&a);
        assert_eq!(exported.refs, 4, "{exported:?}"); // main, feature, team/main, _control
        let b = repo();
        let imported = b.import_store(&bytes[..], BundleLimits::default()).unwrap();
        assert_eq!(imported.refs, 4);
        assert_eq!(imported.commits, exported.commits);
        assert_eq!(imported.objects, exported.objects);
        assert_eq!(
            b.get_json("feature", "/y").unwrap(),
            serde_json::json!([1, 2])
        );
        let b_team = b.fork_namespace(Namespace::new("team").unwrap());
        assert_eq!(
            b_team.get_json("main", "/t").unwrap(),
            serde_json::json!("team")
        );
        assert_eq!(
            b.get_epoch("e").unwrap().seal_hash,
            a.get_epoch("e").unwrap().seal_hash
        );

        // Exported again, the copy is the same store.
        let (again, _) = export(&b);
        let c = repo();
        c.import_store(&again[..], BundleLimits::default()).unwrap();
        for ns in a.list_namespaces().unwrap() {
            let (ra, rc) = (
                a.fork_namespace(ns.clone()).list_branches(None).unwrap(),
                c.fork_namespace(ns).list_branches(None).unwrap(),
            );
            assert_eq!(ra, rc);
        }
    }

    #[test]
    fn importing_twice_is_harmless_and_a_ref_pointing_elsewhere_is_refused() {
        let a = repo();
        a.init().unwrap();
        a.set_json("main", "/x", &serde_json::json!(1), opts("x"))
            .unwrap();
        let (bytes, _) = export(&a);
        let b = repo();
        b.import_store(&bytes[..], BundleLimits::default()).unwrap();
        b.import_store(&bytes[..], BundleLimits::default()).unwrap();

        let c = repo();
        c.init().unwrap();
        let before = c.head("main").unwrap();
        match c.import_store(&bytes[..], BundleLimits::default()) {
            Err(TransferError::RefConflict(refs)) => assert_eq!(refs, vec!["default/main"]),
            other => panic!("expected a conflict, got {other:?}"),
        }
        assert_eq!(c.head("main").unwrap(), before, "no ref moved");
    }

    #[test]
    fn a_damaged_export_moves_no_ref() {
        let a = repo();
        a.init().unwrap();
        a.set_json("main", "/x", &serde_json::json!(1), opts("x"))
            .unwrap();
        let (bytes, _) = export(&a);
        let b = repo();
        assert!(
            b.import_store(&bytes[..bytes.len() - 10], BundleLimits::default())
                .is_err()
        );
        assert!(b.list_branches(None).unwrap().is_empty());
    }

    #[test]
    fn a_swept_store_exports_its_pruned_commits_without_their_trees() {
        // No grace period, so the sweep takes the old trees at once.
        let a = Repository::new(Box::new(
            agentstategraph_storage::SqliteStorage::in_memory()
                .unwrap()
                .with_gc_grace(std::time::Duration::ZERO),
        ));
        a.init().unwrap();
        for i in 0..6 {
            a.set_json("main", "/v", &serde_json::json!(i), opts("v"))
                .unwrap();
        }
        a.extract_history(1000).unwrap();
        let policy = crate::RetentionPolicy {
            keep_recent: 1,
            checkpoint_every: 0,
            keep_milestones: false,
        };
        a.gc_sweep(policy, true, false).unwrap();

        let (bytes, exported) = export(&a);
        assert!(exported.pruned > 0, "{exported:?}");
        let b = repo();
        let imported = b.import_store(&bytes[..], BundleLimits::default()).unwrap();
        assert_eq!(imported.pruned, exported.pruned);
        assert_eq!(b.get_json("main", "/v").unwrap(), serde_json::json!(5));
    }
}
