//! Stall-triggered goal strategist subagent runner.
//!
//! Mirrors [`crate::session::goal_planner`] in shape (an `Outcome`, a
//! spawner trait + [`ChannelSpawner`], a one-shot runner, and a
//! terminal-token parser) but with the OPPOSITE failure semantics: the
//! planner is fail-CLOSED (any failure pauses the goal); the strategist
//! is fail-OPEN / best-effort. It is an advisory enhancement that fires
//! after N consecutive `NotAchieved` verifications to recommend a
//! STRUCTURAL remediation; any failure is logged and the normal loop
//! continues — the goal is never paused on strategist failure.
//!
//! plan.md safety: the strategist writes ONLY to the strategy note
//! ([`GoalTracker::strategy_path`](crate::session::goal_tracker::GoalTracker::strategy_path)),
//! never to `plan.md` (which holds the verifier-judged contract). As
//! best-effort defense-in-depth the runner snapshots `plan.md` before
//! spawning and, via an RAII [`PlanGuard`], restores it to those bytes
//! after the run (and on drop, so a cancelled turn still triggers the
//! restore). This makes contract corruption hard, not literally
//! impossible: a restore I/O failure or a symlink planted at the path is
//! refused and surfaced via `GoalStrategistContractRestoreFailed`
//! telemetry rather than silently corrupting the contract.

use crate::session::events::{Event, GoalStrategistFailReason, GoalStrategistRestoreFailReason};
use crate::session::goal_planner::{
    GOAL_ROLE_AWAIT_BUDGET_EXCEEDED, GOAL_ROLE_SUBAGENT_TYPE, RoleRenderedPrompt,
    RoleSpawnOverride, SpawnError, parse_terminal_response, spawn_with_fail_open_retry,
};
use crate::session::goal_role_tools::RoleToolNames;
use echo_agent_session_events::EventWriter;
use echo_agent_tools::implementations::echo_agent_build::task::backend::{
    ChannelBackend, SubagentBackend,
};
use echo_agent_tools::implementations::echo_agent_build::task::types::{
    SubagentOwner, SubagentRequest, SubagentRuntimeOverrides,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;

// Constants

/// Same general-purpose inventory each verifier skeptic / the planner
/// uses: the strategist reads and greps the workspace to diagnose why
/// the goal is stuck. The configured `agent_type` selects the HARNESS, not
/// this subagent type.
const GOAL_STRATEGIST_SUBAGENT_TYPE: &str = GOAL_ROLE_SUBAGENT_TYPE;

/// Description shown in the pager subagent strip and matched by the
/// e2e coordinator stub to distinguish strategist spawns from skeptics.
pub(crate) const GOAL_STRATEGIST_SUBAGENT_DESCRIPTION: &str = "goal strategist";

const GOAL_STRATEGIST_PROMPT_TEMPLATE: &str = include_str!("templates/goal_strategist_prompt.md");

/// Cap (in `char`s, not bytes) on the recommendation snippet read back
/// from the strategy note and inlined into the continuation directive.
/// Truncation is on a `char` boundary, so the cap is UTF-8-safe but a
/// multibyte note can exceed this many bytes. The full note lives on
/// disk; the model is pointed at it to read the rest.
const GOAL_STRATEGIST_RECOMMENDATION_MAX_CHARS: usize = 4096;

// Outcome + spawner abstraction

/// Result of one strategist attempt. `Advised` carries the strategy
/// note path and the short recommendation read back from it. `FailOpen`
/// carries the reason — every variant is logged and ignored at the call
/// site (the goal keeps running).
#[derive(Debug, Clone)]
#[expect(
    dead_code,
    reason = "`latency_ms` is consumed by telemetry / future use"
)]
pub(crate) enum GoalStrategistOutcome {
    Advised {
        strategy_file: PathBuf,
        recommendation: String,
        latency_ms: u64,
    },
    FailOpen {
        reason: GoalStrategistFailReason,
        latency_ms: u64,
    },
}

/// Subagent spawn abstraction. Production uses [`ChannelSpawner`];
/// tests use `MockSpawner` (defined in the tests module). The trait
/// shape mirrors `GoalPlannerSpawner`; `SpawnError` is reused from the
/// planner module rather than re-declared.
#[async_trait::async_trait]
pub(crate) trait GoalStrategistSpawner: Send + Sync {
    /// Spawn under `id` and return the terminal response when the
    /// subagent finishes. `prompt` carries both the configured-pair render
    /// (`primary`) and the default-toolset fail-open retry render (`fallback`).
    async fn spawn_strategist(
        &self,
        id: &str,
        prompt: RoleRenderedPrompt,
    ) -> Result<String, SpawnError>;
}

// Trigger predicate

/// Pure trigger predicate. Fires when the consecutive-failure count has
/// advanced at least `every` (N) past the count at which the strategist
/// last fired (`last_fired`). Using `>= last_fired + N` rather than a
/// strict `consecutive % N == 0` makes the trigger SKIP-ROBUST: the
/// synthetic concurrent-in-flight path can bump the streak by more than
/// one at a time (e.g. N-1 → N+1), and an exact-equality check would miss
/// the `== N` fire entirely. `every` must be ≥ 1 (the resolver clamps it).
pub(crate) fn strategist_should_fire(consecutive: u32, last_fired: u32, every: u32) -> bool {
    every > 0 && consecutive >= last_fired.saturating_add(every)
}

// Production spawner

pub(crate) struct ChannelSpawner {
    pub(crate) event_tx: tokio::sync::mpsc::UnboundedSender<
        echo_agent_tools::implementations::echo_agent_build::task::types::SubagentEvent,
    >,
    pub(crate) foreground_wait: Option<
        echo_agent_tools::implementations::echo_agent_build::task::types::SubagentForegroundWait,
    >,
    pub(crate) parent_session_id: String,
    pub(crate) parent_prompt_id: Option<String>,
    pub(crate) cwd: Option<String>,
    /// Trace-artifact sink + resolved `task` tool name; `None` disables
    /// recording. See [`crate::session::goal_classifier::record_subagent_trace`].
    pub(crate) trace_sink: Option<(echo_agent_chat_state::ChatStateHandle, String)>,
    /// Resolved per-role model+toolset override. Default (inherit) keeps the
    /// historic `::default()` spawn behavior.
    pub(crate) role_override: RoleSpawnOverride,
    /// Event sink for the spawn-and-retry-once fail-open telemetry; `None`
    /// in tests / when no event log is wired.
    pub(crate) events: Option<EventWriter>,
}

#[async_trait::async_trait]
impl GoalStrategistSpawner for ChannelSpawner {
    async fn spawn_strategist(
        &self,
        id: &str,
        prompt: RoleRenderedPrompt,
    ) -> Result<String, SpawnError> {
        // Clone the primary render for the trace pair only when tracing; the
        // wrapper moves each render into its attempt (no other clone).
        let trace_prompt = self.trace_sink.as_ref().map(|_| prompt.primary.clone());
        let outcome = spawn_with_fail_open_retry(
            "strategist",
            None,
            &self.role_override,
            self.events.as_ref(),
            prompt,
            |model, harness, prompt| self.send_one(id, prompt, model, harness),
        )
        .await;

        match &outcome {
            Ok(text) => crate::session::goal_classifier::record_subagent_trace(
                self.trace_sink.as_ref(),
                id,
                GOAL_STRATEGIST_SUBAGENT_TYPE,
                GOAL_STRATEGIST_SUBAGENT_DESCRIPTION,
                trace_prompt.as_deref(),
                text,
            ),
            Err(SpawnError::Runtime { message, .. }) => {
                crate::session::goal_classifier::record_subagent_trace(
                    self.trace_sink.as_ref(),
                    id,
                    GOAL_STRATEGIST_SUBAGENT_TYPE,
                    GOAL_STRATEGIST_SUBAGENT_DESCRIPTION,
                    trace_prompt.as_deref(),
                    message,
                )
            }
            Err(SpawnError::Transport(_) | SpawnError::Interrupted) => {}
        }
        outcome
    }
}

impl ChannelSpawner {
    /// Send one spawn (model + harness override resolved by the caller) and
    /// await its terminal result. The fail-open wrapper calls this once or
    /// twice (retry on the current model + session harness). The subagent_type
    /// is always [`GOAL_STRATEGIST_SUBAGENT_TYPE`]; `harness_agent_type` selects
    /// the harness flavor (`None` ⇒ session harness).
    async fn send_one(
        &self,
        id: &str,
        prompt: String,
        model: Option<String>,
        harness_agent_type: Option<String>,
    ) -> Result<String, SpawnError> {
        let request = SubagentRequest {
            id: id.to_string(),
            prompt,
            description: GOAL_STRATEGIST_SUBAGENT_DESCRIPTION.to_string(),
            subagent_type: GOAL_STRATEGIST_SUBAGENT_TYPE.to_string(),
            parent_session_id: self.parent_session_id.clone(),
            parent_prompt_id: self.parent_prompt_id.clone(),
            resume_from: None,
            cwd: self.cwd.clone(),
            runtime_overrides: SubagentRuntimeOverrides {
                model,
                harness_agent_type,
                ..Default::default()
            },
            run_in_background: false,
            // Harness-internal: never surface to the model's idle reminder.
            surface_completion: false,
            await_to_completion: false,
            fork_context: false,
            owner: SubagentOwner::Task,
            cancel_token: tokio_util::sync::CancellationToken::new(),
        };
        let backend = ChannelBackend::new(self.event_tx.clone());
        let result = backend
            .spawn_with_foreground_wait(request, self.foreground_wait.as_ref())
            .await
            .map_err(|error| SpawnError::Transport(error.to_string()))?;
        if result.backgrounded {
            let _ = backend.cancel(&result.subagent_id).await;
            return Err(SpawnError::Runtime {
                message: GOAL_ROLE_AWAIT_BUDGET_EXCEEDED.to_owned(),
                cancelled: true,
            });
        }
        if !result.success {
            let message = result.error.unwrap_or_else(|| "unknown error".to_string());
            return Err(SpawnError::Runtime {
                message,
                cancelled: result.cancelled,
            });
        }
        Ok(result.output.to_string())
    }
}

// Runner

pub(crate) struct GoalStrategistInputs<'a> {
    pub objective: &'a str,
    /// The verifier-judged plan; passed for context + protected by the
    /// snapshot/restore guard. May not exist on disk (planner disabled).
    pub plan_file: &'a Path,
    /// Where the strategist writes its advisory note.
    pub strategy_file: &'a Path,
    /// Absolute path to the session traces directory. The strategist reads
    /// the trace files (`chat_history.jsonl`, `events.jsonl`, …) itself to
    /// diagnose the stuck run, rather than being fed a pre-assembled packet.
    pub session_traces_dir: &'a Path,
    /// Per-goal scratch root with the implementer's and each skeptic's captured
    /// test output / artifacts.
    pub scratch_root: &'a Path,
    pub attempt: u32,
    pub consecutive_failures: u32,
    /// Resolved strategist cadence N (fires every N consecutive `NotAchieved`
    /// verifications). Telemetry-only on `GoalStrategistFired`; the firing
    /// decision uses `goal_strategist_every` at the actor.
    pub every: u32,
    pub model_id: &'a str,
    /// Resolved tool names for the strategist role's prompt placeholders
    /// (`{READ_TOOL}`/`{SEARCH_TOOL}`/`{LIST_TOOL}`/`{EXECUTE_TOOL}`). Built
    /// parent-side from the strategist's resolved toolset.
    pub tool_names: &'a RoleToolNames,
    /// Default/parent-toolset tool names used to render the fail-open RETRY
    /// prompt, so a retry that falls back to the default toolset names THAT
    /// toolset's tools. On the inherit path this equals `tool_names`.
    pub inherit_tool_names: &'a RoleToolNames,
}

/// Run one strategist attempt. Fail-OPEN: every failure path returns
/// `FailOpen { reason }` and the caller logs it and continues — the goal
/// is never paused here.
pub(crate) async fn run_goal_strategist(
    spawner: Arc<dyn GoalStrategistSpawner>,
    inputs: GoalStrategistInputs<'_>,
    emit_event: &dyn Fn(Event),
) -> GoalStrategistOutcome {
    let started = std::time::Instant::now();
    emit_event(Event::GoalStrategistFired {
        attempt: inputs.attempt,
        consecutive_failures: inputs.consecutive_failures,
        every: inputs.every,
        model_id: inputs.model_id.to_string(),
    });

    // Best-effort: pre-create the strategy-file parent dir. A failure here
    // is not fatal (fail-open) — the spawn may still succeed if the dir
    // already exists; if it doesn't, the missing-strategy guard catches it.
    if let Some(parent) = inputs.strategy_file.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }

    // plan.md safety: snapshot plan.md byte-for-byte and arm the restore
    // guard BEFORE spawning. The guard restores on drop too, so a cancelled
    // turn (this future dropped mid-`.await`) still triggers the restore.
    let mut plan_guard = PlanGuard::capture(inputs.plan_file);

    let strategy_file_str = inputs.strategy_file.to_string_lossy();
    let plan_file_str = inputs.plan_file.to_string_lossy();
    let traces_dir_str = inputs.session_traces_dir.to_string_lossy();
    let scratch_root_str = inputs.scratch_root.to_string_lossy();
    let with_paths = GOAL_STRATEGIST_PROMPT_TEMPLATE
        .replace("{STRATEGY_FILE}", &strategy_file_str)
        .replace("{PLAN_FILE}", &plan_file_str)
        .replace("{SESSION_TRACES_DIR}", &traces_dir_str)
        .replace("{SCRATCH_ROOT}", &scratch_root_str);
    // Render once per toolset: `primary` for the resolved toolset, `fallback`
    // for the default/parent toolset the explicit-pair retry falls back to.
    let render = |tool_names: &RoleToolNames| -> String {
        let rendered = tool_names.apply(&with_paths);
        let mut full = String::with_capacity(rendered.len() + inputs.objective.len() + 256);
        full.push_str(&rendered);
        full.push_str("\n\nROUND: failed ");
        full.push_str(&inputs.consecutive_failures.to_string());
        full.push_str(" verification rounds in a row.\n\nOBJECTIVE:\n");
        full.push_str(inputs.objective);
        full.push('\n');
        full
    };
    let prompt = RoleRenderedPrompt {
        primary: render(inputs.tool_names),
        fallback: render(inputs.inherit_tool_names),
    };

    let spawn_id = uuid::Uuid::now_v7().to_string();
    let spawn_result = spawner.spawn_strategist(&spawn_id, prompt).await;

    // Restore plan.md byte-for-byte ONCE, regardless of spawn outcome (the
    // guard is idempotent; its `Drop` is the cancellation safety net if the
    // await above is dropped). On a restore failure, surface it via
    // telemetry — not just a log — so a corrupted contract is observable.
    if let Some(reason) = plan_guard.restore() {
        emit_event(Event::GoalStrategistContractRestoreFailed {
            reason: reason.as_const_str(),
            attempt: inputs.attempt,
        });
    }

    let response = match spawn_result {
        Ok(text) => text,
        Err(SpawnError::Transport(detail)) => {
            tracing::warn!(error = %detail, "goal strategist: transport error; failing open");
            return record_fail_open(
                GoalStrategistFailReason::Transport,
                inputs.attempt,
                inputs.consecutive_failures,
                started,
                emit_event,
            );
        }
        Err(SpawnError::Interrupted) => {
            return record_fail_open(
                GoalStrategistFailReason::Aborted,
                inputs.attempt,
                inputs.consecutive_failures,
                started,
                emit_event,
            );
        }
        Err(SpawnError::Runtime { message, cancelled }) => {
            let reason = if cancelled {
                GoalStrategistFailReason::Aborted
            } else {
                GoalStrategistFailReason::Runtime
            };
            tracing::warn!(
                error = %message,
                cancelled,
                "goal strategist: subagent runtime error; failing open",
            );
            return record_fail_open(
                reason,
                inputs.attempt,
                inputs.consecutive_failures,
                started,
                emit_event,
            );
        }
    };

    let recommendation = match read_recommendation(inputs.strategy_file).await {
        Some(text) => text,
        None => {
            tracing::info!(
                strategy_file = %strategy_file_str,
                terminal_token_ok = parse_terminal_response(&response),
                "goal strategist: strategy note missing or empty; failing open",
            );
            return record_fail_open(
                GoalStrategistFailReason::MissingStrategy,
                inputs.attempt,
                inputs.consecutive_failures,
                started,
                emit_event,
            );
        }
    };

    let latency_ms = started.elapsed().as_millis() as u64;
    emit_event(Event::GoalStrategistCompleted {
        attempt: inputs.attempt,
        consecutive_failures: inputs.consecutive_failures,
        latency_ms,
    });
    GoalStrategistOutcome::Advised {
        strategy_file: inputs.strategy_file.to_path_buf(),
        recommendation,
        latency_ms,
    }
}

/// Read the strategy note back, capped at
/// [`GOAL_STRATEGIST_RECOMMENDATION_MAX_CHARS`]. `None` when the file is
/// absent, empty, or unreadable.
async fn read_recommendation(strategy_file: &Path) -> Option<String> {
    let bytes = tokio::fs::read(strategy_file).await.ok()?;
    if bytes.is_empty() {
        return None;
    }
    let text = String::from_utf8_lossy(&bytes);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let snippet: String = trimmed
        .chars()
        .take(GOAL_STRATEGIST_RECOMMENDATION_MAX_CHARS)
        .collect();
    Some(snippet)
}

// plan.md restore guard

/// Snapshot of plan.md captured before the strategist runs.
enum PlanSnapshot {
    /// plan.md did not exist (`NotFound`) — a strategist-created regular
    /// file is removed on restore.
    Absent,
    /// plan.md existed as a regular file; these are its bytes to restore.
    Present(Vec<u8>),
    /// plan.md couldn't be safely snapshotted (a symlink, or metadata/read
    /// failed for a reason other than `NotFound`). Restore refuses to write or
    /// delete it — so a transiently-unreadable contract is never deleted as if
    /// the strategist had created it.
    Unsafe,
}

/// RAII guard that restores plan.md to its pre-strategist bytes — once via
/// [`Self::restore`] on the normal path, and again on `Drop` as a cancellation
/// safety net (the runner future may be dropped mid-`.await`). Uses sync
/// `std::fs` (so `Drop` can call it; plan.md is small, this is rare) and
/// `symlink_metadata` everywhere (never follows a planted symlink).
struct PlanGuard<'a> {
    plan_file: &'a Path,
    snapshot: PlanSnapshot,
    armed: bool,
}

impl<'a> PlanGuard<'a> {
    fn capture(plan_file: &'a Path) -> Self {
        let snapshot = match std::fs::symlink_metadata(plan_file) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => PlanSnapshot::Absent,
            // Any other metadata error → fail safe (don't risk deleting).
            Err(_) => PlanSnapshot::Unsafe,
            // A symlink where the contract should be → refuse to follow.
            Ok(meta) if meta.file_type().is_symlink() => PlanSnapshot::Unsafe,
            Ok(_) => match std::fs::read(plan_file) {
                Ok(bytes) => PlanSnapshot::Present(bytes),
                // Existed but unreadable → never later delete it as "created".
                Err(_) => PlanSnapshot::Unsafe,
            },
        };
        Self {
            plan_file,
            snapshot,
            armed: true,
        }
    }

    /// Restore plan.md to its captured bytes (whole file, not just the
    /// contract sections) and disarm. Idempotent (a second call, incl. the
    /// `Drop` follow-up, is a no-op). Returns `Some(reason)` when the
    /// contract could NOT be guaranteed (write/remove failed, or a symlink
    /// was found) so the caller can emit telemetry.
    fn restore(&mut self) -> Option<GoalStrategistRestoreFailReason> {
        if !self.armed {
            return None;
        }
        self.armed = false;
        self.restore_core()
    }

    fn restore_core(&self) -> Option<GoalStrategistRestoreFailReason> {
        let current = std::fs::symlink_metadata(self.plan_file);
        match &self.snapshot {
            // Never snapshotted safely — do not write or delete.
            PlanSnapshot::Unsafe => None,
            PlanSnapshot::Present(orig) => match &current {
                Ok(meta) if meta.file_type().is_symlink() => {
                    self.log_restore_fail("plan.md became a symlink");
                    Some(GoalStrategistRestoreFailReason::SymlinkTamper)
                }
                // Present-or-vanished regular file: rewrite when it differs.
                _ => {
                    let unchanged = std::fs::read(self.plan_file)
                        .map(|now| now == *orig)
                        .unwrap_or(false);
                    if unchanged {
                        return None;
                    }
                    if std::fs::write(self.plan_file, orig).is_err() {
                        self.log_restore_fail("failed to rewrite plan.md");
                        Some(GoalStrategistRestoreFailReason::WriteFailed)
                    } else {
                        None
                    }
                }
            },
            PlanSnapshot::Absent => match &current {
                Ok(meta) if meta.file_type().is_symlink() => {
                    self.log_restore_fail("strategist planted a symlink at plan.md");
                    Some(GoalStrategistRestoreFailReason::SymlinkTamper)
                }
                // Strategist created a regular file where none existed — remove it.
                Ok(_) => {
                    if std::fs::remove_file(self.plan_file).is_err() {
                        self.log_restore_fail("failed to remove strategist-created plan.md");
                        Some(GoalStrategistRestoreFailReason::RemoveFailed)
                    } else {
                        None
                    }
                }
                // Still absent — nothing to undo.
                Err(_) => None,
            },
        }
    }

    fn log_restore_fail(&self, what: &str) {
        tracing::error!(
            plan_file = %self.plan_file.display(),
            "goal strategist: {what}; verifier contract may be corrupted",
        );
    }
}

impl Drop for PlanGuard<'_> {
    fn drop(&mut self) {
        // Cancellation safety net: if `restore` was never called (the runner
        // future was dropped mid-await), restore now. Telemetry can't be
        // emitted from here (no event sink), so a failure is logged at ERROR.
        if let Some(reason) = self.restore() {
            tracing::error!(
                reason = reason.as_const_str(),
                "goal strategist: plan.md restore failed during drop (cancellation); \
                 contract may be corrupted",
            );
        }
    }
}

fn record_fail_open(
    reason: GoalStrategistFailReason,
    attempt: u32,
    consecutive_failures: u32,
    started: std::time::Instant,
    emit_event: &dyn Fn(Event),
) -> GoalStrategistOutcome {
    let latency_ms = started.elapsed().as_millis() as u64;
    emit_event(Event::GoalStrategistFailed {
        reason: reason.as_const_str(),
        attempt,
        consecutive_failures,
        latency_ms,
    });
    GoalStrategistOutcome::FailOpen { reason, latency_ms }
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[tokio::test]
    async fn channel_spawner_request_is_harness_internal() {
        use echo_agent_tools::implementations::echo_agent_build::task::types::{
            SubagentEvent, SubagentResult,
        };

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let spawner = ChannelSpawner {
            event_tx: tx,
            foreground_wait: None,
            parent_session_id: "parent".into(),
            parent_prompt_id: None,
            cwd: None,
            trace_sink: None,
            role_override: RoleSpawnOverride::default(),
            events: None,
        };
        let handle = tokio::spawn(async move {
            let _ = spawner
                .spawn_strategist(
                    "strat-id",
                    RoleRenderedPrompt {
                        primary: "prompt".into(),
                        fallback: "prompt".into(),
                    },
                )
                .await;
        });

        let SubagentEvent::Spawn(request) = rx.recv().await.expect("spawn event") else {
            panic!("expected Spawn");
        };
        assert!(
            !request.surface_completion,
            "strategist subagent must not surface to the idle reminder"
        );
        assert_eq!(request.description, GOAL_STRATEGIST_SUBAGENT_DESCRIPTION);
        let _ = request.result_tx.send(SubagentResult::default());
        handle.await.unwrap();
    }

    /// 3-role parity: an explicit strategist pair threads `agent_type` as the
    /// request's `harness_agent_type`, not the subagent_type.
    #[tokio::test]
    async fn channel_spawner_threads_harness_override_to_request() {
        use echo_agent_tools::implementations::echo_agent_build::task::types::{
            SubagentEvent, SubagentResult,
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let spawner = ChannelSpawner {
            event_tx: tx,
            foreground_wait: None,
            parent_session_id: "parent".into(),
            parent_prompt_id: None,
            cwd: None,
            trace_sink: None,
            role_override: RoleSpawnOverride {
                model: Some("cfg-model".into()),
                agent_type: Some("cursor".into()),
            },
            events: None,
        };
        let handle = tokio::spawn(async move {
            let _ = spawner
                .spawn_strategist(
                    "strat-id",
                    RoleRenderedPrompt {
                        primary: "prompt".into(),
                        fallback: "prompt".into(),
                    },
                )
                .await;
        });
        let SubagentEvent::Spawn(request) = rx.recv().await.expect("spawn event") else {
            panic!("expected Spawn");
        };
        assert_eq!(
            request.subagent_type, GOAL_STRATEGIST_SUBAGENT_TYPE,
            "strategist always spawns the fixed role subagent type",
        );
        assert_eq!(
            request.runtime_overrides.harness_agent_type.as_deref(),
            Some("cursor"),
            "configured agent_type must thread as the harness override",
        );
        assert_eq!(
            request.runtime_overrides.model.as_deref(),
            Some("cfg-model")
        );
        // Reply SUCCESS so the explicit pair does NOT trigger a fail-open retry.
        let _ = request.result_tx.send(SubagentResult {
            success: true,
            output: std::sync::Arc::from("ok"),
            ..Default::default()
        });
        handle.await.unwrap();
    }

    #[test]
    fn strategist_fires_at_n_and_multiples_not_in_between() {
        // N = 5, last_fired = 0: fires at 5, not before.
        assert!(!strategist_should_fire(4, 0, 5));
        assert!(strategist_should_fire(5, 0, 5));
        assert!(!strategist_should_fire(0, 0, 5), "0 failures never fires");
        // After firing at 5 (last_fired = 5): not at 6..9, fires at 10.
        assert!(!strategist_should_fire(6, 5, 5), "must NOT fire at N+1");
        assert!(!strategist_should_fire(9, 5, 5));
        assert!(strategist_should_fire(10, 5, 5));
        // N = 1: fires every round.
        assert!(strategist_should_fire(1, 0, 1));
        assert!(strategist_should_fire(2, 1, 1));
        // every == 0 is a degenerate guard — never fires.
        assert!(!strategist_should_fire(3, 0, 0));
    }

    /// Skip-robustness: the synthetic concurrent-in-flight path
    /// can bump the streak by more than one, landing PAST a multiple of N
    /// without ever hitting `== N`. The `>= last_fired + N` form still
    /// fires; a strict `% N == 0` would have skipped the window entirely.
    #[test]
    fn strategist_fires_after_a_skipped_multiple() {
        // N = 2, last_fired = 0. A jump straight to 3 (skipping the == 2
        // landing) must still fire.
        assert!(
            strategist_should_fire(3, 0, 2),
            "must fire after the streak skips past the == N landing",
        );
        // Having fired at 3 (last_fired = 3): not at 4, fires at 5.
        assert!(!strategist_should_fire(4, 3, 2));
        assert!(strategist_should_fire(5, 3, 2));
    }

    /// Deterministic spawner with knobs for response text, whether to
    /// write the strategy note, its body, and whether to (mis)write
    /// plan.md to exercise the contract guard.
    struct MockSpawner {
        response: Result<String, SpawnError>,
        write_strategy: bool,
        strategy_body: Vec<u8>,
        strategy_target: PathBuf,
        /// When `Some`, the spawner writes this body to `plan_target`,
        /// simulating a misbehaving strategist that edits the contract.
        plan_overwrite: Option<Vec<u8>>,
        /// When `Some`, the spawner replaces `plan_target` with a symlink to
        /// this path, simulating a strategist that plants a symlink.
        plan_symlink_to: Option<PathBuf>,
        plan_target: PathBuf,
        last_prompt: Mutex<Option<String>>,
    }

    impl MockSpawner {
        fn ok_writes(strategy_file: &Path, plan_file: &Path, body: &[u8]) -> Self {
            Self {
                response: Ok("Done".to_string()),
                write_strategy: true,
                strategy_body: body.to_vec(),
                strategy_target: strategy_file.to_path_buf(),
                plan_overwrite: None,
                plan_symlink_to: None,
                plan_target: plan_file.to_path_buf(),
                last_prompt: Mutex::new(None),
            }
        }

        fn ok_does_not_write(strategy_file: &Path, plan_file: &Path) -> Self {
            Self {
                response: Ok("Done".to_string()),
                write_strategy: false,
                strategy_body: Vec::new(),
                strategy_target: strategy_file.to_path_buf(),
                plan_overwrite: None,
                plan_symlink_to: None,
                plan_target: plan_file.to_path_buf(),
                last_prompt: Mutex::new(None),
            }
        }

        fn fails(err: SpawnError, strategy_file: &Path, plan_file: &Path) -> Self {
            Self {
                response: Err(err),
                write_strategy: false,
                strategy_body: Vec::new(),
                strategy_target: strategy_file.to_path_buf(),
                plan_overwrite: None,
                plan_symlink_to: None,
                plan_target: plan_file.to_path_buf(),
                last_prompt: Mutex::new(None),
            }
        }

        fn with_plan_overwrite(mut self, body: &[u8]) -> Self {
            self.plan_overwrite = Some(body.to_vec());
            self
        }

        #[cfg(unix)]
        fn with_plan_replaced_by_symlink(mut self, target: &Path) -> Self {
            self.plan_symlink_to = Some(target.to_path_buf());
            self
        }
    }

    #[async_trait::async_trait]
    impl GoalStrategistSpawner for MockSpawner {
        async fn spawn_strategist(
            &self,
            #[allow(unused_variables)] id: &str,
            prompt: RoleRenderedPrompt,
        ) -> Result<String, SpawnError> {
            *self.last_prompt.lock().unwrap() = Some(prompt.primary);
            if self.write_strategy {
                let _ = tokio::fs::write(&self.strategy_target, &self.strategy_body).await;
            }
            if let Some(body) = &self.plan_overwrite {
                let _ = tokio::fs::write(&self.plan_target, body).await;
            }
            #[cfg(unix)]
            if let Some(target) = &self.plan_symlink_to {
                let _ = tokio::fs::remove_file(&self.plan_target).await;
                let _ = std::os::unix::fs::symlink(target, &self.plan_target);
            }
            match &self.response {
                Ok(text) => Ok(text.clone()),
                Err(SpawnError::Transport(s)) => Err(SpawnError::Transport(s.clone())),
                Err(SpawnError::Runtime { message, cancelled }) => Err(SpawnError::Runtime {
                    message: message.clone(),
                    cancelled: *cancelled,
                }),
                Err(SpawnError::Interrupted) => Err(SpawnError::Interrupted),
            }
        }
    }

    fn collect_events() -> (Arc<Mutex<Vec<String>>>, impl Fn(Event) + Send + Sync) {
        let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let log_clone = log.clone();
        let emit = move |e: Event| {
            let tag = match e {
                Event::GoalStrategistFired { .. } => "fired".to_string(),
                Event::GoalStrategistCompleted { .. } => "completed".to_string(),
                Event::GoalStrategistFailed { reason, .. } => format!("failed:{reason}"),
                Event::GoalStrategistContractRestoreFailed { reason, .. } => {
                    format!("restore_failed:{reason}")
                }
                other => format!("other:{other:?}"),
            };
            log_clone.lock().unwrap().push(tag);
        };
        (log, emit)
    }

    fn tmp_dir(name: &str) -> PathBuf {
        let tmp = std::env::temp_dir().join(format!(
            "goal-strategist-{name}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        tmp
    }

    /// Shared inherit-default tool names for the test inputs (a `'static`
    /// reference so the `inputs()` helper can hand it out without borrowing a
    /// local temporary).
    fn default_tool_names() -> &'static RoleToolNames {
        use std::sync::OnceLock;
        static TN: OnceLock<RoleToolNames> = OnceLock::new();
        TN.get_or_init(RoleToolNames::inherit_defaults)
    }

    fn inputs<'a>(plan: &'a Path, strategy: &'a Path) -> GoalStrategistInputs<'a> {
        GoalStrategistInputs {
            objective: "do X",
            plan_file: plan,
            strategy_file: strategy,
            session_traces_dir: plan.parent().unwrap(),
            scratch_root: plan.parent().unwrap(),
            attempt: 3,
            consecutive_failures: 5,
            every: 2,
            model_id: "echo-agent-test",
            tool_names: default_tool_names(),
            inherit_tool_names: default_tool_names(),
        }
    }

    #[tokio::test]
    async fn success_path_emits_fired_and_completed_and_returns_advised() {
        let dir = tmp_dir("happy");
        let plan = dir.join("plan.md");
        let strategy = dir.join("strategy.md");
        std::fs::write(&plan, b"# Plan: contract\n").unwrap();
        let spawner = Arc::new(MockSpawner::ok_writes(
            &strategy,
            &plan,
            b"## Diagnosis\n\nSplit the monolith.\n",
        ));
        let (log, emit) = collect_events();

        let outcome = run_goal_strategist(spawner, inputs(&plan, &strategy), &emit).await;

        match outcome {
            GoalStrategistOutcome::Advised { recommendation, .. } => {
                assert!(recommendation.contains("Split the monolith"));
            }
            other => panic!("expected Advised, got {other:?}"),
        }
        let log = log.lock().unwrap();
        assert_eq!(log.as_slice(), ["fired", "completed"], "{log:?}");
    }

    /// `GoalStrategistFired` reports the resolved cadence N from inputs, not a
    /// hardcoded default.
    #[tokio::test]
    async fn fired_event_reports_resolved_cadence() {
        use std::sync::Mutex as StdMutex;
        let dir = tmp_dir("cadence");
        let plan = dir.join("plan.md");
        let strategy = dir.join("strategy.md");
        std::fs::write(&plan, b"# Plan: contract\n").unwrap();
        let spawner = Arc::new(MockSpawner::ok_writes(
            &strategy,
            &plan,
            b"## Diagnosis\n\nx\n",
        ));
        let captured: Arc<StdMutex<Option<u32>>> = Arc::new(StdMutex::new(None));
        let cap = captured.clone();
        let emit = move |e: Event| {
            if let Event::GoalStrategistFired { every, .. } = e {
                *cap.lock().unwrap() = Some(every);
            }
        };
        let mut inputs = inputs(&plan, &strategy);
        inputs.every = 7;
        let _ = run_goal_strategist(spawner, inputs, &emit).await;
        assert_eq!(
            *captured.lock().unwrap(),
            Some(7),
            "GoalStrategistFired.every must report inputs.every (resolved cadence)",
        );
    }

    #[tokio::test]
    async fn missing_strategy_file_fails_open() {
        let dir = tmp_dir("missing");
        let plan = dir.join("plan.md");
        let strategy = dir.join("strategy.md");
        let spawner = Arc::new(MockSpawner::ok_does_not_write(&strategy, &plan));
        let (log, emit) = collect_events();

        let outcome = run_goal_strategist(spawner, inputs(&plan, &strategy), &emit).await;

        assert!(matches!(
            outcome,
            GoalStrategistOutcome::FailOpen {
                reason: GoalStrategistFailReason::MissingStrategy,
                ..
            }
        ));
        assert!(
            log.lock()
                .unwrap()
                .iter()
                .any(|t| t == "failed:missing_strategy_file")
        );
    }

    #[tokio::test]
    async fn transport_error_fails_open() {
        let dir = tmp_dir("transport");
        let plan = dir.join("plan.md");
        let strategy = dir.join("strategy.md");
        let spawner = Arc::new(MockSpawner::fails(
            SpawnError::Transport("channel closed".into()),
            &strategy,
            &plan,
        ));
        let (log, emit) = collect_events();

        let outcome = run_goal_strategist(spawner, inputs(&plan, &strategy), &emit).await;

        assert!(matches!(
            outcome,
            GoalStrategistOutcome::FailOpen {
                reason: GoalStrategistFailReason::Transport,
                ..
            }
        ));
        assert!(log.lock().unwrap().iter().any(|t| t == "failed:transport"));
    }

    #[tokio::test]
    async fn runtime_cancelled_maps_to_aborted() {
        let dir = tmp_dir("aborted");
        let plan = dir.join("plan.md");
        let strategy = dir.join("strategy.md");
        let spawner = Arc::new(MockSpawner::fails(
            SpawnError::Runtime {
                message: "user aborted".into(),
                cancelled: true,
            },
            &strategy,
            &plan,
        ));
        let (log, emit) = collect_events();

        let outcome = run_goal_strategist(spawner, inputs(&plan, &strategy), &emit).await;

        assert!(matches!(
            outcome,
            GoalStrategistOutcome::FailOpen {
                reason: GoalStrategistFailReason::Aborted,
                ..
            }
        ));
        assert!(log.lock().unwrap().iter().any(|t| t == "failed:aborted"));
    }

    /// A strategist edit to plan.md is reverted byte-for-byte — the WHOLE
    /// file, including a non-contract `## Task checklist` block (rationale on
    /// `GoalTracker::strategy_path`).
    #[tokio::test]
    async fn strategist_edit_to_plan_md_is_reverted() {
        let dir = tmp_dir("plan-guard");
        let plan = dir.join("plan.md");
        let strategy = dir.join("strategy.md");
        const CONTRACT: &[u8] =
            b"# Plan\n\n## Acceptance criteria\n\n1. ship it\n\n## Verification plan\n\n1. run tests\n\n## Task checklist\n\n- [x] step one\n";
        std::fs::write(&plan, CONTRACT).unwrap();
        let spawner = Arc::new(
            MockSpawner::ok_writes(&strategy, &plan, b"## Diagnosis\n\nrewrite subsystem\n")
                .with_plan_overwrite(b"# Plan\n\n## Acceptance criteria\n\n1. DIFFERENT\n"),
        );
        let (_log, emit) = collect_events();

        let outcome = run_goal_strategist(spawner, inputs(&plan, &strategy), &emit).await;

        assert!(matches!(outcome, GoalStrategistOutcome::Advised { .. }));
        assert_eq!(
            std::fs::read(&plan).unwrap(),
            CONTRACT,
            "plan.md contract must be restored byte-for-byte after a strategist edit",
        );
    }

    /// plan.md-safety, no-plan variant: when plan.md did not exist before
    /// the run, a strategist that creates it must have the file removed
    /// (the contract owns that path).
    #[tokio::test]
    async fn strategist_created_plan_md_is_removed_when_absent_before() {
        let dir = tmp_dir("plan-create");
        let plan = dir.join("plan.md");
        let strategy = dir.join("strategy.md");
        let spawner = Arc::new(
            MockSpawner::ok_writes(&strategy, &plan, b"## Diagnosis\n")
                .with_plan_overwrite(b"# bogus plan\n"),
        );
        let (_log, emit) = collect_events();

        let _ = run_goal_strategist(spawner, inputs(&plan, &strategy), &emit).await;

        assert!(
            !plan.exists(),
            "strategist-created plan.md must be removed when it was absent before the run",
        );
    }

    /// plan.md-safety on the FAILURE path: a strategist that overwrites
    /// plan.md AND then reports a runtime failure must still have its edit
    /// reverted (the guard restores regardless of spawn outcome).
    #[tokio::test]
    async fn strategist_edit_to_plan_md_is_reverted_on_failure_path() {
        let dir = tmp_dir("plan-guard-fail");
        let plan = dir.join("plan.md");
        let strategy = dir.join("strategy.md");
        const CONTRACT: &[u8] = b"# Plan\n\n## Acceptance criteria\n\n1. ship it\n";
        std::fs::write(&plan, CONTRACT).unwrap();
        let spawner = Arc::new(
            MockSpawner::fails(
                SpawnError::Runtime {
                    message: "boom".into(),
                    cancelled: false,
                },
                &strategy,
                &plan,
            )
            .with_plan_overwrite(b"# tampered\n"),
        );
        let (_log, emit) = collect_events();

        let outcome = run_goal_strategist(spawner, inputs(&plan, &strategy), &emit).await;

        assert!(matches!(
            outcome,
            GoalStrategistOutcome::FailOpen {
                reason: GoalStrategistFailReason::Runtime,
                ..
            }
        ));
        assert_eq!(
            std::fs::read(&plan).unwrap(),
            CONTRACT,
            "plan.md must be restored even when the spawn fails",
        );
    }

    /// Symlink tampering: the strategist replaces the regular
    /// plan.md with a symlink to a secret file. The guard must NOT follow
    /// it (never write the contract bytes through the symlink to the secret)
    /// and must surface a `GoalStrategistContractRestoreFailed` event.
    #[cfg(unix)]
    #[tokio::test]
    async fn restore_refuses_to_follow_a_planted_symlink_and_reports() {
        let dir = tmp_dir("plan-symlink");
        let plan = dir.join("plan.md");
        let strategy = dir.join("strategy.md");
        let secret = dir.join("secret.txt");
        const CONTRACT: &[u8] = b"# Plan\n\n## Acceptance criteria\n\n1. ship it\n";
        const SECRET: &[u8] = b"sensitive - do not overwrite\n";
        std::fs::write(&plan, CONTRACT).unwrap();
        std::fs::write(&secret, SECRET).unwrap();
        let spawner = Arc::new(
            MockSpawner::ok_writes(&strategy, &plan, b"## Diagnosis\n")
                .with_plan_replaced_by_symlink(&secret),
        );
        let (log, emit) = collect_events();

        let _ = run_goal_strategist(spawner, inputs(&plan, &strategy), &emit).await;

        assert_eq!(
            std::fs::read(&secret).unwrap(),
            SECRET,
            "guard must NOT write contract bytes through the planted symlink",
        );
        assert!(
            log.lock()
                .unwrap()
                .iter()
                .any(|t| t == "restore_failed:symlink_tamper"),
            "symlink tampering must surface a restore-failed telemetry event: {:?}",
            log.lock().unwrap(),
        );
    }

    /// A symlink at plan.md AT CAPTURE is snapshotted `Unsafe`, and the
    /// restore is a strict no-op — the symlink is never followed, deleted, or
    /// written through. This is the contract-deletion regression
    /// vector (an `Unsafe` capture must never lead to a delete/overwrite).
    #[cfg(unix)]
    #[test]
    fn capture_of_symlinked_plan_is_unsafe_and_restore_no_ops() {
        let dir = tmp_dir("capture-symlink");
        let secret = dir.join("secret.txt");
        let plan = dir.join("plan.md");
        std::fs::write(&secret, b"secret\n").unwrap();
        std::os::unix::fs::symlink(&secret, &plan).unwrap();

        let mut guard = PlanGuard::capture(&plan);
        assert!(
            matches!(guard.snapshot, PlanSnapshot::Unsafe),
            "a symlink at capture must snapshot Unsafe",
        );
        assert_eq!(guard.restore(), None, "Unsafe restore must be a no-op");
        assert!(
            std::fs::symlink_metadata(&plan)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the symlink must be left in place (never deleted)",
        );
        assert_eq!(
            std::fs::read(&secret).unwrap(),
            b"secret\n",
            "the symlink target must be untouched (never written through)",
        );
    }

    /// `RemoveFailed` — snapshot was `Absent`, but the path that appears
    /// is a directory (not a regular file), so `remove_file` fails. Triggered
    /// portably by creating a directory at the path rather than relying on
    /// filesystem permissions (which root would bypass in CI).
    #[test]
    fn restore_remove_failed_when_created_path_is_a_directory() {
        let dir = tmp_dir("remove-fail");
        let plan = dir.join("plan.md");
        // Absent at capture.
        let mut guard = PlanGuard::capture(&plan);
        // The "strategist" creates a directory where the regular file would be.
        std::fs::create_dir(&plan).unwrap();
        assert_eq!(
            guard.restore(),
            Some(GoalStrategistRestoreFailReason::RemoveFailed),
            "removing a directory must surface RemoveFailed",
        );
    }

    /// `WriteFailed` — snapshot was `Present`, but the path is replaced by
    /// a directory before restore, so the rewrite fails (EISDIR). Portable
    /// (no permission games).
    #[test]
    fn restore_write_failed_when_path_becomes_a_directory() {
        let dir = tmp_dir("write-fail");
        let plan = dir.join("plan.md");
        std::fs::write(&plan, b"# contract\n").unwrap();
        let mut guard = PlanGuard::capture(&plan);
        // The "strategist" swaps the file for a directory at the same path.
        std::fs::remove_file(&plan).unwrap();
        std::fs::create_dir(&plan).unwrap();
        assert_eq!(
            guard.restore(),
            Some(GoalStrategistRestoreFailReason::WriteFailed),
            "rewriting over a directory must surface WriteFailed",
        );
    }

    #[tokio::test]
    async fn prompt_substitutes_paths_and_carries_inputs() {
        let dir = tmp_dir("prompt");
        let plan = dir.join("plan.md");
        let strategy = dir.join("strategy.md");
        std::fs::write(&plan, b"# Plan\n").unwrap();
        let spawner = Arc::new(MockSpawner::ok_writes(&strategy, &plan, b"note\n"));
        let spawner_obs = spawner.clone();
        let (_log, emit) = collect_events();

        let _ = run_goal_strategist(spawner, inputs(&plan, &strategy), &emit).await;

        let prompt = spawner_obs
            .last_prompt
            .lock()
            .unwrap()
            .clone()
            .expect("prompt captured");
        assert!(
            !prompt.contains("{STRATEGY_FILE}")
                && !prompt.contains("{PLAN_FILE}")
                && !prompt.contains("{SESSION_TRACES_DIR}")
                && !prompt.contains("{SCRATCH_ROOT}"),
            "placeholders must be substituted"
        );
        assert!(prompt.contains(&*strategy.to_string_lossy()));
        assert!(prompt.contains("OBJECTIVE:\ndo X"));
        // The session traces dir is substituted in; the strategist reads the
        // traces itself rather than receiving a gaps/diff packet.
        assert!(prompt.contains(&*plan.parent().unwrap().to_string_lossy()));
        assert!(!prompt.contains("VERIFIER GAPS:"));
        assert!(!prompt.contains("REPO CHANGES"));
    }

    use crate::session::goal_role_tools::tests::assert_no_tool_placeholders;

    /// Default/inherit render: the tool placeholders resolve to the literal
    /// parent (echo-agent-build) names, with no placeholder left behind. Guards
    /// against accidental wording drift in the strategist template.
    #[test]
    fn strategist_template_default_render_has_no_placeholders() {
        let rendered = RoleToolNames::inherit_defaults().apply(GOAL_STRATEGIST_PROMPT_TEMPLATE);
        assert_no_tool_placeholders(&rendered);
    }

    /// An explicit named toolset renders the tool names, and the
    /// explicit `from_summary` path leaves no tool placeholder unresolved.
    #[test]
    fn strategist_template_renders_per_agent_type_names() {
        use echo_agent_tools::implementations::echo_agent_build::task::types::SubagentTypeSummary;
        let mut tool_names = std::collections::HashMap::new();
        tool_names.insert(
            echo_agent_tools::types::tool::ToolKind::Read,
            "alt_read".to_string(),
        );
        tool_names.insert(
            echo_agent_tools::types::tool::ToolKind::Execute,
            "alt_shell".to_string(),
        );
        let summary = SubagentTypeSummary {
            can_read: true,
            can_execute: true,
            tool_names,
            ..Default::default()
        };
        let rendered = RoleToolNames::from_summary(&summary).apply(GOAL_STRATEGIST_PROMPT_TEMPLATE);
        assert!(rendered.contains("`alt_read`"));
        assert!(rendered.contains("`alt_shell`"));
        // Kinds absent from the summary fall back to the literal defaults.
        assert!(rendered.contains("`grep`") && rendered.contains("`list_dir`"));
        assert_no_tool_placeholders(&rendered);
    }
}
