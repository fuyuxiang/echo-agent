//! Session-lifecycle RPC requests.

use serde::{Deserialize, Serialize};

use crate::identity::SessionId;
use crate::types::AgentSessionConfig;

/// Top-level session-lifecycle RPC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum SessionLifecycleRequest {
    /// Fork a new session. Response: `SessionChunk::SessionId`.
    Fork(AgentSessionConfig),
    /// Destroy a session. Response: `SessionChunk::Ack`.
    Destroy(SessionId),
    /// List all sessions. Streams `SessionChunk::SessionInfo` (one per
    /// session).
    List,
    /// Apply a (sub)session's worktree back into the parent.
    /// Response: `SessionChunk::Ack`.
    ApplyWorktree(SessionId),
    /// Mark the start of a prompt. Response: `SessionChunk::Ack`.
    BeginPrompt {
        /// Session id.
        session: SessionId,
        /// Monotonically increasing prompt index.
        ///
        /// `u64` (not `usize`) for wire stability: `usize` is
        /// host-dependent and would arbitrarily codegen to `uint64`.
        idx: u64,
    },
    /// Mark the end of a prompt. Response: `SessionChunk::Ack`.
    EndPrompt {
        /// Session id.
        session: SessionId,
        /// Prompt index that just finished.
        idx: u64,
    },
    /// Rewind a session to a target prompt index. Response:
    /// `SessionChunk::RewindResult`.
    Rewind {
        /// Session id.
        session: SessionId,
        /// Target prompt index (0 = beginning).
        target: u64,
    },
    /// Enumerate the available rewind points for a session. Response:
    /// `SessionChunk::RewindPoints`.
    GetRewindPoints(SessionId),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples() -> Vec<SessionLifecycleRequest> {
        vec![
            SessionLifecycleRequest::Fork(AgentSessionConfig::default()),
            SessionLifecycleRequest::Destroy(SessionId::new("s1")),
            SessionLifecycleRequest::List,
            SessionLifecycleRequest::ApplyWorktree(SessionId::new("s1")),
            SessionLifecycleRequest::BeginPrompt {
                session: SessionId::new("s1"),
                idx: 0,
            },
            SessionLifecycleRequest::EndPrompt {
                session: SessionId::new("s1"),
                idx: 0,
            },
            SessionLifecycleRequest::Rewind {
                session: SessionId::new("s1"),
                target: 3_u64,
            },
            SessionLifecycleRequest::GetRewindPoints(SessionId::new("s1")),
        ]
    }

    #[test]
    fn every_variant_round_trips() {
        for req in samples() {
            let json = serde_json::to_string(&req).unwrap();
            let back: SessionLifecycleRequest = serde_json::from_str(&json).unwrap();
            assert_eq!(req, back);
        }
    }
}
