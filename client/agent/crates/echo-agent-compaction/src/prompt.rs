//! The shared compaction prompt seam.
//!
//! [`CompactionPrompt`] is the system+user prompt pair every orchestrator's
//! [`CompactionSampler`](crate::sampler::CompactionSampler) call takes. The
//! per-strategy prompt *content* lives with each subsystem:
//!
//! - steps prompt → [`crate::steps::format_compaction_prompt`]
//! - history prompts → [`crate::history::prompt`]
//! - echo-agent-build summary prompt → [`crate::code_compaction::build_summary_prompt`]

/// System + user prompt pair for the compaction LLM call.
#[derive(Debug, Clone)]
pub struct CompactionPrompt {
    pub system: String,
    pub user: String,
}
