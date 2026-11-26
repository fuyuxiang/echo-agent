//! Helper schema types for JSON Schema generation.
use serde::Deserialize;
/// Schema helper for integers - produces clean integer schema without extra fields.
/// By default schemars adds "format": "uint" and "minimum": 0.0 which we don't want.
///
/// Use with `#[schemars(with = "EchoAgentIntegerSchema")]` on `Option<usize>` fields.
pub struct EchoAgentIntegerSchema;
impl schemars::JsonSchema for EchoAgentIntegerSchema {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "echo_agent_integer_schema".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({ "type": "integer" })
    }
}
/// Largest whole value exactly representable as `f64` (2^53). JSON floats above this
/// cannot be converted to integers without rounding ambiguity.
const F64_EXACT_INTEGER_LIMIT: f64 = 9_007_199_254_740_992.0;
/// Parse a string as `f64`, producing a uniform error message on failure.
fn parse_string_to_f64(s: &str) -> Result<f64, String> {
    s.parse()
        .map_err(|_| format!("expected number, got string \"{s}\""))
}
/// Parse a whole `f64` (positive or negative) into `i64`. Rejects non-finite,
/// fractional, and values outside the exact-integer precision range.
fn parse_lenient_whole_f64(f: f64) -> Result<i64, String> {
    if !f.is_finite() {
        return Err("expected finite number".into());
    }
    if f == 0.0 {
        return Ok(0);
    }
    if f.fract() != 0.0 {
        return Err(format!("expected whole number, got {f}"));
    }
    if f.abs() > F64_EXACT_INTEGER_LIMIT {
        return Err(format!(
            "number {f} exceeds f64 integer precision (whole floats above {F64_EXACT_INTEGER_LIMIT} may be inaccurate)"
        ));
    }
    if f > i64::MAX as f64 || f < i64::MIN as f64 {
        return Err("number out of range for i64".into());
    }
    Ok(f as i64)
}
fn parse_lenient_u64_value(value: &serde_json::Value) -> Result<u64, String> {
    match value {
        serde_json::Value::Number(n) => {
            if let Some(u) = n.as_u64() {
                return Ok(u);
            }
            if let Some(i) = n.as_i64() {
                if i < 0 {
                    return Err("expected non-negative number".into());
                }
                return u64::try_from(i).map_err(|_| "number out of range for u64".into());
            }
            if let Some(f) = n.as_f64() {
                let i = parse_lenient_whole_f64(f)?;
                return u64::try_from(i).map_err(|_| "expected non-negative number".to_string());
            }
            Err("expected number, got invalid numeric representation".into())
        }
        serde_json::Value::String(s) => {
            let i = parse_lenient_whole_f64(parse_string_to_f64(s)?)?;
            u64::try_from(i).map_err(|_| "expected non-negative number".to_string())
        }
        other => Err(format!("expected number, got {other}")),
    }
}
fn deserialize_lenient_option_u64<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    match value {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(v) => parse_lenient_u64_value(&v)
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}
/// Deserialize `Option<u32>` from a JSON number or numeric string (integers or whole floats).
pub fn deserialize_lenient_u32<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_lenient_option_u64(deserializer)?
        .map(|u| {
            u32::try_from(u).map_err(|_| serde::de::Error::custom("number out of range for u32"))
        })
        .transpose()
}
/// Deserialize `Option<u64>` from a JSON number or numeric string (integers or whole floats).
pub fn deserialize_lenient_u64<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_lenient_option_u64(deserializer)
}
/// Deserialize required `usize` from a JSON number or numeric string (integers or whole floats).
pub fn deserialize_lenient_usize<'de, D>(deserializer: D) -> Result<usize, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    let u = parse_lenient_u64_value(&value).map_err(serde::de::Error::custom)?;
    usize::try_from(u)
        .map_err(|_| serde::de::Error::custom(format!("number out of range for usize: {u}")))
}
/// Parse a JSON value as a signed `i64`, accepting numbers, whole floats, and string forms.
///
/// Unlike [`parse_lenient_u64_value`], this allows negative values.
fn parse_lenient_i64_value(value: &serde_json::Value) -> Result<i64, String> {
    match value {
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                return Ok(i);
            }
            if let Some(f) = n.as_f64() {
                return parse_lenient_whole_f64(f);
            }
            Err("expected number, got invalid numeric representation".into())
        }
        serde_json::Value::String(s) => parse_lenient_whole_f64(parse_string_to_f64(s)?),
        other => Err(format!("expected number, got {other}")),
    }
}
/// Deserialize `Option<i64>` from a JSON number or numeric string (integers, whole floats, or string forms).
pub fn deserialize_lenient_i64<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    match value {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(v) => parse_lenient_i64_value(&v)
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}
/// Parse a JSON value as a finite `f64`, accepting numbers (fractional
/// allowed) and numeric string forms.
fn parse_lenient_f64_value(value: &serde_json::Value) -> Result<f64, String> {
    let f = match value {
        serde_json::Value::Number(n) => n
            .as_f64()
            .ok_or("expected number, got invalid numeric representation".to_string())?,
        serde_json::Value::String(s) => parse_string_to_f64(s)?,
        other => return Err(format!("expected number, got {other}")),
    };
    if !f.is_finite() {
        return Err("expected finite number".into());
    }
    Ok(f)
}
/// Deserialize `Option<f64>` from a JSON number or numeric string.
/// Fractional values are allowed (unlike the lenient integer deserializers).
pub fn deserialize_lenient_f64<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    match value {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(v) => parse_lenient_f64_value(&v)
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}
/// Deserialize `Option<String>` from a JSON string, number, or boolean —
/// scalar values are coerced to their string form. Mirrors zod's
/// `z.coerce.string()` used by the TypeScript echo-agent-computer tools, where
/// models routinely send numeric-looking IDs (e.g. CDP request IDs such as
/// `62576.34`) as JSON numbers.
pub fn deserialize_lenient_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    match value {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(s)) => Ok(Some(s)),
        Some(serde_json::Value::Number(n)) => Ok(Some(n.to_string())),
        Some(serde_json::Value::Bool(b)) => Ok(Some(b.to_string())),
        Some(other) => Err(serde::de::Error::custom(format!(
            "expected string, got {other}"
        ))),
    }
}
/// Lenient boolean deserializers (shared via `echo-agent-tool-types`), re-exported so
/// fields reference them under the same `crate::types::schema::` path as above.
pub use echo_agent_tool_types::{deserialize_lenient_bool, deserialize_lenient_option_bool};
#[cfg(test)]
mod tests {
    use super::*;
    fn deserialize_u32(json: &str) -> Result<Option<u32>, serde_json::Error> {
        #[derive(Deserialize)]
        struct Wrapper {
            #[serde(default, deserialize_with = "deserialize_lenient_u32")]
            value: Option<u32>,
        }
        let w: Wrapper = serde_json::from_str(json)?;
        Ok(w.value)
    }
    fn deserialize_u64(json: &str) -> Result<Option<u64>, serde_json::Error> {
        #[derive(Deserialize)]
        struct Wrapper {
            #[serde(default, deserialize_with = "deserialize_lenient_u64")]
            value: Option<u64>,
        }
        let w: Wrapper = serde_json::from_str(json)?;
        Ok(w.value)
    }
    fn deserialize_usize(json: &str) -> Result<usize, serde_json::Error> {
        #[derive(Deserialize)]
        struct Wrapper {
            #[serde(deserialize_with = "deserialize_lenient_usize")]
            value: usize,
        }
        let w: Wrapper = serde_json::from_str(json)?;
        Ok(w.value)
    }
    #[test]
    fn u32_accepts_whole_float() {
        assert_eq!(deserialize_u32(r#"{"value":80.0}"#).unwrap(), Some(80));
    }
    #[test]
    fn u32_accepts_integer() {
        assert_eq!(deserialize_u32(r#"{"value":80}"#).unwrap(), Some(80));
    }
    #[test]
    fn u32_null_is_none() {
        assert_eq!(deserialize_u32(r#"{"value":null}"#).unwrap(), None);
    }
    #[test]
    fn u32_missing_is_none() {
        assert_eq!(deserialize_u32(r#"{}"#).unwrap(), None);
    }
    #[test]
    fn u32_rejects_fractional_float() {
        let err = deserialize_u32(r#"{"value":80.5}"#).unwrap_err();
        assert!(err.to_string().contains("whole number"));
    }
    #[test]
    fn u32_rejects_negative() {
        let err = deserialize_u32(r#"{"value":-1}"#).unwrap_err();
        assert!(err.to_string().contains("non-negative"));
    }
    #[test]
    fn u32_rejects_above_max() {
        let err = deserialize_u32(&format!(r#"{{"value":{}}}"#, u32::MAX as u64 + 1)).unwrap_err();
        assert!(err.to_string().contains("out of range for u32"));
    }
    #[test]
    fn u64_accepts_whole_float() {
        assert_eq!(
            deserialize_u64(r#"{"value":30000.0}"#).unwrap(),
            Some(30_000)
        );
    }
    #[test]
    fn u64_accepts_integer() {
        assert_eq!(deserialize_u64(r#"{"value":5000}"#).unwrap(), Some(5000));
    }
    #[test]
    fn u64_null_is_none() {
        assert_eq!(deserialize_u64(r#"{"value":null}"#).unwrap(), None);
    }
    #[test]
    fn u64_missing_is_none() {
        assert_eq!(deserialize_u64(r#"{}"#).unwrap(), None);
    }
    #[test]
    fn u64_rejects_fractional_float() {
        let err = deserialize_u64(r#"{"value":30000.5}"#).unwrap_err();
        assert!(err.to_string().contains("whole number"));
    }
    #[test]
    fn u64_rejects_negative() {
        let err = deserialize_u64(r#"{"value":-1}"#).unwrap_err();
        assert!(err.to_string().contains("non-negative"));
    }
    #[test]
    fn u64_rejects_above_max() {
        let err = deserialize_u64(r#"{"value":1e20}"#).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("out of range for u64") || msg.contains("f64 integer precision"),
            "unexpected error: {msg}"
        );
    }
    #[test]
    fn u32_rejects_negative_float() {
        let err = deserialize_u32(r#"{"value":-1.0}"#).unwrap_err();
        assert!(err.to_string().contains("non-negative"));
    }
    #[test]
    fn u64_rejects_negative_float() {
        let err = deserialize_u64(r#"{"value":-1.0}"#).unwrap_err();
        assert!(err.to_string().contains("non-negative"));
    }
    #[test]
    fn u32_accepts_string_integer() {
        assert_eq!(deserialize_u32(r#"{"value":"80"}"#).unwrap(), Some(80));
    }
    #[test]
    fn u32_accepts_string_whole_float() {
        assert_eq!(deserialize_u32(r#"{"value":"120.0"}"#).unwrap(), Some(120));
    }
    #[test]
    fn u32_rejects_non_numeric_string() {
        let err = deserialize_u32(r#"{"value":"abc"}"#).unwrap_err();
        assert!(err.to_string().contains("expected number"));
    }
    #[test]
    fn u64_accepts_string_whole_float() {
        assert_eq!(
            deserialize_u64(r#"{"value":"30000.0"}"#).unwrap(),
            Some(30_000)
        );
    }
    #[test]
    fn u32_rejects_string_fractional_float() {
        let err = deserialize_u32(r#"{"value":"80.5"}"#).unwrap_err();
        assert!(err.to_string().contains("whole number"));
    }
    #[test]
    fn u32_rejects_string_negative() {
        let err = deserialize_u32(r#"{"value":"-1"}"#).unwrap_err();
        assert!(err.to_string().contains("non-negative"));
    }
    #[test]
    fn rejects_non_finite_float() {
        assert!(
            parse_lenient_whole_f64(f64::NAN)
                .unwrap_err()
                .contains("finite")
        );
        assert!(
            parse_lenient_whole_f64(f64::INFINITY)
                .unwrap_err()
                .contains("finite")
        );
    }
    #[test]
    fn u32_rejects_float_above_f64_integer_precision() {
        let err = deserialize_u32(r#"{"value":10000000000000000.0}"#).unwrap_err();
        assert!(err.to_string().contains("f64 integer precision"));
    }
    #[test]
    fn usize_rejects_negative() {
        let err = deserialize_usize(r#"{"value":-1}"#).unwrap_err();
        assert!(err.to_string().contains("non-negative"));
    }
    #[test]
    fn usize_rejects_fractional() {
        let err = deserialize_usize(r#"{"value":2.5}"#).unwrap_err();
        assert!(err.to_string().contains("whole number"));
    }
    #[test]
    fn usize_accepts_negative_zero_float() {
        assert_eq!(deserialize_usize(r#"{"value":-0.0}"#).unwrap(), 0);
    }
    fn deserialize_f64(json: &str) -> Result<Option<f64>, serde_json::Error> {
        #[derive(Deserialize)]
        struct Wrapper {
            #[serde(default, deserialize_with = "deserialize_lenient_f64")]
            value: Option<f64>,
        }
        let w: Wrapper = serde_json::from_str(json)?;
        Ok(w.value)
    }
    #[test]
    fn f64_accepts_fractional_float() {
        assert_eq!(deserialize_f64(r#"{"value":2.5}"#).unwrap(), Some(2.5));
    }
    #[test]
    fn f64_accepts_integer() {
        assert_eq!(deserialize_f64(r#"{"value":5}"#).unwrap(), Some(5.0));
    }
    #[test]
    fn f64_accepts_numeric_string() {
        assert_eq!(deserialize_f64(r#"{"value":"0.5"}"#).unwrap(), Some(0.5));
    }
    #[test]
    fn f64_null_and_missing_are_none() {
        assert_eq!(deserialize_f64(r#"{"value":null}"#).unwrap(), None);
        assert_eq!(deserialize_f64(r#"{}"#).unwrap(), None);
    }
    #[test]
    fn f64_rejects_non_numeric_string() {
        let err = deserialize_f64(r#"{"value":"abc"}"#).unwrap_err();
        assert!(err.to_string().contains("expected number"));
    }
    #[test]
    fn f64_rejects_non_finite_string() {
        let err = deserialize_f64(r#"{"value":"NaN"}"#).unwrap_err();
        assert!(err.to_string().contains("finite"));
    }
    fn deserialize_string(json: &str) -> Result<Option<String>, serde_json::Error> {
        #[derive(Deserialize)]
        struct Wrapper {
            #[serde(default, deserialize_with = "deserialize_lenient_string")]
            value: Option<String>,
        }
        let w: Wrapper = serde_json::from_str(json)?;
        Ok(w.value)
    }
    #[test]
    fn string_passes_through() {
        assert_eq!(
            deserialize_string(r#"{"value":"62576.34"}"#).unwrap(),
            Some("62576.34".to_string())
        );
    }
    #[test]
    fn string_coerces_numbers_like_zod() {
        assert_eq!(
            deserialize_string(r#"{"value":62576.34}"#).unwrap(),
            Some("62576.34".to_string())
        );
        assert_eq!(
            deserialize_string(r#"{"value":42}"#).unwrap(),
            Some("42".to_string())
        );
    }
    #[test]
    fn string_coerces_booleans_like_zod() {
        assert_eq!(
            deserialize_string(r#"{"value":true}"#).unwrap(),
            Some("true".to_string())
        );
    }
    #[test]
    fn string_null_and_missing_are_none() {
        assert_eq!(deserialize_string(r#"{"value":null}"#).unwrap(), None);
        assert_eq!(deserialize_string(r#"{}"#).unwrap(), None);
    }
    #[test]
    fn string_rejects_composite_values() {
        let err = deserialize_string(r#"{"value":["a"]}"#).unwrap_err();
        assert!(err.to_string().contains("expected string"));
    }
    fn deserialize_i64(json: &str) -> Result<Option<i64>, serde_json::Error> {
        #[derive(Deserialize)]
        struct Wrapper {
            #[serde(default, deserialize_with = "deserialize_lenient_i64")]
            value: Option<i64>,
        }
        let w: Wrapper = serde_json::from_str(json)?;
        Ok(w.value)
    }
    #[test]
    fn i64_accepts_positive_integer() {
        assert_eq!(deserialize_i64(r#"{"value":42}"#).unwrap(), Some(42));
    }
    #[test]
    fn i64_accepts_negative_integer() {
        assert_eq!(deserialize_i64(r#"{"value":-3}"#).unwrap(), Some(-3));
    }
    #[test]
    fn i64_accepts_whole_float() {
        assert_eq!(deserialize_i64(r#"{"value":100.0}"#).unwrap(), Some(100));
    }
    #[test]
    fn i64_accepts_negative_whole_float() {
        assert_eq!(deserialize_i64(r#"{"value":-10.0}"#).unwrap(), Some(-10));
    }
    #[test]
    fn i64_accepts_string_integer() {
        assert_eq!(deserialize_i64(r#"{"value":"42"}"#).unwrap(), Some(42));
    }
    #[test]
    fn i64_accepts_string_negative() {
        assert_eq!(deserialize_i64(r#"{"value":"-3"}"#).unwrap(), Some(-3));
    }
    #[test]
    fn i64_accepts_string_whole_float() {
        assert_eq!(deserialize_i64(r#"{"value":"120.0"}"#).unwrap(), Some(120));
    }
    #[test]
    fn i64_null_is_none() {
        assert_eq!(deserialize_i64(r#"{"value":null}"#).unwrap(), None);
    }
    #[test]
    fn i64_missing_is_none() {
        assert_eq!(deserialize_i64(r#"{}"#).unwrap(), None);
    }
    #[test]
    fn i64_rejects_fractional_float() {
        let err = deserialize_i64(r#"{"value":2.5}"#).unwrap_err();
        assert!(err.to_string().contains("whole number"));
    }
    #[test]
    fn i64_rejects_non_numeric_string() {
        let err = deserialize_i64(r#"{"value":"abc"}"#).unwrap_err();
        assert!(err.to_string().contains("expected number"));
    }
}
