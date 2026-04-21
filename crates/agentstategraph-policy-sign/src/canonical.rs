//! Canonical-JSON serialization for `Policy` values.
//!
//! The canonical form is the minimal sibling of RFC 8785 JCS that this
//! project needs:
//!
//! - UTF-8 bytes, no BOM.
//! - No whitespace between any JSON tokens.
//! - Object keys are sorted lexicographically (byte order on the UTF-8
//!   representation, which matches Rust's `str` `Ord`).
//! - The `signature` field (if present anywhere in the object tree of
//!   the policy top-level) is removed before serialization so the
//!   canonical bytes are stable across sign / verify round-trips.
//! - Arrays preserve their source order (JSON arrays are ordered
//!   sequences).
//! - Numbers are emitted as `serde_json` emits them. Policies currently
//!   carry only integer-ish `version: u64` and RFC-3339 timestamp
//!   strings, so we do not need JCS's float canonicalization rules.
//!
//! RFC 8785 JCS is intentionally *not* adopted here — it would add
//! dependencies on a full IEEE-754 canonicalizer purely to cover
//! float cases this crate never emits. Revisit if policies ever grow
//! a `f64` field.

use agentstategraph_policy::types::Policy;
use serde_json::Value;

use crate::error::SignError;

/// Serialize `policy` into the canonical bytes used as the signing
/// input. The `signature` key is removed from the top-level object
/// before serialization; see module docs for the full spec.
pub fn canonicalize(policy: &Policy) -> Result<Vec<u8>, SignError> {
    let mut value =
        serde_json::to_value(policy).map_err(|e| SignError::CanonicalizeFailed(e.to_string()))?;
    strip_signature(&mut value);
    let mut out = Vec::new();
    write_canonical(&value, &mut out).map_err(|e| SignError::CanonicalizeFailed(e.to_string()))?;
    Ok(out)
}

/// Same as [`canonicalize`] but operates on an arbitrary
/// `serde_json::Value`. Exposed for tests that want to prove the
/// sort-and-strip behavior without building a full `Policy`.
pub fn canonicalize_value(mut value: Value) -> Result<Vec<u8>, SignError> {
    strip_signature(&mut value);
    let mut out = Vec::new();
    write_canonical(&value, &mut out).map_err(|e| SignError::CanonicalizeFailed(e.to_string()))?;
    Ok(out)
}

fn strip_signature(value: &mut Value) {
    if let Value::Object(map) = value {
        map.remove("signature");
    }
}

fn write_canonical(value: &Value, out: &mut Vec<u8>) -> Result<(), serde_json::Error> {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
            // Scalars: defer to serde_json for correct escaping.
            let s = serde_json::to_vec(value)?;
            out.extend_from_slice(&s);
        }
        Value::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_canonical(item, out)?;
            }
            out.push(b']');
        }
        Value::Object(map) => {
            // serde_json::Map preserves insertion order when the
            // `preserve_order` feature is enabled and byte-key order
            // otherwise. Either way we re-sort explicitly so the
            // crate's behavior does not depend on feature flags.
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            out.push(b'{');
            for (i, (k, v)) in entries.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                // Keys are strings; escape via serde_json.
                let key_bytes = serde_json::to_vec(&Value::String((*k).clone()))?;
                out.extend_from_slice(&key_bytes);
                out.push(b':');
                write_canonical(v, out)?;
            }
            out.push(b'}');
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sorted_keys_at_top_level() {
        let v = json!({ "b": 1, "a": 2, "c": 3 });
        let bytes = canonicalize_value(v).unwrap();
        assert_eq!(
            std::str::from_utf8(&bytes).unwrap(),
            r#"{"a":2,"b":1,"c":3}"#
        );
    }

    #[test]
    fn sorted_keys_nested() {
        let v = json!({ "outer": { "z": 1, "a": 2 } });
        let bytes = canonicalize_value(v).unwrap();
        assert_eq!(
            std::str::from_utf8(&bytes).unwrap(),
            r#"{"outer":{"a":2,"z":1}}"#
        );
    }

    #[test]
    fn different_orderings_produce_same_bytes() {
        let a = json!({ "b": 1, "a": { "z": true, "y": false } });
        let b = json!({ "a": { "y": false, "z": true }, "b": 1 });
        assert_eq!(
            canonicalize_value(a).unwrap(),
            canonicalize_value(b).unwrap()
        );
    }

    #[test]
    fn signature_field_stripped() {
        let v = json!({ "a": 1, "signature": { "algorithm": "ed25519" } });
        let bytes = canonicalize_value(v).unwrap();
        let s = std::str::from_utf8(&bytes).unwrap();
        assert!(!s.contains("signature"), "unexpected: {s}");
        assert_eq!(s, r#"{"a":1}"#);
    }

    #[test]
    fn arrays_preserve_order() {
        // Also verify that object keys *inside* array items are sorted.
        let v = json!([{ "b": 2, "a": 1 }, { "d": 4, "c": 3 }]);
        let bytes = canonicalize_value(v).unwrap();
        assert_eq!(
            std::str::from_utf8(&bytes).unwrap(),
            r#"[{"a":1,"b":2},{"c":3,"d":4}]"#
        );
    }
}
