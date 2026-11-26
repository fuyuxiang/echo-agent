//! Per-agent first-user-message rendering.
//!
//! Mirrors `prompt::context::PromptContext` but for the first user message
//! (the prefix that contains `<user_info>`, `<git_status>`, optional
//! workspace overview, optional rules / skills / MCP listings).
//!
//! `UserMessageTemplate` selects the rendering strategy:
//! - `Default`: the legacy EchoAgent Build prefix (built by the shell layer).
//! - `Custom`: caller-supplied MiniJinja template string (same delimiters as
//!   the system prompt templates).
//!
//! The shell layer gathers session-scoped inputs (cwd, vcs status, rule
//! files, skill registry, MCP servers) and hands them to
//! `UserMessageContext::render`, which dispatches on `template`.
use crate::prompt::agents_md::AgentConfigFile;
use chrono::NaiveDate;
use echo_agent_tools::bridge::ToolBridge;
use echo_agent_tools::implementations::skills::types::SkillInfo;
use echo_agent_tools::types::skill_discovery_tracker::{XmlRenderMode, format_announcement_xml};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;
/// Date format for the `Today's date` field of the user-message preamble
/// (e.g. "Friday Apr 24, 2026"). Any format change is observable to the model.
pub const USER_MESSAGE_DATE_FORMAT: &str = "%A %b %-d, %Y";
/// Per-repo character cap applied to `vcs_status` at render time. The
/// `<git_status>` block has no token budget -- this character cap is the only
/// size control, and it is applied per repo at render, never at gather, so
/// other consumers of the raw status are unaffected.
pub const GIT_STATUS_CHARACTER_LIMIT: usize = 10_000;
const RULES_SECTION_INTRO: &str = "The rules section has a number of possible rules/memories/context that you should consider. In each subsection, we provide instructions about what information the subsection contains and how you should consider/follow the contents of the subsection.";
fn neutralize_file_rule_content(content: &str) -> String {
    content
        .replace("</rules>", "&lt;/rules>")
        .replace("<rules>", "&lt;rules>")
        .replace("</system-reminder>", "&lt;/system-reminder>")
        .replace("<system-reminder>", "&lt;system-reminder>")
        .replace("</system_reminder>", "&lt;/system_reminder>")
        .replace("<system_reminder>", "&lt;system_reminder>")
}
/// Keep markdown headings on their own line instead of glued to the opening tag
/// (`<user_rule># Personal Rules` is not a heading).
fn push_rule_body(out: &mut String, content: &str) {
    if !content.starts_with('\n') {
        out.push('\n');
    }
    out.push_str(content);
    if !content.ends_with('\n') {
        out.push('\n');
    }
}
pub fn format_rules_section(
    workspace_rules: &[RuleEntry],
    user_rules: &[RuleEntry],
) -> Option<String> {
    if workspace_rules.is_empty() && user_rules.is_empty() {
        return None;
    }
    let mut out = String::from("<rules>\n");
    out.push_str(RULES_SECTION_INTRO);
    out.push_str("\n\n\n");
    if !workspace_rules.is_empty() {
        out.push_str(
            "<always_applied_workspace_rules description=\"These are workspace-level rules that the agent must always follow.\">\n",
        );
        for (i, rule) in workspace_rules.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            out.push_str("<always_applied_workspace_rule name=\"");
            out.push_str(&rule.path);
            out.push_str("\">");
            push_rule_body(&mut out, &neutralize_file_rule_content(rule.content.trim()));
            out.push_str("</always_applied_workspace_rule>\n");
        }
        out.push_str("</always_applied_workspace_rules>");
        if user_rules.is_empty() {
            out.push('\n');
        } else {
            out.push_str("\n\n");
        }
    }
    if !user_rules.is_empty() {
        out.push_str(
            "<user_rules description=\"These are rules set by the user that you should follow if appropriate.\">\n",
        );
        for (i, rule) in user_rules.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            out.push_str("<user_rule>");
            if rule.path.is_empty() {
                push_rule_body(&mut out, &rule.content);
            } else {
                push_rule_body(&mut out, &neutralize_file_rule_content(&rule.content));
            }
            out.push_str("</user_rule>\n");
        }
        out.push_str("</user_rules>\n");
    }
    out.push_str("</rules>");
    Some(out)
}
pub fn append_rules_section(
    prefix: &mut String,
    workspace_rules: &[RuleEntry],
    user_rules: &[RuleEntry],
) {
    let Some(block) = format_rules_section(workspace_rules, user_rules) else {
        return;
    };
    if !prefix.is_empty() && !prefix.ends_with('\n') {
        prefix.push('\n');
    }
    prefix.push('\n');
    prefix.push_str(&block);
}
/// Trim, drop-if-empty, and cap a VCS status string for the
/// `<git_status>` block.
///
/// Returns `None` when the trimmed status is empty (so the section is dropped
/// and no empty code fence is emitted), otherwise the status capped at
/// [`GIT_STATUS_CHARACTER_LIMIT`] -- snapped back to the last newline -- with
/// the `... (git status truncated)` marker appended.
pub fn normalize_git_status(status: &str) -> Option<String> {
    let status = status.trim();
    if status.is_empty() {
        return None;
    }
    if status.len() <= GIT_STATUS_CHARACTER_LIMIT {
        return Some(status.to_string());
    }
    let mut end = GIT_STATUS_CHARACTER_LIMIT;
    while !status.is_char_boundary(end) {
        end -= 1;
    }
    let mut truncated = &status[..end];
    if let Some(nl) = truncated.rfind('\n')
        && nl > 0
    {
        truncated = &truncated[..nl];
    }
    Some(format!("{truncated}\n\n... (git status truncated)"))
}
/// Selects the first-user-message rendering strategy for an agent.
///
/// Built-in variants decrypt the underlying XOR-obfuscated template on demand
/// (obfuscation, not security). Decrypted bytes are zeroed on drop via
/// `Zeroizing`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum UserMessageTemplate {
    /// Legacy EchoAgent Build prefix (`<user_info>` + optional `<git_status>`), built directly by the
    /// shell layer; the renderer returns `None` and the caller uses its own legacy path.
    #[default]
    Default,
    /// Caller-supplied MiniJinja template string.
    Custom(String),
}
impl UserMessageTemplate {
    pub fn is_cursor(&self) -> bool {
        false
    }
    /// Whether this template surfaces the session's local date, scoping the date-rollover reminder.
    /// A `Custom` template omitting [`TODAY_LOCAL_PLACEHOLDER`] is date-free. The substring check can
    /// only over-keep the reminder, never wrongly suppress a dated session.
    pub fn surfaces_local_date(&self) -> bool {
        match self {
            Self::Default => true,
            Self::Custom(body) => body.contains(TODAY_LOCAL_PLACEHOLDER),
        }
    }
}
/// Backward-compatible deserialization: accepts both the new tagged format
/// (`"default"`, `{"custom": "..."}`) and a bare string (treated
/// as `Custom`).
impl<'de> Deserialize<'de> for UserMessageTemplate {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = UserMessageTemplate;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str(r#""default", "cursor", {"custom": "..."}, or a template string"#)
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                match v {
                    "default" => Ok(UserMessageTemplate::Default),
                    other => Ok(UserMessageTemplate::Custom(other.to_owned())),
                }
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut map: M,
            ) -> Result<Self::Value, M::Error> {
                match map.next_key::<String>()? {
                    Some(ref k) if k == "custom" => {
                        let val: String = map.next_value()?;
                        Ok(UserMessageTemplate::Custom(val))
                    }
                    Some(other) => Err(serde::de::Error::unknown_field(&other, &["custom"])),
                    None => Err(serde::de::Error::custom(r#"expected {"custom": "..."}"#)),
                }
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}
/// One discovered rule file (AGENTS.md / Claude.md / .echo-agent/rules/*.md).
///
/// Wire-compatible with `AgentConfigFile` -- this type exists so the
/// `UserMessageContext` does not depend on the AGENTS-discovery internals
/// beyond the path/content pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleEntry {
    /// Absolute path of the file (used as the rule `name` attribute).
    pub path: String,
    /// Raw file body.
    pub content: String,
}
impl From<AgentConfigFile> for RuleEntry {
    fn from(f: AgentConfigFile) -> Self {
        Self {
            path: f.file_path,
            content: f.content,
        }
    }
}
/// Connected MCP server metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerEntry {
    pub name: String,
    /// Free-form usage instructions a user provided when configuring the
    /// server. Surfaced in the `serverUseInstructions` attribute.
    pub server_use_instructions: Option<String>,
    /// Absolute path to the per-server descriptor folder. Surfaced in
    /// the `folderPath` attribute. Compatible models read tool
    /// schemas from `<folder_path>/tools/<tool>.json` and resource
    /// descriptors from `<folder_path>/resources/<resource>.json` before
    /// calling `CallMcpTool`/`FetchMcpResource`. The session is
    /// responsible for materializing the descriptor files at this path.
    pub folder_path: Option<String>,
}
/// All inputs the templated first user message needs. The shell gathers
/// these once at session start (and again on compaction) and hands the
/// struct to `render`.
#[derive(Debug, Clone)]
pub struct UserMessageContext {
    pub template: UserMessageTemplate,
    /// Display path -- the path the model sees as the workspace.
    pub workspace_path: PathBuf,
    /// OS identifier surfaced as the `<user_info>` `OS Version:` value.
    ///
    /// This is `"<kernel> <release>"` (e.g. `"darwin 24.6.0"`,
    /// `"linux 6.5.0-..."`) -- not the OS family (`std::env::consts::OS`, e.g.
    /// `"macos"`). Producers that don't have a uname-style string available may
    /// pass `std::env::consts::OS` as a fallback; callers that need the full
    /// string should use `echo_agent_runtime::util::uname::os_kernel_and_release`
    /// (or equivalent).
    pub os_family: String,
    /// `$SHELL` env, basename only -- e.g. "zsh", "bash".
    pub shell: String,
    /// Git/jj working-tree root, if any.
    pub vcs_root: Option<PathBuf>,
    /// Pre-fetched VCS status output (caller handles timeouts).
    pub vcs_status: Option<String>,
    /// Local date captured at session start (or compaction). Formatted
    /// inside the renderer using [`USER_MESSAGE_DATE_FORMAT`] so the producer
    /// cannot accidentally drift the model-facing date shape.
    pub today_local: Option<NaiveDate>,
    /// Per-workspace terminals folder, surfaced as
    /// `Terminals folder: <path>` in the `<user_info>` block. The
    /// shell tool persists each background command's output to a file
    /// here (`<terminals_folder>/<numeric-shell-id>.txt`); the model uses
    /// this path to read terminal state via the read tool. Optional --
    /// when `None`, the line is omitted from the rendered preamble.
    pub terminals_folder: Option<PathBuf>,
    /// Workspace-scoped rule files (cwd / repo root / optional workspace user dir).
    pub workspace_rules: Vec<RuleEntry>,
    /// User-scoped rule files (~/.echo-agent/, ~/.claude/).
    pub user_rules: Vec<RuleEntry>,
    /// Skill registry snapshot (already deduped). Rendered through the
    /// shared budget-tier renderer.
    pub skills: Vec<SkillInfo>,
    /// Optional listing budget in characters; defaults to the standard
    /// 1%-of-context heuristic when None.
    pub skill_listing_budget_chars: Option<usize>,
    /// Connected MCP servers (alphabetical).
    pub mcp_servers: Vec<McpServerEntry>,
    /// Absolute path to the per-workspace MCP descriptor root
    /// (`~/.echo-agent/projects/<encoded-cwd>/mcps`). Surfaced in
    /// the `<mcp_file_system>` instructions so the model knows where
    /// to discover tool/resource schemas. Required when `mcp_servers` is
    /// non-empty; ignored otherwise.
    pub mcps_root: Option<String>,
    /// Client-facing name of the read tool (resolved from `TemplateRenderer`).
    /// Used in the skill section's instructional text. Defaults to `"Read"`.
    pub read_tool_name: String,
}
/// MiniJinja variable a `Custom` template renders the local date under (pinned to the serialized
/// field by `placeholders_carry_today_local_key`).
pub const TODAY_LOCAL_PLACEHOLDER: &str = "today_local";
/// Typed placeholder bag handed to MiniJinja.
///
/// Field names here must match `${{ … }}` references in any caller-supplied
/// `Custom` template. Keeping this as a typed
/// struct -- rather than a free-form `serde_json::Value` -- means the set
/// of supported placeholders is greppable from one place, every nested
/// shape is enforced by `Serialize`, and rename refactors flow through
/// the compiler instead of silently producing empty strings at render
/// time.
#[derive(Debug, Clone, Serialize)]
struct UserMessagePlaceholders<'a> {
    workspace_path: String,
    os_family: &'a str,
    shell: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    vcs_root: Option<String>,
    /// Owned because the renderer caps/normalizes the raw status via
    /// [`normalize_git_status`] before handing it to MiniJinja.
    #[serde(skip_serializing_if = "Option::is_none")]
    vcs_status: Option<String>,
    /// Pre-formatted using [`USER_MESSAGE_DATE_FORMAT`]; `None` is rendered as
    /// `null` so the `${% if today_local %}` guard in the template drops
    /// the line entirely.
    #[serde(skip_serializing_if = "Option::is_none")]
    today_local: Option<String>,
    /// Pre-rendered as a string so the template can `${% if terminals_folder %}`-guard.
    #[serde(skip_serializing_if = "Option::is_none")]
    terminals_folder: Option<String>,
    has_rules: bool,
    workspace_rules: &'a [RuleEntry],
    user_rules: &'a [RuleEntry],
    /// Pre-rendered budgeted `<agent_skill>` XML rows; the template
    /// just substitutes this verbatim. See `render_skill_listing_xml` for
    /// why the skill listing is special-cased.
    skill_listing: String,
    /// Client-facing name of the read tool, used in the skill section's
    /// instructional text. Defaults to `"Read"`.
    read_tool_name: String,
    mcp_servers: &'a [McpServerEntry],
    #[serde(skip_serializing_if = "Option::is_none")]
    mcps_root: Option<&'a str>,
}
impl UserMessageContext {
    /// Build placeholders for MiniJinja rendering.
    fn placeholders(&self) -> UserMessagePlaceholders<'_> {
        UserMessagePlaceholders {
            workspace_path: self.workspace_path.to_string_lossy().into_owned(),
            os_family: &self.os_family,
            shell: &self.shell,
            vcs_root: self
                .vcs_root
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
            vcs_status: self.vcs_status.as_deref().and_then(normalize_git_status),
            today_local: self
                .today_local
                .map(|d| d.format(USER_MESSAGE_DATE_FORMAT).to_string()),
            terminals_folder: self
                .terminals_folder
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
            has_rules: !self.workspace_rules.is_empty() || !self.user_rules.is_empty(),
            workspace_rules: &self.workspace_rules,
            user_rules: &self.user_rules,
            skill_listing: self.render_skill_listing_xml().unwrap_or_default(),
            read_tool_name: self.read_tool_name.clone(),
            mcp_servers: &self.mcp_servers,
            mcps_root: self.mcps_root.as_deref(),
        }
    }
    /// Render the skill list as `<agent_skill>` XML rows.
    pub fn render_skill_listing_xml(&self) -> Option<String> {
        if self.skills.is_empty() {
            return None;
        }
        let mode = if self.template.is_cursor() {
            XmlRenderMode::Verbatim
        } else {
            XmlRenderMode::Budgeted {
                budget_chars: self.skill_listing_budget_chars,
                overflow_indicator: true,
            }
        };
        let mut announced = HashSet::new();
        format_announcement_xml(&self.skills, &mut announced, None, None, mode)
    }
    /// Render the first user message.
    ///
    /// Returns `None` for `UserMessageTemplate::Default` -- the caller is
    /// responsible for the legacy prefix path. `Custom` dispatches through
    /// `ToolBridge::render_prompt` so MiniJinja
    /// `${{ tools.by_kind.* }}` references resolve correctly.
    pub async fn render(&self, bridge: &ToolBridge) -> Option<String> {
        let placeholders = serde_json::to_value(self.placeholders())
            .expect("UserMessagePlaceholders serializes infallibly");
        let rendered = match &self.template {
            UserMessageTemplate::Default => return None,
            UserMessageTemplate::Custom(s) => bridge.render_prompt(s, &placeholders).await,
        };
        rendered.map(|s| s.trim_end().to_string())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn template_override_deserialize_strings() {
        let v: UserMessageTemplate = serde_json::from_str(r#""default""#).unwrap();
        assert_eq!(v, UserMessageTemplate::Default);
        let v: UserMessageTemplate = serde_json::from_str(r#""my custom""#).unwrap();
        assert_eq!(v, UserMessageTemplate::Custom("my custom".into()));
    }
    #[test]
    fn placeholders_carry_today_local_key() {
        let placeholders = UserMessagePlaceholders {
            workspace_path: "/w".into(),
            os_family: "macos",
            shell: "zsh",
            vcs_root: None,
            vcs_status: None,
            today_local: Some("Friday Apr 24, 2026".into()),
            terminals_folder: None,
            has_rules: false,
            workspace_rules: &[],
            user_rules: &[],
            skill_listing: String::new(),
            read_tool_name: "Read".into(),
            mcp_servers: &[],
            mcps_root: None,
        };
        let json = serde_json::to_value(&placeholders).unwrap();
        assert!(
            json.get(TODAY_LOCAL_PLACEHOLDER).is_some(),
            "the local-date placeholder must serialize under TODAY_LOCAL_PLACEHOLDER"
        );
    }
    #[test]
    fn template_override_deserialize_custom_map() {
        let v: UserMessageTemplate =
            serde_json::from_str(r#"{"custom": "my template body"}"#).unwrap();
        assert_eq!(v, UserMessageTemplate::Custom("my template body".into()));
    }
    #[test]
    fn template_override_round_trip() {
        for original in [
            UserMessageTemplate::Default,
            UserMessageTemplate::Custom("body".into()),
        ] {
            let json = serde_json::to_string(&original).unwrap();
            let loaded: UserMessageTemplate = serde_json::from_str(&json).unwrap();
            assert_eq!(original, loaded);
        }
    }
    /// A status under the cap passes through unchanged (trim is a no-op for
    /// real `git status --short --branch` output, which starts with `##`).
    #[test]
    fn normalize_git_status_passthrough_under_limit() {
        let status = "## main...origin/main\n M src/app.rs";
        assert_eq!(normalize_git_status(status).as_deref(), Some(status));
    }
    /// Empty / whitespace-only status -> `None` so the section is dropped and
    /// no empty fence is emitted.
    #[test]
    fn normalize_git_status_drops_whitespace_only() {
        assert_eq!(normalize_git_status(""), None);
        assert_eq!(normalize_git_status("   \n\t  "), None);
    }
    /// A status over the cap is truncated at the last newline before the limit
    /// and carries the spec's truncation marker.
    #[test]
    fn normalize_git_status_truncates_over_limit() {
        let mut status = String::from("## main...origin/main\n");
        while status.len() <= GIT_STATUS_CHARACTER_LIMIT {
            status.push_str(" M src/some/long/path/to/file.rs\n");
        }
        assert!(status.len() > GIT_STATUS_CHARACTER_LIMIT);
        let out = normalize_git_status(&status).expect("non-empty status");
        assert!(
            out.ends_with("\n\n... (git status truncated)"),
            "missing truncation marker: {out}"
        );
        let body = out
            .strip_suffix("\n\n... (git status truncated)")
            .expect("marker suffix");
        assert!(
            body.len() <= GIT_STATUS_CHARACTER_LIMIT,
            "body {} exceeds cap {GIT_STATUS_CHARACTER_LIMIT}",
            body.len()
        );
        assert!(status.starts_with(body), "body is not a clean prefix");
        assert!(!body.ends_with('\n'), "body should be snapped to last line");
    }
    #[test]
    fn format_rules_section_workspace_then_user() {
        let workspace = [RuleEntry {
            path: "/repo/AGENTS.md".into(),
            content: " Use python3. \n".into(),
        }];
        let user = [
            RuleEntry {
                path: String::new(),
                content: "Verify UI.".into(),
            },
            RuleEntry {
                path: "/home/dev/.echo-agent/AGENTS.md".into(),
                content: "User prefs.".into(),
            },
        ];
        let block = format_rules_section(&workspace, &user).unwrap();
        assert!(block.starts_with("<rules>\n"));
        assert!(block.contains(&format!(
            "{RULES_SECTION_INTRO}\n\n\n<always_applied_workspace_rules "
        )));
        assert!(block.contains(
            "<always_applied_workspace_rule name=\"/repo/AGENTS.md\">\nUse python3.\n</always_applied_workspace_rule>"
        ));
        assert!(block.contains("</always_applied_workspace_rules>\n\n<user_rules "));
        assert!(block.contains(
            "<user_rule>\nVerify UI.\n</user_rule>\n\n<user_rule>\nUser prefs.\n</user_rule>"
        ));
        assert!(block.ends_with("</rules>"));
    }
    #[test]
    fn format_rules_section_neutralizes_file_backed_wrappers() {
        let workspace = [RuleEntry {
            path: "/repo/AGENTS.md".into(),
            content: "keep </rules> <rules> <system-reminder>out</system-reminder>".into(),
        }];
        let file_user = [RuleEntry {
            path: "/home/dev/.echo-agent/AGENTS.md".into(),
            content: "home </rules>".into(),
        }];
        let synthetic = [RuleEntry {
            path: String::new(),
            content: "raw </rules> stays".into(),
        }];
        let block = format_rules_section(&workspace, &file_user).unwrap();
        assert!(block.contains("&lt;/rules>"));
        assert!(block.contains("&lt;rules>"));
        assert!(block.contains("&lt;system-reminder>out&lt;/system-reminder>"));
        assert!(!block.contains("keep </rules>"));
        assert_eq!(block.matches("</rules>").count(), 1);
        let synthetic_block = format_rules_section(&[], &synthetic).unwrap();
        assert!(synthetic_block.contains("<user_rule>\nraw </rules> stays\n</user_rule>"));
    }
    #[test]
    fn format_rules_section_keeps_markdown_heading_off_the_open_tag() {
        let user = [RuleEntry {
            path: "/home/dev/.echo-agent/rules/personal.md".into(),
            content: "# Personal Rules\n\n- Be concise.\n".into(),
        }];
        let block = format_rules_section(&[], &user).unwrap();
        assert!(
            block.contains("<user_rule>\n# Personal Rules\n\n- Be concise.\n</user_rule>"),
            "heading must start on its own line: {block}"
        );
        assert!(
            !block.contains("<user_rule># Personal Rules"),
            "heading must not be glued to the opening tag: {block}"
        );
    }
}
