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
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EnforcementMode {
    /// No validation. Schema is documentation only.
    None,
    /// Validate on commit, log warnings, but allow.
    Warn,
    /// Reject commits that violate the schema.
    Enforce,
    /// Apply automatic migrations when schema changes.
    Migrate,
}

impl Default for EnforcementMode {
    fn default() -> Self {
        EnforcementMode::None
    }
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

    if let Some(hint_str) = schema.get("x-agentstategraph-merge").and_then(|v| v.as_str()) {
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
    if let Some(required) = schema.get("required").and_then(|v| v.as_array()) {
        if let Some(obj) = value.as_object() {
            for req in required {
                if let Some(key) = req.as_str() {
                    if !obj.contains_key(key) {
                        errors.push(ValidationError {
                            path: format!("{}/{}", path, key),
                            message: format!("required field '{}' is missing", key),
                            expected: Some("present".to_string()),
                            actual: Some("missing".to_string()),
                        });
                    }
                }
            }
        }
    }

    // Check enum values
    if let Some(enum_values) = schema.get("enum").and_then(|v| v.as_array()) {
        if !enum_values.contains(value) {
            errors.push(ValidationError {
                path: path.to_string(),
                message: format!("value not in allowed enum: {:?}", value),
                expected: Some(format!("{:?}", enum_values)),
                actual: Some(format!("{:?}", value)),
            });
        }
    }

    // Recurse into properties
    if let Some(props) = schema.get("properties").and_then(|v| v.as_object()) {
        if let Some(obj) = value.as_object() {
            for (key, prop_schema) in props {
                if let Some(prop_value) = obj.get(key) {
                    let child_path = format!("{}/{}", path, key);
                    validate_recursive(prop_schema, prop_value, &child_path, errors, warnings);
                }
            }
        }
    }

    // Recurse into array items
    if let Some(items_schema) = schema.get("items") {
        if let Some(arr) = value.as_array() {
            for (i, item) in arr.iter().enumerate() {
                let child_path = format!("{}/{}", path, i);
                validate_recursive(items_schema, item, &child_path, errors, warnings);
            }
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
}
