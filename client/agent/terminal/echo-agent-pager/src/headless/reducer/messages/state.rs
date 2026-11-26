//! The `streaming-messages-json` reducer state: the per-response phase state
//! machine, partial-framing state, terminal metadata buffer, and session facts.

use crate::headless::reducer::McpServer;

use super::wire::MessageUsage;

/// Metadata from the latest `ResponseCompleted`, cleared by the next `flush_assistant`.
#[derive(Default)]
pub(super) struct PendingResponse {
    pub(super) message_id: Option<String>,
    pub(super) stop_reason: Option<String>,
    pub(super) usage: Option<MessageUsage>,
    pub(super) signature: Option<String>,
    /// Provider's matched stop sequence; set only when `stop_reason == "stop_sequence"`.
    pub(super) stop_sequence: Option<String>,
}

/// The real per-response identity from `ResponseStarted`: `message.id`, `model`,
/// and input-side usage. Retained (cloned, never moved) so both the partial
/// `message_start` and the final frame recover the same id/model/usage.
#[derive(Clone, Default)]
pub(super) struct ResponseIdentity {
    pub(super) message_id: Option<String>,
    pub(super) model: Option<String>,
    pub(super) input_tokens: u64,
    pub(super) cache_read_input_tokens: u64,
    pub(super) cache_creation_input_tokens: u64,
}

impl ResponseIdentity {
    /// The input-side `message.usage` this identity seeds (`output_tokens` stays 0).
    pub(super) fn input_usage(&self) -> MessageUsage {
        MessageUsage {
            input_tokens: self.input_tokens,
            cache_read_input_tokens: self.cache_read_input_tokens,
            cache_creation_input_tokens: self.cache_creation_input_tokens,
            ..MessageUsage::default()
        }
    }
}

/// The open block within a partial `message_start` envelope: its wire `index` and kind.
#[derive(Clone, Copy)]
pub(super) struct OpenBlock {
    pub(super) index: usize,
    pub(super) kind: TextKind,
}

/// Typed `--include-partial-messages` framing state; an enum makes "block open with no message" unrepresentable.
pub(super) enum PartialFraming {
    /// No partial `message_start` envelope is open.
    Idle,
    /// A `message_start` envelope is open; `block` is the open content block, if any.
    MessageOpen { block: Option<OpenBlock> },
}

impl PartialFraming {
    pub(super) fn message_open(&self) -> bool {
        matches!(self, PartialFraming::MessageOpen { .. })
    }

    pub(super) fn open_block(&self) -> Option<OpenBlock> {
        match self {
            PartialFraming::MessageOpen { block } => *block,
            PartialFraming::Idle => None,
        }
    }
}

/// The lifecycle phase of the current model response. The retained identity and
/// pending metadata are dropped only when the response flushes, so one response's
/// id, usage, and signature cannot leak onto the next.
#[derive(Default)]
pub(super) enum ResponseState {
    /// No response is open.
    #[default]
    Idle,
    /// A `ResponseStarted` opened this response; its identity is retained until flush.
    Started(ResponseIdentity),
    /// A `ResponseCompleted` closed this response; awaiting flush. Retains the
    /// identity and whether a `ResponseStarted` opened it.
    Completed {
        identity: ResponseIdentity,
        pending: PendingResponse,
        started: bool,
    },
}

impl ResponseState {
    /// This response's retained identity (default when none was surfaced).
    pub(super) fn identity(&self) -> ResponseIdentity {
        match self {
            ResponseState::Idle => ResponseIdentity::default(),
            ResponseState::Started(identity) | ResponseState::Completed { identity, .. } => {
                identity.clone()
            }
        }
    }

    /// Whether a `ResponseStarted` opened the current response.
    pub(super) fn started(&self) -> bool {
        match self {
            ResponseState::Idle => false,
            ResponseState::Started(_) => true,
            ResponseState::Completed { started, .. } => *started,
        }
    }

    /// Whether a `ResponseCompleted` has closed but not yet flushed the response.
    pub(super) fn is_completed(&self) -> bool {
        matches!(self, ResponseState::Completed { .. })
    }

    /// Whether a `ResponseStarted` opened this response and it hasn't completed.
    pub(super) fn is_started(&self) -> bool {
        matches!(self, ResponseState::Started(_))
    }

    /// Whether a response is open at all (not `Idle`).
    pub(super) fn is_active(&self) -> bool {
        !matches!(self, ResponseState::Idle)
    }

    /// The terminal metadata awaiting flush, if the response completed.
    pub(super) fn pending(&self) -> Option<&PendingResponse> {
        match self {
            ResponseState::Completed { pending, .. } => Some(pending),
            _ => None,
        }
    }

    /// Open the response with a real identity from `ResponseStarted`. A well-formed
    /// stream always transitions from `Idle`; the debug assertion catches a skipped flush.
    pub(super) fn open(&mut self, identity: ResponseIdentity) {
        debug_assert!(
            matches!(self, ResponseState::Idle),
            "ResponseState::open called on a non-Idle response; the coordinator \
             must flush the prior response first"
        );
        *self = ResponseState::Started(identity);
    }

    /// Record the terminal `ResponseCompleted`, retaining the identity and `started` marker.
    pub(super) fn complete(&mut self, pending: PendingResponse) {
        *self = ResponseState::Completed {
            identity: self.identity(),
            started: self.started(),
            pending,
        };
    }

    /// Take the terminal metadata and reset to `Idle`, dropping the retained identity.
    pub(super) fn take_pending(&mut self) -> PendingResponse {
        match std::mem::take(self) {
            ResponseState::Completed { pending, .. } => pending,
            _ => PendingResponse::default(),
        }
    }

    /// Drop all per-response state so nothing leaks onto the next response.
    pub(super) fn reset(&mut self) {
        *self = ResponseState::Idle;
    }
}

/// The session facts captured at `MessagesReducer::begin`; `model` is `Option`
/// because a backend may not surface it until the first `ResponseStarted`.
pub(super) struct SessionState {
    pub(super) session_id: String,
    pub(super) model: Option<String>,
    pub(super) cwd: String,
    pub(super) permission_mode: Option<String>,
    /// True when the session authenticated with an API key (vs OAuth).
    pub(super) api_key_auth: bool,
    pub(super) mcp_servers: Vec<McpServer>,
    pub(super) include_partials: bool,
    /// The current model's total context window in tokens, when known.
    pub(super) context_window: Option<u64>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum TextKind {
    Text,
    Thinking,
}
