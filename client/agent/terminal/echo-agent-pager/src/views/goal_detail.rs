//! Expanded goal detail overlay — full-screen popup showing goal progress
//! with token budget bar, todo list, and event history.
//!
//! Rendered as a centered overlay when `AgentView::show_goal_detail` is true
//! and `goal_state` is `Some`. Dismissed by `Esc` or `g`.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use echo_agent_runtime::tools::{TodoItem, TodoStatus};

use echo_agent_runtime::extensions::notification::GoalClassifierVerdict;

use crate::app::agent::{GoalDisplayState, GoalDisplayStatus};
use crate::render::SafeBuf;
use crate::theme::Theme;
use crate::views::agent_status::{
    active_phase_label, classifier_attempts_label, format_tokens_compact,
};
use crate::views::progress_bar::progress_bar_spans;

/// Maximum todo items displayed in the modal before truncation.
const MAX_TODO_DISPLAY: usize = 15;

/// Maximum per-model token rows displayed before a "+N more" summary row.
const MAX_MODEL_DISPLAY: usize = 6;

/// Rows the per-model breakdown contributes to the modal: the capped model
/// rows plus an optional "+N more" overflow row, or 0 when the breakdown is
/// suppressed (a single-model / all-inherit goal collapses to the single
/// tokens line). The cap is applied BEFORE the `u16` cast so the height sum
/// can never overflow, and this is the single source of truth shared by the
/// height calc and the render loop so they stay in lockstep.
fn per_model_row_count(models: &[(String, u64)]) -> u16 {
    if models.len() < 2 {
        return 0;
    }
    let shown = models.len().min(MAX_MODEL_DISPLAY);
    let overflow = usize::from(models.len() > MAX_MODEL_DISPLAY);
    (shown + overflow) as u16
}

// ---------------------------------------------------------------------------
// Token budget color
// ---------------------------------------------------------------------------

/// Choose the progress bar fill color based on usage percentage.
fn budget_color(pct: f32, theme: &Theme) -> Color {
    if pct > 0.80 {
        theme.accent_error
    } else if pct >= 0.50 {
        theme.warning
    } else {
        theme.accent_success
    }
}

/// Format elapsed milliseconds as a compact human-readable duration.
/// Same style as `goal_orchestrator::format_elapsed` — keep in sync.
pub(crate) fn format_elapsed(ms: u64) -> String {
    let total_secs = ms / 1000;
    let hours = total_secs / 3600;
    let mins = (total_secs % 3600) / 60;
    let secs = total_secs % 60;
    if hours > 0 {
        format!("{hours}h{mins:02}m")
    } else if mins > 0 {
        format!("{mins}m{secs:02}s")
    } else {
        format!("{secs}s")
    }
}

// ---------------------------------------------------------------------------
// Status label
// ---------------------------------------------------------------------------

fn status_label(goal: &GoalDisplayState) -> (&'static str, Color, String) {
    let theme = Theme::current();
    match goal.status {
        GoalDisplayStatus::Active => ("Active", theme.accent_success, active_phase_label(goal)),
        GoalDisplayStatus::UserPaused
        | GoalDisplayStatus::BackOffPaused
        | GoalDisplayStatus::NoProgressPaused
        | GoalDisplayStatus::InfraPaused
        | GoalDisplayStatus::Blocked => (goal.status.pause_label(), theme.warning, String::new()),
        GoalDisplayStatus::Failed => ("Failed", theme.accent_error, String::new()),
        GoalDisplayStatus::Interrupted => ("Interrupted", theme.accent_error, String::new()),
        GoalDisplayStatus::BudgetLimited => ("Budget Limited", theme.accent_error, String::new()),
        GoalDisplayStatus::Complete => ("Complete", theme.accent_success, String::new()),
    }
}

// ---------------------------------------------------------------------------
// Wrapping helpers — pause-message reason block
// ---------------------------------------------------------------------------

/// Wrap a string into rows of at most `width` terminal columns.
///
/// Splits on whitespace first, then hard-splits any token wider than
/// `width`. Preserves explicit `\n` line breaks so multi-line block
/// reasons (the concatenated `blocked_reason\nmessage` form emitted by
/// the shell) render with the same structure they had on the wire.
///
/// Width is measured in terminal columns via `UnicodeWidthStr` /
/// `UnicodeWidthChar`, not Unicode code points — CJK / East-Asian Wide
/// characters take 2 columns each, combining marks take 0, and emoji
/// can take 2. Using `chars().count()` here would let model-emitted
/// block reasons containing wide chars overflow the modal's inner
/// rectangle into the right border.
///
/// `width` of zero or one returns a single un-split row to avoid
/// divide-by-zero behaviour at degenerate modal widths (unreachable in
/// practice — the modal bails below width 20).
fn wrap_pause_message_lines(text: &str, width: u16) -> Vec<String> {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

    let w = width as usize;
    if w <= 1 {
        // Collapse the kept `\n` here too so "no control byte in any returned
        // line" holds even on this un-split degenerate path.
        return vec![text.replace('\n', " ")];
    }
    let mut out = Vec::new();
    for paragraph in text.split('\n') {
        if paragraph.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut current = String::new();
        for word in paragraph.split_whitespace() {
            if UnicodeWidthStr::width(word) > w {
                // Hard-split overlong tokens so the row-width invariant
                // holds even for paths/URLs without whitespace. Build
                // each chunk by accumulating chars until the next char
                // would push the chunk past `w` columns, honouring
                // zero-width marks (they don't consume capacity).
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
                let mut chunk = String::new();
                let mut chunk_w = 0usize;
                for ch in word.chars() {
                    let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
                    if cw > w {
                        // Pathological: a single char wider than the
                        // whole row. Emit it alone — anything else
                        // would silently drop the codepoint.
                        if !chunk.is_empty() {
                            out.push(std::mem::take(&mut chunk));
                            chunk_w = 0;
                        }
                        out.push(ch.to_string());
                        continue;
                    }
                    if chunk_w + cw > w {
                        out.push(std::mem::take(&mut chunk));
                        chunk_w = 0;
                    }
                    chunk.push(ch);
                    chunk_w += cw;
                }
                if !chunk.is_empty() {
                    out.push(chunk);
                }
                continue;
            }
            let need = if current.is_empty() {
                UnicodeWidthStr::width(word)
            } else {
                UnicodeWidthStr::width(current.as_str()) + 1 + UnicodeWidthStr::width(word)
            };
            if need > w {
                out.push(std::mem::take(&mut current));
                current.push_str(word);
            } else {
                if !current.is_empty() {
                    current.push(' ');
                }
                current.push_str(word);
            }
        }
        if !current.is_empty() {
            out.push(current);
        }
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// Truncate `text` to at most `budget` terminal columns, appending an
/// ellipsis if truncated. Uses display width (not char count) so CJK
/// and emoji characters measure correctly — matches the
/// `wrap_pause_message_lines` pattern.
pub(crate) fn truncate_to_width(text: &str, budget: usize) -> String {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

    if UnicodeWidthStr::width(text) <= budget {
        return text.to_owned();
    }
    let target = budget.saturating_sub(1); // room for ellipsis
    let mut out = String::new();
    let mut w = 0usize;
    for ch in text.chars() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw > target {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push('\u{2026}');
    out
}

/// Replace control characters (tab, ESC, BEL, …) with spaces so free-form
/// model/wire-derived text (objective title, humanized event detail/name/
/// timestamp, pause reason) can't break a rendered row even if ratatui's own
/// filter regresses. `keep_newlines` preserves `\n` for the pause-reason
/// wrapper (which splits on it before render); single-row sinks pass `false`.
pub(crate) fn strip_control_chars(s: &str, keep_newlines: bool) -> String {
    s.chars()
        .map(|c| {
            if c.is_control() && !(keep_newlines && c == '\n') {
                ' '
            } else {
                c
            }
        })
        .collect()
}

/// Strip control chars from the objective title, then trim (the title is
/// short and centered). A bare `\n` is zero-width to `truncate_to_width` and
/// would otherwise leak into the border row.
fn sanitize_title(s: &str) -> String {
    strip_control_chars(s, false).trim().to_owned()
}

/// Build the wrapped-reason source line for a paused goal's `pause_message`,
/// control-stripped (newlines kept — [`wrap_pause_message_lines`] splits on
/// them for multi-line block reasons and they never reach a rendered row).
/// Shared by the height calc and the render so they wrap identical text.
fn format_pause_reason(msg: &str) -> String {
    format!("Reason: {}", strip_control_chars(msg, true))
}

// ---------------------------------------------------------------------------
// Public render
// ---------------------------------------------------------------------------

/// True when the goal carries at least one signal from the
/// completion classifier — gates rendering of the modal's
/// "Completion review" section so a goal that has never been
/// classified shows nothing extra.
fn has_classifier_activity(goal: &GoalDisplayState) -> bool {
    goal.classifier_runs_attempted.is_some()
        || goal.classifier_max_runs.is_some()
        || goal.last_classifier_verdict.is_some()
        || goal.last_classifier_details_path.is_some()
}

/// Display string for the classifier details-path row: the path when it
/// exists (existence resolved once on receipt and passed as `exists`),
/// `(unavailable)` when a path was reported but the file is missing (a
/// fail-open run may not have written it), or a hyphen when no path was
/// reported at all.
fn classifier_details_display(path: Option<&str>, exists: bool) -> &str {
    match path {
        Some(p) if exists => p,
        Some(_) => "(unavailable)",
        None => "-",
    }
}

/// Human-readable label for a classifier verdict. Explicit match
/// (no wildcard) so adding a third verdict variant forces an audit
/// of every render site.
fn classifier_verdict_label(verdict: Option<GoalClassifierVerdict>) -> &'static str {
    match verdict {
        Some(GoalClassifierVerdict::Achieved) => "Achieved",
        Some(GoalClassifierVerdict::NotAchieved) => "Not Achieved",
        None => "Not yet evaluated",
    }
}

/// Humanize a wire goal-event name (+ optional detail) for the Recent
/// History row — the single wire→display mapping, so machine vocabulary
/// (`goal_paused`, snake_case detail) never reaches the user. Detail is
/// folded into the label for the events that carry one (pause cause,
/// premature-stop pattern); unknown events fall back to a de-snake-cased
/// form so a future shell event still renders readably.
fn humanize_goal_event(event: &str, detail: Option<&str>) -> String {
    // Variable passthroughs (model/wire-derived) are control-stripped so they
    // can't leak control bytes; the fixed labels below are `&'static`.
    let phrase = |d: Option<&str>| d.map(|s| strip_control_chars(&s.replace('_', " "), false));
    match event {
        "goal_created" => "Goal created".into(),
        "planning_started" => "Planning started".into(),
        "planning_completed" => "Planning completed".into(),
        "planning_failed" => "Planning failed".into(),
        "worker_started" => "Worker started".into(),
        "worker_completed" => "Worker completed".into(),
        "worker_failed" => "Worker failed".into(),
        "context_rotated" => "Context rotated".into(),
        // A plain user pause has no extra cause worth showing.
        "goal_paused" => match phrase(detail).filter(|d| d != "user") {
            Some(d) => format!("Paused: {d}"),
            None => "Paused".into(),
        },
        "goal_resumed" => "Resumed".into(),
        "goal_completed" => "Completed".into(),
        "goal_cleared" => "Cleared".into(),
        "budget_exceeded" => "Budget exceeded".into(),
        "premature_stop_detected" => match phrase(detail) {
            Some(d) => format!("Stopped early: {d}"),
            None => "Stopped early".into(),
        },
        other => {
            let mut s = strip_control_chars(&other.replace('_', " "), false);
            if let Some(c) = s.get_mut(0..1) {
                c.make_ascii_uppercase();
            }
            s
        }
    }
}

/// Render a wire RFC3339 event timestamp as a coarse relative time
/// ("2m ago"). Empty stays empty; an unparseable value (legacy / non-RFC3339)
/// is returned verbatim (control-stripped, since it's a raw passthrough).
fn humanize_event_timestamp(ts: &str) -> String {
    if ts.is_empty() {
        return String::new();
    }
    let Ok(dt) = chrono::DateTime::parse_from_rfc3339(ts) else {
        return strip_control_chars(ts, false);
    };
    let secs = chrono::Utc::now()
        .signed_duration_since(dt.with_timezone(&chrono::Utc))
        .num_seconds()
        .max(0) as u64;
    let ago = crate::util::format_time_ago(std::time::Duration::from_secs(secs));
    if ago == "just now" {
        ago
    } else {
        format!("{ago} ago")
    }
}

/// Compute the overlay area (centered, sized to content, clamped to screen).
pub fn goal_detail_area(screen: Rect, goal: &GoalDisplayState, todos: &[TodoItem]) -> Rect {
    let width_pct = 0.90f32;
    let preferred_w = (screen.width as f32 * width_pct) as u16;
    let w = preferred_w
        .clamp(60, 140)
        .min(screen.width.saturating_sub(4));

    // Inner content width matches the render path:
    //   `inner` = block.inner(area) gives `w - 2` (the rounded border).
    //   We further indent by 1 column on each side (`x = inner.x + 1`,
    //   `w = inner.width - 2`), so the usable text width is `w - 4`.
    // Mirror that here so pause-message wrapping computes the same row
    // count the renderer will produce.
    let inner_w = w.saturating_sub(4);

    // Compute content height based on what will actually be rendered. Each
    // optional section OWNS its leading blank separator (rendered only when
    // the section renders) so the height budget and the render path stay in
    // lockstep.
    //   2  border (top + bottom)
    //   1  status line
    //   N  pause_message reason block (wrapped, when paused + Some)
    //   1  pause hint line (only when any paused variant)
    //   1  budget/tokens line
    //   1  progress bar (only if budget set)
    //   1  blank separator (unconditional, before the progress section)
    //   1  progress header / "no progress items yet"
    //   N  todo items (+ optional "+N more")
    //   2-3 subagent block (if active): blank + role line + optional detail line
    //   N  per-model token rows (only with an active subagent + ≥2 models,
    //      capped at MAX_MODEL_DISPLAY + optional "+N more")
    //   5  completion review (if classifier activity): blank + header + 3 lines
    //   3  recent history (if last_event present): blank + header + event line
    //   1  commands hint
    let has_budget = goal.token_budget.is_some_and(|b| b > 0);
    let budget_bar = if has_budget { 1u16 } else { 0 };
    let recovery_hint = if goal.status.is_paused()
        || matches!(
            goal.status,
            GoalDisplayStatus::Failed | GoalDisplayStatus::Interrupted
        ) {
        1u16
    } else {
        0
    };
    // Reason block renders as `Reason: <pause_message>` wrapped to the
    // inner column width. Prefix is part of the wrapped content so
    // continuation rows just continue at column 0 without alignment
    // tricks; matches the renderer's loop exactly. Gated on
    // `is_paused()` to stay in sync with the renderer — a future shell
    // bug that leaks `pause_message` on a non-paused snapshot must not
    // grow the modal box without also rendering content into it.
    let reason_lines = if goal.status.is_paused()
        || matches!(
            goal.status,
            GoalDisplayStatus::Failed | GoalDisplayStatus::Interrupted
        ) {
        goal.pause_message
            .as_deref()
            .map(|m| {
                let formatted = format_pause_reason(m);
                wrap_pause_message_lines(&formatted, inner_w).len() as u16
            })
            .unwrap_or(0)
    } else {
        0
    };
    let todo_lines = if todos.is_empty() {
        1u16 // "No progress items yet"
    } else {
        let item_count = todos.len().min(MAX_TODO_DISPLAY) as u16;
        let overflow = if todos.len() > MAX_TODO_DISPLAY {
            1u16
        } else {
            0
        };
        1 + item_count + overflow // header + items + optional "+N more"
    };
    let subagent_lines = if goal.current_subagent_role.is_some() {
        // blank + role line, plus the detail line ONLY when there's a live
        // metric to show — matches the render, which skips the detail row when
        // every live_* field is None (a just-spawned subagent).
        let has_detail = goal.live_subagent_tokens.is_some()
            || goal.live_context_pct.is_some()
            || goal.live_turn_count.is_some()
            || goal.live_tool_call_count.is_some();
        2 + u16::from(has_detail)
    } else {
        0
    };
    // Gated on an active subagent so the breakdown can't render orphaned;
    // `per_model_row_count` owns the ≥2 collapse + cap (and keeps this
    // height term in lockstep with the render loop below).
    let per_model_lines = if goal.current_subagent_role.is_some() {
        per_model_row_count(&goal.live_tokens_by_model)
    } else {
        0
    };
    let history_lines = if goal.last_event.is_some() {
        3u16 // blank + header + event line
    } else {
        0
    };
    // "Completion review" section: blank + header + 3 content lines.
    // Rendered only when the goal has at least one classifier signal.
    let completion_review_lines = if has_classifier_activity(goal) {
        5u16
    } else {
        0
    };
    let content_h = 2
        + 1
        + reason_lines
        + recovery_hint
        + 1
        + budget_bar
        + 1
        + todo_lines
        + subagent_lines
        + per_model_lines
        + completion_review_lines
        + history_lines
        + 1;
    let v_margin = 2u16;
    let h = content_h.min(screen.height.saturating_sub(v_margin * 2));

    let x = screen.x + (screen.width.saturating_sub(w)) / 2;
    let y = screen.y + (screen.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w, h)
}

/// Render the goal detail overlay into the buffer.
///
/// Draws a bordered popup with:
/// - Title: objective
/// - Status + phase
/// - Token budget progress bar
/// - Todo progress list
/// - Active subagent metrics
/// - Recent event history
/// - Available commands hint
#[allow(clippy::too_many_arguments)]
pub fn render_goal_detail(
    buf: &mut Buffer,
    area: Rect,
    goal: &GoalDisplayState,
    todos: &[TodoItem],
    tick: usize,
    context_used: Option<u64>,
    active_subagent_tokens: u64,
    close_hovered: bool,
) -> Option<Rect> {
    let theme = Theme::current();
    if area.width < 20 || area.height < 6 {
        return None;
    }

    // Clear the popup area.
    let clear_style = Style::default().bg(theme.bg_base);
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            if let Some(cell) = buf.cell_mut(ratatui::layout::Position::new(x, y)) {
                cell.reset();
                cell.set_style(clear_style);
            }
        }
    }

    // Render border.
    let border_style = Style::default().fg(theme.gray).bg(theme.bg_base);
    let block = ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(border_style)
        .style(Style::default().bg(theme.bg_base));
    let inner = block.inner(area);
    ratatui::widgets::Widget::render(block, area, buf);

    // Close button geometry is needed up-front so the title can be budgeted
    // to stop before it. Close button [✗] in top-right (ASCII `[x]` on legacy
    // ConHost).
    let close_text = format!("[{}]", crate::glyphs::ballot_x());
    // Display width (not byte length) so the hit-rect matches the glyph cells
    // and the title budget below is computed from the real column position.
    let close_w = unicode_width::UnicodeWidthStr::width(close_text.as_str()) as u16;
    let close_x = area.x + area.width.saturating_sub(close_w + 1);

    // Title in the top border: the live objective so the user can see WHICH
    // goal is running (with a spinner when active). The objective is
    // truncated by DISPLAY WIDTH (CJK / emoji safe) to the columns between
    // the left inset and the close button, so a long objective can never
    // collide with `[✗]` or overflow the right border.
    let is_active = matches!(goal.status, GoalDisplayStatus::Active);
    let spinner_prefix = if is_active {
        let frames = crate::glyphs::dot_spinner_frames();
        let frame = frames[(tick / 4) % frames.len()];
        format!("{frame} ")
    } else {
        String::new()
    };
    let title_cols = close_x.saturating_sub(area.x + 3) as usize; // 1-col gap before [✗]
    let objective_budget = title_cols
        .saturating_sub(unicode_width::UnicodeWidthStr::width(
            spinner_prefix.as_str(),
        ))
        .saturating_sub(2); // leading + trailing space
    let cleaned = sanitize_title(&goal.objective);
    let objective = if cleaned.is_empty() {
        "Active Goal".to_owned()
    } else {
        truncate_to_width(&cleaned, objective_budget)
    };
    let title_text = format!(" {spinner_prefix}{objective} ");
    let title_style = Style::default()
        .fg(theme.accent_plan)
        .bg(theme.bg_base)
        .add_modifier(Modifier::BOLD);
    buf.set_span_safe(
        area.x + 2,
        area.y,
        &Span::styled(title_text, title_style),
        title_cols as u16,
    );

    let close_style = if close_hovered {
        Style::default()
            .fg(theme.text_primary)
            .bg(theme.bg_base)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.gray).bg(theme.bg_base)
    };
    buf.set_span_safe(
        close_x,
        area.y,
        &Span::styled(close_text, close_style),
        close_w,
    );
    let close_rect = Rect::new(close_x, area.y, close_w, 1);

    let mut y = inner.y;
    let x = inner.x + 1;
    let w = inner.width.saturating_sub(2);

    // ── Status line ──
    let (status_text, status_color, phase_text) = status_label(goal);
    let mut status_spans = vec![
        Span::styled("Status: ", Style::default().fg(theme.gray)),
        Span::styled(
            status_text,
            Style::default()
                .fg(status_color)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    if !phase_text.is_empty() {
        status_spans.push(Span::styled(
            format!(" \u{00b7} {phase_text}"),
            Style::default().fg(theme.gray_bright),
        ));
    }

    buf.set_line_safe(x, y, &Line::from(status_spans), w);
    y += 1;

    if y >= inner.y + inner.height {
        return Some(close_rect);
    }

    if goal.status.is_paused() {
        let hint = format!(
            "Status: {}. Type /goal resume to continue",
            goal.status.pause_label()
        );
        buf.set_line_safe(
            x,
            y,
            &Line::from(Span::styled(hint, Style::default().fg(theme.warning))),
            w,
        );
        y += 1;
    } else if matches!(
        goal.status,
        GoalDisplayStatus::Failed | GoalDisplayStatus::Interrupted
    ) {
        let label = if goal.status == GoalDisplayStatus::Interrupted {
            "Interrupted"
        } else {
            "Failed"
        };
        let hint = format!("Status: {label}. Type /goal clear, then start a new goal");
        buf.set_line_safe(
            x,
            y,
            &Line::from(Span::styled(hint, Style::default().fg(theme.warning))),
            w,
        );
        y += 1;

        if y >= inner.y + inner.height {
            return Some(close_rect);
        }
    }

    //
    if (goal.status.is_paused()
        || matches!(
            goal.status,
            GoalDisplayStatus::Failed | GoalDisplayStatus::Interrupted
        ))
        && let Some(msg) = goal.pause_message.as_deref()
    {
        let formatted = format_pause_reason(msg);
        for line in wrap_pause_message_lines(&formatted, w) {
            if y >= inner.y + inner.height {
                return Some(close_rect);
            }
            buf.set_line_safe(
                x,
                y,
                &Line::from(Span::styled(line, Style::default().fg(theme.warning))),
                w,
            );
            y += 1;
        }
    }

    // ── Budget / tokens line with optional progress bar ──
    let tokens_str =
        format_tokens_compact(goal.live_tokens_used(context_used, active_subagent_tokens));
    let elapsed_str = format_elapsed(goal.live_elapsed_ms());

    let (pct, budget_display) = if let Some(budget) = goal.token_budget.filter(|&b| b > 0) {
        let live = goal.live_tokens_used(context_used, active_subagent_tokens);
        let p = (live as f64 / budget as f64).min(1.0) as f32;
        let budget_str = format_tokens_compact(budget);
        (p, format!("{tokens_str} / {budget_str} tokens"))
    } else {
        (0.0, format!("{tokens_str} tokens"))
    };
    let has_budget = goal.token_budget.is_some_and(|b| b > 0);
    let budget_label = if has_budget {
        let pct_display = format!(" ({:.0}%)", pct * 100.0);
        format!("Budget: {budget_display}{pct_display}  Elapsed: {elapsed_str}")
    } else {
        format!("Tokens: {budget_display}  Elapsed: {elapsed_str}")
    };
    buf.set_line_safe(
        x,
        y,
        &Line::from(Span::styled(
            budget_label,
            Style::default().fg(theme.gray_bright),
        )),
        w,
    );
    y += 1;

    if y >= inner.y + inner.height {
        return Some(close_rect);
    }

    // Progress bar — only when a budget is set.
    if has_budget {
        let bar_w = w.min(30);
        let fg = budget_color(pct, &theme);
        let bg = theme.scrollbar_bg;
        let bar_spans = progress_bar_spans(bar_w, pct, fg, bg);
        let pct_label = format!(" {:.0}%", pct * 100.0);
        let mut line_spans = vec![Span::styled("[", Style::default().fg(theme.gray))];
        line_spans.extend(bar_spans);
        line_spans.push(Span::styled("]", Style::default().fg(theme.gray)));
        line_spans.push(Span::styled(
            pct_label,
            Style::default().fg(theme.gray_bright),
        ));
        buf.set_line_safe(x, y, &Line::from(line_spans), w);
        y += 1;
    }

    if y >= inner.y + inner.height {
        return Some(close_rect);
    }

    // ── Blank separator ──
    y += 1;

    if y >= inner.y + inner.height {
        return Some(close_rect);
    }

    // ── Progress section (todo items) ──
    if todos.is_empty() {
        buf.set_line_safe(
            x,
            y,
            &Line::from(Span::styled(
                "No progress items yet",
                Style::default().fg(theme.gray),
            )),
            w,
        );
        y += 1;
    } else {
        buf.set_line_safe(
            x,
            y,
            &Line::from(Span::styled(
                "Progress:",
                Style::default()
                    .fg(theme.text_primary)
                    .add_modifier(Modifier::BOLD),
            )),
            w,
        );
        y += 1;

        let display_count = todos.len().min(MAX_TODO_DISPLAY);
        for item in &todos[..display_count] {
            if y >= inner.y + inner.height.saturating_sub(1) {
                break;
            }
            let (icon, icon_color) = match item.status {
                TodoStatus::Pending => ("\u{25a1}", theme.gray),
                TodoStatus::InProgress => ("\u{25b6}", theme.warning),
                TodoStatus::Completed => (crate::glyphs::check_mark(), theme.accent_success),
                TodoStatus::Cancelled => (crate::glyphs::ballot_x(), theme.accent_error),
            };
            // Reserve space for "  {icon} " prefix (~4 cols) + content.
            // Use display width (not char count) so CJK / emoji measure correctly.
            let content_budget = (w as usize).saturating_sub(5);
            let content_display = truncate_to_width(&item.content, content_budget);
            let spans = vec![
                Span::raw("  "),
                Span::styled(icon, Style::default().fg(icon_color)),
                Span::raw(" "),
                Span::styled(content_display, Style::default().fg(theme.text_primary)),
            ];
            buf.set_line_safe(x, y, &Line::from(spans), w);
            y += 1;
        }
        if todos.len() > MAX_TODO_DISPLAY {
            let remaining = todos.len() - MAX_TODO_DISPLAY;
            buf.set_line_safe(
                x,
                y,
                &Line::from(Span::styled(
                    format!("  +{remaining} more"),
                    Style::default().fg(theme.gray),
                )),
                w,
            );
            y += 1;
        }
    }

    if y >= inner.y + inner.height {
        return Some(close_rect);
    }

    // ── Active subagent metrics (with a leading blank separator) ──
    if let Some(ref role) = goal.current_subagent_role {
        // Leading blank — budgeted in `subagent_lines` (renders only with the block).
        y += 1;
        if y >= inner.y + inner.height {
            return Some(close_rect);
        }
        let mut subagent_spans = vec![
            Span::styled("Active Subagent: ", Style::default().fg(theme.gray)),
            Span::styled(
                role.as_str(),
                Style::default()
                    .fg(theme.accent_running)
                    .add_modifier(Modifier::BOLD),
            ),
        ];
        let rounds = goal.total_worker_rounds + goal.total_verify_rounds;
        if rounds > 0 {
            subagent_spans.push(Span::styled(
                format!(" (round {rounds})"),
                Style::default().fg(theme.gray),
            ));
        }
        buf.set_line_safe(x, y, &Line::from(subagent_spans), w);
        y += 1;

        if y < inner.y + inner.height {
            // Subagent detail line.
            let mut detail_parts: Vec<String> = Vec::new();
            if let Some(tok) = goal.live_subagent_tokens {
                detail_parts.push(format!(
                    "Tokens: {}",
                    format_tokens_compact(tok.min(i64::MAX as u64) as i64)
                ));
            }
            if let Some(ctx) = goal.live_context_pct {
                detail_parts.push(format!("Context: {ctx}%"));
            }
            if let Some(turns) = goal.live_turn_count {
                detail_parts.push(format!("Turns: {turns}"));
            }
            if let Some(tools) = goal.live_tool_call_count {
                detail_parts.push(format!("Tools: {tools}"));
            }
            if !detail_parts.is_empty() {
                let detail = format!("  {}", detail_parts.join("  "));
                buf.set_line_safe(
                    x,
                    y,
                    &Line::from(Span::styled(detail, Style::default().fg(theme.gray_bright))),
                    w,
                );
                y += 1;
            }
        }

        // Per-model token breakdown, under the active-subagent block.
        // `per_model_row_count` is the shared gate/cap (height ↔ render).
        if per_model_row_count(&goal.live_tokens_by_model) > 0 {
            use unicode_width::UnicodeWidthStr;
            for (model_id, tokens) in goal.live_tokens_by_model.iter().take(MAX_MODEL_DISPLAY) {
                if y >= inner.y + inner.height {
                    return Some(close_rect);
                }
                let tokens_str = format_tokens_compact((*tokens).min(i64::MAX as u64) as i64);
                // Budget the model id to the columns left after the "  "
                // indent and "  <tokens>" suffix, measured in display
                // columns (not bytes) so wide glyphs never overflow the row.
                let id_budget = (w as usize)
                    .saturating_sub(4)
                    .saturating_sub(UnicodeWidthStr::width(tokens_str.as_str()));
                let id = truncate_to_width(model_id, id_budget);
                buf.set_line_safe(
                    x,
                    y,
                    &Line::from(Span::styled(
                        format!("  {id}  {tokens_str}"),
                        Style::default().fg(theme.gray_bright),
                    )),
                    w,
                );
                y += 1;
            }
            if goal.live_tokens_by_model.len() > MAX_MODEL_DISPLAY && y < inner.y + inner.height {
                let remaining = goal.live_tokens_by_model.len() - MAX_MODEL_DISPLAY;
                buf.set_line_safe(
                    x,
                    y,
                    &Line::from(Span::styled(
                        format!("  +{remaining} more"),
                        Style::default().fg(theme.gray),
                    )),
                    w,
                );
                y += 1;
            }
        }
    }

    if y >= inner.y + inner.height {
        return Some(close_rect);
    }

    // ── Completion review (only when classifier has run at least once) ──
    if has_classifier_activity(goal) {
        // Blank separator.
        y += 1;
        if y >= inner.y + inner.height {
            return Some(close_rect);
        }

        buf.set_line_safe(
            x,
            y,
            &Line::from(Span::styled(
                "Completion review:",
                Style::default()
                    .fg(theme.text_primary)
                    .add_modifier(Modifier::BOLD),
            )),
            w,
        );
        y += 1;

        if y < inner.y + inner.height {
            let verdict_label = classifier_verdict_label(goal.last_classifier_verdict);
            buf.set_line_safe(
                x,
                y,
                &Line::from(vec![
                    Span::styled("  Last verdict: ", Style::default().fg(theme.gray)),
                    Span::styled(verdict_label, Style::default().fg(theme.text_secondary)),
                ]),
                w,
            );
            y += 1;
        }

        if y < inner.y + inner.height {
            // Shared label with the chip; empty (no run reserved yet) falls
            // back to a hyphen so the row never reads "Attempts: ".
            let attempts = classifier_attempts_label(goal);
            let attempts_display = if attempts.is_empty() {
                "-".to_owned()
            } else {
                attempts
            };
            buf.set_line_safe(
                x,
                y,
                &Line::from(vec![
                    Span::styled("  Attempts: ", Style::default().fg(theme.gray)),
                    Span::styled(attempts_display, Style::default().fg(theme.text_secondary)),
                ]),
                w,
            );
            y += 1;
        }

        if y < inner.y + inner.height {
            let path_display = classifier_details_display(
                goal.last_classifier_details_path.as_deref(),
                goal.last_classifier_details_exists,
            );
            buf.set_line_safe(
                x,
                y,
                &Line::from(vec![
                    Span::styled("  Details: ", Style::default().fg(theme.gray)),
                    Span::styled(
                        path_display.to_owned(),
                        Style::default().fg(theme.text_secondary),
                    ),
                ]),
                w,
            );
            y += 1;
        }
    }

    if y >= inner.y + inner.height {
        return Some(close_rect);
    }

    // ── Recent history (with a leading blank separator) ──
    if goal.last_event.is_some() {
        // Leading blank — budgeted in `history_lines` (renders only with the block).
        y += 1;
        if y >= inner.y + inner.height {
            return Some(close_rect);
        }
        buf.set_line_safe(
            x,
            y,
            &Line::from(Span::styled(
                "Recent History:",
                Style::default()
                    .fg(theme.text_primary)
                    .add_modifier(Modifier::BOLD),
            )),
            w,
        );
        y += 1;

        if y < inner.y + inner.height
            && let Some(ref event) = goal.last_event
        {
            // Humanize both the event label (folding in the detail) and the
            // timestamp so the user sees "2m ago  Paused: doom loop", not the
            // raw wire vocabulary. The timestamp renders first (left gutter),
            // then the humanized label — matching the span order below.
            let label = humanize_goal_event(event, goal.last_event_detail.as_deref());
            let ts_display =
                humanize_event_timestamp(goal.last_event_timestamp.as_deref().unwrap_or(""));
            let prefix = if ts_display.is_empty() {
                "  ".to_owned()
            } else {
                format!("  {ts_display}  ")
            };
            let spans = vec![
                Span::styled(prefix, Style::default().fg(theme.gray)),
                Span::styled(label, Style::default().fg(theme.text_secondary)),
            ];
            buf.set_line_safe(x, y, &Line::from(spans), w);
            y += 1;
        }
    }

    // ── Commands hint ──
    if y < inner.y + inner.height {
        let hint_style = Style::default().fg(theme.gray_dim);
        let hint = if matches!(
            goal.status,
            GoalDisplayStatus::Failed | GoalDisplayStatus::Interrupted
        ) {
            "Esc: close  /goal clear, then start a new goal"
        } else {
            "Esc: close  /goal resume | pause | status | clear"
        };
        buf.set_line_safe(x, y, &Line::from(Span::styled(hint, hint_style)), w);
    }

    Some(close_rect)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::agent::{GoalDisplayPhase, GoalDisplayState, GoalDisplayStatus};
    use ratatui::layout::Rect;

    fn make_goal() -> GoalDisplayState {
        GoalDisplayState {
            goal_id: "g-1".into(),
            objective: "Implement dark mode".into(),
            status: GoalDisplayStatus::Active,
            phase: GoalDisplayPhase::Executing,
            token_budget: Some(50_000),
            tokens_used: 12_300,
            elapsed_ms: 180_000,
            total_deliverables: 0,
            completed_deliverables: 0,
            current_deliverable_id: None,
            current_deliverable_title: None,
            current_subagent_role: Some("Worker".into()),
            total_worker_rounds: 3,
            total_verify_rounds: 1,
            live_subagent_tokens: Some(8200),
            live_tokens_by_model: Vec::new(),
            live_context_pct: Some(34),
            live_turn_count: Some(5),
            live_tool_call_count: Some(12),
            last_event: Some("Worker #2 started".into()),
            last_event_detail: Some("round 3".into()),
            last_event_timestamp: Some("1m ago".into()),
            token_baseline: 0,
            finished_subagent_tokens: 0,
            deliverables: vec![],
            pause_message: None,
            classifier_runs_attempted: None,
            classifier_max_runs: None,
            last_classifier_verdict: None,
            last_classifier_details_path: None,
            last_classifier_details_exists: false,
            verifying_completion: false,
            planning: false,
            received_at: std::time::Instant::now(),
            elapsed_floor_ms: 0,
        }
    }

    #[test]
    fn goal_detail_area_centered() {
        let screen = Rect::new(0, 0, 120, 40);
        let goal = make_goal();
        let area = goal_detail_area(screen, &goal, &[]);
        assert!(area.x > 0);
        assert!(area.y > 0);
        // 120 * 0.90 = 108, clamped to [60, 140]
        assert_eq!(area.width, 108);
        // content_h = 2+1+1+1+1+1+3+3+1 = 14 (budget, progress, subagent, history)
        assert_eq!(area.height, 14);
        assert!(area.x + area.width <= screen.width);
        assert!(area.y + area.height <= screen.height);
    }

    #[test]
    fn goal_detail_area_small_screen() {
        let screen = Rect::new(0, 0, 30, 15);
        let goal = make_goal();
        let area = goal_detail_area(screen, &goal, &[]);
        assert!(area.width <= 30);
        assert!(area.height <= 15);
        // content_h=14, clamped to 15 - v_margin*2 = 11
        assert_eq!(area.height, 11);
    }

    #[test]
    fn goal_detail_area_tiny_screen() {
        let screen = Rect::new(0, 0, 20, 10);
        let goal = make_goal();
        let area = goal_detail_area(screen, &goal, &[]);
        assert!(area.width <= 20);
        // content_h=14, clamped to 10 - v_margin*2 = 6
        assert_eq!(area.height, 6);
        // Still fits on screen
        assert!(area.y + area.height <= screen.height);
    }

    #[test]
    fn goal_detail_area_very_tiny_screen() {
        // Screen so small the render function would bail (< 6 height),
        // but the area computation itself still produces valid rects.
        let screen = Rect::new(0, 0, 20, 8);
        let goal = make_goal();
        let area = goal_detail_area(screen, &goal, &[]);
        // content_h=14, clamped to 8 - v_margin*2 = 4
        assert_eq!(area.height, 4);
        assert!(area.y + area.height <= screen.height);
    }

    #[test]
    fn goal_detail_area_widens_for_pause_hint() {
        // A paused goal renders one extra row (the "Type /goal resume"
        // hint), so the modal's content height must be exactly +1 row
        // larger than the same goal in the Active state.
        let screen = Rect::new(0, 0, 120, 40);
        let mut goal = make_goal();
        let baseline = goal_detail_area(screen, &goal, &[]).height;
        goal.status = GoalDisplayStatus::UserPaused;
        let with_hint = goal_detail_area(screen, &goal, &[]).height;
        assert_eq!(with_hint, baseline + 1);
    }

    /// Helper: render the goal-detail modal at a known size and return
    /// the buffer flattened to a single string so assertions can
    /// `.contains(...)` on the visible text without worrying about
    /// span boundaries.
    fn render_to_text(goal: &GoalDisplayState) -> String {
        let screen = Rect::new(0, 0, 100, 40);
        let mut buf = ratatui::buffer::Buffer::empty(screen);
        let area = goal_detail_area(screen, goal, &[]);
        render_goal_detail(&mut buf, area, goal, &[], 0, None, 0, false);
        let mut s = String::new();
        for y in 0..screen.height {
            for x in 0..screen.width {
                if let Some(cell) = buf.cell(ratatui::layout::Position::new(x, y)) {
                    s.push_str(cell.symbol());
                }
            }
            s.push('\n');
        }
        s
    }

    #[test]
    fn goal_detail_renders_completion_review_section_when_classifier_ran() {
        // Completion-review section: when the goal carries at least one
        // classifier signal, the modal shows a "Completion review"
        // block with the verdict, attempts counter, and details path.
        // Existence is the cached `last_classifier_details_exists` bool
        // (resolved on receipt), so set it true to render the path.
        let mut goal = make_goal();
        goal.classifier_runs_attempted = Some(1);
        goal.classifier_max_runs = Some(3);
        goal.last_classifier_verdict = Some(GoalClassifierVerdict::NotAchieved);
        goal.last_classifier_details_path = Some("/tmp/goal-details.md".into());
        goal.last_classifier_details_exists = true;

        let text = render_to_text(&goal);
        assert!(
            text.contains("Completion review:"),
            "expected header, got:\n{text}"
        );
        assert!(
            text.contains("Last verdict: Not Achieved"),
            "expected verdict line, got:\n{text}"
        );
        assert!(
            text.contains("Attempts: 1/3"),
            "expected attempts line, got:\n{text}"
        );
        assert!(
            text.contains("/tmp/goal-details.md") && !text.contains("(unavailable)"),
            "expected existing details path to render, got:\n{text}"
        );
    }

    #[test]
    fn goal_detail_renders_achieved_verdict() {
        // Both verdict variants exercise the explicit match — guards
        // against a future wildcard arm collapsing the labels.
        let mut goal = make_goal();
        goal.classifier_runs_attempted = Some(2);
        goal.classifier_max_runs = Some(3);
        goal.last_classifier_verdict = Some(GoalClassifierVerdict::Achieved);

        let text = render_to_text(&goal);
        assert!(
            text.contains("Last verdict: Achieved"),
            "expected Achieved verdict line, got:\n{text}"
        );
    }

    #[test]
    fn goal_detail_omits_completion_review_when_no_classifier_activity() {
        // Defaults: a goal that has never been classified shows
        // nothing extra — the modal must omit the entire section
        // (including its header) when all four classifier fields are
        // `None`.
        let goal = make_goal();
        assert!(goal.classifier_runs_attempted.is_none());
        assert!(goal.classifier_max_runs.is_none());
        assert!(goal.last_classifier_verdict.is_none());
        assert!(goal.last_classifier_details_path.is_none());

        let text = render_to_text(&goal);
        assert!(
            !text.contains("Completion review"),
            "section header must be absent, got:\n{text}"
        );
        assert!(
            !text.contains("Last verdict"),
            "verdict line must be absent, got:\n{text}"
        );
    }

    #[test]
    fn goal_detail_renders_completion_review_with_partial_signal() {
        // Edge case: only `classifier_max_runs` is set (e.g. the goal
        // was configured but the classifier has not yet run). The
        // section still renders so the user can see the configured cap.
        let mut goal = make_goal();
        goal.classifier_max_runs = Some(3);

        let text = render_to_text(&goal);
        assert!(text.contains("Completion review:"));
        assert!(text.contains("Last verdict: Not yet evaluated"));
        assert!(text.contains("Attempts: 0/3"));
        assert!(text.contains("Details: -"));
    }

    #[test]
    fn goal_detail_status_line_shows_verifying_phase_with_counter() {
        // The `verifying_completion` overlay must surface in the modal,
        // not just the chip; the counter comes from the classifier fields.
        let mut goal = make_goal();
        goal.verifying_completion = true;
        goal.classifier_runs_attempted = Some(2);
        goal.classifier_max_runs = Some(3);

        let text = render_to_text(&goal);
        assert!(
            text.contains("Active \u{00b7} Verifying (2/3)"),
            "status line must show the verifying overlay with counter, got:\n{text}"
        );
        assert!(
            !text.contains("Executing"),
            "verifying overlay must replace the Executing phase, got:\n{text}"
        );
    }

    #[test]
    fn goal_detail_status_line_shows_planning_phase() {
        let mut goal = make_goal();
        goal.planning = true;

        let text = render_to_text(&goal);
        assert!(
            text.contains("Active \u{00b7} Planning"),
            "status line must show the planning overlay, got:\n{text}"
        );
    }

    #[test]
    fn goal_detail_status_line_verifying_wins_over_planning() {
        // If both overlays were somehow set, verification wins.
        let mut goal = make_goal();
        goal.verifying_completion = true;
        goal.planning = true;
        goal.classifier_runs_attempted = Some(1);
        goal.classifier_max_runs = Some(3);

        let text = render_to_text(&goal);
        assert!(
            text.contains("Verifying (1/3)"),
            "verifying overlay must win over planning, got:\n{text}"
        );
        assert!(
            !text.contains("\u{00b7} Planning"),
            "planning must not render when verifying is also set, got:\n{text}"
        );
    }

    #[test]
    fn goal_detail_status_line_shows_executing_without_overlays() {
        let goal = make_goal();
        assert!(!goal.verifying_completion);
        assert!(!goal.planning);

        let text = render_to_text(&goal);
        assert!(
            text.contains("Active \u{00b7} Executing"),
            "steady-state status line must show Executing, got:\n{text}"
        );
    }

    #[test]
    fn render_goal_detail_does_not_panic() {
        let screen = Rect::new(0, 0, 80, 30);
        let mut buf = ratatui::buffer::Buffer::empty(screen);
        let goal = make_goal();
        let area = goal_detail_area(screen, &goal, &[]);
        render_goal_detail(&mut buf, area, &goal, &[], 0, None, 0, false);
    }

    #[test]
    fn render_goal_detail_too_small_is_noop() {
        let area = Rect::new(0, 0, 10, 4);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        let goal = make_goal();
        // Should not panic, just bail early.
        render_goal_detail(&mut buf, area, &goal, &[], 0, None, 0, false);
    }

    #[test]
    fn format_elapsed_values() {
        assert_eq!(format_elapsed(0), "0s");
        assert_eq!(format_elapsed(5_000), "5s");
        assert_eq!(format_elapsed(65_000), "1m05s");
        assert_eq!(format_elapsed(3_665_000), "1h01m");
    }

    #[test]
    fn budget_color_green() {
        let theme = Theme::current();
        assert_eq!(budget_color(0.3, &theme), theme.accent_success);
    }

    #[test]
    fn budget_color_yellow() {
        let theme = Theme::current();
        assert_eq!(budget_color(0.6, &theme), theme.warning);
    }

    #[test]
    fn budget_color_red() {
        let theme = Theme::current();
        assert_eq!(budget_color(0.9, &theme), theme.accent_error);
    }

    #[test]
    fn budget_color_boundary_at_50_pct() {
        let theme = Theme::current();
        // Exactly 50% should be yellow per spec (green <50%, yellow 50-80%).
        assert_eq!(budget_color(0.50, &theme), theme.warning);
        // Just below 50% should be green.
        assert_eq!(budget_color(0.499, &theme), theme.accent_success);
    }

    #[test]
    fn render_no_budget() {
        let screen = Rect::new(0, 0, 80, 30);
        let mut buf = ratatui::buffer::Buffer::empty(screen);
        let mut goal = make_goal();
        goal.token_budget = None;
        let area = goal_detail_area(screen, &goal, &[]);
        render_goal_detail(&mut buf, area, &goal, &[], 0, None, 0, false);
    }

    #[test]
    fn render_no_deliverables() {
        let screen = Rect::new(0, 0, 80, 30);
        let mut buf = ratatui::buffer::Buffer::empty(screen);
        let mut goal = make_goal();
        goal.total_deliverables = 0;
        goal.completed_deliverables = 0;
        goal.current_deliverable_id = None;
        goal.current_deliverable_title = None;
        goal.deliverables.clear();
        let area = goal_detail_area(screen, &goal, &[]);
        render_goal_detail(&mut buf, area, &goal, &[], 0, None, 0, false);
    }

    #[test]
    fn render_no_subagent() {
        let screen = Rect::new(0, 0, 80, 30);
        let mut buf = ratatui::buffer::Buffer::empty(screen);
        let mut goal = make_goal();
        goal.current_subagent_role = None;
        goal.live_subagent_tokens = None;
        goal.live_context_pct = None;
        goal.live_turn_count = None;
        goal.live_tool_call_count = None;
        let area = goal_detail_area(screen, &goal, &[]);
        render_goal_detail(&mut buf, area, &goal, &[], 0, None, 0, false);
    }

    #[test]
    fn render_complete_goal() {
        let screen = Rect::new(0, 0, 80, 30);
        let mut buf = ratatui::buffer::Buffer::empty(screen);
        let mut goal = make_goal();
        goal.status = GoalDisplayStatus::Complete;
        goal.phase = GoalDisplayPhase::Idle;
        goal.current_deliverable_id = None;
        goal.current_subagent_role = None;
        let area = goal_detail_area(screen, &goal, &[]);
        render_goal_detail(&mut buf, area, &goal, &[], 0, None, 0, false);
    }

    /// Count rendered rows whose text contains `needle`. Each line in
    /// `render_to_text` is one screen row, so this counts actual rendered
    /// rows (not raw substring hits).
    fn rows_containing(goal: &GoalDisplayState, needle: &str) -> usize {
        render_to_text(goal)
            .lines()
            .filter(|row| row.contains(needle))
            .count()
    }

    #[test]
    fn goal_detail_area_height_matches_rendered_rows_for_0_2_3_models() {
        // Computed modal height must grow by EXACTLY the number of
        // per-model rows actually rendered (0 / 2 / 3). The area is
        // measured at the SAME width `render_to_text` renders at (100) so
        // the height math and the rendered row count cannot silently
        // diverge.
        let screen = Rect::new(0, 0, 100, 40);
        let mut goal = make_goal();

        goal.live_tokens_by_model = Vec::new();
        let baseline = goal_detail_area(screen, &goal, &[]).height;

        // Single model collapses: no extra rows, no growth.
        goal.live_tokens_by_model = vec![("alpha".into(), 12_000)];
        assert_eq!(
            goal_detail_area(screen, &goal, &[]).height,
            baseline,
            "single model must not grow the modal"
        );
        assert_eq!(rows_containing(&goal, "alpha"), 0, "no row for 1 model");

        // Two models: +2 rows, both rendered.
        goal.live_tokens_by_model = vec![("alpha".into(), 12_000), ("bravo".into(), 8_000)];
        assert_eq!(goal_detail_area(screen, &goal, &[]).height, baseline + 2);
        let rendered_two = rows_containing(&goal, "alpha") + rows_containing(&goal, "bravo");
        assert_eq!(rendered_two, 2, "two model rows rendered");

        // Three models: +3 rows, all rendered.
        goal.live_tokens_by_model = vec![
            ("alpha".into(), 12_000),
            ("bravo".into(), 8_000),
            ("charlie".into(), 1_000),
        ];
        assert_eq!(goal_detail_area(screen, &goal, &[]).height, baseline + 3);
        let rendered_three = rows_containing(&goal, "alpha")
            + rows_containing(&goal, "bravo")
            + rows_containing(&goal, "charlie");
        assert_eq!(rendered_three, 3, "three model rows rendered");
    }

    #[test]
    fn render_per_model_breakdown_shows_each_model() {
        let mut goal = make_goal();
        goal.live_tokens_by_model = vec![
            ("echo-agent-4".into(), 12_300),
            ("echo-agent-3-mini".into(), 8_000),
        ];
        let text = render_to_text(&goal);
        assert!(
            text.contains("echo-agent-4"),
            "first model must render:\n{text}"
        );
        assert!(
            text.contains("echo-agent-3-mini"),
            "second model must render:\n{text}"
        );
        // Compact token formatting is reused.
        assert!(
            text.contains("12.3k"),
            "compact tokens must render:\n{text}"
        );
    }

    #[test]
    fn render_single_model_collapses_breakdown() {
        let mut goal = make_goal();
        goal.live_tokens_by_model = vec![("solo-model".into(), 12_300)];
        // Positive collapse assertions: the active-subagent metrics block
        // still renders its single tokens line, and zero per-model rows are
        // emitted for a single-model goal.
        assert!(
            rows_containing(&goal, "Active Subagent:") >= 1,
            "metrics block must still render"
        );
        assert_eq!(
            rows_containing(&goal, "solo-model"),
            0,
            "single-model goal must render no per-model row"
        );
    }

    #[test]
    fn per_model_breakdown_suppressed_without_active_subagent() {
        // The breakdown is gated on the active-subagent block, so it must
        // not render (or grow the modal) when no subagent is active.
        let screen = Rect::new(0, 0, 120, 40);
        let mut goal = make_goal();
        goal.current_subagent_role = None;
        let baseline = goal_detail_area(screen, &goal, &[]).height;

        goal.live_tokens_by_model = vec![("alpha".into(), 12_000), ("bravo".into(), 8_000)];
        assert_eq!(
            goal_detail_area(screen, &goal, &[]).height,
            baseline,
            "no active subagent => breakdown adds no rows"
        );
        assert_eq!(rows_containing(&goal, "alpha"), 0);
        assert_eq!(rows_containing(&goal, "bravo"), 0);
    }

    #[test]
    fn per_model_breakdown_caps_rows_with_plus_n_more() {
        // More than MAX_MODEL_DISPLAY models render the cap plus a
        // single "+N more" row, and the height stays in lockstep. Measured
        // at the same width `render_to_text` renders at (100).
        let screen = Rect::new(0, 0, 100, 40);
        let mut goal = make_goal();
        goal.live_tokens_by_model = Vec::new();
        let baseline = goal_detail_area(screen, &goal, &[]).height;

        let n = MAX_MODEL_DISPLAY + 2;
        goal.live_tokens_by_model = (0..n)
            .map(|i| (format!("model-{i:02}"), (1000 * (n - i)) as u64))
            .collect();

        // Height = MAX_MODEL_DISPLAY rows + 1 "+N more" row.
        assert_eq!(
            goal_detail_area(screen, &goal, &[]).height,
            baseline + (MAX_MODEL_DISPLAY as u16) + 1
        );

        let text = render_to_text(&goal);
        // First MAX rows render; overflow rows do not.
        assert!(text.contains("model-00"), "top model renders:\n{text}");
        assert!(
            text.contains(&format!("model-{:02}", MAX_MODEL_DISPLAY - 1)),
            "last shown model renders:\n{text}"
        );
        assert!(
            !text.contains(&format!("model-{:02}", MAX_MODEL_DISPLAY)),
            "capped model must NOT render:\n{text}"
        );
        assert!(text.contains("+2 more"), "overflow row renders:\n{text}");
    }

    #[test]
    fn render_per_model_truncates_wide_model_id_by_display_width() {
        // A model id wider than the modal must be truncated using
        // display width (not bytes); the full id must not appear and the
        // ellipsis marker must be present.
        let long_id = "x".repeat(200);
        let cjk_id = "宽".repeat(120); // each glyph is 2 display columns
        let mut goal = make_goal();
        goal.live_tokens_by_model = vec![(long_id.clone(), 12_000), (cjk_id.clone(), 8_000)];
        // render_to_text renders at width 100; mirror that exactly so the
        // per-row column budget below matches the render path.
        let screen = Rect::new(0, 0, 100, 40);
        let area = goal_detail_area(screen, &goal, &[]);
        let text = render_to_text(&goal);
        assert!(
            !text.contains(&long_id),
            "overlong ascii id must be truncated"
        );
        assert!(
            !text.contains(&cjk_id),
            "overlong cjk id must be truncated by display width"
        );
        assert!(text.contains('…'), "truncation ellipsis must render");

        // The CJK row's id is budgeted to the columns left after the "  "
        // indent and "  <tokens>" suffix — identical to the render path
        // (`w = inner.width - 2 = area.width - 4`; tokens "8k" is 2 cols).
        let row_w = area.width as usize - 4;
        let budget = row_w - 4 - "8k".len();
        // Display-width truncation keeps as many 2-column glyphs as fit the
        // budget; a byte-length bug (3 bytes per glyph) would keep strictly
        // FEWER. Compare the rendered glyph count to the display-width
        // truncation of the SAME id at the SAME budget.
        let expected_glyphs = truncate_to_width(&cjk_id, budget).matches('宽').count();
        let byte_bug_glyphs = budget.saturating_sub(1) / 3;
        assert!(
            expected_glyphs > byte_bug_glyphs,
            "test must discriminate: display-width keeps {expected_glyphs} glyphs, \
             a byte-length bug would keep {byte_bug_glyphs}"
        );
        let rendered_glyphs = text.matches('宽').count();
        assert_eq!(
            rendered_glyphs, expected_glyphs,
            "rendered CJK glyph count must match display-width truncation \
             ({expected_glyphs}), not a byte-length bug ({byte_bug_glyphs})"
        );
        // And the truncated id fills the column budget to within one
        // 2-column glyph (glyphs + 1-column ellipsis), proving the budget
        // is measured in display columns — a byte-length bug would leave it
        // far short. Banded (not exact) to stay parity-robust.
        let rendered_cols = rendered_glyphs * 2 + 1;
        assert!(
            rendered_cols > budget - 2 && rendered_cols <= budget,
            "display-width truncation must fill the {budget}-column budget \
             (rendered {rendered_cols} cols)"
        );
    }

    #[test]
    fn status_label_variants() {
        let theme = Theme::current();
        let mut goal = make_goal();

        goal.status = GoalDisplayStatus::Active;
        goal.phase = GoalDisplayPhase::Executing;
        let (s, _, p) = status_label(&goal);
        assert_eq!(s, "Active");
        assert_eq!(p, "Executing");

        for (status, expected_label) in [
            (GoalDisplayStatus::UserPaused, "Paused"),
            (GoalDisplayStatus::BackOffPaused, "Paused (back-off)"),
            (GoalDisplayStatus::NoProgressPaused, "Paused (no progress)"),
            (GoalDisplayStatus::InfraPaused, "Paused (error)"),
            (GoalDisplayStatus::Blocked, "Paused (verification blocked)"),
        ] {
            goal.status = status;
            let (s, c, p) = status_label(&goal);
            assert_eq!(s, expected_label, "label for {status:?}");
            assert_eq!(c, theme.warning, "color for {status:?}");
            assert_eq!(p, "", "phase_text for {status:?}");
        }

        goal.status = GoalDisplayStatus::Failed;
        let (s, color, _) = status_label(&goal);
        assert_eq!(s, "Failed");
        assert_eq!(color, theme.accent_error);

        goal.status = GoalDisplayStatus::BudgetLimited;
        let (s, _, _) = status_label(&goal);
        assert_eq!(s, "Budget Limited");

        goal.status = GoalDisplayStatus::Complete;
        let (s, _, _) = status_label(&goal);
        assert_eq!(s, "Complete");
    }

    /// Walk the buffer and collect every cell's symbol into a single string,
    /// joining rows with `\n`. Used to assert that rendered text contains
    /// specific user-visible substrings.
    fn buffer_text(buf: &ratatui::buffer::Buffer) -> String {
        let area = buf.area;
        let mut out = String::new();
        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                if let Some(cell) = buf.cell(ratatui::layout::Position::new(x, y)) {
                    out.push_str(cell.symbol());
                }
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn render_paused_user_shows_resume_hint() {
        let screen = Rect::new(0, 0, 100, 30);
        let mut buf = ratatui::buffer::Buffer::empty(screen);
        let mut goal = make_goal();
        goal.status = GoalDisplayStatus::UserPaused;
        let area = goal_detail_area(screen, &goal, &[]);
        render_goal_detail(&mut buf, area, &goal, &[], 0, None, 0, false);

        let text = buffer_text(&buf);
        assert!(text.contains("Type /goal resume to continue"));
    }

    #[test]
    fn render_active_has_no_resume_hint() {
        let screen = Rect::new(0, 0, 100, 30);
        let mut buf = ratatui::buffer::Buffer::empty(screen);
        let goal = make_goal();
        let area = goal_detail_area(screen, &goal, &[]);
        render_goal_detail(&mut buf, area, &goal, &[], 0, None, 0, false);

        let text = buffer_text(&buf);
        assert!(!text.contains("Type /goal resume to continue"));
    }

    #[test]
    fn commands_hint_includes_resume() {
        let screen = Rect::new(0, 0, 100, 30);
        let mut buf = ratatui::buffer::Buffer::empty(screen);
        let goal = make_goal();
        let area = goal_detail_area(screen, &goal, &[]);
        render_goal_detail(&mut buf, area, &goal, &[], 0, None, 0, false);

        let text = buffer_text(&buf);
        assert!(
            text.contains("/goal resume"),
            "commands hint should advertise /goal resume, got:\n{text}"
        );
    }

    #[test]
    fn render_infra_paused_with_pause_message_shows_reason_line() {
        let screen = Rect::new(0, 0, 100, 30);
        let mut buf = ratatui::buffer::Buffer::empty(screen);
        let mut goal = make_goal();
        goal.status = GoalDisplayStatus::InfraPaused;
        goal.pause_message = Some("Turn failed: upstream unavailable".into());
        let area = goal_detail_area(screen, &goal, &[]);
        render_goal_detail(&mut buf, area, &goal, &[], 0, None, 0, false);

        let text = buffer_text(&buf);
        assert!(
            text.contains("Paused (error)"),
            "modal must show the InfraPaused status label, got:\n{text}"
        );
        assert!(
            text.contains("Reason: Turn failed: upstream unavailable"),
            "modal must render the pause_message text, got:\n{text}"
        );
        assert!(
            text.contains("Type /goal resume to continue"),
            "InfraPaused is paused so the resume hint must render, got:\n{text}"
        );
    }

    #[test]
    fn render_blocked_shows_reason_and_resume_hint() {
        let screen = Rect::new(0, 0, 100, 30);
        let mut buf = ratatui::buffer::Buffer::empty(screen);
        let mut goal = make_goal();
        goal.status = GoalDisplayStatus::Blocked;
        goal.pause_message = Some("no windows sdk".into());
        let area = goal_detail_area(screen, &goal, &[]);
        render_goal_detail(&mut buf, area, &goal, &[], 0, None, 0, false);

        let text = buffer_text(&buf);
        assert!(
            text.contains("Paused (verification blocked)"),
            "modal must show the Blocked status label, got:\n{text}"
        );
        assert!(
            text.contains("Reason: no windows sdk"),
            "modal must render the pause_message text, got:\n{text}"
        );
        assert!(
            text.contains("Type /goal resume to continue"),
            "Blocked is paused so the resume hint must render, got:\n{text}"
        );
    }

    #[test]
    fn render_blocked_without_pause_message_omits_reason_line() {
        // Defensive: if the shell sends `status: "blocked"` but no
        // pause_message (e.g. via a forward-compat raw update), the
        // modal should not crash and should not render a Reason line.
        let screen = Rect::new(0, 0, 100, 30);
        let mut buf = ratatui::buffer::Buffer::empty(screen);
        let mut goal = make_goal();
        goal.status = GoalDisplayStatus::Blocked;
        goal.pause_message = None;
        let area = goal_detail_area(screen, &goal, &[]);
        render_goal_detail(&mut buf, area, &goal, &[], 0, None, 0, false);

        let text = buffer_text(&buf);
        assert!(
            !text.contains("Reason:"),
            "no pause_message means no Reason line, got:\n{text}"
        );
    }

    #[test]
    fn render_non_paused_with_stale_pause_message_omits_reason_line() {
        // Defence in depth. Even if a buggy shell emitted
        // `pause_message: Some(...)` alongside a non-paused status
        // (guaranteed not to happen today, but a future regression on
        // the shell side must not corrupt the modal), the renderer gates
        // on `is_paused()` and skips the Reason block.
        for status in [
            GoalDisplayStatus::Active,
            GoalDisplayStatus::Complete,
            GoalDisplayStatus::BudgetLimited,
        ] {
            let screen = Rect::new(0, 0, 100, 30);
            let mut buf = ratatui::buffer::Buffer::empty(screen);
            let mut goal = make_goal();
            goal.status = status;
            goal.pause_message = Some("stale reason".into());
            let area = goal_detail_area(screen, &goal, &[]);
            render_goal_detail(&mut buf, area, &goal, &[], 0, None, 0, false);

            let text = buffer_text(&buf);
            assert!(
                !text.contains("Reason:"),
                "non-paused status {status:?} must never render a Reason: line even with a stale pause_message, got:\n{text}"
            );
        }
    }

    #[test]
    fn render_blocked_reason_line_appears_after_pause_hint() {
        // Modal visual order: Status → "Type /goal resume" pause hint →
        // "Reason: ...". Pin both the presence of each line and their
        // relative position so a future refactor that reorders the
        // render blocks gets caught.
        let screen = Rect::new(0, 0, 100, 30);
        let mut buf = ratatui::buffer::Buffer::empty(screen);
        let mut goal = make_goal();
        goal.status = GoalDisplayStatus::Blocked;
        goal.pause_message = Some("no windows sdk".into());
        let area = goal_detail_area(screen, &goal, &[]);
        render_goal_detail(&mut buf, area, &goal, &[], 0, None, 0, false);

        let text = buffer_text(&buf);
        let hint_pos = text
            .find("Type /goal resume to continue")
            .expect("pause hint must render");
        let reason_pos = text
            .find("Reason: no windows sdk")
            .expect("reason line must render");
        assert!(
            reason_pos > hint_pos,
            "Reason block must render AFTER the pause hint:\n hint_pos={hint_pos}, reason_pos={reason_pos}\n{text}"
        );
    }

    #[test]
    fn goal_detail_area_grows_for_multi_line_pause_message() {
        // pause_message that spans multiple wrapped rows must enlarge
        // the modal so the wrapped content fits without truncation.
        let screen = Rect::new(0, 0, 80, 40);
        let mut goal = make_goal();
        goal.status = GoalDisplayStatus::UserPaused;
        goal.pause_message = None;
        let baseline = goal_detail_area(screen, &goal, &[]).height;

        goal.pause_message = Some(
            "the user requested a windows binary but the agent runs \
             on macOS and cannot cross-compile to PE/COFF without a \
             toolchain that is not present in this environment"
                .into(),
        );
        let grown = goal_detail_area(screen, &goal, &[]).height;
        assert!(
            grown > baseline,
            "multi-line pause_message must extend the modal height: \
             baseline={baseline}, grown={grown}"
        );
    }

    #[test]
    fn wrap_pause_message_lines_respects_width() {
        use unicode_width::UnicodeWidthStr;
        // Each wrapped row must be at most `width` *terminal columns*
        // wide so it fits inside the modal's inner content rect without
        // overflow into the right border. Pin to width-not-char-count
        // measurement so future refactors can't quietly regress to a
        // chars().count() comparison and start mis-wrapping wide text.
        let text = "alpha beta gamma delta epsilon zeta eta theta iota kappa";
        for width in [10u16, 20, 40] {
            for line in wrap_pause_message_lines(text, width) {
                assert!(
                    UnicodeWidthStr::width(line.as_str()) <= width as usize,
                    "row {line:?} wider than width={width}"
                );
            }
        }
    }

    #[test]
    fn wrap_pause_message_lines_preserves_explicit_newlines() {
        // The shell concatenates `blocked_reason\nmessage`; the wrap
        // must keep them as separate paragraphs so the rendered modal
        // preserves the structural break between the short label and
        // the long body.
        let text = "short reason\nlonger body content";
        let lines = wrap_pause_message_lines(text, 80);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "short reason");
        assert_eq!(lines[1], "longer body content");
    }

    #[test]
    fn wrap_pause_message_lines_degenerate_width_strips_newline() {
        // At unusable width (0/1) the wrapper returns a single un-split row;
        // the kept `\n` must still collapse so the row carries no control byte.
        let out = wrap_pause_message_lines("a\nb", 1);
        assert_eq!(out, vec!["a b".to_string()]);
        assert!(!out[0].contains('\n'));
    }

    #[test]
    fn wrap_pause_message_lines_hard_splits_overlong_tokens() {
        use unicode_width::UnicodeWidthStr;
        // A single 36-char URL/path with width=10 must hard-split into
        // ≤10-column chunks rather than overflowing or returning a
        // single overlong row.
        let text = "abcdefghijklmnopqrstuvwxyz0123456789";
        let lines = wrap_pause_message_lines(text, 10);
        for line in &lines {
            assert!(
                UnicodeWidthStr::width(line.as_str()) <= 10,
                "hard-split row {line:?} wider than 10"
            );
        }
        assert!(lines.len() >= 4);
    }

    #[test]
    fn wrap_pause_message_lines_uses_display_width_for_cjk() {
        use unicode_width::UnicodeWidthStr;
        // Each Han ideograph occupies 2 terminal columns. A 10-char
        // CJK string is 20 columns wide and must NOT fit on a single
        // width=10 row — the chars().count() shape (10) would have
        // mis-wrapped this and silently overflowed the modal border.
        // Use Han chars from a separate word so wrapping isn't blocked
        // by the hard-split path.
        let text = "你好世界 再見天空";
        for line in wrap_pause_message_lines(text, 10) {
            assert!(
                UnicodeWidthStr::width(line.as_str()) <= 10,
                "CJK row {line:?} wider than 10 columns",
            );
        }
        // A single 8-char CJK token at width=10 must hard-split (each
        // char = 2 cols, so 5 chars = 10 cols max per row).
        let single_token = "你好世界再見天空";
        let chunks = wrap_pause_message_lines(single_token, 10);
        assert!(chunks.len() >= 2);
        for line in &chunks {
            assert!(
                UnicodeWidthStr::width(line.as_str()) <= 10,
                "CJK hard-split row {line:?} wider than 10"
            );
        }
    }

    #[test]
    fn wrap_pause_message_lines_uses_display_width_for_emoji() {
        use unicode_width::UnicodeWidthStr;
        // Most emoji occupy 2 terminal columns. At width=6 a string of
        // emojis must hard-split into chunks of at most 3 emoji each.
        // (chars().count() would have allowed 6 emoji = 12 columns and
        // overflowed.)
        let text = "🎉🎊🎈🍰🎁🎀";
        for line in wrap_pause_message_lines(text, 6) {
            assert!(
                UnicodeWidthStr::width(line.as_str()) <= 6,
                "emoji row {line:?} wider than 6 columns",
            );
        }
    }

    // -- Todo rendering tests -----------------------------------------------

    fn make_todo(content: &str, status: TodoStatus) -> TodoItem {
        TodoItem {
            content: content.to_owned(),
            priority: Default::default(),
            status,
            meta: None,
        }
    }

    #[test]
    fn render_with_todo_items_does_not_panic() {
        let screen = Rect::new(0, 0, 100, 30);
        let mut buf = ratatui::buffer::Buffer::empty(screen);
        let goal = make_goal();
        let todos = vec![
            make_todo("Setup project", TodoStatus::Completed),
            make_todo("Implement feature", TodoStatus::InProgress),
            make_todo("Write tests", TodoStatus::Pending),
            make_todo("Cancelled task", TodoStatus::Cancelled),
        ];
        let area = goal_detail_area(screen, &goal, &todos);
        render_goal_detail(&mut buf, area, &goal, &todos, 0, None, 0, false);

        let text = buffer_text(&buf);
        assert!(text.contains("Progress:"), "must render progress header");
        assert!(
            text.contains("Setup project"),
            "must render todo content, got:\n{text}"
        );
    }

    #[test]
    fn render_with_todos_shows_status_icons() {
        let screen = Rect::new(0, 0, 100, 30);
        let mut buf = ratatui::buffer::Buffer::empty(screen);
        let goal = make_goal();
        let todos = vec![
            make_todo("done", TodoStatus::Completed),
            make_todo("wip", TodoStatus::InProgress),
            make_todo("todo", TodoStatus::Pending),
            make_todo("skip", TodoStatus::Cancelled),
        ];
        let area = goal_detail_area(screen, &goal, &todos);
        render_goal_detail(&mut buf, area, &goal, &todos, 0, None, 0, false);

        let text = buffer_text(&buf);
        // Completed = ✓, InProgress = ▶, Pending = □, Cancelled = ✗
        // Match icon + content to disambiguate from the close button [✗].
        assert!(text.contains("\u{2713} done"), "missing ✓ for Completed");
        assert!(text.contains("\u{25b6} wip"), "missing ▶ for InProgress");
        assert!(text.contains("\u{25a1} todo"), "missing □ for Pending");
        assert!(text.contains("\u{2717} skip"), "missing ✗ for Cancelled");
    }

    #[test]
    fn render_with_overflow_shows_plus_n_more() {
        let screen = Rect::new(0, 0, 100, 50);
        let mut buf = ratatui::buffer::Buffer::empty(screen);
        let goal = make_goal();
        let todos: Vec<TodoItem> = (0..20)
            .map(|i| make_todo(&format!("Task {i}"), TodoStatus::Pending))
            .collect();
        let area = goal_detail_area(screen, &goal, &todos);
        render_goal_detail(&mut buf, area, &goal, &todos, 0, None, 0, false);

        let text = buffer_text(&buf);
        assert!(
            text.contains("+5 more"),
            "must show overflow for 20 items (cap=15), got:\n{text}"
        );
    }

    #[test]
    fn goal_detail_area_height_includes_todo_items() {
        let screen = Rect::new(0, 0, 120, 50);
        let goal = make_goal();
        let baseline = goal_detail_area(screen, &goal, &[]).height;
        let todos = vec![
            make_todo("A", TodoStatus::Pending),
            make_todo("B", TodoStatus::Completed),
            make_todo("C", TodoStatus::InProgress),
        ];
        // 3 items + header = 4 lines, vs 1 line for empty ("No progress items yet").
        // Net growth = 3.
        let with_todos = goal_detail_area(screen, &goal, &todos).height;
        assert_eq!(with_todos, baseline + 3);
    }

    #[test]
    fn truncate_to_width_ascii() {
        assert_eq!(truncate_to_width("short", 100), "short");
        assert_eq!(truncate_to_width("hello world", 5), "hell\u{2026}");
    }

    #[test]
    fn truncate_to_width_cjk() {
        use unicode_width::UnicodeWidthStr;
        // Each CJK char = 2 cols. "你好世界" = 8 cols. Budget 5 → 2 chars (4 cols) + ellipsis.
        let result = truncate_to_width("你好世界", 5);
        assert!(
            UnicodeWidthStr::width(result.as_str()) <= 5,
            "truncated CJK {result:?} wider than 5"
        );
        assert!(result.ends_with('\u{2026}'));
    }

    // -- objective in the modal title ---------------------------------------

    #[test]
    fn modal_title_renders_objective() {
        // The objective must appear in the modal title (top border) so the
        // user can see which goal is running — not a static placeholder.
        let goal = make_goal(); // objective = "Implement dark mode"
        let text = render_to_text(&goal);
        assert!(
            text.contains("Implement dark mode"),
            "title must show the objective, got:\n{text}"
        );
        assert!(
            !text.contains("Active Goal"),
            "static placeholder title must be replaced by the objective, got:\n{text}"
        );
    }

    #[test]
    fn modal_title_truncates_long_objective_with_ellipsis() {
        // A long objective must be truncated (with an ellipsis) so it cannot
        // collide with the close button or overflow the border.
        let mut goal = make_goal();
        goal.objective = "x".repeat(300);
        let text = render_to_text(&goal);
        assert!(
            !text.contains(&"x".repeat(300)),
            "overlong objective must be truncated, got:\n{text}"
        );
        assert!(
            text.contains('\u{2026}'),
            "truncation ellipsis must render in the title, got:\n{text}"
        );
    }

    #[test]
    fn modal_title_truncates_wide_objective_by_display_width() {
        // A wide-glyph objective must be truncated by DISPLAY WIDTH so it
        // can't overflow the title columns into the close button / border;
        // the close button must survive.
        let mut goal = make_goal();
        goal.objective = "宽".repeat(120); // 240 display columns
        let screen = Rect::new(0, 0, 100, 40);
        let mut buf = ratatui::buffer::Buffer::empty(screen);
        let area = goal_detail_area(screen, &goal, &[]);
        render_goal_detail(&mut buf, area, &goal, &[], 0, None, 0, false);
        let text = buffer_text(&buf);
        assert!(
            !text.contains(&"宽".repeat(120)),
            "overlong wide objective must be truncated by display width"
        );
        assert!(text.contains('\u{2026}'), "ellipsis must render");
        assert!(
            text.contains(crate::glyphs::ballot_x()) || text.contains("[x]"),
            "the close button must still render alongside a wide title"
        );
    }

    // -- commands hint must not be clipped ----------------------------------

    #[test]
    fn commands_hint_visible_without_subagent_or_history() {
        // With no active subagent and no recent-history event, the height
        // calc and render must agree on the conditional blank separators so
        // the commands hint isn't clipped.
        let screen = Rect::new(0, 0, 100, 40);
        let mut goal = make_goal();
        goal.current_subagent_role = None;
        goal.live_subagent_tokens = None;
        goal.live_context_pct = None;
        goal.live_turn_count = None;
        goal.live_tool_call_count = None;
        goal.live_tokens_by_model = Vec::new();
        goal.last_event = None;
        goal.last_event_detail = None;
        goal.last_event_timestamp = None;
        goal.classifier_runs_attempted = None;
        goal.classifier_max_runs = None;
        goal.last_classifier_verdict = None;
        goal.last_classifier_details_path = None;

        let area = goal_detail_area(screen, &goal, &[]);
        let mut buf = ratatui::buffer::Buffer::empty(screen);
        render_goal_detail(&mut buf, area, &goal, &[], 0, None, 0, false);
        let text = buffer_text(&buf);
        assert!(
            text.contains("Esc: close"),
            "commands hint must render (not clipped) with no subagent/history, got:\n{text}"
        );
    }

    // -- details path existence check ---------------------------------------

    #[test]
    fn classifier_details_display_handles_missing_present_and_none() {
        // Existence is a precomputed bool, so the display is pure: no path →
        // hyphen; path + !exists → "(unavailable)"; path + exists → the path.
        assert_eq!(classifier_details_display(None, false), "-");
        assert_eq!(
            classifier_details_display(Some("/no/such/path/zzz-details.md"), false),
            "(unavailable)"
        );
        assert_eq!(
            classifier_details_display(Some("/tmp/exists.md"), true),
            "/tmp/exists.md"
        );
    }

    #[test]
    fn modal_details_row_shows_unavailable_for_missing_file() {
        // A reported path whose cached existence is false (fail-open may not
        // have written it) must render "(unavailable)" not a dangling path.
        let mut goal = make_goal(); // make_goal default: last_classifier_details_exists = false
        goal.last_classifier_verdict = Some(GoalClassifierVerdict::Achieved);
        goal.last_classifier_details_path = Some("/no/such/path/zzz-details.md".into());
        let text = render_to_text(&goal);
        assert!(
            text.contains("Details: (unavailable)"),
            "missing details file must render (unavailable), got:\n{text}"
        );
        assert!(
            !text.contains("/no/such/path/zzz-details.md"),
            "the dangling path must not render, got:\n{text}"
        );
    }

    // -- Attempts hyphen branch --------------------------------------------

    #[test]
    fn modal_attempts_shows_hyphen_when_classifier_active_without_counts() {
        // Completion review renders (a verdict is present) but no run counter
        // has arrived (both counts absent) → "Attempts: —", never "Attempts: ".
        let mut goal = make_goal();
        goal.classifier_runs_attempted = None;
        goal.classifier_max_runs = None;
        goal.last_classifier_verdict = Some(GoalClassifierVerdict::NotAchieved);
        let text = render_to_text(&goal);
        assert!(
            text.contains("Completion review:"),
            "section must render, got:\n{text}"
        );
        assert!(
            text.contains("Attempts: -"),
            "empty counter must fall back to a hyphen, got:\n{text}"
        );
    }

    // -- Recent-History humanization ----------------------------------------

    #[test]
    fn humanize_goal_event_maps_wire_vocabulary() {
        assert_eq!(
            humanize_goal_event("goal_paused", Some("back_off")),
            "Paused: back off"
        );
        // A plain user pause shows no redundant cause.
        assert_eq!(humanize_goal_event("goal_paused", Some("user")), "Paused");
        assert_eq!(humanize_goal_event("goal_paused", None), "Paused");
        assert_eq!(
            humanize_goal_event("premature_stop_detected", Some("giving_up")),
            "Stopped early: giving up"
        );
        assert_eq!(humanize_goal_event("goal_completed", None), "Completed");
        assert_eq!(
            humanize_goal_event("budget_exceeded", None),
            "Budget exceeded"
        );
        // Forward-compat: an unmapped event de-snakes + capitalizes.
        assert_eq!(
            humanize_goal_event("some_future_event", None),
            "Some future event"
        );
    }

    #[test]
    fn humanize_event_timestamp_relative_and_fallback() {
        assert_eq!(humanize_event_timestamp(""), "");
        // Non-RFC3339 (legacy / fixture) returned verbatim.
        assert_eq!(humanize_event_timestamp("1m ago"), "1m ago");
        // A fresh RFC3339 stamp → "just now"; ~2h ago → "2h ago".
        let now = chrono::Utc::now().to_rfc3339();
        assert_eq!(humanize_event_timestamp(&now), "just now");
        let past = (chrono::Utc::now() - chrono::Duration::hours(2)).to_rfc3339();
        assert_eq!(humanize_event_timestamp(&past), "2h ago");
    }

    #[test]
    fn recent_history_renders_humanized_event_and_relative_time() {
        // The modal must NOT show raw machine vocabulary — event names or
        // RFC3339 stamps — only humanized text.
        let mut goal = make_goal();
        goal.last_event = Some("goal_paused".into());
        goal.last_event_detail = Some("back_off".into());
        goal.last_event_timestamp =
            Some((chrono::Utc::now() - chrono::Duration::minutes(2)).to_rfc3339());
        let text = render_to_text(&goal);
        assert!(
            text.contains("Paused: back off"),
            "humanized event label, got:\n{text}"
        );
        assert!(text.contains("2m ago"), "relative time, got:\n{text}");
        assert!(
            !text.contains("goal_paused"),
            "raw event name must not render, got:\n{text}"
        );
        assert!(
            !text.contains("back_off"),
            "raw snake_case detail must not render, got:\n{text}"
        );
    }

    #[test]
    fn humanizers_strip_control_chars_independently() {
        // Defense-in-depth: the humanizer's variable passthroughs (folded
        // detail, unknown-event name, verbatim-timestamp fallback) must strip
        // control bytes THEMSELVES — not rely on ratatui's filter — so a
        // ratatui change can't leak ESC/newline into a rendered row.
        let label = humanize_goal_event("goal_paused", Some("doom\u{1b}[31m_loop\n\t"));
        assert!(
            !label.chars().any(|c| c.is_control()),
            "folded detail leaked a control char: {label:?}"
        );
        let unknown = humanize_goal_event("weird\u{7}_event\n", None);
        assert!(
            !unknown.chars().any(|c| c.is_control()),
            "unknown-event name leaked a control char: {unknown:?}"
        );
        // Non-RFC3339 → verbatim fallback, still stripped.
        let ts = humanize_event_timestamp("2026\u{1b}]0;evil\u{7}");
        assert!(
            !ts.chars().any(|c| c.is_control()),
            "timestamp fallback leaked a control char: {ts:?}"
        );
    }

    #[test]
    fn pause_reason_wrapped_lines_have_no_control_bytes() {
        // The pause-reason row strips control bytes too. `\n` is kept (the
        // wrapper splits on it) but consumed by wrapping, so no wrapped line
        // carries a control byte. Discriminating (no ratatui): reverting the
        // strip leaves ESC/BEL/tab in the lines.
        let msg = "blocked\u{1b}[31m here\nsecond\tline\u{7}\r";
        for line in wrap_pause_message_lines(&format_pause_reason(msg), 40) {
            assert!(
                !line.chars().any(|c| c.is_control()),
                "wrapped reason line leaked a control char: {line:?}"
            );
        }
        // The kept newline still produces the two-paragraph structure.
        assert!(
            wrap_pause_message_lines(&format_pause_reason(msg), 40).len() >= 2,
            "the preserved newline must still split the reason into paragraphs"
        );
    }

    #[test]
    fn recent_history_render_emits_no_control_bytes() {
        // End-to-end SMOKE check only — it reads cells AFTER ratatui's own
        // control filter, so it passes even if the strip is reverted. The
        // discriminating ("killing") test is
        // `humanizers_strip_control_chars_independently`, which asserts on the
        // humanizer output directly (before ratatui).
        let mut goal = make_goal();
        goal.last_event = Some("goal_paused".into());
        goal.last_event_detail = Some("doom\u{1b}[31m_loop\n\t".into());
        goal.last_event_timestamp = Some("2026-01-01\u{1b}]0;evil\u{7}".into());
        let text = render_to_text(&goal);
        for line in text.lines() {
            assert!(
                !line.chars().any(|c| c.is_control()),
                "no control char may reach a rendered row: {line:?}"
            );
        }
    }

    // -- title control-char / boundary handling -----------------------------

    #[test]
    fn modal_title_collapses_control_chars_to_one_row() {
        // A newline in the objective must be collapsed to a space so the whole
        // objective stays on the single-row title (the bare `\n` is zero-width
        // to truncation and would otherwise leak into the border row).
        let mut goal = make_goal();
        goal.objective = "line one\nline two".into();
        let text = render_to_text(&goal);
        assert!(
            text.contains("line one line two"),
            "newline must collapse to a space on one row, got:\n{text}"
        );
    }

    #[test]
    fn modal_title_blank_objective_falls_back_to_active_goal() {
        let mut goal = make_goal();
        goal.objective = "  \n\t  ".into(); // whitespace/control only
        let text = render_to_text(&goal);
        assert!(
            text.contains("Active Goal"),
            "a blank objective must fall back to the placeholder, got:\n{text}"
        );
    }

    #[test]
    fn truncate_to_width_boundaries() {
        // Exactly at budget → unchanged (no ellipsis).
        assert_eq!(truncate_to_width("abcd", 4), "abcd");
        // One column over → truncated with the ellipsis.
        assert_eq!(truncate_to_width("abcde", 4), "abc\u{2026}");
        // Zero-width combining marks don't consume the budget.
        let combining = "a\u{0301}b\u{0301}"; // 2 display columns
        assert_eq!(
            unicode_width::UnicodeWidthStr::width(combining),
            2,
            "combining marks are zero-width"
        );
        assert_eq!(truncate_to_width(combining, 5), combining);
        // Degenerate budget 0 → just the ellipsis (no codepoint dropped silently).
        assert_eq!(truncate_to_width("x", 0), "\u{2026}");
    }

    // -- subagent / classifier height combos --------------------------------

    #[test]
    fn subagent_just_spawned_budgets_no_detail_row() {
        // A subagent with no live metrics yet renders blank + role (2 rows, no
        // detail row); the budget must match — NOT over-count to 3.
        let screen = Rect::new(0, 0, 100, 40);
        let mut goal = make_goal();
        goal.current_subagent_role = Some("Worker".into());
        goal.live_subagent_tokens = None;
        goal.live_context_pct = None;
        goal.live_turn_count = None;
        goal.live_tool_call_count = None;
        goal.live_tokens_by_model = Vec::new();
        let no_detail_h = goal_detail_area(screen, &goal, &[]).height;

        goal.live_turn_count = Some(3);
        let with_detail_h = goal_detail_area(screen, &goal, &[]).height;
        assert_eq!(
            with_detail_h,
            no_detail_h + 1,
            "the first live metric adds exactly the detail row (no over-count when absent)"
        );

        // The no-detail case must still render the commands hint (no clip).
        goal.live_turn_count = None;
        let area = goal_detail_area(screen, &goal, &[]);
        let mut buf = ratatui::buffer::Buffer::empty(screen);
        render_goal_detail(&mut buf, area, &goal, &[], 0, None, 0, false);
        assert!(
            buffer_text(&buf).contains("Esc: close"),
            "commands hint must render for a just-spawned subagent"
        );
    }

    #[test]
    fn goal_detail_area_height_accounts_for_completion_review() {
        let screen = Rect::new(0, 0, 100, 40);
        let mut goal = make_goal();
        goal.classifier_runs_attempted = None;
        goal.classifier_max_runs = None;
        goal.last_classifier_verdict = None;
        goal.last_classifier_details_path = None;
        let without = goal_detail_area(screen, &goal, &[]).height;

        // Completion review = blank + header + 3 content lines = 5.
        goal.classifier_max_runs = Some(3);
        let with = goal_detail_area(screen, &goal, &[]).height;
        assert_eq!(with, without + 5, "completion review adds exactly 5 rows");

        // And it renders fully alongside the commands hint (no clip).
        let area = goal_detail_area(screen, &goal, &[]);
        let mut buf = ratatui::buffer::Buffer::empty(screen);
        render_goal_detail(&mut buf, area, &goal, &[], 0, None, 0, false);
        let t = buffer_text(&buf);
        assert!(t.contains("Completion review:") && t.contains("Esc: close"));
    }
}
