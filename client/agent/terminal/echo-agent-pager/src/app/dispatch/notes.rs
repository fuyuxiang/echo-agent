//! Feedback, remember-note, btw, and recap dispatchers.

use super::ctx::{NO_SESSION_NOTICE, with_active_agent};
use crate::app::actions::{Effect, FeedbackTraceChoice};
use crate::app::agent::AgentId;
use crate::app::agent_view::{AgentView, PromptInputMode};
use crate::app::app_view::{ActiveView, AppView};
use crate::scrollback::block::RenderBlock;
use crate::scrollback::blocks::{SessionEvent, ToolCallBlock};
use crate::views::question_view::{LocalQuestionKind, QuestionViewState};
use echo_agent_tools::implementations::echo_agent_build::ask_user_question::Question;
use std::sync::atomic::{AtomicU64, Ordering};

/// Monotonic counter for correlating async rewrite responses with the modal
/// that requested them. Prevents stale results from populating a different
/// note's review modal when the user closes and re-opens quickly.
static REWRITE_NONCE: AtomicU64 = AtomicU64::new(0);

fn next_rewrite_nonce() -> u64 {
    REWRITE_NONCE.fetch_add(1, Ordering::Relaxed)
}

pub(crate) use crate::views::question_view::FEEDBACK_QUESTION_LABEL;

/// Minimal mode has no toast surface, so the notice goes to the transcript instead.
fn feedback_notice(app: &mut AppView, message: &str) {
    if app.screen_mode.is_minimal() {
        with_active_agent(app, |agent| {
            agent
                .scrollback
                .push_block(RenderBlock::system(message.to_string()));
        });
    } else {
        app.show_toast(message);
    }
}

/// Why the bare `/feedback` pane refuses to open, if anything blocks it.
fn feedback_pane_blocked(agent: &AgentView) -> Option<&'static str> {
    if agent.active_subagent.is_some() {
        // A fullscreen subagent view hides the prompt, so the pane would have nowhere to draw while still swallowing every key.
        Some("Close the subagent view before sending feedback")
    } else if agent.question_view.is_some() {
        Some("Finish answering the current question first")
    } else if !agent.no_input_overlay_pending()
        || agent.key_owner() != crate::app::agent_view::KeyOwner::Pane
    {
        // Two ways the pane cannot work here. A permission or plan approval holds the composer, even parked in the scrollback, so the
        // pane would hand it the wrong draft on the way out. A viewer outranks every card for keys, so the box would be untypeable.
        Some("Close or answer what's open before sending feedback")
    } else if agent.session.session_id.is_none() {
        Some(NO_SESSION_NOTICE)
    } else {
        None
    }
}

/// Open the freeform report pane. Early exits drop `images`, whose owner
/// cleans up the staged temp files.
pub(super) fn dispatch_open_feedback_pane(
    app: &mut AppView,
    prefill: Option<String>,
    mut images: crate::views::prompt_widget::FeedbackImages,
) -> Vec<Effect> {
    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };

    let blocked = {
        let Some(agent) = app.agents.get(&id) else {
            return vec![];
        };
        feedback_pane_blocked(agent)
    };
    if let Some(message) = blocked {
        feedback_notice(app, message);
        return vec![];
    }

    // An individual coding-data opt-out does not suppress the offer: the card
    // is how opted-out users switch sharing back on. ZDR/team locks have no
    // self-serve path, so they still suppress it. Minimal mode never offers:
    // its `/feedback <text>` path documents sends without a consent card.
    let offer_trace = app.feedback_trace_offer()
        && app.coding_data_sharing_lock().is_none()
        && app.team_name.is_none()
        && !app.is_zdr
        && !app.screen_mode.is_minimal();
    let offer_reenables_sharing = app.coding_data_retention_opt_out;
    let Some(agent) = app.agents.get_mut(&id) else {
        return vec![];
    };
    let question = Question {
        question: FEEDBACK_QUESTION_LABEL.to_string(),
        options: vec![],
        multi_select: Some(false),
        id: None,
    };
    let stashed = agent.prompt.stash();
    let mut state = QuestionViewState::new(
        format!("feedback-{}", uuid::Uuid::new_v4()),
        vec![question],
        stashed,
    )
    .with_local_kind(LocalQuestionKind::Feedback);
    state.feedback_offer_trace = offer_trace;
    state.feedback_offer_reenables_sharing = offer_reenables_sharing;
    let prefill_text = prefill.filter(|s| !s.is_empty());
    if let Some(text) = prefill_text.as_ref()
        && let Some(slot) = state.per_question_freeform.get_mut(0)
    {
        *slot = text.clone();
    }
    let freeform = state.activate_freeform_input();
    agent.prompt.set_text_preserving(&freeform);
    // Inline `/feedback` composed alongside pasted images: the prefill kept
    // their `[Image #N]` placeholders as plain text, so rebind the drained
    // records to them and the pane shows live, removable chips.
    let image_count = images.len();
    agent.prompt.adopt_images(images.take());

    let session_id = agent.session.session_id.clone();
    let report = prefill_text.unwrap_or_default();
    crate::unified_log::info(
        "feedback.pane_open",
        session_id.as_ref().map(|s| s.0.as_ref()),
        Some(serde_json::json!({
            "prefill_chars": report.chars().count(),
            "prefill_images": image_count,
            "offer_trace": offer_trace,
            "screen_mode": app.screen_mode.meta_label(),
        })),
    );
    agent.question_view = Some(state);
    vec![]
}

/// How long the background trace upload may run before it is reported as
/// failed; longer than the shell's own upload timeout so its error wins.
pub(crate) const FEEDBACK_TRACE_UPLOAD_TIMEOUT_MS: u64 = 150_000;

/// The `[telemetry] trace_upload = true` write the /feedback card's "Yes"
/// collects.
pub(super) fn persist_trace_upload_consent() -> Effect {
    Effect::PersistSetting {
        key: "trace_upload",
        value: crate::settings::SettingValue::Bool(true),
        rollback_value: crate::settings::SettingValue::Bool(false),
    }
}

/// Enter remember mode: visual change to prompt bar (remember accent, `#` prefix).
/// No side effects — the user types a memory note and presses Enter to send.
pub(super) fn dispatch_enter_remember_mode(app: &mut AppView) -> Vec<Effect> {
    with_active_agent(app, |agent| {
        agent.prompt_input_mode = PromptInputMode::Remember;
        agent.prompt.set_text("");
    });
    vec![]
}

/// Close the trace-consent funnel opened by `FeedbackTraceCardShown`. Every
/// outcome of a shown card (answer, Esc, displacement) must log exactly once.
pub(crate) fn log_trace_consent_selected(reenables_sharing: bool, choice: FeedbackTraceChoice) {
    use echo_agent_telemetry::events::{FeedbackTraceConsentChoice, FeedbackTraceConsentSelected};
    echo_agent_telemetry::session_ctx::log_event(FeedbackTraceConsentSelected {
        choice: match choice {
            FeedbackTraceChoice::AlwaysUpload => FeedbackTraceConsentChoice::TurnOn,
            FeedbackTraceChoice::NeverAsk => FeedbackTraceConsentChoice::NeverAsk,
            FeedbackTraceChoice::NoUpload => FeedbackTraceConsentChoice::NoUpload,
        },
        reenables_sharing,
    });
}

/// The `feedback.send` unified log plus the POST effect for a committed
/// report. Single writer for both, shared with the displaced-card path.
pub(crate) fn feedback_send_effect(
    agent_id: AgentId,
    session_id: agent_client_protocol::SessionId,
    text: String,
    images: Vec<echo_agent_runtime::session::FeedbackImage>,
    trace: Option<FeedbackTraceChoice>,
    displaced: bool,
) -> Effect {
    let mut payload = serde_json::json!({
        "chars": text.chars().count(),
        "images": images.len(),
        "trace": match trace {
            Some(choice) => format!("{choice:?}"),
            None => "NotOffered".to_string(),
        },
    });
    if displaced {
        payload["displaced"] = serde_json::Value::Bool(true);
    }
    crate::unified_log::info("feedback.send", Some(session_id.0.as_ref()), Some(payload));
    Effect::SendFeedback {
        agent_id,
        session_id,
        feedback_text: text,
        images,
    }
}

/// Commit a report: encode images (with the dropped-attachment notice), close
/// the consent funnel, apply the "no text and no surviving images means do not
/// send" rule, and build the send effect. The single owner of that policy,
/// shared by [`dispatch_send_feedback`] and the displaced-consent-card path in
/// `acp_handler`. `displaced` selects that path's user-facing copy.
pub(crate) fn commit_feedback(
    agent: &mut crate::app::agent_view::AgentView,
    coding_data_retention_opt_out: bool,
    id: AgentId,
    session_id: agent_client_protocol::SessionId,
    text: String,
    images: crate::views::prompt_widget::FeedbackImages,
    trace: Option<FeedbackTraceChoice>,
    displaced: bool,
) -> Option<Effect> {
    // Encode before the emptiness check: encoding can drop attachments, and
    // a report left with no text and no images must not go out blank.
    let (encoded_images, dropped) = encode_feedback_images(images);
    if let Some(notice) = dropped {
        agent.scrollback.push_block(RenderBlock::system(notice));
    }

    // A shown consent card closes its funnel exactly once, sent or not.
    if let Some(choice) = trace {
        log_trace_consent_selected(coding_data_retention_opt_out, choice);
    }

    let trimmed = text.trim().to_string();
    if trimmed.is_empty() && encoded_images.is_empty() {
        agent.scrollback.push_block(RenderBlock::system(
            if displaced {
                "/feedback cancelled because another question opened."
            } else {
                "Please provide feedback text."
            }
            .to_string(),
        ));
        return None;
    }

    agent.scrollback.push_block(RenderBlock::system(
        if displaced {
            "Another question interrupted /feedback. Your report was sent without a trace."
        } else {
            "Thanks for the feedback! The EchoAgent Build team is on it."
        }
        .to_string(),
    ));

    Some(feedback_send_effect(
        id,
        session_id,
        trimmed,
        encoded_images,
        trace,
        displaced,
    ))
}

/// Thank-you is shown immediately; POST is a background effect. The composer is not cleared: the text arrives with the action, not from the prompt.
/// Early exits drop `images`, whose owner cleans up the staged temp files.
pub(super) fn dispatch_send_feedback(
    app: &mut AppView,
    text: String,
    images: crate::views::prompt_widget::FeedbackImages,
    trace: Option<FeedbackTraceChoice>,
) -> Vec<Effect> {
    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    let coding_data_retention_opt_out = app.coding_data_retention_opt_out;
    let Some(agent) = app.agents.get_mut(&id) else {
        return vec![];
    };

    agent.ephemeral_tip.clear_on_submit();

    let Some(session_id) = agent.session.session_id.clone() else {
        agent
            .scrollback
            .push_block(RenderBlock::system(NO_SESSION_NOTICE.to_string()));
        return vec![];
    };

    let Some(send) = commit_feedback(
        agent,
        coding_data_retention_opt_out,
        id,
        session_id.clone(),
        text,
        images,
        trace,
        false,
    ) else {
        // Nothing went out, so no trace-upload side effects either.
        return vec![];
    };

    let mut effects = vec![send];
    match trace {
        None | Some(FeedbackTraceChoice::NoUpload) => {}
        Some(FeedbackTraceChoice::NeverAsk) => {
            app.feedback_trace_choice_latched = true;
            effects.push(Effect::PersistSetting {
                key: "feedback_trace_card",
                value: crate::settings::SettingValue::Bool(false),
                rollback_value: crate::settings::SettingValue::Bool(false),
            });
        }
        Some(FeedbackTraceChoice::AlwaysUpload) => {
            app.feedback_trace_choice_latched = true;
            // The storage proxy rejects uploads while the account is opted
            // out of coding-data sharing, so an opted-out account flips
            // sharing first and parks the upload on that write generation.
            let mut park_seq = None;
            if app.coding_data_retention_opt_out {
                let (sharing, outcome) = super::status::set_coding_data_sharing_tracked(
                    app,
                    true,
                    echo_agent_telemetry::events::CodingDataConsentSource::FeedbackTraceCard,
                );
                effects.extend(sharing);
                match outcome {
                    super::status::SharingWriteOutcome::Claimed(seq) => park_seq = Some(seq),
                    // Sharing is already on (the opt-out mirror was stale):
                    // nothing to wait on, upload now.
                    super::status::SharingWriteOutcome::AlreadySet => {}
                    // The guard refused the opt-in this consent depends on:
                    // send the report alone — no upload, no persisted
                    // consent, and no latch, since nothing happened.
                    super::status::SharingWriteOutcome::Refused => {
                        app.feedback_trace_choice_latched = false;
                        return effects;
                    }
                }
            }
            match park_seq {
                // The consent persist also waits for the confirm: written
                // now, a failed opt-in would leave `trace_upload = true` on
                // disk while the storage proxy keeps rejecting uploads.
                Some(seq) => {
                    app.feedback_trace_upload_pending =
                        Some(crate::app::app_view::PendingFeedbackTraceUpload {
                            seq,
                            agent_id: id,
                            session_id,
                        });
                }
                None => {
                    effects.push(Effect::UploadFeedbackTrace {
                        agent_id: id,
                        session_id,
                    });
                    effects.push(persist_trace_upload_consent());
                }
            }
        }
    }
    effects
}

/// The `#` composer path. Nothing else records the note, so this records it.
pub(super) fn dispatch_send_remember_note(app: &mut AppView, text: String) -> Vec<Effect> {
    send_remember_note(app, text, true)
}

/// The `/remember <text>` path. `dispatch_send_prompt_inner` already recorded the typed command.
pub(super) fn dispatch_send_remember_note_from_command(
    app: &mut AppView,
    text: String,
) -> Vec<Effect> {
    send_remember_note(app, text, false)
}

fn encode_feedback_images(
    images: crate::views::prompt_widget::FeedbackImages,
) -> (
    Vec<echo_agent_runtime::session::FeedbackImage>,
    Option<String>,
) {
    use base64::Engine as _;
    use echo_agent_runtime::session::{
        MAX_FEEDBACK_IMAGE_BYTES, MAX_FEEDBACK_IMAGE_TOTAL_BYTES, MAX_FEEDBACK_IMAGES,
        feedback_image_extension,
    };

    let mut encoded = Vec::new();
    let mut over_count = 0usize;
    let mut unsupported = 0usize;
    let mut too_large = 0usize;
    let mut unreadable = 0usize;
    let mut total_bytes = 0usize;
    for image in images.as_slice() {
        let Some((bytes, mime_type)) = crate::prompt_images::load_for_send(image) else {
            unreadable += 1;
            continue;
        };
        if encoded.len() >= MAX_FEEDBACK_IMAGES {
            over_count += 1;
            continue;
        }
        if feedback_image_extension(&mime_type).is_none() {
            unsupported += 1;
            continue;
        }
        if bytes.len() > MAX_FEEDBACK_IMAGE_BYTES
            || total_bytes + bytes.len() > MAX_FEEDBACK_IMAGE_TOTAL_BYTES
        {
            too_large += 1;
            continue;
        }
        total_bytes += bytes.len();
        encoded.push(echo_agent_runtime::session::FeedbackImage {
            data: base64::engine::general_purpose::STANDARD.encode(&bytes),
            mime_type,
            file_name: image
                .source_path
                .as_deref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned()),
        });
    }
    // Encoding done with the records; dropping the owner deletes the
    // staged temp files.
    drop(images);

    let dropped = over_count + unsupported + too_large + unreadable;
    let notice = (dropped > 0).then(|| {
        const MIB: usize = 1024 * 1024;
        let mut reasons = Vec::new();
        if over_count > 0 {
            reasons.push(format!(
                "{over_count} over the {MAX_FEEDBACK_IMAGES}-image limit"
            ));
        }
        if unsupported > 0 {
            reasons.push(format!(
                "{unsupported} in a format feedback can't carry (PNG, JPEG, or GIF only)"
            ));
        }
        if too_large > 0 {
            reasons.push(format!(
                "{too_large} over the size limit ({} MB each, {} MB combined)",
                MAX_FEEDBACK_IMAGE_BYTES / MIB,
                MAX_FEEDBACK_IMAGE_TOTAL_BYTES / MIB,
            ));
        }
        if unreadable > 0 {
            reasons.push(format!("{unreadable} unreadable"));
        }
        let plural = if dropped == 1 { "" } else { "s" };
        format!(
            "Dropped {dropped} image{plural} from the feedback: {}.",
            reasons.join(", ")
        )
    });
    (encoded, notice)
}

/// Send a raw remember note for LLM-powered rewriting via `echo.agent/memory/rewrite`.
/// Clears remember mode and prompts the LLM to reformat the note with session
/// context. Falls back to direct `SaveMemoryNote` when no session is available.
fn send_remember_note(app: &mut AppView, text: String, record_in_history: bool) -> Vec<Effect> {
    use crate::views::modal::ActiveModal;

    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    let Some(agent) = app.agents.get_mut(&id) else {
        return vec![];
    };

    agent.prompt_input_mode = PromptInputMode::Normal;
    agent.prompt.set_text("");
    // Submitting a memory note retires any edit-contextual ephemeral tip.
    agent.ephemeral_tip.clear_on_submit();

    let trimmed = text.trim().to_string();
    if trimmed.is_empty() {
        agent.scrollback.push_block(RenderBlock::system(
            "Please provide a memory note.".to_string(),
        ));
        return vec![];
    }

    agent.note_draft_consumed();
    if record_in_history {
        // Stored without the `#`. Recall decodes a prefix back into its mode, which would turn `# Context` into a note.
        agent.record_prompt_in_history(&trimmed);
    }

    let cwd = agent.session.cwd.clone();

    let Some(session_id) = agent.session.session_id.clone() else {
        // No session — open modal with raw content only (no LLM rewrite).
        agent.active_modal = Some(ActiveModal::RememberNoteReview {
            raw_content: trimmed.clone(),
            enhanced_content: None, // no session → no LLM rewrite, Tab disabled
            showing_enhanced: false,
            scroll: 0,
            window: crate::views::modal_window::ModalWindowState::new(),
            cached_lines: None,
            cwd,
            agent_id: id,
            rewrite_nonce: Default::default(), // no rewrite in flight, nonce unused
        });
        return vec![];
    };

    // Open modal with raw content, LLM rewrite in flight.
    let nonce = next_rewrite_nonce();
    agent.active_modal = Some(ActiveModal::RememberNoteReview {
        raw_content: trimmed.clone(),
        enhanced_content: None,
        showing_enhanced: false,
        scroll: 0,
        window: crate::views::modal_window::ModalWindowState::new(),
        cached_lines: None,
        cwd: cwd.clone(),
        agent_id: id,
        rewrite_nonce: nonce,
    });

    let context_summary = extract_session_context(agent);

    vec![Effect::RewriteMemoryNote {
        agent_id: id,
        session_id,
        raw_text: trimmed,
        context_summary,
        nonce,
    }]
}

/// Save the currently displayed remember note from the review modal.
pub(super) fn dispatch_save_remember_note_from_modal(app: &mut AppView) -> Vec<Effect> {
    use crate::views::modal::ActiveModal;

    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    let Some(agent) = app.agents.get_mut(&id) else {
        return vec![];
    };

    let (content, cwd) = if let Some(ActiveModal::RememberNoteReview {
        ref raw_content,
        ref enhanced_content,
        showing_enhanced,
        ref cwd,
        ..
    }) = agent.active_modal
    {
        let text = if showing_enhanced {
            enhanced_content.as_deref().unwrap_or(raw_content)
        } else {
            raw_content
        };
        (text.trim().to_string(), cwd.clone())
    } else {
        return vec![];
    };

    agent.active_modal = None;
    agent
        .scrollback
        .push_block(RenderBlock::system("Saving memory note...".to_string()));

    vec![Effect::SaveMemoryNote {
        agent_id: id,
        text: content,
        cwd,
    }]
}

/// Extract session context for the LLM memory rewrite request.
///
/// Walks scrollback in reverse, collecting:
/// - Last 5 user prompts
/// - File paths from recent tool calls (Read, Edit, ListDir)
/// - CWD and git branch
fn extract_session_context(agent: &AgentView) -> String {
    let mut user_prompts: Vec<String> = Vec::new();
    let mut file_paths: Vec<String> = Vec::new();

    // Walk scrollback entries in reverse to collect recent context.
    let len = agent.scrollback.len();
    for i in (0..len).rev() {
        let Some(entry) = agent.scrollback.entry(i) else {
            continue;
        };
        match &entry.block {
            RenderBlock::UserPrompt(prompt) => {
                if user_prompts.len() < 5 {
                    let text = if prompt.text.len() > 200 {
                        let end = prompt
                            .text
                            .char_indices()
                            .map(|(i, _)| i)
                            .take_while(|&i| i <= 200)
                            .last()
                            .unwrap_or(0);
                        format!("{}...", &prompt.text[..end])
                    } else {
                        prompt.text.clone()
                    };
                    user_prompts.push(text);
                }
            }
            RenderBlock::ToolCall(tc) => {
                if file_paths.len() < 20 {
                    match tc {
                        ToolCallBlock::Read(b) => {
                            file_paths.push(b.path.clone());
                        }
                        ToolCallBlock::Edit(b) => {
                            file_paths.push(b.path.clone());
                        }
                        ToolCallBlock::ListDir(b) => {
                            file_paths.push(b.path.clone());
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        // Stop early once we have enough context.
        if user_prompts.len() >= 5 && file_paths.len() >= 20 {
            break;
        }
    }

    let mut parts: Vec<String> = Vec::new();

    // CWD
    parts.push(format!("CWD: {}", agent.session.cwd.display()));

    // Git branch
    if let Some(ref branch) = agent.current_branch {
        parts.push(format!("Branch: {branch}"));
    }

    // Recent prompts (chronological order)
    if !user_prompts.is_empty() {
        user_prompts.reverse();
        parts.push("Recent prompts:".to_string());
        for p in &user_prompts {
            parts.push(format!("- {p}"));
        }
    }

    // Recent file paths (deduplicated, preserving first-seen order)
    if !file_paths.is_empty() {
        let mut seen = std::collections::HashSet::new();
        file_paths.retain(|p| seen.insert(p.clone()));
        parts.push("Recent files:".to_string());
        for p in &file_paths {
            parts.push(format!("- {p}"));
        }
    }

    parts.join("\n")
}

/// Send a /btw side question. Bypasses the prompt queue — works even while
/// the agent is mid-turn. Fires an ACP ext method and shows a loading overlay.
pub(super) fn dispatch_send_btw(app: &mut AppView, question: String) -> Vec<Effect> {
    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    let minimal = app.screen_mode.is_minimal();
    let (session_id, minimal_request_id) = {
        let Some(agent) = app.agents.get_mut(&id) else {
            return vec![];
        };
        let Some(session_id) = agent.session.session_id.clone() else {
            if minimal {
                agent
                    .scrollback
                    .push_block(crate::scrollback::block::RenderBlock::system(
                        NO_SESSION_NOTICE,
                    ));
            } else {
                agent.show_toast(NO_SESSION_NOTICE);
            }
            return vec![];
        };

        // Composer clearing belongs to the submit funnel: `dispatch_send_prompt_inner` clears it
        // when `consume_input` is set, so draft-preserving callers (palette, edited
        // queue row) keep theirs.
        let minimal_request_id = if minimal {
            Some(crate::minimal_api::start_minimal_btw(
                agent,
                question.clone(),
            ))
        } else {
            agent.btw_state = Some(crate::views::btw_overlay::BtwOverlayState::Loading {
                question: question.clone(),
            });
            // Prompt keeps focus while the answer is in flight (panel focuses on Done).
            agent.btw_focused = false;
            None
        };
        (session_id, minimal_request_id)
    };

    vec![Effect::SendBtw {
        agent_id: id,
        session_id,
        question,
        minimal_request_id,
    }]
}

/// Toast when a manual `/recap` produces no summary. Empty sessions get a clear
/// empty-state message; anything else (model failure, empty summary, etc.) keeps
/// the generic failure toast.
pub(crate) fn recap_unavailable_toast(has_user_messages: bool) -> &'static str {
    if has_user_messages {
        "Couldn't generate recap"
    } else {
        "No messages yet"
    }
}

/// Whether scrollback already has a user prompt. Scans entries (not
/// `turn_count`) so it stays correct during `begin_batch`/`end_batch` session
/// load, when `push` defers `rebuild_turns` and `turn_count` can stay 0 while
/// replayed prompts are already present.
pub(crate) fn scrollback_has_user_messages(
    scrollback: &crate::scrollback::state::ScrollbackState,
) -> bool {
    scrollback
        .iter_entries()
        .any(|(_, entry)| entry.block.is_user_prompt())
}

/// Request a session recap. Bypasses the prompt queue — works even while the
/// agent is mid-turn. Fires the `echo.agent/recap` ext method; the recap arrives
/// asynchronously as a `SessionRecap` notification (rendered in scrollback).
///
/// `auto` is `false` for an explicit `/recap` and `true` for the automatic
/// return-from-away recap. For the manual path we clear the prompt and, when
/// no session exists yet, surface a toast; the auto path is best-effort and
/// silently no-ops without an active session.
pub(super) fn dispatch_send_recap(app: &mut AppView, auto: bool) -> Vec<Effect> {
    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    let Some(agent) = app.agents.get_mut(&id) else {
        return vec![];
    };

    // Shell is authoritative (remote settings / config / env). Skip client requests
    // entirely when the feature is off so we never hit `echo.agent/recap`.
    if !app.session_recap_available {
        if !auto {
            agent.show_toast("Session recap is not enabled");
        }
        return vec![];
    }

    let Some(session_id) = agent.session.session_id.clone() else {
        if !auto {
            agent.show_toast(NO_SESSION_NOTICE);
        }
        return vec![];
    };

    if !auto {
        agent.prompt.set_text("");
        // Nothing to summarize yet — show a clear empty-state toast instead of
        // a spinner that ends in "Couldn't generate recap".
        //
        // Skip the short-circuit while session replay is still loading (prompts
        // may not have arrived yet). Prefer an entry scan over `turn_count()`
        // so mid-batch resume (deferred `rebuild_turns`) still sees history.
        if !agent.session.loading_replay && !scrollback_has_user_messages(&agent.scrollback) {
            agent.show_toast(recap_unavailable_toast(false));
            return vec![];
        }
        // Show an immediate loading block with the animated "running" sidebar so
        // the user has feedback that a recap is being generated. The
        // `SessionRecap` handler fills this entry in and stops the animation.
        // Reuse an existing in-flight loading block instead of stacking spinners
        // when `/recap` is pressed repeatedly.
        let already_loading = agent.pending_recap_entry.is_some_and(|eid| {
            agent
                .scrollback
                .get_by_id(eid)
                .is_some_and(|entry| entry.is_running)
        });
        if !already_loading {
            let entry_id =
                agent
                    .scrollback
                    .push(crate::scrollback::entry::ScrollbackEntry::running(
                        RenderBlock::session_event(SessionEvent::Recap {
                            summary: String::new(),
                            auto: false,
                        }),
                    ));
            agent.pending_recap_entry = Some(entry_id);
        }
    } else {
        // Retry backoff only — do not consume the away period on dispatch.
        // The shell often no-ops auto recap until ≥3 min since the last main
        // turn; mark_recap_shown runs when any SessionRecap arrives (auto or
        // manual `/recap`).
        app.notification_service
            .focus_tracker
            .note_auto_recap_attempt();
    }

    vec![Effect::SendRecap { session_id, auto }]
}

// TaskResult handlers.

pub(super) fn handle_memory_note_saved(
    app: &mut AppView,
    agent_id: AgentId,
    result: Result<(), String>,
) -> Vec<Effect> {
    if let Some(agent) = app.agents.get_mut(&agent_id) {
        match result {
            Ok(()) => {
                agent
                    .scrollback
                    .push_block(crate::scrollback::block::RenderBlock::system(format!(
                        "Memory saved to {}",
                        crate::util::display_user_echo_agent_path("memory/MEMORY.md")
                    )));
            }
            Err(error) => {
                agent
                    .scrollback
                    .push_block(crate::scrollback::block::RenderBlock::system(format!(
                        "Couldn't save memory note: {error}"
                    )));
            }
        }
    }
    vec![]
}

pub(super) fn handle_btw_response(
    app: &mut AppView,
    agent_id: AgentId,
    result: Result<String, String>,
    minimal_request_id: Option<uuid::Uuid>,
) -> Vec<Effect> {
    if let Some(agent) = app.agents.get_mut(&agent_id) {
        use crate::views::btw_overlay::BtwOverlayState;
        if let Some(request_id) = minimal_request_id {
            crate::minimal_api::finish_minimal_btw(agent, request_id, result);
            return vec![];
        }
        let question = match &agent.btw_state {
            Some(BtwOverlayState::Loading { question }) => question.clone(),
            _ => String::new(),
        };
        match result {
            Ok(response) => {
                // Answer arrived: show it (until Esc) and focus the panel
                // so Up/Down scroll it until the user returns to the prompt.
                agent.btw_state = Some(BtwOverlayState::done(question, response));
                agent.btw_focused = true;
            }
            Err(error) => {
                // Error stays until Esc; nothing to scroll, keep prompt focus.
                agent.btw_state = Some(BtwOverlayState::Error { question, error });
                agent.btw_focused = false;
            }
        }
    }
    vec![]
}
