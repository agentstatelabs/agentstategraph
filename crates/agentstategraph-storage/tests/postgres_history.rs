//! History extraction on Postgres with several writers (DESIGN.md Plan D
//! t-008). A commit can become visible after a later one, when its
//! transaction started first and committed last. The extractor must fold it
//! anyway rather than step past it.
//!
//! Skipped unless `TEST_DATABASE_URL` is set; `ASG_REQUIRE_POSTGRES` turns the
//! skip into a failure.

#![cfg(feature = "postgres")]

use std::time::{Duration, Instant};

use agentstategraph_core::{Authority, CommitBuilder, Intent, IntentCategory, Object};
use agentstategraph_storage::{CommitStore, ObjectStore, PostgresStorage};

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

#[test]
fn a_commit_that_becomes_visible_late_is_still_folded() {
    let Some(url) = url() else { return };
    let tenant = format!("late-commit-{}", uuid::Uuid::new_v4().simple());
    let storage = PostgresStorage::connect_tenant_blocking(&url, &tenant, 2).unwrap();
    let make = |tag: &str| {
        let root = storage.put_object(&Object::string(tag)).unwrap();
        CommitBuilder::new(
            root,
            "agent/late",
            Authority::simple("test"),
            Intent::new(IntentCategory::Refine, tag),
        )
        .build()
    };
    let slow = make("slow");
    let fast = make("fast");

    // The slow writer's transaction gets its id first and commits last.
    let rt = tokio::runtime::Runtime::new().unwrap();
    let client = rt.block_on(async {
        let (client, conn) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
            .await
            .expect("connect");
        tokio::spawn(conn);
        client.batch_execute("BEGIN").await.unwrap();
        client
            .execute(
                "INSERT INTO commits (tenant_id, id, data, timestamp) VALUES ($1, $2, $3, $4)",
                &[
                    &tenant,
                    &slow.id.as_bytes().as_slice(),
                    &serde_json::to_value(&slow).unwrap(),
                    &slow.timestamp,
                ],
            )
            .await
            .unwrap();
        client
    });
    storage.put_commit(&fast).unwrap();

    // The slow one commits while the extractor is waiting on it.
    let committer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        rt.block_on(client.batch_execute("COMMIT")).unwrap();
    });
    let started = Instant::now();
    let mut folded = 0;
    loop {
        match storage.history_extract_batch(100).unwrap() {
            0 => break,
            n => folded += n,
        }
    }
    committer.join().unwrap();

    assert_eq!(folded, 2, "both commits are folded, the late one included");
    assert!(
        started.elapsed() >= Duration::from_millis(250),
        "the extractor waited for the older transaction"
    );
    assert_eq!(storage.history_undistilled_commit_count().unwrap(), 0);
    let total: i64 = storage
        .history_rollup()
        .unwrap()
        .iter()
        .map(|b| b.commit_count)
        .sum();
    assert_eq!(total, 2);
}
