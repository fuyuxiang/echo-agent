//! echo-agent-sampler - Actor-based sampling layer for EchoAgent echoagent.
//!
//! This crate extracts the HTTP streaming + retry logic out of
//! `echo-agent-runtime`'s session actor into a standalone, reusable
//! component built on the same actor pattern as `echo-agent-hunk-tracker`.
//!
//! ## Layered API
//!
//! - **Layer 1**: [`client::SamplingClient`] returns raw chunk streams.
//! - **Layer 2**: [`stream`] transforms raw streams into [`SamplingEvent`]s.
//! - **Layer 3**: [`SamplerHandle`] manages concurrent requests with retry,
//!   cancellation, and event-based coordination via the actor.
//!
//! The type skeleton, the pure retry / metrics / client logic, the
//! Layer-2 stream transforms ([`stream_chat_completions`],
//! [`stream_responses`], [`stream_messages`], [`collect_response`]),
//! and the actor with its per-request task tie these layers together.

pub mod actor;
pub mod attribution;
pub mod client;
pub mod commands;
pub mod config;
pub mod doom_loop;
mod doom_loop_recovery;
pub mod events;
pub mod handle;
pub mod metrics;
pub mod retry;
pub mod sampling_log;
mod shared_http;
pub mod stream;
pub mod types;

// Public re-exports — the API surface consumers see.
pub use actor::SamplerActor;
pub use actor::request_task::CompletionResult;
pub use attribution::{
    Auth401AttributionCallback, BEARER_SUFFIX_LEN, SamplingConsumer, SharedAttributionCallback,
};
pub use client::{
    ApiBackend, SamplingClient, replace_approved_insecure_http_urls, user_agent_string_for,
};
pub use config::{
    AuthScheme, BearerResolver, HeaderInjector, OriginClientInfo, RetryPolicy, SamplerConfig,
    SharedBearerResolver, SharedHeaderInjector,
};
pub use doom_loop::DoomLoopSignalCollector;
pub use events::{
    SamplingChannel, SamplingErrorInfo, SamplingErrorKind, SamplingEvent, StripReason,
};
pub use handle::SamplerHandle;
pub use metrics::{InferenceLatencyStats, compute_percentiles};
pub use retry::{
    DEFAULT_MAX_RETRIES, MAX_RETRY_BACKOFF, RATE_LIMIT_RETRY_DISABLED, RATE_LIMIT_RETRY_THRESHOLD,
    RetryDecision, classify_error, format_sampling_error, jitter_backoff, resolve_max_retries,
    retry_after_or_backoff, retry_backoff_with_jitter,
};
pub use sampling_log::AuthInfo;
pub use stream::{collect_response, stream_chat_completions, stream_messages, stream_responses};
pub use types::RequestId;
