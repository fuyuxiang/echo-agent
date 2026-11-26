//! Re-exports of the crate-internal event types that live in
//! `echo-agent-session-events`. The orphan-rule items (`From<&permission::Decision>`
//! and the doom-loop categorizer) stay here since they need shell-local
//! types.

pub(crate) use echo_agent_session_events::tracker::EventTracker;
pub(crate) use echo_agent_session_events::types::{
    CancellationCategory, EVENT_SCHEMA_VERSION, Event, GoalClassifierVerdictTelemetry,
    GoalPauseReasonTelemetry, InterjectionSource, Phase, RedirectKind, SessionRelationship,
    ToolCompletedSource, ToolOutcome, TurnOutcomeLabel,
};

// ── Laziness detector (Layer 3) discriminator vocabulary ─────────────
//
// Single source of truth for the `category` field on
// `Event::LazinessClassifierFired` / `LazinessNudgeFired` and the
// `reason` field on `Event::LazinessClassifierAborted`. The producer
// wraps the category strings in `LazinessCategory::as_const_str()`
// (acp_session.rs) for compile-time closure over the set, and the
// abort reasons are emitted only via the `LAZINESS_ABORT_*` consts
// below — no string literals at any producer site.
//
// The category strings are also the lowercase serde representation of
// the classifier's JSON output, so producer (classifier prompt) and
// consumer (Rust enum + telemetry) share one vocabulary.

/// Stalled — the model emitted prose narration claiming progress
/// without any real tool calls.
pub(crate) const LAZINESS_STALLED_NARRATION: &str = "stalled_narration";

/// Stalled — the model asked the user for permission to continue a
/// task that is already in flight.
pub(crate) const LAZINESS_STALLED_PERMISSION_ASKING: &str = "stalled_permission_asking";

/// Stalled — the model has no todo list but a multi-step task is
/// clearly in flight (no active plan tool calls despite a complex
/// pending task).
pub(crate) const LAZINESS_STALLED_NO_TODOS_BUT_TASK_IN_FLIGHT: &str =
    "stalled_no_todos_but_task_in_flight";

/// Stalled — the agent declared completion/success but the transcript
/// shows substantive claims unbacked by tool-call evidence (e.g. claims
/// running `make test` but no `make` tool_call appears; claims
/// "overnight 8+ hour run" but elapsed time is minutes; claims N review
/// rounds but only M happened).
pub(crate) const LAZINESS_STALLED_FALSE_COMPLETION: &str = "stalled_false_completion";

/// Not stalled — the model has genuinely completed its task.
pub(crate) const LAZINESS_NOT_STALLED_COMPLETE: &str = "not_stalled_complete";

/// Not stalled — the model is correctly waiting on a backgrounded task
/// it cannot drive forward.
pub(crate) const LAZINESS_NOT_STALLED_WAITING_BG: &str = "not_stalled_waiting_on_background";

/// Not stalled — the model is correctly waiting on user input for a
/// genuine ambiguity.
pub(crate) const LAZINESS_NOT_STALLED_WAITING_USER: &str = "not_stalled_waiting_on_user";

/// Aborted because a fresh user prompt arrived before classification
/// completed.
pub(crate) const LAZINESS_ABORT_USER_INPUT: &str = "user_input";

/// Aborted because the user switched models mid-classification.
pub(crate) const LAZINESS_ABORT_MODEL_SWITCH: &str = "model_switch";

/// Aborted because the classifier exceeded its wall-clock budget.
pub(crate) const LAZINESS_ABORT_TIMEOUT: &str = "timeout";

/// Aborted because the classifier response failed to parse after the
/// tolerant parser exhausted all three passes.
pub(crate) const LAZINESS_ABORT_CLASSIFIER_ERROR: &str = "classifier_error";

// Compile-time guard against an accidentally-empty const breaking the
// dashboards' group-by. `const _: () = assert!(…)` fires at build
// time, not at first test run.
#[allow(clippy::const_is_empty)]
const _: () = assert!(
    !LAZINESS_STALLED_NARRATION.is_empty()
        && !LAZINESS_STALLED_PERMISSION_ASKING.is_empty()
        && !LAZINESS_STALLED_NO_TODOS_BUT_TASK_IN_FLIGHT.is_empty()
        && !LAZINESS_STALLED_FALSE_COMPLETION.is_empty()
        && !LAZINESS_NOT_STALLED_COMPLETE.is_empty()
        && !LAZINESS_NOT_STALLED_WAITING_BG.is_empty()
        && !LAZINESS_NOT_STALLED_WAITING_USER.is_empty()
        && !LAZINESS_ABORT_USER_INPUT.is_empty()
        && !LAZINESS_ABORT_MODEL_SWITCH.is_empty()
        && !LAZINESS_ABORT_TIMEOUT.is_empty()
        && !LAZINESS_ABORT_CLASSIFIER_ERROR.is_empty(),
    "Laziness discriminator consts must be non-empty",
);

/// Closed set of categories the Layer-3 classifier can return.
///
/// Mirrors the JSON schema in the classifier prompt; `serde` uses the
/// `LAZINESS_*` strings above as the wire format (snake_case). The
/// `as_const_str()` mapping is exhaustive over the variants — the
/// `laziness_category_round_trip` test asserts the variant ↔ const
/// pairing is one-to-one. Mirrors the `TodoGateReason` pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LazinessCategory {
    StalledNarration,
    StalledPermissionAsking,
    StalledNoTodosButTaskInFlight,
    StalledFalseCompletion,
    NotStalledComplete,
    NotStalledWaitingOnBackground,
    NotStalledWaitingOnUser,
}

impl LazinessCategory {
    /// Returns the `LAZINESS_*` string for this variant. Exhaustive
    /// `match` — adding a variant forces a new const + arm.
    pub(crate) fn as_const_str(self) -> &'static str {
        match self {
            Self::StalledNarration => LAZINESS_STALLED_NARRATION,
            Self::StalledPermissionAsking => LAZINESS_STALLED_PERMISSION_ASKING,
            Self::StalledNoTodosButTaskInFlight => LAZINESS_STALLED_NO_TODOS_BUT_TASK_IN_FLIGHT,
            Self::StalledFalseCompletion => LAZINESS_STALLED_FALSE_COMPLETION,
            Self::NotStalledComplete => LAZINESS_NOT_STALLED_COMPLETE,
            Self::NotStalledWaitingOnBackground => LAZINESS_NOT_STALLED_WAITING_BG,
            Self::NotStalledWaitingOnUser => LAZINESS_NOT_STALLED_WAITING_USER,
        }
    }

    /// Four variants count as "stalled" and are eligible for a nudge.
    pub(crate) fn is_stalled(self) -> bool {
        match self {
            Self::StalledNarration
            | Self::StalledPermissionAsking
            | Self::StalledNoTodosButTaskInFlight
            | Self::StalledFalseCompletion => true,
            Self::NotStalledComplete
            | Self::NotStalledWaitingOnBackground
            | Self::NotStalledWaitingOnUser => false,
        }
    }

    /// Every variant of this enum. Used by the producer-consistency
    /// tests to enumerate the closed set rather than a hand-coded
    /// array that would silently drift if a new variant were added.
    /// `as_const_str` and `is_stalled` are compiler-enforced
    /// exhaustive matches; the `_assert_exhaustive` helper below is
    /// the cheap drift guard that forces this list to stay in sync
    /// (adding any variant without listing it here is a compile error).
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "exhaustiveness guard for tests; remove expect if used in prod"
        )
    )]
    pub(crate) const fn all() -> &'static [Self] {
        // The exhaustive match in this helper wires the array length
        // to the variant count: adding a variant breaks `match` AND
        // the array's expected count.
        const fn _assert_exhaustive(c: LazinessCategory) {
            match c {
                LazinessCategory::StalledNarration => (),
                LazinessCategory::StalledPermissionAsking => (),
                LazinessCategory::StalledNoTodosButTaskInFlight => (),
                LazinessCategory::StalledFalseCompletion => (),
                LazinessCategory::NotStalledComplete => (),
                LazinessCategory::NotStalledWaitingOnBackground => (),
                LazinessCategory::NotStalledWaitingOnUser => (),
            }
        }
        &[
            Self::StalledNarration,
            Self::StalledPermissionAsking,
            Self::StalledNoTodosButTaskInFlight,
            Self::StalledFalseCompletion,
            Self::NotStalledComplete,
            Self::NotStalledWaitingOnBackground,
            Self::NotStalledWaitingOnUser,
        ]
    }
}

// ── TodoGate discriminator vocabulary ─────────────────────────────────
//
// Source of truth for the `reason` field on `Event::TodoGateFired`.
// Producer wraps these via `TodoGateReason::as_str()` (acp_session.rs).

/// The TodoGate fired because a content-only turn ended with one or more
/// pending or unbacked in-progress todos.
pub(crate) const TODO_GATE_IN_FLIGHT: &str = "in_flight";

// Compile-time non-empty check — empty would silently break dashboards' group-by.
#[allow(clippy::const_is_empty)]
const _: () = assert!(
    !TODO_GATE_IN_FLIGHT.is_empty(),
    "TodoGate discriminator consts must be non-empty",
);

/// Map a [`CancellationCategory`] to the [`PriorTurnInterrupt`] marker stamped
/// onto the *next* real user turn — but only for the user-interruption subset.
/// Automatic terminations (doom-loop, hook-denied) return `None`: they are not
/// user interruptions, so the follow-up user message carries no marker.
/// Exhaustive `match` (no wildcard) so a new `CancellationCategory` forces an
/// explicit decision here. (Interjection has no `CancellationCategory` — it
/// never cancels a turn — so it is mapped directly at the drain site.)
pub(crate) fn prior_turn_interrupt_from_cancellation(
    category: CancellationCategory,
) -> Option<echo_agent_sampling_types::PriorTurnInterrupt> {
    use echo_agent_sampling_types::PriorTurnInterrupt;
    match category {
        CancellationCategory::MidTurnAbort => Some(PriorTurnInterrupt::MidTurnAbort),
        CancellationCategory::PermissionRejected => Some(PriorTurnInterrupt::PermissionRejected),
        CancellationCategory::PermissionCancelled => Some(PriorTurnInterrupt::PermissionCancelled),
        CancellationCategory::HookDenied => None,
    }
}

// ── GoalClassifier discriminator vocabulary ───────────────────────────
//
// Single source of truth for the `reason` field on
// `Event::GoalClassifierFailOpen` / `Event::GoalClassifierFailClosed`.
// The producer wraps the reason strings via the `as_const_str()`
// methods on `GoalClassifierFailOpenReason` / `GoalClassifierFailClosedReason`
// so the variant set is compiler-enforced; the consts here are the wire
// vocabulary the dashboards group by.

/// Fail-open — legacy wire string; runner does not emit.
pub(crate) const GOAL_CLASSIFIER_FAIL_OPEN_TIMEOUT: &str = "timeout";

/// Fail-open — the subagent spawn returned an error (channel closed,
/// coordinator rejected, etc.). INFRA-class.
pub(crate) const GOAL_CLASSIFIER_FAIL_OPEN_SAMPLER_ERROR: &str = "sampler_error";

/// Fail-open — the user aborted the run mid-classification.
pub(crate) const GOAL_CLASSIFIER_FAIL_OPEN_ABORTED: &str = "aborted";

/// Fail-open — the harness could not pre-create the details-file parent
/// directory or otherwise prepare the on-disk state.
pub(crate) const GOAL_CLASSIFIER_FAIL_OPEN_FILE_WRITE_FAILED: &str = "file_write_failed";

/// Fail-open — the goal was no longer Active when the runner resolved
/// its inputs (status changed during the spawn).
pub(crate) const GOAL_CLASSIFIER_FAIL_OPEN_GOAL_NOT_ACTIVE: &str = "goal_not_active_at_resolve";

/// Fail-closed — a second `update_goal(completed: true)` arrived
/// while a verification stage was already in flight for this goal.
/// Caller must not double-spawn.
pub(crate) const GOAL_CLASSIFIER_FAIL_CLOSED_CONCURRENT: &str = "concurrent_in_flight";

/// Fail-closed — the deferred-completion queue overflowed and the
/// oldest entry was dropped to make room for a newer one. PARSE-class
/// analogue of `ConcurrentInFlight` for the runaway-`completed: true`
/// path.
pub(crate) const GOAL_CLASSIFIER_FAIL_CLOSED_PENDING_QUEUE_FULL: &str = "pending_queue_full";

// Compile-time non-empty guard (mirrors the laziness const block).
#[allow(clippy::const_is_empty)]
const _: () = assert!(
    !GOAL_CLASSIFIER_FAIL_OPEN_TIMEOUT.is_empty()
        && !GOAL_CLASSIFIER_FAIL_OPEN_SAMPLER_ERROR.is_empty()
        && !GOAL_CLASSIFIER_FAIL_OPEN_ABORTED.is_empty()
        && !GOAL_CLASSIFIER_FAIL_OPEN_FILE_WRITE_FAILED.is_empty()
        && !GOAL_CLASSIFIER_FAIL_OPEN_GOAL_NOT_ACTIVE.is_empty()
        && !GOAL_CLASSIFIER_FAIL_CLOSED_CONCURRENT.is_empty()
        && !GOAL_CLASSIFIER_FAIL_CLOSED_PENDING_QUEUE_FULL.is_empty(),
    "GoalClassifier discriminator consts must be non-empty",
);

/// Closed set of fail-open reasons. Variants are INFRA-class:
/// the harness could not verify the verdict and treats the goal as
/// achieved to avoid blocking the user on an internal failure.
///
/// `Timeout` is a legacy wire string production no longer emits; kept so
/// dashboards joining on the historical string still parse cleanly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GoalClassifierFailOpenReason {
    /// Legacy wire only — no production emitter today.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "legacy wire string for dashboard joins; not emitted"
        )
    )]
    Timeout,
    SamplerError,
    /// Retained for wire/dashboard compatibility; no emitter today.
    #[expect(dead_code, reason = "wire vocabulary; no production emitter today")]
    Aborted,
    FileWriteFailed,
    GoalNotActiveAtResolve,
}

impl GoalClassifierFailOpenReason {
    pub(crate) fn as_const_str(self) -> &'static str {
        match self {
            Self::Timeout => GOAL_CLASSIFIER_FAIL_OPEN_TIMEOUT,
            Self::SamplerError => GOAL_CLASSIFIER_FAIL_OPEN_SAMPLER_ERROR,
            Self::Aborted => GOAL_CLASSIFIER_FAIL_OPEN_ABORTED,
            Self::FileWriteFailed => GOAL_CLASSIFIER_FAIL_OPEN_FILE_WRITE_FAILED,
            Self::GoalNotActiveAtResolve => GOAL_CLASSIFIER_FAIL_OPEN_GOAL_NOT_ACTIVE,
        }
    }
}

/// PARSE-class fail-closed reasons. The verification stage routes
/// per-skeptic parse failures (malformed terminal token, missing
/// verdict JSON) through synthetic refute votes instead of this event
/// vocabulary — so the only live variants here are the drain-path
/// race guards (`ConcurrentInFlight`, `PendingQueueFull`).
///
/// No longer emitted: `MalformedTerminalResponse`, `MissingDetailsFile`,
/// `ChangesCaptureFailed`. A future strict-diff mode (where a `git
/// diff` failure must fail-closed instead of degrading to
/// `(unavailable)`) would re-add `ChangesCaptureFailed` here plus the
/// `GOAL_CLASSIFIER_FAIL_CLOSED_CHANGES_CAPTURE_FAILED` wire constant
/// and the matching `as_const_str` arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GoalClassifierFailClosedReason {
    ConcurrentInFlight,
    PendingQueueFull,
}

impl GoalClassifierFailClosedReason {
    pub(crate) fn as_const_str(self) -> &'static str {
        match self {
            Self::ConcurrentInFlight => GOAL_CLASSIFIER_FAIL_CLOSED_CONCURRENT,
            Self::PendingQueueFull => GOAL_CLASSIFIER_FAIL_CLOSED_PENDING_QUEUE_FULL,
        }
    }
}

// ── GoalPlanner discriminator vocabulary ──────────────────────────────
//
// The planner is fail-CLOSED by design (the opposite of the classifier).
// Every reason here represents a path that pauses the goal — there is no
// fail-open analogue.

/// Planner subagent coordinator channel was unreachable.
pub(crate) const GOAL_PLANNER_FAIL_CLOSED_TRANSPORT: &str = "transport";

/// Planner subagent reported a runtime failure.
pub(crate) const GOAL_PLANNER_FAIL_CLOSED_RUNTIME: &str = "runtime";

/// User aborted the planner mid-run.
pub(crate) const GOAL_PLANNER_FAIL_CLOSED_ABORTED: &str = "aborted";

/// Planner did not write `plan.md` (file missing or empty).
pub(crate) const GOAL_PLANNER_FAIL_CLOSED_MISSING_PLAN: &str = "missing_plan_file";

/// Harness could not pre-create the plan-file parent directory.
pub(crate) const GOAL_PLANNER_FAIL_CLOSED_FILE_WRITE_FAILED: &str = "file_write_failed";

#[allow(clippy::const_is_empty)]
const _: () = assert!(
    !GOAL_PLANNER_FAIL_CLOSED_TRANSPORT.is_empty()
        && !GOAL_PLANNER_FAIL_CLOSED_RUNTIME.is_empty()
        && !GOAL_PLANNER_FAIL_CLOSED_ABORTED.is_empty()
        && !GOAL_PLANNER_FAIL_CLOSED_MISSING_PLAN.is_empty()
        && !GOAL_PLANNER_FAIL_CLOSED_FILE_WRITE_FAILED.is_empty(),
    "GoalPlanner discriminator consts must be non-empty",
);

/// Closed set of planner fail-closed reasons. All variants pause the
/// goal — there is no fail-open variant by design.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GoalPlannerFailClosedReason {
    Transport,
    Runtime,
    Aborted,
    MissingPlan,
    FileWriteFailed,
}

impl GoalPlannerFailClosedReason {
    pub(crate) fn as_const_str(self) -> &'static str {
        match self {
            Self::Transport => GOAL_PLANNER_FAIL_CLOSED_TRANSPORT,
            Self::Runtime => GOAL_PLANNER_FAIL_CLOSED_RUNTIME,
            Self::Aborted => GOAL_PLANNER_FAIL_CLOSED_ABORTED,
            Self::MissingPlan => GOAL_PLANNER_FAIL_CLOSED_MISSING_PLAN,
            Self::FileWriteFailed => GOAL_PLANNER_FAIL_CLOSED_FILE_WRITE_FAILED,
        }
    }
}

// ── GoalStrategist discriminator vocabulary ───────────────────────────
//
// The strategist is fail-OPEN by design (the opposite of the planner).
// Every reason here represents a path that is logged and then ignored —
// the goal keeps running. The strategist is a best-effort advisory
// enhancement, never a gate.

/// Strategist subagent coordinator channel was unreachable.
pub(crate) const GOAL_STRATEGIST_FAILED_TRANSPORT: &str = "transport";

/// Strategist subagent reported a runtime failure.
pub(crate) const GOAL_STRATEGIST_FAILED_RUNTIME: &str = "runtime";

/// User aborted the strategist mid-run.
pub(crate) const GOAL_STRATEGIST_FAILED_ABORTED: &str = "aborted";

/// Strategist did not write the strategy note (file missing or empty).
pub(crate) const GOAL_STRATEGIST_FAILED_MISSING_STRATEGY: &str = "missing_strategy_file";

#[allow(clippy::const_is_empty)]
const _: () = assert!(
    !GOAL_STRATEGIST_FAILED_TRANSPORT.is_empty()
        && !GOAL_STRATEGIST_FAILED_RUNTIME.is_empty()
        && !GOAL_STRATEGIST_FAILED_ABORTED.is_empty()
        && !GOAL_STRATEGIST_FAILED_MISSING_STRATEGY.is_empty(),
    "GoalStrategist discriminator consts must be non-empty",
);

/// Closed set of strategist fail-open reasons. All variants are logged
/// and ignored — the goal is never paused on strategist failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GoalStrategistFailReason {
    Transport,
    Runtime,
    Aborted,
    MissingStrategy,
}

impl GoalStrategistFailReason {
    pub(crate) fn as_const_str(self) -> &'static str {
        match self {
            Self::Transport => GOAL_STRATEGIST_FAILED_TRANSPORT,
            Self::Runtime => GOAL_STRATEGIST_FAILED_RUNTIME,
            Self::Aborted => GOAL_STRATEGIST_FAILED_ABORTED,
            Self::MissingStrategy => GOAL_STRATEGIST_FAILED_MISSING_STRATEGY,
        }
    }
}

/// The plan.md restore-guard could not put the contract back. A
/// write to plan.md failed.
pub(crate) const GOAL_STRATEGIST_RESTORE_WRITE_FAILED: &str = "write_failed";

/// Removing a strategist-created plan.md failed.
pub(crate) const GOAL_STRATEGIST_RESTORE_REMOVE_FAILED: &str = "remove_failed";

/// plan.md is (or became) a symlink — refused to restore through it
/// (treated as tampering).
pub(crate) const GOAL_STRATEGIST_RESTORE_SYMLINK_TAMPER: &str = "symlink_tamper";

#[allow(clippy::const_is_empty)]
const _: () = assert!(
    !GOAL_STRATEGIST_RESTORE_WRITE_FAILED.is_empty()
        && !GOAL_STRATEGIST_RESTORE_REMOVE_FAILED.is_empty()
        && !GOAL_STRATEGIST_RESTORE_SYMLINK_TAMPER.is_empty(),
    "GoalStrategist restore discriminator consts must be non-empty",
);

/// Why the plan.md-safety guard could not guarantee the contract. Drives
/// the `reason` on `Event::GoalStrategistContractRestoreFailed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GoalStrategistRestoreFailReason {
    WriteFailed,
    RemoveFailed,
    SymlinkTamper,
}

impl GoalStrategistRestoreFailReason {
    pub(crate) fn as_const_str(self) -> &'static str {
        match self {
            Self::WriteFailed => GOAL_STRATEGIST_RESTORE_WRITE_FAILED,
            Self::RemoveFailed => GOAL_STRATEGIST_RESTORE_REMOVE_FAILED,
            Self::SymlinkTamper => GOAL_STRATEGIST_RESTORE_SYMLINK_TAMPER,
        }
    }
}

// ── GoalSummarizer discriminator vocabulary ───────────────────────────
//
// The summarizer is fail-OPEN by design: it runs ONCE after the goal is
// already verified-achieved, so every reason here is logged and ignored —
// goal completion is never blocked, paused, or un-achieved.

/// Summarizer subagent coordinator channel was unreachable.
pub(crate) const GOAL_SUMMARIZER_FAIL_OPEN_TRANSPORT: &str = "transport";

/// Summarizer subagent reported a runtime failure.
pub(crate) const GOAL_SUMMARIZER_FAIL_OPEN_RUNTIME: &str = "runtime";

/// User aborted the summarizer mid-run.
pub(crate) const GOAL_SUMMARIZER_FAIL_OPEN_ABORTED: &str = "aborted";

/// Summarizer returned an empty (whitespace-only) summary — nothing to
/// surface, so the closing message is skipped.
pub(crate) const GOAL_SUMMARIZER_FAIL_OPEN_EMPTY_SUMMARY: &str = "empty_summary";

#[allow(clippy::const_is_empty)]
const _: () = assert!(
    !GOAL_SUMMARIZER_FAIL_OPEN_TRANSPORT.is_empty()
        && !GOAL_SUMMARIZER_FAIL_OPEN_RUNTIME.is_empty()
        && !GOAL_SUMMARIZER_FAIL_OPEN_ABORTED.is_empty()
        && !GOAL_SUMMARIZER_FAIL_OPEN_EMPTY_SUMMARY.is_empty(),
    "GoalSummarizer discriminator consts must be non-empty",
);

/// Closed set of summarizer fail-open reasons. All variants are logged
/// and ignored — the goal is never un-achieved on summarizer failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GoalSummarizerFailReason {
    Transport,
    Runtime,
    Aborted,
    EmptySummary,
}

impl GoalSummarizerFailReason {
    pub(crate) fn as_const_str(self) -> &'static str {
        match self {
            Self::Transport => GOAL_SUMMARIZER_FAIL_OPEN_TRANSPORT,
            Self::Runtime => GOAL_SUMMARIZER_FAIL_OPEN_RUNTIME,
            Self::Aborted => GOAL_SUMMARIZER_FAIL_OPEN_ABORTED,
            Self::EmptySummary => GOAL_SUMMARIZER_FAIL_OPEN_EMPTY_SUMMARY,
        }
    }
}

// ── GoalRoleModel discriminator vocabulary ────────────────────────────
//
// Source of truth for the `reason` field on `Event::GoalRoleModelFailOpen`.
// Per-role model selection is fail-OPEN by design: a bad/unauthorized
// model, an unusable toolset, or a harness whose flavor the subagent system
// can't represent degrades that role (or skeptic index) to the current model +
// session harness — the goal is never paused.
// The producer (the goal spawn wiring / `resolve_goal_role_override` +
// `spawn_with_fail_open_retry`) wraps the reason strings via
// `GoalRoleModelFailOpenReason::as_const_str`; the consts here are the
// wire vocabulary dashboards group by. The four `describe_subagent_type`
// outcomes (Unknown / NotAllowed / Disabled / Unavailable) get
// distinct wire strings because they distinguish config-bug from
// infra-flakiness on the dashboard.

/// Fail-open — the configured model id is not in the session's model
/// catalog (`find_model_by_id` miss).
pub(crate) const GOAL_ROLE_MODEL_FAIL_OPEN_MODEL_UNKNOWN: &str = "model_unknown";

/// Fail-open — the model is in the catalog but the session's
/// `allowed_models` does not permit it (`ModelInfo.user_selectable == false`).
pub(crate) const GOAL_ROLE_MODEL_FAIL_OPEN_MODEL_UNAUTHORIZED: &str = "model_unauthorized";

/// Fail-open — the configured `agent_type` did not resolve
/// (`describe_subagent_type` ⇒ `Unknown`): either a subagent type, or a `/goal`
/// harness name, that does not exist.
pub(crate) const GOAL_ROLE_MODEL_FAIL_OPEN_TOOLSET_UNKNOWN: &str = "toolset_unknown";

/// Fail-open — the `agent_type` exists but is not on the parent's
/// allow-list (`describe_subagent_type` ⇒ `NotAllowed`).
pub(crate) const GOAL_ROLE_MODEL_FAIL_OPEN_TOOLSET_NOT_ALLOWED: &str = "toolset_not_allowed";

/// Fail-open — the `agent_type` is allow-listed but disabled
/// (`describe_subagent_type` ⇒ `Disabled`).
pub(crate) const GOAL_ROLE_MODEL_FAIL_OPEN_TOOLSET_DISABLED: &str = "toolset_disabled";

/// Fail-open — the coordinator could not describe the toolset (channel
/// closed / responder dropped / timeout; `describe_subagent_type` ⇒
/// `Unavailable`). Infra-flakiness, distinct from the config-bug cases.
pub(crate) const GOAL_ROLE_MODEL_FAIL_OPEN_TOOLSET_UNAVAILABLE: &str = "toolset_unavailable";

/// Fail-open — the toolset built but lacks the capabilities the role
/// requires (e.g. a verifier whose toolset cannot read/grep code).
pub(crate) const GOAL_ROLE_MODEL_FAIL_OPEN_TOOLSET_INCAPABLE: &str = "toolset_incapable";

/// Fail-open — spawning the role with the configured pair returned a
/// `SpawnError`; the spawn-and-retry-once wrapper retried on the current
/// model.
pub(crate) const GOAL_ROLE_MODEL_FAIL_OPEN_SPAWN_FAILED: &str = "spawn_failed";

/// Fail-open — the configured `agent_type` resolves as a STRICT harness whose
/// subagent flavor `resolve_subagent_toolset` can't represent (e.g. `codex`):
/// committing it would silently run echo-agent-build flavor. Distinct from
/// `toolset_unknown` (a name that doesn't resolve at all).
pub(crate) const GOAL_ROLE_MODEL_FAIL_OPEN_HARNESS_FLAVOR_UNSUPPORTED: &str =
    "harness_flavor_unsupported";

#[allow(clippy::const_is_empty)]
const _: () = assert!(
    !GOAL_ROLE_MODEL_FAIL_OPEN_MODEL_UNKNOWN.is_empty()
        && !GOAL_ROLE_MODEL_FAIL_OPEN_MODEL_UNAUTHORIZED.is_empty()
        && !GOAL_ROLE_MODEL_FAIL_OPEN_TOOLSET_UNKNOWN.is_empty()
        && !GOAL_ROLE_MODEL_FAIL_OPEN_TOOLSET_NOT_ALLOWED.is_empty()
        && !GOAL_ROLE_MODEL_FAIL_OPEN_TOOLSET_DISABLED.is_empty()
        && !GOAL_ROLE_MODEL_FAIL_OPEN_TOOLSET_UNAVAILABLE.is_empty()
        && !GOAL_ROLE_MODEL_FAIL_OPEN_TOOLSET_INCAPABLE.is_empty()
        && !GOAL_ROLE_MODEL_FAIL_OPEN_SPAWN_FAILED.is_empty()
        && !GOAL_ROLE_MODEL_FAIL_OPEN_HARNESS_FLAVOR_UNSUPPORTED.is_empty(),
    "GoalRoleModel discriminator consts must be non-empty",
);

/// Closed set of per-role model-selection fail-open reasons (the `reason`
/// wire strings above). Constructed by the goal spawn wiring; the wire
/// vocabulary is the source of truth dashboards group by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GoalRoleModelFailOpenReason {
    ModelUnknown,
    ModelUnauthorized,
    ToolsetUnknown,
    ToolsetNotAllowed,
    ToolsetDisabled,
    ToolsetUnavailable,
    ToolsetIncapable,
    SpawnFailed,
    HarnessFlavorUnsupported,
}

impl GoalRoleModelFailOpenReason {
    pub(crate) fn as_const_str(self) -> &'static str {
        match self {
            Self::ModelUnknown => GOAL_ROLE_MODEL_FAIL_OPEN_MODEL_UNKNOWN,
            Self::ModelUnauthorized => GOAL_ROLE_MODEL_FAIL_OPEN_MODEL_UNAUTHORIZED,
            Self::ToolsetUnknown => GOAL_ROLE_MODEL_FAIL_OPEN_TOOLSET_UNKNOWN,
            Self::ToolsetNotAllowed => GOAL_ROLE_MODEL_FAIL_OPEN_TOOLSET_NOT_ALLOWED,
            Self::ToolsetDisabled => GOAL_ROLE_MODEL_FAIL_OPEN_TOOLSET_DISABLED,
            Self::ToolsetUnavailable => GOAL_ROLE_MODEL_FAIL_OPEN_TOOLSET_UNAVAILABLE,
            Self::ToolsetIncapable => GOAL_ROLE_MODEL_FAIL_OPEN_TOOLSET_INCAPABLE,
            Self::SpawnFailed => GOAL_ROLE_MODEL_FAIL_OPEN_SPAWN_FAILED,
            Self::HarnessFlavorUnsupported => GOAL_ROLE_MODEL_FAIL_OPEN_HARNESS_FLAVOR_UNSUPPORTED,
        }
    }

    /// Every variant of this enum, enumerated alongside a compiler-
    /// enforced exhaustive `match` so adding a variant is a compile error
    /// until it is listed here (and given an `as_const_str` arm). Mirrors
    /// [`LazinessCategory::all`]; drives the exhaustiveness test.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "exhaustiveness guard, exercised only by tests")
    )]
    pub(crate) const fn all() -> &'static [Self] {
        const fn _assert_exhaustive(r: GoalRoleModelFailOpenReason) {
            match r {
                GoalRoleModelFailOpenReason::ModelUnknown => (),
                GoalRoleModelFailOpenReason::ModelUnauthorized => (),
                GoalRoleModelFailOpenReason::ToolsetUnknown => (),
                GoalRoleModelFailOpenReason::ToolsetNotAllowed => (),
                GoalRoleModelFailOpenReason::ToolsetDisabled => (),
                GoalRoleModelFailOpenReason::ToolsetUnavailable => (),
                GoalRoleModelFailOpenReason::ToolsetIncapable => (),
                GoalRoleModelFailOpenReason::SpawnFailed => (),
                GoalRoleModelFailOpenReason::HarnessFlavorUnsupported => (),
            }
        }
        &[
            Self::ModelUnknown,
            Self::ModelUnauthorized,
            Self::ToolsetUnknown,
            Self::ToolsetNotAllowed,
            Self::ToolsetDisabled,
            Self::ToolsetUnavailable,
            Self::ToolsetIncapable,
            Self::SpawnFailed,
            Self::HarnessFlavorUnsupported,
        ]
    }
}

/// Bridge `GoalClassifierVerdict` (shell) → `GoalClassifierVerdictTelemetry`.
/// Mirrors the `GoalPauseReason` → `GoalPauseReasonTelemetry` pattern;
/// exhaustive match catches drift if either side adds a variant.
impl From<crate::session::goal_tracker::GoalClassifierVerdict> for GoalClassifierVerdictTelemetry {
    fn from(verdict: crate::session::goal_tracker::GoalClassifierVerdict) -> Self {
        use crate::session::goal_tracker::GoalClassifierVerdict;
        match verdict {
            GoalClassifierVerdict::Achieved => Self::Achieved,
            GoalClassifierVerdict::NotAchieved => Self::NotAchieved,
        }
    }
}

/// Bridge `GoalPauseReason` (shell) → `GoalPauseReasonTelemetry`
/// (`echo-agent-file-utils`). The two enums are intentional mirrors and live in
/// different crates due to the orphan rule. The exhaustive `match` here
/// ensures the compiler catches drift if either side adds a variant.
impl From<crate::session::goal_tracker::GoalPauseReason> for GoalPauseReasonTelemetry {
    fn from(reason: crate::session::goal_tracker::GoalPauseReason) -> Self {
        use crate::session::goal_tracker::GoalPauseReason;
        match reason {
            GoalPauseReason::User => Self::User,
            GoalPauseReason::BackOff => Self::BackOff,
            GoalPauseReason::NoProgress => Self::NoProgress,
            GoalPauseReason::Verification => Self::Verification,
            GoalPauseReason::Infra => Self::Infra,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::goal_tracker::GoalPauseReason;

    #[test]
    fn prior_turn_interrupt_from_cancellation_maps_user_interrupts_only() {
        use echo_agent_sampling_types::PriorTurnInterrupt;
        // The three user-interrupt causes map to a marker.
        assert_eq!(
            prior_turn_interrupt_from_cancellation(CancellationCategory::MidTurnAbort),
            Some(PriorTurnInterrupt::MidTurnAbort)
        );
        assert_eq!(
            prior_turn_interrupt_from_cancellation(CancellationCategory::PermissionRejected),
            Some(PriorTurnInterrupt::PermissionRejected)
        );
        assert_eq!(
            prior_turn_interrupt_from_cancellation(CancellationCategory::PermissionCancelled),
            Some(PriorTurnInterrupt::PermissionCancelled)
        );
        // Automatic terminations are NOT user interrupts → no marker.
        assert_eq!(
            prior_turn_interrupt_from_cancellation(CancellationCategory::HookDenied),
            None
        );
    }

    #[test]
    fn goal_pause_reason_telemetry_mirrors_all_variants() {
        assert!(matches!(
            GoalPauseReasonTelemetry::from(GoalPauseReason::User),
            GoalPauseReasonTelemetry::User
        ));
        assert!(matches!(
            GoalPauseReasonTelemetry::from(GoalPauseReason::BackOff),
            GoalPauseReasonTelemetry::BackOff
        ));
        assert!(matches!(
            GoalPauseReasonTelemetry::from(GoalPauseReason::NoProgress),
            GoalPauseReasonTelemetry::NoProgress
        ));
        assert!(matches!(
            GoalPauseReasonTelemetry::from(GoalPauseReason::Verification),
            GoalPauseReasonTelemetry::Verification
        ));
        assert!(matches!(
            GoalPauseReasonTelemetry::from(GoalPauseReason::Infra),
            GoalPauseReasonTelemetry::Infra
        ));
    }

    /// Hand-curated expected pairings — kept here (not in `all()`) so
    /// the test can detect a desync between `as_const_str` and the
    /// `pub const` set. If a variant is added to the enum, `all()`
    /// grows AND `as_const_str` grows (both compiler-enforced); this
    /// table must also be extended, which the `assert_eq!(len)` below
    /// catches.
    const EXPECTED_CATEGORY_CONSTS: &[(LazinessCategory, &str)] = &[
        (
            LazinessCategory::StalledNarration,
            LAZINESS_STALLED_NARRATION,
        ),
        (
            LazinessCategory::StalledPermissionAsking,
            LAZINESS_STALLED_PERMISSION_ASKING,
        ),
        (
            LazinessCategory::StalledNoTodosButTaskInFlight,
            LAZINESS_STALLED_NO_TODOS_BUT_TASK_IN_FLIGHT,
        ),
        (
            LazinessCategory::StalledFalseCompletion,
            LAZINESS_STALLED_FALSE_COMPLETION,
        ),
        (
            LazinessCategory::NotStalledComplete,
            LAZINESS_NOT_STALLED_COMPLETE,
        ),
        (
            LazinessCategory::NotStalledWaitingOnBackground,
            LAZINESS_NOT_STALLED_WAITING_BG,
        ),
        (
            LazinessCategory::NotStalledWaitingOnUser,
            LAZINESS_NOT_STALLED_WAITING_USER,
        ),
    ];

    #[test]
    fn laziness_category_all_covers_every_variant() {
        // Closed-set guard: if a 7th variant were added,
        // `LazinessCategory::all()`'s array would grow (and the
        // exhaustive `_assert_exhaustive` match in the impl would
        // force the array to grow too). The test then forces the
        // expected-table to grow via the length equality, and the
        // per-variant assertions below verify nothing was missed.
        let all = LazinessCategory::all();
        assert_eq!(
            all.len(),
            EXPECTED_CATEGORY_CONSTS.len(),
            "LazinessCategory::all() and EXPECTED_CATEGORY_CONSTS drifted",
        );
        let all_set: std::collections::BTreeSet<&LazinessCategory> = all.iter().collect();
        let expected_set: std::collections::BTreeSet<&LazinessCategory> =
            EXPECTED_CATEGORY_CONSTS.iter().map(|(c, _)| c).collect();
        assert_eq!(
            all_set, expected_set,
            "every variant in `all()` must also appear in EXPECTED_CATEGORY_CONSTS",
        );
    }

    #[test]
    fn laziness_category_round_trip_through_const_str_and_serde() {
        // Single source of truth: every variant maps to one and only
        // one const, and the same const deserializes back to the
        // variant. Drives off `LazinessCategory::all()` so a new
        // variant must be added there (compiler-enforced via
        // `_assert_exhaustive`) for this test to even see it.
        for &(variant, expected_const) in EXPECTED_CATEGORY_CONSTS {
            assert_eq!(variant.as_const_str(), expected_const);
            let json = format!("\"{expected_const}\"");
            let parsed: LazinessCategory =
                serde_json::from_str(&json).expect("deserialize const back to variant");
            assert_eq!(parsed, variant);
        }
        let unique: std::collections::BTreeSet<&'static str> = LazinessCategory::all()
            .iter()
            .map(|c| c.as_const_str())
            .collect();
        assert_eq!(unique.len(), LazinessCategory::all().len());
    }

    #[test]
    fn laziness_stalled_false_completion_const_value_and_round_trip() {
        // Pin the wire string for the new category and prove the
        // const ↔ enum mapping is bijective.
        assert_eq!(
            LAZINESS_STALLED_FALSE_COMPLETION,
            "stalled_false_completion"
        );
        assert_eq!(
            LazinessCategory::StalledFalseCompletion.as_const_str(),
            LAZINESS_STALLED_FALSE_COMPLETION,
        );
        let parsed: LazinessCategory =
            serde_json::from_str("\"stalled_false_completion\"").expect("deserialize");
        assert_eq!(parsed, LazinessCategory::StalledFalseCompletion);
        assert!(LazinessCategory::StalledFalseCompletion.is_stalled());
    }

    #[test]
    fn laziness_is_stalled_matches_stalled_consts_only() {
        // Drive off `all()` so a new variant must be classified as
        // stalled or not-stalled — no variant escapes the test.
        let stalled_consts: std::collections::BTreeSet<&'static str> = [
            LAZINESS_STALLED_NARRATION,
            LAZINESS_STALLED_PERMISSION_ASKING,
            LAZINESS_STALLED_NO_TODOS_BUT_TASK_IN_FLIGHT,
            LAZINESS_STALLED_FALSE_COMPLETION,
        ]
        .into_iter()
        .collect();
        for &variant in LazinessCategory::all() {
            let expected = stalled_consts.contains(variant.as_const_str());
            assert_eq!(
                variant.is_stalled(),
                expected,
                "is_stalled() disagrees with stalled-const set for {variant:?}",
            );
        }
    }

    #[test]
    fn laziness_abort_reason_consts_are_distinct() {
        // Driven off `LazinessAbortReason::all()` so a new variant
        // must be added there (compiler-enforced via the exhaustive
        // match in `as_const_str`) before this test can see it. The
        // closed-set guarantee then lives in code, not in a hand-coded
        // test array. Note: the const array here is preserved
        // separately so a desync between `as_const_str` and the
        // `pub const` set is caught.
        let from_enum: std::collections::BTreeSet<&'static str> =
            crate::session::acp_session::LazinessAbortReason::all()
                .iter()
                .map(|r| r.as_const_str())
                .collect();
        let from_consts: std::collections::BTreeSet<&'static str> = [
            LAZINESS_ABORT_USER_INPUT,
            LAZINESS_ABORT_MODEL_SWITCH,
            LAZINESS_ABORT_TIMEOUT,
            LAZINESS_ABORT_CLASSIFIER_ERROR,
        ]
        .into_iter()
        .collect();
        assert_eq!(
            from_enum, from_consts,
            "LazinessAbortReason variants must map 1:1 to LAZINESS_ABORT_* consts",
        );
    }

    /// Hand-curated `(variant, literal_str, const)` triples. `literal_str`
    /// is a HARDCODED string independent of the `pub const`, so a typo in
    /// a const *value* (e.g. `"model_unknown"` → `"model_unkown"`) is
    /// caught — the test asserts `as_const_str() == literal_str` AND
    /// `as_const_str() == const`. Kept separate from `all()` (which is
    /// compiler-enforced exhaustive via its `_assert_exhaustive` match) so
    /// a new variant forces this table to grow too — caught by the length
    /// equality below.
    const EXPECTED_FAIL_OPEN_CONSTS: &[(GoalRoleModelFailOpenReason, &str, &str)] = &[
        (
            GoalRoleModelFailOpenReason::ModelUnknown,
            "model_unknown",
            GOAL_ROLE_MODEL_FAIL_OPEN_MODEL_UNKNOWN,
        ),
        (
            GoalRoleModelFailOpenReason::ModelUnauthorized,
            "model_unauthorized",
            GOAL_ROLE_MODEL_FAIL_OPEN_MODEL_UNAUTHORIZED,
        ),
        (
            GoalRoleModelFailOpenReason::ToolsetUnknown,
            "toolset_unknown",
            GOAL_ROLE_MODEL_FAIL_OPEN_TOOLSET_UNKNOWN,
        ),
        (
            GoalRoleModelFailOpenReason::ToolsetNotAllowed,
            "toolset_not_allowed",
            GOAL_ROLE_MODEL_FAIL_OPEN_TOOLSET_NOT_ALLOWED,
        ),
        (
            GoalRoleModelFailOpenReason::ToolsetDisabled,
            "toolset_disabled",
            GOAL_ROLE_MODEL_FAIL_OPEN_TOOLSET_DISABLED,
        ),
        (
            GoalRoleModelFailOpenReason::ToolsetUnavailable,
            "toolset_unavailable",
            GOAL_ROLE_MODEL_FAIL_OPEN_TOOLSET_UNAVAILABLE,
        ),
        (
            GoalRoleModelFailOpenReason::ToolsetIncapable,
            "toolset_incapable",
            GOAL_ROLE_MODEL_FAIL_OPEN_TOOLSET_INCAPABLE,
        ),
        (
            GoalRoleModelFailOpenReason::SpawnFailed,
            "spawn_failed",
            GOAL_ROLE_MODEL_FAIL_OPEN_SPAWN_FAILED,
        ),
        (
            GoalRoleModelFailOpenReason::HarnessFlavorUnsupported,
            "harness_flavor_unsupported",
            GOAL_ROLE_MODEL_FAIL_OPEN_HARNESS_FLAVOR_UNSUPPORTED,
        ),
    ];

    #[test]
    fn goal_role_model_fail_open_reason_all_covers_every_variant() {
        // Closed-set guard: `all()` grows (its `_assert_exhaustive` match
        // is compiler-enforced) when a variant is added, forcing the
        // expected-table to grow via this length equality.
        let all = GoalRoleModelFailOpenReason::all();
        assert_eq!(
            all.len(),
            EXPECTED_FAIL_OPEN_CONSTS.len(),
            "GoalRoleModelFailOpenReason::all() and EXPECTED_FAIL_OPEN_CONSTS drifted",
        );
    }

    #[test]
    fn goal_role_model_fail_open_reason_const_strs_are_snake_case_and_distinct() {
        // Pin every variant's wire string to a hardcoded literal (catches
        // a const-value typo), confirm it equals the `pub const`, that
        // it's snake_case, and that all are distinct. Driven off the
        // expected table whose length is tied to `all()`, so no variant
        // escapes the assertions.
        let mut seen = std::collections::BTreeSet::new();
        for &(variant, literal_str, wire_const) in EXPECTED_FAIL_OPEN_CONSTS {
            // Independent literal pin — fails if a const value drifts.
            assert_eq!(
                variant.as_const_str(),
                literal_str,
                "{variant:?} wire string drifted from its pinned literal",
            );
            // And the const must agree with the literal.
            assert_eq!(
                wire_const, literal_str,
                "const for {variant:?} drifted from its pinned literal",
            );
            // snake_case wire strings only (lowercase + underscore).
            assert!(
                literal_str
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '_'),
                "wire string {literal_str:?} is not snake_case",
            );
            assert!(seen.insert(variant.as_const_str()), "duplicate wire string");
        }
        assert_eq!(
            seen.len(),
            GoalRoleModelFailOpenReason::all().len(),
            "every fail-open reason must map to a distinct wire string",
        );
    }

    #[test]
    fn goal_summarizer_fail_reason_wire_strings() {
        // Table pins each variant's wire string to a hardcoded literal so a
        // const-value typo (e.g. on the e2e-exercised `runtime`) is caught.
        use GoalSummarizerFailReason as R;
        for (variant, literal) in [
            (R::Transport, "transport"),
            (R::Runtime, "runtime"),
            (R::Aborted, "aborted"),
            (R::EmptySummary, "empty_summary"),
        ] {
            assert_eq!(variant.as_const_str(), literal, "{variant:?} drifted");
        }
    }
}
