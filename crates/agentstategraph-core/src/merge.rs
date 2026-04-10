//! Schema-aware merge engine.
//!
//! AgentStateGraph's merge operates on structured data, not text lines.
//! Many concurrent changes auto-resolve based on type:
//!   - Different keys modified → union both changes
//!   - Identical changes from both sides → deduplicate
//!   - Same scalar modified differently → conflict
//!
//! Future: schema annotations (x-agentstategraph-merge) will enable
//! CRDT-inspired resolution (sum, max, union-by-id, etc.)

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::diff::ObjectResolver;
use crate::object::{Node, Object, ObjectId};

/// The result of a merge operation.
#[derive(Debug, Clone)]
pub enum MergeResult {
    /// Merge succeeded without conflicts.
    Success(Object),
    /// Merge has conflicts that need resolution.
    Conflicts {
        /// The partially merged object (conflicts use "ours" value).
        partial: Object,
        /// The conflicts that couldn't be auto-resolved.
        conflicts: Vec<Conflict>,
    },
    /// Fast-forward: one side is an ancestor of the other.
    FastForward(ObjectId),
}

/// A merge conflict at a specific path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Conflict {
    /// Path where the conflict occurred.
    pub path: String,
    /// The value from "our" side (target branch).
    pub ours: Option<ConflictValue>,
    /// The value from "their" side (source branch).
    pub theirs: Option<ConflictValue>,
    /// The value from the common ancestor (base).
    pub base: Option<ConflictValue>,
    /// A suggested resolution (if the engine can propose one).
    pub suggested_resolution: Option<ConflictValue>,
}

/// Simplified value representation for conflict reporting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ConflictValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Complex(String),
}

impl ConflictValue {
    pub fn from_object(obj: &Object) -> Self {
        match obj {
            Object::Atom(a) => match a {
                crate::object::Atom::Null => ConflictValue::Null,
                crate::object::Atom::Bool(b) => ConflictValue::Bool(*b),
                crate::object::Atom::Int(i) => ConflictValue::Int(*i),
                crate::object::Atom::Float(f) => ConflictValue::Float(*f),
                crate::object::Atom::String(s) => ConflictValue::String(s.clone()),
                crate::object::Atom::Bytes(_) => ConflictValue::String("[bytes]".to_string()),
            },
            Object::Node(n) => match n {
                Node::Map(m) => ConflictValue::Complex(format!("{{map: {} keys}}", m.len())),
                Node::List(l) => ConflictValue::Complex(format!("[list: {} items]", l.len())),
                Node::Set(s) => ConflictValue::Complex(format!("{{set: {} items}}", s.len())),
            },
        }
    }
}

/// Perform a three-way merge of two state trees relative to a common ancestor.
///
/// - `base`: the common ancestor state root
/// - `ours`: the target branch state root (what we're merging INTO)
/// - `theirs`: the source branch state root (what we're merging FROM)
///
/// Returns a MergeResult indicating success, conflicts, or fast-forward.
pub fn three_way_merge(
    resolver: &dyn ObjectResolver,
    base: &ObjectId,
    ours: &ObjectId,
    theirs: &ObjectId,
) -> MergeResult {
    // Fast-forward cases
    if base == ours {
        return MergeResult::FastForward(*theirs);
    }
    if base == theirs {
        return MergeResult::FastForward(*ours);
    }
    if ours == theirs {
        return MergeResult::FastForward(*ours);
    }

    let base_obj = match resolver.resolve(base) {
        Some(obj) => obj,
        None => return MergeResult::FastForward(*theirs),
    };
    let ours_obj = match resolver.resolve(ours) {
        Some(obj) => obj,
        None => return MergeResult::FastForward(*theirs),
    };
    let theirs_obj = match resolver.resolve(theirs) {
        Some(obj) => obj,
        None => return MergeResult::FastForward(*ours),
    };

    let path = String::from("/");
    let mut conflicts = Vec::new();

    let merged = merge_objects(
        resolver,
        &path,
        &base_obj,
        &ours_obj,
        &theirs_obj,
        &mut conflicts,
    );

    if conflicts.is_empty() {
        MergeResult::Success(merged)
    } else {
        MergeResult::Conflicts {
            partial: merged,
            conflicts,
        }
    }
}

/// Core recursive merge logic.
fn merge_objects(
    resolver: &dyn ObjectResolver,
    path: &str,
    base: &Object,
    ours: &Object,
    theirs: &Object,
    conflicts: &mut Vec<Conflict>,
) -> Object {
    // If both sides are identical, no conflict
    if ours == theirs {
        return ours.clone();
    }

    // If only one side changed from base, take that side
    if base == ours {
        return theirs.clone();
    }
    if base == theirs {
        return ours.clone();
    }

    // Both sides changed from base — need type-specific merge
    match (base, ours, theirs) {
        // All three are maps — merge keys
        (
            Object::Node(Node::Map(base_entries)),
            Object::Node(Node::Map(our_entries)),
            Object::Node(Node::Map(their_entries)),
        ) => merge_maps(
            resolver,
            path,
            base_entries,
            our_entries,
            their_entries,
            conflicts,
        ),

        // All three are lists — element-wise merge (limited)
        (
            Object::Node(Node::List(base_items)),
            Object::Node(Node::List(our_items)),
            Object::Node(Node::List(their_items)),
        ) => merge_lists(
            resolver,
            path,
            base_items,
            our_items,
            their_items,
            conflicts,
        ),

        // All three are sets — union
        (
            Object::Node(Node::Set(_base_items)),
            Object::Node(Node::Set(our_items)),
            Object::Node(Node::Set(their_items)),
        ) => merge_sets(our_items, their_items),

        // Both are atoms but different — conflict
        (Object::Atom(_), Object::Atom(_), Object::Atom(_)) => {
            conflicts.push(Conflict {
                path: path.to_string(),
                ours: Some(ConflictValue::from_object(ours)),
                theirs: Some(ConflictValue::from_object(theirs)),
                base: Some(ConflictValue::from_object(base)),
                suggested_resolution: None,
            });
            // Default to "ours" for partial merge
            ours.clone()
        }

        // Type mismatch — conflict
        _ => {
            conflicts.push(Conflict {
                path: path.to_string(),
                ours: Some(ConflictValue::from_object(ours)),
                theirs: Some(ConflictValue::from_object(theirs)),
                base: Some(ConflictValue::from_object(base)),
                suggested_resolution: None,
            });
            ours.clone()
        }
    }
}

fn merge_maps(
    resolver: &dyn ObjectResolver,
    path: &str,
    base_entries: &BTreeMap<String, ObjectId>,
    our_entries: &BTreeMap<String, ObjectId>,
    their_entries: &BTreeMap<String, ObjectId>,
    conflicts: &mut Vec<Conflict>,
) -> Object {
    let mut merged = BTreeMap::new();

    // Collect all keys from all three sides
    let mut all_keys: std::collections::BTreeSet<&String> = std::collections::BTreeSet::new();
    all_keys.extend(base_entries.keys());
    all_keys.extend(our_entries.keys());
    all_keys.extend(their_entries.keys());

    for key in all_keys {
        let base_id = base_entries.get(key);
        let our_id = our_entries.get(key);
        let their_id = their_entries.get(key);
        let child_path = format!("{}{}{}", path, if path == "/" { "" } else { "/" }, key);

        match (base_id, our_id, their_id) {
            // Key exists in all three
            (Some(b), Some(o), Some(t)) => {
                if o == t {
                    // Both sides agree
                    merged.insert(key.clone(), *o);
                } else if b == o {
                    // Only theirs changed
                    merged.insert(key.clone(), *t);
                } else if b == t {
                    // Only ours changed
                    merged.insert(key.clone(), *o);
                } else {
                    // Both changed differently — recurse
                    let base_obj = resolver.resolve(b);
                    let our_obj = resolver.resolve(o);
                    let their_obj = resolver.resolve(t);

                    match (base_obj, our_obj, their_obj) {
                        (Some(bo), Some(oo), Some(to)) => {
                            let merged_child =
                                merge_objects(resolver, &child_path, &bo, &oo, &to, conflicts);
                            // Store the merged object — we need to compute its ID
                            let merged_id = merged_child.id();
                            merged.insert(key.clone(), merged_id);
                        }
                        _ => {
                            // Can't resolve — conflict, keep ours
                            if let Some(o) = our_id {
                                merged.insert(key.clone(), *o);
                            }
                            conflicts.push(Conflict {
                                path: child_path,
                                ours: None,
                                theirs: None,
                                base: None,
                                suggested_resolution: None,
                            });
                        }
                    }
                }
            }
            // Key added by ours only
            (None, Some(o), None) => {
                merged.insert(key.clone(), *o);
            }
            // Key added by theirs only
            (None, None, Some(t)) => {
                merged.insert(key.clone(), *t);
            }
            // Key added by both — check if same value
            (None, Some(o), Some(t)) => {
                if o == t {
                    merged.insert(key.clone(), *o);
                } else {
                    // Both added same key with different values — conflict
                    conflicts.push(Conflict {
                        path: child_path,
                        ours: resolver
                            .resolve(o)
                            .map(|obj| ConflictValue::from_object(&obj)),
                        theirs: resolver
                            .resolve(t)
                            .map(|obj| ConflictValue::from_object(&obj)),
                        base: None,
                        suggested_resolution: None,
                    });
                    merged.insert(key.clone(), *o); // default to ours
                }
            }
            // Key deleted by ours
            (Some(_), None, Some(t)) => {
                if base_id == Some(t) {
                    // Theirs didn't change it, ours deleted — keep deleted
                } else {
                    // Theirs modified, ours deleted — conflict
                    conflicts.push(Conflict {
                        path: child_path,
                        ours: None, // deleted
                        theirs: resolver
                            .resolve(t)
                            .map(|obj| ConflictValue::from_object(&obj)),
                        base: base_id
                            .and_then(|b| resolver.resolve(b))
                            .map(|obj| ConflictValue::from_object(&obj)),
                        suggested_resolution: None,
                    });
                    // Default: keep deleted (ours wins)
                }
            }
            // Key deleted by theirs
            (Some(_), Some(o), None) => {
                if base_id == Some(o) {
                    // Ours didn't change it, theirs deleted — keep deleted
                } else {
                    // Ours modified, theirs deleted — conflict
                    conflicts.push(Conflict {
                        path: child_path,
                        ours: resolver
                            .resolve(o)
                            .map(|obj| ConflictValue::from_object(&obj)),
                        theirs: None, // deleted
                        base: base_id
                            .and_then(|b| resolver.resolve(b))
                            .map(|obj| ConflictValue::from_object(&obj)),
                        suggested_resolution: None,
                    });
                    merged.insert(key.clone(), *o); // default: keep ours
                }
            }
            // Key deleted by both
            (Some(_), None, None) => {
                // Both deleted — agree, don't include
            }
            // Key doesn't exist anywhere
            (None, None, None) => {}
        }
    }

    Object::map(merged)
}

fn merge_lists(
    _resolver: &dyn ObjectResolver,
    path: &str,
    base_items: &[ObjectId],
    our_items: &[ObjectId],
    their_items: &[ObjectId],
    conflicts: &mut Vec<Conflict>,
) -> Object {
    // Simple list merge: if lengths differ or elements differ, conflict
    // Future: smarter merge with move detection
    if our_items == their_items {
        return Object::list(our_items.to_vec());
    }

    // For now, if both sides modified the list differently, it's a conflict
    conflicts.push(Conflict {
        path: path.to_string(),
        ours: Some(ConflictValue::Complex(format!(
            "[list: {} items]",
            our_items.len()
        ))),
        theirs: Some(ConflictValue::Complex(format!(
            "[list: {} items]",
            their_items.len()
        ))),
        base: Some(ConflictValue::Complex(format!(
            "[list: {} items]",
            base_items.len()
        ))),
        suggested_resolution: None,
    });

    // Default to ours
    Object::list(our_items.to_vec())
}

fn merge_sets(our_items: &[ObjectId], their_items: &[ObjectId]) -> Object {
    // Sets merge via union — no conflicts possible
    let mut combined: std::collections::BTreeSet<ObjectId> = std::collections::BTreeSet::new();
    combined.extend(our_items.iter().copied());
    combined.extend(their_items.iter().copied());
    Object::set(combined.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

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
                        entries.insert(k.clone(), self.store_json_inner(v));
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
    fn test_fast_forward_base_equals_ours() {
        let mut r = TestResolver::new();
        let base = r.store_json(&serde_json::json!({"a": 1}));
        let theirs = r.store_json(&serde_json::json!({"a": 2}));

        match three_way_merge(&r, &base, &base, &theirs) {
            MergeResult::FastForward(id) => assert_eq!(id, theirs),
            _ => panic!("expected fast-forward"),
        }
    }

    #[test]
    fn test_no_conflict_different_keys() {
        let mut r = TestResolver::new();
        let base = r.store_json(&serde_json::json!({"a": 1, "b": 2}));
        let ours = r.store_json(&serde_json::json!({"a": 10, "b": 2})); // changed a
        let theirs = r.store_json(&serde_json::json!({"a": 1, "b": 20})); // changed b

        match three_way_merge(&r, &base, &ours, &theirs) {
            MergeResult::Success(merged) => {
                // Both changes should be present
                if let Object::Node(Node::Map(entries)) = &merged {
                    let a = r.objects.get(entries.get("a").unwrap());
                    let b = r.objects.get(entries.get("b").unwrap());
                    // a should be 10 (ours), b should be 20 (theirs)
                    assert_eq!(a, Some(&Object::int(10)));
                    assert_eq!(b, Some(&Object::int(20)));
                } else {
                    panic!("expected map");
                }
            }
            MergeResult::Conflicts { conflicts, .. } => {
                panic!("unexpected conflicts: {:?}", conflicts);
            }
            _ => panic!("expected success"),
        }
    }

    #[test]
    fn test_conflict_same_key_different_values() {
        let mut r = TestResolver::new();
        let base = r.store_json(&serde_json::json!({"x": 1}));
        let ours = r.store_json(&serde_json::json!({"x": 2}));
        let theirs = r.store_json(&serde_json::json!({"x": 3}));

        match three_way_merge(&r, &base, &ours, &theirs) {
            MergeResult::Conflicts { conflicts, .. } => {
                assert_eq!(conflicts.len(), 1);
                assert!(conflicts[0].path.contains("x"));
            }
            _ => panic!("expected conflict"),
        }
    }

    #[test]
    fn test_both_add_same_key_same_value() {
        let mut r = TestResolver::new();
        let base = r.store_json(&serde_json::json!({"a": 1}));
        let ours = r.store_json(&serde_json::json!({"a": 1, "b": 2}));
        let theirs = r.store_json(&serde_json::json!({"a": 1, "b": 2}));

        match three_way_merge(&r, &base, &ours, &theirs) {
            MergeResult::Success(_) | MergeResult::FastForward(_) => {} // ok
            MergeResult::Conflicts { conflicts, .. } => {
                panic!("unexpected conflicts: {:?}", conflicts);
            }
        }
    }

    #[test]
    fn test_both_add_same_key_different_value() {
        let mut r = TestResolver::new();
        let base = r.store_json(&serde_json::json!({"a": 1}));
        let ours = r.store_json(&serde_json::json!({"a": 1, "b": 2}));
        let theirs = r.store_json(&serde_json::json!({"a": 1, "b": 3}));

        match three_way_merge(&r, &base, &ours, &theirs) {
            MergeResult::Conflicts { conflicts, .. } => {
                assert!(conflicts.len() >= 1);
            }
            _ => panic!("expected conflict"),
        }
    }

    #[test]
    fn test_one_deletes_other_modifies() {
        let mut r = TestResolver::new();
        let base = r.store_json(&serde_json::json!({"a": 1, "b": 2}));
        let ours = r.store_json(&serde_json::json!({"a": 1})); // deleted b
        let theirs = r.store_json(&serde_json::json!({"a": 1, "b": 99})); // modified b

        match three_way_merge(&r, &base, &ours, &theirs) {
            MergeResult::Conflicts { conflicts, .. } => {
                assert!(conflicts.len() >= 1, "delete-vs-modify should conflict");
            }
            _ => panic!("expected conflict for delete-vs-modify"),
        }
    }

    #[test]
    fn test_nested_merge_no_conflict() {
        let mut r = TestResolver::new();
        let base = r.store_json(&serde_json::json!({
            "config": {"network": {"subnet": "10.0.0.0/24"}, "dns": "8.8.8.8"}
        }));
        let ours = r.store_json(&serde_json::json!({
            "config": {"network": {"subnet": "192.168.0.0/16"}, "dns": "8.8.8.8"}
        }));
        let theirs = r.store_json(&serde_json::json!({
            "config": {"network": {"subnet": "10.0.0.0/24"}, "dns": "1.1.1.1"}
        }));

        match three_way_merge(&r, &base, &ours, &theirs) {
            MergeResult::Success(merged) => {
                // subnet should be ours (192.168.0.0/16), dns should be theirs (1.1.1.1)
                // This is a successful merge of non-conflicting nested changes
            }
            MergeResult::Conflicts { conflicts, .. } => {
                panic!("unexpected conflicts: {:?}", conflicts);
            }
            _ => panic!("expected success"),
        }
    }

    #[test]
    fn test_both_sides_identical_changes() {
        let mut r = TestResolver::new();
        let base = r.store_json(&serde_json::json!({"x": 1}));
        let ours = r.store_json(&serde_json::json!({"x": 5}));
        let theirs = r.store_json(&serde_json::json!({"x": 5}));

        match three_way_merge(&r, &base, &ours, &theirs) {
            MergeResult::Success(_) | MergeResult::FastForward(_) => {} // ok — both agree
            MergeResult::Conflicts { .. } => panic!("identical changes should not conflict"),
        }
    }
}
