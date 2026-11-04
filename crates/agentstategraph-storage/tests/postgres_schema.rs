//! Schema setup on Postgres (DESIGN.md Plan D t-004/t-006/t-008): the DDL
//! runs once per schema version, not on every connect, and an older database
//! is brought up to date, its existing commits included. A database a newer
//! build set up is used as it is, unless it needs more than this build
//! understands (Plan F t-009). Each test uses its own throwaway database, so
//! rewinding the schema can't disturb other tests.
//!
//! Skipped unless `TEST_DATABASE_URL` is set (its user needs CREATEDB);
//! `ASG_REQUIRE_POSTGRES` turns the skip into a failure.

#![cfg(feature = "postgres")]

use agentstategraph_core::{
    Authority, Commit, CommitBuilder, Epoch, Intent, IntentCategory, Object, ObjectId,
};
use agentstategraph_storage::{
    CommitStore, CommitTrace, EpochStore, ObjectStore, PostgresStorage, StorageError,
};
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
    // version 4 commit traces, the version 5 tenant policies, and the version
    // 7 exact columns.
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
         ALTER TABLE objects DROP COLUMN written_at, DROP COLUMN exact;
         ALTER TABLE commits DROP COLUMN exact;
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
        7,
        "the upgrade stamps the schema version"
    );
    assert_eq!(
        query_one_i64(
            &db,
            "SELECT compatible_from::BIGINT FROM asg_schema WHERE component = 'postgres'"
        ),
        7,
        "and the oldest version a build must understand"
    );
    // Rows from before the exact column read as before, and new ones can
    // hold U+0000.
    assert_eq!(storage.get_commit(&merge.id).unwrap().unwrap().id, merge.id);
    let nul = Object::string("\u{0}");
    let id = storage.put_object(&nul).unwrap();
    assert_eq!(storage.get_object(&id).unwrap(), Some(nul));
    assert_eq!(
        query_one_i64(&db, "SELECT COUNT(*) FROM objects WHERE exact IS NOT NULL"),
        1,
        "only a row holding U+0000 keeps an exact copy"
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

/// A throwaway database set up by this build, as `(url, name)`.
fn fresh_database(url: &str) -> (String, String) {
    let name = format!("asg_schema_{}", uuid::Uuid::new_v4().simple());
    admin(url, &format!("CREATE DATABASE {name}"));
    let db = with_database(url, &name);
    drop(PostgresStorage::connect_tenant_blocking(&db, "t", 2).unwrap());
    (db, name)
}

#[test]
fn a_database_from_a_newer_compatible_build_is_used_as_it_is() {
    let Some(url) = url() else { return };
    let (db, name) = fresh_database(&url);

    // A newer build expanded the schema, so this one can still use it. It
    // must not migrate: the dropped column stays dropped.
    admin(
        &db,
        "UPDATE asg_schema SET version = 99, compatible_from = 5 WHERE component = 'postgres';
         ALTER TABLE sessions DROP COLUMN scope_namespace;",
    );
    let storage = PostgresStorage::connect_tenant_blocking(&db, "new-tenant", 2).unwrap();
    let id = storage.put_object(&Object::string("works")).unwrap();
    assert!(storage.has_object(&id).unwrap());
    assert_eq!(
        query_one_i64(
            &db,
            "SELECT COUNT(*) FROM namespaces WHERE tenant_id = 'new-tenant'"
        ),
        1,
        "a new tenant still gets its default namespace"
    );
    assert_eq!(
        query_one_i64(
            &db,
            "SELECT COUNT(*) FROM information_schema.columns
             WHERE table_name = 'sessions' AND column_name = 'scope_namespace'"
        ),
        0,
        "a newer database must not be migrated"
    );
    assert_eq!(
        query_one_i64(
            &db,
            "SELECT (version * 100 + compatible_from)::BIGINT FROM asg_schema
             WHERE component = 'postgres'"
        ),
        9905,
        "an older build lowers neither number"
    );

    drop(storage);
    admin(&url, &format!("DROP DATABASE {name} WITH (FORCE)"));
}

#[test]
fn a_database_needing_a_newer_build_is_refused_and_left_alone() {
    let Some(url) = url() else { return };
    let (db, name) = fresh_database(&url);
    admin(
        &db,
        "UPDATE asg_schema SET version = 99, compatible_from = 98 WHERE component = 'postgres';",
    );

    let err = PostgresStorage::connect_tenant_blocking(&db, "new-tenant", 2)
        .err()
        .expect("refused");
    match &err {
        StorageError::SchemaTooNew {
            backend,
            found,
            compatible_from,
            supported,
        } => {
            assert_eq!(*backend, "postgres");
            assert_eq!((*found, *compatible_from), (99, 98));
            assert!(*supported < 98);
        }
        other => panic!("expected SchemaTooNew, got {other:?}"),
    }
    assert_eq!(err.code(), "schema_too_new");
    assert_eq!(
        query_one_i64(
            &db,
            "SELECT COUNT(*) FROM namespaces WHERE tenant_id = 'new-tenant'"
        ),
        0,
        "a refused connect writes nothing"
    );

    // The async connect refuses too.
    let rt = tokio::runtime::Runtime::new().unwrap();
    let err = rt
        .block_on(PostgresStorage::connect_tenant(&db, "new-tenant"))
        .err()
        .expect("refused");
    assert_eq!(err.code(), "schema_too_new");

    admin(&url, &format!("DROP DATABASE {name} WITH (FORCE)"));
}
