use std::path::PathBuf;

/// Errors that can occur during hook loading, parsing, or execution.
#[derive(Debug, thiserror::Error)]
pub enum HookError {
    #[error("failed to read hook file {path}: {source}")]
    ReadFile {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("failed to parse hook file {path}: {detail}")]
    ParseFile { path: PathBuf, detail: String },

    #[error("hook {name} in {path}: invalid regex pattern: {source}")]
    InvalidMatcher {
        name: String,
        path: PathBuf,
        source: regex::Error,
    },

    #[error("hook {name} timed out after {elapsed_ms}ms")]
    Timeout { name: String, elapsed_ms: u64 },

    #[error("hook {name} command failed: {source}")]
    CommandFailed {
        name: String,
        source: std::io::Error,
    },

    #[error("hook {name} produced invalid output: {detail}")]
    InvalidOutput { name: String, detail: String },

    #[error("hook {name}: command not found or not executable: {path}")]
    CommandNotFound { name: String, path: PathBuf },

    #[error("hook {name} in {path}: {detail}")]
    InvalidConfig {
        name: String,
        path: PathBuf,
        detail: String,
    },

    #[error(
        "hook {name} in {path}: unsupported handler type '{handler_type}', expected 'command' or 'http'"
    )]
    UnsupportedHandlerType {
        name: String,
        path: PathBuf,
        handler_type: String,
    },
}
