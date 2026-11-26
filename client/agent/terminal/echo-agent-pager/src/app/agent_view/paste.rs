//! Paste routing: bracketed paste, clipboard attachment probe, dropped
//! paths, image paste, and the deferred send-after-paste flow.
#[cfg(test)]
use super::{ActivePane, AgentViewLayout, PromptInputMode, render_dropdown_chrome};
use super::{AgentDeferredSend, AgentView};
use crate::actions::ActionRegistry;
use crate::app::actions::Action;
use crate::app::app_view::InputOutcome;
#[cfg(test)]
use crate::key;
#[cfg(test)]
use crate::theme::Theme;
use crate::views::prompt_widget::{PromptEvent, PromptWidget};
#[cfg(test)]
use crossterm::event::{Event, KeyEvent};
impl AgentView {
    /// Insert a plain-text (caption) clipboard paste into the prompt, matching
    /// the bracketed arm's whitespace policy + slash/suggestion refresh. The
    /// image/file-url portion of a paste is handled by the deferred probe.
    fn insert_prompt_plain_text(
        &mut self,
        clipboard_text: Option<&str>,
    ) -> (InputOutcome, crate::app::actions::ClipboardTextInsertion) {
        self.insert_prompt_text(clipboard_text, false)
    }
    pub(super) fn insert_bracketed_prompt_text(
        &mut self,
        text: &str,
    ) -> (InputOutcome, crate::app::actions::ClipboardTextInsertion) {
        self.insert_prompt_text(Some(text), true)
    }
    fn insert_prompt_text(
        &mut self,
        clipboard_text: Option<&str>,
        activate_bash: bool,
    ) -> (InputOutcome, crate::app::actions::ClipboardTextInsertion) {
        use crate::app::actions::ClipboardTextInsertion;
        let Some(text) = clipboard_text else {
            return (InputOutcome::Changed, ClipboardTextInsertion::Empty);
        };
        if text.trim().is_empty() {
            return (InputOutcome::Unchanged, ClipboardTextInsertion::Empty);
        }
        let paste_text = if activate_bash
            && self.prompt_input_mode == super::PromptInputMode::Normal
            && self.prompt.text().is_empty()
        {
            if let Some(command) = text.strip_prefix("! ") {
                self.prompt_input_mode = super::PromptInputMode::Bash;
                command
            } else {
                text
            }
        } else {
            text
        };
        match self.prompt.handle_paste(paste_text) {
            PromptEvent::Edited => {
                self.prompt.refresh_slash(&self.session.models);
                if let Some(eff) = self.notify_suggestion_text_changed() {
                    self.pending_effects.push(eff);
                }
                if let Some(eff) = self.notify_plugin_cta_text_changed() {
                    self.pending_effects.push(eff);
                }
                (InputOutcome::Changed, ClipboardTextInsertion::Inserted)
            }
            PromptEvent::Ignored => (InputOutcome::Changed, ClipboardTextInsertion::Failed),
        }
    }
    fn reject_shared_queue_image_edit(
        &mut self,
        pasted: &crate::prompt_images::PastedImage,
    ) -> bool {
        if !matches!(
            self.prompt_mode,
            crate::app::queue_edit::PromptMode::EditingQueued {
                server_id: Some(_),
                ..
            }
        ) {
            return false;
        }
        crate::prompt_images::cleanup_temp_file(pasted);
        self.show_toast("Images can't be attached when editing a shared queued prompt");
        true
    }
    /// Enqueue attachment probing off-thread so paste-then-send remains ordered.
    pub(super) fn enqueue_clipboard_attachment_probe(
        &mut self,
        source: crate::app::actions::ClipboardPasteSource,
        change_count: Option<u64>,
    ) {
        let images_dir = crate::prompt_images::session_images_dir(
            self.session.session_id.as_ref(),
            &self.session.cwd,
        );
        self.paste_probe_in_flight += 1;
        let from_feedback_pane = self
            .question_view
            .as_ref()
            .is_some_and(crate::views::question_view::QuestionViewState::is_feedback_report);
        self.pending_effects
            .push(crate::app::actions::Effect::ProbeClipboardAttachment {
                ctx: crate::app::actions::ClipboardPasteContext {
                    target: crate::app::actions::ClipboardPasteTarget::AgentPrompt {
                        agent_id: self.session.id,
                        images_dir,
                        from_feedback_pane,
                    },
                    source,
                },
                change_count,
            });
    }
    /// Ctrl/Cmd+V paste: a file path in the text resolves synchronously and
    /// wins; else the clipboard raster/file-url probe defers off the event loop
    /// (image wins over the caption, inserted on completion only if no image);
    /// else plain text with no raster inserts synchronously.
    pub(super) fn handle_paste_key_deferred(
        &mut self,
        clipboard_text: crate::app::actions::ClipboardTextRead,
    ) -> InputOutcome {
        let tip_showing = self.ephemeral_tip.current_key()
            == Some(crate::tips::clipboard_focus::CLIPBOARD_IMAGE_TIP_KEY);
        self.ephemeral_tip
            .clear(crate::tips::clipboard_focus::CLIPBOARD_IMAGE_TIP_KEY);
        if let Some(text) = clipboard_text.as_deref()
            && let Some((outcome, _)) = self.try_handle_dropped_paths_paste(text)
        {
            return outcome;
        }
        if let Some(change_count) =
            crate::clipboard::attachment_probe_gate(clipboard_text.as_deref())
        {
            self.enqueue_clipboard_attachment_probe(
                crate::app::actions::ClipboardPasteSource::ClipboardKey {
                    text: clipboard_text,
                    tip_showing,
                },
                change_count,
            );
            return InputOutcome::Changed;
        }
        self.insert_prompt_plain_text(clipboard_text.as_deref()).0
    }
    /// Attach the result of a deferred clipboard attachment probe
    /// ([`Effect::ProbeClipboardAttachment`]). The heavy read/decode/persist
    /// already ran off-thread; this only mutates prompt state on the event loop.
    pub(crate) fn complete_clipboard_attachment_paste(
        &mut self,
        ctx: crate::app::actions::ClipboardPasteContext,
        image: crate::app::actions::ProbedAttachment,
        file_urls: Option<String>,
    ) -> crate::app::actions::ClipboardPasteCompletion {
        use crate::app::actions::{
            ClipboardPasteCompletion, ClipboardPasteFailure, ProbedAttachment,
        };
        self.paste_probe_in_flight = self.paste_probe_in_flight.saturating_sub(1);
        if matches!(
            &ctx.target,
            crate::app::actions::ClipboardPasteTarget::AgentPrompt {
                from_feedback_pane: true,
                ..
            }
        ) && !self
            .question_view
            .as_ref()
            .is_some_and(crate::views::question_view::QuestionViewState::is_feedback_report)
        {
            if let ProbedAttachment::Image(pasted) = &image {
                crate::prompt_images::cleanup_temp_file(pasted);
            }
            return ClipboardPasteCompletion::Dropped;
        }
        let insert_deferred_text = matches!(
            &image,
            ProbedAttachment::NoRaster
                | ProbedAttachment::ProbeDropped
                | ProbedAttachment::ProbeFailed
        );
        let attachment = match image {
            ProbedAttachment::Image(pasted) => {
                if self.reject_shared_queue_image_edit(&pasted) {
                    return ClipboardPasteCompletion::Failed(
                        ClipboardPasteFailure::AlreadyReported,
                    );
                }
                let preparation = pasted.preview_preparation();
                if let Err(msg) = self.prompt.insert_image(pasted) {
                    self.show_toast_ticks(&msg, 150);
                    ClipboardPasteCompletion::Failed(ClipboardPasteFailure::AlreadyReported)
                } else {
                    if let Some(preparation) = preparation {
                        self.pending_effects.push(
                            crate::app::actions::Effect::PreparePromptImagePreview { preparation },
                        );
                    }
                    if ctx.source.tip_showing() {
                        echo_agent_telemetry::session_ctx::log_event(
                            echo_agent_telemetry::events::ContextualTip {
                                tip: echo_agent_telemetry::events::ContextualTipKind::ImageInput,
                                action: echo_agent_telemetry::events::ContextualTipAction::Accepted,
                            },
                        );
                    }
                    self.prompt.refresh_slash(&self.session.models);
                    ClipboardPasteCompletion::Handled
                }
            }
            ProbedAttachment::PersistFailed(_) => {
                self.show_toast("Couldn't save pasted image");
                ClipboardPasteCompletion::Failed(ClipboardPasteFailure::AlreadyReported)
            }
            ProbedAttachment::NoRaster => ClipboardPasteCompletion::FullMiss,
            ProbedAttachment::ProbeDropped => ClipboardPasteCompletion::Dropped,
            ProbedAttachment::ProbeFailed => {
                ClipboardPasteCompletion::Failed(ClipboardPasteFailure::AttachmentRead)
            }
        };
        let file = if attachment == ClipboardPasteCompletion::FullMiss {
            file_urls.as_deref().and_then(|urls| {
                self.try_handle_dropped_paths_paste(urls)
                    .map(|(_, completion)| completion)
            })
        } else {
            None
        };
        let text = if insert_deferred_text {
            ctx.source
                .text_to_insert_on_miss()
                .filter(|text| !text.trim().is_empty())
                .map(|text| self.insert_prompt_plain_text(Some(text)).1)
        } else {
            None
        };
        let completion = crate::app::actions::reduce_clipboard_paste_completion(
            &ctx.source,
            attachment,
            file,
            text,
        );
        if completion == ClipboardPasteCompletion::FullMiss && ctx.source.is_clipboard_key() {
            crate::clipboard::log_paste_key_empty_host_clipboard(ctx.target.surface_str());
        }
        completion
    }
    /// Take the kind of any action held back while the paste probes were in flight.
    /// The caller resumes it (via [`Self::resume_deferred_send`]) only when it actually reissues, so a dropped reissue keeps the draft intact.
    pub(crate) fn take_deferred_send_after_paste(&mut self) -> Option<AgentDeferredSend> {
        if self.paste_probe_in_flight != 0 {
            return None;
        }
        self.deferred_send.take()
    }
    /// Resume a drained deferred action, re-deriving the payload from the now-updated prompt so the freshly attached image chip (and its
    /// aligned range) travels with it. Call only when actually reissuing: the interject variant consumes the draft.
    pub(crate) fn resume_deferred_send(&mut self, kind: AgentDeferredSend) -> Option<Action> {
        match kind {
            AgentDeferredSend::SendPrompt => {
                let text = self.prompt.text().to_string();
                (!text.trim().is_empty()).then_some(Action::SendPrompt(text))
            }
            AgentDeferredSend::Interject => {
                let text = self.prompt.text().trim().to_string();
                if !ActionRegistry::interjection_possible(
                    self.session.state.is_turn_running(),
                    !text.is_empty(),
                ) {
                    return None;
                }
                let images = self.prompt.drain_images();
                self.prompt.set_text("");
                self.note_draft_consumed();
                Some(Action::SendPromptNow { text, images })
            }
            AgentDeferredSend::SubmitFeedback => {
                if !self
                    .question_view
                    .as_ref()
                    .is_some_and(crate::views::question_view::QuestionViewState::is_feedback_report)
                {
                    return None;
                }
                match self.submit_question_answers(false) {
                    crate::app::app_view::InputOutcome::Action(action) => Some(action),
                    _ => None,
                }
            }
            AgentDeferredSend::Stash => {
                self.handle_stash_prompt_key();
                None
            }
        }
    }
    /// Consume wrap host-image magic paste (`Some` = handled, never as text).
    pub(super) fn try_handle_wrap_host_image_paste(&mut self, text: &str) -> Option<InputOutcome> {
        let wrap = crate::wrap_clipboard_image::try_decode_wrap_host_image_paste(text)?;
        Some(match wrap {
            crate::wrap_clipboard_image::WrapImagePaste::Image(data) => {
                let pasted = crate::prompt_images::from_clipboard_data(&data);
                let _ = self.handle_image_paste_from_data(pasted);
                self.prompt.refresh_slash(&self.session.models);
                InputOutcome::Changed
            }
            crate::wrap_clipboard_image::WrapImagePaste::NoImage => InputOutcome::Unchanged,
        })
    }
    /// Parse a paste payload as one or more drop-style file paths and
    /// route each entry: image paths become `[Image #N]` chips, non-image
    /// paths get inserted as decoded absolute path text.
    ///
    /// Route a popup pane's `Event::Paste(text)` through the drop
    /// classifier and fall back to a plain text paste into the shared
    /// prompt buffer. Used by the plan-feedback, permission-followup,
    /// plan-approval, and question-view paste arms — all of which share
    /// the same prompt widget as the main Prompt pane and need identical
    /// classifier semantics.
    pub(super) fn route_popup_paste(&mut self, text: &str) -> InputOutcome {
        if let Some((outcome, _)) = self.try_handle_dropped_paths_paste(text) {
            return outcome;
        }
        let _ = self.prompt.handle_paste(text);
        InputOutcome::Changed
    }
    /// The feedback report pane mirrors the composer's bracketed-paste
    /// attachment probe (terminals that deliver Cmd+V as `Event::Paste`
    /// never hit the key path); every other question view stays text-only,
    /// including the trace-consent stage (same invariant as the key path:
    /// a paste there would land in the hidden prompt and never reach the
    /// committed report).
    pub(super) fn route_question_paste(&mut self, text: &str) -> InputOutcome {
        if !self
            .question_view
            .as_ref()
            .is_some_and(crate::views::question_view::QuestionViewState::is_feedback_report)
        {
            return self.route_popup_paste(text);
        }
        if let Some((outcome, _)) = self.try_handle_dropped_paths_paste(text) {
            return outcome;
        }
        self.probe_attachment_around_bracketed_insert(text, |view| {
            let insertion = match view.prompt.handle_paste(text) {
                PromptEvent::Edited => crate::app::actions::ClipboardTextInsertion::Inserted,
                PromptEvent::Ignored => crate::app::actions::ClipboardTextInsertion::Failed,
            };
            (InputOutcome::Changed, insertion)
        })
    }
    /// The bracketed-paste attachment-probe protocol, shared by the composer
    /// arm and the feedback pane: snapshot the clipboard gate BEFORE the text
    /// insertion, insert, then enqueue the off-thread probe carrying the
    /// insertion outcome. Owns both cfg arms so the platform conditioning
    /// cannot drift between call sites.
    pub(super) fn probe_attachment_around_bracketed_insert<R>(
        &mut self,
        text: &str,
        insert: impl FnOnce(&mut Self) -> (R, crate::app::actions::ClipboardTextInsertion),
    ) -> R {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        let change_count = if super::bracketed_paste_should_probe(text) {
            crate::clipboard::attachment_probe_gate(Some(text))
        } else {
            None
        };
        let (result, insertion) = insert(self);
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if let Some(change_count) = change_count {
            self.enqueue_clipboard_attachment_probe(
                crate::app::actions::ClipboardPasteSource::BracketedInserted {
                    text: text.to_owned(),
                    insertion,
                },
                change_count,
            );
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let _ = (text, insertion);
        result
    }
    /// Returns redraw and completion outcomes only when at least one path resolves.
    ///
    /// This is the canonical drag-and-drop / Finder-paste classifier on the
    /// main Prompt pane. It must run BEFORE any clipboard image probe so the
    /// Finder icon attached to a non-image file path doesn't get rendered
    /// as a chip. Other `Event::Paste` arms (permission followup, question
    /// view, plan feedback, plan approval) route through here too.
    ///
    /// `refresh_slash` fires exactly once at the end of the loop when any
    /// entry was inserted. `notify_suggestion_text_changed` fires only when
    /// at least one non-image path was inserted — `[Image #N]` placeholder
    /// text doesn't influence @-mention / file-search completions.
    ///
    /// Entries are processed in **source-token order**:
    /// `"file://{png} file://{txt}"` → `[Image #N] {canon_txt} `;
    /// `"file://{txt} file://{png}"` → `{canon_txt} [Image #N] `.
    ///
    /// **Size guard**: payloads ≥ `DROP_CLASSIFIER_MAX_BYTES`
    /// short-circuit to `None`. The early-return lives inside this
    /// function (not at each call site) so every paste arm — the
    /// main Prompt bracketed-paste arm, the four popup `Event::Paste`
    /// arms (plan-feedback, permission-followup, plan-approval,
    /// question-view), and the Cmd+V `handle_paste_key_deferred` path
    /// (clipboard-text, plus deferred file-urls on completion) — gets the
    /// guard uniformly. Real drag-and-drop payloads (one or more
    /// `file://` URLs) are at most a few KB; anything ≥ 10 MB is a
    /// log/code paste and not worth iterating line-by-line.
    pub(super) fn try_handle_dropped_paths_paste(
        &mut self,
        text: &str,
    ) -> Option<(InputOutcome, crate::app::actions::ClipboardPasteCompletion)> {
        if crate::terminal::terminal_context().is_ssh {
            return None;
        }
        /// Upper bound on the size of a paste payload the drop
        /// classifier will scan. 10 MB matches `MAX_SEND_BYTES` for
        /// individual image attachments — well above any realistic
        /// drop, well below any log/code paste worth iterating.
        const DROP_CLASSIFIER_MAX_BYTES: usize = 10 * 1024 * 1024;
        if text.len() >= DROP_CLASSIFIER_MAX_BYTES {
            return None;
        }
        let dropped = crate::prompt_images::try_read_dropped_paths(text);
        if dropped.is_empty() {
            return None;
        }
        let mut group_open = false;
        let mut inserted_image = false;
        let mut inserted_non_image = false;
        let mut image_cap_reached = false;
        for entry in dropped {
            match entry {
                crate::prompt_images::DroppedPath::Image(img) => {
                    if image_cap_reached {
                        continue;
                    }
                    if self.prompt.images.len() >= PromptWidget::IMAGE_CAP {
                        image_cap_reached = true;
                        self.show_toast(&PromptWidget::cap_reached_toast());
                        continue;
                    }
                    if !group_open {
                        self.prompt.textarea.begin_undo_group();
                        group_open = true;
                    }
                    if self.handle_image_paste_from_data(img) {
                        inserted_image = true;
                    }
                }
                crate::prompt_images::DroppedPath::NonImage(path) => {
                    let to_insert = format!("{} ", path.display());
                    if !group_open {
                        self.prompt.textarea.begin_undo_group();
                        group_open = true;
                    }
                    if matches!(self.prompt.handle_paste(&to_insert), PromptEvent::Edited) {
                        inserted_non_image = true;
                    }
                }
            }
        }
        if group_open {
            self.prompt.textarea.end_undo_group();
        }
        if inserted_image || inserted_non_image {
            self.prompt.refresh_slash(&self.session.models);
        }
        if inserted_non_image && let Some(eff) = self.notify_suggestion_text_changed() {
            self.pending_effects.push(eff);
        }
        if inserted_non_image && let Some(eff) = self.notify_plugin_cta_text_changed() {
            self.pending_effects.push(eff);
        }
        let completion = if inserted_image || inserted_non_image {
            crate::app::actions::ClipboardPasteCompletion::Handled
        } else {
            crate::app::actions::ClipboardPasteCompletion::Failed(
                crate::app::actions::ClipboardPasteFailure::AlreadyReported,
            )
        };
        Some((InputOutcome::Changed, completion))
    }
    /// Persist a `PastedImage` to the session directory and insert it as an
    /// `[Image #N]` chip in the prompt.
    ///
    /// **Does NOT call `refresh_slash`.** Callers are expected to do
    /// that once per logical paste event (at the loop boundary for
    /// the drag-and-drop classifier, immediately after-return for
    /// single-image callers). Centralising the refresh at the caller
    /// boundary avoids the (N+1)-call multiplicity that would otherwise
    /// arise on a mixed N-image-plus-1-non-image drop.
    ///
    /// Returns `true` when the buffer was actually mutated (chip
    /// inserted), `false` when persistence or the cap rejected the
    /// insert. Callers that wrap an undo group around a batch of
    /// inserts use this to defer opening the group until at least one
    /// mutation lands (avoids an empty undo step on Ctrl-Z).
    fn handle_image_paste_from_data(
        &mut self,
        mut pasted: crate::prompt_images::PastedImage,
    ) -> bool {
        if self.reject_shared_queue_image_edit(&pasted) {
            return false;
        }
        let preparation = pasted.preview_preparation();
        if let Some(images_dir) = crate::prompt_images::session_images_dir(
            self.session.session_id.as_ref(),
            &self.session.cwd,
        ) && let Err(e) = crate::prompt_images::persist_to_session(&mut pasted, &images_dir)
        {
            tracing::warn!("failed to persist pasted image: {e}");
            self.show_toast("Couldn't save pasted image");
            return false;
        }
        if let Err(msg) = self.prompt.insert_image(pasted) {
            self.show_toast_ticks(&msg, 150);
            return false;
        }
        if let Some(preparation) = preparation {
            self.pending_effects
                .push(crate::app::actions::Effect::PreparePromptImagePreview { preparation });
        }
        true
    }
}
#[cfg(test)]
pub(super) mod paste_key_tests {
    use super::*;
    use crate::acp::model_state::ModelState;
    use crate::app::agent::{AgentId, AgentSession, AgentState};
    use crate::app::agent_view::test_fixtures::{
        make_followup_permission_state, make_plan_approval_view_state,
    };
    use crate::app::app_view::InputOutcome;
    use crate::clipboard::ImageData;
    use crate::scrollback::state::ScrollbackState;
    use crate::views::prompt_widget::KIND_PASTE;
    fn make_agent() -> AgentView {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        AgentView::new(
            AgentSession {
                id: AgentId(0),
                acp_tx: tx,
                session_id: None,
                models: ModelState::default(),
                state: AgentState::Idle,
                tracker: crate::acp::tracker::AcpUpdateTracker::new(),
                cwd: std::path::PathBuf::from("/tmp"),
                is_worktree: false,
                forked_from: None,
                pending_prompts: std::collections::VecDeque::new(),
                next_queue_id: 0,
                yolo_mode: false,
                auto_mode: false,
                prompt_history: Vec::new(),
                prompt_history_loading: false,
                loading_replay: false,
                restore_degree: None,
                rate_limited: false,
                model_incompatible: false,
                credit_limit_blocked: false,
                free_usage_blocked: false,
                available_commands: Vec::new(),
                available_commands_generation: 0,
                available_tools: None,
                model_switch_pending: false,
                user_model_preference: None,
                deferred_model_switch: None,
                bg_tasks: std::collections::BTreeMap::new(),
                bg_tool_call_to_task: std::collections::HashMap::new(),
                scheduled_tasks: std::collections::HashMap::new(),
                in_flight_prompt: None,
                compact_held_prompt: None,
                current_prompt_id: None,
                created_via_new: false,
            },
            ScrollbackState::new(),
        )
    }
    /// Minimal PNG header (not a valid image, but enough for `from_clipboard_data`
    /// which only copies bytes and MIME type without decoding).
    fn test_image_data() -> ImageData {
        ImageData {
            data: vec![
                0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0, 0, 0, 0, 0,
            ],
            mime_type: "image/png".to_string(),
        }
    }
    #[test]
    fn wrap_host_image_none_paste_not_inserted_as_text() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        let registry = ActionRegistry::defaults();
        let outcome = agent.handle_input(
            &Event::Paste(crate::wrap_clipboard_image::MAGIC_NONE.to_string()),
            &registry,
        );
        assert!(matches!(outcome, InputOutcome::Unchanged));
        assert!(agent.prompt.text().is_empty());
    }
    #[test]
    fn wrap_host_image_malformed_paste_not_inserted_as_text() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        let registry = ActionRegistry::defaults();
        let outcome = agent.handle_input(
            &Event::Paste("ECHO_AGENT_WRAP_IMG\nimage/png\n!!!".to_string()),
            &registry,
        );
        assert!(matches!(outcome, InputOutcome::Unchanged));
        assert!(agent.prompt.text().is_empty());
    }
    #[test]
    fn paste_key_text_inserts_into_prompt() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        let outcome = paste_cmd_v(&mut agent, Some("hello world"));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(agent.prompt.text(), "hello world");
    }
    #[test]
    fn paste_key_multiline_text_creates_element() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        let text = "line1\nline2\nline3\nline4";
        let outcome = paste_cmd_v(&mut agent, Some(text));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(agent.prompt.text(), text);
        assert_eq!(agent.prompt.textarea().elements().len(), 1);
        assert_eq!(agent.prompt.textarea().elements()[0].kind, KIND_PASTE);
    }
    #[test]
    fn paste_key_image_preferred_over_text() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        paste_cmd_v_image(&mut agent, Some("text content"));
        assert_eq!(agent.prompt.images.len(), 1);
        assert!(agent.prompt.text().contains("[Image #1]"));
        assert!(!agent.prompt.text().contains("text content"));
    }
    #[test]
    fn paste_key_image_when_no_text() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        paste_cmd_v_image(&mut agent, None);
        assert_eq!(agent.prompt.images.len(), 1);
        assert!(agent.prompt.text().contains("[Image #1]"));
    }
    #[test]
    fn paste_key_image_when_text_is_empty_string() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        paste_cmd_v_image(&mut agent, Some(""));
        assert_eq!(agent.prompt.images.len(), 1);
    }
    #[test]
    fn paste_key_image_when_text_is_whitespace() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        paste_cmd_v_image(&mut agent, Some("   \n  "));
        assert_eq!(agent.prompt.images.len(), 1);
    }
    #[test]
    fn paste_key_empty_clipboard_consumes_key() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        let outcome = paste_cmd_v(&mut agent, None);
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(agent.prompt.text().is_empty());
        assert!(agent.prompt.images.is_empty());
    }
    /// Whitespace-only Cmd+V inserts no text. Trimmed-empty routes to the
    /// FileUrlsThenImage probe (to catch an image-only pasteboard), so it defers
    /// off the event loop rather than inserting spaces; the completion drops the
    /// whitespace caption (a no-image miss with blank text inserts nothing).
    #[test]
    fn paste_key_whitespace_only_text_with_no_image_or_urls_is_noop() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        let _ = paste_cmd_v(&mut agent, Some("   "));
        assert!(agent.prompt.text().is_empty());
        let _ = paste_cmd_v(&mut agent, Some("\t\n  \t"));
        assert!(agent.prompt.text().is_empty());
    }
    #[test]
    fn paste_key_appends_to_existing_text() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        agent.prompt.textarea.insert_str("existing ");
        let outcome = paste_cmd_v(&mut agent, Some("pasted"));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(agent.prompt.text(), "existing pasted");
    }
    #[test]
    fn paste_key_image_with_existing_text_in_prompt() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        agent.prompt.textarea.insert_str("describe: ");
        paste_cmd_v_image(&mut agent, None);
        assert_eq!(agent.prompt.images.len(), 1);
        assert!(agent.prompt.text().contains("describe: "));
        assert!(agent.prompt.text().contains("[Image #1]"));
    }
    /// A single-newline paste is whitespace-only and inserts no text (trimmed
    /// empty → deferred probe, whitespace caption dropped on the miss).
    #[test]
    fn paste_key_single_newline_text_is_noop() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        let _ = paste_cmd_v(&mut agent, Some("\n"));
        assert!(agent.prompt.text().is_empty());
    }
    #[test]
    fn paste_key_cr_normalized_to_lf() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        let outcome = paste_cmd_v(&mut agent, Some("a\rb\rc"));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(agent.prompt.text(), "a\nb\nc");
    }
    #[test]
    fn paste_key_tabs_expanded_to_spaces() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        let outcome = paste_cmd_v(&mut agent, Some("if true:\n\tpass"));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(!agent.prompt.text().contains('\t'));
        assert_eq!(agent.prompt.text(), "if true:\n    pass");
    }
    /// Empty-string clipboard text is whitespace-only: no text is inserted
    /// (trimmed-empty → deferred probe, blank caption dropped on the miss).
    #[test]
    fn paste_key_empty_string_text_no_image_is_noop() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        let _ = paste_cmd_v(&mut agent, Some(""));
        assert!(agent.prompt.text().is_empty());
    }
    #[test]
    fn paste_key_image_path_detected_as_image() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        let dir = tempfile::tempdir().unwrap();
        let png_path = dir.path().join("test.png");
        let png = make_test_png(10, 10);
        std::fs::write(&png_path, &png).unwrap();
        let path_str = png_path.to_string_lossy().to_string();
        let outcome = paste_cmd_v(&mut agent, Some(&path_str));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(agent.prompt.images.len(), 1);
        assert!(agent.prompt.text().contains("[Image #"));
        assert!(agent.prompt.images[0].preview.is_pending());
        assert!(agent.pending_effects.iter().any(|effect| matches!(
            effect,
            crate::app::actions::Effect::PreparePromptImagePreview { .. }
        )));
    }
    #[test]
    fn paste_key_tiny_image_path_cannot_insert_or_send_immediately() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tiny.png");
        std::fs::write(&path, make_test_png(1, 1)).unwrap();
        let outcome = paste_cmd_v(&mut agent, Some(&path.display().to_string()));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(agent.prompt.images.is_empty());
        assert!(!agent.prompt.text().contains("[Image #"));
        assert!(!agent.pending_effects.iter().any(|effect| matches!(
            effect,
            crate::app::actions::Effect::PreparePromptImagePreview { .. }
        )));
        let enter = agent.handle_prompt_key_for_test(&KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert!(!matches!(
            enter,
            InputOutcome::Action(crate::app::actions::Action::SendPrompt(_))
        ));
    }
    #[test]
    fn paste_key_non_image_path_pasted_as_text() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        let outcome = paste_cmd_v(&mut agent, Some("/tmp/not-an-image.txt"));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(agent.prompt.images.is_empty());
        assert_eq!(agent.prompt.text(), "/tmp/not-an-image.txt");
    }
    #[test]
    fn paste_key_non_image_file_url_with_clipboard_icon_uses_path_not_icon() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        let dir = tempfile::tempdir().unwrap();
        let txt = dir.path().join("notes.txt");
        std::fs::write(&txt, b"hello").unwrap();
        let url = format!("file://{}", txt.display());
        let outcome = paste_cmd_v(&mut agent, Some(&url));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(
            agent.prompt.images.is_empty(),
            "must NOT insert image chip for non-image file; got images: {:?}",
            agent.prompt.images.len()
        );
        assert!(
            !agent.prompt.text().contains("[Image #"),
            "no chip placeholder expected for non-image drop; got {:?}",
            agent.prompt.text()
        );
        let canon_txt = dunce::canonicalize(&txt).unwrap();
        let want_with_trailing_space = format!("{} ", canon_txt.display());
        assert!(
            agent.prompt.text().contains(&want_with_trailing_space),
            "decoded path + trailing space must be inserted; got {:?}, want substring {:?}",
            agent.prompt.text(),
            want_with_trailing_space
        );
    }
    /// Bug A regression: when pbpaste returns `None` (the
    /// `public.utf8-plain-text` representation is absent on the macOS clipboard)
    /// but the deferred probe recovers `public.file-url`, the completion routes
    /// the non-image file to decoded path text (not an `[Image #N]` chip). The
    /// probe's `FileUrlsThenImage` route suppresses the Finder file-icon raster
    /// off-thread, so the completion sees no image.
    #[test]
    fn paste_key_file_urls_probe_recovers_when_text_is_none() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        let dir = tempfile::tempdir().unwrap();
        let txt = dir.path().join("only_furl.txt");
        std::fs::write(&txt, b"hi").unwrap();
        let url = format!("file://{}", txt.display());
        let ctx = agent_completion_ctx(&agent, None);
        let completion = agent.complete_clipboard_attachment_paste(
            ctx,
            crate::app::actions::ProbedAttachment::NoRaster,
            Some(url),
        );
        assert_eq!(
            completion,
            crate::app::actions::ClipboardPasteCompletion::Handled
        );
        assert!(
            agent.prompt.images.is_empty(),
            "file-url path must beat the icon image; got images: {:?}",
            agent.prompt.images.len()
        );
        assert!(
            !agent.prompt.text().contains("[Image #"),
            "no chip placeholder expected; got {:?}",
            agent.prompt.text()
        );
        let canon_txt = dunce::canonicalize(&txt).unwrap();
        let want_with_trailing_space = format!("{} ", canon_txt.display());
        assert!(
            agent.prompt.text().contains(&want_with_trailing_space),
            "decoded path + trailing space must be inserted; got {:?}, want substring {:?}",
            agent.prompt.text(),
            want_with_trailing_space
        );
    }
    /// Bug A regression (sibling of the `None`-text case): macOS `pbpaste`
    /// returns `Some("")` rather than `None` in some configurations. The
    /// deferred file-url recovery must still route the path on completion.
    #[test]
    fn paste_key_file_urls_probe_recovers_when_text_is_empty_string() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        let dir = tempfile::tempdir().unwrap();
        let txt = dir.path().join("empty_text_furl.txt");
        std::fs::write(&txt, b"hi").unwrap();
        let url = format!("file://{}", txt.display());
        let ctx = agent_completion_ctx(&agent, Some(""));
        agent.complete_clipboard_attachment_paste(
            ctx,
            crate::app::actions::ProbedAttachment::NoRaster,
            Some(url),
        );
        assert!(
            agent.prompt.images.is_empty(),
            "file-url path must beat the icon image even when text is \"\"",
        );
        let canon_txt = dunce::canonicalize(&txt).unwrap();
        let want_with_trailing_space = format!("{} ", canon_txt.display());
        assert!(
            agent.prompt.text().contains(&want_with_trailing_space),
            "decoded path + trailing space must be inserted; got {:?}",
            agent.prompt.text(),
        );
    }
    /// Bug A multi-file integration: the macOS pasteboard's `public.file-url`
    /// type carries N newline-joined POSIX paths. The completion must (a) route
    /// the PNG entry to a chip and (b) route the non-image entry to decoded path
    /// text (the Finder file-icon raster is suppressed by the off-thread probe).
    #[test]
    fn paste_key_file_urls_probe_handles_multi_file_payload() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("snap.png");
        let txt = dir.path().join("notes.txt");
        std::fs::write(&png, make_test_png(8, 8)).unwrap();
        std::fs::write(&txt, b"x").unwrap();
        let furl_payload = format!("{}\n{}", png.display(), txt.display());
        let ctx = agent_completion_ctx(&agent, None);
        agent.complete_clipboard_attachment_paste(
            ctx,
            crate::app::actions::ProbedAttachment::NoRaster,
            Some(furl_payload),
        );
        assert_eq!(
            agent.prompt.images.len(),
            1,
            "exactly one chip for the real PNG — not the Finder icon",
        );
        assert!(
            agent.prompt.text().contains("[Image #1]"),
            "PNG chip placeholder must be present; prompt = {:?}",
            agent.prompt.text(),
        );
        let canon_txt = dunce::canonicalize(&txt).unwrap();
        let want_with_trailing_space = format!("{} ", canon_txt.display());
        assert!(
            agent.prompt.text().contains(&want_with_trailing_space),
            "non-image path + trailing space must be inserted; prompt = {:?}",
            agent.prompt.text(),
        );
    }
    /// Bug A regression: when the pbpaste text already classifies as drop paths,
    /// the text-path resolver wins synchronously and NO probe is deferred (so the
    /// off-thread file-url recovery never runs and can't insert a rival path).
    #[test]
    fn paste_key_file_urls_probe_not_double_inserted_when_text_classifies() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        let dir = tempfile::tempdir().unwrap();
        let primary = dir.path().join("notes.txt");
        std::fs::write(&primary, b"primary").unwrap();
        let primary_url = format!("file://{}", primary.display());
        let outcome = paste_cmd_v(&mut agent, Some(&primary_url));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(
            agent.prompt.images.is_empty(),
            "image must lose to classifying text; got images: {}",
            agent.prompt.images.len(),
        );
        assert!(
            deferred_probe_target(&agent).is_none(),
            "a classifying text paste must not also defer a probe"
        );
        let canon_primary = dunce::canonicalize(&primary).unwrap();
        let want_primary = format!("{} ", canon_primary.display());
        assert!(
            agent.prompt.text().contains(&want_primary),
            "primary text path must be inserted; prompt = {:?}",
            agent.prompt.text(),
        );
    }
    #[test]
    fn paste_key_non_image_file_url_percent_encoded_space_round_trips() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("My Documents");
        std::fs::create_dir_all(&sub).unwrap();
        let txt = sub.join("readme report.md");
        std::fs::write(&txt, b"x").unwrap();
        let encoded = txt.display().to_string().replace(' ', "%20");
        let url = format!("file://{}", encoded);
        let outcome = paste_cmd_v(&mut agent, Some(&url));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(agent.prompt.images.is_empty());
        assert!(
            !agent.prompt.text().contains("[Image #"),
            "no chip placeholder expected for non-image drop; got {:?}",
            agent.prompt.text()
        );
        let canon_txt = dunce::canonicalize(&txt).unwrap();
        let want_with_trailing_space = format!("{} ", canon_txt.display());
        assert!(
            agent.prompt.text().contains(&want_with_trailing_space),
            "percent-encoded path + trailing space must round-trip; got {:?}, want substring {:?}",
            agent.prompt.text(),
            want_with_trailing_space
        );
        assert!(
            !agent.prompt.text().contains("%20"),
            "%20 must be decoded; got {:?}",
            agent.prompt.text()
        );
    }
    #[test]
    fn paste_key_multi_file_drop_image_plus_non_image_handles_both() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("snap.png");
        let txt = dir.path().join("notes.txt");
        let png_bytes = make_test_png(8, 8);
        std::fs::write(&png, &png_bytes).unwrap();
        std::fs::write(&txt, b"hi").unwrap();
        let pasted = format!("file://{}\nfile://{}", png.display(), txt.display());
        let outcome = paste_cmd_v(&mut agent, Some(&pasted));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(agent.prompt.images.len(), 1);
        assert!(agent.prompt.text().contains("[Image #1]"));
        assert!(
            !agent.prompt.text().contains("[Image #2]"),
            "non-image must not also produce a chip; prompt = {:?}",
            agent.prompt.text()
        );
        let canon_txt = dunce::canonicalize(&txt).unwrap();
        let want_with_trailing_space = format!("{} ", canon_txt.display());
        assert!(
            agent.prompt.text().contains(&want_with_trailing_space),
            "non-image path + trailing space must be inserted; prompt = {:?}, want substring = {:?}",
            agent.prompt.text(),
            want_with_trailing_space
        );
        let prompt_text = agent.prompt.text();
        let chip_idx = prompt_text.find("[Image #1]").unwrap();
        let path_idx = prompt_text.find(&want_with_trailing_space).unwrap();
        assert!(
            chip_idx < path_idx,
            "chip must precede path text in source order; prompt = {prompt_text:?}"
        );
    }
    #[test]
    fn paste_key_image_path_with_trailing_newline_still_attaches() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("trail.png");
        let png_bytes = make_test_png(8, 8);
        std::fs::write(&png, &png_bytes).unwrap();
        let pasted = format!("file://{}\n", png.display());
        let outcome = paste_cmd_v(&mut agent, Some(&pasted));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(agent.prompt.images.len(), 1);
        assert!(
            agent.prompt.text().contains("[Image #1]"),
            "expected `[Image #1]` chip text; got {:?}",
            agent.prompt.text()
        );
        assert!(
            !agent.prompt.text().contains("file://"),
            "no `file://` fragment should leak into the prompt; got {:?}",
            agent.prompt.text()
        );
    }
    /// Mirrors the strongest Cmd+V drop-path test, but exercises the
    /// `Event::Paste` (bracketed-paste) branch — the actual path a
    /// drag-from-Finder takes through the dispatcher. A future refactor
    /// that re-orders the clipboard-image probe ahead of the path
    /// classifier on this branch would silently regress the headline
    /// bug; this test fails fast in that scenario.
    #[test]
    fn event_paste_non_image_file_url_inserts_decoded_path_not_chip() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        let dir = tempfile::tempdir().unwrap();
        let txt = dir.path().join("event_paste.txt");
        std::fs::write(&txt, b"hello").unwrap();
        let url = format!("file://{}", txt.display());
        let registry = ActionRegistry::defaults();
        let outcome = agent.handle_input(&Event::Paste(url), &registry);
        assert!(
            matches!(outcome, InputOutcome::Changed),
            "Event::Paste path must claim the drop with `Changed`; got {outcome:?}"
        );
        assert!(
            agent.prompt.images.is_empty(),
            "non-image file:// drop must not create an image chip"
        );
        assert!(
            !agent.prompt.text().contains("[Image #"),
            "no chip placeholder expected; got {:?}",
            agent.prompt.text()
        );
        let canon_txt = dunce::canonicalize(&txt).unwrap();
        let want_with_trailing_space = format!("{} ", canon_txt.display());
        assert!(
            agent.prompt.text().contains(&want_with_trailing_space),
            "decoded path + trailing space must be inserted; got {:?}, want substring {:?}",
            agent.prompt.text(),
            want_with_trailing_space
        );
    }
    /// A multi-image drop that pushes us past the cap must NOT block
    /// subsequent non-image entries — the trailing path text must still
    /// be inserted. We assert the cap toast is the live (last-written)
    /// toast at the end of the drop; the dedup latch itself is enforced
    /// statically by the `image_cap_reached` branch in
    /// `try_handle_dropped_paths_paste`.
    #[test]
    fn paste_key_cap_reached_does_not_block_non_image_insert() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        let cap = crate::views::prompt_widget::PromptWidget::IMAGE_CAP;
        for _ in 0..(cap - 1) {
            agent.prompt.insert_image(test_image_paste()).unwrap();
        }
        assert_eq!(agent.prompt.images.len(), cap - 1);
        let dir = tempfile::tempdir().unwrap();
        let mk_png = |name: &str| {
            let p = dir.path().join(name);
            std::fs::write(&p, make_test_png(8, 8)).unwrap();
            p
        };
        let a = mk_png("a.png");
        let b = mk_png("b.png");
        let c = mk_png("c.png");
        let txt = dir.path().join("note.txt");
        std::fs::write(&txt, b"x").unwrap();
        let pasted = format!(
            "file://{}\nfile://{}\nfile://{}\nfile://{}",
            a.display(),
            b.display(),
            c.display(),
            txt.display(),
        );
        let outcome = paste_cmd_v(&mut agent, Some(&pasted));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(agent.prompt.images.len(), cap);
        let canon_txt = dunce::canonicalize(&txt).unwrap();
        let want = format!("{} ", canon_txt.display());
        assert!(
            agent.prompt.text().contains(&want),
            "non-image path must still be inserted past the image cap; prompt = {:?}",
            agent.prompt.text()
        );
        let toast_msg = agent
            .toast
            .as_ref()
            .map(|(msg, _ticks)| msg.clone())
            .unwrap_or_default();
        assert!(
            toast_msg.contains("Image limit reached"),
            "expected cap toast to be the last toast shown; got {toast_msg:?}"
        );
    }
    /// Drive `agent` through the canonical drop-classifier assertions
    /// for one `Event::Paste` arm. The `setup` closure puts the agent
    /// into whatever state the dispatcher needs to route paste through
    /// the target arm (focus, queue, viewer, etc.). All four arms
    /// share these assertions; using a single helper prevents the
    /// four bodies from drifting against each other.
    fn assert_event_paste_arm_decodes_non_image(
        arm_name: &str,
        setup: impl FnOnce(&mut AgentView),
    ) {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        setup(&mut agent);
        let dir = tempfile::tempdir().unwrap();
        let txt = dir.path().join(format!("{arm_name}_paste.txt"));
        std::fs::write(&txt, b"hello").unwrap();
        let url = format!("file://{}", txt.display());
        let registry = ActionRegistry::defaults();
        let outcome = agent.handle_input(&Event::Paste(url), &registry);
        assert!(
            matches!(outcome, InputOutcome::Changed),
            "[{arm_name}] outcome must be Changed; got {outcome:?}"
        );
        assert!(
            agent.prompt.images.is_empty(),
            "[{arm_name}] non-image drop must not create an image chip"
        );
        assert!(
            !agent.prompt.text().contains("[Image #"),
            "[{arm_name}] no chip placeholder expected; got {:?}",
            agent.prompt.text()
        );
        assert!(
            !agent.prompt.text().contains("file://"),
            "[{arm_name}] no `file://` fragment may leak into the prompt; got {:?}",
            agent.prompt.text()
        );
        let canon_txt = dunce::canonicalize(&txt).unwrap();
        let want = format!("{} ", canon_txt.display());
        assert!(
            agent.prompt.text().contains(&want),
            "[{arm_name}] decoded path + trailing space must appear; got {:?}",
            agent.prompt.text()
        );
    }
    /// The plan-feedback / casual-commenting `Event::Paste` arm routes
    /// through the canonical drop classifier. Re-introducing a raw
    /// `self.prompt.handle_paste(text)` here would skip path decoding
    /// and fail this test.
    #[test]
    fn event_paste_plan_feedback_non_image_file_url_decoded_into_prompt() {
        assert_event_paste_arm_decodes_non_image("plan_feedback", |agent| {
            agent.enter_casual_commenting_for_test();
            assert!(
                agent.is_casual_commenting(),
                "fixture must satisfy is_casual_commenting() to reach the plan-feedback paste arm"
            );
        });
    }
    /// Permission-followup `Event::Paste` arm routes through the classifier.
    #[test]
    fn event_paste_permission_followup_non_image_file_url_decoded_into_prompt() {
        assert_event_paste_arm_decodes_non_image("permission_followup", |agent| {
            agent
                .permission_queue
                .push_back(make_followup_permission_state());
        });
    }
    /// Plan-approval-view `Event::Paste` arm routes through the classifier.
    #[test]
    fn event_paste_plan_approval_non_image_file_url_decoded_into_prompt() {
        assert_event_paste_arm_decodes_non_image("plan_approval", |agent| {
            let mut view = make_plan_approval_view_state();
            view.focus = crate::views::plan_approval_view::PlanApprovalFocus::Prompt;
            agent.plan_approval_view = Some(view);
            agent.line_viewer = None;
        });
    }
    #[test]
    fn event_paste_plan_preview_does_not_mutate_hidden_prompt() {
        let mut agent = make_agent();
        agent.prompt.set_text("hidden prompt");
        agent.plan_approval_view = Some(make_plan_approval_view_state());
        agent.line_viewer = None;
        let outcome = agent.handle_input(
            &Event::Paste("ignored".to_owned()),
            &ActionRegistry::defaults(),
        );
        assert!(matches!(outcome, InputOutcome::Unchanged));
        assert_eq!(agent.prompt.text(), "hidden prompt");
    }
    /// Question-view `Event::Paste` arm routes through the classifier when
    /// the question view is in `InputMode` focus.
    #[test]
    fn event_paste_question_view_input_mode_non_image_file_url_decoded_into_prompt() {
        assert_event_paste_arm_decodes_non_image("question_view", |agent| {
            agent.question_view = Some(make_question_view_state_in_input_mode());
        });
    }
    /// Build a `QuestionViewState` already in `InputMode` focus.
    pub(in crate::app::agent_view) fn make_question_view_state_in_input_mode()
    -> crate::views::question_view::QuestionViewState {
        let question = echo_agent_tools::implementations::echo_agent_build::ask_user_question::Question {
            question: "Pick one?".to_string(),
            options: vec![
                echo_agent_tools::implementations::echo_agent_build::ask_user_question::QuestionOption {
                    label: "A".to_string(),
                    description: "Option A".to_string(),
                    preview: None,
                    id: None,
                },
            ],
            multi_select: Some(false),
            id: None,
        };
        let mut state = crate::views::question_view::QuestionViewState::new(
            "tc-1".into(),
            vec![question],
            crate::views::prompt_widget::StashedPrompt {
                text: String::new(),
                cursor: 0,
                images: Vec::new(),
                chip_elements: Vec::new(),
                image_counter: 0,
                image_undo_stash: Vec::new(),
            },
        );
        state.focus = crate::views::question_view::QuestionFocus::InputMode;
        state
    }
    /// Helper: build a minimal `PastedImage` for in-memory cap testing.
    /// Uses a real PNG byte payload so `insert_image` accepts it.
    fn test_image_paste() -> crate::prompt_images::PastedImage {
        let bytes = make_test_png(8, 8);
        crate::prompt_images::from_clipboard_data(&crate::clipboard::ImageData {
            data: bytes,
            mime_type: "image/png".to_string(),
        })
    }
    /// Generate a valid minimal PNG of the given dimensions.
    fn make_test_png(width: u32, height: u32) -> Vec<u8> {
        use image::{ImageBuffer, Rgba};
        let img: ImageBuffer<Rgba<u8>, Vec<u8>> =
            ImageBuffer::from_pixel(width, height, Rgba([128, 64, 32, 255]));
        let mut buf = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        buf
    }
    #[test]
    fn regression_question_modal_fullscreen_overcommit() {
        use crate::views::agent::AgentViewLayoutParams;
        use ratatui::layout::Rect;
        let area = Rect::new(0, 0, 80, 25);
        let params = AgentViewLayoutParams {
            area,
            shortcuts_height: 1,
            ..Default::default()
        };
        let unclamped: u16 = area.height + 3 + 5;
        assert!(unclamped > area.height);
        let clamped = unclamped.min(AgentViewLayout::rows_available_for_prompt(params));
        let layout = AgentViewLayout::compute(AgentViewLayoutParams {
            prompt_height: clamped,
            ..params
        });
        assert!(layout.prompt.y + layout.prompt.height <= area.height);
    }
    /// Wiping a substantial main-prompt draft routes `Action::ShowUndoTip`:
    /// the end-to-end happy path the whole feature exists for.
    #[test]
    fn main_prompt_substantial_wipe_routes_show_undo_tip() {
        let mut agent = make_agent();
        agent.prompt.set_contextual_hints(true, true);
        for _ in 0..25 {
            let _ = agent.handle_prompt_key_for_test(&key!('x').to_key_event());
        }
        let outcome = agent.handle_prompt_key_for_test(&key!('c', CONTROL).to_key_event());
        assert!(
            matches!(outcome, InputOutcome::Action(Action::ShowUndoTip)),
            "substantial main-prompt wipe must route show, got {outcome:?}"
        );
    }
    /// Ctrl+Z while the undo tip is on screen is an acceptance: it restores the
    /// wiped draft and retires the hint (the guarded branch that also emits the
    /// `accepted` telemetry; the emit itself has no in-process capture sink).
    #[test]
    fn ctrl_z_accepts_and_retires_undo_tip() {
        let mut agent = make_agent();
        agent.prompt.set_contextual_hints(true, true);
        for _ in 0..25 {
            let _ = agent.handle_prompt_key_for_test(&key!('x').to_key_event());
        }
        let _ = agent.handle_prompt_key_for_test(&key!('c', CONTROL).to_key_event());
        assert!(agent.prompt.text().is_empty(), "ctrl+c wiped the draft");
        let _ = agent.ephemeral_tip.show(
            crate::tips::clear_detector::undo_tip(),
            &mut std::collections::HashMap::new(),
        );
        let _ = agent.handle_prompt_key_for_test(&key!('z', CONTROL).to_key_event());
        assert!(
            !agent.prompt.text().is_empty(),
            "ctrl+z restored the wiped draft"
        );
        assert!(
            !agent.ephemeral_tip.is_active(),
            "accepting ctrl+z retires the undo tip"
        );
    }
    /// Ctrl+Z attributes ONLY the undo tip: with a different tip on screen the
    /// undo-accept guard is false, so that tip is left untouched (no acceptance
    /// is misattributed to it).
    #[test]
    fn ctrl_z_leaves_a_non_undo_tip_untouched() {
        let mut agent = make_agent();
        agent.prompt.set_contextual_hints(true, true);
        for _ in 0..25 {
            let _ = agent.handle_prompt_key_for_test(&key!('x').to_key_event());
        }
        let _ = agent.handle_prompt_key_for_test(&key!('c', CONTROL).to_key_event());
        let _ = agent.ephemeral_tip.show(
            crate::tips::clipboard_focus::clipboard_image_tip(),
            &mut std::collections::HashMap::new(),
        );
        let _ = agent.handle_prompt_key_for_test(&key!('z', CONTROL).to_key_event());
        assert_eq!(
            agent.ephemeral_tip.current_key(),
            Some(crate::tips::clipboard_focus::CLIPBOARD_IMAGE_TIP_KEY),
            "ctrl+z must not retire a tip that is not the undo tip"
        );
    }
    /// Type a draft across into a planning keyword so the prompt's one-shot
    /// plan-nudge fire is armed (mirrors a real keypress without the
    /// auto-managed input-mode reset a full route would apply).
    fn arm_plan_nudge(agent: &mut AgentView) {
        agent.prompt.set_contextual_hints(true, true);
        for ch in "plan".chars() {
            let _ = agent.prompt.handle_key(&crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char(ch),
                crossterm::event::KeyModifiers::NONE,
            ));
        }
    }
    /// Typing a planning keyword into an idle, normal-mode prompt that is not
    /// already in plan mode routes `Action::ShowPlanNudge`.
    #[test]
    fn typed_planning_keyword_routes_show_plan_nudge() {
        let mut agent = make_agent();
        arm_plan_nudge(&mut agent);
        assert!(
            matches!(agent.take_prompt_tip_signal(), Some(Action::ShowPlanNudge)),
            "idle normal-mode planning keyword must route the plan nudge"
        );
    }
    /// Plan-nudge gates: already in plan mode (optimistic read), a busy turn,
    /// or a special (bash/feedback/remember) input mode each suppress it.
    #[test]
    fn plan_nudge_suppressed_by_state_gates() {
        let mut agent = make_agent();
        agent.plan_mode_pending = Some(true);
        arm_plan_nudge(&mut agent);
        assert!(
            agent.take_prompt_tip_signal().is_none(),
            "plan mode must suppress the nudge"
        );
        let mut agent = make_agent();
        agent.session.state = AgentState::TurnRunning;
        arm_plan_nudge(&mut agent);
        assert!(
            agent.take_prompt_tip_signal().is_none(),
            "a busy turn must suppress the nudge"
        );
        let mut agent = make_agent();
        agent.prompt_input_mode = PromptInputMode::Bash;
        arm_plan_nudge(&mut agent);
        assert!(
            agent.take_prompt_tip_signal().is_none(),
            "a non-Normal input mode must suppress the nudge"
        );
    }
    /// A paste retires the clipboard-image hint that advertised it.
    #[test]
    fn paste_clears_clipboard_image_tip() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        let _ = agent.ephemeral_tip.show(
            crate::tips::clipboard_focus::clipboard_image_tip(),
            &mut std::collections::HashMap::new(),
        );
        assert!(agent.ephemeral_tip.is_active());
        let _ = paste_cmd_v(&mut agent, Some("hello"));
        assert!(
            !agent.ephemeral_tip.is_active(),
            "paste must clear the clipboard-image tip"
        );
    }
    /// A clipboard-IMAGE paste while the hint is on screen runs the acceptance
    /// branch (the guarded `contextual_tip` emit): the image attaches and the
    /// hint retires. The emission has no in-process sink, so this pins the
    /// guarded branch's observable behavior; `current_key()` is unit-tested in
    /// `tips::ephemeral` and the mapping in the telemetry crate.
    #[test]
    fn image_paste_accepts_clipboard_tip_and_attaches() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        let _ = agent.ephemeral_tip.show(
            crate::tips::clipboard_focus::clipboard_image_tip(),
            &mut std::collections::HashMap::new(),
        );
        assert_eq!(
            agent.ephemeral_tip.current_key(),
            Some(crate::tips::clipboard_focus::CLIPBOARD_IMAGE_TIP_KEY)
        );
        paste_cmd_v_image(&mut agent, None);
        assert_eq!(agent.prompt.images.len(), 1, "image attached");
        assert!(
            !agent.ephemeral_tip.is_active(),
            "the image paste retired the clipboard-image hint"
        );
    }
    /// A bracketed `Event::Paste` (not the Cmd+V chord) also retires the
    /// clipboard-image hint (regression guard). A 5-line, no-`://` payload
    /// skips the macOS attachment probe, so the test never reads the real
    /// pasteboard; the clear runs at the top of the prompt paste arm regardless.
    #[test]
    fn bracketed_paste_clears_clipboard_image_tip() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        let _ = agent.ephemeral_tip.show(
            crate::tips::clipboard_focus::clipboard_image_tip(),
            &mut std::collections::HashMap::new(),
        );
        assert!(agent.ephemeral_tip.is_active());
        let registry = ActionRegistry::defaults();
        let _ = agent.handle_input(&Event::Paste("a\nb\nc\nd\ne".to_string()), &registry);
        assert!(
            !agent.ephemeral_tip.is_active(),
            "bracketed paste must clear the clipboard-image tip"
        );
    }
    /// `show_ephemeral_tip` refuses shows that cannot paint, so no seen count,
    /// TTL, or telemetry burns invisibly: an unknown/short terminal, or any
    /// occluding view. One case per occluder predicate term — a per-term typo
    /// (wrong field, duplicate, omission) fails exactly one assertion — closed
    /// by a non-vacuous success that shows and counts.
    #[test]
    fn ephemeral_tip_show_refused_while_unrenderable() {
        use std::collections::HashMap;
        let mut agent = make_agent();
        let mut counts: HashMap<&'static str, u32> = HashMap::new();
        let gated = || {
            crate::tips::EphemeralTip::new("t", ratatui::text::Line::from("hint"))
                .with_session_seen_cap("t_seen", 1)
        };
        let assert_refused =
            |agent: &mut AgentView, counts: &mut HashMap<&'static str, u32>, why: &str| {
                assert!(!agent.show_ephemeral_tip(gated(), counts), "{why}");
                assert!(!agent.ephemeral_tip.is_active(), "{why}: tip must not show");
                assert!(counts.is_empty(), "{why}: must not burn a count");
            };
        assert_refused(&mut agent, &mut counts, "unknown size");
        agent.last_terminal_size = (80, 16);
        assert_refused(&mut agent, &mut counts, "short terminal");
        agent.last_terminal_size = (80, 30);
        agent.active_subagent = Some("child".into());
        assert_refused(&mut agent, &mut counts, "subagent takeover");
        agent.active_subagent = None;
        agent.line_viewer =
            crate::views::file_search::line_viewer::LineViewerState::open_markdown_content(
                "x.md",
                "body".to_string(),
                None,
            );
        assert!(agent.line_viewer.is_some(), "line viewer fixture must open");
        assert_refused(&mut agent, &mut counts, "line viewer");
        agent.line_viewer = None;
        agent.image_viewer = Some(
            crate::prompt_images::ImageViewerState::open_from_path_deferred(std::path::Path::new(
                "x.png",
            )),
        );
        assert_refused(&mut agent, &mut counts, "image viewer");
        agent.image_viewer = None;
        agent.video_viewer = Some(crate::prompt_images::VideoViewerState::test_stub());
        assert_refused(&mut agent, &mut counts, "video viewer");
        agent.video_viewer = None;
        agent.block_viewer = Some(crate::views::block_viewer::BlockViewerPane::for_plain_text(
            "t", "content",
        ));
        assert_refused(&mut agent, &mut counts, "block viewer");
        agent.block_viewer = None;
        agent.gboom = Some(crate::gboom::GboomState::new());
        assert_refused(&mut agent, &mut counts, "gboom");
        agent.gboom = None;
        agent.extensions_modal = Some(crate::views::extensions_modal::ExtensionsModalState::new(
            crate::views::extensions_modal::ExtensionsTab::Hooks,
        ));
        assert_refused(&mut agent, &mut counts, "extensions modal");
        agent.extensions_modal = None;
        agent.agents_modal = Some(crate::views::agents_modal::AgentsModalState::new(
            std::path::Path::new("/nonexistent"),
            &HashMap::new(),
            &crate::app::bundle::BundleState::default(),
            None,
            None,
            None,
        ));
        assert_refused(&mut agent, &mut counts, "agents modal");
        agent.agents_modal = None;
        agent.show_goal_detail = true;
        agent.goal_state = Some(crate::app::agent::GoalDisplayState::test_stub());
        assert_refused(&mut agent, &mut counts, "goal detail overlay");
        agent.show_goal_detail = false;
        agent.goal_state = None;
        agent.prompt.suggestions.dropdown.open = true;
        assert_refused(&mut agent, &mut counts, "prompt dropdown");
        agent.prompt.suggestions.dropdown.open = false;
        assert!(
            agent.show_ephemeral_tip(gated(), &mut counts),
            "renderable show must take the slot"
        );
        assert!(agent.ephemeral_tip.is_active());
        assert_eq!(counts.get("t_seen"), Some(&1));
    }
    /// A resize event must close the show gate until the next draw
    /// re-measures — in EITHER direction. The recorded height describes a
    /// possibly chrome-shrunk paint rect (dashboard overlay header/popup,
    /// dev tracing split), so even a grown terminal does not prove the
    /// banner row can paint; acting on any extrapolated height could burn
    /// a seen count on a tip that never shows.
    #[test]
    fn ephemeral_tip_show_gate_refuses_between_resize_and_redraw() {
        let mut agent = make_agent();
        let mut counts = std::collections::HashMap::new();
        let gated = || {
            crate::tips::EphemeralTip::new("t", ratatui::text::Line::from("hint"))
                .with_session_seen_cap("t_seen", 1)
        };
        agent.note_terminal_size((80, 30));
        agent.note_terminal_resize();
        assert!(!agent.show_ephemeral_tip(gated(), &mut counts));
        assert!(!agent.ephemeral_tip.is_active());
        assert!(counts.is_empty(), "stale-size show must not burn a count");
        assert_eq!(
            agent.last_terminal_size,
            (80, 30),
            "the event must not overwrite the draw-measured rect size"
        );
        agent.note_terminal_size((80, 30));
        assert!(agent.show_ephemeral_tip(gated(), &mut counts));
        assert!(agent.ephemeral_tip.is_active());
        assert_eq!(counts.get("t_seen"), Some(&1));
    }
    /// `note_terminal_size` keeps the draw-path semantics it replaced:
    /// Kitty IDs are invalidated only on an actual size change, the
    /// `(0, 0)` pre-first-draw state never counts as a resize, and every
    /// re-measure clears the resize-event staleness flag.
    #[test]
    fn note_terminal_size_invalidates_kitty_ids_only_on_change() {
        let mut agent = make_agent();
        let path = std::path::PathBuf::from("/img.png");
        agent.inline_media_ids.insert(path, 2);
        agent.note_terminal_size((80, 30));
        assert_eq!(agent.last_terminal_size, (80, 30));
        assert!(!agent.inline_media_ids.is_empty());
        agent.note_terminal_size((80, 30));
        assert!(!agent.inline_media_ids.is_empty());
        agent.note_terminal_resize();
        assert!(agent.terminal_size_stale);
        agent.note_terminal_size((80, 10));
        assert_eq!(agent.last_terminal_size, (80, 10));
        assert!(agent.inline_media_ids.is_empty());
        assert!(!agent.terminal_size_stale, "draw re-measure ends staleness");
    }
    /// `[Copy source]` copies the diagram source with NO render dispatched
    /// (needs no PNG), in every build; a click outside every hit-rect falls
    /// through. Lazy `[Open]`/`[Copy path]` routing is covered (engine-gated) by
    /// `mermaid_open_click_routes_to_lazy_render`.
    #[test]
    fn mermaid_copy_source_click_copies_without_render() {
        use crate::scrollback::blocks::mermaid_content::AffordanceKind;
        use ratatui::layout::Rect;
        let mut agent = make_agent();
        agent.inline_media_hits.mermaid_sources = vec!["flowchart TD\nA-->B\n".to_string()];
        agent.inline_media_hits.mermaid_buttons =
            vec![(Rect::new(35, 0, 13, 1), AffordanceKind::CopySource, 0)];
        let out = agent.handle_inline_media_click(36, 0);
        assert!(matches!(out, Some(InputOutcome::Changed)));
        let toast = agent
            .toast
            .as_ref()
            .map(|(m, _)| m.clone())
            .unwrap_or_default();
        assert!(
            toast.starts_with("Copied")
                || toast.starts_with("Copy sent")
                || toast.starts_with("Clipboard unreachable")
                || toast.starts_with("Copy failed"),
            "copy-source emits a clipboard toast, got {toast:?}",
        );
        assert!(
            !agent.mermaid_needs_tick(),
            "copy source dispatches no render"
        );
        agent.toast = None;
        assert!(agent.handle_inline_media_click(60, 5).is_none());
    }
    /// `[Open]`/`[Copy path]` route to the lazy render path: with no session dir
    /// (so nowhere to cache a PNG) the request reports "not ready" — proving the
    /// click reached `request_mermaid_render` rather than an eager/open path.
    #[test]
    fn mermaid_open_click_routes_to_lazy_render() {
        use crate::scrollback::blocks::mermaid_content::AffordanceKind;
        use ratatui::layout::Rect;
        for kind in [AffordanceKind::Open, AffordanceKind::CopyPath] {
            let mut agent = make_agent();
            agent.inline_media_hits.mermaid_sources = vec!["flowchart TD\nA-->B\n".to_string()];
            agent.inline_media_hits.mermaid_buttons = vec![(Rect::new(12, 0, 6, 1), kind, 0)];
            let out = agent.handle_inline_media_click(13, 0);
            assert!(matches!(out, Some(InputOutcome::Changed)));
            let toast = agent
                .toast
                .as_ref()
                .map(|(m, _)| m.clone())
                .unwrap_or_default();
            assert!(
                toast.contains("not ready"),
                "{kind:?} routes to the lazy render path (no session dir): {toast:?}",
            );
        }
    }
    /// The real painter (`paint_diagram_affordances`) lays the row from the
    /// single layout source of truth: a leading dim `◇ mermaid` label then the
    /// three always-clickable buttons (each registering a hit-rect carrying the
    /// source), with the buttons shifted right past the label.
    #[test]
    fn paints_affordance_row_with_label_and_registers_all_buttons() {
        use crate::scrollback::blocks::mermaid_content::{AffordanceKind, affordance_row};
        use crate::scrollback::render::DiagramAffordancePlacement;
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        let theme = Theme::current();
        let source = "flowchart TD\nA-->B\n".to_string();
        let layout = affordance_row(false);
        let cols = layout.buttons.map(|b| b.col);
        let mut agent = make_agent();
        let rect = Rect::new(0, 0, 80, 1);
        let mut buf = Buffer::empty(rect);
        agent.paint_diagram_affordances(
            &mut buf,
            vec![DiagramAffordancePlacement {
                screen_rect: rect,
                source: source.clone(),
            }],
            &theme,
        );
        let span = |col: u16, w: u16| -> String {
            (col..col + w)
                .map(|x| buf.cell((x, 0)).unwrap().symbol().to_string())
                .collect()
        };
        assert_eq!(span(0, 9), "\u{25c7} mermaid");
        assert_eq!(
            buf.cell((0, 0)).unwrap().fg,
            theme.gray_dim,
            "the ◇ mermaid label is dim",
        );
        assert_eq!(span(cols[0], 12), "[Open Image]");
        assert_eq!(span(cols[1], 17), "[Copy Image Path]");
        assert_eq!(span(cols[2], 13), "[Copy Source]");
        let buttons = &agent.inline_media_hits.mermaid_buttons;
        assert_eq!(buttons.len(), 3);
        assert_eq!(
            buttons.iter().map(|&(_, k, _)| k).collect::<Vec<_>>(),
            vec![
                AffordanceKind::Open,
                AffordanceKind::CopyPath,
                AffordanceKind::CopySource
            ],
        );
        for (i, &(r, _, idx)) in buttons.iter().enumerate() {
            assert_eq!(r.x, cols[i], "hit-rect aligns with painted column");
            assert_eq!(idx, 0, "all buttons index the one source");
        }
        assert_eq!(agent.inline_media_hits.mermaid_sources, vec![source]);
    }
    /// The hovered button is highlighted (BOLD|UNDERLINED, `text_primary`); every
    /// other button stays at the idle `gray` (brighter than the dim `gray_dim`
    /// label so it stays discoverable), with no bold/underline. With the cursor
    /// off the row, all buttons are idle.
    #[test]
    fn paints_affordance_row_highlights_only_the_hovered_button() {
        use crate::scrollback::blocks::mermaid_content::affordance_row;
        use crate::scrollback::render::DiagramAffordancePlacement;
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        use ratatui::style::Modifier;
        let theme = Theme::current();
        let cols = affordance_row(false).buttons.map(|b| b.col);
        let placement = |rect: Rect| DiagramAffordancePlacement {
            screen_rect: rect,
            source: "A-->B\n".to_string(),
        };
        let rect = Rect::new(0, 0, 80, 1);
        let modifier = |buf: &Buffer, x: u16| buf.cell((x, 0)).unwrap().modifier;
        let highlighted = |buf: &Buffer, x: u16| {
            modifier(buf, x).contains(Modifier::BOLD | Modifier::UNDERLINED)
                && buf.cell((x, 0)).unwrap().fg == theme.text_primary
        };
        let idle = |buf: &Buffer, x: u16| {
            !modifier(buf, x).intersects(Modifier::BOLD | Modifier::UNDERLINED)
                && buf.cell((x, 0)).unwrap().fg == theme.gray
        };
        let mut agent = make_agent();
        agent.last_mouse_pos = (cols[1] + 1, 0);
        let mut buf = Buffer::empty(rect);
        agent.paint_diagram_affordances(&mut buf, vec![placement(rect)], &theme);
        assert!(
            highlighted(&buf, cols[1]),
            "hovered [Copy Image Path] is BOLD|UNDERLINED + text_primary",
        );
        assert!(
            idle(&buf, cols[0]),
            "non-hovered [Open Image] stays idle gray"
        );
        assert!(
            idle(&buf, cols[2]),
            "non-hovered [Copy Source] stays idle gray",
        );
        assert_eq!(buf.cell((0, 0)).unwrap().fg, theme.gray_dim);
        assert!(
            !modifier(&buf, 0).contains(Modifier::BOLD),
            "label not bold"
        );
        let mut agent = make_agent();
        agent.last_mouse_pos = (79, 0);
        let mut buf = Buffer::empty(rect);
        agent.paint_diagram_affordances(&mut buf, vec![placement(rect)], &theme);
        for col in cols {
            assert!(idle(&buf, col), "no hover ⇒ all buttons idle gray");
        }
    }
    /// Narrow rows clip whole segments rather than spilling past the row width:
    /// a row wide enough for the label + `[Open]` paints just those and registers
    /// only `[Open]`'s hit-rect; the clipped buttons register none.
    #[test]
    fn paints_affordance_row_clips_segments_to_row_width() {
        use crate::scrollback::render::DiagramAffordancePlacement;
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        let theme = Theme::current();
        let mut agent = make_agent();
        let rect = Rect::new(0, 0, 24, 1);
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 1));
        agent.paint_diagram_affordances(
            &mut buf,
            vec![DiagramAffordancePlacement {
                screen_rect: rect,
                source: "A-->B\n".to_string(),
            }],
            &theme,
        );
        let row: String = (0..80)
            .filter_map(|x| buf.cell((x, 0)).map(|c| c.symbol()))
            .collect();
        assert!(
            row.contains("[Open Image]"),
            "[Open Image] fits and is painted: {row:?}"
        );
        assert!(
            !row.contains("[Copy"),
            "clipped segments are not painted: {row:?}"
        );
        assert_eq!(
            agent.inline_media_hits.mermaid_buttons.len(),
            1,
            "only the [Open Image] hit-rect is registered",
        );
    }
    /// `render_dropdown_chrome` anchors the items band above the prompt by
    /// default (full TUI) and below it when `below = true` (minimal mode).
    #[test]
    fn dropdown_chrome_anchors_above_or_below_the_prompt() {
        use crate::appearance::LayoutConfig;
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        let theme = Theme::current();
        let layout_cfg = LayoutConfig::default();
        let area = Rect::new(0, 0, 80, 25);
        let prompt = Rect::new(0, 10, 80, 2);
        let item_rows = 3u16;
        let mut buf = Buffer::empty(area);
        let above = render_dropdown_chrome(
            &mut buf,
            3,
            item_rows,
            None,
            prompt,
            area,
            &layout_cfg,
            false,
            false,
            &theme,
        )
        .expect("above chrome fits");
        assert!(
            above.items.y < prompt.y,
            "above: items.y {} should be above prompt.y {}",
            above.items.y,
            prompt.y
        );
        let mut buf = Buffer::empty(area);
        let below = render_dropdown_chrome(
            &mut buf,
            3,
            item_rows,
            None,
            prompt,
            area,
            &layout_cfg,
            false,
            true,
            &theme,
        )
        .expect("below chrome fits");
        assert!(
            below.items.y >= prompt.y + prompt.height,
            "below: items.y {} should be at/after prompt bottom {}",
            below.items.y,
            prompt.y + prompt.height
        );
        assert_eq!(below.items.height, item_rows);
    }
    /// Minimal ("embedded") dropdown chrome is flush-left (W-38): no outer
    /// horizontal padding around the panel and no content inset for the item
    /// rows, so the dropdown's `❯` marker sits at column 0 under the prompt's.
    /// The full TUI keeps the layout hpad + 1-col item inset in its boxed panel.
    #[test]
    #[serial_test::serial]
    fn dropdown_chrome_embedded_is_flush_left() {
        use crate::appearance::LayoutConfig;
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        struct EmbedReset;
        impl Drop for EmbedReset {
            fn drop(&mut self) {
                crate::views::modal_window::set_embedded(false);
            }
        }
        let theme = Theme::current();
        let layout_cfg = LayoutConfig::default();
        let area = Rect::new(0, 0, 80, 25);
        let prompt = Rect::new(0, 10, 80, 2);
        let mut buf = Buffer::empty(area);
        let full = render_dropdown_chrome(
            &mut buf,
            3,
            3,
            None,
            prompt,
            area,
            &layout_cfg,
            false,
            false,
            &theme,
        )
        .expect("full chrome fits");
        assert_eq!(full.panel.x, area.x + layout_cfg.eff_hpad_left(false));
        assert_eq!(full.items.x, prompt.x + crate::glyphs::PROMPT_ARROW_WIDTH);
        let _reset = EmbedReset;
        crate::views::modal_window::set_embedded(true);
        let mut buf = Buffer::empty(area);
        let embedded = render_dropdown_chrome(
            &mut buf,
            3,
            3,
            None,
            prompt,
            area,
            &layout_cfg,
            false,
            true,
            &theme,
        )
        .expect("embedded chrome fits");
        assert_eq!(embedded.panel.x, area.x, "no outer hpad in minimal");
        assert_eq!(embedded.panel.width, area.width, "panel spans full width");
        assert_eq!(
            embedded.items.x, prompt.x,
            "items flush with the prompt's left edge"
        );
        assert_eq!(embedded.items.width, prompt.width);
    }
    /// The tool-media inline-image path (`build_inline_media_escapes`, still live
    /// for tool calls) transmits the bytes (`a=t`) before placing them (`a=p`) on
    /// the first paint, then places only (no re-transmit) on a later frame for
    /// the same path — a place-without-transmit would render blank.
    #[test]
    fn tool_media_first_frame_transmits_then_places_only() {
        use crate::terminal::image::{GraphicsProtocol, set_protocol_for_test};
        let _g = set_protocol_for_test(GraphicsProtocol::Kitty);
        let mut agent = make_agent();
        let path = std::path::PathBuf::from("/tmp/tool-media.png");
        agent
            .inline_media_cache
            .insert(path.clone(), make_test_png(40, 20));
        let placement = tool_media_placement(path.clone());
        let first = agent
            .build_inline_media_escapes(&placement)
            .expect("first frame emits escapes");
        assert!(
            first.contains("\x1b_G"),
            "first frame is a Kitty graphics escape"
        );
        assert!(
            first.contains("a=t"),
            "first frame transmits the bytes (a=t), not place-only: {first:?}",
        );
        assert!(
            first.contains("a=p"),
            "first frame also places the image (a=p)"
        );
        let second = agent
            .build_inline_media_escapes(&placement)
            .expect("second frame emits escapes");
        assert!(second.contains("a=p"), "second frame places the image");
        assert!(
            !second.contains("a=t"),
            "second frame must not re-transmit the bytes: {second:?}",
        );
    }
    fn tool_media_placement(
        path: std::path::PathBuf,
    ) -> crate::scrollback::render::InlineMediaPlacement {
        crate::scrollback::render::InlineMediaPlacement {
            info: crate::prompt_images::InlineMediaInfo {
                path,
                width: 40,
                height: 20,
                is_video: false,
                alt_text: String::new(),
            },
            screen_rect: ratatui::layout::Rect::new(0, 0, 20, 6),
            full_rows: 6,
            top_crop_rows: 0,
            filepath_screen_rect: None,
            open_button_screen_rect: None,
            has_button_row: true,
        }
    }
    /// A place-only frame after the byte cache was dropped with the id kept
    /// (subagent eviction, cap eviction) must reload the bytes from disk
    /// instead of dead-ending on a permanent loading spinner.
    #[test]
    fn tool_media_place_only_frame_reloads_bytes_after_cache_drop() {
        use crate::terminal::image::{GraphicsProtocol, set_protocol_for_test};
        let _g = set_protocol_for_test(GraphicsProtocol::Kitty);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("evicted.png");
        std::fs::write(&path, make_test_png(40, 20)).unwrap();
        let mut agent = make_agent();
        let placement = tool_media_placement(path.clone());
        let first = agent
            .build_inline_media_escapes(&placement)
            .expect("first frame loads from disk and emits escapes");
        assert!(first.contains("a=t"), "first frame transmits: {first:?}");
        agent.inline_media_cache.clear();
        let second = agent
            .build_inline_media_escapes(&placement)
            .expect("place-only frame must reload the bytes, not dead-end");
        assert!(second.contains("a=p"), "reloaded frame places: {second:?}");
        assert!(
            !second.contains("a=t"),
            "id kept → no re-transmit: {second:?}"
        );
        assert!(
            agent.inline_media_cache.contains_key(&path),
            "the reload must repopulate the byte cache"
        );
    }
    /// A missing file keeps polling (generation may still be writing it);
    /// a present-but-undecodable file is negative-cached by its `(len,
    /// mtime)` so decode work doesn't re-run every frame while the file is
    /// unchanged, but a rewrite (e.g. a file caught mid-write, finished
    /// later) self-heals without needing an eviction.
    #[test]
    fn tool_media_load_failure_negative_cached_until_file_changes() {
        use crate::terminal::image::{GraphicsProtocol, set_protocol_for_test};
        let _g = set_protocol_for_test(GraphicsProtocol::Kitty);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.png");
        let mut agent = make_agent();
        let placement = tool_media_placement(path.clone());
        assert!(agent.build_inline_media_escapes(&placement).is_none());
        assert!(!agent.inline_media_load_failed.contains_key(&path));
        std::fs::write(&path, b"not a png").unwrap();
        assert!(agent.build_inline_media_escapes(&placement).is_none());
        assert!(agent.inline_media_load_failed.contains_key(&path));
        assert!(
            agent.build_inline_media_escapes(&placement).is_none(),
            "an unchanged broken file must not be re-decoded every frame"
        );
        std::fs::write(&path, make_test_png(40, 20)).unwrap();
        assert!(
            agent.build_inline_media_escapes(&placement).is_some(),
            "a changed file must retry and recover"
        );
        assert!(
            !agent.inline_media_load_failed.contains_key(&path),
            "a successful load must drop the stale failure marker"
        );
    }
    /// A same-length in-place rewrite whose mtime does not move (coarse
    /// clock) must still retry a negative-cached failure: the Unix stamp
    /// includes inode + ctime, which a rewrite always advances.
    #[cfg(unix)]
    #[test]
    fn tool_media_same_length_same_mtime_rewrite_retries_failed_load() {
        use crate::terminal::image::{GraphicsProtocol, set_protocol_for_test};
        let _g = set_protocol_for_test(GraphicsProtocol::Kitty);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("slow-write.png");
        let png = make_test_png(40, 20);
        let mtime =
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let pin_mtime = |p: &std::path::Path| {
            std::fs::File::options()
                .write(true)
                .open(p)
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(mtime))
                .unwrap();
        };
        std::fs::write(&path, vec![0u8; png.len()]).unwrap();
        pin_mtime(&path);
        let mut agent = make_agent();
        let placement = tool_media_placement(path.clone());
        assert!(agent.build_inline_media_escapes(&placement).is_none());
        assert!(agent.inline_media_load_failed.contains_key(&path));
        std::fs::write(&path, &png).unwrap();
        pin_mtime(&path);
        assert!(
            agent.build_inline_media_escapes(&placement).is_some(),
            "a same-length same-mtime rewrite must retry and recover"
        );
        assert!(!agent.inline_media_load_failed.contains_key(&path));
    }
    /// Draining an agent with live inline-media placements returns delete
    /// escapes for every placed id — including ids placed by subagent
    /// fullscreen views — and resets the tracking state so the next draw
    /// re-transmits (used when another view takes over the frame and the
    /// agent's per-frame clears stop running).
    #[test]
    fn take_inline_media_clear_escapes_drains_placements() {
        let mut agent = make_agent();
        agent
            .inline_media_ids
            .insert(std::path::PathBuf::from("/tmp/a.png"), 2);
        agent
            .inline_media_ids
            .insert(std::path::PathBuf::from("/tmp/b.png"), 3);
        agent.last_placed_ids = [2, 3].into_iter().collect();
        agent.inline_media_active = true;
        let mut child = make_agent();
        child
            .inline_media_ids
            .insert(std::path::PathBuf::from("/tmp/c.png"), 4);
        child.inline_media_active = true;
        agent
            .subagent_views
            .insert("child-sid".into(), Box::new(child));
        let esc = agent
            .take_inline_media_clear_escapes()
            .expect("drains placed media");
        assert!(
            esc.contains(&crate::terminal::image::clear_kitty_image(2)),
            "deletes id 2: {esc:?}"
        );
        assert!(
            esc.contains(&crate::terminal::image::clear_kitty_image(3)),
            "deletes id 3: {esc:?}"
        );
        assert!(
            esc.contains(&crate::terminal::image::clear_kitty_image(4)),
            "deletes the subagent view's id 4: {esc:?}"
        );
        assert!(!agent.inline_media_active);
        assert!(agent.inline_media_ids.is_empty());
        assert!(agent.last_placed_ids.is_empty());
        assert!(agent.inline_video.is_none());
        assert!(agent.take_inline_media_clear_escapes().is_none());
    }
    /// An agent with no placements has nothing to clear.
    #[test]
    fn take_inline_media_clear_escapes_none_when_no_placements() {
        let mut agent = make_agent();
        assert!(agent.take_inline_media_clear_escapes().is_none());
    }
    /// The own-only drain deletes this view's placements but leaves
    /// `subagent_views` untouched, so the fullscreen takeover doesn't force
    /// the active child into a pointless re-transmit.
    #[test]
    fn take_own_inline_media_clear_escapes_leaves_children() {
        let mut agent = make_agent();
        agent
            .inline_media_ids
            .insert(std::path::PathBuf::from("/tmp/a.png"), 2);
        agent.last_placed_ids = [2].into_iter().collect();
        agent.inline_media_active = true;
        let mut child = make_agent();
        child
            .inline_media_ids
            .insert(std::path::PathBuf::from("/tmp/c.png"), 4);
        child.inline_media_active = true;
        agent
            .subagent_views
            .insert("child-sid".into(), Box::new(child));
        let esc = agent
            .take_own_inline_media_clear_escapes()
            .expect("drains own placed media");
        assert!(
            esc.contains(&crate::terminal::image::clear_kitty_image(2)),
            "deletes own id 2: {esc:?}"
        );
        assert!(
            !esc.contains(&crate::terminal::image::clear_kitty_image(4)),
            "must not delete the child's id 4: {esc:?}"
        );
        assert!(!agent.inline_media_active);
        assert!(agent.inline_media_ids.is_empty());
        assert!(agent.last_placed_ids.is_empty());
        let child = agent.subagent_views.get("child-sid").unwrap();
        assert!(child.inline_media_active);
        assert_eq!(child.inline_media_ids.len(), 1);
    }
    /// Draw one 80x30 frame — shared fixture for the subagent-takeover
    /// inline-media regression tests below.
    fn draw_media_frame(agent: &mut AgentView) {
        let registry = ActionRegistry::defaults();
        let area = ratatui::layout::Rect::new(0, 0, 80, 30);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        let mut scratch = crate::scrollback::render::ScratchBuffer::new();
        let bundle = crate::app::bundle::BundleState::default();
        agent.draw(
            area,
            &mut buf,
            &registry,
            &mut scratch,
            None,
            false,
            crate::app::agent_view::BannerSlotParams::none(),
            &bundle,
            false,
            false,
            &mut Vec::new(),
            crate::app::agent_view::AppRenderParams::default(),
        );
    }
    /// The scrolled-off/overlay branch of `AgentView::draw` (render.rs) must
    /// stop live inline playback through `stop_inline_playback` — dropping
    /// the frame set and REQUESTING a post-draw purge, never purging
    /// synchronously mid-frame. Pins the render.rs wiring itself (the
    /// helper alone is covered in media.rs). Serialized: the deferred flag
    /// is process-wide.
    #[test]
    #[serial_test::serial(MEMORY_RELEASE_DEFER)]
    fn scrolled_off_video_stop_requests_post_draw_release() {
        use crate::memory_release::test_support;
        test_support::install_counting_hook();
        crate::memory_release::run_deferred_release();
        let mut agent = make_agent();
        agent.inline_media_active = true;
        agent.inline_video = Some(crate::app::agent_view::InlineVideoState {
            path: std::path::PathBuf::from("/tmp/clip.mp4"),
            frames: vec![Vec::new()],
            current_frame: 0,
            last_frame_time: std::time::Instant::now(),
            fps: 1.0,
            finished: false,
        });
        let before = test_support::calls();
        draw_media_frame(&mut agent);
        assert!(
            agent.inline_video.is_none(),
            "scroll-off branch must stop playback"
        );
        assert_eq!(
            test_support::calls(),
            before,
            "no synchronous purge inside AgentView::draw"
        );
        crate::memory_release::run_deferred_release();
        assert_eq!(
            test_support::calls(),
            before + 1,
            "the post-draw drain must purge the dropped frame set"
        );
    }
    /// Entering the fullscreen subagent view must delete the parent's Kitty
    /// placements: the takeover early-returns before every normal per-frame
    /// clear path, and Kitty images survive cell overdraw, so without the
    /// takeover-time drain the parent's image bleeds through the child view.
    #[test]
    fn subagent_fullscreen_draw_clears_parent_inline_media() {
        let mut agent = make_agent();
        agent
            .inline_media_ids
            .insert(std::path::PathBuf::from("/tmp/a.png"), 2);
        agent.last_placed_ids = [2].into_iter().collect();
        agent.inline_media_active = true;
        agent
            .subagent_views
            .insert("child-sid".into(), Box::new(make_agent()));
        agent.active_subagent = Some("child-sid".into());
        draw_media_frame(&mut agent);
        assert!(
            !agent.inline_media_active,
            "takeover frame must drain the parent's inline media"
        );
        assert!(agent.inline_media_ids.is_empty());
        assert!(agent.last_placed_ids.is_empty());
    }
    /// Symmetric regression: after the fullscreen subagent view closes, the
    /// child's per-frame clears stop running, so the parent's next normal draw
    /// must delete whatever the child placed while fullscreen.
    #[test]
    fn draw_after_subagent_close_clears_child_inline_media() {
        let mut agent = make_agent();
        let mut child = make_agent();
        child
            .inline_media_ids
            .insert(std::path::PathBuf::from("/tmp/c.png"), 4);
        child.inline_media_active = true;
        agent
            .subagent_views
            .insert("child-sid".into(), Box::new(child));
        assert!(agent.active_subagent.is_none(), "subagent view is closed");
        draw_media_frame(&mut agent);
        let child = agent.subagent_views.get("child-sid").unwrap();
        assert!(
            !child.inline_media_active,
            "normal draw must drain a closed subagent view's inline media"
        );
        assert!(child.inline_media_ids.is_empty());
    }
    fn ctrl_v_key() -> KeyEvent {
        key!('v', CONTROL).to_key_event()
    }
    /// Target of the enqueued deferred-probe effect, if any.
    fn deferred_probe_target(
        agent: &AgentView,
    ) -> Option<crate::app::actions::ClipboardPasteTarget> {
        deferred_probe_ctx(agent).map(|ctx| ctx.target)
    }
    /// The `ClipboardPasteContext` of the enqueued deferred-probe effect, if any.
    fn deferred_probe_ctx(agent: &AgentView) -> Option<crate::app::actions::ClipboardPasteContext> {
        agent.pending_effects.iter().find_map(|e| match e {
            crate::app::actions::Effect::ProbeClipboardAttachment { ctx, .. } => Some(ctx.clone()),
            _ => None,
        })
    }
    /// Drive a real Cmd+V through the shipped entry point with the given pbpaste
    /// text and an available, raster-free snapshot (the native snapshot gate skips
    /// the deferred image probe), so a text or file-path paste resolves synchronously.
    fn paste_cmd_v(agent: &mut AgentView, clipboard_text: Option<&str>) -> InputOutcome {
        crate::clipboard::set_clipboard_probe_hook(crate::clipboard::ClipboardProbeHook {
            text: clipboard_text.map(str::to_owned),
            snapshot_supported: Some(true),
            snapshot: Some((Some(1), false)),
            ..Default::default()
        });
        let outcome = agent.handle_prompt_key_for_test(&ctrl_v_key());
        crate::clipboard::clear_clipboard_probe_hook();
        outcome
    }
    /// A `ClipboardPasteContext` matching what a real agent Cmd+V enqueues, for
    /// driving `complete_clipboard_attachment_paste` directly in completion tests.
    fn agent_completion_ctx(
        agent: &AgentView,
        clipboard_text: Option<&str>,
    ) -> crate::app::actions::ClipboardPasteContext {
        crate::app::actions::ClipboardPasteContext {
            target: crate::app::actions::ClipboardPasteTarget::AgentPrompt {
                agent_id: agent.session.id,
                images_dir: None,
                from_feedback_pane: false,
            },
            source: crate::app::actions::ClipboardPasteSource::ClipboardKey {
                text: crate::app::actions::ClipboardTextRead::Success(
                    clipboard_text.map(str::to_owned),
                ),
                tip_showing: false,
            },
        }
    }
    /// A probe the feedback pane started must not attach into whatever
    /// replaced the pane: Esc restores the pre-slash composer draft, and the
    /// completion has to drop the screenshot (and its staged file) instead.
    #[test]
    fn feedback_pane_probe_completing_after_dismissal_is_dropped() {
        let mut agent = make_agent();
        let ctx = crate::app::actions::ClipboardPasteContext {
            target: crate::app::actions::ClipboardPasteTarget::AgentPrompt {
                agent_id: agent.session.id,
                images_dir: None,
                from_feedback_pane: true,
            },
            source: crate::app::actions::ClipboardPasteSource::ClipboardKey {
                text: crate::app::actions::ClipboardTextRead::Success(None),
                tip_showing: false,
            },
        };
        let dir = tempfile::tempdir().unwrap();
        let staged = dir.path().join("staged.png");
        std::fs::write(&staged, b"staged").unwrap();
        let mut pasted = crate::prompt_images::from_clipboard_data(&test_image_data());
        pasted.staged_temp_path = Some(staged.clone());
        let completion = agent.complete_clipboard_attachment_paste(
            ctx,
            crate::app::actions::ProbedAttachment::Image(pasted),
            None,
        );
        assert_eq!(
            completion,
            crate::app::actions::ClipboardPasteCompletion::Dropped
        );
        assert!(
            agent.prompt.images.is_empty(),
            "the screenshot must not become a composer chip"
        );
        assert!(!staged.exists(), "the staged temp file must be deleted");
    }
    /// Drive a real Cmd+V that finds a raster (defers), then complete the probe
    /// with a decoded image — the full shipped image-paste path through the
    /// deferred entry point.
    fn paste_cmd_v_image(agent: &mut AgentView, clipboard_text: Option<&str>) {
        crate::clipboard::set_clipboard_probe_hook(crate::clipboard::ClipboardProbeHook {
            text: clipboard_text.map(str::to_owned),
            ..crate::clipboard::ClipboardProbeHook::with_raster(None)
        });
        let _ = agent.handle_prompt_key_for_test(&ctrl_v_key());
        let ctx = deferred_probe_ctx(agent).expect("an image paste must defer a probe");
        crate::clipboard::clear_clipboard_probe_hook();
        let pasted = crate::prompt_images::from_clipboard_data(&test_image_data());
        agent.complete_clipboard_attachment_paste(
            ctx,
            crate::app::actions::ProbedAttachment::Image(pasted),
            None,
        );
    }
    #[test]
    fn agent_paste_snapshot_no_raster_stays_synchronous() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        crate::clipboard::set_clipboard_probe_hook(crate::clipboard::ClipboardProbeHook {
            text: Some("hello world".to_string()),
            snapshot_supported: Some(true),
            snapshot: Some((Some(1), false)),
            ..Default::default()
        });
        let outcome = agent.handle_prompt_key_for_test(&ctrl_v_key());
        let calls = crate::clipboard::clipboard_probe_call_count();
        let deferred = deferred_probe_target(&agent).is_some();
        crate::clipboard::clear_clipboard_probe_hook();
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(calls, 0, "snapshot with no raster must skip the probe");
        assert!(
            !deferred,
            "plain text with no raster must not defer a probe"
        );
        assert!(agent.prompt.images.is_empty());
        assert_eq!(agent.prompt.text(), "hello world");
    }
    #[test]
    fn agent_paste_snapshot_has_image_defers_probe() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        crate::clipboard::set_clipboard_probe_hook(crate::clipboard::ClipboardProbeHook {
            text: Some("caption".to_string()),
            snapshot_supported: Some(true),
            snapshot: Some((Some(1), true)),
            ..Default::default()
        });
        let outcome = agent.handle_prompt_key_for_test(&ctrl_v_key());
        let calls = crate::clipboard::clipboard_probe_call_count();
        let ctx = deferred_probe_ctx(&agent);
        crate::clipboard::clear_clipboard_probe_hook();
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(calls, 0, "probe must NOT run inline on the event loop");
        let ctx = ctx.expect("an agent-targeted probe effect must be enqueued");
        assert!(matches!(
            ctx.target,
            crate::app::actions::ClipboardPasteTarget::AgentPrompt { .. }
        ));
        assert_eq!(ctx.source.text(), Some("caption"));
        assert!(ctx.source.is_clipboard_key());
        assert_eq!(
            agent.prompt.text(),
            "",
            "caption must not be inserted synchronously"
        );
        assert!(agent.prompt.images.is_empty());
    }
    #[test]
    fn agent_cmd_v_probe_ctx_not_bracketed() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        crate::clipboard::set_clipboard_probe_hook(crate::clipboard::ClipboardProbeHook {
            text: Some("caption".to_string()),
            snapshot_supported: Some(true),
            snapshot: Some((Some(1), true)),
            ..Default::default()
        });
        let _ = agent.handle_prompt_key_for_test(&ctrl_v_key());
        let ctx = deferred_probe_ctx(&agent);
        crate::clipboard::clear_clipboard_probe_hook();
        let ctx = ctx.expect("a Cmd+V with a raster must defer a probe");
        assert!(
            !ctx.source.is_bracketed(),
            "Cmd+V source must remain a CLIPBOARD-key read"
        );
    }
    /// Regression: an IME commit delivered as bracketed paste (Otty)
    /// must not attach the unrelated clipboard image.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn agent_bracketed_paste_stamps_ctx_bracketed() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        let registry = ActionRegistry::defaults();
        crate::clipboard::set_clipboard_probe_hook(
            crate::clipboard::ClipboardProbeHook::with_raster(None),
        );
        let _ = agent.handle_input(&Event::Paste("中".to_owned()), &registry);
        let ctx = deferred_probe_ctx(&agent);
        crate::clipboard::clear_clipboard_probe_hook();
        let ctx = ctx.expect("a short bracketed paste with a raster must defer a probe");
        assert!(
            ctx.source.is_bracketed(),
            "bracketed source must let the probe verify payload origin"
        );
        assert_eq!(ctx.source.text(), Some("中"));
        assert_eq!(
            ctx.source.synchronous_insertion(),
            Some(crate::app::actions::ClipboardTextInsertion::Inserted)
        );
        assert_eq!(agent.prompt.text(), "中");
    }
    #[test]
    fn agent_empty_paste_key_defers_probe() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        crate::clipboard::set_clipboard_probe_hook(crate::clipboard::ClipboardProbeHook {
            snapshot_supported: Some(true),
            snapshot: Some((Some(1), false)),
            ..Default::default()
        });
        let _ = agent.handle_prompt_key_for_test(&ctrl_v_key());
        let calls = crate::clipboard::clipboard_probe_call_count();
        let target = deferred_probe_target(&agent);
        crate::clipboard::clear_clipboard_probe_hook();
        assert_eq!(calls, 0, "probe must NOT run inline on the event loop");
        assert!(matches!(
            target,
            Some(crate::app::actions::ClipboardPasteTarget::AgentPrompt { .. })
        ));
        assert!(agent.prompt.images.is_empty());
    }
    #[test]
    fn failed_clipboard_text_read_is_carried_into_deferred_context() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        crate::clipboard::set_clipboard_probe_hook(crate::clipboard::ClipboardProbeHook {
            text_read_failed: true,
            ..crate::clipboard::ClipboardProbeHook::snapshot_unavailable()
        });
        let _ = agent.handle_prompt_key_for_test(&ctrl_v_key());
        let ctx =
            deferred_probe_ctx(&agent).expect("failed text read must still probe attachments");
        crate::clipboard::clear_clipboard_probe_hook();
        assert!(ctx.source.text_read_failed());
    }
    #[test]
    fn agent_completion_attaches_image_to_prompt() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        let pasted = crate::prompt_images::from_clipboard_data(&test_image_data());
        let ctx = agent_completion_ctx(&agent, Some("caption"));
        let completion = agent.complete_clipboard_attachment_paste(
            ctx,
            crate::app::actions::ProbedAttachment::Image(pasted),
            None,
        );
        assert_eq!(
            completion,
            crate::app::actions::ClipboardPasteCompletion::Handled
        );
        assert_eq!(
            agent.prompt.images.len(),
            1,
            "deferred image attaches on completion"
        );
        assert!(agent.prompt.text().contains("[Image #"));
        assert!(
            !agent.prompt.text().contains("caption"),
            "caption must not be inserted when the probe returns an image: {:?}",
            agent.prompt.text()
        );
    }
    /// Image-wins across the deferral boundary: a Cmd+V with both a caption and a
    /// raster attaches ONLY the image (caption suppressed) — never image + caption.
    #[test]
    fn agent_cmd_v_image_wins_no_double_insert() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        crate::clipboard::set_clipboard_probe_hook(crate::clipboard::ClipboardProbeHook {
            text: Some("caption text".to_string()),
            ..crate::clipboard::ClipboardProbeHook::with_raster(None)
        });
        let _ = agent.handle_prompt_key_for_test(&ctrl_v_key());
        let ctx = deferred_probe_ctx(&agent).expect("probe deferred");
        crate::clipboard::clear_clipboard_probe_hook();
        let pasted = crate::prompt_images::from_clipboard_data(&test_image_data());
        agent.complete_clipboard_attachment_paste(
            ctx,
            crate::app::actions::ProbedAttachment::Image(pasted),
            None,
        );
        assert_eq!(agent.prompt.images.len(), 1);
        assert!(agent.prompt.text().contains("[Image #1]"));
        assert!(
            !agent.prompt.text().contains("caption text"),
            "caption must not double-insert alongside the image: {:?}",
            agent.prompt.text()
        );
    }
    /// No-image miss on an image-wins Cmd+V inserts the carried caption instead.
    #[test]
    fn agent_cmd_v_caption_inserted_on_no_image_miss() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        let ctx = agent_completion_ctx(&agent, Some("caption text"));
        let completion = agent.complete_clipboard_attachment_paste(
            ctx,
            crate::app::actions::ProbedAttachment::NoRaster,
            None,
        );
        assert_eq!(
            completion,
            crate::app::actions::ClipboardPasteCompletion::Handled
        );
        assert!(agent.prompt.images.is_empty());
        assert_eq!(
            agent.prompt.text(),
            "caption text",
            "the carried caption inserts when the probe finds no image"
        );
    }
    #[test]
    fn agent_deferred_caption_survives_failed_or_dropped_probe() {
        use crate::app::actions::{
            ClipboardPasteCompletion, ClipboardPasteFailure, ProbedAttachment,
        };
        for (probe, expected) in [
            (
                ProbedAttachment::ProbeFailed,
                ClipboardPasteCompletion::Failed(ClipboardPasteFailure::AttachmentRead),
            ),
            (
                ProbedAttachment::ProbeDropped,
                ClipboardPasteCompletion::Dropped,
            ),
        ] {
            let mut agent = make_agent();
            agent.set_active_pane(ActivePane::Prompt, true);
            let ctx = agent_completion_ctx(&agent, Some("caption text"));
            let completion = agent.complete_clipboard_attachment_paste(ctx, probe, None);
            assert_eq!(completion, expected);
            assert_eq!(agent.prompt.text(), "caption text");
        }
    }
    #[test]
    fn canonical_text_insertion_handles_same_text_replacement() {
        let mut agent = make_agent();
        agent.prompt.set_text("same text");
        agent.prompt.textarea.set_selection(0, "same text".len());
        let (_, completion) = agent.insert_prompt_plain_text(Some("same text"));
        assert_eq!(
            completion,
            crate::app::actions::ClipboardTextInsertion::Inserted
        );
        assert_eq!(agent.prompt.text(), "same text");
    }
    #[test]
    fn empty_and_whitespace_bracketed_completion_are_not_handled() {
        use crate::app::actions::{
            ClipboardPasteCompletion, ClipboardTextInsertion, ProbedAttachment,
        };
        for text in ["", " \n\t"] {
            let mut agent = make_agent();
            let (_, synchronous) = agent.insert_bracketed_prompt_text(text);
            assert_eq!(synchronous, ClipboardTextInsertion::Empty);
            let mut ctx = agent_completion_ctx(&agent, Some(text));
            ctx.source = crate::app::actions::ClipboardPasteSource::BracketedInserted {
                text: text.to_owned(),
                insertion: synchronous,
            };
            let completion =
                agent.complete_clipboard_attachment_paste(ctx, ProbedAttachment::NoRaster, None);
            assert_eq!(
                completion,
                ClipboardPasteCompletion::FullMiss,
                "bracketed payload {text:?}"
            );
        }
    }
    #[test]
    fn bracketed_probe_failure_or_drop_wins_over_synchronous_text() {
        use crate::app::actions::{
            ClipboardPasteCompletion, ClipboardPasteFailure, ClipboardTextInsertion,
            ProbedAttachment,
        };
        for (probe, expected) in [
            (
                ProbedAttachment::ProbeFailed,
                ClipboardPasteCompletion::Failed(ClipboardPasteFailure::AttachmentRead),
            ),
            (
                ProbedAttachment::ProbeDropped,
                ClipboardPasteCompletion::Dropped,
            ),
        ] {
            let mut agent = make_agent();
            let (_, synchronous) = agent.insert_bracketed_prompt_text("inserted");
            assert_eq!(synchronous, ClipboardTextInsertion::Inserted);
            let mut ctx = agent_completion_ctx(&agent, Some("inserted"));
            ctx.source = crate::app::actions::ClipboardPasteSource::BracketedInserted {
                text: "inserted".to_owned(),
                insertion: synchronous,
            };
            let completion = agent.complete_clipboard_attachment_paste(ctx, probe, None);
            assert_eq!(completion, expected);
        }
    }
    #[test]
    fn failed_clipboard_text_read_cannot_become_full_miss() {
        let mut agent = make_agent();
        let mut ctx = agent_completion_ctx(&agent, None);
        ctx.source = crate::app::actions::ClipboardPasteSource::ClipboardKey {
            text: crate::app::actions::ClipboardTextRead::Failed,
            tip_showing: false,
        };
        let completion = agent.complete_clipboard_attachment_paste(
            ctx,
            crate::app::actions::ProbedAttachment::NoRaster,
            None,
        );
        assert_eq!(
            completion,
            crate::app::actions::ClipboardPasteCompletion::Failed(
                crate::app::actions::ClipboardPasteFailure::TextRead,
            )
        );
    }
    #[test]
    fn agent_completion_inserts_unreadable_file_url_as_path_text() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        let ctx = agent_completion_ctx(&agent, None);
        let completion = agent.complete_clipboard_attachment_paste(
            ctx,
            crate::app::actions::ProbedAttachment::NoRaster,
            Some("file:///definitely/missing/echo-agent-primary-paste.png".to_owned()),
        );
        assert_eq!(
            completion,
            crate::app::actions::ClipboardPasteCompletion::Handled
        );
        assert_eq!(
            agent.prompt.text(),
            "/definitely/missing/echo-agent-primary-paste.png "
        );
        assert!(agent.prompt.images.is_empty());
    }
    #[test]
    fn agent_completion_distinguishes_dropped_and_failed_probes() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        let ctx = agent_completion_ctx(&agent, None);
        let dropped = agent.complete_clipboard_attachment_paste(
            ctx,
            crate::app::actions::ProbedAttachment::ProbeDropped,
            None,
        );
        let ctx = agent_completion_ctx(&agent, None);
        let failed = agent.complete_clipboard_attachment_paste(
            ctx,
            crate::app::actions::ProbedAttachment::ProbeFailed,
            None,
        );
        let ctx = agent_completion_ctx(&agent, None);
        let persist_failed = agent.complete_clipboard_attachment_paste(
            ctx,
            crate::app::actions::ProbedAttachment::PersistFailed("disk full".to_owned()),
            None,
        );
        assert_eq!(
            dropped,
            crate::app::actions::ClipboardPasteCompletion::Dropped
        );
        assert_eq!(
            failed,
            crate::app::actions::ClipboardPasteCompletion::Failed(
                crate::app::actions::ClipboardPasteFailure::AttachmentRead,
            )
        );
        assert_eq!(
            persist_failed,
            crate::app::actions::ClipboardPasteCompletion::Failed(
                crate::app::actions::ClipboardPasteFailure::AlreadyReported,
            )
        );
    }
    /// The `ContextualTip { ImageInput, Accepted }` funnel survives the deferral:
    /// a Cmd+V while the clipboard-image tip is showing emits the accept event
    /// when the deferred probe attaches an image.
    #[test]
    fn agent_cmd_v_tip_accept_emitted_on_deferred_image() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        let _ = agent.ephemeral_tip.show(
            crate::tips::clipboard_focus::clipboard_image_tip(),
            &mut std::collections::HashMap::new(),
        );
        crate::clipboard::set_clipboard_probe_hook(
            crate::clipboard::ClipboardProbeHook::with_raster(None),
        );
        let _ = agent.handle_prompt_key_for_test(&ctrl_v_key());
        let ctx = deferred_probe_ctx(&agent).expect("probe deferred");
        crate::clipboard::clear_clipboard_probe_hook();
        assert!(
            ctx.source.tip_showing(),
            "tip-showing must thread into the deferred ctx"
        );
        let pasted = crate::prompt_images::from_clipboard_data(&test_image_data());
        agent.complete_clipboard_attachment_paste(
            ctx,
            crate::app::actions::ProbedAttachment::Image(pasted),
            None,
        );
        assert_eq!(agent.prompt.images.len(), 1);
    }
}
