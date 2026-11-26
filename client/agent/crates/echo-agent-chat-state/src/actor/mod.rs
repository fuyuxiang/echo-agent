//! ChatStateActor — runs in a dedicated tokio task and owns all chat state.
//!
//! This module is organized into submodules by responsibility:
//! - `state`: Internal state types (ChatState)
//! - `mutations`: State mutation handlers (push_user_message, replace_conversation, etc.)
//! - `queries`: Read-only query handlers (get_conversation, snapshot, etc.)

mod mutations;
mod queries;
pub(crate) mod request_builder;
pub mod state;

#[cfg(test)]
mod tests;

use tokio::sync::mpsc;
use tracing::debug;

use crate::commands::{ChatStateCommand, StrictAppendAck};
use crate::events::ChatStateEvent;
use crate::handle::ChatStateHandle;
use crate::persistence::ChatPersistence;
use crate::types::{PruningConfig, TurnCapture};

use echo_agent_sampling_types::{ConversationItem, SamplingConfig};
use state::ChatState;

/// The actor that owns all chat state.
/// Runs in a dedicated tokio task and processes commands sequentially.
pub struct ChatStateActor {
    /// Internal state — conversation, tokens, config, etc.
    state: ChatState,
    /// Pruning configuration for tool-result trimming.
    pruning_config: PruningConfig,
    /// Persistence implementation — owned exclusively, called with `&mut self`.
    persistence: Box<dyn ChatPersistence>,
    /// Channel to receive commands from handles.
    cmd_rx: mpsc::UnboundedReceiver<ChatStateCommand>,
    /// Channel to send events to the session main loop.
    event_tx: mpsc::UnboundedSender<ChatStateEvent>,
    /// Cancellation token for graceful shutdown.
    cancellation_token: tokio_util::sync::CancellationToken,
}

impl ChatStateActor {
    /// Send an event to subscribers, logging if the channel is closed.
    fn send_event(&self, event: ChatStateEvent) {
        if self.event_tx.send(event).is_err() {
            debug!("ChatState event channel closed, event dropped");
        }
    }

    /// Spawn the actor and return a handle to communicate with it.
    pub fn spawn(
        initial_conversation: Vec<ConversationItem>,
        sampling_config: SamplingConfig,
        persistence: Box<dyn ChatPersistence>,
        event_tx: mpsc::UnboundedSender<ChatStateEvent>,
        cancellation_token: tokio_util::sync::CancellationToken,
    ) -> ChatStateHandle {
        Self::spawn_with_pruning(
            initial_conversation,
            sampling_config,
            PruningConfig::default(),
            persistence,
            event_tx,
            cancellation_token,
        )
    }

    /// Spawn the actor with a custom pruning config.
    pub fn spawn_with_pruning(
        initial_conversation: Vec<ConversationItem>,
        sampling_config: SamplingConfig,
        pruning_config: PruningConfig,
        persistence: Box<dyn ChatPersistence>,
        event_tx: mpsc::UnboundedSender<ChatStateEvent>,
        cancellation_token: tokio_util::sync::CancellationToken,
    ) -> ChatStateHandle {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();

        let actor = ChatStateActor {
            state: ChatState::new(initial_conversation, sampling_config),
            pruning_config,
            persistence,
            cmd_rx,
            event_tx,
            cancellation_token,
        };

        tokio::spawn(actor.run());

        ChatStateHandle::new(cmd_tx)
    }

    /// Main actor loop — processes commands until shutdown or cancellation.
    async fn run(mut self) {
        loop {
            tokio::select! {
                biased;
                _ = self.cancellation_token.cancelled() => {
                    debug!("ChatStateActor shutting down via cancellation");
                    break;
                }
                cmd = self.cmd_rx.recv() => {
                    let Some(cmd) = cmd else {
                        debug!("ChatStateActor shutting down: all handles dropped");
                        break;
                    };
                    self.handle_command(cmd).await;
                }
            }
        }
    }

    /// Dispatch a command to the appropriate mutation or query handler.
    async fn handle_command(&mut self, cmd: ChatStateCommand) {
        match cmd {
            // ═══ Mutations ═══
            ChatStateCommand::PushUserMessage { item } => {
                self.push_user_message(item);
            }
            ChatStateCommand::PushUserMessageAndAck { item, reply } => {
                self.push_user_message(item);
                let _ = reply.send(());
            }
            ChatStateCommand::AppendWorkingDirectorySwitchAndAck {
                content,
                cwd_generation,
                reply,
            } => {
                let generation = cwd_generation.get();
                let candidate = ConversationItem::working_directory_switch(content, generation);
                let persist_rx = self
                    .persistence
                    .persist_working_directory_switch_and_ack(&candidate);
                let result = persist_rx.await.unwrap_or_else(|_| {
                    Err(crate::commands::StrictAppendError::Indeterminate(
                        std::io::Error::new(
                            std::io::ErrorKind::BrokenPipe,
                            "working-directory switch acknowledgement dropped; retry by generation",
                        ),
                    ))
                });
                let authoritative = match &result {
                    Ok(StrictAppendAck::Appended)
                    | Err(crate::commands::StrictAppendError::Committed {
                        acknowledgement: StrictAppendAck::Appended,
                        ..
                    }) => Some(&candidate),
                    Ok(StrictAppendAck::AlreadyPresent(authoritative))
                    | Err(crate::commands::StrictAppendError::Committed {
                        acknowledgement: StrictAppendAck::AlreadyPresent(authoritative),
                        ..
                    }) => Some(authoritative),
                    Err(_) => None,
                };
                if let Some(authoritative) = authoritative {
                    self.converge_working_directory_switch(generation, authoritative.clone());
                }
                let _ = reply.send(result);
            }
            ChatStateCommand::PushUserMessageWithRepairReason { item, reason } => {
                self.push_user_message_with_repair_reason(item, reason);
            }
            ChatStateCommand::PushAssistantResponse { item } => {
                self.push_message(item);
            }
            ChatStateCommand::PushToolResult { item } => {
                self.push_message(item);
            }
            ChatStateCommand::PushModelOutput { item } => {
                self.push_model_output(item);
            }
            ChatStateCommand::PushUnreportedModelOutput { item } => {
                self.push_unreported_model_output(item);
            }
            ChatStateCommand::RecordTokenUsage { total_tokens } => {
                self.record_token_usage(total_tokens);
            }
            ChatStateCommand::RecordLastTurnUsage { usage } => {
                self.record_last_turn_usage(usage);
            }
            ChatStateCommand::RecordModelCallUsage {
                model_id,
                usage,
                api_duration_ms,
                cost_usd_ticks,
            } => {
                self.record_model_call_usage(model_id, &usage, api_duration_ms, cost_usd_ticks);
            }
            ChatStateCommand::RecordSubagentUsage {
                by_model,
                attribute_to_prompt,
                incomplete,
                reply,
            } => {
                self.record_subagent_usage(&by_model, attribute_to_prompt, incomplete);
                let _ = reply.send(());
            }
            ChatStateCommand::MarkUsageIncomplete {
                prompt,
                session,
                reply,
            } => {
                self.mark_usage_incomplete(prompt, session);
                let _ = reply.send(());
            }
            ChatStateCommand::IncrementPromptIndex => {
                self.increment_prompt_index();
            }
            ChatStateCommand::UpdateSamplingConfig { config } => {
                self.state.sampling_config = config;
            }
            ChatStateCommand::RecordAgentEditedPath { path } => {
                self.state.agent_edited_paths.insert(path);
            }
            ChatStateCommand::RecordStreamStart { timestamp_ms } => {
                self.state.stream_start_ms = Some(timestamp_ms);
            }
            ChatStateCommand::RecordTurnStart { timestamp_ms } => {
                self.state.turn_start_ms = Some(timestamp_ms);
            }
            ChatStateCommand::ReplaceConversation {
                items,
                is_compaction,
            } => {
                self.replace_conversation(items, is_compaction);
            }
            ChatStateCommand::RepairHistory {
                dry_run,
                turn_active,
                reply,
            } => {
                // Checked here so refusal and mutation are serialized; a
                // `false` at processing time means pre-turn state (see the
                // command's doc).
                let blocked = turn_active
                    .as_ref()
                    .map(|f| f.load(std::sync::atomic::Ordering::SeqCst))
                    .unwrap_or(false);
                let result = if blocked {
                    Err(crate::commands::RepairHistoryBlocked)
                } else {
                    Ok(self.repair_history(dry_run))
                };
                let _ = reply.send(result);
            }
            ChatStateCommand::StripConversationImages { urls, reply } => {
                match self.strip_conversation_images(&urls) {
                    None => {
                        let _ = reply.send(crate::StripOutcome::NoMatch);
                    }
                    Some((stripped, ack_rx)) => {
                        // Await the disk ack off-actor: the persistence
                        // channel already orders the write against later
                        // commands, and blocking here would stall reads
                        // behind an fsync.
                        tokio::spawn(async move {
                            let outcome = match ack_rx.await {
                                Ok(Ok(())) => crate::StripOutcome::Applied { stripped },
                                Ok(Err(_)) | Err(_) => {
                                    crate::StripOutcome::WriteFailed { stripped }
                                }
                            };
                            let _ = reply.send(outcome);
                        });
                    }
                }
            }
            ChatStateCommand::ReplaceSystemHead { prompt, reply } => {
                let changed = self.replace_system_head(&prompt);
                let _ = reply.send(changed);
            }
            ChatStateCommand::CachePromptText { text } => {
                self.state.prompt_texts.push(text);
            }
            ChatStateCommand::RecordCompactionAt { prompt_index } => {
                self.state.last_compaction_prompt_index = Some(prompt_index);
            }
            ChatStateCommand::Flush => {
                self.persistence.flush();
            }
            ChatStateCommand::UpdateCredentials { credentials } => {
                self.state.credentials = credentials;
            }
            ChatStateCommand::RestoreSnapshot(snapshot) => {
                self.restore_snapshot(*snapshot);
            }
            ChatStateCommand::BeginTurnCapture => {
                self.state.turn_capture = Some(state::TurnCaptureState {
                    turn_start_offset: self.state.conversation.len(),
                    pre_replacement_messages: Vec::new(),
                    compaction_occurred: false,
                });
            }
            ChatStateCommand::AppendHarnessTraceItems { items } => {
                self.state.harness_trace_buffer.extend(items);
            }
            ChatStateCommand::FlushHarnessTraceTurn => {
                self.state.seal_harness_trace_turn();
            }
            ChatStateCommand::RepairDanglingAfterHarnessHalt { class } => {
                self.repair_dangling_after_harness_halt(class);
            }

            // ═══ Queries ═══
            //
            // Read queries are pure reads — repair only at write boundaries:
            // `ChatState::new()` (startup) and `push_user_message()` (new turn).
            // `BuildConversationRequest` retains the guard because it is only
            // ever issued by the agent loop between turns, never by background tasks.
            ChatStateCommand::BuildConversationRequest {
                tool_definitions,
                memory_reminder,
                persist_memory_reminder,
                trace,
                conv_id,
                req_id,
                reply,
            } => {
                self.ensure_conversation_integrity();
                let request = self.build_conversation_request(
                    tool_definitions,
                    memory_reminder,
                    persist_memory_reminder,
                    trace,
                    conv_id,
                    req_id,
                );
                let _ = reply.send(request);
            }
            ChatStateCommand::GetConversation { reply } => {
                tracing::debug!(
                    conversation_len = self.state.conversation.len(),
                    "ChatState: cloning full conversation for GetConversation"
                );
                let _ = reply.send(self.state.conversation.clone());
            }
            ChatStateCommand::GetPromptIndex { reply } => {
                let _ = reply.send(self.state.prompt_index);
            }
            ChatStateCommand::GetLastCompactionPromptIndex { reply } => {
                let _ = reply.send(self.state.last_compaction_prompt_index);
            }
            ChatStateCommand::GetTotalTokens { reply } => {
                let _ = reply.send(self.state.total_tokens);
            }
            ChatStateCommand::GetLastTurnUsage { reply } => {
                let _ = reply.send(self.state.last_turn_usage.clone());
            }
            ChatStateCommand::GetPromptUsage { reply } => {
                let _ = reply.send(self.state.prompt_usage.clone());
            }
            ChatStateCommand::GetSessionUsage { reply } => {
                let _ = reply.send(self.state.session_usage.clone());
            }
            ChatStateCommand::GetEstimatedTotalTokens { reply } => {
                let _ =
                    reply.send(self.state.total_tokens + self.state.estimated_tokens_since_model);
            }
            ChatStateCommand::GetSamplingConfig { reply } => {
                let _ = reply.send(self.state.sampling_config.clone());
            }
            ChatStateCommand::GetAgentEditedPaths { reply } => {
                let _ = reply.send(self.state.agent_edited_paths.clone());
            }
            ChatStateCommand::GetNotificationMeta { reply } => {
                let _ = reply.send(self.get_notification_meta());
            }
            ChatStateCommand::Snapshot { reply } => {
                tracing::debug!(
                    conversation_len = self.state.conversation.len(),
                    "ChatState: cloning full state for Snapshot"
                );
                let _ = reply.send(self.snapshot());
            }
            ChatStateCommand::TruncateToPromptIndex {
                target_prompt_index,
                reply,
            } => {
                self.truncate_to_prompt_index(target_prompt_index);
                self.state.turn_capture = None;
                self.state.prompt_usage = None;
                // `harness_trace_buffer` / `harness_trace_turns` intentionally
                // survive a rewind: the goal planner / verifier subagents
                // genuinely ran, so their sealed trace turns stay uploadable as
                // siblings even when the live turn that triggered them is undone.
                let _ = reply.send(());
            }
            ChatStateCommand::CheckAutoCompactNeeded {
                threshold_percent,
                reply,
            } => {
                let _ = reply.send(self.check_auto_compact_needed(threshold_percent));
            }
            ChatStateCommand::GetCredentials { reply } => {
                let _ = reply.send(self.state.credentials.clone());
            }
            ChatStateCommand::GetLastModelMetadata { reply } => {
                let _ = reply.send(self.get_last_model_metadata());
            }
            ChatStateCommand::TakeTurnMessages { reply } => {
                let result = self.state.turn_capture.take().map(|cap| {
                    let mut messages = cap.pre_replacement_messages;
                    messages.extend(
                        Self::turn_tail(&self.state.conversation, cap.turn_start_offset)
                            .iter()
                            .cloned(),
                    );
                    TurnCapture {
                        messages,
                        compaction_occurred: cap.compaction_occurred,
                    }
                });
                let _ = reply.send(result);
            }
            ChatStateCommand::TakeHarnessTraceTurns { reply } => {
                // Defensive seal: a phase that recorded items but never flushed
                // still rides its own turn rather than stranding.
                self.state.seal_harness_trace_turn();
                let _ = reply.send(std::mem::take(&mut self.state.harness_trace_turns));
            }

            // ─── Narrow targeted queries ──────────────────────────────────
            ChatStateCommand::GetConversationLen { reply } => {
                let _ = reply.send(self.get_conversation_len());
            }
            ChatStateCommand::HasDanglingToolCalls { reply } => {
                let _ = reply.send(self.has_dangling_tool_calls());
            }
            ChatStateCommand::GetLastAssistantText { reply } => {
                let _ = reply.send(self.get_last_assistant_text());
            }
            ChatStateCommand::GetLastAssistantTextInTurn { reply } => {
                let _ = reply.send(self.get_last_assistant_text_in_turn());
            }
            ChatStateCommand::GetFirstUserText { reply } => {
                let _ = reply.send(self.get_first_user_text());
            }
            ChatStateCommand::GetConversationItemAt { index, reply } => {
                let _ = reply.send(self.get_conversation_item_at(index));
            }
            ChatStateCommand::GetLastUserQueryText { reply } => {
                let _ = reply.send(self.get_last_user_query_text());
            }
            ChatStateCommand::GetConversationCounts { reply } => {
                let _ = reply.send(self.get_conversation_counts());
            }
            ChatStateCommand::GetSystemMessage { reply } => {
                let _ = reply.send(self.get_system_message());
            }
            ChatStateCommand::GetEstimatedMessagesTokens { reply } => {
                let _ = reply.send(state::estimate_messages_tokens(&self.state.conversation));
            }
        }
    }
}
