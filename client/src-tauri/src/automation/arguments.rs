//! Bounded transport normalization only for declared parameter types.
//! No text, references, booleans, or execution authority is inferred here.
use serde_json::{Number, Value};

pub(super) fn number(value: &Value) -> Option<Number> {
    if let Some(number) = value.as_number() {
        return Some(number.clone());
    }
    let text = value.as_str()?;
    if text.is_empty() || text.len() > 64 || text.trim() != text {
        return None;
    }
    serde_json::from_str::<Number>(text).ok()
}

pub(super) fn string_array(value: &Value, max: usize) -> Result<Vec<String>, &'static str> {
    let decoded;
    let value = if let Some(text) = value.as_str() {
        if text.len() > 65536 || !text.trim_start().starts_with('[') {
            return Err("must be a JSON string array");
        }
        decoded = serde_json::from_str::<Value>(text).map_err(|_| "must be a JSON string array")?;
        &decoded
    } else {
        value
    };
    // XML-based providers encode repeated <item> fields as this exact wrapper.
    // Accept one transport layer only; never coerce arbitrary objects or values.
    let singleton;
    let value = if let Some(object) = value.as_object() {
        if object.len() != 1 {
            return Err("must be a string array");
        }
        let item = object.get("item").ok_or("must be a string array")?;
        if let Some(text) = item.as_str() {
            singleton = serde_json::json!([text]);
            &singleton
        } else {
            item
        }
    } else {
        value
    };
    let array = value.as_array().ok_or("must be a string array")?;
    if array.len() > max {
        return Err("too many array items");
    }
    array
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or("array items must be strings")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn provider_encoded_arrays_are_decoded_once_without_guessing_or_relaxing_limits() {
        assert_eq!(
            string_array(&json!("[\"receipt-a\",\"receipt-b\"]"), 2).unwrap(),
            vec!["receipt-a", "receipt-b"]
        );
        assert_eq!(string_array(&json!(["cmd"]), 4).unwrap(), vec!["cmd"]);
        assert_eq!(
            string_array(&json!({"item":["a","b"]}), 2).unwrap(),
            vec!["a", "b"]
        );
        assert_eq!(string_array(&json!({"item":"a"}), 2).unwrap(), vec!["a"]);
        for bad in [
            json!("a,b"),
            json!("\"[\\\"id\\\"]\""),
            json!([12]),
            json!({"id":"a"}),
            json!({"item":{"item":"a"}}),
            json!({"item":[12]}),
            json!({"item":"a","other":[]}),
            json!(["a", "b", "c"]),
        ] {
            assert!(string_array(&bad, 2).is_err(), "{bad}");
        }
    }
    #[test]
    fn numeric_transport_strings_preserve_coordinates_signs_and_integer_ranges() {
        for (raw, expected) in [
            ("261", 261.0),
            ("261.0", 261.0),
            ("-12.5", -12.5),
            ("1e2", 100.0),
        ] {
            assert_eq!(number(&json!(raw)).and_then(|n| n.as_f64()), Some(expected));
        }
        assert_eq!(number(&json!(261)).unwrap().as_f64(), Some(261.0));
        assert_eq!(
            number(&json!("18446744073709551615")).unwrap().as_u64(),
            Some(u64::MAX)
        );
        assert!(number(&json!("1.5")).unwrap().as_u64().is_none());
        assert!(number(&json!("-1")).unwrap().as_u64().is_none());
    }
    #[test]
    fn malformed_nonfinite_and_non_numeric_transport_values_stay_invalid() {
        for raw in [
            json!(true),
            json!(null),
            json!({"item":"261"}),
            json!([261]),
            json!("NaN"),
            json!("Infinity"),
            json!("1e999"),
            json!(""),
            json!(" 2"),
            json!("2px"),
            json!("0x20"),
            json!("+2"),
            json!("01"),
            json!("2;open"),
        ] {
            assert!(number(&raw).is_none(), "{raw}");
        }
    }
}
