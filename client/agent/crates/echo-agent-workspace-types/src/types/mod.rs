//! Supporting structs/enums referenced from requests, chunks, and events.
//!
//! Every type in this module is a **placeholder**: the canonical
//! implementations live in other crates today (`echo-agent-hunk-tracker`,
//! `echo-agent-runtime`, `echo-agent-tools`, ...). We define minimal
//! serializable shapes here so the wire-types crate's API surface
//! compiles end-to-end. Each type carries a
//! `// TODO(workspace): align with <canonical type>` comment naming the
//! crate it should eventually be reconciled against.
//!
//! Until those subsystems are extracted, the only contract these types
//! must satisfy is that they're `Debug + Clone + Serialize + Deserialize`
//! and that the field shapes are sensible for the wire-format walkthrough.

pub mod config;
pub mod files;
pub mod git;
pub mod hunk;
pub mod interaction;
pub mod memory;
pub mod permission;
pub mod plan_mode;
pub mod plugins;
pub mod search;
pub mod session;
pub mod skills;
pub mod tools;

pub use config::{
    AgentSessionConfig, CapabilityMode, IsolationMode, PermissionPolicy, ProjectConfig,
    ToolServerConfig,
};
pub use files::{FileReference, ResolvedFile};
pub use git::{
    GitBranchInfo, GitDiff, GitDiffArgs, GitMetadata, GitStatus, GitStatusOpts, VcsKind,
};
pub use hunk::{Hunk, HunkAction};
pub use interaction::{UserAnswer, UserQuestion, UserQuestionOption};
pub use memory::MemoryChunk;
pub use permission::{PermissionDecision, PermissionRequest};
pub use plan_mode::{PlanModeDecision, PlanModeTransition};
pub use plugins::{HookInfo, PluginInfo};
pub use search::{ContentMatch, FuzzyMatch, FuzzySearchArgs, RipgrepArgs, RipgrepStats};
pub use session::{
    AgentSessionInfo, FsEventKind, LspServerStatus, McpServerStatus, RewindPoint, RewindResult,
};
pub use skills::SkillInfo;
pub use tools::{ToolCallResult, ToolDef, ToolOutputChunk, ToolProgress};
