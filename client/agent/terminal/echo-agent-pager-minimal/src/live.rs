//! Minimal-mode live region: the small pinned viewport holding the running-turn
//! tail (model B), optional todos / `/btw` panels, a one-line status indicator,
//! and the always-focused prompt.
//!
//! Layout (top → bottom): live tail · todos · `/btw` · status · prompt ·
//! overlay/info · the `[ui.status_line]` row.
//! The tail shows the bottom of the uncommitted run (streaming
//! message / running tool) so output is visible as it generates; finished blocks
//! scroll up into native scrollback via [`super::commit`]. When idle the tail is
//! empty and only status + prompt (+ optional panels) show.
use echo_agent_pager::app::PagerTerminal;
use echo_agent_pager::app::app_view::{ActiveView, AppView};
use echo_agent_pager::minimal_api;
use echo_agent_pager::render::Renderable;
use echo_agent_pager::scrollback::state::ScrollbackState;
use echo_agent_pager::scrollback::wrappers::EntryRenderer;
use echo_agent_pager::theme::Theme;
use echo_agent_pager::views::prompt_widget::{PromptBg, PromptStyle};
use echo_agent_pager::views::turn_status;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Widget};
/// Left inset (columns) for every auxiliary live-region row: the status row,
/// the info bar, the exit hint, and the todo panel — and the prompt's
/// `chrome_pad_left`.
///
/// Minimal is flush-left: committed/tail blocks zero block pads via
/// [`super::commit::committed_appearance`] and reclaim the accent column via
/// `hide_accent`, so content glyphs (`◆` / `$` / message text) start at column
/// 0, matching the welcome card's outer edge. The prompt and auxiliary rows
/// share that left edge (no chrome pad) so nothing sits ragged against the
/// welcome box.
pub(super) fn live_left_inset(_appearance: &echo_agent_pager::appearance::AppearanceConfig) -> u16 {
    0
}
/// Shrink `area` from the left by `inset` columns (clamped to the width).
fn inset_left(area: Rect, inset: u16) -> Rect {
    let dx = inset.min(area.width);
    Rect {
        x: area.x + dx,
        width: area.width - dx,
        ..area
    }
}
/// Drop cached `/btw` geometry so minimal input cannot scroll an invisible
/// panel after a modal host path skipped painting it.
fn clear_btw_geometry(agent: &mut echo_agent_pager::app::agent_view::AgentView) {
    agent.last_btw_selection_model =
        echo_agent_pager::scrollback::text_selection::ResolvedSelectionModel::default();
    agent.last_btw_area = Rect::default();
}
/// Keep a paintable `/btw` area only when it is wholly inside the frame buffer.
fn paintable_btw_area(frame_area: Rect, area: Rect) -> Option<Rect> {
    (minimal_api::minimal_btw_geometry_is_paintable(area)
        && area.x >= frame_area.x
        && area.y >= frame_area.y
        && area.x.saturating_add(area.width) <= frame_area.x.saturating_add(frame_area.width)
        && area.y.saturating_add(area.height) <= frame_area.y.saturating_add(frame_area.height))
    .then_some(area)
}
/// The prompt style used by the minimal live region.
///
/// Shared with [`super::overlay::sync_viewport`] so viewport sizing measures the
/// prompt's height exactly as the live region will draw it.
///
/// `input_mode` wires special composer modes (bash `! `, feedback `~ `,
/// remember `# `) the same way the full TUI does — without this, `!` on an
/// empty prompt would flip mode invisibly (key consumed, default `❯` remains).
pub(super) fn prompt_style(
    appearance: &echo_agent_pager::appearance::AppearanceConfig,
    input_mode: echo_agent_pager::app::agent_view::PromptInputMode,
    theme: &Theme,
    multiline: bool,
) -> PromptStyle {
    PromptStyle {
        focused: true,
        show_prefix: appearance.prompt.show_prefix,
        vpad_top: 0,
        compact: appearance.prompt.compact,
        chrome: true,
        chrome_pad_left: live_left_inset(appearance),
        chrome_pad_right: 0,
        bg: PromptBg::Canvas(Color::Reset),
        accent_color_override: input_mode.accent_color(theme),
        border_color_override: None,
        prefix_override: input_mode.prefix_override(theme),
        placeholder_when_focused: false,
        placeholder_override: input_mode.placeholder_override(multiline),
        show_accent_line: false,
        show_borders: false,
        title: None,
        image_preview: true,
    }
}
/// Draw the pinned live region (tail + status + prompt) into the inline viewport.
pub fn draw_live(app: &mut AppView, terminal: &mut PagerTerminal) {
    let force_todos = minimal_api::minimal_show_todos(app);
    let auth_hint = crate::auth::minimal_auth_hint(
        &app.auth_state,
        &app.trust_state,
        app.has_access(),
        app.is_zdr_blocked(),
    );
    let pending_hint = minimal_pending_hint(&app.pending_action);
    let transcript_hint = if minimal_api::minimal_ctrl_o_opens_transcript(app) {
        "ctrl+o transcript"
    } else {
        "/transcript"
    };
    let transcript_progress = minimal_api::minimal_transcript_progress(app);
    let status_line_frame = minimal_api::status_line_frame(app);
    let AppView {
        cursor,
        agents,
        active_view,
        appearance,
        ..
    } = app;
    let agent_id = match active_view {
        ActiveView::Agent(id) => Some(*id),
        _ => None,
    };
    let theme = Theme::current();
    let commit_app = super::commit::committed_appearance(appearance);
    let compact = appearance.prompt.compact;
    let (input_mode, multiline) = agent_id
        .and_then(|id| agents.get(&id))
        .map(|a| (a.prompt_input_mode, a.multiline_mode))
        .unwrap_or_default();
    let style = prompt_style(appearance, input_mode, &theme, multiline);
    let row_inset = live_left_inset(appearance);
    let layout_cfg = &appearance.scrollback.layout;
    let term_h = terminal.last_known_area().height;
    if let Some(id) = agent_id
        && let Some(agent) = agents.get_mut(&id)
    {
        clear_btw_geometry(agent);
    }
    echo_agent_pager::render::draw::draw_frame(terminal, cursor, |frame, _link_spans| {
        let area = frame.area();
        if area.height == 0 || area.width < 4 {
            return (None, None);
        }
        Clear.render(area, frame.buffer_mut());
        let agent = agent_id.and_then(|id| agents.get_mut(&id));
        let Some(agent) = agent else {
            crate::auth::render_auth(frame.buffer_mut(), area, &theme, &auth_hint);
            return (None, None);
        };
        agent.active_pane = echo_agent_pager::app::agent_view::AgentPane::Prompt;
        let status_activity = minimal_advance_phase_timer(agent);
        let show_todos = crate::todo::todo_panel_visible(agent, force_todos);
        let queued = agent.session.pending_prompts.len() + agent.shared_queue.len();
        if let Some(kind) = super::panel::active(agent) {
            let cursor = super::panel::render(frame.buffer_mut(), area, agent, kind, &theme);
            return (cursor, None);
        }
        if super::overlay::app_modal_active(agent) {
            super::overlay::render_app_modal(frame.buffer_mut(), area, agent, compact);
            return (None, None);
        }
        if minimal_api::extensions_modal(agent).is_some() {
            let tick = (now_millis() / 100) as u64;
            if let Some(state) = minimal_api::extensions_modal_mut(agent) {
                echo_agent_pager::views::extensions_modal::render_extensions_modal(
                    frame.buffer_mut(),
                    area,
                    state,
                    None,
                    compact,
                    tick,
                );
            }
            return (None, None);
        }
        if let Some(modal) = super::overlay::active_modal(agent) {
            let status_h = 1u16.min(area.height);
            let sl_h = status_line_frame
                .height()
                .min(area.height.saturating_sub(status_h + 1));
            let content_w = area.width as usize;
            let modal_h = super::overlay::modal_height(modal, agent, term_h, content_w)
                .min(area.height.saturating_sub(status_h + sl_h))
                .max(1);
            let tail_h = area.height.saturating_sub(status_h + modal_h + sl_h);
            let tick = (now_millis() / 100) as u64;
            if tail_h > 0 {
                let turn_running = agent.session.state.is_turn_running();
                draw_tail(
                    frame.buffer_mut(),
                    Rect {
                        x: area.x,
                        y: area.y,
                        width: area.width,
                        height: tail_h,
                    },
                    &agent.scrollback,
                    turn_running,
                    &theme,
                    &commit_app,
                    &agent.session.cwd,
                    tick,
                );
            }
            render_minimal_status(
                frame.buffer_mut(),
                inset_left(
                    Rect {
                        x: area.x,
                        y: area.y + tail_h,
                        width: area.width,
                        height: status_h,
                    },
                    row_inset,
                ),
                agent,
                &status_activity,
                transcript_progress,
                &theme,
            );
            let modal_area = Rect {
                x: area.x,
                y: area.y + tail_h + status_h,
                width: area.width,
                height: modal_h,
            };
            if sl_h > 0 {
                render_config_status_line(
                    frame.buffer_mut(),
                    Rect {
                        x: area.x,
                        y: modal_area.y + modal_h,
                        width: area.width,
                        height: sl_h,
                    },
                    agent,
                    &status_line_frame,
                    &theme,
                );
            }
            let cursor = super::overlay::render_modal(
                frame.buffer_mut(),
                modal_area,
                modal,
                agent,
                &theme,
                term_h,
            );
            return (cursor, None);
        }
        let status_h = 1u16.min(area.height);
        let sl_h = status_line_frame
            .height()
            .min(area.height.saturating_sub(status_h + 1));
        let overlay_h = super::overlay::overlay_rows(&agent.prompt, area.width)
            .min(area.height.saturating_sub(status_h + sl_h + 1));
        let info_h = if overlay_h == 0 {
            1u16.min(area.height.saturating_sub(status_h + sl_h + 1))
        } else {
            0
        };
        let below_h = overlay_h + info_h + sl_h;
        let avail = area.height.saturating_sub(status_h + below_h);
        let prompt_h = agent
            .prompt
            .desired_height(area.width, &style, false, avail)
            .min(avail)
            .max(1);
        let rest = avail.saturating_sub(prompt_h);
        let raw_btw = if minimal_api::minimal_btw_surface_available(agent) {
            echo_agent_pager::views::btw_overlay::btw_panel_height(
                agent.btw_state.as_ref(),
                area.width,
            )
        } else {
            0
        };
        let btw_desired = minimal_api::minimal_btw_visible_height(raw_btw, area.width, rest);
        let after_btw = rest.saturating_sub(btw_desired);
        let todos_cap = if force_todos {
            after_btw
        } else {
            after_btw.min(crate::todo::MAX_TODO_ROWS)
        };
        let todo_lines = if show_todos {
            crate::todo::todo_panel_lines(agent, todos_cap, force_todos)
        } else {
            Vec::new()
        };
        let todos_h = (todo_lines.len() as u16).min(after_btw);
        let btw_h = btw_desired;
        let tail_h = rest.saturating_sub(todos_h + btw_h);
        let tick = agent.scrollback.animation_tick();
        if tail_h > 0 {
            let tail_area = Rect {
                x: area.x,
                y: area.y,
                width: area.width,
                height: tail_h,
            };
            let turn_running = agent.session.state.is_turn_running();
            draw_tail(
                frame.buffer_mut(),
                tail_area,
                &agent.scrollback,
                turn_running,
                &theme,
                &commit_app,
                &agent.session.cwd,
                tick,
            );
        }
        if todos_h > 0 {
            crate::todo::render_todo_panel(
                frame.buffer_mut(),
                inset_left(
                    Rect {
                        x: area.x,
                        y: area.y + tail_h,
                        width: area.width,
                        height: todos_h,
                    },
                    row_inset,
                ),
                &theme,
                &todo_lines,
            );
        }
        let btw_area = paintable_btw_area(
            area,
            Rect {
                x: area.x,
                y: area.y.saturating_add(tail_h).saturating_add(todos_h),
                width: area.width,
                height: btw_h,
            },
        );
        if let (Some(btw), Some(btw_area)) = (agent.btw_state.as_ref(), btw_area) {
            let focused = minimal_api::btw_focused(agent);
            echo_agent_pager::views::btw_overlay::render_btw_panel(
                frame.buffer_mut(),
                btw,
                btw_area,
                tick,
                focused,
                None,
                &mut agent.last_btw_selection_model,
                None,
                &[],
                None,
            );
            agent.last_btw_area = btw_area;
        }
        let status_area = inset_left(
            Rect {
                x: area.x,
                y: area.y + tail_h + todos_h + btw_h,
                width: area.width,
                height: status_h,
            },
            row_inset,
        );
        render_minimal_status(
            frame.buffer_mut(),
            status_area,
            agent,
            &status_activity,
            transcript_progress,
            &theme,
        );
        let prompt_area = Rect {
            x: area.x,
            y: area.y + tail_h + todos_h + btw_h + status_h,
            width: area.width,
            height: prompt_h,
        };
        if overlay_h > 0 {
            let overlay_area = Rect {
                height: area.height.saturating_sub(sl_h),
                ..area
            };
            super::overlay::render(
                frame.buffer_mut(),
                overlay_area,
                prompt_area,
                &mut agent.prompt,
                layout_cfg,
                compact,
                &theme,
            );
        } else if info_h > 0 {
            let info_area = inset_left(
                Rect {
                    x: area.x,
                    y: prompt_area.y + prompt_h,
                    width: area.width,
                    height: info_h,
                },
                row_inset,
            );
            if let Some(hint) = &pending_hint {
                render_exit_hint(frame.buffer_mut(), info_area, &theme, hint);
            } else {
                render_prompt_info(
                    frame.buffer_mut(),
                    info_area,
                    agent,
                    queued,
                    transcript_hint,
                    &theme,
                );
            }
        }
        if sl_h > 0 {
            render_config_status_line(
                frame.buffer_mut(),
                Rect {
                    x: area.x,
                    y: prompt_area.y + prompt_h + overlay_h + info_h,
                    width: area.width,
                    height: sl_h,
                },
                agent,
                &status_line_frame,
                &theme,
            );
        }
        let result = agent
            .prompt
            .draw(frame.buffer_mut(), prompt_area, None, &style, None, None);
        (
            result.cursor_pos,
            result
                .post_flush_escapes
                .map(echo_agent_pager::terminal::overlay::PostFlush::from),
        )
    });
}
fn live_tail_renderer<'a>(
    entry: &'a echo_agent_pager::scrollback::entry::ScrollbackEntry,
    theme: &'a Theme,
    appearance: &echo_agent_pager::appearance::AppearanceConfig,
    cwd: &'a std::path::Path,
    tick: u64,
) -> EntryRenderer<'a> {
    super::commit::minimal_renderer(entry, theme, appearance.clone(), cwd, tick)
}
/// Render the uncommitted tail (entries past the commit frontier), bottom-anchored
/// so the most recent output is always visible; the topmost visible entry is
/// clipped via `with_skip_rows` when the run is taller than the tail area.
///
/// Starts at the shared [`super::commit::scan_frontier`] stop point so it renders
/// exactly the entries [`tail_height`] measured (the viewport was sized to that —
/// any disagreement makes the prompt jump on commit).
#[allow(clippy::too_many_arguments)]
fn draw_tail(
    buf: &mut Buffer,
    area: Rect,
    sb: &ScrollbackState,
    turn_running: bool,
    theme: &Theme,
    appearance: &echo_agent_pager::appearance::AppearanceConfig,
    cwd: &std::path::Path,
    tick: u64,
) {
    if area.height == 0 {
        return;
    }
    let width = area.width;
    let renderer = |e| live_tail_renderer(e, theme, appearance, cwd, tick);
    let mut entries = Vec::new();
    let mut i = super::commit::scan_frontier(sb, turn_running).tail_start;
    while let Some(e) = sb.get(i) {
        entries.push(e);
        i += 1;
    }
    if entries.is_empty() {
        return;
    }
    let gap = super::commit::MINIMAL_BLOCK_GAP;
    let heights: Vec<u16> = entries
        .iter()
        .map(|e| renderer(*e).desired_height(width))
        .collect();
    let total: u16 = heights
        .iter()
        .fold(0u16, |acc, &h| acc.saturating_add(h).saturating_add(gap));
    let mut skip_top = total.saturating_sub(area.height);
    let mut y = area.y;
    let bottom = area.y + area.height;
    for (e, &content_h) in entries.iter().zip(&heights) {
        let slot_h = content_h.saturating_add(gap);
        if skip_top >= slot_h {
            skip_top -= slot_h;
            continue;
        }
        let slot_skip = skip_top;
        skip_top = 0;
        let entry_skip = slot_skip.min(content_h);
        let visible_content = content_h.saturating_sub(entry_skip);
        if visible_content > 0 {
            let draw_h = visible_content.min(bottom.saturating_sub(y));
            if draw_h == 0 {
                break;
            }
            let rect = Rect {
                x: area.x,
                y,
                width,
                height: draw_h,
            };
            renderer(*e).with_skip_rows(entry_skip).render(rect, buf);
            y += draw_h;
            if y >= bottom {
                break;
            }
        }
        let gap_skipped = slot_skip.saturating_sub(entry_skip);
        let gap_visible = gap
            .saturating_sub(gap_skipped)
            .min(bottom.saturating_sub(y));
        y += gap_visible;
        if y >= bottom {
            break;
        }
    }
}
/// Resolve the current turn activity and advance the phase timer when it
/// changes. The full TUI runs this inside its own `draw` (reset
/// `activity_started_at` on every phase transition); minimal has a separate
/// draw path, so it must drive the same logic or the phase timer would never
/// reset. Returns the resolved activity for [`render_minimal_status`].
fn minimal_advance_phase_timer(
    agent: &mut echo_agent_pager::app::agent_view::AgentView,
) -> Option<echo_agent_pager::acp::tracker::TurnActivity> {
    let activity = minimal_api::resolve_turn_activity(agent);
    if activity.as_ref() != minimal_api::last_activity(agent) {
        agent.activity_started_at = Some(std::time::Instant::now());
        minimal_api::set_last_activity(agent, activity.clone());
    }
    activity
}
/// Render the one-line minimal status indicator above the prompt.
///
/// Reuses the full-TUI [`turn_status::render_turn_status`] widget so minimal
/// surfaces the same rich activity detail (`Run …` / `Thinking…` /
/// `Waiting on subagent…` / `Retrying (attempt N)…` / `Cancelling…`), the
/// per-phase + turn timers, and the "… still running" cue (running commands /
/// monitors / loops / background subagents) — instead of collapsing
/// everything to "working…". Keyboard-only, so the mouse `[stop]` / `[↓]`
/// buttons are suppressed (`None`), and `flat_background` keeps the row
/// transparent like the rest of the live region. When the widget would draw
/// nothing a small `minimal · /help` hint is shown instead.
fn render_minimal_status(
    buf: &mut Buffer,
    area: Rect,
    agent: &echo_agent_pager::app::agent_view::AgentView,
    activity: &Option<echo_agent_pager::acp::tracker::TurnActivity>,
    transcript_progress: Option<(usize, usize)>,
    theme: &Theme,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    if let Some((done, total)) = transcript_progress {
        let style = theme.primary().bg(Color::Reset);
        buf.set_style(area, style);
        buf.set_span(
            area.x,
            area.y,
            &Span::styled(format!("rendering transcript… {done}/{total}"), style),
            area.width,
        );
        return;
    }
    let watchers = minimal_api::watchers(agent);
    let drain_blocked = minimal_api::drain_blocked(agent);
    let parked = minimal_api::renders_parked(agent);
    if !turn_status::should_show(
        &agent.session.state,
        drain_blocked,
        minimal_api::mcp_init_progress(agent),
        watchers,
        parked,
    ) {
        render_idle_hint(buf, area, theme);
        return;
    }
    let is_pending_user_input =
        !agent.permission_queue.is_empty() || minimal_api::question_view(agent).is_some();
    let goal_verifying = agent
        .goal_state
        .as_ref()
        .is_some_and(|g| g.verifying_completion);
    turn_status::render_turn_status(
        buf,
        area,
        turn_status::TurnStatusArgs {
            state: &agent.session.state,
            activity,
            turn_elapsed: agent.turn_elapsed(),
            activity_started_at: agent.activity_started_at,
            tick: agent.scrollback.animation_tick(),
            drain_blocked,
            buttons: None,
            has_running_execute: false,
            total_tokens: agent.context_state.as_ref().map(|c| c.used),
            mcp_init_progress: minimal_api::mcp_init_progress(agent),
            is_bash_turn: agent.bash_turn,
            is_pending_user_input,
            goal_verifying,
            watchers,
            parked,
            flat_background: true,
            held_queue: minimal_api::held_queue_count(agent),
            held_queue_top_sendable: minimal_api::held_queue_top_sendable(agent),
        },
    );
}
/// A `Reserved` frame paints nothing but must still record the size a command
/// script is told (`COLUMNS`/`LINES`): it sizes the script's first run.
fn render_config_status_line(
    buf: &mut Buffer,
    area: Rect,
    agent: &mut echo_agent_pager::app::agent_view::AgentView,
    frame: &echo_agent_pager::views::status_line::StatusLineFrame,
    theme: &Theme,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let Some(padding) = frame.padding() else {
        return;
    };
    if let Some(width) = minimal_api::status_line_inner_width(area.width, padding) {
        agent.last_status_line_size = Some(echo_agent_pager::views::status_line::RowSize {
            cols: width,
            lines: area.height,
        });
    }
    if let Some(display) = frame.display() {
        let _ = echo_agent_pager::views::status_line::render_status_line(
            buf, area, display, padding, theme,
        );
    }
}
/// Idle status: `minimal · [/fullscreen to go back ·] /help` (+ auto-set note).
fn render_idle_hint(buf: &mut Buffer, area: Rect, theme: &Theme) {
    let style = theme.dim().bg(Color::Reset);
    buf.set_style(area, style);
    let auto = echo_agent_pager::app::minimal_auto_set_for_mouse_leak();
    let switch_back = echo_agent_pager::app::minimal_show_switch_back_to_fullscreen();
    let hint = match (auto, switch_back) {
        (true, true) => {
            "minimal · auto-set on JetBrains/Windows due to JetBrains mouse reporting issues \
             · /fullscreen to go back · /help"
        }
        (true, false) => {
            "minimal · auto-set on JetBrains/Windows due to JetBrains mouse reporting issues · /help"
        }
        (false, true) => "minimal · /fullscreen to go back · /help",
        (false, false) => "minimal · /help",
    };
    buf.set_span(area.x, area.y, &Span::styled(hint, style), area.width);
}
/// Render the one-line info bar directly below the prompt: the selected model,
/// the active session mode (the Shift+Tab cycle: plan / always-approve / auto),
/// context usage (absolute + percentage), an `N queued` count when prompts
/// are waiting behind a running turn, and the full-transcript shortcut hint
/// (`transcript_hint`: "ctrl+o transcript", or "/transcript" where Ctrl+O is
/// the interject chord — Apple Terminal). Mirrors the regular TUI's model
/// label, mode flags, and context bar; the transcript hint stands in for the
/// full TUI's shortcuts bar, which minimal never renders — without it the
/// folded conversation has no visible way back to the full view. The mode flag
/// keeps its accent color so the Shift+Tab cycle — otherwise invisible in
/// minimal mode — is always shown. Drawn only when no menu/dropdown owns the
/// band below the prompt (the caller gates on that). The elapsed-time / token
/// count lives in the turn-status row above the prompt (see
/// [`render_minimal_status`]), so it is not repeated here.
fn render_prompt_info(
    buf: &mut Buffer,
    area: Rect,
    agent: &echo_agent_pager::app::agent_view::AgentView,
    queued: usize,
    transcript_hint: &str,
    theme: &Theme,
) {
    use echo_agent_pager::views::context_bar::fmt_tokens;
    let base = theme.primary().bg(Color::Reset);
    let sep = theme.dim().bg(Color::Reset);
    let mut segs: Vec<(String, Style)> = Vec::new();
    if let Some(label) = agent.prompt_input_mode.prompt_info_override() {
        segs.push((label.to_string(), base));
    } else {
        if let Some(model) = agent.session.models.current_model_name() {
            let label = match agent.session.models.reasoning_effort {
                Some(eff) => format!("{model} ({eff})"),
                None => model,
            };
            segs.push((label, base));
        }
        let effective_plan =
            minimal_api::plan_mode_pending(agent).unwrap_or(minimal_api::plan_mode_active(agent));
        let mode_flag: Option<(&str, Color)> = if effective_plan {
            Some(("plan", theme.accent_plan))
        } else if agent.session.is_yolo() {
            Some(("always-approve", theme.warning))
        } else if agent.session.is_auto() {
            Some(("auto", theme.accent_system))
        } else {
            None
        };
        if let Some((label, color)) = mode_flag {
            segs.push((label.to_string(), base.fg(color)));
        }
        let used = agent.context_state.as_ref().map(|c| c.used);
        let total = agent
            .context_state
            .as_ref()
            .and_then(|c| (c.total > 0).then_some(c.total))
            .or_else(|| agent.session.models.get_context_window());
        if let (Some(used), Some(total)) = (used, total)
            && total > 0
        {
            let pct = echo_agent_token_estimation::usage_percentage(used, total);
            segs.push((
                format!("{} / {} ({:.0}%)", fmt_tokens(used), fmt_tokens(total), pct),
                base,
            ));
        }
    }
    if queued > 0 {
        segs.push((format!("{queued} queued"), base));
        segs.push(("/queue".to_string(), base));
    }
    segs.push((transcript_hint.to_string(), base));
    if segs.is_empty() {
        return;
    }
    buf.set_style(area, base);
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (i, (text, style)) in segs.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", sep));
        }
        spans.push(Span::styled(text, style));
    }
    buf.set_line(area.x, area.y, &Line::from(spans), area.width);
}
/// The double-press confirmation hint to show under the prompt (e.g. "press
/// Ctrl+q again to quit"), or `None` when nothing is armed / it has expired or
/// is a silent arm (no label). Mirrors the full-TUI shortcuts-bar `PendingHint`,
/// which minimal does not render.
fn minimal_pending_hint(
    pending: &Option<echo_agent_pager::app::app_view::PendingAction>,
) -> Option<String> {
    let pending = pending.as_ref()?;
    if pending.expired() {
        return None;
    }
    let label = pending.label?;
    Some(format!(
        "press {} again to {label}",
        pending.shortcut.display()
    ))
}
/// Render the one-line double-press confirmation hint under the prompt, in the
/// warning color so it stands out from the model/context info row.
fn render_exit_hint(buf: &mut Buffer, area: Rect, theme: &Theme, hint: &str) {
    let style = Style::default().fg(theme.warning).bg(Color::Reset);
    buf.set_style(area, style);
    buf.set_span(
        area.x,
        area.y,
        &Span::styled(hint.to_string(), style),
        area.width,
    );
}
/// Height (rows) of the tail that will REMAIN after this frame's commit pass —
/// i.e. the entries `commit_active` will NOT consume, from the first
/// non-committable entry (past the scan cursor) onward.
///
/// The overlay host sizes the live viewport to this *post-commit* tail so the
/// prompt sits right after the streaming output (no fixed gap while a turn is
/// "thinking" with nothing streamed yet). Sizing to the post-commit tail
/// (rather than the current tail) is load-bearing: because `sync_viewport` runs
/// just *before* `commit_active`, the viewport is already at its post-commit
/// height when the commit's `insert_before` prints finalized blocks — so it can
/// reposition the correctly-sized viewport to sit directly after them
/// (content-anchored). Sizing to the tall streaming tail instead left the
/// viewport oversized at commit time, and the following collapse stranded the
/// prompt at the top of the screen (the "snaps to top" bug).
pub(super) fn tail_height(
    agent: &echo_agent_pager::app::agent_view::AgentView,
    width: u16,
    appearance: &echo_agent_pager::appearance::AppearanceConfig,
) -> u16 {
    let theme = Theme::current();
    let sb = &agent.scrollback;
    let turn_running = agent.session.state.is_turn_running();
    let gap = super::commit::MINIMAL_BLOCK_GAP;
    let mut i = super::commit::scan_frontier(sb, turn_running).tail_start;
    let mut total = 0u16;
    while let Some(e) = sb.get(i) {
        let h =
            live_tail_renderer(e, &theme, appearance, &agent.session.cwd, 0).desired_height(width);
        total = total.saturating_add(h).saturating_add(gap);
        i += 1;
    }
    total
}
fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn agent() -> echo_agent_pager::app::agent_view::AgentView {
        minimal_api::test_agent_view(Some("s1"), std::path::PathBuf::from("/tmp"))
    }
    #[test]
    fn btw_area_must_be_fully_paintable() {
        let frame = Rect::new(0, 0, 80, 20);
        assert_eq!(
            paintable_btw_area(frame, Rect::new(0, 4, 80, 3)),
            Some(Rect::new(0, 4, 80, 3))
        );
        assert!(!minimal_api::minimal_btw_size_is_paintable(11, 3));
        assert!(minimal_api::minimal_btw_size_is_paintable(12, 3));
        assert!(!minimal_api::minimal_btw_size_is_paintable(80, 2));
        assert!(paintable_btw_area(frame, Rect::new(0, 4, 11, 3)).is_none());
        assert!(paintable_btw_area(frame, Rect::new(0, 4, 80, 2)).is_none());
        assert!(paintable_btw_area(frame, Rect::new(0, 19, 80, 3)).is_none());
        assert!(paintable_btw_area(frame, Rect::new(79, 4, 2, 3)).is_none());
    }
    #[test]
    fn config_status_line_paints_and_records_the_script_size() {
        use echo_agent_pager::views::status_line::{
            RowSize, SanitizedText, StatusLineDisplay, StatusLineFrame,
        };
        use std::sync::Arc;
        let theme = Theme::current();
        let area = Rect::new(0, 0, 40, 1);
        let row_text = |buf: &Buffer| -> String {
            (0..area.width)
                .filter_map(|x| buf.cell((x, 0)).map(|c| c.symbol().to_string()))
                .collect()
        };
        let mut painted = agent();
        let mut buf = Buffer::empty(area);
        let frame = StatusLineFrame::On {
            display: Arc::new(StatusLineDisplay::Text(SanitizedText::new("demo row"))),
            padding: 2,
        };
        render_config_status_line(&mut buf, area, &mut painted, &frame, &theme);
        assert_eq!(
            painted.last_status_line_size,
            Some(RowSize { cols: 36, lines: 1 }),
            "a script's COLUMNS excludes the padding on both sides"
        );
        assert!(
            row_text(&buf).contains("demo row"),
            "the row must paint the script's text"
        );
        let mut reserved = agent();
        let mut buf = Buffer::empty(area);
        render_config_status_line(
            &mut buf,
            area,
            &mut reserved,
            &StatusLineFrame::Reserved { padding: 0 },
            &theme,
        );
        assert_eq!(
            reserved.last_status_line_size,
            Some(RowSize { cols: 40, lines: 1 })
        );
        assert_eq!(
            row_text(&buf).trim(),
            "",
            "a reserved row holds space but paints nothing"
        );
    }
    #[test]
    fn tail_height_uses_owning_session_cwd_for_tool_paths() {
        use echo_agent_pager::app::agent::AgentState;
        use echo_agent_pager::scrollback::RenderBlock;
        use echo_agent_pager::scrollback::entry::ScrollbackEntry;
        use echo_agent_pager::scrollback::types::DisplayMode;
        let cwd = std::path::PathBuf::from("/alternate/worktree");
        let mut agent = minimal_api::test_agent_view(Some("s1"), cwd.clone());
        agent.session.state = AgentState::TurnRunning;
        let mut entry = ScrollbackEntry::running(RenderBlock::edit(
            "/alternate/worktree/src/components/really_long_file_name.rs",
            None,
        ));
        entry.set_display_mode(DisplayMode::Expanded);
        agent.scrollback.push(entry);
        let appearance = super::super::commit::committed_appearance(
            &echo_agent_pager::appearance::AppearanceConfig::default(),
        );
        let theme = Theme::current();
        let entry = agent.scrollback.get(0).unwrap();
        let (width, painted_height, visible_accent_height) = (10..=40)
            .find_map(|width| {
                let painted =
                    live_tail_renderer(entry, &theme, &appearance, &cwd, 0).desired_height(width);
                let visible_accent = EntryRenderer::new(entry, &theme)
                    .with_appearance(appearance.clone())
                    .with_cwd(Some(&cwd))
                    .with_tick(0)
                    .with_flat_background(true)
                    .desired_height(width);
                (painted != visible_accent).then_some((width, painted, visible_accent))
            })
            .expect("fixture must wrap differently when the accent column is reclaimed");
        assert_ne!(painted_height, visible_accent_height);
        assert_eq!(
            tail_height(&agent, width, &appearance),
            painted_height.saturating_add(super::super::commit::MINIMAL_BLOCK_GAP)
        );
    }
    /// The tail and the committed footprint are one builder with a different
    /// tick; this is the net for anyone tempted to fork them again.
    #[test]
    fn the_animation_tick_never_changes_a_blocks_height() {
        use echo_agent_pager::scrollback::RenderBlock;
        use echo_agent_pager::scrollback::entry::ScrollbackEntry;
        minimal_api::set_show_thinking_blocks(true);
        let theme = Theme::current();
        let cwd = std::path::PathBuf::from("/tmp");
        let appearance = super::super::commit::committed_appearance(
            &echo_agent_pager::appearance::AppearanceConfig::default(),
        );
        let long = "reasoning that wraps a good few times even at a hundred and \
                    twenty columns because it simply keeps going and going and going";
        for block in [
            RenderBlock::thinking(long),
            RenderBlock::agent_message(long),
            RenderBlock::execute("ls -la"),
        ] {
            let entry = ScrollbackEntry::new(block);
            for width in [20u16, 40, 80, 120] {
                let live =
                    live_tail_renderer(&entry, &theme, &appearance, &cwd, 7).desired_height(width);
                let committed = live_tail_renderer(
                    &entry,
                    &theme,
                    &appearance,
                    &cwd,
                    super::super::commit::COMMITTED_TICK,
                )
                .desired_height(width);
                assert_eq!(
                    live, committed,
                    "{:?} @{width}: a block's height must not depend on the tick, or the \
                     prompt jumps on commit",
                    entry.block
                );
            }
        }
    }
    #[test]
    fn minimal_status_shows_rich_activity_and_idle_hint() {
        use echo_agent_pager::acp::tracker::TurnActivity;
        use echo_agent_pager::app::agent::AgentState;
        let theme = Theme::current();
        let area = Rect::new(0, 0, 60, 1);
        let read = |buf: &Buffer| -> String {
            (0..area.width)
                .filter_map(|x| buf.cell((x, 0)).map(|c| c.symbol().to_string()))
                .collect()
        };
        echo_agent_pager::app::set_minimal_show_switch_back_to_fullscreen_for_test(false);
        let a = agent();
        let mut buf = Buffer::empty(area);
        render_minimal_status(&mut buf, area, &a, &None, None, &theme);
        let idle = read(&buf);
        assert!(idle.contains("/help"), "idle hint: {idle:?}");
        assert!(
            !idle.contains("/fullscreen"),
            "cold start must not show switch-back: {idle:?}"
        );
        echo_agent_pager::app::set_minimal_show_switch_back_to_fullscreen_for_test(true);
        let mut buf = Buffer::empty(area);
        render_minimal_status(&mut buf, area, &a, &None, None, &theme);
        let switched = read(&buf);
        assert!(
            switched.contains("/fullscreen to go back"),
            "relaunch into minimal must show switch-back: {switched:?}"
        );
        echo_agent_pager::app::set_minimal_show_switch_back_to_fullscreen_for_test(false);
        let mut a = agent();
        a.session.state = AgentState::TurnRunning;
        let mut buf = Buffer::empty(area);
        render_minimal_status(
            &mut buf,
            area,
            &a,
            &Some(TurnActivity::Responding),
            None,
            &theme,
        );
        let text = read(&buf);
        assert!(text.contains("Responding"), "rich activity: {text:?}");
        let mut buf = Buffer::empty(area);
        render_minimal_status(
            &mut buf,
            area,
            &a,
            &Some(TurnActivity::Retrying {
                attempt: 2,
                max_retries: 3,
                reason: "transient error".to_string(),
            }),
            None,
            &theme,
        );
        assert!(read(&buf).contains("Retrying"), "retry: {:?}", read(&buf));
    }
    #[test]
    fn minimal_status_shows_idle_watching_cue() {
        use echo_agent_pager::app::agent::AgentState;
        let theme = Theme::current();
        let area = Rect::new(0, 0, 60, 1);
        let read = |buf: &Buffer| -> String {
            (0..area.width)
                .filter_map(|x| buf.cell((x, 0)).map(|c| c.symbol().to_string()))
                .collect()
        };
        let mut a = agent();
        a.session.state = AgentState::Idle;
        a.session.scheduled_tasks.insert(
            "loop-1".to_string(),
            echo_agent_pager::app::agent::ScheduledTaskInfo {
                task_id: "loop-1".to_string(),
                prompt: "do the thing".to_string(),
                human_schedule: "every 5m".to_string(),
                created_at: std::time::Instant::now(),
                next_fire_at: None,
                tag: "loop".to_string(),
                last_subagent_id: None,
            },
        );
        assert_eq!(minimal_api::watchers(&a).loops, 1);
        let mut buf = Buffer::empty(area);
        render_minimal_status(&mut buf, area, &a, &None, None, &theme);
        let text = read(&buf);
        assert!(
            text.contains("1 loop still running"),
            "watching cue: {text:?}"
        );
        assert!(!text.contains("/help"), "not the idle hint: {text:?}");
    }
    #[test]
    fn prompt_style_bash_mode_shows_bang_prefix() {
        use echo_agent_pager::app::agent_view::PromptInputMode;
        use echo_agent_pager::appearance::AppearanceConfig;
        let appearance = AppearanceConfig::default();
        let theme = Theme::current();
        let normal = prompt_style(&appearance, PromptInputMode::Normal, &theme, false);
        assert!(normal.prefix_override.is_none());
        assert!(normal.accent_color_override.is_none());
        assert!(normal.placeholder_override.is_none());
        let bash = prompt_style(&appearance, PromptInputMode::Bash, &theme, false);
        assert_eq!(
            bash.prefix_override,
            Some(("! ", theme.command)),
            "bash mode must paint the yellow `! ` prefix (full-TUI parity)"
        );
        assert_eq!(bash.accent_color_override, Some(theme.command));
        assert!(
            bash.placeholder_override.is_none(),
            "bash keeps the default placeholder"
        );
    }
    #[test]
    fn prompt_info_renders_model_context_and_queued() {
        let mut a = agent();
        a.context_state = Some(echo_agent_runtime::session::ContextInfo {
            used: 276_000,
            total: 2_000_000,
            ..Default::default()
        });
        let theme = Theme::current();
        let area = Rect::new(0, 0, 80, 1);
        let mut buf = Buffer::empty(area);
        render_prompt_info(&mut buf, area, &a, 3, "ctrl+o transcript", &theme);
        let text: String = (0..area.width)
            .filter_map(|x| buf.cell((x, 0)).map(|c| c.symbol().to_string()))
            .collect();
        assert!(text.contains("276K"), "absolute used tokens: {text:?}");
        assert!(text.contains("2.0M"), "total context window: {text:?}");
        assert!(text.contains('%'), "percentage: {text:?}");
        assert!(text.contains("3 queued"), "queued count: {text:?}");
        assert!(
            text.trim_end().ends_with("ctrl+o transcript"),
            "trailing transcript hint: {text:?}"
        );
    }
    #[test]
    fn prompt_info_bash_mode_shows_run_shell_command() {
        use echo_agent_pager::app::agent_view::PromptInputMode;
        let mut a = agent();
        a.prompt_input_mode = PromptInputMode::Bash;
        a.context_state = Some(echo_agent_runtime::session::ContextInfo {
            used: 276_000,
            total: 2_000_000,
            ..Default::default()
        });
        let theme = Theme::current();
        let area = Rect::new(0, 0, 80, 1);
        let mut buf = Buffer::empty(area);
        render_prompt_info(&mut buf, area, &a, 2, "ctrl+o transcript", &theme);
        let text: String = (0..area.width)
            .filter_map(|x| buf.cell((x, 0)).map(|c| c.symbol().to_string()))
            .collect();
        assert!(
            text.contains("Run shell command"),
            "bash mode info label: {text:?}"
        );
        assert!(
            !text.contains("276K"),
            "context usage hidden under bash mode: {text:?}"
        );
        assert!(text.contains("2 queued"), "queued still shown: {text:?}");
        assert!(
            text.trim_end().ends_with("ctrl+o transcript"),
            "transcript hint still trails: {text:?}"
        );
    }
    /// Where Ctrl+O is the interject chord (Apple Terminal) the caller passes
    /// the `/transcript` fallback, and the info row advertises that instead.
    #[test]
    fn prompt_info_shows_slash_transcript_fallback_hint() {
        let a = agent();
        let theme = Theme::current();
        let area = Rect::new(0, 0, 80, 1);
        let mut buf = Buffer::empty(area);
        render_prompt_info(&mut buf, area, &a, 0, "/transcript", &theme);
        let text: String = (0..area.width)
            .filter_map(|x| buf.cell((x, 0)).map(|c| c.symbol().to_string()))
            .collect();
        assert!(text.contains("/transcript"), "fallback hint: {text:?}");
        assert!(!text.contains("ctrl+o"), "no dead chord: {text:?}");
    }
    #[test]
    fn prompt_info_shows_session_mode_flag() {
        let theme = Theme::current();
        let area = Rect::new(0, 0, 80, 1);
        let read = |buf: &Buffer| -> String {
            (0..area.width)
                .filter_map(|x| buf.cell((x, 0)).map(|c| c.symbol().to_string()))
                .collect()
        };
        let render = |a: &echo_agent_pager::app::agent_view::AgentView| -> String {
            let mut buf = Buffer::empty(area);
            render_prompt_info(&mut buf, area, a, 0, "ctrl+o transcript", &theme);
            read(&buf)
        };
        let mut a = agent();
        let text = render(&a);
        assert!(!text.contains("plan"), "normal shows no flag: {text:?}");
        assert!(!text.contains("always-approve"), "normal: {text:?}");
        minimal_api::set_plan_mode_pending(&mut a, Some(true));
        assert!(render(&a).contains("plan"), "plan flag: {:?}", render(&a));
        minimal_api::set_plan_mode_pending(&mut a, None);
        minimal_api::set_plan_mode_active(&mut a, false);
        minimal_api::set_yolo_mode_for_test(&mut a.session, true);
        minimal_api::set_auto_mode_for_test(&mut a.session, true);
        let text = render(&a);
        assert!(text.contains("always-approve"), "yolo flag: {text:?}");
        minimal_api::set_yolo_mode_for_test(&mut a.session, false);
        let text = render(&a);
        assert!(text.contains("auto"), "auto flag: {text:?}");
    }
    #[test]
    fn pending_hint_formats_press_again() {
        use crossterm::event::{KeyCode, KeyModifiers};
        use echo_agent_pager::app::actions::Action;
        use echo_agent_pager::app::app_view::PendingAction;
        use echo_agent_pager::input::key::KeyShortcut;
        assert!(minimal_pending_hint(&None).is_none());
        let shortcut = KeyShortcut::new(KeyCode::Char('q'), KeyModifiers::CONTROL);
        let pending = Some(PendingAction::new(Action::Quit, shortcut, "quit"));
        assert_eq!(
            minimal_pending_hint(&pending).as_deref(),
            Some("press Ctrl+q again to quit")
        );
        let silent = Some(PendingAction::with_ttl(
            Action::Quit,
            shortcut,
            None,
            std::time::Duration::from_secs(1),
        ));
        assert!(minimal_pending_hint(&silent).is_none());
        let expired = Some(PendingAction::with_ttl(
            Action::Quit,
            shortcut,
            Some("quit"),
            std::time::Duration::ZERO,
        ));
        assert!(minimal_pending_hint(&expired).is_none());
    }
}
