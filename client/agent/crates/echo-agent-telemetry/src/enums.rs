//! Shared telemetry/config enums extracted from shell.
//!
//! These were originally defined inside `echo-agent-runtime` (in
//! `session::mcp_servers` and `util::config`) but are referenced by
//! telemetry payload structs in this crate, so they live here and shell
//! re-exports them from their original paths to keep callers unchanged.

/// MCP initialization strategy
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum McpInitStrategy {
    /// Wait for MCP initialization before first LLM call
    #[default]
    Blocking,
    /// Start immediately, advertise tools as they become available
    Progressive,
}

impl<S: AsRef<str>> From<S> for McpInitStrategy {
    fn from(s: S) -> Self {
        match s.as_ref() {
            "progressive" => McpInitStrategy::Progressive,
            _ => McpInitStrategy::Blocking,
        }
    }
}

/// How a PR creation was performed. Shared between the shell's session
/// signals (`turn_result.json`) and the `pr_created` telemetry event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrCreationSource {
    /// `gh pr create` via the bash tool.
    Bash,
    /// An MCP `create_pull_request` tool.
    Mcp,
}

/// How the agent handles tool execution permissions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    /// Prompt the user for each tool call (default).
    #[default]
    Ask,
    /// Approve everything without prompting.
    AlwaysApprove,
    /// LLM transcript classifier reviews non-fast-path tool calls.
    Auto,
}

impl PermissionMode {
    pub fn is_always_approve(self) -> bool {
        matches!(self, Self::AlwaysApprove)
    }

    pub fn is_auto(self) -> bool {
        matches!(self, Self::Auto)
    }

    pub fn from_yolo(yolo: bool) -> Self {
        if yolo { Self::AlwaysApprove } else { Self::Ask }
    }
}
