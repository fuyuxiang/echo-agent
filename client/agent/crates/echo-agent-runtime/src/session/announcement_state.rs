//! Persisted announcement tracking state for session resumption.
//!
//! Tracks which MCP servers and skills have already been announced
//! via `<system-reminder>` messages so that resumed sessions don't
//! re-inject duplicate listings.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Persisted announcement tracking state.
///
/// Restored on session resume so the fresh actor "remembers" what was
/// already announced.  The existing delta/fingerprint comparison logic
/// then correctly handles changes (new/removed/updated servers or skills)
/// without creating duplicates.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AnnouncementState {
    /// Fingerprints of MCP servers that have been announced.
    /// Maps server_name → `McpServerFingerprint`.
    ///
    /// The hash values use FNV-1a (deterministic, portable) so that
    /// persisted fingerprints remain valid across Rust versions, build
    /// profiles, and CPU architectures.
    pub mcp_server_fingerprints: HashMap<String, McpServerFingerprint>,

    /// Names of skills already announced via system-reminder.
    /// Uses the skill's `dedup_key()` (which is the skill name).
    pub announced_skill_names: HashSet<String>,
}

/// Serializable MCP server fingerprint for persistence.
///
/// This is the serializable counterpart of the in-memory
/// `ServerFingerprint` type alias `(usize, u64, u64)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerFingerprint {
    pub tool_count: usize,
    pub description_hash: u64,
    pub tool_names_hash: u64,
}

/// Convert from in-memory fingerprint map to persistable map.
pub(crate) fn to_persisted_fingerprints(
    in_memory: &HashMap<String, (usize, u64, u64)>,
) -> HashMap<String, McpServerFingerprint> {
    in_memory
        .iter()
        .map(|(name, &(tc, dh, tnh))| {
            (
                name.clone(),
                McpServerFingerprint {
                    tool_count: tc,
                    description_hash: dh,
                    tool_names_hash: tnh,
                },
            )
        })
        .collect()
}

/// Convert from persisted fingerprint map to in-memory map.
pub(crate) fn from_persisted_fingerprints(
    persisted: &HashMap<String, McpServerFingerprint>,
) -> HashMap<String, (usize, u64, u64)> {
    persisted
        .iter()
        .map(|(name, fp)| {
            (
                name.clone(),
                (fp.tool_count, fp.description_hash, fp.tool_names_hash),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_round_trip() {
        let state = AnnouncementState {
            mcp_server_fingerprints: HashMap::from([(
                "github".to_string(),
                McpServerFingerprint {
                    tool_count: 5,
                    description_hash: 12345678,
                    tool_names_hash: 87654321,
                },
            )]),
            announced_skill_names: HashSet::from(["commit".to_string(), "review".to_string()]),
        };
        let json = serde_json::to_string(&state).unwrap();
        let loaded: AnnouncementState = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.mcp_server_fingerprints.len(), 1);
        assert_eq!(loaded.announced_skill_names.len(), 2);
        let fp = &loaded.mcp_server_fingerprints["github"];
        assert_eq!(fp.tool_count, 5);
        assert_eq!(fp.description_hash, 12345678);
        assert_eq!(fp.tool_names_hash, 87654321);
    }

    #[test]
    fn backward_compat_empty_json() {
        let loaded: AnnouncementState = serde_json::from_str("{}").unwrap();
        assert!(loaded.mcp_server_fingerprints.is_empty());
        assert!(loaded.announced_skill_names.is_empty());
    }

    #[test]
    fn fingerprint_conversion_round_trip() {
        let in_memory: HashMap<String, (usize, u64, u64)> =
            HashMap::from([("srv".to_string(), (3, 111, 222))]);
        let persisted = to_persisted_fingerprints(&in_memory);
        let back = from_persisted_fingerprints(&persisted);
        assert_eq!(in_memory, back);
    }
}
