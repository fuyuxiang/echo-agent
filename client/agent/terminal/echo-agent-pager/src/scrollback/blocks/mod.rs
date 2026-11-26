//! Block implementations for v3 pager.
//!
//! Each block type represents a different kind of content in the scrollback.

mod agent;
mod bg_task;
mod btw;
mod context_info;
mod credit_limit;
pub mod markdown_content;
pub mod mermaid_content;
mod quote_bar;
mod session_event;
mod subagent;
mod system;
mod thinking;
pub mod tool;
mod user;
mod workflow;

pub use agent::AgentMessageBlock;
pub use bg_task::{BgTaskBlock, BgTaskKind};
pub use btw::BtwBlock;
pub use context_info::ContextInfoBlock;
pub use credit_limit::{CreditLimitBlock, CreditLimitCardAction};
pub use session_event::{SessionEvent, SessionEventBlock};
pub use subagent::{SubagentBlock, SubagentBlockKind};
pub use system::SystemMessageBlock;
pub use thinking::ThinkingBlock;
pub use tool::{
    DiffLineOutput, DiffRenderConfig, DiscoveredTool, EditToolCallBlock, ExecuteToolCallBlock,
    IntegrationSearchToolCallBlock, LineRange, ListDirToolCallBlock, OtherToolCallBlock,
    ReadToolCallBlock, SearchFileMatch, SearchLineMatch, SearchToolCallBlock, ToolCallBlock,
    UseToolCallBlock, discovered_tool_action, render_diff_hunk_highlighted,
    render_diff_hunks_highlighted,
};
pub use user::UserPromptBlock;
pub use workflow::{WorkflowBlock, WorkflowBlockPhase, WorkflowBlockStatus};

// Backwards compatibility alias
pub type EditBlock = EditToolCallBlock;
