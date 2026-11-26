//! Session initialization concern for `SessionActor`: `initialize`, prefix
//! readiness, skills reload and reminders, session info, and model-metadata
//! refresh.
use super::*;
impl SessionActor {
    /// `true` for session-based ACP auth methods.
    fn is_session_based_auth(&self) -> bool {
        self.auth_method_id
            .load()
            .as_deref()
            .is_some_and(crate::agent::auth_method::is_session_based_method)
    }
    pub(super) fn to_acp_error(&self, err: SamplingError) -> acp::Error {
        if err.is_auth_error() {
            let method_guard = self.auth_method_id.load();
            let method = method_guard.as_deref();
            let msg = if method.is_some_and(crate::agent::auth_method::is_session_based_method) {
                crate::agent::auth_method::AUTH_ERROR_SESSION_EXPIRED
            } else {
                crate::agent::auth_method::AUTH_ERROR_API_KEY
            };
            echo_agent_telemetry::unified_log::error(
                "sampling auth error",
                Some(self.session_info.id.0.as_ref()),
                Some(serde_json::json!({
                    "method": method.map(|id| id.0.as_ref()),
                    "error": format!("{err}"),
                })),
            );
            return acp::Error::auth_required().data(msg);
        }
        map_sampling_err_to_acp(err)
    }
    /// Set up `[system, skill_reminder?]` — prefix is deferred to background.
    pub(super) async fn initialize(&self, system_prompt: String) {
        let bridge = self.agent.borrow().tool_bridge().clone();
        bridge.on_skill_discovery_clear().await;
        save_system_prompt(&self.session_info, &system_prompt);
        let system_message = ConversationItem::system(system_prompt);
        let mut messages = vec![system_message];
        if let Some(effects) = self.inject_baseline_skill_reminder(&mut messages).await
            && effects.send_available_commands
        {
            self.send_available_commands_update().await;
        }
        self.chat_state_handle
            .replace_conversation(messages.clone());
        persist_chat_history_jsonl_sync(&self.session_info, &messages);
    }
    /// Ensure the conversation carries the correct baseline skill (and
    /// workflow) `<system-reminder>`: exactly one for an agent that has
    /// skills/workflows and uses reminders, and none for an agent that
    /// renders skills inline via `<agent_skills>` with no workflows, or
    /// when nothing is pending.
    ///
    /// Called from `initialize` (fresh start, conversation is just `[system]`)
    /// and the zero-turn harness rebuild (`handle_rebuild_agent_for_definition`,
    /// conversation is the inherited zero-turn shape). Both drain the current
    /// bridge's pending baseline.
    ///
    /// Idempotent: strips any existing baseline skill reminder before
    /// injecting, so a reminder-using -> reminder-using rebuild -- where
    /// `rewrite_zero_turn_prefix` keeps the inherited reminder (it only drops it
    /// for an inline-rendering target) -- cannot double-list.
    ///
    /// The inline-rendering agent still drains (after enabling XML format) so `announced_names` is
    /// populated and later discovery reminders don't re-announce the baseline;
    /// `wrap_skill_reminder` returns `None` for the inline-rendering agent+`BaselineChange`.
    ///
    /// Returns the drained effects so callers can honor `send_available_commands`
    /// on their own schedule.
    pub(super) async fn inject_baseline_skill_reminder(
        &self,
        conversation: &mut Vec<ConversationItem>,
    ) -> Option<echo_agent_tools::types::skill_discovery_tracker::SkillUpdateEffects> {
        let bridge = self.agent.borrow().tool_bridge().clone();
        let is_cursor = self.is_cursor_harness();
        if is_cursor {
            bridge.set_skill_listing_xml_format(true).await;
        }
        conversation.retain(|item| {
            !matches!(
                item,
                ConversationItem::User(u)
                    if u.synthetic_reason
                        == Some(echo_agent_sampling_types::SyntheticReason::SystemReminder)
            )
        });
        let effects = bridge.apply_pending_skill_update().await;
        let skill_text = effects
            .as_ref()
            .and_then(|update| {
                if is_cursor
                    && update.kind
                        == echo_agent_tools::types::skill_discovery_tracker::SkillUpdateKind::BaselineChange
                {
                    None
                } else {
                    update.system_reminder.as_deref()
                }
            });
        if let Some(body) = crate::session::workflow::listing::merge_listing_sections(
            skill_text,
            self.workflow_listing_for_prompt().as_deref(),
        ) {
            let tag = self.reminder_wrapper_tag();
            conversation.push(ConversationItem::system_reminder(format!(
                "<{tag}>\n{body}\n</{tag}>"
            )));
        }
        effects
    }
    pub(super) async fn build_prefix_background(&self) -> String {
        let start = std::time::Instant::now();
        if matches!(self.mcp_strategy.get(), McpInitStrategy::Blocking) {
            use echo_agent_core::prompt::user_message::UserMessageTemplate;
            let mcp_wait = match self.agent.borrow().definition().user_message_template {
                UserMessageTemplate::Default => std::time::Duration::from_secs(15),
                _ => std::time::Duration::from_secs(60),
            };
            self.wait_for_mcp_handshakes_bounded(mcp_wait).await;
        }
        let prefix = self.build_user_message_prefix().await;
        tracing::info!(
            session_id = %self.session_info.id.0,
            elapsed_ms = start.elapsed().as_millis() as u64,
            "build_prefix_background: done"
        );
        prefix
    }
    /// Await the background prefix and inject at conversation index 1.
    /// Falls back to synchronous build on timeout (10s) or panic.
    pub(super) async fn ensure_prefix_ready(&self) {
        let Some(mut handle) = self.deferred_prefix.take() else {
            return;
        };
        let start = std::time::Instant::now();
        const WAIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
        let (prefix, source) = match tokio::time::timeout(WAIT_TIMEOUT, &mut handle).await {
            Ok(Ok(p)) => (p, "background"),
            Ok(Err(join_err)) => {
                tracing::warn!(
                    session_id = %self.session_info.id.0,
                    error = %join_err,
                    "ensure_prefix_ready: background task panicked, sync fallback"
                );
                (self.build_user_message_prefix().await, "sync_fallback")
            }
            Err(_elapsed) => {
                handle.abort();
                tracing::warn!(
                    session_id = %self.session_info.id.0,
                    timeout_ms = WAIT_TIMEOUT.as_millis() as u64,
                    "ensure_prefix_ready: background task not ready, sync fallback"
                );
                (self.build_user_message_prefix().await, "sync_fallback")
            }
        };
        let mut conversation = self.chat_state_handle.get_conversation().await;
        let insert_at = conversation.len().min(1);
        conversation.insert(insert_at, ConversationItem::user(prefix));
        if !self.startup_hints.preserve_inherited_system
            && !conversation_has_project_instructions(&conversation)
            && let Some(agents_md_reminder) = self.agent.borrow().agents_md_user_reminder()
        {
            let agents_md_at = (insert_at + 1).min(conversation.len());
            conversation.insert(
                agents_md_at,
                ConversationItem::project_instructions(agents_md_reminder),
            );
        }
        if let Some(personas_reminder) = self.agent.borrow().personas_user_reminder() {
            let personas_at = conversation
                .len()
                .min(
                    conversation
                        .iter()
                        .position(|item| {
                            matches!(item, ConversationItem::User(u) if u.synthetic_reason.is_none())
                        })
                        .unwrap_or(conversation.len()),
                );
            conversation.insert(
                personas_at,
                ConversationItem::system_reminder(personas_reminder),
            );
        }
        self.chat_state_handle.replace_conversation(conversation);
        tracing::info!(
            session_id = %self.session_info.id.0,
            source,
            elapsed_ms = start.elapsed().as_millis() as u64,
            "ensure_prefix_ready: done"
        );
    }
    /// Re-discover skills from disk, update the SkillManager baseline,
    /// and re-advertise slash commands to the client. Returns the number
    /// of skills discovered.
    pub(super) async fn reload_skills_from_disk(&self) -> usize {
        let cwd = &self.session_info.cwd;
        let skills_config = crate::util::config::load_config().await.skills;
        let plugin_snapshot = self.plugin_registry.borrow().clone();
        let new_skills = echo_agent_core::prompt::skills::list_skills_with_plugins(
            Some(cwd),
            &skills_config,
            plugin_snapshot.as_deref(),
            self.rebuild_spec.compat,
        )
        .await;
        let skill_count = new_skills.len();
        tracing::info!(
            session_id = %self.session_info.id.0,
            skill_count,
            "Reloaded skills from disk",
        );
        let bridge = self.agent.borrow().tool_bridge().clone();
        bridge.update_skill_baseline(new_skills).await;
        match bridge.apply_pending_skill_update().await {
            Some(effects) => self.apply_skill_update_effects(effects).await,
            None => self.send_available_commands_update().await,
        }
        skill_count
    }
    /// Skills used for slash resolve, mid-turn interjection expansion, and
    /// prompt skill listings. Same source as ACU: product REST for chat-kind
    /// (TTL-cached; never disk), `SkillManager` for Build.
    pub(crate) async fn slash_skills_for_resolve(
        &self,
    ) -> Vec<echo_agent_tools::implementations::skills::types::SkillInfo> {
        match slash_commands::acu_skill_source(self.is_chat_kind) {
            slash_commands::AcuSkillSource::Product => Vec::new(),
            slash_commands::AcuSkillSource::Disk => {
                let bridge = self.tool_bridge_handle();
                bridge.slash_skills().await
            }
        }
    }
    /// Send `AvailableCommandsUpdate` to the client.
    ///
    /// Chat-kind sessions advertise the product Skills REST catalog (same
    /// source as `list_commands(kind=chat)`). Build sessions read disk skills
    /// from the tools layer (`SkillManager`). Chat never falls back to disk.
    /// Product REST failure (or missing auth) reuses the shared last-successful
    /// product catalog (same as `list_commands(kind=chat)`); if none exists yet,
    /// advertises builtins only (never invents product skill names, never disk).
    /// Empty product success still advertises builtins.
    pub(super) async fn send_available_commands_update(&self) {
        let bridge = self.agent.borrow().tool_bridge().clone();
        let skills = match slash_commands::acu_skill_source(self.is_chat_kind) {
            slash_commands::AcuSkillSource::Product => {
                return;
            }
            slash_commands::AcuSkillSource::Disk => bridge.slash_skills().await,
        };
        let tool_names: Vec<String> = bridge
            .tool_definitions()
            .await
            .into_iter()
            .map(|td| td.function.name)
            .collect();
        let has_workflow_runs = !self.workflow_tracker().await.lock().list().is_empty();
        let availability = self.build_command_availability(&tool_names, has_workflow_runs);
        self.maybe_reconcile_active_goal_without_plan().await;
        let (_, workflows) = self.named_workflow_snapshot();
        let commands = slash_commands::available_commands(&skills, availability, &workflows);
        let meta = Some(slash_commands::build_tools_meta(&tool_names));
        tracing::info!(
            session_id = %self.session_info.id.0,
            command_count = commands.len(),
            tool_count = tool_names.len(),
            is_chat_kind = self.is_chat_kind,
            "Advertising available slash commands",
        );
        self.send_update(
            acp::SessionUpdate::AvailableCommandsUpdate(
                acp::AvailableCommandsUpdate::new(commands).meta(meta),
            ),
            None,
        )
        .await;
    }
    /// Build the wrapped `<system[_-]reminder>` carrier for a skill
    /// update, applying the harness-specific gate and tag selection.
    ///
    /// Returns `None` when no reminder should be emitted -- either
    /// because the effect carried no body, or because the compat
    /// harness is suppressing this kind of update. The compat preamble
    /// snapshots the full skill baseline in `<agent_skills>`, so a
    /// `BaselineChange` reminder fired for it would be redundant.
    /// `Discovery` reminders (skills found mid-session via tool
    /// navigation into directories the baseline hadn't seen) are kept
    /// for both harnesses; the preamble cannot list those.
    ///
    /// Tag selection for skill reminders. Centralized here so new call sites
    /// cannot accidentally drift the gating or tag selection.
    pub(super) fn wrap_skill_reminder(
        &self,
        effects: &echo_agent_tools::types::skill_discovery_tracker::SkillUpdateEffects,
    ) -> Option<ConversationItem> {
        use echo_agent_tools::types::skill_discovery_tracker::SkillUpdateKind;
        let is_cursor = self.is_cursor_harness();
        if is_cursor && effects.kind == SkillUpdateKind::BaselineChange {
            return None;
        }
        let text = effects.system_reminder.as_deref()?;
        let tag = self.reminder_wrapper_tag();
        Some(ConversationItem::system_reminder(format!(
            "<{tag}>\n{text}\n</{tag}>"
        )))
    }
    /// Apply skill update side-effects produced by the tools layer.
    ///
    /// The tools layer (`SkillManager`) owns skill state and projections.
    /// This method applies only the conversation/UI effects that require
    /// session capabilities:
    /// - Injecting a `<system-reminder>` user message (skill announcement)
    /// - Refreshing slash command advertisement via the ACP gateway
    ///
    /// Slash command data is read from `bridge.slash_skills()`.
    /// `PromptContext` is not involved. The system prompt is not mutated.
    ///
    /// Apply skill update effects: inject a system-reminder and refresh
    /// slash commands. Both default and compat agents receive mid-session
    /// discovery reminders.
    pub(super) async fn apply_skill_update_effects(
        &self,
        effects: echo_agent_tools::types::skill_discovery_tracker::SkillUpdateEffects,
    ) {
        if effects.send_available_commands {
            self.send_available_commands_update().await;
        }
        let Some(item) = self.wrap_skill_reminder(&effects) else {
            self.persist_announcement_state().await;
            return;
        };
        let turn_running = self.current_prompt_id.lock().map_or_else(
            |poisoned| poisoned.into_inner().is_some(),
            |guard| guard.is_some(),
        );
        if turn_running {
            self.pending_skill_reminders.lock().push(item);
        } else {
            self.chat_state_handle.push_user_message(item);
            self.persist_announcement_state().await;
        }
    }
    pub(super) async fn flush_pending_skill_reminders(&self) {
        let activation = self.plan_mode.lock().take_pending_activation();
        if let Some(text) = activation {
            self.chat_state_handle
                .push_user_message(ConversationItem::system_reminder(text));
            self.plan_mode.lock().record_reminder_injected();
            self.persist_plan_mode_state();
        }
        let items: Vec<ConversationItem> =
            std::mem::take(&mut *self.pending_skill_reminders.lock());
        if items.is_empty() {
            return;
        }
        for item in items {
            self.chat_state_handle.push_user_message(item);
        }
        self.persist_announcement_state().await;
    }
    /// Idle threshold for proactive model metadata refresh on session resume.
    /// If the session has been idle longer than this, we fetch fresh model config
    /// from cli-chat-proxy before the next API request to catch context_window changes.
    pub(super) const IDLE_REFRESH_THRESHOLD_SECS: i64 = 600;
    /// Record the current time as the last API request timestamp.
    pub(super) fn record_api_request_time(&self) {
        let now_ms = chrono::Utc::now().timestamp_millis();
        self.last_api_request_at
            .store(now_ms, std::sync::atomic::Ordering::Relaxed);
    }
    /// Check if the session has been idle and proactively refresh model metadata.
    ///
    /// Called at the start of each turn. If idle > `IDLE_REFRESH_THRESHOLD_SECS`,
    /// fetches `/models-v2` from cli-chat-proxy and updates the cached
    /// context_window / max_completion_tokens if remote settings changed them.
    ///
    /// Skipped for BYOK users (no remote settings, no `/models-v2`).
    pub(super) async fn maybe_refresh_model_metadata_on_resume(&self) {
        if !self.is_session_based_auth() {
            return;
        }
        let last_request_ms = self
            .last_api_request_at
            .load(std::sync::atomic::Ordering::Relaxed);
        if last_request_ms == 0 {
            return;
        }
        let now_ms = chrono::Utc::now().timestamp_millis();
        let idle_secs = (now_ms - last_request_ms) / 1000;
        if idle_secs < Self::IDLE_REFRESH_THRESHOLD_SECS {
            return;
        }
        let Some(current_config) = self.chat_state_handle.get_sampling_config().await else {
            return;
        };
        let current_model = &current_config.model;
        let base_url = &current_config.base_url;
        if !crate::util::is_cli_chat_proxy_url(base_url) {
            return;
        }
        tracing::info!(
            idle_secs,
            threshold_secs = Self::IDLE_REFRESH_THRESHOLD_SECS,
            "Session resumed after idle — refreshing model metadata from cli-chat-proxy"
        );
        let Some(ref am) = self.auth_manager else {
            tracing::debug!("No auth manager available for model metadata refresh");
            return;
        };
        let _ = am.auth().await;
        let provider: Arc<dyn echo_agent_auth::AuthCredentialProvider> = Arc::new(
            crate::auth::credential_provider::ShellAuthCredentialProvider::new(
                am.clone(),
                None,
                None,
            ),
        );
        let middleware_client =
            crate::http::with_auth_retry(crate::http::shared_client(), provider);
        let url = format!("{}/models-v2", base_url);
        let parse_models_response =
            |json: serde_json::Value| -> Option<(std::num::NonZeroU64, Option<u32>)> {
                let data = json.get("data")?.as_array()?;
                for entry in data {
                    let parsed = crate::remote::client::parse_remote_model_value(entry, base_url)?;
                    if parsed.model == *current_model {
                        return Some((parsed.context_window, parsed.max_completion_tokens));
                    }
                }
                None
            };
        #[allow(unused_mut)]
        let mut request = middleware_client
            .get(&url)
            .header("X-Echo-Agent-Token-Auth", "echo-agent-cli")
            .header("x-echo-agent-client-version", echo_agent_version::VERSION)
            .header(
                crate::http::CLIENT_MODE_HEADER,
                crate::http::process_client_mode(),
            )
            .timeout(std::time::Duration::from_secs(5));
        let built = match request.build() {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to build idle-refresh models request");
                return;
            }
        };
        let (response, stamp) =
            match echo_agent_auth::execute_with_stamp(&middleware_client, built).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to fetch models for idle refresh");
                    return;
                }
            };
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            crate::auth::attribution::record_consumer_401(
                am,
                None,
                crate::auth::attribution::ConsumerKind::IdleResumeModelRefresh,
                "",
                stamp.as_ref().map(|s| s.0.as_str()),
            );
        }
        let result = if !response.status().is_success() {
            tracing::warn!(
                status = response.status().as_u16(),
                "Failed to fetch models for idle refresh"
            );
            None
        } else {
            response
                .json::<serde_json::Value>()
                .await
                .ok()
                .and_then(parse_models_response)
        };
        let Some((new_context_window, new_max_completion_tokens)) = result else {
            tracing::debug!("Model metadata refresh: no update or fetch failed");
            return;
        };
        let mut config_changed = false;
        let mut updated_config = current_config.clone();
        if current_config.context_window != new_context_window
            && self.compaction.context_window_override.is_none()
        {
            tracing::info!(
                old_context_window = current_config.context_window.get(),
                new_context_window = new_context_window.get(),
                "Context window updated on session resume"
            );
            updated_config.context_window = new_context_window;
            config_changed = true;
        }
        if let Some(new_mct) = new_max_completion_tokens
            && current_config.max_completion_tokens != Some(new_mct)
        {
            tracing::info!(
                old_max_completion_tokens = current_config.max_completion_tokens,
                new_max_completion_tokens = new_mct,
                "Max completion tokens updated on session resume"
            );
            updated_config.max_completion_tokens = Some(new_mct);
            config_changed = true;
        }
        if config_changed {
            self.chat_state_handle
                .update_sampling_config(updated_config);
        }
    }
    /// Update cached sampling config if model metadata changed (from response headers).
    pub(super) async fn handle_model_metadata_update(
        &self,
        metadata: crate::sampling::ResponseModelMetadata,
    ) {
        if let Some(ref etag) = metadata.models_etag {
            self.models_manager.refresh_if_new_etag(etag.clone()).await;
        }
        let current_config = match self.chat_state_handle.get_sampling_config().await {
            Some(cfg) => cfg,
            None => return,
        };
        let mut config_changed = false;
        let mut new_context_window = current_config.context_window;
        let mut new_max_completion_tokens = current_config.max_completion_tokens;
        if let Some(new_cw) = metadata.context_window.and_then(std::num::NonZeroU64::new)
            && current_config.context_window != new_cw
            && self.compaction.context_window_override.is_none()
        {
            if new_cw < current_config.context_window {
                tracing::warn!(
                    current_context_window = current_config.context_window.get(),
                    header_context_window = new_cw.get(),
                    "Ignoring context_window downgrade from response header"
                );
            } else {
                tracing::info!(
                    old_context_window = current_config.context_window.get(),
                    new_context_window = new_cw.get(),
                    "Model context_window upgraded via response header"
                );
                new_context_window = new_cw;
                config_changed = true;
            }
        }
        if let Some(new_mct) = metadata.max_completion_tokens
            && current_config.max_completion_tokens != Some(new_mct)
        {
            tracing::info!(
                old_max_completion_tokens = current_config.max_completion_tokens,
                new_max_completion_tokens = new_mct,
                "Model max_completion_tokens changed via response header"
            );
            new_max_completion_tokens = Some(new_mct);
            config_changed = true;
        }
        if !config_changed {
            return;
        }
        let updated_config = echo_agent_sampling_types::SamplingConfig {
            context_window: new_context_window,
            max_completion_tokens: new_max_completion_tokens,
            ..current_config
        };
        self.chat_state_handle
            .update_sampling_config(updated_config);
    }
    /// Inject the actor's managed Read-deny globs into the current ToolBridge so
    /// the Grep tool excludes policy-forbidden paths. No-op when empty. Called on
    /// session setup and re-called after an agent rebuild (the rebuilt bridge
    pub(super) async fn inject_deny_read_globs(&self) {
        if self.deny_read_globs.is_empty() {
            return;
        }
        self.agent
            .borrow()
            .tool_bridge()
            .update_resource(echo_agent_tools::types::resources::DenyReadGlobs(
                self.deny_read_globs.clone(),
            ))
            .await;
    }
    /// Shared by `/session-info`, `/context`, and `GetSessionInfo`.
    pub(super) async fn build_session_info(&self) -> SessionInfoData {
        let config = self.chat_state_handle.get_sampling_config().await;
        let model = config.as_ref().map(|c| c.model.clone());
        let context_window = config.as_ref().map(|c| c.context_window.get()).unwrap_or(0);
        let model_metadata = self.chat_state_handle.get_last_model_metadata().await;
        let total_tokens = self.chat_state_handle.get_estimated_total_tokens().await;
        let counts = self.chat_state_handle.get_conversation_counts().await;
        let turns = counts.user;
        let turn_index = self.chat_state_handle.get_prompt_index().await as u64;
        tracing::info!(turn_index, turns, resolved_model_id = ?model_metadata.resolved_model_id, model_fingerprint = ?model_metadata.model_fingerprint, "build_session_info");
        let model_fingerprint = model_metadata.model_fingerprint;
        let resolved_model_id = model_metadata.resolved_model_id.filter(|resolved| {
            model
                .as_deref()
                .is_some_and(|m| should_show_resolved_model(m, resolved))
        });
        let signals = self.signals_handle().snapshot().await;
        let compaction_count = signals.as_ref().map(|s| s.compaction_count).unwrap_or(0);
        let turn_count = signals.as_ref().map(|s| s.turn_count).unwrap_or(0);
        let tool_call_count = signals.as_ref().map(|s| s.tool_call_count).unwrap_or(0);
        let system_message = self.chat_state_handle.get_system_message().await;
        let system_prompt_tokens = system_message
            .as_ref()
            .map(echo_agent_chat_state::estimate_system_message_tokens)
            .unwrap_or(0);
        let backend_search_active = self.backend_search_active();
        let tool_defs: Vec<_> = self
            .prepare_tool_definitions_inner()
            .await
            .into_iter()
            .filter(|td| !backend_search_active || td.function.name != "web_search")
            .collect();
        let tool_definitions_count = tool_defs.len();
        let tool_definitions_tokens =
            echo_agent_chat_state::estimate_tool_definitions_tokens(&tool_defs);
        let message_count = self.chat_state_handle.get_conversation_len().await;
        let message_tokens = self.chat_state_handle.get_estimated_messages_tokens().await;
        let usage_categories = self.usage_categories().await;
        let free_tokens = echo_agent_token_estimation::free_tokens(context_window, total_tokens);
        let usage_pct =
            echo_agent_token_estimation::usage_percentage_u8(total_tokens, context_window);
        let api_backend = config.as_ref().map(|c| format!("{:?}", c.api_backend));
        let agent_name = self.agent.borrow().definition().name.clone();
        let show_model_fingerprint = model
            .as_deref()
            .map(|id| self.models_manager.model_show_model_fingerprint(id))
            .unwrap_or(false);
        let conversation_id = None;
        SessionInfoData {
            model,
            model_display_name: None,
            resolved_model_id,
            model_fingerprint,
            show_model_fingerprint,
            api_backend,
            conversation_id,
            agent_name: Some(agent_name),
            turns: turns as u64,
            turn_index,
            context: ContextInfo {
                used: total_tokens,
                total: context_window,
                system_prompt_tokens,
                tool_definitions_count: tool_definitions_count as u64,
                tool_definitions_tokens,
                compaction_count: compaction_count as u64,
                turn_count: turn_count as u64,
                tool_call_count: tool_call_count as u64,
                message_count: message_count as u64,
                message_tokens,
                free_tokens,
                usage_pct,
                auto_compact_threshold_percent: self.compaction.threshold_percent.get(),
                usage_categories,
            },
        }
    }
    /// Build the `/context` usage rows for the skills listing, the workflow
    /// listing, and the MCP server listing (see [`TokenUsageCategory`]).
    ///
    /// Under templated sessions, the skills row estimates the mid-session
    /// envelope; the baseline lives in the first-message preamble with the
    /// same rows, so the difference is a few dozen tokens of envelope text.
    pub(super) async fn usage_categories(&self) -> Vec<TokenUsageCategory> {
        let bridge = self.tool_bridge_handle();
        let mut rows = Vec::new();
        if let Some(listing) = bridge.skill_listing_snapshot().await {
            rows.push(TokenUsageCategory::skills_listing(
                &listing.text,
                listing.skill_count,
            ));
        }
        if let Some((listing, count)) = self.workflow_listing_snapshot() {
            rows.push(TokenUsageCategory::workflows_listing(&listing, count));
        }
        if let Some(announcement) = self.mcp_announcement_snapshot().await {
            rows.push(TokenUsageCategory::mcp_servers(
                &announcement.text,
                announcement.server_count,
            ));
        }
        rows
    }
}
