//! Error types for agent construction.

/// Errors that can occur during Agent construction.
#[derive(Debug, thiserror::Error)]
pub enum AgentBuildError {
    /// Failed to parse the agent definition file (bad YAML frontmatter,
    /// missing closing `---`, or invalid Markdown structure).
    #[error("failed to parse agent definition: {0}")]
    ParseError(String),

    /// Required fields are missing from the definition (name, description).
    #[error("missing required field in agent definition: {0}")]
    MissingField(String),

    /// A tool name override references a tool that doesn't exist in the
    /// registry (typo in the definition's `toolNameOverrides`).
    #[error("tool name override references nonexistent tool '{0}'")]
    UnknownToolOverride(String),

    /// IO error during AGENTS.md or skills discovery.
    #[error("IO error during agent construction: {0}")]
    IoError(#[from] std::io::Error),

    /// Failed to build the session's tokio runtime (fd exhaustion: the
    /// runtime needs epoll/kqueue + waker fds).
    #[error("failed to build session runtime: {0}")]
    RuntimeBuild(std::io::Error),

    /// MiniJinja template rendering failed (extend or full mode).
    /// Includes line numbers and context from the template.
    #[error("template rendering error: {0}")]
    MiniJinjaError(#[from] minijinja::Error),

    /// Tool registry error (e.g., unsatisfied requirements during finalization).
    #[error("tool error: {0}")]
    ToolError(String),

    /// A configuration value is present but invalid (e.g. `max_turns = 0`).
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
}
