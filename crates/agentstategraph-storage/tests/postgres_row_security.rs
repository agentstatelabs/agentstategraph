//! Postgres row-level security as a backstop behind every query's tenant
//! filter (DESIGN.md Plan F t-003). Every tenant table has the tenant policy,
//! which does nothing until [`PostgresStorage::set_row_security`] switches it
//! on. Then a statement sees and writes only its connection's tenant's rows,
//! the tables' owner included. The conformance suite runs every storage
//! operation with it on; these tests check the switch and the backstop.
//!
//! Each test gets a schema of its own, owned by a role that row-level security
//! binds (`tests/support/row_security.rs`). Skipped unless `TEST_DATABASE_URL`
//! is set; `ASG_REQUIRE_POSTGRES` turns the skip into a failure.

#![cfg(feature = "postgres")]

#[path = "support/row_security.rs"]
mod row_security;

use agentstategraph_core::{
    Authority, Commit, CommitBuilder, Intent, IntentCategory, Namespace, Object, ObjectId,
};
use agentstategraph_storage::postgres::{TENANT_SETTING, TENANT_TABLES, add_tenant_policy};
use agentstategraph_storage::{
    CommitStore, ObjectStore, PostgresStorage, RefStore, RowSecurity, TenantPool,
};
use row_security::Admin;
use tokio_postgres::error::SqlState;

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

fn tenant(tag: &str) -> String {
    format!("rls-{tag}-{}", uuid::Uuid::new_v4().simple())
}

/// A fresh schema owned by the test role, dropped with this.
struct Schema {
    admin: Admin,
    name: String,
    /// Logs in as the test role, with this schema on its search path.
    url: String,
}

impl Schema {
    fn new(admin_url: &str, tag: &str) -> Self {
        let name = format!("asg_rls_{tag}_{}", uuid::Uuid::new_v4().simple());
        let admin = Admin::connect(admin_url);
        admin.role_and_schema(&name);
        Self {
            url: row_security::role_url(admin_url, &name),
            admin,
            name,
        }
    }

    /// `tenant`'s storage, as the test role, which owns the tables it creates.
    fn connect(&self, tenant: &str, pool_size: usize) -> PostgresStorage {
        PostgresStorage::connect_tenant_blocking(&self.url, tenant, pool_size)
            .expect("connect as the test role")
    }

    /// Rows in `table`, every tenant's, as the test's own role sees them.
    fn rows(&self, table: &str) -> i64 {
        self.admin
            .count(&format!("SELECT count(*) FROM {}.{table}", self.name), &[])
    }

    /// `tenant`'s rows in `table`.
    fn rows_of(&self, table: &str, tenant: &str) -> i64 {
        self.admin.count(
            &format!(
                "SELECT count(*) FROM {}.{table} WHERE tenant_id = $1",
                self.name
            ),
            &[&tenant],
        )
    }
}

impl Drop for Schema {
    fn drop(&mut self) {
        self.admin.drop_schema(&self.name);
    }
}

fn sorted(tables: &[&str]) -> Vec<String> {
    let mut tables: Vec<String> = tables.iter().map(|t| t.to_string()).collect();
    tables.sort();
    tables
}

/// Count the rows of `table` on a connection from `pool`, with no tenant
/// filter of the query's own.
fn unfiltered(pool: &TenantPool, table: &str) -> i64 {
    PostgresStorage::run(async {
        pool.get()
            .await
            .expect("a connection")
            .query_one(&format!("SELECT count(*) FROM {table}"), &[])
            .await
            .unwrap_or_else(|e| panic!("count {table}: {e:?}"))
            .get(0)
    })
}

/// Run `sql` on a connection from `pool`, and return its error, if any.
fn try_sql(
    pool: &TenantPool,
    sql: &str,
    params: &[&(dyn tokio_postgres::types::ToSql + Sync)],
) -> Result<u64, tokio_postgres::Error> {
    PostgresStorage::run(async {
        pool.get()
            .await
            .expect("a connection")
            .execute(sql, params)
            .await
    })
}

/// What a tenant wrote, to read back.
struct Wrote {
    commit: ObjectId,
}

/// Write a little of everything a host writes most: objects, a commit and the
/// ref to it, a namespace and search rows, under the same names in every
/// tenant.
fn write(storage: &PostgresStorage, tag: &str) -> Wrote {
    let ns = Namespace::default_ns();
    let root = storage.put_object(&Object::string(tag)).unwrap();
    let commit: Commit = CommitBuilder::new(
        root,
        "agent/test",
        Authority::simple("test"),
        Intent::new(IntentCategory::Refine, tag),
    )
    .build();
    storage.put_commit(&commit).unwrap();
    storage.set_ref(&ns, "main", commit.id).unwrap();
    storage
        .create_namespace(&Namespace::new("rls-extra").unwrap())
        .unwrap();
    storage
        .leaf_index_build(
            "default",
            "main",
            &[("/k".to_string(), format!("value of {tag}"))],
        )
        .unwrap();
    Wrote { commit: commit.id }
}

fn read_back(storage: &PostgresStorage, tag: &str, wrote: &Wrote) {
    let ns = Namespace::default_ns();
    assert_eq!(storage.get_ref(&ns, "main").unwrap(), Some(wrote.commit));
    assert!(storage.get_commit(&wrote.commit).unwrap().is_some());
    assert_eq!(
        storage
            .leaf_index_search("default", "main", "value", 10)
            .unwrap(),
        Some(vec![("/k".to_string(), format!("value of {tag}"))])
    );
}

#[test]
fn it_is_off_until_switched_on_and_a_superuser_skips_it() {
    let Some(url) = url() else { return };
    let schema = Schema::new(&url, "switch");
    let storage = schema.connect(&tenant("a"), 2);

    let off = storage.row_security().unwrap();
    assert_eq!(
        off,
        RowSecurity {
            on: Vec::new(),
            off: sorted(TENANT_TABLES),
            unguarded: Vec::new(),
            role_bypasses: false,
        }
    );
    assert!(!off.enforced());

    let on = storage.set_row_security(true).unwrap();
    assert_eq!(on.on, sorted(TENANT_TABLES));
    assert!(on.off.is_empty() && on.unguarded.is_empty());
    assert!(on.enforced());
    assert_eq!(storage.row_security().unwrap(), on);
    assert_eq!(storage.set_row_security(true).unwrap(), on, "on again");

    // The test's own role, as CI and scripts/test-postgres.sh run it, is a
    // superuser: the tables say on, and it skips it anyway.
    let admin = PostgresStorage::connect_tenant_blocking(
        &row_security::admin_url_in(&url, &schema.name),
        &tenant("admin"),
        1,
    )
    .unwrap();
    let seen = admin.row_security().unwrap();
    assert_eq!(seen.on, on.on);
    assert_eq!(seen.role_bypasses, schema.admin.bypasses_row_security());
    assert_eq!(seen.enforced(), !seen.role_bypasses);
    drop(admin);

    let off = storage.set_row_security(false).unwrap();
    assert_eq!(off.off, sorted(TENANT_TABLES));
    assert!(off.on.is_empty());
}

#[test]
fn with_it_on_a_statement_sees_and_writes_only_its_tenants_rows() {
    let Some(url) = url() else { return };
    let schema = Schema::new(&url, "backstop");
    let a = schema.connect(&tenant("a"), 2);
    let b = a.for_tenant(&tenant("b")).unwrap();
    let wrote_a = write(&a, "a");
    let wrote_b = write(&b, "b");
    a.set_row_security(true).unwrap();

    // The storage works as before, for every tenant, including one opened now.
    read_back(&a, "a", &wrote_a);
    read_back(&b, "b", &wrote_b);
    let c = a.for_tenant(&tenant("c")).unwrap();
    assert_eq!(c.list_namespaces().unwrap(), vec![Namespace::default_ns()]);

    // A query with no tenant filter of its own, on A's connection, counts
    // only A's rows, in every table.
    let pool = a.tenant_pool();
    for table in TENANT_TABLES {
        assert_eq!(
            unfiltered(&pool, table),
            schema.rows_of(table, a.tenant_id()),
            "{table}"
        );
    }
    for table in ["objects", "commits", "refs", "namespaces", "leaf_rows"] {
        assert!(
            schema.rows_of(table, a.tenant_id()) < schema.rows(table),
            "both tenants have rows in {table}"
        );
    }

    // A connection with no tenant set sees no rows at all.
    let bare = Admin::connect(&schema.url);
    assert_eq!(bare.count("SELECT count(*) FROM objects", &[]), 0);

    // A's connection can't write a row of B's, or touch B's rows.
    let refused = try_sql(
        &pool,
        "INSERT INTO namespaces (tenant_id, name, created_at) VALUES ($1, 'sneaky', now()::text)",
        &[&b.tenant_id()],
    )
    .expect_err("A wrote a row of B's");
    assert_eq!(refused.code(), Some(&SqlState::INSUFFICIENT_PRIVILEGE));
    assert_eq!(
        try_sql(
            &pool,
            "UPDATE namespaces SET created_at = created_at WHERE tenant_id = $1",
            &[&b.tenant_id()],
        )
        .unwrap(),
        0
    );
    assert_eq!(
        try_sql(
            &pool,
            "DELETE FROM refs WHERE tenant_id = $1",
            &[&b.tenant_id()]
        )
        .unwrap(),
        0
    );
    read_back(&b, "b", &wrote_b);

    // Nor can the tables' owner turn it off for a query: Postgres refuses
    // the query instead, which is what pg_dump run as the owner meets.
    let refused = PostgresStorage::run(async {
        let client = pool.get().await.unwrap();
        client
            .batch_execute("SET row_security = off")
            .await
            .unwrap();
        client
            .query_one("SELECT count(*) FROM objects", &[])
            .await
            .expect_err("the owner read with row security off")
    });
    assert_eq!(refused.code(), Some(&SqlState::INSUFFICIENT_PRIVILEGE));

    // Off again, the same query counts every tenant's rows.
    a.set_row_security(false).unwrap();
    assert_eq!(
        unfiltered(&a.tenant_pool(), "objects"),
        schema.rows("objects")
    );
}

#[test]
fn a_hosts_table_with_the_policy_is_switched_with_the_rest() {
    let Some(url) = url() else { return };
    let schema = Schema::new(&url, "host");
    let a = schema.connect(&tenant("a"), 2);
    let b = a.for_tenant(&tenant("b")).unwrap();
    let (pool_a, pool_b) = (a.tenant_pool(), b.tenant_pool());
    PostgresStorage::run(async {
        let client = pool_a.get().await.unwrap();
        client
            .batch_execute(
                "CREATE TABLE host_guarded (tenant_id TEXT NOT NULL, v TEXT);
                 CREATE TABLE host_unguarded (tenant_id TEXT NOT NULL, v TEXT);",
            )
            .await
            .unwrap();
        add_tenant_policy(&client, "host_guarded").await.unwrap();
        add_tenant_policy(&client, "host_guarded")
            .await
            .expect("adding it again does nothing");
    });
    for pool in [&pool_a, &pool_b] {
        for table in ["host_guarded", "host_unguarded"] {
            try_sql(
                pool,
                &format!("INSERT INTO {table} VALUES ($1, 'x')"),
                &[&pool.tenant_id()],
            )
            .unwrap();
        }
    }

    let off = a.row_security().unwrap();
    assert!(off.off.contains(&"host_guarded".to_string()));
    assert_eq!(off.unguarded, ["host_unguarded"]);

    let on = a.set_row_security(true).unwrap();
    assert!(on.on.contains(&"host_guarded".to_string()));
    assert!(on.off.is_empty());
    assert_eq!(
        on.unguarded,
        ["host_unguarded"],
        "a table without the policy"
    );
    assert_eq!(unfiltered(&pool_a, "host_guarded"), 1);
    assert_eq!(
        unfiltered(&pool_a, "host_unguarded"),
        2,
        "nothing guards it"
    );
}

#[test]
fn every_connection_from_a_tenant_pool_has_its_tenant() {
    let Some(url) = url() else { return };
    let schema = Schema::new(&url, "pool");
    // One connection, so each checkout reuses the last one's.
    let storage = schema.connect(&tenant("a"), 1);
    let setting = |pool: &TenantPool| -> String {
        PostgresStorage::run(async {
            pool.get()
                .await
                .unwrap()
                .query_one("SELECT current_setting($1)", &[&TENANT_SETTING])
                .await
                .unwrap()
                .get(0)
        })
    };
    let own = storage.tenant_pool();
    let odd = own.for_tenant(r"o'brien\x");
    assert_eq!(setting(&own), storage.tenant_id());
    assert_eq!(setting(&odd), r"o'brien\x");
    assert_eq!(setting(&own), storage.tenant_id());
}
