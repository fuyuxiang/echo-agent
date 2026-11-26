//! Workspace-scoped events (FS changes, server lifecycle, discovery,
//! ...).
//!
//! `WorkspaceEvent` carries only
//! **workspace-observed external state** -- filesystem watcher fires,
//! background subprocess lifecycle (LSP / MCP), background indexing
//! progress, file-watcher-detected config changes, and so on.
//!
//! Sampler-caused state never goes here. In particular, hunk events
//! (`HunkRecorded`, `HunkAccepted`, `HunkRejected`) used to live on
//! this enum and were removed: hunks come from tool writes
//! (sampler-caused) and `act_on_hunk` RPCs (sampler-caused), so the
//! sampler already has that state from the originating call. It can
//! re-snapshot via `list_hunks()` if it needs to reconcile.
//!
//! `ToolsChanged` is included on this enum because the tool registry
//! is a workspace state observation: an MCP server snapshot change
//! (workspace-observed) and a sampler-initiated `update_tool_config`
//! both produce the same downstream effect (other subscribers and the
//! sampler's UI need to re-fetch `tool_definitions`). Tool execution
//! itself is **not** broadcast here -- it stays on the sampler-caused
//! tool-result path.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::types::{
    FsEventKind, HookInfo, LspServerStatus, McpServerStatus, PluginInfo, SkillInfo, VcsKind,
};

/// Workspace-scoped event.
///
/// One event per fact, broadcast to every subscriber of the workspace
/// channel. Topic filtering happens client-side via
/// [`WorkspaceTopicSet`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum WorkspaceEvent {
    /// Filesystem watcher fired.
    FsChanged {
        /// Affected path (absolute).
        path: PathBuf,
        /// Event kind.
        kind: FsEventKind,
    },
    /// Git HEAD moved.
    GitHeadChanged {
        /// New commit sha.
        commit: String,
        /// New branch name (None for detached HEAD).
        branch: Option<String>,
        /// VCS kind.
        vcs: VcsKind,
    },
    /// Git lock is held by an external process; reads may block until
    /// `until`.
    GitLockHeld {
        /// Best-effort wall-clock estimate of when the lock will be
        /// released.
        ///
        /// An `Instant` would be natural, but `std::time::Instant`
        /// is process-local (monotonic from process boot) and not
        /// `Serialize` -- it is meaningless to a receiver in another
        /// process. Wall-clock UTC is the only correct choice for a
        /// wire type.
        until: DateTime<Utc>,
    },
    /// Skill discovery surfaced changes.
    SkillsChanged {
        /// Newly added skills.
        added: Vec<SkillInfo>,
        /// Ids of removed skills.
        removed: Vec<String>,
    },
    /// Plugin discovery surfaced changes.
    PluginsChanged {
        /// Current plugin set.
        plugins: Vec<PluginInfo>,
        /// Whether the project is trusted (gates plugin execution).
        project_trusted: bool,
    },
    /// Hook discovery surfaced changes.
    HooksChanged {
        /// Current hook set.
        hooks: Vec<HookInfo>,
        /// Whether the project is trusted (gates hook execution).
        project_trusted: bool,
    },
    /// MCP server transitioned state.
    McpServerStateChanged {
        /// Server identifier.
        server: String,
        /// New status.
        status: McpServerStatus,
    },
    /// LSP server transitioned state.
    LspServerStateChanged {
        /// Server identifier.
        server: String,
        /// New status.
        status: LspServerStatus,
    },
    /// Codebase index ingested more files.
    CodebaseIndexUpdated {
        /// Total files indexed so far.
        files_indexed: u64,
    },
    /// Project config changed on disk.
    ProjectConfigChanged,
    /// Permission policy changed on disk.
    PermissionPolicyChanged,
    /// A session's tool registry was rebuilt.
    ///
    /// Triggered when:
    /// - `WorkspaceChannel::update_tool_config` swaps a session's
    ///   `effective_tool_config`, or
    /// - an MCP server snapshot changes and the workspace re-resolves
    ///   each session's `FinalizedToolset`.
    ///
    /// Subscribers should re-fetch tool definitions for the affected
    /// session via `WorkspaceChannel::tool_definitions`.
    ToolsChanged {
        /// Affected session id.
        session_id: String,
    },
}

/// Topic discriminator for workspace events.
///
/// Used by `EventBus::subscribe_filtered` to skip uninteresting events.
/// The mapping from event variant to topic is documented inline; topic
/// filtering is purely a delivery optimisation -- it never changes the
/// event payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceTopic {
    /// Filesystem-watcher events.
    Fs,
    /// VCS background events (HEAD moves, external git lock held).
    /// Note: hunk events are not on the EventBus -- see the
    /// module-level doc-comment.
    Vcs,
    /// Skill / plugin / hook discovery.
    Discovery,
    /// MCP / LSP server state.
    Servers,
    /// Codebase index progress.
    Index,
    /// Config / permission policy.
    Config,
    /// Session tool-registry rebuilds (capability filter, MCP merge,
    /// explicit `update_tool_config`).
    Tools,
}

impl WorkspaceEvent {
    /// Topic this event belongs to (used for filtering).
    pub fn topic(&self) -> WorkspaceTopic {
        match self {
            Self::FsChanged { .. } => WorkspaceTopic::Fs,
            Self::GitHeadChanged { .. } | Self::GitLockHeld { .. } => WorkspaceTopic::Vcs,
            Self::SkillsChanged { .. }
            | Self::PluginsChanged { .. }
            | Self::HooksChanged { .. } => WorkspaceTopic::Discovery,
            Self::McpServerStateChanged { .. } | Self::LspServerStateChanged { .. } => {
                WorkspaceTopic::Servers
            }
            Self::CodebaseIndexUpdated { .. } => WorkspaceTopic::Index,
            Self::ProjectConfigChanged | Self::PermissionPolicyChanged => WorkspaceTopic::Config,
            Self::ToolsChanged { .. } => WorkspaceTopic::Tools,
        }
    }
}

/// Set of workspace topics. Implemented as a small bitmask to keep
/// filter checks branch-free.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkspaceTopicSet {
    /// Bitmask: bit `n` is `WorkspaceTopic` discriminant `n`.
    bits: u32,
}

impl WorkspaceTopicSet {
    /// Empty set (matches no events).
    pub const fn empty() -> Self {
        Self { bits: 0 }
    }

    /// Set containing every topic.
    pub fn all() -> Self {
        let mut s = Self::empty();
        for t in [
            WorkspaceTopic::Fs,
            WorkspaceTopic::Vcs,
            WorkspaceTopic::Discovery,
            WorkspaceTopic::Servers,
            WorkspaceTopic::Index,
            WorkspaceTopic::Config,
            WorkspaceTopic::Tools,
        ] {
            s = s.with(t);
        }
        s
    }

    /// Return a copy of this set with `topic` added.
    #[must_use]
    pub fn with(mut self, topic: WorkspaceTopic) -> Self {
        self.bits |= 1u32 << topic_index(topic);
        self
    }

    /// Whether `topic` is contained in the set.
    pub fn contains(&self, topic: WorkspaceTopic) -> bool {
        self.bits & (1u32 << topic_index(topic)) != 0
    }

    /// Whether the set is empty.
    pub fn is_empty(&self) -> bool {
        self.bits == 0
    }
}

fn topic_index(topic: WorkspaceTopic) -> u32 {
    // Stable indices (do not reorder existing variants without bumping
    // the wire-compat manifest).
    match topic {
        WorkspaceTopic::Fs => 0,
        WorkspaceTopic::Vcs => 1,
        WorkspaceTopic::Discovery => 2,
        WorkspaceTopic::Servers => 3,
        WorkspaceTopic::Index => 4,
        WorkspaceTopic::Config => 5,
        WorkspaceTopic::Tools => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples() -> Vec<WorkspaceEvent> {
        vec![
            WorkspaceEvent::FsChanged {
                path: PathBuf::from("/x"),
                kind: FsEventKind::Modified,
            },
            WorkspaceEvent::GitHeadChanged {
                commit: "deadbeef".into(),
                branch: Some("main".into()),
                vcs: VcsKind::Git,
            },
            WorkspaceEvent::GitLockHeld { until: Utc::now() },
            WorkspaceEvent::SkillsChanged {
                added: vec![],
                removed: vec![],
            },
            WorkspaceEvent::PluginsChanged {
                plugins: vec![],
                project_trusted: true,
            },
            WorkspaceEvent::HooksChanged {
                hooks: vec![],
                project_trusted: true,
            },
            WorkspaceEvent::McpServerStateChanged {
                server: "fs".into(),
                status: McpServerStatus::Running,
            },
            WorkspaceEvent::LspServerStateChanged {
                server: "rust-analyzer".into(),
                status: LspServerStatus::Running,
            },
            WorkspaceEvent::CodebaseIndexUpdated { files_indexed: 100 },
            WorkspaceEvent::ProjectConfigChanged,
            WorkspaceEvent::PermissionPolicyChanged,
            WorkspaceEvent::ToolsChanged {
                session_id: "main".into(),
            },
        ]
    }

    #[test]
    fn every_variant_round_trips() {
        for ev in samples() {
            let json = serde_json::to_string(&ev).unwrap();
            let back: WorkspaceEvent = serde_json::from_str(&json).unwrap();
            assert_eq!(ev, back);
        }
    }

    #[test]
    fn topic_set_contains_what_was_added() {
        let s = WorkspaceTopicSet::empty()
            .with(WorkspaceTopic::Fs)
            .with(WorkspaceTopic::Vcs);
        assert!(s.contains(WorkspaceTopic::Fs));
        assert!(s.contains(WorkspaceTopic::Vcs));
        assert!(!s.contains(WorkspaceTopic::Discovery));
        assert!(!s.is_empty());
    }

    #[test]
    fn all_contains_every_topic() {
        let s = WorkspaceTopicSet::all();
        for t in [
            WorkspaceTopic::Fs,
            WorkspaceTopic::Vcs,
            WorkspaceTopic::Discovery,
            WorkspaceTopic::Servers,
            WorkspaceTopic::Index,
            WorkspaceTopic::Config,
            WorkspaceTopic::Tools,
        ] {
            assert!(s.contains(t));
        }
    }

    #[test]
    fn topic_classification_matches_variants() {
        assert_eq!(
            WorkspaceEvent::FsChanged {
                path: PathBuf::from("x"),
                kind: FsEventKind::Created,
            }
            .topic(),
            WorkspaceTopic::Fs
        );
        assert_eq!(
            WorkspaceEvent::ProjectConfigChanged.topic(),
            WorkspaceTopic::Config
        );
        assert_eq!(
            WorkspaceEvent::ToolsChanged {
                session_id: "s".into()
            }
            .topic(),
            WorkspaceTopic::Tools
        );
    }
}
