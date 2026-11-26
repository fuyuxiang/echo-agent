// Glob re-exports keep the flat `crate::util::config::*` paths that in-crate and cross-crate callers rely on.

mod auto_mode;
mod compaction;
mod crash_handler;
mod display_refresh;
mod features;
mod mcp;
mod system_prompt;
mod tool_approvals;
mod toolset;
mod ui;
mod version;

pub use auto_mode::*;
pub use compaction::*;
pub use crash_handler::*;
pub use display_refresh::*;
pub use features::*;
pub use mcp::*;
pub use system_prompt::*;
pub use tool_approvals::*;
pub use toolset::*;
pub use ui::*;
pub use version::*;

// Single crate-wide env-mutation mutex; `permissions.rs` tests name it via this module's path.
#[cfg(test)]
pub(crate) use auto_mode::AUTO_PERMISSION_MODE_ENV_LOCK;
