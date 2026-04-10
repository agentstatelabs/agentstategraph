//! Content-addressed objects — the fundamental unit of state storage.
//!
//! All state in AgentStateGraph is composed of Objects. Every Object is individually
//! content-addressed via BLAKE3 hash of its canonical serialization.
//!
//! Two objects with identical content always produce the same ObjectId,
//! enabling automatic deduplication of identical subtrees.

use blake3;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

/// A BLAKE3 hash identifying an object by its content.
/// Two objects with identical content always produce the same ObjectId.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ObjectId([u8; 32]);

impl ObjectId {
    /// Create an ObjectId from raw bytes.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Return the raw bytes of this ObjectId.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Compute the ObjectId for the given canonical bytes.
    pub fn hash(data: &[u8]) -> Self {
        Self(*blake3::hash(data).as_bytes())
    }

    /// Display as a short hex prefix (for logging/debugging).
    pub fn short(&self) -> String {
        format!("sg_{}", to_hex(&self.0[..6]))
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sg_{}", to_hex(&self.0))
    }
}

/// Convert bytes to hex string without external dependency.
fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

impl fmt::Debug for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ObjectId({})", self.short())
    }
}

/// A leaf value in the state tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum Atom {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Bytes(Vec<u8>),
}

/// A container value in the state tree.
/// Nodes reference children by ObjectId, forming a Merkle DAG.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum Node {
    /// String-keyed map. Keys are sorted for canonical serialization.
    Map(BTreeMap<String, ObjectId>),
    /// Ordered list of values.
    List(Vec<ObjectId>),
    /// Unordered set of unique values. Sorted by ObjectId for canonical serialization.
    Set(Vec<ObjectId>),
}

/// An Object is either an Atom (leaf) or a Node (container).
/// This is the fundamental unit of state storage in AgentStateGraph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Object {
    Atom(Atom),
    Node(Node),
}

impl Object {
    /// Compute the canonical serialization of this object.
    /// The serialization is deterministic: map keys are sorted,
    /// sets are sorted by ObjectId, and numeric types use fixed encoding.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        // serde_json with BTreeMap guarantees sorted keys
        serde_json::to_vec(self).expect("Object serialization should never fail")
    }

    /// Compute the content-address (ObjectId) of this object.
    pub fn id(&self) -> ObjectId {
        ObjectId::hash(&self.canonical_bytes())
    }

    // -- Convenience constructors --

    pub fn null() -> Self {
        Object::Atom(Atom::Null)
    }

    pub fn bool(v: bool) -> Self {
        Object::Atom(Atom::Bool(v))
    }

    pub fn int(v: i64) -> Self {
        Object::Atom(Atom::Int(v))
    }

    pub fn float(v: f64) -> Self {
        Object::Atom(Atom::Float(v))
    }

    pub fn string(v: impl Into<String>) -> Self {
        Object::Atom(Atom::String(v.into()))
    }

    pub fn bytes(v: Vec<u8>) -> Self {
        Object::Atom(Atom::Bytes(v))
    }

    pub fn map(entries: BTreeMap<String, ObjectId>) -> Self {
        Object::Node(Node::Map(entries))
    }

    pub fn empty_map() -> Self {
        Object::Node(Node::Map(BTreeMap::new()))
    }

    pub fn list(items: Vec<ObjectId>) -> Self {
        Object::Node(Node::List(items))
    }

    pub fn set(items: Vec<ObjectId>) -> Self {
        let mut sorted = items;
        sorted.sort();
        sorted.dedup();
        Object::Node(Node::Set(sorted))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_content_addressing_deterministic() {
        let obj1 = Object::string("hello");
        let obj2 = Object::string("hello");
        assert_eq!(
            obj1.id(),
            obj2.id(),
            "identical objects must have the same ObjectId"
        );
    }

    #[test]
    fn test_different_content_different_id() {
        let obj1 = Object::string("hello");
        let obj2 = Object::string("world");
        assert_ne!(
            obj1.id(),
            obj2.id(),
            "different objects must have different ObjectIds"
        );
    }

    #[test]
    fn test_map_key_order_irrelevant() {
        // BTreeMap sorts keys, so insertion order doesn't matter
        let id_a = Object::string("a").id();
        let id_b = Object::string("b").id();

        let mut map1 = BTreeMap::new();
        map1.insert("first".to_string(), id_a);
        map1.insert("second".to_string(), id_b);

        let mut map2 = BTreeMap::new();
        map2.insert("second".to_string(), id_b);
        map2.insert("first".to_string(), id_a);

        let obj1 = Object::map(map1);
        let obj2 = Object::map(map2);
        assert_eq!(
            obj1.id(),
            obj2.id(),
            "maps with same entries in different order must have same ObjectId"
        );
    }

    #[test]
    fn test_set_dedup_and_sort() {
        let id_a = Object::string("a").id();
        let id_b = Object::string("b").id();

        let obj1 = Object::set(vec![id_a, id_b, id_a]); // duplicate
        let obj2 = Object::set(vec![id_b, id_a]); // different order

        assert_eq!(obj1.id(), obj2.id(), "sets must be deduplicated and sorted");
    }

    #[test]
    fn test_object_id_display() {
        let obj = Object::string("test");
        let id = obj.id();
        let display = format!("{}", id);
        assert!(
            display.starts_with("sg_"),
            "ObjectId display should start with sg_ prefix"
        );
    }

    #[test]
    fn test_atom_variants() {
        // Ensure all atom types produce distinct ObjectIds
        let ids: Vec<ObjectId> = vec![
            Object::null().id(),
            Object::bool(true).id(),
            Object::bool(false).id(),
            Object::int(42).id(),
            Object::float(3.14).id(),
            Object::string("test").id(),
            Object::bytes(vec![1, 2, 3]).id(),
        ];

        for i in 0..ids.len() {
            for j in (i + 1)..ids.len() {
                assert_ne!(
                    ids[i], ids[j],
                    "different atom types/values must have different ObjectIds"
                );
            }
        }
    }
}
