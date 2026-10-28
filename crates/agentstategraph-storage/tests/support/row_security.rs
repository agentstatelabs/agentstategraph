//! A Postgres role and schema for tests that run with row-level security on.
//!
//! The role is neither a superuser nor `BYPASSRLS`, so row-level security
//! binds it, and it owns its schema, so the storage it connects creates its
//! tables there as their owner: the policies bind it only because they're
//! forced. The test's own `TEST_DATABASE_URL` role sets it up, so that role
//! needs to be able to create roles (CI's is a superuser).

// Each test binary that includes this uses only some of it.
#![allow(dead_code)]

/// The role the storage connects as. Roles are global to the cluster, so it's
/// shared by every test and created once.
pub const ROLE: &str = "asg_row_security_test";
const PASSWORD: &str = "asg-row-security-test";

/// A connection of the test's own, as the `TEST_DATABASE_URL` role.
pub struct Admin {
    rt: tokio::runtime::Runtime,
    client: tokio_postgres::Client,
}

impl Admin {
    pub fn connect(url: &str) -> Self {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let client = rt.block_on(async {
            let (client, conn) = tokio_postgres::connect(url, tokio_postgres::NoTls)
                .await
                .expect("admin connect");
            tokio::spawn(conn);
            client
        });
        Self { rt, client }
    }

    pub fn execute(&self, sql: &str) {
        self.rt
            .block_on(self.client.batch_execute(sql))
            .unwrap_or_else(|e| panic!("admin sql: {e:?}\n{sql}"));
    }

    pub fn count(&self, sql: &str, params: &[&(dyn tokio_postgres::types::ToSql + Sync)]) -> i64 {
        self.rt
            .block_on(self.client.query_one(sql, params))
            .unwrap_or_else(|e| panic!("admin query: {e:?}\n{sql}"))
            .get(0)
    }

    /// Whether this connection's role skips row-level security.
    pub fn bypasses_row_security(&self) -> bool {
        self.rt
            .block_on(self.client.query_one(
                "SELECT rolsuper OR rolbypassrls FROM pg_roles WHERE rolname = current_user",
                &[],
            ))
            .expect("read the role")
            .get(0)
    }

    /// Create the role unless it exists, and a fresh schema `schema` it owns
    /// (dropping any earlier one of that name).
    pub fn role_and_schema(&self, schema: &str) {
        // Two first runs at once can collide on the catalog's unique index
        // rather than see the role already there.
        self.execute(&format!(
            "DO $$ BEGIN
                 CREATE ROLE {ROLE} LOGIN NOSUPERUSER NOBYPASSRLS PASSWORD '{PASSWORD}';
             EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL;
             END $$;
             DROP SCHEMA IF EXISTS {schema} CASCADE;
             CREATE SCHEMA {schema} AUTHORIZATION {ROLE};"
        ));
    }

    pub fn drop_schema(&self, schema: &str) {
        self.execute(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"));
    }
}

/// `url` with a connection option added: `url_value` (URL-encoded) for the URL
/// form, `kv_value` for the key/value form.
fn with_option(url: &str, key: &str, url_value: &str, kv_value: &str) -> String {
    if url.starts_with("postgres://") || url.starts_with("postgresql://") {
        let sep = if url.contains('?') { '&' } else { '?' };
        format!("{url}{sep}{key}={url_value}")
    } else {
        format!("{url} {key}={kv_value}")
    }
}

/// `admin_url`, logging in as [`ROLE`] instead, with `schema` alone on its
/// search path.
pub fn role_url(admin_url: &str, schema: &str) -> String {
    let url = if let Some((scheme, rest)) = admin_url.split_once("://") {
        let host = match rest.find('/') {
            Some(slash) => (&rest[..slash], &rest[slash..]),
            None => (rest, ""),
        };
        let authority = host.0.rsplit_once('@').map_or(host.0, |(_, h)| h);
        format!("{scheme}://{ROLE}:{PASSWORD}@{authority}{}", host.1)
    } else {
        // Later keys win in the key/value form.
        format!("{admin_url} user={ROLE} password={PASSWORD}")
    };
    with_option(
        &url,
        "options",
        &format!("-c%20search_path%3D{schema}"),
        &format!("'-c search_path={schema}'"),
    )
}

/// `admin_url` with `schema` alone on its search path, still as the admin.
pub fn admin_url_in(admin_url: &str, schema: &str) -> String {
    with_option(
        admin_url,
        "options",
        &format!("-c%20search_path%3D{schema}"),
        &format!("'-c search_path={schema}'"),
    )
}
