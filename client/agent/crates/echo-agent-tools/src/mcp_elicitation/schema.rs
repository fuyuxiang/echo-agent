//! Elicitation `requestedSchema` parsing: size limits, the immutable field
//! specification model, and the schema → spec conversion. Validation of
//! submitted values lives in [`super::validate`].

use serde_json::Value;
use std::collections::HashSet;

pub const MAX_ELICIT_FIELDS: usize = 32;
pub const MAX_ELICIT_MESSAGE_CHARS: usize = 4096;
pub const MAX_ELICIT_URL_CHARS: usize = 2048;
pub const MAX_ELICIT_ID_CHARS: usize = 128;
pub const MAX_ELICIT_NAME_CHARS: usize = 64;
pub const MAX_ELICIT_TITLE_CHARS: usize = 128;
pub const MAX_ELICIT_DESC_CHARS: usize = 512;
pub const MAX_ELICIT_ENUM_VALUES: usize = 32;
pub const MAX_ELICIT_ENUM_VALUE_CHARS: usize = 128;
pub const MAX_ELICIT_SCHEMA_BYTES: usize = 64 * 1024;
pub const MAX_ELICIT_DRAFT_CHARS: usize = 4096;

pub fn chars_within(s: &str, max: usize) -> bool {
    s.chars().count() <= max
}

pub fn take_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

fn schema_bytes_ok(schema: &Value) -> bool {
    serde_json::to_vec(schema)
        .map(|b| b.len() <= MAX_ELICIT_SCHEMA_BYTES)
        .unwrap_or(false)
}

/// Immutable description of one form field, parsed from the server's
/// `requestedSchema`. Carries schema constraints and defaults only — user
/// input, selections, and display errors live with the consumer (the pager),
/// which submits values back through [`super::validate_form`].
#[derive(Debug, Clone)]
pub struct ElicitFieldSpec {
    pub name: String,
    pub title: String,
    pub description: Option<String>,
    pub required: bool,
    pub kind: ElicitFieldKind,
}

/// One selectable option of a single- or multi-select field. `label` falls
/// back to `value` when the schema gives no display title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElicitOption {
    pub value: String,
    pub label: String,
}

#[derive(Debug, Clone)]
pub enum ElicitFieldKind {
    String {
        format: Option<ElicitTextFormat>,
        min_length: Option<u64>,
        max_length: Option<u64>,
        default: Option<String>,
    },
    /// `type: "number"` — validated as a finite `f64`.
    Number {
        minimum: Option<f64>,
        maximum: Option<f64>,
        default: Option<String>,
    },
    /// `type: "integer"` — parsed and range-checked losslessly as `i64`
    /// (never through `f64`, which rounds above 2^53 and saturates casts).
    Integer {
        minimum: Option<i64>,
        maximum: Option<i64>,
        default: Option<String>,
    },
    Boolean {
        default: bool,
    },
    SingleSelect {
        options: Vec<ElicitOption>,
        default_index: Option<usize>,
    },
    /// `type: "array"` multi-select enum (`items.enum` or titled
    /// `items.anyOf` const/title entries). Submits a JSON string array.
    MultiSelect {
        options: Vec<ElicitOption>,
        min_items: Option<u64>,
        max_items: Option<u64>,
        default_indexes: Vec<usize>,
    },
    Unsupported {
        reason: String,
    },
}

/// The four `format` values the MCP elicitation spec allows on string
/// fields. Unknown format strings are annotations per JSON Schema and get
/// no validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElicitTextFormat {
    Email,
    Uri,
    Date,
    DateTime,
}

impl ElicitTextFormat {
    fn from_schema(format: &str) -> Option<Self> {
        match format {
            "email" => Some(Self::Email),
            "uri" => Some(Self::Uri),
            "date" => Some(Self::Date),
            "date-time" => Some(Self::DateTime),
            _ => None,
        }
    }
}

/// Parse a `requestedSchema` into field specs. `Err` is a human-readable
/// reason the whole form is unusable (malformed schema, over caps).
pub fn parse_form_schema(schema: &Value) -> Result<Vec<ElicitFieldSpec>, String> {
    let Some(obj) = schema.as_object() else {
        return Err("requestedSchema must be a JSON object".into());
    };

    let type_ok = obj
        .get("type")
        .and_then(|t| t.as_str())
        .is_none_or(|t| t == "object");
    if !type_ok {
        return Err("requestedSchema.type must be \"object\"".into());
    }

    let required: HashSet<String> = obj
        .get("required")
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    let Some(props) = obj.get("properties").and_then(|p| p.as_object()) else {
        return Err("requestedSchema.properties is required".into());
    };

    if !schema_bytes_ok(schema) {
        return Err(format!(
            "requestedSchema exceeds {MAX_ELICIT_SCHEMA_BYTES} bytes"
        ));
    }

    if props.len() > MAX_ELICIT_FIELDS {
        return Err(format!(
            "requestedSchema.properties exceeds {MAX_ELICIT_FIELDS} fields"
        ));
    }

    let mut fields = Vec::with_capacity(props.len());
    for (name, prop) in props {
        if !chars_within(name, MAX_ELICIT_NAME_CHARS) {
            return Err(format!(
                "requestedSchema property name exceeds {MAX_ELICIT_NAME_CHARS} characters"
            ));
        }
        fields.push(field_from_schema(name, prop, required.contains(name))?);
    }
    Ok(fields)
}

fn field_from_schema(name: &str, prop: &Value, required: bool) -> Result<ElicitFieldSpec, String> {
    let title = prop.get("title").and_then(|t| t.as_str()).unwrap_or(name);
    if !chars_within(title, MAX_ELICIT_TITLE_CHARS) {
        return Err(format!(
            "requestedSchema title exceeds {MAX_ELICIT_TITLE_CHARS} characters"
        ));
    }
    let title = title.to_string();
    let description = prop.get("description").and_then(|d| d.as_str());
    if let Some(d) = description
        && !chars_within(d, MAX_ELICIT_DESC_CHARS)
    {
        return Err(format!(
            "requestedSchema description exceeds {MAX_ELICIT_DESC_CHARS} characters"
        ));
    }
    let description = description.map(str::to_string);
    // Defaults become drafts, so they get the draft cap — not the (smaller)
    // description cap, which would fail schemas whose defaults are legal to
    // type by hand.
    if let Some(Value::String(s)) = prop.get("default")
        && !chars_within(s, MAX_ELICIT_DRAFT_CHARS)
    {
        return Err(format!(
            "requestedSchema default exceeds {MAX_ELICIT_DRAFT_CHARS} characters"
        ));
    }
    let default_str = prop.get("default").map(|d| match d {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        other => other.to_string(),
    });

    let kind = field_kind_from_schema(prop, default_str)?;
    Ok(ElicitFieldSpec {
        name: name.to_string(),
        title,
        description,
        required,
        kind,
    })
}

fn field_kind_from_schema(
    prop: &Value,
    default_str: Option<String>,
) -> Result<ElicitFieldKind, String> {
    // Legacy single-select: `enum` (+ optional parallel `enumNames` labels).
    if let Some(values) = prop.get("enum").and_then(|e| e.as_array()) {
        let names = prop.get("enumNames").and_then(|n| n.as_array());
        let options: Vec<ElicitOption> = values
            .iter()
            .enumerate()
            .filter_map(|(i, v)| {
                let value = json_scalar_to_string(v)?;
                let label = names
                    .and_then(|n| n.get(i))
                    .and_then(|l| l.as_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| value.clone());
                Some(ElicitOption { value, label })
            })
            .collect();
        check_options(&options)?;
        let default_index = default_option_index(&options, default_str.as_deref());
        return Ok(ElicitFieldKind::SingleSelect {
            options,
            default_index,
        });
    }

    // Titled single-select: `oneOf` of `const`/`title` entries.
    if let Some(one_of) = prop.get("oneOf").and_then(|o| o.as_array()) {
        let options = const_title_options(one_of);
        if !options.is_empty() {
            check_options(&options)?;
            let default_index = default_option_index(&options, default_str.as_deref());
            return Ok(ElicitFieldKind::SingleSelect {
                options,
                default_index,
            });
        }
    }

    let ty = prop
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or("string");
    let kind = match ty {
        "string" => ElicitFieldKind::String {
            format: prop
                .get("format")
                .and_then(|f| f.as_str())
                .and_then(ElicitTextFormat::from_schema),
            min_length: prop.get("minLength").and_then(|v| v.as_u64()),
            max_length: prop.get("maxLength").and_then(|v| v.as_u64()),
            default: default_str,
        },
        "number" => ElicitFieldKind::Number {
            minimum: prop.get("minimum").and_then(|v| v.as_f64()),
            maximum: prop.get("maximum").and_then(|v| v.as_f64()),
            default: default_str,
        },
        "integer" => ElicitFieldKind::Integer {
            minimum: integer_bound(prop.get("minimum"), /*lower*/ true),
            maximum: integer_bound(prop.get("maximum"), /*lower*/ false),
            default: default_str,
        },
        "boolean" => ElicitFieldKind::Boolean {
            default: prop
                .get("default")
                .and_then(|d| d.as_bool())
                .unwrap_or(false),
        },
        "array" => multi_select_from_schema(prop)?,
        other => ElicitFieldKind::Unsupported {
            reason: format!("unsupported type \"{other}\""),
        },
    };
    Ok(kind)
}

/// Multi-select enum: `items.enum` (untitled) or `items.anyOf` const/title
/// entries (titled; `oneOf` accepted as an alias). Any other `items` shape
/// is unsupported rather than a parse error, matching how unknown scalar
/// types degrade.
fn multi_select_from_schema(prop: &Value) -> Result<ElicitFieldKind, String> {
    let Some(items) = prop.get("items") else {
        return Ok(ElicitFieldKind::Unsupported {
            reason: "array without items".into(),
        });
    };
    let options: Vec<ElicitOption> =
        if let Some(values) = items.get("enum").and_then(|e| e.as_array()) {
            values
                .iter()
                .filter_map(|v| {
                    let value = json_scalar_to_string(v)?;
                    Some(ElicitOption {
                        label: value.clone(),
                        value,
                    })
                })
                .collect()
        } else if let Some(entries) = items
            .get("anyOf")
            .or_else(|| items.get("oneOf"))
            .and_then(|o| o.as_array())
        {
            const_title_options(entries)
        } else {
            return Ok(ElicitFieldKind::Unsupported {
                reason: "array without enum items".into(),
            });
        };
    if options.is_empty() {
        return Ok(ElicitFieldKind::Unsupported {
            reason: "array without enum items".into(),
        });
    }
    check_options(&options)?;

    let default_indexes = prop
        .get("default")
        .and_then(|d| d.as_array())
        .map(|defaults| {
            defaults
                .iter()
                .filter_map(|d| d.as_str())
                .filter_map(|d| options.iter().position(|o| o.value == d))
                .collect()
        })
        .unwrap_or_default();

    Ok(ElicitFieldKind::MultiSelect {
        options,
        min_items: prop.get("minItems").and_then(|v| v.as_u64()),
        max_items: prop.get("maxItems").and_then(|v| v.as_u64()),
        default_indexes,
    })
}

fn const_title_options(entries: &[Value]) -> Vec<ElicitOption> {
    entries
        .iter()
        .filter_map(|entry| {
            let value = entry.get("const").and_then(|c| c.as_str())?.to_string();
            let label = entry
                .get("title")
                .and_then(|t| t.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| value.clone());
            Some(ElicitOption { value, label })
        })
        .collect()
}

fn json_scalar_to_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn default_option_index(options: &[ElicitOption], default: Option<&str>) -> Option<usize> {
    default.and_then(|d| options.iter().position(|o| o.value == d))
}

/// An `integer` field's schema bound, taken losslessly when the schema
/// gives an integer. A fractional bound (legal JSON Schema) is tightened
/// inward to the nearest satisfiable integer.
fn integer_bound(v: Option<&Value>, lower: bool) -> Option<i64> {
    let v = v?;
    if let Some(i) = v.as_i64() {
        return Some(i);
    }
    let f = v.as_f64()?;
    let tightened = if lower { f.ceil() } else { f.floor() };
    if tightened >= i64::MIN as f64 && tightened <= i64::MAX as f64 {
        Some(tightened as i64)
    } else {
        None
    }
}

fn check_options(options: &[ElicitOption]) -> Result<(), String> {
    if options.len() > MAX_ELICIT_ENUM_VALUES {
        return Err(format!(
            "requestedSchema enum exceeds {MAX_ELICIT_ENUM_VALUES} values"
        ));
    }
    if options.iter().any(|o| {
        !chars_within(&o.value, MAX_ELICIT_ENUM_VALUE_CHARS)
            || !chars_within(&o.label, MAX_ELICIT_ENUM_VALUE_CHARS)
    }) {
        return Err(format!(
            "requestedSchema enum value exceeds {MAX_ELICIT_ENUM_VALUE_CHARS} characters"
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "schema_tests.rs"]
mod tests;
