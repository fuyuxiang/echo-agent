use crate::types::{ArgumentType, SchemaType, ToolArgument};
use serde_json::Value;
use std::collections::HashSet;

/// Parse a JSON Schema "parameters" object into a list of [`ToolArgument`]s.
///
/// This function extracts flat, top-level properties from an
/// "object"-typed schema. It is intentionally a minimal subset of
/// JSON Schema — just enough for render tools to work as they don't speak JSON schema.
///
/// # Supported keywords
///
/// | Keyword | Scope | Notes |
/// |---|---|---|
/// | `properties` | top-level | Each key becomes a [`ToolArgument`] |
/// | `required` | top-level | Marks arguments as required |
/// | `$defs` | top-level | Resolved when referenced by `$ref` |
/// | `type` | per-property | String (`"string"`) or array (`["string", "null"]`) via [`SchemaType::from_value`] |
/// | `description` | per-property | Mapped to [`ToolArgument::description`] |
/// | `default` | per-property | Mapped to [`ToolArgument::default`] |
/// | `enum` | per-property | Mapped to [`ToolArgument::allowed_values`] |
/// | `minimum` / `maximum` | per-property | Inclusive numeric bounds |
/// | `exclusiveMinimum` / `exclusiveMaximum` | per-property | Exclusive numeric bounds |
/// | `$ref` | per-property | Resolved against `$defs` for enum types |
/// | `anyOf` | per-property | Resolved: `$ref` branches follow `$defs`, type-only branches infer [`SchemaType`] |
/// | `oneOf` | in `$defs` | `const` values extracted as enum variants |
///
/// For composite types (`array`, `object`), the entire property schema
/// is stored in [`ToolArgument::schema`] so downstream consumers can
/// inspect nested structure.
///
/// # Not supported (ignored)
///
/// The following JSON Schema features are **not** extracted and will be
/// silently dropped. Callers that need them should either pre-process
/// the schema or use the raw `schema` field on composite arguments.
///
/// - Composition: `allOf`
/// - Conditionals: `if` / `then` / `else`, `dependentRequired`,
///   `dependentSchemas`
/// - Object keywords: `patternProperties`, `additionalProperties`,
///   `propertyNames`, `minProperties`, `maxProperties`
/// - String keywords: `pattern`, `minLength`, `maxLength`, `format`
/// - Array keywords: `items`, `prefixItems`, `minItems`, `maxItems`,
///   `uniqueItems`
///
/// # Returns
///
/// An empty `Vec` when `schema` has no `"properties"` key (or it is
/// not an object).
///
/// JSON Schema spec: <https://json-schema.org/understanding-json-schema>
pub fn parse_arguments_from_schema_lossy(schema: &serde_json::Value) -> Vec<ToolArgument> {
    let properties = match schema.get("properties").and_then(|p| p.as_object()) {
        Some(p) => p,
        None => return Vec::new(),
    };

    let required: HashSet<&str> = schema
        .get("required")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();

    let defs = schema.get("$defs").and_then(|v| v.as_object());

    properties
        .iter()
        .map(|(name, prop)| {
            let (resolved_type, resolved_values, resolved_default, resolved_schema) =
                resolve_ref_type(prop.as_object(), defs);

            let arg_type = resolved_type.unwrap_or_else(|| {
                prop.get("type")
                    .map(SchemaType::from_value)
                    .unwrap_or_default()
            });

            let description = prop
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .to_string();

            let default = resolved_default.or_else(|| prop.get("default").cloned());
            let is_required = required.contains(name.as_str());

            let allowed_values = if let Some(vals) = resolved_values {
                vals
            } else {
                prop.get("allowed_values")
                    .or_else(|| prop.get("enum"))
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default()
            };

            let schema = if let Some(raw) = resolved_schema {
                Some(raw)
            } else if arg_type.is_composite() {
                Some(prop.clone())
            } else {
                None
            };

            let minimum = prop.get("minimum").and_then(|v| v.as_number()).cloned();
            let maximum = prop.get("maximum").and_then(|v| v.as_number()).cloned();
            let exclusive_minimum = prop
                .get("exclusiveMinimum")
                .and_then(|v| v.as_number())
                .cloned();
            let exclusive_maximum = prop
                .get("exclusiveMaximum")
                .and_then(|v| v.as_number())
                .cloned();

            let mut arg = ToolArgument::new(name.clone(), description)
                .with_type(arg_type)
                .with_allowed_values(allowed_values);
            if !is_required {
                arg = arg.set_optional();
            }
            if let Some(d) = default {
                arg = arg.with_default(d);
            }
            if let Some(s) = schema {
                arg = arg.with_schema(s);
            }
            if let Some(min) = minimum {
                arg = arg.with_minimum(min);
            }
            if let Some(max) = maximum {
                arg = arg.with_maximum(max);
            }
            if let Some(min) = exclusive_minimum {
                arg = arg.with_exclusive_minimum(min);
            }
            if let Some(max) = exclusive_maximum {
                arg = arg.with_exclusive_maximum(max);
            }
            arg
        })
        .collect()
}

// ---------------------------------------------------------------------------
// $ref / $defs / anyOf / oneOf resolution
// ---------------------------------------------------------------------------

/// Resolve type info from a property, following `$ref` → `$defs` and
/// `anyOf` patterns that schemars generates for Rust enums and
/// `Option<Enum>` types.
///
/// Returns `(type, allowed_values, default, raw_schema)`.
fn resolve_ref_type(
    prop: Option<&serde_json::Map<String, Value>>,
    defs: Option<&serde_json::Map<String, Value>>,
) -> (
    Option<SchemaType>,
    Option<Vec<Value>>,
    Option<Value>,
    Option<Value>,
) {
    let prop = match prop {
        Some(p) => p,
        None => return (None, None, None, None),
    };

    // Pattern 1: `anyOf` — schemars uses this for `Option<Enum>` and
    // union types.
    if let Some(any_of) = prop.get("anyOf").and_then(|v| v.as_array()) {
        // Check if any branch is a `$ref` to a `$defs` enum.
        for item in any_of {
            if let Some(ref_path) = item.get("$ref").and_then(|v| v.as_str())
                && let Some(enum_name) = ref_path.strip_prefix("#/$defs/")
                && let Some(enum_def) = defs.and_then(|d| d.get(enum_name))
            {
                let (ty, vals, def) = extract_enum_from_def(enum_def);
                return (ty.map(SchemaType::Single), vals, def, None);
            }
        }

        // No $ref found — infer a union type from the branches' `type` fields.
        if prop.get("type").is_none() {
            let types: Vec<ArgumentType> = any_of
                .iter()
                .filter_map(|branch| {
                    branch
                        .get("type")
                        .and_then(|t| t.as_str())
                        .and_then(ArgumentType::from_schema_type)
                })
                .collect();

            let schema_type = match types.len() {
                0 => None,
                1 => Some(SchemaType::Single(types[0])),
                _ => Some(SchemaType::Multiple(types)),
            };

            if schema_type.is_some() {
                return (schema_type, None, None, Some(Value::Object(prop.clone())));
            }
        }
    }

    // Pattern 2: direct `$ref` (no `anyOf` wrapper) — schemars uses
    // this for non-optional enum fields.
    if let Some(ref_path) = prop.get("$ref").and_then(|v| v.as_str())
        && let Some(enum_name) = ref_path.strip_prefix("#/$defs/")
        && let Some(enum_def) = defs.and_then(|d| d.get(enum_name))
    {
        let (ty, vals, def) = extract_enum_from_def(enum_def);
        return (ty.map(SchemaType::Single), vals, def, None);
    }

    (None, None, None, None)
}

/// Extract enum info from a `$defs` entry.
///
/// Handles two schemars patterns:
/// - Compact: `{ "type": "string", "enum": ["a", "b"] }`
/// - oneOf:   `{ "oneOf": [{ "const": "a" }, { "const": "b" }] }`
fn extract_enum_from_def(
    enum_def: &Value,
) -> (Option<ArgumentType>, Option<Vec<Value>>, Option<Value>) {
    let Some(obj) = enum_def.as_object() else {
        return (None, None, None);
    };

    // Compact form: `"enum": [...]`
    if let Some(values) = obj.get("enum").and_then(|v| v.as_array())
        && !values.is_empty()
    {
        let first_value = values.first().cloned();
        let arg_type = infer_arg_type(&first_value);
        return (Some(arg_type), Some(values.clone()), first_value);
    }

    // oneOf form: `"oneOf": [{"const": ...}, ...]`
    let Some(one_of) = obj.get("oneOf").and_then(|v| v.as_array()) else {
        return (None, None, None);
    };

    let mut values = Vec::new();
    let mut first_value = None;

    for variant in one_of {
        if let Some(const_val) = variant.get("const") {
            if first_value.is_none() {
                first_value = Some(const_val.clone());
            }
            values.push(const_val.clone());
        }
    }

    if values.is_empty() {
        return (None, None, None);
    }

    let arg_type = infer_arg_type(&first_value);
    (Some(arg_type), Some(values), first_value)
}

/// Infer the [`ArgumentType`] from a sample enum value.
fn infer_arg_type(sample: &Option<Value>) -> ArgumentType {
    match sample {
        Some(Value::String(_)) => ArgumentType::String,
        Some(Value::Number(n)) if n.is_i64() || n.is_u64() => ArgumentType::Integer,
        Some(Value::Number(_)) => ArgumentType::Number,
        Some(Value::Bool(_)) => ArgumentType::Boolean,
        _ => ArgumentType::String,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ArgumentType;

    #[test]
    fn parse_schema_basic() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query"
                },
                "limit": {
                    "type": "integer",
                    "description": "Max results",
                    "default": 10
                }
            },
            "required": ["query"]
        });

        let args = parse_arguments_from_schema_lossy(&schema);
        assert_eq!(args.len(), 2);

        let query = args.iter().find(|a| a.name == "query").unwrap();
        assert_eq!(query.arg_type, ArgumentType::String);
        assert!(query.required);
        assert!(query.default.is_none());

        let limit = args.iter().find(|a| a.name == "limit").unwrap();
        assert_eq!(limit.arg_type, ArgumentType::Integer);
        assert!(!limit.required);
        assert_eq!(limit.default, Some(serde_json::json!(10)));
    }

    #[test]
    fn parse_schema_enum_values() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "mode": {
                    "type": "string",
                    "description": "Processing mode",
                    "enum": ["fast", "slow", "auto"],
                    "default": "auto"
                }
            }
        });

        let args = parse_arguments_from_schema_lossy(&schema);
        assert_eq!(args.len(), 1);
        assert_eq!(args[0].allowed_values.len(), 3);
        assert!(!args[0].required);
    }

    #[test]
    fn parse_schema_composite_stores_schema() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "filters": {
                    "type": "object",
                    "description": "Filter object",
                    "properties": {
                        "key": { "type": "string" }
                    }
                },
                "tags": {
                    "type": "array",
                    "description": "Tag list",
                    "items": { "type": "string" }
                }
            },
            "required": ["filters"]
        });

        let args = parse_arguments_from_schema_lossy(&schema);
        assert_eq!(args.len(), 2);

        let filters = args.iter().find(|a| a.name == "filters").unwrap();
        assert_eq!(filters.arg_type, ArgumentType::Object);
        assert!(filters.schema.is_some());
        assert!(filters.required);

        let tags = args.iter().find(|a| a.name == "tags").unwrap();
        assert_eq!(tags.arg_type, ArgumentType::Array);
        assert!(tags.schema.is_some());
        assert!(!tags.required);
    }

    #[test]
    fn parse_schema_primitives_no_schema() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "flag": { "type": "boolean", "description": "A flag" }
            }
        });

        let args = parse_arguments_from_schema_lossy(&schema);
        assert_eq!(args[0].arg_type, ArgumentType::Boolean);
        assert!(args[0].schema.is_none());
    }

    #[test]
    fn parse_schema_empty_or_missing() {
        assert!(parse_arguments_from_schema_lossy(&serde_json::json!({})).is_empty());
        assert!(parse_arguments_from_schema_lossy(&serde_json::json!(null)).is_empty());
        assert!(
            parse_arguments_from_schema_lossy(&serde_json::json!({"properties": null})).is_empty()
        );
    }

    #[test]
    fn parse_schema_required_and_default_orthogonal() {
        // Per JSON Schema, required and default are independent.
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "x": { "type": "integer", "description": "test", "default": 1 }
            },
            "required": ["x"]
        });

        let args = parse_arguments_from_schema_lossy(&schema);
        assert!(args[0].required, "required should be preserved");
        assert_eq!(args[0].default, Some(serde_json::json!(1)));
    }

    #[test]
    fn parse_schema_null_type() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "placeholder": {
                    "type": "null",
                    "description": "Always null"
                }
            }
        });

        let args = parse_arguments_from_schema_lossy(&schema);
        assert_eq!(args.len(), 1);
        assert_eq!(args[0].arg_type, ArgumentType::Null);
    }

    #[test]
    fn parse_schema_nullable_string() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": ["string", "null"],
                    "description": "Optional name"
                }
            },
            "required": ["name"]
        });

        let args = parse_arguments_from_schema_lossy(&schema);
        assert_eq!(args.len(), 1);
        assert!(args[0].arg_type.is_nullable());
        assert_eq!(args[0].arg_type.primary_type(), ArgumentType::String);
    }

    #[test]
    fn parse_schema_multi_type_no_null() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "value": {
                    "type": ["string", "integer"],
                    "description": "String or integer"
                }
            }
        });

        let args = parse_arguments_from_schema_lossy(&schema);
        assert_eq!(args.len(), 1);
        assert!(!args[0].arg_type.is_nullable());
        assert!(args[0].arg_type.contains(ArgumentType::String));
        assert!(args[0].arg_type.contains(ArgumentType::Integer));
    }

    // -- $ref / $defs / anyOf resolution ----------------------------------------

    #[test]
    fn parse_schema_any_of_ref_resolves_enum() {
        // schemars pattern for `Option<MyEnum>` with oneOf-style defs.
        let schema = serde_json::json!({
            "type": "object",
            "$defs": {
                "MyEnum": {
                    "oneOf": [
                        {"const": "a"},
                        {"const": "b"}
                    ]
                }
            },
            "properties": {
                "mode": {
                    "anyOf": [
                        {"$ref": "#/$defs/MyEnum"},
                        {"type": "null"}
                    ],
                    "default": null,
                    "description": "The mode"
                }
            },
            "required": ["mode"]
        });

        let args = parse_arguments_from_schema_lossy(&schema);
        let mode = &args[0];
        assert_eq!(mode.arg_type, ArgumentType::String);
        assert_eq!(mode.allowed_values, vec!["a", "b"]);
        // The resolved default is "a" (first enum variant), not null.
        assert_eq!(mode.default, Some(serde_json::json!("a")));
        assert!(mode.schema.is_none());
    }

    #[test]
    fn parse_schema_direct_ref_resolves_enum() {
        // schemars pattern for a required (non-Option) enum field.
        let schema = serde_json::json!({
            "type": "object",
            "$defs": {
                "Color": {
                    "oneOf": [
                        {"const": "red"},
                        {"const": "blue"}
                    ]
                }
            },
            "properties": {
                "color": {
                    "$ref": "#/$defs/Color",
                    "description": "Pick a color"
                }
            },
            "required": ["color"]
        });

        let args = parse_arguments_from_schema_lossy(&schema);
        let color = &args[0];
        assert_eq!(color.arg_type, ArgumentType::String);
        assert_eq!(color.allowed_values, vec!["red", "blue"]);
    }

    #[test]
    fn parse_schema_compact_enum_in_defs() {
        // schemars pattern with compact `"enum": [...]` in $defs.
        let schema = serde_json::json!({
            "type": "object",
            "$defs": {
                "Duration": {
                    "type": "string",
                    "enum": ["6", "10"]
                }
            },
            "properties": {
                "dur_optional": {
                    "anyOf": [
                        {"$ref": "#/$defs/Duration"},
                        {"type": "null"}
                    ],
                    "default": null,
                    "description": "Duration (optional)"
                },
                "dur_required": {
                    "$ref": "#/$defs/Duration",
                    "description": "Duration (required)"
                }
            },
            "required": ["dur_optional", "dur_required"]
        });

        let args = parse_arguments_from_schema_lossy(&schema);
        assert_eq!(args.len(), 2);

        let opt = args.iter().find(|a| a.name == "dur_optional").unwrap();
        assert_eq!(opt.allowed_values, vec!["6", "10"]);

        let req = args.iter().find(|a| a.name == "dur_required").unwrap();
        assert_eq!(req.allowed_values, vec!["6", "10"]);
    }

    #[test]
    fn parse_schema_any_of_union_type_without_ref() {
        // anyOf without $ref — infer union type from branches.
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "to": {
                    "anyOf": [
                        {"type": "string"},
                        {"type": "array", "items": {"type": "string"}}
                    ],
                    "description": "Recipients"
                }
            },
            "required": ["to"]
        });

        let args = parse_arguments_from_schema_lossy(&schema);
        let to = &args[0];
        assert_eq!(to.arg_type.primary_type(), ArgumentType::String);
        assert!(to.required);
        assert!(to.schema.is_some(), "anyOf schema should be stored");
    }

    #[test]
    fn parse_schema_no_defs_still_works() {
        // Properties with no $ref/$defs should work exactly as before.
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "A name",
                    "enum": ["x", "y"],
                    "default": "x"
                }
            }
        });

        let args = parse_arguments_from_schema_lossy(&schema);
        assert_eq!(args[0].allowed_values, vec!["x", "y"]);
        assert_eq!(args[0].default, Some(serde_json::json!("x")));
    }

    // -- numeric constraints --------------------------------------------------

    #[test]
    fn parse_schema_numeric_constraints() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "offset": {
                    "type": "integer",
                    "description": "Start line",
                    "minimum": 0,
                    "default": 1
                },
                "limit": {
                    "type": "integer",
                    "description": "Line count",
                    "exclusiveMinimum": 0,
                    "default": 2000
                },
                "timeout": {
                    "type": "integer",
                    "description": "Timeout in seconds",
                    "minimum": 0,
                    "maximum": 600
                }
            },
            "required": ["timeout"]
        });

        let args = parse_arguments_from_schema_lossy(&schema);

        let offset = args.iter().find(|a| a.name == "offset").unwrap();
        assert_eq!(offset.minimum, Some(serde_json::Number::from(0)));
        assert!(offset.maximum.is_none());
        assert!(offset.exclusive_minimum.is_none());

        let limit = args.iter().find(|a| a.name == "limit").unwrap();
        assert_eq!(limit.exclusive_minimum, Some(serde_json::Number::from(0)));
        assert!(limit.minimum.is_none());

        let timeout = args.iter().find(|a| a.name == "timeout").unwrap();
        assert_eq!(timeout.minimum, Some(serde_json::Number::from(0)));
        assert_eq!(timeout.maximum, Some(serde_json::Number::from(600)));
        assert!(timeout.required);
    }
}
