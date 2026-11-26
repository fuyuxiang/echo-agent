//! `/btw` side question inline panel.
//!
//! Renders as a compact bordered panel above the prompt input box,
//! below the scrollback. Shows the question and a loading indicator
//! while the response is in-flight. Once the response arrives the
//! panel stays on screen until the user presses Esc, at which point
//! the content is persisted to scrollback as a collapsed `BtwBlock`.

use crate::render::SafeBuf;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Widget};
use unicode_width::UnicodeWidthStr;

use crate::render::osc8::{LinkOverlay, scan_lines_for_url_overlays};
use crate::scrollback::blocks::markdown_content::MarkdownContent;
use crate::scrollback::render::map_hyperlinks_to_overlay;
use crate::scrollback::text_selection::{
    ResolvedSelectableLine, ResolvedSelectionModel, VisibleBlockGeometry,
};
use crate::theme::Theme;

/// Synthetic entry index for btw overlay selection (never collides with real scrollback).
pub const BTW_OVERLAY_ENTRY_IDX: usize = usize::MAX;
const BTW_OVERLAY_RANGE_ID: u16 = 0;

/// State of the /btw inline panel.
#[derive(Debug, Clone)]
pub enum BtwOverlayState {
    /// Waiting for the shell to respond.
    Loading { question: String },
    /// Response received — stays on screen until Esc.
    Done {
        question: String,
        /// Rendered markdown content (same renderer as regular agent messages).
        /// Boxed to keep the enum small (`MarkdownContent` is large).
        content: Box<MarkdownContent>,
        /// Line offset for scrolling through long responses.
        scroll_offset: usize,
    },
    /// Request failed (shown until user presses Esc).
    Error { question: String, error: String },
}

impl BtwOverlayState {
    /// Build a `Done` state, rendering `response` as markdown via the same
    /// [`MarkdownContent`] renderer used for regular agent messages so the
    /// inline panel shows formatted tables, headings, lists, etc.
    pub fn done(question: String, response: String) -> Self {
        Self::Done {
            question,
            content: Box::new(MarkdownContent::new(response)),
            scroll_offset: 0,
        }
    }

    pub fn question(&self) -> &str {
        match self {
            Self::Loading { question }
            | Self::Done { question, .. }
            | Self::Error { question, .. } => question,
        }
    }

    /// Scroll the Done response up by `n` lines. No-op for other states.
    pub fn scroll_up(&mut self, n: usize) {
        if let Self::Done { scroll_offset, .. } = self {
            *scroll_offset = scroll_offset.saturating_sub(n);
        }
    }

    /// Scroll the Done response down by `n` lines, clamped to `max_offset`.
    pub fn scroll_down(&mut self, n: usize, max_offset: usize) {
        if let Self::Done { scroll_offset, .. } = self {
            *scroll_offset = (*scroll_offset + n).min(max_offset);
        }
    }

    /// Current scroll offset (0 for non-Done states).
    pub fn scroll_offset(&self) -> usize {
        match self {
            Self::Done { scroll_offset, .. } => *scroll_offset,
            _ => 0,
        }
    }

    /// Max scroll offset for the Done response at `content_width`.
    /// Returns 0 if the response fits within `max_body_lines`.
    pub fn max_scroll_offset(&self, content_width: usize, max_body_lines: usize) -> usize {
        match self {
            Self::Done { content, .. } => {
                if content_width == 0 {
                    return 0;
                }
                let total = content.with_wrapped_lines(content_width, |w| w.lines.len());
                total.saturating_sub(max_body_lines)
            }
            _ => 0,
        }
    }

    pub fn full_selection_model(&self, content_width: usize) -> ResolvedSelectionModel {
        let mut model = ResolvedSelectionModel::default();
        let Self::Done { content, .. } = self else {
            return model;
        };
        if content_width == 0 {
            return model;
        }
        content.with_wrapped_lines(content_width, |wrapped| {
            for (idx, (line, joiner)) in
                wrapped.lines.iter().zip(wrapped.joiners.iter()).enumerate()
            {
                let text = line_plain_text(line);
                // The markdown wrapper emits `None` for the first piece of each
                // source line (reconstruct treats that as a newline) and
                // `Some(" ")` for soft-wrapped continuations.
                let joiner_to_previous = if idx == 0 { None } else { joiner.clone() };
                model.push_line(ResolvedSelectableLine {
                    entry_idx: BTW_OVERLAY_ENTRY_IDX,
                    range_id: BTW_OVERLAY_RANGE_ID,
                    block_line_idx: idx,
                    screen_y: 0,
                    screen_x: 0,
                    selectable_cols: 0..crate::scrollback::types::str_display_cells(&text) as u16,
                    text,
                    painted_region: None,
                    joiner_to_previous,
                });
            }
        });
        model
    }
}

/// Show each spinner frame for this many animation ticks.
const SPINNER_DIVISOR: u64 = 4;

/// Maximum body lines shown for a Done response.
pub const DONE_MAX_BODY_LINES: u16 = 12;

/// Concatenate a line's span contents into plain text (styles stripped).
///
/// Used to build the selection model from rendered markdown lines.
fn line_plain_text(line: &Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// Wrap an error message to `content_width` columns, capped at `max_lines`
/// with an ellipsis on the last line when cut. The caller passes the rows it
/// can actually paint, so the truncation marker lands on a visible row even
/// when the layout hands the panel a shorter rect than it asked for.
fn wrapped_error_lines(error: &str, content_width: usize, max_lines: usize) -> Vec<String> {
    if content_width == 0 {
        return vec![String::new()];
    }
    let mut lines: Vec<String> = textwrap::wrap(error.trim(), content_width)
        .into_iter()
        .map(|l| l.into_owned())
        .collect();
    if lines.len() > max_lines {
        lines.truncate(max_lines);
        if let Some(last) = lines.last_mut() {
            // Make room: a full-width last line + '…' would exceed
            // `content_width` and the renderer would cut the ellipsis off.
            while last.width() >= content_width {
                last.pop();
            }
            last.push('\u{2026}');
        }
    }
    lines
}

/// Compute the desired height of the btw inline panel.
///
/// Returns 0 when there is nothing to show (state is `None`).
/// Loading = 3 rows (top border + 1 body + bottom border).
/// Done / Error = 2 (borders) + min(wrapped lines, DONE_MAX_BODY_LINES).
///
/// `panel_width` is the full panel width (`render_btw_panel`'s `area.width`);
/// body text gets `panel_width - 4` (border + padding), matching the render.
pub fn btw_panel_height(state: Option<&BtwOverlayState>, panel_width: u16) -> u16 {
    let cw = panel_width.saturating_sub(4) as usize; // border + pad
    match state {
        None => 0,
        Some(BtwOverlayState::Loading { .. }) => 3,
        Some(BtwOverlayState::Error { error, .. }) => {
            2 + wrapped_error_lines(error, cw, DONE_MAX_BODY_LINES as usize).len() as u16
        }
        Some(BtwOverlayState::Done { content, .. }) => {
            let total = if cw > 0 {
                content.with_wrapped_lines(cw, |w| w.lines.len())
            } else {
                1
            };
            let body = total.clamp(1, DONE_MAX_BODY_LINES as usize) as u16;
            2 + body // top border + body + bottom border
        }
    }
}

/// Render the /btw inline panel into the given rect.
///
/// The panel renders as a compact bordered box with the question in the
/// top border and the status in the body. It sits in the normal layout
/// flow (above queue / turn status / prompt).
///
/// When `link_overlay` is `Some`, markdown hyperlinks in the Done body are
/// mapped into screen-space overlay links (same path as scrollback) so OSC 8
/// and click-to-open work inside the panel.
#[allow(clippy::too_many_arguments)]
pub fn render_btw_panel(
    buf: &mut Buffer,
    state: &BtwOverlayState,
    area: Rect,
    tick: u64,
    focused: bool,
    hit_close: Option<&mut crate::app::agent_view::HitArea>,
    selection_model: &mut ResolvedSelectionModel,
    link_overlay: Option<&mut LinkOverlay>,
    // Generated-media paths for resolving relative file-path link targets.
    media_paths: &[std::path::PathBuf],
    // Session cwd for resolving relative markdown link targets to on-disk files.
    cwd: Option<&std::path::Path>,
) {
    if area.width < 12 || area.height < 3 {
        return;
    }
    let theme = Theme::current();
    let bg = theme.bg_base;

    let content_x = area.x + 2;
    let content_width = area.width.saturating_sub(4) as usize;
    if content_width == 0 {
        return;
    }

    // Only show focus (accent ring + ↑↓ hint) when there's actually something to
    // scroll; `max_scroll_offset` is 0 for non-Done states and answers that fit.
    let max_body = area.height.saturating_sub(2) as usize;
    let focus_active = focused && state.max_scroll_offset(content_width, max_body) > 0;

    let border_color = if focus_active {
        theme.accent_user
    } else {
        theme.gray_dim
    };
    let border_style = Style::default().fg(border_color).bg(bg);

    // ── Clear area and draw rounded border ──
    Clear.render(area, buf);
    buf.set_style(area, Style::default().bg(bg));
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style)
        .style(Style::default().bg(bg))
        .render(area, buf);

    // ── Hint in top border (right side): scroll position + [Esc] ──
    // Built BEFORE the title so the title can reserve room for it and truncate
    // the question, rather than the question pushing [Esc] off-screen. The close
    // affordance ([Esc]) always stays visible: its columns are reserved here
    // first, and on panels too narrow for the full Done-state hint we drop the
    // scroll indicator and keep a bare "[Esc]" (fallback below).
    let hint = match state {
        BtwOverlayState::Loading { .. } | BtwOverlayState::Error { .. } => "[Esc]".to_string(),
        BtwOverlayState::Done {
            content,
            scroll_offset,
            ..
        } => {
            let total = content.with_wrapped_lines(content_width, |w| w.lines.len());
            if total > max_body {
                // Clamp offset to valid range in case terminal resized or
                // content_width differs from what the input handler estimated.
                let offset = (*scroll_offset).min(total.saturating_sub(max_body));
                let pos = offset + 1;
                let end = (offset + max_body).min(total);
                if focus_active {
                    format!("{pos}-{end}/{total}  \u{2191}\u{2193}  [Esc]")
                } else {
                    // Not focused: arrows go to the prompt, so omit the ↑↓ hint.
                    format!("{pos}-{end}/{total}  [Esc]")
                }
            } else {
                "[Esc]".to_string()
            }
        }
    };
    let title_x = area.x + 2;
    let mut hint_text = format!(" {hint} ");
    let mut hint_w = hint_text.width() as u16;
    // Right-align the hint just inside the right border, without underflowing on
    // very narrow panels.
    let mut hint_x = (area.x + area.width).saturating_sub(1 + hint_w);
    // On a narrow panel the Done-state hint (scroll position + ↑↓ + [Esc]) can be
    // wide enough to leave no room for the title (hint_x < title_x). Fall back to
    // a bare "[Esc]" so the close affordance — and its mouse hit target — always
    // survives; at 7 columns it fits at the minimum panel width (12), and the
    // title regains room too.
    if hint_x < title_x {
        hint_text = " [Esc] ".to_string();
        hint_w = hint_text.width() as u16;
        hint_x = (area.x + area.width).saturating_sub(1 + hint_w);
    }

    // ── Title in top border: " /btw <question> " ──
    // Reserve the hint's columns (everything left of `hint_x`, minus the title's
    // own two padding spaces) so a long question truncates instead of hiding the
    // hint.
    let question = state.question();
    let title_prefix = "/btw ";
    let max_title = hint_x.saturating_sub(title_x).saturating_sub(2) as usize;
    let title_style = Style::default()
        .fg(theme.accent_user)
        .bg(bg)
        .add_modifier(Modifier::BOLD);
    let full_title = format!("{title_prefix}{question}");
    let truncated = if full_title.width() > max_title {
        let mut s = String::new();
        let mut w = 0;
        for ch in full_title.chars() {
            let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if w + cw + 1 > max_title {
                break;
            }
            s.push(ch);
            w += cw;
        }
        s.push('\u{2026}');
        s
    } else {
        full_title
    };
    let title_text = format!(" {truncated} ");
    let title_line = Line::from(Span::styled(title_text.clone(), title_style));
    // Clamp the paint width so the title can never bleed into the hint region,
    // even in degenerate narrow layouts.
    let title_render_w = (title_text.width() as u16).min(hint_x.saturating_sub(title_x));
    buf.set_line(title_x, area.y, &title_line, title_render_w);

    // ── Render the hint (always visible — its space was reserved above) ──
    if hint_w > 0 && hint_x >= title_x {
        let is_hovered = hit_close.as_ref().is_some_and(|h| h.hovered);
        let hint_style = if is_hovered {
            Style::default()
                .fg(theme.text_primary)
                .bg(bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.gray).bg(bg)
        };
        let hint_line = Line::from(Span::styled(hint_text, hint_style));
        buf.set_line(hint_x, area.y, &hint_line, hint_w);
        // Set hit area for mouse click handling (top border row).
        if let Some(hit) = hit_close {
            hit.set(Some(Rect {
                x: hint_x,
                y: area.y,
                width: hint_w,
                height: 1,
            }));
        }
    } else if let Some(hit) = hit_close {
        hit.clear();
    }

    // ── Body (between borders) ──
    let body_y = area.y + 1;
    match state {
        BtwOverlayState::Loading { .. } => {
            let frames = crate::glyphs::braille_spinner_frames();
            let frame_idx = ((tick / SPINNER_DIVISOR) % frames.len() as u64) as usize;
            let spinner = frames[frame_idx];
            let loading_style = Style::default().fg(theme.gray).bg(bg);
            let line = Line::from(vec![
                Span::styled(format!("{spinner} "), loading_style),
                Span::styled("Answering\u{2026}", loading_style),
            ]);
            buf.set_line(content_x, body_y, &line, content_width as u16);
        }
        BtwOverlayState::Done {
            content,
            scroll_offset,
            ..
        } => {
            // One wrap pass for paint, selection, and link mapping (same as
            // scrollback reusing cached BlockOutput).
            let block_output = content.output(content_width);
            let total = block_output.lines.len();
            let content_skip = (*scroll_offset).min(total.saturating_sub(max_body));
            let end = (content_skip + max_body).min(total);
            let visible_count = end.saturating_sub(content_skip);
            for (row, idx) in (content_skip..end).enumerate() {
                let bl = &block_output.lines[idx];
                // Content paints bidi-aware (when rtl_bidi is on) so the shared
                // selection machinery, which maps visual columns, agrees with
                // the drawn cells — matching scrollback/list content.
                buf.set_line_safe_bidi(
                    content_x,
                    body_y + row as u16,
                    &bl.content,
                    content_width as u16,
                );
                let text = line_plain_text(&bl.content);
                let joiner_to_previous = if idx == 0 { None } else { bl.joiner.clone() };
                selection_model.push_line(ResolvedSelectableLine {
                    entry_idx: BTW_OVERLAY_ENTRY_IDX,
                    range_id: BTW_OVERLAY_RANGE_ID,
                    block_line_idx: idx,
                    screen_y: body_y + row as u16,
                    screen_x: content_x,
                    selectable_cols: 0..crate::scrollback::types::str_display_cells(&text) as u16,
                    text,
                    painted_region: None,
                    joiner_to_previous,
                });
            }
            if visible_count > 0 {
                let body_area = Rect {
                    x: content_x,
                    y: body_y,
                    width: content_width as u16,
                    height: visible_count as u16,
                };
                selection_model.content_area = body_area;
                selection_model.visible_blocks.push(VisibleBlockGeometry {
                    entry_idx: BTW_OVERLAY_ENTRY_IDX,
                    area: body_area,
                    content_area: body_area,
                    selection_area: body_area,
                    content_width: content_width as u16,
                    top_clipped: false,
                    bottom_clipped: false,
                    drag_startable: true,
                });
            }
            // Markdown hyperlinks + plain-text URL / file-path scan (parity
            // with scrollback's map_hyperlinks + scan_lines_for_url_overlays).
            if let Some(overlay) = link_overlay {
                let max_screen_y = body_y.saturating_add(visible_count as u16);
                content.with_hyperlinks(|hyperlinks| {
                    if hyperlinks.is_empty() {
                        return;
                    }
                    map_hyperlinks_to_overlay(
                        hyperlinks,
                        &block_output,
                        content_skip,
                        body_y,
                        max_screen_y,
                        content_x,
                        /* content_line_offset */ 0,
                        media_paths,
                        cwd,
                        overlay,
                    );
                });
                let visible_lines = block_output
                    .lines
                    .iter()
                    .enumerate()
                    .skip(content_skip)
                    .map(|(idx, bl)| {
                        let visible_offset = (idx - content_skip) as u16;
                        let screen_row = body_y + visible_offset;
                        (screen_row, &bl.content, bl.joiner.as_deref())
                    })
                    .take_while(|(screen_row, _, _)| *screen_row < max_screen_y);
                scan_lines_for_url_overlays(visible_lines, content_x, media_paths, overlay);
            }
        }
        BtwOverlayState::Error { error, .. } => {
            let error_style = Style::default().fg(theme.accent_error).bg(bg);
            // Cap at the rows this rect can paint: the minimal renderer
            // routinely hands the panel a shorter rect than it asked for,
            // and the ellipsis must land on a row the user can see.
            let max_rows =
                (area.height.saturating_sub(2) as usize).min(DONE_MAX_BODY_LINES as usize);
            for (i, text) in wrapped_error_lines(error, content_width, max_rows)
                .into_iter()
                .enumerate()
            {
                let line = Line::from(Span::styled(text, error_style));
                buf.set_line(content_x, body_y + i as u16, &line, content_width as u16);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::osc8::resolve_link_target;

    fn render_with_model(
        state: &BtwOverlayState,
        width: u16,
        height: u16,
    ) -> ResolvedSelectionModel {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        let mut model = ResolvedSelectionModel::default();
        render_btw_panel(
            &mut buf,
            state,
            area,
            0,
            false,
            None,
            &mut model,
            None,
            &[],
            None,
        );
        model
    }

    /// Render the panel and return the raw buffer for cell inspection.
    fn render_to_buffer(state: &BtwOverlayState, width: u16, height: u16) -> Buffer {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        let mut model = ResolvedSelectionModel::default();
        render_btw_panel(
            &mut buf,
            state,
            area,
            0,
            false,
            None,
            &mut model,
            None,
            &[],
            None,
        );
        buf
    }

    /// Concatenated symbols of buffer row `y` across `width` columns.
    fn row_text(buf: &Buffer, width: u16, y: u16) -> String {
        (0..width)
            .filter_map(|x| buf.cell((x, y)).map(|c| c.symbol().to_string()))
            .collect()
    }

    fn render_with_links(
        state: &BtwOverlayState,
        width: u16,
        height: u16,
    ) -> (ResolvedSelectionModel, LinkOverlay) {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        let mut model = ResolvedSelectionModel::default();
        let mut links = LinkOverlay::new();
        render_btw_panel(
            &mut buf,
            state,
            area,
            0,
            false,
            None,
            &mut model,
            Some(&mut links),
            &[],
            None,
        );
        (model, links)
    }

    /// Build a Done state with an explicit scroll offset.
    fn done_with_scroll(response: &str, scroll_offset: usize) -> BtwOverlayState {
        let mut state = BtwOverlayState::done("q".to_string(), response.to_string());
        if let BtwOverlayState::Done {
            scroll_offset: so, ..
        } = &mut state
        {
            *so = scroll_offset;
        }
        state
    }

    /// `n` distinct rendered lines via CommonMark hard breaks (two trailing
    /// spaces), so each `lineNN` maps 1:1 to a rendered line.
    fn hard_break_lines(n: usize) -> String {
        (0..n)
            .map(|i| format!("line{i:02}"))
            .collect::<Vec<_>>()
            .join("  \n")
    }

    #[test]
    fn done_state_populates_selection_model() {
        let state = BtwOverlayState::done(
            "test".to_string(),
            "line one and line two and line three".to_string(),
        );
        let model = render_with_model(&state, 40, 8);
        assert!(!model.ranges.is_empty(), "should have selectable ranges");
        let range = &model.ranges[0];
        assert_eq!(range.entry_idx, BTW_OVERLAY_ENTRY_IDX);
        assert_eq!(range.range_id, BTW_OVERLAY_RANGE_ID);
        assert!(!range.lines.is_empty());
        for (i, line) in range.lines.iter().enumerate() {
            assert_eq!(line.block_line_idx, i);
            assert_eq!(line.screen_y, 1 + i as u16); // body_y = area.y + 1
        }
        assert!(
            !model.visible_blocks.is_empty(),
            "should have visible block geometry"
        );
    }

    #[test]
    fn loading_state_does_not_populate_selection_model() {
        let state = BtwOverlayState::Loading {
            question: "q".to_string(),
        };
        let model = render_with_model(&state, 40, 4);
        assert!(model.ranges.is_empty());
        assert!(model.visible_blocks.is_empty());
    }

    #[test]
    fn error_state_does_not_populate_selection_model() {
        let state = BtwOverlayState::Error {
            question: "q".to_string(),
            error: "something went wrong".to_string(),
        };
        let model = render_with_model(&state, 40, 4);
        assert!(model.ranges.is_empty());
        assert!(model.visible_blocks.is_empty());
    }

    /// A long error wraps across body rows instead of truncating to one line
    /// with an ellipsis (the old behavior cut off the actionable tail).
    #[test]
    fn error_state_wraps_long_message_across_rows() {
        let error = "Rate limited (429): you've hit the rate limit for your \
                     plan. Try again later or upgrade for higher limits.";
        let state = BtwOverlayState::Error {
            question: "q".to_string(),
            error: error.to_string(),
        };
        // width 40 → content_width 36; the message needs 4 rows.
        let height = btw_panel_height(Some(&state), 40);
        assert!(height > 3, "long error must grow the panel, got {height}");

        let buf = render_to_buffer(&state, 40, height);
        let body: String = (1..height - 1).map(|y| row_text(&buf, 40, y)).collect();
        assert!(
            body.contains("upgrade for higher limits"),
            "tail lost: {body}"
        );
        assert!(!body.contains('\u{2026}'), "no ellipsis when fully shown");
    }

    /// Short errors keep the compact 3-row panel.
    #[test]
    fn error_panel_height_stays_compact_for_short_error() {
        let state = BtwOverlayState::Error {
            question: "q".to_string(),
            error: "boom".to_string(),
        };
        assert_eq!(btw_panel_height(Some(&state), 40), 3);
        let buf = render_to_buffer(&state, 40, 3);
        assert!(row_text(&buf, 40, 1).contains("boom"));
    }

    /// Pathologically long errors cap at DONE_MAX_BODY_LINES with an ellipsis,
    /// and every line (including the ellipsized last one) fits the width so
    /// the renderer can't cut the ellipsis off.
    #[test]
    fn error_body_caps_at_max_lines_with_ellipsis() {
        let max = DONE_MAX_BODY_LINES as usize;
        let error = "word ".repeat(400);
        let lines = wrapped_error_lines(&error, 36, max);
        assert_eq!(lines.len(), max);
        assert!(lines.last().unwrap().ends_with('\u{2026}'));
        for line in &lines {
            assert!(line.width() <= 36, "line exceeds width: {line:?}");
        }
        // A word exactly filling the last line must lose a char to the '…'.
        let exact = vec!["x".repeat(36); max + 1].join(" ");
        let lines = wrapped_error_lines(&exact, 36, max);
        assert_eq!(lines.last().unwrap().width(), 36);
        assert!(lines.last().unwrap().ends_with('\u{2026}'));
        // Wide (2-column) chars: the pop loop must free enough columns.
        let cjk = "字".repeat(18 * (max + 1));
        let lines = wrapped_error_lines(&cjk, 36, max);
        assert!(lines.last().unwrap().ends_with('\u{2026}'));
        assert!(lines.last().unwrap().width() <= 36);
    }

    /// Paragraph breaks survive the wrap, and a token wider than the panel
    /// (a long URL) is broken instead of overflowing.
    #[test]
    fn error_wrap_keeps_paragraphs_and_breaks_long_tokens() {
        let error = "first paragraph\n\nhttps://example.com/very/long/path/that/cannot/fit/in/one/panel/row";
        let lines = wrapped_error_lines(error, 20, DONE_MAX_BODY_LINES as usize);
        assert!(lines.contains(&String::new()), "blank paragraph gap kept");
        for line in &lines {
            assert!(line.width() <= 20, "line exceeds width: {line:?}");
        }
        assert!(lines.len() > 3, "URL must break across rows");
    }

    /// If the layout hands the panel a shorter rect than requested (routine in
    /// minimal mode), the body caps at the rows it got — ellipsis on the last
    /// visible row, bottom border intact.
    #[test]
    fn error_render_clamps_to_short_rect() {
        let error = "word ".repeat(100);
        let state = BtwOverlayState::Error {
            question: "q".to_string(),
            error,
        };
        // Desired height is far taller than the 5 rows given.
        assert!(btw_panel_height(Some(&state), 40) > 5);
        let buf = render_to_buffer(&state, 40, 5);
        let last_body = row_text(&buf, 40, 3);
        assert!(last_body.contains("word"), "last body row used");
        assert!(
            last_body.contains('\u{2026}'),
            "cut body must end in an ellipsis: {last_body}"
        );
        assert!(
            row_text(&buf, 40, 4).contains('╰'),
            "bottom border must survive the clamp"
        );
    }

    #[test]
    fn scroll_offset_shifts_block_line_idx() {
        // 10 short lines, each on its own rendered line (hard breaks).
        let response = hard_break_lines(10);
        let state_0 = done_with_scroll(&response, 0);
        let state_2 = done_with_scroll(&response, 2);
        // height=6 → max_body=4; 10 lines → offset 2 is valid.
        let model_0 = render_with_model(&state_0, 40, 6);
        let model_2 = render_with_model(&state_2, 40, 6);
        assert!(!model_0.ranges.is_empty());
        assert!(!model_2.ranges.is_empty());
        assert_eq!(model_0.ranges[0].lines[0].block_line_idx, 0);
        assert_eq!(model_2.ranges[0].lines[0].block_line_idx, 2);
    }

    #[test]
    fn full_selection_model_spans_entire_response() {
        let response = hard_break_lines(20);
        let state = done_with_scroll(&response, 8);
        let model = state.full_selection_model(40);
        assert_eq!(model.ranges.len(), 1);
        assert_eq!(model.ranges[0].lines.len(), 20);
        assert_eq!(model.ranges[0].lines[0].block_line_idx, 0);
        assert_eq!(model.ranges[0].lines[19].block_line_idx, 19);
        assert_eq!(model.ranges[0].lines[19].text, "line19");
    }

    #[test]
    fn copy_includes_lines_scrolled_out_of_view() {
        use crate::scrollback::text_selection::{
            ActiveTextDrag, RangeHit, reconstruct_selection_text,
        };
        let response = hard_break_lines(20);
        let state = done_with_scroll(&response, 8);
        let full_model = state.full_selection_model(40);
        let drag = ActiveTextDrag {
            anchor: RangeHit {
                entry_idx: BTW_OVERLAY_ENTRY_IDX,
                range_id: BTW_OVERLAY_RANGE_ID,
                block_line_idx: 2,
                col_within_range: 0,
            },
            head: RangeHit {
                entry_idx: BTW_OVERLAY_ENTRY_IDX,
                range_id: BTW_OVERLAY_RANGE_ID,
                block_line_idx: 14,
                col_within_range: 5,
            },
            kind: Default::default(),
            anchor_content_width: None,
        };
        let text = reconstruct_selection_text(&full_model, &drag).expect("reconstruct");
        let expected = (2..=14)
            .map(|i| format!("line{i:02}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(text, expected);
    }

    /// Regression for the actual bug: the Done overlay must render markdown
    /// (bold, headings, tables) instead of echoing the raw source.
    #[test]
    fn done_state_renders_markdown_not_raw_source() {
        let response =
            "**Bold intro**\n\n### Heading\n\n| Item | Qty |\n|------|-----|\n| Bow | 1 |";
        let state = BtwOverlayState::done("q".to_string(), response.to_string());
        // Wide + tall enough to render the whole response without scrolling.
        let model = render_with_model(&state, 60, 16);
        let rendered: String = model
            .ranges
            .iter()
            .flat_map(|r| r.lines.iter())
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");

        // Markdown syntax is consumed, not shown literally.
        assert!(
            !rendered.contains("**"),
            "bold markers should be rendered away, got: {rendered:?}"
        );
        assert!(
            !rendered.contains("###"),
            "heading markers should be rendered away, got: {rendered:?}"
        );
        assert!(
            !rendered.contains("---"),
            "table separator should be rendered, not raw dashes, got: {rendered:?}"
        );
        // Content survives and the table is drawn with box-drawing borders.
        assert!(rendered.contains("Bold intro"), "got: {rendered:?}");
        assert!(rendered.contains("Heading"), "got: {rendered:?}");
        assert!(
            rendered.contains("Item") && rendered.contains("Bow"),
            "table cells should render, got: {rendered:?}"
        );
        assert!(
            rendered.contains('│') || rendered.contains('─'),
            "table should render with box-drawing chars, got: {rendered:?}"
        );
    }

    /// Regression: Done overlay must expose markdown hyperlinks as overlay
    /// links (OSC 8 / click-to-open), not only paint styled text.
    #[test]
    fn done_state_maps_markdown_links_to_overlay() {
        let url = "https://example.com/btw-link";
        let response = format!("See [docs]({url}) for details.");
        let state = BtwOverlayState::done("q".to_string(), response);
        let (_model, overlay) = render_with_links(&state, 60, 8);
        assert!(
            !overlay.is_empty(),
            "expected at least one overlay link for markdown href"
        );
        let found = overlay.links().iter().any(|l| {
            resolve_link_target(&l.target)
                .and_then(|resolved| resolved.osc8_url)
                .as_deref()
                .unwrap_or("")
                == url
        });
        assert!(
            found,
            "overlay should contain {url}, got: {:?}",
            overlay
                .links()
                .iter()
                .filter_map(
                    |l| resolve_link_target(&l.target).and_then(|resolved| resolved.osc8_url)
                )
                .collect::<Vec<_>>()
        );
        // Links live in the body (row >= 1), not the title border.
        for link in overlay.links() {
            assert!(
                link.screen_row >= 1,
                "link should be in panel body, got row {}",
                link.screen_row
            );
            assert!(link.col_end > link.col_start);
        }
    }

    #[test]
    fn done_state_maps_plain_url_autolinks() {
        let url = "https://example.com/plain";
        let state = BtwOverlayState::done("q".to_string(), format!("Visit {url} please."));
        let (_model, overlay) = render_with_links(&state, 60, 8);
        assert!(
            overlay
                .links()
                .iter()
                .any(|l| resolve_link_target(&l.target)
                    .and_then(|resolved| resolved.osc8_url)
                    .as_deref()
                    .unwrap_or("")
                    == url),
            "plain URL should become an overlay link, got: {:?}",
            overlay
                .links()
                .iter()
                .filter_map(
                    |l| resolve_link_target(&l.target).and_then(|resolved| resolved.osc8_url)
                )
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn done_state_scans_file_paths_like_scrollback() {
        // Absolute path text (not a markdown hyperlink) should still become a
        // file:// overlay via scan_lines_for_url_overlays.
        let path = "/Users/test/project/src/main.rs";
        let state = BtwOverlayState::done("q".to_string(), format!("See {path} for details."));
        let (_model, overlay) = render_with_links(&state, 80, 8);
        let urls: Vec<_> = overlay
            .links()
            .iter()
            .filter_map(|l| resolve_link_target(&l.target).and_then(|resolved| resolved.osc8_url))
            .collect();
        assert!(
            urls.iter()
                .any(|u| u.contains("main.rs") && u.starts_with("file://")),
            "file path should map to file:// overlay, got: {urls:?}"
        );
    }

    #[test]
    fn scrolled_links_use_visible_rows_only() {
        // Many lines so scroll_offset > 0; a link on the last line should map
        // to a body row, not the pre-scroll absolute line index.
        let mut lines: Vec<String> = (0..20).map(|i| format!("line{i:02}")).collect();
        let url = "https://example.com/scrolled";
        lines.push(format!("[end]({url})"));
        let response = lines.join("  \n");
        let mut state = BtwOverlayState::done("q".to_string(), response);
        if let BtwOverlayState::Done {
            scroll_offset: so, ..
        } = &mut state
        {
            // 20 lineNN + 1 link = 21 lines. height=6 → max_body=4; offset is
            // clamped to total−max_body = 17, so visible indices 17..20 and the
            // link (idx 20) is the last visible row.
            *so = 18;
        }
        let (_model, overlay) = render_with_links(&state, 60, 6);
        let link = overlay
            .links()
            .iter()
            .find(|l| {
                resolve_link_target(&l.target)
                    .and_then(|resolved| resolved.osc8_url)
                    .as_deref()
                    .unwrap_or("")
                    == url
            })
            .expect("scrolled link should still map when visible");
        // Body starts at row 1; clamped offset 17 + 4 visible rows → link at
        // visible index 3 → screen_row = 1 + 3 = 4.
        assert_eq!(
            link.screen_row, 4,
            "link should be on last visible body row"
        );
    }

    /// Regression: a long question must not push the [Esc] hint off the top
    /// border. The question truncates (…) and the hint stays visible.
    #[test]
    fn long_question_truncates_title_but_keeps_esc_hint() {
        let long_q = "please also double-check the error handling and the retry \
                      logic across every single call site in the whole module";
        let state = BtwOverlayState::Loading {
            question: long_q.to_string(),
        };
        let width = 40;
        let buf = render_to_buffer(&state, width, 4);
        let top = row_text(&buf, width, 0);
        assert!(
            top.contains("[Esc]"),
            "[Esc] hint must stay visible when the question is long, got: {top:?}"
        );
        assert!(
            top.contains('\u{2026}'),
            "long question should be truncated with an ellipsis, got: {top:?}"
        );
    }

    /// A short question keeps its full title AND the [Esc] hint (no regression
    /// to the common case).
    #[test]
    fn short_question_shows_full_title_and_esc_hint() {
        let state = BtwOverlayState::Loading {
            question: "hi".to_string(),
        };
        let width = 40;
        let buf = render_to_buffer(&state, width, 4);
        let top = row_text(&buf, width, 0);
        assert!(top.contains("/btw hi"), "full title expected, got: {top:?}");
        assert!(top.contains("[Esc]"), "hint expected, got: {top:?}");
        assert!(
            !top.contains('\u{2026}'),
            "short title must not be truncated, got: {top:?}"
        );
    }

    /// Regression: on a panel too narrow for the Done-state scroll hint
    /// ("pos-end/total  [Esc]"), the overlay falls back to a bare "[Esc]" so the
    /// close affordance never disappears (and the wide scroll indicator is
    /// dropped rather than hiding [Esc]).
    #[test]
    fn done_narrow_panel_falls_back_to_bare_esc() {
        // 50 lines → answer overflows, so the hint gains a "1-4/50" scroll prefix
        // that is far too wide for a 14-col panel.
        let response = hard_break_lines(50);
        let state = done_with_scroll(&response, 0);
        let width = 14; // below the full-hint width, above the 12-col minimum
        let buf = render_to_buffer(&state, width, 6);
        let top = row_text(&buf, width, 0);
        assert!(
            top.contains("[Esc]"),
            "[Esc] must survive on a narrow Done panel, got: {top:?}"
        );
        assert!(
            !top.contains("/50"),
            "the wide scroll indicator should be dropped in favor of a bare [Esc], got: {top:?}"
        );
    }

    /// The clickable [Esc] hit area is still registered even when the question
    /// is long enough to force truncation.
    #[test]
    fn long_question_still_registers_esc_hit_area() {
        let state = BtwOverlayState::Loading {
            question: "x".repeat(200),
        };
        let area = Rect::new(0, 0, 40, 4);
        let mut buf = Buffer::empty(area);
        let mut model = ResolvedSelectionModel::default();
        let mut hit = crate::app::agent_view::HitArea::default();
        render_btw_panel(
            &mut buf,
            &state,
            area,
            0,
            false,
            Some(&mut hit),
            &mut model,
            None,
            &[],
            None,
        );
        let rect = hit
            .rect
            .expect("[Esc] hit area must be set even with a long question");
        assert_eq!(rect.y, 0, "hit area lives on the top border row");
        assert!(rect.width > 0, "hit area must be non-empty");
        // The hit area sits inside the right border, not off-screen.
        assert!(
            rect.x + rect.width <= area.width,
            "hit area must be within the panel: {rect:?}"
        );
    }
}
