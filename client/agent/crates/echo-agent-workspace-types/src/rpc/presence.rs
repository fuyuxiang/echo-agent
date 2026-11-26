//! Client-presence attestation (`workspace.presence.note`).

use serde::{Deserialize, Serialize};

use super::{RpcActivityClass, WorkspaceRpc};

/// Canonical `ClientPresence` idle-withhold window. The guest tracker uses it
/// as its default and the gateway's refresh cadence must stay well inside it
/// (pinned by a gateway test).
pub const PRESENCE_ACTIVITY_WINDOW_MS: u64 = 90_000;

/// Note client presence for the session. Fire-and-forget: the response is
/// empty. A visible note stamps the guest's `ClientPresence` idle withhold; a
/// hidden note deliberately stamps nothing, so the existing window expires on
/// its own — a hide must never make the sandbox hibernate sooner than
/// silence would.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresenceNoteReq {
    pub session_id: String,
    pub visible: bool,
    /// Strictly increasing per conversation. The guest drops a note whose
    /// `seq` is not newer than the last applied one, so a slow superseded
    /// visible note cannot re-arm a withhold after a newer hidden note.
    /// Absent (old gateway) is treated as always-newest.
    #[serde(default)]
    pub seq: Option<u64>,
}

impl WorkspaceRpc for PresenceNoteReq {
    const METHOD: &'static str = "workspace.presence.note";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = ();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_constant() {
        assert_eq!("workspace.presence.note", PresenceNoteReq::METHOD);
        assert_eq!(RpcActivityClass::Read, PresenceNoteReq::ACTIVITY);
    }

    /// A note from an old gateway (no `seq`) still parses.
    #[test]
    fn seq_is_optional_on_the_wire() {
        let req: PresenceNoteReq =
            serde_json::from_str(r#"{"session_id":"s","visible":true}"#).unwrap();
        assert_eq!(None, req.seq);
    }
}
