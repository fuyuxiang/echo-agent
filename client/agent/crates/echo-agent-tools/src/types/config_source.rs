use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Where a piece of configuration was loaded from.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ConfigSource {
    /// Built-in / bundled with the binary.
    Builtin,
    /// Bundled skill shipped with the binary (extracted to ~/.echo-agent/skills/
    /// or injected via bundled skill dirs).
    Bundled { path: PathBuf },
    /// Server-synced (e.g. ~/.echo-agent/server-skills from the skill store).
    Server { path: PathBuf },
    /// Project-scoped: cwd/.echo-agent/ or cwd/.claude/.
    Project { path: PathBuf },
    /// User-scoped: ~/.echo-agent/ or ~/.claude/.
    User { path: PathBuf },
    /// Plugin-provided component.
    Plugin { plugin_name: String, path: PathBuf },
    /// config.toml `[mcp_servers.*]`, `[skills]`, etc. `path` is
    /// domain-specific: the declaring config.toml for MCP servers, the
    /// skill's own SKILL.md for `[skills].paths` skills.
    ConfigToml { path: PathBuf },
    /// `~/.claude.json` MCP servers.
    ClaudeJson { path: PathBuf },
    /// `.mcp.json` project-level MCP config.
    McpJson { path: PathBuf },
    /// CLI override (`--plugin-dir`, `--mcp-server`).
    Cli { path: PathBuf },
    /// Managed (server-managed / IT-deployed).
    Managed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<PathBuf>,
    },
}

impl ConfigSource {
    /// Short human-readable label for terminal display.
    pub fn display_short(&self) -> String {
        match self {
            Self::Builtin => "builtin".into(),
            Self::Bundled { path } => format!("bundled: {}", path.display()),
            Self::Server { path } => format!("server: {}", path.display()),
            Self::Project { path } => format!("project: {}", path.display()),
            Self::User { path } => format!("user: {}", path.display()),
            Self::Plugin { plugin_name, .. } => format!("plugin: {plugin_name}"),
            Self::ConfigToml { path } => format!("config: {}", path.display()),
            Self::ClaudeJson { .. } => "~/.claude.json".into(),
            Self::McpJson { path } => format!(".mcp.json: {}", path.display()),
            Self::Cli { .. } => "cli".into(),
            Self::Managed { .. } => "managed".into(),
        }
    }

    /// Compact label for columnar terminal display (no paths).
    pub fn display_label(&self) -> String {
        match self {
            Self::Builtin => "builtin".into(),
            Self::Bundled { .. } => "bundled".into(),
            Self::Server { .. } => "server".into(),
            Self::Project { .. } => "project".into(),
            Self::User { .. } => "user".into(),
            Self::Plugin { plugin_name, .. } => format!("plugin: {plugin_name}"),
            Self::ConfigToml { .. } => "config".into(),
            Self::ClaudeJson { .. } => "~/.claude.json".into(),
            Self::McpJson { .. } => ".mcp.json".into(),
            Self::Cli { .. } => "cli".into(),
            Self::Managed { .. } => "managed".into(),
        }
    }

    /// Plugin name if this is a plugin-provided component, `None` otherwise.
    pub fn plugin_name(&self) -> Option<&str> {
        match self {
            Self::Plugin { plugin_name, .. } => Some(plugin_name),
            _ => None,
        }
    }

    /// Filesystem path, if any.
    pub fn path(&self) -> Option<&std::path::Path> {
        match self {
            Self::Builtin => None,
            Self::Bundled { path }
            | Self::Server { path }
            | Self::Project { path }
            | Self::User { path }
            | Self::Plugin { path, .. }
            | Self::ConfigToml { path }
            | Self::ClaudeJson { path }
            | Self::McpJson { path }
            | Self::Cli { path } => Some(path),
            Self::Managed { path } => path.as_deref(),
        }
    }
}
