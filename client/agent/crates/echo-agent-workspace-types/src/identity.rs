//! Identifier types for sessions, tool calls, and hunks.
//!
//! Each identifier is a newtype wrapper around `String`. Strings are used
//! (rather than `Uuid` directly) so callers can pick whatever id scheme
//! they like (UUIDs, ULIDs, slugs, ...) and so the wire format stays
//! human-readable.
//!
//! The inner field is **not** public: callers must construct ids via
//! `new()`, `From<String>`, or `From<&str>`, and read them back via
//! `as_str()` or `Display`. Keeping the inner field private prevents
//! callers from poking arbitrary strings into the newtype and bypassing
//! whatever invariants we add later (e.g. non-empty, ASCII-only, ...).

use serde::{Deserialize, Serialize};
use std::fmt;

/// Unique session identifier. Used to scope per-session operations.
#[derive(Debug, Clone, Default, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(pub(crate) String);

impl SessionId {
    /// Construct a new session id from any string-like value.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Borrow the wrapped string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for SessionId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for SessionId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

/// Unique tool call identifier within a session.
#[derive(Debug, Clone, Default, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ToolCallId(pub(crate) String);

impl ToolCallId {
    /// Construct a new tool call id from any string-like value.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Borrow the wrapped string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ToolCallId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for ToolCallId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for ToolCallId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

/// Unique hunk identifier produced by the hunk tracker.
///
/// TODO(workspace): align with `echo_agent_hunk_tracker::HunkId` (currently
/// `pub struct HunkId(pub Arc<str>)`) when the tracker's wire surface
/// gets extracted into this crate.
#[derive(Debug, Clone, Default, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HunkId(pub(crate) String);

impl HunkId {
    /// Construct a new hunk id from any string-like value.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Borrow the wrapped string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for HunkId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for HunkId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for HunkId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_serializes_transparently() {
        let id = SessionId::new("sess-123");
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"sess-123\"");
        let back: SessionId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);
    }

    #[test]
    fn tool_call_id_serializes_transparently() {
        let id = ToolCallId::new("call-abc");
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"call-abc\"");
        let back: ToolCallId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);
    }

    #[test]
    fn hunk_id_serializes_transparently() {
        let id = HunkId::new("hunk-xyz");
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"hunk-xyz\"");
        let back: HunkId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);
    }

    #[test]
    fn ids_implement_display() {
        assert_eq!(SessionId::new("a").to_string(), "a");
        assert_eq!(ToolCallId::new("b").to_string(), "b");
        assert_eq!(HunkId::new("c").to_string(), "c");
    }
}
