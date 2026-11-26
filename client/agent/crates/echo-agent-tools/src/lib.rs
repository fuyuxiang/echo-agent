//! EchoAgent tools library.

pub use echo_agent_version::VERSION;

/// Default maximum output size (in bytes) for tool results sent to the model.
/// 40 KB ≈ 10 000 tokens
pub const DEFAULT_TOOL_OUTPUT_BYTES: usize = 40_000;

/// Default maximum output size (in characters) for bash/terminal tool results.
/// 20 000 chars ≈ 5 000 tokens. Matches the common `SHELL_CHAR_HARD_LIMIT`.
pub const DEFAULT_TOOL_OUTPUT_CHARS: usize = 20_000;

/// MCP inline tool-result cap (`MCP_MAX_OUTPUT_BYTES` and host/env helpers).
pub use util::mcp_truncate::{
    ENV_ECHO_AGENT_MAX_MCP_OUTPUT_BYTES, ENV_MAX_MCP_OUTPUT_BYTES, MCP_MAX_OUTPUT_BYTES,
    mcp_max_output_bytes, mcp_max_output_bytes_from_env, set_mcp_max_output_bytes,
};

pub mod attribution;

pub mod bridge;
pub mod computer;
pub mod gitignore;
pub mod implementations;
pub mod mcp_elicitation;
pub mod media_gen_limits;
pub mod normalization;
pub mod notification;
pub mod persistence;
pub mod registry;
pub mod reminders;
pub mod retry;
pub mod tool_taxonomy;
pub mod types;
pub mod util;
pub mod versions;

pub use attribution::{
    Auth401AttributionCallback, BEARER_SUFFIX_LEN, SharedAttributionCallback, ToolConsumer,
};
pub use implementations::echo_agent_build::is_task_tool_id;
pub use implementations::{SEARCH_TOOL_NAME, USE_TOOL_NAME};
