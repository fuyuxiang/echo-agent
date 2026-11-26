//! Internal actor protocol.
//!
//! `SamplerCommand` is `pub(crate)` because it is the wire between
//! [`SamplerHandle`](crate::handle::SamplerHandle) and the actor task,
//! not a public type. External callers always go through `SamplerHandle`.

use tokio::sync::oneshot;

use echo_agent_sampling_types::{ConversationRequest, ConversationResponse, SamplingError};

use crate::config::SamplerConfig;
use crate::metrics::InferenceLatencyStats;
use crate::types::RequestId;

/// Commands sent from a [`SamplerHandle`](crate::handle::SamplerHandle)
/// to the actor task.
///
/// Large payloads (`ConversationRequest`, `SamplerConfig`) are boxed so
/// every command stays cheap to copy through the mpsc channel.
pub(crate) enum SamplerCommand {
    /// Submit a new sampling request. Fire-and-forget — results come via
    /// events. When `completion_tx` is set the per-request task also
    /// signals that channel for `submit_and_collect` callers.
    Submit {
        request_id: RequestId,
        request: Box<ConversationRequest>,
        config: Option<Box<SamplerConfig>>,
        completion_tx: Option<
            oneshot::Sender<Result<(ConversationResponse, InferenceLatencyStats), SamplingError>>,
        >,
    },

    /// Cancel an in-flight request.
    Cancel { request_id: RequestId },

    /// Update the default sampling config (model switch, auth refresh).
    UpdateConfig { config: Box<SamplerConfig> },

    /// Query: is a specific request still in flight?
    IsActive {
        request_id: RequestId,
        reply: oneshot::Sender<bool>,
    },

    /// Query: how many requests are in flight?
    ActiveCount { reply: oneshot::Sender<usize> },
}
