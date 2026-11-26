//! ToolBridge: re-exported from `echo-agent-tools`.
//!
//! The bridge implementation now lives in `echo_agent_tools::bridge`.
//! This module re-exports everything for backward compatibility.

pub use echo_agent_tools::bridge::{ToolBridge, ToolBridgeResult};
