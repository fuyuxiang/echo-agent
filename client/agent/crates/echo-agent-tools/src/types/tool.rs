//! Tool types and post-execution reminders.
//!
//! The tool runtime contract (`Tool` trait) lives in `echo_agent_tool_runtime`.
//! Tool metadata (kind, namespace, fingerprinting, etc.) lives in
//! `crate::types::tool_metadata::ToolMetadata`.
//!
//! This module provides:
//! - `ToolNamespace`, `ToolKind` — classification enums
//! - `Reminder` — post-execution system reminders (per-tool + cross-cutting)
use crate::types::output::ToolOutput;
use crate::types::requirements::{Expr, ToolRequirement};
use crate::types::resources::SharedResources;
/// The toolset a tool belongs to.
///
/// Serializes to snake_case (`echo_agent_build`, `mcp`, …) for the
/// canonical tool `_meta` wire contract. PascalCase aliases are accepted on
/// deserialize so legacy persisted/manifest values still parse. The
/// `Display` impl remains PascalCase for existing qualified id strings
/// (e.g. `"EchoAgentBuild:read_file"`); only the serde form goes on the wire.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    derive_more::Display,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
    strum::EnumIter,
)]
#[serde(rename_all = "snake_case")]
pub enum ToolNamespace {
    #[serde(alias = "EchoAgentBuild")]
    EchoAgentBuild,
    #[serde(alias = "EchoAgentBuildConcise")]
    EchoAgentBuildConcise,
    #[serde(alias = "EchoAgentBuildHashline")]
    EchoAgentBuildHashline,
    #[serde(alias = "Codex")]
    Codex,
    #[serde(rename = "opencode", alias = "OpenCode", alias = "open_code")]
    OpenCode,
    #[serde(rename = "mcp", alias = "MCP")]
    MCP,
}
/// Categorizes what a tool does at a high level.
///
/// Serializes as snake_case strings (e.g. `"read"`, `"list_dir"`, `"web_search"`).
/// `Other` is the default for tools that don't fit neatly elsewhere, and the
/// `#[serde(other)]` sink so a consumer pinned to an older schema deserializes
/// a newer `kind` to `Other` instead of erroring. The `JsonSchema` impl (in
/// [`crate::tool_taxonomy`]) mirrors that openness: an advisory string, not a
/// closed enum.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    strum::EnumCount,
    strum::EnumIter,
    strum::IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum ToolKind {
    Read,
    Edit,
    Delete,
    ListDir,
    Write,
    Move,
    Search,
    Lsp,
    Execute,
    Plan,
    WebSearch,
    WebFetch,
    BackgroundTaskAction,
    WaitTasksAction,
    KillTaskAction,
    List,
    Skill,
    MemorySearch,
    MemoryGet,
    Task,
    EnterPlan,
    ExitPlan,
    AskUser,
    ImageGen,
    VideoGen,
    ImageToVideo,
    ReferenceToVideo,
    DeployApp,
    InitOrUpdateApp,
    SearchTool,
    UseTool,
    Monitor,
    GoalUpdate,
    Workflow,
    #[serde(other)]
    Other,
}
impl ToolKind {
    /// Total number of `ToolKind` variants (powered by `strum::EnumCount`).
    ///
    /// Used by downstream compile-time assertions (e.g. `ALL_TOOL_KINDS` in
    /// `capability.rs`) to catch missing variants when the enum grows.
    pub const VARIANT_COUNT: usize = <Self as strum::EnumCount>::COUNT;
    /// Stable snake_case key for this kind (the `tools.by_kind.<key>` template key).
    pub fn as_key(self) -> &'static str {
        self.into()
    }
}
/// System reminders that fire after a tool call completes.
///
/// Implemented by:
/// - **Per-tool reminders** on tool structs (e.g., `ReadFileTool`: empty
///   file, offset past end).
/// - **Cross-cutting reminders** on standalone structs (e.g.,
///   `SkillDiscoveryReminder`) that react to any tool call.
#[async_trait::async_trait]
pub trait Reminder {
    /// Requirements for this reminder to be active.
    fn requires_expr(&self) -> Expr<ToolRequirement> {
        Expr::True
    }
    /// Collect reminders after a tool execution completes.
    async fn collect_reminders(
        &self,
        _resources: SharedResources,
        _tool_output: &ToolOutput,
    ) -> Vec<String> {
        vec![]
    }
}
