//! Serializable registry-level errors.
//!
//! Variants here are for failures that occur **inside** the registry —
//! mismatched session, server-id collisions, optimistic-concurrency stale
//! generation. Wire-level transport errors (`tool_not_found`, etc.) live
//! in [`crate::ToolErrorWire`].

use serde::{Deserialize, Serialize};

use crate::{ServerId, SessionId, ToolId};

#[derive(thiserror::Error, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum RegistryError {
    /// A different connection already owns this `(session, tool)`.
    #[serde(rename = "tool_already_registered")]
    #[error("tool already registered: {tool_id}")]
    AlreadyRegistered { tool_id: ToolId },

    /// The registration's session does not match the connection's bound
    /// session.
    #[error("session mismatch: token session={token_session}, registration session={reg_session}")]
    SessionMismatch {
        token_session: SessionId,
        reg_session: SessionId,
    },

    /// `server_id` collides with an active server in this session owned by
    /// a different connection. Fails the entire `register_*` batch with a
    /// top-level JSON-RPC error.
    #[error("server_id {server_id} collides with an active server in this session")]
    ServerIdCollision { server_id: ServerId },

    /// `server_id` is already in use on this connection by an earlier
    /// registration with a different tool set.
    #[error("server_id {server_id} already owned by an earlier registration on this connection")]
    ServerIdInUse { server_id: ServerId },

    /// Description failed structural validation (e.g. derived `tool_id`
    /// invalid, reserved prefix on a client-supplied `server_id`).
    #[error("invalid description: {message}")]
    InvalidDescription { message: String },

    /// `if_match_generation` precondition failed.
    #[error("stale generation: expected={expected}, actual={actual}")]
    StaleGeneration { expected: u64, actual: u64 },
}
