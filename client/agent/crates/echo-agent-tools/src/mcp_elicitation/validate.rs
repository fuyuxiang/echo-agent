//! Pure validation of submitted elicitation form values against parsed
//! [`ElicitFieldSpec`]s. Schema parsing lives in [`super::schema`].

use serde_json::{Map, Value};

use super::schema::{ElicitFieldKind, ElicitFieldSpec, ElicitTextFormat};

/// The user's submitted value for one field, parallel to a
/// [`ElicitFieldSpec`]. Selections are indexes into the spec's options.
#[derive(Debug, Clone)]
pub enum ElicitFieldValue<'a> {
    /// String / Number / Integer fields: the raw text draft. An empty
    /// draft means "not provided"; anything else is validated and
    /// submitted **verbatim** — JSON Schema string values and length
    /// constraints do not trim whitespace.
    Draft(&'a str),
    Bool(bool),
    Choice(Option<usize>),
    /// Selected option indexes of a multi-select, in option order.
    MultiChoice(&'a [usize]),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormValidationError {
    pub field: String,
    pub message: String,
}

/// Validate one submitted form. `values` is parallel to `specs` (a missing
/// entry is treated as empty). Returns the accepted `content` object or the
/// per-field errors; pure — display state stays with the caller.
pub fn validate_form(
    specs: &[ElicitFieldSpec],
    values: &[ElicitFieldValue<'_>],
) -> Result<Map<String, Value>, Vec<FormValidationError>> {
    let mut content = Map::new();
    let mut errors = Vec::new();
    for (i, spec) in specs.iter().enumerate() {
        let value = values
            .get(i)
            .cloned()
            .unwrap_or(ElicitFieldValue::Draft(""));
        match validate_field(spec, &value) {
            Ok(Some(v)) => {
                content.insert(spec.name.clone(), v);
            }
            Ok(None) => {}
            Err(message) => errors.push(FormValidationError {
                field: spec.name.clone(),
                message,
            }),
        }
    }
    if errors.is_empty() {
        Ok(content)
    } else {
        Err(errors)
    }
}

/// Validate one field. `Ok(None)` means "omit from content" (empty and not
/// required).
pub fn validate_field(
    spec: &ElicitFieldSpec,
    value: &ElicitFieldValue<'_>,
) -> Result<Option<Value>, String> {
    match (&spec.kind, value) {
        (ElicitFieldKind::Unsupported { .. }, _) => {
            if spec.required {
                Err("unsupported field type".into())
            } else {
                Ok(None)
            }
        }
        (ElicitFieldKind::Boolean { .. }, ElicitFieldValue::Bool(b)) => Ok(Some(Value::Bool(*b))),
        (ElicitFieldKind::SingleSelect { options, .. }, ElicitFieldValue::Choice(choice)) => {
            match choice.and_then(|i| options.get(i)) {
                Some(option) => Ok(Some(Value::String(option.value.clone()))),
                None if spec.required => Err("required".into()),
                None => Ok(None),
            }
        }
        (
            ElicitFieldKind::MultiSelect {
                options,
                min_items,
                max_items,
                ..
            },
            ElicitFieldValue::MultiChoice(selected),
        ) => {
            let values: Vec<Value> = selected
                .iter()
                .filter_map(|&i| options.get(i))
                .map(|o| Value::String(o.value.clone()))
                .collect();
            if let Some(min) = *min_items
                && (values.len() as u64) < min
            {
                return Err(format!("select at least {min}"));
            }
            if let Some(max) = *max_items
                && (values.len() as u64) > max
            {
                return Err(format!("select at most {max}"));
            }
            // JSON Schema semantics (reviewer-confirmed): `required` only
            // demands the property be present and `minItems` defaults to 0,
            // so an empty required multi-select submits `[]`. Only an
            // optional field with nothing selected is omitted.
            if values.is_empty() && !spec.required {
                return Ok(None);
            }
            Ok(Some(Value::Array(values)))
        }
        (
            ElicitFieldKind::String {
                format,
                min_length,
                max_length,
                ..
            },
            ElicitFieldValue::Draft(draft),
        ) => {
            // The draft is validated and submitted exactly as typed: JSON
            // Schema does not trim, so whitespace counts toward length
            // constraints and is part of the accepted content.
            if draft.is_empty() {
                return if spec.required {
                    Err("required".into())
                } else {
                    Ok(None)
                };
            }
            if let Some(min) = *min_length
                && (draft.chars().count() as u64) < min
            {
                return Err(format!("min length {min}"));
            }
            if let Some(max) = *max_length
                && (draft.chars().count() as u64) > max
            {
                return Err(format!("max length {max}"));
            }
            if let Some(fmt) = format
                && let Some(msg) = validate_text_format(*fmt, draft)
            {
                return Err(msg);
            }
            Ok(Some(Value::String(draft.to_string())))
        }
        (
            ElicitFieldKind::Integer {
                minimum, maximum, ..
            },
            ElicitFieldValue::Draft(draft),
        ) => {
            // Numeric fields submit the parsed number, not the text, so
            // surrounding whitespace is a tolerated input artifact here.
            let s = draft.trim();
            if s.is_empty() {
                return if spec.required {
                    Err("required".into())
                } else {
                    Ok(None)
                };
            }
            // Lossless: never routed through `f64`, so 1e20 is rejected
            // instead of silently saturating and values above 2^53 keep
            // every digit.
            let Ok(n) = s.parse::<i64>() else {
                return Err("must be an integer".into());
            };
            if let Some(min) = *minimum
                && n < min
            {
                return Err(format!("min {min}"));
            }
            if let Some(max) = *maximum
                && n > max
            {
                return Err(format!("max {max}"));
            }
            Ok(Some(Value::Number(serde_json::Number::from(n))))
        }
        (
            ElicitFieldKind::Number {
                minimum, maximum, ..
            },
            ElicitFieldValue::Draft(draft),
        ) => {
            let s = draft.trim();
            if s.is_empty() {
                return if spec.required {
                    Err("required".into())
                } else {
                    Ok(None)
                };
            }
            let Ok(n) = s.parse::<f64>() else {
                return Err("invalid number".into());
            };
            if let Some(min) = *minimum
                && n < min
            {
                return Err(format!("min {min}"));
            }
            if let Some(max) = *maximum
                && n > max
            {
                return Err(format!("max {max}"));
            }
            match serde_json::Number::from_f64(n) {
                Some(num) => Ok(Some(Value::Number(num))),
                None => Err("invalid number".into()),
            }
        }
        // A value of the wrong shape for the field (caller bug): surface as
        // a validation error instead of silently accepting or panicking.
        _ => Err("invalid value".into()),
    }
}

fn validate_text_format(format: ElicitTextFormat, s: &str) -> Option<String> {
    match format {
        ElicitTextFormat::Email => {
            if is_plausible_email(s) {
                None
            } else {
                Some("invalid email".into())
            }
        }
        ElicitTextFormat::Uri => {
            // Any absolute URI (scheme required), not just http(s):
            // `urn:`, `mailto:`, `ftp:` etc. are all valid `format: uri`.
            if url::Url::parse(s).is_ok() {
                None
            } else {
                Some("invalid URI".into())
            }
        }
        ElicitTextFormat::Date => {
            // RFC 3339 full-date: zero-padded and calendar-valid.
            let padded = s.len() == 10;
            if padded && chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok() {
                None
            } else {
                Some("use YYYY-MM-DD".into())
            }
        }
        ElicitTextFormat::DateTime => {
            if chrono::DateTime::parse_from_rfc3339(s).is_ok() {
                None
            } else {
                Some("use RFC 3339 date-time".into())
            }
        }
    }
}

/// Pragmatic email shape check: one `@`, a non-empty local part without
/// whitespace, and a hostname-shaped domain with at least two labels.
fn is_plausible_email(s: &str) -> bool {
    let Some((local, domain)) = s.split_once('@') else {
        return false;
    };
    if local.is_empty()
        || local.chars().count() > 64
        || local.chars().any(|c| c.is_whitespace() || c == '@')
    {
        return false;
    }
    let labels: Vec<&str> = domain.split('.').collect();
    labels.len() >= 2
        && labels.iter().all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
}

#[cfg(test)]
#[path = "validate_tests.rs"]
mod tests;
