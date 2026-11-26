//! `EchoAgentBuildConcise` namespace — concise variants of core EchoAgentBuild tools.
//!
//! These tools share implementation with `echo_agent_build` via `pub(crate)` helpers
//! but produce concise output (compact line numbers, shorter messages,
//! concise bash formatting).

pub mod bash;
pub mod read_file;
pub mod search_replace;

pub use bash::BashConciseTool;
pub use read_file::ReadFileConciseTool;
pub use search_replace::SearchReplaceConciseTool;
