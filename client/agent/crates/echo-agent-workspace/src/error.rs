//! Workspace error types.
use crate::capability::CapabilityMode;
/// Errors surfaced by the workspace public API.
///
/// `#[non_exhaustive]` so adding new variants is a non-breaking change.
/// Tests should match on variants rather than scrape the `Display` text.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WorkspaceError {
    #[error("parent session not found: {0}")]
    ParentSessionNotFound(String),
    #[error("session not found: {0}")]
    SessionNotFound(String),
    #[error("session already exists: {0}")]
    SessionAlreadyExists(String),
    #[error("agent_id must be non-empty")]
    EmptyAgentId,
    #[error("the main session cannot be dropped")]
    CannotDropMainSession,
    #[error("toolset finalization failed: {0}")]
    Finalize(String),
    #[error("capability widening rejected: child {child:?} is not a subset of parent {parent:?}")]
    CapabilityWidening {
        parent: CapabilityMode,
        child: CapabilityMode,
    },
    #[error("session {caller:?} is not authorised to operate on session {target:?}")]
    Unauthorized { caller: String, target: String },
    /// A toolset mutation was rejected because the target session has an
    /// active turn. Retryable at the turn boundary (`after_turn`).
    #[error("turn active for session {0}; retry the tool-config update at the turn boundary")]
    TurnActive(String),
    #[error("maximum fork depth exceeded for parent session {parent:?}")]
    MaxDepthExceeded { parent: String },
    #[error("internal task failure: {0}")]
    JoinError(String),
    #[error("invalid hunk action: {0}")]
    InvalidHunkAction(String),
    #[error("hunk action failed: {0}")]
    HunkActionFailed(String),
    /// An error from the server connection or tool server.
    #[error("hub error: {0}")]
    HubError(String),
    #[error("unknown workspace method: {0}")]
    UnknownMethod(String),
    #[error("workspace archive export failed: {0}")]
    ExportArchiveLimitExceeded(String),
    #[error("github export error: {message}")]
    ExportGithub {
        kind: echo_agent_workspace_types::rpc::export_github::ExportGithubError,
        message: String,
    },
    /// The workspace is draining/shutting down and is no longer accepting new
    /// sessions. Surfaced when a `bind`/create races a terminal drain so the
    /// shared upload queue is never torn down out from under a fresh session.
    #[error("workspace is shutting down; not accepting new sessions")]
    ShuttingDown,
    /// The session's toolset is externally owned — installed by a local
    /// (shell) bind, its `Terminal` resource is not the session-owned
    /// backend — so an RPC-driven toolset mutation is refused instead of
    /// silently skipped. Hard error: retrying cannot succeed while the
    /// local bind holds the toolset.
    #[error("toolset externally owned (local bind), mutation refused: {0}")]
    ToolsetExternallyOwned(String),
}
impl WorkspaceError {
    /// Low-cardinality `error_kind` metric label: the variant name in
    /// snake_case; `DeployError` reports its per-kind `wire_code()`.
    pub fn metric_kind(&self) -> &'static str {
        match self {
            Self::ParentSessionNotFound(_) => "parent_session_not_found",
            Self::SessionNotFound(_) => "session_not_found",
            Self::SessionAlreadyExists(_) => "session_already_exists",
            Self::EmptyAgentId => "empty_agent_id",
            Self::CannotDropMainSession => "cannot_drop_main_session",
            Self::Finalize(_) => "finalize",
            Self::CapabilityWidening { .. } => "capability_widening",
            Self::Unauthorized { .. } => "unauthorized",
            Self::TurnActive(_) => "turn_active",
            Self::MaxDepthExceeded { .. } => "max_depth_exceeded",
            Self::JoinError(_) => "join_error",
            Self::InvalidHunkAction(_) => "invalid_hunk_action",
            Self::HunkActionFailed(_) => "hunk_action_failed",
            Self::HubError(_) => "hub_error",
            Self::UnknownMethod(_) => "unknown_method",
            Self::ExportArchiveLimitExceeded(_) => "export_archive_limit_exceeded",
            Self::ExportGithub { kind, .. } => kind.wire_code(),
            Self::ShuttingDown => "shutting_down",
            Self::ToolsetExternallyOwned(_) => "toolset_externally_owned",
        }
    }
}
/// Convenience alias for the workspace's primary `Result` type.
pub type WorkspaceResult<T> = Result<T, WorkspaceError>;
#[cfg(test)]
mod tests {
    use super::WorkspaceError;
    #[test]
    fn metric_kind_is_message_free() {
        let err = WorkspaceError::HubError("something wildly unique 12345".into());
        assert_eq!(err.metric_kind(), "hub_error");
    }
}
