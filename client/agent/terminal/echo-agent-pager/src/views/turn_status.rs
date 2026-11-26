//! Turn status line — single-row widget showing current turn activity.
//!
//! Layout: `⠧ Run command 0.2s              1m20s ⇣12k [stop]`
//!
//! - Spinner (left, slowed to ~7.5fps)
//! - Activity label (colored per activity type, truncates if needed)
//! - Phase timer `Xs` (gray, never truncates)
//! - Queued-send hint `· N queued, Enter to send now` (gray, sendable waits only)
//! - Fill space
//! - Turn timer `Xm Ys` and optional token count `⇣Nk` (right-aligned, gray)
//! - Cancel button `[stop]` (right-aligned, red on hover)
//!
//! Hidden when idle (0 height). Appears between scrollback and prompt.

use std::time::{Duration, Instant};

use echo_agent_workspace::permission::mcp_pretty_name_if_qualified;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::acp::tracker::{TurnActivity, WaitingReason};
use crate::app::agent::{AgentCommand, AgentState};
use crate::app::agent_view::McpInitProgress;
use crate::render::line_utils::truncate_str;
use crate::theme::Theme;

/// Show each spinner frame for this many animation ticks.
/// At ~30fps, 4 ticks = ~133ms per frame = ~7.5 spinner fps.
pub(crate) const SPINNER_DIVISOR: u64 = 4;

/// Show each monitor-pulse frame for this many animation ticks — twice the
/// [`SPINNER_DIVISOR`] dwell (~3.75 fps). The idle still-running cue should
/// breathe calmly rather than read like the active turn spinner, so its
/// `○ ◎ ◉ ◎` cycle runs at roughly half the speed (~1.07s per loop).
pub(crate) const MONITOR_PULSE_DIVISOR: u64 = 8;

/// Pulse speed for every "waiting on you" diamond — the drain-blocked
/// status, the pending-user-input status, and the plan-approval status
/// all share this cadence. `pulse_brightness` returns `sin²(tick*speed)`,
/// which has period π, so at ~30fps this is ~1.3s per cycle
/// (`π / (0.08 * 30) ≈ 1.31`).
///
/// Always route diamond rendering through [`pending_diamond_color`] so
/// the three call sites can never silently drift apart.
pub(crate) const USER_WAITING_PULSE_SPEED: f32 = 0.08;

/// Compute the pulsing diamond color for any "waiting on you" cue.
///
/// Blends `accent` toward `theme.bg_base` using a `sin²` pulse driven by
/// [`USER_WAITING_PULSE_SPEED`]. Brightness ranges from 0.3 (dim) to 1.0
/// (full accent) so the diamond stays visible at the trough.
///
/// Pass `theme.accent_user` for user-input waits (permission prompts,
/// `ask_user_question`, the drain-blocked idle status) and
/// `theme.accent_plan` for plan-approval waits.
pub(crate) fn pending_diamond_color(theme: &Theme, accent: Color, tick: u64) -> Color {
    let brightness = crate::theme::pulse_brightness(tick, USER_WAITING_PULSE_SPEED);
    crate::render::color::blend_color(theme.bg_base, accent, 0.3 + brightness * 0.7)
        .unwrap_or(accent)
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

/// Output from rendering the turn status line.
#[derive(Debug, Default)]
pub struct TurnStatusOutput {
    /// Hit area for the cancel button, if rendered.
    /// `None` when the button is not shown (idle, parked, drain-blocked).
    pub cancel_button: Option<Rect>,
    /// Hit area for the background-demote button, if rendered.
    pub bg_button: Option<Rect>,
    /// Hit area for the still-running watcher cue (click opens the tasks
    /// pane). `None` on keyboard-only hosts.
    pub watching_cue: Option<Rect>,
}

/// Hover state for the turn-status row's mouse affordances (`[stop]`, `[↓]`,
/// the still-running watcher cue). `Some(_)` renders them; `None` marks a
/// keyboard-only host (minimal mode — no mouse capture) and suppresses all.
#[derive(Debug, Clone, Copy, Default)]
pub struct MouseButtons {
    /// Whether the mouse is over the `[stop]` cancel button.
    pub cancel_hovered: bool,
    /// Whether the mouse is over the `[↓]` send-to-background button.
    pub bg_hovered: bool,
    /// Whether the mouse is over the still-running watcher cue.
    pub watching_hovered: bool,
}

/// Counts of idle-surviving "watcher" work — background jobs that can wake
/// the agent for a new turn while it sits idle (commands and monitors on
/// completion/events, `/loop` tasks on a timer, background subagents on
/// finish). They share one persistent still-running cue above the prompt.
/// Broader than the tasks-pane `Watchers` group (monitors + loops only).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Watchers {
    /// Running background commands (non-monitor `background: true` tasks).
    pub commands: usize,
    /// Running `monitor` background tasks.
    pub monitors: usize,
    /// Active scheduled `/loop` tasks.
    pub loops: usize,
    /// Running background subagents. While the agent is idle, any running
    /// subagent is a background one — a foreground subagent would keep the
    /// parent in `TurnRunning`.
    pub subagents: usize,
    pub workflows: usize,
}

impl Watchers {
    /// Total watcher count across all kinds.
    pub fn total(self) -> usize {
        self.commands + self.monitors + self.loops + self.subagents + self.workflows
    }

    /// Awaitable in-flight work — the kinds a blocking `wait_tasks` /
    /// `get_task_output` wait can resolve on (commands, monitors, subagents;
    /// scheduled `/loop` tasks and workflows are not task waits).
    pub fn awaitable_work(self) -> usize {
        self.commands + self.monitors + self.subagents
    }
}

/// Format a counts-first `"… still running"` cue from `(count, noun)` pairs,
/// listing only the non-zero kinds (plain-`s` plurals) — e.g.
/// `"1 command · 2 monitors still running"`. `None` when every count is
/// zero. Single owner of the format mechanics so the agent view's idle cue
/// and the dashboard's background-work label cannot drift.
pub(crate) fn format_still_running<'a>(
    kinds: impl IntoIterator<Item = (usize, &'a str)>,
) -> Option<String> {
    use std::fmt::Write as _;
    let mut label = String::with_capacity(48);
    for (count, noun) in kinds {
        if count == 0 {
            continue;
        }
        if !label.is_empty() {
            label.push_str(" \u{00b7} ");
        }
        let plural = if count == 1 { "" } else { "s" };
        let _ = write!(label, "{count} {noun}{plural}");
    }
    if label.is_empty() {
        return None;
    }
    label.push_str(" still running");
    Some(label)
}

/// The idle watcher cue's label — e.g.
/// `"1 command · 2 monitors · 1 loop · 1 subagent still running"`. Leads
/// with the counts (not an ambient "watching") so a glance under a
/// "Worked for X" marker still reads as unfinished work. `None` when no
/// watchers are live.
fn still_running_label(watchers: Watchers) -> Option<String> {
    format_still_running([
        (watchers.commands, "command"),
        (watchers.monitors, "monitor"),
        (watchers.loops, "loop"),
        (watchers.subagents, "subagent"),
        (watchers.workflows, "workflow"),
    ])
}

/// Whether the turn is blocked in a wait the shell aborts as soon as the
/// user sends a message (`get_task_output` with `timeout_ms`, `wait_tasks`,
/// `Await*`, and a foreground subagent await — mirrors the shell's blocking
/// waits, whose send-now routing cancels the blocked turn and runs the new
/// message next). Typing is actionable during these, which is what the
/// parked-wait rendering (`AgentView::is_parked_on_sendable_wait` /
/// `renders_parked`) builds on.
///
/// `Subagent` is included: the shell treats a blocked foreground subagent
/// await like the other blocking waits, so Enter sends promptly and pre-wait
/// rows read as held. `Model` waits stay excluded — the model is actively
/// producing the turn, so a message typed there queues behind real work. Pure
/// predicate over the resolved activity; no turn-lifecycle side effects.
pub fn is_sendable_wait(activity: &Option<TurnActivity>) -> bool {
    matches!(
        activity,
        Some(TurnActivity::Waiting(
            WaitingReason::TaskOutput { waits: true, .. }
                | WaitingReason::TasksComplete
                | WaitingReason::Sleep
                | WaitingReason::Subagent { .. }
        ))
    )
}

/// Inputs to [`render_turn_status`] — one frame's worth of turn state.
#[derive(Debug)]
pub struct TurnStatusArgs<'a> {
    pub state: &'a AgentState,
    pub activity: &'a Option<TurnActivity>,
    pub turn_elapsed: Option<Duration>,
    pub activity_started_at: Option<Instant>,
    pub tick: u64,
    pub drain_blocked: bool,
    /// Mouse affordances + hover state; `None` for keyboard-only hosts.
    pub buttons: Option<MouseButtons>,
    pub has_running_execute: bool,
    /// Context-window tokens used, shown as `⇣Nk`.
    pub total_tokens: Option<u64>,
    pub mcp_init_progress: Option<&'a McpInitProgress>,
    pub is_bash_turn: bool,
    pub is_pending_user_input: bool,
    pub goal_verifying: bool,
    pub watchers: Watchers,
    /// Parked on a sendable wait (`AgentView::renders_parked`).
    pub parked: bool,
    /// Transparent right-side background so the row blends with the
    /// terminal's own background (minimal mode).
    pub flat_background: bool,
    pub held_queue: usize,
    pub held_queue_top_sendable: bool,
}

/// Render the turn status line into the given area.
///
/// The caller is responsible for only allocating a 1-row area when
/// `should_show()` returns true (and 0 rows when false).
pub fn render_turn_status(
    buf: &mut Buffer,
    area: Rect,
    args: TurnStatusArgs<'_>,
) -> TurnStatusOutput {
    let TurnStatusArgs {
        state,
        activity,
        turn_elapsed,
        activity_started_at,
        tick,
        drain_blocked,
        buttons,
        has_running_execute,
        total_tokens,
        mcp_init_progress,
        is_bash_turn,
        is_pending_user_input,
        goal_verifying,
        watchers,
        parked,
        flat_background,
        held_queue,
        held_queue_top_sendable,
    } = args;
    // Resolve the mouse affordances: a keyboard-only host (`None`) suppresses
    // both buttons and reports no hover.
    let show_buttons = buttons.is_some();
    let cancel_hovered = buttons.is_some_and(|b| b.cancel_hovered);
    let bg_hovered = buttons.is_some_and(|b| b.bg_hovered);
    if area.height == 0 || area.width < 10 {
        return TurnStatusOutput::default();
    }

    let theme = Theme::current();

    // MCP startup seed (total == 0) while idle — show "Starting session…"
    // above the prompt until the shell reports real server counts. Real MCP
    // progress (total > 0) renders as the compact top-bar chip instead, not
    // here. Auto-expires via `is_visible()` if the shell never reports.
    if state.is_idle()
        && !drain_blocked
        && let Some(progress) = mcp_init_progress
        && progress.total == 0
        && progress.is_visible()
    {
        render_starting_session(buf, area, progress, tick, &theme);
        return TurnStatusOutput::default();
    }

    // Special case: drain is blocked (user editing front prompt, agent idle).
    // No cancel button in this state.
    if drain_blocked && state.is_idle() {
        // Pulsing diamond in accent_user, blending toward bg.
        let diamond_color = pending_diamond_color(&theme, theme.accent_user, tick);
        let spans = vec![
            Span::styled(
                format!("{} ", crate::glyphs::diamond_filled()),
                Style::default().fg(diamond_color),
            ),
            Span::styled(
                "agent idle ~ waiting on your edit",
                Style::default().fg(theme.gray),
            ),
        ];
        buf.set_line(area.x, area.y, &Line::from(spans), area.width);
        return TurnStatusOutput::default();
    }

    // Idle or parked: persistent cue (not scrollback — it must never scroll
    // away). Lower priority than the starting-session and drain-blocked cues
    // above. Parked never falls through to the running-turn chrome
    // (spinner/timers/[stop]) — the wait aborts the moment the user types,
    // so that chrome would lie.
    if state.is_idle() || parked {
        // Parked with held queued rows: the queued hint IS the input-semantics
        // story (Enter acts on the queue immediately), so it replaces the
        // generic interrupt copy.
        let parked_suffix = if held_queue > 0 && held_queue_top_sendable {
            format!(" \u{00b7} {held_queue} queued, Enter to send now")
        } else if held_queue > 0 {
            format!(" \u{00b7} {held_queue} queued")
        } else {
            " \u{00b7} send a message to interrupt".to_string()
        };
        let cue = match (still_running_label(watchers), parked) {
            (Some(label), true) => Some(format!("{label}{parked_suffix}")),
            (Some(label), false) => Some(label),
            (None, true) => Some(format!("waiting{parked_suffix}")),
            (None, false) => None,
        };
        if let Some(cue) = cue {
            // Pulsing concentric circle (○ ◎ ◉ ◎) on a calm ambient cadence:
            // the agent is idle, so this breath runs slower than the active
            // turn spinner (see MONITOR_PULSE_DIVISOR).
            let frames = crate::glyphs::monitor_icon_frames();
            let frame_idx = (tick / MONITOR_PULSE_DIVISOR) as usize % frames.len();
            let icon = format!("{} ", frames[frame_idx]);
            let label_fg = if buttons.is_some_and(|b| b.watching_hovered) {
                theme.text_primary
            } else {
                theme.gray
            };
            let cue_width = (icon.width() + cue.width()).min(area.width as usize) as u16;
            let spans = vec![
                Span::styled(icon, Style::default().fg(theme.accent_system)),
                Span::styled(cue, Style::default().fg(label_fg)),
            ];
            buf.set_line(area.x, area.y, &Line::from(spans), area.width);
            // The cue opens the tasks pane on click — only advertise the hit
            // area when there are tasks to show (a watcherless parked cue has
            // nothing behind it).
            return TurnStatusOutput {
                watching_cue: (show_buttons && watchers.total() > 0)
                    .then(|| Rect::new(area.x, area.y, cue_width, 1)),
                ..TurnStatusOutput::default()
            };
        }
        return TurnStatusOutput::default();
    }

    // Shown while running AND while cancelling (the click routes to the
    // cancel-retry path); hidden when idle or on a keyboard-only host.
    let show_cancel = show_buttons
        && matches!(
            state,
            AgentState::TurnRunning
                | AgentState::CommandRunning { .. }
                | AgentState::TurnCancelling
                | AgentState::CommandCancelling { .. }
        );

    // ── Compute activity style and label ──
    let (activity_style, label, is_tool) =
        compute_activity(&theme, state, activity, is_bash_turn, goal_verifying);

    // Early return for idle (shouldn't happen if should_show is respected, but be safe).
    if matches!(state, AgentState::Idle) {
        return TurnStatusOutput::default();
    }

    // ── Build right-aligned content first (to know how much space is left) ──
    // Format: `1m20s` or `1m20s ⇣12k` (with tokens).
    let turn_timer_str = match (turn_elapsed, total_tokens) {
        (Some(d), Some(tokens)) if tokens > 0 => {
            format!(
                "{} {}{}",
                format_turn_timer(d),
                crate::glyphs::token_arrow(),
                format_tokens_short(tokens)
            )
        }
        (Some(d), _) => format_turn_timer(d),
        _ => String::new(),
    };
    let turn_timer_width = turn_timer_str.width();

    // Bg button: [↓] normally, [send to bg] when hovered. Running execute
    // tools only, and never while cancelling (demote no-ops there).
    let show_bg = show_cancel
        && has_running_execute
        && matches!(
            state,
            AgentState::TurnRunning | AgentState::CommandRunning { .. }
        );
    let bg_str = if show_bg {
        if bg_hovered {
            " [send to bg]"
        } else {
            " [\u{2193}]"
        }
    } else {
        ""
    };
    let bg_width = bg_str.width();

    // Cancel button: always `[stop]`. Leading space only when the bg button
    // is not shown (otherwise they're adjacent). Every arm is a `&'static str`
    // so the per-frame status line never allocates. Hover state is conveyed by
    // color (red on hover, see `cancel_style`), not by swapping the label.
    let cancel_str: &str = match (show_cancel, show_bg) {
        (false, _) => "",
        (true, true) => "[stop]",
        (true, false) => " [stop]",
    };
    let cancel_width = cancel_str.width();

    let right_width = turn_timer_width + bg_width + cancel_width;

    // ── Build components ──
    // While a tool is blocked on a permission prompt or `ask_user_question`,
    // swap the running braille spinner for a pulsing `◆`. Same animation
    // shape the drain-blocked and plan-approval indicators already use,
    // so every "your turn" status reads with one consistent visual cue.
    let spinner_str = if is_pending_user_input {
        format!("{} ", crate::glyphs::diamond_filled())
    } else {
        let frames = crate::glyphs::braille_spinner_frames();
        let frame_idx = (tick / SPINNER_DIVISOR) as usize % frames.len();
        format!("{} ", frames[frame_idx])
    };
    let spinner_width = spinner_str.width();

    // "Ask" tools (AskUserQuestion): suppress the phase timer so the user
    // doesn't feel time-pressured while answering questions.
    let is_asking = is_tool
        && matches!(
            activity,
            Some(TurnActivity::ToolRunning { title, .. })
                if title.starts_with("Ask: ") || title.starts_with("Ask ")
        );

    // Phase timer (gray, same as turn timer) — hidden for ask tools
    let phase_timer_str = if is_asking {
        String::new()
    } else {
        activity_started_at
            .map(|t| format!(" {}", format_turn_timer(t.elapsed())))
            .unwrap_or_default()
    };
    let phase_timer_width = phase_timer_str.width();

    // Timer style (gray for both phase and turn timers).
    //
    // Right-side elements (turn timer, bg button, cancel button) must set
    // fg, bg, AND remove_modifier explicitly. fill_background() paints
    // bg_base on every cell before widgets render, but set_line() for the
    // left content may overwrite fg/modifiers on cells in the right zone.
    // A Style with bg:None (the default) cannot restore bg after a reset,
    // and a Style without remove_modifier cannot clear leaked modifiers.
    // Right-side cells normally paint `bg_base`; in a flat-background host
    // (minimal mode) use the terminal's own background so the row stays
    // transparent like the rest of the live region.
    let timer_bg = if flat_background {
        Color::Reset
    } else {
        theme.bg_base
    };
    let timer_style = Style::default()
        .fg(theme.gray)
        .bg(timer_bg)
        .remove_modifier(Modifier::all());

    // Available width for activity label (only the label truncates)
    // Layout: spinner + label + phase_timer + queued_hint + gap(1) + turn_timer + cancel
    let min_gap = 1;
    let available_for_label = (area.width as usize)
        .saturating_sub(spinner_width)
        .saturating_sub(phase_timer_width)
        .saturating_sub(min_gap)
        .saturating_sub(right_width);

    // ── Render left side: spinner + label (truncated) + phase_timer + queued_hint ──
    let mut left_spans: Vec<Span<'static>> = Vec::with_capacity(5);

    // Spinner color: usually inherits the activity color (green for tools,
    // secondary for thinking/responding, yellow for retries). While the
    // tool is parked on the user we render `◆` with a smooth pulse from
    // dim→bright in `accent_user`, matching the drain-blocked and
    // plan-approval indicators so every "your turn" status has the same
    // visual cadence.
    let spinner_style = if is_pending_user_input {
        let diamond_color = pending_diamond_color(&theme, theme.accent_user, tick);
        Style::default().fg(diamond_color)
    } else {
        activity_style
    };
    left_spans.push(Span::styled(spinner_str, spinner_style));

    // Activity label (potentially truncated)
    let mut queued_hint: Option<Span<'static>> = None;
    if is_tool {
        if let Some(TurnActivity::ToolRunning { title, description }) = activity {
            if is_asking {
                // Ask tools: render as a unified gray label (like Thinking/Responding),
                // not as a command invocation — yellow is reserved for shell commands.
                let detail = title
                    .strip_prefix("Ask: ")
                    .or_else(|| title.strip_prefix("Ask "))
                    .unwrap_or(title.as_str());
                let msg = format!("Waiting on answers for {detail}");
                let display = truncate_str(&msg, available_for_label);
                left_spans.push(Span::styled(display, activity_style));
            } else if let Some(desc) = description
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                // Bash (and similar) tools carry a human description — prefer
                // that over the raw command for the status line so a sleep /
                // long-running exec reads as `{description}…` rather than
                // `Run sleep 5 && …`.
                let msg = crate::acp::tracker::format_waiting_for_subject(desc);
                let display = truncate_str(&msg, available_for_label);
                left_spans.push(Span::styled(display, activity_style));
            } else if let Some(query) = title.strip_prefix("Web search: ") {
                // Web search: "Search " (muted) + query (yellow)
                let prefix = "Search ";
                let prefix_width = prefix.width();
                let query = query.trim_matches('"');
                let max_query = available_for_label.saturating_sub(prefix_width).max(5);
                let display = truncate_str(query, max_query);
                left_spans.push(Span::styled(prefix, Style::default().fg(theme.gray)));
                left_spans.push(Span::styled(display, Style::default().fg(theme.command)));
            } else if let Some(url) = title.strip_prefix("Fetch: ") {
                // Fetch tools: "Fetch " (muted) + URL (yellow)
                let prefix = "Fetch ";
                let prefix_width = prefix.width();
                let max_url = available_for_label.saturating_sub(prefix_width).max(5);
                let display = truncate_str(url, max_url);
                left_spans.push(Span::styled(prefix, Style::default().fg(theme.gray)));
                left_spans.push(Span::styled(display, Style::default().fg(theme.command)));
            } else {
                // Normal tool: "Run " (muted) + command (syntax-highlighted).
                // For qualified MCP tool names the activity title is the
                // raw `server__action` string from ACP; prettify it to
                // `(Server) Action` so the spinner doesn't show the ugly
                // delimiter form. Non-MCP titles (bash commands etc.) are
                // returned untouched by `mcp_pretty_name_if_qualified`.
                let prefix = "Run ";
                let pretty = mcp_pretty_name_if_qualified(title.as_str());
                let detail = pretty.as_str();
                let prefix_width = prefix.width();
                let max_cmd = available_for_label.saturating_sub(prefix_width).max(5);
                let first_line = detail.lines().next().unwrap_or(detail);
                let display = truncate_str(first_line, max_cmd);
                left_spans.push(Span::styled(prefix, Style::default().fg(theme.gray)));
                left_spans.extend(crate::views::tasks_pane::highlight_bash_command(&display));
            }
        }
    } else {
        // Sendable wait holding queued messages: the persistent inline hint
        // saying why the queue is paused and how to send anyway. On the status
        // row (not an ephemeral tip) so it stays visible for the whole wait,
        // and dropped before the label truncates on a narrow terminal.
        // "Enter to send now" is advertised only when Enter would actually
        // send the top row (bash / client-expanded local rows refuse with a
        // toast — see `AgentView::held_queue_top_sendable`).
        let suffix = if held_queue > 0 && is_sendable_wait(activity) {
            if held_queue_top_sendable {
                format!(" · {held_queue} queued, Enter to send now")
            } else {
                format!(" · {held_queue} queued")
            }
        } else {
            String::new()
        };
        if !suffix.is_empty() && label.width() + suffix.width() <= available_for_label {
            left_spans.push(Span::styled(label.clone(), activity_style));
            queued_hint = Some(Span::styled(suffix, Style::default().fg(theme.gray)));
        } else {
            let display = truncate_str(&label, available_for_label);
            left_spans.push(Span::styled(display, activity_style));
        }
    }

    // Phase timer (gray, never truncates)
    if !phase_timer_str.is_empty() {
        left_spans.push(Span::styled(phase_timer_str, timer_style));
    }

    // After the phase timer, so the elapsed time reads as the wait's, not the hint's.
    if let Some(hint) = queued_hint {
        left_spans.push(hint);
    }

    // Render left side
    let left_line = Line::from(left_spans);
    buf.set_line(area.x, area.y, &left_line, area.width);

    // ── Render right side: turn_timer + bg + cancel ──
    let right_start_x = area.x + area.width.saturating_sub(right_width as u16);

    // Helper: build a fully-specified right-side style (fg + bg + clear mods).
    let right_style = |fg| {
        Style::default()
            .fg(fg)
            .bg(timer_bg)
            .remove_modifier(Modifier::all())
    };

    // Turn timer (gray)
    let mut x = right_start_x;
    if !turn_timer_str.is_empty() {
        let span = Span::styled(turn_timer_str.clone(), timer_style);
        buf.set_span(x, area.y, &span, turn_timer_width as u16);
        x += turn_timer_width as u16;
    }

    // Bg button — accent_running on hover
    let bg_button_rect = if show_bg && !bg_str.is_empty() {
        let bg_x = x;
        let bg_style = if bg_hovered {
            right_style(theme.accent_running)
        } else {
            right_style(theme.gray)
        };
        let span = Span::styled(bg_str, bg_style);
        buf.set_span(x, area.y, &span, bg_width as u16);
        x += bg_width as u16;
        Some(Rect::new(bg_x, area.y, bg_str.width() as u16, 1))
    } else {
        None
    };

    // Cancel button — accent_error (red) on hover, gray at rest
    let cancel_button_rect = if show_cancel && !cancel_str.is_empty() {
        let cancel_x = x;
        let cancel_style = if cancel_hovered {
            right_style(theme.accent_error)
        } else {
            right_style(theme.gray)
        };
        let span = Span::styled(cancel_str, cancel_style);
        buf.set_span(x, area.y, &span, cancel_width as u16);
        Some(Rect::new(cancel_x, area.y, cancel_width as u16, 1))
    } else {
        None
    };

    TurnStatusOutput {
        cancel_button: cancel_button_rect,
        bg_button: bg_button_rect,
        watching_cue: None,
    }
}

/// Compute activity style, label, and whether it's a tool.
fn compute_activity(
    theme: &Theme,
    state: &AgentState,
    activity: &Option<TurnActivity>,
    is_bash_turn: bool,
    goal_verifying: bool,
) -> (Style, String, bool) {
    match (state, activity) {
        (AgentState::TurnCancelling | AgentState::CommandCancelling { .. }, _) => (
            Style::default().fg(theme.accent_error),
            "Cancelling…".to_string(),
            false,
        ),
        // Goal-mode completion verification runs in-turn after the model
        // stops streaming. The harness drives the skeptic panel (the model
        // itself is idle), but the turn's last streaming activity can still
        // read as `Responding`/`Thinking`; label the whole window
        // "Verifying…" so the multi-minute panel isn't mislabelled as the
        // model responding (or a hung "Waiting…").
        (AgentState::TurnRunning, _) if goal_verifying => (
            Style::default().fg(theme.text_secondary),
            "Verifying…".to_string(),
            false,
        ),
        (AgentState::TurnRunning, Some(TurnActivity::Thinking)) => (
            Style::default().fg(theme.text_secondary),
            "Thinking…".to_string(),
            false,
        ),
        (AgentState::TurnRunning, Some(TurnActivity::Responding)) => (
            Style::default().fg(theme.text_secondary),
            "Responding…".to_string(),
            false,
        ),
        (AgentState::TurnRunning, Some(TurnActivity::ToolRunning { title, description })) => {
            // "Ask" tools (AskUserQuestion) use gray spinner like Thinking —
            // green feels out of place when the user is answering questions.
            // Human descriptions (e.g. bash `description`) also use muted
            // secondary — they read as a wait subject (`Wait 5s…`), not a
            // green `Run <command>` invocation.
            let is_ask = title.starts_with("Ask: ") || title.starts_with("Ask ");
            let has_desc = description
                .as_deref()
                .map(str::trim)
                .is_some_and(|s| !s.is_empty());
            let style = if is_ask || has_desc {
                Style::default().fg(theme.text_secondary)
            } else {
                Style::default().fg(theme.accent_success)
            };
            (style, String::new(), true)
        }
        (AgentState::TurnRunning, Some(TurnActivity::AutoCompacting)) => (
            Style::default().fg(theme.text_secondary),
            "Compacting…".to_string(),
            false,
        ),
        (AgentState::TurnRunning, Some(TurnActivity::Retrying { attempt, .. })) => (
            Style::default().fg(theme.warning),
            format!("Retrying (attempt {attempt})…"),
            false,
        ),
        (AgentState::TurnRunning, Some(TurnActivity::WritingToolCall(writing))) => (
            Style::default().fg(theme.text_secondary),
            writing.label(),
            false,
        ),
        (AgentState::TurnRunning, Some(TurnActivity::Waiting(reason))) => (
            // Explicit wait reason (model / subagent / task output / tasks /
            // sleep): name what the agent is blocked on instead of a generic
            // "Waiting…". See `WaitingReason` and `AgentView::resolve_turn_activity`.
            Style::default().fg(theme.text_secondary),
            reason.label(),
            false,
        ),
        (AgentState::TurnRunning, None) if is_bash_turn => (
            // Bash turn: not inference, show generic "Running…".
            Style::default().fg(theme.text_secondary),
            "Running…".to_string(),
            false,
        ),
        (AgentState::TurnRunning, None) => (
            // Fallback: a running inference turn with no resolved activity. The
            // view resolves this gap into Waiting(Model/Subagent) before render,
            // so this is now a rarely-hit safety net.
            Style::default().fg(theme.text_secondary),
            "Waiting…".to_string(),
            false,
        ),
        (
            AgentState::CommandRunning {
                command:
                    command @ (AgentCommand::CreateWorktree
                    | AgentCommand::RestoreWorktree
                    | AgentCommand::RestoreCode
                    | AgentCommand::ForkSession),
                ..
            },
            _,
        ) => (
            Style::default().fg(theme.gray),
            format!("{}…", command.display_name()),
            false,
        ),
        (AgentState::CommandRunning { command, .. }, _) => (
            Style::default().fg(theme.text_secondary),
            format!("{}…", command.display_name()),
            false,
        ),
        (AgentState::Idle, _) => (Style::default(), String::new(), false),
    }
}

/// Whether the idle "Starting session…" indicator wants the turn-status row.
///
/// True only for a fresh `total == 0` startup seed (gated by
/// [`McpInitProgress::is_visible`] so an orphaned seed expires). Real MCP
/// progress (`total > 0`) renders as the top-bar chip instead, so it does not
/// drive this row.
fn starting_session_visible(progress: Option<&McpInitProgress>) -> bool {
    progress.is_some_and(|p| p.total == 0 && p.is_visible())
}

/// Render the idle "Starting session…" indicator above the prompt.
///
/// Format: `⠋ Starting session… 0:01` — braille spinner + label + elapsed
/// timer. Rendered in `theme.gray_dim` (the dimmest gray) so it reads as
/// quiet/ambient, matching the top-bar MCP chip and the directory path — this
/// is non-blocking startup, not foreground activity. Shown only while the MCP
/// init progress is a startup seed (`total == 0`), before the shell reports
/// real server counts; real progress (`total > 0`) renders as the top-bar chip.
fn render_starting_session(
    buf: &mut Buffer,
    area: Rect,
    progress: &McpInitProgress,
    tick: u64,
    theme: &Theme,
) {
    let frames = crate::glyphs::braille_spinner_frames();
    let frame_idx = (tick / SPINNER_DIVISOR) as usize % frames.len();
    let timer_str = format!(" {}", format_turn_timer(progress.started_at.elapsed()));
    let style = Style::default().fg(theme.gray_dim);
    let spans = vec![
        Span::styled(format!("{} ", frames[frame_idx]), style),
        Span::styled("Starting session…", style),
        Span::styled(timer_str, style),
    ];
    buf.set_line(area.x, area.y, &Line::from(spans), area.width);
}

/// Whether the turn status line should be visible.
///
/// Returns true when a turn is active (Running or Cancelling), when the drain
/// is blocked (agent idle, waiting on user edit), while the MCP startup seed
/// is showing "Starting session…" (a fresh `total == 0` seed), or when the
/// agent is idle but background watchers are still running
/// (`watchers.total() > 0`) — running commands and monitors wake the agent on
/// completion/events, scheduled `/loop` tasks fire prompts, and background
/// subagents inject a completion turn, any of which can start a new turn.
///
/// A parked turn always shows the row, watchers or not.
///
/// Real MCP progress (`total > 0`) renders as a compact chip in the top status
/// bar instead, so it does not affect this row.
pub fn should_show(
    state: &AgentState,
    drain_blocked: bool,
    mcp_init_progress: Option<&McpInitProgress>,
    watchers: Watchers,
    parked: bool,
) -> bool {
    if parked {
        return true;
    }
    !state.is_idle()
        || drain_blocked
        || starting_session_visible(mcp_init_progress)
        || watchers.total() > 0
}

/// Format a duration for the turn/phase timer.
///
/// Re-exports [`crate::util::format_duration`] under the old name for
/// backwards compatibility within this module.
pub use crate::util::format_duration as format_turn_timer;

/// Format a token count for compact display.
///
/// - Under 1000: `1`, `10`, `100` (raw number)
/// - 1k-100k: `1.23k`, `10.1k` (with decimal)
/// - 100k-1m: `100k`, `500k` (whole thousands)
/// - 1m+: `1.23m`, `10.1m` (with decimal)
fn format_tokens_short(tokens: u64) -> String {
    if tokens < 1000 {
        format!("{tokens}")
    } else if tokens < 100_000 {
        // 1k-99.9k: show one or two decimals for precision
        let k = tokens as f64 / 1000.0;
        if tokens < 10_000 {
            format!("{k:.2}k") // 1.23k
        } else {
            format!("{k:.1}k") // 10.1k
        }
    } else if tokens < 1_000_000 {
        // 100k-999k: whole thousands
        let k = tokens / 1000;
        format!("{k}k")
    } else {
        // 1m+: show with decimal
        let m = tokens as f64 / 1_000_000.0;
        if tokens < 10_000_000 {
            format!("{m:.2}m") // 1.23m
        } else {
            format!("{m:.1}m") // 10.1m
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// Sendable waits = exactly the wait reasons the shell aborts on a queued
    /// user prompt (blocking task-output / wait_tasks / Await, and a blocked
    /// foreground subagent await — all take the send-now path). Model waits —
    /// where typing only queues behind the actively-streaming turn — and
    /// non-wait activities keep the busy spinner.
    #[test]
    fn sendable_wait_matches_shell_interruptible_waits() {
        let task_wait = |waits| {
            Some(TurnActivity::Waiting(WaitingReason::TaskOutput {
                task_ids: vec!["t-1".into()],
                subject: Some("sleep 300".into()),
                waits,
            }))
        };
        assert!(is_sendable_wait(&task_wait(true)));
        assert!(
            !is_sendable_wait(&task_wait(false)),
            "instant polls are not blocking waits"
        );
        assert!(is_sendable_wait(&Some(TurnActivity::Waiting(
            WaitingReason::TasksComplete
        ))));
        assert!(is_sendable_wait(&Some(TurnActivity::Waiting(
            WaitingReason::Sleep
        ))));
        assert!(!is_sendable_wait(&Some(TurnActivity::Waiting(
            WaitingReason::Model
        ))));
        assert!(
            is_sendable_wait(&Some(TurnActivity::Waiting(WaitingReason::subagent()))),
            "the shell aborts a blocked foreground subagent await on send-now, \
             so Enter during it must read as sendable"
        );
        assert!(!is_sendable_wait(&Some(TurnActivity::Thinking)));
        assert!(!is_sendable_wait(&None));
    }

    #[test]
    fn format_subsecond() {
        assert_eq!(format_turn_timer(Duration::from_millis(500)), "0.5s");
        assert_eq!(format_turn_timer(Duration::from_millis(120)), "0.1s");
    }

    #[test]
    fn format_under_10s_has_decimal() {
        assert_eq!(format_turn_timer(Duration::from_secs_f64(5.2)), "5.2s");
        assert_eq!(format_turn_timer(Duration::from_secs_f64(9.9)), "9.9s");
    }

    #[test]
    fn format_10s_plus_no_decimal() {
        assert_eq!(format_turn_timer(Duration::from_secs(10)), "10s");
        assert_eq!(format_turn_timer(Duration::from_secs(32)), "32s");
        assert_eq!(format_turn_timer(Duration::from_secs(59)), "59s");
    }

    #[test]
    fn format_minutes() {
        assert_eq!(format_turn_timer(Duration::from_secs(60)), "1m0s");
        assert_eq!(format_turn_timer(Duration::from_secs(80)), "1m20s");
        assert_eq!(format_turn_timer(Duration::from_secs(600)), "10m0s");
    }

    #[test]
    fn activity_label_reads_verifying_while_goal_verifying_overriding_stale_activity() {
        let theme = Theme::current();
        // Running turn, no streaming activity, goal verifying → "Verifying…".
        let (_, label, _) = compute_activity(&theme, &AgentState::TurnRunning, &None, false, true);
        assert_eq!(label, "Verifying…");
        // Same state without the verifying flag → generic "Waiting…".
        let (_, label, _) = compute_activity(&theme, &AgentState::TurnRunning, &None, false, false);
        assert_eq!(label, "Waiting…");
        // During verification the model is idle but its last streaming
        // activity (Responding/Thinking) can linger — the flag overrides it
        // so the panel reads "Verifying…", not "Responding…" (the bug).
        for activity in [TurnActivity::Responding, TurnActivity::Thinking] {
            let (_, label, _) = compute_activity(
                &theme,
                &AgentState::TurnRunning,
                &Some(activity),
                false,
                true,
            );
            assert_eq!(label, "Verifying…");
        }
        // Without the flag the streaming label stands.
        let (_, label, _) = compute_activity(
            &theme,
            &AgentState::TurnRunning,
            &Some(TurnActivity::Responding),
            false,
            false,
        );
        assert_eq!(label, "Responding…");
    }

    #[test]
    fn waiting_reason_renders_specific_label() {
        use crate::acp::tracker::WaitingReason;
        let theme = Theme::current();
        let cases = [
            (WaitingReason::Model, "Waiting for response…"),
            (WaitingReason::subagent(), "Waiting on subagent…"),
            (
                WaitingReason::Subagent {
                    display: Some("fix flaky test: Running: cargo test".into()),
                },
                "fix flaky test: Running: cargo test…",
            ),
            (WaitingReason::task_output(), "Waiting on task output…"),
            (
                WaitingReason::TaskOutput {
                    task_ids: vec!["t1".into()],
                    subject: Some("compile release".into()),
                    waits: false,
                },
                "compile release…",
            ),
            (WaitingReason::TasksComplete, "Waiting on tasks…"),
            (WaitingReason::Sleep, "Sleeping…"),
        ];
        for (reason, expected) in cases {
            let (_, label, is_tool) = compute_activity(
                &theme,
                &AgentState::TurnRunning,
                &Some(TurnActivity::Waiting(reason.clone())),
                false,
                false,
            );
            assert_eq!(label, expected, "reason {reason:?}");
            assert!(!is_tool, "waiting is not a tool activity");
        }
    }

    #[test]
    fn bash_turn_still_renders_running_not_waiting() {
        let theme = Theme::current();
        // A bash (non-inference) turn with no activity keeps its own "Running…"
        // label — the view leaves it as `None` rather than Waiting(Model).
        let (_, label, _) = compute_activity(&theme, &AgentState::TurnRunning, &None, true, false);
        assert_eq!(label, "Running…");
    }

    #[test]
    fn format_hours() {
        assert_eq!(format_turn_timer(Duration::from_secs(3600)), "1h0m");
        assert_eq!(format_turn_timer(Duration::from_secs(3725)), "1h2m");
    }

    #[test]
    fn should_show_when_running() {
        assert!(should_show(
            &AgentState::TurnRunning,
            false,
            None,
            Watchers::default(),
            false
        ));
        assert!(should_show(
            &AgentState::TurnCancelling,
            false,
            None,
            Watchers::default(),
            false
        ));
        assert!(!should_show(
            &AgentState::Idle,
            false,
            None,
            Watchers::default(),
            false
        ));
    }

    /// Cancelling keeps `[stop]` clickable (the retry affordance for a lost
    /// cancel); a revert to the running-only gate strands mouse users.
    #[test]
    fn cancelling_keeps_stop_button_clickable() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 1));
        let output = render_turn_status(
            &mut buf,
            Rect::new(0, 0, 80, 1),
            TurnStatusArgs {
                state: &AgentState::TurnCancelling,
                activity: &None,
                turn_elapsed: Some(Duration::from_secs(3)),
                activity_started_at: None,
                tick: 0,
                drain_blocked: false,
                buttons: Some(MouseButtons::default()),
                has_running_execute: false,
                total_tokens: None,
                mcp_init_progress: None,
                is_bash_turn: false,
                is_pending_user_input: false,
                goal_verifying: false,
                watchers: Watchers::default(),
                parked: false,
                flat_background: false,
                held_queue: 0,
                held_queue_top_sendable: false,
            },
        );
        assert!(
            output.cancel_button.is_some(),
            "cancelling must keep a clickable [stop] (cancel-retry affordance)"
        );
        let text = buffer_text(&buf, Rect::new(0, 0, 80, 1));
        assert!(
            text.contains("Cancelling") && text.contains("[stop]"),
            "got: {text:?}"
        );
    }

    #[test]
    fn should_show_when_drain_blocked() {
        assert!(should_show(
            &AgentState::Idle,
            true,
            None,
            Watchers::default(),
            false
        ));
    }

    #[test]
    fn should_show_when_watchers_running() {
        // Idle but a watcher (command, monitor, loop, or subagent) is still
        // running → row stays visible so the persistent "… still running" cue
        // can show.
        for watchers in [
            Watchers {
                commands: 1,
                ..Watchers::default()
            },
            Watchers {
                monitors: 1,
                ..Watchers::default()
            },
            Watchers {
                loops: 1,
                ..Watchers::default()
            },
            Watchers {
                subagents: 1,
                ..Watchers::default()
            },
        ] {
            assert!(should_show(&AgentState::Idle, false, None, watchers, false));
        }
        // Idle with no watchers and nothing else pending → hidden.
        assert!(!should_show(
            &AgentState::Idle,
            false,
            None,
            Watchers::default(),
            false
        ));
    }

    #[test]
    fn should_show_parked_always() {
        assert!(should_show(
            &AgentState::TurnRunning,
            false,
            None,
            Watchers {
                commands: 1,
                ..Watchers::default()
            },
            true
        ));
        assert!(should_show(
            &AgentState::TurnRunning,
            false,
            None,
            Watchers::default(),
            true
        ));
    }

    #[test]
    fn should_show_when_starting_session() {
        // A fresh total == 0 seed shows "Starting session…" above the prompt.
        let seed = McpInitProgress {
            total: 0,
            connected: 0,
            started_at: Instant::now(),
        };
        assert!(should_show(
            &AgentState::Idle,
            false,
            Some(&seed),
            Watchers::default(),
            false
        ));

        // Real progress (total > 0) is the top-bar chip — it must NOT drive
        // this row.
        let connecting = McpInitProgress {
            total: 3,
            connected: 1,
            started_at: Instant::now(),
        };
        assert!(!should_show(
            &AgentState::Idle,
            false,
            Some(&connecting),
            Watchers::default(),
            false
        ));

        // An expired seed must not drive the row either.
        let expired = McpInitProgress {
            total: 0,
            connected: 0,
            started_at: Instant::now() - McpInitProgress::SEED_EXPIRE - Duration::from_secs(1),
        };
        assert!(!should_show(
            &AgentState::Idle,
            false,
            Some(&expired),
            Watchers::default(),
            false
        ));
    }

    /// Collect every rendered glyph in `area` into a single string.
    fn buffer_text(buf: &Buffer, area: Rect) -> String {
        (area.y..area.y + area.height)
            .map(|y| {
                (area.x..area.x + area.width)
                    .filter_map(|x| buf.cell((x, y)).map(|c| c.symbol().to_string()))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Baseline render args: idle agent on a mouse host with the given watchers.
    fn idle_args<'a>(watchers: Watchers) -> TurnStatusArgs<'a> {
        TurnStatusArgs {
            state: &AgentState::Idle,
            activity: &None,
            turn_elapsed: None,
            activity_started_at: None,
            tick: 0,
            drain_blocked: false,
            buttons: Some(MouseButtons::default()),
            has_running_execute: false,
            total_tokens: None,
            mcp_init_progress: None,
            is_bash_turn: false,
            is_pending_user_input: false,
            goal_verifying: false,
            watchers,
            parked: false,
            flat_background: false,
            held_queue: 0,
            held_queue_top_sendable: false,
        }
    }

    /// Render `args` into a `width`×1 row.
    fn render_row(args: TurnStatusArgs<'_>, width: u16) -> (TurnStatusOutput, Buffer) {
        let area = Rect::new(0, 0, width, 1);
        let mut buf = Buffer::empty(area);
        let output = render_turn_status(&mut buf, area, args);
        (output, buf)
    }

    /// Render `args` into a `width`×1 row, returning the visible text.
    fn render_row_text(args: TurnStatusArgs<'_>, width: u16) -> String {
        let (_, buf) = render_row(args, width);
        buffer_text(&buf, buf.area)
    }

    /// Invoke `render_turn_status` for an idle agent with the given MCP seed.
    fn render_idle_with_mcp(progress: &McpInitProgress) -> String {
        let mut args = idle_args(Watchers::default());
        args.mcp_init_progress = Some(progress);
        render_row_text(args, 60)
    }

    /// Invoke `render_turn_status` for an idle agent with the given watcher
    /// counts at animation tick `tick`.
    fn render_idle_with_watchers_at_tick(watchers: Watchers, tick: u64) -> String {
        render_idle_with_watchers_in_width(watchers, tick, 72)
    }

    /// [`render_idle_with_watchers_at_tick`] with an explicit row width.
    fn render_idle_with_watchers_in_width(watchers: Watchers, tick: u64, width: u16) -> String {
        let mut args = idle_args(watchers);
        args.tick = tick;
        render_row_text(args, width)
    }

    /// Invoke `render_turn_status` for a PARKED running turn (the stopped
    /// look) with the given watcher counts.
    fn render_parked_with_watchers(watchers: Watchers) -> String {
        let activity = Some(TurnActivity::Waiting(WaitingReason::TasksComplete));
        let mut args = idle_args(watchers);
        args.state = &AgentState::TurnRunning;
        args.activity = &activity;
        args.turn_elapsed = Some(Duration::from_secs(5));
        args.parked = true;
        render_row_text(args, 72)
    }

    /// Invoke `render_turn_status` for an idle agent with the given watcher
    /// counts at the first animation tick.
    fn render_idle_with_watchers(watchers: Watchers) -> String {
        render_idle_with_watchers_at_tick(watchers, 0)
    }

    /// Invoke `render_turn_status` for an idle agent with `n` running
    /// monitors at animation tick `tick`.
    fn render_idle_with_monitors_at_tick(n: usize, tick: u64) -> String {
        render_idle_with_watchers_at_tick(
            Watchers {
                monitors: n,
                ..Watchers::default()
            },
            tick,
        )
    }

    /// Invoke `render_turn_status` for an idle agent with `n` running
    /// monitors at the first animation tick.
    fn render_idle_with_monitors(n: usize) -> String {
        render_idle_with_monitors_at_tick(n, 0)
    }

    #[test]
    fn idle_with_monitors_renders_still_running_cue() {
        let text = render_idle_with_monitors(2);
        assert!(
            text.contains("2 monitors still running"),
            "idle with monitors must render the still-running cue, got: {text:?}"
        );
    }

    #[test]
    fn idle_with_one_monitor_uses_singular() {
        let text = render_idle_with_monitors(1);
        assert!(
            text.contains("1 monitor still running") && !text.contains("monitors"),
            "single monitor must use the singular noun, got: {text:?}"
        );
    }

    #[test]
    fn idle_with_no_monitors_renders_nothing() {
        let text = render_idle_with_monitors(0);
        assert!(
            text.trim().is_empty(),
            "idle with no monitors must render nothing, got: {text:?}"
        );
    }

    /// Mouse hosts get a hit rect hugging exactly the rendered cue text, and
    /// hover brightens the label; keyboard-only hosts get neither.
    #[test]
    fn watching_cue_is_clickable_on_mouse_hosts_only() {
        let theme = Theme::current();
        let watchers = Watchers {
            monitors: 1,
            ..Watchers::default()
        };
        // First label cell (after the 2-col icon).
        let label_fg = |buf: &Buffer| buf.cell((2, 0)).map(|c| c.fg);

        let (output, buf) = render_row(idle_args(watchers), 60);
        let rect = output.watching_cue.expect("mouse host must get a hit rect");
        let rendered_width = buffer_text(&buf, buf.area).trim_end().width() as u16;
        assert_eq!(rect, Rect::new(0, 0, rendered_width, 1));
        assert_eq!(label_fg(&buf), Some(theme.gray));

        let mut args = idle_args(watchers);
        args.buttons = Some(MouseButtons {
            watching_hovered: true,
            ..MouseButtons::default()
        });
        let (_, buf) = render_row(args, 60);
        assert_eq!(label_fg(&buf), Some(theme.text_primary));

        let mut args = idle_args(watchers);
        args.buttons = None;
        let (output, _) = render_row(args, 60);
        assert!(output.watching_cue.is_none());
    }

    #[test]
    fn idle_with_loops_renders_still_running_cue() {
        let text = render_idle_with_watchers(Watchers {
            loops: 2,
            ..Watchers::default()
        });
        assert!(
            text.contains("2 loops still running"),
            "idle with loops must render the still-running cue, got: {text:?}"
        );
    }

    #[test]
    fn idle_with_one_loop_uses_singular() {
        let text = render_idle_with_watchers(Watchers {
            loops: 1,
            ..Watchers::default()
        });
        assert!(
            text.contains("1 loop still running") && !text.contains("loops"),
            "single loop must use the singular noun, got: {text:?}"
        );
    }

    #[test]
    fn idle_with_subagents_renders_still_running_cue() {
        let text = render_idle_with_watchers(Watchers {
            subagents: 2,
            ..Watchers::default()
        });
        assert!(
            text.contains("2 subagents still running"),
            "idle with subagents must render the still-running cue, got: {text:?}"
        );
    }

    #[test]
    fn idle_with_one_subagent_uses_singular() {
        let text = render_idle_with_watchers(Watchers {
            subagents: 1,
            ..Watchers::default()
        });
        assert!(
            text.contains("1 subagent still running") && !text.contains("subagents"),
            "single subagent must use the singular noun, got: {text:?}"
        );
    }

    #[test]
    fn idle_with_one_workflow_counts_run_once() {
        let text = render_idle_with_watchers(Watchers {
            workflows: 1,
            ..Watchers::default()
        });
        assert!(text.contains("1 workflow still running"), "got: {text:?}");
    }

    #[test]
    fn idle_with_monitors_and_loops_lists_both() {
        // Both watcher kinds present → one cue lists monitors then loops,
        // each with its own count, joined by the middle-dot separator.
        let text = render_idle_with_watchers(Watchers {
            monitors: 1,
            loops: 2,
            ..Watchers::default()
        });
        assert!(
            text.contains("1 monitor \u{00b7} 2 loops still running"),
            "both kinds must be listed in one cue, got: {text:?}"
        );
    }

    #[test]
    fn idle_with_all_watcher_kinds_lists_all() {
        // Commands, monitors, loops, and subagents present → one cue lists
        // all four in order, middle-dot separated.
        let text = render_idle_with_watchers(Watchers {
            commands: 1,
            monitors: 2,
            loops: 1,
            subagents: 3,
            workflows: 0,
        });
        assert!(
            text.contains(
                "1 command \u{00b7} 2 monitors \u{00b7} 1 loop \u{00b7} 3 subagents still running"
            ),
            "all kinds must be listed in one cue, got: {text:?}"
        );
    }

    #[test]
    fn narrow_area_clips_cue_tail_keeping_counts() {
        // 40 cols with three kinds: the row tail-clips with no ellipsis, so
        // the leading counts survive and the trailing suffix is what gets
        // cut. Pins the narrow-pane tradeoff of leading with the counts; a
        // smarter compact fallback would be a behavior change.
        let watchers = Watchers {
            commands: 1,
            monitors: 2,
            loops: 1,
            ..Watchers::default()
        };
        let text = render_idle_with_watchers_in_width(watchers, 0, 40);
        assert!(
            text.contains("1 command \u{00b7} 2 monitors \u{00b7} 1 loop"),
            "the counts must survive the clip, got: {text:?}"
        );
    }

    #[test]
    fn idle_with_commands_renders_still_running_cue() {
        // Plain background commands (non-monitor bg tasks) count as watchers:
        // they wake the agent with a task-completed turn, so the cue must show.
        let text = render_idle_with_watchers(Watchers {
            commands: 2,
            ..Watchers::default()
        });
        assert!(
            text.contains("2 commands still running"),
            "idle with bg commands must render the still-running cue, got: {text:?}"
        );
        let text = render_idle_with_watchers(Watchers {
            commands: 1,
            ..Watchers::default()
        });
        assert!(
            text.contains("1 command still running") && !text.contains("commands"),
            "single command must use the singular noun, got: {text:?}"
        );
    }

    #[test]
    fn parked_with_watchers_renders_cue_not_running_chrome() {
        // The wait aborts as soon as the user types, so busy chrome would lie.
        let text = render_parked_with_watchers(Watchers {
            commands: 2,
            ..Watchers::default()
        });
        assert!(
            text.contains("2 commands still running \u{00b7} send a message to interrupt"),
            "parked with bg work must render the interruptible still-running cue, got: {text:?}"
        );
        assert!(
            !text.contains("Waiting") && !text.contains("[stop]"),
            "parked must not render the running-turn chrome, got: {text:?}"
        );
    }

    #[test]
    fn parked_without_watchers_renders_waiting_cue() {
        let text = render_parked_with_watchers(Watchers::default());
        assert!(
            text.contains("waiting \u{00b7} send a message to interrupt"),
            "watcherless parked must render the waiting interrupt cue, got: {text:?}"
        );
        assert!(
            !text.contains("[stop]"),
            "watcherless parked must not render the running-turn chrome, got: {text:?}"
        );
    }

    #[test]
    fn parked_with_held_queue_renders_queued_hint() {
        // The queued hint replaces the interrupt copy (Enter = send-now).
        let activity = Some(TurnActivity::Waiting(WaitingReason::TasksComplete));
        let mut args = idle_args(Watchers {
            commands: 1,
            ..Watchers::default()
        });
        args.state = &AgentState::TurnRunning;
        args.activity = &activity;
        args.parked = true;
        args.held_queue = 1;
        args.held_queue_top_sendable = true;
        let text = render_row_text(args, 80);
        assert!(
            text.contains("1 queued, Enter to send now"),
            "parked with a held row must advertise the queued hint, got: {text:?}"
        );
        assert!(
            !text.contains("send a message to interrupt"),
            "queued hint replaces the interrupt copy, got: {text:?}"
        );
    }

    #[test]
    fn idle_with_no_watchers_renders_nothing() {
        let text = render_idle_with_watchers(Watchers::default());
        assert!(
            text.trim().is_empty(),
            "idle with no watchers must render nothing, got: {text:?}"
        );
    }

    #[test]
    fn queued_hint_renders_after_phase_timer() {
        let activity = Some(TurnActivity::Waiting(WaitingReason::subagent()));
        let mut args = idle_args(Watchers::default());
        args.state = &AgentState::TurnRunning;
        args.activity = &activity;
        args.activity_started_at = Some(Instant::now() - Duration::from_secs(359));
        args.held_queue = 1;
        args.held_queue_top_sendable = true;
        let text = render_row_text(args, 80);
        assert!(
            text.contains("Waiting on subagent… 5m59s · 1 queued, Enter to send now"),
            "phase timer must sit between the wait label and the queued hint, got: {text:?}"
        );
    }

    #[test]
    fn still_running_label_lists_only_nonzero_kinds() {
        assert_eq!(
            still_running_label(Watchers {
                commands: 2,
                ..Watchers::default()
            }),
            Some("2 commands still running".into())
        );
        assert_eq!(
            still_running_label(Watchers {
                monitors: 2,
                ..Watchers::default()
            }),
            Some("2 monitors still running".into())
        );
        assert_eq!(
            still_running_label(Watchers {
                loops: 1,
                ..Watchers::default()
            }),
            Some("1 loop still running".into())
        );
        assert_eq!(
            still_running_label(Watchers {
                subagents: 1,
                ..Watchers::default()
            }),
            Some("1 subagent still running".into())
        );
        assert_eq!(
            still_running_label(Watchers {
                monitors: 1,
                loops: 2,
                ..Watchers::default()
            }),
            Some("1 monitor \u{00b7} 2 loops still running".into())
        );
        assert_eq!(
            still_running_label(Watchers {
                commands: 1,
                monitors: 1,
                loops: 1,
                subagents: 2,
                workflows: 0,
            }),
            Some(
                "1 command \u{00b7} 1 monitor \u{00b7} 1 loop \u{00b7} 2 subagents still running"
                    .into()
            )
        );
        assert_eq!(still_running_label(Watchers::default()), None);
    }

    #[test]
    fn idle_monitor_icon_animates_across_ticks() {
        // The leading glyph cycles through monitor_icon_frames() as `tick`
        // advances, so two ticks a full frame apart (0 vs MONITOR_PULSE_DIVISOR)
        // must render different icons — proving the cue is animated, not static.
        let frame0 = render_idle_with_monitors_at_tick(1, 0);
        let frame1 = render_idle_with_monitors_at_tick(1, MONITOR_PULSE_DIVISOR);
        let icon0 = frame0.chars().next();
        let icon1 = frame1.chars().next();
        assert_ne!(
            icon0, icon1,
            "monitor icon must animate between frames, got {frame0:?} vs {frame1:?}"
        );
    }

    #[test]
    fn idle_zero_server_seed_renders_starting_session() {
        // total == 0 seed → "Starting session…" above the prompt.
        let text = render_idle_with_mcp(&McpInitProgress {
            total: 0,
            connected: 0,
            started_at: Instant::now(),
        });
        assert!(
            text.contains("Starting session"),
            "idle 0-server seed must render 'Starting session…', got: {text:?}"
        );
    }

    #[test]
    fn idle_active_mcp_progress_renders_nothing_in_turn_status() {
        // total > 0 is the top-bar chip — the turn-status row stays empty.
        let text = render_idle_with_mcp(&McpInitProgress {
            total: 3,
            connected: 1,
            started_at: Instant::now(),
        });
        assert!(
            text.trim().is_empty(),
            "active MCP progress must NOT render in the turn-status row, got: {text:?}"
        );
    }

    #[test]
    fn expired_seed_renders_nothing() {
        // An expired total == 0 seed renders nothing — defense-in-depth.
        let text = render_idle_with_mcp(&McpInitProgress {
            total: 0,
            connected: 0,
            started_at: Instant::now() - McpInitProgress::SEED_EXPIRE - Duration::from_secs(1),
        });
        assert!(
            text.trim().is_empty(),
            "expired seed must render nothing, got: {text:?}"
        );
    }

    #[test]
    fn format_tokens_under_1k() {
        assert_eq!(format_tokens_short(0), "0");
        assert_eq!(format_tokens_short(1), "1");
        assert_eq!(format_tokens_short(10), "10");
        assert_eq!(format_tokens_short(100), "100");
        assert_eq!(format_tokens_short(999), "999");
    }

    #[test]
    fn format_tokens_1k_to_10k() {
        assert_eq!(format_tokens_short(1000), "1.00k");
        assert_eq!(format_tokens_short(1230), "1.23k");
        assert_eq!(format_tokens_short(1500), "1.50k");
        assert_eq!(format_tokens_short(9990), "9.99k");
        assert_eq!(format_tokens_short(9999), "10.00k"); // rounds up
    }

    #[test]
    fn format_tokens_10k_to_100k() {
        assert_eq!(format_tokens_short(10000), "10.0k");
        assert_eq!(format_tokens_short(10100), "10.1k");
        assert_eq!(format_tokens_short(12345), "12.3k");
        assert_eq!(format_tokens_short(99999), "100.0k"); // rounds up
    }

    #[test]
    fn format_tokens_100k_to_1m() {
        assert_eq!(format_tokens_short(100000), "100k");
        assert_eq!(format_tokens_short(128000), "128k");
        assert_eq!(format_tokens_short(500000), "500k");
        assert_eq!(format_tokens_short(999000), "999k");
    }

    #[test]
    fn format_tokens_millions() {
        assert_eq!(format_tokens_short(1_000_000), "1.00m");
        assert_eq!(format_tokens_short(1_230_000), "1.23m");
        assert_eq!(format_tokens_short(9_999_000), "10.00m"); // rounds
        assert_eq!(format_tokens_short(10_000_000), "10.0m");
        assert_eq!(format_tokens_short(10_100_000), "10.1m");
    }

    #[test]
    fn user_waiting_pulse_speed_is_stable() {
        // The drain-blocked, pending-user-input, and plan-approval cues
        // all read from this single constant via `pending_diamond_color`,
        // so this assertion guards against an accidental tweak that
        // would silently change the cadence of every "your turn" cue.
        assert_eq!(USER_WAITING_PULSE_SPEED, 0.08);
    }
}
