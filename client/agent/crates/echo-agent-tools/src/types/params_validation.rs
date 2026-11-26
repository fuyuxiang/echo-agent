use serde::de::DeserializeOwned;

use crate::types::resources::ResourceType;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ParamValidationError {
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bad_value: Option<serde_json::Value>,
    pub category: String,
}

impl ParamValidationError {
    pub fn new(message: impl Into<String>, category: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            field_path: None,
            expected: None,
            bad_value: None,
            category: category.into(),
        }
    }

    pub fn with_field_path(mut self, field_path: impl Into<String>) -> Self {
        self.field_path = Some(field_path.into());
        self
    }

    pub fn with_expected(mut self, expected: impl Into<String>) -> Self {
        self.expected = Some(expected.into());
        self
    }

    pub fn with_bad_value(mut self, bad_value: serde_json::Value) -> Self {
        self.bad_value = Some(bad_value);
        self
    }
}

pub fn validate_params_json<T>(json: &serde_json::Value) -> Result<(), ParamValidationError>
where
    T: DeserializeOwned + ResourceType,
{
    let typed: T = serde_path_to_error::deserialize(json.clone()).map_err(|err| {
        let path = normalize_serde_path(err.path().to_string());
        let message = err.inner().to_string();
        let mut out = ParamValidationError::new(message.clone(), classify_serde_error(&message));
        if let Some(path) = path.clone() {
            out = out.with_field_path(path.clone());
            if let Some(value) = value_at_path(json, &path) {
                out = out.with_bad_value(value.clone());
            }
        }
        if let Some(expected) = extract_expected(&message) {
            out = out.with_expected(expected);
        }
        out
    })?;

    T::validate_params_value(&typed).map_err(|mut err| {
        if err.bad_value.is_none()
            && let Some(path) = err.field_path.as_deref()
            && let Some(value) = value_at_path(json, path)
        {
            err.bad_value = Some(value.clone());
        }
        err
    })
}

fn normalize_serde_path(path: String) -> Option<String> {
    if path == "." { None } else { Some(path) }
}

fn value_at_path<'a>(value: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut current = value;
    let mut chars = path.chars().peekable();
    let mut key = String::new();

    while let Some(ch) = chars.next() {
        match ch {
            '.' => {
                if !key.is_empty() {
                    current = current.get(&key)?;
                    key.clear();
                }
            }
            '[' => {
                if !key.is_empty() {
                    current = current.get(&key)?;
                    key.clear();
                }
                let mut index = String::new();
                while let Some(&next) = chars.peek() {
                    chars.next();
                    if next == ']' {
                        break;
                    }
                    index.push(next);
                }
                let idx: usize = index.parse().ok()?;
                current = current.get(idx)?;
            }
            _ => key.push(ch),
        }
    }

    if !key.is_empty() {
        current = current.get(&key)?;
    }

    Some(current)
}

fn classify_serde_error(message: &str) -> &'static str {
    if message.contains("unknown field") {
        "params_unknown_field"
    } else if message.contains("missing field") {
        "params_missing_field"
    } else if message.contains("unknown variant") {
        "params_unknown_variant"
    } else if message.contains("invalid type") {
        "params_type"
    } else {
        "params_invalid"
    }
}

fn extract_expected(message: &str) -> Option<String> {
    let marker = ", expected ";
    message
        .split_once(marker)
        .map(|(_, expected)| expected.trim_end_matches('.').to_owned())
}
