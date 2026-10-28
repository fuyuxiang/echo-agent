//! Memory concern for `SessionActor`: memory flush, the dream pipeline,
//! memory tool registration, and note rewriting.

use super::*;

#[derive(Debug)]
pub(super) struct MemoryFlushSnapshot {
    counts: echo_agent_chat_state::ConversationCounts,
    chat_history: Vec<ConversationItem>,
}

struct FlushLockGuard<'a>(&'a super::memory_state::SessionMemory);
impl Drop for FlushLockGuard<'_> {
    fn drop(&mut self) {
        self.0.release_flush_lock();
    }
}

use crate::session::memory::pending::{PendingMemory, recover_pending_jobs};

impl SessionActor {
    /// Persist the work before starting the model call. A quick quit, provider
    /// error or crash leaves a bounded, retryable job in this workspace.
    pub(super) async fn queue_memory_extraction(self: &std::sync::Arc<Self>) {
        if self.memory.jobs.is_stopping() {
            return;
        }
        if let Err(error) = self.checkpoint_memory_extraction().await {
            tracing::warn!(%error, "could not checkpoint memory extraction");
            return;
        }
        let actor = self.clone();
        self.memory.jobs.spawn(async move {
            actor.recover_pending_memories().await;
        });
    }

    /// Local, durable work only. Shutdown can retry this without repeating hooks.
    async fn checkpoint_memory_extraction(&self) -> Result<(), String> {
        use echo_agent_memory::records;
        if self.startup_hints.is_subagent
            || !self.memory.setting("memory", None, "enabled", true)
            || !self
                .memory
                .setting("compaction", Some("memory_flush"), "enabled", true)
        {
            return Ok(());
        }
        let Some(storage) = self.memory.storage() else {
            return Ok(());
        };
        if storage.is_ephemeral()
            || records::source_blocked(&storage, &self.session_info.id.0)
                .map_err(|e| format!("无法核对记忆来源：{e}"))?
        {
            return Ok(());
        }
        let snapshot = self.snapshot_memory_flush_state().await;
        if crate::session::helpers::session_compact::extract_real_user_queries(&snapshot.chat_history).is_empty() {
            return Ok(());
        }
        let job = PendingMemory {
            queued_at: records::now(),
            source_id: self.session_info.id.to_string(),
            conversation_len: snapshot.counts.total,
            history: crate::session::helpers::memory_flush_window::select_flush_window(
                snapshot.chat_history,
                80,
            ),
        };
        if job.history.is_empty() {
            return Ok(());
        }
        // History lengths shrink after compaction and can stay equal after an
        // edit/retry. Only exact completed content is safe to skip.
        if self.memory.jobs.completed(&job.identity().map_err(|e| e.to_string())?) {
            return Ok(());
        }
        crate::session::memory::pending::checkpoint(&storage, &job)
            .map_err(|e| format!("记忆待处理内容未能保存：{e}"))?;
        records::defer_extraction(&storage);
        Ok(())
    }

    pub(super) async fn recover_pending_memories(&self) {
        if self.memory.jobs.is_stopping()
            || self.startup_hints.is_subagent
            || !self.memory.setting("memory", None, "enabled", true)
            || !self
                .memory
                .setting("compaction", Some("memory_flush"), "enabled", true)
        {
            return;
        }
        let Some(storage) = self.memory.storage() else {
            return;
        };
        recover_pending_jobs(
            &storage,
            || {
                self.memory.is_enabled()
                    && !self
                        .memory
                        .is_flushing
                        .load(std::sync::atomic::Ordering::Relaxed)
            },
            |job| async move {
                let identity = job.identity().ok();
                let counts = echo_agent_chat_state::ConversationCounts {
                    total: job.history.len(),
                    user: job
                        .history
                        .iter()
                        .filter(|i| matches!(i, ConversationItem::User(_)))
                        .count(),
                    assistant: job
                        .history
                        .iter()
                        .filter(|i| matches!(i, ConversationItem::Assistant(_)))
                        .count(),
                    tool_result: job
                        .history
                        .iter()
                        .filter(|i| matches!(i, ConversationItem::ToolResult(_)))
                        .count(),
                };
                let completed = self
                    .run_memory_flush_for_source(
                        "task_completed",
                        Some(MemoryFlushSnapshot {
                            counts,
                            chat_history: job.history,
                        }),
                        &job.source_id,
                        Some(job.queued_at),
                    )
                    .await;
                if completed && job.source_id == self.session_info.id.to_string() {
                    if let Some(identity) = identity { self.memory.jobs.mark_completed(identity); }
                    self.last_idle_flush_conversation_len
                        .store(job.conversation_len, std::sync::atomic::Ordering::Relaxed);
                }
                completed
            },
        )
        .await;
    }
}

/// Build first-turn injection backend params without mutating the shared
/// session params.
///
/// This clones the session-wide backend params so tool-search and
/// compaction-recovery backends keep their original `search_source` and search
/// thresholds. The returned effective min score preserves the historical
/// first-turn default of `0.0` unless the injection config explicitly
/// overrides it.
pub(super) fn build_initial_injection_backend_params(
    params: &crate::session::memory::MemoryBackendParams,
    initial_injection_config: &crate::config::MemoryInitialInjectionConfig,
) -> (crate::session::memory::MemoryBackendParams, f64) {
    let mut injection_params = params.clone();
    injection_params.search_source = crate::session::memory::MemorySearchSource::Injection;
    let effective_min_score = initial_injection_config
        .min_score
        .map(|min_score| {
            injection_params.search_config.min_score = min_score;
            min_score as f64
        })
        .unwrap_or(0.0);
    (injection_params, effective_min_score)
}

impl SessionActor {
    pub(super) fn emit_memory_session_summary(
        &self,
        telem: &super::memory_state::MemoryTelemetry,
        total_chunks_at_end: usize,
        session_end_result: &str,
    ) {
        echo_agent_telemetry::session_ctx::log_event(
            echo_agent_telemetry::memory_telemetry::MemorySessionSummary {
                session_id: self.session_info.id.to_string(),
                memory_enabled: self.memory.setting("memory", None, "enabled", true),
                session_duration_secs: self.session_start.elapsed().as_secs(),
                flush_count: telem.flush_count,
                flush_success_count: telem.flush_success_count,
                flush_error_count: telem.flush_error_count,
                tool_search_count: telem.tool_search_count,
                injection_count: telem.injection_count,
                recovery_search_count: telem.compaction_recovery_count,
                total_chunks_at_end,
                chunks_added_this_session: telem.chunks_added as usize,
                session_end_result: session_end_result.to_owned(),
                dream_count: telem.dream_count,
                dream_success_count: telem.dream_success_count,
                dream_error_count: telem.dream_error_count,
            },
        );
    }

    /// Finish local memory writes only. Model extraction, embeddings and dream
    /// are recovered in the next live session, never on the shutdown ACK path.
    pub(super) async fn run_session_end_memory_pipeline(
        &self,
        log_suffix: &str,
    ) -> Result<(), String> {
        if self.startup_hints.is_subagent || !self.memory.setting("memory", None, "enabled", true) {
            return Ok(());
        }
        let save = self.memory.setting(
            "memory",
            Some("session"),
            "save_on_end",
            self.memory.save_on_end,
        );
        let Some(storage) = self.memory.storage() else {
            return Ok(());
        };
        if save {
            self.checkpoint_memory_extraction().await?;
        }
        let conversation = self.chat_state_handle.get_conversation().await;
        let result = crate::session::memory::hooks::on_session_end(
            &storage,
            &conversation,
            &self.session_info.id.0,
            save,
        );
        let outcome = match result {
            crate::session::memory::hooks::SessionEndResult::Written(path) => {
                self.send_echo_agent_notification(EchoAgentSessionUpdate::MemorySessionSaved {
                    path,
                })
                .await;
                "written"
            }
            crate::session::memory::hooks::SessionEndResult::Skipped => "skipped",
            crate::session::memory::hooks::SessionEndResult::Failed(error) => {
                return Err(format!("本地记忆摘要未能保存：{error}"));
            }
        };
        tracing::info!(target: echo_agent_telemetry::memory_log::TARGET, outcome, "MEMORY_SESSION_END: {log_suffix}; network work deferred");
        self.emit_memory_session_summary(
            &self.memory.telemetry_snapshot(),
            storage.total_chunk_count(),
            outcome,
        );
        Ok(())
    }

    /// Reindex a single file and embed any new chunks.
    ///
    /// Used after flush writes and session-end writes to keep the index
    /// and embeddings up to date immediately.
    pub(super) async fn reindex_and_embed(&self, path: &std::path::Path, source: &str) {
        self.memory.reindex_and_embed(path, source).await;
    }

    /// Common setup for dream methods: storage, lock, sessions dir, and truncated session id.
    fn dream_context(
        &self,
    ) -> Option<(
        crate::session::memory::MemoryStorage,
        crate::session::memory::dream_lock::DreamLock,
        std::path::PathBuf,
        String,
    )> {
        let storage = self.memory.storage()?;
        let workspace_dir = storage.workspace_dir();
        let lock = crate::session::memory::dream_lock::DreamLock::new(workspace_dir);
        let sessions_dir = storage.sessions_dir();
        let sid = &self.session_info.id.0;
        let sid8 = sid[..8.min(sid.len())].to_owned();
        Some((storage, lock, sessions_dir, sid8))
    }

    /// Run dream consolidation if gates pass.
    ///
    /// Called at session end after the session summary is written.
    /// Uses the same sampling client infrastructure as flush but sends
    /// the dream prompt instead. The model call has a 60s timeout.
    pub(super) async fn maybe_run_dream(&self) {
        if !self.memory.is_enabled()
            || !self.memory.setting(
                "memory",
                Some("dream"),
                "enabled",
                self.memory.dream_config.enabled,
            )
        {
            return;
        }
        if self.startup_hints.is_subagent {
            tracing::debug!(
                target: echo_agent_telemetry::memory_log::TARGET,
                "MEMORY_SUBAGENT_SKIP: skipping dream for subagent session"
            );
            return;
        }

        use crate::session::memory::dream::*;

        let Some((storage, lock, sessions_dir, sid8)) = self.dream_context() else {
            return;
        };

        let mut live_config = self.memory.dream_config;
        live_config.enabled = true;
        let gate = check_dream_gates(&live_config, &lock, &sessions_dir, Some(&sid8));
        let sessions = match gate {
            DreamGate::Open { sessions } => sessions,
            other => {
                tracing::info!(
                    target: echo_agent_telemetry::memory_log::TARGET,
                    gate = ?other,
                    "MEMORY_DREAM: gate check result, skipping"
                );
                return;
            }
        };

        tracing::info!(
            target: echo_agent_telemetry::memory_log::TARGET,
            session_count = sessions.len(),
            "MEMORY_DREAM: gates passed, starting consolidation"
        );

        let _ = self
            .run_dream_inner(&storage, &lock, &sessions_dir, &sessions, "MEMORY_DREAM")
            .await;
    }

    /// Run dream from `/dream` slash command, bypassing time/session gates.
    pub(super) async fn run_dream_slash_command(&self) -> Result<bool, String> {
        use crate::session::memory::dream_lock::sessions_since;

        if !self.memory.is_enabled() {
            return Err("memory is not enabled for this session".into());
        }
        let Some((storage, lock, sessions_dir, sid8)) = self.dream_context() else {
            return Err("memory storage is not available for this session".into());
        };

        let sessions = match sessions_since(
            &sessions_dir,
            std::time::SystemTime::UNIX_EPOCH,
            Some(&sid8),
        ) {
            Ok(s) if s.is_empty() => {
                tracing::info!(
                    target: echo_agent_telemetry::memory_log::TARGET,
                    "MEMORY_DREAM_SLASH: no session logs found, nothing to consolidate"
                );
                return Ok(false);
            }
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    target: echo_agent_telemetry::memory_log::TARGET,
                    error = %e,
                    "MEMORY_DREAM_SLASH: failed to list sessions"
                );
                return Err(format!("failed to list session summaries: {e}"));
            }
        };

        tracing::info!(
            target: echo_agent_telemetry::memory_log::TARGET,
            session_count = sessions.len(),
            "MEMORY_DREAM_SLASH: starting manual consolidation"
        );

        self.run_dream_inner(
            &storage,
            &lock,
            &sessions_dir,
            &sessions,
            "MEMORY_DREAM_SLASH",
        )
        .await
    }

    /// Shared dream execution: build message, call model, execute, record result.
    async fn run_dream_inner(
        &self,
        storage: &crate::session::memory::MemoryStorage,
        lock: &crate::session::memory::dream_lock::DreamLock,
        sessions_dir: &std::path::Path,
        sessions: &[String],
        log_prefix: &str,
    ) -> Result<bool, String> {
        use crate::session::memory::dream::*;

        if !self.memory.is_enabled() {
            return Ok(false);
        }
        let existing_memory = std::fs::read_to_string(storage.workspace_memory_file()).ok();
        let safe_existing = existing_memory.as_deref().map(|text| {
            text.split("\n\n")
                .filter(|paragraph| {
                    echo_agent_memory::records::permitted(storage, paragraph).unwrap_or(false)
                })
                .collect::<Vec<_>>()
                .join("\n\n")
        });
        let safe_sessions: Vec<_> = sessions
            .iter()
            .filter(|stem| {
                let path = sessions_dir.join(format!("{stem}.md"));
                !echo_agent_memory::records::source_path_blocked(storage, &path).unwrap_or(true)
                    && std::fs::read_to_string(path).is_ok_and(|text| {
                        echo_agent_memory::records::permitted(storage, &text).unwrap_or(false)
                    })
            })
            .cloned()
            .collect();

        let dream_msg = match build_dream_user_message(
            sessions_dir,
            &safe_sessions,
            safe_existing.as_deref(),
        ) {
            Some(msg) => msg,
            None => {
                tracing::info!(
                    target: echo_agent_telemetry::memory_log::TARGET,
                    "{log_prefix}: no readable session content, skipping"
                );
                return Ok(false);
            }
        };

        let model_response = match tokio::time::timeout(
            std::time::Duration::from_secs(30 * 60),
            self.run_dream_model_call(&dream_msg.content),
        )
        .await
        {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                tracing::warn!(
                    target: echo_agent_telemetry::memory_log::TARGET,
                    error = %e,
                    "{log_prefix}: model call failed"
                );
                self.memory.record_dream_result(false);
                return Err(format!("dream model call failed: {e}"));
            }
            Err(_) => {
                tracing::warn!(
                    target: echo_agent_telemetry::memory_log::TARGET,
                    "{log_prefix}: model call timed out (30m)"
                );
                self.memory.record_dream_result(false);
                return Err("dream model call timed out (30m)".into());
            }
        };

        if !self.memory.is_enabled()
            || !echo_agent_memory::records::permitted(storage, &model_response).unwrap_or(false)
        {
            return Ok(false);
        }
        let result = execute_dream_with_expected(
            lock,
            storage,
            &model_response,
            sessions.len(),
            self.memory.dream_config.stale_lock_secs,
            sessions_dir,
            &dream_msg.processed_stems,
            Some(existing_memory.as_deref()),
            Some(&dream_msg.source_hashes),
        );

        match &result.status {
            DreamStatus::Completed { .. } => self.memory.record_dream_result(true),
            DreamStatus::Failed(_) => self.memory.record_dream_result(false),
            _ => self.memory.record_dream_neutral(),
        }

        let dream_path = if matches!(result.status, DreamStatus::Completed { .. }) {
            let path = storage.workspace_memory_file();
            self.memory.reindex_and_embed(&path, "workspace").await;

            // Remove stale index chunks only for session files that
            // were actually deleted — stems skipped by the recency guard
            // are still on disk and must remain searchable.
            if !result.cleaned_stems.is_empty() {
                let deleted_paths: Vec<std::path::PathBuf> = result
                    .cleaned_stems
                    .iter()
                    .map(|stem| sessions_dir.join(format!("{stem}.md")))
                    .collect();
                self.memory.delete_paths_from_index(&deleted_paths);
            }

            Some(path.display().to_string())
        } else {
            None
        };

        let dream_result_str = match &result.status {
            DreamStatus::Completed { chars_written } => format!("written ({chars_written} chars)"),
            DreamStatus::NothingToConsolidate => "nothing to consolidate".into(),
            DreamStatus::Skipped(reason) => format!("skipped: {reason}"),
            DreamStatus::Failed(err) => format!("failed: {err}"),
        };
        self.send_echo_agent_notification(EchoAgentSessionUpdate::MemoryDreamCompleted {
            result: dream_result_str,
            path: dream_path,
        })
        .await;

        tracing::info!(
            target: echo_agent_telemetry::memory_log::TARGET,
            status = ?result.status,
            sessions_eligible = result.sessions_eligible,
            sessions_cleaned = result.cleaned_stems.len(),
            "{log_prefix}: consolidation complete"
        );
        match result.status {
            DreamStatus::Completed { .. } => Ok(true),
            DreamStatus::NothingToConsolidate | DreamStatus::Skipped(_) => Ok(false),
            DreamStatus::Failed(error) => Err(error),
        }
    }

    /// Make the dream model call using the session's sampling client.
    async fn run_dream_model_call(&self, user_message: &str) -> Result<String, acp::Error> {
        let sampling_client = self.prepare_chat_completion(false).await?;
        let model = self
            .chat_state_handle
            .get_sampling_config()
            .await
            .map(|c| c.model)
            .unwrap_or_default();
        let session_id = self.session_info.id.to_string();
        let request = ConversationRequest {
            preflight: self.memory.storage().map(|storage| {
                std::sync::Arc::new(crate::session::memory_state::MemoryRequestPolicy {
                    storage,
                    extraction: true,
                })
                    as std::sync::Arc<dyn echo_agent_sampling_types::ConversationRequestPreflight>
            }),
            items: vec![
                ConversationItem::system(format!(
                    "{}\nCurrent local date: {}",
                    crate::session::memory::dream::DREAM_SYSTEM_PROMPT,
                    chrono::Local::now().to_rfc3339()
                )),
                ConversationItem::user(user_message),
            ],
            model: Some(model),
            x_echo_agent_conv_id: Some(format!("dream-{}", uuid::Uuid::new_v4())),
            x_echo_agent_req_id: Some(format!("echo-agent-dream-{}", uuid::Uuid::new_v4())),
            x_echo_agent_session_id: Some(session_id),
            x_echo_agent_agent_id: Some(echo_agent_telemetry::id::agent_id()),
            ..Default::default()
        };
        let response = sampling_client
            .conversation_collect(request)
            .await
            .map_err(|e| {
                acp::Error::internal_error().data(format!("dream model call failed: {e}"))
            })?;
        Ok(response.assistant_text())
    }

    /// Run a memory flush turn that summarizes recent conversation into a
    /// session log. Sets `is_flushing` to suppress auto-compact during the call.
    ///
    /// Flush failure is non-fatal; compaction proceeds regardless.
    ///
    /// Returns `true` only when the flush completed successfully (including a
    /// valid nothing-to-store/duplicate result). Failures return `false` so
    /// idle and pre-compaction callers retry instead of advancing checkpoints.
    pub(super) async fn run_memory_flush(
        &self,
        trigger: &str,
        snapshot: Option<MemoryFlushSnapshot>,
    ) -> bool {
        self.run_memory_flush_for_source(trigger, snapshot, &self.session_info.id.0, None)
            .await
    }

    pub(super) async fn run_memory_flush_for_source(
        &self,
        trigger: &str,
        snapshot: Option<MemoryFlushSnapshot>,
        source_id: &str,
        source_at: Option<i64>,
    ) -> bool {
        use echo_agent_memory::{flush::*, records};
        let observed_at = source_at
            .filter(|at| *at > 0 && *at <= records::now())
            .unwrap_or_else(records::now);
        if !self.memory.is_enabled()
            || (trigger != "user_requested"
                && trigger != "slash_command"
                && !self
                    .memory
                    .setting("compaction", Some("memory_flush"), "enabled", true))
        {
            return false;
        }
        let Some(storage_for_records) = self.memory.storage() else {
            return false;
        };
        if records::source_blocked(&storage_for_records, source_id).unwrap_or(true) {
            return true;
        }
        // Atomically acquire the flushing lock. If another flush is already
        // running (idle timer, pre-compaction, or user-requested), skip.
        if !self.memory.try_acquire_flush_lock() {
            tracing::info!(
                target: echo_agent_telemetry::memory_log::TARGET,
                "MEMORY_FLUSH: skipped — another flush is already in progress (trigger={trigger})"
            );
            return false;
        }

        let _flush_guard = FlushLockGuard(&self.memory);
        let Ok(Some(_activity_guard)) = records::begin_extraction_activity(&storage_for_records)
        else {
            return false;
        };
        tracing::info!(target: echo_agent_telemetry::memory_log::TARGET, "MEMORY_FLUSH: starting");
        let flush_start = std::time::Instant::now();

        self.send_echo_agent_notification(EchoAgentSessionUpdate::MemoryFlushStarted)
            .await;

        let result = async {
            let sampling_client = self.prepare_chat_completion(false).await?;
            let MemoryFlushSnapshot {
                counts,
                chat_history,
            } = match snapshot {
                Some(snapshot) => snapshot,
                None => self.snapshot_memory_flush_state().await,
            };
            echo_agent_telemetry::session_ctx::log_event(
                echo_agent_telemetry::memory_telemetry::MemoryFlushStart {
                    session_id: self.session_info.id.to_string(),
                    trigger: trigger.to_owned(),
                    conversation_len: counts.total,
                    user_message_count: counts.user,
                },
            );
            tracing::info!(
                target: echo_agent_telemetry::memory_log::TARGET,
                "MEMORY_FLUSH: conversation has {user} user, {assistant} assistant, {tool} tool messages ({total} total)",
                user = counts.user,
                assistant = counts.assistant,
                tool = counts.tool_result,
                total = counts.total,
            );
            let recent = crate::session::helpers::memory_flush_window::select_flush_window(
                chat_history,
                80,
            );

            let system_prompt = format!("{}\nCurrent local date: {}\nExisting memories (manual and pinned entries must be preserved):\n{}",
                FLUSH_SYSTEM_PROMPT, chrono::DateTime::from_timestamp(observed_at, 0).map(|time| time.with_timezone(&chrono::Local).to_rfc3339()).unwrap_or_else(|| chrono::Local::now().to_rfc3339()),
                records::extraction_context(&storage_for_records).unwrap_or_default());
            let mut items: Vec<ConversationItem> = vec![ConversationItem::system(system_prompt)];
            tracing::info!(
                target: echo_agent_telemetry::memory_log::TARGET,
                "MEMORY_FLUSH: sending {n} recent messages to model (+ system prompt + user closer)",
                n = recent.len(),
            );
            let evidence_text = recent.iter().filter_map(|item| super::memory_tools::message_text(item).map(|(_, text)| text)).collect::<Vec<_>>().join("\n");
            items.extend(
                echo_agent_chat_state::compaction_utils::ModelRequestHistory::from_raw(recent).into_items(),
            );
            items.push(ConversationItem::user(
                "Now write the memory summary as described in the system prompt.",
            ));

            let model = match self.memory.flush_config.flush_model.clone() {
                Some(m) => m,
                None => self.chat_state_handle.get_sampling_config().await
                    .map(|c| c.model)
                    .unwrap_or_default(),
            };
            tracing::info!(
                target: echo_agent_telemetry::memory_log::TARGET,
                "MEMORY_FLUSH: using model={model}"
            );
            let session_id = self.session_info.id.to_string();
            let request = ConversationRequest {
                preflight: Some(std::sync::Arc::new(crate::session::memory_state::MemoryRequestPolicy { storage: storage_for_records.clone(), extraction: true })),
                items,
                model: Some(model),
                x_echo_agent_conv_id: Some(format!("flush-{}", uuid::Uuid::new_v4())),
                x_echo_agent_req_id: Some(format!("echo-agent-flush-{}", uuid::Uuid::new_v4())),
                x_echo_agent_session_id: Some(session_id.clone()),
                x_echo_agent_agent_id: Some(echo_agent_telemetry::id::agent_id()),
                ..Default::default()
            };

            // Run on the multi-threaded runtime so it doesn't block the
            // session's LocalSet.
            let handle = tokio::spawn(async move {
                let response = sampling_client
                    .conversation_collect(request)
                    .await
                    .map_err(|e| format!("flush model call failed: {e}"))?;
                let text = response.assistant_text();
                let text = text.trim().strip_prefix("```json").or_else(|| text.trim().strip_prefix("```"))
                    .map(|value| value.trim().strip_suffix("```").unwrap_or(value).trim().to_owned()).unwrap_or(text);
                records::grounded_extraction(&text, &evidence_text).map_err(|error| error.to_string())
            });
            // Abort the spawned task if this future is dropped (session
            // cancellation), preventing orphan HTTP streams.
            struct AbortOnDrop(tokio::task::AbortHandle);
            impl Drop for AbortOnDrop {
                fn drop(&mut self) {
                    self.0.abort();
                }
            }
            let _guard = AbortOnDrop(handle.abort_handle());
            handle
                .await
                .map_err(|e| {
                    acp::Error::internal_error()
                        .data(format!("flush stream task panicked: {e}"))
                })?
                .map_err(|e| acp::Error::internal_error().data(e))
        }
        .await;

        if !self.memory.is_enabled()
            || (trigger != "user_requested"
                && trigger != "slash_command"
                && !self
                    .memory
                    .setting("compaction", Some("memory_flush"), "enabled", true))
            || records::source_blocked(&storage_for_records, source_id).unwrap_or(true)
        {
            return false;
        }
        // (outcome_string, response_length, accepted_length, was_truncated, written_path)
        let (outcome, response_len, accepted_len, was_truncated, flush_path) = match result {
            Ok(response_text) => {
                let resp_len = response_text.len();
                match process_flush_response(&response_text, &self.memory.flush_config) {
                    FlushResult::NothingToStore => {
                        tracing::debug!("memory flush: nothing to store");
                        ("nothing to store".to_string(), resp_len, 0, false, None)
                    }
                    FlushResult::Accepted(content) => {
                        let structured = content.trim_start().starts_with('{');
                        let content = if structured {
                            match records::apply_extraction_at(
                                &storage_for_records,
                                &content,
                                source_id,
                                observed_at,
                            ) {
                                Ok(value) => {
                                    for dir in [
                                        storage_for_records.global_dir(),
                                        storage_for_records.workspace_dir(),
                                    ] {
                                        let path = dir.join(records::RECORD_DOCUMENT);
                                        self.reindex_and_embed(
                                            &path,
                                            storage_for_records.classify_source(&path),
                                        )
                                        .await;
                                    }
                                    value
                                }
                                Err(error) => {
                                    let _ = records::set_activity(
                                        &storage_for_records,
                                        &records::MemoryActivity {
                                            attempted_at: Some(records::now()),
                                            outcome: "failed".into(),
                                            detail: error.to_string(),
                                            ..records::activity(&storage_for_records)
                                        },
                                    );
                                    return false;
                                }
                            }
                        } else {
                            content
                        };
                        let acc_len = content.len();
                        let truncated = acc_len < resp_len;

                        // Semantic dedup: check if this content overlaps with
                        // existing memory chunks before writing.
                        let is_sem_dup = if structured {
                            content.is_empty()
                        } else if let Some(storage) = self.memory.storage() {
                            if let Some(index) = self.memory.open_index(&storage) {
                                let provider = if let Some(ref params) = self.memory.backend_params
                                {
                                    params.make_embedding_provider().await.map(|provider| {
                                        provider.with_request_permit(storage.request_permit())
                                    })
                                } else {
                                    None
                                };
                                let threshold = self
                                    .memory
                                    .flush_config
                                    .semantic_dedup_threshold
                                    .unwrap_or(SEMANTIC_DEDUP_SIMILARITY_THRESHOLD);
                                is_semantically_duplicate(
                                    &content,
                                    &index,
                                    provider.as_ref().map(|p| {
                                        p as &dyn crate::session::memory::embedding::EmbeddingProvider
                                    }),
                                    threshold,
                                )
                                .await
                            } else {
                                false
                            }
                        } else {
                            false
                        };

                        if is_sem_dup {
                            tracing::info!(
                                "memory flush: semantic duplicate detected, skipping write"
                            );
                            (
                                "semantic duplicate".to_string(),
                                resp_len,
                                acc_len,
                                truncated,
                                None,
                            )
                        } else if let Some(storage) = self.memory.storage() {
                            let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
                            let session_id = source_id;
                            match storage
                                .write_daily_log(&date, trigger, session_id, &content, true)
                            {
                                Ok(path) => {
                                    tracing::info!("memory flush wrote session log");
                                    self.reindex_and_embed(&path, "session").await;
                                    *self.memory.last_flush_content.borrow_mut() = Some(content);
                                    (
                                        "written".to_string(),
                                        resp_len,
                                        acc_len,
                                        truncated,
                                        Some(path.display().to_string()),
                                    )
                                }
                                Err(e) => {
                                    tracing::warn!(error = %e, "memory flush write failed");
                                    (
                                        format!("write failed: {e}"),
                                        resp_len,
                                        acc_len,
                                        truncated,
                                        None,
                                    )
                                }
                            }
                        } else {
                            (
                                "storage not configured".to_string(),
                                resp_len,
                                acc_len,
                                truncated,
                                None,
                            )
                        }
                    }
                    FlushResult::Rejected(reason) => {
                        tracing::warn!(reason = %reason, "memory flush response rejected");
                        (format!("rejected: {reason}"), resp_len, 0, false, None)
                    }
                }
            }
            Err(e) => {
                let detail = e
                    .data
                    .as_ref()
                    .and_then(|d| d.as_str())
                    .unwrap_or("memory flush failed");
                tracing::warn!(error = detail, "memory flush failed, skipping");
                (format!("skipped: {detail}"), 0, 0, false, None)
            }
        };

        tracing::info!(target: echo_agent_telemetry::memory_log::TARGET, outcome = %outcome, "MEMORY_FLUSH: completed");
        let flush_outcome = if outcome.starts_with("written") {
            "written"
        } else if outcome.starts_with("nothing") {
            "nothing_to_store"
        } else if outcome.starts_with("rejected") {
            "rejected"
        } else if outcome.starts_with("semantic duplicate") {
            "nothing_to_store"
        } else {
            "error"
        };
        self.memory.record_flush_result(flush_outcome);
        echo_agent_telemetry::session_ctx::log_event(
            echo_agent_telemetry::memory_telemetry::MemoryFlushComplete {
                session_id: self.session_info.id.to_string(),
                trigger: trigger.to_owned(),
                outcome: flush_outcome.to_owned(),
                duration_ms: flush_start.elapsed().as_millis() as u64,
                response_length: response_len,
                accepted_length: accepted_len,
                was_truncated,
            },
        );

        let flush_trigger = match trigger {
            "slash_command" => echo_agent_telemetry::events::MemoryFlushTrigger::SlashCommand,
            "interval" => echo_agent_telemetry::events::MemoryFlushTrigger::Interval,
            "pre_compaction" => echo_agent_telemetry::events::MemoryFlushTrigger::PreCompaction,
            _ => echo_agent_telemetry::events::MemoryFlushTrigger::UserRequested,
        };
        echo_agent_telemetry::session_ctx::log_event(echo_agent_telemetry::events::MemoryFlushed {
            trigger: flush_trigger,
            success: flush_outcome == "written",
            duration_ms: flush_start.elapsed().as_millis() as u64,
            response_length: response_len,
        });

        let completed = matches!(flush_outcome, "written" | "nothing_to_store");
        let current_activity = records::activity(&storage_for_records);
        let _ = records::set_activity(
            &storage_for_records,
            &records::MemoryActivity {
                attempted_at: Some(records::now()),
                succeeded_at: if completed {
                    Some(records::now())
                } else {
                    current_activity.succeeded_at
                },
                outcome: if completed {
                    if flush_outcome == "written" {
                        "saved"
                    } else {
                        "unchanged"
                    }
                } else {
                    "failed"
                }
                .into(),
                detail: if completed {
                    String::new()
                } else {
                    outcome.clone()
                },
                records_written: current_activity.records_written,
            },
        );
        self.send_echo_agent_notification(EchoAgentSessionUpdate::MemoryFlushCompleted {
            result: outcome,
            path: flush_path,
        })
        .await;
        completed
    }

    /// Capture the flush inputs before compaction mutates conversation history.
    pub(super) async fn snapshot_memory_flush_state(&self) -> MemoryFlushSnapshot {
        let (counts, conversation) = tokio::join!(
            self.chat_state_handle.get_conversation_counts(),
            self.chat_state_handle.get_conversation(),
        );
        let chat_history =
            echo_agent_chat_state::compaction_utils::prepare_conversation_for_summarization(
                conversation,
            );
        MemoryFlushSnapshot {
            counts,
            chat_history,
        }
    }

    /// Rewrite a raw memory note into well-structured markdown via a one-shot
    /// LLM call using the `echo-agent-build` model.
    ///
    /// Follows the same streaming pattern as [`handle_ai_suggest`]: prepares
    /// a sampling client, builds a system+user prompt, streams the response,
    /// and returns the collected text.
    pub(super) async fn handle_rewrite_memory_note(
        &self,
        raw_text: &str,
        context_summary: &str,
    ) -> Result<String, String> {
        // Upper-bound check to prevent unbounded LLM input.
        const MAX_INPUT_BYTES: usize = 32 * 1024; // 32 KB
        let combined_len = raw_text.len() + context_summary.len();
        if combined_len > MAX_INPUT_BYTES {
            return Err(format!(
                "memory note input too large ({combined_len} bytes, max {MAX_INPUT_BYTES})"
            ));
        }

        let sampling_client = self
            .prepare_chat_completion(false)
            .await
            .map_err(|e| format!("failed to prepare client: {e}"))?;

        let system = "You are a memory note formatter. Rewrite the user's note into \
            well-structured markdown suitable for a persistent MEMORY.md file. The note should be:\n\
            - Concise but complete\n\
            - Start with a descriptive ## heading\n\
            - Include enough context to be useful months later\n\
            - Reference specific files, decisions, or patterns when relevant\n\
            - Use bullet points for multiple items\n\
            - Do NOT include timestamps or session IDs\n\
            - Do NOT add information that is not present in the original note\n\n\
            Return ONLY the formatted markdown, no explanations.";

        let user_msg = format!(
            "Session context:\n{context_summary}\n\nRewrite this note as a memory entry:\n\n{raw_text}"
        );

        let items = vec![
            ConversationItem::system(system.to_owned()),
            ConversationItem::user(user_msg),
        ];

        let request = ConversationRequest {
            items,
            tools: vec![],
            model: Some("echo-agent-build".to_owned()),
            temperature: Some(0.3),
            max_output_tokens: Some(1024),
            ..Default::default()
        };

        let request_id = echo_agent_sampler::RequestId::random();
        let idle_timeout = std::time::Duration::from_secs(15);

        let result = match sampling_client.api_backend() {
            crate::sampling::ApiBackend::ChatCompletions => {
                let (raw, meta) = sampling_client
                    .conversation_stream(request)
                    .await
                    .map_err(|e| format!("rewrite stream failed: {e}"))?;
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
                    .map_err(|e| format!("rewrite stream failed: {e}"))?;
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
                    .map_err(|e| format!("rewrite stream failed: {e}"))?;
                let events =
                    echo_agent_sampler::stream_messages(raw, meta, request_id, idle_timeout);
                echo_agent_sampler::collect_response(events).await
            }
        };

        match result {
            Ok((response, _metrics)) => {
                let text = response.assistant_text();
                if text.is_empty() {
                    Err("LLM returned empty response".to_string())
                } else {
                    Ok(text)
                }
            }
            Err(e) => {
                tracing::debug!(error = %e.message, "memory note rewrite inference failed");
                Err(format!("rewrite inference failed: {}", e.message))
            }
        }
    }
}
