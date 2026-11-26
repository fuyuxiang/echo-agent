//! Shared DTO types for hooks/plugins ACP extensions.
//!
//! This crate defines the wire format for `echo.agent/hooks/*` and `echo.agent/plugins/*`
//! ACP extension methods. It is dependency-free (only `serde`) so both
//! `echo-agent-runtime` and `echo-agent-pager` can depend on it without pulling
//! in domain logic.
//!
//! Conversion from domain types (`HookSpec`, `LoadedPlugin`) to these DTOs
//! lives in the shell's extension handlers, not here.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// Plugin scope.
///
/// Maps from `PluginScope` in `echo-agent-core`. Variant renames:
/// - source `CliOverride` -> DTO `Cli` (matches Display output "cli")
/// - source `ConfigPath` -> DTO `Config` (matches Display output "config")
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginScope {
    Cli,
    Project,
    User,
    Config,
}

/// The concrete discovery source a plugin came from.
///
/// Maps from `PluginOrigin` in `echo-agent-core`. Optional on [`PluginInfo`]
/// so older shells (which don't send it) deserialize to `None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PluginOrigin {
    /// CLI `--plugin-dir`.
    CliOverride,
    /// Project `.echo-agent/plugins/`.
    ProjectEchoAgent,
    /// Project `.claude/plugins/`.
    ProjectClaude,
    /// `$ECHO_AGENT_HOME/plugins/`.
    UserEchoAgent,
    /// `~/.claude/plugins/`.
    UserClaude,
    /// A compat marketplace clone.
    ClaudeMarketplace {
        /// Marketplace name from the settings/registry entry.
        marketplace: String,
    },
    /// Compat install from `installed_plugins.json`.
    ClaudeInstalled {
        /// Marketplace name from the `name@marketplace` key, when present.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        marketplace: Option<String>,
    },
    /// EchoAgent's install registry (marketplace or direct git/local install).
    MarketplaceInstall {
        /// Marketplace source display name (None for direct installs).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_name: Option<String>,
        /// Git URL of the installed repo (None for local installs).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        git_url: Option<String>,
    },
    /// `[plugins].paths` in config.
    ConfigPath,
    /// Catch-all for variants added after this client was built, so a newer
    /// shell never breaks an older pager's whole plugins list. Consumers
    /// must treat it like a missing origin.
    #[serde(other)]
    Unknown,
}

/// Hook event type.
///
/// Maps from `HookEventName` in `echo-agent-hooks`. The source type's
/// `SubagentEnd` variant (backward-compat alias) is collapsed into
/// `SubagentStop` during conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    // Session lifecycle
    SessionStart,
    SessionEnd,
    Stop,
    StopFailure,
    StopCancelled,
    // Tool events
    PreToolUse,
    PostToolUse,
    PostToolUseFailure,
    PermissionDenied,
    // User / notification
    UserPromptSubmit,
    Notification,
    // Subagent
    SubagentStart,
    SubagentStop,
    // Compaction
    PreCompact,
    PostCompact,
    /// An event added after this client was built: it keeps one unrecognized name from blanking
    /// the whole list. Lossy on re-serialize, so only deserialize through this type.
    #[serde(other)]
    Unknown,
}

impl HookEvent {
    /// Never fails: a name this build does not know deserializes to [`Self::Unknown`] through the
    /// `#[serde(other)]` variant. Avoids a `serde_json::Value` round trip on UI paths.
    pub fn from_wire(name: &str) -> Self {
        use serde::de::IntoDeserializer as _;
        let name: serde::de::value::StrDeserializer<'_, serde::de::value::Error> =
            name.into_deserializer();
        Self::deserialize(name).unwrap_or(Self::Unknown)
    }

    /// The events that report a turn ending, at most one of which fires per turn. Exhaustive, so
    /// a fourth is a compile error rather than a silent miss in a consumer.
    pub fn is_turn_end(self) -> bool {
        match self {
            Self::Stop | Self::StopFailure | Self::StopCancelled => true,
            Self::SessionStart
            | Self::SessionEnd
            | Self::PreToolUse
            | Self::PostToolUse
            | Self::PostToolUseFailure
            | Self::PermissionDenied
            | Self::UserPromptSubmit
            | Self::Notification
            | Self::SubagentStart
            | Self::SubagentStop
            | Self::PreCompact
            | Self::PostCompact
            | Self::Unknown => false,
        }
    }
}

impl std::fmt::Display for HookEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SessionStart => write!(f, "Session Start"),
            Self::PreToolUse => write!(f, "Pre-Tool Use"),
            Self::PostToolUse => write!(f, "Post-Tool Use"),
            Self::PostToolUseFailure => write!(f, "Post-Tool Use Failure"),
            Self::SessionEnd => write!(f, "Session End"),
            Self::Stop => write!(f, "Stop"),
            Self::StopFailure => write!(f, "Stop Failure"),
            Self::StopCancelled => write!(f, "Stop Cancelled"),
            Self::Notification => write!(f, "Notification"),
            Self::UserPromptSubmit => write!(f, "Prompt Submit"),
            Self::PermissionDenied => write!(f, "Permission Denied"),
            Self::SubagentStart => write!(f, "Subagent Start"),
            Self::SubagentStop => write!(f, "Subagent Stop"),
            Self::PreCompact => write!(f, "Pre-Compact"),
            Self::PostCompact => write!(f, "Post-Compact"),
            Self::Unknown => write!(f, "Unknown"),
        }
    }
}
/// Hook handler type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookHandlerType {
    Command,
    Http,
}

/// Plugin hook status -- derived from trust + has_hooks + has_inline_hooks_only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookStatus {
    /// Trusted and active (file-based hooks).
    Active,
    /// Trusted and active (inline hooks only).
    ActiveInline,
    /// Untrusted -- hooks exist but are blocked.
    Blocked,
    /// No hooks configured for this plugin.
    None,
}

/// Plugin MCP server status -- derived from trust + mcp_server_count + has_inline_mcp_only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpStatus {
    /// Trusted and active (file-based config).
    Active,
    /// Trusted and active (inline config only).
    ActiveInline,
    /// Untrusted -- MCP servers exist but are blocked.
    Blocked,
    /// No MCP servers configured.
    None,
}

/// Machine-readable outcome status for action responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeStatus {
    /// Operation completed successfully.
    Success,
    /// Operation failed due to a validation or input error.
    ValidationError,
    /// Confirmation is required before proceeding.
    ConfirmationRequired,
    /// Target not found (plugin name, hook path, etc.).
    NotFound,
    /// Operation failed due to an internal/IO error.
    InternalError,
    /// Operation not supported in the current session state.
    Unsupported,
}

// ---------------------------------------------------------------------------
// Hook types
// ---------------------------------------------------------------------------

/// A single hook's metadata for display in the pager.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookInfo {
    /// Full name including scope prefix (e.g., "global/safety:pre_tool_use[0].hooks[0]").
    pub name: String,
    /// Event type this hook runs on.
    pub event: HookEvent,
    /// Handler type.
    pub handler_type: HookHandlerType,
    /// Raw matcher pattern from config (for display). None = matches all tools.
    /// Maps from `HookSpec.configured_matcher` (not the compiled regex).
    pub matcher: Option<String>,
    /// Command path (for command handlers).
    pub command: Option<String>,
    /// HTTP URL (for http handlers).
    pub url: Option<String>,
    /// Timeout in milliseconds.
    pub timeout_ms: u64,
    /// Source directory of the hook definition file.
    pub source_dir: String,
    /// Whether this hook is disabled via ~/.echo-agent/disabled-hooks.
    #[serde(default)]
    pub disabled: bool,
}

/// Response for `echo.agent/hooks/list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HooksListResponse {
    pub hooks: Vec<HookInfo>,
    /// Whether the current project's git root is trusted for hook execution.
    pub project_trusted: bool,
    /// Errors encountered while loading hook config files (parse failures, etc.).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub load_errors: Vec<String>,
}

// ---------------------------------------------------------------------------
// Plugin types
// ---------------------------------------------------------------------------

/// A single plugin's metadata for display in the pager.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginInfo {
    /// User-facing plugin name.
    pub name: String,
    /// Stable plugin ID (format: "<scope>/<hex8>/<name>").
    pub id: String,
    /// Absolute path to plugin root directory.
    pub root: String,
    /// Plugin scope.
    pub scope: PluginScope,
    /// Deprecated: always `true`. Trust/untrust has been replaced by
    /// enable/disable. Kept for serialization compatibility; will be removed.
    pub trusted: bool,
    /// Whether the plugin is enabled (not in [plugins].disabled list).
    pub enabled: bool,
    /// Version from manifest (if available).
    pub version: Option<String>,
    /// Description from manifest (if available).
    pub description: Option<String>,
    /// Number of skill subdirectories.
    pub skill_count: usize,
    /// Skill names (directory names under skills/).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skill_names: Vec<String>,
    /// Number of agent .md files.
    pub agent_count: usize,
    /// Agent/persona names (filenames without .md extension).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agent_names: Vec<String>,
    /// Hook status (active, active_inline, blocked, none).
    pub hook_status: HookStatus,
    /// Number of hook specs defined.
    #[serde(default)]
    pub hook_count: usize,
    /// Number of MCP servers.
    pub mcp_server_count: usize,
    /// MCP server status (active, active_inline, blocked, none).
    pub mcp_status: McpStatus,
    /// Marketplace source display name (None for non-marketplace installs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marketplace_source: Option<String>,
    /// The concrete discovery source (None when sent by an older shell).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<PluginOrigin>,
    /// Warning when this plugin shadowed another with the same name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict: Option<String>,
}

/// Response for `echo.agent/plugins/list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginsListResponse {
    pub plugins: Vec<PluginInfo>,
}

// ---------------------------------------------------------------------------
// MCP server types
// ---------------------------------------------------------------------------

/// Source of an MCP server configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpServerSource {
    /// Managed by the platform (e.g., OAuth connectors).
    Managed,
    /// Locally configured (config.toml, .mcp.json, plugins, etc.).
    Local,
}

/// Session-level status of an MCP server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpSessionStatus {
    Ready,
    Initializing,
    Unavailable,
}

/// A tool exposed by an MCP server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolInfo {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Summary of an MCP server for display in the pager.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerInfo {
    pub name: String,
    pub source: McpServerSource,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<McpSessionStatus>,
    /// Number of tools this server exposes.
    pub tool_count: usize,
    /// Tool names (for display when expanded).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<McpToolInfo>,
    /// Config source label (e.g., "plugin: my-plugin", "config.toml", ".mcp.json").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_source: Option<String>,
}

/// Response for `echo.agent/mcp/list` as consumed by the pager.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServersListResponse {
    pub servers: Vec<McpServerInfo>,
}

// ---------------------------------------------------------------------------
// Plugin component inventory (from marketplace catalogs)
// ---------------------------------------------------------------------------

const MAX_COMPONENT_NAME_CHARS: usize = 120;
const MAX_COMPONENT_DESC_CHARS: usize = 120;

/// Maximum items kept per component category when sanitizing catalog data.
pub const MAX_COMPONENTS_PER_CATEGORY: usize = 50;

/// One concrete thing a plugin provides (a skill, command, agent, etc.).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ComponentItem {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl ComponentItem {
    /// Build an item with control characters stripped and the description
    /// truncated, defending against terminal-escape injection from
    /// catalog-supplied strings.
    pub fn new(name: impl Into<String>, description: Option<String>) -> Self {
        let mut item = Self {
            name: name.into(),
            description,
        };
        item.sanitize();
        item
    }

    fn sanitize(&mut self) {
        self.name = truncate_chars(&strip_control_chars(&self.name), MAX_COMPONENT_NAME_CHARS);
        self.description = self
            .description
            .take()
            .map(|d| truncate_chars(&strip_control_chars(&d), MAX_COMPONENT_DESC_CHARS))
            .filter(|d| !d.is_empty());
    }
}

fn strip_control_chars(s: &str) -> String {
    s.chars()
        .filter(|c| {
            !c.is_control()
                && !matches!(
                    c,
                    '\u{200b}'..='\u{200f}'
                        | '\u{202a}'..='\u{202e}'
                        | '\u{2066}'..='\u{2069}'
                        | '\u{feff}'
                )
        })
        .collect()
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    match s.char_indices().nth(max_chars) {
        Some((idx, _)) => s[..idx].to_string(),
        None => s.to_string(),
    }
}

/// Full inventory of a plugin's components, sourced from a marketplace
/// catalog (`plugin-index.json`).
///
/// Serde deserialization bypasses [`ComponentItem::new`], so values are not
/// sanitized by construction: every consumer that renders catalog-derived
/// data to a terminal must call [`Self::sanitize`] at its ingestion point.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PluginComponents {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<ComponentItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<ComponentItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<ComponentItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_servers: Vec<ComponentItem>,
    /// `name` = hook event (e.g. "PreToolUse"), `description` = optional matcher.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hooks: Vec<ComponentItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lsp_servers: Vec<ComponentItem>,
}

/// Stable identifier for one of the six component categories. Consumers
/// map this to their own display labels via exhaustive `match` so adding a
/// category is a compile error until every consumer handles it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentCategory {
    Skills,
    Commands,
    Agents,
    McpServers,
    Hooks,
    LspServers,
}

impl PluginComponents {
    /// Canonical category enumeration; the single source of truth for which
    /// fields exist and their display order.
    pub fn categories(&self) -> [(ComponentCategory, &[ComponentItem]); 6] {
        [
            (ComponentCategory::Skills, self.skills.as_slice()),
            (ComponentCategory::Commands, self.commands.as_slice()),
            (ComponentCategory::Agents, self.agents.as_slice()),
            (ComponentCategory::McpServers, self.mcp_servers.as_slice()),
            (ComponentCategory::Hooks, self.hooks.as_slice()),
            (ComponentCategory::LspServers, self.lsp_servers.as_slice()),
        ]
    }

    fn categories_mut(&mut self) -> [&mut Vec<ComponentItem>; 6] {
        [
            &mut self.skills,
            &mut self.commands,
            &mut self.agents,
            &mut self.mcp_servers,
            &mut self.hooks,
            &mut self.lsp_servers,
        ]
    }

    pub fn is_empty(&self) -> bool {
        self.categories().iter().all(|(_, items)| items.is_empty())
    }

    /// One-line summary like "3 skills · 1 MCP server · 2 commands",
    /// omitting empty categories. `None` when there is nothing to show.
    pub fn summary_line(&self) -> Option<String> {
        let parts: Vec<String> = self
            .categories()
            .iter()
            .filter(|(_, items)| !items.is_empty())
            .map(|(category, items)| {
                let (singular, plural) = match category {
                    ComponentCategory::Skills => ("skill", "skills"),
                    ComponentCategory::Commands => ("command", "commands"),
                    ComponentCategory::Agents => ("agent", "agents"),
                    ComponentCategory::McpServers => ("MCP server", "MCP servers"),
                    ComponentCategory::Hooks => ("hook", "hooks"),
                    ComponentCategory::LspServers => ("LSP server", "LSP servers"),
                };
                let label = if items.len() == 1 { singular } else { plural };
                format!("{} {}", items.len(), label)
            })
            .collect();
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(" \u{b7} "))
        }
    }

    /// Strip control characters, truncate descriptions, and cap each
    /// category at [`MAX_COMPONENTS_PER_CATEGORY`] items. Applied when
    /// loading untrusted catalog data.
    pub fn sanitize(&mut self) {
        for items in self.categories_mut() {
            items.truncate(MAX_COMPONENTS_PER_CATEGORY);
            for item in items.iter_mut() {
                item.sanitize();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Action types
// ---------------------------------------------------------------------------

/// Request wrapper for `echo.agent/hooks/action`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HooksActionRequest {
    pub session_id: String,
    pub action: HooksAction,
}

/// Hook management actions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HooksAction {
    /// Re-discover and reload all hooks mid-session.
    Reload,
    Trust,
    Untrust,
    Add {
        path: String,
    },
    Remove {
        path: String,
    },
    /// Enable a disabled hook by name.
    Enable {
        hook_name: String,
    },
    /// Disable a hook by name.
    Disable {
        hook_name: String,
    },
    /// Enable or disable all hooks from a source directory at once.
    ToggleSource {
        /// Hook names to toggle.
        hook_names: Vec<String>,
        /// If true, disable all; if false, enable all.
        disable: bool,
    },
}

/// Request wrapper for `echo.agent/plugins/action`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginsActionRequest {
    pub session_id: String,
    pub action: PluginsAction,
}

/// Plugin management actions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PluginsAction {
    Reload,
    Install {
        source: String,
    },
    Uninstall {
        #[serde(alias = "pluginId")]
        plugin_id: String,
        /// If true, skip multi-plugin repo confirmation.
        #[serde(default)]
        confirmed: bool,
    },
    Update {
        #[serde(alias = "pluginId")]
        plugin_id: Option<String>,
    },
    Add {
        path: String,
    },
    Remove {
        path: String,
    },
    /// Enable a disabled plugin by ID.
    Enable {
        #[serde(alias = "pluginId")]
        plugin_id: String,
    },
    /// Disable a plugin by ID (adds to disabled list in config).
    Disable {
        #[serde(alias = "pluginId")]
        plugin_id: String,
    },
}

/// Shared action response for both `echo.agent/hooks/action` and `echo.agent/plugins/action`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionOutcome {
    /// Machine-readable outcome status.
    pub status: OutcomeStatus,
    /// Human-readable result message.
    pub message: String,
    /// Whether the pager should auto-trigger a plugins reload.
    pub requires_reload: bool,
    /// Whether the change requires a session restart to take effect.
    pub requires_restart: bool,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hooks_action_serde_roundtrip() {
        let action = HooksAction::Add {
            path: "/home/user/.echo-agent/hooks".into(),
        };
        let json = serde_json::to_string(&action).unwrap();
        let parsed: HooksAction = serde_json::from_str(&json).unwrap();
        assert_eq!(action, parsed);
    }

    #[test]
    fn plugins_action_serde_roundtrip() {
        let action = PluginsAction::Install {
            source: "github.com/foo/bar".into(),
        };
        let json = serde_json::to_string(&action).unwrap();
        let parsed: PluginsAction = serde_json::from_str(&json).unwrap();
        assert_eq!(action, parsed);
    }

    #[test]
    fn action_outcome_serde_roundtrip() {
        let outcome = ActionOutcome {
            status: OutcomeStatus::Success,
            message: "Installed 1 plugin(s)".into(),
            requires_reload: true,
            requires_restart: false,
        };
        let json = serde_json::to_string(&outcome).unwrap();
        let parsed: ActionOutcome = serde_json::from_str(&json).unwrap();
        assert_eq!(outcome, parsed);
    }

    #[test]
    fn hooks_action_tagged_enum_format() {
        let action = HooksAction::Trust;
        let json = serde_json::to_string(&action).unwrap();
        assert_eq!(json, r#"{"type":"trust"}"#);

        let action = HooksAction::Add {
            path: "/tmp/hooks".into(),
        };
        let json = serde_json::to_string(&action).unwrap();
        assert!(json.contains(r#""type":"add""#));
        assert!(json.contains(r#""path":"/tmp/hooks""#));
    }

    #[test]
    fn plugins_action_tagged_enum_format() {
        let action = PluginsAction::Reload;
        let json = serde_json::to_string(&action).unwrap();
        assert_eq!(json, r#"{"type":"reload"}"#);

        let action = PluginsAction::Uninstall {
            plugin_id: "user/abc123/my-plugin".into(),
            confirmed: false,
        };
        let json = serde_json::to_string(&action).unwrap();
        assert!(json.contains(r#""type":"uninstall""#));
        assert!(json.contains(r#""plugin_id":"user/abc123/my-plugin""#));
    }

    #[test]
    fn outcome_status_serde() {
        for (status, expected) in [
            (OutcomeStatus::Success, r#""success""#),
            (OutcomeStatus::ValidationError, r#""validation_error""#),
            (
                OutcomeStatus::ConfirmationRequired,
                r#""confirmation_required""#,
            ),
            (OutcomeStatus::NotFound, r#""not_found""#),
            (OutcomeStatus::InternalError, r#""internal_error""#),
            (OutcomeStatus::Unsupported, r#""unsupported""#),
        ] {
            let json = serde_json::to_string(&status).unwrap();
            assert_eq!(json, expected);
            let parsed: OutcomeStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(status, parsed);
        }
    }

    #[test]
    fn hook_info_camel_case_fields() {
        let hook = HookInfo {
            name: "global/test".into(),
            event: HookEvent::PreToolUse,
            handler_type: HookHandlerType::Command,
            matcher: Some("Bash".into()),
            command: Some("check.sh".into()),
            url: None,
            timeout_ms: 5000,
            source_dir: "/home/user/.echo-agent/hooks".into(),
            disabled: false,
        };
        let json = serde_json::to_string(&hook).unwrap();
        assert!(json.contains("handlerType"));
        assert!(json.contains("timeoutMs"));
        assert!(json.contains("sourceDir"));
        // Verify roundtrip.
        let parsed: HookInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(hook, parsed);
    }

    #[test]
    fn plugin_info_camel_case_fields() {
        let plugin = PluginInfo {
            name: "test-plugin".into(),
            id: "user/abc12345/test-plugin".into(),
            root: "/home/user/.echo-agent/plugins/test-plugin".into(),
            scope: PluginScope::User,
            trusted: true,
            enabled: true,
            version: Some("1.0.0".into()),
            description: Some("A test plugin".into()),
            skill_count: 2,
            skill_names: vec!["hello".into(), "check".into()],
            agent_names: vec!["reviewer".into()],
            agent_count: 1,
            hook_status: HookStatus::Active,
            hook_count: 3,
            mcp_server_count: 0,
            mcp_status: McpStatus::None,
            marketplace_source: None,
            origin: Some(PluginOrigin::UserEchoAgent),
            conflict: None,
        };
        let json = serde_json::to_string(&plugin).unwrap();
        assert!(json.contains("skillCount"));
        assert!(json.contains("agentCount"));
        assert!(json.contains("hookStatus"));
        assert!(json.contains("mcpServerCount"));
        assert!(json.contains("mcpStatus"));
        let parsed: PluginInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(plugin, parsed);
    }

    #[test]
    fn plugin_origin_serde_roundtrip_all_variants() {
        for origin in [
            PluginOrigin::CliOverride,
            PluginOrigin::ProjectEchoAgent,
            PluginOrigin::ProjectClaude,
            PluginOrigin::UserEchoAgent,
            PluginOrigin::UserClaude,
            PluginOrigin::ClaudeMarketplace {
                marketplace: "mp".into(),
            },
            PluginOrigin::ClaudeInstalled { marketplace: None },
            PluginOrigin::ClaudeInstalled {
                marketplace: Some("mp".into()),
            },
            PluginOrigin::MarketplaceInstall {
                source_name: None,
                git_url: None,
            },
            PluginOrigin::MarketplaceInstall {
                source_name: Some("EchoAgent Official".into()),
                git_url: Some("https://example.com/r.git".into()),
            },
            PluginOrigin::ConfigPath,
            PluginOrigin::Unknown,
        ] {
            let json = serde_json::to_string(&origin).unwrap();
            let parsed: PluginOrigin = serde_json::from_str(&json).unwrap();
            assert_eq!(origin, parsed, "{json}");
        }
    }

    #[test]
    fn plugin_origin_unknown_future_variant_degrades_to_unknown() {
        let parsed: PluginOrigin =
            serde_json::from_str(r#"{"type":"some_future_variant"}"#).unwrap();
        assert_eq!(parsed, PluginOrigin::Unknown);
        let parsed: PluginOrigin =
            serde_json::from_str(r#"{"type":"cloud_install","bucket":"b"}"#).unwrap();
        assert_eq!(parsed, PluginOrigin::Unknown);
    }

    #[test]
    fn plugin_info_with_future_origin_variant_still_parses() {
        let json = r#"{
            "name": "future-plugin",
            "id": "user/abc12345/future-plugin",
            "root": "/tmp/future-plugin",
            "scope": "user",
            "trusted": true,
            "enabled": true,
            "version": null,
            "description": null,
            "skillCount": 0,
            "agentCount": 0,
            "hookStatus": "none",
            "mcpServerCount": 0,
            "mcpStatus": "none",
            "origin": {"type": "some_future_variant", "extra": 1}
        }"#;
        let parsed: PluginInfo = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.origin, Some(PluginOrigin::Unknown));
        assert_eq!(parsed.name, "future-plugin");
    }

    #[test]
    fn plugin_origin_tagged_snake_case_format() {
        let json = serde_json::to_string(&PluginOrigin::ClaudeMarketplace {
            marketplace: "mp".into(),
        })
        .unwrap();
        assert_eq!(json, r#"{"type":"claude_marketplace","marketplace":"mp"}"#);
        let json = serde_json::to_string(&PluginOrigin::UserClaude).unwrap();
        assert_eq!(json, r#"{"type":"user_claude"}"#);
    }

    #[test]
    fn plugin_info_without_origin_field_deserializes_to_none() {
        // Wire payload from an older shell that predates the origin field.
        let json = r#"{
            "name": "old-plugin",
            "id": "user/abc12345/old-plugin",
            "root": "/tmp/old-plugin",
            "scope": "user",
            "trusted": true,
            "enabled": true,
            "version": null,
            "description": null,
            "skillCount": 0,
            "agentCount": 0,
            "hookStatus": "none",
            "mcpServerCount": 0,
            "mcpStatus": "none"
        }"#;
        let parsed: PluginInfo = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.origin, None);
        assert_eq!(parsed.marketplace_source, None);
        assert_eq!(parsed.name, "old-plugin");
    }

    #[test]
    fn hook_event_serde_snake_case() {
        for (event, expected) in [
            (HookEvent::SessionStart, r#""session_start""#),
            (HookEvent::PreToolUse, r#""pre_tool_use""#),
            (HookEvent::PostToolUse, r#""post_tool_use""#),
            (HookEvent::PostToolUseFailure, r#""post_tool_use_failure""#),
            (HookEvent::SessionEnd, r#""session_end""#),
            (HookEvent::Stop, r#""stop""#),
            (HookEvent::StopFailure, r#""stop_failure""#),
            (HookEvent::StopCancelled, r#""stop_cancelled""#),
            (HookEvent::Notification, r#""notification""#),
            (HookEvent::UserPromptSubmit, r#""user_prompt_submit""#),
            (HookEvent::PermissionDenied, r#""permission_denied""#),
            (HookEvent::SubagentStart, r#""subagent_start""#),
            (HookEvent::SubagentStop, r#""subagent_stop""#),
            (HookEvent::PreCompact, r#""pre_compact""#),
            (HookEvent::PostCompact, r#""post_compact""#),
        ] {
            let json = serde_json::to_string(&event).unwrap();
            assert_eq!(json, expected, "HookEvent::{event:?} serialized wrong");
            let parsed: HookEvent = serde_json::from_str(&json).unwrap();
            assert_eq!(event, parsed);
        }
    }

    /// A newer shell can name an event this build has never heard of. Both readers must fall back
    /// rather than fail, or one unknown name blanks the whole plugin list.
    #[test]
    fn an_unrecognized_event_name_reads_as_unknown() {
        assert_eq!(
            HookEvent::from_wire("some_future_event"),
            HookEvent::Unknown
        );
        let parsed: HookEvent = serde_json::from_str(r#""some_future_event""#).unwrap();
        assert_eq!(parsed, HookEvent::Unknown);
    }

    #[test]
    fn marketplace_plugin_entry_roundtrip_preserves_homepage_and_keywords() {
        let entry = MarketplacePluginEntry {
            name: "demo".into(),
            version: Some("1.2.3".into()),
            description: Some("A demo plugin".into()),
            category: Some("development".into()),
            author: Some("echoagent".into()),
            tags: vec!["cli".into()],
            keywords: vec!["search".into(), "index".into()],
            domains: vec!["example.com".into()],
            homepage: Some("https://example.com/demo".into()),
            relative_path: "plugins/demo".into(),
            skill_count: 1,
            has_hooks: true,
            has_agents: false,
            has_mcp: false,
            install_status: "not_installed".into(),
            installed_version: None,
            components: None,
            remote_url: None,
            remote_ref: None,
            remote_sha: None,
            remote_subdir: None,
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("homepage"), "{json}");
        assert!(json.contains("keywords"), "{json}");
        let parsed: MarketplacePluginEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.homepage.as_deref(), Some("https://example.com/demo"));
        assert_eq!(
            parsed.keywords,
            vec!["search".to_string(), "index".to_string()]
        );
        assert_eq!(parsed.domains, vec!["example.com".to_string()]);
        assert_eq!(parsed.tags, vec!["cli".to_string()]);
    }

    #[test]
    fn marketplace_plugin_entry_defaults_when_homepage_and_keywords_absent() {
        let json = r#"{
            "name": "old",
            "version": null,
            "description": null,
            "category": null,
            "author": null,
            "tags": ["legacy"],
            "relativePath": "plugins/old",
            "skillCount": 0,
            "hasHooks": false,
            "hasAgents": false,
            "hasMcp": false,
            "installStatus": "not_installed",
            "installedVersion": null
        }"#;
        let parsed: MarketplacePluginEntry = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.homepage, None);
        assert!(parsed.keywords.is_empty());
        assert!(parsed.domains.is_empty());
        assert_eq!(parsed.tags, vec!["legacy".to_string()]);
        assert_eq!(parsed.components, None);
    }

    fn item(name: &str, desc: Option<&str>) -> ComponentItem {
        ComponentItem::new(name, desc.map(str::to_string))
    }

    #[test]
    fn component_item_new_strips_control_chars_and_truncates() {
        let long_desc = "x".repeat(200);
        let it = ComponentItem::new("evil\u{1b}[31mname\n", Some(format!("\u{7}{long_desc}")));
        assert_eq!(it.name, "evil[31mname");
        let desc = it.description.unwrap();
        assert_eq!(desc.chars().count(), 120);
        assert!(desc.chars().all(|c| c == 'x'));

        let long_name = "n".repeat(500);
        let it = ComponentItem::new(long_name, None);
        assert_eq!(it.name.chars().count(), 120);
    }

    #[test]
    fn component_item_new_strips_unicode_spoofing_chars() {
        let it = ComponentItem::new(
            "a\u{202e}b\u{200b}c\u{feff}d\u{2066}e\u{200f}f\u{2069}g",
            Some("x\u{202d}y\u{200c}z".to_string()),
        );
        assert_eq!(it.name, "abcdefg");
        assert_eq!(it.description.as_deref(), Some("xyz"));
    }

    #[test]
    fn plugin_components_summary_line_pluralizes_and_omits_empty() {
        let components = PluginComponents {
            skills: vec![item("a", None), item("b", None), item("c", None)],
            mcp_servers: vec![item("srv", None)],
            commands: vec![item("/x", None), item("/y", None)],
            ..Default::default()
        };
        assert_eq!(
            components.summary_line().as_deref(),
            Some("3 skills \u{b7} 2 commands \u{b7} 1 MCP server")
        );
        assert!(!components.is_empty());
        assert_eq!(PluginComponents::default().summary_line(), None);
        assert!(PluginComponents::default().is_empty());
    }

    #[test]
    fn plugin_components_sanitize_caps_categories() {
        let mut components = PluginComponents {
            skills: (0..60)
                .map(|i| ComponentItem {
                    name: format!("s{i}\u{1b}"),
                    description: Some("d".repeat(300)),
                })
                .collect(),
            ..Default::default()
        };
        components.sanitize();
        assert_eq!(components.skills.len(), MAX_COMPONENTS_PER_CATEGORY);
        assert_eq!(components.skills[0].name, "s0");
        assert_eq!(
            components.skills[0].description.as_ref().unwrap().len(),
            120
        );
    }

    fn one_item_per_category() -> PluginComponents {
        let dirty = |name: &str| ComponentItem {
            name: format!("{name}\u{1b}"),
            description: None,
        };
        PluginComponents {
            skills: vec![dirty("s")],
            commands: vec![dirty("c")],
            agents: vec![dirty("a")],
            mcp_servers: vec![dirty("m")],
            hooks: vec![dirty("h")],
            lsp_servers: vec![dirty("l")],
        }
    }

    #[test]
    fn plugin_components_every_consumer_path_covers_all_six_categories() {
        let mut components = one_item_per_category();
        assert_eq!(components.categories().len(), 6);
        assert!(
            components
                .categories()
                .iter()
                .all(|(_, items)| items.len() == 1)
        );
        assert_eq!(
            components.summary_line().as_deref(),
            Some(
                "1 skill \u{b7} 1 command \u{b7} 1 agent \u{b7} 1 MCP server \u{b7} 1 hook \u{b7} 1 LSP server"
            )
        );
        components.sanitize();
        for (_, items) in components.categories() {
            assert!(!items[0].name.contains('\u{1b}'));
        }
    }

    #[test]
    fn plugin_components_serde_roundtrip_camel_case() {
        let components = PluginComponents {
            skills: vec![item("brainstorming", Some("Structured ideation"))],
            mcp_servers: vec![item("notion", None)],
            lsp_servers: vec![item("rust-analyzer", None)],
            hooks: vec![item("PreToolUse", Some("Bash"))],
            ..Default::default()
        };
        let json = serde_json::to_string(&components).unwrap();
        assert!(json.contains("mcpServers"), "{json}");
        assert!(json.contains("lspServers"), "{json}");
        assert!(!json.contains("commands"), "{json}");
        let parsed: PluginComponents = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, components);
        assert_eq!(parsed.skills[0].name, "brainstorming");
        assert_eq!(
            parsed.skills[0].description.as_deref(),
            Some("Structured ideation")
        );
    }

    #[test]
    fn marketplace_plugin_entry_roundtrips_components() {
        let json = r#"{
            "name": "p",
            "version": null,
            "description": null,
            "category": null,
            "author": null,
            "relativePath": "plugins/p",
            "skillCount": 0,
            "hasHooks": false,
            "hasAgents": false,
            "hasMcp": false,
            "installStatus": "not_installed",
            "installedVersion": null,
            "components": {
                "skills": [{"name": "code-review", "description": "Review staged changes"}],
                "unknownField": []
            }
        }"#;
        let parsed: MarketplacePluginEntry = serde_json::from_str(json).unwrap();
        let components = parsed.components.clone().expect("components present");
        assert_eq!(components.skills.len(), 1);
        assert_eq!(components.skills[0].name, "code-review");
        let reserialized = serde_json::to_string(&parsed).unwrap();
        assert!(reserialized.contains("code-review"), "{reserialized}");
    }
}

// ---------------------------------------------------------------------------
// Marketplace types (wire format for echo.agent/marketplace/* ACP endpoints)
// ---------------------------------------------------------------------------

/// Response for `echo.agent/marketplace/list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketplaceListResponse {
    pub sources: Vec<MarketplaceScanResult>,
}

impl MarketplaceListResponse {
    /// Sanitize all catalog-derived components in the response. Every
    /// consumer that renders this data to a terminal must call this at its
    /// ingestion point (deserialization bypasses [`ComponentItem::new`]).
    pub fn sanitize(&mut self) {
        for source in &mut self.sources {
            for plugin in &mut source.plugins {
                if let Some(components) = plugin.components.as_mut() {
                    components.sanitize();
                }
            }
        }
    }
}

/// Result of scanning a single marketplace source.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketplaceScanResult {
    pub source_name: String,
    pub source_kind: String,
    pub source_url_or_path: String,
    pub plugins: Vec<MarketplacePluginEntry>,
    pub error: Option<String>,
}

/// A marketplace plugin with install status.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketplacePluginEntry {
    pub name: String,
    pub version: Option<String>,
    pub description: Option<String>,
    pub category: Option<String>,
    pub author: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub domains: Vec<String>,
    #[serde(default)]
    pub homepage: Option<String>,
    pub relative_path: String,
    pub skill_count: usize,
    pub has_hooks: bool,
    pub has_agents: bool,
    pub has_mcp: bool,
    pub install_status: String,
    pub installed_version: Option<String>,
    /// Structured inventory from the marketplace catalog. None = no catalog
    /// data for this plugin (or the sender predates this field).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub components: Option<PluginComponents>,
    /// Remote git URL for URL-sourced plugins (not present for local plugins).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_url: Option<String>,
    /// Git ref (branch/tag) for remote URL sources.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_subdir: Option<String>,
}

/// Request wrapper for `echo.agent/marketplace/action`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketplaceActionRequest {
    pub session_id: String,
    pub action: MarketplaceAction,
}

/// Marketplace management actions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MarketplaceAction {
    /// Re-scan all sources (git: pull, local: re-read).
    Refresh {
        /// If set, only refresh this source (by canonical URL/path).
        #[serde(default)]
        #[serde(alias = "sourceUrlOrPath")]
        source_url_or_path: Option<String>,
    },
    /// Install a plugin from a marketplace source.
    Install {
        /// Canonical source identity (git URL or local path).
        #[serde(alias = "sourceUrlOrPath")]
        source_url_or_path: String,
        #[serde(alias = "pluginRelativePath")]
        plugin_relative_path: String,
    },
    /// Update an installed marketplace plugin to the latest version.
    Update {
        /// Canonical source identity (git URL or local path).
        #[serde(alias = "sourceUrlOrPath")]
        source_url_or_path: String,
        #[serde(alias = "pluginRelativePath")]
        plugin_relative_path: String,
    },
    /// Uninstall a marketplace-installed plugin.
    Uninstall {
        /// Canonical source identity.
        #[serde(alias = "sourceUrlOrPath")]
        source_url_or_path: String,
        #[serde(alias = "pluginRelativePath")]
        plugin_relative_path: String,
    },
    /// Add a new marketplace source (git URL).
    AddSource {
        /// Git URL of the marketplace repo.
        url: String,
    },
    /// Remove a marketplace source.
    RemoveSource {
        /// Canonical source identity (git URL or local path).
        #[serde(alias = "sourceUrlOrPath")]
        source_url_or_path: String,
    },
}

#[cfg(test)]
mod marketplace_desktop_contract_tests {
    use super::*;

    #[test]
    fn marketplace_desktop_camel_case_actions_keep_legacy_wire_compatibility() {
        let source = "https://127.0.0.1:4443/review-market.git";
        for kind in ["install", "update", "uninstall", "remove_source", "refresh"] {
            let mut desktop = serde_json::json!({"type":kind,"sourceUrlOrPath":source});
            let mut legacy = serde_json::json!({"type":kind,"source_url_or_path":source});
            if matches!(kind, "install" | "update" | "uninstall") {
                desktop["pluginRelativePath"] = "plugins/review-plugin".into();
                legacy["plugin_relative_path"] = "plugins/review-plugin".into();
            }
            let actual: MarketplaceAction = serde_json::from_value(desktop)
                .unwrap_or_else(|error| panic!("Actual desktop {kind} action rejected: {error}"));
            let previous: MarketplaceAction = serde_json::from_value(legacy).unwrap();
            assert_eq!(
                actual, previous,
                "Desktop source selection changed for {kind}"
            );
            let serialized = serde_json::to_value(actual).unwrap();
            assert_eq!(serialized["source_url_or_path"], source);
            assert!(
                serialized.get("sourceUrlOrPath").is_none(),
                "Legacy Runtime wire changed"
            );
        }
    }

    #[test]
    fn plugins_desktop_camel_case_actions_keep_legacy_wire_compatibility() {
        for kind in ["enable", "disable", "update", "uninstall"] {
            let desktop = serde_json::json!({"type":kind,"pluginId":"user/01234567/review-plugin"});
            let legacy = serde_json::json!({"type":kind,"plugin_id":"user/01234567/review-plugin"});
            let actual: PluginsAction = serde_json::from_value(desktop).unwrap();
            let previous: PluginsAction = serde_json::from_value(legacy).unwrap();
            assert_eq!(actual, previous);
            let value = serde_json::to_value(actual).unwrap();
            assert_eq!(value["plugin_id"], "user/01234567/review-plugin");
            assert!(value.get("pluginId").is_none());
        }
        assert!(
            serde_json::from_value::<PluginsAction>(
                serde_json::json!({"type":"enable","pluginId":"a","plugin_id":"b"})
            )
            .is_err(),
            "Conflicting desktop/legacy IDs must not select an arbitrary target"
        );
        assert!(serde_json::from_value::<MarketplaceAction>(serde_json::json!({"type":"remove_source","sourceUrlOrPath":"a","source_url_or_path":"b"})).is_err());
    }
}
