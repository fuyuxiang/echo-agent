//! Per-session event log (`events.jsonl`).
//!
//! The typed [`Event`] variants, the shared [`EventWriter`] that appends them
//! as JSON lines, and the per-session [`EventTracker`] holding the turn-local
//! state those events are derived from.

pub mod log;
pub mod tracker;
pub mod types;

pub use log::EventWriter;
pub use tracker::EventTracker;
pub use types::{
    CancellationCategory, EVENT_SCHEMA_VERSION, Event, McpConfigServer, McpErrorCategory,
    PermissionDecision, Phase, SessionRelationship, ToolCompletedSource, ToolOutcome,
    TurnOutcomeLabel,
};
