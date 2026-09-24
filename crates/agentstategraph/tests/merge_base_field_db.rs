//! Merge base and merge preview against a real field DB.
//!
//! Ignored by default — it needs a large local database that is not shipped to
//! CI. Opening a store runs schema migrations, so point it at a *copy*.
//!
//! This is the case that motivated the linear merge base: on a CTX store, the
//! merge base of a plan branch with 1,304 common ancestors against `main` never
//! finished, and because every commit read holds the storage connection lock,
//! the stalled merge starved every other request to the hub.
//!
//! Run:
//!   ASG_MERGE_FIELD_DB=/path/to/copy.db \
//!   ASG_MERGE_NAMESPACE=sessiondrift \
//!   ASG_MERGE_SOURCE=codex-live-patch-authoring-workflow \
//!   ASG_MERGE_TARGET=main \
//!     cargo test -p agentstategraph --test merge_base_field_db -- --ignored --nocapture

use std::time::{Duration, Instant};

use agentstategraph::Repository;
use agentstategraph_core::Namespace;
use agentstategraph_storage::SqliteStorage;

#[test]
#[ignore = "needs ASG_MERGE_FIELD_DB pointing at a copy of a real field DB"]
fn merge_base_and_preview_finish_on_a_real_store() {
    let Ok(path) = std::env::var("ASG_MERGE_FIELD_DB") else {
        eprintln!("skip: set ASG_MERGE_FIELD_DB to a copy of a field DB");
        return;
    };
    let var = |k: &str, default: &str| std::env::var(k).unwrap_or_else(|_| default.to_string());
    let ns = var("ASG_MERGE_NAMESPACE", "default");
    let source = var("ASG_MERGE_SOURCE", "main");
    let target = var("ASG_MERGE_TARGET", "main");

    let storage = SqliteStorage::open(&path).expect("open field db copy");
    let repo = Repository::new(Box::new(storage))
        .with_namespace(Namespace::new(ns.clone()).expect("namespace"));

    let t0 = Instant::now();
    let base = repo.merge_base(&source, &target).expect("merge base");
    let base_took = t0.elapsed();
    println!(
        "{ns}: merge_base({source}, {target}) = {} in {base_took:?}",
        base.short()
    );

    let t1 = Instant::now();
    let preview = repo.preview_merge(&source, &target).expect("preview merge");
    let preview_took = t1.elapsed();
    println!("{ns}: preview_merge in {preview_took:?} -> {preview:?}");

    // Generous: the point is "finishes", against a baseline of "never did".
    assert!(
        base_took < Duration::from_secs(60),
        "merge base took {base_took:?}"
    );
    assert!(
        preview_took < Duration::from_secs(120),
        "preview took {preview_took:?}"
    );
}
