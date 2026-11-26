//! Achievement-triggered goal summarizer subagent runner.
//!
//! Mirrors [`crate::session::goal_strategist`] in shape but fires on the
//! OPPOSITE condition: exactly ONCE after a goal is verified-ACHIEVED, to
//! produce the closing user-facing summary that becomes the last thing the
//! user reads.
//!
//! Fail-OPEN: the goal is already complete before it runs, so any failure
//! (transport / runtime / cancel / empty output) is logged via
//! `GoalSummarizerFailOpen` and ignored — completion is never blocked.
//!
//! Read-only: the summary IS the subagent's terminal output (no file
//! read-back), and the spawn pins a read-only capability mode.

use crate::session::events::{Event, GoalSummarizerFailReason};
use crate::session::goal_planner::{
    GOAL_ROLE_AWAIT_BUDGET_EXCEEDED, GOAL_ROLE_SUBAGENT_TYPE, RoleRenderedPrompt,
    RoleSpawnOverride, SpawnError, spawn_with_fail_open_retry,
};
use crate::session::goal_role_tools::RoleToolNames;
use echo_agent_session_events::EventWriter;
use echo_agent_tool_types::SubagentCapabilityMode;
use echo_agent_tools::implementations::echo_agent_build::task::backend::{
    ChannelBackend, SubagentBackend,
};
use echo_agent_tools::implementations::echo_agent_build::task::types::{
    SubagentOwner, SubagentRequest, SubagentRuntimeOverrides,
};
use std::path::Path;
use std::sync::Arc;

// Constants

/// Same general-purpose inventory the other goal roles use; the read-only
/// capability mode (set on the spawn) narrows it to inspect-only tools. A
/// configured `agent_type` selects the HARNESS, not this subagent type.
const GOAL_SUMMARIZER_SUBAGENT_TYPE: &str = GOAL_ROLE_SUBAGENT_TYPE;

/// Description shown in the pager subagent strip and matched by the e2e
/// coordinator stub to distinguish summarizer spawns from other roles.
pub(crate) const GOAL_SUMMARIZER_SUBAGENT_DESCRIPTION: &str = "goal summarizer";

const GOAL_SUMMARIZER_PROMPT_TEMPLATE: &str = include_str!("templates/goal_summarizer_prompt.md");

/// Hard backstop on the surfaced summary length, in chars (`chars().take` is
/// char-boundary-safe). Sits well above a compliant summary — it only clips a
/// model that ignores the prompt's word cap.
const GOAL_SUMMARIZER_SUMMARY_MAX_CHARS: usize = 1200;

// Outcome + spawner abstraction

/// Result of the one summarizer attempt. `Summarized` carries the closing
/// summary text the caller surfaces to the user. `FailOpen` carries the reason
/// — every variant is logged and ignored at the call site (the goal stays
/// completed; only the closing summary is skipped).
#[derive(Debug, Clone)]
#[expect(
    dead_code,
    reason = "`latency_ms` is consumed by telemetry / future use"
)]
pub(crate) enum GoalSummarizerOutcome {
    Summarized {
        summary: String,
        latency_ms: u64,
    },
    FailOpen {
        reason: GoalSummarizerFailReason,
        latency_ms: u64,
    },
}

/// Subagent spawn abstraction. Production uses [`ChannelSpawner`]; tests use
/// `MockSpawner` (defined in the tests module). `SpawnError` is reused from the
/// planner module rather than re-declared.
#[async_trait::async_trait]
pub(crate) trait GoalSummarizerSpawner: Send + Sync {
    /// Spawn under `id` and return the terminal response (the summary) when the
    /// subagent finishes. `prompt` carries the configured-pair render
    /// (`primary`) and the default-toolset retry render (`fallback`); the
    /// summarizer always inherits, so only `primary` is used.
    async fn spawn_summarizer(
        &self,
        id: &str,
        prompt: RoleRenderedPrompt,
    ) -> Result<String, SpawnError>;
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
    /// Event sink for the spawn-and-retry-once fail-open telemetry; `None` in
    /// tests / when no event log is wired.
    pub(crate) events: Option<EventWriter>,
}

#[async_trait::async_trait]
impl GoalSummarizerSpawner for ChannelSpawner {
    async fn spawn_summarizer(
        &self,
        id: &str,
        prompt: RoleRenderedPrompt,
    ) -> Result<String, SpawnError> {
        // Clone the primary render for the trace pair only when tracing.
        let trace_prompt = self.trace_sink.as_ref().map(|_| prompt.primary.clone());
        // The summarizer always inherits the current model + general-purpose
        // toolset (no per-role model key), so the wrapper runs a single attempt.
        let override_ = RoleSpawnOverride::default();
        let outcome = spawn_with_fail_open_retry(
            "summarizer",
            None,
            &override_,
            self.events.as_ref(),
            prompt,
            |model, harness, prompt| self.send_one(id, prompt, model, harness),
        )
        .await;

        match &outcome {
            Ok(text) => crate::session::goal_classifier::record_subagent_trace(
                self.trace_sink.as_ref(),
                id,
                GOAL_SUMMARIZER_SUBAGENT_TYPE,
                GOAL_SUMMARIZER_SUBAGENT_DESCRIPTION,
                trace_prompt.as_deref(),
                text,
            ),
            Err(SpawnError::Runtime { message, .. }) => {
                crate::session::goal_classifier::record_subagent_trace(
                    self.trace_sink.as_ref(),
                    id,
                    GOAL_SUMMARIZER_SUBAGENT_TYPE,
                    GOAL_SUMMARIZER_SUBAGENT_DESCRIPTION,
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
    /// await its terminal result. The subagent_type is always
    /// [`GOAL_SUMMARIZER_SUBAGENT_TYPE`]; `harness_agent_type` selects the
    /// harness flavor (`None` ⇒ session harness). Pins a read-only capability
    /// mode so the subagent can inspect but never edit or execute.
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
            description: GOAL_SUMMARIZER_SUBAGENT_DESCRIPTION.to_string(),
            subagent_type: GOAL_SUMMARIZER_SUBAGENT_TYPE.to_string(),
            parent_session_id: self.parent_session_id.clone(),
            parent_prompt_id: self.parent_prompt_id.clone(),
            resume_from: None,
            cwd: self.cwd.clone(),
            runtime_overrides: SubagentRuntimeOverrides {
                model,
                harness_agent_type,
                capability_mode: Some(SubagentCapabilityMode::ReadOnly),
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

pub(crate) struct GoalSummarizerInputs<'a> {
    pub objective: &'a str,
    /// The verifier-judged plan (acceptance criteria); read for context. May
    /// not exist on disk (planner disabled).
    pub plan_file: &'a Path,
    /// Path string of the verifier's rescued final details file (used only to
    /// substitute `{DETAILS_FILE}` in the prompt; the read-only subagent opens
    /// it itself). `None` renders the `(unavailable)` sentinel.
    pub details_file: Option<&'a str>,
    /// Absolute path to the session traces dir; the summarizer may skim
    /// `chat_history.jsonl` for intent.
    pub session_traces_dir: &'a Path,
    pub attempt: u32,
    pub model_id: &'a str,
    /// Resolved §7 tool names for the inherited parent toolset.
    pub tool_names: &'a RoleToolNames,
}

/// Run the one summarizer attempt. Fail-OPEN: every failure path returns
/// `FailOpen { reason }` and the caller logs it and completes the goal — the
/// summary is the only thing skipped, never the completion.
pub(crate) async fn run_goal_summarizer(
    spawner: Arc<dyn GoalSummarizerSpawner>,
    inputs: GoalSummarizerInputs<'_>,
    emit_event: &dyn Fn(Event),
) -> GoalSummarizerOutcome {
    let started = std::time::Instant::now();
    emit_event(Event::GoalSummarizerFired {
        attempt: inputs.attempt,
        model_id: inputs.model_id.to_string(),
    });

    let plan_file_str = inputs.plan_file.to_string_lossy();
    let details_str = inputs.details_file.unwrap_or("(unavailable)");
    let traces_dir_str = inputs.session_traces_dir.to_string_lossy();
    let with_paths = GOAL_SUMMARIZER_PROMPT_TEMPLATE
        .replace("{PLAN_FILE}", &plan_file_str)
        .replace("{DETAILS_FILE}", details_str)
        .replace("{SESSION_TRACES_DIR}", &traces_dir_str);
    let rendered = inputs.tool_names.apply(&with_paths);
    let mut prompt_text = String::with_capacity(rendered.len() + inputs.objective.len() + 32);
    prompt_text.push_str(&rendered);
    prompt_text.push_str("\n\nOBJECTIVE:\n");
    prompt_text.push_str(inputs.objective);
    prompt_text.push('\n');
    // Inherit path (default override never retries): only `primary` is read, so
    // `fallback` stays empty — no needless clone of the rendered prompt.
    let prompt = RoleRenderedPrompt {
        primary: prompt_text,
        fallback: String::new(),
    };

    let spawn_id = uuid::Uuid::now_v7().to_string();
    let response = match spawner.spawn_summarizer(&spawn_id, prompt).await {
        Ok(text) => text,
        Err(SpawnError::Transport(detail)) => {
            tracing::warn!(error = %detail, "goal summarizer: transport error; failing open");
            return record_fail_open(
                GoalSummarizerFailReason::Transport,
                inputs.attempt,
                started,
                emit_event,
            );
        }
        Err(SpawnError::Interrupted) => {
            return record_fail_open(
                GoalSummarizerFailReason::Aborted,
                inputs.attempt,
                started,
                emit_event,
            );
        }
        Err(SpawnError::Runtime { message, cancelled }) => {
            let reason = if cancelled {
                GoalSummarizerFailReason::Aborted
            } else {
                GoalSummarizerFailReason::Runtime
            };
            tracing::warn!(
                error = %message,
                cancelled,
                "goal summarizer: subagent runtime error; failing open",
            );
            return record_fail_open(reason, inputs.attempt, started, emit_event);
        }
    };

    let trimmed = response.trim();
    if trimmed.is_empty() {
        tracing::info!("goal summarizer: empty summary; failing open");
        return record_fail_open(
            GoalSummarizerFailReason::EmptySummary,
            inputs.attempt,
            started,
            emit_event,
        );
    }

    // Backstop the prompt's word cap (a model can ignore it). A strict char
    // prefix has fewer bytes than the whole, so the byte-length compare below
    // detects a real cut.
    let mut summary: String = trimmed
        .chars()
        .take(GOAL_SUMMARIZER_SUMMARY_MAX_CHARS)
        .collect();
    if summary.len() < trimmed.len() {
        summary.push_str(" […]");
    }

    let latency_ms = started.elapsed().as_millis() as u64;
    emit_event(Event::GoalSummarizerCompleted {
        attempt: inputs.attempt,
        latency_ms,
    });
    GoalSummarizerOutcome::Summarized {
        summary,
        latency_ms,
    }
}

fn record_fail_open(
    reason: GoalSummarizerFailReason,
    attempt: u32,
    started: std::time::Instant,
    emit_event: &dyn Fn(Event),
) -> GoalSummarizerOutcome {
    let latency_ms = started.elapsed().as_millis() as u64;
    emit_event(Event::GoalSummarizerFailOpen {
        reason: reason.as_const_str(),
        attempt,
        latency_ms,
    });
    GoalSummarizerOutcome::FailOpen { reason, latency_ms }
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    #[test]
    fn prompt_render_resolves_tool_placeholders_and_inlines_inputs() {
        let dir = tmp_dir("render");
        let plan = dir.join("plan.md");
        let details = dir.join("details.md");
        let plan_file_str = plan.to_string_lossy();
        let details_str = details.to_string_lossy();
        let traces_dir_str = dir.to_string_lossy();
        let with_paths = GOAL_SUMMARIZER_PROMPT_TEMPLATE
            .replace("{PLAN_FILE}", &plan_file_str)
            .replace("{DETAILS_FILE}", &details_str)
            .replace("{SESSION_TRACES_DIR}", &traces_dir_str);
        let rendered = RoleToolNames::inherit_defaults().apply(&with_paths);
        // §7 tool placeholders resolve to the default echo-agent-build names.
        assert!(rendered.contains("read_file"));
        assert!(rendered.contains("grep"));
        assert!(rendered.contains("list_dir"));
        crate::session::goal_role_tools::tests::assert_no_tool_placeholders(&rendered);
    }

    /// Deterministic spawner: returns a fixed response (or error) and records
    /// the prompt it was handed.
    struct MockSpawner {
        response: Result<String, SpawnError>,
        last_prompt: Mutex<Option<String>>,
    }

    impl MockSpawner {
        fn ok(summary: &str) -> Self {
            Self {
                response: Ok(summary.to_string()),
                last_prompt: Mutex::new(None),
            }
        }

        fn fails(err: SpawnError) -> Self {
            Self {
                response: Err(err),
                last_prompt: Mutex::new(None),
            }
        }
    }

    #[async_trait::async_trait]
    impl GoalSummarizerSpawner for MockSpawner {
        async fn spawn_summarizer(
            &self,
            #[allow(unused_variables)] id: &str,
            prompt: RoleRenderedPrompt,
        ) -> Result<String, SpawnError> {
            *self.last_prompt.lock().unwrap() = Some(prompt.primary);
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
                Event::GoalSummarizerFired { .. } => "fired".to_string(),
                Event::GoalSummarizerCompleted { .. } => "completed".to_string(),
                Event::GoalSummarizerFailOpen { reason, .. } => format!("fail_open:{reason}"),
                other => format!("other:{other:?}"),
            };
            log_clone.lock().unwrap().push(tag);
        };
        (log, emit)
    }

    fn tmp_dir(name: &str) -> PathBuf {
        let tmp = std::env::temp_dir().join(format!(
            "goal-summarizer-{name}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        tmp
    }

    fn inputs<'a>(plan: &'a Path, tool_names: &'a RoleToolNames) -> GoalSummarizerInputs<'a> {
        GoalSummarizerInputs {
            objective: "do X",
            plan_file: plan,
            details_file: None,
            session_traces_dir: plan.parent().unwrap(),
            attempt: 2,
            model_id: "echo-agent-test",
            tool_names,
        }
    }

    #[tokio::test]
    async fn success_path_emits_fired_and_completed_and_returns_summary() {
        let dir = tmp_dir("happy");
        let plan = dir.join("plan.md");
        let tn = RoleToolNames::inherit_defaults();
        let spawner = Arc::new(MockSpawner::ok(
            "  Shipped the feature.\n\n- did a thing\n  ",
        ));
        let (log, emit) = collect_events();

        let outcome = run_goal_summarizer(spawner, inputs(&plan, &tn), &emit).await;

        match outcome {
            GoalSummarizerOutcome::Summarized { summary, .. } => {
                assert!(summary.starts_with("Shipped the feature."));
                assert!(summary.contains("did a thing"));
            }
            other => panic!("expected Summarized, got {other:?}"),
        }
        assert_eq!(log.lock().unwrap().as_slice(), ["fired", "completed"]);
    }

    #[tokio::test]
    async fn empty_summary_fails_open() {
        let dir = tmp_dir("empty");
        let plan = dir.join("plan.md");
        let tn = RoleToolNames::inherit_defaults();
        let spawner = Arc::new(MockSpawner::ok("   \n  \t "));
        let (log, emit) = collect_events();

        let outcome = run_goal_summarizer(spawner, inputs(&plan, &tn), &emit).await;

        assert!(matches!(
            outcome,
            GoalSummarizerOutcome::FailOpen {
                reason: GoalSummarizerFailReason::EmptySummary,
                ..
            }
        ));
        assert!(
            log.lock()
                .unwrap()
                .iter()
                .any(|t| t == "fail_open:empty_summary")
        );
    }

    #[tokio::test]
    async fn transport_error_fails_open() {
        let dir = tmp_dir("transport");
        let plan = dir.join("plan.md");
        let tn = RoleToolNames::inherit_defaults();
        let spawner = Arc::new(MockSpawner::fails(SpawnError::Transport("closed".into())));
        let (log, emit) = collect_events();

        let outcome = run_goal_summarizer(spawner, inputs(&plan, &tn), &emit).await;

        assert!(matches!(
            outcome,
            GoalSummarizerOutcome::FailOpen {
                reason: GoalSummarizerFailReason::Transport,
                ..
            }
        ));
        assert!(
            log.lock()
                .unwrap()
                .iter()
                .any(|t| t == "fail_open:transport")
        );
    }

    #[tokio::test]
    async fn runtime_cancelled_maps_to_aborted() {
        let dir = tmp_dir("aborted");
        let plan = dir.join("plan.md");
        let tn = RoleToolNames::inherit_defaults();
        let spawner = Arc::new(MockSpawner::fails(SpawnError::Runtime {
            message: "user aborted".into(),
            cancelled: true,
        }));
        let (log, emit) = collect_events();

        let outcome = run_goal_summarizer(spawner, inputs(&plan, &tn), &emit).await;

        assert!(matches!(
            outcome,
            GoalSummarizerOutcome::FailOpen {
                reason: GoalSummarizerFailReason::Aborted,
                ..
            }
        ));
        assert!(log.lock().unwrap().iter().any(|t| t == "fail_open:aborted"));
    }

    #[tokio::test]
    async fn runtime_not_cancelled_maps_to_runtime_wire_string() {
        let dir = tmp_dir("runtime");
        let plan = dir.join("plan.md");
        let tn = RoleToolNames::inherit_defaults();
        let spawner = Arc::new(MockSpawner::fails(SpawnError::Runtime {
            message: "subagent crashed".into(),
            cancelled: false,
        }));
        let (log, emit) = collect_events();

        let outcome = run_goal_summarizer(spawner, inputs(&plan, &tn), &emit).await;

        assert!(matches!(
            outcome,
            GoalSummarizerOutcome::FailOpen {
                reason: GoalSummarizerFailReason::Runtime,
                ..
            }
        ));
        // Pins the `"runtime"` wire const emitted on `GoalSummarizerFailOpen`.
        assert!(log.lock().unwrap().iter().any(|t| t == "fail_open:runtime"));
    }

    #[tokio::test]
    async fn oversized_summary_is_truncated_with_marker() {
        let dir = tmp_dir("toolong");
        let plan = dir.join("plan.md");
        let tn = RoleToolNames::inherit_defaults();
        // A runaway wall of text well past the cap.
        let wall = "x".repeat(GOAL_SUMMARIZER_SUMMARY_MAX_CHARS + 500);
        let spawner = Arc::new(MockSpawner::ok(&wall));
        let (_log, emit) = collect_events();

        let outcome = run_goal_summarizer(spawner, inputs(&plan, &tn), &emit).await;

        let GoalSummarizerOutcome::Summarized { summary, .. } = outcome else {
            panic!("expected Summarized");
        };
        assert!(summary.ends_with(" […]"), "truncation marker appended");
        assert_eq!(
            summary.chars().count(),
            GOAL_SUMMARIZER_SUMMARY_MAX_CHARS + " […]".chars().count(),
            "capped at the limit (+ marker)",
        );
    }

    #[tokio::test]
    async fn compliant_summary_is_returned_unchanged() {
        let dir = tmp_dir("compliant");
        let plan = dir.join("plan.md");
        let tn = RoleToolNames::inherit_defaults();
        let spawner = Arc::new(MockSpawner::ok("Shipped X.\n- a\n- b\nVerified by tests."));
        let (_log, emit) = collect_events();

        let outcome = run_goal_summarizer(spawner, inputs(&plan, &tn), &emit).await;

        let GoalSummarizerOutcome::Summarized { summary, .. } = outcome else {
            panic!("expected Summarized");
        };
        assert_eq!(summary, "Shipped X.\n- a\n- b\nVerified by tests.");
        assert!(!summary.contains("[…]"));
    }

    #[tokio::test]
    async fn prompt_carries_objective() {
        let dir = tmp_dir("prompt");
        let plan = dir.join("plan.md");
        let tn = RoleToolNames::inherit_defaults();
        let spawner = Arc::new(MockSpawner::ok("ok"));
        let captured = spawner.clone();
        let (_log, emit) = collect_events();

        let _ = run_goal_summarizer(spawner, inputs(&plan, &tn), &emit).await;

        let prompt = captured.last_prompt.lock().unwrap().clone().unwrap();
        assert!(prompt.contains("OBJECTIVE:\ndo X"));
    }

    #[tokio::test]
    async fn channel_spawner_request_is_harness_internal_and_read_only() {
        use echo_agent_tool_types::SubagentCapabilityMode;
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
            events: None,
        };
        let handle = tokio::spawn(async move {
            let _ = spawner
                .spawn_summarizer(
                    "sum-id",
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
            "summarizer subagent must not surface to the idle reminder"
        );
        assert_eq!(request.description, GOAL_SUMMARIZER_SUBAGENT_DESCRIPTION);
        assert_eq!(
            request.runtime_overrides.capability_mode,
            Some(SubagentCapabilityMode::ReadOnly),
            "summarizer must spawn with a read-only toolset",
        );
        let _ = request.result_tx.send(SubagentResult::default());
        handle.await.unwrap();
    }
}
