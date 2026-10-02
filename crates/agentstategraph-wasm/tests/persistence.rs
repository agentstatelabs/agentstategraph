//! IndexedDB persistence round trip, as the documented JS bridge drives it:
//! drain after writes, load into a fresh instance on startup. Native — the
//! bridge is plain Rust on both sides of `JsValue`.

#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeMap;

use agentstategraph_wasm::WasmAgentStateGraph;
use serde_json::{json, Value};

/// What IndexedDB holds: one object store per kind, keyed by id or ref key.
#[derive(Default)]
struct Persisted {
    objects: BTreeMap<String, String>,
    commits: BTreeMap<String, String>,
    refs: BTreeMap<String, String>,
}

impl Persisted {
    /// `flushToIndexedDB` from the WASM guide, plus ref deletions.
    fn flush(&mut self, sg: &WasmAgentStateGraph) {
        for (k, v) in pairs(&sg.drain_pending_objects()) {
            self.objects.insert(k, v);
        }
        for (k, v) in pairs(&sg.drain_pending_commits()) {
            self.commits.insert(k, v);
        }
        for (k, v) in pairs(&sg.drain_pending_refs()) {
            self.refs.insert(k, v);
        }
        let deleted: Vec<String> = serde_json::from_str(&sg.drain_deleted_refs()).unwrap();
        for k in deleted {
            self.refs.remove(&k);
        }
    }

    /// Startup: a fresh instance, loaded from what was persisted.
    fn open(&self) -> WasmAgentStateGraph {
        let sg = WasmAgentStateGraph::new(Some("test".into())).unwrap();
        let dump = |m: &BTreeMap<String, String>| {
            serde_json::to_string(&m.iter().collect::<Vec<_>>()).unwrap()
        };
        sg.load_objects(&dump(&self.objects)).unwrap();
        sg.load_commits(&dump(&self.commits)).unwrap();
        sg.load_refs(&dump(&self.refs)).unwrap();
        sg
    }
}

fn pairs(json: &str) -> Vec<(String, String)> {
    serde_json::from_str(json).unwrap()
}

fn set(sg: &WasmAgentStateGraph, path: &str, value: Value, namespace: Option<&str>) {
    sg.set(
        path,
        &value.to_string(),
        "Checkpoint",
        "write",
        None,
        None,
        None,
        namespace.map(str::to_string),
    )
    .unwrap();
}

fn get(sg: &WasmAgentStateGraph, path: &str, namespace: Option<&str>) -> Value {
    serde_json::from_str(&sg.get(path, None, namespace.map(str::to_string)).unwrap()).unwrap()
}

/// The binding drove one storage instance and drained another, so nothing a
/// page wrote survived a reload.
#[test]
fn writes_survive_a_reload() {
    let mut db = Persisted::default();
    let sg = WasmAgentStateGraph::new(Some("test".into())).unwrap();
    db.flush(&sg);
    set(&sg, "/app/count", json!(1), None);
    db.flush(&sg);

    let reopened = db.open();
    assert_eq!(get(&reopened, "/app/count", None), json!(1));
}

/// A fresh instance queues an empty `main` from `init()` before the page
/// loads. Flushing it after the load replaced the persisted `main`.
#[test]
fn loading_supersedes_the_empty_main_queued_by_init() {
    let mut db = Persisted::default();
    let sg = WasmAgentStateGraph::new(Some("test".into())).unwrap();
    set(&sg, "/kept", json!("yes"), None);
    db.flush(&sg);

    let reopened = db.open();
    db.flush(&reopened); // the first save after startup
    let again = db.open();
    assert_eq!(get(&again, "/kept", None), json!("yes"));
}

/// Values written through a speculation keep their JSON shape and exact
/// integers.
#[test]
fn speculation_values_keep_their_shape() {
    let sg = WasmAgentStateGraph::new(Some("test".into())).unwrap();
    let handle = sg.speculate(None, None, None).unwrap();
    let doc = json!({"ids": [u64::MAX], "nested": {"ok": true}});
    sg.spec_set(handle, "/doc", &doc.to_string()).unwrap();
    sg.commit_speculation(handle, "Checkpoint", "commit", None, None)
        .unwrap();
    assert_eq!(get(&sg, "/doc", None), doc);
}
