use crate::implementations::echo_agent_build::bash::BashError;
use crate::implementations::echo_agent_build::todo::TodoError;
use crate::implementations::echo_agent_build::web_fetch::WebFetchError;

/// Error type for SearchReplace tool operations.
#[derive(thiserror::Error, Debug)]
pub enum SearchReplaceError {
    #[error("File not found: {0}")]
    FileNotFound(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Invalid UTF-8 in file: {0}")]
    InvalidUtf8(String),
}

//
// These impls let tool code use `?` directly on domain errors without going
// through any intermediary enum.

/// Generate `impl From<$err> for echo_agent_tool_runtime::ToolError` that wraps the
/// error as an execution failure tagged with the given tool ID.
macro_rules! impl_runtime_error_from {
    ($($err:ty => $tool_id:literal),+ $(,)?) => {
        $(
            impl From<$err> for echo_agent_tool_runtime::ToolError {
                fn from(err: $err) -> Self {
                    echo_agent_tool_runtime::ToolError::execution(
                        echo_agent_tool_protocol::ToolId::new($tool_id).expect("valid static tool id"),
                        err.to_string(),
                    )
                }
            }
        )+
    };
}

impl_runtime_error_from! {
    BashError          => "run_terminal_cmd",
    TodoError          => "todo_write",
    WebFetchError      => "web_fetch",
    SearchReplaceError => "search_replace",
}
