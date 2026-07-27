//! `--from`/merge RefSpec resolution: branch names, full commit hashes, and
//! unique `sg_`/hex commit-id prefixes, with distinct error variants.

use agentstategraph::{CommitOptions, RepoError, Repository};
use agentstategraph_core::{IntentCategory, Object};
use agentstategraph_storage::SqliteStorage;

fn repo() -> Repository {
    let r = Repository::new(Box::new(
        SqliteStorage::in_memory().expect("in-memory sqlite"),
    ));
    r.init().unwrap();
    r
}

fn opts(desc: &str) -> CommitOptions {
    CommitOptions::new("agent/test", IntentCategory::Checkpoint, desc)
}

/// Advance main and return its head commit id.
fn commit(r: &Repository, path: &str, v: i64, desc: &str) -> agentstategraph_core::ObjectId {
    r.set("main", path, &Object::int(v), opts(desc)).unwrap()
}

#[test]
fn branch_from_full_commit_hash() {
    let r = repo();
    let c = commit(&r, "/x", 1, "c1");
    commit(&r, "/y", 2, "c2"); // main moves on; c is now historical

    // sg_-prefixed full hash
    r.branch("recovery", &format!("{c}")).unwrap();
    assert_eq!(r.get("recovery", "/x").unwrap(), Object::int(1));
    // recovery is pinned at c, so /y must NOT be present
    assert!(r.get("recovery", "/y").is_err());
}

#[test]
fn branch_from_unique_prefix() {
    let r = repo();
    let c = commit(&r, "/x", 1, "c1");
    commit(&r, "/y", 2, "c2");

    let short = c.short(); // "sg_<12 hex>"
    r.branch("recovery", &short).unwrap();
    assert_eq!(r.get("recovery", "/x").unwrap(), Object::int(1));

    // Raw hex prefix (no sg_) must also resolve.
    let raw = &c.to_hex()[..10];
    r.branch("recovery2", raw).unwrap();
    assert_eq!(r.get("recovery2", "/x").unwrap(), Object::int(1));
}

#[test]
fn branch_from_orphaned_commit() {
    // A commit whose branch was deleted must still be resolvable by id.
    let r = repo();
    r.branch("temp", "main").unwrap();
    let c = r
        .set("temp", "/orphan", &Object::int(9), opts("on temp"))
        .unwrap();
    assert!(r.delete_branch("temp").unwrap());

    r.branch("recovery", &format!("{c}")).unwrap();
    assert_eq!(r.get("recovery", "/orphan").unwrap(), Object::int(9));
}

#[test]
fn unknown_branch_name_is_branch_not_found() {
    let r = repo();
    let err = r.branch("feat", "definitely-not-a-ref").unwrap_err();
    assert!(matches!(err, RepoError::BranchNotFound(_)), "got {err:?}");
}

#[test]
fn hex_shaped_miss_is_commit_not_found() {
    let r = repo();
    // Well-formed but nonexistent prefix.
    let err = r.branch("feat", "sg_deadbeef").unwrap_err();
    assert!(matches!(err, RepoError::CommitNotFound(_)), "got {err:?}");
}

#[test]
fn ambiguous_prefix_errors() {
    let r = repo();
    // With more than 16 commits, pigeonhole guarantees at least two commit ids
    // share the same first hex digit, so some 1-hex-digit prefix is ambiguous.
    for i in 0..24 {
        commit(&r, &format!("/k{i}"), i, "c");
    }

    let mut saw_ambiguous = false;
    for digit in "0123456789abcdef".chars() {
        if let Err(RepoError::AmbiguousCommitPrefix { count, .. }) =
            r.branch(&format!("b_{digit}"), &digit.to_string())
        {
            assert!(count >= 2);
            saw_ambiguous = true;
            break;
        }
    }
    assert!(
        saw_ambiguous,
        "expected some 1-hex-digit prefix to be ambiguous"
    );
}
