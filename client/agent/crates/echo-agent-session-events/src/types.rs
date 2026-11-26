use serde::{Deserialize, Serialize};

/// Schema version for the event log format. Bumped on breaking changes.
pub const EVENT_SCHEMA_VERSION: &str = "1.0";

/// A single event in the per-turn event log.
///
/// Each variant maps to a line in `events.jsonl`. The `type` field is the
/// snake_case variant name (via `#[serde(tag = "type")]`). The `ts` field
/// is added by [`crate::log::EventWriter::emit`] at recording time.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    TurnStarted {
        session_id: String,
        turn_number: u64,
        model_id: String,
        yolo_mode: bool,
        conversation_message_count: usize,
        session_relationship: SessionRelationship,
        schema_version: String,
        /// Set when this turn is the user's redirect after a Ctrl+C / Esc abort
        /// of the previous turn: `cancel_then_send` (the user typed a fresh
        /// prompt) or `queued_after_cancel` (a prompt sat queued behind the
        /// aborted turn and was promoted). `None` for normal turns. Pairs with
        /// the `interjected` event's `redirect_kind` so the trace pipeline can
        /// query every user redirect through one shared field.
        #[serde(skip_serializing_if = "Option::is_none")]
        redirect_kind: Option<RedirectKind>,
    },
    PhaseChanged {
        phase: Phase,
    },
    FirstToken,
    LoopStarted {
        loop_index: u32,
    },
    ToolStarted {
        tool_name: String,
    },
    ToolCompleted {
        tool_name: String,
        /// Dispatch wall time; a cancel row reuses the duration measured at dispatch.
        duration_ms: u64,
        outcome: ToolOutcome,
        /// Model/ACP tool call id; matches the conversation's `tool_result`.
        /// Omitted on write when empty.
        #[serde(skip_serializing_if = "String::is_empty")]
        tool_call_id: String,
        /// Which emitter wrote this row. Shell (default) is omitted on the wire
        /// and is what package joins should use; workspace rows time the
        /// hub/proxy hop for the same call.
        #[serde(skip_serializing_if = "ToolCompletedSource::is_shell")]
        source: ToolCompletedSource,
    },
    PermissionRequested {
        tool_name: String,
    },
    PermissionResolved {
        tool_name: String,
        decision: PermissionDecision,
        wait_ms: u64,
    },
    TurnEnded {
        outcome: TurnOutcomeLabel,
        #[serde(skip_serializing_if = "Option::is_none")]
        cancellation_category: Option<CancellationCategory>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cancellation_context: Option<serde_json::Value>,
    },
    /// A mid-turn user interjection was merged into the running turn. Unlike
    /// `TurnEnded`, an interjection never ends the turn — the user steered
    /// in-flight (Ctrl+Enter) or promoted a queued prompt into the running
    /// turn. `source` distinguishes those two paths; `image_count` is how
    /// many images rode along (0 for text-only). Emitted at enqueue time,
    /// once per interjection.
    Interjected {
        source: InterjectionSource,
        image_count: u32,
        /// Always [`RedirectKind::Interjection`]. Carried so the shared
        /// `redirect_kind` field is queryable uniformly across every redirect
        /// event (`interjected` + the next-turn-after-abort `turn_started`).
        redirect_kind: RedirectKind,
    },
    YoloToggled {
        enabled: bool,
    },
    /// Emitted when goal mode auto-pauses an active goal. The `reason`
    /// records which automatic trigger fired: user cancel, infra-classified
    /// turn error, consecutive-failed-turn back-off, or verification block.
    GoalAutoPaused {
        reason: GoalPauseReasonTelemetry,
    },
    /// Runtime TodoGate nudged the model because a content-only turn ended
    /// with pending or unbacked in_progress todos. `reason` is the
    /// `TODO_GATE_*` discriminator constant in `echo-agent-runtime::session::events`.
    TodoGateFired {
        fires: u32,
        pending: usize,
        in_progress: usize,
        reason: &'static str,
    },
    /// TodoGate hit its per-prompt fire cap. Distinct event so cap-exhaustion
    /// is not conflated with a normal fire in the dashboards.
    TodoGateExhausted {
        pending: usize,
    },
    /// Layer-3 LazinessDetector classifier completed and produced a verdict.
    /// Fires even in observation-only mode (`max_nudges_per_session = 0`)
    /// so dashboards can validate classification quality before any nudges
    /// are injected. `category` is one of the `LAZINESS_*` discriminator
    /// constants in `echo-agent-runtime::session::events`.
    LazinessClassifierFired {
        model_id: String,
        category: &'static str,
        confidence: f32,
    },
    /// Layer-3 LazinessDetector injected a system-reminder nudge into the
    /// session. Always preceded by a `LazinessClassifierFired` for the
    /// same classification. Suppressed when the per-session cap is 0.
    LazinessNudgeFired {
        model_id: String,
        category: &'static str,
        nudges_remaining: u32,
    },
    /// Layer-3 LazinessDetector terminated without producing a verdict.
    /// `reason` is one of the `LAZINESS_ABORT_*` discriminator constants
    /// in `echo-agent-runtime::session::events`.
    LazinessClassifierAborted {
        reason: &'static str,
    },
    /// Goal-achievement classifier subagent was invoked. Fires once per
    /// classifier attempt regardless of outcome; pairs with exactly one
    /// of `GoalClassifierVerdict`, `GoalClassifierFailOpen`, or
    /// `GoalClassifierFailClosed` once the run terminates.
    GoalClassifierFired {
        attempt: u32,
        max_runs: u32,
        model_id: String,
    },
    /// Goal-achievement classifier returned a parsed verdict (Achieved or
    /// NotAchieved). `latency_ms` is the spawn-to-parse wall clock.
    GoalClassifierVerdict {
        verdict: GoalClassifierVerdictTelemetry,
        attempt: u32,
        latency_ms: u64,
    },
    /// Goal-achievement classifier could not produce a usable verdict due
    /// to an INFRA-class failure (timeout, sampler error, abort, file IO).
    /// Caller fails OPEN — treats as Achieved — and records the reason.
    GoalClassifierFailOpen {
        reason: &'static str,
        attempt: u32,
        latency_ms: u64,
    },
    /// Goal-achievement classifier could not produce a usable verdict due
    /// to a PARSE-class failure (malformed terminal token, missing details
    /// file). Caller fails CLOSED — treats as NotAchieved.
    GoalClassifierFailClosed {
        reason: &'static str,
        attempt: u32,
    },
    /// Goal-achievement classifier hit the per-goal run cap. Distinct event
    /// so cap exhaustion is not conflated with a normal verdict.
    GoalClassifierCapReached {
        attempt: u32,
    },
    /// Mid-turn `update_goal(completed: true)` was deferred to the next
    /// turn-end drain (Guard 2). `pending_depth` is the queue length
    /// AFTER the push so dashboards can spot accumulation in real time.
    GoalClassifierMidTurnDeferred {
        pending_depth: u32,
    },
    /// `update_goal(completed: true)` arrived AFTER the classifier
    /// cap had already auto-paused the goal. `attempts_seen` is the
    /// real `classifier_runs_attempted` snapshot (typically the cap),
    /// never `0`.
    GoalClassifierDroppedAfterCap {
        attempts_seen: u32,
    },
    /// A cap-pause cleared the pending-classifier-completions queue.
    /// One summary event per pause, not per-entry — `dropped` is the
    /// total entry count.
    GoalClassifierPendingQueueCleared {
        dropped: u32,
    },
    /// Goal planner subagent was invoked. Fires once per attempt;
    /// pairs with exactly one of `GoalPlannerCompleted` or
    /// `GoalPlannerFailClosed` once the run terminates. `max_runs`
    /// mirrors the classifier event for dashboard symmetry — the
    /// planner cap is always `1` today.
    GoalPlannerFired {
        attempt: u32,
        max_runs: u32,
        model_id: String,
    },
    /// Planner subagent wrote a plan file successfully.
    /// `latency_ms` is the spawn-to-write wall clock.
    GoalPlannerCompleted {
        attempt: u32,
        latency_ms: u64,
    },
    /// Planner subagent failed and the harness paused the goal
    /// fail-closed. `reason` is one of the `GOAL_PLANNER_FAIL_CLOSED_*`
    /// discriminator constants in `echo-agent-runtime::session::events`.
    GoalPlannerFailClosed {
        reason: &'static str,
        attempt: u32,
        latency_ms: u64,
    },
    /// Stall-triggered strategist subagent was invoked after
    /// `consecutive_failures` consecutive `NotAchieved` verifications.
    /// Fires once per trigger (at N, 2N, …); pairs with exactly one of
    /// `GoalStrategistCompleted` or `GoalStrategistFailed`. Unlike the
    /// planner the strategist is fail-OPEN — a failure never pauses the
    /// goal. `attempt` is the verifier attempt that triggered it. `every`
    /// is the resolved cadence N, so a configured override is observable.
    GoalStrategistFired {
        attempt: u32,
        consecutive_failures: u32,
        every: u32,
        model_id: String,
    },
    /// Strategist subagent wrote a strategy note successfully.
    /// `latency_ms` is the spawn-to-write wall clock.
    GoalStrategistCompleted {
        attempt: u32,
        consecutive_failures: u32,
        latency_ms: u64,
    },
    /// Strategist subagent failed; the harness logged it and continued
    /// the normal loop (fail-OPEN — the goal is NOT paused). `reason` is
    /// one of the `GOAL_STRATEGIST_FAILED_*` discriminator constants in
    /// `echo-agent-runtime::session::events`.
    GoalStrategistFailed {
        reason: &'static str,
        attempt: u32,
        consecutive_failures: u32,
        latency_ms: u64,
    },
    /// The plan.md-safety guard could not restore the verifier-judged
    /// contract to its pre-strategist bytes (a write/remove failed, or a
    /// symlink was planted at the path). The contract may be corrupted —
    /// surfaced so it is observable rather than a silent `warn!`. `reason`
    /// is one of the `GOAL_STRATEGIST_RESTORE_*` discriminator constants in
    /// `echo-agent-runtime::session::events`.
    GoalStrategistContractRestoreFailed {
        reason: &'static str,
        attempt: u32,
    },
    /// Goal summarizer subagent was invoked ONCE after the goal was
    /// verified-achieved (real `Achieved`, not the infra fail-open), to
    /// generate the closing user-facing summary. Pairs with exactly one of
    /// `GoalSummarizerCompleted` or `GoalSummarizerFailOpen`. Fail-OPEN — a
    /// failure never blocks completion. `attempt` is the achieving verifier
    /// attempt; `model_id` is the inherited session model.
    GoalSummarizerFired {
        attempt: u32,
        model_id: String,
    },
    /// Summarizer returned a non-empty summary; the harness surfaced it as the
    /// goal turn's closing message. `latency_ms` is the spawn-to-summary wall
    /// clock.
    GoalSummarizerCompleted {
        attempt: u32,
        latency_ms: u64,
    },
    /// Summarizer failed (transport / runtime / cancel / empty output); the
    /// harness skipped the closing summary and completed the goal normally
    /// (fail-OPEN — completion is never blocked). `reason` is one of the
    /// `GOAL_SUMMARIZER_FAIL_OPEN_*` discriminator constants in
    /// `echo-agent-runtime::session::events`.
    GoalSummarizerFailOpen {
        reason: &'static str,
        attempt: u32,
        latency_ms: u64,
    },

    /// A `/goal` subagent role (planner, strategist, or a skeptic index)
    /// committed to an explicit model+toolset selection. `role` is one of
    /// `planner|strategist|skeptic`; `skeptic_idx` is set only for the
    /// skeptic panel. `source` is the resolution provenance: a
    /// committed explicit pair is always `remote` (the only non-inherit
    /// source); `default`/kill-switch resolutions inherit the current
    /// model and do not emit this event. Emitted once per role/skeptic-
    /// index when an explicit selection is committed.
    GoalRoleModelResolved {
        role: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        skeptic_idx: Option<u32>,
        model_id: String,
        agent_type: String,
        source: &'static str,
    },
    /// A `/goal` subagent role fell open to the current model because its
    /// configured pair was unusable. `role` is one of
    /// `planner|strategist|skeptic`; `skeptic_idx` is set only for the
    /// skeptic panel. `reason` is one of the
    /// `GOAL_ROLE_MODEL_FAIL_OPEN_*` discriminator constants in
    /// `echo-agent-runtime::session::events`. Fail-open never pauses the goal.
    GoalRoleModelFailOpen {
        role: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        skeptic_idx: Option<u32>,
        reason: &'static str,
    },

    /// One skeptic in the adversarial panel returned a verdict. Fires
    /// `N` times per verification stage (where N is
    /// `goal_verifier_count`). `confidence` is the JSON `confidence`
    /// field; the wire vocabulary is `high|medium|low|unknown`.
    /// `latency_ms` is the per-skeptic spawn-to-verdict wall clock —
    /// dashboards can surface slow outliers even though the panel-
    /// level emission is batched via `join_all`.
    GoalVerifierSkepticVerdict {
        attempt: u32,
        skeptic_idx: u32,
        refuted: bool,
        confidence: &'static str,
        latency_ms: u64,
    },
    /// Aggregate verdict across all N skeptics. `refuted_count` /
    /// `total` is the majority-refute fraction; `achieved` is the
    /// stage's final verdict (true ⇒ survives, false ⇒ majority-refute).
    GoalVerifierAggregateVerdict {
        attempt: u32,
        refuted_count: u32,
        total: u32,
        achieved: bool,
    },
    /// The stop-detector matched a known bail/hand-off/verdict
    /// pattern in the LAST paragraph of the assistant's turn-final
    /// text while the goal stayed `Active` with pending todos. The
    /// harness defeated the premature stop by queuing the bail-specific
    /// continuation reminder; this event records the matched pattern
    /// label so dashboards can audit precision/recall of the regex
    /// panel. `pattern` is one of the stable labels
    /// enumerated by
    /// `echo-agent-runtime::session::goal_stop_detector::PATTERN_LABELS`;
    /// the source-string provenance for each label is pinned by the
    /// adjacent `STOP_REGEX_SOURCES` table.
    ///
    /// Under-counts by design: fires only when a fresh bail continuation
    /// is queued. If a classifier-rejection nudge is already pending, the
    /// shared idempotency gate suppresses both the duplicate push and
    /// JSON-RPC message and was skipped instead of tearing down the
    /// this event, so dashboards see a lower bound.
    GoalPrematureStopDetected {
        pattern: &'static str,
    },

    // ── MCP Diagnostics ──────────────────────────────────────────
    McpConfigResolved {
        servers: Vec<McpConfigServer>,
        disabled: Vec<String>,
    },
    McpManagedConfigResult {
        server_count: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    #[serde(rename = "mcp_oauth_discovery_timeout")]
    McpOAuthDiscoveryTimeout {
        server_name: String,
        url: String,
    },
    /// Verdict of the anonymous-access tie-break after an inconclusive OAuth probe:
    /// `accepted`, `auth_challenged`, or `unreachable`.
    #[serde(rename = "mcp_oauth_probe_resolved")]
    McpOAuthProbeResolved {
        server_name: String,
        verdict: String,
    },
    McpServerStarting {
        server_name: String,
        transport: String,
        target: String,
        timeout_sec: u64,
    },
    McpServerConnected {
        server_name: String,
        transport: String,
        tool_count: u32,
        duration_ms: u64,
        tools: Vec<String>,
    },
    McpServerFailed {
        server_name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        transport: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        target: Option<String>,
        error_type: McpErrorCategory,
        error_message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        timeout_sec: Option<u64>,
    },
    McpToolRegistrationFailed {
        server_name: String,
        tool_name: String,
        error: String,
    },
    McpInitCompleted {
        total_servers: u32,
        succeeded: u32,
        failed: u32,
        auth_required: u32,
        total_tools: u32,
        duration_ms: u64,
        is_reinit: bool,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        failed_servers: Vec<String>,
    },
    McpInitCancelled {
        reason: String,
    },
    McpToolCallStarted {
        server_name: String,
        tool_name: String,
        call_id: String,
        timeout_sec: u64,
    },
    McpToolCallCompleted {
        server_name: String,
        tool_name: String,
        call_id: String,
        duration_ms: u64,
        success: bool,
        is_timeout: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        reconnect_attempted: bool,
        auth_retry_attempted: bool,
    },
    McpTransportError {
        server_name: String,
        tool_name: String,
        error: String,
    },
    /// A line on an MCP stdio server's stdout could not be decoded as a
    /// transport. Surfaces the otherwise-invisible "connector shows but
    /// doesn't work" case (a server logging to stdout, a JSON-RPC batch
    /// array, or an off-spec response). Distinct from `McpTransportError`,
    /// environment; either the orchestrator called
    /// which is a per-tool-call transport failure.
    McpTransportDecodeError {
        server_name: String,
        error: String,
        /// Truncated copy of the offending line, for diagnosis.
        sample: String,
    },
    McpTransportReconnect {
        server_name: String,
        success: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    McpAuthRetry {
        server_name: String,
        trigger: String,
        success: bool,
    },
    McpHealthCheck {
        server_name: String,
        healthy: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        client_state: Option<String>,
    },
    McpServerToggled {
        server_name: String,
        enabled: bool,
    },
}

/// Who emitted a [`Event::ToolCompleted`] row.
///
/// Wire: shell is omitted (legacy empty/`source` absent); workspace is
/// `"workspace"`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCompletedSource {
    /// Shell dispatch clock — join against these.
    #[default]
    Shell,
    /// Workspace hub/proxy hop clock.
    Workspace,
}

impl ToolCompletedSource {
    pub fn is_shell(&self) -> bool {
        matches!(self, Self::Shell)
    }
}

/// Where a mid-turn interjection originated. Drives the `source` field on
/// [`Event::Interjected`].
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InterjectionSource {
    /// Direct `echo.agent/interject` while a turn was running (Ctrl+Enter).
    Direct,
    /// A queued (not-yet-running) prompt promoted into the running turn via
    /// `InterjectQueuedPrompt` (queue "send now").
    Queue,
}

/// The user-redirect mechanism behind an event — the shared discriminator that
/// lets the trace pipeline query every user steer through one field. Present on
/// [`Event::Interjected`] (always [`RedirectKind::Interjection`]) and, for the
/// next turn after a Ctrl+C / Esc abort, on [`Event::TurnStarted`]
/// ([`RedirectKind::CancelThenSend`] / [`RedirectKind::QueuedAfterCancel`]).
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RedirectKind {
    /// Mid-turn interjection — Ctrl+O / `echo.agent/interject`, or "Send now" on a
    /// queued row. The turn keeps running; nothing is cancelled.
    Interjection,
    /// The turn was aborted (Ctrl+C / Esc) and the user then typed and sent a
    /// fresh prompt as the next turn.
    CancelThenSend,
    /// The turn was aborted (Ctrl+C / Esc) while a prompt sat queued behind it;
    /// that queued prompt was promoted as the next turn.
    QueuedAfterCancel,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpErrorCategory {
    SpawnFailed,
    Timeout,
    HandshakeFailed,
    AuthRequired,
    ClientError,
}

/// Server entry in `McpConfigResolved`.
#[derive(Debug, Clone, Serialize)]
pub struct McpConfigServer {
    pub name: String,
    pub transport: String,
    pub source: String,
}

/// Telemetry mirror of `echo-agent-runtime`'s `GoalClassifierVerdict`. Two
/// crates due to the orphan rule; the conversion lives in
/// `echo-agent-runtime/src/session/events.rs`.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalClassifierVerdictTelemetry {
    Achieved,
    NotAchieved,
}

/// Telemetry mirror of `echo-agent-runtime`'s `GoalPauseReason`. The two types
/// live in separate crates (orphan rule); the conversion lives in
/// `echo-agent-runtime/src/session/events.rs`.
///
/// **Invariant:** when adding a new variant to either side, add the
/// matching variant here so the compiler-enforced `From` impl on the
/// shell side catches the drift at build time.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalPauseReasonTelemetry {
    User,
    BackOff,
    /// Verification stage saw no fingerprint change in the flagged gaps
    /// across consecutive attempts and auto-paused before the run cap.
    NoProgress,
    /// Verification determined the goal is not achievable in this
    /// `update_goal(blocked_reason: ...)`, or every refuter classified
    /// `update_goal(blocked_reason: ...)`, or every refuter classified
    /// its gap as a contradiction / unverifiable blocker.
    Verification,
    /// Turn finished with `PromptTurnResult::Err` (infrastructure failure).
    Infra,
}

/// Outcome of a single tool call. More granular than a boolean -- distinguishes
/// between tools that executed vs tools that were never run.
#[derive(Debug, Clone, Copy, Serialize, strum::IntoStaticStr)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum ToolOutcome {
    /// Tool executed and returned a result.
    Success,
    /// Tool executed but returned an error.
    Error,
    /// User rejected the permission prompt.
    PermissionRejected,
    /// User cancelled the permission prompt (Cmd+C).
    PermissionCancelled,
    /// User provided a followup message instead of approving.
    Followup,
    /// A user-configured hook blocked execution.
    HookDenied,
    /// Tool not found or arguments couldn't be parsed.
    InvalidTool,
    /// Tool was running when the turn was cancelled (Cmd+C).
    Cancelled,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    WaitingForModel,
    StreamingText,
    StreamingReasoning,
    ToolExecution,
    PermissionPrompt,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionRelationship {
    Primary,
    #[allow(dead_code)]
    Subagent,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOutcomeLabel {
    Completed,
    Cancelled,
    Error,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    Allow,
    Deny,
    Cancelled,
    Followup,
}

// `Deserialize`/`PartialEq`/`Eq`/`Hash` let the workspace decode
// `cancellation_category` strings back into this enum. `snake_case` keeps the
// wire form identical, so adding `Deserialize` doesn't change serialization.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CancellationCategory {
    HookDenied,
    PermissionRejected,
    PermissionCancelled,
    MidTurnAbort,
}

// Note: `From<&permission::Decision> for PermissionDecision` crosses the
// crate boundary (orphan rule) and lives in
// `echo-agent-runtime/src/session/events.rs`.

#[cfg(test)]
mod tests {
    use super::*;

    /// Every variant must survive a `to_value` -> `from_value` round-trip.
    #[test]
    fn cancellation_category_round_trips_every_variant() {
        for variant in [
            CancellationCategory::HookDenied,
            CancellationCategory::PermissionRejected,
            CancellationCategory::PermissionCancelled,
            CancellationCategory::MidTurnAbort,
        ] {
            let value = serde_json::to_value(variant).unwrap();
            let decoded: CancellationCategory = serde_json::from_value(value).unwrap();
            assert_eq!(decoded, variant, "{variant:?} must round-trip");
        }
    }

    /// Serialization is unchanged by the added derives (bare snake_case strings).
    #[test]
    fn cancellation_category_serializes_snake_case() {
        for (variant, expected) in [
            (CancellationCategory::HookDenied, "\"hook_denied\""),
            (
                CancellationCategory::PermissionRejected,
                "\"permission_rejected\"",
            ),
            (
                CancellationCategory::PermissionCancelled,
                "\"permission_cancelled\"",
            ),
            (CancellationCategory::MidTurnAbort, "\"mid_turn_abort\""),
        ] {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, expected, "{variant:?} must serialize to {expected}");
        }
    }

    #[test]
    fn tool_completed_source_omits_shell_writes_workspace() {
        let shell = serde_json::to_value(Event::ToolCompleted {
            tool_name: "bash".into(),
            duration_ms: 10,
            outcome: ToolOutcome::Success,
            tool_call_id: "c1".into(),
            source: ToolCompletedSource::Shell,
        })
        .unwrap();
        assert!(shell.get("source").is_none());

        let workspace = serde_json::to_value(Event::ToolCompleted {
            tool_name: "bash".into(),
            duration_ms: 10,
            outcome: ToolOutcome::Success,
            tool_call_id: "c1".into(),
            source: ToolCompletedSource::Workspace,
        })
        .unwrap();
        assert_eq!(workspace["source"], "workspace");
    }

    #[test]
    fn interjected_event_serializes_tag_source_and_count() {
        let ev = Event::Interjected {
            source: InterjectionSource::Direct,
            image_count: 2,
            redirect_kind: RedirectKind::Interjection,
        };
        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["type"], "interjected");
        assert_eq!(v["source"], "direct");
        assert_eq!(v["image_count"], 2);
        // Shared discriminator: always present on interjected events.
        assert_eq!(v["redirect_kind"], "interjection");

        let queue = serde_json::to_value(Event::Interjected {
            source: InterjectionSource::Queue,
            image_count: 0,
            redirect_kind: RedirectKind::Interjection,
        })
        .unwrap();
        assert_eq!(queue["source"], "queue");
        assert_eq!(queue["image_count"], 0);
        assert_eq!(queue["redirect_kind"], "interjection");
    }

    #[test]
    fn redirect_kind_serializes_snake_case() {
        for (variant, expected) in [
            (RedirectKind::Interjection, "\"interjection\""),
            (RedirectKind::CancelThenSend, "\"cancel_then_send\""),
            (RedirectKind::QueuedAfterCancel, "\"queued_after_cancel\""),
        ] {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, expected, "{variant:?} must serialize to {expected}");
        }
    }

    #[test]
    fn turn_started_redirect_kind_present_when_set_omitted_when_none() {
        let with_kind = serde_json::to_value(Event::TurnStarted {
            session_id: "s".into(),
            turn_number: 2,
            model_id: "echo-agent-4".into(),
            yolo_mode: false,
            conversation_message_count: 3,
            session_relationship: SessionRelationship::Primary,
            schema_version: EVENT_SCHEMA_VERSION.into(),
            redirect_kind: Some(RedirectKind::QueuedAfterCancel),
        })
        .unwrap();
        assert_eq!(with_kind["type"], "turn_started");
        assert_eq!(with_kind["redirect_kind"], "queued_after_cancel");

        let normal = serde_json::to_value(Event::TurnStarted {
            session_id: "s".into(),
            turn_number: 1,
            model_id: "echo-agent-4".into(),
            yolo_mode: false,
            conversation_message_count: 0,
            session_relationship: SessionRelationship::Primary,
            schema_version: EVENT_SCHEMA_VERSION.into(),
            redirect_kind: None,
        })
        .unwrap();
        assert!(
            normal.get("redirect_kind").is_none(),
            "redirect_kind must be omitted on a normal turn, got {normal}"
        );
    }

    #[test]
    fn goal_pause_reason_telemetry_serializes_snake_case() {
        for (variant, expected) in [
            (GoalPauseReasonTelemetry::User, "\"user\""),
            (GoalPauseReasonTelemetry::BackOff, "\"back_off\""),
            (GoalPauseReasonTelemetry::NoProgress, "\"no_progress\""),
            (GoalPauseReasonTelemetry::Verification, "\"verification\""),
            (GoalPauseReasonTelemetry::Infra, "\"infra\""),
        ] {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, expected, "{variant:?} must serialize to {expected}");
        }
    }

    #[test]
    fn goal_strategist_fired_serializes_cadence_field() {
        // `every` must serialize as a plain number on the wire.
        let ev = Event::GoalStrategistFired {
            attempt: 2,
            consecutive_failures: 6,
            every: 3,
            model_id: "echo-agent-4".to_string(),
        };
        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["type"], "goal_strategist_fired");
        assert_eq!(v["attempt"], 2);
        assert_eq!(v["consecutive_failures"], 6);
        assert_eq!(v["every"], 3);
        assert_eq!(v["model_id"], "echo-agent-4");
    }

    #[test]
    fn goal_summarizer_events_serialize_tag_and_fields() {
        let fired = Event::GoalSummarizerFired {
            attempt: 2,
            model_id: "echo-agent-4".to_string(),
        };
        let v = serde_json::to_value(&fired).unwrap();
        assert_eq!(v["type"], "goal_summarizer_fired");
        assert_eq!(v["attempt"], 2);
        assert_eq!(v["model_id"], "echo-agent-4");

        let completed = Event::GoalSummarizerCompleted {
            attempt: 2,
            latency_ms: 42,
        };
        let v = serde_json::to_value(&completed).unwrap();
        assert_eq!(v["type"], "goal_summarizer_completed");
        assert_eq!(v["attempt"], 2);
        assert_eq!(v["latency_ms"], 42);

        let failed = Event::GoalSummarizerFailOpen {
            reason: "transport",
            attempt: 2,
            latency_ms: 7,
        };
        let v = serde_json::to_value(&failed).unwrap();
        assert_eq!(v["type"], "goal_summarizer_fail_open");
        assert_eq!(v["reason"], "transport");
        assert_eq!(v["attempt"], 2);
        assert_eq!(v["latency_ms"], 7);
    }

    #[test]
    fn goal_role_model_resolved_serializes_tag_and_fields() {
        let ev = Event::GoalRoleModelResolved {
            role: "skeptic",
            skeptic_idx: Some(2),
            model_id: "echo-agent-4".to_string(),
            agent_type: "general-purpose".to_string(),
            source: "remote",
        };
        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["type"], "goal_role_model_resolved");
        assert_eq!(v["role"], "skeptic");
        assert_eq!(v["skeptic_idx"], 2);
        assert_eq!(v["model_id"], "echo-agent-4");
        assert_eq!(v["agent_type"], "general-purpose");
        assert_eq!(v["source"], "remote");
    }

    #[test]
    fn goal_role_model_resolved_omits_skeptic_idx_when_none() {
        let ev = Event::GoalRoleModelResolved {
            role: "planner",
            skeptic_idx: None,
            model_id: "echo-agent-4".to_string(),
            agent_type: "general-purpose".to_string(),
            source: "remote",
        };
        let obj = serde_json::to_value(&ev).unwrap();
        assert!(
            obj.get("skeptic_idx").is_none(),
            "skeptic_idx must be omitted when None, got {obj}"
        );
        assert_eq!(obj["role"], "planner");
    }

    #[test]
    fn goal_role_model_fail_open_serializes_tag_and_fields() {
        let ev = Event::GoalRoleModelFailOpen {
            role: "skeptic",
            skeptic_idx: Some(1),
            reason: "toolset_unavailable",
        };
        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["type"], "goal_role_model_fail_open");
        assert_eq!(v["role"], "skeptic");
        assert_eq!(v["skeptic_idx"], 1);
        assert_eq!(v["reason"], "toolset_unavailable");
    }

    #[test]
    fn goal_role_model_fail_open_omits_skeptic_idx_when_none() {
        let ev = Event::GoalRoleModelFailOpen {
            role: "strategist",
            skeptic_idx: None,
            reason: "model_unauthorized",
        };
        let obj = serde_json::to_value(&ev).unwrap();
        assert!(
            obj.get("skeptic_idx").is_none(),
            "skeptic_idx must be omitted when None, got {obj}"
        );
        assert_eq!(obj["type"], "goal_role_model_fail_open");
        assert_eq!(obj["role"], "strategist");
        assert_eq!(obj["reason"], "model_unauthorized");
    }
}
