//! Laziness / stop-detector concern for `SessionActor`.

use super::*;

/// Per-fire metadata captured at the top of `maybe_fire_laziness_check`
/// when `--laziness-debug-log` is set. Threaded through the
/// `maybe_write_laziness_debug_log` helper so every JSONL line
/// references the same snapshot point.
pub(crate) struct LazinessFireMeta {
    pub session_id: String,
    pub todo_snapshot: Vec<DebugTodoSnapshot>,
    pub backing_task_count: usize,
}

/// Closed-set discriminator over the outcome of one classifier fire,
/// used purely to drive the JSONL line shape. The production code
/// path does not match on this — it uses `LazinessDecision` /
/// `LazinessAbortReason` directly.
pub(crate) enum LazinessFireOutcome {
    /// User input, model switch, timeout, or sampler/HTTP failure
    /// before a verdict was produced. `error_detail` is `Some` only
    /// for `ClassifierError` reasons (sampler/HTTP failure or
    /// `prepare_chat_completion` failure).
    Aborted {
        reason: LazinessAbortReason,
        error_detail: Option<String>,
    },
    /// Sampler returned text but `parse_classifier_output` rejected
    /// it. Both the raw text and the parse-error detail are kept for
    /// offline analysis.
    ParseError {
        raw_text: String,
        parse_error_detail: String,
    },
    /// Sampler returned text and it parsed cleanly. The debug
    /// decision (`WouldNudge` / `NoNudgeLowConfidence` /
    /// `NoNudgeNotStalled`) is computed inside `build_laziness_debug_line`
    /// from `parsed`.
    Verdict {
        parsed: ClassifierOutput,
        raw_text: String,
    },
    /// Classifier returned `Nudge` but harness policy blocked injection
    /// (e.g. session not in an active goal).
    Suppressed {
        reason: LazinessSuppressReason,
        parsed: ClassifierOutput,
        raw_text: String,
    },
}

/// Why a `Nudge` verdict was not injected into the transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LazinessSuppressReason {
    /// Post-classifier gate: goal harness is off or goal is not `Active`.
    NotGoalMode,
}

/// Pure helper — build a `LazinessDebugLogLine` from the captured
/// per-fire metadata and a classifier outcome. Extracted so the
/// JSON-line shape stays unit-testable without a `SessionActor`.
/// Consumes `meta` so its `String` + `Vec` fields move into the
/// output line (no clones on the per-fire hot path).
pub(crate) fn build_laziness_debug_line(
    meta: LazinessFireMeta,
    model_id: &str,
    items_sent: usize,
    classifier_elapsed_ms: u64,
    outcome: LazinessFireOutcome,
) -> LazinessDebugLogLine {
    let LazinessFireMeta {
        session_id,
        todo_snapshot,
        backing_task_count,
    } = meta;
    let base = LazinessDebugLogLine {
        timestamp: chrono::Utc::now().to_rfc3339(),
        session_id,
        model_id: model_id.to_owned(),
        items_sent,
        todo_snapshot,
        backing_task_count,
        classifier_raw_output: None,
        parsed: None,
        decision: DebugDecision::Aborted,
        abort_reason: None,
        error_detail: None,
        classifier_elapsed_ms,
    };
    match outcome {
        LazinessFireOutcome::Aborted {
            reason,
            error_detail,
        } => LazinessDebugLogLine {
            abort_reason: Some(reason.as_const_str()),
            error_detail,
            ..base
        },
        LazinessFireOutcome::ParseError {
            raw_text,
            parse_error_detail,
        } => LazinessDebugLogLine {
            classifier_raw_output: Some(raw_text),
            abort_reason: Some(LazinessAbortReason::ClassifierError.as_const_str()),
            error_detail: Some(parse_error_detail),
            ..base
        },
        LazinessFireOutcome::Verdict { parsed, raw_text } => {
            let decision = classify_debug_decision(&parsed, LAZINESS_DEFAULT_MIN_CONFIDENCE);
            LazinessDebugLogLine {
                classifier_raw_output: Some(raw_text),
                parsed: Some(DebugClassifierOutput::from(parsed)),
                decision,
                ..base
            }
        }
        LazinessFireOutcome::Suppressed {
            reason,
            parsed,
            raw_text,
        } => {
            let decision = match reason {
                LazinessSuppressReason::NotGoalMode => DebugDecision::SuppressedNotGoalMode,
            };
            LazinessDebugLogLine {
                classifier_raw_output: Some(raw_text),
                parsed: Some(DebugClassifierOutput::from(parsed)),
                decision,
                ..base
            }
        }
    }
}

/// Pure helper — given a parsed classifier output and the production
/// min-confidence threshold, classify what production WOULD have done.
/// Extracted so the JSON-line shape can be unit-tested without a
/// SessionActor.
pub(crate) fn classify_debug_decision(
    parsed: &ClassifierOutput,
    min_confidence: f32,
) -> DebugDecision {
    if !parsed.category.is_stalled() {
        return DebugDecision::NoNudgeNotStalled;
    }
    if parsed.confidence < min_confidence {
        return DebugDecision::NoNudgeLowConfidence;
    }
    DebugDecision::WouldNudge
}

#[derive(Debug, Clone, serde::Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DebugDecision {
    /// Parsed verdict was `stalled_*` AND confidence >= threshold.
    /// Production would have injected a nudge here (subject to its
    /// cap, which debug mode ignores).
    WouldNudge,
    /// Parsed verdict was `not_stalled_*`. Production would have
    /// fired classifier telemetry only.
    NoNudgeNotStalled,
    /// Parsed verdict was `stalled_*` but below the configured
    /// min-confidence threshold (default 0.7).
    NoNudgeLowConfidence,
    /// User input / model switch / timeout / parse error before a
    /// verdict was produced. The corresponding `abort_reason` field
    /// names which.
    Aborted,
    /// Classifier returned `stalled_*` above threshold but injection was
    /// blocked because the session is not in an active goal.
    SuppressedNotGoalMode,
}

#[derive(Debug, Clone, serde::Serialize, PartialEq)]
pub(crate) struct DebugClassifierOutput {
    pub category: String,
    pub confidence: f32,
    pub evidence: String,
}

impl From<ClassifierOutput> for DebugClassifierOutput {
    fn from(p: ClassifierOutput) -> Self {
        Self {
            category: p.category.as_const_str().to_string(),
            confidence: p.confidence,
            evidence: p.evidence,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, PartialEq)]
pub(crate) struct DebugTodoSnapshot {
    pub id: String,
    pub status: &'static str,
}

#[derive(Debug, Clone, serde::Serialize, PartialEq)]
pub(crate) struct LazinessDebugLogLine {
    pub timestamp: String,
    pub session_id: String,
    pub model_id: String,
    /// Number of conversation items sent to the classifier (excluding
    /// the prepended system prompt). Capped at `LAZINESS_CONTEXT_ITEM_LIMIT`.
    pub items_sent: usize,
    pub todo_snapshot: Vec<DebugTodoSnapshot>,
    pub backing_task_count: usize,
    /// Raw classifier output text. `None` when the call aborted
    /// before producing any output.
    pub classifier_raw_output: Option<String>,
    /// Parsed structured output. `None` when raw output failed
    /// `parse_classifier_output` or the call aborted.
    pub parsed: Option<DebugClassifierOutput>,
    pub decision: DebugDecision,
    /// One of the `LAZINESS_ABORT_*` consts when `decision == Aborted`;
    /// `None` when the classifier produced a verdict.
    pub abort_reason: Option<&'static str>,
    /// Human-readable detail when the sampler call itself failed
    /// (e.g., backend rejection of the request shape) or when parsing
    /// the response failed. Surfaced verbatim so an operator can see
    /// what the model backend complained about. `None` for clean
    /// classifier runs and for non-error aborts (user_input /
    /// model_switch / timeout).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_detail: Option<String>,
    pub classifier_elapsed_ms: u64,
}

/// Append a single JSONL line to the debug-log file. Creates the file
/// on first write; subsequent writes append. Concurrent callers rely
/// on `O_APPEND` for atomic ordering (JSONL lines fit under `PIPE_BUF`).
pub(crate) async fn append_laziness_debug_log_line(
    log_handle: &std::sync::Arc<std::path::Path>,
    line: &LazinessDebugLogLine,
) -> std::io::Result<()> {
    let mut json = serde_json::to_string(line).map_err(std::io::Error::other)?;
    json.push('\n');
    let path = std::sync::Arc::clone(log_handle);
    tokio::task::spawn_blocking(move || {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&*path)?;
        file.write_all(json.as_bytes())?;
        file.flush()
    })
    .await
    .map_err(std::io::Error::other)?
}

impl SessionActor {
    /// Snapshot the generation counters that gate the classifier's
    /// abort arms. Captured at function entry; subsequent polls compare
    /// against these snapshots to detect a user prompt or model switch
    /// that occurred during the idle wait or sampler call.
    ///
    /// Pure helper: no `&mut self`, no lock acquisition.
    pub(super) fn laziness_abort_snapshot(&self) -> (u64, u64) {
        (
            self.user_input_generation
                .load(std::sync::atomic::Ordering::Acquire),
            self.models_manager.model_switch_generation(),
        )
    }

    /// Compare current generation counters against a starting snapshot.
    /// Returns `Some(reason)` when a user prompt or model switch
    /// happened since the snapshot, `None` otherwise. User-input is
    /// checked first so a near-simultaneous prompt + switch attributes
    /// to the user (the more actionable signal in dashboards).
    pub(super) fn laziness_abort_check(&self, snapshot: (u64, u64)) -> Option<LazinessAbortReason> {
        let (start_user, start_switch) = snapshot;
        if self
            .user_input_generation
            .load(std::sync::atomic::Ordering::Acquire)
            != start_user
        {
            return Some(LazinessAbortReason::UserInput);
        }
        if self.models_manager.model_switch_generation() != start_switch {
            return Some(LazinessAbortReason::ModelSwitch);
        }
        None
    }

    /// Emit a typed `LazinessClassifierAborted` telemetry event for
    /// the given reason. Centralized so every abort path goes through
    /// one site and the closed `LAZINESS_ABORT_*` const set stays
    /// authoritative.
    pub(super) fn emit_laziness_abort(&self, reason: LazinessAbortReason) {
        self.events
            .emit(crate::session::events::Event::LazinessClassifierAborted {
                reason: reason.as_const_str(),
            });
    }

    /// React to a model-switch wakeup from
    /// [`crate::agent::models::ModelsManager`]. Zeroes
    /// `nudges_used_this_session` so the Layer-3 cap is
    /// per-(session, model) rather than per-session. Called from the
    /// actor's main `select!` loop's `_ = model_switch_rx.changed() => …`
    /// arm and exercised directly by unit tests.
    pub(crate) async fn handle_model_switch_for_laziness(&self, new_gen: u64) {
        let mut state = self.state.lock().await;
        if state.nudges_used_this_session != 0 {
            tracing::debug!(
                prior_count = state.nudges_used_this_session,
                new_gen,
                "laziness: resetting nudges_used_this_session on model switch",
            );
            state.nudges_used_this_session = 0;
        }
    }

    /// Layer-3 LazinessDetector: ask the active session model whether
    /// the current idle slice looks like a stall, and if so push a
    /// `<system-reminder>` nudge into chat history for the next real
    /// user turn to pick up. Default behavior is no-op — the feature
    /// is per-model opt-in (default off) with a separate per-session
    /// nudge cap (default 0). See plan §3 "Layer 3 — LazinessDetector".
    ///
    /// **Invisibility contract** (mirrors the memory-flush path):
    /// the sampler call goes through `prepare_chat_completion().conversation_collect()`
    /// — a side-channel direct-HTTP call that does NOT publish events
    /// on the per-session shared sampler channel. The client never
    /// sees "EchoAgent is thinking", streaming token chunks, or any other
    /// session update from a classifier fire. Stalled verdicts only
    /// queue a `<system-reminder>` into chat state via
    /// `push_system_reminder`; no `InputItem` is pushed into
    /// `pending_inputs`, so no synthetic turn fires. The next real
    /// user prompt picks up the reminder as context.
    ///
    /// **Dev toggle (`--laziness-debug-log <path>`)**: when set
    /// (`self.laziness_debug_log.is_some()`), this method
    ///   - forces `cfg.enabled = true` (the classifier fires
    ///     regardless of per-model opt-in),
    ///   - bypasses the idle-threshold wait (effective threshold is
    ///     0 ms, so the classifier fires on every turn end), and
    ///   - appends a JSONL log line at every return point (abort,
    ///     parse error, verdict).
    ///
    /// The cap (`max_nudges_per_session`) and the production
    /// telemetry (`LazinessClassifierFired` / `LazinessNudgeFired` /
    /// `LazinessClassifierAborted`) flow through the same code path
    /// in both modes — debug mode adds logging, it does not bypass
    /// the production decision logic.
    pub(crate) async fn maybe_fire_laziness_check(self: Arc<Self>) {
        let model_id_acp = self.models_manager.current_model_id();
        let model_id = model_id_acp.0.to_string();
        let cfg = self.models_manager.laziness_detector_for(&model_id);
        let debug_mode = self.laziness_debug_log.is_some();

        // Classifier-fire predicate (NOT the nudge-fire predicate —
        // the cap check is inside `evaluate_laziness`). Dev-flag
        // bypass: `debug_mode` forces the classifier to fire even
        // when per-model `enabled = false`, so eval traffic can be
        // collected against any model without flipping its catalog
        // entry.
        if !cfg.enabled && !debug_mode {
            return;
        }
        // Idle predicate — same conditions Layer-2 notification drain
        // consults. Skipped in debug mode (dev flag fires on every
        // turn end to maximize eval-set coverage).
        if !debug_mode {
            let state = self.state.lock().await;
            if !is_session_idle_for_injection(&state) {
                return;
            }
        }

        // Capture the per-fire metadata only when debug mode is
        // active — production builds skip the (cheap but non-zero)
        // tool-bridge reads. Held in an `Option` and consumed via
        // `.take()` at the (single) reached log-write site so the
        // String + Vec fields are moved into the log line, not cloned.
        let mut meta = if debug_mode {
            Some(LazinessFireMeta {
                session_id: self.session_info.id.0.to_string(),
                todo_snapshot: self.snapshot_todos_for_debug_log().await,
                backing_task_count: self.snapshot_backing_task_count_for_debug_log().await,
            })
        } else {
            None
        };

        let started = std::time::Instant::now();
        let idle_threshold = if debug_mode {
            std::time::Duration::ZERO
        } else {
            std::time::Duration::from_millis(
                cfg.idle_threshold_ms
                    .unwrap_or(LAZINESS_DEFAULT_IDLE_THRESHOLD_MS),
            )
        };
        let poll_interval = std::time::Duration::from_millis(LAZINESS_ABORT_POLL_INTERVAL_MS);
        let abort_snapshot = self.laziness_abort_snapshot();

        // Idle wait — polls the generation counters periodically so
        // user input / model switch is observed within `poll_interval`.
        // We use a generation-counter snapshot+poll instead of
        // `tokio::sync::Notify::notified()` to avoid the stored-permit
        // hazard: a `notify_one()` issued before this task spawns
        // would otherwise fire the abort arm on the very first poll.
        // In debug mode the threshold is 0 so this loop is a no-op.
        let deadline = tokio::time::Instant::now() + idle_threshold;
        loop {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                break;
            }
            let next_wait = (deadline - now).min(poll_interval);
            tokio::time::sleep(next_wait).await;
            if let Some(reason) = self.laziness_abort_check(abort_snapshot) {
                let elapsed_ms = started.elapsed().as_millis() as u64;
                // `items_sent: 0` is a sentinel meaning "abort
                // fired before the conversation request was built",
                // not "request was built with an empty conversation".
                self.maybe_write_laziness_debug_log(
                    meta.take(),
                    &model_id,
                    0,
                    elapsed_ms,
                    LazinessFireOutcome::Aborted {
                        reason,
                        error_detail: None,
                    },
                )
                .await;
                self.emit_laziness_abort(reason);
                return;
            }
        }

        // Re-check idle after the sleep and snapshot `nudges_used`.
        // Debug mode bypasses the idle gate (the dev flag forces a
        // fire even mid-turn) but still reads the counter so
        // `evaluate_laziness` sees consistent state with production.
        let nudges_used = {
            let state = self.state.lock().await;
            if !debug_mode && !is_session_idle_for_injection(&state) {
                return;
            }
            state.nudges_used_this_session
        };

        // Build the classifier request as a TWO-ITEM conversation that
        // is COMPLETELY INDEPENDENT from the session under classification:
        //
        //   [0] System(classifier prompt — "you are a strict JSON
        //                                   classifier, you are NOT the
        //                                   agent in the transcript")
        //   [1] User("Classify the following transcript:\n\n<flattened text>")
        //
        // The session's chat items are flattened to plain `[role] text`
        // lines INSIDE the user message — the request body never
        // contains a `ConversationItem::Assistant`, so the model has
        // no assistant turn to continue. This is the fix for the
        // "Done. Let me know what part you're curious about." bug
        // where the model was finishing the agent's own thought
        // instead of classifying the transcript as data.
        //
        // `items_sent` (the count surfaced to telemetry / debug log)
        // reports the number of SOURCE chat items the classifier saw,
        // not the 2 wire items. That matches the historical meaning
        // and keeps the cost dashboard interpretable.
        let mut source_items = self.chat_state_handle.get_conversation().await;
        let window_start = laziness_window_start(
            &source_items,
            LAZINESS_CONTEXT_ITEM_LIMIT,
            LAZINESS_MIN_USER_TURNS,
            LAZINESS_MIN_ASSISTANT_TURNS,
        );
        if window_start > 0 {
            source_items.drain(0..window_start);
        }
        let items_count_after_trim = source_items.len();
        // Per-model override resolves through the harness default so
        // an absent `include_reasoning` value defers to
        // `LAZINESS_INCLUDE_REASONING` rather than forcing a config
        // edit on every model entry.
        let include_reasoning = cfg.include_reasoning.unwrap_or(LAZINESS_INCLUDE_REASONING);
        let transcript_text = flatten_transcript_for_classifier(&source_items, include_reasoning);

        // Harness-truth signals — these are NOT agent-authored, so a
        // lazy agent can't fabricate them to mask a stall:
        //  * `outstanding_subagents`: live `spawn_subagent` calls that
        //    haven't returned. If the agent claims "I launched X" but
        //    this count is 0, that's a `stalled_narration` smoking gun.
        //  * `outstanding_background_tasks`: terminal tasks (bash with
        //    `background: true`, monitors) that haven't completed.
        // We intentionally do NOT inject TodoState — the todo list is
        // agent-authored and can be lazy/fabricated alongside the
        // assistant prose; including it would confirm the lie rather
        // than catch it.
        let backing_task_count = self.snapshot_backing_task_count_for_debug_log().await;
        // Reuse the same field on `meta` if we already captured it
        // above so debug-mode telemetry stays consistent with what
        // the classifier saw.
        if let Some(m) = meta.as_mut() {
            m.backing_task_count = backing_task_count;
        }
        // `turn_elapsed_seconds` is also harness-truth (snapped by the
        // session actor at turn start) — the agent cannot fabricate it.
        // Cross-check with claims of "overnight run", "N hours of work",
        // etc. in the prose audit. The pure helper handles the
        // negative-delta / absent-timestamp branches; sub-second
        // deltas truncate to 0, which the classifier reads as an
        // explicit "very recent" signal rather than an absent field.
        let turn_start_ms = self
            .chat_state_handle
            .get_notification_meta()
            .await
            .and_then(|m| m.turn_start_ms);
        let turn_elapsed_seconds = turn_elapsed_seconds_from_start_ms(
            turn_start_ms,
            chrono::Utc::now().timestamp_millis(),
        );
        let runtime_state = format_runtime_state_line(backing_task_count, turn_elapsed_seconds);

        let items = vec![
            ConversationItem::System(echo_agent_sampling_types::SystemItem {
                content: std::sync::Arc::<str>::from(LAZINESS_CLASSIFIER_PROMPT),
            }),
            ConversationItem::User(echo_agent_sampling_types::UserItem {
                content: vec![echo_agent_sampling_types::ContentPart::Text {
                    text: std::sync::Arc::<str>::from(format!(
                        "{LAZINESS_USER_PREAMBLE}\
                         === BEGIN TRANSCRIPT ===\n\
                         {runtime_state}\
                         {transcript_text}\
                         === END TRANSCRIPT ===\n"
                    )),
                }],
                synthetic_reason: None,
                ..Default::default()
            }),
        ];

        // Telemetry attribution headers — match `run_memory_flush`
        // so backend trace correlation can distinguish "classifier
        // fired in session X" from background traffic.
        let session_id_str = self.session_info.id.to_string();
        let request = ConversationRequest {
            items,
            tools: vec![],
            hosted_tools: vec![],
            tool_choice: None,
            model: Some(model_id.clone()),
            temperature: Some(0.0),
            max_output_tokens: Some(LAZINESS_MAX_OUTPUT_TOKENS),
            // Don't pass `reasoning_effort` — `echo-agent-4.5` (and
            // other tool-flavoured EchoAgent variants) reject the field at
            // the proxy with `400 Bad Request: Model does not support
            // parameter reasoningEffort`. Omitting it lets each model
            // apply its own default. The classifier task is one short
            // JSON object — even on reasoning-capable models the
            // default suffices; no need to force it off.
            reasoning_effort: None,
            x_echo_agent_conv_id: Some(format!("trace-classifier-{}", uuid::Uuid::new_v4())),
            x_echo_agent_req_id: Some(format!("{LAZINESS_REQ_ID_PREFIX}{}", uuid::Uuid::new_v4())),
            x_echo_agent_session_id: Some(session_id_str),
            x_echo_agent_agent_id: Some(echo_agent_telemetry::id::agent_id()),
            ..ConversationRequest::default()
        };

        // Invisibility-critical: build a fresh `SamplingClient` via
        // `prepare_chat_completion` and call `conversation_collect`
        // directly. This is the same side-channel pattern
        // `run_memory_flush`, `run_dream_model_call`, and
        // `image_describe` use. The per-session `sampler_handle`
        // would forward streaming events on the shared sampler
        // channel — every `ChannelToken { Text }` becomes an ACP
        // `AgentMessageChunk` → the pager UI renders mid-classifier
        // reasoning + text deltas. `conversation_collect` does NOT
        // publish on that channel, so the client sees nothing.
        let sampling_client = match self.prepare_chat_completion(false).await {
            Ok(c) => c,
            Err(err) => {
                let detail = err.to_string();
                tracing::debug!(error = %detail, "laziness classifier: prepare_chat_completion failed");
                let elapsed_ms = started.elapsed().as_millis() as u64;
                self.maybe_write_laziness_debug_log(
                    meta.take(),
                    &model_id,
                    items_count_after_trim,
                    elapsed_ms,
                    LazinessFireOutcome::Aborted {
                        reason: LazinessAbortReason::ClassifierError,
                        error_detail: Some(detail),
                    },
                )
                .await;
                self.emit_laziness_abort(LazinessAbortReason::ClassifierError);
                return;
            }
        };

        // Sampler call wrapped in a generation-poll loop AND a
        // wall-clock timeout. Dropping the `conversation_collect`
        // future on abort cancels the in-flight HTTP request via
        // standard `Drop` semantics — no actor-side cleanup needed.
        let timeout = tokio::time::sleep(std::time::Duration::from_millis(
            LAZINESS_CLASSIFIER_TIMEOUT_MS,
        ));
        tokio::pin!(timeout);
        let sampler_future = sampling_client.conversation_collect(request);
        tokio::pin!(sampler_future);
        let response = loop {
            tokio::select! {
                biased;
                out = &mut sampler_future => match out {
                    Ok(response) => break response,
                    Err(err) => {
                        let detail = err.to_string();
                        tracing::debug!(error = %detail, "laziness classifier sampler call failed");
                        let elapsed_ms = started.elapsed().as_millis() as u64;
                        self.maybe_write_laziness_debug_log(
                            meta.take(),
                            &model_id,
                            items_count_after_trim,
                            elapsed_ms,
                            LazinessFireOutcome::Aborted {
                                reason: LazinessAbortReason::ClassifierError,
                                error_detail: Some(detail),
                            },
                        )
                        .await;
                        self.emit_laziness_abort(LazinessAbortReason::ClassifierError);
                        return;
                    }
                },
                _ = &mut timeout => {
                    let elapsed_ms = started.elapsed().as_millis() as u64;
                    self.maybe_write_laziness_debug_log(
                        meta.take(),
                        &model_id,
                        items_count_after_trim,
                        elapsed_ms,
                        LazinessFireOutcome::Aborted {
                            reason: LazinessAbortReason::Timeout,
                            error_detail: None,
                        },
                    )
                    .await;
                    self.emit_laziness_abort(LazinessAbortReason::Timeout);
                    return;
                }
                _ = tokio::time::sleep(poll_interval) => {
                    if let Some(reason) = self.laziness_abort_check(abort_snapshot) {
                        let elapsed_ms = started.elapsed().as_millis() as u64;
                        self.maybe_write_laziness_debug_log(
                            meta.take(),
                            &model_id,
                            items_count_after_trim,
                            elapsed_ms,
                            LazinessFireOutcome::Aborted {
                                reason,
                                error_detail: None,
                            },
                        )
                        .await;
                        self.emit_laziness_abort(reason);
                        return;
                    }
                }
            }
        };

        let elapsed_ms = started.elapsed().as_millis() as u64;
        let raw_text = response.assistant_text();
        let parsed = match parse_classifier_output(&raw_text) {
            Ok(p) => p,
            Err(parse_err) => {
                let parse_error_detail = parse_err.to_string();
                // Truncate raw to 200 chars for offline analysis.
                let snippet: String = raw_text.chars().take(200).collect();
                tracing::debug!(
                    error = %parse_error_detail,
                    raw_truncated = %snippet,
                    "laziness classifier output not parseable",
                );
                self.maybe_write_laziness_debug_log(
                    meta.take(),
                    &model_id,
                    items_count_after_trim,
                    elapsed_ms,
                    LazinessFireOutcome::ParseError {
                        raw_text,
                        parse_error_detail,
                    },
                )
                .await;
                self.emit_laziness_abort(LazinessAbortReason::ClassifierError);
                return;
            }
        };

        let category = parsed.category;
        let confidence = parsed.confidence;
        let decision =
            evaluate_laziness(&parsed, &cfg, nudges_used, LAZINESS_DEFAULT_MIN_CONFIDENCE);

        let goal_active = laziness_injection_active(
            self.goal_harness_enabled(),
            self.goal_tracker.lock().status(),
        );

        let debug_outcome = match &decision {
            LazinessDecision::Nudge { .. } if !goal_active => LazinessFireOutcome::Suppressed {
                reason: LazinessSuppressReason::NotGoalMode,
                parsed: parsed.clone(),
                raw_text: raw_text.clone(),
            },
            _ => LazinessFireOutcome::Verdict { parsed, raw_text },
        };
        self.maybe_write_laziness_debug_log(
            meta.take(),
            &model_id,
            items_count_after_trim,
            elapsed_ms,
            debug_outcome,
        )
        .await;

        let LazinessDecision::Nudge { evidence, .. } = decision else {
            // No nudge — the `reason` discriminator is observable in
            // the telemetry pipeline via the `LazinessClassifierFired`
            // category + the absence of a paired `LazinessNudgeFired`.
            self.events
                .emit(crate::session::events::Event::LazinessClassifierFired {
                    model_id,
                    category: category.as_const_str(),
                    confidence,
                });
            return;
        };
        // Nudge branch: clone once because both `LazinessClassifierFired`
        // here and `LazinessNudgeFired` below need `model_id`.
        self.events
            .emit(crate::session::events::Event::LazinessClassifierFired {
                model_id: model_id.clone(),
                category: category.as_const_str(),
                confidence,
            });

        if !goal_active {
            return;
        }

        let names = self.resolve_goal_tool_names().await;
        let nudge_text = build_laziness_nudge(category, &evidence, Some(&names.todo));
        if nudge_text.is_empty() {
            // Defensive — `evaluate_laziness` only returns Nudge for
            // stalled_* variants which all produce non-empty text.
            return;
        }

        // Final injection step — race-tight ordering:
        // hold the state lock from the idle re-check through the chat
        // history push AND the counter increment. `push_system_reminder`
        // delegates to a non-blocking `chat_state_handle.push_user_message`
        // (channel send + return), so it is safe to call under the
        // `TokioMutex<State>` guard — no inversion, no deadlock.
        //
        // No `pending_inputs.push_back` and no `maybe_start_running_task`
        // wake: the reminder is invisible until the next real user
        // prompt drains it as part of the system-reminder block, so
        // the user never sees a phantom turn.
        let mut state = self.state.lock().await;
        // abort_check FIRST so a user prompt
        // (or model switch) that landed between sampler return
        // and lock acquire is attributed via the typed
        // `LazinessClassifierAborted` event rather than swallowed
        // by the idle re-check's silent return. The two cases
        // overlap (a fresh prompt makes `pending_inputs`
        // non-empty AND bumps `user_input_generation`), so this
        // ordering routes the dashboard-actionable signal to
        // telemetry instead of dropping it.
        //
        // This also guards the case where a model switch can land
        // between the sampler returning and this lock acquire; the
        // main loop's `model_switch_rx.changed()` arm would have
        // zeroed `nudges_used_this_session`, and a nudge emitted
        // here would attribute it to the NEW model's freshly-reset
        // budget while the chat slice we classified belongs to the
        // OLD model. Holding the lock guarantees the abort
        // observation is in the same critical section as the
        // would-be increment.
        if let Some(reason) = self.laziness_abort_check(abort_snapshot) {
            self.emit_laziness_abort(reason);
            return;
        }
        if !debug_mode && !is_session_idle_for_injection(&state) {
            // The idle predicate failed for some reason that is
            // NOT a fresh user prompt or model switch (e.g. a
            // notification drain queued a synthetic input, or the
            // user cancelled and `notifications_suppressed` is
            // true). Silent return is appropriate here — the
            // condition we wanted to nudge no longer holds, but
            // it isn't a Laziness-typed abort either.
            return;
        }
        self.push_system_reminder(&nudge_text);
        state.nudges_used_this_session = state.nudges_used_this_session.saturating_add(1);
        let nudges_remaining = cfg
            .max_nudges_per_session
            .saturating_sub(state.nudges_used_this_session);
        self.events
            .emit(crate::session::events::Event::LazinessNudgeFired {
                model_id,
                category: category.as_const_str(),
                nudges_remaining,
            });
    }

    /// Append a single line to the debug log file when `meta` is
    /// `Some` (i.e. `--laziness-debug-log` was set). No-op in
    /// production. Centralizes the optional-log write so every
    /// return point in `maybe_fire_laziness_check` is a single call.
    /// Takes `meta` by value — each callsite passes `meta.take()`
    /// so the `String` + `Vec<DebugTodoSnapshot>` fields move into
    /// the log line without an extra clone.
    async fn maybe_write_laziness_debug_log(
        &self,
        meta: Option<LazinessFireMeta>,
        model_id: &str,
        items_sent: usize,
        classifier_elapsed_ms: u64,
        outcome: LazinessFireOutcome,
    ) {
        let Some(meta) = meta else { return };
        let Some(handle) = self.laziness_debug_log.as_ref() else {
            return;
        };
        let line =
            build_laziness_debug_line(meta, model_id, items_sent, classifier_elapsed_ms, outcome);
        if let Err(e) = append_laziness_debug_log_line(handle, &line).await {
            tracing::warn!(error = %e, "failed to append laziness debug log line");
        }
    }

    async fn snapshot_todos_for_debug_log(&self) -> Vec<DebugTodoSnapshot> {
        use crate::tools::todo::{TodoState, TodoStatus};
        use echo_agent_tools::types::resources::State;
        let bridge = self.tool_bridge_handle();
        bridge
            .read_resource::<State<TodoState>>()
            .await
            .map(|state| {
                state
                    .0
                    .todo_items_with_ids()
                    .map(|(id, item)| DebugTodoSnapshot {
                        id: id.clone(),
                        status: match item.status {
                            TodoStatus::Pending => "pending",
                            TodoStatus::InProgress => "in_progress",
                            TodoStatus::Completed => "completed",
                            TodoStatus::Cancelled => "cancelled",
                        },
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Count of outstanding background terminal tasks. The full
    /// gate-side count would also include outstanding subagents for
    /// the just-ended prompt, but the debug fire runs after turn end
    /// — the prompt-scoped subagent count isn't available without
    /// plumbing the prompt_id through, which is more wiring than a
    /// prototype warrants. Operators correlating debug log lines with
    /// gate decisions can look at the session's events.jsonl for the
    /// full subagent state.
    async fn snapshot_backing_task_count_for_debug_log(&self) -> usize {
        self.tool_bridge_handle()
            .list_background_tasks()
            .await
            .into_iter()
            .filter(echo_agent_tools::computer::types::TaskSnapshot::is_outstanding)
            .count()
    }
}
