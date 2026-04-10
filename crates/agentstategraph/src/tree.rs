//! State tree operations — get, set, delete values by path.
//!
//! The state tree is a Merkle DAG of Objects. Modifications create new
//! objects along the modified path (structural sharing), leaving all
//! other subtrees untouched.

use std::collections::BTreeMap;

use agentstategraph_core::{Atom, Node, Object, ObjectId, PathComponent, StatePath};
use agentstategraph_storage::{ObjectStore, StorageError};

/// Read a value from the state tree at the given path.
/// Returns the Object at that path, or an error if the path doesn't exist.
pub fn tree_get(
    store: &dyn ObjectStore,
    root: &ObjectId,
    path: &StatePath,
) -> Result<Object, TreeError> {
    let root_obj = store
        .get_object(root)?
        .ok_or_else(|| TreeError::ObjectNotFound(*root))?;

    if path.is_root() {
        return Ok(root_obj);
    }

    let mut current = root_obj;
    for (i, component) in path.components().iter().enumerate() {
        current = navigate(&current, component, store, path, i)?;
    }

    Ok(current)
}

/// Set a value in the state tree at the given path.
/// Returns the new root ObjectId after the modification.
/// Creates intermediate maps as needed for new paths.
///
/// This operation uses structural sharing — only objects along the
/// modified path are new; all other subtrees retain their ObjectIds.
pub fn tree_set(
    store: &dyn ObjectStore,
    root: &ObjectId,
    path: &StatePath,
    value: &Object,
) -> Result<ObjectId, TreeError> {
    // Store the new value and get its ID
    let value_id = store.put_object(value)?;

    if path.is_root() {
        return Ok(value_id);
    }

    // Load the root
    let root_obj = store
        .get_object(root)?
        .ok_or_else(|| TreeError::ObjectNotFound(*root))?;

    // Recursively set the value, rebuilding the path from leaf to root
    let new_root = set_recursive(store, &root_obj, path.components(), 0, value_id)?;
    let new_root_id = store.put_object(&new_root)?;
    Ok(new_root_id)
}

/// Delete a value from the state tree at the given path.
/// Returns the new root ObjectId after the deletion.
pub fn tree_delete(
    store: &dyn ObjectStore,
    root: &ObjectId,
    path: &StatePath,
) -> Result<ObjectId, TreeError> {
    if path.is_root() {
        // Deleting root → return empty map
        let empty = Object::empty_map();
        let id = store.put_object(&empty)?;
        return Ok(id);
    }

    let root_obj = store
        .get_object(root)?
        .ok_or_else(|| TreeError::ObjectNotFound(*root))?;

    let new_root = delete_recursive(store, &root_obj, path.components(), 0)?;
    let new_root_id = store.put_object(&new_root)?;
    Ok(new_root_id)
}

/// Convert an Object to a serde_json::Value for user-facing output.
/// Recursively resolves all ObjectId references.
pub fn tree_to_json(store: &dyn ObjectStore, obj: &Object) -> Result<serde_json::Value, TreeError> {
    match obj {
        Object::Atom(atom) => Ok(atom_to_json(atom)),
        Object::Node(node) => match node {
            Node::Map(entries) => {
                let mut map = serde_json::Map::new();
                for (key, child_id) in entries {
                    let child = store
                        .get_object(child_id)?
                        .ok_or_else(|| TreeError::ObjectNotFound(*child_id))?;
                    map.insert(key.clone(), tree_to_json(store, &child)?);
                }
                Ok(serde_json::Value::Object(map))
            }
            Node::List(items) => {
                let mut arr = Vec::new();
                for child_id in items {
                    let child = store
                        .get_object(child_id)?
                        .ok_or_else(|| TreeError::ObjectNotFound(*child_id))?;
                    arr.push(tree_to_json(store, &child)?);
                }
                Ok(serde_json::Value::Array(arr))
            }
            Node::Set(items) => {
                let mut arr = Vec::new();
                for child_id in items {
                    let child = store
                        .get_object(child_id)?
                        .ok_or_else(|| TreeError::ObjectNotFound(*child_id))?;
                    arr.push(tree_to_json(store, &child)?);
                }
                Ok(serde_json::Value::Array(arr))
            }
        },
    }
}

/// List all paths in the state tree under a given prefix.
/// Returns leaf paths (paths that point to atoms/values, not intermediate maps).
pub fn tree_list_paths(
    store: &dyn ObjectStore,
    root: &ObjectId,
    prefix: &str,
    max_depth: usize,
) -> Result<Vec<String>, TreeError> {
    let root_obj = store
        .get_object(root)?
        .ok_or_else(|| TreeError::ObjectNotFound(*root))?;

    // If prefix is non-empty, navigate to the subtree first
    let (start_obj, base_path) = if prefix.is_empty() || prefix == "/" {
        (root_obj, String::new())
    } else {
        let path = StatePath::parse(prefix)
            .map_err(|e| TreeError::PathNotFound(e.to_string()))?;
        let obj = tree_get(store, root, &path)?;
        (obj, prefix.to_string())
    };

    let mut paths = Vec::new();
    collect_paths(store, &start_obj, &base_path, max_depth, 0, &mut paths)?;
    Ok(paths)
}

fn collect_paths(
    store: &dyn ObjectStore,
    obj: &Object,
    current_path: &str,
    max_depth: usize,
    depth: usize,
    paths: &mut Vec<String>,
) -> Result<(), TreeError> {
    if depth > max_depth {
        return Ok(());
    }

    match obj {
        Object::Atom(_) => {
            let path = if current_path.is_empty() { "/".to_string() } else { current_path.to_string() };
            paths.push(path);
        }
        Object::Node(node) => match node {
            Node::Map(entries) => {
                if entries.is_empty() {
                    let path = if current_path.is_empty() { "/".to_string() } else { current_path.to_string() };
                    paths.push(path);
                } else {
                    for (key, child_id) in entries {
                        let child_path = format!("{}/{}", current_path, key);
                        let child = store.get_object(child_id)?
                            .ok_or_else(|| TreeError::ObjectNotFound(*child_id))?;
                        collect_paths(store, &child, &child_path, max_depth, depth + 1, paths)?;
                    }
                }
            }
            Node::List(items) => {
                for (i, child_id) in items.iter().enumerate() {
                    let child_path = format!("{}/{}", current_path, i);
                    let child = store.get_object(child_id)?
                        .ok_or_else(|| TreeError::ObjectNotFound(*child_id))?;
                    collect_paths(store, &child, &child_path, max_depth, depth + 1, paths)?;
                }
            }
            Node::Set(items) => {
                for (i, child_id) in items.iter().enumerate() {
                    let child_path = format!("{}/{}", current_path, i);
                    let child = store.get_object(child_id)?
                        .ok_or_else(|| TreeError::ObjectNotFound(*child_id))?;
                    collect_paths(store, &child, &child_path, max_depth, depth + 1, paths)?;
                }
            }
        },
    }
    Ok(())
}

/// Search state values for a query string. Returns matching paths + values.
pub fn tree_search_values(
    store: &dyn ObjectStore,
    root: &ObjectId,
    query: &str,
    max_results: usize,
) -> Result<Vec<(String, String)>, TreeError> {
    let root_obj = store
        .get_object(root)?
        .ok_or_else(|| TreeError::ObjectNotFound(*root))?;

    let query_lower = query.to_lowercase();
    let mut results = Vec::new();
    search_recursive(store, &root_obj, "", &query_lower, max_results, &mut results)?;
    Ok(results)
}

fn search_recursive(
    store: &dyn ObjectStore,
    obj: &Object,
    current_path: &str,
    query: &str,
    max_results: usize,
    results: &mut Vec<(String, String)>,
) -> Result<(), TreeError> {
    if results.len() >= max_results {
        return Ok(());
    }

    match obj {
        Object::Atom(atom) => {
            let value_str = match atom {
                Atom::String(s) => s.clone(),
                Atom::Int(i) => i.to_string(),
                Atom::Float(f) => f.to_string(),
                Atom::Bool(b) => b.to_string(),
                _ => return Ok(()),
            };
            if value_str.to_lowercase().contains(query) {
                let path = if current_path.is_empty() { "/".to_string() } else { current_path.to_string() };
                results.push((path, value_str));
            }
        }
        Object::Node(node) => match node {
            Node::Map(entries) => {
                for (key, child_id) in entries {
                    if results.len() >= max_results { break; }
                    // Also match on key names
                    if key.to_lowercase().contains(query) {
                        let path = format!("{}/{}", current_path, key);
                        results.push((path.clone(), format!("[key match: {}]", key)));
                    }
                    let child = store.get_object(child_id)?
                        .ok_or_else(|| TreeError::ObjectNotFound(*child_id))?;
                    let child_path = format!("{}/{}", current_path, key);
                    search_recursive(store, &child, &child_path, query, max_results, results)?;
                }
            }
            Node::List(items) | Node::Set(items) => {
                for (i, child_id) in items.iter().enumerate() {
                    if results.len() >= max_results { break; }
                    let child = store.get_object(child_id)?
                        .ok_or_else(|| TreeError::ObjectNotFound(*child_id))?;
                    let child_path = format!("{}/{}", current_path, i);
                    search_recursive(store, &child, &child_path, query, max_results, results)?;
                }
            }
        },
    }
    Ok(())
}

/// Convert a serde_json::Value to Objects and store them.
/// Returns the root ObjectId.
pub fn json_to_tree(
    store: &dyn ObjectStore,
    value: &serde_json::Value,
) -> Result<ObjectId, TreeError> {
    let obj = json_to_object(store, value)?;
    let id = store.put_object(&obj)?;
    Ok(id)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn navigate(
    current: &Object,
    component: &PathComponent,
    store: &dyn ObjectStore,
    path: &StatePath,
    _depth: usize,
) -> Result<Object, TreeError> {
    match (current, component) {
        (Object::Node(Node::Map(entries)), PathComponent::Key(key)) => {
            let child_id = entries
                .get(key)
                .ok_or_else(|| TreeError::PathNotFound(path.to_string()))?;
            store
                .get_object(child_id)?
                .ok_or_else(|| TreeError::ObjectNotFound(*child_id))
        }
        (Object::Node(Node::List(items)), PathComponent::Index(idx)) => {
            let child_id = items.get(*idx).ok_or_else(|| TreeError::IndexOutOfBounds {
                index: *idx,
                length: items.len(),
            })?;
            store
                .get_object(child_id)?
                .ok_or_else(|| TreeError::ObjectNotFound(*child_id))
        }
        (Object::Node(Node::Map(_)), PathComponent::Index(_)) => Err(TreeError::TypeMismatch {
            path: path.to_string(),
            expected: "list".to_string(),
            found: "map".to_string(),
        }),
        (Object::Node(Node::List(_)), PathComponent::Key(_)) => Err(TreeError::TypeMismatch {
            path: path.to_string(),
            expected: "map".to_string(),
            found: "list".to_string(),
        }),
        (Object::Atom(_), _) => Err(TreeError::CannotNavigateAtom(path.to_string())),
        _ => Err(TreeError::PathNotFound(path.to_string())),
    }
}

fn set_recursive(
    store: &dyn ObjectStore,
    current: &Object,
    components: &[PathComponent],
    depth: usize,
    value_id: ObjectId,
) -> Result<Object, TreeError> {
    if depth >= components.len() {
        // We've consumed all path components — return the object at value_id
        return store
            .get_object(&value_id)?
            .ok_or_else(|| TreeError::ObjectNotFound(value_id));
    }

    let component = &components[depth];
    let is_last = depth == components.len() - 1;

    match (current, component) {
        (Object::Node(Node::Map(entries)), PathComponent::Key(key)) => {
            let child_value_id = if is_last {
                value_id
            } else {
                // Navigate deeper
                let child = if let Some(child_id) = entries.get(key) {
                    store
                        .get_object(child_id)?
                        .ok_or_else(|| TreeError::ObjectNotFound(*child_id))?
                } else {
                    // Path doesn't exist yet — create intermediate maps
                    Object::empty_map()
                };
                let new_child = set_recursive(store, &child, components, depth + 1, value_id)?;
                store.put_object(&new_child)?
            };

            let mut new_entries = entries.clone();
            new_entries.insert(key.clone(), child_value_id);
            Ok(Object::map(new_entries))
        }
        (Object::Node(Node::List(items)), PathComponent::Index(idx)) => {
            if *idx >= items.len() {
                return Err(TreeError::IndexOutOfBounds {
                    index: *idx,
                    length: items.len(),
                });
            }

            let child_value_id = if is_last {
                value_id
            } else {
                let child = store
                    .get_object(&items[*idx])?
                    .ok_or_else(|| TreeError::ObjectNotFound(items[*idx]))?;
                let new_child = set_recursive(store, &child, components, depth + 1, value_id)?;
                store.put_object(&new_child)?
            };

            let mut new_items = items.clone();
            new_items[*idx] = child_value_id;
            Ok(Object::list(new_items))
        }
        (Object::Atom(_), PathComponent::Key(key)) => {
            // Overwrite atom with a map containing the key
            // This supports creating nested paths from scratch
            let mut entries = BTreeMap::new();
            if is_last {
                entries.insert(key.clone(), value_id);
            } else {
                let child = Object::empty_map();
                let new_child = set_recursive(store, &child, components, depth + 1, value_id)?;
                let child_id = store.put_object(&new_child)?;
                entries.insert(key.clone(), child_id);
            }
            Ok(Object::map(entries))
        }
        _ => Err(TreeError::TypeMismatch {
            path: format!("at depth {}", depth),
            expected: "map or list".to_string(),
            found: "incompatible type".to_string(),
        }),
    }
}

fn delete_recursive(
    store: &dyn ObjectStore,
    current: &Object,
    components: &[PathComponent],
    depth: usize,
) -> Result<Object, TreeError> {
    let component = &components[depth];
    let is_last = depth == components.len() - 1;

    match (current, component) {
        (Object::Node(Node::Map(entries)), PathComponent::Key(key)) => {
            if !entries.contains_key(key) {
                return Err(TreeError::PathNotFound(format!("key '{}' not found", key)));
            }

            if is_last {
                let mut new_entries = entries.clone();
                new_entries.remove(key);
                Ok(Object::map(new_entries))
            } else {
                let child_id = entries.get(key).unwrap();
                let child = store
                    .get_object(child_id)?
                    .ok_or_else(|| TreeError::ObjectNotFound(*child_id))?;
                let new_child = delete_recursive(store, &child, components, depth + 1)?;
                let new_child_id = store.put_object(&new_child)?;
                let mut new_entries = entries.clone();
                new_entries.insert(key.clone(), new_child_id);
                Ok(Object::map(new_entries))
            }
        }
        (Object::Node(Node::List(items)), PathComponent::Index(idx)) => {
            if *idx >= items.len() {
                return Err(TreeError::IndexOutOfBounds {
                    index: *idx,
                    length: items.len(),
                });
            }

            if is_last {
                let mut new_items = items.clone();
                new_items.remove(*idx);
                Ok(Object::list(new_items))
            } else {
                let child = store
                    .get_object(&items[*idx])?
                    .ok_or_else(|| TreeError::ObjectNotFound(items[*idx]))?;
                let new_child = delete_recursive(store, &child, components, depth + 1)?;
                let new_child_id = store.put_object(&new_child)?;
                let mut new_items = items.clone();
                new_items[*idx] = new_child_id;
                Ok(Object::list(new_items))
            }
        }
        _ => Err(TreeError::PathNotFound(format!(
            "cannot navigate at depth {}",
            depth
        ))),
    }
}

fn atom_to_json(atom: &Atom) -> serde_json::Value {
    match atom {
        Atom::Null => serde_json::Value::Null,
        Atom::Bool(b) => serde_json::Value::Bool(*b),
        Atom::Int(i) => serde_json::json!(*i),
        Atom::Float(f) => serde_json::json!(*f),
        Atom::String(s) => serde_json::Value::String(s.clone()),
        Atom::Bytes(b) => {
            use serde_json::Value;
            // Encode bytes as base64 string
            Value::String(format!("base64:{}", base64_encode(b)))
        }
    }
}

fn json_to_object(store: &dyn ObjectStore, value: &serde_json::Value) -> Result<Object, TreeError> {
    match value {
        serde_json::Value::Null => Ok(Object::null()),
        serde_json::Value::Bool(b) => Ok(Object::bool(*b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(Object::int(i))
            } else if let Some(f) = n.as_f64() {
                Ok(Object::float(f))
            } else {
                Err(TreeError::InvalidJson(
                    "unsupported number type".to_string(),
                ))
            }
        }
        serde_json::Value::String(s) => Ok(Object::string(s.clone())),
        serde_json::Value::Array(arr) => {
            let mut ids = Vec::new();
            for item in arr {
                let child = json_to_object(store, item)?;
                let id = store.put_object(&child)?;
                ids.push(id);
            }
            Ok(Object::list(ids))
        }
        serde_json::Value::Object(map) => {
            let mut entries = BTreeMap::new();
            for (key, val) in map {
                let child = json_to_object(store, val)?;
                let id = store.put_object(&child)?;
                entries.insert(key.clone(), id);
            }
            Ok(Object::map(entries))
        }
    }
}

/// Simple base64 encoding without external dependency.
fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum TreeError {
    #[error("object not found: {0}")]
    ObjectNotFound(ObjectId),

    #[error("path not found: {0}")]
    PathNotFound(String),

    #[error("index {index} out of bounds (length {length})")]
    IndexOutOfBounds { index: usize, length: usize },

    #[error("type mismatch at {path}: expected {expected}, found {found}")]
    TypeMismatch {
        path: String,
        expected: String,
        found: String,
    },

    #[error("cannot navigate into atom at: {0}")]
    CannotNavigateAtom(String),

    #[error("invalid JSON: {0}")]
    InvalidJson(String),

    #[error("storage error: {0}")]
    Storage(#[from] StorageError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentstategraph_storage::MemoryStorage;

    fn setup() -> (MemoryStorage, ObjectId) {
        let store = MemoryStorage::new();

        // Build a test state tree:
        // {
        //   "name": "test-cluster",
        //   "nodes": [
        //     { "hostname": "jetson-01", "status": "healthy" },
        //     { "hostname": "jetson-02", "status": "unhealthy" }
        //   ],
        //   "config": { "network": { "subnet": "10.0.0.0/24" } }
        // }
        let root_id = json_to_tree(
            &store,
            &serde_json::json!({
                "name": "test-cluster",
                "nodes": [
                    { "hostname": "jetson-01", "status": "healthy" },
                    { "hostname": "jetson-02", "status": "unhealthy" }
                ],
                "config": {
                    "network": { "subnet": "10.0.0.0/24" }
                }
            }),
        )
        .unwrap();

        (store, root_id)
    }

    #[test]
    fn test_get_root() {
        let (store, root_id) = setup();
        let root = tree_get(&store, &root_id, &StatePath::root()).unwrap();
        assert!(matches!(root, Object::Node(Node::Map(_))));
    }

    #[test]
    fn test_get_simple_key() {
        let (store, root_id) = setup();
        let path = StatePath::parse("/name").unwrap();
        let obj = tree_get(&store, &root_id, &path).unwrap();
        assert_eq!(obj, Object::string("test-cluster"));
    }

    #[test]
    fn test_get_nested_path() {
        let (store, root_id) = setup();
        let path = StatePath::parse("/config/network/subnet").unwrap();
        let obj = tree_get(&store, &root_id, &path).unwrap();
        assert_eq!(obj, Object::string("10.0.0.0/24"));
    }

    #[test]
    fn test_get_array_index() {
        let (store, root_id) = setup();
        let path = StatePath::parse("/nodes/0/hostname").unwrap();
        let obj = tree_get(&store, &root_id, &path).unwrap();
        assert_eq!(obj, Object::string("jetson-01"));
    }

    #[test]
    fn test_get_nonexistent_path() {
        let (store, root_id) = setup();
        let path = StatePath::parse("/nonexistent").unwrap();
        assert!(tree_get(&store, &root_id, &path).is_err());
    }

    #[test]
    fn test_set_existing_value() {
        let (store, root_id) = setup();
        let path = StatePath::parse("/nodes/1/status").unwrap();

        let new_root = tree_set(&store, &root_id, &path, &Object::string("healthy")).unwrap();

        // Verify the change
        let obj = tree_get(&store, &new_root, &path).unwrap();
        assert_eq!(obj, Object::string("healthy"));

        // Verify old root is unchanged (immutability)
        let old_obj = tree_get(&store, &root_id, &path).unwrap();
        assert_eq!(old_obj, Object::string("unhealthy"));
    }

    #[test]
    fn test_set_new_key() {
        let (store, root_id) = setup();
        let path = StatePath::parse("/version").unwrap();

        let new_root = tree_set(&store, &root_id, &path, &Object::string("1.0")).unwrap();

        let obj = tree_get(&store, &new_root, &path).unwrap();
        assert_eq!(obj, Object::string("1.0"));

        // Original keys still accessible
        let name = tree_get(&store, &new_root, &StatePath::parse("/name").unwrap()).unwrap();
        assert_eq!(name, Object::string("test-cluster"));
    }

    #[test]
    fn test_set_creates_intermediate_maps() {
        let (store, root_id) = setup();
        let path = StatePath::parse("/new/deeply/nested/value").unwrap();

        let new_root = tree_set(&store, &root_id, &path, &Object::int(42)).unwrap();

        let obj = tree_get(&store, &new_root, &path).unwrap();
        assert_eq!(obj, Object::int(42));
    }

    #[test]
    fn test_structural_sharing() {
        let (store, root_id) = setup();

        // Modify only /name — /config subtree should be shared
        let path = StatePath::parse("/name").unwrap();
        let new_root = tree_set(&store, &root_id, &path, &Object::string("new-name")).unwrap();

        // Get the config ObjectId from both roots — should be the same
        let old_root_obj = store.get_object(&root_id).unwrap().unwrap();
        let new_root_obj = store.get_object(&new_root).unwrap().unwrap();

        if let (Object::Node(Node::Map(old_entries)), Object::Node(Node::Map(new_entries))) =
            (&old_root_obj, &new_root_obj)
        {
            assert_eq!(
                old_entries.get("config"),
                new_entries.get("config"),
                "unmodified subtrees should share the same ObjectId (structural sharing)"
            );
            assert_ne!(
                old_entries.get("name"),
                new_entries.get("name"),
                "modified values should have different ObjectIds"
            );
        } else {
            panic!("roots should be maps");
        }
    }

    #[test]
    fn test_delete_key() {
        let (store, root_id) = setup();
        let path = StatePath::parse("/name").unwrap();

        let new_root = tree_delete(&store, &root_id, &path).unwrap();

        assert!(tree_get(&store, &new_root, &path).is_err());

        // Other keys still accessible
        let subnet = tree_get(
            &store,
            &new_root,
            &StatePath::parse("/config/network/subnet").unwrap(),
        )
        .unwrap();
        assert_eq!(subnet, Object::string("10.0.0.0/24"));
    }

    #[test]
    fn test_delete_array_element() {
        let (store, root_id) = setup();
        let path = StatePath::parse("/nodes/0").unwrap();

        let new_root = tree_delete(&store, &root_id, &path).unwrap();

        // After deleting index 0, old index 1 is now at index 0
        let hostname = tree_get(
            &store,
            &new_root,
            &StatePath::parse("/nodes/0/hostname").unwrap(),
        )
        .unwrap();
        assert_eq!(hostname, Object::string("jetson-02"));
    }

    #[test]
    fn test_json_roundtrip() {
        let (store, root_id) = setup();
        let root = tree_get(&store, &root_id, &StatePath::root()).unwrap();
        let json = tree_to_json(&store, &root).unwrap();

        // Re-import
        let new_root_id = json_to_tree(&store, &json).unwrap();

        // Same content should produce same ObjectId
        assert_eq!(
            root_id, new_root_id,
            "JSON roundtrip should produce same ObjectId"
        );
    }
}
