//! Subagent role and persona configuration types.
//!
//! These are the canonical definitions for `SubagentRole`, `SubagentPersona`,
//! and `PersonaIOField`. The shell re-exports them via
//! `echo_agent_runtime::config::{SubagentRole, SubagentPersona, PersonaIOField}`.
//!
//! Methods that remain in `echo-agent-runtime` (on `SubagentsConfig`):
//! - `discover_personas()` / `discover_roles()` — filesystem discovery
//!   coupled to the shell's config resolution pipeline.
//! - `resolve()` — config layering (CLI > env > TOML > remote) is
//!   shell-specific. This crate receives already-resolved maps.

use echo_agent_tools::implementations::skills::discovery::extract_first_paragraph;
use std::path::PathBuf;

use serde::Deserialize;

/// A declarative subagent role definition from config.
///
/// Roles provide named presets that callers can reference via the
/// `subagent_type` field in the task tool. Each role can specify
/// a default capability mode, model override, and custom prompt.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct SubagentRole {
    /// Human-readable description of what this role does.
    pub description: String,
    /// Default capability mode for agents using this role.
    /// One of: "read-only", "read-write", "execute", "all".
    /// Not a model-facing spawn argument; `general-purpose` stays `all`.
    #[serde(default)]
    pub default_capability_mode: Option<String>,
    /// Model override for this role. If set, agents using this role
    /// default to this model unless the spawn-time `model` override
    /// is provided.
    #[serde(default)]
    pub model: Option<String>,
    /// Default reasoning effort for this role (e.g. "low", "medium", "high").
    /// Can be overridden per-spawn via `reasoning_effort` in the task tool.
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    /// Path to a prompt/instruction file (relative to workspace root).
    /// Loaded at spawn time and prepended to the child's prompt as a
    /// `<role-instructions>` block.
    #[serde(default)]
    pub prompt_file: Option<String>,
    /// Default isolation mode ("none" or "worktree").
    #[serde(default)]
    pub default_isolation: Option<String>,
    /// Base directory for resolving relative `prompt_file` references.
    /// Set to the parent dir of the source `.toml` file during discovery.
    #[serde(skip)]
    pub source_dir: Option<PathBuf>,
}

/// A named persona/SOUL definition controlling tone, style, and behavior.
///
/// Personas are referenced by name via the `persona` field in the task tool.
/// Their instructions are prepended to the child's prompt as a `<persona>`
/// XML block.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct SubagentPersona {
    /// Inline instruction text applied as a persona layer.
    pub instructions: Option<String>,
    /// Optional short description shown in persona summaries.
    /// Falls back to first-paragraph extraction from `instructions`.
    pub description: Option<String>,
    /// Path to an instruction file (relative to workspace root).
    /// Content is loaded at spawn time and merged with `instructions`.
    /// If both are set, `instructions` is prepended before file content.
    pub instructions_file: Option<String>,
    /// Declared inputs this persona expects. The parent agent reads these
    /// to know what file paths or context to provide in the prompt.
    #[serde(default)]
    pub inputs: Vec<PersonaIOField>,
    /// Declared outputs this persona produces. The parent agent reads
    /// these to know what artifacts to expect and pass to the next agent.
    #[serde(default)]
    pub outputs: Vec<PersonaIOField>,
    /// Default isolation mode when this persona is used.
    #[serde(default)]
    pub default_isolation: Option<String>,
    /// Model override when this persona is used.
    #[serde(default)]
    pub model: Option<String>,
    /// Default reasoning effort for this persona (e.g. "low", "medium", "high").
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    /// Base directory for resolving relative file references.
    /// Set to the parent dir of the source `.toml` file during discovery.
    /// When `None`, relative paths resolve against the workspace cwd.
    #[serde(skip)]
    pub source_dir: Option<PathBuf>,
    /// Absolute path to the source file this persona was loaded from.
    /// Populated during discovery; `None` for inline config personas.
    #[serde(skip)]
    pub source_path: Option<String>,
}

/// A declared input or output for a persona.
///
/// Enables the parent agent to discover what a persona needs (inputs)
/// and what it produces (outputs) without hardcoded knowledge of the
/// persona's protocol.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct PersonaIOField {
    /// Short identifier (e.g. "review_file", "summary_file").
    pub name: String,
    /// What kind of artifact: "file", "text", etc.
    #[serde(default = "PersonaIOField::default_io_type")]
    pub io_type: String,
    /// Whether this input/output is required.
    #[serde(default)]
    pub required: bool,
    /// Human-readable description shown in the task tool help.
    pub description: String,
}

impl PersonaIOField {
    fn default_io_type() -> String {
        "file".to_string()
    }
}

impl SubagentPersona {
    /// Render a human-readable summary of this persona's IO contract
    /// for inclusion in the task tool description.
    pub fn render_io_summary(&self, name: &str) -> String {
        let fallback;
        let desc = if let Some(d) = self.description.as_deref().filter(|s| !s.trim().is_empty()) {
            d
        } else {
            fallback = self
                .instructions
                .as_deref()
                .and_then(extract_first_paragraph);
            fallback.as_deref().unwrap_or("Custom persona")
        };
        let scope = match self.source_path.as_deref() {
            Some(path) if path.contains("/bundled/") => "[bundled]",
            Some(_) => "[user]",
            None => "[local]",
        };
        let mut lines = vec![format!("- **{name}** {scope}: {desc}")];
        if let Some(ref path) = self.source_path {
            lines.push(format!("  Path: {path}"));
        }
        if !self.inputs.is_empty() {
            lines.push("    Expects in prompt:".to_string());
            for io in &self.inputs {
                let req = if io.required { "REQUIRED" } else { "optional" };
                lines.push(format!(
                    "      - `{}` ({}, {}): {}",
                    io.name, io.io_type, req, io.description
                ));
            }
        }
        if !self.outputs.is_empty() {
            lines.push("    Produces:".to_string());
            for io in &self.outputs {
                let req = if io.required { "REQUIRED" } else { "optional" };
                lines.push(format!(
                    "      - `{}` ({}, {}): {}",
                    io.name, io.io_type, req, io.description
                ));
            }
        }
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subagent_role_deserialize_defaults() {
        let role: SubagentRole = toml::from_str("").unwrap();
        assert_eq!(role.description, "");
        assert!(role.default_capability_mode.is_none());
        assert!(role.model.is_none());
        assert!(role.prompt_file.is_none());
    }

    #[test]
    fn subagent_role_deserialize_full() {
        let toml_str = r#"
description = "Research agent"
default_capability_mode = "read-only"
model = "echo-agent-3"
reasoning_effort = "high"
prompt_file = ".echo-agent/prompts/researcher.md"
default_isolation = "worktree"
"#;
        let role: SubagentRole = toml::from_str(toml_str).unwrap();
        assert_eq!(role.description, "Research agent");
        assert_eq!(role.default_capability_mode.as_deref(), Some("read-only"));
        assert_eq!(role.model.as_deref(), Some("echo-agent-3"));
        assert_eq!(role.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(
            role.prompt_file.as_deref(),
            Some(".echo-agent/prompts/researcher.md")
        );
        assert_eq!(role.default_isolation.as_deref(), Some("worktree"));
    }

    #[test]
    fn subagent_persona_deserialize_defaults() {
        let persona: SubagentPersona = toml::from_str("").unwrap();
        assert!(persona.instructions.is_none());
        assert!(persona.description.is_none());
        assert!(persona.instructions_file.is_none());
        assert!(persona.inputs.is_empty());
        assert!(persona.outputs.is_empty());
    }

    #[test]
    fn subagent_persona_deserialize_full() {
        let toml_str = r#"
instructions = "You are a concise writer."
description = "A concise writing persona."
instructions_file = ".echo-agent/personas/concise.md"
model = "echo-agent-3-fast"
reasoning_effort = "low"
default_isolation = "none"

[[inputs]]
name = "review_file"
io_type = "file"
required = true
description = "Path to the review notes file"

[[outputs]]
name = "summary_file"
io_type = "file"
required = false
description = "Path to write the summary"
"#;
        let persona: SubagentPersona = toml::from_str(toml_str).unwrap();
        assert_eq!(
            persona.instructions.as_deref(),
            Some("You are a concise writer.")
        );
        assert_eq!(
            persona.instructions_file.as_deref(),
            Some(".echo-agent/personas/concise.md")
        );
        assert_eq!(persona.model.as_deref(), Some("echo-agent-3-fast"));
        assert_eq!(persona.reasoning_effort.as_deref(), Some("low"));
        assert_eq!(persona.inputs.len(), 1);
        assert_eq!(persona.inputs[0].name, "review_file");
        assert!(persona.inputs[0].required);
        assert_eq!(persona.outputs.len(), 1);
        assert_eq!(persona.outputs[0].name, "summary_file");
        assert!(!persona.outputs[0].required);
        assert_eq!(
            persona.description.as_deref(),
            Some("A concise writing persona.")
        );
    }

    #[test]
    fn persona_io_field_default_io_type_is_file() {
        let json = r#"{"name": "test", "description": "a test field"}"#;
        let field: PersonaIOField = serde_json::from_str(json).unwrap();
        assert_eq!(field.io_type, "file");
        assert!(!field.required);
    }

    #[test]
    fn render_io_summary_uses_explicit_description() {
        let persona = SubagentPersona {
            description: Some("A focused code reviewer.".to_owned()),
            instructions: Some("Ignore this line.\nAnd this one.".to_owned()),
            ..Default::default()
        };
        let summary = persona.render_io_summary("reviewer");
        assert!(summary.contains("A focused code reviewer."));
        assert!(!summary.contains("Ignore this line"));
    }

    #[test]
    fn render_io_summary_extracts_first_paragraph_from_instructions() {
        let persona = SubagentPersona {
            instructions: Some(
                "You are a meticulous code reviewer. Review code and produce structured review\n\
                 notes in a Markdown file at the path given in the prompt.\n\n\
                 Process:\n1. Read the code."
                    .to_owned(),
            ),
            ..Default::default()
        };
        let summary = persona.render_io_summary("reviewer");
        assert!(
            summary.contains("You are a meticulous code reviewer. Review code and produce structured review notes in a Markdown file at the path given in the prompt."),
            "should join multi-line first paragraph: {summary}"
        );
        assert!(!summary.contains("Process"));
    }

    #[test]
    fn render_io_summary_falls_back_to_custom_persona() {
        let persona = SubagentPersona::default();
        let summary = persona.render_io_summary("empty");
        assert!(summary.contains("Custom persona"));
    }

    #[test]
    fn render_io_summary_extracts_lead_paragraph_before_list() {
        let persona = SubagentPersona {
            instructions: Some(
                "You are a thorough researcher. When exploring a question:\n\
                 - Exhaust all reasonable search avenues before concluding\n\
                 - Always cite specific file paths"
                    .to_owned(),
            ),
            ..Default::default()
        };
        let summary = persona.render_io_summary("researcher");
        assert!(summary.contains("You are a thorough researcher. When exploring a question:"));
        assert!(!summary.contains("Always cite specific file paths"));
    }

    #[test]
    fn render_io_summary_headings_only_instructions_falls_back() {
        let persona = SubagentPersona {
            instructions: Some("# Heading\n## Sub".to_owned()),
            ..Default::default()
        };
        let summary = persona.render_io_summary("test");
        assert!(summary.contains("Custom persona"));
    }

    #[test]
    fn render_io_summary_empty_description_falls_through_to_instructions() {
        let persona = SubagentPersona {
            description: Some("".to_owned()),
            instructions: Some("Actual description here.".to_owned()),
            ..Default::default()
        };
        let summary = persona.render_io_summary("test");
        assert!(summary.contains("Actual description here."));
    }

    #[test]
    fn render_io_summary_whitespace_description_falls_through_to_instructions() {
        let persona = SubagentPersona {
            description: Some("   ".to_owned()),
            instructions: Some("Real content.".to_owned()),
            ..Default::default()
        };
        let summary = persona.render_io_summary("test");
        assert!(summary.contains("Real content."));
    }
}
