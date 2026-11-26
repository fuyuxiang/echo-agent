use serde::{Deserialize, Serialize};

/// Typed metadata for a prompt `TextContent._meta` field.
///
/// Replaces ad-hoc `serde_json::json!()` construction on the sender side
/// and manual `.get()` parsing on the receiver side.
///
/// Wire-compatible with the existing format: `{"bash_command": "ls -la"}`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptBlockMeta {
    /// Direct bash command to execute (bypasses agent loop).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bash_command: Option<String>,
}

impl PromptBlockMeta {
    /// Create meta for a direct bash command.
    pub fn bash(command: impl Into<String>) -> Self {
        Self {
            bash_command: Some(command.into()),
        }
    }

    /// Try to parse from a freeform `_meta` map.
    pub fn from_value(value: &agent_client_protocol::Meta) -> Option<Self> {
        serde_json::from_value(serde_json::Value::Object(value.clone())).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bash_roundtrip_serde() {
        let meta = PromptBlockMeta::bash("ls -la");
        let json = serde_json::to_value(&meta).unwrap();
        let parsed: PromptBlockMeta = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.bash_command, Some("ls -la".to_string()));
    }

    #[test]
    fn from_value_legacy_compat() {
        let val = serde_json::json!({"bash_command": "ls"});
        let meta = PromptBlockMeta::from_value(val.as_object().unwrap()).unwrap();
        assert_eq!(meta.bash_command, Some("ls".to_string()));
    }

    #[test]
    fn from_value_unrelated_meta() {
        let val = serde_json::json!({"other": 1});
        let meta = PromptBlockMeta::from_value(val.as_object().unwrap());
        assert!(meta.is_some());
        assert_eq!(meta.unwrap().bash_command, None);
    }

    #[test]
    fn from_value_empty_object() {
        let val = serde_json::json!({});
        let meta = PromptBlockMeta::from_value(val.as_object().unwrap());
        assert!(meta.is_some());
        assert_eq!(meta.unwrap().bash_command, None);
    }

    #[test]
    fn skip_serializing_none() {
        let meta = PromptBlockMeta { bash_command: None };
        let json = serde_json::to_value(&meta).unwrap();
        assert!(!json.as_object().unwrap().contains_key("bash_command"));
    }
}
