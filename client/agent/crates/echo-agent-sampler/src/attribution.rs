//! 401 attribution hook for the sampling client.
//!
//! The caller wires an [`Auth401AttributionCallback`] into
//! [`crate::SamplerConfig::attribution_callback`]; the sampler invokes it at
//! each UNAUTHORIZED arm with the bearer fragment that went on the wire, so an
//! observer can split "sent a stale snapshot" from "sent the live token and
//! was still rejected". `None` (the default) makes the 401 sites silent.
//!
//! This crate stays decoupled from `echo-agent-runtime`: no shell types, no
//! auth-manager dependency.

use std::sync::Arc;

pub use echo_agent_auth::bearer_fragment::BEARER_SUFFIX_LEN;

/// A 401-emitting site in [`crate::SamplingClient`]; its string identifier
/// becomes the `consumer` field so queries can break 401s down by API path.
/// Sampler endpoints only — tool clients use `echo_agent_tools::ToolConsumer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SamplingConsumer {
    /// `chat_completion_stream`: OpenAI-compatible streaming OpenAI Chat Completions API.
    ChatCompletionsStream,
    /// `chat_completion`: OpenAI-compatible non-streaming OpenAI Chat Completions API.
    ChatCompletions,
    /// `create_response_stream`: Responses API streaming.
    ResponsesStream,
    /// `create_response`: Responses API non-streaming.
    Responses,
    /// `messages_stream`: Anthropic Messages API streaming.
    MessagesStream,
    /// `messages`: Anthropic Messages API non-streaming.
    Messages,
}

impl SamplingConsumer {
    /// Stable string identifier for this emit site. Callbacks
    /// typically combine this with a fixed prefix (e.g. the client
    /// type) when building the consumer field of the attribution
    /// event.
    pub fn as_endpoint(self) -> &'static str {
        match self {
            Self::ChatCompletionsStream => "chat_completions_stream",
            Self::ChatCompletions => "chat_completions",
            Self::ResponsesStream => "responses_stream",
            Self::Responses => "responses",
            Self::MessagesStream => "messages_stream",
            Self::Messages => "messages",
        }
    }
}

/// Hook invoked by [`crate::SamplingClient`] at every 401 response site.
///
/// Must be cheap and non-blocking — it runs on the user-visible 401 error path.
///
/// The `Debug` bound is structural: [`crate::SamplerConfig`] derives `Debug`
/// and holds an `Option<Arc<dyn Auth401AttributionCallback>>`. Do not remove it.
pub trait Auth401AttributionCallback: Send + Sync + std::fmt::Debug {
    /// `sent_bearer_suffix` is the [`BEARER_SUFFIX_LEN`]-char tail of the
    /// bearer sent on the wire, truncated before crossing this boundary so the
    /// full credential never leaves [`crate::SamplingClient`]. `None` means no
    /// bearer header was sent at all.
    fn record_401(&self, consumer: SamplingConsumer, sent_bearer_suffix: Option<&str>);
}

/// Shared, cheap-to-clone alias for the attribution callback.
pub type SharedAttributionCallback = Arc<dyn Auth401AttributionCallback>;
