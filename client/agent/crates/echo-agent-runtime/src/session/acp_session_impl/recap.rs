//! Recap and `/btw` side questions on `SessionActor`.
//!
//! Shared cache-aligned request setup lives in [`super::side_call`].
//! Per-turn dashboard summary lifecycle lives in [`super::turn_summary`].

use super::side_call::{AuxCall, log_prompt_cache_usage};
use super::*;

use crate::session::SideQuestionError;
use echo_agent_sampling_types::SamplingError;

/// Max characters of a recap persisted to `summary.json` for listing surfaces.
/// The full recap can be long and rides every row of the session-list response;
/// listing cards only show a short preview, so bound what goes on the wire.
const RECAP_PERSIST_MAX_CHARS: usize = 240;

/// Retry policy for the one-shot `/btw` model call: 3 attempts total
/// (1 try + 2 retries), 500ms → 1s jittered backoff. Deliberately short —
/// nothing like the sampler actor's budget — so a fleet-wide capacity event
/// can't multiply side-question traffic into a retry storm.
fn side_question_retry_policy() -> backon::ExponentialBuilder {
    backon::ExponentialBuilder::default()
        .with_max_times(2)
        .with_min_delay(std::time::Duration::from_millis(500))
        .with_max_delay(std::time::Duration::from_secs(1))
        .with_jitter()
}

/// Retry transient failures per the canonical [`SamplingError::is_retryable`]
/// rule (5xx incl. Cloudflare 52x, stream/connect glitches), minus the shared
/// vetoes (`x-should-retry: false`, context length) and rate limits — a 429
/// needs `Retry-After`-scale waits, not this sub-second budget.
fn should_retry_side_question(e: &SamplingError) -> bool {
    e.is_retryable() && !e.is_rate_limited() && !e.is_retry_vetoed()
}

/// Clone the base `/btw` request and stamp a fresh `req_id`, so retried
/// attempts never collide in logs. Everything else is byte-identical.
fn build_side_question_attempt(base: &ConversationRequest) -> ConversationRequest {
    let mut request = base.clone();
    request.x_echo_agent_req_id = Some(format!("echo-agent-btw-{}", uuid::Uuid::new_v4()));
    request
}
impl SessionActor {
    /// Answers a `/btw` side question with one model call over the parent session's context, and saves it to `btw_history.jsonl` under a new
    /// btw session ID. Client tool calls are dropped rather than run, hosted search still runs, and transient failures retry on a short budget.
    pub(super) async fn handle_side_question(
        &self,
        question: &str,
    ) -> Result<String, SideQuestionError> {
        let btw_session_id = format!("btw-{}", uuid::Uuid::new_v4());
        let parent_session_id = self.session_info.id.to_string();
        let asked_at = chrono::Utc::now();

        let sampling_client = self
            .prepare_chat_completion(false)
            .await
            .map_err(|e| SideQuestionError::PrepareClient(e.to_string()))?;

        // Full conversation snapshot including system prompt, reasoning, tool calls, and results.
        let mut items = self.chat_state_handle.get_conversation().await;

        let sampling_config = self.chat_state_handle.get_sampling_config().await;
        let reasoning_effort = sampling_config.as_ref().and_then(|c| c.reasoning_effort);
        if super::side_call::should_strip_side_call_reasoning(
            sampling_client.api_backend(),
            reasoning_effort,
        ) {
            items = echo_agent_chat_state::compaction_utils::strip_reasoning_blocks(items);
        }

        // /btw fires mid-turn, so the snapshot may end with an assistant message whose tool_calls have no matching ToolResult yet.
        crate::session::helpers::session_recap::pop_trailing_tool_run(&mut items);

        let (instruction, tool_specs, hosted_tools) =
            self.side_question_prompt_and_tools(question).await;
        items.push(instruction);

        let model = sampling_config.map(|c| c.model).unwrap_or_default();

        let persist = |answer: String, success: bool, error: Option<String>, attempts: u32| {
            let _ = self.notifications.persistence_tx.send(PersistenceMsg::Btw(
                crate::session::persistence::BtwEntry {
                    btw_session_id: btw_session_id.clone(),
                    parent_session_id: parent_session_id.clone(),
                    asked_at,
                    question: question.to_string(),
                    answer,
                    model: model.clone(),
                    success,
                    error,
                    attempts,
                },
            ));
        };

        // Built once; each attempt clones it and stamps a fresh req_id, and retries are rare so the success path pays one clone.
        let base_request = self.parent_cached_request(AuxCall {
            items,
            tools: tool_specs,
            hosted_tools,
            model: model.clone(),
            reasoning_effort,
            backend: sampling_client.api_backend(),
            conv_id: btw_session_id.clone(),
            req_id: format!("echo-agent-btw-{}", uuid::Uuid::new_v4()),
        });

        // conversation_collect is one-shot (no sampler-actor retry); /btw adds
        // its own bounded transient-failure retry (policy + predicate above).
        use backon::Retryable as _;
        let attempts = std::cell::Cell::new(1u32);
        let result =
            (|| sampling_client.conversation_collect(build_side_question_attempt(&base_request)))
                .retry(side_question_retry_policy())
                .when(should_retry_side_question)
                .notify(|e: &SamplingError, backoff: std::time::Duration| {
                    attempts.set(attempts.get() + 1);
                    tracing::warn!(
                        backoff_ms = backoff.as_millis() as u64,
                        error = %e,
                        "side question transient failure; retrying"
                    );
                })
                .await;

        match result {
            Ok(response) => {
                log_prompt_cache_usage("btw", sampling_client.api_backend(), &response);
                let content = response.assistant_text();
                if content.is_empty() {
                    let err = SideQuestionError::EmptyResponse;
                    persist(String::new(), false, Some(err.to_string()), attempts.get());
                    return Err(err);
                }
                persist(content.clone(), true, None, attempts.get());
                Ok(content)
            }
            Err(e) => {
                let err = SideQuestionError::from(e);
                persist(String::new(), false, Some(err.to_string()), attempts.get());
                Err(err)
            }
        }
    }

    /// The `/btw` instruction and tools. Tools ship unchanged so the cached prefix matches, so only the prompt keeps the model from calling them.
    async fn side_question_prompt_and_tools(
        &self,
        question: &str,
    ) -> (
        ConversationItem,
        Vec<ToolSpec>,
        Vec<echo_agent_sampling_types::HostedTool>,
    ) {
        let tag = self.reminder_wrapper_tag();
        let instruction = ConversationItem::user(format!(
            "<{tag}>This is a side question from the user. \
             You must answer this question directly in a single response.\n\n\
             IMPORTANT CONTEXT:\n\
             - You are a separate, lightweight agent spawned to answer this one question\n\
             - The main agent is NOT interrupted - it continues working independently in the background\n\
             - You share the conversation context but are a completely separate instance\n\
             - Do NOT reference being interrupted or what you were \"previously doing\" - that framing is incorrect\n\n\
             CRITICAL CONSTRAINTS:\n\
             - Do NOT call any tools, respond with plain text only\n\
             - A tool call cannot help you: nothing runs on the user's machine and you get no turn in which to read a result\n\
             - This is a one-off response - there will be no follow-up turns\n\
             - You can ONLY provide information based on what you already know from the conversation context\n\
             - NEVER say things like \"Let me try...\", \"I'll now...\", \"Let me check...\", or promise to take any action\n\
             - If you don't know the answer, say so - do not offer to look it up or investigate\n\n\
             Simply answer the question with the information you have.</{tag}>\n\n\
             {question}"
        ));
        // Same tools as the main turn: they serialize into the cached prefix, and a side question must not search past the active cutoff.
        let tool_specs = self.turn_base_tool_specs(&self.prepare_tool_definitions().await);
        (instruction, tool_specs, self.hosted_tools_for_turn())
    }

    /// Generate a session recap and broadcast it via
    /// [`SessionUpdate::SessionRecap`](crate::extensions::notification::SessionUpdate::SessionRecap).
    ///
    /// Snapshots the conversation, appends a single recap instruction turn
    /// (reusing the prompt prefix verbatim so the provider cache stays warm),
    /// makes one tool-free model call, and emits the cleaned one-line summary
    /// for display only. It never mutates the conversation.
    ///
    /// Best-effort: a failed or empty generation is logged and dropped.
    /// A missing recap must never disrupt the session.
    pub(super) async fn handle_recap(&self, auto: bool) {
        use crate::session::helpers::session_recap;

        // Snapshot before the first await so a prompt accepted while we await
        // the conversation reads as bumped-after-capture and cancels this recap.
        let recap_epoch = self.recap_epoch.get();

        let conversation = self.chat_state_handle.get_conversation().await;
        let main_turns = session_recap::main_turn_count(&conversation);

        let stored = self.last_recap_main_turn.get();
        let last = if stored > main_turns {
            let healed = main_turns.saturating_sub(1);
            self.last_recap_main_turn.set(healed);
            // Keep disk aligned after compaction/rewind so resume does not
            // re-load a watermark above current main turns.
            session_recap::save_recap_watermark(
                &crate::session::persistence::session_dir(&self.session_info),
                healed,
            );
            healed
        } else {
            stored
        };

        const RECAP_MIN_IDLE_MS: i64 = 3 * 60 * 1000;
        let last_ms = self
            .last_api_request_at
            .load(std::sync::atomic::Ordering::Relaxed);
        let idle_ms = chrono::Utc::now().timestamp_millis() - last_ms;
        let idle_ok = last_ms != 0 && idle_ms >= RECAP_MIN_IDLE_MS;

        if let Err(reason) = session_recap::recap_gate(main_turns, last, auto, idle_ok) {
            tracing::debug!(auto, main_turns, last, reason, "skipping recap");
            // A manual `/recap` shows a loading spinner; tell the client there
            // is nothing to recap so it can clear it (auto shows none).
            if !auto {
                self.emit_recap_unavailable().await;
            }
            return;
        }

        // Serialize recap work: watermark alone cannot exclude concurrent manual
        // re-recaps once last == main_turns (in-flight or finished). Claim after
        // the gate with no await between check and set — LocalSet + Cell makes
        // that atomic for concurrent spawn_local Recap cmds.
        if self.recap_in_flight.get() {
            tracing::debug!(auto, main_turns, "skipping recap: another recap in flight");
            if !auto {
                self.emit_recap_unavailable().await;
            }
            return;
        }
        self.recap_in_flight.set(true);
        // Clear in-flight on every exit. Advance watermark only on success/suppress
        // (not on failure/empty/cancel) so auto can retry later for this turn if needed.
        let clear_in_flight = || self.recap_in_flight.set(false);

        let setup = match self.prepare_side_call().await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "recap: failed to prepare sampling client");
                clear_in_flight();
                // A manual `/recap` shows a loading spinner; clear it on failure.
                if !auto {
                    self.emit_recap_unavailable().await;
                }
                return;
            }
        };
        let tag = self.reminder_wrapper_tag();
        let items = session_recap::budget_recap_items(
            conversation,
            tag,
            setup.strip_reasoning,
            setup.context_window,
        );
        let strip_reasoning = setup.strip_reasoning;
        let model = setup.model.clone();
        let started_at = chrono::Utc::now().to_rfc3339();
        let x_echo_agent_conv_id = format!("recap-{}", uuid::Uuid::new_v4());
        let x_echo_agent_req_id = format!("echo-agent-recap-{}", uuid::Uuid::new_v4());
        let request = self
            .side_call_request(
                &setup,
                items,
                x_echo_agent_conv_id.clone(),
                x_echo_agent_req_id.clone(),
            )
            .await;
        // The artifact records the exact model-facing items after trust
        // projection; canonical conversation state remains raw.
        let chat_history_for_artifact = request.items.clone();

        let response = match setup.client.conversation_collect(request).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "recap: model call failed");
                self.persist_recap_request_artifact(
                    chat_history_for_artifact,
                    &model,
                    auto,
                    strip_reasoning,
                    tag,
                    &x_echo_agent_req_id,
                    &x_echo_agent_conv_id,
                    started_at,
                    None,
                    None,
                    Some(&e.to_string()),
                );
                clear_in_flight();
                // A manual `/recap` shows a loading spinner; clear it on failure.
                if !auto {
                    self.emit_recap_unavailable().await;
                }
                return;
            }
        };

        log_prompt_cache_usage("recap", setup.client.api_backend(), &response);
        let raw_response = response.assistant_text();
        let summary = session_recap::clean_recap_text(&raw_response);
        if summary.is_empty() {
            tracing::debug!("recap: model returned empty summary");
            self.persist_recap_request_artifact(
                chat_history_for_artifact,
                &model,
                auto,
                strip_reasoning,
                tag,
                &x_echo_agent_req_id,
                &x_echo_agent_conv_id,
                started_at,
                None,
                Some(raw_response.as_str()).filter(|s| !s.is_empty()),
                Some("empty summary after clean_recap_text"),
            );
            clear_in_flight();
            // A manual `/recap` shows a loading spinner; clear it when empty.
            if !auto {
                self.emit_recap_unavailable().await;
            }
            return;
        }

        // New prompt while generating: keep artifact, skip display, leave watermark.
        // Applies to manual `/recap` too: spinner-less clients (e.g. EchoAgent
        // Desktop) would otherwise append the late recap mid-turn.
        if self.recap_was_cancelled(recap_epoch) {
            tracing::info!(
                auto,
                recap_epoch,
                current_epoch = self.recap_epoch.get(),
                "session recap cancelled (new prompt while generating; not shown)"
            );
            self.persist_recap_request_artifact(
                chat_history_for_artifact,
                &model,
                auto,
                strip_reasoning,
                tag,
                &x_echo_agent_req_id,
                &x_echo_agent_conv_id,
                started_at,
                Some(summary.as_str()),
                Some(raw_response.as_str()),
                Some("cancelled: new prompt while recap generating"),
            );
            self.drop_recap_after_cancel(auto).await;
            return;
        }

        // Auto long-tail: save artifact, do not show. Manual always shows.
        if auto && session_recap::should_suppress_auto_recap_display(&raw_response, &summary) {
            tracing::info!(
                raw_bytes = raw_response.len(),
                summary_bytes = summary.len(),
                "session recap suppressed (auto long-tail; artifact saved, not shown)"
            );
            self.persist_recap_request_artifact(
                chat_history_for_artifact,
                &model,
                auto,
                strip_reasoning,
                tag,
                &x_echo_agent_req_id,
                &x_echo_agent_conv_id,
                started_at,
                Some(summary.as_str()),
                Some(raw_response.as_str()),
                Some("auto recap suppressed: long-tail output not shown"),
            );
            // Commit watermark only if still live (no await between check and mark).
            let _ = self.try_commit_recap(recap_epoch, main_turns);
            return;
        }

        tracing::info!(auto, chars = summary.len(), "session recap generated");
        self.persist_recap_request_artifact(
            chat_history_for_artifact,
            &model,
            auto,
            strip_reasoning,
            tag,
            &x_echo_agent_req_id,
            &x_echo_agent_conv_id,
            started_at,
            Some(summary.as_str()),
            Some(raw_response.as_str()),
            None,
        );
        // Final cancel check immediately before mark+emit (no await between).
        if !self.try_commit_recap(recap_epoch, main_turns) {
            if !auto {
                self.emit_recap_unavailable().await;
            }
            return;
        }
        // Persist a bounded preview of the committed recap so listing surfaces
        // (`/resume`, `/session-info`) can show it whenever available. Only a
        // preview: the full recap can be long and rides every row of the
        // session-list response, while the card shows a short line. Distinct
        // from the per-turn `last_turn_summary`; last-writer-wins.
        let recap_preview: String = summary.chars().take(RECAP_PERSIST_MAX_CHARS).collect();
        let _ = self.notifications.persistence_tx.send(
            crate::session::persistence::PersistenceMsg::LastRecap(Some(recap_preview)),
        );
        self.send_echo_agent_notification(
            crate::extensions::notification::SessionUpdate::SessionRecap { summary, auto },
        )
        .await;
    }

    pub(crate) fn recap_was_cancelled(&self, epoch: u64) -> bool {
        self.recap_epoch.get() != epoch
    }

    /// If still live, advance watermark and clear in-flight; else clear only.
    /// Returns whether the recap may emit (or count as done for suppress).
    pub(crate) fn try_commit_recap(&self, recap_epoch: u64, main_turns: usize) -> bool {
        if self.recap_was_cancelled(recap_epoch) {
            self.recap_in_flight.set(false);
            false
        } else {
            self.last_recap_main_turn.set(main_turns);
            self.recap_in_flight.set(false);
            crate::session::helpers::session_recap::save_recap_watermark(
                &crate::session::persistence::session_dir(&self.session_info),
                main_turns,
            );
            true
        }
    }

    /// Cancel-branch cleanup after generation: clear in-flight; manual clients
    /// get `SessionRecapUnavailable` so their spinner can clear.
    pub(crate) async fn drop_recap_after_cancel(&self, auto: bool) {
        self.recap_in_flight.set(false);
        if !auto {
            self.emit_recap_unavailable().await;
        }
    }

    /// Persist a recap request artifact for offline prompt / garble analysis.
    /// Writes `{session_dir}/recap_requests/{request_id}.json` containing the
    /// exact `ConversationItem` list sent to the model plus the cleaned
    /// summary and raw assistant text (or error). Rides on the post-turn
    /// session archive to cloud storage like compaction request artifacts.
    /// Best-effort: send-failures are logged at `warn` and never surfaced —
    /// clear the loading spinner it is showing instead of animating forever.
    /// a missing artifact must never disrupt recap display.
    #[allow(clippy::too_many_arguments)]
    fn persist_recap_request_artifact(
        &self,
        chat_history: Vec<ConversationItem>,
        model: &str,
        auto: bool,
        strip_reasoning: bool,
        reminder_tag: &str,
        x_echo_agent_req_id: &str,
        x_echo_agent_conv_id: &str,
        started_at: String,
        summary: Option<&str>,
        raw_response: Option<&str>,
        error: Option<&str>,
    ) {
        use crate::extensions::notification::RecapRequestFile;
        use crate::session::persistence::PersistenceMsg;

        let artifact = RecapRequestFile {
            schema_version: 1,
            request_id: uuid::Uuid::new_v4().to_string(),
            created_at: started_at,
            trigger: if auto { "auto" } else { "manual" }.to_owned(),
            model: model.to_owned(),
            x_echo_agent_req_id: x_echo_agent_req_id.to_owned(),
            x_echo_agent_conv_id: x_echo_agent_conv_id.to_owned(),
            strip_reasoning,
            reminder_tag: reminder_tag.to_owned(),
            chat_history,
            summary: summary.map(str::to_owned),
            raw_response: raw_response.map(str::to_owned),
            error: error.map(str::to_owned),
        };

        if self
            .notifications
            .persistence_tx
            .send(PersistenceMsg::RecapRequest(artifact))
            .is_err()
        {
            tracing::warn!(
                session_id = %self.session_info.id.0,
                "Failed to send recap request artifact to persistence channel"
            );
        }
    }

    /// Tell the live client that a manual `/recap` produced no recap, so it can
    ///
    /// Only the manual path shows a spinner, so callers gate this on `!auto`.
    async fn emit_recap_unavailable(&self) {
        self.send_echo_agent_notification(
            crate::extensions::notification::SessionUpdate::SessionRecapUnavailable,
        )
        .await;
    }

    /// Handle an AI-powered shell command suggestion request.
    /// Builds a minimal prompt from the prefix and CWD, calls the sampler
    ///
    /// with low temperature and small max_tokens, and returns the suggestion.
    pub(super) async fn handle_ai_suggest(
        &self,
        prefix: &str,
        cwd: &str,
        model_override: Option<&str>,
    ) -> Option<String> {
        let sampling_client = self.prepare_chat_completion(false).await.ok()?;

        let system = "You are a shell command autocomplete engine. \
            Given a partial command, output ONLY the completed command. \
            No explanation, no markdown, no quotes. Just the raw command.";

        let user_msg = format!("CWD: {cwd}\nPartial command: {prefix}");

        let items = vec![
            ConversationItem::system(system.to_owned()),
            ConversationItem::user(user_msg),
        ];

        let model = match model_override {
            Some(m) => m.to_owned(),
            None => "echo-agent-build".to_owned(),
        };

        let request = ConversationRequest {
            items,
            tools: vec![],
            model: Some(model),
            temperature: Some(0.1),
            max_output_tokens: Some(50),
            ..Default::default()
        };

        let request_id = echo_agent_sampler::RequestId::random();
        let idle_timeout = std::time::Duration::from_secs(5);

        let result = match sampling_client.api_backend() {
            crate::sampling::ApiBackend::ChatCompletions => {
                let (raw, meta) = sampling_client.conversation_stream(request).await.ok()?;
                let events = echo_agent_sampler::stream_chat_completions(
                    raw,
                    meta,
                    request_id,
                    idle_timeout,
                );
                echo_agent_sampler::collect_response(events).await
            }
            crate::sampling::ApiBackend::Responses => {
                let (raw, meta, doom_loop) = sampling_client
                    .conversation_stream_responses(request)
                    .await
                    .ok()?;
                let events = echo_agent_sampler::stream_responses(
                    raw,
                    meta,
                    request_id,
                    idle_timeout,
                    doom_loop,
                );
                echo_agent_sampler::collect_response(events).await
            }
            crate::sampling::ApiBackend::Messages => {
                let (raw, meta) = sampling_client
                    .conversation_stream_messages(request)
                    .await
                    .ok()?;
                let events =
                    echo_agent_sampler::stream_messages(raw, meta, request_id, idle_timeout);
                echo_agent_sampler::collect_response(events).await
            }
        };

        match result {
            Ok((response, _metrics)) => {
                let text = response.assistant_text();
                if text.is_empty() { None } else { Some(text) }
            }
            Err(e) => {
                tracing::debug!(error = %e.message, "AI suggest inference failed");
                None
            }
        }
    }

    /// Predict the user's likely next prompt for tab-autocomplete ghost text.
    /// Fired by the client after a turn completes. Builds a compact text-only
    /// transcript of the recent conversation (see
    /// [`prompt_suggest::build_transcript`]) and makes one tool-free model
    /// call. The model is resolved by
    /// [`prompt_suggest::effective_suggest_model`]: env
    /// (`ECHO_AGENT_PROMPT_SUGGESTIONS_MODEL`) > `[models] prompt_suggestion`
    /// (config.toml) > remote `prompt_suggestion_model` (remote settings) >
    /// (config.toml) > remote `prompt_suggestion_model` (remote settings) >
    /// [`prompt_suggest::DEFAULT_SUGGEST_MODEL`] (`echo-agent-build-0.1`). Every
    /// tier except env is catalog-guarded against this shell's own model
    /// catalog — when the effective model is not sampleable here (e.g.
    /// `echo-agent-build-0.1` for OAuth users) the request is **skipped
    /// entirely** instead of fired doomed. The session model is never used:
    /// a per-turn background call must stay on the small model.
    /// Temperature, max_output_tokens, and
    /// reasoning_effort are left unset — mirrors [`Self::handle_recap`]: the
    /// proxy may inject provider defaults, a small token cap silently empties
    /// a reasoning model's response, and some models (e.g. `echo-agent-build`)
    /// reject an explicit `reasoningEffort` with a 400. Output is filtered
    /// through [`prompt_suggest::sanitize_suggestion`]; any failure returns
    /// through [`prompt_suggest::sanitize_suggestion`]; any failure returns
    /// `None`.
    pub(super) async fn handle_suggest_prompt(
        &self,
        model_override: Option<&str>,
    ) -> Option<String> {
        use crate::session::helpers::prompt_suggest;

        let pin = self.models_manager.prompt_suggest_model_pin();
        let Some(model) = prompt_suggest::effective_suggest_model(&pin, model_override, |m| {
            self.models_manager.model_in_catalog(m)
        }) else {
            tracing::debug!(
                pin = ?pin,
                client_hint = ?model_override,
                "prompt suggest: effective model not in catalog; skipping request"
            );
            return None;
        };

        let conversation = self.chat_state_handle.get_conversation().await;
        let Some(transcript) = prompt_suggest::build_transcript(&conversation) else {
            tracing::debug!(
                items = conversation.len(),
                "prompt suggest: no usable transcript"
            );
            return None;
        };

        let sampling_client = match self.prepare_chat_completion(false).await {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!(error = %e, "prompt suggest: sampling client unavailable");
                return None;
            }
        };

        tracing::debug!(
            model = %model,
            transcript_len = transcript.len(),
            "prompt suggest: requesting"
        );

        let cwd = self
            .tool_context
            .cwd
            .as_path()
            .to_string_lossy()
            .into_owned();
        let items = vec![
            ConversationItem::system(prompt_suggest::SUGGEST_PROMPT_SYSTEM.to_owned()),
            ConversationItem::user(prompt_suggest::suggest_prompt_user_message(
                &transcript,
                &cwd,
            )),
        ];

        let request = ConversationRequest {
            items,
            tools: vec![],
            model: Some(model),
            temperature: None,
            x_echo_agent_conv_id: Some(format!("promptsuggest-{}", uuid::Uuid::new_v4())),
            x_echo_agent_req_id: Some(format!("echo-agent-promptsuggest-{}", uuid::Uuid::new_v4())),
            x_echo_agent_session_id: Some(self.session_info.id.to_string()),
            x_echo_agent_agent_id: Some(echo_agent_telemetry::id::agent_id()),
            ..Default::default()
        };

        let response = match sampling_client.conversation_collect(request).await {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!(error = %e, "prompt suggest inference failed");
                return None;
            }
        };

        let raw = response.assistant_text();
        let mut suggestion = prompt_suggest::sanitize_suggestion(&raw);
        // Deterministic anti-repeat backstop: never ghost a multi-word
        // prompt the user already sent (the prompt asks the model not to,
        // but a filter guarantees it).
        if let Some(s) = &suggestion
            && prompt_suggest::is_repeat_of_user_message(s, &conversation)
        {
            tracing::debug!("prompt suggest: rejected repeat of a past user prompt");
            suggestion = None;
        }
        tracing::debug!(
            raw_preview = %echo_agent_tools::util::truncate_str(raw.trim(), 60),
            accepted = suggestion.is_some(),
            "prompt suggest: response"
        );
        suggestion
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api(status: u16, message: &str, should_retry: Option<bool>) -> SamplingError {
        SamplingError::Api {
            status: reqwest::StatusCode::from_u16(status).unwrap(),
            message: message.into(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry,
            error_code: None,
        }
    }

    #[test]
    fn side_question_retries_transient_failures_only() {
        // Transient: overload (stream + proxy-wrapped 500 + 529), generic
        // 5xx, and Cloudflare edge 52x (SEV-576: /btw died on a 522).
        assert!(should_retry_side_question(&SamplingError::StreamError {
            error_type: "overloaded_error".into(),
            message: "Overloaded".into(),
            code: None,
        }));
        assert!(should_retry_side_question(&api(
            500,
            "stream error (overloaded_error): Overloaded",
            None
        )));
        for code in [503u16, 522, 529] {
            assert!(
                should_retry_side_question(&api(code, "transient", None)),
                "{code} must retry"
            );
        }

        // Server veto (`x-should-retry: false`) wins over any retryable status.
        assert!(!should_retry_side_question(&api(
            522,
            "timed out",
            Some(false)
        )));
        // Deterministic context-length failures never retry, even on 529.
        assert!(!should_retry_side_question(&api(
            529,
            "invalid_request_error: prompt is too long: 300000 tokens > 200000 maximum",
            None
        )));
        // Rate limits need Retry-After-scale waits, not this sub-second
        // budget; origin TLS and client errors never clear on retry.
        for code in [429u16, 525, 526, 400] {
            assert!(
                !should_retry_side_question(&api(code, "not transient", None)),
                "{code} must NOT retry"
            );
        }
    }

    /// The wired policy: 3 attempts total, backoff within the configured
    /// bounds (500ms + 1s base, jitter adds up to the current delay), and a
    /// fresh request id stamped per attempt.
    #[tokio::test(start_paused = true)]
    async fn side_question_retry_wiring_caps_attempts_and_bounds_backoff() {
        use backon::Retryable as _;

        let calls = std::cell::Cell::new(0u32);
        let start = tokio::time::Instant::now();
        let result: Result<(), SamplingError> = (|| async {
            calls.set(calls.get() + 1);
            Err(SamplingError::StreamError {
                error_type: "overloaded_error".into(),
                message: "Overloaded".into(),
                code: None,
            })
        })
        .retry(side_question_retry_policy())
        .when(should_retry_side_question)
        .await;

        assert!(result.is_err());
        assert_eq!(calls.get(), 3, "1 try + 2 retries");
        // Base delays 500ms + 1s; jitter adds (0, delay) per sleep.
        let elapsed = start.elapsed();
        assert!(
            elapsed >= std::time::Duration::from_millis(1_500),
            "elapsed {elapsed:?} below minimum backoff"
        );
        assert!(
            elapsed <= std::time::Duration::from_millis(3_100),
            "elapsed {elapsed:?} above maximum backoff"
        );
    }

    #[test]
    fn side_question_attempts_get_fresh_request_ids() {
        let base = ConversationRequest {
            x_echo_agent_conv_id: Some("btw-test".into()),
            ..Default::default()
        };
        let a = build_side_question_attempt(&base);
        let b = build_side_question_attempt(&base);
        let (a_id, b_id) = (
            a.x_echo_agent_req_id.unwrap(),
            b.x_echo_agent_req_id.unwrap(),
        );
        assert!(a_id.starts_with("echo-agent-btw-"));
        assert_ne!(a_id, b_id, "each attempt must get a fresh req_id");
        // Everything except the request id is byte-identical to the base.
        assert_eq!(a.x_echo_agent_conv_id, base.x_echo_agent_conv_id);
    }
}
