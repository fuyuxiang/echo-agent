//! Capability-mode filtering for session toolsets.

use echo_agent_tools::registry::types::{ToolConfig, ToolServerConfig};
use echo_agent_tools::types::tool::ToolKind;

/// Capability mode applied to a session's toolset.
///
/// A partial order is defined via [`CapabilityMode::is_subset_of`]:
/// `ReadOnly < ReadWrite < All` and `ReadOnly < Execute < All`.
/// `ReadWrite` and `Execute` are *incomparable* (neither is a subset
/// of the other). `fork_session` enforces `child <= parent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityMode {
    /// Reading and searching only. No edits, no shell, no background tasks.
    ReadOnly,
    /// Read + edit. No shell execution.
    ReadWrite,
    /// Read + shell execution + background-task control. No edits.
    Execute,
    /// Every tool kind allowed.
    All,
}

impl Default for CapabilityMode {
    /// Defaults to [`CapabilityMode::ReadWrite`] (subagent default;
    /// the main/root session is always `All`).
    fn default() -> Self {
        Self::ReadWrite
    }
}

impl CapabilityMode {
    /// Filter `config.tools` by capability mode, returning a copy with
    /// disallowed tools dropped.
    ///
    /// Tools whose `kind` is `None` (baseline, e.g. ad-hoc tools
    /// declared via `ToolConfig::simple`) are preserved across all
    /// modes. **MCP-origin** `kind: None` tools are NOT preserved by
    /// this method; see `resolve_session_toolset` for the asymmetric
    /// handling.
    pub fn filter(self, config: &ToolServerConfig) -> ToolServerConfig {
        let kept: Vec<ToolConfig> = config
            .tools
            .iter()
            .filter(|tool| match tool.kind {
                None => true,
                Some(kind) => kind_allowed(self, kind),
            })
            .cloned()
            .collect();
        ToolServerConfig {
            tools: kept,
            behavior_preset: config.behavior_preset.clone(),
        }
    }

    /// Whether every kind allowed by `self` is also allowed by `other`.
    /// Used by `fork_session` to reject capability widening.
    pub fn is_subset_of(self, other: CapabilityMode) -> bool {
        for kind in ALL_TOOL_KINDS {
            if kind_allowed(self, *kind) && !kind_allowed(other, *kind) {
                return false;
            }
        }
        true
    }
}

/// Every `ToolKind` variant. Used by `is_subset_of` and by parameterised
/// tests. When a new variant is added to `ToolKind`, the compile-time
/// assertion below fires so it can't be silently omitted.
pub(crate) const ALL_TOOL_KINDS: &[ToolKind] = &[
    ToolKind::Read,
    ToolKind::Edit,
    ToolKind::Delete,
    ToolKind::ListDir,
    ToolKind::Write,
    ToolKind::Move,
    ToolKind::Search,
    ToolKind::Lsp,
    ToolKind::Execute,
    ToolKind::Plan,
    ToolKind::WebSearch,
    ToolKind::WebFetch,
    ToolKind::BackgroundTaskAction,
    ToolKind::WaitTasksAction,
    ToolKind::KillTaskAction,
    ToolKind::List,
    ToolKind::Skill,
    ToolKind::MemorySearch,
    ToolKind::MemoryGet,
    ToolKind::Task,
    ToolKind::EnterPlan,
    ToolKind::ExitPlan,
    ToolKind::AskUser,
    ToolKind::ImageGen,
    ToolKind::VideoGen,
    ToolKind::ImageToVideo,
    ToolKind::ReferenceToVideo,
    ToolKind::DeployApp,
    ToolKind::InitOrUpdateApp,
    ToolKind::SearchTool,
    ToolKind::UseTool,
    ToolKind::Monitor,
    ToolKind::GoalUpdate,
    ToolKind::Workflow,
    ToolKind::Other,
];

// Compile-time guard: if a new `ToolKind` variant is added but not listed in
// `ALL_TOOL_KINDS`, this assertion fails.
const _: () = assert!(
    ALL_TOOL_KINDS.len() == ToolKind::VARIANT_COUNT,
    "ALL_TOOL_KINDS is out of sync with ToolKind — add the new variant"
);

/// Maps `(CapabilityMode, ToolKind)` -> kept-or-dropped.
///
/// This `match` is intentionally exhaustive: when `ToolKind` gains a
/// new variant the compiler errors here, forcing a triage decision.
pub(crate) fn kind_allowed(mode: CapabilityMode, kind: ToolKind) -> bool {
    use CapabilityMode as M;
    use ToolKind::*;

    if matches!(mode, M::All) {
        return true;
    }

    match kind {
        // Meta tools: always allowed.
        Plan | EnterPlan | ExitPlan | AskUser | Skill | SearchTool | GoalUpdate => true,

        // Read class.
        Read | MemoryGet | MemorySearch => {
            matches!(mode, M::ReadOnly | M::ReadWrite | M::Execute)
        }

        // Search class.
        Search | WebSearch | WebFetch => {
            matches!(mode, M::ReadOnly | M::ReadWrite | M::Execute)
        }

        // Inspect class.
        Lsp | ListDir | List => matches!(mode, M::ReadOnly | M::ReadWrite | M::Execute),

        // Edit class.
        Edit | Write | Delete | Move | ImageGen | VideoGen | ImageToVideo | ReferenceToVideo
        | DeployApp | InitOrUpdateApp => matches!(mode, M::ReadWrite),

        // Bash / shell.
        Execute => matches!(mode, M::Execute),

        BackgroundTaskAction | WaitTasksAction | KillTaskAction | Task | Monitor | Workflow => {
            matches!(mode, M::Execute)
        }

        // Integration dispatch.
        UseTool => matches!(mode, M::ReadWrite | M::Execute),

        // Catch-all -- only `All` mode keeps it (early-return above).
        Other => false,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::tool_config::test_support;
    use echo_agent_tools::types::tool::ToolKind;

    fn make_cfg(tools: Vec<ToolConfig>) -> ToolServerConfig {
        ToolServerConfig {
            tools,
            behavior_preset: None,
        }
    }

    #[test]
    fn capability_mode_filter_table_is_exhaustive_per_kind() {
        for &mode in &[
            CapabilityMode::ReadOnly,
            CapabilityMode::ReadWrite,
            CapabilityMode::Execute,
            CapabilityMode::All,
        ] {
            for &kind in ALL_TOOL_KINDS {
                let id = format!("kind_{kind:?}");
                let cfg = make_cfg(vec![test_support::tc(&id, Some(kind))]);
                let out = mode.filter(&cfg);
                let expected_present = kind_allowed(mode, kind);
                let actually_present = out.tools.iter().any(|t| t.id == id);
                assert_eq!(
                    actually_present, expected_present,
                    "({mode:?}, {kind:?}): expected present={expected_present}, got {actually_present}"
                );
            }
        }
    }

    #[test]
    fn capability_mode_filter_anchored_membership() {
        let cfg = make_cfg(vec![
            test_support::tc("read", Some(ToolKind::Read)),
            test_support::tc("search", Some(ToolKind::Search)),
            test_support::tc("inspect", Some(ToolKind::Lsp)),
            test_support::tc("edit", Some(ToolKind::Edit)),
            test_support::tc("write", Some(ToolKind::Write)),
            test_support::tc("bash", Some(ToolKind::Execute)),
            test_support::tc("bg", Some(ToolKind::BackgroundTaskAction)),
            test_support::tc("plan", Some(ToolKind::Plan)),
            test_support::tc("ask", Some(ToolKind::AskUser)),
            test_support::tc("other", Some(ToolKind::Other)),
        ]);

        let names = |c: &ToolServerConfig| -> Vec<String> {
            c.tools.iter().map(|t| t.id.clone()).collect()
        };

        let ro = CapabilityMode::ReadOnly.filter(&cfg);
        assert_eq!(names(&ro), vec!["read", "search", "inspect", "plan", "ask"]);

        let rw = CapabilityMode::ReadWrite.filter(&cfg);
        assert_eq!(
            names(&rw),
            vec!["read", "search", "inspect", "edit", "write", "plan", "ask"]
        );

        let ex = CapabilityMode::Execute.filter(&cfg);
        assert_eq!(
            names(&ex),
            vec!["read", "search", "inspect", "bash", "bg", "plan", "ask"]
        );

        let all = CapabilityMode::All.filter(&cfg);
        assert_eq!(
            names(&all),
            vec![
                "read", "search", "inspect", "edit", "write", "bash", "bg", "plan", "ask", "other"
            ]
        );
    }

    #[test]
    fn capability_mode_baseline_kind_none_always_kept_via_filter() {
        let cfg = make_cfg(vec![
            test_support::tc("baseline.opaque", None),
            test_support::tc("baseline.also_opaque", None),
            test_support::tc("edit_dropped", Some(ToolKind::Edit)),
        ]);

        for mode in [
            CapabilityMode::ReadOnly,
            CapabilityMode::ReadWrite,
            CapabilityMode::Execute,
            CapabilityMode::All,
        ] {
            let filtered = mode.filter(&cfg);
            let ids: Vec<&str> = filtered.tools.iter().map(|t| t.id.as_str()).collect();
            assert!(
                ids.contains(&"baseline.opaque"),
                "kind: None tool dropped under {mode:?}: {ids:?}"
            );
            assert!(
                ids.contains(&"baseline.also_opaque"),
                "kind: None tool dropped under {mode:?}: {ids:?}"
            );
        }
    }

    #[test]
    fn capability_mode_preserves_behavior_preset_across_all_modes() {
        let mut cfg = make_cfg(vec![test_support::tc("read", Some(ToolKind::Read))]);
        cfg.behavior_preset = Some("current".to_owned());
        for mode in [
            CapabilityMode::ReadOnly,
            CapabilityMode::ReadWrite,
            CapabilityMode::Execute,
            CapabilityMode::All,
        ] {
            let out = mode.filter(&cfg);
            assert_eq!(
                out.behavior_preset.as_deref(),
                Some("current"),
                "behavior_preset lost under {mode:?}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // is_subset_of partial order
    // -----------------------------------------------------------------------

    #[test]
    fn capability_mode_is_subset_of_reflexive() {
        for &m in &[
            CapabilityMode::ReadOnly,
            CapabilityMode::ReadWrite,
            CapabilityMode::Execute,
            CapabilityMode::All,
        ] {
            assert!(m.is_subset_of(m), "{m:?} must be a subset of itself");
        }
    }

    #[test]
    fn capability_mode_is_subset_of_strict_chains() {
        assert!(CapabilityMode::ReadOnly.is_subset_of(CapabilityMode::ReadWrite));
        assert!(CapabilityMode::ReadOnly.is_subset_of(CapabilityMode::All));
        assert!(CapabilityMode::ReadWrite.is_subset_of(CapabilityMode::All));
        assert!(CapabilityMode::ReadOnly.is_subset_of(CapabilityMode::Execute));
        assert!(CapabilityMode::Execute.is_subset_of(CapabilityMode::All));
    }

    #[test]
    fn capability_mode_is_subset_of_widening_rejected() {
        assert!(!CapabilityMode::All.is_subset_of(CapabilityMode::ReadOnly));
        assert!(!CapabilityMode::ReadWrite.is_subset_of(CapabilityMode::ReadOnly));
        assert!(!CapabilityMode::Execute.is_subset_of(CapabilityMode::ReadOnly));
        assert!(!CapabilityMode::All.is_subset_of(CapabilityMode::ReadWrite));
        assert!(!CapabilityMode::All.is_subset_of(CapabilityMode::Execute));
    }

    #[test]
    fn capability_mode_is_subset_of_incomparable_pairs() {
        assert!(!CapabilityMode::ReadWrite.is_subset_of(CapabilityMode::Execute));
        assert!(!CapabilityMode::Execute.is_subset_of(CapabilityMode::ReadWrite));
    }
}
