//! Shared picker rendering helpers.
//!
//! Three rendering paths exist:
//!
//! 1. **`render_picker()`** -- full-frame picker used by the welcome-screen
//!    session picker. Handles frame, search bar, divider, entries, and
//!    shortcuts. Two layout modes:
//!    - **FullScreen** — centered content filling the available area (session picker).
//! - **Floating** — dimmed background with a bordered popup (command palette, arg picker).
//!
//! 2. **`render_picker_content()`** -- content-only rendering (entries +
//!    scrollbar) into a provided area. Used by modal popups (command
//!    palette, arg picker, session picker, doc picker) whose chrome is
//!    handled by [`super::modal_window::render_modal_window`].
//!
//! 3. **Shared primitives** -- `render_search_bar`, `render_divider`,
//!    `render_picker_row`, `render_picker_entry`, `handle_picker_input`,
//!    etc. are used by both paths above.

use std::collections::HashSet;
use std::sync::LazyLock;

use crossterm::event::KeyEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::input::line_editor::{LineEditOutcome, LineEditor};
use crate::render::line_utils::truncate_str;
use crate::render::wrapping::word_wrap_line;
use crate::theme::Theme;
use crate::views::shortcuts_bar::{HintItem, PendingHint, ShortcutsBar};

/// Effective base background for picker chrome. In minimal mode every UI element
/// is terminal-transparent (`Color::Reset`) — no opaque panels, no selection /
/// hover bands (selection is shown via accent label text instead). Otherwise the
/// caller-provided `bg` (or the default `bg_base`) is used.
fn picker_base_bg(bg: Option<Color>, theme: &Theme) -> Color {
    if crate::views::modal_window::embedded() {
        Color::Reset
    } else {
        bg.unwrap_or(theme.bg_base)
    }
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A single entry in a picker list — either a section header or a selectable row.
pub enum PickerEntry<'a> {
    /// Non-selectable section header (rendered as `── label ──`).
    Header { label: &'a str },
    /// A selectable row.
    Row(PickerRow<'a>),
}

/// A selectable row in a picker list.
pub struct PickerRow<'a> {
    /// Primary label text (left-aligned).
    pub label: &'a str,
    /// Secondary text (right-aligned, e.g. shortcut, ID+time, description).
    pub right_label: &'a str,
    /// Whether this row is the selected/cursor row.
    pub selected: bool,
    /// Whether detail fields are shown below this row.
    pub expanded: bool,
    /// Detail fields shown when expanded (empty = not expandable).
    pub fields: &'a [PickerField<'a>],
    /// Additional lines rendered below the label (e.g., command details).
    /// Shown only when expanded.
    pub description_lines: &'a [&'a str],
    /// Secondary lines rendered below the label while the row is collapsed
    /// (e.g., an at-a-glance summary). Hidden when expanded, where
    /// `description_lines` and `fields` take over.
    pub summary_lines: &'a [&'a str],
    /// Whether this row should be dimmed (e.g., disabled plugins/hooks).
    pub dimmed: bool,
    /// Indentation level (0 = top-level, 1 = nested under a group header, etc.).
    /// Each level adds 2 spaces of left padding.
    pub indent: u8,
    /// Optional badge text shown right after the label (e.g., "[installed]").
    pub badge: &'a str,
    /// Color for the badge text. If `None`, uses `gray`.
    pub badge_color: Option<ratatui::style::Color>,
    /// Whether this row is a collapsible group header. When true, the
    /// ›/◆ fold indicator is shown even if `fields` and
    /// `description_lines` are empty. The `expanded` field controls
    /// which glyph is rendered.
    pub collapsible: bool,
    /// Underline the final description line (when expanded) so it reads as a
    /// clickable link. Used for the Managed connectors URL.
    pub underline_last_desc: bool,
}

/// A key-value pair shown in an expanded picker row.
pub struct PickerField<'a> {
    pub label: &'a str,
    pub value: &'a str,
}

/// Frame returned by both [`render_floating_frame`] and [`render_fullscreen_frame`].
pub struct PickerFrame {
    /// Content area for search bar and entries.
    pub content: Rect,
    /// Close button `[\u{2717}]` rect for mouse hit-testing.
    pub close_button: Rect,
}

/// How the picker is framed on screen.
///
/// Used by the welcome-screen session picker. Modal popups (command
/// palette, arg picker, etc.) use [`super::modal_window::ModalWindow`]
/// for chrome and [`render_picker_content`] for the entry list.
#[derive(Debug, Clone, Default)]
pub enum PickerMode {
    /// Centered content filling the given area (no border, no dim).
    FullScreen,
    /// Floating popup with dimmed background and rounded border.
    #[default]
    Floating,
    /// Centered popup with configurable dimensions.
    Popup(PopupConfig),
}

impl PickerMode {
    /// Toggle between modes.
    pub fn toggle(self) -> Self {
        match self {
            Self::FullScreen => Self::Popup(PopupConfig::default()),
            Self::Floating => Self::FullScreen,
            Self::Popup(_) => Self::FullScreen,
        }
    }
}

/// Render a picker frame in the given mode.
///
/// Dispatches to [`render_floating_frame`] or [`render_fullscreen_frame`]
/// based on `mode`. Returns `None` if the area is too small.
pub fn render_picker_frame(
    buf: &mut Buffer,
    area: Rect,
    theme: &Theme,
    mode: PickerMode,
    title: Option<&str>,
    close_hovered: bool,
) -> Option<PickerFrame> {
    match mode {
        PickerMode::Floating => render_floating_frame(buf, area, theme, close_hovered),
        PickerMode::Popup(popup_cfg) => {
            // Use the popup frame (configurable dimensions), then wrap in PickerFrame.
            let inner = render_popup_frame(buf, area, theme, &popup_cfg)?;
            // In Popup mode, the close button is rendered later (by tab bar or search bar),
            // so return a default close_button rect that will be overwritten.
            Some(PickerFrame {
                content: inner,
                close_button: Rect::default(),
            })
        }
        PickerMode::FullScreen => render_fullscreen_frame(buf, area, theme, title, close_hovered),
    }
}

// ---------------------------------------------------------------------------
// Scroll computation
// ---------------------------------------------------------------------------

/// Compute minimal scroll offset to keep `selected` visible in a window of `visible` items
/// out of `total`.
pub fn compute_scroll_offset(
    selected: usize,
    total: usize,
    visible: usize,
    num_headers: usize,
) -> usize {
    let total_with_spacers = total.saturating_add(num_headers.saturating_sub(1));
    if total_with_spacers <= visible {
        0
    } else {
        // Center the selection, accounting for spacer rows before headers
        let centered = selected.saturating_sub(visible / 2);
        let max_scroll = total
            .saturating_add(num_headers.saturating_sub(1))
            .saturating_sub(visible);
        centered.min(max_scroll)
    }
}
// ---------------------------------------------------------------------------
// Search bar
// ---------------------------------------------------------------------------

const SEARCH_BAR_LABEL: &str = " search: ";
const SEARCH_BAR_TRAILING_GAP: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchBarLayout {
    render_width: u16,
    input_width: usize,
    trailing_width: u16,
}

impl SearchBarLayout {
    pub fn input_width(self) -> usize {
        self.input_width
    }

    pub fn trailing_width(self) -> u16 {
        self.trailing_width
    }
}

pub fn search_bar_layout(width: u16, trailing_width: u16) -> SearchBarLayout {
    let label_width = (SEARCH_BAR_LABEL.len() as u16).min(width);
    let available_input = width - label_width;
    let trailing_reserved = if trailing_width > 0
        && available_input
            >= trailing_width
                .saturating_add(SEARCH_BAR_TRAILING_GAP)
                .saturating_add(1)
    {
        trailing_width.saturating_add(SEARCH_BAR_TRAILING_GAP)
    } else {
        0
    };
    let render_width = width - trailing_reserved;
    let input_width = render_width.saturating_sub(label_width) as usize;
    SearchBarLayout {
        render_width,
        input_width,
        trailing_width: if trailing_reserved == 0 {
            0
        } else {
            trailing_width
        },
    }
}

/// Render a search bar row: ` search: {query}_` or ` / to search` hint.
///
/// - `active`: whether the cursor blinks (search mode is engaged).
/// - `show_hint`: show `/ to search` placeholder when query is empty and not active.
/// - `query_cursor`: byte offset of the editing cursor within `query`.
///   The visual cursor is placed after the display-width of `query[..query_cursor]`.
/// - `bg`: optional background color for the search text (used in Floating mode).
#[allow(clippy::too_many_arguments)]
pub fn render_search_bar(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    theme: &Theme,
    query: &str,
    active: bool,
    show_hint: bool,
    query_cursor: usize,
    bg: Option<ratatui::style::Color>,
) {
    render_search_bar_with_label(
        buf,
        x,
        y,
        width,
        theme,
        SEARCH_BAR_LABEL,
        query,
        active,
        show_hint,
        query_cursor,
        bg,
    );
}

#[allow(clippy::too_many_arguments)]
pub fn render_search_bar_with_viewport(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    layout: SearchBarLayout,
    theme: &Theme,
    query: &str,
    active: bool,
    show_hint: bool,
    bg: Option<ratatui::style::Color>,
    viewport: echo_agent_ratatui_textarea::SingleLineViewport,
) {
    render_search_bar_with_label_viewport(
        buf,
        x,
        y,
        layout.render_width,
        theme,
        SEARCH_BAR_LABEL,
        query,
        active,
        show_hint,
        0,
        bg,
        Some(viewport),
    );
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_picker_search_bar(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    theme: &Theme,
    state: &PickerState,
    active: bool,
    show_hint: bool,
    bg: Option<ratatui::style::Color>,
) {
    render_line_editor_search_bar(buf, x, y, width, theme, &state.query, active, show_hint, bg);
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_line_editor_search_bar(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    theme: &Theme,
    editor: &LineEditor,
    active: bool,
    show_hint: bool,
    bg: Option<ratatui::style::Color>,
) {
    render_line_editor_search_bar_with_label(
        buf,
        x,
        y,
        width,
        theme,
        SEARCH_BAR_LABEL,
        editor,
        active,
        show_hint,
        bg,
    );
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_picker_search_bar_with_label(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    theme: &Theme,
    label: &str,
    state: &PickerState,
    active: bool,
    show_hint: bool,
    bg: Option<ratatui::style::Color>,
) {
    render_line_editor_search_bar_with_label(
        buf,
        x,
        y,
        width,
        theme,
        label,
        &state.query,
        active,
        show_hint,
        bg,
    );
}

#[allow(clippy::too_many_arguments)]
fn render_line_editor_search_bar_with_label(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    theme: &Theme,
    label: &str,
    editor: &LineEditor,
    active: bool,
    show_hint: bool,
    bg: Option<ratatui::style::Color>,
) {
    let input_width = width.saturating_sub(label.len() as u16) as usize;
    let viewport = editor.viewport(input_width);
    render_search_bar_with_label_viewport(
        buf,
        x,
        y,
        width,
        theme,
        label,
        editor.text(),
        active,
        show_hint,
        editor.cursor_byte(),
        bg,
        Some(viewport),
    );
}

/// Like [`render_search_bar`] but with a caller-supplied prompt `label`
/// (e.g. `" path: "`) instead of the default `" search: "`. The label
/// width is measured in bytes (ASCII), matching the input-window math.
#[allow(clippy::too_many_arguments)]
pub fn render_search_bar_with_label(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    theme: &Theme,
    label: &str,
    query: &str,
    active: bool,
    show_hint: bool,
    query_cursor: usize,
    bg: Option<ratatui::style::Color>,
) {
    render_search_bar_with_label_viewport(
        buf,
        x,
        y,
        width,
        theme,
        label,
        query,
        active,
        show_hint,
        query_cursor,
        bg,
        None,
    );
}

#[allow(clippy::too_many_arguments)]
fn render_search_bar_with_label_viewport(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    theme: &Theme,
    label: &str,
    query: &str,
    active: bool,
    show_hint: bool,
    query_cursor: usize,
    bg: Option<ratatui::style::Color>,
    viewport: Option<echo_agent_ratatui_textarea::SingleLineViewport>,
) {
    // Minimal mode renders every UI element background-free.
    let bg = if crate::views::modal_window::embedded() {
        None
    } else {
        bg
    };
    let bg_style = |style: Style| -> Style { if let Some(c) = bg { style.bg(c) } else { style } };

    // "Always-active" mode: cursor always visible (Floating mode search).
    let always_active = !active && !show_hint;

    if active || !query.is_empty() || always_active {
        let label_w = label.len() as u16;
        buf.set_line(
            x,
            y,
            &Line::from(Span::styled(
                label,
                bg_style(Style::default().fg(theme.gray)),
            )),
            width,
        );

        let input_x = x + label_w;
        let input_width = width.saturating_sub(label_w) as usize;
        let cursor_limit = input_width.saturating_sub(1);
        let cursor_col = if let Some(viewport) = viewport {
            if !query.is_empty() {
                let displayed = &query[viewport.visible_byte_range];
                buf.set_span(
                    input_x,
                    y,
                    &Span::styled(displayed, bg_style(Style::default().fg(theme.text_primary))),
                    displayed.width() as u16,
                );
            }
            viewport.cursor_display_column
        } else {
            // Compatibility path for raw-string inputs that have not adopted LineEditor.
            let mut cursor_byte = query_cursor.min(query.len());
            while cursor_byte > 0 && !query.is_char_boundary(cursor_byte) {
                cursor_byte -= 1;
            }
            let prefix_width = query[..cursor_byte].width();
            let (start_byte, cursor_col) = if prefix_width <= cursor_limit {
                (0, prefix_width)
            } else {
                let mut skipped_width = 0usize;
                let mut start_byte = 0usize;
                for (index, character) in query.char_indices() {
                    if index >= cursor_byte || prefix_width - skipped_width <= cursor_limit {
                        break;
                    }
                    skipped_width += character.width().unwrap_or(0);
                    start_byte = index + character.len_utf8();
                }
                (start_byte, prefix_width - skipped_width)
            };
            if !query.is_empty() {
                let displayed = truncate_str(&query[start_byte..], cursor_limit);
                buf.set_span(
                    input_x,
                    y,
                    &Span::styled(
                        &displayed,
                        bg_style(Style::default().fg(theme.text_primary)),
                    ),
                    displayed.width() as u16,
                );
            }
            cursor_col
        };

        let cursor_display_w = (cursor_col as u16).min(cursor_limit as u16);

        if active || always_active {
            let cursor_x = input_x + cursor_display_w;
            if cursor_x < x + width {
                // Inverse-video the cell at the cursor position so the
                // character underneath remains visible (matching the
                // rename overlay's cursor style).
                if let Some(cell) = buf.cell_mut((cursor_x, y)) {
                    let cursor_fg = if let Some(c) = bg { c } else { theme.bg_base };
                    cell.set_style(Style::default().fg(cursor_fg).bg(theme.text_primary));
                }
            }
        }
    } else if show_hint {
        buf.set_line(
            x,
            y,
            &Line::from(Span::styled(
                " / to search",
                bg_style(Style::default().fg(theme.gray_dim)),
            )),
            width,
        );
    }
}

// ---------------------------------------------------------------------------
// Divider
// ---------------------------------------------------------------------------

/// Render a horizontal `─` divider.
pub fn render_divider(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    theme: &Theme,
    bg: Option<ratatui::style::Color>,
) {
    // In minimal mode every element is terminal-transparent, so ignore any
    // caller-supplied opaque background (e.g. `bg_base`) and force `Reset`.
    let bg = if crate::views::modal_window::embedded() {
        Some(Color::Reset)
    } else {
        bg
    };
    let style = if let Some(c) = bg {
        Style::default().fg(theme.gray_dim).bg(c)
    } else {
        Style::default().fg(theme.gray_dim)
    };
    for cx in x..x + width {
        if let Some(cell) = buf.cell_mut((cx, y)) {
            cell.set_char('\u{2500}');
            cell.set_style(style);
        }
    }
}

// ---------------------------------------------------------------------------
// Tab bar (shared)
// ---------------------------------------------------------------------------

/// Hit areas returned by [`render_tab_bar`].
pub struct TabBarHitAreas {
    /// One rect per tab label; `None` if the tab didn't fit.
    pub tab_rects: Vec<Option<Rect>>,
    /// Close button rect.
    pub close_button: Rect,
}

/// Render a horizontal tab bar: `[ Label1 ]  [ Label2 ]  ...`
///
/// Active tab is bold with `bg_light`; inactive tabs are gray on `bg_base`.
/// The close button `[\u{2717}]` is rendered right-aligned on the same row.
/// Returns hit areas for mouse click and close button detection.
#[allow(clippy::too_many_arguments)]
pub fn render_tab_bar(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    theme: &Theme,
    labels: &[&str],
    active: usize,
    close_hovered: bool,
) -> TabBarHitAreas {
    if width < 6 {
        return TabBarHitAreas {
            tab_rects: vec![None; labels.len()],
            close_button: Rect::default(),
        };
    }

    // Clear the tab row.
    let clear_style = Style::default().fg(theme.text_primary).bg(theme.bg_base);
    for cx in x..x + width {
        if let Some(cell) = buf.cell_mut((cx, y)) {
            cell.reset();
            cell.set_style(clear_style);
        }
    }

    // Close button right-aligned (reuse shared helper).
    let close_rect = render_close_button(buf, x, y, width, theme, close_hovered, None);

    // Tab labels.
    let mut tab_rects = Vec::with_capacity(labels.len());
    let mut cx = x + 1; // 1 char left padding
    for (i, label) in labels.iter().enumerate() {
        let label_w = label.width() as u16;
        let tab_w = label_w + 2; // " Label "

        if cx + tab_w > close_rect.x.saturating_sub(1) {
            tab_rects.push(None);
            continue;
        }

        let (fg, bg, mods) = if i == active {
            (theme.text_primary, theme.bg_light, Modifier::BOLD)
        } else {
            (theme.gray, theme.bg_base, Modifier::empty())
        };

        let style = Style::default().fg(fg).bg(bg).add_modifier(mods);
        let pad = Style::default().bg(bg);

        buf.set_span(cx, y, &Span::styled(" ", pad), 1);
        buf.set_span(cx + 1, y, &Span::styled(*label, style), label_w);
        buf.set_span(cx + 1 + label_w, y, &Span::styled(" ", pad), 1);

        tab_rects.push(Some(Rect::new(cx, y, tab_w, 1)));
        cx += tab_w + 2; // 2 chars gap between tabs
    }

    TabBarHitAreas {
        tab_rects,
        close_button: close_rect,
    }
}

// ---------------------------------------------------------------------------
// Popup frame (shared)
// ---------------------------------------------------------------------------

/// Configuration for a centered popup frame.
#[derive(Debug, Clone)]
pub struct PopupConfig {
    /// Width as a fraction of the available area (0.0 - 1.0).
    pub width_pct: f32,
    /// Height as a fraction of the available area (0.0 - 1.0).
    pub height_pct: f32,
    /// Minimum popup dimensions.
    pub min_width: u16,
    pub min_height: u16,
}

impl Default for PopupConfig {
    fn default() -> Self {
        Self {
            width_pct: 0.65,
            height_pct: 0.60,
            min_width: 40,
            min_height: 12,
        }
    }
}

/// Render a centered popup frame with dimmed background and rounded border.
///
/// Returns the inner content area. The close button is NOT rendered here;
/// callers typically render it via [`render_tab_bar`] or [`render_close_button`]
/// on the first row of the content area.
///
/// Returns `None` if the area is too small.
pub fn render_popup_frame(
    buf: &mut Buffer,
    area: Rect,
    theme: &Theme,
    config: &PopupConfig,
) -> Option<Rect> {
    use ratatui::widgets::{Block, BorderType, Borders, Clear, Widget};

    if area.height < config.min_height || area.width < config.min_width {
        return None;
    }

    // Compute centered popup area (before rendering anything).
    let popup_w = ((area.width as f32 * config.width_pct) as u16)
        .max(config.min_width)
        .min(area.width);
    let popup_h = ((area.height as f32 * config.height_pct) as u16)
        .max(config.min_height)
        .min(area.height.saturating_sub(2));

    // Pre-check inner dimensions (border is 1 cell on each side) before any
    // rendering so we never leave a half-drawn popup on screen.
    if popup_h.saturating_sub(2) < 3 || popup_w.saturating_sub(2) < 10 {
        return None;
    }

    let popup_x = area.x + (area.width.saturating_sub(popup_w)) / 2;
    let popup_y = area.y + (area.height.saturating_sub(popup_h)) / 2;
    let popup_area = Rect::new(popup_x, popup_y, popup_w, popup_h);

    // Pre-compute inner area so we can bail before rendering side-effects.
    let base_style = Style::default().fg(theme.text_primary).bg(theme.bg_base);
    let border = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.gray_dim))
        .style(base_style);
    let inner = border.inner(popup_area);

    if inner.height < 3 || inner.width < 10 {
        return None;
    }

    // Safe to render: dim background, clear, and draw bordered popup.
    crate::views::file_search::line_viewer::dim_area(buf, area, theme.bg_base, 0.5);
    Clear.render(popup_area, buf);
    buf.set_style(popup_area, base_style);
    border.render(popup_area, buf);
    Some(inner)
}

// ---------------------------------------------------------------------------
// Search bar filter indicator
// ---------------------------------------------------------------------------

/// Render a right-aligned filter indicator on a search bar row.
///
/// Draws e.g. `Enabled  f` at the right edge. Used by plugin/hooks modal.
///
/// `active` controls the label styling: when `false` (default/"All" state),
/// the label renders in `gray_dim`; when `true` (filter active), in `gray`;
/// when `hovered`, in `text_primary + BOLD`.
#[allow(clippy::too_many_arguments)]
pub fn render_filter_indicator(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    theme: &Theme,
    label: &str,
    key_hint: &str,
    active: bool,
    hovered: bool,
) -> Rect {
    let label_w = label.width() as u16;
    let hint_w = key_hint.width() as u16;
    let total_w = label_w + 1 + hint_w + 1; // "Label k "
    let start_x = x + width.saturating_sub(total_w + 1);

    let label_fg = if hovered {
        theme.text_primary
    } else if active {
        theme.gray
    } else {
        theme.gray_dim
    };
    let label_mods = if hovered {
        Modifier::BOLD
    } else {
        Modifier::empty()
    };
    let label_style = Style::default()
        .fg(label_fg)
        .bg(theme.bg_base)
        .add_modifier(label_mods);
    let hint_style = Style::default()
        .fg(label_fg)
        .bg(theme.bg_base)
        .add_modifier(Modifier::BOLD);

    buf.set_span(start_x, y, &Span::styled(label, label_style), label_w);
    buf.set_span(
        start_x + label_w + 1,
        y,
        &Span::styled(key_hint, hint_style),
        hint_w,
    );

    Rect::new(start_x, y, total_w, 1)
}

// ---------------------------------------------------------------------------
// Picker rows
// ---------------------------------------------------------------------------

/// Render a single picker row with unified visual style:
/// - Selected: `\u{276f} label` in `text_primary+BOLD`, `bg_visual` background.
/// - Normal: `  label` in `gray_bright`.
/// - Right text: right-aligned in `gray_dim` (or `gray+bg_visual` when selected).
///
/// Compute the exact number of visual rows a picker row will consume
/// at the given width. Used for scroll offset calculation.
/// Return the byte offset in `s` that covers at most `max_width` display columns.
/// Parse `[bracket]` highlight markers in a string into styled spans.
/// Text inside `[...]` gets `highlight_style`, the rest gets `base_style`.
/// Brackets are stripped from the output.
fn parse_highlight_spans<'a>(s: &'a str, base_style: Style, highlight_style: Style) -> Line<'a> {
    let mut spans = Vec::new();
    let mut rest = s;
    while let Some(open) = rest.find('[') {
        if open > 0 {
            spans.push(Span::styled(&rest[..open], base_style));
        }
        rest = &rest[open + 1..];
        if let Some(close) = rest.find(']') {
            spans.push(Span::styled(&rest[..close], highlight_style));
            rest = &rest[close + 1..];
        } else {
            // Unmatched '[' — render literally.
            spans.push(Span::styled("[", base_style));
        }
    }
    if !rest.is_empty() {
        spans.push(Span::styled(rest, base_style));
    }
    Line::from(spans)
}

/// Render styled spans into a buffer, truncating to `max_width` columns.
fn render_styled_spans(buf: &mut Buffer, spans: &Line<'_>, x: u16, y: u16, max_width: usize) {
    let mut cx = x;
    let end_x = x + max_width as u16;
    for span in &spans.spans {
        if cx >= end_x {
            break;
        }
        let avail = (end_x - cx) as usize;
        let sw = span.content.width();
        if sw <= avail {
            buf.set_span(cx, y, span, sw as u16);
            cx += sw as u16;
        } else {
            let end = byte_offset_for_width(&span.content, avail);
            if end > 0 {
                buf.set_span(
                    cx,
                    y,
                    &Span::styled(&span.content[..end], span.style),
                    avail as u16,
                );
            }
            break;
        }
    }
}

fn byte_offset_for_width(s: &str, max_width: usize) -> usize {
    let mut w = 0usize;
    for (i, ch) in s.char_indices() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw > max_width {
            return i;
        }
        w += cw;
    }
    s.len()
}
/// Visual row count for a description line (matches [`render_picker_row`] wrapping).
fn description_visual_rows(desc: &str, max_w: usize) -> usize {
    if max_w == 0 {
        return 1;
    }
    let base = Style::default();
    let line = parse_highlight_spans(desc, base, base);
    word_wrap_line(&line, max_w).len().max(1)
}

/// Left indent (columns) for picker row description and detail lines.
const DESC_INDENT: u16 = 4;

pub fn compute_row_height(row: &PickerRow<'_>, width: u16) -> usize {
    let mut rows = 1usize; // main label line
    let indent = DESC_INDENT;
    let max_w = width.saturating_sub(indent) as usize;
    if !row.expanded {
        if max_w > 0 {
            for line in row.summary_lines {
                rows += description_visual_rows(line, max_w);
            }
        }
        return rows;
    }
    if max_w == 0 {
        return rows;
    }
    // Description lines (word-wrapped; must match render_picker_row).
    for desc in row.description_lines {
        rows += description_visual_rows(desc, max_w);
    }
    // Expanded field values (word-wrapped).
    let label_col = 13usize; // "{:<12} "
    let val_w = max_w.saturating_sub(label_col).max(1);
    for field in row.fields {
        let val_len = field.value.width();
        rows += if val_len <= val_w {
            1
        } else {
            val_len.div_ceil(val_w)
        };
    }
    rows
}
///
/// When `row.expanded && !row.fields.is_empty()`, renders key-value detail lines
/// below (indented, label in `gray`, value in `gray_bright`).
///
/// Rows consumed by a rendered picker row/entry, plus the row band of its
/// underlined link line (recorded from what is painted) for click hit-testing.
pub struct RenderedRow {
    pub rows: u16,
    pub link_band: Option<std::ops::Range<u16>>,
}

/// `max_rows` caps rendering to available vertical space; detail fields beyond
/// that limit are not drawn.
///
/// `bg` is the base background color (used in Floating mode popups).
#[allow(clippy::too_many_arguments)]
pub fn render_picker_row(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    theme: &Theme,
    row: &PickerRow,
    hovered: bool,
    bg: Option<ratatui::style::Color>,
    max_rows: u16,
) -> RenderedRow {
    if max_rows == 0 {
        return RenderedRow {
            rows: 0,
            link_band: None,
        };
    }
    let base_bg = picker_base_bg(bg, theme);
    let embed = crate::views::modal_window::embedded_row_style(theme, row.selected);
    let row_bg = match embed {
        Some(e) => e.bg,
        None if hovered => theme.bg_hover,
        None if row.selected => theme.bg_visual,
        None => base_bg,
    };
    let meta_fg = embed.map_or(theme.gray, |e| e.fg(theme.gray));

    // Fill row background.
    let row_rect = Rect {
        x,
        y,
        width,
        height: 1,
    };
    buf.set_style(row_rect, Style::default().bg(row_bg));

    // Left side: indent + cursor indicator + fold indicator + label.
    let indent_str = if row.indent > 0 {
        "  ".repeat(row.indent as usize)
    } else {
        String::new()
    };
    // Show ›/◆ fold indicator for expandable rows (those with fields
    // or description lines). Non-expandable rows show a ◆ diamond.
    // Selection is indicated by bg_hover/bg_visual row highlight
    // (no ❯ cursor glyph), matching the import-claude modal's style.
    let is_expandable =
        row.collapsible || !row.fields.is_empty() || !row.description_lines.is_empty();
    let fold_width: u16 = 2; // "› " or "◆ "
    let label_style = if row.selected {
        Style::default()
            .fg(embed.map_or(theme.text_primary, |e| e.fg(theme.text_primary)))
            .bg(row_bg)
            .add_modifier(Modifier::BOLD)
    } else if row.dimmed {
        Style::default().fg(theme.gray_dim).bg(row_bg)
    } else {
        Style::default().fg(theme.text_primary).bg(row_bg)
    };

    let prefix_width = indent_str.width() as u16 + fold_width;
    let trailing_pad = 1u16; // space before border/scrollbar
    let badge_width = if row.badge.is_empty() {
        0u16
    } else {
        row.badge.width() as u16 + 1
    }; // +1 space before badge
    // Cap right column at half width so long descriptions don't crush labels.
    let fixed = prefix_width + trailing_pad + badge_width;
    let content_width = width.saturating_sub(fixed);
    let (max_label_width, truncated_right) = if row.right_label.is_empty() {
        (content_width as usize, String::new())
    } else {
        let gap = 2u16;
        let usable = content_width.saturating_sub(gap);
        let max_right = (usable / 2) as usize;
        let right = truncate_str(row.right_label, max_right);
        let right_cols = right.width() as u16;
        let max_label = usable.saturating_sub(right_cols) as usize;
        (max_label, right)
    };
    let truncated_label = truncate_str(row.label, max_label_width);
    let right_width = truncated_right.width() as u16;

    // Render as separate spans: indent, fold indicator (shared), label.
    let mut cur_x = x;
    if !indent_str.is_empty() {
        buf.set_span(
            cur_x,
            y,
            &Span::styled(&indent_str, label_style),
            indent_str.width() as u16,
        );
        cur_x += indent_str.width() as u16;
    }
    if is_expandable {
        if embed.is_some_and(|e| e.selected) {
            let glyph = if row.expanded {
                format!("{} ", crate::glyphs::diamond_filled())
            } else {
                format!("{} ", crate::glyphs::chevron())
            };
            buf.set_span(
                cur_x,
                y,
                &Span::styled(glyph, Style::default().fg(meta_fg).bg(row_bg)),
                fold_width,
            );
            cur_x += fold_width;
        } else {
            cur_x += crate::views::modal_window::render_fold_indicator(
                buf,
                cur_x,
                y,
                !row.expanded, // collapsed = !expanded
                false,         // picker rows don't track fold hover
                Some(row_bg),
                theme,
            );
        }
    } else {
        // Non-expandable: show ◆ diamond to indicate a leaf entry.
        let diamond_fg = embed.map_or(theme.gray_dim, |e| e.fg(theme.gray_dim));
        let style = Style::default().fg(diamond_fg).bg(row_bg);
        buf.set_span(
            cur_x,
            y,
            &Span::styled(format!("{} ", crate::glyphs::diamond_filled()), style),
            fold_width,
        );
        cur_x += fold_width;
    }
    buf.set_span(
        cur_x,
        y,
        &Span::styled(&truncated_label, label_style),
        truncated_label.width() as u16,
    );
    let full_label_width = prefix_width + truncated_label.width() as u16;

    // Badge (right after label).
    if !row.badge.is_empty() {
        let badge_x = x + full_label_width + 1;
        let badge_fg = row.badge_color.unwrap_or(meta_fg);
        let badge_style = Style::default().fg(badge_fg).bg(row_bg);
        buf.set_span(
            badge_x,
            y,
            &Span::styled(row.badge, badge_style),
            row.badge.width() as u16,
        );
    }

    if right_width > 0 {
        let right_style = Style::default().fg(meta_fg).bg(row_bg);
        let right_x = x + width.saturating_sub(right_width + trailing_pad);
        buf.set_span(
            right_x,
            y,
            &Span::styled(&truncated_right, right_style),
            right_width,
        );
    }

    // Description lines (shown when expanded) or summary lines (collapsed).
    let mut rows = 1u16;
    let mut link_band: Option<std::ops::Range<u16>> = None;
    let secondary_lines: &[&str] = if row.expanded {
        row.description_lines
    } else {
        row.summary_lines
    };
    if !secondary_lines.is_empty() {
        let indent = DESC_INDENT;
        let desc_style = Style::default().fg(theme.gray).bg(base_bg);
        let highlight_style = Style::default()
            .fg(theme.text_primary)
            .bg(base_bg)
            .add_modifier(Modifier::BOLD);
        // Underline the final description line when the row opts in, so it reads as a link.
        let link_style = highlight_style.add_modifier(Modifier::UNDERLINED);
        let last_line = secondary_lines.len().saturating_sub(1);
        let max_w = width.saturating_sub(indent) as usize;
        for (li, desc) in secondary_lines.iter().enumerate() {
            let is_link = row.underline_last_desc && row.expanded && li == last_line;
            let hl = if is_link { link_style } else { highlight_style };
            // Render with [bracket] highlight markers: text inside
            // [...] is shown in bold/bright, brackets are stripped.
            let line = parse_highlight_spans(desc, desc_style, hl);
            let wrapped = word_wrap_line(&line, max_w);
            let link_start = y + rows;
            for wrap_line in wrapped {
                if rows >= max_rows {
                    break;
                }
                render_styled_spans(buf, &wrap_line, x + indent, y + rows, max_w);
                rows += 1;
            }
            // Record only painted rows, so a vertically-clipped link records nothing.
            if is_link && y + rows > link_start {
                link_band = Some(link_start..(y + rows));
            }
        }
    }

    // Expanded detail fields.
    if row.expanded && !row.fields.is_empty() {
        let indent = DESC_INDENT;
        let field_label_style = Style::default().fg(theme.gray).bg(base_bg);
        let field_value_style = Style::default().fg(theme.gray_bright).bg(base_bg);

        for field in row.fields {
            if rows >= max_rows {
                break;
            }
            let fy = y + rows;
            if field.label.is_empty() {
                // Loading indicator or similar \u{2014} no label column.
                let line = Line::from(Span::styled(
                    field.value,
                    Style::default().fg(theme.gray_dim).bg(base_bg),
                ));
                buf.set_line(x + indent, fy, &line, width.saturating_sub(indent));
            } else {
                let label_text = format!("{:<12} ", field.label);
                let label_col = label_text.len() as u16;
                let max_val_w = width.saturating_sub(indent + label_col) as usize;
                // First line: label + start of value.
                let val = field.value;
                let first_chunk_len = byte_offset_for_width(val, max_val_w);
                let first_break = if val.width() <= max_val_w {
                    val.len()
                } else if let Some(pos) = val[..first_chunk_len].rfind(' ') {
                    pos + 1
                } else {
                    first_chunk_len
                };
                let line = Line::from(vec![
                    Span::styled(label_text, field_label_style),
                    Span::styled(&val[..first_break], field_value_style),
                ]);
                buf.set_line(x + indent, fy, &line, width.saturating_sub(indent));
                // Continuation lines (indented to align with value column).
                let mut remaining = val[first_break..].trim_start();
                while !remaining.is_empty() {
                    rows += 1;
                    if rows >= max_rows {
                        break;
                    }
                    let cy = y + rows;
                    let chunk_len = byte_offset_for_width(remaining, max_val_w);
                    let break_at = if remaining.width() <= max_val_w {
                        remaining.len()
                    } else if let Some(pos) = remaining[..chunk_len].rfind(' ') {
                        pos + 1
                    } else {
                        chunk_len
                    };
                    let chunk = &remaining[..break_at];
                    buf.set_span(
                        x + indent + label_col,
                        cy,
                        &Span::styled(chunk, field_value_style),
                        chunk.width() as u16,
                    );
                    remaining = remaining[break_at..].trim_start();
                }
            }
            rows += 1;
        }
    }

    RenderedRow { rows, link_band }
}

/// Render a single picker entry (header or row).
///
/// `max_rows` caps rendering to available vertical space.
#[allow(clippy::too_many_arguments)]
pub fn render_picker_entry(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    theme: &Theme,
    entry: &PickerEntry,
    hovered: bool,
    bg: Option<ratatui::style::Color>,
    max_rows: u16,
) -> RenderedRow {
    if max_rows == 0 {
        return RenderedRow {
            rows: 0,
            link_band: None,
        };
    }
    match entry {
        PickerEntry::Header { label } => {
            let base_bg = picker_base_bg(bg, theme);
            let header_style = Style::default()
                .fg(theme.gray)
                .bg(base_bg)
                .add_modifier(Modifier::BOLD);
            let sep_style = Style::default().fg(theme.gray_dim).bg(base_bg);
            let title = format!(" {} ", label);
            let title_w = title.width();
            let remaining = width as usize - title_w.min(width as usize);
            let sep: String = std::iter::repeat_n('\u{2500}', remaining).collect();
            let line = Line::from(vec![
                Span::styled(title, header_style),
                Span::styled(sep, sep_style),
            ]);
            buf.set_line(x, y, &line, width);
            RenderedRow {
                rows: 1,
                link_band: None,
            }
        }
        PickerEntry::Row(row) => {
            render_picker_row(buf, x, y, width, theme, row, hovered, bg, max_rows)
        }
    }
}

// ---------------------------------------------------------------------------
// Close button
// ---------------------------------------------------------------------------

/// Render a `[\u{2717}]` close button right-aligned at `(x..x+width, y)`.
///
/// Returns the `Rect` for mouse hit-testing.
pub fn render_close_button(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    theme: &Theme,
    hovered: bool,
    bg: Option<ratatui::style::Color>,
) -> Rect {
    let text = crate::glyphs::ballot_x_button();
    let w: u16 = 3;
    let bx = x + width.saturating_sub(w);
    let style = if hovered {
        let mut s = Style::default()
            .fg(theme.text_primary)
            .add_modifier(Modifier::BOLD);
        if let Some(c) = bg {
            s = s.bg(c);
        }
        s
    } else {
        let mut s = Style::default().fg(theme.gray);
        if let Some(c) = bg {
            s = s.bg(c);
        }
        s
    };
    buf.set_span(bx, y, &Span::styled(text, style), w);
    Rect::new(bx, y, w, 1)
}

// ---------------------------------------------------------------------------
// Floating frame
// ---------------------------------------------------------------------------

/// Render the floating popup frame: dim background, rounded border, close button.
///
/// Returns `None` if the area is too small to render anything.
pub fn render_floating_frame(
    buf: &mut Buffer,
    area: Rect,
    theme: &Theme,
    close_hovered: bool,
) -> Option<PickerFrame> {
    use ratatui::widgets::{Block, BorderType, Borders, Clear, Widget};

    if area.height < 8 || area.width < 30 {
        return None;
    }

    // Dim background.
    crate::views::file_search::line_viewer::dim_area(buf, area, theme.bg_base, 0.5);

    // Compute popup area (65% width, fixed height for 20 entries).
    let popup_w = ((area.width as f32 * 0.65) as u16).max(44).min(area.width);
    let popup_h = (4 + 20).min(area.height.saturating_sub(2));
    let popup_x = area.x + (area.width.saturating_sub(popup_w)) / 2;
    let popup_y = area.y + (area.height.saturating_sub(popup_h)) / 3; // bias upward
    let popup_area = Rect::new(popup_x, popup_y, popup_w, popup_h);

    // Clear and draw bordered popup.
    Clear.render(popup_area, buf);
    let base_style = Style::default().fg(theme.text_primary).bg(theme.bg_base);
    buf.set_style(popup_area, base_style);
    let border = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.gray_dim))
        .style(base_style);
    let inner = border.inner(popup_area);
    border.render(popup_area, buf);

    if inner.height < 3 || inner.width < 10 {
        return None;
    }

    let close_button = render_close_button(
        buf,
        inner.x,
        inner.y,
        inner.width,
        theme,
        close_hovered,
        Some(theme.bg_base),
    );

    Some(PickerFrame {
        content: inner,
        close_button,
    })
}

// ---------------------------------------------------------------------------
// Bordered frame primitive
// ---------------------------------------------------------------------------

/// Layout returned by [`render_bordered_frame`].
pub struct BorderedFrame {
    /// Title row area (between top border and separator). Caller fills this.
    pub title_row: Rect,
    /// Content area (between separator and bottom border). Caller fills this.
    pub content: Rect,
}

/// Render a bordered panel with a title row separated from content.
///
/// Draws:
/// ```text
/// \u{250c}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2510}
/// \u{2502}  <title row>  \u{2502}
/// \u{251c}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2524}
/// \u{2502}  <content>    \u{2502}
/// \u{2514}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2518}
/// ```
///
/// The caller is responsible for rendering title and content into the
/// returned rects. This function only draws the frame chrome.
///
/// Returns `None` if the area is too small (< 5 rows or < 10 cols).
pub fn render_bordered_frame(
    buf: &mut Buffer,
    area: Rect,
    border_color: ratatui::style::Color,
    bg: ratatui::style::Color,
) -> Option<BorderedFrame> {
    use ratatui::widgets::{Block, Borders, Clear, Widget};

    if area.height < 5 || area.width < 10 {
        return None;
    }

    let base_style = Style::default().fg(border_color).bg(bg);
    let border_style = Style::default().fg(border_color).bg(bg);

    // Clear area.
    Clear.render(area, buf);
    buf.set_style(area, Style::default().bg(bg));

    // Header block: top border + title row (no bottom border).
    let header_area = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: 2,
    };
    let header_block = Block::default()
        .borders(Borders::TOP | Borders::LEFT | Borders::RIGHT)
        .border_style(base_style);
    header_block.render(header_area, buf);

    let title_row = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: 1,
    };

    // Content frame: all borders below the header.
    let content_area = Rect {
        x: area.x,
        y: area.y + 2,
        width: area.width,
        height: area.height.saturating_sub(2),
    };
    let content_block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style);
    let content = content_block.inner(content_area);
    content_block.render(content_area, buf);

    // Connect header to content frame with T-junctions.
    if let Some(cell) = buf.cell_mut((area.x, content_area.y)) {
        cell.set_char('\u{251c}');
        cell.set_style(border_style);
    }
    if let Some(cell) = buf.cell_mut((area.x + area.width.saturating_sub(1), content_area.y)) {
        cell.set_char('\u{2524}');
        cell.set_style(border_style);
    }

    if content.width == 0 || content.height == 0 {
        return None;
    }

    Some(BorderedFrame { title_row, content })
}

// ---------------------------------------------------------------------------
// Full-screen frame
// ---------------------------------------------------------------------------

/// Render a full-screen bordered picker panel using [`render_bordered_frame`].
///
/// Fills the title row with optional title text and a close button.
/// Returns `None` if the area is too small.
pub fn render_fullscreen_frame(
    buf: &mut Buffer,
    area: Rect,
    theme: &Theme,
    title: Option<&str>,
    close_hovered: bool,
) -> Option<PickerFrame> {
    let frame = render_bordered_frame(buf, area, theme.gray_dim, theme.bg_base)?;

    if title.is_some() {
        // Fill title row.
        if let Some(title_text) = title {
            let title_line = Line::from(vec![
                Span::styled("  ", Style::default().bg(theme.bg_base)),
                Span::styled(
                    title_text,
                    Style::default()
                        .fg(theme.text_primary)
                        .bg(theme.bg_base)
                        .add_modifier(Modifier::BOLD),
                ),
            ]);
            buf.set_line(
                frame.title_row.x,
                frame.title_row.y,
                &title_line,
                frame.title_row.width,
            );
        }

        // Close button on the title row.
        let close_button = render_close_button(
            buf,
            frame.title_row.x,
            frame.title_row.y,
            frame.title_row.width,
            theme,
            close_hovered,
            Some(theme.bg_base),
        );

        Some(PickerFrame {
            content: frame.content,
            close_button,
        })
    } else {
        // No title: reclaim the title row + separator for content.
        let extra = frame.content.y.saturating_sub(frame.title_row.y);
        let content = Rect {
            x: frame.content.x + 1,
            y: frame.title_row.y,
            width: frame.content.width.saturating_sub(2),
            height: frame.content.height + extra,
        };
        Some(PickerFrame {
            content,
            close_button: Rect::default(),
        })
    }
}

// ---------------------------------------------------------------------------
// Unified picker: state, config, outcome, render, input
// ---------------------------------------------------------------------------

/// Persistent picker state -- callers own this and pass `&mut` to input.
///
/// Fields used by both the `render_picker()` path (welcome screen) and
/// the `ModalWindow` + `render_picker_content()` path (modal popups):
/// `selected`, query editor, `search_active`, `expanded`, `hovered`,
/// `scroll_offset`, `hit_areas`.
///
/// Fields used **only** by the `render_picker()` path (welcome screen):
/// `mode`, `close_hovered`, `tab_hit_areas`, `filter_area`,
/// `filter_hovered`. Modal popups delegate these responsibilities to
/// [`super::modal_window::ModalWindowState`].
#[derive(Debug, Clone)]
pub struct PickerState {
    /// Currently selected index in the filtered entries list.
    pub selected: usize,
    /// Canonical single-line search editor.
    query: LineEditor,
    /// Whether the search input is focused (only relevant when `show_search_hint` is true).
    pub search_active: bool,
    /// Indices of expanded entries in original data (empty = all collapsed). Caller-managed.
    pub expanded: HashSet<usize>,
    /// Display mode (Floating, FullScreen, or Popup). Only used by
    /// `render_picker()` (welcome screen); modal popups ignore this.
    pub mode: PickerMode,
    /// Whether the mouse is hovering over the close button. Only used by
    /// `render_picker()` (welcome screen); modal popups use
    /// `ModalWindowState::close_hovered`.
    pub close_hovered: bool,
    /// Mouse-hover highlight (independent of selected). `None` when not hovering.
    pub hovered: Option<usize>,
    /// Mouse-scroll viewport offset (visual row). `None` = auto-center on `selected`.
    /// Keyboard nav resets to `None`.
    pub scroll_offset: Option<usize>,
    /// Hit areas from the last render (for mouse hit-testing).
    pub hit_areas: Option<PickerHitAreas>,
    /// Entry index and absolute row band of the underlined link line from the
    /// last render (the Managed connectors URL), for click-to-open hit-testing.
    pub link_band: Option<(usize, std::ops::Range<u16>)>,
    /// Hit areas for tab labels (one per tab, `None` if tab didn't fit).
    pub tab_hit_areas: Option<Vec<Option<Rect>>>,
    /// Hit area for the filter indicator in the search bar.
    pub filter_area: Option<Rect>,
    /// Whether the mouse is hovering over the filter indicator.
    pub filter_hovered: bool,
    /// Whether the tab bar region has keyboard focus. When true, Left/Right cycle tabs.
    pub tabs_focused: bool,
    /// Suppress the list selection highlight while keyboard focus is on the search bar (via edge navigation); cleared once the selection is meaningful again (typing, navigating back into the list, mouse click/hover). Session-picker rows gate their `selected` flag on `!selection_hidden`.
    pub selection_hidden: bool,
}

impl Default for PickerState {
    fn default() -> Self {
        Self {
            selected: 0,
            query: LineEditor::default(),
            search_active: false,
            expanded: HashSet::new(),
            mode: PickerMode::Floating,
            close_hovered: false,
            hovered: None,
            scroll_offset: None,
            hit_areas: None,
            link_band: None,
            tab_hit_areas: None,
            filter_area: None,
            filter_hovered: false,
            tabs_focused: false,
            selection_hidden: false,
        }
    }
}

impl PickerState {
    /// Create a new `PickerState` with the given display mode.
    pub fn with_mode(mode: PickerMode) -> Self {
        Self {
            mode,
            ..Self::default()
        }
    }

    /// Open directly in input mode (`search_active = true`) — for type-to-find
    /// pickers (command palette, `/model` & `/theme` arg picker) so typing
    /// filters immediately. Under `vim_normal_first`, Esc still drops to nav and
    /// `i` re-enters input.
    pub fn input_active() -> Self {
        Self {
            search_active: true,
            ..Self::default()
        }
    }

    /// Reset transient state (query, selection, search, expanded) while
    /// preserving the display mode and clearing hit areas.
    pub fn reset(&mut self) {
        self.selected = 0;
        self.query.reset();
        self.search_active = false;
        self.expanded.clear();
        self.close_hovered = false;
        self.hit_areas = None;
        self.link_band = None;
        self.tab_hit_areas = None;
        self.filter_area = None;
        self.filter_hovered = false;
        self.tabs_focused = false;
        self.selection_hidden = false;
    }

    /// Clear the search query and the selection/scroll state that tracks it,
    /// returning to an empty-query view without touching focus flags
    /// (`tabs_focused`) or hit areas. Used by the vim Esc-to-nav-mode path.
    pub fn clear_query(&mut self) {
        self.query.reset();
        self.scroll_offset = None;
        self.selected = 0;
        self.selection_hidden = false;
        self.expanded.clear();
    }

    pub fn query(&self) -> &str {
        self.query.text()
    }

    pub fn query_cursor(&self) -> usize {
        self.query.cursor_byte()
    }

    /// Replace the query, remove line breaks, and place the cursor at text end.
    pub fn set_query(&mut self, query: impl Into<String>) {
        self.query.set_text(query);
    }

    fn edit_query(&mut self, key: &KeyEvent) -> LineEditOutcome {
        self.query.handle_key(key)
    }

    pub(crate) fn paste_query(&mut self, text: &str) -> LineEditOutcome {
        self.query.insert_paste(text)
    }

    /// When a search query is active on an expandable picker, force-expand
    /// all entries so that matches inside collapsed groups/trees are
    /// explicitly visible. Individual items can still be manually collapsed
    /// by the user (those toggles are respected).
    pub fn expand_all_for_search(&mut self, entry_count: usize) {
        if entry_count > 0 {
            for i in 0..entry_count {
                self.expanded.insert(i);
            }
        }
    }
}

/// Unified hit areas for all pickers.
#[derive(Debug, Clone)]
pub struct PickerHitAreas {
    /// Close button rect.
    pub close_button: Rect,
    /// Search bar rect.
    pub search_bar: Rect,
    /// Per-item click rects (only selectable rows, not headers).
    pub item_rects: Vec<Rect>,
    /// Maps visual index → entry index: `item_rects[i]` corresponds to
    /// `entries[entry_indices[i]]`.
    pub entry_indices: Vec<usize>,
    /// One rect per tab label; `None` if the tab didn't fit.
    pub tab_rects: Vec<Option<Rect>>,
    /// Hit rect for the filter indicator. `None` if no filter.
    pub filter_rect: Option<Rect>,
}

/// Configuration for picker behavior (non-state, provided per-call).
pub struct PickerConfig<'a> {
    /// Title shown in fullscreen mode's title row (e.g. "Resume session").
    pub title: Option<&'a str>,
    /// When true, show "/ to search" hint; user must activate search explicitly.
    /// When false, search is always active (cursor always visible).
    pub show_search_hint: bool,
    /// Whether the picker supports e:expand and y:copy.
    pub expandable: bool,
    /// Whether Esc clears the query before closing.
    pub esc_clears_query: bool,
    /// Keybinding hints rendered at the bottom — inside the frame in fullscreen,
    /// at the bottom of the full area in floating mode.
    pub shortcuts: Option<&'a [HintItem]>,
    /// Pending double-press confirmation (replaces all shortcuts with "press again").
    pub pending_hint: Option<PendingHint>,
    /// External area for shortcuts in floating mode. When provided, shortcuts
    /// render here instead of at the bottom of `area`. Lets the caller align
    /// shortcuts with the surrounding layout (e.g. `layout.shortcuts`).
    pub shortcuts_area: Option<Rect>,
    /// Per-entry selectability: `non_selectable[i]` is true if entry `i` should
    /// be skipped during Up/Down navigation (e.g. section headers).
    /// Can be shorter than entry count (missing entries are selectable).
    pub non_selectable: &'a [bool],
    /// Non-selectable rows that still accept clicks (e.g. MCP section labels).
    /// Same length semantics as [`Self::non_selectable`].
    pub non_selectable_clickable: &'a [bool],
    /// Tab labels for a tab bar at the top. None = no tabs.
    pub tabs: Option<&'a [&'a str]>,
    /// Active tab index (only used if tabs is Some).
    pub active_tab: usize,
    /// Filter indicator in the search bar. None = no filter.
    pub filter_label: Option<&'a str>,
    /// Key hint for the filter (e.g., "f").
    pub filter_key_hint: Option<&'a str>,
    /// Whether the filter is active (not in its default state).
    pub filter_active: bool,
    /// Pinned single-line note rendered between the search/filter chrome and
    /// the first entry (e.g. the hidden-external sessions hint). Render-only:
    /// never part of the entry list, hit areas, or scrolling.
    pub header_note: Option<&'a str>,
    /// Custom action keys that produce `PickerOutcome::Action`.
    /// Each entry is `(key_char, description)` shown in shortcuts.
    pub action_keys: &'a [(char, &'a str)],
    /// If true, suppress the search bar entirely (and any text input into
    /// `state.query()`). The first content row is replaced by `config.title`
    /// rendered as a plain title. Useful for read-only cheatsheet modals.
    pub disable_search: bool,
    /// If true, render the bottom shortcuts bar using ONLY `config.shortcuts`
    /// (no auto-added nav / e:expand / y:copy / action_keys entries). The
    /// underlying key dispatch is unaffected — the keys still work, they
    /// just aren't advertised in the bar. Useful for compact bars that
    /// surface only the most important hints; the full list lives in the
    /// shortcuts cheatsheet modal.
    pub compact_bottom_bar: bool,
    /// If true (and `show_search_hint` is also true), only `/` or a click
    /// on the search bar activates search mode. Typing arbitrary
    /// printable characters is ignored instead of auto-starting a query.
    /// Default behavior (false) auto-activates search on any printable
    /// character — useful for fuzzy pickers but disruptive for tabs
    /// where letters double as action keys.
    pub search_only_on_slash: bool,
    /// When true, the picker opens/stays in nav mode (vim) whenever search is
    /// not active: j/k navigate and printable chars don't type; `i`/`/` enter
    /// search (unless search is disabled, or the char is bound as an action key
    /// on the picker, which takes precedence). Type-to-find pickers still open in
    /// input via [`PickerState::input_active`] and drop to nav on Esc. Callers
    /// gate this to `load_vim_mode()`. Mirrors scrollback vim-mode.
    pub vim_normal_first: bool,
}

/// Standard picker shortcuts: navigate, select, close.
///
/// Returns a static reference — no per-call allocation.
pub fn picker_shortcuts() -> &'static [HintItem] {
    static SHORTCUTS: LazyLock<Vec<HintItem>> = LazyLock::new(|| {
        vec![
            HintItem {
                keys: vec![],
                label: "nav".into(),
                custom_display: Some("\u{2191}/\u{2193}"),
                description: None,
                pinned: false,
            },
            HintItem::new(crate::key!(Enter), "select"),
            HintItem::new(crate::key!(Esc), "close"),
        ]
    });
    &SHORTCUTS
}

/// What happened after processing an input event.
pub enum PickerOutcome {
    /// User selected entry at this index (in the filtered entries list).
    Selected(usize),
    /// User pressed Esc / clicked close — caller decides how to handle.
    Closed,
    /// User wants to expand/toggle entry at this index (if config.expandable).
    Expand(usize),
    /// User wants to collapse entry at this index (Shift-E).
    Collapse(usize),
    /// User wants to copy entry at this index (if config.expandable).
    Copy(usize),
    /// User pressed Enter with a non-empty query but no matching entries.
    /// Caller can use the query string from `state.query()` to attempt a direct lookup.
    SubmitQuery,
    /// Visual state or query cursor changed; query text is unchanged.
    Changed,
    /// Query text changed; hosts should refresh filtering/search once.
    QueryChanged,
    /// Nothing changed.
    Unchanged,
    /// User switched to tab at given index.
    TabChanged(usize),
    /// User clicked/pressed the filter key — caller should cycle the filter.
    FilterCycled,
    /// Custom action key pressed.
    Action(char),
    /// Click on a non-selectable but clickable row (e.g. section label).
    NonSelectableClick(usize),
}

/// Hit areas for picker content (entries + scrollbar, no frame/tabs/search chrome).
///
/// Returned by [`render_picker_content`] so callers that render their own
/// frame/chrome (via [`super::modal_window`]) can wire up mouse hit-testing
/// for the content portion independently of the picker's built-in frame.
#[derive(Debug, Clone)]
pub struct PickerContentHitAreas {
    /// Per-item click rects (only selectable rows, not headers).
    pub item_rects: Vec<Rect>,
    /// Maps visual index → entry index: `item_rects[i]` corresponds to
    /// `entries[entry_indices[i]]`.
    pub entry_indices: Vec<usize>,
}

/// Render picker content (entries + scrollbar) into the given area.
///
/// This is the content-only portion of [`render_picker`], extracted so
/// callers that render their own frame/chrome (via [`super::modal_window`])
/// can draw entries into a provided content area without the picker's
/// built-in frame, tab bar, or search bar.
///
/// The existing [`render_picker`] calls this internally for backwards
/// compatibility with non-migrated callers.
#[allow(clippy::too_many_arguments)]
pub fn render_picker_content(
    buf: &mut Buffer,
    content_area: Rect,
    theme: &Theme,
    state: &mut PickerState,
    entries: &[PickerEntry<'_>],
    non_selectable: &[bool],
    non_selectable_clickable: &[bool],
    bg: Option<ratatui::style::Color>,
    loading: bool,
) -> PickerContentHitAreas {
    render_picker_content_inner(
        buf,
        content_area,
        theme,
        state,
        entries,
        non_selectable,
        non_selectable_clickable,
        bg,
        loading,
        0,
        None,
    )
}

/// Like [`render_picker_content`] but with an explicit scrollbar
/// x-position. When `scrollbar_x` is `Some(x)`, the scrollbar is
/// rendered at that column instead of `content_area.x + content_area.width - 1`.
/// Used by modals with h_pad to place the scrollbar flush against the border.
/// `loading_tick` animates the loading spinner (pass 0 for a static frame).
#[allow(clippy::too_many_arguments)]
pub fn render_picker_content_with_scrollbar_x(
    buf: &mut Buffer,
    content_area: Rect,
    theme: &Theme,
    state: &mut PickerState,
    entries: &[PickerEntry<'_>],
    non_selectable: &[bool],
    non_selectable_clickable: &[bool],
    bg: Option<ratatui::style::Color>,
    loading: bool,
    loading_tick: u64,
    scrollbar_x: u16,
) -> PickerContentHitAreas {
    render_picker_content_inner(
        buf,
        content_area,
        theme,
        state,
        entries,
        non_selectable,
        non_selectable_clickable,
        bg,
        loading,
        loading_tick,
        Some(scrollbar_x),
    )
}

#[allow(clippy::too_many_arguments)]
pub fn render_picker_in_modal(
    buf: &mut Buffer,
    content_area: Rect,
    inner_x: u16,
    inner_width: u16,
    theme: &Theme,
    state: &mut PickerState,
    entries: &[PickerEntry<'_>],
    non_selectable: &[bool],
    loading: bool,
) {
    let search_active = state.search_active;
    render_picker_in_modal_inner(
        buf,
        content_area,
        inner_x,
        inner_width,
        theme,
        state,
        entries,
        non_selectable,
        loading,
        search_active,
        true,
    );
}

/// Explicit search cursor/hint (ShortcutsHelp).
#[allow(clippy::too_many_arguments)]
pub fn render_picker_in_modal_inner(
    buf: &mut Buffer,
    content_area: Rect,
    inner_x: u16,
    inner_width: u16,
    theme: &Theme,
    state: &mut PickerState,
    entries: &[PickerEntry<'_>],
    non_selectable: &[bool],
    loading: bool,
    search_active: bool,
    show_search_hint: bool,
) {
    render_picker_search_bar(
        buf,
        content_area.x,
        content_area.y,
        content_area.width,
        theme,
        state,
        search_active,
        show_search_hint,
        Some(theme.bg_base),
    );
    let sep_y = content_area.y + 1;
    if sep_y < content_area.y + content_area.height {
        render_divider(buf, inner_x, sep_y, inner_width, theme, Some(theme.bg_base));
    }
    let entries_start_y = sep_y + 1;
    let search_bar_rect = Rect::new(content_area.x, content_area.y, content_area.width, 1);
    let entries_area = Rect {
        x: content_area.x,
        y: entries_start_y,
        width: content_area.width,
        height: content_area
            .height
            .saturating_sub(entries_start_y.saturating_sub(content_area.y)),
    };
    let content_hit = render_picker_content_with_scrollbar_x(
        buf,
        entries_area,
        theme,
        state,
        entries,
        non_selectable,
        &[],
        Some(theme.bg_base),
        loading,
        0,
        inner_x + inner_width - 1,
    );
    state.hit_areas = Some(PickerHitAreas {
        close_button: Rect::default(),
        search_bar: search_bar_rect,
        item_rects: content_hit.item_rects,
        entry_indices: content_hit.entry_indices,
        tab_rects: vec![],
        filter_rect: None,
    });
}

#[allow(clippy::too_many_arguments)]
fn render_picker_content_inner(
    buf: &mut Buffer,
    content_area: Rect,
    theme: &Theme,
    state: &mut PickerState,
    entries: &[PickerEntry<'_>],
    non_selectable: &[bool],
    non_selectable_clickable: &[bool],
    bg: Option<ratatui::style::Color>,
    loading: bool,
    loading_tick: u64,
    scrollbar_x_override: Option<u16>,
) -> PickerContentHitAreas {
    // Cleared each paint; set below if a row underlines its last description line.
    state.link_band = None;
    let is_clickable_non_sel = |i: usize| non_selectable_clickable.get(i).copied().unwrap_or(false);
    let empty_hit = PickerContentHitAreas {
        item_rects: vec![],
        entry_indices: vec![],
    };

    if content_area.height == 0 || content_area.width == 0 {
        return empty_hit;
    }

    // Loading state — animated dot spinner centered in the content area.
    if loading {
        let spinner_frames = crate::glyphs::dot_spinner_frames();
        let frame = spinner_frames[(loading_tick / 4) as usize % spinner_frames.len()];
        let msg = format!("{frame} Loading\u{2026}");
        let msg_style = Style::default().fg(theme.gray);
        let cx = content_area.x + content_area.width.saturating_sub(msg.width() as u16) / 2;
        let cy = content_area.y + content_area.height / 2;
        buf.set_string(cx, cy, msg, msg_style);
        return empty_hit;
    }

    // Empty state.
    if entries.is_empty() {
        let msg_style = Style::default()
            .fg(theme.gray_dim)
            .bg(picker_base_bg(bg, theme));
        buf.set_string(content_area.x, content_area.y, "  No matches", msg_style);
        return empty_hit;
    }

    let is_non_sel = |i: usize| non_selectable.get(i).copied().unwrap_or(false);

    // Ensure state.selected doesn't point to a non-selectable entry.
    {
        if is_non_sel(state.selected) {
            let mut s = state.selected;
            while is_non_sel(s) && s < entries.len().saturating_sub(1) {
                s += 1;
            }
            if !is_non_sel(s) {
                state.selected = s;
            }
        }
    }

    let max_visible = content_area.height as usize;
    let area_w = content_area.width;

    // Heights must use the same wrap width as rendering (narrower when scrollbar shown).
    let compute_heights = |wrap_w: u16| -> Vec<usize> {
        entries
            .iter()
            .enumerate()
            .map(|(idx, e)| match e {
                PickerEntry::Header { .. } => {
                    if idx == 0 {
                        1
                    } else {
                        2
                    }
                }
                PickerEntry::Row(row) => compute_row_height(row, wrap_w),
            })
            .collect()
    };

    let heights_full = compute_heights(area_w);
    let total_full: usize = heights_full.iter().sum();
    let needs_scroll = total_full > max_visible;
    let content_w = if needs_scroll {
        area_w.saturating_sub(1)
    } else {
        area_w
    };
    let entry_heights = if needs_scroll {
        compute_heights(content_w)
    } else {
        heights_full
    };
    let total_visual_rows: usize = entry_heights.iter().sum();

    // Find the visual row offset of the selected entry.
    let selected_visual_row: usize = entry_heights[..state.selected.min(entries.len())]
        .iter()
        .sum();

    // Compute scroll offset in visual rows.
    let scroll_visual = if let Some(offset) = state.scroll_offset {
        let max_scroll = total_visual_rows.saturating_sub(max_visible);
        offset.min(max_scroll)
    } else {
        let centered = selected_visual_row.saturating_sub(max_visible / 2);
        let max_scroll = total_visual_rows.saturating_sub(max_visible);
        centered.min(max_scroll)
    };
    state.scroll_offset = Some(scroll_visual);

    let content_w = content_w as usize;

    let mut item_rects = Vec::new();
    let mut entry_indices = Vec::new();
    let mut link_band: Option<(usize, std::ops::Range<u16>)> = None;

    // Skip entries until we've consumed scroll_visual visual rows.
    let mut visual_rows_consumed = 0usize;
    let mut start_entry = 0usize;
    for (idx, &h) in entry_heights.iter().enumerate() {
        if visual_rows_consumed + h > scroll_visual {
            start_entry = idx;
            break;
        }
        visual_rows_consumed += h;
        start_entry = idx + 1;
    }
    let rows_above = scroll_visual.saturating_sub(visual_rows_consumed);
    if rows_above > 0 {
        start_entry += 1;
    }
    let mut y = content_area.y;

    let mut first_visible = start_entry == 0;
    for (entry_idx, entry) in entries.iter().enumerate().skip(start_entry) {
        if y >= content_area.y + content_area.height {
            break;
        }
        let is_header = matches!(entry, PickerEntry::Header { .. });

        if is_header && !first_visible {
            y += 1;
            if y >= content_area.y + content_area.height {
                break;
            }
        }
        first_visible = false;

        let is_hovered = !is_header && state.hovered == Some(entry_idx);
        let remaining = (content_area.y + content_area.height).saturating_sub(y);
        let rendered = render_picker_entry(
            buf,
            content_area.x,
            y,
            content_w as u16,
            theme,
            entry,
            is_hovered,
            bg,
            remaining,
        );
        let rows_consumed = rendered.rows;
        if let Some(band) = rendered.link_band {
            link_band = Some((entry_idx, band));
        }

        if !is_header && (!is_non_sel(entry_idx) || is_clickable_non_sel(entry_idx)) {
            let row_rect = Rect {
                x: content_area.x,
                y,
                width: content_area.width,
                height: rows_consumed,
            };
            item_rects.push(row_rect);
            entry_indices.push(entry_idx);
        }
        y += rows_consumed;
    }
    state.link_band = link_band;

    // Scrollbar. (Globally suppressed in minimal mode via
    // `render::scrollbar::set_scrollbars_hidden`.)
    if needs_scroll {
        let sb_x = scrollbar_x_override.unwrap_or(content_area.x + content_area.width - 1);
        let sb_area = Rect::new(sb_x, content_area.y, 1, max_visible as u16);
        crate::render::scrollbar::render_scrollbar_styled(
            buf,
            Some(sb_area),
            total_visual_rows as u16,
            max_visible as u16,
            scroll_visual as u16,
            Style::default().bg(bg.unwrap_or(theme.bg_base)),
            Style::default()
                .fg(theme.gray_dim)
                .bg(bg.unwrap_or(theme.bg_base)),
        );
    }

    PickerContentHitAreas {
        item_rects,
        entry_indices,
    }
}

/// Render the unified picker. Caller provides filtered entries.
///
/// Returns hit areas for mouse interaction. The caller stores these in
/// `state.hit_areas` for use by `handle_picker_input`.
/// `loading_tick` animates the loading spinner (pass 0 for a static frame).
#[allow(clippy::too_many_arguments)]
pub fn render_picker(
    buf: &mut Buffer,
    area: Rect,
    theme: &Theme,
    state: &mut PickerState,
    entries: &[PickerEntry<'_>],
    config: &PickerConfig<'_>,
    loading: bool,
    loading_tick: u64,
) -> PickerHitAreas {
    let empty_hit = PickerHitAreas {
        close_button: Rect::default(),
        search_bar: Rect::default(),
        item_rects: vec![],
        entry_indices: vec![],
        tab_rects: vec![],
        filter_rect: None,
    };

    let frame = match render_picker_frame(
        buf,
        area,
        theme,
        state.mode.clone(),
        config.title,
        state.close_hovered,
    ) {
        Some(f) => f,
        None => return empty_hit,
    };

    let bg = match state.mode {
        PickerMode::Floating | PickerMode::Popup(_) => Some(theme.bg_base),
        PickerMode::FullScreen => None,
    };

    let is_fullscreen = matches!(state.mode, PickerMode::FullScreen);
    let raw_content = frame.content;

    // Reserve last row for shortcuts in fullscreen mode (rendered inside frame).
    // In floating mode, use shortcuts_area if provided, otherwise area bottom.
    let (shortcuts_y, shortcuts_x, shortcuts_w) = if config.shortcuts.is_some() {
        if is_fullscreen {
            (
                Some(raw_content.y + raw_content.height.saturating_sub(1)),
                raw_content.x,
                raw_content.width,
            )
        } else if let Some(sa) = config.shortcuts_area {
            (Some(sa.y), sa.x, sa.width)
        } else {
            (
                Some(area.y + area.height.saturating_sub(1)),
                area.x,
                area.width,
            )
        }
    } else {
        (None, 0, 0)
    };
    let content = if is_fullscreen && config.shortcuts.is_some() {
        Rect {
            height: raw_content.height.saturating_sub(1),
            ..raw_content
        }
    } else {
        raw_content
    };

    // ── Tab bar (optional) ──
    // When tabs are configured, render a tab bar on the first row of the content
    // area and advance the content origin downward.
    let mut close_button = frame.close_button;
    let mut tab_rects_out: Vec<Option<Rect>> = vec![];
    let content = if let Some(tabs) = config.tabs {
        let tab_hit = render_tab_bar(
            buf,
            content.x,
            content.y,
            content.width,
            theme,
            tabs,
            config.active_tab,
            state.close_hovered,
        );
        close_button = tab_hit.close_button;
        tab_rects_out = tab_hit.tab_rects;
        // Advance content below tab bar + divider.
        let tab_rows = 1u16; // tab bar
        let div_y = content.y + tab_rows;
        if div_y < content.y + content.height {
            render_divider(buf, content.x, div_y, content.width, theme, bg);
        }
        let used = tab_rows + 1; // tab bar + divider
        Rect {
            y: content.y + used,
            height: content.height.saturating_sub(used),
            ..content
        }
    } else {
        content
    };
    state.tab_hit_areas = if tab_rects_out.is_empty() {
        None
    } else {
        Some(tab_rects_out.clone())
    };

    let search_bar_rect = Rect::new(content.x, content.y, content.width, 1);

    // Search bar width: floating/popup mode subtracts space for close button in same row.
    // When tabs are present, close button is in the tab bar row, not the search row.
    let search_width = if config.tabs.is_some() {
        content.width // close button already in tab bar
    } else {
        match state.mode {
            PickerMode::Floating | PickerMode::Popup(_) => content.width.saturating_sub(4),
            PickerMode::FullScreen => content.width,
        }
    };

    if config.disable_search {
        // Replace the search bar row with a plain title (e.g. "Keyboard
        // Shortcuts" for the cheatsheet modal). Mirrors the search-bar
        // padding for visual consistency. Renders a [\u{2717}] close button at
        // the right edge so popup-mode pickers (which don't draw one in
        // the frame) still have a discoverable close affordance.
        let title_text = config.title.unwrap_or("");
        let title_style = Style::default()
            .fg(theme.text_primary)
            .add_modifier(Modifier::BOLD);
        let title_style = if let Some(c) = bg {
            title_style.bg(c)
        } else {
            title_style
        };
        let pad_style = if let Some(c) = bg {
            Style::default().bg(c)
        } else {
            Style::default()
        };
        // Leading 1-char pad to match search bar's " search:" alignment.
        buf.set_span(content.x, content.y, &Span::styled(" ", pad_style), 1);
        let title_w = title_text.width() as u16;
        let max_title_w = search_width.saturating_sub(1);
        let title_w = title_w.min(max_title_w);
        if title_w > 0 {
            buf.set_span(
                content.x + 1,
                content.y,
                &Span::styled(title_text, title_style),
                title_w,
            );
        }
        // Pad rest of the row with bg so the dim doesn't bleed through.
        let used = 1 + title_w;
        if used < search_width {
            let rest = search_width - used;
            let pad: String = " ".repeat(rest as usize);
            buf.set_span(
                content.x + used,
                content.y,
                &Span::styled(pad, pad_style),
                rest,
            );
        }
        // Render [\u{2717}] close button on the right edge of the title row,
        // matching the floating-frame close-button style. Stash its rect
        // in close_button so handle_picker_input can hit-test clicks.
        if config.tabs.is_none() {
            close_button = render_close_button(
                buf,
                content.x,
                content.y,
                content.width,
                theme,
                state.close_hovered,
                bg,
            );
        }
    } else {
        // Cursor tracks focus (`search_active`) for every picker — like the Settings pane; `show_search_hint` is input-only and no longer forces an always-on cursor.
        render_picker_search_bar(
            buf,
            content.x,
            content.y,
            search_width,
            theme,
            state,
            state.search_active,
            true,
            bg,
        );
    }

    // ── Filter indicator (optional) ──
    let filter_rect_out = if let Some(filter_label) = config.filter_label {
        let key_hint = config.filter_key_hint.unwrap_or("f");
        let rect = render_filter_indicator(
            buf,
            content.x,
            content.y,
            search_width,
            theme,
            filter_label,
            key_hint,
            config.filter_active,
            state.filter_hovered,
        );
        state.filter_area = Some(rect);
        Some(rect)
    } else {
        state.filter_area = None;
        None
    };

    // Divider below search.
    let sep_y = content.y + 1;
    if sep_y < content.y + content.height {
        render_divider(buf, content.x, sep_y, content.width, theme, bg);
    }

    let mut entries_start_y = sep_y + 1;

    // Pinned header note: reserve the first list row so it stays visible
    // regardless of list scroll.
    if let Some(note) = config.header_note
        && entries_start_y < content.y + content.height
    {
        let note_style = Style::default().fg(theme.gray_dim);
        let note_style = if let Some(c) = bg {
            note_style.bg(c)
        } else {
            note_style
        };
        buf.set_stringn(
            content.x + 1,
            entries_start_y,
            note,
            content.width.saturating_sub(1) as usize,
            note_style,
        );
        entries_start_y += 1;
    }

    // Delegate entry rendering + scrollbar to render_picker_content.
    let entries_area = Rect {
        x: content.x,
        y: entries_start_y,
        width: content.width,
        height: (content.y + content.height).saturating_sub(entries_start_y),
    };
    let content_hit = render_picker_content_inner(
        buf,
        entries_area,
        theme,
        state,
        entries,
        config.non_selectable,
        config.non_selectable_clickable,
        bg,
        loading,
        loading_tick,
        None,
    );
    let item_rects = content_hit.item_rects;
    let entry_indices = content_hit.entry_indices;

    // Shortcuts rendering (both modes).
    // Combine base shortcuts + expandable hints + action keys, OR — when
    // compact_bottom_bar is set — render only `config.shortcuts`.
    if let Some(sy) = shortcuts_y {
        let mut all_hints: Vec<HintItem> = Vec::new();
        // Base shortcuts (nav, select, close).
        if let Some(shortcuts) = config.shortcuts {
            all_hints.extend_from_slice(shortcuts);
        }
        // Surface `i` in vim nav mode so users discover how to start typing.
        if config.vim_normal_first && !state.search_active {
            all_hints.push(HintItem::new(crate::key!('i'), "search"));
        }
        // Expandable: add e=expand and y=copy hints.
        if config.expandable && !config.compact_bottom_bar {
            all_hints.push(HintItem {
                keys: vec![],
                label: "expand".into(),
                custom_display: Some("e/Shift+e"),
                description: None,
                pinned: false,
            });
            all_hints.push(HintItem {
                keys: vec![],
                label: "copy".into(),
                custom_display: Some("y"),
                description: None,
                pinned: false,
            });
        }
        // Action keys (per-tab: toggle, reload, add, etc.).
        if !config.compact_bottom_bar {
            for &(ch, desc) in config.action_keys {
                let display: &'static str = match ch {
                    ' ' => "space",
                    'a' => "a",
                    'd' => "d",
                    'e' => "e",
                    'f' => "f",
                    'i' => "i",
                    'r' => "r",
                    'u' => "u",
                    'x' => "x",
                    _ => "",
                };
                if !display.is_empty() {
                    all_hints.push(HintItem {
                        keys: vec![],
                        label: std::borrow::Cow::Owned(desc.to_string()),
                        custom_display: Some(display),
                        description: None,
                        pinned: false,
                    });
                }
            }
        }
        if !all_hints.is_empty() {
            // Inset by 1 cell so hints don't hug the left border.
            let shortcuts_rect = Rect::new(shortcuts_x + 1, sy, shortcuts_w.saturating_sub(1), 1);
            ShortcutsBar::new(&all_hints)
                .with_pending(config.pending_hint)
                .render(shortcuts_rect, buf);
        }
    }

    PickerHitAreas {
        close_button,
        search_bar: search_bar_rect,
        item_rects,
        entry_indices,
        tab_rects: tab_rects_out,
        filter_rect: filter_rect_out,
    }
}

/// First selectable index at or after `selected`.
///
/// Scans forward from the clamped selection, then restarts from the top when
/// needed. If every row is non-selectable, both scans stop at the last index,
/// so that last index is returned. Shared by picker input clamping and modal
/// render so highlight and footer stay aligned.
pub fn first_selectable_index(
    selected: usize,
    entry_count: usize,
    non_selectable: &[bool],
) -> usize {
    if entry_count == 0 {
        return 0;
    }
    let is_non_sel = |i: usize| non_selectable.get(i).copied().unwrap_or(false);
    let mut s = selected.min(entry_count - 1);
    while is_non_sel(s) && s < entry_count - 1 {
        s += 1;
    }
    if is_non_sel(s) {
        s = 0;
        while is_non_sel(s) && s < entry_count - 1 {
            s += 1;
        }
    }
    s
}

/// Clamp selection to a selectable row after a host changes the picker entries.
pub fn clamp_picker_selection(
    state: &mut PickerState,
    entry_count: usize,
    non_selectable: &[bool],
) {
    state.selected = first_selectable_index(state.selected, entry_count, non_selectable);
}

/// Handle one picker event; hosts re-filter only after [`PickerOutcome::QueryChanged`].
pub fn handle_picker_input(
    ev: &crossterm::event::Event,
    state: &mut PickerState,
    entry_count: usize,
    config: &PickerConfig<'_>,
) -> PickerOutcome {
    use crossterm::event::{Event, KeyCode, KeyEventKind, MouseEventKind};
    let is_non_sel = |i: usize| config.non_selectable.get(i).copied().unwrap_or(false);
    let is_clickable_non_sel = |i: usize| {
        config
            .non_selectable_clickable
            .get(i)
            .copied()
            .unwrap_or(false)
    };
    // Clamp selected to valid range — entries may have changed since last input
    // (e.g., query filter reduced the list).
    clamp_picker_selection(state, entry_count, config.non_selectable);

    // Precompute first/last selectable for boundary-aware Up/Down navigation
    // (search focus at edges, skipping any non-selectable headers).
    let first_selectable = if entry_count == 0 {
        0
    } else {
        let mut s = 0usize;
        while is_non_sel(s) && s + 1 < entry_count {
            s += 1;
        }
        if is_non_sel(s) { 0 } else { s }
    };
    let last_selectable = if entry_count == 0 {
        0
    } else {
        let mut s = entry_count - 1;
        while is_non_sel(s) && s > 0 {
            s -= 1;
        }
        if is_non_sel(s) { entry_count - 1 } else { s }
    };

    // ── Mouse handling (hit area based) ──
    if let Event::Mouse(mouse) = ev
        && let Some(ref hit) = state.hit_areas
    {
        let pos = ratatui::layout::Position::new(mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                if hit.close_button.contains(pos) {
                    return PickerOutcome::Closed;
                }
                if hit.search_bar.contains(pos) {
                    state.search_active = true;
                    state.tabs_focused = false;
                    state.selection_hidden = true;
                    return PickerOutcome::Changed;
                }
                // Tab click.
                for (i, tab_rect) in hit.tab_rects.iter().enumerate() {
                    if let Some(rect) = tab_rect
                        && rect.contains(pos)
                        && i != config.active_tab
                    {
                        return PickerOutcome::TabChanged(i);
                    }
                }
                // Filter click.
                if let Some(filter_rect) = hit.filter_rect
                    && filter_rect.contains(pos)
                {
                    return PickerOutcome::FilterCycled;
                }
                for (i, rect) in hit.item_rects.iter().enumerate() {
                    if rect.contains(pos)
                        && let Some(&entry_idx) = hit.entry_indices.get(i)
                    {
                        state.tabs_focused = false;
                        if is_non_sel(entry_idx) && is_clickable_non_sel(entry_idx) {
                            return PickerOutcome::NonSelectableClick(entry_idx);
                        }
                        state.selected = entry_idx;
                        state.selection_hidden = false;
                        return PickerOutcome::Selected(entry_idx);
                    }
                }
                return PickerOutcome::Changed; // consume click
            }
            MouseEventKind::Moved => {
                let mut changed = false;
                let on_close = hit.close_button.contains(pos);
                if on_close != state.close_hovered {
                    state.close_hovered = on_close;
                    changed = true;
                }
                // Filter hover.
                if let Some(filter_area) = state.filter_area {
                    let on_filter = filter_area.contains(pos);
                    if on_filter != state.filter_hovered {
                        state.filter_hovered = on_filter;
                        changed = true;
                    }
                }
                // Row hover.
                let mut on_item = None;
                for (vis_idx, rect) in hit.item_rects.iter().enumerate() {
                    if rect.contains(pos) {
                        let entry_idx = hit.entry_indices.get(vis_idx).copied().unwrap_or(vis_idx);
                        on_item = Some(entry_idx);
                        break;
                    }
                }
                if on_item != state.hovered {
                    state.hovered = on_item;
                    if let Some(idx) = on_item
                        && !is_non_sel(idx)
                    {
                        state.selected = idx;
                        state.tabs_focused = false;
                        state.selection_hidden = false;
                    }
                    changed = true;
                }
                return if changed {
                    PickerOutcome::Changed
                } else {
                    PickerOutcome::Unchanged
                };
            }
            MouseEventKind::ScrollDown => {
                let current = state.scroll_offset.unwrap_or(0);
                state.scroll_offset = Some(current + 3);
                state.hovered = None;
                return PickerOutcome::Changed;
            }
            MouseEventKind::ScrollUp => {
                let current = state.scroll_offset.unwrap_or(0);
                state.scroll_offset = Some(current.saturating_sub(3));
                state.hovered = None;
                return PickerOutcome::Changed;
            }
            _ => return PickerOutcome::Changed,
        }
    }

    fn finish_query_edit(
        state: &mut PickerState,
        outcome: LineEditOutcome,
    ) -> Option<PickerOutcome> {
        match outcome {
            LineEditOutcome::Unhandled => None,
            LineEditOutcome::HandledNoChange | LineEditOutcome::CursorChanged => {
                Some(PickerOutcome::Changed)
            }
            LineEditOutcome::TextChanged => {
                state.selected = 0;
                state.selection_hidden = false;
                state.scroll_offset = None;
                state.tabs_focused = false;
                state.expanded.clear();
                Some(PickerOutcome::QueryChanged)
            }
        }
    }

    fn is_plain_query_character(key: &KeyEvent) -> bool {
        matches!(key.code, KeyCode::Char(_))
            && (key.modifiers.is_empty() || key.modifiers == crossterm::event::KeyModifiers::SHIFT)
    }

    fn is_legacy_alt_word_key(key: &KeyEvent) -> bool {
        matches!(key.code, KeyCode::Char('b' | 'f'))
            && key.modifiers == crossterm::event::KeyModifiers::ALT
    }

    fn handle_paste(
        state: &mut PickerState,
        text: impl AsRef<str>,
        config: &PickerConfig<'_>,
    ) -> PickerOutcome {
        // vim_normal_first: printables don't type until search is entered, so a
        // paste while in nav mode is ignored.
        if config.vim_normal_first && !state.search_active {
            return PickerOutcome::Unchanged;
        }
        let outcome = state.paste_query(text.as_ref());
        if outcome == LineEditOutcome::TextChanged && config.show_search_hint {
            state.search_active = true;
        }
        finish_query_edit(state, outcome).unwrap_or(PickerOutcome::Unchanged)
    }

    // ── Key handling ──
    if let Event::Key(key) = ev {
        if key.kind == KeyEventKind::Release {
            return PickerOutcome::Unchanged;
        }

        if crate::input::key::is_paste_key(key) || crate::input::key::is_inline_paste_key(key) {
            if let Some(text) = crate::clipboard::system_clipboard_get() {
                if !crate::clipboard::clipboard_text_is_pasteable(Some(&text)) {
                    crate::clipboard::log_paste_key_empty_host_clipboard("picker");
                }
                return handle_paste(state, text, config);
            }
            crate::clipboard::log_paste_key_empty_host_clipboard("picker");
            return PickerOutcome::Unchanged;
        }

        // Search mode (currently active). Also reachable for vim_normal_first
        // pickers without a search hint, so typing/Esc/Backspace work once
        // search is entered via `i`/`/`.
        if (config.show_search_hint || config.vim_normal_first) && state.search_active {
            if key.code == KeyCode::Esc {
                let query_changed = config.vim_normal_first && !state.query().is_empty();
                state.search_active = false;
                // vim_normal_first: Esc leaves search for nav mode and clears the
                // query in one step (mirrors scrollback vim-mode).
                if config.vim_normal_first {
                    state.clear_query();
                }
                return if query_changed {
                    PickerOutcome::QueryChanged
                } else {
                    PickerOutcome::Changed
                };
            }
            if let Some(tabs) = config.tabs {
                let tab_count = tabs.len();
                if tab_count > 1 {
                    let shift = key
                        .modifiers
                        .contains(crossterm::event::KeyModifiers::SHIFT);
                    if key.code == KeyCode::Tab && !shift {
                        return PickerOutcome::TabChanged((config.active_tab + 1) % tab_count);
                    }
                    if key.code == KeyCode::BackTab || (key.code == KeyCode::Tab && shift) {
                        return PickerOutcome::TabChanged(
                            (config.active_tab + tab_count - 1) % tab_count,
                        );
                    }
                }
            }
            // Up/Down/Enter exit search mode, then (for arrows) navigate to the
            // adjacent "edge" item to support cycling between search and list.
            if key.code == KeyCode::Up || key.code == KeyCode::Down || key.code == KeyCode::Enter {
                state.search_active = false;
                state.selection_hidden = false;
                if entry_count > 0 {
                    state.selected = state.selected.min(entry_count - 1);
                }
                if key.code == KeyCode::Up || key.code == KeyCode::Down {
                    if key.code == KeyCode::Down {
                        if entry_count > 0 {
                            state.selected = first_selectable;
                        }
                    } else {
                        // Up from search: if tabs configured, move focus to the
                        // tab bar region so Left/Right can cycle tabs.
                        if config.tabs.is_some() {
                            state.tabs_focused = true;
                        } else if entry_count > 0 {
                            state.selected = last_selectable;
                        }
                    }
                    state.hovered = None;
                    state.scroll_offset = None;
                    return PickerOutcome::Changed;
                }
            }
            if config.expandable && state.query().is_empty() {
                if key.code == KeyCode::Right {
                    return PickerOutcome::Expand(state.selected);
                }
                if key.code == KeyCode::Left {
                    return PickerOutcome::Collapse(state.selected);
                }
            }
            if key.code != KeyCode::Enter {
                let outcome = state.edit_query(key);
                if let Some(outcome) = finish_query_edit(state, outcome) {
                    return outcome;
                }
                return PickerOutcome::Unchanged;
            }
        }

        // j/k alias to Down/Up for nav when a search hint is shown (hint mode)
        // or under vim_normal_first (always-active pickers included).
        let jk_navigates = config.show_search_hint || config.vim_normal_first;

        // When the tabs region is focused (reached by Up arrow from search
        // or top of list when tabs are configured), handle arrows for tab
        // cycling (delegated to the tabs block above), boundary Down/Up to
        // move focus back into content/search, and printable chars to start
        // a query (exiting tabs focus).
        if state.tabs_focused {
            let is_down_j = jk_navigates
                && !state.search_active
                && key.code == KeyCode::Char('j')
                && key.modifiers.is_empty();
            let is_down = key.code == KeyCode::Down || is_down_j;
            if is_down {
                state.tabs_focused = false;
                // Down from the tabs region goes to search (so the full
                // arrow cycle is: list bottom → tabs → search → list top).
                // Under vim_normal_first nav never opens search, so j/Down from
                // the tabs region moves into the list instead.
                if config.show_search_hint && !config.vim_normal_first {
                    state.search_active = true;
                    state.selection_hidden = true;
                } else if entry_count > 0 {
                    state.selected = first_selectable;
                    state.selection_hidden = false;
                }
                state.hovered = None;
                state.scroll_offset = None;
                return PickerOutcome::Changed;
            }

            let is_up_k = jk_navigates
                && !state.search_active
                && key.code == KeyCode::Char('k')
                && key.modifiers.is_empty();
            let is_up = key.code == KeyCode::Up || is_up_k;
            if is_up {
                state.tabs_focused = false;
                // Wrap "up" from tabs to the bottom of the list (if any).
                if entry_count > 0 {
                    state.selected = last_selectable;
                }
                state.selection_hidden = false;
                state.hovered = None;
                state.scroll_offset = None;
                return PickerOutcome::Changed;
            }

            if key.code == KeyCode::Enter {
                // "Activating" the tab bar does nothing special; leave focus
                // or drop it. Drop it to return to content.
                state.tabs_focused = false;
                return PickerOutcome::Changed;
            }

            // Printable characters while tabs focused: exit focus and start
            // a search query (mirrors behavior from non-active search hint).
            if config.show_search_hint && !state.search_active {
                if key.code == KeyCode::Char('/') && key.modifiers.is_empty() {
                    state.tabs_focused = false;
                    state.search_active = true;
                    return PickerOutcome::Changed;
                }
                if !config.search_only_on_slash
                    && !config.vim_normal_first
                    && is_legacy_alt_word_key(key)
                {
                    return PickerOutcome::Changed;
                }
                if !config.search_only_on_slash
                    && !config.vim_normal_first
                    && is_plain_query_character(key)
                {
                    let outcome = state.edit_query(key);
                    if outcome == LineEditOutcome::TextChanged {
                        state.tabs_focused = false;
                        state.search_active = true;
                    }
                    if let Some(outcome) = finish_query_edit(state, outcome) {
                        return outcome;
                    }
                }
            }

            // For other keys (including action keys, Esc, etc.) while tabs
            // focused we fall through so the normal paths (Tab switch,
            // action keys, Esc close, etc.) can still apply. L/R are
            // handled in the tabs block later and will have returned already.
        }

        // Ctrl+F: toggle mode.
        if key.code == KeyCode::Char('f')
            && key
                .modifiers
                .contains(crossterm::event::KeyModifiers::CONTROL)
        {
            state.mode = state.mode.clone().toggle();
            return PickerOutcome::Changed;
        }

        // Esc.
        if key.code == KeyCode::Esc {
            if config.esc_clears_query && !state.query().is_empty() {
                state.clear_query();
                state.tabs_focused = false;
                return PickerOutcome::QueryChanged;
            }
            state.tabs_focused = false;
            return PickerOutcome::Closed;
        }

        // Enter.
        if key.code == KeyCode::Enter {
            if entry_count > 0 && !is_non_sel(state.selected) {
                return PickerOutcome::Selected(state.selected);
            }
            if entry_count == 0 && !state.query().is_empty() {
                return PickerOutcome::SubmitQuery;
            }
            return PickerOutcome::Changed;
        }

        // Navigation: Down (+ j in hint or vim_normal_first mode).
        let is_down_j = jk_navigates
            && !state.search_active
            && key.code == KeyCode::Char('j')
            && key.modifiers.is_empty();
        let is_down = key.code == KeyCode::Down || is_down_j;
        if is_down {
            let at_bottom = entry_count == 0 || state.selected == last_selectable;

            // Under vim_normal_first, j/Down clamp at the list bottom (the field
            // doc promises only `i`/`/` open search; never auto-jump to tabs).
            if at_bottom && !state.search_active && !config.vim_normal_first {
                // When a tab bar is present, Down from the last list item
                // moves focus to the tabs region (so Left/Right can cycle
                // tabs) instead of the search bar.
                if config.tabs.is_some() {
                    state.tabs_focused = true;
                    state.search_active = false;
                    state.hovered = None;
                    state.scroll_offset = None;
                    return PickerOutcome::Changed;
                }
                if config.show_search_hint {
                    state.search_active = true;
                    state.selection_hidden = true;
                    state.hovered = None;
                    state.scroll_offset = None;
                    return PickerOutcome::Changed;
                }
            }

            if entry_count == 0 {
                return PickerOutcome::Changed;
            }

            let mut new_sel = (state.selected + 1).min(entry_count - 1);
            while is_non_sel(new_sel) && new_sel < entry_count - 1 {
                new_sel += 1;
            }
            if is_non_sel(new_sel) {
                new_sel = state.selected;
            }
            state.selected = new_sel;
            state.selection_hidden = false;
            state.hovered = None;
            state.scroll_offset = None;
            return PickerOutcome::Changed;
        }

        // Navigation: Up (+ k in hint or vim_normal_first mode).
        let is_up_k = jk_navigates
            && !state.search_active
            && key.code == KeyCode::Char('k')
            && key.modifiers.is_empty();
        let is_up = key.code == KeyCode::Up || is_up_k;
        if is_up {
            let at_top = entry_count == 0 || state.selected == first_selectable;

            // Under vim_normal_first, k/Up clamp at the list top (only `i`/`/`
            // open search).
            if at_top && config.show_search_hint && !state.search_active && !config.vim_normal_first
            {
                state.search_active = true;
                state.selection_hidden = true;
                state.hovered = None;
                state.scroll_offset = None;
                return PickerOutcome::Changed;
            }

            if entry_count == 0 {
                return PickerOutcome::Changed;
            }

            let mut new_sel = state.selected.saturating_sub(1);
            while is_non_sel(new_sel) && new_sel > 0 {
                new_sel = new_sel.saturating_sub(1);
            }
            if is_non_sel(new_sel) {
                new_sel = state.selected;
            }
            state.selected = new_sel;
            state.selection_hidden = false;
            state.hovered = None;
            state.scroll_offset = None;
            return PickerOutcome::Changed;
        }

        // ── Custom action keys (checked first — override built-in expand/copy) ──
        // Only when not in search mode.
        if !state.search_active {
            for &(action_char, _) in config.action_keys {
                if key.code == KeyCode::Char(action_char) && key.modifiers.is_empty() {
                    return PickerOutcome::Action(action_char);
                }
            }
        }

        // Expandable actions (y: copy, e/→: expand, E/←: collapse, space: toggle).
        if config.expandable && entry_count > 0 && !state.search_active && !state.tabs_focused {
            if key.code == KeyCode::Char('y') && key.modifiers.is_empty() {
                return PickerOutcome::Copy(state.selected);
            }
            if key.code == KeyCode::Char('e') && key.modifiers.is_empty() {
                return PickerOutcome::Expand(state.selected);
            }
            if key.code == KeyCode::Char('E') {
                return PickerOutcome::Collapse(state.selected);
            }
            // Outside search, arrows always act on the selected row; the
            // retained query only filters which rows are visible.
            if key.code == KeyCode::Right {
                return PickerOutcome::Expand(state.selected);
            }
            if key.code == KeyCode::Left {
                return PickerOutcome::Collapse(state.selected);
            }
            if key.code == KeyCode::Char(' ') && key.modifiers.is_empty() {
                // Space triggers the 'toggle' action if one is configured,
                // otherwise falls through.
                if config.action_keys.iter().any(|&(_, desc)| desc == "toggle") {
                    return PickerOutcome::Action(' ');
                }
            }
        }

        // ── Tab switching ──
        // Tab/Shift-Tab (and BackTab) always cycle tabs when configured
        // (and not in search).
        //
        // When the tab bar region has been focused via Up/Down arrows
        // (`tabs_focused`), Left/Right (and h/l) also cycle tabs. This
        // lets callers make L/R expand/collapse the *selected list item*
        // by default, while still allowing arrow nav of the tab bar after
        // the user explicitly arrows "up" to it.
        if let Some(tabs) = config.tabs {
            let tab_count = tabs.len();
            if tab_count > 1 && !state.search_active {
                // Dedicated L/R handling only when the tabs list itself is selected.
                if state.tabs_focused {
                    if key.code == KeyCode::Left || key.code == KeyCode::Char('h') {
                        return PickerOutcome::TabChanged(
                            (config.active_tab + tab_count - 1) % tab_count,
                        );
                    }
                    if key.code == KeyCode::Right || key.code == KeyCode::Char('l') {
                        return PickerOutcome::TabChanged((config.active_tab + 1) % tab_count);
                    }
                }
                let forward = key.code == KeyCode::Tab
                    && !key
                        .modifiers
                        .contains(crossterm::event::KeyModifiers::SHIFT);
                let backward = (key.code == KeyCode::Tab
                    && key
                        .modifiers
                        .contains(crossterm::event::KeyModifiers::SHIFT))
                    || key.code == KeyCode::BackTab;
                if forward {
                    return PickerOutcome::TabChanged((config.active_tab + 1) % tab_count);
                }
                if backward {
                    return PickerOutcome::TabChanged(
                        (config.active_tab + tab_count - 1) % tab_count,
                    );
                }
            }
        }

        // ── Filter cycling ──
        // 'f' key (not in search mode) toggles the filter.
        if config.filter_label.is_some()
            && !state.search_active
            && key.code == KeyCode::Char('f')
            && key.modifiers.is_empty()
        {
            return PickerOutcome::FilterCycled;
        }
        // vim_normal_first opens the picker in nav mode; `/` or `i` is the only
        // way into search (printable chars don't type until then). Mirrors
        // scrollback vim-mode. This intentionally shadows the hint `/` handler
        // below under vim and also covers always-active pickers (no search hint).
        if config.vim_normal_first
            && !state.search_active
            && !config.disable_search
            && key.modifiers.is_empty()
            && matches!(key.code, KeyCode::Char('i') | KeyCode::Char('/'))
        {
            state.search_active = true;
            state.tabs_focused = false;
            return PickerOutcome::Changed;
        }
        if !config.show_search_hint && !config.disable_search && !config.vim_normal_first {
            if key.code == KeyCode::Char('/')
                && key.modifiers.is_empty()
                && state.query().is_empty()
                && !state.search_active
            {
                state.search_active = true;
                state.tabs_focused = false;
                return PickerOutcome::Changed;
            }
            let outcome = state.edit_query(key);
            if let Some(outcome) = finish_query_edit(state, outcome) {
                return outcome;
            }
        }
        // Hint-based search, not active.
        if config.show_search_hint && !state.search_active {
            if key.code == KeyCode::Char('/') && key.modifiers.is_empty() {
                state.search_active = true;
                return PickerOutcome::Changed;
            }
            if !config.search_only_on_slash
                && !config.vim_normal_first
                && is_legacy_alt_word_key(key)
            {
                return PickerOutcome::Changed;
            }
            if !config.search_only_on_slash
                && !config.vim_normal_first
                && is_plain_query_character(key)
            {
                let outcome = state.edit_query(key);
                if outcome == LineEditOutcome::TextChanged {
                    state.search_active = true;
                    state.tabs_focused = false;
                }
                if let Some(outcome) = finish_query_edit(state, outcome) {
                    return outcome;
                }
            }
        }

        return PickerOutcome::Unchanged; // unhandled key — no state change
    }

    // ── Paste ──
    if let Event::Paste(text) = ev {
        return handle_paste(state, text, config);
    }

    if matches!(ev, crossterm::event::Event::Resize(_, _)) {
        return PickerOutcome::Changed;
    }

    PickerOutcome::Unchanged
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    // Minimal picker config for input tests. Flip `vim_normal_first` per-test to
    // exercise the dormant nav mode; everything else mirrors a basic picker.
    fn cfg(show_search_hint: bool, vim_normal_first: bool) -> PickerConfig<'static> {
        PickerConfig {
            title: None,
            show_search_hint,
            expandable: false,
            esc_clears_query: true,
            shortcuts: None,
            pending_hint: None,
            shortcuts_area: None,
            non_selectable: &[],
            non_selectable_clickable: &[],
            tabs: None,
            active_tab: 0,
            filter_label: None,
            filter_key_hint: None,
            filter_active: false,
            header_note: None,
            action_keys: &[],
            disable_search: false,
            compact_bottom_bar: false,
            search_only_on_slash: false,
            vim_normal_first,
        }
    }

    fn press(c: char) -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
    }

    fn press_esc() -> Event {
        Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
    }

    fn press_up() -> Event {
        Event::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))
    }

    #[test]
    fn flag_off_always_active_char_types() {
        // show_search_hint=false: always-active search still types as today.
        let config = cfg(false, false);
        let mut state = PickerState::default();
        let outcome = handle_picker_input(&press('a'), &mut state, 3, &config);
        assert!(matches!(outcome, PickerOutcome::QueryChanged));
        assert_eq!(state.query(), "a");
    }

    #[test]
    fn flag_off_hint_char_auto_activates_search() {
        // show_search_hint=true: a non-jk char auto-activates search as today.
        let config = cfg(true, false);
        let mut state = PickerState::default();
        let outcome = handle_picker_input(&press('a'), &mut state, 3, &config);
        assert!(matches!(outcome, PickerOutcome::QueryChanged));
        assert!(state.search_active);
        assert_eq!(state.query(), "a");
    }

    #[test]
    fn always_active_slash_on_empty_query_focuses_search_without_typing() {
        // show_search_hint=false picker (e.g. the `/docs` how-to picker) shows
        // the "/ to search" placeholder while unfocused: pressing the
        // advertised `/` must focus search, not type a literal `/`.
        let config = cfg(false, false);
        let mut state = PickerState::default();
        let outcome = handle_picker_input(&press('/'), &mut state, 3, &config);
        assert!(matches!(outcome, PickerOutcome::Changed));
        assert!(state.search_active);
        assert!(state.query().is_empty());
        // Typing after the activation chord filters normally.
        let outcome = handle_picker_input(&press('t'), &mut state, 3, &config);
        assert!(matches!(outcome, PickerOutcome::QueryChanged));
        assert_eq!(state.query(), "t");
    }

    #[test]
    fn always_active_slash_mid_query_inserts_literal_slash() {
        // A `/` typed into a non-empty query is plausible query text
        // (path-like searches) and must keep inserting.
        let config = cfg(false, false);
        let mut state = PickerState::default();
        handle_picker_input(&press('a'), &mut state, 3, &config);
        handle_picker_input(&press('b'), &mut state, 3, &config);
        assert_eq!(state.query(), "ab");
        let outcome = handle_picker_input(&press('/'), &mut state, 3, &config);
        assert!(matches!(outcome, PickerOutcome::QueryChanged));
        assert_eq!(state.query(), "ab/");
        assert!(!state.search_active);
    }

    #[test]
    fn always_active_slash_inserts_when_search_already_focused() {
        // Pickers that open input-focused (`input_active()`: command palette,
        // arg picker; the dashboard location picker) never show the
        // "/ to search" placeholder, so a leading `/` there is query text
        // (e.g. an absolute path) and must insert even on an empty query.
        let config = cfg(false, false);
        let mut state = PickerState::input_active();
        let outcome = handle_picker_input(&press('/'), &mut state, 3, &config);
        assert!(matches!(outcome, PickerOutcome::QueryChanged));
        assert_eq!(state.query(), "/");
    }

    #[test]
    fn hint_slash_still_activates_search_without_typing() {
        // show_search_hint=true pickers keep their existing `/` handling.
        let config = cfg(true, false);
        let mut state = PickerState::default();
        let outcome = handle_picker_input(&press('/'), &mut state, 3, &config);
        assert!(matches!(outcome, PickerOutcome::Changed));
        assert!(state.search_active);
        assert!(state.query().is_empty());
        let outcome = handle_picker_input(&press('t'), &mut state, 3, &config);
        assert!(matches!(outcome, PickerOutcome::QueryChanged));
        assert_eq!(state.query(), "t");
    }

    #[test]
    fn search_bar_cursor_visible_only_when_search_active() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;

        // A `show_search_hint: false` picker (command palette / arg-picker family):
        // the cursor must track focus (`search_active`), not render always-on.
        let theme = Theme::current();
        let config = cfg(false, false);
        let area = Rect::new(0, 0, 60, 16);

        // Render the picker; report whether the search row drew a cursor (an
        // inverse-video cell whose bg == text_primary) plus its visible text.
        let render_search_row = |search_active: bool| -> (bool, String) {
            let mut state = PickerState::with_mode(PickerMode::FullScreen);
            state.search_active = search_active;
            let mut buf = Buffer::empty(area);
            let hit = render_picker(&mut buf, area, &theme, &mut state, &[], &config, false, 0);
            let y = hit.search_bar.y;
            let mut has_cursor = false;
            let mut text = String::new();
            for x in hit.search_bar.x..hit.search_bar.x + hit.search_bar.width {
                if let Some(cell) = buf.cell((x, y)) {
                    text.push_str(cell.symbol());
                    if cell.bg == theme.text_primary {
                        has_cursor = true;
                    }
                }
            }
            (has_cursor, text)
        };

        let (focused_cursor, _) = render_search_row(true);
        assert!(
            focused_cursor,
            "focused search bar (search_active) should render a cursor",
        );

        let (unfocused_cursor, unfocused_text) = render_search_row(false);
        assert!(
            !unfocused_cursor,
            "unfocused search bar must not render a cursor",
        );
        assert!(
            unfocused_text.contains("/ to search"),
            "unfocused search bar should show the `/ to search` placeholder, got {unfocused_text:?}",
        );
    }

    #[test]
    fn viewport_search_bar_reserves_counter_without_text_or_cursor_overlap() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;

        let theme = Theme::current();
        let width = 20u16;
        let counter_width = "12/34".width() as u16;
        let layout = search_bar_layout(width, counter_width);
        assert_eq!(layout.input_width(), 5);
        assert_eq!(layout.trailing_width(), counter_width);

        let mut editor = LineEditor::default();
        editor.set_text("123456789中e\u{301}👩🏽\u{200d}💻z");
        let viewport = editor.viewport(layout.input_width());
        let mut buffer = Buffer::empty(Rect::new(0, 0, width, 1));
        buffer.set_string(0, 0, "#".repeat(width as usize), Style::default());
        render_search_bar_with_viewport(
            &mut buffer,
            0,
            0,
            layout,
            &theme,
            editor.text(),
            true,
            false,
            None,
            viewport,
        );

        for x in layout.render_width..width {
            assert_eq!(
                buffer[(x, 0)].symbol(),
                "#",
                "reserved counter cell {x} was overwritten",
            );
        }
        let cursor_x = (0..layout.render_width)
            .find(|x| buffer[(*x, 0)].bg == theme.text_primary)
            .expect("cursor inside search render width");
        assert!(cursor_x < layout.render_width);
    }

    #[test]
    fn narrow_search_bar_omits_real_counters_to_preserve_caret_cell() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;

        let theme = Theme::current();
        for counter in ["no matches", "bad pattern", "12/34"] {
            let counter_width = counter.width() as u16;
            let width = SEARCH_BAR_LABEL.len() as u16 + counter_width + SEARCH_BAR_TRAILING_GAP;
            let layout = search_bar_layout(width, counter_width);
            assert_eq!(
                layout.trailing_width(),
                0,
                "{counter:?} must be omitted without one editor cell",
            );
            assert!(layout.input_width() >= 1);

            let mut editor = LineEditor::default();
            editor.set_text("long-query-中e\u{301}👩🏽\u{200d}💻");
            let viewport = editor.viewport(layout.input_width());
            let mut buffer = Buffer::empty(Rect::new(0, 0, width, 1));
            render_search_bar_with_viewport(
                &mut buffer,
                0,
                0,
                layout,
                &theme,
                editor.text(),
                true,
                false,
                None,
                viewport,
            );
            let cursor_x = (0..width)
                .find(|x| buffer[(*x, 0)].bg == theme.text_primary)
                .expect("active query keeps a visible caret");
            assert!(cursor_x < width);

            let fit_layout = search_bar_layout(width + 1, counter_width);
            assert_eq!(fit_layout.trailing_width(), counter_width);
            assert_eq!(fit_layout.input_width(), 1);
            let viewport = editor.viewport(fit_layout.input_width());
            let mut fit_buffer = Buffer::empty(Rect::new(0, 0, width + 1, 1));
            render_search_bar_with_viewport(
                &mut fit_buffer,
                0,
                0,
                fit_layout,
                &theme,
                editor.text(),
                true,
                false,
                None,
                viewport,
            );
            let cursor_x = (0..fit_layout.render_width)
                .find(|x| fit_buffer[(*x, 0)].bg == theme.text_primary)
                .expect("just-fit counter preserves one caret cell");
            assert!(cursor_x < fit_layout.render_width);
        }
    }

    #[test]
    fn underline_last_desc_underlines_only_the_link_line() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;

        let theme = Theme::current();
        // Wide enough that each description line fits on a single visual row:
        // y+0 = label, y+1 = instruction, y+2 = bracket-highlighted URL.
        let area = Rect::new(0, 0, 60, 6);
        let desc: &[&str] = &["some instruction text", "[example.com/link]"];

        let render = |underline_last_desc: bool| -> (Buffer, Option<std::ops::Range<u16>>) {
            let row = PickerRow {
                label: "Group header",
                right_label: "",
                selected: false,
                expanded: true,
                fields: &[],
                description_lines: desc,
                summary_lines: &[],
                dimmed: false,
                indent: 0,
                badge: "",
                badge_color: None,
                collapsible: true,
                underline_last_desc,
            };
            let mut buf = Buffer::empty(area);
            let rendered = render_picker_row(
                &mut buf,
                area.x,
                area.y,
                area.width,
                &theme,
                &row,
                false,
                None,
                area.height,
            );
            (buf, rendered.link_band)
        };

        // Rows (relative to area top) that have any underlined cell.
        let underlined_rows = |buf: &Buffer| -> Vec<u16> {
            (area.y..area.y + area.height)
                .filter(|&y| {
                    (0..area.width).any(|x| {
                        buf.cell((x, y))
                            .map(|c| c.modifier.contains(Modifier::UNDERLINED))
                            .unwrap_or(false)
                    })
                })
                .collect()
        };

        let (on, on_band) = render(true);
        // Only the final (link) description line (row y+2) is underlined; the
        // label (y+0) and instruction (y+1) rows are not.
        assert_eq!(underlined_rows(&on), vec![2]);
        // Render <-> hit-test parity: the recorded band equals the painted rows.
        assert_eq!(on_band, Some(2..3));

        let (off, off_band) = render(false);
        assert!(
            underlined_rows(&off).is_empty(),
            "opt-out rows underline nothing"
        );
        assert_eq!(off_band, None, "opt-out records no link band");
    }

    #[test]
    fn vim_not_searching_char_does_not_type() {
        for hint in [true, false] {
            let config = cfg(hint, true);
            let mut state = PickerState::default();
            let outcome = handle_picker_input(&press('a'), &mut state, 3, &config);
            assert!(state.query().is_empty(), "hint={hint}");
            assert!(!state.search_active, "hint={hint}");
            assert!(matches!(outcome, PickerOutcome::Unchanged), "hint={hint}");
        }
    }

    #[test]
    fn vim_i_enters_search_without_typing() {
        for hint in [true, false] {
            let config = cfg(hint, true);
            let mut state = PickerState::default();
            let outcome = handle_picker_input(&press('i'), &mut state, 3, &config);
            assert!(matches!(outcome, PickerOutcome::Changed), "hint={hint}");
            assert!(state.search_active, "hint={hint}");
            assert!(state.query().is_empty(), "hint={hint}");
        }
    }

    /// `input_active()` opens directly in input mode; `default()` does not.
    /// Used by type-to-find pickers (command palette, `/model` & `/theme` arg
    /// picker) so typing filters immediately on open.
    #[test]
    fn input_active_starts_in_search_mode() {
        assert!(PickerState::input_active().search_active);
        assert!(!PickerState::default().search_active);
    }

    #[test]
    fn expandable_picker_routes_arrows_by_focus() {
        let mut config = cfg(true, false);
        config.expandable = true;

        let mut search = PickerState::input_active();
        assert!(matches!(
            handle_picker_input(
                &Event::Key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)),
                &mut search,
                3,
                &config,
            ),
            PickerOutcome::Expand(0)
        ));

        search.set_query("match");
        let outcome = handle_picker_input(&press_esc(), &mut search, 3, &config);
        assert!(matches!(outcome, PickerOutcome::Changed));
        assert!(!search.search_active);
        assert!(matches!(
            handle_picker_input(
                &Event::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)),
                &mut search,
                3,
                &config,
            ),
            PickerOutcome::Collapse(0)
        ));
    }

    #[test]
    fn first_hint_search_edit_leaves_expansion_to_the_host() {
        let mut config = cfg(true, false);
        config.expandable = true;
        let mut state = PickerState::default();

        let outcome = handle_picker_input(&press('a'), &mut state, 3, &config);
        assert!(matches!(outcome, PickerOutcome::QueryChanged));
        assert!(state.expanded.is_empty());
    }

    #[test]
    fn set_query_sanitizes_before_filtering_and_places_cursor_at_end() {
        let mut state = PickerState::default();
        state.set_query("alpha\r\nbeta\n");
        assert_eq!(state.query(), "alphabeta");
        assert_eq!(state.query_cursor(), state.query().len());

        let visible = ["alpha", "alphabeta", "beta"]
            .into_iter()
            .filter(|candidate| candidate.contains(state.query()))
            .collect::<Vec<_>>();
        assert_eq!(visible, vec!["alphabeta"]);
    }

    #[test]
    fn clamp_selection_skips_group_headers() {
        let mut state = PickerState::default();
        clamp_picker_selection(&mut state, 3, &[true, false, false]);
        assert_eq!(state.selected, 1);
    }

    #[test]
    fn vim_char_types_after_entering_search() {
        for hint in [true, false] {
            let config = cfg(hint, true);
            let mut state = PickerState::default();
            handle_picker_input(&press('i'), &mut state, 3, &config);
            let outcome = handle_picker_input(&press('a'), &mut state, 3, &config);
            assert!(
                matches!(outcome, PickerOutcome::QueryChanged),
                "hint={hint}"
            );
            assert_eq!(state.query(), "a", "hint={hint}");
        }
    }

    #[test]
    fn vim_esc_while_searching_exits_search_and_clears_query() {
        for hint in [true, false] {
            let config = cfg(hint, true);
            let mut state = PickerState::default();
            handle_picker_input(&press('i'), &mut state, 3, &config);
            handle_picker_input(&press('a'), &mut state, 3, &config);
            assert_eq!(state.query(), "a", "hint={hint}");
            let outcome = handle_picker_input(&press_esc(), &mut state, 3, &config);
            assert!(
                matches!(outcome, PickerOutcome::QueryChanged),
                "hint={hint}"
            );
            assert!(!state.search_active, "hint={hint}");
            assert!(state.query().is_empty(), "hint={hint}");
        }
    }

    #[test]
    fn vim_esc_while_not_searching_closes() {
        for hint in [true, false] {
            let config = cfg(hint, true);
            let mut state = PickerState::default();
            let outcome = handle_picker_input(&press_esc(), &mut state, 3, &config);
            assert!(matches!(outcome, PickerOutcome::Closed), "hint={hint}");
        }
    }

    #[test]
    fn vim_j_k_navigate_when_not_searching() {
        // Works for both hint and always-active (show_search_hint=false) pickers.
        for hint in [true, false] {
            let config = cfg(hint, true);
            let mut state = PickerState::default();
            handle_picker_input(&press('j'), &mut state, 3, &config);
            assert_eq!(state.selected, 1, "hint={hint} after j");
            handle_picker_input(&press('k'), &mut state, 3, &config);
            assert_eq!(state.selected, 0, "hint={hint} after k");
        }
    }

    #[test]
    fn vim_j_at_bottom_clamps_without_opening_search() {
        // Edge-cycle is disabled under vim: j at the last item stays put and
        // never opens search or jumps to the tabs region.
        for hint in [true, false] {
            let config = cfg(hint, true);
            let mut state = PickerState {
                selected: 2,
                ..PickerState::default()
            };
            let outcome = handle_picker_input(&press('j'), &mut state, 3, &config);
            assert_eq!(state.selected, 2, "hint={hint}");
            assert!(!state.search_active, "hint={hint}");
            assert!(!state.tabs_focused, "hint={hint}");
            assert!(matches!(outcome, PickerOutcome::Changed), "hint={hint}");
        }
    }

    #[test]
    fn vim_k_at_top_clamps_without_opening_search() {
        for hint in [true, false] {
            let config = cfg(hint, true);
            let mut state = PickerState::default(); // selected = 0 (top)
            let outcome = handle_picker_input(&press('k'), &mut state, 3, &config);
            assert_eq!(state.selected, 0, "hint={hint}");
            assert!(!state.search_active, "hint={hint}");
            assert!(matches!(outcome, PickerOutcome::Changed), "hint={hint}");
        }
    }

    #[test]
    fn vim_slash_enters_search_without_typing() {
        for hint in [true, false] {
            let config = cfg(hint, true);
            let mut state = PickerState::default();
            let outcome = handle_picker_input(&press('/'), &mut state, 3, &config);
            assert!(matches!(outcome, PickerOutcome::Changed), "hint={hint}");
            assert!(state.search_active, "hint={hint}");
            assert!(state.query().is_empty(), "hint={hint}");
        }
    }

    #[test]
    fn vim_action_key_takes_precedence_over_i_entry() {
        // A picker binding `i` as an action key (e.g. extensions install/auth)
        // triggers the action; the vim `i` search-entry does not shadow it.
        let mut config = cfg(true, true);
        config.action_keys = &[('i', "x")];
        let mut state = PickerState::default();
        let outcome = handle_picker_input(&press('i'), &mut state, 3, &config);
        assert!(matches!(outcome, PickerOutcome::Action('i')));
        assert!(!state.search_active);
    }

    #[test]
    fn paste_search_leaves_expansion_to_the_host() {
        let mut config = cfg(true, false);
        config.expandable = true;
        let mut state = PickerState::default();

        let outcome =
            handle_picker_input(&Event::Paste("needle".to_string()), &mut state, 3, &config);
        assert!(matches!(outcome, PickerOutcome::QueryChanged));
        assert!(state.expanded.is_empty());
    }

    #[test]
    fn vim_paste_suppressed_when_not_searching() {
        for hint in [true, false] {
            let config = cfg(hint, true);
            let mut state = PickerState::default();
            let outcome =
                handle_picker_input(&Event::Paste("hello".to_string()), &mut state, 3, &config);
            assert!(state.query().is_empty(), "hint={hint}");
            assert!(matches!(outcome, PickerOutcome::Unchanged), "hint={hint}");
        }
    }

    #[test]
    fn vim_paste_types_after_entering_search() {
        // Paste is only suppressed in nav mode; once search is entered it pastes.
        let config = cfg(true, true);
        let mut state = PickerState::default();
        handle_picker_input(&press('i'), &mut state, 3, &config);
        let outcome = handle_picker_input(&Event::Paste("hi".to_string()), &mut state, 3, &config);
        assert!(matches!(outcome, PickerOutcome::QueryChanged));
        assert_eq!(state.query(), "hi");
    }

    #[test]
    fn vim_disable_search_makes_i_and_slash_inert() {
        // disable_search pickers (read-only cheatsheets) use no search hint;
        // under vim, `i` and `/` must not enter search.
        let mut config = cfg(false, true);
        config.disable_search = true;
        for c in ['i', '/'] {
            let mut state = PickerState::default();
            let outcome = handle_picker_input(&press(c), &mut state, 3, &config);
            assert!(!state.search_active, "c={c}");
            assert!(state.query().is_empty(), "c={c}");
            assert!(matches!(outcome, PickerOutcome::Unchanged), "c={c}");
        }
    }

    #[test]
    fn vim_with_tabs_jk_navigate_and_char_does_not_type() {
        let mut config = cfg(true, true);
        config.tabs = Some(&["a", "b"]);
        let mut state = PickerState::default();
        // j navigates the list without opening search or jumping to the tab bar.
        handle_picker_input(&press('j'), &mut state, 3, &config);
        assert_eq!(state.selected, 1);
        assert!(!state.search_active);
        assert!(!state.tabs_focused);
        // A bare printable char does not type while in nav mode.
        let outcome = handle_picker_input(&press('a'), &mut state, 3, &config);
        assert!(state.query().is_empty());
        assert!(matches!(outcome, PickerOutcome::Unchanged));
    }

    #[test]
    fn vim_i_up_j_does_not_reopen_search_with_tabs() {
        // i (enter search) → Up (exit into tabs region) → j must move into the
        // list, not reopen search (only `i`/`/` start search under vim).
        let mut config = cfg(true, true);
        config.tabs = Some(&["a", "b"]);
        let mut state = PickerState::default();
        handle_picker_input(&press('i'), &mut state, 3, &config);
        assert!(state.search_active);
        handle_picker_input(&press_up(), &mut state, 3, &config);
        assert!(state.tabs_focused);
        assert!(!state.search_active);
        handle_picker_input(&press('j'), &mut state, 3, &config);
        assert!(!state.search_active);
        assert!(!state.tabs_focused);
    }

    #[test]
    fn query_cursor_edits_do_not_reset_list_state() {
        let config = cfg(false, false);
        let mut state = PickerState::default();
        state.set_query("alpha-beta");
        state.selected = 2;
        state.expanded.insert(1);
        state.scroll_offset = Some(4);

        let left_word = Event::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT));
        let outcome = handle_picker_input(&left_word, &mut state, 3, &config);
        assert!(matches!(outcome, PickerOutcome::Changed));
        assert_eq!(state.query(), "alpha-beta");
        assert_eq!(state.query_cursor(), "alpha-".len());
        assert_eq!(state.selected, 2);
        assert_eq!(state.expanded, HashSet::from([1]));
        assert_eq!(state.scroll_offset, Some(4));

        let mut empty = PickerState {
            selected: 2,
            ..PickerState::default()
        };
        let outcome = handle_picker_input(
            &Event::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)),
            &mut empty,
            3,
            &config,
        );
        assert!(matches!(outcome, PickerOutcome::Changed));
        assert_eq!(empty.selected, 2);
    }

    #[test]
    fn query_text_edits_reset_list_state_once() {
        let config = cfg(false, false);
        let mut state = PickerState::default();
        state.set_query("alpha-beta");
        state.selected = 2;
        state.expanded.insert(1);
        state.scroll_offset = Some(4);

        let outcome = handle_picker_input(
            &Event::Key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT)),
            &mut state,
            3,
            &config,
        );
        assert!(matches!(outcome, PickerOutcome::QueryChanged));
        assert_eq!(state.query(), "alpha-");
        assert_eq!(state.selected, 0);
        assert!(state.expanded.is_empty());
        assert_eq!(state.scroll_offset, None);
    }

    #[test]
    fn canonical_alt_words_work_in_normal_and_vim_input() {
        let mut state = PickerState::default();
        state.set_query("alpha-beta");
        let config = cfg(false, false);
        for (character, cursor) in [('b', "alpha-".len()), ('f', "alpha-beta".len())] {
            let outcome = handle_picker_input(
                &Event::Key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::ALT)),
                &mut state,
                3,
                &config,
            );
            assert!(matches!(outcome, PickerOutcome::Changed));
            assert_eq!(state.query_cursor(), cursor);
        }

        let config = cfg(true, true);
        let mut vim_state = PickerState::default();
        handle_picker_input(&press('i'), &mut vim_state, 3, &config);
        vim_state.set_query("alpha-beta");
        let outcome = handle_picker_input(
            &Event::Key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT)),
            &mut vim_state,
            3,
            &config,
        );
        assert!(matches!(outcome, PickerOutcome::QueryChanged));
        assert_eq!(vim_state.query(), "alpha-");

        let mut hinted = PickerState::default();
        let hinted_config = cfg(true, false);
        let outcome = handle_picker_input(
            &Event::Key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT)),
            &mut hinted,
            3,
            &hinted_config,
        );
        assert!(matches!(outcome, PickerOutcome::Changed));
        assert!(hinted.query().is_empty());
        assert!(!hinted.search_active);

        let ctrl_w = Event::Key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        let mut navigation = PickerState::default();
        assert!(matches!(
            handle_picker_input(&ctrl_w, &mut navigation, 3, &hinted_config),
            PickerOutcome::Unchanged
        ));
        navigation.search_active = true;
        assert!(matches!(
            handle_picker_input(&ctrl_w, &mut navigation, 3, &hinted_config),
            PickerOutcome::Changed
        ));
    }

    #[test]
    fn picker_graphemes_paste_and_viewport_use_line_editor() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;

        let config = cfg(false, false);
        let grapheme = "👩🏽\u{200d}💻";
        let mut state = PickerState::default();
        state.set_query(format!("a{grapheme}b"));
        handle_picker_input(
            &Event::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)),
            &mut state,
            3,
            &config,
        );
        let viewport = state.query.viewport(3);
        assert_eq!(
            &state.query()[viewport.visible_byte_range.clone()],
            format!("{grapheme}b")
        );
        assert_eq!(viewport.cursor_display_column, 2);
        let area = Rect::new(0, 0, 12, 1);
        let mut buffer = Buffer::empty(area);
        let theme = Theme::current();
        render_picker_search_bar(
            &mut buffer,
            area.x,
            area.y,
            area.width,
            &theme,
            &state,
            true,
            false,
            None,
        );
        assert_eq!(
            buffer.cell((11, 0)).expect("cursor cell").bg,
            theme.text_primary
        );

        handle_picker_input(
            &Event::Key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
            &mut state,
            3,
            &config,
        );
        assert_eq!(state.query(), "ab");
        let outcome =
            handle_picker_input(&Event::Paste("x\r\ny".to_owned()), &mut state, 3, &config);
        assert!(matches!(outcome, PickerOutcome::QueryChanged));
        assert_eq!(state.query(), "axyb");
    }
}
