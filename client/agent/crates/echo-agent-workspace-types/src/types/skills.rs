//! Discovery shapes for skills surfaced by `OpsChunk::Skills` and
//! `WorkspaceEvent::SkillsChanged`.
//!
//! NOTE: this `source`-keyed `SkillInfo` is **not** the wire shape of
//! the `workspace.discover_skills` RPC -- that is
//! [`crate::rpc::skills::SkillInfo`] (`scope`-keyed).

use serde::{Deserialize, Serialize};

/// Discovered skill metadata.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillInfo {
    /// Stable identifier (also the slash-command name).
    pub id: String,
    /// Display name.
    #[serde(default)]
    pub display_name: String,
    /// Short human-readable description.
    #[serde(default)]
    pub description: String,
    /// Path to the skill definition (typically `SKILL.md`) as a string.
    #[serde(default)]
    pub path: String,
    /// Source bucket: `"global"`, `"workspace"`, `"server"`, `"bundled"`, etc.
    #[serde(default)]
    pub source: String,
}
