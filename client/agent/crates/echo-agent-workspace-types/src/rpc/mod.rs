//! Canonical wire types for hub-proxied `workspace.*` RPC methods,
//! shared by the server (hub_server), the shell proxy client
//! (`WorkspaceOps`), and clients that cannot depend on
//! `echo-agent-workspace`. Types not yet migrated here live next to their
//! `WorkspaceOp` impls in that crate; each type has exactly one
//! [`WorkspaceRpc`] impl. [`RpcError`]-code-to-error-enum mapping is
//! deliberately not defined here.

use serde::Serialize;
use serde::de::DeserializeOwned;

pub mod agents_md;
pub mod code_nav;
pub mod deploy;
pub mod envelope;
pub mod export;
pub mod export_github;
pub mod fs;
pub mod git;
pub mod hooks;
pub mod hunks;
pub mod presence;
pub mod repos;
pub mod search;
pub mod session;
pub mod skills;
pub mod workspace;
pub mod worktree;

pub use envelope::{RpcEnvelope, RpcError};

/// Tool ID for the `WorkspaceRpcHandler` (workspace method dispatch).
pub const WORKSPACE_RPC_TOOL_ID: &str = "workspace_rpc";

/// Tool ID used for `WorkspaceEvent` notification frames.
pub const WORKSPACE_EVENTS_TOOL_ID: &str = "workspace_events";

/// Tool ID used for `ToolNotification` forwarding frames.
pub const WORKSPACE_TOOL_NOTIFICATIONS_TOOL_ID: &str = "workspace_tool_notifications";

/// Tool ID used for workspace-originated client ext-notification frames
/// (e.g. `echo.agent/search/fuzzy/status`). Carries `{ method, params }`.
pub const WORKSPACE_CLIENT_EXT_NOTIFICATIONS_TOOL_ID: &str = "workspace_client_ext_notifications";

/// What a workspace RPC's execution says about human presence, consumed by
/// the idle-hibernation activity tracker.
///
/// `Mutation` marks client-driven writes (file writes, git commits, hunk
/// actions, …): evidence a person is working through the workspace API, so
/// the sandbox must not be idle-hibernated underneath them. `Read` covers
/// everything else — reads, polls, discovery — plus deliberate exceptions
/// that do mutate but must never hold a sandbox alive: teardown
/// (`drop_session`), maintenance (`worktree_gc`, db rebuilds), and
/// agent-turn boundaries already tracked through `turn_active`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RpcActivityClass {
    /// A client-driven write — counts as workspace activity.
    Mutation,
    /// Reads, polls, and non-activity mutations — never counts.
    Read,
}

/// Marker trait for typed workspace RPC requests. Client and server use
/// the same struct for the same method. `Response` is bounded both ways
/// because servers serialize it into the [`RpcEnvelope`] and clients
/// deserialize it out.
pub trait WorkspaceRpc: Serialize {
    /// Wire method name (e.g. `"workspace.git_status_ext"`).
    const METHOD: &'static str;
    /// Whether executing this method counts as human activity for idle
    /// hibernation. No default, so every method is classified explicitly.
    const ACTIVITY: RpcActivityClass;
    type Response: Serialize + DeserializeOwned + Send;
}
