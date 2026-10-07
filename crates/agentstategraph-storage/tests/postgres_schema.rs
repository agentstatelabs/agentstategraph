//! Schema setup on Postgres (DESIGN.md Plan D t-004/t-006): the DDL runs once
//! per schema version, not on every connect, and an older database is
//! brought up to date. Uses its own throwaway database, so rewinding the
//! schema can't disturb other tests.
//!
//! Skipped unless `TEST_DATABASE_URL` is set (its user needs CREATEDB);
//! `ASG_REQUIRE_POSTGRES` turns the skip into a failure.

#![cfg(feature = "postgres")]

use agentstategraph_core::{Epoch, ObjectId};
use agentstategraph_storage::{EpochStore, PostgresStorage};
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

    // A current database, rewound to look like one from before the version
    // stamp and before epochs had a seal digest.
    drop(PostgresStorage::connect_tenant_blocking(&db, "t", 2).unwrap());
    admin(
        &db,
        "DROP TABLE asg_schema; ALTER TABLE epochs DROP COLUMN seal_hash;",
    );

    let storage = PostgresStorage::connect_tenant_blocking(&db, "t", 2).unwrap();
    assert_eq!(
        query_one_i64(
            &db,
            "SELECT version::BIGINT FROM asg_schema WHERE component = 'postgres'"
        ),
        1,
        "the upgrade stamps the schema version"
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
