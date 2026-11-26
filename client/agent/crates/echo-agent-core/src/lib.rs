//! Agent builder, definition parsing, and system prompt assembly.
//!
//! This crate extracts a first-class `Agent` type from `echo-agent-runtime`.
//! An `Agent` bundles tools, system prompt, system-reminder policy,
//! compaction policy, and model configuration into a single, portable
//! object that any host can consume.

pub mod agent;
pub mod builder;
pub mod compaction;
pub mod config;
pub mod discovery;
pub mod error;
pub mod plugins;
pub mod prompt;
pub mod repo;
pub mod system_reminder;
pub mod timing;

pub use agent::Agent;
pub use builder::AgentBuilder;
pub use compaction::CompactionPolicy;
pub use config::AgentDefinition;
pub use config::preset_names;
pub use config::toolset_for_preset;
pub use config::workspace_echo_agent_build_toolset;
pub use error::AgentBuildError;
pub use prompt::context::{DEFAULT_SYSTEM_PROMPT_LABEL, PromptContext};
pub use system_reminder::ReminderPolicy;
