//! The payload clients receive. What each field means is documented once, in
//! `echo-agent-pager/docs/user-guide/25-status-line.md`, which a test holds to
//! this type; the comments here record only what that guide cannot.
//!
//! Two rules hold it together: a value EchoAgent cannot source is `None` rather
//! than zero, and fields are snake_case, the one exception to the camelCase
//! rule in `echo-agent-pager/docs/internal/28-extension-methods.md`, because
//! renaming one silently breaks every script that reads it.

use serde::{Deserialize, Serialize};

/// The payload's shape, which a script branches on instead of the release in
/// `version`. Adding a field never bumps it; removing or retyping one does.
pub const STATUS_LINE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StatusLineContext {
    /// The one field whose own `default` matters: `Default` sets the current
    /// version, so without this an old payload would claim to be current.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<u32>,
    pub cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Filled by the client, not the agent, since the name is renameable
    /// locally. Absent from the notification, present on a command row's stdin.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    pub model: StatusLineModel,
    pub workspace: StatusLineWorkspace,
    pub version: String,
    pub cost: StatusLineCost,
    pub context_window: StatusLineContextWindow,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<StatusLineEffort>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree: Option<StatusLineWorktree>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn: Option<StatusLineTurn>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger: Option<StatusLineTrigger>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StatusLineTrigger {
    State,
    #[serde(rename = "refresh_interval")]
    RefreshInterval,
}

impl Default for StatusLineContext {
    fn default() -> Self {
        Self {
            schema_version: Some(STATUS_LINE_SCHEMA_VERSION),
            cwd: String::new(),
            session_id: None,
            session_name: None,
            prompt_id: None,
            transcript_path: None,
            model: StatusLineModel::default(),
            workspace: StatusLineWorkspace::default(),
            version: String::new(),
            cost: StatusLineCost::default(),
            context_window: StatusLineContextWindow::default(),
            effort: None,
            worktree: None,
            turn: None,
            trigger: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StatusLineTurn {
    /// Unix milliseconds, so a client subtracts it from its own clock.
    pub started_at_ms: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StatusLineWorktree {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub main_worktree_root: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StatusLineModel {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StatusLineEffort {
    pub level: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StatusLineWorkspace {
    pub current_dir: String,
    /// Not `project_dir`, which names a launch directory elsewhere.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_worktree: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<StatusLineRepo>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StatusLineRepo {
    pub host: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StatusLineCost {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_cost_usd: Option<f64>,
    /// Since this process attached, not since the session was created.
    pub total_duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_api_duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StatusLineContextWindow {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<u64>,
    /// Not `total_*`, which is used elsewhere for the live window.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_output_tokens: Option<u64>,
    /// Cumulative, where `current_usage` elsewhere is one call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_usage: Option<StatusLineSessionUsage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub used_percentage: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining_percentage: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_compact_threshold_percent: Option<u8>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StatusLineSessionUsage {
    /// Disjoint from the cache buckets, so the three sum without overlap.
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
}

#[cfg(test)]
#[path = "context_tests.rs"]
mod tests;
