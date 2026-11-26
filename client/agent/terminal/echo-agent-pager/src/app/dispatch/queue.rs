//! Prompt-queue dispatch: the server-authoritative immediate-send routing
//! helpers, optimistic queue echoes, the local drip-feed drain
//! ([`maybe_drain_queue`]), the turn-start shim, and the queue-interject
//! action arm. Split out of `dispatch.rs` verbatim (pure code motion).

use super::ctx::{NO_SESSION_NOTICE, active_agent_session_id, with_active_agent};
use crate::acp::meta::user_prompt_meta;
use crate::app::actions::Effect;
use crate::app::agent::{AgentCommand, AgentId};
use crate::app::agent_view::{AgentView, PromptMode};
use crate::app::app_view::{ActiveView, AppView};
use crate::scrollback::EntryId;
use crate::scrollback::block::RenderBlock;
use crate::scrollback::state::ScrollbackState;
use agent_client_protocol as acp;
use std::time::Instant;

fn page_flip_on_send() -> bool {
    crate::appearance::cache::load_page_flip_on_send()
}

/// Slash output skips the prompt drain; pin it at the top when page-flip is on.
pub(super) fn push_and_page_flip(scrollback: &mut ScrollbackState, block: RenderBlock) {
    scrollback.push_block(block);
    if !page_flip_on_send() {
        return;
    }
    let idx = scrollback.len() - 1;
    scrollback.scroll_to_entry_top(idx);
    scrollback.enable_follow_with_preserve();
}

fn combine_queued_prompts_enabled() -> bool {
    crate::appearance::cache::load_combine_queued_prompts()
}

/// Whether a prompt/command submitted right now should take the
/// server-authoritative immediate-send path: the **server is busy**
/// (running a turn or still holding queued prompts), the session exists, the
/// local drip-feed queue is empty, and we're not mid-edit / model-switch /
/// replay. Kind-specific extras (e.g. the plain-prompt "no images" rule) are
/// checked by the caller.
///
/// **Server-busy — `is_turn_running() || !shared_queue.is_empty()`:** the
/// immediate-send path is for prompts that must queue server-side rather than
/// start a turn locally. It is NOT enough to check `is_turn_running()`: in
/// leader mode there is a turn-end window where this client has processed the
/// turn-end (so it is locally `Idle`, `current_prompt_id` cleared) but has not
/// yet adopted the leader's broadcast that the next prompt was promoted. In
/// that window `is_turn_running()` is false, yet the agent is busy and its
/// queue is non-empty — which this client sees as a non-empty `shared_queue`
/// mirror. Without the queue check, a prompt sent then takes the local
/// drip-feed path and is optimistically promoted to a running turn on THIS
/// client, while the leader appends it BEHIND the existing queue — it shows as
/// running here but queued on every other client (confirmed via qtrace:
/// `send_route_plain immediate=false is_turn_running=false shared_queue_len=5`
/// followed by `local_drain`). Treating a non-empty `shared_queue` as
/// server-busy routes it to the server queue, where the broadcast then drives
/// adoption consistently for all clients.
///
/// **FIFO guard — `pending_prompts.is_empty()`:** a prompt may only jump onto
/// the server queue when there is nothing ahead of it in the local drip-feed
/// queue. The two queues are merged for display/drain as *server rows first,
/// then local rows* ([`QueuePane::sync_from_merged`]), which is only correct
/// while every server-queued prompt is older than every local one. That
/// invariant breaks during the startup race: prompts typed while the session is
/// still "Starting…" go local (no session/turn yet); once the first one drains
/// and the turn starts, a newly-typed prompt would immediate-send onto the
/// server queue and render *ahead* of the still-pending older local prompt
/// (e.g. `[2, 3]` shown/run as `[3, 2]`). Requiring an empty local queue keeps
/// later prompts behind the older ones (they join the local queue and drain in
/// order), preserving FIFO.
///
/// **Leader / follow-up Steer gate:** the shared queue exists to hold every
/// attached client to one order. `[ui].follow_up_behavior = "steer"` also
/// takes this path so the shell can promote the row to a mid-turn
/// interjection at the next tool batch or model step.
pub(super) fn immediate_server_send_eligible(agent: &AgentView, leader_mode: bool) -> bool {
    let server_busy = agent.session.state.is_turn_running() || !agent.shared_queue.is_empty();
    (leader_mode || crate::appearance::cache::load_follow_up_steer())
        && server_busy
        && agent.session.session_id.is_some()
        && agent.session.pending_prompts.is_empty()
        && !matches!(agent.prompt_mode, PromptMode::EditingQueued { .. })
        && !agent.session.model_switch_pending
        && !agent.session.loading_replay
}

/// Push the optimistic shared-queue echo for an immediate server-authoritative
/// send and mirror it into the owning agent so the queue pane renders it
/// immediately, before the confirming `echo.agent/queue/changed` broadcast.
pub(super) fn push_server_queue_echo(
    app: &mut AppView,
    agent_id: AgentId,
    session_id: &str,
    prompt_id: &str,
    text: &str,
    kind: &str,
) {
    app.push_optimistic_prompt_echo(session_id, prompt_id, text, kind);
    let snapshot = app
        .shared_prompt_queue(session_id)
        .cloned()
        .unwrap_or_default();
    if let Some(agent) = app.agents.get_mut(&agent_id) {
        agent.shared_queue = snapshot;
        // Track the unconfirmed echo so a queue-row send-now against it is
        // parked until the confirming broadcast (see
        // `AgentView::send_now_awaiting_confirm`).
        agent.optimistic_queue_ids.insert(prompt_id.to_string());
    }
}

/// Retire the optimistic placeholder for a prompt that has definitively left
/// the server-authoritative queue (restored on cancel, removed, drained, or
/// otherwise resolved without becoming the running turn).
///
/// The agent's `pending_inputs` is the single source of truth for queue
/// contents and order; the only client-side queue state is the optimistic echo
/// that bridges the round-trip before the confirming `echo.agent/queue/changed`
/// broadcast. Once a prompt's RPC resolves (or we pull it back into the input
/// on cancel) it will never reappear in a future broadcast, so its echo must be
/// dropped — otherwise the reconcile in [`AppView::apply_queue_changed`] keeps
/// re-pinning the stale row onto the END of every subsequent broadcast, which
/// both resurrects the removed prompt and scrambles the queue order.
///
/// Takes the two backing maps by `&mut` (rather than `&mut AppView`) so callers
/// can invoke it while holding a disjoint borrow of `app.agents`.
pub(super) fn retire_optimistic_echo(
    optimistic: &mut std::collections::HashMap<
        String,
        Vec<crate::app::prompt_queue::QueueEntryWire>,
    >,
    shared: &mut std::collections::HashMap<String, Vec<crate::app::prompt_queue::QueueEntryWire>>,
    session_id: &str,
    prompt_id: &str,
) {
    if let Some(opt) = optimistic.get_mut(session_id) {
        opt.retain(|e| e.id != prompt_id);
        if opt.is_empty() {
            optimistic.remove(session_id);
        }
    }
    if let Some(q) = shared.get_mut(session_id) {
        q.retain(|e| e.id != prompt_id);
        if q.is_empty() {
            shared.remove(session_id);
        }
    }
}

/// Drain prompt-side images and snapshot all chip elements (paste blocks,
/// @-file refs, image chips) into the most recently enqueued `QueuedPrompt`.
///
/// Must be called after `enqueue_prompt` / `push_back` and before
/// `prompt.set_text("")` (which clears element and image state). If the
/// last queued entry already has `wire_blocks` (skill injection), images
/// are dropped with a toast instead of merged.
pub(super) fn drain_prompt_state_to_last_queued(agent: &mut AgentView) {
    let prompt_state = agent.prompt.stash();
    let (_, images, chip_elements) = prompt_state.into_submission();

    let Some(entry) = agent.session.pending_prompts.back_mut() else {
        return;
    };

    entry.chip_elements = chip_elements;

    if images.is_empty() {
        return;
    }

    // wire_blocks policy: skill-injected prompts do not carry prompt images.
    if entry.wire_blocks.is_some() {
        agent.show_toast("Images removed (skill prompt)");
        return;
    }

    entry.images = images;
}

/// Prepend `<system-reminder>` framing to a cron prompt for the model.
///
/// Delegates to the shared implementation in `echo_agent_tools::reminders`.
/// The UI shows the raw `prompt` text via `RenderBlock::cron_prompt`; this
/// wrapped version is only sent to the model via `Effect::SendPrompt` so
/// the model knows the message is a scheduled task execution, not a human.
fn format_cron_prompt(prompt: &str, task_id: &str, human_schedule: &str) -> String {
    echo_agent_tools::reminders::format_scheduled_task_prompt(prompt, task_id, human_schedule)
}

/// Try to send the next queued entry (prompt, command, bash, or cron) if the agent is idle.
///
/// Called after enqueue operations and task completions to advance the queue.
///
/// Branches on `QueueEntryKind`:
/// - **Prompt**: pushes user prompt block to scrollback, starts turn, returns `Effect::SendPrompt`
/// - **Command**: starts command, returns the appropriate `Effect` (e.g., `Effect::Compact`)
/// - **BashCommand**: starts turn (no user block), returns `Effect::SendBashCommand`
/// - **Cron**: pushes cron prompt block to scrollback, starts turn, returns `Effect::SendPrompt`
pub(super) struct QueueDrain {
    pub(super) effects: Vec<Effect>,
    pub(super) page_flip_entry: Option<EntryId>,
}

impl QueueDrain {
    fn blocked() -> Self {
        Self {
            effects: Vec::new(),
            page_flip_entry: None,
        }
    }
}

/// Release a locally-queued prompt into a running turn that is parked on a
/// sendable wait (subagent / task output).
///
/// The local drip-feed queue otherwise holds every queued prompt until the
/// turn ends, so a message sent while waiting never reaches the shell.
/// `/btw` is an ext-method and does not produce an ACP session batch, so
/// this has to run on the send path itself (and when the /btw answer lands),
/// not only after the next session update.
///
/// Only a parked wait counts. Live watchers (`/loop`, background commands)
/// survive idle thinking turns, and a foreground tool is not a wait, so
/// treating either as busy would interject a follow-up the user meant to
/// queue. Goes out as `echo.agent/interject`, never send-now (cancel semantics).
///
/// Releases the **last** queued plain prompt (the one this send just
/// pushed), not the front. An earlier follow-up queued while thinking
/// must stay queued. Only a row whose wire payload is its display text
/// is released.
///
/// `agent_id` targets that session. `None` uses the focused agent.
pub(crate) fn maybe_release_queued_prompt_into_turn(
    app: &mut AppView,
    agent_id: Option<AgentId>,
) -> Vec<Effect> {
    use crate::app::agent::QueueEntryKind;

    let id = match agent_id {
        Some(id) => id,
        None => match app.active_view {
            ActiveView::Agent(id) => id,
            _ => return Vec::new(),
        },
    };
    let Some(agent) = app.agents.get(&id) else {
        return Vec::new();
    };
    if !agent.session.state.is_turn_running() || agent.session.session_id.is_none() {
        return Vec::new();
    }
    // Mid-outage: the interject effect has no requeue path, so a row released
    // into a dead channel is simply lost. Leave it queued.
    if app.reconnect_pending {
        return Vec::new();
    }
    if !agent.is_parked_on_sendable_wait() {
        return Vec::new();
    }
    if matches!(agent.prompt_mode, PromptMode::EditingQueued { .. })
        || !agent
            .session
            .pending_prompts
            .back()
            .is_some_and(|p| matches!(p.kind, QueueEntryKind::Prompt) && p.wire_matches_display())
    {
        return Vec::new();
    }

    let Some(agent) = app.agents.get_mut(&id) else {
        return Vec::new();
    };
    let Some(queued) = agent.session.pending_prompts.pop_back() else {
        return Vec::new();
    };
    crate::unified_log::info(
        "prompt.release_into_turn",
        agent.session.session_id.as_ref().map(|s| s.0.as_ref()),
        Some(serde_json::json!({
            "remaining_in_queue": agent.session.pending_prompts.len(),
            "prompt_len": queued.text.len(),
        })),
    );
    super::interject::dispatch_interject_on(app, id, queued.text, queued.images)
}

pub(super) fn maybe_drain_queue(agent: &mut AgentView) -> QueueDrain {
    use crate::app::agent::QueueEntryKind;
    use crate::unified_log as ulog;

    let sid = agent.session.session_id.as_ref().map(|s| s.0.as_ref());
    let queue_depth = agent.session.pending_prompts.len();

    let log_blocked = |reason: &str, sid: Option<&str>| {
        if queue_depth > 0 {
            ulog::debug(
                "prompt.drain_blocked",
                sid,
                Some(serde_json::json!({"reason": reason, "queue_depth": queue_depth})),
            );
        }
    };

    if !agent.session.state.is_idle() {
        log_blocked("turn_running", sid);
        return QueueDrain::blocked();
    }
    // Hold the drain during an in-flight model switch. See the
    // `model_switch_pending` field doc for why a reconnect must clear it.
    if agent.session.model_switch_pending {
        log_blocked("model_switch_pending", sid);
        return QueueDrain::blocked();
    }
    if agent.session.loading_replay {
        log_blocked("loading_replay", sid);
        return QueueDrain::blocked();
    }
    // Server-owned next turn: a non-running server row (including this
    // client's own in-flight send-now echo) drains shell-side — the
    // `queue/changed(running_prompt_id)` adoption starts it. Draining a LOCAL
    // row now would optimistically promote it as the running turn while the
    // shell runs the server row, whose deltas then fail the prompt-id gate
    // and render nothing (the FIFO invariant documented on
    // `immediate_server_send_eligible`).
    let running = agent.session.current_prompt_id.as_deref();
    if agent
        .shared_queue
        .iter()
        .any(|e| Some(e.id.as_str()) != running)
    {
        log_blocked("server_queue_owns_next_turn", sid);
        return QueueDrain::blocked();
    }
    let Some(session_id) = agent.session.session_id.clone() else {
        log_blocked("no_session_id", None);
        return QueueDrain::blocked();
    };

    // Block drain if the user is editing the front prompt.
    if let PromptMode::EditingQueued { id, .. } = &agent.prompt_mode
        && agent
            .session
            .pending_prompts
            .front()
            .is_some_and(|p| p.id == *id)
    {
        // The prompt being edited is next to send — don't drain it
        // from under the user. The turn status line will show a
        // "waiting on your edit" indicator.
        log_blocked("user_editing_front", Some(&session_id.0));
        return QueueDrain::blocked();
    }

    // Row the user is actively editing (if any). The front-row case is already
    // handled above; pass it so a combined drain also stops before an edited
    // *follower* instead of merging it away.
    let editing_id = match &agent.prompt_mode {
        PromptMode::EditingQueued { id, .. } => Some(*id),
        _ => None,
    };
    let queued = match if combine_queued_prompts_enabled() {
        agent.session.dequeue_combined_prompt(editing_id)
    } else {
        agent.session.dequeue_prompt()
    } {
        Some(q) => q,
        None => return QueueDrain::blocked(),
    };

    // A new turn is starting: follow-up chips belong to the previous
    // response and must not linger into it.
    agent.clear_follow_ups();

    // This client is now sending its own prompt — it "takes the wheel" and is
    // no longer a passive viewer. Clearing this restores strict prompt-id gate
    // semantics (so stale chunks from a later rewind/cancel of THIS turn are
    // dropped, not adopted). See `AgentView::attached_as_viewer`.
    agent.attached_as_viewer = false;

    ulog::info(
        "prompt.drain",
        Some(&session_id.0),
        Some(serde_json::json!({
            "kind": queued.kind.as_label(),
            "remaining_in_queue": agent.session.pending_prompts.len(),
            "prompt_len": queued.text.len(),
        })),
    );
    // qtrace: a LOCAL drip-feed drain promotes this prompt to the running turn
    // client-side (renders a scrollback block + sets current_prompt_id). In
    // leader mode this is the suspected divergence point — the server may queue
    // the prompt behind others instead of running it.
    tracing::debug!(
        target: "qtrace",
        pid = std::process::id(),
        event = "local_drain",
        kind = queued.kind.as_label(),
        remaining = agent.session.pending_prompts.len(),
        shared_queue_len = agent.shared_queue.len(),
        session = session_id.0.as_ref(),
        text = %queued.text.chars().take(48).collect::<String>(),
        "draining prompt LOCALLY as a new running turn",
    );

    let agent_id = agent.session.id;

    // Track whether this turn is a bash-mode command for post-turn focus.
    agent.bash_turn = queued.kind == QueueEntryKind::BashCommand;
    agent.cron_task_id = if queued.kind == QueueEntryKind::Cron {
        queued.task_id.clone()
    } else {
        None
    };
    // Generate a fresh prompt_id for every outgoing prompt/command. This is
    // threaded through PromptRequest._meta to the agent and echoed on every
    // SessionNotification + the PromptResponse, letting us correlate
    // notifications back to the originating prompt for cancel/rewind.
    let prompt_id = uuid::Uuid::new_v4().to_string();

    // Record it as self-originated so the ACP gate treats this turn's deltas as
    // ours (drive it; drop a stale post-rewind chunk on a mismatch) rather than
    // adopting them as another client's turn. The `Cron` arm overrides
    // `prompt_id` with a `scheduler-fired-` prefix and records that id itself.
    if queued.kind != QueueEntryKind::Cron {
        agent.note_self_originated_prompt(&prompt_id);
    }

    match queued.kind {
        QueueEntryKind::Prompt => {
            agent.start_turn_boundary(Some(&prompt_id));
            agent.session.current_prompt_id = Some(prompt_id.clone());
            // Scrollback shows display text (never raw skill XML). Combined
            // drains paint one bubble per original follow-up.
            let is_skill = queued.display_as_skill;
            let multi = echo_agent_prompt_queue::is_combined(&queued.combined_texts);
            let (prompt_idx, prompt_entry_id, combined_entries) = if multi {
                let (first_idx, _, last_id, all_ids) =
                    paint_or_reuse_combined_user_bubbles(agent, &queued.combined_texts);
                (first_idx, last_id, all_ids)
            } else {
                let block = if is_skill {
                    RenderBlock::skill_prompt(&queued.text)
                } else if !queued.skill_token_ranges.is_empty() {
                    RenderBlock::user_prompt_with_skill_tokens(
                        &queued.text,
                        queued.skill_token_ranges.clone(),
                    )
                } else {
                    RenderBlock::user_prompt(&queued.text)
                };
                let id = agent.scrollback.push_block(block);
                (agent.scrollback.len().saturating_sub(1), id, vec![id])
            };
            // Stash for cancel-with-restore. Only plain (non-skill) prompts
            // can be reversed back into the input box.
            if queued.wire_blocks.is_none() {
                let earlier = combined_entries
                    .iter()
                    .copied()
                    .filter(|id| *id != prompt_entry_id)
                    .collect();
                agent.session.in_flight_prompt = Some(crate::app::agent::InFlightPrompt {
                    text: queued.text.clone(),
                    images: queued.images.clone(),
                    scrollback_entry: prompt_entry_id,
                    combined_scrollback_entries: earlier,
                    chip_elements: queued.chip_elements.clone(),
                });
            }
            agent.turn_started_at = Some(Instant::now());
            let flip = page_flip_on_send();
            agent.scrollback.follow_new_turn(Some(prompt_idx), flip);

            let combined_segs = queued.combined_texts.clone();
            let effects = if let Some(mut blocks) = queued.wire_blocks {
                // Skill injection: send structured blocks.
                // Annotate the first text block's meta with the display text
                // so the pager can reconstruct the clean prompt on session
                // restore (replay). Without this, replay shows the raw skill
                // instructions instead of the user-facing display text.
                if let Some(acp::ContentBlock::Text(tb)) = blocks.first_mut() {
                    let map = tb.meta.get_or_insert_with(acp::Meta::new);
                    map.insert(
                        user_prompt_meta::DISPLAY_TEXT.into(),
                        serde_json::Value::String(queued.text),
                    );
                    if is_skill {
                        map.insert(
                            user_prompt_meta::DISPLAY_AS_SKILL.into(),
                            serde_json::Value::Bool(true),
                        );
                    }
                    echo_agent_prompt_queue::stamp_combined_display_texts(map, &combined_segs);
                } else {
                    tracing::debug!(
                        "wire_blocks[0] is not TextContent — displayText annotation skipped"
                    );
                }
                vec![Effect::SendPromptBlocks {
                    agent_id,
                    session_id,
                    blocks,
                    prompt_id,
                }]
            } else if !queued.images.is_empty() {
                // Image-bearing prompt: build text + image content blocks.
                // Pass the session cwd so orphan `[Image #N: <path>]`
                // placeholders (paste from a previous session, etc.)
                // can be recovered from disk via the shared helper.
                // Token ranges are NOT stamped here: the builder rewrites the
                // text (placeholder stripping), which would shift byte offsets.
                let mut blocks = crate::prompt_images::build_content_blocks_with_workspace(
                    queued.text,
                    queued.images,
                    Some(std::path::Path::new(&agent.session.cwd)),
                );
                if let Some(acp::ContentBlock::Text(tb)) = blocks.first_mut() {
                    let map = tb.meta.get_or_insert_with(acp::Meta::new);
                    echo_agent_prompt_queue::stamp_combined_display_texts(map, &combined_segs);
                }
                vec![Effect::SendPromptBlocks {
                    agent_id,
                    session_id,
                    blocks,
                    prompt_id,
                }]
            } else if multi {
                // Stamp combinedDisplayTexts so reload paints multi-bubble. No
                // skillTokenRanges: dequeue_combined_prompt clears them on every
                // combined drain (multi paints plain per-segment bubbles).
                let mut tb = acp::TextContent::new(queued.text);
                let map = tb.meta.get_or_insert_with(acp::Meta::new);
                echo_agent_prompt_queue::stamp_combined_display_texts(map, &combined_segs);
                vec![Effect::SendPromptBlocks {
                    agent_id,
                    session_id,
                    blocks: vec![acp::ContentBlock::Text(tb)],
                    prompt_id,
                }]
            } else {
                // Normal prompt: send text as-is.
                vec![Effect::SendPrompt {
                    agent_id,
                    session_id,
                    text: queued.text,
                    prompt_id,
                    skill_token_ranges: queued.skill_token_ranges,
                }]
            };
            QueueDrain {
                effects,
                page_flip_entry: flip.then_some(prompt_entry_id),
            }
        }
        QueueEntryKind::Command => {
            // Currently only `/compact` — future slash commands will branch here.
            agent.session.start_command(AgentCommand::Compact);
            // Compact owns the pane; a leftover wake marker must not shadow stop.
            agent.running_wake_turn = None;
            agent.turn_started_at = Some(Instant::now());

            QueueDrain {
                effects: vec![Effect::Compact {
                    agent_id,
                    session_id,
                }],
                page_flip_entry: None,
            }
        }
        QueueEntryKind::BashCommand => {
            // Start turn but do NOT push a user prompt block.
            // The execute block from the shell IS the visual entry.
            agent.start_turn_boundary(Some(&prompt_id));
            agent.session.current_prompt_id = Some(prompt_id.clone());
            agent.turn_started_at = Some(Instant::now());

            agent.scrollback.follow_new_turn(None, page_flip_on_send());

            QueueDrain {
                effects: vec![Effect::SendBashCommand {
                    agent_id,
                    session_id,
                    command: queued.text,
                    prompt_id,
                }],
                page_flip_entry: None,
            }
        }
        QueueEntryKind::Cron => {
            let prompt_id = format!("scheduler-fired-{prompt_id}");
            agent.note_self_originated_prompt(&prompt_id);
            agent.start_turn_boundary(Some(&prompt_id));
            agent.session.current_prompt_id = Some(prompt_id.clone());
            let prompt_entry_id = agent
                .scrollback
                .push_block(RenderBlock::cron_prompt(&queued.text));
            agent.turn_started_at = Some(Instant::now());

            let prompt_idx = agent.scrollback.len().saturating_sub(1);
            let flip = page_flip_on_send();
            agent.scrollback.follow_new_turn(Some(prompt_idx), flip);

            let framed_text = format_cron_prompt(
                &queued.text,
                queued.task_id.as_deref().unwrap_or("unknown"),
                queued.human_schedule.as_deref().unwrap_or("unknown"),
            );

            let mut meta_map = serde_json::Map::new();
            meta_map.insert(
                user_prompt_meta::DISPLAY_TEXT.into(),
                serde_json::Value::String(queued.text),
            );
            meta_map.insert(
                user_prompt_meta::DISPLAY_AS_CRON.into(),
                serde_json::Value::Bool(true),
            );
            let blocks = vec![acp::ContentBlock::Text(
                acp::TextContent::new(framed_text).meta(Some(meta_map)),
            )];

            QueueDrain {
                effects: vec![Effect::SendPromptBlocks {
                    agent_id,
                    session_id,
                    blocks,
                    prompt_id,
                }],
                page_flip_entry: flip.then_some(prompt_entry_id),
            }
        }
    }
}

/// Whether [`apply_turn_start_shim`] renders its own user block (i.e.
/// `display_block` is `Some`). When true the pager owns the block and must
/// swallow the leader's user-echo; when false (bash, or a viewer with no local
/// text) the echo is the only source and must render. Kept in sync with the
/// shim's match via a `debug_assert!` there.
pub(crate) fn shim_renders_own_user_block(kind: &str, text: Option<&str>) -> bool {
    match kind {
        "bash" => false,
        _ => text.is_some(),
    }
}

/// Trailing turn-starting `UserPrompt` matching `text`, scanning back past
/// turn-boundary chrome. `claim_interjection` skips other interjections and
/// keeps the oldest match.
fn trailing_user_prompt_matching(
    agent: &AgentView,
    text: &str,
    claim_interjection: bool,
) -> Option<(usize, crate::scrollback::EntryId)> {
    let mut found = None;
    for idx in (0..agent.scrollback.len()).rev() {
        let Some(entry) = agent.scrollback.entry(idx) else {
            break;
        };
        match &entry.block {
            RenderBlock::UserPrompt(ub) if ub.text == text && ub.is_interjection => {
                if claim_interjection {
                    found = Some((idx, entry.id));
                    continue;
                }
                break;
            }
            RenderBlock::UserPrompt(ub) if ub.text == text && !ub.is_interjection => {
                if !claim_interjection {
                    return Some((idx, entry.id));
                }
                break;
            }
            RenderBlock::UserPrompt(ub) if claim_interjection && ub.is_interjection => continue,
            RenderBlock::SessionEvent(_) | RenderBlock::System(_) => continue,
            _ => break,
        }
    }
    found
}

/// Last `n` non-interjection user prompts (oldest → newest), scanning back past
/// turn chrome. Used to reuse multi-bubble paints when the echo already rendered.
fn trailing_user_prompts(
    agent: &AgentView,
    n: usize,
) -> Option<Vec<(usize, crate::scrollback::EntryId, String)>> {
    if n == 0 {
        return None;
    }
    let mut out = Vec::with_capacity(n);
    for idx in (0..agent.scrollback.len()).rev() {
        let entry = agent.scrollback.entry(idx)?;
        match &entry.block {
            RenderBlock::UserPrompt(ub) if !ub.is_interjection => {
                out.push((idx, entry.id, ub.text.clone()));
                if out.len() == n {
                    out.reverse();
                    return Some(out);
                }
            }
            RenderBlock::SessionEvent(_) | RenderBlock::System(_) => continue,
            _ => return None,
        }
    }
    None
}

/// Paint one user bubble per combined segment, or reuse matching trailing
/// bubbles. Drops a single joined-body bubble if the echo raced ahead.
///
/// Returns `(first_idx, first_id, last_id, all_segment_ids oldest→newest)`.
fn paint_or_reuse_combined_user_bubbles(
    agent: &mut AgentView,
    segments: &[String],
) -> (
    usize,
    crate::scrollback::EntryId,
    crate::scrollback::EntryId,
    Vec<crate::scrollback::EntryId>,
) {
    if let Some(existing) = trailing_user_prompts(agent, segments.len()).filter(|rows| {
        rows.iter()
            .map(|(_, _, t)| t.as_str())
            .eq(segments.iter().map(String::as_str))
    }) {
        let (first_idx, first_id, _) = existing[0];
        let (_, last_id, _) = existing[existing.len() - 1];
        let ids = existing.iter().map(|(_, id, _)| *id).collect();
        return (first_idx, first_id, last_id, ids);
    }

    let joined = echo_agent_prompt_queue::join_texts(segments.iter().map(String::as_str));
    if let Some((_, id)) = trailing_user_prompt_matching(agent, &joined, false) {
        agent.scrollback.remove_entry(id);
    }

    let mut first_idx = None;
    let mut first_id = None;
    let mut last_id = None;
    let mut all_ids = Vec::with_capacity(segments.len());
    for seg in segments {
        let id = agent
            .scrollback
            .push_block(RenderBlock::user_prompt(seg.clone()));
        all_ids.push(id);
        if first_idx.is_none() {
            first_idx = Some(agent.scrollback.len().saturating_sub(1));
            first_id = Some(id);
        }
        last_id = Some(id);
    }
    (
        first_idx.expect("segments non-empty"),
        first_id.expect("segments non-empty"),
        last_id.expect("segments non-empty"),
        all_ids,
    )
}

/// Paint the user block for a send-now'd prompt at dispatch: arming hides the
/// queue row, and the shell echo is swallowed at adoption (`expect_user_echo`)
/// or swept as a duplicate — so without this the message has no visible
/// representation until the turn-start adoption (which reuses the block via
/// `send_now_painted_blocks`). Call wherever the
/// expectation is armed, only for rows whose adoption paints its own block;
/// `kind` must build the same block the shim would. `edited` marks an
/// edit-interject override (fresher than the mirror text the adoption sees).
pub(super) fn push_send_now_user_block(
    agent: &mut AgentView,
    prompt_id: &str,
    kind: &str,
    text: &str,
    edited: bool,
) {
    // Viewers never run the shim and keep their echo (their only block
    // source) — painting here would double-render against it.
    if agent.attached_as_viewer {
        return;
    }
    // Repeat arm: same text is already painted; new text (edit re-arm)
    // replaces the block.
    if let Some((existing, _)) = agent.send_now_painted_blocks.get(prompt_id).copied()
        && let Some(idx) = agent.scrollback.index_of_id(existing)
    {
        let same = matches!(
            agent.scrollback.entry(idx).map(|e| &e.block),
            Some(RenderBlock::UserPrompt(ub)) if ub.text == text
        );
        if same {
            return;
        }
        agent.scrollback.remove_entry(existing);
    }
    // Always a fresh block: at dispatch this prompt's echo cannot have landed
    // yet, so a same-text trailing match is a stale earlier bubble — claiming
    // it would leave the send-now with no new visible message.
    let block = match kind {
        "cron" => RenderBlock::cron_prompt(text.to_string()),
        _ => RenderBlock::user_prompt(text.to_string()),
    };
    let entry_id = agent.scrollback.push_block(block);
    // Send-now keeps the viewport where it is (no entry-top jump).
    agent.scrollback.enable_follow_mode();
    agent
        .send_now_painted_blocks
        .insert(prompt_id.to_string(), (entry_id, edited));
}

/// Whether a Send Now row should paint an optimistic block. Returns `false`
/// unless the client expects a Send Now cancel, or the row belongs to an active
/// goal on a committed, running turn. Arms the cancel expectation only on the
/// expects-cancel path; an active goal paints WITHOUT arming so its interjection
/// notification can claim the block. Bash rows and idle sessions do neither.
fn paint_send_now_and_maybe_arm(agent: &mut AgentView, id: &str) -> bool {
    let expects_cancel = agent.expects_send_now_cancel();
    let goal_active = agent
        .goal_state
        .as_ref()
        .is_some_and(|goal| matches!(goal.status, crate::app::agent::GoalDisplayStatus::Active));
    if !(expects_cancel
        || (goal_active && agent.session.state.is_turn_running() && agent.front_message_committed))
    {
        return false;
    }
    if expects_cancel {
        agent.arm_send_now_expectation(id.to_string());
    }
    true
}

/// Active goals paint without arming cancellation so their authoritative
/// interjection notification can claim the block.
pub(super) fn arm_send_now_and_paint_dispatched(
    agent: &mut AgentView,
    prompt_id: &str,
    text: &str,
) {
    if !paint_send_now_and_maybe_arm(agent, prompt_id) {
        return;
    }
    push_send_now_user_block(agent, prompt_id, "prompt", text, /* edited */ false);
}

/// Active-goal rows paint without arming cancellation; bash rows remain queued.
pub(crate) fn arm_send_now_and_paint(agent: &mut AgentView, id: &str, new_text: Option<&str>) {
    if !paint_send_now_and_maybe_arm(agent, id) {
        return;
    }
    let row = agent
        .shared_queue
        .iter()
        .find(|e| e.id == id)
        .map(|e| (e.kind.clone(), e.text.clone()));
    if let Some((kind, text)) = row
        && shim_renders_own_user_block(&kind, Some(&text))
    {
        let edited = new_text.is_some_and(|t| t != text);
        push_send_now_user_block(agent, id, &kind, new_text.unwrap_or(&text), edited);
    }
}

/// Turn-start shim for a server-authoritative prompt the leader just drained
/// into the running slot.
///
/// Mirrors the matching arm of [`maybe_drain_queue`] EXCEPT it does NOT mint a
/// `prompt_id` (it adopts the one the leader reported) and does NOT emit a send
/// `Effect` (the prompt was already sent at enqueue time). The scrollback block
/// and focus flag are branched on the adopted entry's `kind`:
///
/// - `"bash"`     — no user block (the shell's execute block IS the entry); set
///   `agent.bash_turn = true` for post-turn focus + TurnComplete suppression.
/// - `"cron"`     — render the cron text via `cron_prompt`.
/// - otherwise (plain `"prompt"`) — render the user-prompt block + stash an
///   `in_flight_prompt` for Ctrl+C rewind.
///
/// `start_turn` calls `expect_user_echo`, so the optimistic block here and the
/// server's user-echo `session/update` are de-duplicated (not double-rendered).
pub(crate) fn apply_turn_start_shim(
    agent: &mut AgentView,
    prompt_id: String,
    text: Option<String>,
    kind: &str,
    combined_texts: Option<Vec<String>>,
) -> Option<EntryId> {
    // Re-derive the per-turn viewer flag (see the ACP gate). This shim adopts a
    // turn the leader drained into the running slot: if THIS client originated
    // it (its own queued/immediate prompt), it drives it; otherwise it is
    // viewing a turn another client drives, so `attached_as_viewer` must flip
    // back to true even if this pane has sent prompts before (the flag is no
    // longer a one-way latch) — that drives `handle_prompt_complete` + the
    // viewer chrome correctly.
    let adopted_from_other_client = !agent.is_self_originated_prompt(&prompt_id);
    // Sticky pin + still-armed send-now expect (not cleared on adopt — the
    // cancel rail may still need it). Either covers adopt-before-cancel.
    let skip_entry_top = agent
        .follow_without_jump_prompt_id
        .as_ref()
        .is_some_and(|id| id == &prompt_id)
        || agent
            .expect_send_now_cancel
            .as_ref()
            .is_some_and(|id| id == &prompt_id);
    // Always drop pin (hit or miss) so a stale queue-row id cannot skip later.
    agent.follow_without_jump_prompt_id = None;
    tracing::debug!(
        target: "qtrace",
        pid = std::process::id(),
        event = "turn_start_shim",
        prompt_id = %prompt_id,
        kind,
        adopted_from_other_client,
        skip_entry_top,
        prev_current_prompt_id = agent.session.current_prompt_id.as_deref().unwrap_or(""),
        shared_queue_len = agent.shared_queue.len(),
        text = %text.as_deref().unwrap_or("").chars().take(48).collect::<String>(),
        "adopting server-driven running turn (turn-start shim)",
    );
    agent.start_turn_boundary(Some(&prompt_id));
    agent.session.current_prompt_id = Some(prompt_id.clone());
    agent.attached_as_viewer = adopted_from_other_client;
    // A new (adopted) turn is starting: drop the prior turn's chips but KEEP the
    // seen ring, so a buffer-replayed `echo.agent/follow_ups` for an older response
    // stays rejected (no stale revival). This is correct for BOTH passive-viewer
    // and self-driven adoption: the adopted turn's OWN follow_ups still
    // re-render via the stamped `promptId` match in `apply_follow_ups` (the
    // current_prompt_id set just above), so no seen-ring un-recording is needed.
    agent.clear_follow_ups();
    // The adopted turn's follow_ups may have arrived on the ext channel BEFORE
    // this turn-start adoption (separate channels) and been buffered — render
    // them now that the turn is current.
    agent.flush_pending_follow_ups(&prompt_id);

    // Combined turn: one user bubble per original follow-up (painted below).
    let multi_segments: Option<Vec<String>> = combined_texts.filter(|v| v.len() >= 2);

    // Display block (if any) + whether Ctrl+C can restore into the composer.
    let (display_block, rewindable): (Option<RenderBlock>, bool) = match kind {
        "bash" => {
            agent.bash_turn = true;
            (None, false)
        }
        "cron" => (text.as_deref().map(RenderBlock::cron_prompt), false),
        _ if multi_segments.is_some() => (None, true),
        _ => (text.as_deref().map(RenderBlock::user_prompt), true),
    };

    debug_assert!(
        multi_segments.is_some()
            || display_block.is_some() == shim_renders_own_user_block(kind, text.as_deref()),
        "shim_renders_own_user_block must mirror apply_turn_start_shim's display_block"
    );

    let page_flip_entry = if let Some(segments) = multi_segments {
        let (prompt_idx, first_id, last_id, all_ids) =
            paint_or_reuse_combined_user_bubbles(agent, &segments);
        if rewindable {
            let restore = text.clone().unwrap_or_else(|| {
                echo_agent_prompt_queue::join_texts(segments.iter().map(String::as_str))
            });
            let earlier = all_ids.into_iter().filter(|id| *id != last_id).collect();
            // An adopted turn arrives with text only, never the original
            // attachments, so a Ctrl+C rewind restores just the joined text.
            // The local drain path, which owns the data, restores images/chips.
            agent.session.in_flight_prompt = Some(crate::app::agent::InFlightPrompt {
                text: restore,
                images: Vec::new(),
                scrollback_entry: last_id,
                combined_scrollback_entries: earlier,
                chip_elements: Vec::new(),
            });
        }
        if skip_entry_top {
            agent.scrollback.set_selected(Some(prompt_idx));
            agent.scrollback.enable_follow_mode();
            None
        } else {
            let flip = page_flip_on_send();
            agent.scrollback.follow_new_turn(Some(prompt_idx), flip);
            flip.then_some(first_id)
        }
    } else if let Some(block) = display_block {
        // The block may already be painted: consume the send-now paint's
        // id-keyed entry, else reuse a trailing echo block by text — never
        // double-push the user-prompt row.
        let claim_interjection = prompt_id.starts_with("interject-fallback-");
        let map_painted = agent.send_now_painted_blocks.remove(&prompt_id).and_then(
            |(id, edited)| -> Option<(usize, crate::scrollback::EntryId)> {
                let idx = agent.scrollback.index_of_id(id)?;
                // Text drift resolves by freshness: an `edited` paint is
                // newer than the adoption's captured mirror text — keep it;
                // otherwise the adoption is fresher — swap the stale block.
                let RenderBlock::UserPrompt(ub) = &agent.scrollback.entry(idx)?.block else {
                    return None;
                };
                if text.as_deref() != Some(ub.text.as_str()) && !edited {
                    agent.scrollback.remove_entry(id);
                    return None;
                }
                // Drop an unarmed echo's duplicate copy of this prompt;
                // blocks claimed by other pending send-nows are excluded
                // (identical stacked texts).
                if let Some((_, dup)) = text
                    .as_deref()
                    .and_then(|t| trailing_user_prompt_matching(agent, t, claim_interjection))
                    && dup != id
                    && !agent
                        .send_now_painted_blocks
                        .values()
                        .any(|(v, _)| *v == dup)
                {
                    agent.scrollback.remove_entry(dup);
                }
                Some((agent.scrollback.index_of_id(id)?, id))
            },
        );
        let already_painted = map_painted.or_else(|| {
            text.as_deref()
                .and_then(|t| trailing_user_prompt_matching(agent, t, claim_interjection))
                // Never claim a block owned by another pending send-now.
                .filter(|(_, id)| !agent.send_now_painted_blocks.values().any(|(v, _)| v == id))
        });
        let (prompt_idx, prompt_entry_id) = if let Some(found) = already_painted {
            if claim_interjection
                && let Some(RenderBlock::UserPrompt(ub)) =
                    agent.scrollback.entry_mut(found.0).map(|e| &mut e.block)
            {
                ub.is_interjection = false;
            }
            found
        } else {
            let id = agent.scrollback.push_block(block);
            (agent.scrollback.len().saturating_sub(1), id)
        };
        if rewindable && let Some(text) = text {
            // The rewind restore must match the on-screen (possibly edited)
            // block text, not the adoption's stale mirror text.
            let restore_text = match agent.scrollback.entry(prompt_idx).map(|e| &e.block) {
                Some(RenderBlock::UserPrompt(ub)) if ub.text != text => ub.text.clone(),
                _ => text,
            };
            agent.session.in_flight_prompt = Some(crate::app::agent::InFlightPrompt {
                text: restore_text,
                images: Vec::new(),
                scrollback_entry: prompt_entry_id,
                combined_scrollback_entries: Vec::new(),
                chip_elements: Vec::new(),
            });
        }
        if skip_entry_top {
            // Send-now: follow at the tail; never entry-top jump.
            agent.scrollback.set_selected(Some(prompt_idx));
            agent.scrollback.enable_follow_mode();
            None
        } else {
            let flip = page_flip_on_send();
            agent.scrollback.follow_new_turn(Some(prompt_idx), flip);
            flip.then_some(prompt_entry_id)
        }
    } else {
        // No local block to render — this is a synthetic/cron/bash adoption with
        // no shared-queue text. `start_turn` above called `expect_user_echo`,
        // which would swallow the agent's live user-message broadcast — but for
        // these turns that broadcast is the ONLY source of the user block (e.g.
        // the cron `↻ echo hello` header, rendered from `_meta.displayAsCron` /
        // `displayText`). Clear the skip so `handle_user_message` renders it
        // instead of dropping it (the cause of viewers missing the cron header).
        agent.session.tracker.clear_user_echo_skip();
        agent.scrollback.follow_new_turn(None, page_flip_on_send());
        None
    };

    agent.turn_started_at = Some(Instant::now());

    // After the echo-skip above; mirror the live-arm transitions the flush bypasses.
    agent.flush_pending_adoption_updates(&prompt_id);
    if agent.session.tracker.activity().is_some() {
        agent.session.in_flight_prompt = None;
    }
    if let Some(commands) = agent.session.tracker.take_pending_acp_commands() {
        agent.session.available_commands = commands;
        agent.session.available_commands_generation += 1;
    }
    if let Some(tools) = agent.session.tracker.take_pending_acp_tools() {
        agent.session.available_tools = Some(tools.into_iter().collect());
    }
    page_flip_entry
}

pub(crate) fn note_peek_page_flip(
    app: &mut AppView,
    agent_id: AgentId,
    page_flip_entry: Option<EntryId>,
) {
    let Some(entry_id) = page_flip_entry else {
        return;
    };
    let Some(mut dash) = app.dashboard.take() else {
        return;
    };
    dash.note_page_flip_for_lease(agent_id, entry_id, &app.agents);
    app.dashboard = Some(dash);
}

/// Drain the next queued prompt and, when that page-flips under a lease, note it.
pub(crate) fn maybe_drain_queue_and_note_peek(app: &mut AppView, agent_id: AgentId) -> Vec<Effect> {
    let drain = {
        let Some(agent) = app.agents.get_mut(&agent_id) else {
            return vec![];
        };
        maybe_drain_queue(agent)
    };
    note_peek_page_flip(app, agent_id, drain.page_flip_entry);
    drain.effects
}

/// Try to drain the next queued prompt (triggered after editing completes).
pub(super) fn dispatch_drain_queue(app: &mut AppView) -> Vec<Effect> {
    if app.reconnect_pending {
        return vec![];
    }
    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    maybe_drain_queue_and_note_peek(app, id)
}

/// `Action::QueueInterjectShared` arm: map the (possibly edited) queue
/// interject to a fire-and-forget effect scoped to the active agent's
/// session.
pub(super) fn dispatch_queue_interject_shared(
    app: &mut AppView,
    id: String,
    expected_version: u64,
    new_text: Option<String>,
) -> Vec<Effect> {
    match active_agent_session_id(app) {
        Some(session_id) => {
            with_active_agent(app, |agent| {
                // Edited override is user-typed text; keep it Ctrl+R recallable.
                if let Some(text) = &new_text {
                    agent.record_prompt_in_history(text);
                }
                agent.note_self_originated_prompt(&id);
                arm_send_now_and_paint(agent, &id, new_text.as_deref());
            });
            vec![Effect::QueueInterject {
                session_id,
                id,
                expected_version,
                new_text,
            }]
        }
        None => vec![],
    }
}

/// Decides whether the edited row is dropped and whether its text reaches the send path.
enum EditedCommandGate {
    /// Drop the row and run the command. Carries the bound session a server-row removal needs.
    Run { session_id: acp::SessionId },
    /// `dispatch_send_prompt_inner` refuses this one before running it: let it print the refusal
    /// and keep the row.
    RefusedBySendPath,
    /// No bound session: a server-row removal has no address, and a command that needs one would
    /// fail after the row was gone. Keep the row and say so.
    NeedsSession,
}

/// `Action::RunEditedQueuedCommand` arm: the row's edited text resolved to a pager builtin, so drop
/// the row and run the text through slash dispatch, the owner of resolution and command telemetry.
///
/// Every gate that can refuse the command (active view, the send path's screen-mode refusal, a
/// bound session) runs before the removal, so a refusal leaves the row queued with its pre-edit
/// text instead of trading it for a hint.
pub(super) fn dispatch_run_edited_queued_command(
    app: &mut AppView,
    local_id: u64,
    server: Option<crate::app::actions::SharedQueueTarget>,
    text: String,
) -> Vec<Effect> {
    if app.reconnect_pending {
        // Nothing runs and nothing drains while reconnecting (see `dispatch_drain_queue`), so the
        // row just stays put.
        app.show_toast(super::prompt::RECONNECTING_NOTICE);
        return vec![];
    }
    // The send half is bound to the active view (the dashboard popup forwards keys to an attached
    // agent without switching it), so resolve the removal target the same way. The edit exit has
    // already taken the composer text, so a silent bail would drop the command without a trace.
    let ActiveView::Agent(agent_id) = app.active_view else {
        app.show_toast("Open the session to run this command");
        return vec![];
    };
    // The screen-mode refusal is resolved the executor's way (`get_for_dispatch` → `mode_support`)
    // so it cannot disagree with `dispatch_send_prompt_inner`; that path's restricted-command
    // upsell can't fire here because the classifier already required the same lookup.
    let screen_mode = app.screen_mode;
    let gate = {
        let Some(agent) = app.agents.get(&agent_id) else {
            return vec![];
        };
        let registry = agent.prompt.slash_controller.registry();
        let mode_refused = crate::slash::parse_invocation(text.trim()).is_some_and(|invocation| {
            registry
                .get_for_dispatch(invocation.token)
                .is_some_and(|command| {
                    command
                        .mode_support()
                        .refusal(invocation.token, screen_mode)
                        .is_some()
                })
        });
        if mode_refused {
            EditedCommandGate::RefusedBySendPath
        } else {
            // Fail closed on the session itself rather than on what a command declares:
            // `session_scoped` is a menu-offering hint, so it says nothing reliable about whether
            // `run()` needs a bound session.
            match agent.session.session_id.clone() {
                Some(session_id) => EditedCommandGate::Run { session_id },
                None => EditedCommandGate::NeedsSession,
            }
        }
    };

    let mut effects = Vec::new();
    let sends = match gate {
        EditedCommandGate::Run { session_id } => {
            let Some(agent) = app.agents.get_mut(&agent_id) else {
                return vec![];
            };
            match server {
                // Server rows are never mutated client-side; the `echo.agent/queue/changed` rebroadcast
                // is the visual result.
                Some(server) => effects.push(Effect::QueueRemove {
                    session_id,
                    id: server.id,
                    expected_version: server.expected_version,
                }),
                None => {
                    if let Some(removed) = agent.remove_local_queue_row(local_id) {
                        // Defensive: the edit exit already cleaned the temp paths the composer
                        // shared with this row. Covers images the composer never held.
                        for image in &removed.images {
                            crate::prompt_images::cleanup_temp_file(image);
                        }
                    }
                }
            }
            true
        }
        EditedCommandGate::RefusedBySendPath => true,
        // Row kept and nothing runs: a command that ignores the missing session (`/compact`
        // enqueues regardless) would leave a second row.
        EditedCommandGate::NeedsSession => {
            if let Some(agent) = app.agents.get_mut(&agent_id) {
                agent.show_toast(NO_SESSION_NOTICE);
            }
            false
        }
    };
    if sends {
        effects.extend(super::prompt::dispatch_send_prompt_inner(
            app, text, /* consume_input */ false, /* literal */ false,
            /* is_follow_up */ false,
        ));
    }
    // The edit lock is released either way, so a command that starts no turn (or a refusal that
    // keeps the row) must not strand the queue, exactly as the plain save's `DrainQueue` did.
    effects.extend(maybe_drain_queue_and_note_peek(app, agent_id));
    effects
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::actions::{Action, SharedQueueTarget};
    use crate::app::agent::AgentState;
    use crate::app::agent_view::test_fixtures::{
        complete_task_output_wait_call, count_turn_markers, running_subagent_info,
        simulate_subagent_wait, simulate_task_output_wait, simulate_task_output_wait_call,
    };
    use crate::app::dispatch::router::dispatch;
    use crate::app::dispatch::tests::{
        end_turn, enqueue_local, last_system_text, test_app_with_agent,
    };

    /// A running background bash task for the work-count fixtures.
    fn running_bg_task(task_id: &str) -> crate::app::agent::BgTaskState {
        crate::app::agent::BgTaskState {
            task_id: task_id.into(),
            tool_call_id: format!("call-{task_id}"),
            command: "sleep 5".into(),
            description: None,
            cwd: "/tmp".into(),
            output_file: "/tmp/out".into(),
            status: crate::app::agent::BgTaskStatus::Running,
            start_time: std::time::SystemTime::now(),
            end_time: None,
            exit_code: None,
            signal: None,
            stdout: String::new(),
            stdout_line_count: 0,
            truncated: false,
            pending_kill: false,
            kill_requested_at: None,
            scrollback_entry_id: None,
            is_monitor: false,
            restored_from_replay: false,
        }
    }

    #[test]
    fn format_cron_prompt_includes_framing() {
        let out = super::format_cron_prompt("do stuff", "task-1", "every 5m");
        assert!(out.starts_with("<system-reminder>"));
        assert!(out.contains("task task-1"));
        assert!(out.contains("every 5m"));
        assert!(out.contains("do stuff"));
        assert!(
            !out.contains("<user_query>"),
            "must not add <user_query> — shell does that"
        );
        assert!(out.ends_with("do stuff"));
    }

    // ── Drain-blocking tests ───────────────────────────────────────────

    #[test]
    fn drain_blocked_when_editing_front_prompt() {
        let mut app = test_app_with_agent();
        let id = AgentId(0);

        // Queue 3 prompts, first one drains immediately (turn starts); the
        // follow-ups populate the local queue directly (see `enqueue_local`).
        dispatch(Action::SendPrompt("first".into()), &mut app);
        enqueue_local(&mut app, id, "second");
        enqueue_local(&mut app, id, "third");
        assert!(app.agents[&id].session.state.is_turn_running());
        assert_eq!(app.agents[&id].session.queue_len(), 2);

        // Simulate user editing "second" (which becomes front after "first" ends).
        let second_id = app.agents[&id].session.pending_prompts[0].id;
        app.agents.get_mut(&id).unwrap().prompt_mode = PromptMode::EditingQueued {
            id: second_id,
            original: "second".into(),
            server_id: None,
            kind: crate::app::agent::QueueEntryKind::Prompt,
        };

        // Turn ends → should NOT drain "second" (user is editing it), only FetchBilling.
        let effects = dispatch(end_turn(), &mut app);
        assert_eq!(effects.len(), 1);
        assert!(matches!(
            &effects[0],
            Effect::FetchBilling { silent: true, .. }
        ));
        assert!(app.agents[&id].session.state.is_idle());
        // "second" should still be in the queue.
        assert_eq!(app.agents[&id].session.queue_len(), 2);
        assert_eq!(app.agents[&id].session.pending_prompts[0].text, "second");
    }

    #[test]
    fn drain_not_blocked_when_editing_non_front_prompt() {
        let mut app = test_app_with_agent();
        let id = AgentId(0);

        // Queue 3 prompts, first drains.
        dispatch(Action::SendPrompt("first".into()), &mut app);
        enqueue_local(&mut app, id, "second");
        enqueue_local(&mut app, id, "third");

        // Simulate user editing "third" (NOT the front).
        let third_id = app.agents[&id].session.pending_prompts[1].id;
        app.agents.get_mut(&id).unwrap().prompt_mode = PromptMode::EditingQueued {
            id: third_id,
            original: "third".into(),
            server_id: None,
            kind: crate::app::agent::QueueEntryKind::Prompt,
        };

        // Turn ends → should drain "second" (front, not being edited) + FetchBilling.
        let effects = dispatch(end_turn(), &mut app);
        assert_eq!(effects.len(), 2);
        assert!(matches!(&effects[0], Effect::SendPrompt { text, .. } if text == "second"));
        assert!(matches!(
            &effects[1],
            Effect::FetchBilling { silent: true, .. }
        ));
        // "third" should still be in queue.
        assert_eq!(app.agents[&id].session.queue_len(), 1);
        assert_eq!(app.agents[&id].session.pending_prompts[0].text, "third");
    }

    // Edited row that resolved to a pager builtin

    fn run_edited_queued_command(
        app: &mut AppView,
        local_id: u64,
        server: Option<SharedQueueTarget>,
        text: &str,
    ) -> Vec<Effect> {
        dispatch(
            Action::RunEditedQueuedCommand {
                local_id,
                server,
                text: text.into(),
            },
            app,
        )
    }

    /// The fixture's shared queue with one row, `p1` at version 2.
    fn seed_shared_row(app: &mut AppView, id: AgentId) {
        app.agents.get_mut(&id).unwrap().shared_queue =
            vec![crate::app::prompt_queue::QueueEntryWire {
                id: "p1".into(),
                version: 2,
                owner: None,
                last_editor: None,
                kind: "prompt".into(),
                text: "what is the default".into(),
                position: 0,
                combined_texts: None,
            }];
    }

    fn shared_target() -> Option<SharedQueueTarget> {
        Some(SharedQueueTarget {
            id: "p1".into(),
            expected_version: 2,
        })
    }

    #[test]
    fn run_edited_queued_command_drops_local_row_then_runs_command() {
        let mut app = test_app_with_agent();
        let id = AgentId(0);
        app.agents.get_mut(&id).unwrap().session.state = AgentState::TurnRunning;
        enqueue_local(&mut app, id, "what is the default");
        let local_id = app.agents[&id].session.pending_prompts[0].id;

        let effects =
            run_edited_queued_command(&mut app, local_id, None, "/btw what is the default");

        assert!(
            matches!(effects.as_slice(), [Effect::SendBtw { .. }]),
            "expected the command to run"
        );
        assert_eq!(app.agents[&id].session.queue_len(), 0);
    }

    /// Server row: a versioned remove, then the command. The shared mirror is never mutated
    /// client-side: the rebroadcast is the source of truth.
    #[test]
    fn run_edited_queued_command_removes_server_row_then_runs_command() {
        let mut app = test_app_with_agent();
        let id = AgentId(0);
        seed_shared_row(&mut app, id);

        let effects = run_edited_queued_command(&mut app, 7, shared_target(), "/btw why");

        match effects.as_slice() {
            [
                Effect::QueueRemove {
                    id: removed,
                    expected_version,
                    ..
                },
                Effect::SendBtw { .. },
            ] => {
                assert_eq!(removed, "p1");
                assert_eq!(*expected_version, 2);
            }
            other => panic!("expected [QueueRemove, SendBtw], got {other:?}"),
        }
        assert_eq!(app.agents[&id].shared_queue.len(), 1);
    }

    /// A command that returns without starting a turn must not strand the rows queued behind
    /// the one it replaced.
    #[test]
    fn run_edited_queued_command_drains_the_row_behind_it() {
        let mut app = test_app_with_agent();
        let id = AgentId(0);
        enqueue_local(&mut app, id, "front");
        enqueue_local(&mut app, id, "behind");
        let local_id = app.agents[&id].session.pending_prompts[0].id;

        let effects = run_edited_queued_command(&mut app, local_id, None, "/btw why");

        assert!(
            matches!(
                effects.as_slice(),
                [Effect::SendBtw { .. }, Effect::SendPrompt { text, .. }] if text == "behind"
            ),
            "expected the command then the next row's turn"
        );
        assert_eq!(app.agents[&id].session.queue_len(), 0);
    }

    /// An enqueueing builtin re-enters the local queue at the tail, so the row's
    /// position is not preserved.
    #[test]
    fn run_edited_queued_command_with_enqueueing_builtin_re_adds_at_the_tail() {
        use crate::app::agent::QueueEntryKind;
        let mut app = test_app_with_agent();
        let id = AgentId(0);
        app.agents.get_mut(&id).unwrap().session.state = AgentState::TurnRunning;
        enqueue_local(&mut app, id, "edited into a command");
        enqueue_local(&mut app, id, "behind");
        let local_id = app.agents[&id].session.pending_prompts[0].id;

        let effects = run_edited_queued_command(&mut app, local_id, None, "/compact");

        assert!(effects.is_empty(), "no turn starts mid-turn");
        let rows: Vec<(&str, QueueEntryKind)> = app.agents[&id]
            .session
            .pending_prompts
            .iter()
            .map(|p| (p.text.as_str(), p.kind))
            .collect();
        assert_eq!(
            rows,
            vec![
                ("behind", QueueEntryKind::Prompt),
                ("/compact", QueueEntryKind::Command),
            ]
        );
    }

    /// Reconnecting: the guard bails before the removal, so the row survives for a retry.
    #[test]
    fn run_edited_queued_command_while_reconnecting_keeps_row() {
        let mut app = test_app_with_agent();
        let id = AgentId(0);
        enqueue_local(&mut app, id, "what is the default");
        let local_id = app.agents[&id].session.pending_prompts[0].id;
        app.reconnect_pending = true;

        let effects = run_edited_queued_command(&mut app, local_id, None, "/btw why");

        assert!(effects.is_empty());
        assert_eq!(app.agents[&id].session.queue_len(), 1);
    }

    /// The dashboard popup forwards keys to an attached agent without making it the active view.
    /// The send half would no-op there, so nothing runs, the row keeps its text, and the toast
    /// lands on the surface the user is actually looking at.
    #[test]
    fn run_edited_queued_command_off_active_view_keeps_row() {
        let mut app = test_app_with_agent();
        let id = AgentId(0);
        enqueue_local(&mut app, id, "what is the default");
        let local_id = app.agents[&id].session.pending_prompts[0].id;
        app.active_view = ActiveView::AgentDashboard;
        app.dashboard = Some(crate::views::dashboard::DashboardState::default());

        let effects = run_edited_queued_command(&mut app, local_id, None, "/btw why");

        assert!(effects.is_empty());
        assert_eq!(app.agents[&id].session.queue_len(), 1);
        assert_eq!(
            app.dashboard.as_ref().unwrap().error_toast.as_deref(),
            Some("Open the session to run this command")
        );
    }

    /// A command the current screen mode refuses is a pre-execution refusal: the user gets the hint
    /// and the row keeps its pre-edit text.
    #[test]
    fn run_edited_queued_command_refused_by_screen_mode_keeps_row() {
        let mut app = test_app_with_agent();
        let id = AgentId(0);
        // Mid-turn so the surviving row is observable rather than drained.
        app.agents.get_mut(&id).unwrap().session.state = AgentState::TurnRunning;
        enqueue_local(&mut app, id, "what is the default");
        let local_id = app.agents[&id].session.pending_prompts[0].id;

        // `/fullscreen` is minimal-only and the fixture's mode is not minimal.
        let effects = run_edited_queued_command(&mut app, local_id, None, "/fullscreen");

        assert!(effects.is_empty());
        assert_eq!(
            app.agents[&id].session.pending_prompts[0].text, "what is the default",
            "the row survives with its pre-edit text"
        );
        assert!(last_system_text(&app, id).contains("already in fullscreen"));
    }

    /// A refusal releases the edit lock too, so the queue must not park behind the row
    /// that was not removed.
    #[test]
    fn run_edited_queued_command_refusal_still_drains_the_queue() {
        let mut app = test_app_with_agent();
        let id = AgentId(0);
        enqueue_local(&mut app, id, "what is the default");
        let local_id = app.agents[&id].session.pending_prompts[0].id;

        let effects = run_edited_queued_command(&mut app, local_id, None, "/fullscreen");

        assert!(
            matches!(
                effects.as_slice(),
                [Effect::SendPrompt { text, .. }] if text == "what is the default"
            ),
            "expected the kept row to drain"
        );
    }

    /// Fail closed while the session is binding: the row survives (a server row's removal has no
    /// address either) and the user is told why.
    #[test]
    fn run_edited_queued_command_without_session_keeps_row() {
        let mut app = test_app_with_agent();
        let id = AgentId(0);
        seed_shared_row(&mut app, id);
        app.agents.get_mut(&id).unwrap().session.session_id = None;

        let effects = run_edited_queued_command(&mut app, 7, shared_target(), "/btw why");

        assert!(effects.is_empty(), "no unaddressed QueueRemove");
        assert_eq!(app.agents[&id].shared_queue.len(), 1);
        assert_eq!(
            app.agents[&id]
                .toast
                .as_ref()
                .map(|(text, _)| text.as_str()),
            Some("No active session")
        );
    }

    /// A command that produces no effect at all still costs the row: the removal is not tied to
    /// something coming back from the send path.
    #[test]
    fn run_edited_queued_command_drops_row_for_effect_free_builtin() {
        let mut app = test_app_with_agent();
        let id = AgentId(0);
        enqueue_local(&mut app, id, "edited into a command");
        let local_id = app.agents[&id].session.pending_prompts[0].id;

        let effects = run_edited_queued_command(&mut app, local_id, None, "/help");

        assert!(effects.is_empty(), "the palette is state, not an effect");
        assert_eq!(app.agents[&id].session.queue_len(), 0, "row dropped");
        assert!(
            matches!(
                app.agents[&id].active_modal,
                Some(crate::views::modal::ActiveModal::CommandPalette { .. })
            ),
            "the command ran"
        );
    }

    /// Row already gone (a rebroadcast or a concurrent delete): nothing to remove, but the command
    /// the user typed still runs.
    #[test]
    fn run_edited_queued_command_with_unknown_local_id_still_runs() {
        let mut app = test_app_with_agent();
        let id = AgentId(0);
        app.agents.get_mut(&id).unwrap().session.state = AgentState::TurnRunning;
        enqueue_local(&mut app, id, "someone else's row");

        let effects = run_edited_queued_command(&mut app, 9999, None, "/btw why");

        assert!(
            matches!(effects.as_slice(), [Effect::SendBtw { .. }]),
            "expected the command to run"
        );
        assert_eq!(app.agents[&id].session.queue_len(), 1);
    }

    #[test]
    fn drain_queue_action_sends_front_prompt() {
        let mut app = test_app_with_agent();
        let id = AgentId(0);

        // Queue a prompt but don't drain (set turn running first).
        app.agents.get_mut(&id).unwrap().session.state = AgentState::TurnRunning;
        enqueue_local(&mut app, id, "queued");
        assert_eq!(app.agents[&id].session.queue_len(), 1);

        // Set idle to simulate turn end (without going through PromptResponse).
        app.agents.get_mut(&id).unwrap().session.state = AgentState::Idle;

        // DrainQueue should pop and send.
        let effects = dispatch(Action::DrainQueue, &mut app);
        assert_eq!(effects.len(), 1);
        assert!(matches!(&effects[0], Effect::SendPrompt { text, .. } if text == "queued"));
        assert_eq!(app.agents[&id].session.queue_len(), 0);
    }

    #[test]
    fn drain_scroll_honors_page_flip_setting() {
        fn app_at_bottom() -> AppView {
            let mut app = test_app_with_agent();
            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            for i in 0..40 {
                agent
                    .scrollback
                    .push_block(RenderBlock::agent_message(format!("filler {i}")));
            }
            agent.scrollback.prepare_layout(80, 8);
            agent.scrollback.goto_bottom();
            app
        }

        crate::appearance::cache::set_page_flip_on_send(false);
        let mut app = app_at_bottom();
        let bottom = app.agents[&AgentId(0)].scrollback.scroll_offset();
        dispatch(Action::SendPrompt("go".into()), &mut app);
        let sb = &app.agents[&AgentId(0)].scrollback;
        assert!(sb.is_follow_mode());
        assert!(!sb.is_follow_preserve_scroll());
        assert_eq!(sb.scroll_offset(), bottom);
        assert_eq!(sb.selected(), Some(sb.len() - 1));

        let mut app = app_at_bottom();
        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        agent.scrollback.scroll_up(10);
        let reading = agent.scrollback.scroll_offset();
        dispatch(Action::SendPrompt("go".into()), &mut app);
        let sb = &app.agents[&AgentId(0)].scrollback;
        assert!(!sb.is_follow_mode());
        assert_eq!(sb.scroll_offset(), reading);
        assert_eq!(sb.selected(), Some(sb.len() - 1));

        crate::appearance::cache::set_page_flip_on_send(true);
        let mut app = app_at_bottom();
        dispatch(Action::SendPrompt("go".into()), &mut app);
        let sb = &app.agents[&AgentId(0)].scrollback;
        assert!(sb.is_follow_mode());
        assert!(sb.is_follow_preserve_scroll());
        assert_eq!(sb.selected(), Some(sb.len() - 1));

        crate::appearance::cache::set_page_flip_on_send(
            echo_agent_runtime::agent::config::UiConfig::PAGE_FLIP_ON_SEND_DEFAULT,
        );
    }

    #[test]
    fn drain_queue_when_empty_does_nothing() {
        let mut app = test_app_with_agent();
        let effects = dispatch(Action::DrainQueue, &mut app);
        assert!(effects.is_empty());
    }

    /// Server-queue edit Actions map to fire-and-forget Effects scoped to
    /// the active agent's session.
    #[test]
    fn queue_edit_actions_map_to_scoped_effects() {
        let mut app = test_app_with_agent();

        let effects = dispatch(
            Action::QueueRemoveShared {
                id: "p1".into(),
                expected_version: 4,
            },
            &mut app,
        );
        match effects.as_slice() {
            [
                Effect::QueueRemove {
                    session_id,
                    id,
                    expected_version,
                },
            ] => {
                assert_eq!(session_id.0.as_ref(), "test-session");
                assert_eq!(id, "p1");
                assert_eq!(*expected_version, 4);
            }
            other => panic!("expected QueueRemove, got {other:?}"),
        }

        let effects = dispatch(
            Action::QueueReorderShared {
                ordered_ids: vec!["p2".into(), "p1".into()],
            },
            &mut app,
        );
        match effects.as_slice() {
            [
                Effect::QueueReorder {
                    session_id,
                    ordered_ids,
                },
            ] => {
                assert_eq!(session_id.0.as_ref(), "test-session");
                assert_eq!(ordered_ids, &vec!["p2".to_string(), "p1".to_string()]);
            }
            other => panic!("expected QueueReorder, got {other:?}"),
        }

        let effects = dispatch(Action::QueueClearShared, &mut app);
        match effects.as_slice() {
            [Effect::QueueClear { session_id }] => {
                assert_eq!(session_id.0.as_ref(), "test-session");
            }
            other => panic!("expected QueueClear, got {other:?}"),
        }

        let effects = dispatch(
            Action::QueueEditShared {
                id: "p1".into(),
                new_text: "new body".into(),
            },
            &mut app,
        );
        match effects.as_slice() {
            [
                Effect::QueueEdit {
                    session_id,
                    id,
                    new_text,
                },
            ] => {
                assert_eq!(session_id.0.as_ref(), "test-session");
                assert_eq!(id, "p1");
                assert_eq!(new_text, "new body");
            }
            other => panic!("expected QueueEdit, got {other:?}"),
        }

        let effects = dispatch(
            Action::QueueInterjectShared {
                id: "p1".into(),
                expected_version: 5,
                new_text: None,
            },
            &mut app,
        );
        match effects.as_slice() {
            [
                Effect::QueueInterject {
                    session_id,
                    id,
                    expected_version,
                    new_text,
                },
            ] => {
                assert_eq!(session_id.0.as_ref(), "test-session");
                assert_eq!(id, "p1");
                assert_eq!(*expected_version, 5);
                assert_eq!(*new_text, None, "plain interject carries no override");
            }
            other => panic!("expected QueueInterject, got {other:?}"),
        }
        // Plain interjects re-send an existing queue row: no history insert.
        assert!(app.agents[&AgentId(0)].session.prompt_history.is_empty());

        // Edited interject: the same arm carrying the edited text as the
        // newText override.
        let effects = dispatch(
            Action::QueueInterjectShared {
                id: "p1".into(),
                expected_version: 5,
                new_text: Some("edited body".into()),
            },
            &mut app,
        );
        match effects.as_slice() {
            [
                Effect::QueueInterject {
                    session_id,
                    id,
                    expected_version,
                    new_text,
                },
            ] => {
                assert_eq!(session_id.0.as_ref(), "test-session");
                assert_eq!(id, "p1");
                assert_eq!(*expected_version, 5);
                assert_eq!(new_text.as_deref(), Some("edited body"));
            }
            other => panic!("expected single QueueInterject with newText, got {other:?}"),
        }
        // The user typed the edited text — it must be Ctrl+R recallable.
        assert_eq!(
            app.agents[&AgentId(0)]
                .session
                .prompt_history
                .first()
                .map(String::as_str),
            Some("edited body")
        );
    }

    #[test]
    fn arm_send_now_skips_paint_while_front_uncommitted() {
        let mut app = test_app_with_agent();
        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        agent.session.state = AgentState::TurnRunning;
        agent.start_turn_boundary(Some("p-front"));
        agent.session.current_prompt_id = Some("p-front".into());
        agent.shared_queue = vec![crate::app::prompt_queue::QueueEntryWire {
            id: "p-next".into(),
            version: 1,
            owner: None,
            last_editor: None,
            kind: "prompt".into(),
            text: "flush me".into(),
            position: 0,
            combined_texts: None,
        }];
        let before = agent.scrollback.len();
        arm_send_now_and_paint(agent, "p-next", None);
        assert!(agent.expect_send_now_cancel.is_none());
        assert_eq!(agent.scrollback.len(), before);
        assert!(!agent.send_now_painted_blocks.contains_key("p-next"));

        agent.front_message_committed = true;
        arm_send_now_and_paint(agent, "p-next", None);
        assert_eq!(agent.expect_send_now_cancel.as_deref(), Some("p-next"));
        assert!(agent.send_now_painted_blocks.contains_key("p-next"));
    }

    /// The turn-start shim sets `bash_turn` and pushes NO user block for
    /// an adopted `bash` entry.
    #[test]
    fn shim_bash_kind_sets_bash_turn_and_no_user_block() {
        let mut app = test_app_with_agent();
        let id = AgentId(0);
        let agent = app.agents.get_mut(&id).unwrap();
        let before = agent.scrollback.len();

        let page_flip_entry = apply_turn_start_shim(
            agent,
            "p1".to_string(),
            Some("ls -la".to_string()),
            "bash",
            None,
        );

        assert!(page_flip_entry.is_none());
        assert!(agent.bash_turn, "bash adoption must set bash_turn");
        assert!(agent.session.state.is_turn_running());
        assert_eq!(agent.session.current_prompt_id.as_deref(), Some("p1"));
        // No user/display block is pushed (the shell's execute block IS the entry).
        assert_eq!(agent.scrollback.len(), before);
        assert!(agent.session.in_flight_prompt.is_none());
    }

    #[test]
    fn shim_interject_fallback_reuses_interjection_bubble() {
        let mut app = test_app_with_agent();
        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        agent
            .scrollback
            .push_block(RenderBlock::interjection_prompt("steer-a"));
        agent
            .scrollback
            .push_block(RenderBlock::interjection_prompt("steer-b"));
        let before = agent.scrollback.len();
        apply_turn_start_shim(
            agent,
            "interject-fallback-a".into(),
            Some("steer-a".into()),
            "prompt",
            None,
        );
        apply_turn_start_shim(
            agent,
            "interject-fallback-b".into(),
            Some("steer-b".into()),
            "prompt",
            None,
        );
        assert_eq!(agent.scrollback.len(), before);
        match &agent.scrollback.entry(0).unwrap().block {
            RenderBlock::UserPrompt(ub) => {
                assert!(!ub.is_interjection);
                assert_eq!(ub.text, "steer-a");
            }
            other => panic!("expected user bubble, got {other:?}"),
        }
        match &agent.scrollback.entry(1).unwrap().block {
            RenderBlock::UserPrompt(ub) => {
                assert!(!ub.is_interjection);
                assert_eq!(ub.text, "steer-b");
            }
            other => panic!("expected user bubble, got {other:?}"),
        }
    }

    #[test]
    fn drain_reports_page_flip_only_when_prompt_starts() {
        crate::appearance::cache::set_page_flip_on_send(true);
        let mut app = test_app_with_agent();
        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        agent.session.enqueue_prompt("first".into());
        let started = maybe_drain_queue(agent);
        let entry_id = started.page_flip_entry.expect("prompt starts a page flip");
        assert_eq!(
            agent.scrollback.index_of_id(entry_id),
            agent.scrollback.selected()
        );

        agent.session.enqueue_prompt("queued".into());
        let blocked = maybe_drain_queue(agent);
        assert!(blocked.effects.is_empty());
        assert!(blocked.page_flip_entry.is_none());
    }

    /// Turn-start path: the leader/viewer adoption shim
    /// clears the previous response's follow-up chips.
    #[test]
    fn shim_clears_follow_up_chips() {
        let mut app = test_app_with_agent();
        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        agent.apply_follow_ups("resp-1".into(), vec!["a".into()]);
        apply_turn_start_shim(
            agent,
            "p9".to_string(),
            Some("hi".to_string()),
            "prompt",
            None,
        );
        assert!(agent.follow_ups.is_none(), "turn adoption must clear chips");
    }

    /// FIX 4 (a) via the `queue/changed` shim: after the shim adopts a turn
    /// (setting `current_prompt_id`), a re-delivery of THAT turn's follow_ups
    /// re-renders — the stamped `promptId` matches the adopted turn, so chips
    /// that were applied then cleared reappear (no un-recording needed).
    #[test]
    fn shim_adopted_turn_redelivery_rerenders() {
        let mut app = test_app_with_agent();
        let agent = app.agents.get_mut(&AgentId(0)).unwrap();

        // Follow-ups for the (to-be-)adopted turn p9 already applied + shown.
        assert!(agent.apply_follow_ups_with_prompt("resp-1".into(), Some("p9"), vec!["a".into()]));

        // The shim adopts turn p9 (viewer: "p9" is not self-originated): it
        // clears the shown chips but KEEPS the seen ring and sets
        // current_prompt_id = "p9".
        apply_turn_start_shim(
            agent,
            "p9".to_string(),
            Some("hi".to_string()),
            "prompt",
            None,
        );
        assert!(agent.attached_as_viewer, "p9 is a viewer-adopted turn");
        assert!(agent.follow_ups.is_none(), "turn adoption clears chips");
        assert!(
            agent.follow_up_seen.contains_key("resp-1"),
            "adoption keeps the seen ring"
        );

        // A re-delivery of the adopted turn's follow_ups re-renders (promptId
        // p9 == current_prompt_id).
        let changed =
            agent.apply_follow_ups_with_prompt("resp-1".into(), Some("p9"), vec!["a".into()]);
        assert!(changed, "re-delivery of the adopted turn must re-render");
        assert_eq!(
            agent
                .follow_ups
                .as_ref()
                .expect("chips re-populated")
                .suggestions,
            vec!["a"]
        );
    }

    /// FIX 4 (b) via the shim: after starting a NEW turn, a buffer-replayed
    /// `echo.agent/follow_ups` for a PRIOR turn's response stays rejected (its
    /// `promptId` is not the active turn and it is already seen) — no stale
    /// revival. Covers the self-driven turn start (`p-self`).
    #[test]
    fn shim_new_turn_rejects_prior_turn_replay() {
        let mut app = test_app_with_agent();
        let agent = app.agents.get_mut(&AgentId(0)).unwrap();

        // A prior turn (p-prev) produced resp-1's chips.
        agent.session.current_prompt_id = Some("p-prev".into());
        assert!(agent.apply_follow_ups_with_prompt(
            "resp-1".into(),
            Some("p-prev"),
            vec!["a".into()]
        ));

        // This client starts a new self-driven turn (p-self).
        agent.note_self_originated_prompt("p-self");
        apply_turn_start_shim(
            agent,
            "p-self".to_string(),
            Some("hi".to_string()),
            "prompt",
            None,
        );
        assert!(
            !agent.attached_as_viewer,
            "a self-originated turn is driver, not viewer"
        );
        assert!(agent.follow_ups.is_none(), "turn start hides chips");
        assert!(
            agent.follow_up_seen.contains_key("resp-1"),
            "the seen ring entry for the prior response is kept"
        );

        // A replayed PRIOR turn (p-prev/resp-1) must NOT revive on the new turn.
        let changed =
            agent.apply_follow_ups_with_prompt("resp-1".into(), Some("p-prev"), vec!["a".into()]);
        assert!(
            !changed,
            "a replayed prior-turn follow_ups must stay rejected on the new turn"
        );
        assert!(
            agent.follow_ups.is_none(),
            "no stale chips may resurface from the replay"
        );
    }

    /// Regression: a viewer adopting a synthetic/cron turn (no shared-queue
    /// text → no local block) must CLEAR the `expect_user_echo` flag that
    /// `start_turn` set, so the agent's live user-message broadcast — the only
    /// source of the cron `↻` header — renders instead of being swallowed.
    /// When the shim DOES render a block (queue text present), it keeps the
    /// skip so the broadcast isn't duplicated.
    #[test]
    fn shim_clears_echo_skip_only_when_it_renders_no_block() {
        // No queue text → no local block → clear the skip (let broadcast render).
        let mut app = test_app_with_agent();
        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        apply_turn_start_shim(agent, "cron-1".to_string(), None, "prompt", None);
        assert!(
            !agent.session.tracker.expects_user_echo(),
            "no-block adoption must clear the echo skip so the cron broadcast renders"
        );

        // Queue text present → local block rendered → keep the skip (no dup).
        let mut app2 = test_app_with_agent();
        let agent2 = app2.agents.get_mut(&AgentId(0)).unwrap();
        apply_turn_start_shim(
            agent2,
            "p2".to_string(),
            Some("hello world".to_string()),
            "prompt",
            None,
        );
        assert!(
            agent2.session.tracker.expects_user_echo(),
            "rendering the user block locally must keep the skip to avoid duplicating the broadcast"
        );
    }

    #[test]
    fn send_now_shim_skips_scroll_to_entry_top() {
        // This test exercises the send-now exception within the page-flip
        // behavior, so pin the setting ON (the cache is thread-local).
        crate::appearance::cache::set_page_flip_on_send(true);
        fn seed_tall_scrollback(agent: &mut crate::app::agent_view::AgentView) -> usize {
            for i in 0..40 {
                agent
                    .scrollback
                    .push_block(RenderBlock::agent_message(format!(
                        "seeded assistant line {i}: filler so entry-top and bottom offsets diverge"
                    )));
            }
            agent.scrollback.prepare_layout(80, 8);
            agent.scrollback.goto_bottom();
            agent.scrollback.scroll_offset()
        }

        let mut app_normal = test_app_with_agent();
        let agent_n = app_normal.agents.get_mut(&AgentId(0)).unwrap();
        let bottom_n = seed_tall_scrollback(agent_n);
        agent_n.note_self_originated_prompt("p-normal");
        apply_turn_start_shim(
            agent_n,
            "p-normal".into(),
            Some("normal next".into()),
            "prompt",
            None,
        );
        let normal_offset = agent_n.scrollback.scroll_offset();
        assert!(agent_n.scrollback.is_follow_mode());
        assert_ne!(
            normal_offset, bottom_n,
            "normal adoption should leave bottom via scroll_to_entry_top"
        );

        let mut app_send = test_app_with_agent();
        let agent_s = app_send.agents.get_mut(&AgentId(0)).unwrap();
        seed_tall_scrollback(agent_s);
        agent_s.note_self_originated_prompt("p-send-now");
        agent_s.arm_send_now_expectation("p-send-now".into());
        assert!(agent_s.follow_without_jump_prompt_id.is_some());
        // Cancel-rail take: only sticky pin remains.
        let _ = agent_s.expect_send_now_cancel.take();
        apply_turn_start_shim(
            agent_s,
            "p-send-now".into(),
            Some("hurry up".into()),
            "prompt",
            None,
        );
        assert!(agent_s.follow_without_jump_prompt_id.is_none());
        assert!(agent_s.scrollback.is_follow_mode());
        let send_now_offset = agent_s.scrollback.scroll_offset();
        assert_ne!(
            send_now_offset, normal_offset,
            "send-now must not use scroll_to_entry_top"
        );

        // Miss: armed for A, adopt B — entry-top path, pin still dropped.
        let mut app_miss = test_app_with_agent();
        let agent_m = app_miss.agents.get_mut(&AgentId(0)).unwrap();
        let bottom_m = seed_tall_scrollback(agent_m);
        agent_m.note_self_originated_prompt("p-b");
        agent_m.arm_send_now_expectation("p-a".into());
        apply_turn_start_shim(agent_m, "p-b".into(), Some("other".into()), "prompt", None);
        assert!(agent_m.follow_without_jump_prompt_id.is_none());
        assert_ne!(
            agent_m.scrollback.scroll_offset(),
            bottom_m,
            "mismatched arm must still scroll_to_entry_top"
        );
    }

    /// Shell user-echo can paint the prompt before the deferred turn-start
    /// shim runs. Reuse that trailing block — do not push a second copy.
    #[test]
    fn shim_reuses_already_painted_user_prompt() {
        let mut app = test_app_with_agent();
        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        agent.note_self_originated_prompt("p-dup");
        // Simulate the shell echo winning the race.
        agent
            .scrollback
            .push_block(RenderBlock::user_prompt("/deslop"));
        let before = agent.scrollback.len();
        apply_turn_start_shim(
            agent,
            "p-dup".into(),
            Some("/deslop".into()),
            "prompt",
            None,
        );
        assert_eq!(
            agent.scrollback.len(),
            before,
            "must not push a second user-prompt block when echo already painted"
        );
        let last = agent.scrollback.entry(before - 1).expect("trailing entry");
        match &last.block {
            RenderBlock::UserPrompt(ub) => assert_eq!(ub.text, "/deslop"),
            other => panic!("expected user prompt, got {other:?}"),
        }
    }

    #[test]
    fn shim_paints_one_bubble_per_combined_segment() {
        let mut app = test_app_with_agent();
        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        agent.note_self_originated_prompt("p-combo");
        let before = agent.scrollback.len();
        apply_turn_start_shim(
            agent,
            "p-combo".into(),
            Some("first\n\nsecond".into()),
            "prompt",
            Some(vec!["first".into(), "second".into()]),
        );
        assert_eq!(agent.scrollback.len(), before + 2);
        assert_eq!(user_prompt_count(agent, "first"), 1);
        assert_eq!(user_prompt_count(agent, "second"), 1);
        assert_eq!(user_prompt_count(agent, "first\n\nsecond"), 0);
        assert_eq!(
            agent.session.in_flight_prompt.as_ref().unwrap().text,
            "first\n\nsecond"
        );
    }

    #[test]
    fn shim_replaces_joined_echo_with_multi_bubbles() {
        let mut app = test_app_with_agent();
        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        agent.note_self_originated_prompt("p-combo");
        agent
            .scrollback
            .push_block(RenderBlock::user_prompt("first\n\nsecond"));
        apply_turn_start_shim(
            agent,
            "p-combo".into(),
            Some("first\n\nsecond".into()),
            "prompt",
            Some(vec!["first".into(), "second".into()]),
        );
        assert_eq!(user_prompt_count(agent, "first"), 1);
        assert_eq!(user_prompt_count(agent, "second"), 1);
        assert_eq!(user_prompt_count(agent, "first\n\nsecond"), 0);
    }

    #[test]
    fn shim_reuses_already_painted_combined_segments() {
        let mut app = test_app_with_agent();
        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        agent.note_self_originated_prompt("p-combo");
        agent
            .scrollback
            .push_block(RenderBlock::user_prompt("first"));
        agent
            .scrollback
            .push_block(RenderBlock::user_prompt("second"));
        let before = agent.scrollback.len();
        apply_turn_start_shim(
            agent,
            "p-combo".into(),
            Some("first\n\nsecond".into()),
            "prompt",
            Some(vec!["first".into(), "second".into()]),
        );
        assert_eq!(agent.scrollback.len(), before);
        assert_eq!(user_prompt_count(agent, "first"), 1);
        assert_eq!(user_prompt_count(agent, "second"), 1);
    }

    /// Number of `UserPrompt` blocks with exactly `text`.
    fn user_prompt_count(agent: &crate::app::agent_view::AgentView, text: &str) -> usize {
        (0..agent.scrollback.len())
            .filter_map(|i| agent.scrollback.entry(i))
            .filter(|e| matches!(&e.block, RenderBlock::UserPrompt(ub) if ub.text == text))
            .count()
    }

    /// Queue-row send-now paints at dispatch; the adoption reuses the block.
    #[test]
    fn queue_interject_shared_paints_user_block_at_arm() {
        let mut app = test_app_with_agent();
        let id = AgentId(0);
        {
            let agent = app.agents.get_mut(&id).unwrap();
            agent.session.state = AgentState::TurnRunning;
            agent.shared_queue = vec![crate::app::prompt_queue::QueueEntryWire {
                id: "p-ty".into(),
                version: 1,
                owner: None,
                last_editor: None,
                kind: "prompt".into(),
                text: "ty".into(),
                position: 0,
                combined_texts: None,
            }];
        }
        let effects = dispatch(
            Action::QueueInterjectShared {
                id: "p-ty".into(),
                expected_version: 1,
                new_text: None,
            },
            &mut app,
        );
        assert!(matches!(
            effects.as_slice(),
            [Effect::QueueInterject { .. }]
        ));
        let agent = app.agents.get_mut(&id).unwrap();
        assert_eq!(
            user_prompt_count(agent, "ty"),
            1,
            "send-now must paint the user block at dispatch"
        );
        // The adoption shim reuses the painted block instead of double-pushing.
        agent.note_self_originated_prompt("p-ty");
        apply_turn_start_shim(agent, "p-ty".into(), Some("ty".into()), "prompt", None);
        assert_eq!(
            user_prompt_count(agent, "ty"),
            1,
            "turn-start adoption must reuse the dispatch-painted block"
        );
    }

    /// No paint when idle (adoption renders the drain) or for bash rows.
    #[test]
    fn queue_interject_shared_skips_paint_when_not_arming_or_bash() {
        let mut app = test_app_with_agent();
        let id = AgentId(0);
        app.agents.get_mut(&id).unwrap().shared_queue =
            vec![crate::app::prompt_queue::QueueEntryWire {
                id: "p-idle".into(),
                version: 1,
                owner: None,
                last_editor: None,
                kind: "prompt".into(),
                text: "idle row".into(),
                position: 0,
                combined_texts: None,
            }];
        // Idle (no running turn): expects_send_now_cancel is false — no arm, no paint.
        let _ = dispatch(
            Action::QueueInterjectShared {
                id: "p-idle".into(),
                expected_version: 1,
                new_text: None,
            },
            &mut app,
        );
        assert_eq!(user_prompt_count(&app.agents[&id], "idle row"), 0);

        // Bash row mid-turn: armed, but its adoption paints no user block.
        {
            let agent = app.agents.get_mut(&id).unwrap();
            agent.session.state = AgentState::TurnRunning;
            agent.shared_queue = vec![crate::app::prompt_queue::QueueEntryWire {
                id: "p-bash".into(),
                version: 1,
                owner: None,
                last_editor: None,
                kind: "bash".into(),
                text: "ls -la".into(),
                position: 0,
                combined_texts: None,
            }];
        }
        let _ = dispatch(
            Action::QueueInterjectShared {
                id: "p-bash".into(),
                expected_version: 1,
                new_text: None,
            },
            &mut app,
        );
        assert_eq!(user_prompt_count(&app.agents[&id], "ls -la"), 0);
    }

    /// Composer/local-row send-now paints at dispatch too.
    #[test]
    fn send_prompt_now_paints_user_block_at_arm() {
        let mut app = test_app_with_agent();
        let id = AgentId(0);
        app.agents.get_mut(&id).unwrap().session.state = AgentState::TurnRunning;
        let effects = dispatch(
            Action::SendPromptNow {
                text: "hurry".into(),
                images: vec![],
            },
            &mut app,
        );
        assert!(matches!(effects.as_slice(), [Effect::SendPromptNow { .. }]));
        assert_eq!(user_prompt_count(&app.agents[&id], "hurry"), 1);
    }

    /// The reuse scan looks past turn-boundary chrome landing between the
    /// paint and the adoption (interject no-op, natural drain).
    #[test]
    fn shim_reuses_painted_block_past_turn_chrome() {
        let mut app = test_app_with_agent();
        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        agent.note_self_originated_prompt("p-late");
        push_send_now_user_block(agent, "p-late", "prompt", "ty", false);
        agent.scrollback.push_block(RenderBlock::session_event(
            crate::scrollback::blocks::SessionEvent::TurnCompleted {
                elapsed: Some(std::time::Duration::from_secs(2)),
            },
        ));
        agent
            .scrollback
            .push_block(RenderBlock::system("connection restored"));
        apply_turn_start_shim(agent, "p-late".into(), Some("ty".into()), "prompt", None);
        assert_eq!(
            user_prompt_count(agent, "ty"),
            1,
            "reuse must look past trailing SessionEvent/System chrome"
        );
        // A content block ends the scan.
        agent
            .scrollback
            .push_block(RenderBlock::agent_message("done"));
        agent.note_self_originated_prompt("p-again");
        apply_turn_start_shim(agent, "p-again".into(), Some("ty".into()), "prompt", None);
        assert_eq!(
            user_prompt_count(agent, "ty"),
            2,
            "content between ends the reuse scan"
        );
    }

    /// Idempotent per prompt id; new-text re-arm replaces; blank rows paint
    /// (their adoption renders a blank bubble too).
    #[test]
    fn push_send_now_user_block_dedupes_and_replaces_on_new_text() {
        let mut app = test_app_with_agent();
        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        push_send_now_user_block(agent, "p-1", "prompt", "ty", false);
        push_send_now_user_block(agent, "p-1", "prompt", "ty", false);
        assert_eq!(user_prompt_count(agent, "ty"), 1);
        push_send_now_user_block(agent, "p-1", "prompt", "ty edited", true);
        assert_eq!(user_prompt_count(agent, "ty"), 0);
        assert_eq!(user_prompt_count(agent, "ty edited"), 1);
        let before = agent.scrollback.len();
        push_send_now_user_block(agent, "p-2", "prompt", "   ", false);
        assert_eq!(agent.scrollback.len(), before + 1, "blank rows paint too");
    }

    /// Stacked send-nows: each adoption reuses its own id-keyed block.
    #[test]
    fn stacked_send_nows_each_reuse_their_own_block() {
        let mut app = test_app_with_agent();
        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        agent.note_self_originated_prompt("p-thx");
        agent.note_self_originated_prompt("p-ty");
        push_send_now_user_block(agent, "p-thx", "prompt", "thx", false);
        push_send_now_user_block(agent, "p-ty", "prompt", "ty", false);
        apply_turn_start_shim(agent, "p-thx".into(), Some("thx".into()), "prompt", None);
        assert_eq!(user_prompt_count(agent, "thx"), 1);
        agent
            .scrollback
            .push_block(RenderBlock::agent_message("You're welcome."));
        apply_turn_start_shim(agent, "p-ty".into(), Some("ty".into()), "prompt", None);
        assert_eq!(user_prompt_count(agent, "ty"), 1);
    }

    /// Identical-text stacked send-nows get one block each (no aliasing).
    #[test]
    fn stacked_identical_text_send_nows_get_distinct_blocks() {
        let mut app = test_app_with_agent();
        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        push_send_now_user_block(agent, "p-1", "prompt", "go", false);
        push_send_now_user_block(agent, "p-2", "prompt", "go", false);
        assert_eq!(user_prompt_count(agent, "go"), 2);
        assert_ne!(
            agent.send_now_painted_blocks["p-1"].0,
            agent.send_now_painted_blocks["p-2"].0
        );
        // The sibling's block must survive the first adoption's dup sweep.
        agent.note_self_originated_prompt("p-1");
        agent.note_self_originated_prompt("p-2");
        apply_turn_start_shim(agent, "p-1".into(), Some("go".into()), "prompt", None);
        assert_eq!(user_prompt_count(agent, "go"), 2);
        apply_turn_start_shim(agent, "p-2".into(), Some("go".into()), "prompt", None);
        assert_eq!(user_prompt_count(agent, "go"), 2);
        assert!(agent.send_now_painted_blocks.is_empty());
    }

    /// Viewers never paint (their echo is the block source).
    #[test]
    fn viewer_send_now_does_not_paint() {
        let mut app = test_app_with_agent();
        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        agent.attached_as_viewer = true;
        push_send_now_user_block(agent, "p-v", "prompt", "hi", false);
        assert_eq!(user_prompt_count(agent, "hi"), 0);
        assert!(agent.send_now_painted_blocks.is_empty());
    }

    /// Drifted non-edited paint is swapped for the adoption's text.
    #[test]
    fn shim_swaps_stale_painted_text_for_adoption_text() {
        let mut app = test_app_with_agent();
        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        agent.note_self_originated_prompt("p-edit");
        push_send_now_user_block(agent, "p-edit", "prompt", "old text", false);
        apply_turn_start_shim(
            agent,
            "p-edit".into(),
            Some("new text".into()),
            "prompt",
            None,
        );
        assert_eq!(user_prompt_count(agent, "old text"), 0);
        assert_eq!(user_prompt_count(agent, "new text"), 1);
    }

    /// Edited paint outranks the adoption's stale mirror text.
    #[test]
    fn shim_keeps_edited_paint_over_stale_adoption_text() {
        let mut app = test_app_with_agent();
        let id = AgentId(0);
        {
            let agent = app.agents.get_mut(&id).unwrap();
            agent.session.state = AgentState::TurnRunning;
            agent.shared_queue = vec![crate::app::prompt_queue::QueueEntryWire {
                id: "p-ed".into(),
                version: 1,
                owner: None,
                last_editor: None,
                kind: "prompt".into(),
                text: "original".into(),
                position: 0,
                combined_texts: None,
            }];
        }
        let _ = dispatch(
            Action::QueueInterjectShared {
                id: "p-ed".into(),
                expected_version: 1,
                new_text: Some("edited body".into()),
            },
            &mut app,
        );
        let agent = app.agents.get_mut(&id).unwrap();
        assert_eq!(
            user_prompt_count(agent, "edited body"),
            1,
            "the paint must show the edited text the shell will run"
        );
        // Adoption captures the pre-edit mirror text; the edited paint wins.
        agent.note_self_originated_prompt("p-ed");
        apply_turn_start_shim(
            agent,
            "p-ed".into(),
            Some("original".into()),
            "prompt",
            None,
        );
        assert_eq!(user_prompt_count(agent, "edited body"), 1);
        assert_eq!(user_prompt_count(agent, "original"), 0);
        // The Ctrl+C rewind restore must match the on-screen (edited) text.
        assert_eq!(
            agent
                .session
                .in_flight_prompt
                .as_ref()
                .map(|p| p.text.as_str()),
            Some("edited body")
        );
    }

    /// An early unarmed echo's duplicate is swept at adoption.
    #[test]
    fn shim_drops_echo_duplicate_of_painted_block() {
        let mut app = test_app_with_agent();
        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        agent.note_self_originated_prompt("p-echo");
        push_send_now_user_block(agent, "p-echo", "prompt", "ty", false);
        // Echo slips in before the adoption (no promptId, skip not armed yet).
        agent.scrollback.push_block(RenderBlock::user_prompt("ty"));
        apply_turn_start_shim(agent, "p-echo".into(), Some("ty".into()), "prompt", None);
        assert_eq!(user_prompt_count(agent, "ty"), 1);
    }

    /// A painted-pending row stays hidden after the arm drops; the pair
    /// resolves at adoption or retire, never by the arm's lifetime.
    #[test]
    fn painted_pending_row_stays_hidden_after_arm_drop() {
        let mut app = test_app_with_agent();
        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        agent.session.state = AgentState::TurnRunning;
        agent.shared_queue = vec![crate::app::prompt_queue::QueueEntryWire {
            id: "p-drop".into(),
            version: 1,
            owner: None,
            last_editor: None,
            kind: "prompt".into(),
            text: "held".into(),
            position: 0,
            combined_texts: None,
        }];
        arm_send_now_and_paint(agent, "p-drop", None);
        assert!(agent.visible_queue_is_empty(), "armed row is hidden");
        // Interactive cancel drops the arm; the block still owns the message.
        agent.clear_send_now_expectation();
        assert!(
            agent.visible_queue_is_empty(),
            "painted-pending row must stay hidden after the arm drops"
        );
        assert_eq!(user_prompt_count(agent, "held"), 1);
        // Retiring resolves both: block gone, row visible again.
        agent.retire_send_now_painted_block("p-drop");
        assert!(!agent.visible_queue_is_empty());
        assert_eq!(user_prompt_count(agent, "held"), 0);
    }

    /// The adoption's text fallback must not claim a block owned by another
    /// pending send-now (map-less adoption racing a painted sibling).
    #[test]
    fn shim_fallback_skips_blocks_claimed_by_pending_send_nows() {
        let mut app = test_app_with_agent();
        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        push_send_now_user_block(agent, "p-owned", "prompt", "go", false);
        // p-other adopts with the same text but no map entry of its own.
        agent.note_self_originated_prompt("p-other");
        apply_turn_start_shim(agent, "p-other".into(), Some("go".into()), "prompt", None);
        assert_eq!(
            user_prompt_count(agent, "go"),
            2,
            "the fallback must push fresh instead of claiming p-owned's block"
        );
        assert!(agent.send_now_painted_blocks.contains_key("p-owned"));
    }

    /// A never-run send-now takes its block with it (no requeue duplicate).
    #[test]
    fn retire_send_now_painted_block_removes_ghost() {
        let mut app = test_app_with_agent();
        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        push_send_now_user_block(agent, "p-fail", "prompt", "lost?", false);
        assert_eq!(user_prompt_count(agent, "lost?"), 1);
        agent.retire_send_now_painted_block("p-fail");
        assert_eq!(user_prompt_count(agent, "lost?"), 0);
        assert!(agent.send_now_painted_blocks.is_empty());
    }

    /// The cancel-and-send arm survives the *matching* prompt's turn start (so
    /// the cancel rail can still suppress the marker when it races after adopt),
    /// but a non-matching (stale) arm is dropped.
    #[test]
    fn start_turn_boundary_keeps_send_now_cancel_expectation() {
        let mut app = test_app_with_agent();
        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        agent.arm_send_now_expectation("p-send-now".into());
        agent.start_turn_boundary(Some("p-send-now"));
        assert_eq!(
            agent.expect_send_now_cancel.as_deref(),
            Some("p-send-now"),
            "adopt must not clear the cancel-marker arm before the cancel PromptResponse"
        );
        assert!(
            agent.follow_without_jump_prompt_id.is_some(),
            "sticky pin is independent of start_turn_boundary"
        );
        // A different prompt starting the turn means the arm is stale — drop it
        // so it cannot suppress that turn's later real cancel marker.
        agent.start_turn_boundary(Some("p-other"));
        assert!(
            agent.expect_send_now_cancel.is_none(),
            "a non-matching turn start clears the stale send-now arm"
        );
    }

    #[test]
    fn drain_queue_when_running_does_nothing() {
        let mut app = test_app_with_agent();
        let id = AgentId(0);

        app.agents.get_mut(&id).unwrap().session.state = AgentState::TurnRunning;
        enqueue_local(&mut app, id, "queued");

        // DrainQueue while running → no effect.
        let effects = dispatch(Action::DrainQueue, &mut app);
        assert!(effects.is_empty());
        assert_eq!(app.agents[&id].session.queue_len(), 1);
    }

    #[test]
    fn drain_queue_blocked_during_loading_replay() {
        let mut app = test_app_with_agent();
        let id = AgentId(0);

        // Simulate session-resume state: Idle but still replaying.
        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.loading_replay = true;
        agent.session.enqueue_cron_prompt(
            "check status".into(),
            "task-1".into(),
            "every 5m".into(),
        );

        // Drain while loading_replay is true → must be blocked.
        let effects = maybe_drain_queue(app.agents.get_mut(&id).unwrap()).effects;
        assert!(
            effects.is_empty(),
            "drain must be blocked during loading_replay"
        );
        assert_eq!(
            app.agents[&id].session.queue_len(),
            1,
            "cron prompt must stay queued"
        );

        // Clear loading_replay (simulates SessionLoaded completing).
        app.agents.get_mut(&id).unwrap().session.loading_replay = false;

        // Drain again → should succeed now.
        let effects = maybe_drain_queue(app.agents.get_mut(&id).unwrap()).effects;
        assert_eq!(effects.len(), 1);
        assert!(
            matches!(&effects[0], Effect::SendPromptBlocks { .. }),
            "expected SendPromptBlocks effect, got: {:?}",
            effects[0]
        );
        assert_eq!(
            app.agents[&id].session.queue_len(),
            0,
            "queue must be empty after drain"
        );
        assert_eq!(
            app.agents[&id].cron_task_id.as_deref(),
            Some("task-1"),
            "cron_task_id must track the running cron task"
        );
    }

    #[test]
    fn drain_after_editing_sends_correct_prompt() {
        // Regression: editing #3, prompts #1 and #2 drain, #3 becomes front.
        // User presses Enter (save) → DrainQueue should send #3's updated text,
        // NOT #4 or the old text.
        let mut app = test_app_with_agent();
        let id = AgentId(0);

        // Queue 4 prompts, first drains.
        dispatch(Action::SendPrompt("p1".into()), &mut app);
        enqueue_local(&mut app, id, "p2");
        enqueue_local(&mut app, id, "p3");
        enqueue_local(&mut app, id, "p4");
        assert_eq!(app.agents[&id].session.queue_len(), 3); // p2, p3, p4

        // End turn for p1 → sets Idle → maybe_drain_queue pops p2 → Running again.
        // Queue is now: p3, p4.
        dispatch(end_turn(), &mut app);
        assert_eq!(app.agents[&id].session.queue_len(), 2);

        // Start editing p3 (now front).
        let p3_id = app.agents[&id].session.pending_prompts[0].id;
        app.agents.get_mut(&id).unwrap().prompt_mode = PromptMode::EditingQueued {
            id: p3_id,
            original: "p3".into(),
            server_id: None,
            kind: crate::app::agent::QueueEntryKind::Prompt,
        };

        // End turn for p2 → should NOT drain p3 (being edited), only FetchBilling.
        let effects = dispatch(end_turn(), &mut app);
        assert_eq!(effects.len(), 1);
        assert!(
            matches!(&effects[0], Effect::FetchBilling { silent: true, .. }),
            "drain should be blocked, only billing refresh"
        );
        assert_eq!(app.agents[&id].session.queue_len(), 2); // p3, p4

        // Simulate user saving edited text.
        app.agents
            .get_mut(&id)
            .unwrap()
            .session
            .pending_prompts
            .iter_mut()
            .find(|p| p.id == p3_id)
            .unwrap()
            .text = "p3-edited".into();
        app.agents.get_mut(&id).unwrap().prompt_mode = PromptMode::Normal;

        // DrainQueue after edit → should send "p3-edited", not "p4".
        let effects = dispatch(Action::DrainQueue, &mut app);
        assert_eq!(effects.len(), 1);
        assert!(
            matches!(&effects[0], Effect::SendPrompt { text, .. } if text == "p3-edited"),
            "should send the edited prompt"
        );
        // p4 should still be in queue.
        assert_eq!(app.agents[&id].session.queue_len(), 1);
        assert_eq!(app.agents[&id].session.pending_prompts[0].text, "p4");
    }

    #[test]
    fn drain_queue_blocked_during_reconnect() {
        let mut app = test_app_with_agent();
        let id = AgentId(0);

        // Enqueue a prompt while not reconnecting so it's queued.
        app.agents.get_mut(&id).unwrap().session.state = AgentState::TurnRunning;
        enqueue_local(&mut app, id, "queued");
        assert_eq!(app.agents[&id].session.queue_len(), 1);

        // Set idle + reconnect_pending: DrainQueue should be blocked.
        app.agents.get_mut(&id).unwrap().session.state = AgentState::Idle;
        app.reconnect_pending = true;

        let effects = dispatch(Action::DrainQueue, &mut app);
        assert!(effects.is_empty());
        assert_eq!(app.agents[&id].session.queue_len(), 1);
    }

    /// Regression (leader mode): a prompt queued during a turn must drain once
    /// the leader connection reconnects. Every normal drain trigger
    /// (PromptResponse / DrainQueue / send-prompt / session-created)
    /// early-returns while `reconnect_pending` is set, deferring the drain to
    /// the event loop's reconnect-complete arm. That arm clears
    /// `reconnect_pending`, force-idles the agent, then dispatches
    /// `Action::DrainQueue`. This test exercises that final drain step and
    /// guards against the queue silently stalling after a reconnect.
    #[test]
    fn drain_queue_after_reconnect_sends_queued_prompt() {
        let mut app = test_app_with_agent();
        let id = AgentId(0);

        // A prompt was queued behind a running turn before the outage.
        app.agents.get_mut(&id).unwrap().session.state = AgentState::TurnRunning;
        enqueue_local(&mut app, id, "queued");
        assert_eq!(app.agents[&id].session.queue_len(), 1);

        // During the outage every drain trigger is suppressed.
        app.reconnect_pending = true;
        let blocked = dispatch(Action::DrainQueue, &mut app);
        assert!(blocked.is_empty(), "drain must stay blocked mid-reconnect");
        assert_eq!(app.agents[&id].session.queue_len(), 1);

        // Reconnect completes: the event loop clears `reconnect_pending` and
        // force-idles the agent, then dispatches DrainQueue. Mirror that here.
        app.reconnect_pending = false;
        app.agents.get_mut(&id).unwrap().session.state = AgentState::Idle;

        let effects = dispatch(Action::DrainQueue, &mut app);
        assert_eq!(
            effects.len(),
            1,
            "queued prompt must drain once reconnect clears"
        );
        assert!(matches!(&effects[0], Effect::SendPrompt { .. }));
        assert!(app.agents[&id].session.state.is_turn_running());
        assert_eq!(
            app.agents[&id].session.queue_len(),
            0,
            "queue must be empty after the post-reconnect drain"
        );
    }

    #[test]
    fn parked_wait_renders_parked_without_markers() {
        let mut app = test_app_with_agent();
        let id = AgentId(0);
        dispatch(Action::SendPrompt("first".into()), &mut app);
        simulate_task_output_wait(app.agents.get_mut(&id).unwrap(), "bg-1");

        let agent = app.agents.get_mut(&id).unwrap();
        assert!(agent.renders_parked(), "live wait = parked look");
        assert_eq!(count_turn_markers(agent), 0, "a park writes no marker");
    }

    #[test]
    fn sibling_batch_park_writes_no_markers() {
        use crate::acp::meta::NotificationMeta;
        use std::sync::Arc;

        let mut app = test_app_with_agent();
        let id = AgentId(0);
        dispatch(Action::SendPrompt("first".into()), &mut app);
        let agent = app.agents.get_mut(&id).unwrap();

        // Blocking get_task_output registers first; anchor still running.
        simulate_task_output_wait_call(agent, "wait-1", "bg-anchor", 120_000);
        agent
            .session
            .bg_tasks
            .insert("bg-anchor".into(), running_bg_task("bg-anchor"));
        assert_eq!(count_turn_markers(agent), 0);

        for i in 2..=5 {
            let tc_id = format!("wait-batch-tc{i}");
            agent.session.handle_update(
                acp::SessionUpdate::ToolCall(
                    acp::ToolCall::new(
                        acp::ToolCallId::new(Arc::from(tc_id.as_str())),
                        "run_terminal_command",
                    )
                    .kind(acp::ToolKind::Execute)
                    .status(acp::ToolCallStatus::Pending),
                ),
                &NotificationMeta::default(),
                &mut agent.scrollback,
            );
        }
        assert_eq!(
            count_turn_markers(agent),
            0,
            "a sibling-batch park must write zero markers"
        );
    }

    #[test]
    fn chips_and_completions_mid_park_add_no_markers() {
        use crate::scrollback::block::RenderBlock;

        let mut app = test_app_with_agent();
        let id = AgentId(0);
        dispatch(Action::SendPrompt("first".into()), &mut app);
        let agent = app.agents.get_mut(&id).unwrap();

        simulate_task_output_wait_call(agent, "wait-1", "bg-1", 30_000);
        assert_eq!(count_turn_markers(agent), 0);

        agent.scrollback.push_block(RenderBlock::bg_task_completed(
            "sleep 5",
            "bg-2",
            std::time::Duration::from_secs(5),
        ));
        assert_eq!(count_turn_markers(agent), 0, "chips add no markers");
        assert!(agent.renders_parked(), "chips keep the parked look");
    }

    #[test]
    fn wait_completion_clears_parked_look() {
        let mut app = test_app_with_agent();
        let id = AgentId(0);
        dispatch(Action::SendPrompt("first".into()), &mut app);
        let agent = app.agents.get_mut(&id).unwrap();

        simulate_task_output_wait_call(agent, "wait-1", "bg-1", 15_000);
        assert!(agent.renders_parked());

        complete_task_output_wait_call(agent, "wait-1");
        assert!(!agent.renders_parked(), "no parked look between parks");
        assert_eq!(count_turn_markers(agent), 0);

        simulate_task_output_wait_call(agent, "wait-2", "bg-1", 15_000);
        assert!(agent.renders_parked(), "a re-park flips the look back on");
        assert_eq!(count_turn_markers(agent), 0, "re-parks stay markerless");
    }

    #[test]
    fn interjection_during_park_adds_no_marker() {
        use crate::scrollback::block::RenderBlock;

        let mut app = test_app_with_agent();
        let id = AgentId(0);
        dispatch(Action::SendPrompt("first".into()), &mut app);
        simulate_task_output_wait(app.agents.get_mut(&id).unwrap(), "bg-1");
        assert!(app.agents[&id].renders_parked());

        let _ = dispatch(
            Action::Interject {
                text: "hurry up".into(),
                images: Vec::new(),
            },
            &mut app,
        );

        let agent = &app.agents[&id];
        assert_eq!(
            count_turn_markers(agent),
            0,
            "no marker around the interjection"
        );
        assert!(
            matches!(
                agent.scrollback.last().map(|e| &e.block),
                Some(RenderBlock::UserPrompt(_))
            ),
            "the user prompt row lands with nothing under it"
        );
    }

    #[test]
    fn parked_wait_holds_queue_and_explains_itself() {
        let mut app = test_app_with_agent();
        let id = AgentId(0);
        dispatch(Action::SendPrompt("first".into()), &mut app);
        enqueue_local(&mut app, id, "queued row");
        simulate_task_output_wait(app.agents.get_mut(&id).unwrap(), "bg-1");

        let agent = app.agents.get_mut(&id).unwrap();
        assert_eq!(count_turn_markers(agent), 0);
        assert!(
            agent.renders_parked(),
            "the parked look is queue-occupancy-independent"
        );
        assert_eq!(
            agent.held_queue_count(),
            1,
            "held row feeds the inline status hint"
        );

        assert_eq!(agent.session.pending_prompts.len(), 1);
    }

    /// A foreground-subagent wait is sendable but never parks (parent blocked, not completed).
    #[test]
    fn subagent_wait_holds_queue_but_never_parks() {
        let mut app = test_app_with_agent();
        let id = AgentId(0);
        dispatch(Action::SendPrompt("first".into()), &mut app);
        enqueue_local(&mut app, id, "queued row");
        simulate_subagent_wait(app.agents.get_mut(&id).unwrap());

        let agent = app.agents.get_mut(&id).unwrap();
        assert_eq!(
            agent.held_queue_count(),
            1,
            "held row feeds the inline status hint"
        );
        assert_eq!(count_turn_markers(agent), 0);
        assert!(!agent.renders_parked());

        // Even with an empty queue + live subagent, a subagent wait never parks.
        agent
            .subagent_sessions
            .insert("child-1".into(), running_subagent_info("child-1"));
        agent.session.pending_prompts.clear();
        assert_eq!(
            count_turn_markers(agent),
            0,
            "subagent wait must never park"
        );
        assert!(
            !agent.renders_parked(),
            "subagent wait keeps running chrome"
        );
    }

    /// T1 regression: a live chunk must un-park even when the wait's terminal
    /// ToolCallUpdate never reached this client.
    #[test]
    fn parked_look_clears_when_model_resumes_streaming() {
        let mut app = test_app_with_agent();
        let id = AgentId(0);
        dispatch(Action::SendPrompt("first".into()), &mut app);
        simulate_task_output_wait(app.agents.get_mut(&id).unwrap(), "bg-1");

        let agent = app.agents.get_mut(&id).unwrap();
        assert!(agent.renders_parked(), "parked look active during the wait");

        // The model resumes with a message chunk (no Completed for the wait).
        let meta = crate::acp::meta::NotificationMeta::default();
        agent.session.handle_update(
            agent_client_protocol::SessionUpdate::AgentMessageChunk(
                agent_client_protocol::ContentChunk::new(
                    agent_client_protocol::ContentBlock::Text(
                        agent_client_protocol::TextContent::new("resuming".to_string()),
                    ),
                ),
            ),
            &meta,
            &mut agent.scrollback,
        );

        assert!(
            !agent.renders_parked(),
            "a live stream must un-park the chrome (spinner returns)"
        );
        assert!(
            !agent.is_parked_on_sendable_wait(),
            "the stale wait must not survive a resumed stream"
        );

        // A new-stream thought (different stream_start_ms) also un-parks; same-stream must not.
        {
            let wait_meta = crate::acp::meta::NotificationMeta {
                stream_start_ms: Some(1),
                ..Default::default()
            };
            agent.session.handle_update(
                acp::SessionUpdate::ToolCall(
                    acp::ToolCall::new(
                        acp::ToolCallId::new(std::sync::Arc::from("wait-2")),
                        "get_command_or_subagent_output",
                    )
                    .kind(acp::ToolKind::Other)
                    .status(acp::ToolCallStatus::Pending)
                    .content(vec![])
                    .locations(vec![]),
                ),
                &wait_meta,
                &mut agent.scrollback,
            );
            agent.session.handle_update(
                acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
                    acp::ToolCallId::new(std::sync::Arc::from("wait-2")),
                    acp::ToolCallUpdateFields::new().raw_input(Some(serde_json::json!({
                        "task_ids": ["bg-2"],
                        "timeout_ms": 30_000,
                    }))),
                )),
                &wait_meta,
                &mut agent.scrollback,
            );
        }
        assert!(agent.is_parked_on_sendable_wait());
        let thought_meta = crate::acp::meta::NotificationMeta {
            stream_start_ms: Some(9_001),
            ..Default::default()
        };
        agent.session.handle_update(
            agent_client_protocol::SessionUpdate::AgentThoughtChunk(
                agent_client_protocol::ContentChunk::new(
                    agent_client_protocol::ContentBlock::Text(
                        agent_client_protocol::TextContent::new("thinking again".to_string()),
                    ),
                ),
            ),
            &thought_meta,
            &mut agent.scrollback,
        );
        assert!(
            !agent.is_parked_on_sendable_wait(),
            "a new-stream thought must clear the stale wait"
        );
    }

    /// T4 regression: the hint must only advertise "Enter to send now" when
    /// the TOP held row would actually send (a bash top row no-ops).
    #[test]
    fn held_hint_advertises_send_now_only_for_sendable_top() {
        let mut app = test_app_with_agent();
        let id = AgentId(0);
        dispatch(Action::SendPrompt("first".into()), &mut app);
        simulate_task_output_wait(app.agents.get_mut(&id).unwrap(), "bg-1");

        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.enqueue_bash_command("git status".into());
        assert_eq!(agent.held_queue_count(), 1);
        assert!(
            !agent.held_queue_top_sendable(),
            "a bash top row must not advertise Enter-send-now"
        );

        agent.session.pending_prompts.clear();
        agent.session.enqueue_prompt("plain follow-up".into());
        assert!(agent.held_queue_top_sendable());

        // A server row (renders first in the merge) is always sendable.
        agent.shared_queue = vec![crate::app::prompt_queue::QueueEntryWire {
            id: "srv-1".into(),
            version: 0,
            owner: None,
            last_editor: None,
            kind: "bash".into(),
            text: "server bash".into(),
            position: 0,
            combined_texts: None,
        }];
        agent.session.pending_prompts.clear();
        agent.session.enqueue_bash_command("still bash".into());
        assert!(
            agent.held_queue_top_sendable(),
            "a server top row sends now regardless of kind"
        );
    }

    #[test]
    fn held_queue_count_matches_pane_with_send_now_echo() {
        let mut app = test_app_with_agent();
        let id = AgentId(0);
        dispatch(Action::SendPrompt("first".into()), &mut app);
        simulate_task_output_wait(app.agents.get_mut(&id).unwrap(), "bg-1");

        let agent = app.agents.get_mut(&id).unwrap();
        let running = agent
            .session
            .current_prompt_id
            .clone()
            .unwrap_or_else(|| "running".into());
        agent.expect_send_now_cancel = Some("send-now-echo".into());
        agent.shared_queue = vec![
            crate::app::prompt_queue::QueueEntryWire {
                id: "send-now-echo".into(),
                version: 0,
                owner: None,
                last_editor: None,
                kind: "prompt".into(),
                text: "send now payload".into(),
                position: 0,
                combined_texts: None,
            },
            crate::app::prompt_queue::QueueEntryWire {
                id: "held-1".into(),
                version: 0,
                owner: None,
                last_editor: None,
                kind: "prompt".into(),
                text: "genuine held".into(),
                position: 1,
                combined_texts: None,
            },
        ];
        agent.session.current_prompt_id = Some(running);
        agent.session.pending_prompts.clear();

        assert_eq!(
            agent.held_queue_count(),
            1,
            "hint counts only the genuine held row"
        );
        agent.sync_queue_pane();
        let pane_len = agent.queue.entry_ids().len();
        assert_eq!(pane_len, 1, "pane must drop the send-now echo too");
        assert_eq!(
            agent.held_queue_count(),
            pane_len,
            "hint and pane share one visible-held filter"
        );
        assert_eq!(
            agent.visible_held_queue_len(),
            pane_len,
            "ungated visible length matches pane rows"
        );
        assert!(agent.held_queue_top_sendable());
        assert_eq!(
            format!(" · {} queued, Enter to send now", agent.held_queue_count()),
            " · 1 queued, Enter to send now"
        );

        agent
            .shared_queue
            .push(crate::app::prompt_queue::QueueEntryWire {
                id: "held-2".into(),
                version: 0,
                owner: None,
                last_editor: None,
                kind: "prompt".into(),
                text: "second held".into(),
                position: 2,
                combined_texts: None,
            });
        assert_eq!(agent.held_queue_count(), 2);
        agent.sync_queue_pane();
        let pane_len = agent.queue.entry_ids().len();
        assert_eq!(pane_len, 2);
        assert_eq!(agent.held_queue_count(), pane_len);
        assert_eq!(
            format!(" · {} queued, Enter to send now", agent.held_queue_count()),
            " · 2 queued, Enter to send now"
        );
    }

    #[test]
    fn has_held_user_queue_includes_armed_send_now_echo() {
        let mut app = test_app_with_agent();
        let id = AgentId(0);
        dispatch(Action::SendPrompt("first".into()), &mut app);
        simulate_task_output_wait(app.agents.get_mut(&id).unwrap(), "bg-1");

        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.pending_prompts.clear();
        agent.shared_queue.clear();
        assert!(
            !agent.has_held_user_queue(),
            "empty held occupancy while parked"
        );

        agent.expect_send_now_cancel = Some("send-now-echo".into());
        agent.shared_queue = vec![crate::app::prompt_queue::QueueEntryWire {
            id: "send-now-echo".into(),
            version: 0,
            owner: None,
            last_editor: None,
            kind: "prompt".into(),
            text: "send now payload".into(),
            position: 0,
            combined_texts: None,
        }];
        assert!(
            agent.has_held_user_queue(),
            "armed send-now occupies hold even when pane-visible count is 0"
        );
        assert_eq!(
            agent.visible_held_queue_len(),
            0,
            "pane still hides the echo"
        );
        assert_eq!(agent.held_queue_count(), 0);
    }

    /// An arm that became the running turn is not held occupancy — otherwise a
    /// new prompt is wrongly held behind an empty queue after a send-now adopts.
    #[test]
    fn has_held_user_queue_excludes_arm_that_is_running() {
        let mut app = test_app_with_agent();
        let id = AgentId(0);
        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.pending_prompts.clear();
        agent.shared_queue.clear();

        agent.expect_send_now_cancel = Some("p-run".into());
        agent.session.current_prompt_id = Some("p-run".into());
        assert!(
            !agent.has_held_user_queue(),
            "an arm for the running turn is not held occupancy"
        );

        agent.session.current_prompt_id = Some("p-other".into());
        assert!(
            agent.has_held_user_queue(),
            "an arm for a non-running prompt still occupies hold"
        );
    }

    #[test]
    fn local_delete_of_last_held_row_adds_no_marker() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut app = test_app_with_agent();
        let id = AgentId(0);
        dispatch(Action::SendPrompt("first".into()), &mut app);
        enqueue_local(&mut app, id, "held row");
        simulate_task_output_wait(app.agents.get_mut(&id).unwrap(), "bg-1");

        let agent = app.agents.get_mut(&id).unwrap();
        assert_eq!(count_turn_markers(agent), 0);
        assert!(
            agent.renders_parked(),
            "parked look on even with a held row"
        );

        // Delete the row through the queue-pane key path.
        agent.queue.sync_from_merged(
            &agent.session.pending_prompts,
            &agent.shared_queue,
            agent.session.current_prompt_id.as_deref(),
            agent.expect_send_now_cancel.as_deref(),
            &agent.send_now_painted_blocks,
        );
        let ids = agent.queue.entry_ids();
        assert_eq!(ids.len(), 1);
        agent.queue.list_state.select_by_id(ids[0]);
        let registry = crate::actions::ActionRegistry::defaults();
        let _ = agent.handle_queue_key(
            &KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
            &registry,
        );

        assert!(agent.session.pending_prompts.is_empty());
        assert_eq!(
            count_turn_markers(agent),
            0,
            "deleting the last held row must not write a marker"
        );
        assert!(agent.renders_parked(), "the stopped look stays on");
    }

    /// T3 regression: a task-tool refinement that OMITS `run_in_background`
    /// means background (the shell's serde default is true) — the provisional
    /// foreground Subagent wait must clear, not stick as "Waiting on subagent…".
    #[test]
    fn task_refinement_without_background_field_defaults_to_background() {
        use std::sync::Arc;

        use crate::acp::tracker::{TurnActivity, WaitingReason};

        let mut app = test_app_with_agent();
        let id = AgentId(0);
        dispatch(Action::SendPrompt("first".into()), &mut app);
        let agent = app.agents.get_mut(&id).unwrap();

        let meta = crate::acp::meta::NotificationMeta::default();
        // Meta-less task ToolCall (old shell): provisional Subagent wait.
        agent.session.handle_update(
            agent_client_protocol::SessionUpdate::ToolCall(
                agent_client_protocol::ToolCall::new(
                    agent_client_protocol::ToolCallId::new(Arc::from("task-old-shell")),
                    "task",
                )
                .kind(agent_client_protocol::ToolKind::Other)
                .status(agent_client_protocol::ToolCallStatus::Pending)
                .content(vec![])
                .locations(vec![]),
            ),
            &meta,
            &mut agent.scrollback,
        );
        assert!(
            matches!(
                agent.resolve_turn_activity(),
                Some(TurnActivity::Waiting(WaitingReason::Subagent { .. }))
            ),
            "the meta-less spawn starts with a provisional foreground wait"
        );
        // Refinement with raw_input that omits run_in_background/background.
        agent.session.handle_update(
            agent_client_protocol::SessionUpdate::ToolCallUpdate(
                agent_client_protocol::ToolCallUpdate::new(
                    agent_client_protocol::ToolCallId::new(Arc::from("task-old-shell")),
                    agent_client_protocol::ToolCallUpdateFields::new().raw_input(Some(
                        serde_json::json!({
                            "variant": "Task",
                            "task_id": "t-1",
                            "prompt": "explore"
                        }),
                    )),
                ),
            ),
            &meta,
            &mut agent.scrollback,
        );

        assert!(
            !matches!(
                agent.resolve_turn_activity(),
                Some(TurnActivity::Waiting(WaitingReason::Subagent { .. }))
            ),
            "an omitted run_in_background field means background: the provisional \
             foreground wait must clear"
        );
    }

    /// A plain mid-turn interjection needs no suppression state for a later
    /// park in the same turn.
    #[test]
    fn interjection_then_later_park_still_renders_parked() {
        let mut app = test_app_with_agent();
        let id = AgentId(0);
        dispatch(Action::SendPrompt("first".into()), &mut app);

        let _ = dispatch(
            Action::Interject {
                text: "heads up".into(),
                images: Vec::new(),
            },
            &mut app,
        );
        assert!(
            !app.agents[&id].renders_parked(),
            "no wait → no parked look"
        );

        simulate_task_output_wait(app.agents.get_mut(&id).unwrap(), "bg-1");
        let agent = app.agents.get_mut(&id).unwrap();
        assert!(agent.renders_parked(), "later park renders parked");
        assert_eq!(count_turn_markers(agent), 0, "and stays markerless");
    }

    /// Parked chrome must clear OSC 9;4 (and treat the tab title as idle) so
    /// Ghostty/WezTerm drop the progress bar while the session looks stopped.
    #[test]
    fn parked_wait_clears_progress_bar_notification() {
        let mut app = test_app_with_agent();
        let id = AgentId(0);
        dispatch(Action::SendPrompt("first".into()), &mut app);

        app.update_notifications();
        assert!(
            app.notification_service.is_progress_active(),
            "live turn must keep the OSC 9;4 progress indicator active"
        );

        simulate_task_output_wait(app.agents.get_mut(&id).unwrap(), "bg-1");
        let agent = app.agents.get_mut(&id).unwrap();
        assert!(agent.renders_parked());
        assert!(
            agent.session.state.is_busy(),
            "server-side turn remains running while parked"
        );

        app.pending_notification_escapes = None;
        app.update_notifications();
        assert!(
            !app.notification_service.is_progress_active(),
            "parked look must clear OSC 9;4 so the terminal progress bar stops"
        );
    }
}
