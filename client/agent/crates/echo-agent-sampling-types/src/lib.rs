//! Pure data types for the EchoAgent sampling / chat-completion API layer.
//!
//! This crate contains the API-agnostic conversation types, chat completion
//! request/response types, streaming types, and error types used across the
//! EchoAgent agent stack.  It intentionally contains **no I/O** (no HTTP clients,
//! no file system access) so it can be depended on by downstream crates
//! (e.g., `echo-agent-chat-state`) without pulling in the full `echo-agent-runtime`.

pub mod conversation;
pub mod doom_loop;
pub mod error;
pub mod messages;
pub mod provider_error;
pub mod serde_helpers;
pub mod tool_overrides;
pub mod types;

pub use self::conversation::*;
pub use self::doom_loop::{
    DOOM_LOOP_CHECK_EVENT_TYPE, DOOM_LOOP_CHECK_HEADER, DoomLoopPeek, DoomLoopRecoveryPolicy,
    DoomLoopSignal, DoomLoopSignalKind, is_check_event, peek_doom_loop,
};
pub use self::error::{
    ApiErrorCode, EmptyReason, EmptyResponseContext, INVALID_IMAGE_ERROR_CODE,
    ResponseModelMetadata, Result, SamplingError, SentCredential, is_context_length_error,
    is_retryable_api_status, parse_error_code, status_user_message, user_facing_api_error_message,
};
pub use self::tool_overrides::{
    ClearableField, MAX_WEB_SEARCH_DOMAINS, SearchDateBound, SearchDateBoundError, ToolOverrides,
    ToolOverridesUpdate, WebSearchOptions, WebSearchOptionsError, XSearchOptions,
};
pub use self::types::*;

// Re-export async-openai crate Responses API types under `rs` namespace
pub use async_openai::types::responses as rs;
