//! Session-lifecycle chunks (`SessionChunk`).

use serde::{Deserialize, Serialize};

use crate::chunks::ChunkKind;
use crate::identity::SessionId;
use crate::types::{AgentSessionInfo, RewindPoint, RewindResult};

/// Streaming chunk for a session-lifecycle call.
///
/// Most variants are unary; `SessionInfo` is streamed by
/// `SessionLifecycleRequest::List` (one chunk per session).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum SessionChunk {
    /// Response to `SessionLifecycleRequest::Fork` -- the new session id.
    SessionId(SessionId),
    /// One session metadata snapshot, streamed by
    /// `SessionLifecycleRequest::List`.
    SessionInfo(AgentSessionInfo),
    /// Response to `SessionLifecycleRequest::Rewind`.
    RewindResult(RewindResult),
    /// Response to `SessionLifecycleRequest::GetRewindPoints`.
    RewindPoints(Vec<RewindPoint>),
    /// Acknowledgement for void session ops (`Destroy`,
    /// `ApplyWorktree`, `BeginPrompt`, `EndPrompt`).
    Ack,
}

impl SessionChunk {
    /// Discriminator for the current variant.
    pub fn kind(&self) -> ChunkKind {
        match self {
            Self::SessionId(_) => ChunkKind::SessionId,
            Self::SessionInfo(_) => ChunkKind::SessionInfo,
            Self::RewindResult(_) => ChunkKind::RewindResult,
            Self::RewindPoints(_) => ChunkKind::RewindPoints,
            Self::Ack => ChunkKind::SessionAck,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn samples() -> Vec<SessionChunk> {
        vec![
            SessionChunk::SessionId(SessionId::new("s1")),
            SessionChunk::SessionInfo(AgentSessionInfo::default()),
            SessionChunk::RewindResult(RewindResult::default()),
            SessionChunk::RewindPoints(vec![]),
            SessionChunk::Ack,
        ]
    }

    #[test]
    fn kind_discriminators_are_unique() {
        let kinds: HashSet<ChunkKind> = samples().iter().map(SessionChunk::kind).collect();
        assert_eq!(
            kinds.len(),
            samples().len(),
            "duplicate SessionChunk kind()"
        );
    }

    #[test]
    fn round_trips_through_json() {
        for chunk in samples() {
            let json = serde_json::to_string(&chunk).unwrap();
            let back: SessionChunk = serde_json::from_str(&json).unwrap();
            assert_eq!(chunk, back);
        }
    }
}
