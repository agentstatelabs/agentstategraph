//! Structured diff engine — schema-aware, typed diffs between state trees.
//!
//! Unlike git's line-based text diffs, StateGraph diffs operate on typed
//! structured data. Each DiffOp describes a specific change (SetValue,
//! AddKey, RemoveKey, etc.) at an exact path in the state tree.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use crate::object::{Atom, Node, Object, ObjectId};
use crate::path::StatePath;

/// A single structured change operation within a diff.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum DiffOp {
    /// A value was changed at this path.
    SetValue {
        path: String,
        old: DiffValue,
        new: DiffValue,
    },
    /// A key was added to a map.
    AddKey {
        path: String,
        key: String,
        value: DiffValue,
    },
    /// A key was removed from a map.
    RemoveKey {
        path: String,
        key: String,
        old_value: DiffValue,
    },
    /// An element was added to a list.
    AddElement {
        path: String,
        index: usize,
        value: DiffValue,
    },
    /// An element was removed from a list.
    RemoveElement {
        path: String,
        index: usize,
        old_value: DiffValue,
    },
    /// An element was added to a set.
    AddToSet {
        path: String,
        value: DiffValue,
    },
    /// An element was removed from a set.
    RemoveFromSet {
        path: String,
        old_value: DiffValue,
    },
    /// The type of a node changed (e.g., map became a list).
    ChangeType {
        path: String,
        old_type: String,
        new_type: String,
    },
}

/// A JSON-serializable representation of a value in a diff.
/// We can't use Object directly because it contains ObjectIds
/// which aren't meaningful to external consumers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DiffValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Bytes(Vec<u8>),
    /// For complex values (maps, lists), we store a summary
    /// rather than the full deep value.
    Complex(String),
}

impl DiffValue {
    /// Create a DiffValue from an Atom.
    pub fn from_atom(atom: &Atom) -> Self {
        match atom {
            Atom::Null => DiffValue::Null,
            Atom::Bool(b) => DiffValue::Bool(*b),
            Atom::Int(i) => DiffValue::Int(*i),
            Atom::Float(f) => DiffValue::Float(*f),
            Atom::String(s) => DiffValue::String(s.clone()),
            Atom::Bytes(b) => DiffValue::Bytes(b.clone()),
        }
    }

    /// Create a summary DiffValue for a Node.
    pub fn from_node(node: &Node) -> Self {
        match node {
            Node::Map(entries) => {
                DiffValue::Complex(format!("{{map: {} keys}}", entries.len()))
            }
            Node::List(items) => {
                DiffValue::Complex(format!("[list: {} items]", items.len()))
            }
            Node::Set(items) => {
                DiffValue::Complex(format!("{{set: {} items}}", items.len()))
            }
        }
    }

    /// Create a DiffValue from an Object.
    pub fn from_object(obj: &Object) -> Self {
        match obj {
            Object::Atom(atom) => Self::from_atom(atom),
            Object::Node(node) => Self::from_node(node),
        }
    }
}

/// Trait for resolving ObjectIds to Objects during diff computation.
/// This decouples the diff algorithm from the storage backend.
pub trait ObjectResolver {
    fn resolve(&self, id: &ObjectId) -> Option<Object>;
}

/// Compute a structured diff between two state trees.
///
/// Returns a list of DiffOps describing every change needed to transform
/// the state at `old_root` into the state at `new_root`.
pub fn diff(
    resolver: &dyn ObjectResolver,
    old_root: &ObjectId,
    new_root: &ObjectId,
) -> Vec<DiffOp> {
    // If roots are identical, no changes
    if old_root == new_root {
        return Vec::new();
    }

    let old_obj = resolver.resolve(old_root);
    let new_obj = resolver.resolve(new_root);

    match (old_obj, new_obj) {
        (Some(old), Some(new)) => {
            diff_objects(resolver, &StatePath::root(), &old, &new)
        }
        (None, Some(_)) => {
            // Old didn't exist — entire new tree is an addition
            // This shouldn't normally happen at root level
            Vec::new()
        }
        (Some(_), None) => {
            // New doesn't exist — entire old tree was deleted
            Vec::new()
        }
        (None, None) => Vec::new(),
    }
}

/// Recursively diff two objects at the given path.
fn diff_objects(
    resolver: &dyn ObjectResolver,
    path: &StatePath,
    old: &Object,
    new: &Object,
) -> Vec<DiffOp> {
    if old == new {
        return Vec::new();
    }

    match (old, new) {
        // Both atoms — value changed
        (Object::Atom(old_atom), Object::Atom(new_atom)) => {
            vec![DiffOp::SetValue {
                path: path.to_string(),
                old: DiffValue::from_atom(old_atom),
                new: DiffValue::from_atom(new_atom),
            }]
        }

        // Both maps — compare keys
        (Object::Node(Node::Map(old_entries)), Object::Node(Node::Map(new_entries))) => {
            diff_maps(resolver, path, old_entries, new_entries)
        }

        // Both lists — compare elements
        (Object::Node(Node::List(old_items)), Object::Node(Node::List(new_items))) => {
            diff_lists(resolver, path, old_items, new_items)
        }

        // Both sets — compare membership
        (Object::Node(Node::Set(old_items)), Object::Node(Node::Set(new_items))) => {
            diff_sets(resolver, path, old_items, new_items)
        }

        // Type changed
        _ => {
            vec![DiffOp::ChangeType {
                path: path.to_string(),
                old_type: type_name(old),
                new_type: type_name(new),
            }]
        }
    }
}

fn diff_maps(
    resolver: &dyn ObjectResolver,
    path: &StatePath,
    old_entries: &std::collections::BTreeMap<String, ObjectId>,
    new_entries: &std::collections::BTreeMap<String, ObjectId>,
) -> Vec<DiffOp> {
    let mut ops = Vec::new();

    let old_keys: BTreeSet<&String> = old_entries.keys().collect();
    let new_keys: BTreeSet<&String> = new_entries.keys().collect();

    // Removed keys
    for key in old_keys.difference(&new_keys) {
        let old_id = &old_entries[*key];
        let old_obj = resolver.resolve(old_id);
        ops.push(DiffOp::RemoveKey {
            path: path.to_string(),
            key: (*key).clone(),
            old_value: old_obj
                .map(|o| DiffValue::from_object(&o))
                .unwrap_or(DiffValue::Null),
        });
    }

    // Added keys
    for key in new_keys.difference(&old_keys) {
        let new_id = &new_entries[*key];
        let new_obj = resolver.resolve(new_id);
        ops.push(DiffOp::AddKey {
            path: path.to_string(),
            key: (*key).clone(),
            value: new_obj
                .map(|o| DiffValue::from_object(&o))
                .unwrap_or(DiffValue::Null),
        });
    }

    // Modified keys (same key, different ObjectId)
    for key in old_keys.intersection(&new_keys) {
        let old_id = &old_entries[*key];
        let new_id = &new_entries[*key];

        if old_id != new_id {
            let child_path = path.push_key(*key);
            let old_obj = resolver.resolve(old_id);
            let new_obj = resolver.resolve(new_id);

            match (old_obj, new_obj) {
                (Some(old_child), Some(new_child)) => {
                    ops.extend(diff_objects(resolver, &child_path, &old_child, &new_child));
                }
                _ => {
                    // Can't resolve — report as a value change
                    ops.push(DiffOp::SetValue {
                        path: child_path.to_string(),
                        old: DiffValue::Null,
                        new: DiffValue::Null,
                    });
                }
            }
        }
    }

    ops
}

fn diff_lists(
    resolver: &dyn ObjectResolver,
    path: &StatePath,
    old_items: &[ObjectId],
    new_items: &[ObjectId],
) -> Vec<DiffOp> {
    let mut ops = Vec::new();
    let max_len = old_items.len().max(new_items.len());

    for i in 0..max_len {
        match (old_items.get(i), new_items.get(i)) {
            (Some(old_id), Some(new_id)) => {
                if old_id != new_id {
                    let child_path = path.push_index(i);
                    let old_obj = resolver.resolve(old_id);
                    let new_obj = resolver.resolve(new_id);

                    match (old_obj, new_obj) {
                        (Some(old_child), Some(new_child)) => {
                            ops.extend(diff_objects(
                                resolver,
                                &child_path,
                                &old_child,
                                &new_child,
                            ));
                        }
                        _ => {
                            ops.push(DiffOp::SetValue {
                                path: child_path.to_string(),
                                old: DiffValue::Null,
                                new: DiffValue::Null,
                            });
                        }
                    }
                }
            }
            (None, Some(new_id)) => {
                // Element added
                let new_obj = resolver.resolve(new_id);
                ops.push(DiffOp::AddElement {
                    path: path.to_string(),
                    index: i,
                    value: new_obj
                        .map(|o| DiffValue::from_object(&o))
                        .unwrap_or(DiffValue::Null),
                });
            }
            (Some(old_id), None) => {
                // Element removed
                let old_obj = resolver.resolve(old_id);
                ops.push(DiffOp::RemoveElement {
                    path: path.to_string(),
                    index: i,
                    old_value: old_obj
                        .map(|o| DiffValue::from_object(&o))
                        .unwrap_or(DiffValue::Null),
                });
            }
            (None, None) => unreachable!(),
        }
    }

    ops
}

fn diff_sets(
    resolver: &dyn ObjectResolver,
    path: &StatePath,
    old_items: &[ObjectId],
    new_items: &[ObjectId],
) -> Vec<DiffOp> {
    let mut ops = Vec::new();

    let old_set: BTreeSet<&ObjectId> = old_items.iter().collect();
    let new_set: BTreeSet<&ObjectId> = new_items.iter().collect();

    // Removed from set
    for id in old_set.difference(&new_set) {
        let obj = resolver.resolve(id);
        ops.push(DiffOp::RemoveFromSet {
            path: path.to_string(),
            old_value: obj
                .map(|o| DiffValue::from_object(&o))
                .unwrap_or(DiffValue::Null),
        });
    }

    // Added to set
    for id in new_set.difference(&old_set) {
        let obj = resolver.resolve(id);
        ops.push(DiffOp::AddToSet {
            path: path.to_string(),
            value: obj
                .map(|o| DiffValue::from_object(&o))
                .unwrap_or(DiffValue::Null),
        });
    }

    ops
}

fn type_name(obj: &Object) -> String {
    match obj {
        Object::Atom(Atom::Null) => "null".to_string(),
        Object::Atom(Atom::Bool(_)) => "bool".to_string(),
        Object::Atom(Atom::Int(_)) => "int".to_string(),
        Object::Atom(Atom::Float(_)) => "float".to_string(),
        Object::Atom(Atom::String(_)) => "string".to_string(),
        Object::Atom(Atom::Bytes(_)) => "bytes".to_string(),
        Object::Node(Node::Map(_)) => "map".to_string(),
        Object::Node(Node::List(_)) => "list".to_string(),
        Object::Node(Node::Set(_)) => "set".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, HashMap};

    /// Simple in-memory resolver for testing.
    struct TestResolver {
        objects: HashMap<ObjectId, Object>,
    }

    impl TestResolver {
        fn new() -> Self {
            Self {
                objects: HashMap::new(),
            }
        }

        fn store(&mut self, obj: &Object) -> ObjectId {
            let id = obj.id();
            self.objects.insert(id, obj.clone());
            id
        }

        fn store_json(&mut self, value: &serde_json::Value) -> ObjectId {
            self.store_json_inner(value)
        }

        fn store_json_inner(&mut self, value: &serde_json::Value) -> ObjectId {
            let obj = match value {
                serde_json::Value::Null => Object::null(),
                serde_json::Value::Bool(b) => Object::bool(*b),
                serde_json::Value::Number(n) => {
                    if let Some(i) = n.as_i64() {
                        Object::int(i)
                    } else {
                        Object::float(n.as_f64().unwrap())
                    }
                }
                serde_json::Value::String(s) => Object::string(s.clone()),
                serde_json::Value::Array(arr) => {
                    let ids: Vec<ObjectId> = arr.iter().map(|v| self.store_json_inner(v)).collect();
                    Object::list(ids)
                }
                serde_json::Value::Object(map) => {
                    let mut entries = BTreeMap::new();
                    for (k, v) in map {
                        let id = self.store_json_inner(v);
                        entries.insert(k.clone(), id);
                    }
                    Object::map(entries)
                }
            };
            self.store(&obj)
        }
    }

    impl ObjectResolver for TestResolver {
        fn resolve(&self, id: &ObjectId) -> Option<Object> {
            self.objects.get(id).cloned()
        }
    }

    #[test]
    fn test_identical_trees_no_diff() {
        let mut r = TestResolver::new();
        let root = r.store_json(&serde_json::json!({"a": 1, "b": 2}));
        let ops = diff(&r, &root, &root);
        assert!(ops.is_empty());
    }

    #[test]
    fn test_simple_value_change() {
        let mut r = TestResolver::new();
        let old = r.store_json(&serde_json::json!({"status": "healthy"}));
        let new = r.store_json(&serde_json::json!({"status": "unhealthy"}));

        let ops = diff(&r, &old, &new);
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            DiffOp::SetValue { path, old, new } => {
                assert_eq!(path, "/status");
                assert_eq!(*old, DiffValue::String("healthy".to_string()));
                assert_eq!(*new, DiffValue::String("unhealthy".to_string()));
            }
            _ => panic!("expected SetValue, got {:?}", ops[0]),
        }
    }

    #[test]
    fn test_add_key() {
        let mut r = TestResolver::new();
        let old = r.store_json(&serde_json::json!({"a": 1}));
        let new = r.store_json(&serde_json::json!({"a": 1, "b": 2}));

        let ops = diff(&r, &old, &new);
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            DiffOp::AddKey { path, key, value } => {
                assert_eq!(path, "/");
                assert_eq!(key, "b");
                assert_eq!(*value, DiffValue::Int(2));
            }
            _ => panic!("expected AddKey, got {:?}", ops[0]),
        }
    }

    #[test]
    fn test_remove_key() {
        let mut r = TestResolver::new();
        let old = r.store_json(&serde_json::json!({"a": 1, "b": 2}));
        let new = r.store_json(&serde_json::json!({"a": 1}));

        let ops = diff(&r, &old, &new);
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            DiffOp::RemoveKey { path, key, old_value } => {
                assert_eq!(path, "/");
                assert_eq!(key, "b");
                assert_eq!(*old_value, DiffValue::Int(2));
            }
            _ => panic!("expected RemoveKey, got {:?}", ops[0]),
        }
    }

    #[test]
    fn test_nested_changes() {
        let mut r = TestResolver::new();
        let old = r.store_json(&serde_json::json!({
            "config": {
                "network": { "subnet": "10.0.0.0/24" },
                "dns": "8.8.8.8"
            }
        }));
        let new = r.store_json(&serde_json::json!({
            "config": {
                "network": { "subnet": "192.168.0.0/16" },
                "dns": "8.8.8.8"
            }
        }));

        let ops = diff(&r, &old, &new);
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            DiffOp::SetValue { path, .. } => {
                assert_eq!(path, "/config/network/subnet");
            }
            _ => panic!("expected SetValue at nested path"),
        }
    }

    #[test]
    fn test_multiple_changes() {
        let mut r = TestResolver::new();
        let old = r.store_json(&serde_json::json!({
            "a": 1,
            "b": 2,
            "c": 3
        }));
        let new = r.store_json(&serde_json::json!({
            "a": 10,
            "b": 2,
            "d": 4
        }));

        let ops = diff(&r, &old, &new);
        // a changed, c removed, d added = 3 ops
        assert_eq!(ops.len(), 3);

        let op_types: Vec<String> = ops
            .iter()
            .map(|op| match op {
                DiffOp::SetValue { path, .. } => format!("set:{}", path),
                DiffOp::AddKey { key, .. } => format!("add:{}", key),
                DiffOp::RemoveKey { key, .. } => format!("remove:{}", key),
                _ => "other".to_string(),
            })
            .collect();

        assert!(op_types.contains(&"set:/a".to_string()));
        assert!(op_types.contains(&"remove:c".to_string()));
        assert!(op_types.contains(&"add:d".to_string()));
    }

    #[test]
    fn test_list_element_changed() {
        let mut r = TestResolver::new();
        let old = r.store_json(&serde_json::json!({"items": [1, 2, 3]}));
        let new = r.store_json(&serde_json::json!({"items": [1, 99, 3]}));

        let ops = diff(&r, &old, &new);
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            DiffOp::SetValue { path, old, new } => {
                assert_eq!(path, "/items/1");
                assert_eq!(*old, DiffValue::Int(2));
                assert_eq!(*new, DiffValue::Int(99));
            }
            _ => panic!("expected SetValue"),
        }
    }

    #[test]
    fn test_list_element_added() {
        let mut r = TestResolver::new();
        let old = r.store_json(&serde_json::json!({"items": [1, 2]}));
        let new = r.store_json(&serde_json::json!({"items": [1, 2, 3]}));

        let ops = diff(&r, &old, &new);
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            DiffOp::AddElement { path, index, value } => {
                assert_eq!(path, "/items");
                assert_eq!(*index, 2);
                assert_eq!(*value, DiffValue::Int(3));
            }
            _ => panic!("expected AddElement"),
        }
    }

    #[test]
    fn test_list_element_removed() {
        let mut r = TestResolver::new();
        let old = r.store_json(&serde_json::json!({"items": [1, 2, 3]}));
        let new = r.store_json(&serde_json::json!({"items": [1, 2]}));

        let ops = diff(&r, &old, &new);
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            DiffOp::RemoveElement { path, index, old_value } => {
                assert_eq!(path, "/items");
                assert_eq!(*index, 2);
                assert_eq!(*old_value, DiffValue::Int(3));
            }
            _ => panic!("expected RemoveElement"),
        }
    }

    #[test]
    fn test_type_change() {
        let mut r = TestResolver::new();
        let old = r.store_json(&serde_json::json!({"value": "string"}));
        let new = r.store_json(&serde_json::json!({"value": 42}));

        let ops = diff(&r, &old, &new);
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            DiffOp::SetValue { path, .. } => {
                assert_eq!(path, "/value");
            }
            _ => panic!("expected SetValue for atom type change"),
        }
    }

    #[test]
    fn test_structural_type_change() {
        let mut r = TestResolver::new();
        let old = r.store_json(&serde_json::json!({"value": "was a string"}));
        let new_list = Object::list(vec![r.store(&Object::int(1))]);
        let new_id = r.store(&new_list);

        let mut entries = BTreeMap::new();
        entries.insert("value".to_string(), new_id);
        let new_root = r.store(&Object::map(entries));

        let old = r.store_json(&serde_json::json!({"value": "was a string"}));
        let ops = diff(&r, &old, &new_root);

        assert_eq!(ops.len(), 1);
        match &ops[0] {
            DiffOp::ChangeType { path, old_type, new_type } => {
                assert_eq!(path, "/value");
                assert_eq!(old_type, "string");
                assert_eq!(new_type, "list");
            }
            _ => panic!("expected ChangeType"),
        }
    }

    #[test]
    fn test_diff_serializable() {
        let mut r = TestResolver::new();
        let old = r.store_json(&serde_json::json!({"a": 1}));
        let new = r.store_json(&serde_json::json!({"a": 2, "b": 3}));

        let ops = diff(&r, &old, &new);
        let json = serde_json::to_string_pretty(&ops).unwrap();
        assert!(json.contains("SetValue"));
        assert!(json.contains("AddKey"));

        // Deserialize roundtrip
        let parsed: Vec<DiffOp> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, ops);
    }
}
