//! Toolset-swap guard policy: every trigger evaluates the one decision table,
//! [`SwapPolicy::evaluate`] over a [`SessionSnapshot`]. The exhaustive match
//! is the spec; the matrix test's `expected_decision` mirrors it row by row.

use prometheus::{IntCounterVec, register_int_counter_vec};

use crate::activity::ActivityTracker;
use crate::session::WorkspaceSession;

/// Toolset installs/swaps by trigger (`create`/`fork`/`owner_rebind`/
/// `update_tool_config`/`mcp_snapshot`/`hub_tools`/`other`) plus the guard
/// state at swap time; record via [`record_toolset_swap`] only.
pub(crate) static WORKSPACE_TOOLSET_SWAP_TOTAL: std::sync::LazyLock<IntCounterVec> =
    std::sync::LazyLock::new(|| {
        register_int_counter_vec!(
            "echo_agent_workspace_toolset_swap_total",
            "Session toolset installs and swaps, by trigger and guard state",
            &["trigger", "turn_active", "in_flight"]
        )
        .unwrap()
    });

/// Toolset swaps rejected by the turn-safety guards, by reason
/// (`turn_active` = RPC entry check, `turn_active_late` = post-resolve
/// re-check, `in_flight` = owner-rebind keep-old) and trigger.
pub(crate) static WORKSPACE_TOOLSET_SWAP_REJECTED_TOTAL: std::sync::LazyLock<IntCounterVec> =
    std::sync::LazyLock::new(|| {
        register_int_counter_vec!(
            "echo_agent_workspace_toolset_swap_rejected_total",
            "Session toolset swaps rejected by the turn-safety guards, by reason and trigger",
            &["reason", "trigger"]
        )
        .unwrap()
    });

/// Rebinds (`session.bind` against an existing session) that carried a changed
/// explicit toolset and re-resolved it, by result. A steady `ok` stream is the
/// resume path correcting sessions that were created by metadata-less binds.
static WORKSPACE_BIND_REBIND_RERESOLVE_TOTAL: std::sync::LazyLock<IntCounterVec> =
    std::sync::LazyLock::new(|| {
        register_int_counter_vec!(
            "echo_agent_workspace_bind_rebind_reresolve_total",
            "session.bind rebinds that re-resolved a changed explicit toolset, by result",
            &["result"]
        )
        .unwrap()
    });

/// Zero-init this module's metric families. See [`crate::init_metrics`].
pub(crate) fn init_metrics() {
    const TRIGGERS: &[SwapTrigger] = &[
        SwapTrigger::OwnerRebind,
        SwapTrigger::UpdateRpc,
        SwapTrigger::McpSnapshot,
        SwapTrigger::HubTools,
        SwapTrigger::Other,
    ];
    for trigger in TRIGGERS {
        for turn_active in ["true", "false"] {
            for in_flight in ["true", "false"] {
                WORKSPACE_TOOLSET_SWAP_TOTAL
                    .with_label_values(&[trigger.metric_label(), turn_active, in_flight])
                    .inc_by(0);
            }
        }
    }
    // Only these reason/trigger pairs are reachable: in-flight guards owner
    // rebinds; the two turn-active guards fire on the update RPC.
    for (reason, trigger) in [
        (DeferReason::InFlightCalls, SwapTrigger::OwnerRebind),
        (DeferReason::TurnActive, SwapTrigger::UpdateRpc),
        (DeferReason::TurnActiveLate, SwapTrigger::UpdateRpc),
    ] {
        WORKSPACE_TOOLSET_SWAP_REJECTED_TOTAL
            .with_label_values(&[reason.metric_reason(), trigger.metric_label()])
            .inc_by(0);
    }
    for result in ["skipped_externally_owned", "ok", "error"] {
        WORKSPACE_BIND_REBIND_RERESOLVE_TOTAL
            .with_label_values(&[result])
            .inc_by(0);
    }
}

/// Record a toolset install/swap on [`WORKSPACE_TOOLSET_SWAP_TOTAL`],
/// stamping the session's turn/in-flight state at swap time.
pub(crate) fn record_toolset_swap(tracker: &ActivityTracker, trigger: &str, session_id: &str) {
    let turn_active = bool_label(tracker.is_turn_active(session_id));
    let in_flight = bool_label(tracker.session_active_tool_calls(session_id) > 0);
    WORKSPACE_TOOLSET_SWAP_TOTAL
        .with_label_values(&[trigger, turn_active, in_flight])
        .inc();
}

fn bool_label(v: bool) -> &'static str {
    if v { "true" } else { "false" }
}

/// What initiated a toolset swap attempt. The trigger fixes both the metric
/// `trigger` label and the guard set the policy applies (see the module table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SwapTrigger {
    /// Hub `session.bind` against an existing session that carried a changed
    /// explicit toolset (`WorkspaceHandle::rebind_existing_hub_session`).
    OwnerRebind,
    /// The `workspace.update_tool_config` RPC.
    UpdateRpc,
    /// `re_resolve_all_sessions` after an MCP snapshot change.
    McpSnapshot,
    /// `re_resolve_all_sessions` after a remote tools change/notification.
    HubTools,
    /// `re_resolve_all_sessions` from an unrecognized source (test callers
    /// only today). Snapshot-rebuild policy, `other` metric label.
    Other,
}

impl SwapTrigger {
    pub(crate) fn from_rebuild_source(source: &str) -> Self {
        match source {
            "mcp_snapshot_changed" => Self::McpSnapshot,
            "hub_tools_changed" | "hub_notification" => Self::HubTools,
            _ => Self::Other,
        }
    }

    /// The `trigger` label on [`WORKSPACE_TOOLSET_SWAP_TOTAL`] and
    /// [`WORKSPACE_TOOLSET_SWAP_REJECTED_TOTAL`]. Dashboards depend on these
    /// exact values.
    pub(crate) fn metric_label(self) -> &'static str {
        match self {
            Self::OwnerRebind => "owner_rebind",
            Self::UpdateRpc => "update_tool_config",
            Self::McpSnapshot => "mcp_snapshot",
            Self::HubTools => "hub_tools",
            Self::Other => "other",
        }
    }

    /// Whether the apply path re-evaluates post-resolve, pre-install. Only the
    /// update RPC: a turn can start mid-resolve (turn hooks are lock-free);
    /// owner rebinds must answer inside the server's ack budget, so they don't.
    pub(crate) fn rechecks_after_resolve(self) -> bool {
        self == Self::UpdateRpc
    }
}

/// Why a swap was skipped (nothing resolved, toolset and fingerprint kept).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SkipReason {
    /// The toolset `Terminal` is not the session-owned backend (local/shell
    /// bind): a rebuild would detach tools from the shell's live task table.
    ExternallyOwned,
}

/// Why a swap was deferred (existing toolset kept; a later attempt applies).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeferReason {
    /// Owner rebind arrived while the session had tool calls in flight, with
    /// either an `explicit → different-explicit` change or a stale-heal
    /// identical re-apply.
    InFlightCalls,
    /// The session's turn is active and the config differs (update-RPC entry
    /// check); retryable at the turn boundary.
    TurnActive,
    /// [`Self::TurnActive`] detected by the post-resolve re-check: the turn
    /// started during the re-resolve and the resolved toolset was discarded.
    TurnActiveLate,
}

impl DeferReason {
    /// The `reason` label on [`WORKSPACE_TOOLSET_SWAP_REJECTED_TOTAL`].
    pub(crate) fn metric_reason(self) -> &'static str {
        match self {
            Self::InFlightCalls => "in_flight",
            Self::TurnActive => "turn_active",
            Self::TurnActiveLate => "turn_active_late",
        }
    }
}

/// What a trigger should do with its candidate config, per the module table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "a non-Apply decision means the config must NOT be installed"]
pub(crate) enum SwapDecision {
    /// Resolve and install the candidate config.
    Apply,
    /// Identical fingerprint: the live toolset already reflects the candidate.
    Reuse,
    /// Deliberate skip: leave toolset AND fingerprint untouched.
    Skip(SkipReason),
    /// Keep the existing toolset for now; a later attempt applies.
    Defer(DeferReason),
}

/// How the candidate config's fingerprint relates to the session's stored one.
/// Produced under a single lock acquisition (see [`classify`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BindFingerprintTransition {
    /// Candidate fingerprint equals the stored one.
    Unchanged,
    /// Stored fingerprint is `None` (default resolution) and the candidate
    /// differs.
    FromDefault,
    /// Stored fingerprint is explicit and the candidate differs.
    FromExplicit,
}

/// Classify `candidate` against the stored bind fingerprint in one poison-safe
/// lock acquisition, so the decision cannot straddle a concurrent
/// fingerprint write (`set_if_unset` runs outside `update_lock`).
fn classify(
    stored: &std::sync::Mutex<Option<serde_json::Value>>,
    candidate: Option<&serde_json::Value>,
) -> BindFingerprintTransition {
    let guard = stored.lock().unwrap_or_else(|e| e.into_inner());
    if guard.as_ref() == candidate {
        BindFingerprintTransition::Unchanged
    } else if guard.is_some() {
        BindFingerprintTransition::FromExplicit
    } else {
        BindFingerprintTransition::FromDefault
    }
}

/// One coherent read (under `update_lock`) of the session state the policy
/// keys on. Turn/in-flight reads are tracker-side lock-free, so a decision
/// can go stale during a long resolve — see `rechecks_after_resolve`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SessionSnapshot {
    /// `None` = no candidate config: an in-place rebuild of the session's
    /// current baseline (snapshot-driven triggers), never Reuse-exempt.
    transition: Option<BindFingerprintTransition>,
    turn_active: bool,
    in_flight_calls: u32,
    toolset_terminal_session_owned: bool,
    /// The last snapshot-driven rebuild failed and kept a stale toolset
    /// ([`WorkspaceSession::stale_resolve`]): an identical fingerprint does
    /// not prove the live toolset is current, only that the *config* is.
    stale_resolve: bool,
}

impl SessionSnapshot {
    /// Capture against a candidate bind-config fingerprint (`None` = default
    /// resolution) — the owner-rebind and update-RPC triggers.
    pub(crate) async fn capture(
        session: &WorkspaceSession,
        tracker: &ActivityTracker,
        candidate_fingerprint: Option<&serde_json::Value>,
    ) -> Self {
        let transition = classify(&session.bind_tool_config_fingerprint, candidate_fingerprint);
        Self::with_transition(session, tracker, Some(transition)).await
    }

    /// Capture for an in-place rebuild of the session's current baseline (the
    /// snapshot-driven triggers): no candidate config, so no fingerprint
    /// transition to exempt on.
    pub(crate) async fn capture_for_rebuild(
        session: &WorkspaceSession,
        tracker: &ActivityTracker,
    ) -> Self {
        Self::with_transition(session, tracker, None).await
    }

    async fn with_transition(
        session: &WorkspaceSession,
        tracker: &ActivityTracker,
        transition: Option<BindFingerprintTransition>,
    ) -> Self {
        let session_id = session.session_id();
        Self {
            transition,
            turn_active: tracker.is_turn_active(session_id),
            in_flight_calls: tracker.session_active_tool_calls(session_id),
            toolset_terminal_session_owned: session.toolset_terminal_is_session_owned().await,
            stale_resolve: session.stale_resolve(),
        }
    }

    /// Tool calls in flight at capture time.
    pub(crate) fn in_flight_calls(&self) -> u32 {
        self.in_flight_calls
    }
}

/// The toolset-swap guard policy. Stateless: the whole table lives in
/// [`Self::evaluate`].
pub(crate) struct SwapPolicy;

impl SwapPolicy {
    /// Decide what `trigger` should do with its candidate, per the module
    /// table. Pure function of the snapshot — callers act on the decision
    /// under the same `update_lock` hold the snapshot was captured under.
    pub(crate) fn evaluate(snap: &SessionSnapshot, trigger: SwapTrigger) -> SwapDecision {
        use BindFingerprintTransition::{FromExplicit, Unchanged};
        match (trigger, snap.transition) {
            (SwapTrigger::UpdateRpc | SwapTrigger::OwnerRebind, Some(Unchanged))
                if !snap.stale_resolve =>
            {
                SwapDecision::Reuse
            }
            (
                SwapTrigger::McpSnapshot | SwapTrigger::HubTools | SwapTrigger::Other,
                Some(Unchanged),
            ) => SwapDecision::Reuse,

            (SwapTrigger::OwnerRebind, Some(FromExplicit | Unchanged))
                if snap.in_flight_calls > 0 =>
            {
                SwapDecision::Defer(DeferReason::InFlightCalls)
            }
            (SwapTrigger::OwnerRebind, _) if !snap.toolset_terminal_session_owned => {
                SwapDecision::Skip(SkipReason::ExternallyOwned)
            }
            (SwapTrigger::OwnerRebind, _) => SwapDecision::Apply,

            (SwapTrigger::UpdateRpc, _) if snap.turn_active => {
                SwapDecision::Defer(DeferReason::TurnActive)
            }
            (SwapTrigger::UpdateRpc, _) if !snap.toolset_terminal_session_owned => {
                SwapDecision::Skip(SkipReason::ExternallyOwned)
            }
            (SwapTrigger::UpdateRpc, _) => SwapDecision::Apply,

            (SwapTrigger::McpSnapshot | SwapTrigger::HubTools | SwapTrigger::Other, _)
                if !snap.toolset_terminal_session_owned =>
            {
                SwapDecision::Skip(SkipReason::ExternallyOwned)
            }
            (SwapTrigger::McpSnapshot | SwapTrigger::HubTools | SwapTrigger::Other, _) => {
                SwapDecision::Apply
            }
        }
    }
}

/// What acting on a [`SwapDecision`] ultimately did — the key of
/// [`record_swap_decision`]. No `Reused` action: a reuse changes nothing
/// and no metric family counts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SwapAction {
    /// [`SwapDecision::Skip`] honored.
    Skipped(SkipReason),
    /// [`SwapDecision::Defer`] honored (existing toolset kept).
    Deferred(DeferReason),
    /// [`SwapDecision::Apply`] succeeded: toolset resolved and installed.
    Applied,
    /// [`SwapDecision::Apply`] failed: the re-resolve errored, existing kept.
    ApplyFailed,
}

/// The single chokepoint all three swap metric families emit from (swap
/// total on `Applied`, rejected total on `Deferred`, rebind-reresolve on
/// owner-rebind results), so label values cannot drift per call site.
pub(crate) fn record_swap_decision(
    tracker: &ActivityTracker,
    trigger: SwapTrigger,
    session_id: &str,
    action: SwapAction,
) {
    match action {
        SwapAction::Deferred(reason) => {
            WORKSPACE_TOOLSET_SWAP_REJECTED_TOTAL
                .with_label_values(&[reason.metric_reason(), trigger.metric_label()])
                .inc();
        }
        SwapAction::Skipped(SkipReason::ExternallyOwned) => {
            if trigger == SwapTrigger::OwnerRebind {
                WORKSPACE_BIND_REBIND_RERESOLVE_TOTAL
                    .with_label_values(&["skipped_externally_owned"])
                    .inc();
            }
        }
        SwapAction::Applied => {
            record_toolset_swap(tracker, trigger.metric_label(), session_id);
            if trigger == SwapTrigger::OwnerRebind {
                WORKSPACE_BIND_REBIND_RERESOLVE_TOTAL
                    .with_label_values(&["ok"])
                    .inc();
            }
        }
        SwapAction::ApplyFailed => {
            if trigger == SwapTrigger::OwnerRebind {
                WORKSPACE_BIND_REBIND_RERESOLVE_TOTAL
                    .with_label_values(&["error"])
                    .inc();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(
        transition: Option<BindFingerprintTransition>,
        turn_active: bool,
        in_flight_calls: u32,
        owned: bool,
        stale_resolve: bool,
    ) -> SessionSnapshot {
        SessionSnapshot {
            transition,
            turn_active,
            in_flight_calls,
            toolset_terminal_session_owned: owned,
            stale_resolve,
        }
    }

    const ALL_TRIGGERS: [SwapTrigger; 5] = [
        SwapTrigger::OwnerRebind,
        SwapTrigger::UpdateRpc,
        SwapTrigger::McpSnapshot,
        SwapTrigger::HubTools,
        SwapTrigger::Other,
    ];

    const ALL_TRANSITIONS: [Option<BindFingerprintTransition>; 4] = [
        Some(BindFingerprintTransition::Unchanged),
        Some(BindFingerprintTransition::FromDefault),
        Some(BindFingerprintTransition::FromExplicit),
        None,
    ];

    /// Spec mirror of the module decision table, maintained independently of
    /// [`SwapPolicy::evaluate`].
    fn expected_decision(
        trigger: SwapTrigger,
        transition: Option<BindFingerprintTransition>,
        turn_active: bool,
        in_flight_calls: u32,
        owned: bool,
        stale_resolve: bool,
    ) -> SwapDecision {
        if transition == Some(BindFingerprintTransition::Unchanged)
            && !(matches!(trigger, SwapTrigger::UpdateRpc | SwapTrigger::OwnerRebind)
                && stale_resolve)
        {
            return SwapDecision::Reuse;
        }
        match trigger {
            SwapTrigger::OwnerRebind => {
                if matches!(
                    transition,
                    Some(
                        BindFingerprintTransition::FromExplicit
                            | BindFingerprintTransition::Unchanged
                    )
                ) && in_flight_calls > 0
                {
                    SwapDecision::Defer(DeferReason::InFlightCalls)
                } else if !owned {
                    SwapDecision::Skip(SkipReason::ExternallyOwned)
                } else {
                    SwapDecision::Apply
                }
            }
            SwapTrigger::UpdateRpc => {
                if turn_active {
                    SwapDecision::Defer(DeferReason::TurnActive)
                } else if !owned {
                    SwapDecision::Skip(SkipReason::ExternallyOwned)
                } else {
                    SwapDecision::Apply
                }
            }
            SwapTrigger::McpSnapshot | SwapTrigger::HubTools | SwapTrigger::Other => {
                if !owned {
                    SwapDecision::Skip(SkipReason::ExternallyOwned)
                } else {
                    SwapDecision::Apply
                }
            }
        }
    }

    #[test]
    fn evaluate_matches_decision_table_over_full_matrix() {
        for trigger in ALL_TRIGGERS {
            for transition in ALL_TRANSITIONS {
                for turn_active in [false, true] {
                    for in_flight_calls in [0u32, 2] {
                        for owned in [true, false] {
                            for stale_resolve in [false, true] {
                                let got = SwapPolicy::evaluate(
                                    &snap(
                                        transition,
                                        turn_active,
                                        in_flight_calls,
                                        owned,
                                        stale_resolve,
                                    ),
                                    trigger,
                                );
                                let expected = expected_decision(
                                    trigger,
                                    transition,
                                    turn_active,
                                    in_flight_calls,
                                    owned,
                                    stale_resolve,
                                );
                                assert_eq!(
                                    got, expected,
                                    "trigger={trigger:?} transition={transition:?} \
                                     turn_active={turn_active} in_flight={in_flight_calls} \
                                     owned={owned} stale_resolve={stale_resolve}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn identical_fingerprint_reuses_regardless_of_gates() {
        for trigger in ALL_TRIGGERS {
            let decision = SwapPolicy::evaluate(
                &snap(
                    Some(BindFingerprintTransition::Unchanged),
                    true,
                    3,
                    false,
                    false,
                ),
                trigger,
            );
            assert_eq!(decision, SwapDecision::Reuse, "trigger={trigger:?}");
        }
    }

    #[test]
    fn update_rpc_identical_reapply_recovers_after_failed_rebuild() {
        let identical = Some(BindFingerprintTransition::Unchanged);
        assert_eq!(
            SwapPolicy::evaluate(
                &snap(identical, false, 0, true, true),
                SwapTrigger::UpdateRpc
            ),
            SwapDecision::Apply,
            "idle + session-owned: the identical re-apply repairs the stale toolset"
        );
        assert_eq!(
            SwapPolicy::evaluate(
                &snap(identical, true, 0, true, true),
                SwapTrigger::UpdateRpc
            ),
            SwapDecision::Defer(DeferReason::TurnActive),
            "the recovery apply is turn-gated like any mutation"
        );
        assert_eq!(
            SwapPolicy::evaluate(
                &snap(identical, false, 0, false, true),
                SwapTrigger::UpdateRpc
            ),
            SwapDecision::Skip(SkipReason::ExternallyOwned),
            "an externally-owned toolset cannot be rebuilt, stale or not"
        );
        for trigger in [
            SwapTrigger::McpSnapshot,
            SwapTrigger::HubTools,
            SwapTrigger::Other,
        ] {
            assert_eq!(
                SwapPolicy::evaluate(&snap(identical, false, 0, true, true), trigger),
                SwapDecision::Reuse,
                "trigger={trigger:?}: snapshot triggers carry no recovery lever"
            );
        }
    }

    #[test]
    fn owner_rebind_identical_reapply_recovers_after_failed_rebuild() {
        let identical = Some(BindFingerprintTransition::Unchanged);
        assert_eq!(
            SwapPolicy::evaluate(
                &snap(identical, false, 0, true, true),
                SwapTrigger::OwnerRebind
            ),
            SwapDecision::Apply,
            "a reconnect's identical rebind heals the stale toolset"
        );
        assert_eq!(
            SwapPolicy::evaluate(
                &snap(identical, false, 2, true, true),
                SwapTrigger::OwnerRebind
            ),
            SwapDecision::Defer(DeferReason::InFlightCalls),
            "the heal defers while tool calls are in flight"
        );
        assert_eq!(
            SwapPolicy::evaluate(
                &snap(identical, false, 0, false, true),
                SwapTrigger::OwnerRebind
            ),
            SwapDecision::Skip(SkipReason::ExternallyOwned),
            "an externally-owned toolset cannot be rebuilt, stale or not"
        );
        assert_eq!(
            SwapPolicy::evaluate(
                &snap(identical, false, 0, true, false),
                SwapTrigger::OwnerRebind
            ),
            SwapDecision::Reuse,
            "without the stale marker the identical rebind stays a no-op reuse"
        );
    }

    #[test]
    fn owner_rebind_from_default_applies_mid_turn_with_calls_in_flight() {
        let decision = SwapPolicy::evaluate(
            &snap(
                Some(BindFingerprintTransition::FromDefault),
                true,
                2,
                true,
                false,
            ),
            SwapTrigger::OwnerRebind,
        );
        assert_eq!(decision, SwapDecision::Apply);
    }

    #[test]
    fn owner_rebind_in_flight_defer_wins_over_external_ownership() {
        let decision = SwapPolicy::evaluate(
            &snap(
                Some(BindFingerprintTransition::FromExplicit),
                false,
                1,
                false,
                false,
            ),
            SwapTrigger::OwnerRebind,
        );
        assert_eq!(decision, SwapDecision::Defer(DeferReason::InFlightCalls));
    }

    #[test]
    fn update_rpc_turn_gate_wins_over_external_ownership() {
        let decision = SwapPolicy::evaluate(
            &snap(
                Some(BindFingerprintTransition::FromDefault),
                true,
                0,
                false,
                false,
            ),
            SwapTrigger::UpdateRpc,
        );
        assert_eq!(decision, SwapDecision::Defer(DeferReason::TurnActive));
    }

    #[test]
    fn owner_rebind_ignores_turn_active() {
        let decision = SwapPolicy::evaluate(
            &snap(
                Some(BindFingerprintTransition::FromExplicit),
                true,
                0,
                true,
                false,
            ),
            SwapTrigger::OwnerRebind,
        );
        assert_eq!(decision, SwapDecision::Apply);
    }

    #[test]
    fn snapshot_rebuilds_ignore_turn_and_in_flight_gates() {
        for trigger in [
            SwapTrigger::McpSnapshot,
            SwapTrigger::HubTools,
            SwapTrigger::Other,
        ] {
            assert_eq!(
                SwapPolicy::evaluate(&snap(None, true, 4, true, false), trigger),
                SwapDecision::Apply,
                "trigger={trigger:?}"
            );
            assert_eq!(
                SwapPolicy::evaluate(&snap(None, true, 4, false, false), trigger),
                SwapDecision::Skip(SkipReason::ExternallyOwned),
                "trigger={trigger:?}"
            );
        }
    }

    #[test]
    fn metric_labels_are_locked() {
        assert_eq!(SwapTrigger::OwnerRebind.metric_label(), "owner_rebind");
        assert_eq!(SwapTrigger::UpdateRpc.metric_label(), "update_tool_config");
        assert_eq!(SwapTrigger::McpSnapshot.metric_label(), "mcp_snapshot");
        assert_eq!(SwapTrigger::HubTools.metric_label(), "hub_tools");
        assert_eq!(SwapTrigger::Other.metric_label(), "other");
        assert_eq!(DeferReason::InFlightCalls.metric_reason(), "in_flight");
        assert_eq!(DeferReason::TurnActive.metric_reason(), "turn_active");
        assert_eq!(
            DeferReason::TurnActiveLate.metric_reason(),
            "turn_active_late"
        );
    }

    #[test]
    fn only_update_rpc_rechecks_after_resolve() {
        for trigger in ALL_TRIGGERS {
            assert_eq!(
                trigger.rechecks_after_resolve(),
                trigger == SwapTrigger::UpdateRpc,
                "trigger={trigger:?}"
            );
        }
    }

    #[test]
    fn rebuild_source_mapping_matches_legacy_labels() {
        assert_eq!(
            SwapTrigger::from_rebuild_source("mcp_snapshot_changed"),
            SwapTrigger::McpSnapshot
        );
        assert_eq!(
            SwapTrigger::from_rebuild_source("hub_tools_changed"),
            SwapTrigger::HubTools
        );
        assert_eq!(
            SwapTrigger::from_rebuild_source("hub_notification"),
            SwapTrigger::HubTools
        );
        assert_eq!(
            SwapTrigger::from_rebuild_source("test_preserves_feed"),
            SwapTrigger::Other
        );
    }

    #[test]
    fn classify_is_poison_safe() {
        let stored: std::sync::Arc<std::sync::Mutex<Option<serde_json::Value>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Some(serde_json::json!({"a": 1}))));
        let poisoner = stored.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.lock().unwrap();
            panic!("poison the fingerprint lock");
        })
        .join();
        assert!(stored.is_poisoned(), "precondition: the lock is poisoned");

        let same = serde_json::json!({"a": 1});
        let other = serde_json::json!({"b": 2});
        assert_eq!(
            classify(&stored, Some(&same)),
            BindFingerprintTransition::Unchanged
        );
        assert_eq!(
            classify(&stored, Some(&other)),
            BindFingerprintTransition::FromExplicit
        );
        assert_eq!(
            classify(&stored, None),
            BindFingerprintTransition::FromExplicit
        );
    }

    #[test]
    fn classify_from_default_transitions() {
        let stored = std::sync::Mutex::new(None);
        let candidate = serde_json::json!({"a": 1});
        assert_eq!(
            classify(&stored, Some(&candidate)),
            BindFingerprintTransition::FromDefault
        );
        assert_eq!(
            classify(&stored, None),
            BindFingerprintTransition::Unchanged
        );
    }
}
