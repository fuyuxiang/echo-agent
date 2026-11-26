//! First-class, inspectable system prompt context.
//!
//! `PromptContext` captures the agent-specific inputs to prompt rendering
//! as a serializable struct. Users can dump it as JSON and inspect
//! individual sections.
//!
//! Rendering is done by `ToolBridge::render_prompt()` which delegates to
//! `TemplateRenderer` in `echo-agent-tools`. This struct does NOT own a
//! render engine — it provides placeholders and discovered sections.
use crate::config::PromptMode;
use crate::prompt::agents_md::{self, AgentConfigFile};
use crate::prompt::template::{apply_patch_template, base_template, subagent_template};
use serde::de;
use serde::{Deserialize, Serialize};
/// Selects which base template to use for `Extend` mode rendering.
///
/// Built-in variants decrypt the template on demand and never store
/// the plaintext persistently, ensuring it is zeroed after use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TemplateOverride {
    /// Use the standard base template (or subagent template based on audience).
    #[default]
    None,
    /// Use the apply-patch profile prompt template (decrypted on demand).
    Codex,
    /// A caller-provided custom template string.
    Custom(String),
}
/// Backward-compatible deserialization: accepts both the new tagged format
/// (`"none"`, `"codex"`, `{"custom": "..."}`) and the legacy format where
/// `system_prompt` was `Option<String>` (a raw template string).
impl<'de> Deserialize<'de> for TemplateOverride {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct Visitor;
        impl<'de> de::Visitor<'de> for Visitor {
            type Value = TemplateOverride;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str(r#""none", "codex", "cursor", {"custom": "..."}, or a template string"#)
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<TemplateOverride, E> {
                match v {
                    "none" => Ok(TemplateOverride::None),
                    "codex" => Ok(TemplateOverride::Codex),
                    other => Ok(TemplateOverride::Custom(other.to_owned())),
                }
            }
            fn visit_map<M: de::MapAccess<'de>>(
                self,
                mut map: M,
            ) -> Result<TemplateOverride, M::Error> {
                match map.next_key::<String>()? {
                    Some(ref k) if k == "custom" => {
                        let val: String = map.next_value()?;
                        Ok(TemplateOverride::Custom(val))
                    }
                    Some(other) => Err(de::Error::unknown_field(&other, &["custom"])),
                    Option::None => Err(de::Error::custom(r#"expected {"custom": "..."}"#)),
                }
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}
/// Controls which base template and catalog sections are rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PromptAudience {
    /// Top-level interactive session. Full base template, all catalog sections.
    #[default]
    Primary,
    /// Child/subagent session. Compact base template, no persona/subagent catalogs.
    Subagent,
}
use echo_agent_tools::bridge::ToolBridge;
use echo_agent_tools::types::template_renderer::TemplateRenderer;
/// Agent-specific inputs for system prompt rendering.
///
/// Serializable (JSON/YAML) so users can dump it and inspect fields.
/// Rendering goes through `ToolBridge::render_prompt()`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptContext {
    /// Schema version for forward-compatible persistence.
    pub version: u32,
    /// Which prompt mode produced this context.
    pub prompt_mode: PromptMode,
    /// Whether this is a primary (parent) or subagent (child) session.
    /// Controls base template choice and catalog section rendering.
    #[serde(default)]
    pub audience: PromptAudience,
    /// Custom body: appended after base template (Extend) or the entire
    /// prompt (Full). `None` = base template only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_body: Option<String>,
    /// Persists plan-agent verification policy across prompt reconstruction.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub include_browser_verification: bool,
    /// Which base template to use for `Extend` mode.
    /// `TemplateOverride::None` = standard base/subagent template.
    /// `TemplateOverride::Codex` = apply-patch profile template (decrypted on demand).
    /// `TemplateOverride::Custom` = caller-provided template string.
    #[serde(default, skip_serializing_if = "is_template_override_none")]
    pub system_prompt: TemplateOverride,
    /// AGENTS.md files discovered during build, in precedence order
    /// (repo root → CWD; deeper files override).
    pub agents_md_files: Vec<AgentConfigFile>,
    /// Pre-rendered persona summaries for system prompt injection.
    /// Each entry is a formatted string like:
    /// `- **reviewer** [user]: Writes structured review notes...`
    #[serde(default)]
    pub persona_summaries: Vec<String>,
    /// ISO-8601 UTC timestamp captured at build time.
    pub build_timestamp_utc: String,
    /// Whether the memory system is enabled for this session.
    /// When true, the system prompt includes a `<memory>` section telling
    /// the model it can use `memory_search` and `memory_get`.
    #[serde(default)]
    pub memory_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_global_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_workspace_path: Option<String>,
    /// Role instructions to include in the system prompt.
    /// Moved from the user task prompt so they're part of durable identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_instructions: Option<String>,
    /// Persona instructions to include in the system prompt.
    /// Moved from the user task prompt so they're part of durable identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona_instructions: Option<String>,
    /// OS name for the `<user_info>` system prompt block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os_name: Option<String>,
    /// User's default shell for the `<user_info>` system prompt block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell_path: Option<String>,
    /// Model-facing working directory for the `<user_info>` system prompt block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    /// Current date (`YYYY-MM-DD`) in the user's local timezone, for the
    /// `<user_info>` system prompt block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_date: Option<String>,
    /// Whether the agent is running in a non-interactive (headless / SDK /
    /// stdio / generic-ACP).
    #[serde(default)]
    pub is_non_interactive: bool,
    /// Identity in the primary echo-agent-build system prompt (`You are <label>…`).
    /// Not the UI picker name. Defaults to [`DEFAULT_SYSTEM_PROMPT_LABEL`].
    #[serde(default = "default_system_prompt_label")]
    pub system_prompt_label: String,
}
/// Default identity in the primary system prompt.
pub const DEFAULT_SYSTEM_PROMPT_LABEL: &str = "Echo";
fn default_system_prompt_label() -> String {
    DEFAULT_SYSTEM_PROMPT_LABEL.to_string()
}
fn is_template_override_none(t: &TemplateOverride) -> bool {
    matches!(t, TemplateOverride::None)
}
impl PromptContext {
    /// Normalize this context for persistence based on audience.
    ///
    /// For `Subagent` audience, applies the same suppression as the render
    /// path: persona summaries are cleared. AGENTS.md is delivered in full,
    /// identical to the primary agent.
    pub fn normalize_for_persistence(&mut self) {
        if self.audience != PromptAudience::Subagent {
            return;
        }
        self.persona_summaries.clear();
    }
}
impl Default for PromptContext {
    fn default() -> Self {
        Self {
            version: 1,
            prompt_mode: PromptMode::Extend,
            audience: PromptAudience::default(),
            prompt_body: None,
            include_browser_verification: false,
            system_prompt: TemplateOverride::None,
            agents_md_files: vec![],
            persona_summaries: vec![],
            build_timestamp_utc: chrono::Utc::now().to_rfc3339(),
            memory_enabled: false,
            memory_global_path: None,
            memory_workspace_path: None,
            role_instructions: None,
            persona_instructions: None,
            os_name: None,
            shell_path: None,
            working_directory: None,
            current_date: None,
            is_non_interactive: false,
            system_prompt_label: default_system_prompt_label(),
        }
    }
}
impl PromptContext {
    /// Format the AGENTS.md section as a `<system-reminder>` block.
    ///
    /// Returns `None` if no AGENTS.md files were discovered.
    pub fn format_agents_md_section(&self) -> Option<String> {
        agents_md::format_agents_md_section(&self.agents_md_files)
    }
    /// AGENTS.md content for injection as a prepended user message.
    ///
    /// - Subagents and primary sessions both get the full block, so a child
    ///   verifier sees the same project instructions as the main agent.
    pub fn agents_md_user_reminder(&self) -> Option<String> {
        if self.include_browser_verification {
            return None;
        }
        self.format_agents_md_section()
    }
    /// Personas content for injection as a prepended user message.
    ///
    /// Returns the `<system-reminder>` block to prepend as a user message,
    /// wrapping the `<personas>` section.
    ///
    /// - Subagents never get personas (`task` itself is a parent-only tool).
    pub fn personas_user_reminder(&self) -> Option<String> {
        if self.audience == PromptAudience::Subagent {
            return None;
        }
        let section = self.format_personas_section()?;
        Some(format!("<system-reminder>\n{section}</system-reminder>"))
    }
    /// Format the personas section content.
    ///
    /// Always returns `None` — the `persona` parameter has been removed
    /// from the task tool input, so persona summaries are no longer
    /// injected into the conversation.
    pub fn format_personas_section(&self) -> Option<String> {
        None
    }
    /// Build the placeholder JSON for template rendering.
    ///
    /// These are the agent-specific values that get merged with the
    /// tool context in `TemplateRenderer::render_with_extra()`.
    pub fn placeholders(&self) -> serde_json::Value {
        serde_json::json!({
            "memory_enabled": self.memory_enabled,
            "memory_global_path": self.memory_global_path.as_deref().unwrap_or(""),
            "memory_workspace_path": self.memory_workspace_path.as_deref().unwrap_or(""),
            "role_instructions": self.role_instructions.as_deref().unwrap_or(""),
            "persona_instructions": self.persona_instructions.as_deref().unwrap_or(""),
            "os_name": self.os_name.as_deref().unwrap_or(""),
            "shell_path": self.shell_path.as_deref().unwrap_or(""),
            "working_directory": self.working_directory.as_deref().unwrap_or(""),
            "current_date": self.current_date.as_deref().unwrap_or(""),
            "is_non_interactive": self.is_non_interactive,
            "system_prompt_label": self.system_prompt_label.as_str(),
            "include_browser_verification": self.include_browser_verification,
        })
    }
    /// Render the full system prompt via `ToolBridge`.
    ///
    /// Tool names (`${{ tools.by_kind.* }}`) are resolved by the
    /// `TemplateRenderer` inside the bridge. Agent-specific fields
    /// (`memory_enabled`, `role_instructions`, etc.) are passed as placeholders.
    ///
    /// Both the base template AND the `prompt_body` are rendered through
    /// MiniJinja so that `${{ tools.by_kind.* }}` variables resolve
    /// correctly regardless of prompt mode.
    pub async fn render(&self, tool_bridge: &ToolBridge) -> Option<String> {
        let renderer = tool_bridge.template_renderer_snapshot().await?;
        self.render_with_renderer(&renderer)
    }
    /// Render the full system prompt from a finalized tool-name renderer.
    ///
    /// Hosts that do not own a [`ToolBridge`] use this path so they still
    /// consume the production base-template and prompt-body composition.
    pub fn render_with_renderer(&self, renderer: &TemplateRenderer) -> Option<String> {
        let placeholders = self.placeholders();
        let render = |template: &str| renderer.render_with_extra(template, &placeholders).ok();
        let prompt = match self.prompt_mode {
            PromptMode::Extend => {
                let decrypted;
                let base = match &self.system_prompt {
                    TemplateOverride::Custom(template) => template.as_str(),
                    TemplateOverride::Codex => {
                        decrypted = apply_patch_template();
                        &decrypted
                    }
                    TemplateOverride::None => {
                        decrypted = if self.audience == PromptAudience::Subagent {
                            subagent_template()
                        } else {
                            base_template()
                        };
                        &decrypted
                    }
                };
                let mut prompt = render(base)?;
                if let Some(body) = &self.prompt_body {
                    prompt.push_str("\n\n");
                    prompt.push_str(&render(body).unwrap_or_else(|| body.clone()));
                }
                prompt
            }
            PromptMode::Full => render(self.prompt_body.as_deref().unwrap_or(""))?,
        };
        Some(prompt)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    /// Fixed timestamp for deterministic tests.
    const TEST_TIMESTAMP: &str = "2025-06-15T12:00:00+00:00";
    fn test_context() -> PromptContext {
        PromptContext {
            version: 1,
            prompt_mode: PromptMode::Extend,
            audience: PromptAudience::Primary,
            prompt_body: None,
            include_browser_verification: false,
            system_prompt: TemplateOverride::None,
            agents_md_files: vec![],
            persona_summaries: vec![],
            build_timestamp_utc: TEST_TIMESTAMP.to_string(),
            memory_enabled: false,
            memory_global_path: None,
            memory_workspace_path: None,
            role_instructions: None,
            persona_instructions: None,
            os_name: None,
            shell_path: None,
            working_directory: None,
            current_date: None,
            is_non_interactive: false,
            system_prompt_label: default_system_prompt_label(),
        }
    }
    #[test]
    fn standard_primary_template_renders_browser_verification_only_when_flagged() {
        let renderer = TemplateRenderer::new(Default::default(), Default::default());
        let mut ctx = test_context();
        ctx.include_browser_verification = true;
        let on = ctx.render_with_renderer(&renderer).unwrap();
        ctx.include_browser_verification = false;
        let off = ctx.render_with_renderer(&renderer).unwrap();
        let block_start = on
            .find("\n\n<browser_verification>")
            .expect("flagged standard template must render browser verification");
        assert_eq!(&on[..block_start], off);
        assert!(on.ends_with("</browser_verification>"));
        assert!(!off.contains("<browser_verification>"));
    }
    #[test]
    fn full_mode_flag_does_not_append_browser_verification() {
        let renderer = TemplateRenderer::new(Default::default(), Default::default());
        let ctx = PromptContext {
            prompt_mode: PromptMode::Full,
            prompt_body: Some("base".into()),
            include_browser_verification: true,
            ..test_context()
        };
        assert_eq!(ctx.render_with_renderer(&renderer).as_deref(), Some("base"));
    }
    #[test]
    fn test_json_round_trip() {
        let ctx = test_context();
        let json = serde_json::to_string_pretty(&ctx).unwrap();
        let ctx2: PromptContext = serde_json::from_str(&json).unwrap();
        assert_eq!(ctx.version, ctx2.version);
        assert_eq!(ctx.build_timestamp_utc, ctx2.build_timestamp_utc);
        assert_eq!(ctx.agents_md_files.len(), ctx2.agents_md_files.len());
    }
    #[test]
    fn test_json_round_trip_with_agents_md() {
        let mut ctx = test_context();
        ctx.agents_md_files = vec![
            AgentConfigFile {
                file_name: "AGENTS.md".to_string(),
                file_path: "/repo/AGENTS.md".to_string(),
                content: "# Repo instructions".to_string(),
            },
            AgentConfigFile {
                file_name: "AGENTS.md".to_string(),
                file_path: "/repo/sub/AGENTS.md".to_string(),
                content: "# Sub instructions".to_string(),
            },
        ];
        let json = serde_json::to_string(&ctx).unwrap();
        let ctx2: PromptContext = serde_json::from_str(&json).unwrap();
        assert_eq!(ctx2.agents_md_files.len(), 2);
        assert_eq!(ctx2.agents_md_files[0].content, "# Repo instructions");
        assert_eq!(ctx2.agents_md_files[1].file_path, "/repo/sub/AGENTS.md");
    }
    #[test]
    fn test_template_override_deserialize_new_format() {
        let v: TemplateOverride = serde_json::from_str(r#""none""#).unwrap();
        assert_eq!(v, TemplateOverride::None);
        let v: TemplateOverride = serde_json::from_str(r#""codex""#).unwrap();
        assert_eq!(v, TemplateOverride::Codex);
        let v: TemplateOverride = serde_json::from_str(r#"{"custom": "my template"}"#).unwrap();
        assert_eq!(v, TemplateOverride::Custom("my template".to_string()));
    }
    #[test]
    fn test_template_override_deserialize_legacy_string() {
        let v: TemplateOverride = serde_json::from_str(r#""You are a coding agent...""#).unwrap();
        assert_eq!(
            v,
            TemplateOverride::Custom("You are a coding agent...".to_string())
        );
    }
    #[test]
    fn test_template_override_round_trip() {
        for original in [
            TemplateOverride::None,
            TemplateOverride::Codex,
            TemplateOverride::Custom("my custom prompt".to_string()),
        ] {
            let json = serde_json::to_string(&original).unwrap();
            let loaded: TemplateOverride = serde_json::from_str(&json).unwrap();
            assert_eq!(original, loaded);
        }
    }
    #[test]
    fn test_prompt_context_legacy_system_prompt_field() {
        let legacy_json = r#"{
            "version": 1,
            "prompt_mode": "extend",
            "os_name": "linux",
            "shell_path": "/bin/bash",
            "working_directory": "/workspace",
            "build_timestamp_utc": "2025-01-01T00:00:00Z",
            "agents_md_files": [],
            "system_prompt": "You are a coding agent running in the CLI."
        }"#;
        let ctx: PromptContext = serde_json::from_str(legacy_json).unwrap();
        assert_eq!(
            ctx.system_prompt,
            TemplateOverride::Custom("You are a coding agent running in the CLI.".to_string())
        );
    }
    #[test]
    fn test_prompt_context_missing_system_prompt_field() {
        let json = r#"{
            "version": 1,
            "prompt_mode": "extend",
            "os_name": "linux",
            "shell_path": "/bin/bash",
            "working_directory": "/workspace",
            "build_timestamp_utc": "2025-01-01T00:00:00Z",
            "agents_md_files": []
        }"#;
        let ctx: PromptContext = serde_json::from_str(json).unwrap();
        assert_eq!(ctx.system_prompt, TemplateOverride::None);
    }
    #[test]
    fn test_placeholders_contains_agent_fields() {
        let ctx = test_context();
        let p = ctx.placeholders();
        assert_eq!(p["memory_enabled"], false);
        assert!(p.get("role_instructions").is_some());
        assert!(p.get("persona_instructions").is_some());
        assert_eq!(p["system_prompt_label"], DEFAULT_SYSTEM_PROMPT_LABEL);
    }
    #[test]
    fn test_placeholders_system_prompt_label_override() {
        let mut ctx = test_context();
        ctx.system_prompt_label = "EchoAgent Internal".into();
        let p = ctx.placeholders();
        assert_eq!(p["system_prompt_label"], "EchoAgent Internal");
    }
    #[test]
    fn test_missing_system_prompt_label_deserializes_to_default() {
        let json = r#"{"version":1,"prompt_mode":"extend","agents_md_files":[],"build_timestamp_utc":"2025-06-15T12:00:00+00:00"}"#;
        let ctx: PromptContext = serde_json::from_str(json).unwrap();
        assert_eq!(ctx.system_prompt_label, DEFAULT_SYSTEM_PROMPT_LABEL);
    }
    #[test]
    fn test_placeholders_includes_all_eight_keys() {
        let mut ctx = test_context();
        ctx.os_name = Some("linux".into());
        ctx.shell_path = Some("/bin/bash".into());
        ctx.working_directory = Some("/workspace".into());
        ctx.current_date = Some("2026-03-26".into());
        ctx.memory_enabled = true;
        ctx.role_instructions = Some("test role".into());
        ctx.persona_instructions = Some("test persona".into());
        let p = ctx.placeholders();
        assert_eq!(p["os_name"], "linux");
        assert_eq!(p["shell_path"], "/bin/bash");
        assert_eq!(p["working_directory"], "/workspace");
        assert_eq!(p["current_date"], "2026-03-26");
        assert_eq!(p["memory_enabled"], true);
        assert_eq!(p["role_instructions"], "test role");
        assert_eq!(p["persona_instructions"], "test persona");
    }
    #[test]
    fn test_placeholders_user_info_defaults_to_empty() {
        let ctx = test_context();
        let p = ctx.placeholders();
        assert_eq!(p["os_name"], "");
        assert_eq!(p["shell_path"], "");
        assert_eq!(p["working_directory"], "");
        assert_eq!(p["current_date"], "");
    }
    #[test]
    fn test_user_info_fields_serialization_round_trip() {
        let mut ctx = test_context();
        ctx.os_name = Some("linux".into());
        ctx.shell_path = Some("/bin/bash".into());
        ctx.working_directory = Some("/workspace/project".into());
        ctx.current_date = Some("2026-03-26".into());
        let json = serde_json::to_string_pretty(&ctx).unwrap();
        assert!(json.contains(r#""os_name": "linux""#));
        assert!(json.contains(r#""shell_path": "/bin/bash""#));
        assert!(json.contains(r#""working_directory": "/workspace/project""#));
        assert!(json.contains(r#""current_date": "2026-03-26""#));
        let ctx2: PromptContext = serde_json::from_str(&json).unwrap();
        assert_eq!(ctx2.os_name.as_deref(), Some("linux"));
        assert_eq!(ctx2.shell_path.as_deref(), Some("/bin/bash"));
        assert_eq!(
            ctx2.working_directory.as_deref(),
            Some("/workspace/project")
        );
        assert_eq!(ctx2.current_date.as_deref(), Some("2026-03-26"));
    }
    #[test]
    fn test_user_info_fields_none_skipped_in_json() {
        let ctx = test_context();
        let json = serde_json::to_string(&ctx).unwrap();
        assert!(!json.contains("os_name"));
        assert!(!json.contains("shell_path"));
        assert!(!json.contains("\"working_directory\""));
        assert!(!json.contains("current_date"));
    }
    #[test]
    fn test_user_info_backward_compat_deserialization() {
        let json = r#"{
            "version": 1,
            "prompt_mode": "extend",
            "build_timestamp_utc": "2025-01-01T00:00:00Z",
            "agents_md_files": []
        }"#;
        let ctx: PromptContext = serde_json::from_str(json).unwrap();
        assert!(ctx.os_name.is_none());
        assert!(ctx.shell_path.is_none());
        assert!(ctx.working_directory.is_none());
        assert!(ctx.current_date.is_none());
    }
    #[test]
    fn test_placeholders_memory_enabled() {
        let mut ctx = test_context();
        ctx.memory_enabled = true;
        let p = ctx.placeholders();
        assert_eq!(p["memory_enabled"], true);
    }
    #[test]
    fn test_default_context() {
        let ctx = PromptContext::default();
        assert_eq!(ctx.version, 1);
        assert!(matches!(ctx.prompt_mode, PromptMode::Extend));
        assert!(ctx.prompt_body.is_none());
        assert!(ctx.agents_md_files.is_empty());
    }
    #[test]
    fn test_format_agents_md_section_empty() {
        let ctx = test_context();
        assert!(ctx.format_agents_md_section().is_none());
    }
    #[test]
    fn test_format_agents_md_section_non_empty() {
        let mut ctx = test_context();
        ctx.agents_md_files = vec![AgentConfigFile {
            file_name: "AGENTS.md".to_string(),
            file_path: "/repo/AGENTS.md".to_string(),
            content: "# Instructions".to_string(),
        }];
        let section = ctx.format_agents_md_section().unwrap();
        assert!(section.contains("# Instructions"));
        assert!(section.contains("<system-reminder>"));
    }
    #[test]
    fn test_format_personas_section_empty() {
        let ctx = test_context();
        assert!(ctx.format_personas_section().is_none());
    }
    #[test]
    fn test_format_personas_section_always_none() {
        let mut ctx = test_context();
        ctx.persona_summaries = vec!["- **reviewer** [user]: Meticulous code reviewer".to_string()];
        assert!(
            ctx.format_personas_section().is_none(),
            "persona section is disabled — persona param removed from task tool"
        );
    }
    /// AGENTS.md must reach the system prompt for the default template even
    /// AGENTS.md user reminder must be present for the default template
    /// when files are present.
    #[test]
    fn agents_md_user_reminder_included_for_default_template() {
        let mut ctx = test_context();
        ctx.system_prompt = TemplateOverride::None;
        ctx.agents_md_files = vec![AgentConfigFile {
            file_name: "AGENTS.md".to_string(),
            file_path: "/repo/AGENTS.md".to_string(),
            content: "# XYZZY_AGENTS_MD_MARKER".to_string(),
        }];
        let section = ctx
            .agents_md_user_reminder()
            .expect("default template must include AGENTS.md user reminder when files exist");
        assert!(section.contains("<system-reminder>"));
        assert!(section.contains("XYZZY_AGENTS_MD_MARKER"));
    }
    #[test]
    fn agents_md_user_reminder_suppressed_when_rules_are_in_the_prefix() {
        let mut ctx = test_context();
        ctx.include_browser_verification = true;
        ctx.agents_md_files = vec![AgentConfigFile {
            file_name: "AGENTS.md".to_string(),
            file_path: "/repo/AGENTS.md".to_string(),
            content: "# XYZZY_AGENTS_MD_MARKER".to_string(),
        }];
        assert!(ctx.agents_md_user_reminder().is_none());
        assert!(ctx.format_agents_md_section().is_some());
    }
    #[test]
    fn personas_user_reminder_always_none() {
        let mut ctx = test_context();
        ctx.persona_summaries = vec!["- **reviewer** [user]: Meticulous code reviewer".to_string()];
        assert!(
            ctx.personas_user_reminder().is_none(),
            "persona reminder is disabled — persona param removed from task tool"
        );
    }
    fn child_general_purpose_context() -> PromptContext {
        use crate::prompt::subagent_prompts;
        PromptContext {
            version: 1,
            prompt_mode: PromptMode::Extend,
            audience: PromptAudience::Subagent,
            prompt_body: Some(subagent_prompts::GENERAL_PURPOSE_PROMPT.to_string()),
            include_browser_verification: false,
            system_prompt: TemplateOverride::None,
            agents_md_files: vec![],
            persona_summaries: vec![
                "- **reviewer** [user]: Code reviewer".to_string(),
                "- **implementer** [user]: Code implementer".to_string(),
            ],
            build_timestamp_utc: TEST_TIMESTAMP.to_string(),
            memory_enabled: true,
            memory_global_path: None,
            memory_workspace_path: None,
            role_instructions: None,
            persona_instructions: None,
            os_name: None,
            shell_path: None,
            working_directory: None,
            current_date: None,
            is_non_interactive: false,
            system_prompt_label: default_system_prompt_label(),
        }
    }
    #[test]
    fn child_prompt_excludes_persona_catalog() {
        let ctx = child_general_purpose_context();
        assert!(ctx.format_personas_section().is_none());
        assert!(ctx.personas_user_reminder().is_none());
    }
    #[test]
    fn child_prompt_uses_subagent_audience() {
        let ctx = child_general_purpose_context();
        assert_eq!(ctx.audience, super::PromptAudience::Subagent);
    }
    #[test]
    fn child_prompt_includes_agents_md_when_present() {
        let mut ctx = child_general_purpose_context();
        ctx.agents_md_files = vec![AgentConfigFile {
            file_name: "AGENTS.md".to_string(),
            file_path: "/workspace/AGENTS.md".to_string(),
            content: "Build with `cargo build`".to_string(),
        }];
        let section = ctx.format_agents_md_section();
        assert!(
            section.is_some(),
            "child prompt should include AGENTS.md when files are discovered"
        );
    }
    #[test]
    fn child_prompt_no_agents_md_when_empty() {
        let ctx = child_general_purpose_context();
        let section = ctx.format_agents_md_section();
        assert!(
            section.is_none(),
            "child prompt has no AGENTS.md when none discovered"
        );
    }
    #[test]
    fn child_prompt_delivers_full_agents_md() {
        use crate::prompt::agents_md::AgentConfigFile;
        let mut ctx = child_general_purpose_context();
        ctx.agents_md_files = vec![AgentConfigFile {
            file_name: "AGENTS.md".to_string(),
            file_path: "/repo/AGENTS.md".to_string(),
            content: "X".repeat(5000),
        }];
        assert_eq!(ctx.audience, super::PromptAudience::Subagent);
        let reminder = ctx.agents_md_user_reminder().unwrap();
        assert!(
            reminder.contains(&"X".repeat(5000)),
            "child must receive full AGENTS.md content"
        );
        assert!(
            !reminder.contains("truncated"),
            "child AGENTS.md must not be truncated"
        );
    }
    #[test]
    fn child_prompt_uses_extend_mode() {
        let ctx = child_general_purpose_context();
        assert!(
            matches!(ctx.prompt_mode, PromptMode::Extend),
            "CURRENT: child uses Extend mode (inherits full base template)"
        );
    }
    #[test]
    fn child_prompt_has_prompt_body() {
        let ctx = child_general_purpose_context();
        assert!(
            ctx.prompt_body.is_some(),
            "CURRENT: child has a prompt body (GENERAL_PURPOSE_PROMPT)"
        );
        let body = ctx.prompt_body.as_deref().unwrap();
        assert!(
            body.contains("Strengths") && body.contains("Guidelines"),
            "body should contain structured general-purpose guidance sections"
        );
    }
    #[test]
    fn child_prompt_placeholders_include_memory_and_workspace() {
        let ctx = child_general_purpose_context();
        let placeholders = ctx.placeholders();
        assert_eq!(
            placeholders.get("memory_enabled").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert!(placeholders.get("role_instructions").is_some());
        assert!(placeholders.get("persona_instructions").is_some());
    }
    #[test]
    fn child_prompt_placeholders_include_role_and_persona() {
        let mut ctx = child_general_purpose_context();
        ctx.role_instructions = Some("Follow Rust conventions strictly".into());
        ctx.persona_instructions = Some("You are a meticulous reviewer".into());
        let placeholders = ctx.placeholders();
        assert_eq!(
            placeholders
                .get("role_instructions")
                .and_then(|v| v.as_str()),
            Some("Follow Rust conventions strictly")
        );
        assert_eq!(
            placeholders
                .get("persona_instructions")
                .and_then(|v| v.as_str()),
            Some("You are a meticulous reviewer")
        );
    }
    #[test]
    fn child_prompt_placeholders_empty_role_persona_when_unset() {
        let ctx = child_general_purpose_context();
        let placeholders = ctx.placeholders();
        assert_eq!(
            placeholders
                .get("role_instructions")
                .and_then(|v| v.as_str()),
            Some("")
        );
        assert_eq!(
            placeholders
                .get("persona_instructions")
                .and_then(|v| v.as_str()),
            Some("")
        );
    }
    #[test]
    fn child_prompt_has_no_system_prompt_override() {
        let ctx = child_general_purpose_context();
        assert!(
            ctx.system_prompt == TemplateOverride::None,
            "CURRENT: child has no custom system_prompt (uses BASE_TEMPLATE)"
        );
    }
    #[test]
    fn parent_vs_child_section_differences() {
        let parent = test_context();
        let child = child_general_purpose_context();
        assert_eq!(parent.audience, super::PromptAudience::Primary);
        assert_eq!(child.audience, super::PromptAudience::Subagent);
        assert!(!child.persona_summaries.is_empty());
        assert!(child.memory_enabled);
        assert!(child.prompt_body.is_some());
        assert!(parent.prompt_body.is_none());
    }
    #[test]
    fn child_prompt_context_is_complete() {
        let ctx = child_general_purpose_context();
        assert!(ctx.prompt_body.is_some());
        assert!(matches!(ctx.prompt_mode, PromptMode::Extend));
        assert_eq!(ctx.audience, super::PromptAudience::Subagent);
        assert!(ctx.memory_enabled);
        assert!(ctx.system_prompt == TemplateOverride::None);
        let p = ctx.placeholders();
        assert!(p.get("memory_enabled").is_some());
        assert!(p.get("role_instructions").is_some());
        assert!(p.get("persona_instructions").is_some());
    }
    fn render_subagent_template(ctx: minijinja::Value) -> String {
        let mut env = minijinja::Environment::new();
        env.set_syntax(
            minijinja::syntax::SyntaxConfig::builder()
                .block_delimiters("${%", "%}")
                .variable_delimiters("${{", "}}")
                .comment_delimiters("${#", "#}")
                .build()
                .unwrap(),
        );
        let tmpl = crate::prompt::template::subagent_template();
        env.add_template("prompt", &tmpl).unwrap();
        env.get_template("prompt").unwrap().render(ctx).unwrap()
    }
    fn base_template_ctx() -> minijinja::Value {
        minijinja::context! {
            os_name => "linux",
            shell_path => "/bin/bash",
            working_directory => "/workspace",
            current_date => "2026-03-26",
            memory_enabled => true,
            role_instructions => "",
            persona_instructions => "",
            tools => minijinja::context! {
                by_kind => minijinja::context! {
                    read => "hashline_read",
                    edit => "hashline_edit",
                    search => "hashline_grep",
                    execute => "run_terminal_cmd",
                    background_task_action => "get_task_output",
                    memory_search => "memory_search",
                    memory_get => "memory_get",
                }
            },
        }
    }
    #[test]
    fn child_rendered_prompt_includes_memory_section() {
        let rendered = render_subagent_template(base_template_ctx());
        assert!(
            rendered.contains("<memory>"),
            "should contain <memory> section"
        );
        assert!(
            rendered.contains("memory_search"),
            "should reference memory_search"
        );
    }
    #[test]
    fn child_rendered_prompt_includes_user_info_block() {
        let rendered = render_subagent_template(base_template_ctx());
        assert!(rendered.contains("OS: linux"), "should contain OS value");
        assert!(
            rendered.contains("Shell: /bin/bash"),
            "should contain Shell value"
        );
        assert!(
            rendered.contains("Workspace Path: /workspace"),
            "should contain Workspace Path value"
        );
        assert!(
            rendered.contains("Current Date: 2026-03-26"),
            "should contain Current Date value"
        );
    }
    #[test]
    fn child_rendered_prompt_includes_project_instructions_like_main_agent() {
        let rendered = render_subagent_template(base_template_ctx());
        assert!(
            rendered.contains("<project_instructions_spec>"),
            "subagent must include project_instructions_spec"
        );
        assert!(
            rendered.contains("## Project Instruction Files"),
            "subagent project instructions must match the main agent spec"
        );
        assert!(
            rendered.contains("you must check for additional project instruction files"),
            "subagent must be told to proactively check nested AGENTS.md"
        );
    }
    #[test]
    fn child_rendered_prompt_excludes_parent_only_sections() {
        let rendered = render_subagent_template(base_template_ctx());
        assert!(!rendered.contains("## Task Management"));
        assert!(!rendered.contains("## No time estimates"));
    }
    #[test]
    fn child_rendered_prompt_includes_role_and_persona_sections() {
        let ctx = minijinja::context! {
            os_name => "linux",
            shell_path => "/bin/bash",
            working_directory => "/workspace",
            current_date => "2026-03-26",
            memory_enabled => false,
            role_instructions => "Follow Rust conventions",
            persona_instructions => "You are a code reviewer",
            tools => minijinja::context! {
                by_kind => minijinja::context! {
                    read => "hashline_read",
                    edit => "hashline_edit",
                    search => "hashline_grep",
                    execute => "run_terminal_cmd",
                    background_task_action => "get_task_output",
                }
            },

        };
        let rendered = render_subagent_template(ctx);
        assert!(rendered.contains("<role-instructions>"));
        assert!(rendered.contains("Follow Rust conventions"));
        assert!(rendered.contains("<persona>"));
        assert!(rendered.contains("You are a code reviewer"));
        assert!(
            !rendered.contains("<memory>"),
            "memory should be absent when disabled"
        );
    }
    #[test]
    fn child_rendered_prompt_has_hashline_guidance() {
        let rendered = render_subagent_template(base_template_ctx());
        assert!(
            rendered.contains("hashline workflow"),
            "should include hashline guidance"
        );
        assert!(
            rendered.contains("batch semantics"),
            "should include batch semantics"
        );
    }
    #[test]
    fn child_rendered_prompt_has_background_tasks_when_execute_available() {
        let rendered = render_subagent_template(base_template_ctx());
        assert!(
            rendered.contains("<background_tasks>"),
            "should include background_tasks section when execute tool exists"
        );
        assert!(
            rendered.contains("background"),
            "background_tasks should mention background flag"
        );
    }
    #[test]
    fn child_rendered_prompt_has_code_change_rules_when_edit_available() {
        let rendered = render_subagent_template(base_template_ctx());
        assert!(
            rendered.contains("<making_code_changes>"),
            "should include making_code_changes when edit tools are available"
        );
        assert!(
            rendered.contains("</making_code_changes>"),
            "making_code_changes section should be properly closed"
        );
    }
    #[test]
    fn child_rendered_prompt_omits_background_tasks_without_execute() {
        let ctx = minijinja::context! {
            os_name => "linux",
            shell_path => "/bin/bash",
            working_directory => "/workspace",
            current_date => "2026-03-26",
            memory_enabled => false,
            role_instructions => "",
            persona_instructions => "",
            tools => minijinja::context! {
                by_kind => minijinja::context! {
                    read => "hashline_read",
                    edit => "hashline_edit",
                    search => "hashline_grep",
                }
            },
        };
        let rendered = render_subagent_template(ctx);
        assert!(
            !rendered.contains("<background_tasks>"),
            "background_tasks should be absent without execute tool"
        );
        assert!(
            rendered.contains("hashline workflow"),
            "hashline guidance should still be present"
        );
    }
    #[test]
    fn child_rendered_prompt_omits_code_change_rules_without_edit_tools() {
        let ctx = minijinja::context! {
            os_name => "linux",
            shell_path => "/bin/bash",
            working_directory => "/workspace",
            current_date => "2026-03-26",
            memory_enabled => false,
            role_instructions => "",
            persona_instructions => "",
            tools => minijinja::context! {
                by_kind => minijinja::context! {
                    read => "hashline_read",
                    search => "hashline_grep",
                    execute => "run_terminal_cmd",
                    background_task_action => "get_task_output",
                }
            },

        };
        let rendered = render_subagent_template(ctx);
        assert!(
            !rendered.contains("<making_code_changes>"),
            "read-only agents should not see code change rules"
        );
        assert!(rendered.contains("<tool_calling>"));
        assert!(rendered.contains("<background_tasks>"));
        assert!(rendered.contains("<formatting>"));
    }
    #[test]
    fn rendered_prompt_size_read_only() {
        let ctx = minijinja::context! {
            os_name => "linux",
            shell_path => "/bin/bash",
            working_directory => "/workspace",
            current_date => "2026-03-26",
            memory_enabled => false,
            role_instructions => "",
            persona_instructions => "",
            tools => minijinja::context! {
                by_kind => minijinja::context! {
                    read => "hashline_read",
                    search => "hashline_grep",
                    execute => "run_terminal_cmd",
                    background_task_action => "get_task_output",
                }
            },

        };
        let rendered = render_subagent_template(ctx);
        let full = render_subagent_template(base_template_ctx());
        assert!(
            rendered.len() < full.len(),
            "read-only prompt ({}) should be smaller than general-purpose ({})",
            rendered.len(),
            full.len()
        );
    }
    #[test]
    fn child_rendered_prompt_omits_edit_references_without_edit_tool() {
        let ctx = minijinja::context! {
            os_name => "linux",
            shell_path => "/bin/bash",
            working_directory => "/workspace",
            current_date => "2026-03-26",
            memory_enabled => false,
            role_instructions => "",
            persona_instructions => "",
            tools => minijinja::context! {
                by_kind => minijinja::context! {
                    read => "read_file",
                    search => "grep",
                    execute => "run_terminal_cmd",
                    background_task_action => "get_task_output",
                }
            },

        };
        let rendered = render_subagent_template(ctx);
        assert!(
            !rendered.contains("for editing"),
            "should not mention editing when edit tool is absent"
        );
    }
    #[test]
    fn child_rendered_prompt_omits_execute_references_without_execute_tool() {
        let ctx = minijinja::context! {
            os_name => "linux",
            shell_path => "/bin/bash",
            working_directory => "/workspace",
            current_date => "2026-03-26",
            memory_enabled => false,
            role_instructions => "",
            persona_instructions => "",
            tools => minijinja::context! {
                by_kind => minijinja::context! {
                    read => "read_file",
                    edit => "search_replace",
                    search => "grep",
                }
            },
        };
        let rendered = render_subagent_template(ctx);
        assert!(
            !rendered.contains("system commands"),
            "should not mention system commands when execute tool is absent"
        );
        assert!(
            !rendered.contains("Reserve"),
            "should not mention Reserve (bash) when execute tool is absent"
        );
    }
    #[test]
    fn child_rendered_prompt_omits_both_edit_and_execute_references() {
        let ctx = minijinja::context! {
            os_name => "linux",
            shell_path => "/bin/bash",
            working_directory => "/workspace",
            current_date => "2026-03-26",
            memory_enabled => false,
            role_instructions => "",
            persona_instructions => "",
            tools => minijinja::context! {
                by_kind => minijinja::context! {
                    read => "read_file",
                    search => "grep",
                }
            },
        };
        let rendered = render_subagent_template(ctx);
        assert!(
            !rendered.contains("for editing"),
            "should not mention editing"
        );
        assert!(
            !rendered.contains("system commands"),
            "should not mention system commands"
        );
        assert!(!rendered.contains("Reserve"), "should not mention Reserve");
        assert!(
            rendered.contains("`read_file` for reading."),
            "tool_calling line should end cleanly after read reference"
        );
    }
    #[test]
    fn test_prompt_body_none_skipped_in_json() {
        let ctx = test_context();
        let json = serde_json::to_string(&ctx).unwrap();
        assert!(
            !json.contains("prompt_body"),
            "prompt_body should be skipped when None"
        );
    }
    #[test]
    fn test_prompt_body_some_included_in_json() {
        let mut ctx = test_context();
        ctx.prompt_body = Some("custom body".to_string());
        let json = serde_json::to_string(&ctx).unwrap();
        assert!(
            json.contains("prompt_body"),
            "prompt_body should be included when Some"
        );
    }
    /// Verify that AGENTS.md file paths rewritten to the display cwd are
    /// rendered into the system prompt correctly. When `AgentConfigFile.file_path`
    /// uses the display path, the rendered `## From:` line must not contain
    /// the overlay/worktree path.
    #[test]
    fn test_agents_md_paths_use_display_cwd_in_rendered_section() {
        let display_path = "/home/user/my-project";
        let overlay_path = "/root/.echo-agent/worktrees/my-project/ab-123-a-overlay";
        let ctx = PromptContext {
            agents_md_files: vec![AgentConfigFile {
                file_name: "AGENTS.md".to_string(),
                file_path: format!("{display_path}/AGENTS.md"),
                content: "# Project rules".to_string(),
            }],
            ..test_context()
        };
        let section = ctx.format_agents_md_section().unwrap();
        assert!(
            section.contains(&format!("## From: {display_path}/AGENTS.md")),
            "rendered AGENTS section must show the display path"
        );
        assert!(
            !section.contains(overlay_path),
            "rendered AGENTS section must not contain the overlay path"
        );
    }
    #[test]
    fn full_mode_prelude_contains_user_info_block() {
        let prelude_format = format!(
            "<user_info>\nOS: {}\nShell: {}\nWorkspace Path: {}\nCurrent Date: {}\n</user_info>",
            "linux", "/bin/bash", "/workspace/project", "2026-03-24"
        );
        assert!(prelude_format.contains("<user_info>"));
        assert!(prelude_format.contains("Workspace Path: /workspace/project"));
        assert!(prelude_format.contains("</user_info>"));
    }
    #[test]
    fn built_in_prompts_do_not_contain_user_info_block() {
        let gp = super::super::subagent_prompts::GENERAL_PURPOSE_PROMPT;
        let explore = super::super::subagent_prompts::EXPLORE_PROMPT;
        let plan = super::super::subagent_prompts::PLAN_PROMPT;
        assert!(
            !gp.contains("OS: linux"),
            "prompt text should not contain actual OS value"
        );
        assert!(
            !explore.contains("Workspace Path:"),
            "prompt text should not contain Workspace Path field"
        );
        assert!(
            !plan.contains("Shell: /bin/bash"),
            "prompt text should not contain actual Shell value"
        );
    }
    #[test]
    fn workspace_boundary_in_general_purpose_prompt() {
        let prompt = super::super::subagent_prompts::GENERAL_PURPOSE_PROMPT;
        assert!(
            prompt.contains("Workspace boundary"),
            "general-purpose prompt should contain workspace boundary guidance"
        );
        assert!(
            prompt.contains("<user_info>"),
            "general-purpose prompt should reference <user_info>"
        );
    }
    #[test]
    fn workspace_boundary_in_explore_prompt() {
        let prompt = super::super::subagent_prompts::EXPLORE_PROMPT;
        assert!(
            prompt.contains("Workspace boundary"),
            "explore prompt should contain workspace boundary guidance"
        );
        assert!(
            prompt.contains("default search scope"),
            "explore should mention default search scope"
        );
    }
    #[test]
    fn workspace_boundary_in_plan_prompt() {
        let prompt = super::super::subagent_prompts::PLAN_PROMPT;
        assert!(
            prompt.contains("Workspace boundary"),
            "plan prompt should contain workspace boundary guidance"
        );
        assert!(
            prompt.contains("default analysis scope"),
            "plan should mention default analysis scope"
        );
    }
    #[test]
    fn general_purpose_prompt_specialization_keywords() {
        let prompt = super::super::subagent_prompts::GENERAL_PURPOSE_PROMPT;
        let keywords = [
            "broad searches",
            "Multi-file analysis",
            "NEVER create files",
            "documentation files",
            "absolute file paths",
        ];
        for kw in &keywords {
            assert!(
                prompt.contains(kw),
                "general-purpose prompt missing specialization keyword: {kw}"
            );
        }
    }
    #[test]
    fn explore_prompt_specialization_keywords() {
        let prompt = super::super::subagent_prompts::EXPLORE_PROMPT;
        let keywords = [
            "read-only",
            "READ-ONLY MODE",
            "glob patterns",
            "regex",
            "parallel tool calls",
            "thoroughness level",
        ];
        for kw in &keywords {
            assert!(
                prompt.contains(kw),
                "explore prompt missing specialization keyword: {kw}"
            );
        }
    }
    #[test]
    fn plan_prompt_specialization_keywords() {
        let prompt = super::super::subagent_prompts::PLAN_PROMPT;
        let keywords = [
            "read-only",
            "READ-ONLY MODE",
            "architect",
            "Critical Files for Implementation",
            "trade-offs",
            "step-by-step",
        ];
        for kw in &keywords {
            assert!(
                prompt.contains(kw),
                "plan prompt missing specialization keyword: {kw}"
            );
        }
    }
    #[test]
    fn trimmed_prompts_no_redundant_identity() {
        let gp = super::super::subagent_prompts::GENERAL_PURPOSE_PROMPT;
        let explore = super::super::subagent_prompts::EXPLORE_PROMPT;
        let plan = super::super::subagent_prompts::PLAN_PROMPT;
        for (name, prompt) in [
            ("general-purpose", gp),
            ("explore", explore),
            ("plan", plan),
        ] {
            assert!(
                !prompt.contains("You are a EchoAgent Build agent"),
                "{name} prompt should not duplicate base template identity"
            );
        }
    }
    #[test]
    fn trimmed_prompts_no_redundant_formatting_rules() {
        let gp = super::super::subagent_prompts::GENERAL_PURPOSE_PROMPT;
        let explore = super::super::subagent_prompts::EXPLORE_PROMPT;
        let plan = super::super::subagent_prompts::PLAN_PROMPT;
        for (name, prompt) in [
            ("general-purpose", gp),
            ("explore", explore),
            ("plan", plan),
        ] {
            assert!(
                !prompt.contains("avoid using emojis"),
                "{name} prompt should not duplicate formatting rules from base template"
            );
        }
    }
    #[test]
    fn all_prompts_reference_tool_templates() {
        let gp = super::super::subagent_prompts::GENERAL_PURPOSE_PROMPT;
        let explore = super::super::subagent_prompts::EXPLORE_PROMPT;
        let plan = super::super::subagent_prompts::PLAN_PROMPT;
        for (name, prompt) in [
            ("general-purpose", gp),
            ("explore", explore),
            ("plan", plan),
        ] {
            assert!(
                prompt.contains("${{ tools.by_kind."),
                "{name} prompt should reference tool template variables"
            );
        }
    }
    #[test]
    fn read_only_prompts_share_consistent_constraint() {
        let explore = super::super::subagent_prompts::EXPLORE_PROMPT;
        let plan = super::super::subagent_prompts::PLAN_PROMPT;
        for (name, prompt) in [("explore", explore), ("plan", plan)] {
            assert!(
                prompt.contains("NO file editing tools"),
                "{name} prompt must declare no editing tools"
            );
            assert!(
                prompt.contains("Do not create, modify, or delete"),
                "{name} prompt must forbid create/modify/delete"
            );
        }
        let gp = super::super::subagent_prompts::GENERAL_PURPOSE_PROMPT;
        assert!(
            !gp.contains("READ-ONLY MODE"),
            "general-purpose should not be read-only"
        );
    }
    #[test]
    fn normalize_clears_persona_summaries_for_subagent() {
        let mut ctx = child_general_purpose_context();
        ctx.persona_summaries = vec!["- **reviewer**: Reviews code".to_string()];
        assert!(!ctx.persona_summaries.is_empty());
        ctx.normalize_for_persistence();
        assert!(
            ctx.persona_summaries.is_empty(),
            "persona summaries must be cleared for subagent"
        );
    }
    #[test]
    fn normalize_preserves_full_agents_md_for_subagent() {
        let mut ctx = child_general_purpose_context();
        ctx.agents_md_files = vec![super::super::agents_md::AgentConfigFile {
            file_name: "AGENTS.md".to_string(),
            file_path: "/repo/AGENTS.md".to_string(),
            content: "X".repeat(5000),
        }];
        ctx.normalize_for_persistence();
        assert_eq!(
            ctx.agents_md_files[0].content.chars().count(),
            5000,
            "AGENTS content must be preserved in full for subagents (no cap)"
        );
    }
    #[test]
    fn normalize_preserves_short_agents_md() {
        let mut ctx = child_general_purpose_context();
        ctx.agents_md_files = vec![super::super::agents_md::AgentConfigFile {
            file_name: "AGENTS.md".to_string(),
            file_path: "/repo/sub/AGENTS.md".to_string(),
            content: "Short rules".to_string(),
        }];
        ctx.normalize_for_persistence();
        assert_eq!(ctx.agents_md_files[0].content, "Short rules");
    }
    #[test]
    fn normalize_preserves_role_and_persona_instructions() {
        let mut ctx = child_general_purpose_context();
        ctx.role_instructions = Some("Follow Rust conventions".to_string());
        ctx.persona_instructions = Some("You are a code reviewer".to_string());
        ctx.normalize_for_persistence();
        assert_eq!(
            ctx.role_instructions.as_deref(),
            Some("Follow Rust conventions")
        );
        assert_eq!(
            ctx.persona_instructions.as_deref(),
            Some("You are a code reviewer")
        );
    }
    #[test]
    fn normalize_is_noop_for_primary() {
        let mut ctx = test_context();
        ctx.persona_summaries = vec!["- **reviewer**: Reviews code".to_string()];
        ctx.normalize_for_persistence();
        assert!(
            !ctx.persona_summaries.is_empty(),
            "primary must keep persona summaries"
        );
    }
}
