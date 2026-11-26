use strum::AsRefStr;

/// Scope/priority of a skill based on where it was discovered.
/// Lower values have higher priority (Local overrides Repo overrides User).
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
    AsRefStr,
)]
#[repr(u8)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum SkillScope {
    /// cwd/.echo-agent/skills (highest priority)
    Local = 0,
    /// repo_root/.echo-agent/skills
    Repo = 1,
    /// ~/.echo-agent/skills
    User = 2,
    /// ~/.echo-agent/server-skills (synced from the skill store)
    Server = 3,
    /// platform built-in skills (lowest precedence)
    Bundled = 4,
    /// plugin-provided skills (lowest priority for bare-name resolution)
    Plugin = 5,
}

const fn default_true() -> bool {
    true
}

/// Skill info returned by the list extension method.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SkillInfo {
    /// Command identity: slash name, dedup key, listing label. Plugin skills
    /// and same-scope name-collision losers (`dedupe_skills` re-key) use the
    /// directory basename here, not frontmatter `name`.
    pub name: String,
    /// Frontmatter `name`, kept as the display label when `name` is overridden
    /// to the directory basename (plugin skills; same-scope collision re-key).
    /// None = `name` is the label. Not a plugin marker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub description: String,
    /// True when `description` came from frontmatter rather than being derived
    /// from the body. Gates whether plugin skills appear in the listing.
    #[serde(default)]
    pub has_user_specified_description: bool,
    /// Glob patterns (gitignore-style). When set, the skill is held back from
    /// the listing until a matching file is touched. None = always shown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paths: Option<Vec<String>>,
    /// Trigger phrases for model matching, separate from description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub when_to_use: Option<String>,
    /// Short description for compact UI display (optional, falls back to description)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub short_description: Option<String>,
    /// Author of the skill, extracted from frontmatter `metadata.author`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// Hint text shown in autocomplete for this skill's arguments (e.g. "commit message").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub argument_hint: Option<String>,
    /// License for the skill (e.g. "Apache-2.0").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    /// Environment requirements (e.g. "Requires git, docker, jq").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compatibility: Option<String>,
    /// Arbitrary key-value metadata from frontmatter (string values only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<std::collections::HashMap<String, String>>,
    pub path: String,
    /// Scope/priority of the skill
    pub scope: SkillScope,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_source: Option<crate::types::config_source::ConfigSource>,
    /// Plugin namespace for plugin-backed skills only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin_name: Option<String>,
    /// Plugin version for plugin-backed skills only (from manifest).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin_version: Option<String>,
    /// Plugin root dir for plugin-backed skills, used for ${CLAUDE_PLUGIN_ROOT} expansion.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin_root: Option<String>,
    /// Plugin data dir for plugin-backed skills, used for ${CLAUDE_PLUGIN_DATA} expansion.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin_data: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<Vec<String>>,
    /// Optional model override for skill execution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Optional reasoning effort override.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Whether this skill can be invoked by the user via /skill-name.
    /// Skills with user_invocable=false are not shown in the skill tool.
    #[serde(default = "default_true")]
    pub user_invocable: bool,
    /// If true, the model cannot auto-invoke this skill; only user slash commands work.
    /// Skills with this flag are shown in the system prompt but filtered from the skill tool.
    #[serde(default)]
    pub disable_model_invocation: bool,

    /// Whether this skill is enabled. Disabled skills are still listed
    /// but excluded from the system prompt and skill tool invocation.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Populated for agent definition `skills:` preloading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

impl SkillInfo {
    /// Dedup key. Plugin skills use `plugin:<name>` (name is the directory
    /// basename, so siblings never collide); others use the bare name.
    pub fn dedup_key(&self) -> String {
        match &self.plugin_name {
            Some(plugin) => format!("{}:{}", plugin, self.name),
            None => self.name.clone(),
        }
    }

    /// Display label: frontmatter `name` if set, else the command identity.
    pub fn label(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.name)
    }
}

/// Extract the skill name from a path if it points to a `SKILL.md` file.
///
/// Returns the parent directory name (e.g. `"/skills/deploy/SKILL.md"` → `"deploy"`).
/// Returns `None` for non-SKILL.md paths or bare `"SKILL.md"` with no parent.
pub fn skill_name_from_path(path: &str) -> Option<&str> {
    let p = std::path::Path::new(path);
    if p.file_name()?.to_str()? == "SKILL.md" {
        p.parent()?.file_name()?.to_str()
    } else {
        None
    }
}

impl Default for SkillInfo {
    fn default() -> Self {
        Self {
            name: String::new(),
            display_name: None,
            description: String::new(),
            has_user_specified_description: false,
            paths: None,
            when_to_use: None,
            short_description: None,
            author: None,
            argument_hint: None,
            license: None,
            compatibility: None,
            metadata: None,
            path: String::new(),
            scope: SkillScope::Local,
            config_source: None,
            plugin_name: None,
            plugin_version: None,
            plugin_root: None,
            plugin_data: None,
            allowed_tools: None,
            model: None,
            effort: None,
            user_invocable: true,
            disable_model_invocation: false,
            enabled: true,
            body: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SkillInfo, skill_name_from_path};

    #[test]
    fn label_prefers_display_name_then_name() {
        let mut s = SkillInfo {
            name: "deploy".to_owned(),
            ..SkillInfo::default()
        };
        assert_eq!(s.label(), "deploy");
        s.display_name = Some("Deploy to Prod".to_owned());
        assert_eq!(s.label(), "Deploy to Prod");
    }

    #[test]
    fn extracts_skill_name() {
        assert_eq!(
            skill_name_from_path("/home/user/.echo-agent/skills/deploy/SKILL.md"),
            Some("deploy"),
        );
    }

    #[test]
    fn nested_path() {
        assert_eq!(
            skill_name_from_path("/repo/.echo-agent/skills/my-skill/SKILL.md"),
            Some("my-skill"),
        );
    }

    #[test]
    fn non_skill_returns_none() {
        assert_eq!(skill_name_from_path("/src/main.rs"), None);
    }

    #[test]
    fn lowercase_returns_none() {
        assert_eq!(skill_name_from_path("/skills/deploy/skill.md"), None);
    }

    #[test]
    fn bare_skill_md_returns_none() {
        assert_eq!(skill_name_from_path("SKILL.md"), None);
    }
}
