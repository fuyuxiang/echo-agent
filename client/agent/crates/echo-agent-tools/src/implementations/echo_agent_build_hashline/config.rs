//! Hashline scheme configuration — shared across all hashline tools.

use super::scheme::{AnchorScheme, ChunkFingerprint, ContentOnly};

/// Example anchor strings for tool descriptions.
#[derive(Debug, Clone)]
pub struct ExampleAnchors {
    pub anchor: String,
    pub read_line1: String,
    pub read_line2: String,
    pub grep_match: String,
    pub grep_context: String,
}

/// Configurable parameters for the hashline anchor scheme.
///
/// Stored as a resource (`Params<HashlineSchemeParams>`) so all three
/// hashline tools use the same scheme within a session.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct HashlineSchemeParams {
    /// Active scheme: `"chunk"` (default) or `"content_only"`.
    #[serde(default = "default_scheme_name")]
    pub scheme: String,
    /// Anchor hash length in characters (1–4).
    #[serde(default = "default_hash_len")]
    pub hash_len: usize,
    /// Chunk size for the chunk scheme.
    #[serde(default = "default_chunk_size")]
    pub chunk_size: usize,
}

fn default_scheme_name() -> String {
    "chunk".to_owned()
}
fn default_hash_len() -> usize {
    3
}
fn default_chunk_size() -> usize {
    8
}

impl crate::types::resources::ResourceType for HashlineSchemeParams {
    const ID: &'static str = "hashline_scheme_params";

    fn validate_params_value(
        value: &Self,
    ) -> Result<(), crate::types::params_validation::ParamValidationError> {
        match value.validate() {
            Ok(()) => Ok(()),
            Err(message) if message.contains("unknown scheme") => {
                Err(crate::types::params_validation::ParamValidationError::new(
                    message,
                    "params_constraint",
                )
                .with_field_path("scheme")
                .with_expected("\"chunk\" or \"content_only\""))
            }
            Err(message) if message.contains("hash_len") => {
                Err(crate::types::params_validation::ParamValidationError::new(
                    message,
                    "params_constraint",
                )
                .with_field_path("hash_len")
                .with_expected("1..=4"))
            }
            Err(message) if message.contains("chunk_size") => {
                Err(crate::types::params_validation::ParamValidationError::new(
                    message,
                    "params_constraint",
                )
                .with_field_path("chunk_size")
                .with_expected("> 0 when scheme is \"chunk\""))
            }
            Err(message) => Err(crate::types::params_validation::ParamValidationError::new(
                message,
                "params_constraint",
            )),
        }
    }
}

impl Default for HashlineSchemeParams {
    fn default() -> Self {
        Self {
            scheme: default_scheme_name(),
            hash_len: default_hash_len(),
            chunk_size: default_chunk_size(),
        }
    }
}

impl HashlineSchemeParams {
    /// Validate the parameters. Returns an error message if invalid.
    pub fn validate(&self) -> Result<(), String> {
        match self.scheme.as_str() {
            "chunk" | "content_only" => {}
            other => {
                return Err(format!(
                    "unknown scheme \"{other}\": expected \"chunk\" or \"content_only\""
                ));
            }
        }
        if self.hash_len == 0 || self.hash_len > 4 {
            return Err(format!("hash_len must be 1..=4, got {}", self.hash_len));
        }
        if self.scheme == "chunk" && self.chunk_size == 0 {
            return Err("chunk_size must be > 0".to_owned());
        }
        Ok(())
    }

    /// Generate example anchor strings for use in tool descriptions.
    /// Returns (single_anchor, line_with_anchor) based on the configured scheme.
    /// Returns `(anchor, read_line1, read_line2, grep_match, grep_context)`.
    pub fn example_anchors(&self) -> ExampleAnchors {
        let len = self.hash_len.clamp(1, 4);
        let hash = &"abcd"[..len];
        let ctx = &"rstu"[..len];
        match self.scheme.as_str() {
            "content_only" => ExampleAnchors {
                anchor: format!("22:{hash}"),
                read_line1: format!("   1:{hash}→fn main() {{"),
                read_line2: format!("   2:{hash}→    let x = 1;"),
                grep_match: format!("2:{hash}:    let x = 1;"),
                grep_context: format!("3:{hash}-    let y = 2;"),
            },
            _ => ExampleAnchors {
                anchor: format!("22:{hash}:{ctx}"),
                read_line1: format!("   1:{hash}:{ctx}→fn main() {{"),
                read_line2: format!("   2:{hash}:{ctx}→    let x = 1;"),
                grep_match: format!("2:{hash}:{ctx}:    let x = 1;"),
                grep_context: format!("3:{hash}:{ctx}-    let y = 2;"),
            },
        }
    }

    /// Replace description placeholders with scheme-appropriate examples.
    pub fn render_description(&self, template: &str) -> String {
        let ex = self.example_anchors();
        template
            .replace("{example_anchor}", &ex.anchor)
            .replace("{example_line1}", &ex.read_line1)
            .replace("{example_line2}", &ex.read_line2)
            .replace("{grep_match}", &ex.grep_match)
            .replace("{grep_context}", &ex.grep_context)
    }

    /// Build a `ToolDefinition` with scheme-aware description rendering.
    ///
    /// Shared by all 3 hashline tools' `versioned_definition` overrides.
    pub fn build_tool_definition(
        &self,
        template: &str,
        client_name: &str,
        description_override: Option<&str>,
        renderer: &crate::types::template_renderer::TemplateRenderer,
        param_map: &std::collections::HashMap<String, String>,
        input_schema: &serde_json::Value,
    ) -> crate::types::definition::ToolDefinition {
        let raw = description_override.unwrap_or(template);
        let with_examples = self.render_description(raw);
        let description = renderer.render(&with_examples).unwrap_or(with_examples);
        let schema = if param_map.is_empty() {
            input_schema.clone()
        } else {
            crate::util::remap::remap_schema_properties(input_schema, param_map)
        };
        crate::types::definition::ToolDefinition::function(client_name, Some(description), schema)
    }

    /// Validate and build the anchor scheme. Returns an error if params are invalid.
    pub fn build_scheme(&self) -> Result<Box<dyn AnchorScheme>, String> {
        self.validate()?;
        Ok(match self.scheme.as_str() {
            "content_only" => Box::new(ContentOnly::with_hash_len(self.hash_len)),
            _ => Box::new(ChunkFingerprint::with_params(
                self.hash_len,
                self.chunk_size,
            )),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_builds_chunk_scheme() {
        let scheme = HashlineSchemeParams::default().build_scheme().unwrap();
        assert_eq!(scheme.name(), "chunk_v1");
        assert_eq!(scheme.hash_len(), 3);
    }

    #[test]
    fn content_only_builds() {
        let params = HashlineSchemeParams {
            scheme: "content_only".to_owned(),
            hash_len: 2,
            chunk_size: 8,
        };
        let scheme = params.build_scheme().unwrap();
        assert_eq!(scheme.name(), "content_only_v1");
        assert_eq!(scheme.hash_len(), 2);
    }

    #[test]
    fn custom_chunk_params() {
        let params = HashlineSchemeParams {
            scheme: "chunk".to_owned(),
            hash_len: 4,
            chunk_size: 16,
        };
        let scheme = params.build_scheme().unwrap();
        assert_eq!(scheme.name(), "chunk_v1");
        assert_eq!(scheme.hash_len(), 4);
    }

    #[test]
    fn unknown_scheme_rejected() {
        let params = HashlineSchemeParams {
            scheme: "bogus".to_owned(),
            hash_len: 3,
            chunk_size: 8,
        };
        assert!(params.build_scheme().is_err());
        assert!(params.validate().unwrap_err().contains("unknown scheme"));
    }

    #[test]
    fn hash_len_zero_rejected() {
        let params = HashlineSchemeParams {
            scheme: "chunk".to_owned(),
            hash_len: 0,
            chunk_size: 8,
        };
        assert!(params.build_scheme().is_err());
    }

    #[test]
    fn hash_len_five_rejected() {
        let params = HashlineSchemeParams {
            scheme: "chunk".to_owned(),
            hash_len: 5,
            chunk_size: 8,
        };
        assert!(params.build_scheme().is_err());
    }

    #[test]
    fn chunk_size_zero_rejected() {
        let params = HashlineSchemeParams {
            scheme: "chunk".to_owned(),
            hash_len: 3,
            chunk_size: 0,
        };
        assert!(params.build_scheme().is_err());
    }

    #[test]
    fn content_only_ignores_chunk_size() {
        // chunk_size doesn't matter for content_only scheme.
        let params = HashlineSchemeParams {
            scheme: "content_only".to_owned(),
            hash_len: 3,
            chunk_size: 0,
        };
        assert!(params.build_scheme().is_ok());
    }

    #[test]
    fn deserializes_from_json() {
        let json = r#"{"scheme":"content_only","hash_len":2,"chunk_size":4}"#;
        let params: HashlineSchemeParams = serde_json::from_str(json).unwrap();
        assert_eq!(params.scheme, "content_only");
        assert_eq!(params.hash_len, 2);
    }

    #[test]
    fn render_description_chunk_3() {
        let params = HashlineSchemeParams::default();
        let rendered = params.render_description("anchor={example_anchor}");
        assert_eq!(rendered, "anchor=22:abc:rst");
    }

    #[test]
    fn render_description_content_only_2() {
        let params = HashlineSchemeParams {
            scheme: "content_only".to_owned(),
            hash_len: 2,
            chunk_size: 8,
        };
        let rendered = params.render_description("anchor={example_anchor}");
        assert_eq!(rendered, "anchor=22:ab");
        assert!(!rendered.contains(":rs"));
    }

    #[test]
    fn render_grep_examples_chunk() {
        let params = HashlineSchemeParams::default();
        let rendered = params.render_description("{grep_match} / {grep_context}");
        assert!(rendered.contains("2:abc:rst:"), "match: {rendered}");
        assert!(rendered.contains("3:abc:rst-"), "context: {rendered}");
    }

    #[test]
    fn render_grep_examples_content_only() {
        let params = HashlineSchemeParams {
            scheme: "content_only".to_owned(),
            hash_len: 2,
            chunk_size: 8,
        };
        let rendered = params.render_description("{grep_match} / {grep_context}");
        assert!(rendered.contains("2:ab:"), "match: {rendered}");
        assert!(rendered.contains("3:ab-"), "context: {rendered}");
    }

    #[test]
    fn render_does_not_panic_on_invalid_hash_len() {
        let params = HashlineSchemeParams {
            scheme: "chunk".to_owned(),
            hash_len: 100, // invalid but clamped
            chunk_size: 8,
        };
        let rendered = params.render_description("{example_anchor}");
        assert_eq!(rendered, "22:abcd:rstu"); // clamped to 4
    }

    #[test]
    fn render_does_not_panic_on_zero_hash_len() {
        let params = HashlineSchemeParams {
            scheme: "chunk".to_owned(),
            hash_len: 0, // invalid but clamped
            chunk_size: 8,
        };
        let rendered = params.render_description("{example_anchor}");
        assert_eq!(rendered, "22:a:r"); // clamped to 1
    }

    #[test]
    fn build_tool_definition_renders_examples() {
        use crate::types::template_renderer::TemplateRenderer;
        let params = HashlineSchemeParams::default();
        let renderer = TemplateRenderer::new(
            std::collections::HashMap::new(),
            std::collections::HashMap::new(),
        );
        let schema = serde_json::json!({"type": "object"});
        let def = params.build_tool_definition(
            "Read {example_anchor} and {grep_match}",
            "test_tool",
            None,
            &renderer,
            &std::collections::HashMap::new(),
            &schema,
        );
        let desc = def.function.description.as_deref().unwrap();
        assert!(desc.contains("22:abc:rst"), "anchor: {desc}");
        assert!(desc.contains("2:abc:rst:"), "grep match: {desc}");
        assert!(
            !desc.contains("{example_anchor}"),
            "placeholder not resolved: {desc}"
        );
    }

    #[test]
    fn build_tool_definition_invalid_params_no_panic() {
        use crate::types::template_renderer::TemplateRenderer;
        let params = HashlineSchemeParams {
            scheme: "bogus".to_owned(),
            hash_len: 999,
            chunk_size: 0,
        };
        let renderer = TemplateRenderer::new(
            std::collections::HashMap::new(),
            std::collections::HashMap::new(),
        );
        let schema = serde_json::json!({"type": "object"});
        let def = params.build_tool_definition(
            "{example_anchor}",
            "test",
            None,
            &renderer,
            &std::collections::HashMap::new(),
            &schema,
        );
        assert!(def.function.description.is_some());
    }
}
