//! `create_ref` and `cas_delete_ref` must decide atomically: exactly one of
//! several concurrent creators wins, and a ref that moved is not deleted.

use std::sync::{Arc, Barrier};

use agentstategraph_core::{Namespace, ObjectId};
use agentstategraph_storage::{MemoryStorage, RefStore, SqliteStorage};

fn exactly_one_creator<S: RefStore + 'static>(store: Arc<S>) {
    const RACERS: usize = 8;
    let ns = Namespace::default_ns();
    for round in 0..50 {
        let name = format!("r{round}");
        let barrier = Arc::new(Barrier::new(RACERS));
        let handles: Vec<_> = (0..RACERS)
            .map(|i| {
                let (store, barrier, ns, name) = (
                    Arc::clone(&store),
                    Arc::clone(&barrier),
                    ns.clone(),
                    name.clone(),
                );
                std::thread::spawn(move || {
                    barrier.wait();
                    let target = ObjectId::hash(format!("{i}").as_bytes());
                    store
                        .create_ref(&ns, &name, target)
                        .unwrap()
                        .then_some(target)
                })
            })
            .collect();
        let won: Vec<ObjectId> = handles
            .into_iter()
            .filter_map(|h| h.join().unwrap())
            .collect();
        assert_eq!(won.len(), 1, "round {round}: {} creators won", won.len());
        assert_eq!(store.get_ref(&ns, &name).unwrap(), Some(won[0]));
    }
}

fn delete_only_what_was_seen<S: RefStore>(store: &S) {
    let ns = Namespace::default_ns();
    let (seen, moved) = (ObjectId::hash(b"seen"), ObjectId::hash(b"moved"));
    store.set_ref(&ns, "b", seen).unwrap();
    store.set_ref(&ns, "b", moved).unwrap();
    assert!(!store.cas_delete_ref(&ns, "b", seen).unwrap());
    assert_eq!(store.get_ref(&ns, "b").unwrap(), Some(moved));
    assert!(store.cas_delete_ref(&ns, "b", moved).unwrap());
    assert_eq!(store.get_ref(&ns, "b").unwrap(), None);
    assert!(!store.cas_delete_ref(&ns, "b", moved).unwrap());
    // An existing ref is never replaced by create_ref.
    store.set_ref(&ns, "c", seen).unwrap();
    assert!(!store.create_ref(&ns, "c", moved).unwrap());
    assert_eq!(store.get_ref(&ns, "c").unwrap(), Some(seen));
}

#[test]
fn memory() {
    exactly_one_creator(Arc::new(MemoryStorage::new()));
    delete_only_what_was_seen(&MemoryStorage::new());
}

#[test]
fn sqlite() {
    exactly_one_creator(Arc::new(SqliteStorage::in_memory().unwrap()));
    delete_only_what_was_seen(&SqliteStorage::in_memory().unwrap());
}
