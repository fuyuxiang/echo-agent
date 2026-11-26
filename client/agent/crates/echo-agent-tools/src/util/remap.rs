//! Utilities for remapping tool/parameter names in JSON values and schemas.

use std::collections::HashMap;

/// Remap top-level keys in a JSON object using a reverse map (model-facing → canonical).
///
/// Used to transform incoming tool input from the model (which may use randomized
/// parameter names) back to canonical names before deserialization.
///
/// Only remaps top-level keys. Nested objects are not affected.
/// Keys not in the map are passed through unchanged.
pub fn remap_json_keys(
    raw: serde_json::Value,
    reverse_map: &HashMap<String, String>,
) -> serde_json::Value {
    match raw {
        serde_json::Value::Object(map) => {
            let mut new_map = serde_json::Map::new();
            for (key, value) in map {
                let canonical = reverse_map.get(&key).cloned().unwrap_or(key);
                new_map.insert(canonical, value);
            }
            serde_json::Value::Object(new_map)
        }
        other => other,
    }
}

/// Build a reverse map (model-facing → canonical) from a canonical → model-facing map.
///
/// Panics in debug mode if two canonical names map to the same model-facing
/// name (collision would silently drop one mapping).
pub fn reverse_map(map: &HashMap<String, String>) -> HashMap<String, String> {
    let reversed: HashMap<_, _> = map.iter().map(|(k, v)| (v.clone(), k.clone())).collect();
    debug_assert_eq!(
        reversed.len(),
        map.len(),
        "tool name map has duplicate model-facing names"
    );
    reversed
}

/// Remap property names in a JSON Schema object.
///
/// Renames keys in the `"properties"` object and updates entries in the
/// `"required"` array according to the given map (canonical → model-facing).
/// Properties/required entries not in the map keep their canonical names.
pub fn remap_schema_properties(
    schema: &serde_json::Value,
    param_map: &HashMap<String, String>,
) -> serde_json::Value {
    if param_map.is_empty() {
        return schema.clone();
    }

    let mut schema = schema.clone();

    // Remap keys in "properties"
    if let Some(serde_json::Value::Object(props)) = schema.get("properties").cloned() {
        let mut new_props = serde_json::Map::new();
        for (key, value) in props {
            let new_key = param_map.get(&key).cloned().unwrap_or(key);
            new_props.insert(new_key, value);
        }
        schema["properties"] = serde_json::Value::Object(new_props);
    }

    // Remap entries in "required" array
    if let Some(serde_json::Value::Array(items)) = schema.get("required").cloned() {
        let new_items: Vec<serde_json::Value> = items
            .into_iter()
            .map(|item| {
                if let serde_json::Value::String(s) = &item
                    && let Some(mapped) = param_map.get(s.as_str())
                {
                    return serde_json::Value::String(mapped.clone());
                }
                item
            })
            .collect();
        schema["required"] = serde_json::Value::Array(new_items);
    }

    schema
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remap_json_keys_basic() {
        let raw = serde_json::json!({"find": "old", "replace_with": "new"});
        let reverse = HashMap::from([
            ("find".to_string(), "old_string".to_string()),
            ("replace_with".to_string(), "new_string".to_string()),
        ]);
        let result = remap_json_keys(raw, &reverse);
        assert_eq!(result["old_string"], "old");
        assert_eq!(result["new_string"], "new");
    }

    #[test]
    fn remap_json_keys_unmapped_passthrough() {
        let raw = serde_json::json!({"file_path": "test.rs", "unknown": true});
        let reverse = HashMap::from([("find".to_string(), "old_string".to_string())]);
        let result = remap_json_keys(raw, &reverse);
        assert_eq!(result["file_path"], "test.rs");
        assert_eq!(result["unknown"], true);
    }

    #[test]
    fn remap_json_keys_empty_map() {
        let raw = serde_json::json!({"old_string": "x"});
        let result = remap_json_keys(raw.clone(), &HashMap::new());
        assert_eq!(result, raw);
    }

    #[test]
    fn remap_json_keys_non_object_passthrough() {
        let raw = serde_json::json!("just a string");
        let result = remap_json_keys(raw.clone(), &HashMap::from([("a".into(), "b".into())]));
        assert_eq!(result, raw);

        let raw_arr = serde_json::json!([1, 2, 3]);
        let result = remap_json_keys(raw_arr.clone(), &HashMap::from([("a".into(), "b".into())]));
        assert_eq!(result, raw_arr);
    }

    #[test]
    fn reverse_map_basic() {
        let map = HashMap::from([
            ("old_string".to_string(), "find".to_string()),
            ("new_string".to_string(), "replace_with".to_string()),
        ]);
        let rev = reverse_map(&map);
        assert_eq!(rev.get("find").unwrap(), "old_string");
        assert_eq!(rev.get("replace_with").unwrap(), "new_string");
    }

    #[test]
    fn remap_schema_properties_basic() {
        let schema = serde_json::json!({
            "properties": {
                "old_string": {"type": "string"},
                "new_string": {"type": "string"},
                "file_path": {"type": "string"},
            },
            "required": ["file_path", "old_string", "new_string"]
        });
        let param_map = HashMap::from([
            ("old_string".to_string(), "find".to_string()),
            ("new_string".to_string(), "replace_with".to_string()),
        ]);
        let result = remap_schema_properties(&schema, &param_map);
        // Properties remapped
        assert!(result["properties"]["find"].is_object());
        assert!(result["properties"]["replace_with"].is_object());
        assert!(result["properties"]["file_path"].is_object());
        assert!(result["properties"].get("old_string").is_none());
        // Required array remapped
        let required: Vec<String> = result["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert!(required.contains(&"find".to_string()));
        assert!(required.contains(&"replace_with".to_string()));
        assert!(required.contains(&"file_path".to_string()));
        assert!(!required.contains(&"old_string".to_string()));
    }

    #[test]
    fn remap_schema_properties_empty_map() {
        let schema = serde_json::json!({"properties": {"x": {"type": "string"}}});
        let result = remap_schema_properties(&schema, &HashMap::new());
        assert_eq!(result, schema);
    }
}
