//! Schema setup on Postgres (DESIGN.md Plan D t-004/t-006/t-008): the DDL
//! runs once per schema version, not on every connect, and an older database
//! is brought up to date, its existing commits included. Uses its own throwaway database, so rewinding the
//! schema can't disturb other tests.
//!
//! Skipped unless `TEST_DATABASE_URL` is set (its user needs CREATEDB);
//! `ASG_REQUIRE_POSTGRES` turns the skip into a failure.

#![cfg(feature = "postgres")]

use agentstategraph_core::{
    Authority, Commit, CommitBuilder, Epoch, Intent, IntentCategory, Object, ObjectId,
};
use agentstategraph_storage::{CommitStore, CommitTrace, EpochStore, ObjectStore, PostgresStorage};
use chrono::Utc;

fn url() -> Option<String> {
    match std::env::var("TEST_DATABASE_URL") {
        Ok(u) if !u.is_empty() => Some(u),
        _ => {
            let required =
                std::env::var("ASG_REQUIRE_POSTGRES").is_ok_and(|v| !v.is_empty() && v != "0");
            assert!(
                !required,
                "ASG_REQUIRE_POSTGRES is set, but TEST_DATABASE_URL isn't"
            );
            eprintln!("skip: TEST_DATABASE_URL not set");
            None
        }
    }
}

/// `url` pointed at database `name` instead.
fn with_database(url: &str, name: &str) -> String {
    let (base, query) = match url.split_once('?') {
        Some((b, q)) => (b, format!("?{q}")),
        None => (url, String::new()),
    };
    let slash = base.rfind('/').expect("URL has a database path");
    format!("{}/{name}{query}", &base[..slash])
}

/// Run SQL against `url` directly, outside the storage layer.
fn admin(url: &str, sql: &str) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let (client, conn) = tokio_postgres::connect(url, tokio_postgres::NoTls)
            .await
            .expect("admin connect");
        tokio::spawn(conn);
        client.batch_execute(sql).await.expect("admin sql");
    });
}

fn query_one_i64(url: &str, sql: &str) -> i64 {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let (client, conn) = tokio_postgres::connect(url, tokio_postgres::NoTls)
            .await
            .expect("admin connect");
        tokio::spawn(conn);
        client
            .query_one(sql, &[])
            .await
            .expect("admin query")
            .get(0)
    })
}

#[test]
fn an_older_database_is_upgraded_once_and_then_left_alone() {
    let Some(url) = url() else { return };
    let name = format!("asg_schema_{}", uuid::Uuid::new_v4().simple());
    admin(&url, &format!("CREATE DATABASE {name}"));
    let db = with_database(&url, &name);

    // A current database with a merge in its history, rewound to look like
    // one from before the version stamp (version 1), epoch seal digests, the
    // version 2 history and search tables, the version 3 GC columns, the
    // version 4 commit traces, and the version 5 tenant policies.
    let old = PostgresStorage::connect_tenant_blocking(&db, "t", 2).unwrap();
    let commit = |tag: &str, parents: &[ObjectId]| -> Commit {
        let root = old.put_object(&Object::string(tag)).unwrap();
        let c = CommitBuilder::new(
            root,
            "agent/schema",
            Authority::simple("test"),
            Intent::new(IntentCategory::Refine, tag),
        )
        .parents(parents.to_vec())
        .build();
        old.put_commit(&c).unwrap();
        c
    };
    let base = commit("base", &[]);
    let left = commit("left", &[base.id]);
    let right = commit("right", &[base.id]);
    let merge = commit("merge", &[left.id, right.id]);
    drop(old);
    admin(
        &db,
        "DROP TABLE asg_schema;
         ALTER TABLE epochs DROP COLUMN seal_hash;
         DROP INDEX idx_commits_data_id;
         ALTER TABLE commits DROP COLUMN xid;
         DROP TABLE asg_history_cursor, asg_history_commit_rollup, asg_history_milestone,
                    leaf_rows, leaf_index_refs, gc_pending_roots, commit_traces;
         ALTER TABLE objects DROP COLUMN written_at;
         DO $$ DECLARE t text; BEGIN
             FOR t IN SELECT tablename FROM pg_policies WHERE policyname = 'asg_tenant' LOOP
                 EXECUTE format('DROP POLICY asg_tenant ON %I', t);
             END LOOP;
         END $$;",
    );
    let policies = |db: &str| {
        query_one_i64(
            db,
            "SELECT COUNT(*) FROM pg_policies WHERE policyname = 'asg_tenant'",
        )
    };
    assert_eq!(policies(&db), 0);

    let storage = PostgresStorage::connect_tenant_blocking(&db, "t", 2).unwrap();
    assert_eq!(
        query_one_i64(
            &db,
            "SELECT version::BIGINT FROM asg_schema WHERE component = 'postgres'"
        ),
        5,
        "the upgrade stamps the schema version"
    );
    assert_eq!(
        policies(&db),
        agentstategraph_storage::postgres::TENANT_TABLES.len() as i64,
        "every tenant table has the tenant policy"
    );
    let trace = CommitTrace {
        trace_id: "4bf92f3577b34da6a3ce929d0e0e4736".into(),
        span_id: "00f067aa0ba902b7".into(),
    };
    storage.set_commit_trace(&merge.id, &trace).unwrap();
    assert_eq!(
        storage.get_commit_trace(&merge.id).unwrap(),
        Some(trace),
        "the commit traces table is back"
    );
    storage
        .create_epoch(&Epoch::new("e", "upgraded", vec![]))
        .unwrap();
    let seal = ObjectId::hash(b"seal");
    storage
        .seal_epoch("e", "done", Utc::now(), &[], &seal)
        .unwrap();
    assert_eq!(
        storage.get_epoch("e").unwrap().unwrap().seal_hash,
        Some(seal),
        "the dropped column is back"
    );

    // Commits from before the upgrade walk and fold like new ones.
    let walk = storage.list_commits_dag(&merge.id, 10).unwrap();
    assert_eq!(walk.commits.len(), 4);
    assert!(!walk.truncated);
    assert_eq!(storage.history_undistilled_commit_count().unwrap(), 4);
    assert_eq!(storage.history_extract_batch(10).unwrap(), 4);
    assert_eq!(storage.history_undistilled_commit_count().unwrap(), 0);
    storage
        .leaf_index_build("default", "main", &[("/a".into(), "found".into())])
        .unwrap();
    assert_eq!(
        storage
            .leaf_index_search("default", "main", "fou", 10)
            .unwrap(),
        Some(vec![("/a".into(), "found".into())])
    );

    // Objects from before the upgrade have no write time, so no grace period
    // spares them: no ref reaches these four, and a sweep takes them at once.
    assert_eq!(
        storage.history_gc_sweep(&[], 100).unwrap().objects_deleted,
        4
    );

    // Up to date: connecting again runs no DDL. A column dropped by hand stays
    // dropped, which shows the setup didn't touch the tables.
    admin(&db, "ALTER TABLE sessions DROP COLUMN scope_namespace;");
    drop(PostgresStorage::connect_tenant_blocking(&db, "t2", 2).unwrap());
    assert_eq!(
        query_one_i64(
            &db,
            "SELECT COUNT(*) FROM information_schema.columns
             WHERE table_name = 'sessions' AND column_name = 'scope_namespace'"
        ),
        0,
        "an up-to-date database must not re-run the DDL"
    );

    drop(storage);
    admin(&url, &format!("DROP DATABASE {name} WITH (FORCE)"));
}
