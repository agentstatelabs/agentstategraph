//! Schema system — optional JSON Schema validation with merge hints.
//!
//! Schemas use x-agentstategraph-merge annotations to tell the merge engine
//! how to handle each field. This enables CRDT-inspired auto-resolution
//! of concurrent changes.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A schema definition with merge hints.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Schema {
    /// The raw JSON Schema document.
    pub json_schema: serde_json::Value,
    /// Extracted merge hints per path.
    pub merge_hints: HashMap<String, MergeHint>,
    /// Enforcement mode.
    pub enforcement: EnforcementMode,
}

/// How a field should be merged when both sides change it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MergeHint {
    /// Most recent commit's value wins.
    LastWriterWins,
    /// Merge arrays of records by a key field.
    UnionById(String),
    /// Union of both sets of values.
    Union,
    /// Add the deltas from both sides.
    Sum,
    /// Take the higher value.
    Max,
    /// Take the lower value.
    Min,
    /// Concatenate (source then target).
    Concat,
    /// Always flag as conflict.
    Manual,
    /// Invoke a named resolution function.
    Custom(String),
}

/// How strictly the schema is enforced.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum EnforcementMode {
    /// No validation. Schema is documentation only.
    #[default]
    None,
    /// Validate on commit, log warnings, but allow.
    Warn,
    /// Reject commits that violate the schema.
    Enforce,
    /// Apply automatic migrations when schema changes.
    Migrate,
}

/// Validation result for a state tree against a schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationResult {
    pub valid: bool,
    pub errors: Vec<ValidationError>,
    pub warnings: Vec<String>,
}

/// A schema validation error.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationError {
    pub path: String,
    pub message: String,
    pub expected: Option<String>,
    pub actual: Option<String>,
}

impl Schema {
    /// Create a new schema from a JSON Schema document.
    /// Extracts x-agentstategraph-merge hints from the schema.
    pub fn from_json_schema(schema: serde_json::Value, enforcement: EnforcementMode) -> Self {
        let merge_hints = extract_merge_hints(&schema, "");
        Self {
            json_schema: schema,
            merge_hints,
            enforcement,
        }
    }

    /// Get the merge hint for a specific path.
    pub fn merge_hint_for(&self, path: &str) -> Option<&MergeHint> {
        self.merge_hints.get(path)
    }

    /// Validate a JSON value against this schema.
    /// Basic validation — checks required fields and types.
    pub fn validate(&self, value: &serde_json::Value) -> ValidationResult {
        let mut errors = Vec::new();
        let mut warnings = Vec::new();

        validate_recursive(&self.json_schema, value, "", &mut errors, &mut warnings);

        ValidationResult {
            valid: errors.is_empty(),
            errors,
            warnings,
        }
    }
}

/// Extract x-agentstategraph-merge hints from a JSON Schema document.
fn extract_merge_hints(schema: &serde_json::Value, path: &str) -> HashMap<String, MergeHint> {
    let mut hints = HashMap::new();

    if let Some(hint_str) = schema
        .get("x-agentstategraph-merge")
        .and_then(|v| v.as_str())
    {
        let id_field = schema
            .get("x-agentstategraph-id-field")
            .and_then(|v| v.as_str())
            .unwrap_or("id")
            .to_string();

        let hint = match hint_str {
            "last-writer-wins" => MergeHint::LastWriterWins,
            "union-by-id" => MergeHint::UnionById(id_field),
            "union" => MergeHint::Union,
            "sum" => MergeHint::Sum,
            "max" => MergeHint::Max,
            "min" => MergeHint::Min,
            "concat" => MergeHint::Concat,
            "manual" => MergeHint::Manual,
            other => MergeHint::Custom(other.to_string()),
        };
        hints.insert(path.to_string(), hint);
    }

    // Recurse into properties
    if let Some(props) = schema.get("properties").and_then(|v| v.as_object()) {
        for (key, prop_schema) in props {
            let child_path = if path.is_empty() {
                format!("/{}", key)
            } else {
                format!("{}/{}", path, key)
            };
            hints.extend(extract_merge_hints(prop_schema, &child_path));
        }
    }

    // Recurse into items (for arrays)
    if let Some(items) = schema.get("items") {
        let child_path = format!("{}/*", path);
        hints.extend(extract_merge_hints(items, &child_path));
    }

    hints
}

/// Basic recursive validation against a JSON Schema.
// `warnings` is threaded through so future validation rules can surface
// non-blocking findings (deprecation notices etc.) without restructuring
// call sites. Present but unused today.
#[allow(clippy::only_used_in_recursion)]
fn validate_recursive(
    schema: &serde_json::Value,
    value: &serde_json::Value,
    path: &str,
    errors: &mut Vec<ValidationError>,
    warnings: &mut Vec<String>,
) {
    // Check type
    if let Some(expected_type) = schema.get("type").and_then(|v| v.as_str()) {
        let actual_type = json_type_name(value);
        if expected_type != actual_type {
            errors.push(ValidationError {
                path: path.to_string(),
                message: format!("expected type '{}', found '{}'", expected_type, actual_type),
                expected: Some(expected_type.to_string()),
                actual: Some(actual_type.to_string()),
            });
            return;
        }
    }

    // Check required fields
    if let Some(required) = schema.get("required").and_then(|v| v.as_array())
        && let Some(obj) = value.as_object()
    {
        for req in required {
            if let Some(key) = req.as_str()
                && !obj.contains_key(key)
            {
                errors.push(ValidationError {
                    path: format!("{}/{}", path, key),
                    message: format!("required field '{}' is missing", key),
                    expected: Some("present".to_string()),
                    actual: Some("missing".to_string()),
                });
            }
        }
    }

    // Check enum values
    if let Some(enum_values) = schema.get("enum").and_then(|v| v.as_array())
        && !enum_values.contains(value)
    {
        errors.push(ValidationError {
            path: path.to_string(),
            message: format!("value not in allowed enum: {:?}", value),
            expected: Some(format!("{:?}", enum_values)),
            actual: Some(format!("{:?}", value)),
        });
    }

    // Recurse into properties
    if let Some(props) = schema.get("properties").and_then(|v| v.as_object())
        && let Some(obj) = value.as_object()
    {
        for (key, prop_schema) in props {
            if let Some(prop_value) = obj.get(key) {
                let child_path = format!("{}/{}", path, key);
                validate_recursive(prop_schema, prop_value, &child_path, errors, warnings);
            }
        }
    }

    // Recurse into array items
    if let Some(items_schema) = schema.get("items")
        && let Some(arr) = value.as_array()
    {
        for (i, item) in arr.iter().enumerate() {
            let child_path = format!("{}/{}", path, i);
            validate_recursive(items_schema, item, &child_path, errors, warnings);
        }
    }
}

fn json_type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                "integer"
            } else {
                "number"
            }
        }
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_merge_hints() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "nodes": {
                    "type": "array",
                    "x-agentstategraph-merge": "union-by-id",
                    "x-agentstategraph-id-field": "node_id"
                },
                "request_count": {
                    "type": "integer",
                    "x-agentstategraph-merge": "sum"
                },
                "config": {
                    "type": "object",
                    "x-agentstategraph-merge": "last-writer-wins"
                }
            }
        });

        let s = Schema::from_json_schema(schema, EnforcementMode::None);
        assert_eq!(
            s.merge_hint_for("/nodes"),
            Some(&MergeHint::UnionById("node_id".to_string()))
        );
        assert_eq!(s.merge_hint_for("/request_count"), Some(&MergeHint::Sum));
        assert_eq!(
            s.merge_hint_for("/config"),
            Some(&MergeHint::LastWriterWins)
        );
    }

    #[test]
    fn test_validate_type_check() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "count": { "type": "integer" }
            },
            "required": ["name"]
        });

        let s = Schema::from_json_schema(schema, EnforcementMode::Enforce);

        // Valid
        let valid = serde_json::json!({"name": "test", "count": 5});
        let result = s.validate(&valid);
        assert!(result.valid, "errors: {:?}", result.errors);

        // Missing required field
        let missing = serde_json::json!({"count": 5});
        let result = s.validate(&missing);
        assert!(!result.valid);
        assert!(result.errors[0].message.contains("required"));

        // Wrong type
        let wrong_type = serde_json::json!({"name": 123, "count": 5});
        let result = s.validate(&wrong_type);
        assert!(!result.valid);
        assert!(result.errors[0].message.contains("expected type"));
    }

    #[test]
    fn test_validate_enum() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "status": {
                    "type": "string",
                    "enum": ["healthy", "unhealthy", "draining"]
                }
            }
        });

        let s = Schema::from_json_schema(schema, EnforcementMode::Enforce);

        let valid = serde_json::json!({"status": "healthy"});
        assert!(s.validate(&valid).valid);

        let invalid = serde_json::json!({"status": "unknown"});
        assert!(!s.validate(&invalid).valid);
    }

    #[test]
    fn test_validate_nested() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "nodes": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "hostname": { "type": "string" },
                            "status": { "type": "string", "enum": ["healthy", "unhealthy"] }
                        },
                        "required": ["hostname", "status"]
                    }
                }
            }
        });

        let s = Schema::from_json_schema(schema, EnforcementMode::Enforce);

        let valid = serde_json::json!({
            "nodes": [
                {"hostname": "node-1", "status": "healthy"},
                {"hostname": "node-2", "status": "unhealthy"}
            ]
        });
        assert!(s.validate(&valid).valid);

        let invalid = serde_json::json!({
            "nodes": [
                {"hostname": "node-1"}  // missing required "status"
            ]
        });
        assert!(!s.validate(&invalid).valid);
    }

    // --- All merge hint variants ---

    #[test]
    fn test_extract_all_merge_hint_variants() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "a": { "x-agentstategraph-merge": "last-writer-wins" },
                "b": { "x-agentstategraph-merge": "union-by-id" }, // default id field
                "c": { "x-agentstategraph-merge": "union" },
                "d": { "x-agentstategraph-merge": "sum" },
                "e": { "x-agentstategraph-merge": "max" },
                "f": { "x-agentstategraph-merge": "min" },
                "g": { "x-agentstategraph-merge": "concat" },
                "h": { "x-agentstategraph-merge": "manual" },
                "i": { "x-agentstategraph-merge": "my-custom-resolver" }
            }
        });

        let s = Schema::from_json_schema(schema, EnforcementMode::None);

        assert_eq!(s.merge_hint_for("/a"), Some(&MergeHint::LastWriterWins));
        // union-by-id without explicit id-field defaults to "id"
        assert_eq!(
            s.merge_hint_for("/b"),
            Some(&MergeHint::UnionById("id".to_string()))
        );
        assert_eq!(s.merge_hint_for("/c"), Some(&MergeHint::Union));
        assert_eq!(s.merge_hint_for("/d"), Some(&MergeHint::Sum));
        assert_eq!(s.merge_hint_for("/e"), Some(&MergeHint::Max));
        assert_eq!(s.merge_hint_for("/f"), Some(&MergeHint::Min));
        assert_eq!(s.merge_hint_for("/g"), Some(&MergeHint::Concat));
        assert_eq!(s.merge_hint_for("/h"), Some(&MergeHint::Manual));
        assert_eq!(
            s.merge_hint_for("/i"),
            Some(&MergeHint::Custom("my-custom-resolver".to_string()))
        );
    }

    #[test]
    fn test_merge_hint_for_unknown_path_returns_none() {
        let schema = serde_json::json!({ "type": "object" });
        let s = Schema::from_json_schema(schema, EnforcementMode::None);
        assert_eq!(s.merge_hint_for("/nonexistent"), None);
        assert_eq!(s.merge_hint_for(""), None);
    }

    #[test]
    fn test_union_by_id_explicit_id_field() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "records": {
                    "x-agentstategraph-merge": "union-by-id",
                    "x-agentstategraph-id-field": "record_id"
                }
            }
        });
        let s = Schema::from_json_schema(schema, EnforcementMode::None);
        assert_eq!(
            s.merge_hint_for("/records"),
            Some(&MergeHint::UnionById("record_id".to_string()))
        );
    }

    // --- items/* path for array schemas ---

    #[test]
    fn test_extract_merge_hints_from_items() {
        let schema = serde_json::json!({
            "type": "array",
            "x-agentstategraph-merge": "concat",
            "items": {
                "x-agentstategraph-merge": "last-writer-wins"
            }
        });
        let s = Schema::from_json_schema(schema, EnforcementMode::None);
        // Root-level hint
        assert_eq!(s.merge_hint_for(""), Some(&MergeHint::Concat));
        // Items hint
        assert_eq!(s.merge_hint_for("/*"), Some(&MergeHint::LastWriterWins));
    }

    // --- EnforcementMode default ---

    #[test]
    fn test_enforcement_mode_default_is_none() {
        assert_eq!(EnforcementMode::default(), EnforcementMode::None);
    }

    // --- json_type_name coverage ---

    #[test]
    fn test_type_mismatch_null() {
        let schema = serde_json::json!({ "type": "string" });
        let s = Schema::from_json_schema(schema, EnforcementMode::Enforce);
        let result = s.validate(&serde_json::Value::Null);
        assert!(!result.valid);
        let e = &result.errors[0];
        assert_eq!(e.expected.as_deref(), Some("string"));
        assert_eq!(e.actual.as_deref(), Some("null"));
    }

    #[test]
    fn test_type_mismatch_boolean() {
        let schema = serde_json::json!({ "type": "integer" });
        let s = Schema::from_json_schema(schema, EnforcementMode::Enforce);
        let result = s.validate(&serde_json::json!(true));
        assert!(!result.valid);
        assert_eq!(result.errors[0].actual.as_deref(), Some("boolean"));
    }

    #[test]
    fn test_type_mismatch_array() {
        let schema = serde_json::json!({ "type": "object" });
        let s = Schema::from_json_schema(schema, EnforcementMode::Enforce);
        let result = s.validate(&serde_json::json!([1, 2, 3]));
        assert!(!result.valid);
        assert_eq!(result.errors[0].actual.as_deref(), Some("array"));
    }

    #[test]
    #[allow(clippy::approx_constant)] // 3.14 here is just arbitrary float test data, not PI
    fn test_type_number_vs_integer() {
        // float is "number", not "integer"
        let schema = serde_json::json!({ "type": "number" });
        let s = Schema::from_json_schema(schema, EnforcementMode::Enforce);
        let result = s.validate(&serde_json::json!(3.14));
        assert!(result.valid, "3.14 should satisfy type:number");

        let schema = serde_json::json!({ "type": "integer" });
        let s = Schema::from_json_schema(schema, EnforcementMode::Enforce);
        let result = s.validate(&serde_json::json!(42));
        assert!(result.valid, "42 should satisfy type:integer");
    }

    // --- Validation edge cases ---

    #[test]
    fn test_validate_empty_schema_accepts_anything() {
        let schema = serde_json::json!({});
        let s = Schema::from_json_schema(schema, EnforcementMode::Enforce);
        assert!(s.validate(&serde_json::json!(42)).valid);
        assert!(s.validate(&serde_json::json!({"a": 1})).valid);
        assert!(s.validate(&serde_json::Value::Null).valid);
    }

    #[test]
    fn test_validate_multiple_required_fields_missing() {
        let schema = serde_json::json!({
            "type": "object",
            "required": ["a", "b", "c"]
        });
        let s = Schema::from_json_schema(schema, EnforcementMode::Enforce);
        let result = s.validate(&serde_json::json!({"a": 1}));
        assert!(!result.valid);
        assert_eq!(
            result.errors.len(),
            2,
            "b and c should both be reported missing"
        );
        let paths: Vec<_> = result.errors.iter().map(|e| e.path.as_str()).collect();
        assert!(paths.iter().any(|p| p.contains("b")));
        assert!(paths.iter().any(|p| p.contains("c")));
    }

    #[test]
    fn test_validation_error_fields_populated() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "x": { "type": "string" }
            }
        });
        let s = Schema::from_json_schema(schema, EnforcementMode::Enforce);
        let result = s.validate(&serde_json::json!({"x": 42}));
        assert!(!result.valid);
        let e = &result.errors[0];
        assert!(!e.path.is_empty());
        assert!(!e.message.is_empty());
        assert!(e.expected.is_some());
        assert!(e.actual.is_some());
    }

    #[test]
    fn test_validate_array_items_recursively() {
        let schema = serde_json::json!({
            "type": "array",
            "items": { "type": "integer" }
        });
        let s = Schema::from_json_schema(schema, EnforcementMode::Enforce);

        assert!(s.validate(&serde_json::json!([1, 2, 3])).valid);

        let result = s.validate(&serde_json::json!([1, "bad", 3]));
        assert!(!result.valid);
        assert_eq!(result.errors.len(), 1);
        assert!(result.errors[0].path.contains("1")); // index 1 is "bad"
    }

    // --- Schema serialization ---

    #[test]
    fn test_schema_serializes_and_deserializes() {
        let schema = Schema::from_json_schema(
            serde_json::json!({
                "type": "object",
                "properties": {
                    "count": { "x-agentstategraph-merge": "sum" }
                }
            }),
            EnforcementMode::Warn,
        );

        let json = serde_json::to_string(&schema).unwrap();
        let restored: Schema = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.enforcement, EnforcementMode::Warn);
        assert_eq!(restored.merge_hint_for("/count"), Some(&MergeHint::Sum));
    }

    #[test]
    fn test_merge_hint_serializes_and_deserializes() {
        let hints = vec![
            MergeHint::LastWriterWins,
            MergeHint::UnionById("id".to_string()),
            MergeHint::Union,
            MergeHint::Sum,
            MergeHint::Max,
            MergeHint::Min,
            MergeHint::Concat,
            MergeHint::Manual,
            MergeHint::Custom("custom-fn".to_string()),
        ];
        for hint in hints {
            let json = serde_json::to_string(&hint).unwrap();
            let restored: MergeHint = serde_json::from_str(&json).unwrap();
            assert_eq!(restored, hint, "round-trip failed for {hint:?}");
        }
    }
}
