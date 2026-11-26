use super::*;
use echo_agent_lifecycle::ShutdownPolicy;

/// Running-turn display fields for `echo.agent/queue/changed` (clients paint turn-start UI).
pub(super) struct RunningPromptDisplay {
    pub id: String,
    pub text: String,
    pub kind: String,
    pub combined_texts: Option<Vec<String>>,
}

/// Arguments to [`SessionActor::queue_input`]; per-field semantics live on
/// [`SessionCommand::Prompt`].
pub(crate) struct QueueInputRequest {
    pub(crate) prompt_blocks: Vec<acp::ContentBlock>,
    pub(crate) prompt_id: String,
    pub(crate) input_origin: InputOrigin,
    pub(crate) prompt_mode: PromptMode,
    pub(crate) trace_gcs_config: Option<crate::session::repo_changes::TraceExportConfig>,
    pub(crate) artifact_tracker: Option<crate::upload::manifest::ArtifactTracker>,
    pub(crate) client_identifier: Option<String>,
    pub(crate) screen_mode: Option<String>,
    pub(crate) verbatim: bool,
    pub(crate) json_schema: Option<serde_json::Value>,
    pub(crate) send_now: bool,
    pub(crate) task_wake_fallback: Option<TaskWakeFallback>,
    pub(crate) tool_overrides_update: Option<echo_agent_sampling_types::ToolOverridesUpdate>,
    pub(crate) respond_to: oneshot::Sender<PromptTurnResult>,
    pub(crate) persist_ack: Option<oneshot::Sender<()>>,
    pub(crate) parsed_prompt_tx: Option<oneshot::Sender<ParsedPromptInfo>>,
}

impl QueueInputRequest {
    pub(crate) fn from_legacy_prompt_id(
        prompt_blocks: Vec<acp::ContentBlock>,
        prompt_id: String,
        prompt_mode: PromptMode,
        respond_to: oneshot::Sender<PromptTurnResult>,
    ) -> Self {
        let input_origin = InputOrigin::from_prompt_id(&prompt_id);
        Self::from_input_origin(
            prompt_blocks,
            prompt_id,
            input_origin,
            prompt_mode,
            respond_to,
        )
    }

    pub(crate) fn from_input_origin(
        prompt_blocks: Vec<acp::ContentBlock>,
        prompt_id: String,
        input_origin: InputOrigin,
        prompt_mode: PromptMode,
        respond_to: oneshot::Sender<PromptTurnResult>,
    ) -> Self {
        Self {
            prompt_blocks,
            prompt_id,
            input_origin,
            prompt_mode,
            trace_gcs_config: None,
            artifact_tracker: None,
            client_identifier: None,
            screen_mode: None,
            verbatim: false,
            json_schema: None,
            send_now: false,
            task_wake_fallback: None,
            tool_overrides_update: None,
            respond_to,
            persist_ack: None,
            parsed_prompt_tx: None,
        }
    }
}

impl SessionActor {
    /// Queue a user-originated prompt (writes to prompt history).
    ///
    /// `send_now` (or a user prompt arriving during an interruptible wait)
    /// inserts the prompt to run next. Returns `true` when the caller must
    /// cancel the running turn.
    #[must_use = "true means the caller must cancel the running turn"]
    pub(super) async fn queue_input(&self, request: QueueInputRequest) -> bool {
        let QueueInputRequest {
            prompt_blocks,
            prompt_id,
            input_origin,
            prompt_mode,
            trace_gcs_config,
            artifact_tracker,
            client_identifier,
            screen_mode,
            verbatim,
            json_schema,
            send_now,
            task_wake_fallback,
            tool_overrides_update,
            respond_to,
            persist_ack,
            parsed_prompt_tx,
        } = request;
        tracing::info!("queueing prompt: {prompt_id}");
        let queue_depth = { self.state.lock().await.pending_inputs.len() };
        echo_agent_telemetry::unified_log::info(
            "shell.prompt.queued",
            Some(self.session_info.id.0.as_ref()),
            Some(serde_json::json!({
                "prompt_id": prompt_id,
                "queue_depth": queue_depth,
            })),
        );

        // Log prompt to per-CWD fast history file immediately when queued
        // (not in handle_prompt, because prompt might be cancelled before processing)
        // Extract raw text from prompt_blocks (without <user_query> tags)
        let raw_prompt_text: String = prompt_blocks
            .iter()
            .filter_map(|block| {
                if let acp::ContentBlock::Text(t) = block {
                    Some(t.text.trim())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n\n");

        let policy = input_origin.policy();

        // Bump before any await so a LocalSet recap cannot commit/emit after
        // this Prompt was accepted but before handle_prompt runs.
        if policy.authority.is_human_intent() {
            self.invalidate_side_calls_for_new_prompt();
        }

        // Prompt history is human-authored history, not a generic conversational log.
        if policy.analytics.is_human_prompt()
            && !raw_prompt_text.is_empty()
            && !self.startup_hints.is_subagent
        {
            let cwd = self.session_info.cwd.clone();
            let session_id = self.session_info.id.to_string();
            let is_bash = Self::extract_bash_command(&prompt_blocks).is_some();
            // Await inline so the append is durable before quit drops the agent runtime's detached tasks.
            let entry = crate::session::prompt_history::PromptEntry {
                timestamp: chrono::Utc::now(),
                session_id,
                prompt: raw_prompt_text,
                is_bash,
            };
            crate::session::prompt_history::append_prompt_async(cwd, entry).await;
        }

        // Capture trace config template from the first real user prompt so
        // synthetic auto-wake turns can reuse the same GCS bucket/method.
        let (trace_gcs_config, artifact_tracker) = if let Some(ref cfg) = trace_gcs_config {
            *self.trace_config_template.borrow_mut() = Some(TraceConfigTemplate {
                bucket_url: cfg.bucket_url.clone(),
                upload_method: cfg.upload_method.clone(),
            });
            (trace_gcs_config, artifact_tracker)
        } else {
            (trace_gcs_config, artifact_tracker)
        };

        if let crate::session::PromptOrigin::SubagentCompleted { subagent_id } =
            input_origin.as_prompt_origin()
        {
            self.mark_completions_reported(&[subagent_id]).await;
        }

        // For synthetic prompts, derive trace config from the template
        // captured during the first real user prompt.
        let (trace_gcs_config, artifact_tracker) =
            if input_origin.is_synthetic() && trace_gcs_config.is_none() {
                if let Some(template) = self.trace_config_template.borrow().clone() {
                    let cfg = crate::session::repo_changes::TraceExportConfig {
                        bucket_url: template.bucket_url,
                        service_account_key: None,
                        prefix_dir: None,
                        gcs_prefix: Some(format!(
                            "{}/turn_{}",
                            self.session_info.id.0,
                            self.chat_state_handle.get_prompt_index().await,
                        )),
                        absolute_paths: false,
                        archive_name_override: None,
                        upload_method: template.upload_method,
                    };
                    (
                        Some(cfg),
                        Some(crate::upload::manifest::new_artifact_tracker()),
                    )
                } else {
                    (None, None)
                }
            } else {
                (trace_gcs_config, artifact_tracker)
            };

        let mut state = self.state.lock().await;

        // User prompts have priority over queued synthetic auto-wake prompts;
        // the guarded sweep exempts the running turn's own slot (see
        // `State::sweep_pending_inputs`). Gate deliberately keyed on
        // completion-id-bearing synthetics only (pre-existing shape): a queue
        // holding only drain/goal-summary synthetics is never preempted.
        if policy.authority.is_human_intent() {
            let preempt_armed = state.pending_inputs.iter().any(|i| {
                i.input_origin.completion_id().is_some()
                    && state.running_prompt_id() != Some(i.prompt_id.as_str())
            });
            if preempt_armed {
                let dropped =
                    state.sweep_pending_inputs(|i| i.input_origin.is_preemptible_runtime_wake());
                if let Some(reservations) = &self.tool_context.task_completion_reservations {
                    for task_id in dropped
                        .iter()
                        .filter_map(|item| item.input_origin.completion_id())
                    {
                        reservations.release(task_id);
                    }
                }
                tracing::info!(
                    dropped_count = dropped.len(),
                    "auto-wake: dropping pending synthetic prompts (user prompt has priority)"
                );
            }
        }

        let queue_mutation_policy = QueueMutationPolicy::from_input_origin(&input_origin);
        let queue_meta = if !queue_mutation_policy.is_visible() {
            None
        } else {
            // Derive the wire `kind` from the prompt content so the shared
            // queue / `running_prompt_id` adoption picks the right display
            // shim. Bash commands carry a bash
            // `PromptBlockMeta`; everything user-submitted here is otherwise a
            // plain prompt. (Cron prompts are server-injected via their own
            // path and render client-side.)
            let kind = if Self::extract_bash_command(&prompt_blocks).is_some() {
                "bash"
            } else {
                "prompt"
            };
            Some(crate::session::prompt_queue::QueueEntryMeta {
                id: prompt_id.clone(),
                version: 0,
                owner: client_identifier.clone(),
                last_editor: None,
                kind: kind.to_string(),
                text: Self::queue_text_from_blocks(&prompt_blocks),
                combined_texts: None,
            })
        };
        let log_prompt_id = prompt_id.clone();
        let log_kind = queue_meta
            .as_ref()
            .map(|m| m.kind.clone())
            .unwrap_or_else(|| "synthetic".to_string());
        let log_owner = client_identifier.clone().unwrap_or_default();
        let mut item = InputItem {
            prompt_id,
            prompt_blocks,
            prompt_mode,
            trace_gcs_config,
            artifact_tracker,
            client_identifier,
            screen_mode,
            verbatim,
            json_schema,
            input_origin,
            task_wake_fallback,
            tool_overrides_update,
            respond_to,
            persist_ack,
            parsed_prompt_tx,
            queue_meta,
            queue_mutation_policy,
            send_now: false,
        };

        // Use `running_prompt_id()` not `current_prompt_id` (cleared while front
        // unpopped). A blocking wait never grants permission to replace a turn.
        let running_front_id = state.running_prompt_id().map(str::to_string);
        let turn_running = running_front_id.is_some();
        let goal_active = self.goal_tracker.lock().status()
            == Some(crate::session::goal_tracker::GoalStatus::Active);
        let blocked_in_wait = self.tool_context.blocking_wait_depth.depth() > 0;
        // Drain-policy rows are held work: visible user/protected rows and
        // queue-hidden human fallbacks (interjection fallback). Runtime wakes
        // (CancelWithProducer / DropEphemeral) do not block auto-send-now.
        let held_user_queue = state.pending_inputs.iter().any(|queued| {
            queued.input_origin.policy().shutdown == ShutdownPolicy::Drain
                && Some(queued.prompt_id.as_str()) != running_front_id.as_deref()
        });
        // Keep goal steering responsive without changing an ordinary queued
        // message into a destructive send-now on non-goal turns.
        let auto_goal_steering = goal_active && turn_running && blocked_in_wait && !held_user_queue;
        let send_now = item.is_queue_editable() && (send_now || auto_goal_steering);
        let front_awaiting_commit_now = Self::front_awaiting_commit(&state);
        let cancel_running_turn =
            send_now && Self::send_now_cancels_running_turn(&state, goal_active);
        let merge_into_goal = send_now
            && turn_running
            && goal_active
            && Self::extract_bash_command(&item.prompt_blocks).is_none();
        if merge_into_goal {
            self.enqueue_prompt_as_planner_steering(&item);
            self.enqueue_prompt_as_interjection(
                item,
                crate::session::events::InterjectionSource::Direct,
            );
        } else if send_now {
            item.send_now = true;
            let insert_at = Self::send_now_insert_index(&state, running_front_id.as_deref());
            state.pending_inputs.insert(insert_at, item);
        } else {
            state.pending_inputs.push_back(item);
        }
        // qtrace: server appended a prompt to the authoritative FIFO. The index
        // it lands at vs whether a turn is already running tells us if it will
        // run next or queue behind others (the leader-mode source of truth
        // that clients must mirror).
        tracing::debug!(
            target: "qtrace",
            pid = std::process::id(),
            event = "server_queue_input",
            prompt_id = %log_prompt_id,
            kind = %log_kind,
            owner = %log_owner,
            send_now,
            cancel_running_turn,
            new_depth = state.pending_inputs.len(),
            running_task_present = state.running_task.is_some(),
            session = self.session_info.id.0.as_ref(),
            "server appended prompt to pending_inputs",
        );
        if send_now && turn_running {
            echo_agent_telemetry::unified_log::info(
                "shell.prompt.send_now_decision",
                Some(self.session_info.id.0.as_ref()),
                Some(serde_json::json!({
                    "prompt_id": log_prompt_id,
                    "cancels_turn": cancel_running_turn,
                    "blocked_in_wait": blocked_in_wait,
                    "goal_active": goal_active,
                    "merged_as_interjection": merge_into_goal,
                    "front_awaiting_commit": front_awaiting_commit_now,
                })),
            );
        }
        // Broadcast the new authoritative queue to all subscribers
        // (fire-and-forget, never persisted).
        self.broadcast_queue_changed(&state);
        cancel_running_turn
    }

    /// Extract a plain-text summary of a prompt's content blocks for the
    /// shared queue display.
    ///
    /// Prefers a block's `displayText` meta (the compact user-facing form, e.g.
    /// `/loop 5s echo "x"`) over the raw wire text. A client that expands a
    /// slash skill locally sends the full expanded instruction as the wire text
    /// with the compact invocation stamped in `displayText`; the shared queue —
    /// and the turn-start shim that renders other clients' user block from this
    /// text — must show the compact form, not the raw expansion. Falls back to
    /// the joined block text when no `displayText` is present.
    pub(super) fn queue_text_from_blocks(blocks: &[acp::ContentBlock]) -> String {
        if let Some(display) = blocks.iter().find_map(|block| {
            let acp::ContentBlock::Text(t) = block else {
                return None;
            };
            t.meta
                .as_ref()
                .and_then(|m| m.get("displayText"))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        }) {
            return display;
        }
        blocks
            .iter()
            .filter_map(|block| {
                if let acp::ContentBlock::Text(t) = block {
                    Some(t.text.trim())
                } else {
                    None
                }
            })
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Project the user-visible (queued, not-yet-running) prompts into the
    /// wire shape, in queue order. The currently-running prompt is excluded
    /// (it is shown via the normal turn stream, not the queue).
    pub(super) fn build_queue_wire(
        &self,
        state: &State,
    ) -> Vec<crate::session::prompt_queue::QueueEntryWire> {
        // Race-free running identity (`running_task` lives under the same lock
        // as `pending_inputs`); the `current_prompt_id` pin is cleared early by
        // `handle_completion`, which would briefly re-list the finished-but-
        // unpopped front as a queued row.
        let running_id = state.running_prompt_id();
        let mut out = Vec::new();
        for item in &state.pending_inputs {
            // Hidden rows never carry wire meta; gate on the policy so the
            // visibility helper stays live in production (not test-only).
            if !item.is_queue_visible() {
                continue;
            }
            let Some(meta) = &item.queue_meta else {
                continue;
            };
            if running_id == Some(meta.id.as_str()) {
                // This item is the in-flight turn, not a queued prompt.
                continue;
            }
            out.push(crate::session::prompt_queue::QueueEntryWire {
                id: meta.id.clone(),
                version: meta.version,
                owner: meta.owner.clone(),
                last_editor: meta.last_editor.clone(),
                kind: meta.kind.clone(),
                text: meta.text.clone(),
                combined_texts: meta.combined_texts.clone(),
                position: out.len(),
            });
        }
        out
    }

    /// Broadcast the current authoritative prompt queue to all subscribers
    /// Fire-and-forget via the gateway, carrying `sessionId`
    /// so session routing fans it to every attached client. Never persisted.
    pub(super) fn broadcast_queue_changed(&self, state: &State) {
        let running = state.running_prompt_id().and_then(|pid| {
            state
                .pending_inputs
                .iter()
                .find(|i| i.prompt_id == pid)
                .map(Self::running_display_from_item)
        });
        self.broadcast_queue_changed_inner(state, running);
    }

    /// Broadcast with explicit running-turn display (promote before `running_task`
    /// so clients paint before the user-echo races in).
    pub(super) fn broadcast_queue_changed_promoting(
        &self,
        state: &State,
        running: RunningPromptDisplay,
    ) {
        self.broadcast_queue_changed_inner(state, Some(running));
    }

    pub(super) fn running_display_from_item(item: &InputItem) -> RunningPromptDisplay {
        let meta = item.queue_meta.as_ref();
        RunningPromptDisplay {
            id: item.prompt_id.clone(),
            text: meta
                .map(|m| m.text.clone())
                .unwrap_or_else(|| Self::queue_text_from_blocks(&item.prompt_blocks)),
            kind: meta
                .map(|m| m.kind.clone())
                .unwrap_or_else(|| "prompt".to_string()),
            combined_texts: meta
                .and_then(|m| m.combined_texts.clone())
                .filter(|v| v.len() >= 2),
        }
    }

    fn broadcast_queue_changed_inner(&self, state: &State, running: Option<RunningPromptDisplay>) {
        let running_id = running.as_ref().map(|r| r.id.clone());
        // Exclude the running/promoting row from `entries` (same as when
        // `running_task` is set).
        let mut entries = self.build_queue_wire(state);
        if let Some(rid) = running_id.as_deref() {
            entries.retain(|e| e.id != rid);
            for (i, e) in entries.iter_mut().enumerate() {
                e.position = i;
            }
        }
        let (running_text, running_kind, running_combined_texts) = match running {
            Some(r) => (Some(r.text), Some(r.kind), r.combined_texts),
            None => (None, None, None),
        };
        let payload = crate::session::prompt_queue::QueueChanged {
            session_id: self.session_info.id.0.to_string(),
            entries,
            running_prompt_id: running_id,
            running_text,
            running_kind,
            running_combined_texts,
        };
        tracing::debug!(
            target: "qtrace",
            pid = std::process::id(),
            event = "server_broadcast_queue",
            running_prompt_id = payload.running_prompt_id.as_deref().unwrap_or(""),
            combined_segs = payload
                .running_combined_texts
                .as_ref()
                .map(|v| v.len())
                .unwrap_or(0),
            entry_count = payload.entries.len(),
            entries = ?payload.entries.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            session = self.session_info.id.0.as_ref(),
            "broadcasting echo.agent/queue/changed to subscribers",
        );
        if let Ok(params) = serde_json::value::to_raw_value(&payload) {
            self.notifications
                .gateway
                .forward_fire_and_forget(acp::ExtNotification::new(
                    crate::session::prompt_queue::QUEUE_CHANGED_METHOD,
                    params.into(),
                ));
        }
    }

    /// Whether `prompt_id` is the currently in-flight turn (and so must never
    /// be removed/reordered by a queue edit). Keyed on `state.running_task`
    /// (race-free under the caller's state lock), not the `current_prompt_id`
    /// pin, which `handle_completion` clears while the finished front is still
    /// unpopped — a queue edit in that window must still refuse the front.
    fn is_running_prompt(state: &State, prompt_id: &str) -> bool {
        state.running_prompt_id() == Some(prompt_id)
    }

    /// The running front's user message is not committed yet; a send-now
    /// cancel would invisibly destroy it before the model sees it (Esc/Ctrl+C
    /// still cancels such turns).
    fn front_awaiting_commit(state: &State) -> bool {
        !state.front_message_committed
            && state
                .pending_inputs
                .front()
                .is_some_and(|front| state.running_prompt_id() == Some(front.prompt_id.as_str()))
    }

    /// The send-now guard's commit point: cleared at promote, set at each
    /// intake path's commit. The guard spares the cancel until it is set; a
    /// missed intake path fails soft (its turns are spared, never cancelled).
    pub(super) async fn mark_front_message_committed(&self) {
        self.state.lock().await.front_message_committed = true;
    }

    /// Insertion point for a send-now prompt: behind the running front (which
    /// `handle_completion` pops) and behind earlier send-now prompts (FIFO).
    fn send_now_insert_index(state: &State, running_front_id: Option<&str>) -> usize {
        let mut insert_at = usize::from(matches!(
            (state.pending_inputs.front(), running_front_id),
            (Some(front_item), Some(running)) if front_item.prompt_id == running
        ));
        while state
            .pending_inputs
            .get(insert_at)
            .is_some_and(|queued| queued.send_now)
        {
            insert_at += 1;
        }
        insert_at
    }

    fn send_now_cancels_running_turn(state: &State, goal_active: bool) -> bool {
        state.running_prompt_id().is_some() && !goal_active && !Self::front_awaiting_commit(state)
    }

    /// True when the next drainable user row (FIFO, non-synthetic, not the running front) is free
    /// of a live edit hold. The goal round loop yields on this so queued user work runs between
    /// rounds instead of starving behind continuations. A row under an unexpired hold must not
    /// yield: promote is blocked while editing, so a yield would only re-arm the goal behind a
    /// parked queue. Synthetics ahead of the user row do not block the yield. A hold older than
    /// `EDIT_HOLD_TTL` counts as expired here: the leaked-hold GC runs only in
    /// `maybe_start_running_task`, which cannot fire while the in-turn goal loop keeps looping, so
    /// without this a crashed or disconnected editor's stale hold would park the queue for the
    /// whole goal.
    pub(super) async fn has_runnable_queued_user_row(&self) -> bool {
        let state = self.state.lock().await;
        let running = state.running_prompt_id();
        state
            .pending_inputs
            .iter()
            .filter(|item| running != Some(item.prompt_id.as_str()))
            .find(|item| !item.input_origin.is_synthetic())
            .is_some_and(|next| match state.edit_holds.get(next.prompt_id.as_str()) {
                Some(since) => since.elapsed() >= super::EDIT_HOLD_TTL,
                None => true,
            })
    }

    /// True when a goal continuation (`GoalSummary` / `GoalClassifierNudge`) is
    /// already queued to resume the goal. A user turn that runs while one is
    /// pending must not also drive the in-turn goal loop: the queued
    /// continuation is the single resume point, so driving the goal from the
    /// user turn as well would run the goal twice.
    pub(super) async fn has_pending_goal_continuation(&self) -> bool {
        let state = self.state.lock().await;
        state.pending_inputs.iter().any(|item| {
            matches!(
                item.input_origin.as_prompt_origin(),
                crate::session::PromptOrigin::GoalSummary
                    | crate::session::PromptOrigin::GoalClassifierNudge
            )
        })
    }

    fn enqueue_prompt_as_planner_steering(&self, item: &InputItem) {
        let steering = item
            .prompt_blocks
            .iter()
            .filter_map(|block| match block {
                acp::ContentBlock::Text(text) => Some(text.text.trim()),
                _ => None,
            })
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        self.goal_tracker.lock().steer_planner(steering);
    }

    fn enqueue_prompt_as_interjection(
        &self,
        item: InputItem,
        source: crate::session::events::InterjectionSource,
    ) {
        let InputItem {
            prompt_id,
            prompt_blocks,
            respond_to,
            ..
        } = item;
        let mut text_parts = Vec::new();
        let mut attachments = Vec::new();
        for block in prompt_blocks {
            match block {
                acp::ContentBlock::Text(text) => {
                    let text = text.text.trim();
                    if !text.is_empty() {
                        text_parts.push(text.to_string());
                    }
                }
                acp::ContentBlock::Image(image) => attachments.push(image),
                _ => {}
            }
        }
        let text = text_parts.join("\n\n");
        let image_count = attachments.len() as u32;
        self.pending_interjections.push(PendingInterjection {
            scope: None,
            text: text.clone(),
            attachments,
        });
        self.broadcast_interjection(&text, Some(&prompt_id));
        self.events
            .emit(crate::session::events::Event::Interjected {
                source,
                image_count,
                redirect_kind: crate::session::events::RedirectKind::Interjection,
            });
        Self::respond_removed_prompt(respond_to);
        tracing::info!(
            ?source,
            prompt_id = %prompt_id,
            "queued prompt promoted as a mid-turn interjection"
        );
    }

    /// Move held user prompts into `pending_interjections` so the next drain
    /// injects them. Stops (does not skip over) at: non-editable/protected
    /// rows (pinned queue mutation policy); bash (FIFO); send-now
    /// (cancel-and-run-next must not become continue-with-interject); edit
    /// hold (would inject stale pre-edit text); a different `queue_meta.owner`
    /// than the running prompt (leader mode: another client's next-turn row
    /// stays queued); or a row with `tool_overrides_update` (interjection
    /// discards the override payload that a normal turn drain would apply).
    /// No-op when idle or the held queue is empty.
    pub(super) async fn promote_queued_as_interjections(&self) {
        let mut state = self.state.lock().await;
        let running_front_id = state.running_prompt_id().map(str::to_string);
        let Some(running_id) = running_front_id.as_deref() else {
            return;
        };
        let running_owner = state
            .pending_inputs
            .iter()
            .find(|item| item.prompt_id == running_id)
            .and_then(|item| item.queue_meta.as_ref())
            .and_then(|meta| meta.owner.as_deref())
            .map(str::to_string);
        let mut promoted = Vec::new();
        loop {
            // Hidden runtime wakes may be skipped, but a visible protected row
            // stops the prefix rather than being skipped over.
            let Some(pos) = state.pending_inputs.iter().position(|item| {
                Some(item.prompt_id.as_str()) != running_front_id.as_deref()
                    && (item.is_queue_visible() || !item.input_origin.is_synthetic())
            }) else {
                break;
            };
            let item = &state.pending_inputs[pos];
            let item_owner = item
                .queue_meta
                .as_ref()
                .and_then(|meta| meta.owner.as_deref());
            if !item.is_queue_editable()
                || Self::extract_bash_command(&item.prompt_blocks).is_some()
                || item.send_now
                || state.edit_holds.contains_key(&item.prompt_id)
                || item_owner != running_owner.as_deref()
                || item.tool_overrides_update.is_some()
            {
                break;
            }
            if let Some(item) = state.pending_inputs.remove(pos) {
                promoted.push(item);
            }
        }
        if promoted.is_empty() {
            return;
        }
        let goal_active = self.goal_tracker.lock().status()
            == Some(crate::session::goal_tracker::GoalStatus::Active);
        for item in promoted {
            if goal_active {
                self.enqueue_prompt_as_planner_steering(&item);
            }
            self.enqueue_prompt_as_interjection(
                item,
                crate::session::events::InterjectionSource::Queue,
            );
        }
        self.broadcast_queue_changed(&state);
    }

    /// Resolve a removed prompt's pending RPC with `Ok(RemovedFromQueue)` before dropping it. A
    /// dropped sender would look like the running turn failing; the `Ok` lets the client discard it.
    /// It never ran, so token count is `0` and there is no `tool_overrides` echo.
    pub(super) fn respond_removed_prompt(respond_to: oneshot::Sender<PromptTurnResult>) {
        let _ = respond_to.send(Ok(PromptTurnOk {
            stop_reason: acp::StopReason::Cancelled,
            total_tokens: 0,
            turn_snapshot: None,
            completion_kind: PromptCompletionKind::RemovedFromQueue,
            structured_output: None,
            usage: None,
            tool_overrides: None,
        }));
    }

    pub(super) async fn handle_remove_queued_prompt(
        &self,
        id: &str,
        expected_version: u64,
        owner: Option<&str>,
    ) {
        let mut state = self.state.lock().await;
        let mut removed = false;
        if !Self::is_running_prompt(&state, id)
            && let Some(pos) = state
                .pending_inputs
                .iter()
                .position(|item| item.editable_queue_meta_matches(id, expected_version, owner))
        {
            if let Some(item) = state.pending_inputs.remove(pos) {
                Self::respond_removed_prompt(item.respond_to);
            }
            removed = true;
        }
        // Missing/stale editable controls still clear leaked holds; a protected row is a no-op.
        if !Self::has_protected_row(&state, id) {
            state.edit_holds.remove(id);
        }
        if !removed {
            tracing::debug!(
                queued_id = %id,
                expected_version,
                "queue remove was a no-op (drained / stale / not owner); rebroadcasting"
            );
        }
        // Always re-broadcast the authoritative queue so the client reconciles.
        self.broadcast_queue_changed(&state);
    }

    /// Atomically interject a queued (not-yet-running) prompt into the running
    /// turn. In a single state-lock hold the actor removes the
    /// prompt from `pending_inputs` and pushes its text into
    /// `pending_interjections`, so the in-flight turn merges it at the next safe
    /// point (`drain_pending_interjections`) and the prompt can never both
    /// interject AND later run as its own turn — the race the client-side
    /// "interject + queue/remove" pair could not avoid.
    ///
    /// Mirrors [`handle_remove_queued_prompt`]'s versioned/owner gate and
    /// [`SessionCommand::Interject`]'s broadcast-then-buffer. An uncommitted
    /// front is never cancelled; the promoted row still runs next.
    ///
    /// During an active goal, plain prompts become steering while bash stays queued.
    /// Missing, stale, running, or foreign rows are benign no-ops.
    ///
    /// Always re-broadcasts `echo.agent/queue/changed` so every client reconciles
    /// (the row vanishes on success, is unchanged on a no-op).
    /// `new_text` (when `Some`) replaces the stored queue text in the
    /// interjection — the client edited the row before interjecting. It rides
    /// the same version check, so a stale version no-ops the edit too.
    /// Exception: when the interject no-ops but the row is still queued, a
    /// version-matching `new_text` is saved to the row as an LWW edit so the
    /// edit isn't silently lost when the row later drains as its own turn.
    #[must_use = "true means the caller must cancel the running turn"]
    pub(super) async fn handle_interject_queued_prompt(
        &self,
        id: &str,
        expected_version: u64,
        owner: Option<&str>,
        new_text: Option<&str>,
    ) -> bool {
        let mut state = self.state.lock().await;
        let running_front_id = state.running_prompt_id().map(str::to_string);
        let turn_running = running_front_id.is_some();
        let goal_active = self.goal_tracker.lock().status()
            == Some(crate::session::goal_tracker::GoalStatus::Active);
        // Sampled early; the insert below never displaces the front.
        let cancel_decision = Self::send_now_cancels_running_turn(&state, goal_active);
        let front_awaiting_commit_now = Self::front_awaiting_commit(&state);
        let row_matches =
            |item: &InputItem| item.editable_queue_meta_matches(id, expected_version, owner);
        let running_is_row = running_front_id.as_deref() == Some(id);
        let pos = if running_is_row {
            None
        } else {
            state.pending_inputs.iter().position(row_matches)
        };
        let mut cancel_running_turn = false;
        if let Some(pos) = pos
            && let Some(mut item) = state.pending_inputs.remove(pos)
        {
            // Client-edited text wins (LWW).
            if let Some(new_text) = new_text.filter(|t| !t.trim().is_empty()) {
                Self::apply_queued_prompt_edit(&mut item, new_text.to_string(), owner);
            }
            let merge_into_goal = turn_running
                && goal_active
                && Self::extract_bash_command(&item.prompt_blocks).is_none();
            if merge_into_goal {
                self.enqueue_prompt_as_planner_steering(&item);
                self.enqueue_prompt_as_interjection(
                    item,
                    crate::session::events::InterjectionSource::Queue,
                );
                tracing::info!(
                    queued_id = %id,
                    "send-now: queued row will steer the active goal turn"
                );
            } else {
                item.send_now = true;
                let insert_at = Self::send_now_insert_index(&state, running_front_id.as_deref());
                state.pending_inputs.insert(insert_at, item);
                cancel_running_turn = cancel_decision;
                tracing::info!(queued_id = %id, cancel_running_turn, "send-now: promoted queued prompt to run next");
            }
            echo_agent_telemetry::unified_log::info(
                "shell.prompt.send_now_decision",
                Some(self.session_info.id.0.as_ref()),
                Some(serde_json::json!({
                    "prompt_id": id,
                    "from_queue_row": true,
                    "cancels_turn": cancel_running_turn,
                    "goal_active": goal_active,
                    "merged_as_interjection": merge_into_goal,
                    "front_awaiting_commit": front_awaiting_commit_now,
                })),
            );
        } else if let Some(new_text) = new_text
            && !new_text.trim().is_empty()
            && !running_is_row
            && let Some(item) = state
                .pending_inputs
                .iter_mut()
                .find(|item| row_matches(item))
        {
            // The send-now no-opped but the row is still queued: keep the
            // edit as an LWW write so it isn't silently lost. Stale versions
            // get no fallback (LWW); the running turn is never edited.
            Self::apply_queued_prompt_edit(item, new_text.to_string(), owner);
            tracing::info!(
                queued_id = %id,
                "send-now no-opped; saved the edit to the queued row"
            );
        } else {
            tracing::debug!(
                queued_id = %id,
                expected_version,
                turn_running,
                "queue send-now no-op (running id / stale / drained / not owner); rebroadcasting"
            );
        }
        if !Self::has_protected_row(&state, id) {
            state.edit_holds.remove(id);
        }
        // Always re-broadcast the authoritative queue so the client reconciles.
        self.broadcast_queue_changed(&state);
        cancel_running_turn
    }

    /// Reorder queued prompts to match `ordered_ids`. The
    /// running turn (front, if active) stays pinned at the front; queued items
    /// not named in `ordered_ids` keep their relative order behind the named
    /// ones. Idempotent; re-broadcasts the result.
    pub(super) async fn handle_reorder_queue(&self, ordered_ids: &[String]) {
        let mut state = self.state.lock().await;

        // Protected/hidden/running rows pin their absolute slots. Reorder only
        // editable rows across the remaining slots.
        let running_id = state.running_prompt_id().map(str::to_string);
        let mut queued = Vec::new();
        let slots: Vec<Option<InputItem>> = std::mem::take(&mut state.pending_inputs)
            .into_iter()
            .map(|item| {
                let is_queueable = item.is_queue_editable()
                    && item
                        .queue_meta
                        .as_ref()
                        .is_some_and(|m| running_id.as_deref() != Some(m.id.as_str()));
                if is_queueable {
                    queued.push(item);
                    None
                } else {
                    Some(item)
                }
            })
            .collect();
        queued.sort_by_key(|item| {
            item.queue_meta
                .as_ref()
                .and_then(|m| ordered_ids.iter().position(|x| x == &m.id))
                .unwrap_or(usize::MAX)
        });
        let mut queued = queued.into_iter();
        state.pending_inputs = slots
            .into_iter()
            .filter_map(|slot| slot.or_else(|| queued.next()))
            .collect();

        self.broadcast_queue_changed(&state);
    }

    /// Clear queued prompts. When `owner` is `Some`, only that
    /// client's queued items are removed. The running turn is never touched.
    pub(super) async fn handle_clear_queue(&self, owner: Option<&str>) {
        let mut state = self.state.lock().await;
        // Partition rather than `retain`: each cleared user prompt still has a
        // client awaiting its `respond_to`, so it must be resolved with
        // `Cancelled` (see [`respond_removed_prompt`]) instead of being
        // dropped — a bare drop surfaces as "session failed to respond" and a
        // spurious "Turn failed" on the running turn.
        let running_id = state.running_prompt_id().map(str::to_string);
        let mut kept = VecDeque::with_capacity(state.pending_inputs.len());
        for item in std::mem::take(&mut state.pending_inputs) {
            let keep = !item.is_queue_editable()
                || match &item.queue_meta {
                    // Non-queue (synthetic) items always stay.
                    None => true,
                    // Never drop the in-flight turn; keep items NOT owned by the
                    // requester (owner-scoped clear).
                    Some(meta) => {
                        running_id.as_deref() == Some(meta.id.as_str())
                            || owner.is_some_and(|o| meta.owner.as_deref() != Some(o))
                    }
                };
            if keep {
                kept.push_back(item);
            } else {
                Self::respond_removed_prompt(item.respond_to);
            }
        }
        state.pending_inputs = kept;
        self.broadcast_queue_changed(&state);
    }

    /// Replace the text of a queued (not-yet-running) prompt in place
    /// (LWW).
    ///
    /// Semantics — last write wins via the actor's serialized mailbox.
    /// Concretely, for an entry whose `queue_meta.id == id`:
    /// 1. Rebuild the underlying `prompt_blocks` as a single
    ///    [`acp::TextContent`] block carrying `new_text` (any non-text blocks
    ///    such as pasted images on the original prompt are not preserved — the
    ///    user has explicitly typed replacement text).
    /// 2. Update `queue_meta.text`, bump `queue_meta.version`, and record
    ///    `last_editor` (the original `owner` attribution is preserved).
    /// 3. Re-broadcast `echo.agent/queue/changed` so every subscriber renders the
    ///    new text and version.
    ///
    /// **No-op cases** (edit discarded; the id's hold is still cleared so promote is not parked,
    /// since promote or remove already broadcast the queue change):
    /// - The id is not in `pending_inputs` (already drained / removed).
    /// - The id names the currently-running turn — editing the live turn is
    ///   out of scope.
    /// - `new_text` is blank (a queued prompt is never blanked).
    ///
    /// Every path clears the id's hold under the queue lock so a stale edit
    /// request cannot leave promote parked.
    pub(super) async fn handle_edit_queued_prompt(
        &self,
        id: &str,
        new_text: String,
        editor: Option<&str>,
    ) {
        let mut state = self.state.lock().await;
        let mut should_broadcast = false;
        if new_text.trim().is_empty() {
            tracing::debug!(queued_id = %id, "queue edit no-op: empty newText");
        } else if Self::is_running_prompt(&state, id) {
            // Locked first: the promoter arms `running_task` under this lock.
            tracing::debug!(
                queued_id = %id,
                "queue edit no-op: id names the running turn"
            );
        } else if let Some(pos) = state
            .pending_inputs
            .iter()
            .position(|item| item.is_queue_editable() && item.has_queue_id(id))
        {
            if let Some(item) = state.pending_inputs.get_mut(pos) {
                Self::apply_queued_prompt_edit(item, new_text, editor);
            }
            should_broadcast = true;
        } else {
            tracing::debug!(
                queued_id = %id,
                "queue edit no-op: id not found (already drained / removed)"
            );
        }
        if !Self::has_protected_row(&state, id) {
            state.edit_holds.remove(id);
        }
        if should_broadcast {
            self.broadcast_queue_changed(&state);
        }
    }

    /// Stamp (or re-stamp) a queue-edit hold. `insert` refreshes the TTL so
    /// re-entering edit after a dropped release does not inherit an aged bound.
    pub(crate) async fn handle_hold_edit(&self, id: String) {
        let mut state = self.state.lock().await;
        if Self::has_editable_row(&state, &id) {
            state.edit_holds.insert(id, std::time::Instant::now());
        }
    }

    pub(crate) async fn handle_release_edit(&self, id: &str) {
        let mut state = self.state.lock().await;
        if Self::has_editable_row(&state, id) {
            state.edit_holds.remove(id);
        }
    }

    fn has_editable_row(state: &State, id: &str) -> bool {
        state
            .pending_inputs
            .iter()
            .any(|item| item.is_queue_editable() && item.has_queue_id(id))
    }

    fn has_protected_row(state: &State, id: &str) -> bool {
        state
            .pending_inputs
            .iter()
            .any(|item| item.is_queue_protected() && item.has_queue_id(id))
    }

    /// Merge consecutive plain prompts into `pending[0]` via
    /// [`echo_agent_prompt_queue::combine_prefix_len`]. `skip_ids` holds rows under
    /// composer edit. Merged-away items complete as
    /// [`PromptCompletionKind::RemovedFromQueue`].
    pub(super) fn combine_front_pending_inputs(
        pending: &mut std::collections::VecDeque<InputItem>,
        skip_ids: &[&str],
    ) {
        use echo_agent_prompt_queue::{CombineGate, combine_prefix_len};

        if pending.len() < 2 {
            return;
        }
        let gates: Vec<CombineGate<'_>> = pending.iter().map(Self::combine_gate).collect();
        let n = combine_prefix_len(gates, skip_ids);
        if n < 2 {
            return;
        }
        for _ in 1..n {
            let Some(next) = pending.remove(1) else {
                break;
            };
            // The follower's text is folded into the front's turn below, so it
            // still runs — but its own queue row is gone, so it resolves as
            // RemovedFromQueue (the same completion a client sees for an
            // explicit dequeue). The multi-client UI repaints its bubble from
            // the promote broadcast's `running_combined_texts`.
            Self::respond_removed_prompt(next.respond_to);
            let extra = Self::joined_text_blocks(&next.prompt_blocks);
            if let Some(front) = pending.front_mut() {
                Self::append_text_to_prompt(front, &extra);
            }
        }
    }

    fn combine_gate(item: &InputItem) -> echo_agent_prompt_queue::CombineGate<'_> {
        let is_bash = Self::extract_bash_command(&item.prompt_blocks).is_some();
        let is_plain_prompt = item.is_queue_editable()
            && item.queue_meta.as_ref().map(|m| m.kind.as_str()) == Some("prompt")
            && !is_bash;
        let mut has_text = false;
        let mut has_images = false;
        let mut is_expanded_skill = false;
        let mut non_text_non_image = false;
        for block in &item.prompt_blocks {
            match block {
                acp::ContentBlock::Text(t) => {
                    if Self::has_display_text(t) {
                        is_expanded_skill = true;
                    }
                    if !t.text.is_empty() {
                        has_text = true;
                    }
                }
                acp::ContentBlock::Image(_) => has_images = true,
                _ => non_text_non_image = true,
            }
        }
        // Follower eligibility also requires single plain text; encode via
        // is_expanded_skill / has_images / non_text_non_image.
        let text = item
            .queue_meta
            .as_ref()
            .map(|m| m.text.as_str())
            .unwrap_or("");
        echo_agent_prompt_queue::CombineGate {
            id: item.prompt_id.as_str(),
            // A row with its own override can't merge into another turn (that would drop its bound).
            is_plain_prompt: is_plain_prompt
                && has_text
                && !non_text_non_image
                && item.tool_overrides_update.is_none(),
            is_synthetic: item.input_origin.is_synthetic(),
            is_expanded_skill,
            is_bash,
            has_images,
            text: if text.is_empty() {
                // Fall back so empty meta still participates when blocks have text.
                item.prompt_blocks
                    .iter()
                    .find_map(|b| match b {
                        acp::ContentBlock::Text(t) if !t.text.is_empty() => Some(t.text.as_str()),
                        _ => None,
                    })
                    .unwrap_or("")
            } else {
                text
            },
        }
    }

    fn has_display_text(t: &acp::TextContent) -> bool {
        t.meta
            .as_ref()
            .and_then(|m| m.get("displayText"))
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.trim().is_empty())
    }

    fn joined_text_blocks(blocks: &[acp::ContentBlock]) -> String {
        use echo_agent_prompt_queue::join_texts;
        join_texts(blocks.iter().filter_map(|block| match block {
            acp::ContentBlock::Text(t) if !t.text.is_empty() => Some(t.text.as_str()),
            _ => None,
        }))
    }

    fn append_text_to_prompt(item: &mut InputItem, extra: &str) {
        use echo_agent_prompt_queue::TEXT_SEPARATOR;

        if extra.is_empty() {
            return;
        }
        if let Some(meta) = item.queue_meta.as_mut() {
            match meta.combined_texts.as_mut() {
                Some(segs) => segs.push(extra.to_string()),
                None => {
                    meta.combined_texts = Some(vec![meta.text.clone(), extra.to_string()]);
                }
            }
        }
        // Append to the LAST text block so a multi-text front stays ordered
        // (front text first, then the follower); `combined_texts` mirrors that.
        if let Some(acp::ContentBlock::Text(t)) = item
            .prompt_blocks
            .iter_mut()
            .rev()
            .find(|b| matches!(b, acp::ContentBlock::Text(_)))
        {
            if !t.text.is_empty() {
                t.text.push_str(TEXT_SEPARATOR);
            }
            t.text.push_str(extra);
        }
        if let Some(meta) = item.queue_meta.as_mut() {
            meta.text = Self::queue_text_from_blocks(&item.prompt_blocks);
        }
        Self::stamp_combined_display_texts_meta(item);
    }

    fn stamp_combined_display_texts_meta(item: &mut InputItem) {
        use echo_agent_prompt_queue::stamp_combined_display_texts;

        let Some(segs) = item
            .queue_meta
            .as_ref()
            .and_then(|m| m.combined_texts.as_ref())
            .cloned()
        else {
            return;
        };
        // Stamp the first text block (matches append_text_to_prompt); an
        // image-first front would otherwise lose the replay multi-bubble meta.
        let Some(acp::ContentBlock::Text(t)) = item
            .prompt_blocks
            .iter_mut()
            .find(|b| matches!(b, acp::ContentBlock::Text(_)))
        else {
            return;
        };
        let map = t.meta.get_or_insert_with(acp::Meta::new);
        stamp_combined_display_texts(map, &segs);
    }

    /// Replace a queued item's prompt body with `new_text` and bump its LWW
    /// version metadata. Shared by `handle_edit_queued_prompt` and the
    /// turn-ended fallback in `handle_interject_queued_prompt`.
    ///
    /// Replaces the text blocks with a single text block carrying the new
    /// text; Image blocks are RETAINED — the queue-edit wire is text-only,
    /// so a text edit must never silently detach the row's pasted images
    /// (mirrors the pager's local-row edit semantics). Other non-text
    /// blocks are still dropped — an explicit retype is a fresh prompt
    /// body. The `displayText` meta is left unset so the queue text shown
    /// to other clients is exactly what the editor typed (no stale skill
    /// expansion).
    fn apply_queued_prompt_edit(item: &mut InputItem, new_text: String, editor: Option<&str>) {
        // A bash row executes `extract_bash_command`'s meta value, not the
        // block text — rebuild the meta with the edited text or the edit
        // demotes the row to a plain model prompt.
        let meta = Self::extract_bash_command(&item.prompt_blocks)
            .is_some()
            .then(|| {
                let value = serde_json::to_value(
                    crate::extensions::prompt_meta::PromptBlockMeta::bash(new_text.clone()),
                )
                .expect("PromptBlockMeta serializes");
                value
                    .as_object()
                    .cloned()
                    .expect("PromptBlockMeta serializes to object")
            });
        let mut blocks = vec![acp::ContentBlock::Text(
            acp::TextContent::new(new_text.clone()).meta(meta),
        )];
        blocks.extend(
            std::mem::take(&mut item.prompt_blocks)
                .into_iter()
                .filter(|b| matches!(b, acp::ContentBlock::Image(_))),
        );
        item.prompt_blocks = blocks;
        if let Some(meta) = item.queue_meta.as_mut() {
            meta.text = new_text;
            meta.combined_texts = None;
            meta.version = meta.version.saturating_add(1);
            meta.last_editor = editor.map(str::to_string);
        }
    }
}
