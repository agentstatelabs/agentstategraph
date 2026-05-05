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
                assert!(!conflicts.is_empty());
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
                assert!(!conflicts.is_empty(), "delete-vs-modify should conflict");
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
            MergeResult::Success(_merged) => {
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

    // --- Set union ---

    #[test]
    fn test_set_union_no_conflict() {
        let mut r = TestResolver::new();
        let a = r.store(&Object::int(1));
        let b = r.store(&Object::int(2));
        let c = r.store(&Object::int(3));

        // base: {1}, ours: {1,2}, theirs: {1,3} → merged: {1,2,3}
        let base = r.store(&Object::set(vec![a]));
        let ours = r.store(&Object::set(vec![a, b]));
        let theirs = r.store(&Object::set(vec![a, c]));

        match three_way_merge(&r, &base, &ours, &theirs) {
            MergeResult::Success(merged) => {
                if let Object::Node(Node::Set(items)) = merged {
                    assert_eq!(
                        items.len(),
                        3,
                        "union of {{1,2}} and {{1,3}} should have 3 items"
                    );
                    assert!(items.contains(&a));
                    assert!(items.contains(&b));
                    assert!(items.contains(&c));
                } else {
                    panic!("expected set");
                }
            }
            MergeResult::Conflicts { conflicts, .. } => {
                panic!("sets never conflict, got: {:?}", conflicts);
            }
            MergeResult::FastForward(_) => panic!("expected success"),
        }
    }

    #[test]
    fn test_set_disjoint_union() {
        let mut r = TestResolver::new();
        let a = r.store(&Object::int(1));
        let b = r.store(&Object::int(2));
        let c = r.store(&Object::int(3));

        // base: {}, ours: {1,2}, theirs: {3} → merged: {1,2,3}
        let base = r.store(&Object::set(vec![]));
        let ours = r.store(&Object::set(vec![a, b]));
        let theirs = r.store(&Object::set(vec![c]));

        match three_way_merge(&r, &base, &ours, &theirs) {
            MergeResult::Success(merged) => {
                if let Object::Node(Node::Set(items)) = merged {
                    assert_eq!(items.len(), 3);
                } else {
                    panic!("expected set");
                }
            }
            MergeResult::Conflicts { conflicts, .. } => {
                panic!("sets never conflict: {:?}", conflicts);
            }
            MergeResult::FastForward(_) => panic!("expected success"),
        }
    }

    // --- List conflict ---

    #[test]
    fn test_list_conflict_both_sides_modify() {
        let mut r = TestResolver::new();
        let x = r.store(&Object::int(10));
        let y = r.store(&Object::int(20));
        let z = r.store(&Object::int(30));
        let w = r.store(&Object::int(40));

        let base = r.store(&Object::list(vec![x, y]));
        let ours = r.store(&Object::list(vec![x, z])); // changed second element
        let theirs = r.store(&Object::list(vec![x, w])); // changed second element differently

        match three_way_merge(&r, &base, &ours, &theirs) {
            MergeResult::Conflicts { conflicts, .. } => {
                assert_eq!(conflicts.len(), 1);
                // Conflict value should describe list lengths
                assert!(matches!(
                    &conflicts[0].ours,
                    Some(ConflictValue::Complex(s)) if s.contains("list")
                ));
                assert!(matches!(
                    &conflicts[0].theirs,
                    Some(ConflictValue::Complex(s)) if s.contains("list")
                ));
                assert!(matches!(
                    &conflicts[0].base,
                    Some(ConflictValue::Complex(s)) if s.contains("list")
                ));
            }
            _ => panic!("expected list conflict"),
        }
    }

    #[test]
    fn test_list_same_after_change_no_conflict() {
        let mut r = TestResolver::new();
        let x = r.store(&Object::int(10));
        let y = r.store(&Object::int(20));

        let base = r.store(&Object::list(vec![x]));
        let ours = r.store(&Object::list(vec![x, y]));
        let theirs = r.store(&Object::list(vec![x, y])); // both made identical change

        match three_way_merge(&r, &base, &ours, &theirs) {
            MergeResult::Success(_) | MergeResult::FastForward(_) => {}
            MergeResult::Conflicts { conflicts, .. } => {
                panic!(
                    "identical list changes should not conflict: {:?}",
                    conflicts
                );
            }
        }
    }

    // --- Both-sides delete ---

    #[test]
    fn test_both_sides_delete_same_key() {
        let mut r = TestResolver::new();
        let base = r.store_json(&serde_json::json!({"a": 1, "b": 2, "c": 3}));
        let ours = r.store_json(&serde_json::json!({"a": 1, "c": 3})); // deleted b
        let theirs = r.store_json(&serde_json::json!({"a": 1, "c": 3})); // also deleted b

        match three_way_merge(&r, &base, &ours, &theirs) {
            MergeResult::Success(merged) => {
                if let Object::Node(Node::Map(entries)) = merged {
                    assert!(!entries.contains_key("b"), "b should have been deleted");
                    assert!(entries.contains_key("a"));
                    assert!(entries.contains_key("c"));
                } else {
                    panic!("expected map");
                }
            }
            MergeResult::FastForward(id) => {
                // FastForward to ours/theirs is also acceptable — both sides deleted b
                assert!(id == ours || id == theirs);
            }
            MergeResult::Conflicts { conflicts, .. } => {
                panic!("both-sides-delete should not conflict: {:?}", conflicts);
            }
        }
    }

    #[test]
    fn test_one_side_deletes_unchanged_key() {
        let mut r = TestResolver::new();
        // ours deletes a key that theirs didn't touch — since theirs==base the engine
        // fast-forwards to ours, which already has b removed.
        let base = r.store_json(&serde_json::json!({"a": 1, "b": 2}));
        let ours = r.store_json(&serde_json::json!({"a": 1})); // deleted b
        let theirs = r.store_json(&serde_json::json!({"a": 1, "b": 2})); // unchanged == base

        match three_way_merge(&r, &base, &ours, &theirs) {
            MergeResult::FastForward(id) => {
                // Fast-forward to ours — correct: theirs==base so we take ours
                assert_eq!(id, ours);
            }
            MergeResult::Success(merged) => {
                if let Object::Node(Node::Map(entries)) = merged {
                    assert!(!entries.contains_key("b"));
                } else {
                    panic!("expected map");
                }
            }
            MergeResult::Conflicts { conflicts, .. } => {
                panic!("clean delete should not conflict: {:?}", conflicts);
            }
        }
    }

    // --- Deeply nested ---

    #[test]
    fn test_deeply_nested_conflict_at_leaf() {
        let mut r = TestResolver::new();
        let base = r.store_json(&serde_json::json!({
            "level1": {
                "level2": {
                    "level3": {"value": 1}
                }
            }
        }));
        let ours = r.store_json(&serde_json::json!({
            "level1": {
                "level2": {
                    "level3": {"value": 42}
                }
            }
        }));
        let theirs = r.store_json(&serde_json::json!({
            "level1": {
                "level2": {
                    "level3": {"value": 99}
                }
            }
        }));

        match three_way_merge(&r, &base, &ours, &theirs) {
            MergeResult::Conflicts { conflicts, .. } => {
                assert_eq!(conflicts.len(), 1);
                assert!(
                    conflicts[0].path.contains("value"),
                    "conflict path should identify the leaf: {}",
                    conflicts[0].path
                );
            }
            _ => panic!("expected exactly one deep conflict"),
        }
    }

    #[test]
    fn test_deeply_nested_no_conflict_different_branches() {
        let mut r = TestResolver::new();
        let base = r.store_json(&serde_json::json!({
            "a": {"x": 1, "y": 2},
            "b": {"x": 1, "y": 2}
        }));
        let ours = r.store_json(&serde_json::json!({
            "a": {"x": 99, "y": 2}, // changed a.x
            "b": {"x": 1, "y": 2}
        }));
        let theirs = r.store_json(&serde_json::json!({
            "a": {"x": 1, "y": 2},
            "b": {"x": 1, "y": 77} // changed b.y
        }));

        match three_way_merge(&r, &base, &ours, &theirs) {
            MergeResult::Success(_) => {}
            MergeResult::Conflicts { conflicts, .. } => {
                panic!("no conflicts expected: {:?}", conflicts);
            }
            MergeResult::FastForward(_) => panic!("expected success"),
        }
    }

    // --- Large maps ---

    #[test]
    fn test_large_map_exact_conflict_count() {
        let mut r = TestResolver::new();

        let mut base_map = serde_json::Map::new();
        let mut our_map = serde_json::Map::new();
        let mut their_map = serde_json::Map::new();

        for i in 0..100_i64 {
            let key = format!("key{:03}", i);
            base_map.insert(key.clone(), serde_json::Value::Number(i.into()));
            if i >= 1 && i <= 5 {
                // Keys 1-5: ours=i+1000, theirs=i+2000 → 5 distinct conflicts
                our_map.insert(key.clone(), serde_json::Value::Number((i + 1000).into()));
                their_map.insert(key.clone(), serde_json::Value::Number((i + 2000).into()));
            } else {
                our_map.insert(key.clone(), serde_json::Value::Number(i.into()));
                their_map.insert(key.clone(), serde_json::Value::Number(i.into()));
            }
        }

        let base = r.store_json(&serde_json::Value::Object(base_map));
        let ours = r.store_json(&serde_json::Value::Object(our_map));
        let theirs = r.store_json(&serde_json::Value::Object(their_map));

        match three_way_merge(&r, &base, &ours, &theirs) {
            MergeResult::Conflicts { conflicts, partial } => {
                assert_eq!(conflicts.len(), 5, "exactly 5 conflicts expected");
                // Partial merge should still contain all 100 keys (ours wins on conflicts)
                if let Object::Node(Node::Map(entries)) = partial {
                    assert_eq!(entries.len(), 100);
                } else {
                    panic!("expected map");
                }
            }
            _ => panic!("expected exactly 5 conflicts"),
        }
    }

    // --- Conflict value reporting ---

    #[test]
    fn test_conflict_values_populated() {
        let mut r = TestResolver::new();
        let base = r.store_json(&serde_json::json!({"x": 1}));
        let ours = r.store_json(&serde_json::json!({"x": 2}));
        let theirs = r.store_json(&serde_json::json!({"x": 3}));

        match three_way_merge(&r, &base, &ours, &theirs) {
            MergeResult::Conflicts { conflicts, .. } => {
                let c = &conflicts[0];
                assert_eq!(c.ours, Some(ConflictValue::Int(2)));
                assert_eq!(c.theirs, Some(ConflictValue::Int(3)));
                assert_eq!(c.base, Some(ConflictValue::Int(1)));
                assert!(c.suggested_resolution.is_none());
            }
            _ => panic!("expected conflict"),
        }
    }

    // --- ConflictValue::from_object coverage ---

    #[test]
    fn test_conflict_value_from_object_atoms() {
        assert_eq!(
            ConflictValue::from_object(&Object::null()),
            ConflictValue::Null
        );
        assert_eq!(
            ConflictValue::from_object(&Object::bool(true)),
            ConflictValue::Bool(true)
        );
        assert_eq!(
            ConflictValue::from_object(&Object::bool(false)),
            ConflictValue::Bool(false)
        );
        assert_eq!(
            ConflictValue::from_object(&Object::int(42)),
            ConflictValue::Int(42)
        );
        assert_eq!(
            ConflictValue::from_object(&Object::int(-1)),
            ConflictValue::Int(-1)
        );
        assert_eq!(
            ConflictValue::from_object(&Object::string("hello".to_string())),
            ConflictValue::String("hello".to_string())
        );
        // Bytes → "[bytes]" string
        assert_eq!(
            ConflictValue::from_object(&Object::bytes(vec![1, 2, 3])),
            ConflictValue::String("[bytes]".to_string())
        );
    }

    #[test]
    fn test_conflict_value_from_object_nodes() {
        // Map
        let map = Object::map(std::collections::BTreeMap::from([
            ("a".to_string(), Object::int(1).id()),
            ("b".to_string(), Object::int(2).id()),
        ]));
        assert_eq!(
            ConflictValue::from_object(&map),
            ConflictValue::Complex("{map: 2 keys}".to_string())
        );

        // List
        let list = Object::list(vec![
            Object::int(1).id(),
            Object::int(2).id(),
            Object::int(3).id(),
        ]);
        assert_eq!(
            ConflictValue::from_object(&list),
            ConflictValue::Complex("[list: 3 items]".to_string())
        );

        // Set
        let set = Object::set(vec![Object::int(1).id()]);
        assert_eq!(
            ConflictValue::from_object(&set),
            ConflictValue::Complex("{set: 1 items}".to_string())
        );
    }

    // --- Fast-forward edge cases ---

    #[test]
    fn test_fast_forward_base_equals_theirs() {
        let mut r = TestResolver::new();
        let base = r.store_json(&serde_json::json!({"a": 1}));
        let ours = r.store_json(&serde_json::json!({"a": 2}));

        // base == theirs → fast-forward to ours
        match three_way_merge(&r, &base, &ours, &base) {
            MergeResult::FastForward(id) => assert_eq!(id, ours),
            _ => panic!("expected fast-forward to ours"),
        }
    }

    #[test]
    fn test_fast_forward_ours_equals_theirs() {
        let mut r = TestResolver::new();
        let base = r.store_json(&serde_json::json!({"a": 1}));
        let ours = r.store_json(&serde_json::json!({"a": 99}));
        // Same content as ours — id will match
        let theirs = ours;

        match three_way_merge(&r, &base, &ours, &theirs) {
            MergeResult::FastForward(id) => assert_eq!(id, ours),
            _ => panic!("expected fast-forward when ours==theirs"),
        }
    }

    // --- Type mismatch conflict ---

    #[test]
    fn test_type_mismatch_conflict() {
        let mut r = TestResolver::new();
        // base: scalar, ours: map, theirs: list — type mismatch
        let base = r.store_json(&serde_json::json!({"x": 1}));
        let ours = r.store_json(&serde_json::json!({"x": {"nested": true}}));
        let theirs = r.store_json(&serde_json::json!({"x": [1, 2, 3]}));

        match three_way_merge(&r, &base, &ours, &theirs) {
            MergeResult::Conflicts { conflicts, .. } => {
                assert!(!conflicts.is_empty());
                assert!(conflicts[0].path.contains("x"));
            }
            _ => panic!("expected conflict on type mismatch"),
        }
    }
}
