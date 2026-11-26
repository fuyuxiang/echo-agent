//! Hub tool ID constants, canonical in `echo_agent_workspace_types::rpc`
//! and re-exported here for existing importers.
//!
//! `WORKSPACE_TOOL_NOTIFICATIONS_TOOL_ID` is intentionally producer-less today;
//! see [`crate::hub_channel::extract_tool_notification`].

pub use echo_agent_workspace_types::rpc::{
    WORKSPACE_CLIENT_EXT_NOTIFICATIONS_TOOL_ID, WORKSPACE_EVENTS_TOOL_ID, WORKSPACE_RPC_TOOL_ID,
    WORKSPACE_TOOL_NOTIFICATIONS_TOOL_ID,
};
