//! Truncation helpers for the external OTEL stream.
//!
//! Constants follow common customer-pipeline parity values: strings
//! longer than 512 chars collapse to their first 128 chars plus a marker,
//! tool-input JSON is capped at 4 KB / depth 2 / 20 items per collection, and
//! gated prompt/content text is capped at 60 KB.

/// Values longer than this are truncated…
pub const MAX_STRING_LEN: usize = 512;
/// …to their first 128 chars + [`TRUNCATION_MARKER`].
pub const TRUNCATED_PREFIX_LEN: usize = 128;
/// Marker appended to truncated strings.
pub const TRUNCATION_MARKER: &str = "…[truncated]";
/// Total serialized-JSON budget for gated tool parameters.
pub const MAX_TOOL_INPUT_JSON_BYTES: usize = 4 * 1024;
/// Maximum JSON nesting depth preserved in gated tool parameters.
pub const MAX_JSON_DEPTH: usize = 2;
/// Maximum items preserved per JSON array/object in gated tool parameters.
pub const MAX_COLLECTION_ITEMS: usize = 20;
/// Cap for gated prompt/content text.
pub const MAX_CONTENT_BYTES: usize = 60 * 1024;
/// File-extension attribute cap (`"rs"`, `"tsx"`, …).
pub const MAX_FILE_EXTENSION_LEN: usize = 10;

/// Truncate on a `char` boundary at or before `max_bytes`.
fn floor_char_boundary(s: &str, max_bytes: usize) -> usize {
    if max_bytes >= s.len() {
        return s.len();
    }
    let mut idx = max_bytes;
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

/// Standard attribute-value truncation: strings whose `char` count exceeds
/// [`MAX_STRING_LEN`] collapse to their first [`TRUNCATED_PREFIX_LEN`] chars
/// plus [`TRUNCATION_MARKER`]. Returns `None` when unchanged.
pub fn truncate_value(s: &str) -> Option<String> {
    // Counting chars (not bytes) keeps the limit stable for non-ASCII text;
    // chars_count > MAX_STRING_LEN implies the string is "long" regardless of
    // encoding width.
    if s.chars().count() <= MAX_STRING_LEN {
        return None;
    }
    let truncated: String = s.chars().take(TRUNCATED_PREFIX_LEN).collect();
    Some(format!("{truncated}{TRUNCATION_MARKER}"))
}

/// Apply [`truncate_value`], returning an owned string either way.
pub fn truncate_value_owned(s: String) -> String {
    truncate_value(&s).unwrap_or(s)
}

/// Cap gated prompt/content text at [`MAX_CONTENT_BYTES`] (UTF-8-safe).
pub fn truncate_content(s: &str) -> Option<String> {
    if s.len() <= MAX_CONTENT_BYTES {
        return None;
    }
    let idx = floor_char_boundary(s, MAX_CONTENT_BYTES);
    Some(format!("{}{TRUNCATION_MARKER}", &s[..idx]))
}

/// Reduce a JSON value for the gated `tool_parameters` attribute: depth
/// capped at [`MAX_JSON_DEPTH`], collections capped at
/// [`MAX_COLLECTION_ITEMS`] entries, strings truncated per [`truncate_value`].
/// The serialized result is finally clamped to [`MAX_TOOL_INPUT_JSON_BYTES`].
pub fn reduce_tool_input(value: &serde_json::Value) -> String {
    let reduced = reduce_json(value, 0);
    let serialized = reduced.to_string();
    if serialized.len() <= MAX_TOOL_INPUT_JSON_BYTES {
        return serialized;
    }
    // Over budget even after structural reduction: clamp the serialized text.
    // The result may not be valid JSON, but it is bounded and marked.
    let idx = floor_char_boundary(&serialized, MAX_TOOL_INPUT_JSON_BYTES);
    format!("{}{TRUNCATION_MARKER}", &serialized[..idx])
}

fn reduce_json(value: &serde_json::Value, depth: usize) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::String(s) => Value::String(truncate_value(s).unwrap_or_else(|| s.clone())),
        Value::Array(items) => {
            if depth >= MAX_JSON_DEPTH {
                return Value::String(format!("[array:{}]", items.len()));
            }
            items
                .iter()
                .take(MAX_COLLECTION_ITEMS)
                .map(|v| reduce_json(v, depth + 1))
                .collect()
        }
        Value::Object(map) => {
            if depth >= MAX_JSON_DEPTH {
                return Value::String(format!("{{object:{}}}", map.len()));
            }
            map.iter()
                .take(MAX_COLLECTION_ITEMS)
                .map(|(k, v)| (k.clone(), reduce_json(v, depth + 1)))
                .collect()
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_strings_pass_unchanged() {
        assert_eq!(truncate_value("hello"), None);
        let exactly_max: String = "a".repeat(MAX_STRING_LEN);
        assert_eq!(truncate_value(&exactly_max), None);
    }

    #[test]
    fn long_strings_collapse_to_prefix_plus_marker() {
        let long: String = "x".repeat(MAX_STRING_LEN + 1);
        let out = truncate_value(&long).expect("must truncate");
        assert!(out.starts_with(&"x".repeat(TRUNCATED_PREFIX_LEN)));
        assert!(out.ends_with(TRUNCATION_MARKER));
        assert_eq!(
            out.chars().count(),
            TRUNCATED_PREFIX_LEN + TRUNCATION_MARKER.chars().count()
        );
    }

    #[test]
    fn truncation_is_utf8_boundary_safe() {
        // Multi-byte chars around both limits must not split a char.
        let long: String = "é".repeat(MAX_STRING_LEN + 5);
        let out = truncate_value(&long).expect("must truncate");
        assert!(out.starts_with(&"é".repeat(TRUNCATED_PREFIX_LEN)));

        let content: String = "🎉".repeat(MAX_CONTENT_BYTES / 4 + 10);
        let out = truncate_content(&content).expect("must truncate");
        assert!(out.len() <= MAX_CONTENT_BYTES + TRUNCATION_MARKER.len());
        // Round-trip as a str: would panic at construction if we split a char.
        assert!(out.ends_with(TRUNCATION_MARKER));
    }

    #[test]
    fn content_under_cap_passes() {
        assert_eq!(truncate_content("short prompt"), None);
    }

    #[test]
    fn tool_input_depth_capped() {
        let v = serde_json::json!({"a": {"b": {"c": {"d": 1}}}});
        let out = reduce_tool_input(&v);
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        // Depth 0 = root object, depth 1 = a's object; b's value is at depth 2 → collapsed.
        assert_eq!(parsed["a"]["b"], serde_json::json!("{object:1}"));
    }

    #[test]
    fn tool_input_collection_items_capped() {
        let items: Vec<serde_json::Value> = (0..50).map(|i| serde_json::json!(i)).collect();
        let v = serde_json::Value::Array(items);
        let out = reduce_tool_input(&v);
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed.as_array().unwrap().len(), MAX_COLLECTION_ITEMS);
    }

    #[test]
    fn tool_input_total_budget_enforced() {
        let big: String = "y".repeat(300);
        let map: serde_json::Map<String, serde_json::Value> = (0..MAX_COLLECTION_ITEMS)
            .map(|i| (format!("key_{i:02}"), serde_json::json!(big.clone())))
            .collect();
        let v = serde_json::json!({"a": map.clone(), "b": map});
        let out = reduce_tool_input(&v);
        assert!(out.len() <= MAX_TOOL_INPUT_JSON_BYTES + TRUNCATION_MARKER.len());
    }

    #[test]
    fn tool_input_strings_truncated_inside_json() {
        let v = serde_json::json!({"text": "z".repeat(MAX_STRING_LEN + 1)});
        let out = reduce_tool_input(&v);
        assert!(out.contains(TRUNCATION_MARKER));
        assert!(!out.contains(&"z".repeat(MAX_STRING_LEN + 1)));
    }
}
