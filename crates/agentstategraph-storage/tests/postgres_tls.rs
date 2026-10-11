//! TLS to Postgres (DESIGN.md Plan D t-007), against a server that accepts
//! TLS connections only, with a certificate from a private CA that is valid
//! for "localhost" alone. `scripts/test-postgres.sh --tls` starts one and sets
//! `TEST_DATABASE_TLS_URL` (host `localhost`) and `TEST_DATABASE_TLS_ROOTCERT`
//! (that CA). Skipped when they're unset; `ASG_REQUIRE_POSTGRES_TLS` turns the
//! skip into a failure.

#![cfg(feature = "postgres-tls")]

use agentstategraph_core::{Namespace, Object};
use agentstategraph_storage::{ObjectStore, PostgresStorage, RefStore, StorageError};

struct TlsServer {
    url: String,
    root_cert: String,
}

fn server() -> Option<TlsServer> {
    match (
        std::env::var("TEST_DATABASE_TLS_URL"),
        std::env::var("TEST_DATABASE_TLS_ROOTCERT"),
    ) {
        (Ok(url), Ok(root_cert)) if !url.is_empty() && !root_cert.is_empty() => {
            Some(TlsServer { url, root_cert })
        }
        _ => {
            let required =
                std::env::var("ASG_REQUIRE_POSTGRES_TLS").is_ok_and(|v| !v.is_empty() && v != "0");
            assert!(
                !required,
                "ASG_REQUIRE_POSTGRES_TLS is set, but TEST_DATABASE_TLS_URL / _ROOTCERT aren't"
            );
            eprintln!("skip: TEST_DATABASE_TLS_URL / TEST_DATABASE_TLS_ROOTCERT not set");
            None
        }
    }
}

/// `url` with extra query options appended.
fn with_options(url: &str, options: &str) -> String {
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}{options}")
}

fn connect(url: &str) -> Result<PostgresStorage, StorageError> {
    let tenant = format!("tls-{}", uuid::Uuid::new_v4().simple());
    PostgresStorage::connect_tenant_blocking(url, &tenant, 2)
}

fn message(result: Result<PostgresStorage, StorageError>) -> String {
    match result {
        Ok(_) => panic!("connected, but this connection should have been refused"),
        Err(e) => format!("{e:?}"),
    }
}

#[test]
fn verified_tls_reads_and_writes() {
    let Some(server) = server() else { return };
    for mode in ["require", "verify-full", "verify-ca"] {
        let url = with_options(
            &server.url,
            &format!("sslmode={mode}&sslrootcert={}", server.root_cert),
        );
        // The server takes TLS connections only, so getting here means TLS.
        let storage = connect(&url).unwrap_or_else(|e| panic!("sslmode={mode}: {e:?}"));
        let obj = Object::string(mode);
        let id = storage.put_object(&obj).unwrap();
        assert_eq!(storage.get_object(&id).unwrap(), Some(obj));
        storage
            .set_ref(&Namespace::default_ns(), "main", id)
            .unwrap();
    }
}

#[test]
fn require_verifies_the_certificate() {
    let Some(server) = server() else { return };
    // Without the private CA, the server's certificate doesn't verify.
    let msg = message(connect(&with_options(&server.url, "sslmode=require")));
    assert!(
        msg.contains("certificate"),
        "expected a certificate error, got {msg}"
    );
}

#[test]
fn the_host_name_is_checked() {
    let Some(server) = server() else { return };
    // The certificate names "localhost" only.
    let by_ip = server.url.replacen("@localhost", "@127.0.0.1", 1);
    assert_ne!(
        by_ip, server.url,
        "TEST_DATABASE_TLS_URL must use host localhost"
    );
    let url = with_options(
        &by_ip,
        &format!("sslmode=verify-full&sslrootcert={}", server.root_cert),
    );
    let msg = message(connect(&url));
    assert!(
        msg.contains("certificate"),
        "expected a certificate error, got {msg}"
    );
}

#[test]
fn plaintext_is_refused_by_this_server() {
    let Some(server) = server() else { return };
    // What makes the passing cases proof of TLS.
    message(connect(&with_options(&server.url, "sslmode=disable")));
}
