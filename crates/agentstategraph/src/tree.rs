//! State tree operations — get, set, delete values by path.
//!
//! The state tree is a Merkle DAG of Objects. Modifications create new
//! objects along the modified path (structural sharing), leaving all
//! other subtrees untouched.

use std::collections::{BTreeMap, HashMap};

use agentstategraph_core::{Atom, Node, Object, ObjectId, PathComponent, StatePath};
use agentstategraph_storage::{ObjectStore, StorageError};

/// The most objects one `batch_get_objects` call asks for.
const PREFETCH_BATCH: usize = 1000;

/// A subtree's objects, read a level at a time with `batch_get_objects`, so
/// that walking it costs one storage call per level instead of one per object.
/// On Postgres that is the difference between a few queries and one per node,
/// which for a large plan or memory tree is thousands. An object it doesn't
/// hold (below the levels loaded) comes from the backing store, so a walk over
/// it is always complete; the levels only decide how much is fetched up front.
struct Prefetched<'a> {
    store: &'a dyn ObjectStore,
    objects: HashMap<ObjectId, Object>,
}

impl<'a> Prefetched<'a> {
    /// Load the objects in the `levels` levels below `root`.
    fn load(store: &'a dyn ObjectStore, root: &Object, levels: usize) -> Result<Self, TreeError> {
        Self::load_many(store, &[root], levels)
    }

    /// Load the objects in the `levels` levels below each of `roots`, the
    /// same level of every root in one batch.
    fn load_many(
        store: &'a dyn ObjectStore,
        roots: &[&Object],
        levels: usize,
    ) -> Result<Self, TreeError> {
        let mut objects: HashMap<ObjectId, Object> = HashMap::new();
        let mut frontier: Vec<ObjectId> = roots.iter().flat_map(|r| child_ids(r)).collect();
        let mut level = 0;
        while !frontier.is_empty() && level < levels {
            level += 1;
            // A subtree shared by several parents is loaded once.
            let mut seen = std::collections::HashSet::new();
            frontier.retain(|id| !objects.contains_key(id) && seen.insert(*id));
            let mut next = Vec::new();
            for chunk in frontier.chunks(PREFETCH_BATCH) {
                for (id, obj) in chunk.iter().zip(store.batch_get_objects(chunk)?) {
                    if let Some(obj) = obj {
                        next.extend(child_ids(&obj));
                        objects.insert(*id, obj);
                    }
                }
            }
            frontier = next;
        }
        Ok(Self { store, objects })
    }
}

impl ObjectStore for Prefetched<'_> {
    fn get_object(&self, id: &ObjectId) -> Result<Option<Object>, StorageError> {
        match self.objects.get(id) {
            Some(obj) => Ok(Some(obj.clone())),
            None => self.store.get_object(id),
        }
    }

    fn put_object(&self, obj: &Object) -> Result<ObjectId, StorageError> {
        self.store.put_object(obj)
    }

    fn has_object(&self, id: &ObjectId) -> Result<bool, StorageError> {
        Ok(self.objects.contains_key(id) || self.store.has_object(id)?)
    }
}

/// The ids a node points at, in order; none for an atom.
fn child_ids(obj: &Object) -> Vec<ObjectId> {
    match obj {
        Object::Atom(_) => Vec::new(),
        Object::Node(Node::Map(entries)) => entries.values().copied().collect(),
        Object::Node(Node::List(items) | Node::Set(items)) => items.clone(),
    }
}

/// Absolute ceiling on how deep `tree_list_paths` / `tree_search_values`
/// will ever recurse, regardless of the caller-supplied `max_depth`.
///
/// Set to twice `agentstategraph_core::MAX_PATH_DEPTH` (64 → 128) so we
/// can still enumerate legacy data that was written by older ASG versions
/// without the current path-depth cap. A caller passing `usize::MAX` is
/// clamped to this value, preventing unbounded traversal / OOM.
/// (security threat model v2, F2)
pub const LIST_PATHS_HARD_CEILING: usize = 128;

/// Absolute cap on the number of results returned by `tree_list_paths`
/// and `tree_search_values`. Once this many entries have been collected,
/// traversal short-circuits and the extras are dropped. Callers receive
/// the truncated vec without an explicit marker — keep the API simple.
/// (security threat model v2, F2)
pub const LIST_PATHS_MAX_RESULTS: usize = 10_000;

/// Read a value from the state tree at the given path.
/// Returns the Object at that path, or an error if the path doesn't exist.
pub fn tree_get(
    store: &dyn ObjectStore,
    root: &ObjectId,
    path: &StatePath,
) -> Result<Object, TreeError> {
    let root_obj = store
        .get_object(root)?
        .ok_or(TreeError::ObjectNotFound(*root))?;

    if path.is_root() {
        return Ok(root_obj);
    }

    let mut current = root_obj;
    for (i, component) in path.components().iter().enumerate() {
        current = navigate(&current, component, store, path, i)?;
    }

    Ok(current)
}

/// [`tree_get`] for several paths at once: the objects along all of them
/// are fetched a level at a time, one batch per level, so reading many
/// paths costs about as many storage calls as reading the deepest one.
///
/// `None` for a path that doesn't exist: a missing key or index, or a path
/// that runs into an atom or a container of the wrong kind.
pub fn tree_get_many(
    store: &dyn ObjectStore,
    root: &ObjectId,
    paths: &[StatePath],
) -> Result<Vec<Option<Object>>, TreeError> {
    let root_obj = store
        .get_object(root)?
        .ok_or(TreeError::ObjectNotFound(*root))?;
    let mut current: Vec<Option<Object>> = vec![Some(root_obj); paths.len()];
    let deepest = paths
        .iter()
        .map(|p| p.components().len())
        .max()
        .unwrap_or(0);
    for level in 0..deepest {
        // The child each path still being walked wants at this level.
        let mut wanted: Vec<(usize, ObjectId)> = Vec::new();
        for (i, path) in paths.iter().enumerate() {
            let Some(component) = path.components().get(level) else {
                continue;
            };
            match current[i]
                .as_ref()
                .and_then(|obj| child_id_at(obj, component))
            {
                Some(id) => wanted.push((i, id)),
                None => current[i] = None,
            }
        }
        let mut seen = std::collections::HashSet::new();
        let ids: Vec<ObjectId> = wanted
            .iter()
            .map(|(_, id)| *id)
            .filter(|id| seen.insert(*id))
            .collect();
        let mut fetched: HashMap<ObjectId, Object> = HashMap::with_capacity(ids.len());
        for chunk in ids.chunks(PREFETCH_BATCH) {
            for (id, obj) in chunk.iter().zip(store.batch_get_objects(chunk)?) {
                fetched.insert(*id, obj.ok_or(TreeError::ObjectNotFound(*id))?);
            }
        }
        for (i, id) in wanted {
            current[i] = fetched.get(&id).cloned();
        }
    }
    Ok(current)
}

/// The id a path component names in `obj`, by [`navigate`]'s rules: a key
/// into a map, an index into a list, nothing else.
fn child_id_at(obj: &Object, component: &PathComponent) -> Option<ObjectId> {
    match (obj, component) {
        (Object::Node(Node::Map(entries)), PathComponent::Key(key)) => entries.get(key).copied(),
        (Object::Node(Node::List(items)), PathComponent::Index(idx)) => items.get(*idx).copied(),
        _ => None,
    }
}

/// [`tree_to_json`] for several objects, their subtrees read together a
/// level at a time.
pub fn trees_to_json(
    store: &dyn ObjectStore,
    objs: &[Option<Object>],
) -> Result<Vec<Option<serde_json::Value>>, TreeError> {
    let roots: Vec<&Object> = objs.iter().flatten().collect();
    let prefetched = Prefetched::load_many(store, &roots, usize::MAX)?;
    objs.iter()
        .map(|obj| obj.as_ref().map(|o| to_json(&prefetched, o)).transpose())
        .collect()
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
        .ok_or(TreeError::ObjectNotFound(*root))?;

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
        .ok_or(TreeError::ObjectNotFound(*root))?;

    let new_root = delete_recursive(store, &root_obj, path.components(), 0)?;
    let new_root_id = store.put_object(&new_root)?;
    Ok(new_root_id)
}

/// Convert an Object to a serde_json::Value for user-facing output.
/// Recursively resolves all ObjectId references, reading the subtree a level
/// at a time.
pub fn tree_to_json(store: &dyn ObjectStore, obj: &Object) -> Result<serde_json::Value, TreeError> {
    let prefetched = Prefetched::load(store, obj, usize::MAX)?;
    to_json(&prefetched, obj)
}

fn to_json(store: &dyn ObjectStore, obj: &Object) -> Result<serde_json::Value, TreeError> {
    match obj {
        Object::Atom(atom) => Ok(atom_to_json(atom)),
        Object::Node(node) => match node {
            Node::Map(entries) => {
                let mut map = serde_json::Map::new();
                for (key, child_id) in entries {
                    let child = store
                        .get_object(child_id)?
                        .ok_or(TreeError::ObjectNotFound(*child_id))?;
                    map.insert(key.clone(), to_json(store, &child)?);
                }
                Ok(serde_json::Value::Object(map))
            }
            Node::List(items) => {
                let mut arr = Vec::new();
                for child_id in items {
                    let child = store
                        .get_object(child_id)?
                        .ok_or(TreeError::ObjectNotFound(*child_id))?;
                    arr.push(to_json(store, &child)?);
                }
                Ok(serde_json::Value::Array(arr))
            }
            Node::Set(items) => {
                let mut arr = Vec::new();
                for child_id in items {
                    let child = store
                        .get_object(child_id)?
                        .ok_or(TreeError::ObjectNotFound(*child_id))?;
                    arr.push(to_json(store, &child)?);
                }
                Ok(serde_json::Value::Array(arr))
            }
        },
    }
}

/// Like [`tree_to_json`] but stops at `max_depth` levels below `obj`: a Node at
/// the depth limit is rendered as a compact `{ "_truncated": true, ... }`
/// placeholder (its child keys, or length) instead of being recursed into, so
/// deep subtrees are never loaded or materialized. Used for cheap shallow reads
/// of a large ref (plan perf-slow-endpoints t-007). Depth is counted from
/// `obj`: `max_depth = 0` summarizes a Node immediately; an Atom is always
/// returned as its value.
pub fn tree_to_json_capped(
    store: &dyn ObjectStore,
    obj: &Object,
    max_depth: usize,
) -> Result<serde_json::Value, TreeError> {
    // Nodes at `max_depth` are summarized from themselves, so levels
    // 1..=max_depth are all the walk reads.
    let prefetched = Prefetched::load(store, obj, max_depth)?;
    to_json_capped(&prefetched, obj, max_depth, 0)
}

fn to_json_capped(
    store: &dyn ObjectStore,
    obj: &Object,
    max_depth: usize,
    depth: usize,
) -> Result<serde_json::Value, TreeError> {
    match obj {
        Object::Atom(atom) => Ok(atom_to_json(atom)),
        Object::Node(node) => {
            if depth >= max_depth {
                // Boundary: summarize from the node itself — its keys/length are
                // in hand, so we never load (or serialize) any child object.
                let summary = match node {
                    Node::Map(entries) => serde_json::json!({
                        "_truncated": true,
                        "_keys": entries.keys().cloned().collect::<Vec<_>>(),
                    }),
                    Node::List(items) | Node::Set(items) => serde_json::json!({
                        "_truncated": true,
                        "_len": items.len(),
                    }),
                };
                return Ok(summary);
            }
            match node {
                Node::Map(entries) => {
                    let mut map = serde_json::Map::new();
                    for (key, child_id) in entries {
                        let child = store
                            .get_object(child_id)?
                            .ok_or(TreeError::ObjectNotFound(*child_id))?;
                        map.insert(
                            key.clone(),
                            to_json_capped(store, &child, max_depth, depth + 1)?,
                        );
                    }
                    Ok(serde_json::Value::Object(map))
                }
                Node::List(items) | Node::Set(items) => {
                    let mut arr = Vec::new();
                    for child_id in items {
                        let child = store
                            .get_object(child_id)?
                            .ok_or(TreeError::ObjectNotFound(*child_id))?;
                        arr.push(to_json_capped(store, &child, max_depth, depth + 1)?);
                    }
                    Ok(serde_json::Value::Array(arr))
                }
            }
        }
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
        .ok_or(TreeError::ObjectNotFound(*root))?;

    // If prefix is non-empty, navigate to the subtree first
    let (start_obj, base_path) = if prefix.is_empty() || prefix == "/" {
        (root_obj, String::new())
    } else {
        let path = StatePath::parse(prefix).map_err(|e| TreeError::PathNotFound(e.to_string()))?;
        let obj = tree_get(store, root, &path)?;
        (obj, prefix.to_string())
    };

    // Clamp the caller-supplied depth to the hard ceiling so `usize::MAX`
    // cannot produce unbounded recursion. See LIST_PATHS_HARD_CEILING.
    let depth = max_depth.min(LIST_PATHS_HARD_CEILING);
    let mut paths = Vec::new();
    // The walk loads children down to depth + 1 before it stops.
    let prefetched = Prefetched::load(store, &start_obj, depth + 1)?;
    collect_paths(&prefetched, &start_obj, &base_path, depth, 0, &mut paths)?;
    // Truncate to the absolute result cap — a malicious/wide tree cannot
    // make us materialize more than LIST_PATHS_MAX_RESULTS entries.
    if paths.len() > LIST_PATHS_MAX_RESULTS {
        paths.truncate(LIST_PATHS_MAX_RESULTS);
    }
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
    if paths.len() >= LIST_PATHS_MAX_RESULTS {
        return Ok(());
    }

    match obj {
        Object::Atom(_) => {
            let path = if current_path.is_empty() {
                "/".to_string()
            } else {
                current_path.to_string()
            };
            paths.push(path);
        }
        Object::Node(node) => match node {
            Node::Map(entries) => {
                if entries.is_empty() {
                    let path = if current_path.is_empty() {
                        "/".to_string()
                    } else {
                        current_path.to_string()
                    };
                    paths.push(path);
                } else {
                    for (key, child_id) in entries {
                        let child_path = format!("{}/{}", current_path, key);
                        let child = store
                            .get_object(child_id)?
                            .ok_or(TreeError::ObjectNotFound(*child_id))?;
                        collect_paths(store, &child, &child_path, max_depth, depth + 1, paths)?;
                    }
                }
            }
            Node::List(items) => {
                for (i, child_id) in items.iter().enumerate() {
                    let child_path = format!("{}/{}", current_path, i);
                    let child = store
                        .get_object(child_id)?
                        .ok_or(TreeError::ObjectNotFound(*child_id))?;
                    collect_paths(store, &child, &child_path, max_depth, depth + 1, paths)?;
                }
            }
            Node::Set(items) => {
                for (i, child_id) in items.iter().enumerate() {
                    let child_path = format!("{}/{}", current_path, i);
                    let child = store
                        .get_object(child_id)?
                        .ok_or(TreeError::ObjectNotFound(*child_id))?;
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
        .ok_or(TreeError::ObjectNotFound(*root))?;

    let query_lower = query.to_lowercase();
    // Clamp the caller's max_results to LIST_PATHS_MAX_RESULTS so a huge
    // query can't force us to materialize an unbounded result set.
    let limit = max_results.min(LIST_PATHS_MAX_RESULTS);
    let mut results = Vec::new();
    let prefetched = Prefetched::load(store, &root_obj, LIST_PATHS_HARD_CEILING + 1)?;
    search_recursive(
        &prefetched,
        &root_obj,
        "",
        &query_lower,
        limit,
        0,
        &mut results,
    )?;
    Ok(results)
}

fn search_recursive(
    store: &dyn ObjectStore,
    obj: &Object,
    current_path: &str,
    query: &str,
    max_results: usize,
    depth: usize,
    results: &mut Vec<(String, String)>,
) -> Result<(), TreeError> {
    if results.len() >= max_results {
        return Ok(());
    }
    // Hard ceiling on traversal depth — mirrors tree_list_paths so a
    // pathologically deep or cyclic-seeming graph can't infinite-loop.
    if depth > LIST_PATHS_HARD_CEILING {
        return Ok(());
    }

    match obj {
        Object::Atom(atom) => {
            let value_str = match atom {
                Atom::String(s) => s.clone(),
                Atom::Int(i) => i.to_string(),
                Atom::UInt(u) => u.to_string(),
                Atom::Float(f) => f.to_string(),
                Atom::Bool(b) => b.to_string(),
                _ => return Ok(()),
            };
            if value_str.to_lowercase().contains(query) {
                let path = if current_path.is_empty() {
                    "/".to_string()
                } else {
                    current_path.to_string()
                };
                results.push((path, value_str));
            }
        }
        Object::Node(node) => match node {
            Node::Map(entries) => {
                for (key, child_id) in entries {
                    if results.len() >= max_results {
                        break;
                    }
                    // Also match on key names
                    if key.to_lowercase().contains(query) {
                        let path = format!("{}/{}", current_path, key);
                        results.push((path.clone(), format!("[key match: {}]", key)));
                    }
                    let child = store
                        .get_object(child_id)?
                        .ok_or(TreeError::ObjectNotFound(*child_id))?;
                    let child_path = format!("{}/{}", current_path, key);
                    search_recursive(
                        store,
                        &child,
                        &child_path,
                        query,
                        max_results,
                        depth + 1,
                        results,
                    )?;
                }
            }
            Node::List(items) | Node::Set(items) => {
                for (i, child_id) in items.iter().enumerate() {
                    if results.len() >= max_results {
                        break;
                    }
                    let child = store
                        .get_object(child_id)?
                        .ok_or(TreeError::ObjectNotFound(*child_id))?;
                    let child_path = format!("{}/{}", current_path, i);
                    search_recursive(
                        store,
                        &child,
                        &child_path,
                        query,
                        max_results,
                        depth + 1,
                        results,
                    )?;
                }
            }
        },
    }
    Ok(())
}

/// Collect EVERY `(path, value)` string leaf of the tree at `root`, with no
/// query filter and no result cap — the one-time full walk that seeds the
/// leaf-value index (plan perf-slow-endpoints t-006). Unlike
/// [`tree_search_values`] it does not emit key-name matches; the index is over
/// values only. Depth is bounded by [`LIST_PATHS_HARD_CEILING`], mirroring the
/// other tree walks.
pub fn tree_collect_leaves(
    store: &dyn ObjectStore,
    root: &ObjectId,
) -> Result<Vec<(String, String)>, TreeError> {
    let root_obj = store
        .get_object(root)?
        .ok_or(TreeError::ObjectNotFound(*root))?;
    let mut out = Vec::new();
    collect_leaves(store, &root_obj, "", 0, &mut out)?;
    Ok(out)
}

fn collect_leaves_recursive(
    store: &dyn ObjectStore,
    obj: &Object,
    current_path: &str,
    depth: usize,
    out: &mut Vec<(String, String)>,
) -> Result<(), TreeError> {
    if depth > LIST_PATHS_HARD_CEILING {
        return Ok(());
    }
    match obj {
        Object::Atom(atom) => {
            let value_str = match atom {
                Atom::String(s) => s.clone(),
                Atom::Int(i) => i.to_string(),
                Atom::UInt(u) => u.to_string(),
                Atom::Float(f) => f.to_string(),
                Atom::Bool(b) => b.to_string(),
                _ => return Ok(()),
            };
            let path = if current_path.is_empty() {
                "/".to_string()
            } else {
                current_path.to_string()
            };
            out.push((path, value_str));
        }
        Object::Node(node) => match node {
            Node::Map(entries) => {
                for (key, child_id) in entries {
                    let child = store
                        .get_object(child_id)?
                        .ok_or(TreeError::ObjectNotFound(*child_id))?;
                    let child_path = format!("{}/{}", current_path, key);
                    collect_leaves_recursive(store, &child, &child_path, depth + 1, out)?;
                }
            }
            Node::List(items) | Node::Set(items) => {
                for (i, child_id) in items.iter().enumerate() {
                    let child = store
                        .get_object(child_id)?
                        .ok_or(TreeError::ObjectNotFound(*child_id))?;
                    let child_path = format!("{}/{}", current_path, i);
                    collect_leaves_recursive(store, &child, &child_path, depth + 1, out)?;
                }
            }
        },
    }
    Ok(())
}

/// Stringify an atom the way the value index/search does. `None` for atom
/// kinds that are not indexed (Null, Bytes).
fn atom_value_string(atom: &Atom) -> Option<String> {
    match atom {
        Atom::String(s) => Some(s.clone()),
        Atom::Int(i) => Some(i.to_string()),
        Atom::UInt(u) => Some(u.to_string()),
        Atom::Float(f) => Some(f.to_string()),
        Atom::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// [`collect_leaves_recursive`] over `obj`'s subtree, read a level at a time.
fn collect_leaves(
    store: &dyn ObjectStore,
    obj: &Object,
    current_path: &str,
    depth: usize,
    out: &mut Vec<(String, String)>,
) -> Result<(), TreeError> {
    let levels = (LIST_PATHS_HARD_CEILING + 1).saturating_sub(depth);
    let prefetched = Prefetched::load(store, obj, levels)?;
    collect_leaves_recursive(&prefetched, obj, current_path, depth, out)
}

/// [`collect_leaf_paths`] over `obj`'s subtree, read a level at a time.
fn collect_paths_of(
    store: &dyn ObjectStore,
    obj: &Object,
    current_path: &str,
    depth: usize,
    out: &mut Vec<String>,
) -> Result<(), TreeError> {
    let levels = (LIST_PATHS_HARD_CEILING + 1).saturating_sub(depth);
    let prefetched = Prefetched::load(store, obj, levels)?;
    collect_leaf_paths(&prefetched, obj, current_path, depth, out)
}

/// Collect just the leaf PATHS under `obj` (for removals during a diff).
fn collect_leaf_paths(
    store: &dyn ObjectStore,
    obj: &Object,
    current_path: &str,
    depth: usize,
    out: &mut Vec<String>,
) -> Result<(), TreeError> {
    if depth > LIST_PATHS_HARD_CEILING {
        return Ok(());
    }
    match obj {
        Object::Atom(atom) => {
            if atom_value_string(atom).is_some() {
                out.push(if current_path.is_empty() {
                    "/".to_string()
                } else {
                    current_path.to_string()
                });
            }
        }
        Object::Node(node) => {
            let children: Vec<(String, ObjectId)> = match node {
                Node::Map(entries) => entries
                    .iter()
                    .map(|(k, id)| (format!("{}/{}", current_path, k), *id))
                    .collect(),
                Node::List(items) | Node::Set(items) => items
                    .iter()
                    .enumerate()
                    .map(|(i, id)| (format!("{}/{}", current_path, i), *id))
                    .collect(),
            };
            for (child_path, child_id) in children {
                let child = store
                    .get_object(&child_id)?
                    .ok_or(TreeError::ObjectNotFound(child_id))?;
                collect_leaf_paths(store, &child, &child_path, depth + 1, out)?;
            }
        }
    }
    Ok(())
}

/// Diff the string leaves of two trees, returning `(removed_paths, added
/// (path, value))` for the transition `old_root -> new_root`. Content-addressed:
/// a subtree whose ObjectId is unchanged is skipped whole, so the cost is
/// proportional to what a commit actually changed — not the tree size. This is
/// what makes incremental index maintenance on the write path cheap. `old_root
/// = None` treats every leaf of `new_root` as added. See plan t-006.
pub fn tree_diff_leaves(
    store: &dyn ObjectStore,
    old_root: Option<&ObjectId>,
    new_root: &ObjectId,
) -> Result<(Vec<String>, Vec<(String, String)>), TreeError> {
    let mut removed = Vec::new();
    let mut added = Vec::new();
    diff_recursive(
        store,
        old_root.copied(),
        Some(*new_root),
        "",
        0,
        &mut removed,
        &mut added,
    )?;
    Ok((removed, added))
}

#[allow(clippy::too_many_arguments)]
fn diff_recursive(
    store: &dyn ObjectStore,
    old_id: Option<ObjectId>,
    new_id: Option<ObjectId>,
    path: &str,
    depth: usize,
    removed: &mut Vec<String>,
    added: &mut Vec<(String, String)>,
) -> Result<(), TreeError> {
    // Content-addressing: identical ids (or both absent) => identical subtree.
    if old_id == new_id {
        return Ok(());
    }
    if depth > LIST_PATHS_HARD_CEILING {
        return Ok(());
    }
    let old_obj = match old_id {
        Some(id) => store.get_object(&id)?,
        None => None,
    };
    let new_obj = match new_id {
        Some(id) => store.get_object(&id)?,
        None => None,
    };
    match (old_obj, new_obj) {
        (None, None) => {}
        (Some(o), None) => collect_paths_of(store, &o, path, depth, removed)?,
        (None, Some(n)) => collect_leaves(store, &n, path, depth, added)?,
        (Some(Object::Atom(_)), Some(Object::Atom(new_atom))) => {
            // Same path, changed value: drop the old row, add the new one.
            let leaf = if path.is_empty() { "/" } else { path }.to_string();
            removed.push(leaf.clone());
            if let Some(v) = atom_value_string(&new_atom) {
                added.push((leaf, v));
            }
        }
        (Some(o @ Object::Node(_)), Some(Object::Atom(new_atom))) => {
            collect_paths_of(store, &o, path, depth, removed)?;
            let leaf = if path.is_empty() { "/" } else { path }.to_string();
            if let Some(v) = atom_value_string(&new_atom) {
                added.push((leaf, v));
            }
        }
        (Some(Object::Atom(_)), Some(n @ Object::Node(_))) => {
            let leaf = if path.is_empty() { "/" } else { path }.to_string();
            removed.push(leaf);
            collect_leaves(store, &n, path, depth, added)?;
        }
        (Some(Object::Node(on)), Some(Object::Node(nn))) => match (on, nn) {
            (Node::Map(oe), Node::Map(ne)) => {
                let mut keys: std::collections::BTreeSet<&String> = oe.keys().collect();
                keys.extend(ne.keys());
                for k in keys {
                    let child_path = format!("{}/{}", path, k);
                    diff_recursive(
                        store,
                        oe.get(k).copied(),
                        ne.get(k).copied(),
                        &child_path,
                        depth + 1,
                        removed,
                        added,
                    )?;
                }
            }
            (Node::List(oi) | Node::Set(oi), Node::List(ni) | Node::Set(ni)) => {
                let max = oi.len().max(ni.len());
                for i in 0..max {
                    let child_path = format!("{}/{}", path, i);
                    diff_recursive(
                        store,
                        oi.get(i).copied(),
                        ni.get(i).copied(),
                        &child_path,
                        depth + 1,
                        removed,
                        added,
                    )?;
                }
            }
            // Container kind changed (map <-> sequence): treat as full replace.
            (old_node, new_node) => {
                collect_paths_of(store, &Object::Node(old_node), path, depth, removed)?;
                collect_leaves(store, &Object::Node(new_node), path, depth, added)?;
            }
        },
    }
    Ok(())
}

/// Convert a serde_json::Value to Objects and store them.
/// Returns the root ObjectId.
///
/// The objects are built in memory and stored with one `batch_put_objects`
/// call: one statement per thousand objects on Postgres, one transaction on
/// SQLite, rather than a round trip per node.
pub fn json_to_tree(
    store: &dyn ObjectStore,
    value: &serde_json::Value,
) -> Result<ObjectId, TreeError> {
    let mut objects = Vec::new();
    let root = json_to_objects(value, &mut objects);
    let id = root.id();
    objects.push(root);
    store.batch_put_objects(&objects)?;
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
                .ok_or(TreeError::ObjectNotFound(*child_id))
        }
        (Object::Node(Node::List(items)), PathComponent::Index(idx)) => {
            let child_id = items.get(*idx).ok_or(TreeError::IndexOutOfBounds {
                index: *idx,
                length: items.len(),
            })?;
            store
                .get_object(child_id)?
                .ok_or(TreeError::ObjectNotFound(*child_id))
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
            .ok_or(TreeError::ObjectNotFound(value_id));
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
                        .ok_or(TreeError::ObjectNotFound(*child_id))?
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
        (Object::Atom(Atom::Null), PathComponent::Key(key)) => {
            // A null is a placeholder: start the subtree there. Any other
            // value is data, and writing below it must not replace it — that
            // turned `/a = 30` plus a write to `/a/b` into `{"b": ...}` with
            // the 30 gone, and `Ok`.
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
        (Object::Atom(atom), _) => Err(TreeError::TypeMismatch {
            path: format!("at depth {}", depth),
            expected: "map or list".to_string(),
            found: format!("a {} value", atom_kind(atom)),
        }),
        _ => Err(TreeError::TypeMismatch {
            path: format!("at depth {}", depth),
            expected: "map or list".to_string(),
            found: "incompatible type".to_string(),
        }),
    }
}

fn atom_kind(atom: &Atom) -> &'static str {
    match atom {
        Atom::Null => "null",
        Atom::Bool(_) => "boolean",
        Atom::Int(_) | Atom::UInt(_) | Atom::Float(_) => "number",
        Atom::String(_) => "string",
        Atom::Bytes(_) => "bytes",
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
                    .ok_or(TreeError::ObjectNotFound(*child_id))?;
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
        Atom::UInt(u) => serde_json::json!(*u),
        Atom::Float(f) => serde_json::json!(*f),
        Atom::String(s) => serde_json::Value::String(s.clone()),
        Atom::Bytes(b) => {
            use serde_json::Value;
            // Encode bytes as base64 string
            Value::String(format!("base64:{}", base64_encode(b)))
        }
    }
}

/// `value` as an Object, pushing every object below it onto `out`, each
/// child before its parent.
fn json_to_objects(value: &serde_json::Value, out: &mut Vec<Object>) -> Object {
    let mut child = |value: &serde_json::Value| {
        let obj = json_to_objects(value, out);
        let id = obj.id();
        out.push(obj);
        id
    };
    match value {
        serde_json::Value::Null => Object::null(),
        serde_json::Value::Bool(b) => Object::bool(*b),
        serde_json::Value::Number(n) => Object::from_json_number(n),
        serde_json::Value::String(s) => Object::string(s.clone()),
        serde_json::Value::Array(arr) => Object::list(arr.iter().map(&mut child).collect()),
        serde_json::Value::Object(map) => Object::map(
            map.iter()
                .map(|(key, val)| (key.clone(), child(val)))
                .collect::<BTreeMap<_, _>>(),
        ),
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

impl TreeError {
    /// A stable code for this kind of error; see `RepoError::code`.
    pub fn code(&self) -> &'static str {
        match self {
            TreeError::ObjectNotFound(_) => "object_not_found",
            TreeError::PathNotFound(_) => "path_not_found",
            TreeError::IndexOutOfBounds { .. } => "index_out_of_bounds",
            TreeError::TypeMismatch { .. } => "type_mismatch",
            TreeError::CannotNavigateAtom(_) => "not_a_container",
            TreeError::InvalidJson(_) => "invalid_json",
            TreeError::Storage(e) => e.code(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentstategraph_storage::SqliteStorage;

    fn setup() -> (SqliteStorage, ObjectId) {
        let store = SqliteStorage::in_memory().expect("in-memory sqlite");

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
    fn test_list_paths_truncates_at_max_results() {
        let store = SqliteStorage::in_memory().expect("in-memory sqlite");
        // Build a wide tree: a single map with > LIST_PATHS_MAX_RESULTS keys.
        let mut obj = serde_json::Map::new();
        let wide = LIST_PATHS_MAX_RESULTS + 500;
        for i in 0..wide {
            obj.insert(format!("k{}", i), serde_json::Value::from(i as i64));
        }
        let root_id = json_to_tree(&store, &serde_json::Value::Object(obj)).unwrap();

        let paths = tree_list_paths(&store, &root_id, "/", 10).unwrap();
        assert_eq!(
            paths.len(),
            LIST_PATHS_MAX_RESULTS,
            "list_paths must cap results at LIST_PATHS_MAX_RESULTS"
        );
    }

    #[test]
    fn test_list_paths_clamps_usize_max_depth() {
        // A usize::MAX max_depth used to drive unbounded recursion; now
        // it must clamp to LIST_PATHS_HARD_CEILING and return promptly.
        let (store, root_id) = setup();
        let paths = tree_list_paths(&store, &root_id, "/", usize::MAX).unwrap();
        // Just assert we got here without panicking / OOMing, and that we
        // returned a plausible, bounded result set.
        assert!(!paths.is_empty());
        assert!(paths.len() <= LIST_PATHS_MAX_RESULTS);
    }

    #[test]
    fn test_to_json_capped_truncates_nodes_at_depth() {
        let (store, root_id) = setup();
        let root = tree_get(&store, &root_id, &StatePath::root()).unwrap();
        let capped = tree_to_json_capped(&store, &root, 1).unwrap();

        // Atom child is returned as-is.
        assert_eq!(capped["name"], "test-cluster");
        // A List child at the cap becomes a length placeholder, not its items.
        assert_eq!(capped["nodes"]["_truncated"], true);
        assert_eq!(capped["nodes"]["_len"], 2);
        assert!(capped["nodes"].get("0").is_none());
        // A Map child at the cap becomes a keys placeholder.
        assert_eq!(capped["config"]["_truncated"], true);
        assert_eq!(capped["config"]["_keys"][0], "network");
        assert!(capped["config"]["network"].is_null());
    }

    #[test]
    fn test_to_json_capped_deep_enough_matches_full() {
        let (store, root_id) = setup();
        let root = tree_get(&store, &root_id, &StatePath::root()).unwrap();
        // A cap deeper than the tree yields the same result as the full walk.
        let full = tree_to_json(&store, &root).unwrap();
        let capped = tree_to_json_capped(&store, &root, 64).unwrap();
        assert_eq!(full, capped);
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

    // -- Batched object I/O (hosted-hub-mvp t-014) --
    //
    // On Postgres every storage call is a round trip, so a walk that fetches
    // one object at a time costs one query per node. These count the calls a
    // walk makes instead of timing it.

    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

    /// A store that counts single and batched gets and puts.
    #[derive(Default)]
    struct Counting {
        inner: agentstategraph_storage::MemoryStorage,
        gets: AtomicUsize,
        batch_gets: AtomicUsize,
        puts: AtomicUsize,
        batch_puts: AtomicUsize,
    }

    impl Counting {
        fn reset(&self) {
            for c in [&self.gets, &self.batch_gets, &self.puts, &self.batch_puts] {
                c.store(0, Relaxed);
            }
        }
        fn counts(&self) -> (usize, usize, usize, usize) {
            (
                self.gets.load(Relaxed),
                self.batch_gets.load(Relaxed),
                self.puts.load(Relaxed),
                self.batch_puts.load(Relaxed),
            )
        }
    }

    impl ObjectStore for Counting {
        fn get_object(&self, id: &ObjectId) -> Result<Option<Object>, StorageError> {
            self.gets.fetch_add(1, Relaxed);
            self.inner.get_object(id)
        }
        fn put_object(&self, obj: &Object) -> Result<ObjectId, StorageError> {
            self.puts.fetch_add(1, Relaxed);
            self.inner.put_object(obj)
        }
        fn has_object(&self, id: &ObjectId) -> Result<bool, StorageError> {
            self.inner.has_object(id)
        }
        fn batch_get_objects(&self, ids: &[ObjectId]) -> Result<Vec<Option<Object>>, StorageError> {
            self.batch_gets.fetch_add(1, Relaxed);
            self.inner.batch_get_objects(ids)
        }
        fn batch_put_objects(&self, objs: &[Object]) -> Result<Vec<ObjectId>, StorageError> {
            self.batch_puts.fetch_add(1, Relaxed);
            self.inner.batch_put_objects(objs)
        }
    }

    /// A plan-shaped tree: 40 plans of 12 tasks each, with a shared
    /// subtree (every task's `owner`), 5 levels deep below the root.
    fn plans() -> serde_json::Value {
        let owner = serde_json::json!({"agent": "claude-code", "team": "core"});
        let plans: serde_json::Map<String, serde_json::Value> = (0..40)
            .map(|p| {
                let tasks: Vec<serde_json::Value> = (0..12)
                    .map(|t| {
                        serde_json::json!({
                            "id": format!("t-{t:03}"),
                            "title": format!("task {t} of plan {p}"),
                            "status": if t % 3 == 0 { "done" } else { "pending" },
                            "blocked_by": [format!("t-{:03}", t.max(1) - 1)],
                            "owner": owner,
                        })
                    })
                    .collect();
                (
                    format!("plan-{p:02}"),
                    serde_json::json!({"name": format!("plan {p}"), "tasks": tasks}),
                )
            })
            .collect();
        serde_json::json!({ "plans": plans, "version": 1 })
    }

    /// Levels below the root: plans / plan-NN / tasks / [i] / owner / agent.
    const PLANS_DEPTH: usize = 6;

    fn stored(value: &serde_json::Value) -> (Counting, Object) {
        let store = Counting::default();
        let root_id = json_to_tree(&store, value).unwrap();
        let root = store.inner.get_object(&root_id).unwrap().unwrap();
        store.reset();
        (store, root)
    }

    #[test]
    fn a_tree_is_written_with_one_batch_put() {
        let store = Counting::default();
        let value = plans();
        let root_id = json_to_tree(&store, &value).unwrap();
        let (_, _, puts, batch_puts) = store.counts();
        assert_eq!((puts, batch_puts), (0, 1));
        let root = store.inner.get_object(&root_id).unwrap().unwrap();
        assert_eq!(tree_to_json(&store.inner, &root).unwrap(), value);
        // The id is the same as storing node by node gave.
        assert_eq!(root_id, root.id());
    }

    #[test]
    fn a_tree_is_read_a_level_at_a_time() {
        let value = plans();
        let (store, root) = stored(&value);
        assert_eq!(tree_to_json(&store, &root).unwrap(), value);
        let (gets, batch_gets, _, _) = store.counts();
        assert_eq!(gets, 0, "no object fetched on its own");
        assert!(batch_gets <= PLANS_DEPTH, "{batch_gets} batch calls");
    }

    #[test]
    fn a_capped_read_loads_only_its_levels() {
        let (store, root) = stored(&plans());
        let capped = tree_to_json_capped(&store, &root, 2).unwrap();
        let (gets, batch_gets, _, _) = store.counts();
        assert_eq!((gets, batch_gets), (0, 2));
        // Depth 2 is summarized from the plan nodes themselves.
        let plan = &capped["plans"]["plan-07"];
        assert_eq!(plan["_truncated"], true);
        assert_eq!(plan["_keys"], serde_json::json!(["name", "tasks"]));
        assert_eq!(capped["version"], 1);
    }

    #[test]
    fn paths_search_and_leaves_read_a_level_at_a_time() {
        let (store, root) = stored(&plans());
        let root_id = root.id();
        let leaves = |s: &Counting| -> usize {
            let (gets, batch_gets, _, _) = s.counts();
            s.reset();
            assert!(gets <= 1, "{gets} single gets");
            assert!(batch_gets <= PLANS_DEPTH + 1, "{batch_gets} batch calls");
            batch_gets
        };

        let paths = tree_list_paths(&store, &root_id, "", 64).unwrap();
        leaves(&store);
        // 40 plans x (name + 12 tasks x 6 leaves) + version.
        assert_eq!(paths.len(), 40 * (1 + 12 * 6) + 1);

        let found = tree_search_values(&store, &root_id, "of plan 39", 100).unwrap();
        leaves(&store);
        assert_eq!(found.len(), 12);

        let collected = tree_collect_leaves(&store, &root_id).unwrap();
        leaves(&store);
        assert_eq!(collected.len(), paths.len());

        let (removed, added) = tree_diff_leaves(&store, None, &root_id).unwrap();
        leaves(&store);
        assert!(removed.is_empty());
        assert_eq!(added.len(), paths.len());
    }

    /// Many paths read together match `tree_get` path by path, at one batch
    /// per level of the deepest path.
    #[test]
    fn many_paths_are_read_a_level_at_a_time() {
        let (store, root) = stored(&plans());
        let root_id = root.id();
        let raw = [
            "/plans/plan-03/tasks/2/title",
            "/plans/plan-39/name",
            "/plans/plan-07/tasks/11/owner",
            "/version",
            "/",
            "/plans/plan-99/name",     // missing key
            "/plans/plan-03/tasks/12", // index out of range
            "/version/deeper",         // through an atom
            "/plans/0",                // index into a map
            "/plans/plan-03/name/0",   // index into an atom
        ];
        let paths: Vec<StatePath> = raw.iter().map(|p| StatePath::parse(p).unwrap()).collect();
        let many = tree_get_many(&store, &root_id, &paths).unwrap();
        let (gets, batch_gets, _, _) = store.counts();
        assert_eq!(gets, 1, "the root only");
        // The deepest paths have five components.
        assert_eq!(batch_gets, 5, "one per level of the deepest path");
        for (path, got) in paths.iter().zip(&many) {
            let one = tree_get(&store.inner, &root_id, path).ok();
            assert_eq!(got, &one, "{path}");
        }
        assert!(many[5..].iter().all(Option::is_none));

        store.reset();
        let json = trees_to_json(&store, &many).unwrap();
        let (gets, batch_gets, _, _) = store.counts();
        assert_eq!(gets, 0);
        assert!(batch_gets <= PLANS_DEPTH, "{batch_gets}");
        assert_eq!(json[0], Some(serde_json::json!("task 2 of plan 3")));
        assert_eq!(
            json[2],
            Some(serde_json::json!({"agent": "claude-code", "team": "core"}))
        );
        assert_eq!(json[4], Some(plans()));
        assert_eq!(json[5], None);
    }

    /// The levels loaded up front only decide what's fetched in batches: a walk
    /// past them fetches the rest itself and still returns the whole tree.
    #[test]
    fn a_shallow_prefetch_still_reads_the_whole_tree() {
        let value = plans();
        let (store, root) = stored(&value);
        let prefetched = Prefetched::load(&store, &root, 2).unwrap();
        assert_eq!(to_json(&prefetched, &root).unwrap(), value);
        let (gets, batch_gets, _, _) = store.counts();
        assert_eq!(batch_gets, 2);
        assert!(gets > 0, "the levels below were fetched one by one");
    }
}
