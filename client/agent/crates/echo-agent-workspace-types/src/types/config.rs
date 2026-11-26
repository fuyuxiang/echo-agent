//! Configuration shapes referenced from session lifecycle requests and
//! `OpsChunk::ProjectConfig` / `OpsChunk::Permissions`.
//!
//! TODO(workspace): align with the canonical project / permission /
//! agent-session config types in `echo-agent-config` and friends.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Filesystem isolation strategy for a forked session.
///
/// `Default` returns [`IsolationMode::None`], which is appropriate for
/// the root session (which shares the workspace's working tree). Note
/// that subagent forks should explicitly opt into a more restrictive
/// mode (e.g. `Worktree`); relying on `Default`
/// for a subagent gives it shared-tree access, which is rarely the
/// right default for an exploratory child agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolationMode {
    /// No isolation: subagent shares the parent's working tree.
    #[default]
    None,
    /// Run the subagent in a copy-on-write git worktree.
    Worktree,
    /// Run the subagent inside a sandbox/container.
    Sandbox,
}

/// Capability mode applied to a forked session.
///
/// `Default` returns [`CapabilityMode::ReadWrite`], which is
/// appropriate for the root session. Subagents should explicitly opt
/// into a more restrictive mode (typically
/// `ReadOnly`); relying on `Default` for a subagent gives it
/// read+write access, which is rarely the right default for an
/// exploratory child agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityMode {
    /// Full read+write capability (default for the root session).
    #[default]
    ReadWrite,
    /// Read-only: tools that mutate state are unavailable.
    ReadOnly,
    /// No tools at all.
    None,
}

/// Per-tool-server configuration knob.
///
/// TODO(workspace): align with the actual MCP/tool-server config in
/// `echo-agent-tools` once the wire surface is firm.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolServerConfig {
    /// Tool server identifier.
    pub id: String,
    /// Whether this tool server is enabled for the session.
    #[serde(default)]
    pub enabled: bool,
    /// Optional command override (for dynamically launched servers).
    #[serde(default)]
    pub command: Option<String>,
    /// Free-form arguments (key/value).
    #[serde(default)]
    pub args: BTreeMap<String, String>,
}

/// Configuration applied when forking a session via
/// `SessionLifecycleRequest::Fork`.
///
/// `Default` returns a config with `IsolationMode::None` and
/// `CapabilityMode::ReadWrite`. **Be careful using `Default` to
/// construct a subagent fork**: a subagent
/// should typically be forked with `Worktree` isolation and a more
/// restrictive capability mode -- the defaults here are oriented at
/// the root session, not subagents. Construct subagent configs by
/// fully-naming the relevant fields (or use a builder helper) rather
/// than relying on `..Default::default()`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSessionConfig {
    /// Agent identifier (e.g. `"subagent-explore"`).
    pub agent_id: String,
    /// Filesystem isolation strategy.
    #[serde(default)]
    pub isolation: IsolationMode,
    /// Capability mode (read-only, read-write, none).
    #[serde(default)]
    pub capability_mode: CapabilityMode,
    /// Optional per-tool-server overrides.
    #[serde(default)]
    pub tool_config: Vec<ToolServerConfig>,
    /// Maximum recursion depth for subagent nesting. 0 = no further nesting.
    #[serde(default)]
    pub max_depth: u32,
    /// Working directory override (relative to workspace root).
    #[serde(default)]
    pub cwd_override: Option<String>,
    /// Extra environment variables to set for the subagent.
    #[serde(default)]
    pub extra_env: BTreeMap<String, String>,
}

/// Project configuration returned by `OpsChunk::ProjectConfig`.
///
/// TODO(workspace): align with `echo_agent_config::ProjectConfig`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectConfig {
    /// Free-form key/value config (placeholder).
    #[serde(default)]
    pub values: BTreeMap<String, String>,
    /// Whether the project is trusted (allows hooks/plugins to run).
    #[serde(default)]
    pub trusted: bool,
}

/// Permission policy returned by `OpsChunk::Permissions`.
///
/// TODO(workspace): align with the canonical permission policy type
/// (currently a free-form JSON shape).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionPolicy {
    /// Tool patterns that are unconditionally allowed (no prompt).
    #[serde(default)]
    pub allow: Vec<String>,
    /// Tool patterns that are unconditionally denied.
    #[serde(default)]
    pub deny: Vec<String>,
    /// Tool patterns that always prompt for permission.
    #[serde(default)]
    pub ask: Vec<String>,
}
