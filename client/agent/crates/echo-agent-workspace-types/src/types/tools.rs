//! Tool-related shapes referenced from `ToolChunk`.
//!
//! TODO(workspace): align with the canonical tool types in
//! `echo-agent-tools` (`ToolDef`, `ToolCallResult`, `ToolProgress`).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::identity::ToolCallId;

/// One incremental tool output frame (e.g. bash stdout).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolOutputChunk {
    /// Tool call this output belongs to.
    pub call_id: ToolCallId,
    /// Output stream identifier (`"stdout"` or `"stderr"` are the
    /// most common values).
    #[serde(default)]
    pub stream: String,
    /// Raw bytes from the tool. Encoded as a standard (RFC 4648)
    /// base64 string in JSON via the `bytes_as_base64` module --
    /// serde's default JSON representation of `Vec<u8>` would be a
    /// JSON array of integers, which is wasteful for byte streams.
    /// In binary serializers (postcard, bincode) the underlying bytes
    /// go over as length-prefixed bytes.
    #[serde(default, with = "bytes_as_base64")]
    pub bytes: Vec<u8>,
    /// Wall-clock timestamp the chunk was emitted (UTC).
    ///
    /// Default on missing field is the Unix epoch (`DateTime::default()`)
    /// rather than `Utc::now()`: we deliberately want a deterministic,
    /// distinguishable sentinel rather than the receiver's wall clock
    /// pretending to be the originator's.
    #[serde(default)]
    pub at: DateTime<Utc>,
}

/// Lifecycle / progress event emitted by a tool.
///
/// Holds an `f32` `fraction` field on the `Percent` variant, so the
/// enum cannot derive `Eq` (only `PartialEq`).
///
/// Tagged with `tag = "type", content = "data"` (adjacent tagging) to
/// match every other wire enum in the crate. See `crate::lib`
/// doc-comment "# Wire format" for the rationale -- adjacent tagging is
/// the only form that works uniformly across struct, newtype, and unit
/// variants. Important: `ToolProgress` is itself nested inside
/// `ToolChunk::Progress(ToolProgress)`, so using a uniform adjacent
/// shape keeps the rendered JSON consistent across the whole tree (no
/// mix of adjacent + internal tagging in a single document).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ToolProgress {
    /// Tool started (after permission was granted, before execution).
    Started {
        /// Tool call this progress is for.
        call_id: ToolCallId,
    },
    /// Free-form status string (e.g. `"installing dependencies"`).
    Status {
        /// Tool call this progress is for.
        call_id: ToolCallId,
        /// Status message.
        message: String,
    },
    /// Quantitative progress (used for downloads / installs).
    Percent {
        /// Tool call this progress is for.
        call_id: ToolCallId,
        /// Completed fraction in `[0.0, 1.0]`.
        fraction: f32,
    },
}

/// Terminal result emitted as exactly one `ToolChunk::Final` per call.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallResult {
    /// Tool call this result belongs to.
    pub call_id: ToolCallId,
    /// Process / tool exit code (0 = success).
    #[serde(default)]
    pub exit_code: i32,
    /// Optional human-readable summary.
    #[serde(default)]
    pub summary: String,
    /// Optional JSON-encoded structured result (tool-defined).
    #[serde(default)]
    pub output_json: String,
    /// Whether the call was cancelled (rather than finishing naturally).
    #[serde(default)]
    pub cancelled: bool,
}

/// Tool definition surfaced via `ToolChunk::Definitions`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDef {
    /// Stable tool name (e.g. `"read_file"`).
    pub name: String,
    /// Human-readable description (often shown in tool listings).
    #[serde(default)]
    pub description: String,
    /// JSON Schema for the tool's input arguments.
    #[serde(default)]
    pub input_schema_json: String,
    /// Whether invocations require explicit user permission.
    #[serde(default)]
    pub requires_permission: bool,
}

/// Module-private base64 codec for `ToolOutputChunk::bytes`.
///
/// Uses the `base64` crate's standard (RFC 4648) engine. The raw
/// `Vec<u8>` field would otherwise serialize as a JSON array of
/// integers, which is wasteful for byte streams.
mod bytes_as_base64 {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], ser: S) -> Result<S::Ok, S::Error> {
        STANDARD.encode(bytes).serialize(ser)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(de)?;
        STANDARD.decode(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_output_bytes_round_trip_through_base64() {
        for payload in [
            b"".to_vec(),
            b"a".to_vec(),
            b"ab".to_vec(),
            b"abc".to_vec(),
            b"abcd".to_vec(),
            (0u8..=255).collect(),
        ] {
            let chunk = ToolOutputChunk {
                call_id: ToolCallId::new("call-1"),
                stream: "stdout".into(),
                bytes: payload.clone(),
                at: chrono::Utc::now(),
            };
            let json = serde_json::to_string(&chunk).unwrap();
            let back: ToolOutputChunk = serde_json::from_str(&json).unwrap();
            assert_eq!(back.bytes, payload, "round-trip failed for {payload:?}");
        }
    }

    #[test]
    fn tool_output_chunk_uses_snake_case_field_names() {
        let chunk = ToolOutputChunk {
            call_id: ToolCallId::new("c1"),
            stream: "stdout".into(),
            bytes: b"hi".to_vec(),
            at: DateTime::<Utc>::default(),
        };
        let json = serde_json::to_string(&chunk).unwrap();
        // snake_case field names (matches gRPC field convention).
        assert!(json.contains("\"call_id\""), "got {json}");
        assert!(!json.contains("\"callId\""), "got {json}");
    }

    #[test]
    fn tool_output_chunk_at_defaults_to_epoch() {
        // Omitted `at` field should deserialize to `DateTime::default()`,
        // not `Utc::now()`.
        let json = r#"{"call_id":"c1","stream":"stdout","bytes":""}"#;
        let chunk: ToolOutputChunk = serde_json::from_str(json).unwrap();
        assert_eq!(chunk.at, DateTime::<Utc>::default());
    }
}
