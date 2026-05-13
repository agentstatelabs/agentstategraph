//! Cross-tenant regression test for `PostgresStorage`.
//!
//! Two `connect_tenant` instances share a `DATABASE_URL` but use
//! different tenant ids. Each writes a distinct value at a
//! distinct ref. Neither instance must be able to observe the
//! other's ref or its target object.
//!
//! Skipped unless `TEST_DATABASE_URL` is set in the environment —
//! we must not fail CI in environments without a Postgres.

#![cfg(feature = "postgres")]

use agentstategraph_core::{Atom, Namespace, Object};
use agentstategraph_storage::{ObjectStore, PostgresStorage, RefStore};

#[test]
fn postgres_tenants_cannot_see_each_others_refs() {
    let url = match std::env::var("TEST_DATABASE_URL") {
        Ok(u) if !u.is_empty() => u,
        _ => {
            eprintln!(
                "skip: postgres_tenants_cannot_see_each_others_refs — \
                 TEST_DATABASE_URL not set"
            );
            return;
        }
    };

    // The PostgresStorage sync wrappers rely on `tokio::task::block_in_place`,
    // which needs a multi-thread runtime.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");

    runtime.block_on(async move {
        let tenant_a = PostgresStorage::connect_tenant(&url, "tenant-a")
            .await
            .expect("connect tenant-a");
        let tenant_b = PostgresStorage::connect_tenant(&url, "tenant-b")
            .await
            .expect("connect tenant-b");

        // Each tenant writes a distinct object and points "main" at it.
        let obj_a = Object::Atom(Atom::String("value-for-tenant-a".into()));
        let obj_b = Object::Atom(Atom::String("value-for-tenant-b".into()));

        let id_a =
            <PostgresStorage as ObjectStore>::put_object(&tenant_a, &obj_a).expect("put object A");
        let id_b =
            <PostgresStorage as ObjectStore>::put_object(&tenant_b, &obj_b).expect("put object B");

        let ns = Namespace::default_ns();
        tenant_a.set_ref(&ns, "main", id_a).expect("set ref A");
        tenant_b.set_ref(&ns, "main", id_b).expect("set ref B");

        // Each tenant sees its own target.
        let a_sees = tenant_a
            .get_ref(&ns, "main")
            .expect("get A")
            .expect("A has main");
        let b_sees = tenant_b
            .get_ref(&ns, "main")
            .expect("get B")
            .expect("B has main");
        assert_eq!(a_sees, id_a, "tenant-a must see its own ref target");
        assert_eq!(b_sees, id_b, "tenant-b must see its own ref target");
        assert_ne!(a_sees, b_sees, "the two tenants must have distinct targets");

        // Neither tenant can read the other's object by id.
        let cross_a = <PostgresStorage as ObjectStore>::get_object(&tenant_a, &id_b)
            .expect("get-object A<-B");
        assert!(cross_a.is_none(), "tenant-a must NOT see tenant-b's object");
        let cross_b = <PostgresStorage as ObjectStore>::get_object(&tenant_b, &id_a)
            .expect("get-object B<-A");
        assert!(cross_b.is_none(), "tenant-b must NOT see tenant-a's object");

        // list_refs must not leak the other tenant's refs.
        let a_refs = tenant_a.list_refs(&ns, "").expect("list A");
        assert!(
            a_refs.iter().all(|(_, tgt)| *tgt != id_b),
            "tenant-a list_refs leaked tenant-b's target: {:?}",
            a_refs
        );
        let b_refs = tenant_b.list_refs(&ns, "").expect("list B");
        assert!(
            b_refs.iter().all(|(_, tgt)| *tgt != id_a),
            "tenant-b list_refs leaked tenant-a's target: {:?}",
            b_refs
        );

        // Cleanup — best effort, don't fail the test on teardown noise.
        let _ = tenant_a.delete_ref(&ns, "main");
        let _ = tenant_b.delete_ref(&ns, "main");
    });
}
