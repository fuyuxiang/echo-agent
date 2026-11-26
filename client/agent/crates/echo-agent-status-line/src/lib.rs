//! The status-line contract. [`config`] is what a user writes in
//! `[ui.status_line]`; [`context`] is what the agent sends clients to draw.
//!
//! A leaf crate, upstream of the agent and of every client.

pub mod config;
pub mod context;

/// The client capability that turns the row on, advertised in `initialize`'s
/// `clientCapabilities._meta`. Absent means off.
pub const STATUS_LINE_CAPABILITY: &str = "echo.agent/statusLine";

/// The per-session spelling of [`STATUS_LINE_CAPABILITY`], injected by a leader
/// into `session/new`, `session/load` and `session/resume` `_meta`. A leader
/// multiplexes clients, so the answer travels with the session, not the
/// process.
pub const CLIENT_STATUS_LINE_META: &str = "clientStatusLine";

/// Re-exported to the root, where a caller looks for it, from the module whose
/// private fields it fills in.
#[cfg(any(test, feature = "test-support"))]
pub use config::test_support;
pub use config::{ResolvedStatusLine, StatusLineConfig, StatusLineItem, StatusLineType};
pub use context::{
    STATUS_LINE_SCHEMA_VERSION, StatusLineContext, StatusLineContextWindow, StatusLineCost,
    StatusLineEffort, StatusLineModel, StatusLineRepo, StatusLineSessionUsage, StatusLineTrigger,
    StatusLineTurn, StatusLineWorkspace, StatusLineWorktree,
};
