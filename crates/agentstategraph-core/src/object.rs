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

    /// Full 64-character lowercase hex of the 32-byte id, WITHOUT the `sg_`
    /// display prefix. Useful for prefix matching against a stored id.
    pub fn to_hex(&self) -> String {
        to_hex(&self.0)
    }

    /// Parse a full commit id from hex, tolerating an optional `sg_` prefix and
    /// surrounding whitespace. Returns `None` unless the input decodes to
    /// exactly 32 bytes (64 hex chars). For partial ids use [`ObjectId::to_hex`]
    /// and compare prefixes.
    pub fn from_hex(s: &str) -> Option<Self> {
        let hex = s.trim().strip_prefix("sg_").unwrap_or(s.trim());
        if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let mut bytes = [0u8; 32];
        for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
            let hi = (chunk[0] as char).to_digit(16)?;
            let lo = (chunk[1] as char).to_digit(16)?;
            bytes[i] = (hi * 16 + lo) as u8;
        }
        Some(Self(bytes))
    }

    /// Normalize a user-supplied commit-ref fragment for prefix matching:
    /// strips an optional `sg_` prefix and lowercases. Returns `None` if the
    /// remainder is empty or contains non-hex characters.
    pub fn normalize_hex_prefix(s: &str) -> Option<String> {
        let frag = s.trim().strip_prefix("sg_").unwrap_or(s.trim());
        if frag.is_empty() || !frag.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        Some(frag.to_ascii_lowercase())
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
    /// An integer above `i64::MAX` — a u64 id or hash. Every integer that
    /// fits `i64` is `Int`, so each number has one representation (and one
    /// object id); build integers with [`Object::uint`] or
    /// [`Object::from_json_number`] rather than this variant directly.
    /// Last, so the serialized form of every other variant is unchanged.
    UInt(u64),
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

    /// An unsigned integer: `Int` when it fits `i64`, `UInt` above.
    pub fn uint(v: u64) -> Self {
        match i64::try_from(v) {
            Ok(i) => Object::Atom(Atom::Int(i)),
            Err(_) => Object::Atom(Atom::UInt(v)),
        }
    }

    /// The atom for a JSON number, exactly: `Int` when it fits `i64`,
    /// `UInt` above `i64::MAX`, `Float` otherwise. Falling back to `f64` for
    /// every integer that missed `i64` stored u64 ids and hashes as different
    /// numbers.
    pub fn from_json_number(n: &serde_json::Number) -> Self {
        if let Some(i) = n.as_i64() {
            Object::int(i)
        } else if let Some(u) = n.as_u64() {
            Object::uint(u)
        } else {
            Object::float(n.as_f64().unwrap_or(f64::NAN))
        }
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

    #[test]
    fn integers_have_one_representation() {
        assert_eq!(Object::uint(5), Object::int(5));
        assert_eq!(Object::uint(i64::MAX as u64), Object::int(i64::MAX));
        assert_eq!(Object::uint(u64::MAX), Object::Atom(Atom::UInt(u64::MAX)));
        let n = |v: serde_json::Value| match v {
            serde_json::Value::Number(n) => Object::from_json_number(&n),
            _ => unreachable!(),
        };
        assert_eq!(n(serde_json::json!(5u64)), Object::int(5));
        assert_eq!(n(serde_json::json!(u64::MAX)), Object::uint(u64::MAX));
        assert_eq!(n(serde_json::json!(-3)), Object::int(-3));
        assert_eq!(n(serde_json::json!(0.5)), Object::float(0.5));
    }

    #[test]
    fn existing_atoms_serialize_unchanged() {
        // Object ids hash the serialized form: adding UInt must not move them.
        assert_eq!(
            String::from_utf8(Object::int(42).canonical_bytes()).unwrap(),
            r#"{"kind":"Atom","type":"Int","value":42}"#
        );
        assert_eq!(
            String::from_utf8(Object::uint(u64::MAX).canonical_bytes()).unwrap(),
            r#"{"kind":"Atom","type":"UInt","value":18446744073709551615}"#
        );
    }

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
            Object::float(std::f64::consts::PI).id(),
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
