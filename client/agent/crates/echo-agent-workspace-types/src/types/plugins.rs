//! Discovery shapes for plugins and hooks surfaced by `OpsChunk::Plugins`,
//! `OpsChunk::Plugin`, `WorkspaceEvent::PluginsChanged`, and
//! `WorkspaceEvent::HooksChanged`.
//!
//! TODO(workspace): align with the canonical types in
//! `echo-agent-hooks-plugins-types` and `echo-agent-plugin-marketplace`.

use serde::{Deserialize, Serialize};

/// Plugin metadata.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginInfo {
    /// Stable identifier.
    pub id: String,
    /// Display name.
    #[serde(default)]
    pub name: String,
    /// Plugin version (semver).
    #[serde(default)]
    pub version: String,
    /// Filesystem path to the plugin (as a string).
    #[serde(default)]
    pub path: String,
    /// Source: `"global"`, `"workspace"`, `"marketplace"`, ...
    #[serde(default)]
    pub source: String,
    /// Whether the plugin is currently enabled.
    #[serde(default)]
    pub enabled: bool,
}

/// Hook metadata.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookInfo {
    /// Stable identifier (e.g. `"pre-tool-call"`).
    pub id: String,
    /// Display name.
    #[serde(default)]
    pub name: String,
    /// Hook event the script attaches to (e.g. `"PreToolUse"`).
    ///
    /// TODO(workspace): the event field will become a typed enum once
    /// aligned with `echo_agent_hooks_plugins_types::HookEvent` -- right
    /// now it's a free-form string for placeholder convenience, which
    /// allows typos through.
    #[serde(default)]
    pub event: String,
    /// Originating plugin id, if the hook came from a plugin.
    #[serde(default)]
    pub plugin_id: Option<String>,
    /// Whether this hook is currently enabled.
    #[serde(default)]
    pub enabled: bool,
}
