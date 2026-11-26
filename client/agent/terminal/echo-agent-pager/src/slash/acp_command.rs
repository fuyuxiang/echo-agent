//! Wrapper that turns an ACP `AvailableCommand` into a `SlashCommand`.
//!
//! ACP-advertised commands appear in the dropdown but pass through to the
//! shell for execution. The wrapper stores `String` fields -- consistent
//! with the `&str` trait design.
//!
//! Skills (`SkillMeta::Skill`) are also passed through as `/name args` for
//! the shell to expand, but marked `InjectSkill` for rendering.

use agent_client_protocol as acp;
use echo_agent_tools::implementations::skills::types::SkillScope;

use super::command::{CommandExecCtx, CommandProvenance, CommandResult, SlashCommand};

/// Identity of a skill as advertised in ACP `_meta`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillIdentity {
    pub path: String,
    pub scope: SkillScope,
    /// Plugin install name when present (`acme`).
    pub plugin_name: Option<String>,
}

impl SkillIdentity {
    /// Plugin install name, else the scope (plugin skills can carry any scope).
    pub fn source(&self) -> &str {
        self.plugin_name.as_deref().unwrap_or(self.scope.as_ref())
    }
}

/// Parsed ACP `_meta` skill fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillMeta {
    /// No skill keys.
    Absent,
    Skill(SkillIdentity),
    /// Unknown `scope` string (e.g. `"workflow"`). Pass through, don't error.
    Foreign,
    /// Skill-like keys present but invalid. Invocation errors rather than
    /// silently degrading.
    Malformed,
}

impl SkillMeta {
    pub fn parse(meta: Option<&serde_json::Map<String, serde_json::Value>>) -> Self {
        let Some(m) = meta else {
            return SkillMeta::Absent;
        };
        let path_val = m.get("path");
        let scope_val = m.get("scope");
        if path_val.is_none() && scope_val.is_none() {
            return SkillMeta::Absent;
        }
        let path = path_val.and_then(|v| v.as_str());
        let scope: Option<SkillScope> =
            scope_val.and_then(|v| serde_json::from_value(v.clone()).ok());
        match (path, scope) {
            (Some(path), Some(scope)) => SkillMeta::Skill(SkillIdentity {
                path: path.to_string(),
                scope,
                plugin_name: trimmed_string_field(m, "pluginName"),
            }),
            (_, None) if scope_val.is_some_and(|v| v.is_string()) => SkillMeta::Foreign,
            _ => SkillMeta::Malformed,
        }
    }
}

fn trimmed_string_field(
    m: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Option<String> {
    m.get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// A slash command backed by an ACP `AvailableCommand`.
pub struct AcpSlashCommand {
    name: String,
    description: String,
    has_args: bool,
    arg_hint: Option<String>,
    skill: SkillMeta,
}

impl SlashCommand for AcpSlashCommand {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn provenance(&self) -> CommandProvenance {
        match &self.skill {
            SkillMeta::Skill(identity) => CommandProvenance::Skill {
                source: identity.source().to_string(),
            },
            _ => CommandProvenance::Shell,
        }
    }

    fn usage(&self) -> &str {
        &self.name
    }

    fn takes_args(&self) -> bool {
        self.has_args
    }

    /// ACP commands always accept Enter -- args are never required locally.
    /// The shell validates.
    fn args_required(&self) -> bool {
        false
    }

    fn arg_placeholder(&self) -> Option<&str> {
        self.arg_hint.as_deref()
    }

    fn is_skill(&self) -> bool {
        matches!(self.skill, SkillMeta::Skill(_))
    }

    fn run(&self, _ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        let text = if args.trim().is_empty() {
            format!("/{}", self.name)
        } else {
            format!("/{} {}", self.name, args)
        };
        match self.skill {
            SkillMeta::Malformed => {
                CommandResult::Error(format!("Malformed skill metadata for /{}", self.name))
            }
            SkillMeta::Absent | SkillMeta::Foreign => CommandResult::PassThrough(text),
            SkillMeta::Skill(_) => CommandResult::InjectSkill {
                display_text: text.clone(),
                prompt_blocks: vec![acp::ContentBlock::Text(acp::TextContent::new(text))],
                display_as_skill: true,
                scheduled_task_preview: None,
            },
        }
    }
}

impl From<&acp::AvailableCommand> for AcpSlashCommand {
    fn from(cmd: &acp::AvailableCommand) -> Self {
        let arg_hint = cmd.input.as_ref().and_then(|input| match input {
            acp::AvailableCommandInput::Unstructured(u) => Some(u.hint.clone()),
            // TODO(acp-0.10): `AvailableCommandInput` is #[non_exhaustive].
            _ => None,
        });

        Self {
            name: cmd.name.clone(),
            description: cmd.description.clone(),
            // ACP commands always accept free-form input. The shell handles
            // whatever text follows the command name. The `input` field only
            // determines the placeholder hint, not whether args are allowed.
            has_args: true,
            arg_hint,
            skill: SkillMeta::parse(cmd.meta.as_ref()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_cmd(name: &str, meta: Option<serde_json::Value>) -> acp::AvailableCommand {
        let mut cmd = acp::AvailableCommand::new(name.to_string(), format!("{name} command"));
        if let Some(m) = meta.and_then(|v| v.as_object().cloned()) {
            cmd = cmd.meta(m);
        }
        cmd
    }

    fn parse(meta: serde_json::Value) -> SkillMeta {
        SkillMeta::parse(meta.as_object())
    }

    #[test]
    fn no_meta_or_unrelated_meta_is_absent() {
        assert_eq!(SkillMeta::parse(None), SkillMeta::Absent);
        assert_eq!(
            parse(serde_json::json!({"foo": "bar", "baz": 42})),
            SkillMeta::Absent
        );
    }

    #[test]
    fn valid_skill_meta_parses_identity() {
        let meta = serde_json::json!({
            "scope": "local",
            "path": "/home/user/.echo-agent/skills/commit/SKILL.md",
        });
        assert_eq!(
            parse(meta),
            SkillMeta::Skill(SkillIdentity {
                path: "/home/user/.echo-agent/skills/commit/SKILL.md".to_string(),
                scope: SkillScope::Local,
                plugin_name: None,
            })
        );
    }

    #[test]
    fn plugin_meta_carries_plugin_name() {
        let meta = serde_json::json!({
            "scope": "plugin",
            "path": "/plugins/acme/skills/login/SKILL.md",
            "pluginName": "acme",
            "qualifiedName": "acme:login",
        });
        assert_eq!(
            parse(meta),
            SkillMeta::Skill(SkillIdentity {
                path: "/plugins/acme/skills/login/SKILL.md".to_string(),
                scope: SkillScope::Plugin,
                plugin_name: Some("acme".to_string()),
            })
        );
    }

    #[test]
    fn unknown_scope_string_is_foreign_not_malformed() {
        let meta = serde_json::json!({
            "scope": "workflow",
            "path": ".echo-agent/workflows/pr-cleanup.rhai",
        });
        assert_eq!(parse(meta), SkillMeta::Foreign);
    }

    #[test]
    fn missing_or_mistyped_skill_keys_are_malformed() {
        for (label, meta) in [
            ("scope without path", serde_json::json!({"scope": "user"})),
            (
                "path without scope",
                serde_json::json!({"path": "/path/to/SKILL.md"}),
            ),
            (
                "path is not a string",
                serde_json::json!({"scope": "local", "path": 42}),
            ),
            (
                "scope is not a string",
                serde_json::json!({"scope": 42, "path": "/path/to/SKILL.md"}),
            ),
        ] {
            assert_eq!(parse(meta), SkillMeta::Malformed, "{label}");
        }
    }

    #[test]
    fn empty_plugin_name_is_dropped() {
        let meta = serde_json::json!({
            "scope": "plugin",
            "path": "/x/SKILL.md",
            "pluginName": "  ",
        });
        match parse(meta) {
            SkillMeta::Skill(identity) => assert_eq!(identity.plugin_name, None),
            other => panic!("expected Skill, got {other:?}"),
        }
    }

    fn identity(scope: SkillScope, plugin_name: Option<&str>) -> SkillIdentity {
        SkillIdentity {
            path: "/x/SKILL.md".to_string(),
            scope,
            plugin_name: plugin_name.map(str::to_string),
        }
    }

    #[test]
    fn source_prefers_plugin_name_over_scope() {
        assert_eq!(identity(SkillScope::Plugin, Some("acme")).source(), "acme");
        assert_eq!(identity(SkillScope::Repo, Some("acme")).source(), "acme");
        assert_eq!(identity(SkillScope::Plugin, None).source(), "plugin");
        assert_eq!(identity(SkillScope::Local, None).source(), "local");
    }

    #[test]
    fn provenance_distinguishes_skills_from_shell_commands() {
        let skill = AcpSlashCommand::from(&make_cmd(
            "login",
            Some(serde_json::json!({
                "scope": "plugin",
                "path": "/plugins/acme/skills/login/SKILL.md",
                "pluginName": "acme",
            })),
        ));
        assert!(skill.is_skill());
        assert_eq!(
            skill.provenance(),
            CommandProvenance::Skill {
                source: "acme".to_string()
            }
        );

        let shell_cmd = AcpSlashCommand::from(&make_cmd("flush", None));
        assert!(!shell_cmd.is_skill());
        assert_eq!(shell_cmd.provenance(), CommandProvenance::Shell);
    }

    fn make_skill_cmd(name: &str, path: &str, scope: SkillScope) -> AcpSlashCommand {
        AcpSlashCommand {
            name: name.to_string(),
            description: format!("{name} skill"),
            has_args: true,
            arg_hint: None,
            skill: SkillMeta::Skill(SkillIdentity {
                path: path.to_string(),
                scope,
                plugin_name: None,
            }),
        }
    }

    fn make_exec_ctx() -> CommandExecCtx<'static> {
        use crate::acp::model_state::ModelState;
        let models = Box::leak(Box::new(ModelState::default()));
        let bundle = Box::leak(Box::new(crate::app::bundle::BundleState::default()));
        CommandExecCtx {
            models,
            session_id: None,
            bundle_state: bundle,
            screen_mode: crate::app::ScreenMode::Inline,
            billing_surface_visible: true,
            usage_command_visible: true,
            pager_state: crate::settings::PagerLocalSnapshot {
                multiline_mode: false,
                yolo_mode: false,
                ..crate::settings::PagerLocalSnapshot::default()
            },
        }
    }

    #[test]
    fn run_non_skill_passes_through() {
        let cmd = AcpSlashCommand::from(&make_cmd("flush", None));
        let mut ctx = make_exec_ctx();
        let result = cmd.run(&mut ctx, "");
        assert!(matches!(result, CommandResult::PassThrough(t) if t == "/flush"));
    }

    #[test]
    fn run_foreign_kind_passes_through_with_args() {
        let cmd = AcpSlashCommand::from(&make_cmd(
            "pr-cleanup",
            Some(serde_json::json!({
                "scope": "workflow",
                "path": ".echo-agent/workflows/pr-cleanup.rhai",
            })),
        ));
        let mut ctx = make_exec_ctx();
        match cmd.run(&mut ctx, "fix the branch") {
            CommandResult::PassThrough(text) => assert_eq!(text, "/pr-cleanup fix the branch"),
            other => panic!("expected PassThrough, got {other:?}"),
        }
    }

    #[test]
    fn run_malformed_meta_returns_error() {
        let cmd = AcpSlashCommand::from(&make_cmd(
            "broken",
            Some(serde_json::json!({"scope": "local", "path": 42})),
        ));
        let mut ctx = make_exec_ctx();
        let result = cmd.run(&mut ctx, "");
        assert!(matches!(result, CommandResult::Error(msg) if msg.contains("Malformed")));
    }

    #[test]
    fn run_skill_sends_raw_slash_text_to_shell() {
        for (name, args, expected) in [
            ("commit", "fix the auth bug", "/commit fix the auth bug"),
            ("deploy", "", "/deploy"),
            ("local:compact", "", "/local:compact"),
        ] {
            let cmd = make_skill_cmd(name, "/nonexistent/path/SKILL.md", SkillScope::Local);
            let mut ctx = make_exec_ctx();
            match cmd.run(&mut ctx, args) {
                CommandResult::InjectSkill {
                    display_text,
                    prompt_blocks,
                    ..
                } => {
                    assert_eq!(display_text, expected, "/{name} {args}");
                    let [acp::ContentBlock::Text(block)] = &prompt_blocks[..] else {
                        panic!("/{name}: expected a single Text block, got {prompt_blocks:?}");
                    };
                    assert_eq!(block.text, expected, "/{name} {args}");
                    assert!(
                        !block.text.contains('<'),
                        "/{name}: no client-side XML markup: {}",
                        block.text
                    );
                }
                other => panic!("/{name}: expected InjectSkill, got {other:?}"),
            }
        }
    }
}
