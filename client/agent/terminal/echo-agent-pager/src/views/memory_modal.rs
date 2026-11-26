//! `/memory` browser modal -- view-state, rendering, and input handling.
//!
//! A centered popup with ModalWindow chrome, horizontally split into a
//! searchable file list (left, ~40%) and a read-only content preview
//! pane (right, ~60%). File list shows all memory files grouped by source
//! (Global, Workspace, Sessions) with session logs in reverse chronological
//! order. Selecting a file loads its content into the preview pane.
//!
//! Layout collapses to single-pane (list only) on narrow terminals (< 80 cols).
//!
//! `/` enters filter mode (type to search, Escape to exit).
//! `x` deletes with double-press confirmation (session logs only).

use std::borrow::Cow;
use std::path::PathBuf;

use crossterm::event::KeyModifiers;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::app::actions::Action;
use crate::app::app_view::InputOutcome;
use crate::input::line_editor::{LineEditOutcome, LineEditor};

use crate::render::SafeBuf;
use crate::render::scrollbar::{ScrollbarClickResult, render_scrollbar, scrollbar_click_to_offset};
use crate::scrollback::blocks::markdown_content::MarkdownContent;
use crate::theme::Theme;
use crate::views::modal_window::{
    self, ModalContentArea, ModalSizing, ModalWindowConfig, ModalWindowState, Shortcut,
};

const SPLIT_MIN_WIDTH: u16 = 80;
const LIST_WIDTH_RATIO: f64 = 0.40;
const MAX_PREVIEW_BYTES: u64 = 1_048_576;

#[derive(Debug, Clone)]
pub struct MemoryFileEntry {
    pub path: PathBuf,
    /// `"global"`, `"workspace"`, or `"session"`.
    pub source: String,
    pub label: String,
    /// Pre-formatted `"size · modified"` string for the list row.
    /// Empty for headers.
    pub meta_display: String,
    pub is_header: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryModalMode {
    Browse,
    /// Filter input is focused — all single-char keys go to the filter.
    /// Only Escape (exit filter) and Enter (no-op / stay) remain active.
    FilterFocused,
    ConfirmingDelete {
        idx: usize,
    },
}

#[derive(Debug, Clone)]
pub struct MemoryModalState {
    pub window: ModalWindowState,
    pub entries: Vec<MemoryFileEntry>,
    pub selected: usize,
    pub scroll_offset: usize,
    pub preview_markdown: Option<MarkdownContent>,
    pub preview_scroll: usize,
    pub mode: MemoryModalMode,
    query: LineEditor,
    /// Whether memory is currently enabled for this session.
    pub memory_enabled: bool,
    /// Whether the modal is rendered in fullscreen mode (persisted to config).
    pub fullscreen: bool,
    /// Cached filtered indices. Recomputed when `query` or `entries` change.
    filtered_cache: Vec<usize>,
    /// Cached total lines in the preview (for scrollbar rendering).
    preview_total_lines: usize,
    /// Hit-test rects for mouse interaction (set during render).
    list_area: Rect,
    preview_area: Rect,
    list_scrollbar_area: Option<Rect>,
    preview_scrollbar_area: Option<Rect>,
}

impl MemoryModalState {
    pub fn new(entries: Vec<MemoryFileEntry>) -> Self {
        let filtered_cache = compute_filtered(&entries, "");
        let mut state = Self {
            window: ModalWindowState::new(),
            entries,
            selected: 0,
            scroll_offset: 0,
            preview_markdown: None,
            preview_scroll: 0,
            mode: MemoryModalMode::Browse,
            query: LineEditor::default(),
            memory_enabled: true,
            fullscreen: load_fullscreen_pref(),
            filtered_cache,
            preview_total_lines: 0,
            list_area: Rect::default(),
            preview_area: Rect::default(),
            list_scrollbar_area: None,
            preview_scrollbar_area: None,
        };
        state.advance_past_headers();
        state.load_preview();
        state
    }

    pub fn filtered_indices(&self) -> &[usize] {
        &self.filtered_cache
    }

    pub fn query(&self) -> &str {
        self.query.text()
    }

    pub fn query_cursor_byte(&self) -> usize {
        self.query.cursor_byte()
    }

    #[cfg(test)]
    fn set_query(&mut self, query: impl Into<String>) {
        self.query.set_text(query);
    }

    #[cfg(test)]
    fn set_query_cursor_byte(&mut self, cursor_byte: usize) -> LineEditOutcome {
        self.query.set_cursor_byte(cursor_byte)
    }

    #[cfg(test)]
    fn query_viewport(&self, width: usize) -> echo_agent_ratatui_textarea::SingleLineViewport {
        self.query.viewport(width)
    }

    fn invalidate_filter(&mut self) {
        self.filtered_cache = compute_filtered(&self.entries, self.query());
    }

    pub fn selected_entry(&self) -> Option<&MemoryFileEntry> {
        self.filtered_cache
            .get(self.selected)
            .and_then(|&i| self.entries.get(i))
    }

    /// Advance `selected` past any leading headers to the first selectable entry.
    fn advance_past_headers(&mut self) {
        let filtered = &self.filtered_cache;
        for (i, &orig) in filtered.iter().enumerate() {
            if !self.entries[orig].is_header {
                self.selected = i;
                return;
            }
        }
    }

    pub fn select_next(&mut self) {
        if self.advance_next() {
            self.load_preview();
        }
    }

    pub fn select_prev(&mut self) {
        if self.advance_prev() {
            self.load_preview();
        }
    }

    /// Move `selected` forward to the next non-header entry without
    /// loading preview. Returns `true` if the selection changed.
    fn advance_next(&mut self) -> bool {
        let filtered = &self.filtered_cache;
        let mut next = self.selected + 1;
        while next < filtered.len() {
            if !self.entries[filtered[next]].is_header {
                self.selected = next;
                return true;
            }
            next += 1;
        }
        false
    }

    /// Move `selected` backward to the previous non-header entry without
    /// loading preview. Returns `true` if the selection changed.
    fn advance_prev(&mut self) -> bool {
        if self.selected == 0 {
            return false;
        }
        let filtered = &self.filtered_cache;
        let mut prev = self.selected - 1;
        loop {
            if !self.entries[filtered[prev]].is_header {
                self.selected = prev;
                return true;
            }
            if prev == 0 {
                break;
            }
            prev -= 1;
        }
        false
    }

    /// Select the entry at a given filtered index (used by mouse click).
    /// Skips headers. Returns `true` if the selection changed.
    pub fn select_at(&mut self, filt_idx: usize) -> bool {
        let filtered = &self.filtered_cache;
        if filt_idx >= filtered.len() {
            return false;
        }
        if self.entries[filtered[filt_idx]].is_header {
            return false;
        }
        if self.selected == filt_idx {
            return false;
        }
        self.selected = filt_idx;
        self.load_preview();
        true
    }

    pub fn clamp_selected(&mut self) {
        let filtered = &self.filtered_cache;
        if filtered.is_empty() {
            self.selected = 0;
            self.preview_markdown = None;
            return;
        }
        if self.selected >= filtered.len() {
            self.selected = filtered.len() - 1;
        }
        if self.entries[filtered[self.selected]].is_header {
            // Try forward.
            for (i, &orig) in filtered.iter().enumerate().skip(self.selected + 1) {
                if !self.entries[orig].is_header {
                    self.selected = i;
                    self.load_preview();
                    return;
                }
            }
            // Try backward.
            for (i, &orig) in filtered.iter().enumerate().take(self.selected).rev() {
                if !self.entries[orig].is_header {
                    self.selected = i;
                    self.load_preview();
                    return;
                }
            }
        }
        self.load_preview();
    }

    fn load_preview(&mut self) {
        self.preview_scroll = 0;
        self.preview_markdown = self
            .selected_entry()
            .filter(|e| !e.is_header)
            .and_then(|e| {
                let path = std::path::Path::new(&e.path);
                match std::fs::metadata(path) {
                    Ok(meta) if meta.len() > MAX_PREVIEW_BYTES => {
                        Some(MarkdownContent::new("*(File too large to preview)*"))
                    }
                    Err(_) => None,
                    _ => std::fs::read_to_string(path).ok().map(MarkdownContent::new),
                }
            });
    }
}

/// Compute filtered indices from entries and query. Section headers are
/// preserved only when at least one entry in the section matches.
fn compute_filtered(entries: &[MemoryFileEntry], query: &str) -> Vec<usize> {
    if query.is_empty() {
        return (0..entries.len()).collect();
    }
    let needle = query.to_lowercase();
    let mut result = Vec::new();
    let mut pending_header: Option<usize> = None;
    let mut section_has_match = false;
    for (i, entry) in entries.iter().enumerate() {
        if entry.is_header {
            // Note: `pending_header` is `Some` here only when the previous
            // section had zero matches (`.take()` clears it on first match),
            // so `section_has_match` is always false — this guard is a
            // defensive no-op. Kept for safety.
            if let Some(h) = pending_header
                && section_has_match
            {
                result.push(h);
            }
            pending_header = Some(i);
            section_has_match = false;
        } else if entry.label.to_lowercase().contains(&needle)
            || entry.source.to_lowercase().contains(&needle)
        {
            if let Some(h) = pending_header.take() {
                result.push(h);
                section_has_match = true;
            }
            result.push(i);
        }
    }
    // Same defensive no-op as the in-loop guard above.
    if let Some(h) = pending_header
        && section_has_match
    {
        result.push(h);
    }
    result
}

pub fn build_entries(
    files: Vec<echo_agent_runtime::extensions::notification::MemoryFileInfo>,
) -> Vec<MemoryFileEntry> {
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut global = Vec::new();
    let mut workspace = Vec::new();
    let mut session = Vec::new();

    for f in files {
        let label = file_label(&f.path);
        let size = crate::util::format_bytes(f.size_bytes);
        let modified = format_modified(f.modified_epoch_secs, now_secs);
        let meta_display = format!("{size} \u{00B7} {modified}");
        let bucket = match f.source.as_str() {
            "global" => &mut global,
            "workspace" => &mut workspace,
            _ => &mut session,
        };
        bucket.push(MemoryFileEntry {
            label,
            meta_display,
            path: f.path.into(),
            source: f.source,
            is_header: false,
        });
    }

    // Session logs: reverse chronological (newest first).
    session.reverse();

    let mut entries = Vec::new();
    let mut push_section = |label: &str, items: Vec<MemoryFileEntry>| {
        if !items.is_empty() {
            entries.push(MemoryFileEntry {
                path: PathBuf::new(),
                source: String::new(),
                meta_display: String::new(),
                label: label.to_string(),
                is_header: true,
            });
            entries.extend(items);
        }
    };
    push_section("Global", global);
    push_section("Workspace", workspace);
    push_section("Sessions", session);
    entries
}

pub fn render_memory_modal(
    buf: &mut Buffer,
    full_area: Rect,
    state: &mut MemoryModalState,
    compact: bool,
) {
    let theme = Theme::current();
    let shortcuts = build_shortcuts(&state.mode, state.memory_enabled, state.fullscreen);

    let modal_config = ModalWindowConfig {
        title: "Memory",
        tabs: None,
        shortcuts: &shortcuts,
        sizing: if state.fullscreen {
            ModalSizing {
                width_pct: 1.0,
                max_width: u16::MAX,
                min_width: 44,
                v_margin: 0,
                h_pad: 2,
                v_pad: 0,
                footer_lines: 2,
            }
        } else {
            ModalSizing {
                width_pct: 0.75,
                max_width: 140,
                min_width: 44,
                v_margin: 3,
                h_pad: 2,
                v_pad: 1,
                footer_lines: 2,
            }
        }
        .with_compact(compact),
        fold_info: None,
    };

    let Some(ModalContentArea {
        content: content_area,
        ..
    }) =
        modal_window::render_modal_window(buf, full_area, &mut state.window, &modal_config, &theme)
    else {
        return;
    };

    if content_area.height < 2 || content_area.width < 10 {
        return;
    }

    let show_preview = content_area.width >= SPLIT_MIN_WIDTH;
    let list_width = if show_preview {
        (content_area.width as f64 * LIST_WIDTH_RATIO) as u16
    } else {
        content_area.width
    };

    let list_area = Rect {
        x: content_area.x,
        y: content_area.y,
        width: list_width,
        height: content_area.height,
    };
    state.list_area = list_area;

    render_file_list(buf, list_area, state, &theme);

    if show_preview {
        let preview_x = content_area.x + list_width + 1;
        let preview_width = content_area.width.saturating_sub(list_width + 1);
        if preview_width > 2 {
            let sep_x = content_area.x + list_width;
            let sep_style = Style::default().fg(theme.gray_dim);
            for y in content_area.y..content_area.y + content_area.height {
                if let Some(cell) = buf.cell_mut((sep_x, y)) {
                    cell.set_symbol("\u{2502}");
                    cell.set_style(sep_style);
                }
            }

            let preview_area = Rect {
                x: preview_x,
                y: content_area.y,
                width: preview_width,
                height: content_area.height,
            };
            state.preview_area = preview_area;
            render_preview(buf, preview_area, state, &theme);
        } else {
            state.preview_area = Rect::default();
            state.preview_scrollbar_area = None;
        }
    } else {
        state.preview_area = Rect::default();
        state.preview_scrollbar_area = None;
    }
}

fn render_file_list(buf: &mut Buffer, area: Rect, state: &mut MemoryModalState, theme: &Theme) {
    let search_y = area.y;
    let filter_focused = matches!(state.mode, MemoryModalMode::FilterFocused);
    let viewport = state.query.viewport(area.width as usize);
    if state.query().is_empty() {
        let placeholder = if filter_focused {
            "type to filter..."
        } else {
            "/ to filter..."
        };
        buf.set_span(
            area.x,
            search_y,
            &Span::styled(
                placeholder,
                Style::default().fg(theme.gray_dim).bg(theme.bg_base),
            ),
            area.width,
        );
    } else {
        let leading;
        let visible = if filter_focused {
            &state.query()[viewport.visible_byte_range.clone()]
        } else {
            leading = crate::render::line_utils::truncate_str(state.query(), area.width as usize);
            &leading
        };
        buf.set_span(
            area.x,
            search_y,
            &Span::styled(
                visible,
                Style::default().fg(theme.text_primary).bg(theme.bg_base),
            ),
            area.width,
        );
    }
    if filter_focused {
        let cursor_x = area.x + viewport.cursor_display_column as u16;
        if cursor_x < area.x + area.width
            && let Some(cell) = buf.cell_mut((cursor_x, search_y))
        {
            cell.set_style(Style::default().fg(theme.bg_base).bg(theme.text_primary));
        }
    }

    let entries_start_y = search_y + 1;
    let available_height = area.height.saturating_sub(1) as usize;
    if state.selected < state.scroll_offset {
        state.scroll_offset = state.selected;
    }
    if state.selected >= state.scroll_offset + available_height {
        state.scroll_offset = state.selected.saturating_sub(available_height - 1);
    }

    // Compute scrollbar area before borrowing filtered_cache.
    let total_entries = sat_u16(state.filtered_indices().len());
    let sb_area = if total_entries > available_height as u16 && area.width > 4 {
        Some(Rect {
            x: area.x + area.width - 1,
            y: entries_start_y,
            width: 1,
            height: available_height as u16,
        })
    } else {
        None
    };
    state.list_scrollbar_area = sb_area;
    let content_width = if sb_area.is_some() {
        area.width.saturating_sub(2)
    } else {
        area.width
    };

    let filtered = state.filtered_indices();
    let end = filtered.len().min(state.scroll_offset + available_height);
    let visible = &filtered[state.scroll_offset..end];

    for (row, &orig_idx) in visible.iter().enumerate() {
        let y = entries_start_y + row as u16;
        if y >= area.y + area.height {
            break;
        }
        let entry = &state.entries[orig_idx];
        let filt_idx = state.scroll_offset + row;
        let is_selected = filt_idx == state.selected;

        if entry.is_header {
            let header_style = Style::default()
                .fg(theme.accent_user)
                .bg(theme.bg_base)
                .add_modifier(Modifier::BOLD);
            let line = Line::from(Span::styled(&entry.label, header_style));
            buf.set_line(area.x, y, &line, content_width);
        } else {
            let bg = if is_selected {
                theme.bg_visual
            } else {
                theme.bg_base
            };
            let label_style = Style::default().fg(theme.text_primary).bg(bg);
            let meta_style = Style::default().fg(theme.gray).bg(bg);

            let row_rect = Rect {
                x: area.x,
                y,
                width: content_width,
                height: 1,
            };
            buf.set_style(row_rect, Style::default().bg(bg));

            let max_label_w = content_width.saturating_sub(2) as usize;
            let truncated_label: Cow<str> = if entry.label.width() > max_label_w {
                let trunc = truncate_to_width(&entry.label, max_label_w.saturating_sub(3));
                format!("{trunc}...").into()
            } else {
                Cow::Borrowed(&entry.label)
            };
            buf.set_span(
                area.x + 1,
                y,
                &Span::styled(truncated_label.as_ref(), label_style),
                content_width.saturating_sub(1),
            );

            if is_selected
                && matches!(state.mode, MemoryModalMode::ConfirmingDelete { idx } if idx == filt_idx)
            {
                let hint = " [x to confirm]";
                let hint_w = hint.len() as u16;
                let hint_x = (area.x + content_width).saturating_sub(hint_w + 1);
                buf.set_span(
                    hint_x,
                    y,
                    &Span::styled(hint, Style::default().fg(theme.accent_error).bg(bg)),
                    hint_w,
                );
            } else {
                let meta_w = entry.meta_display.width() as u16;
                let meta_x = (area.x + content_width).saturating_sub(meta_w + 1);
                if meta_x > area.x + 1 {
                    buf.set_span(
                        meta_x,
                        y,
                        &Span::styled(&entry.meta_display, meta_style),
                        meta_w,
                    );
                }
            }
        }
    }

    // List scrollbar.
    render_scrollbar(
        buf,
        sb_area,
        total_entries,
        sat_u16(available_height),
        sat_u16(state.scroll_offset),
        false,
    );
}

fn render_preview(buf: &mut Buffer, area: Rect, state: &mut MemoryModalState, theme: &Theme) {
    if state.preview_markdown.is_none() {
        state.preview_total_lines = 0;
        state.preview_scrollbar_area = None;
        let msg = "No file selected";
        let style = Style::default().fg(theme.gray_dim).bg(theme.bg_base);
        let cy = area.y + area.height / 2;
        let cx = area.x + area.width.saturating_sub(msg.width() as u16) / 2;
        buf.set_span(cx, cy, &Span::styled(msg, style), area.width);
        return;
    }

    // Reserve scrollbar column if needed (computed after first pass).
    // We do a two-pass approach: first compute total at full width to decide
    // if scrollbar is needed, then re-compute at narrowed width if so.
    let full_width = area.width as usize;
    if full_width == 0 {
        return;
    }

    buf.set_style(area, Style::default().bg(theme.bg_base));

    let total_at_full = state
        .preview_markdown
        .as_ref()
        .unwrap()
        .with_wrapped_lines(full_width, |w| w.lines.len());
    let visible = area.height as usize;

    let (content_width, sb_area) = if total_at_full > visible && area.width > 4 {
        let sb = Rect {
            x: area.x + area.width - 1,
            y: area.y,
            width: 1,
            height: area.height,
        };
        (full_width.saturating_sub(2), Some(sb))
    } else {
        (full_width, None)
    };

    let total = if sb_area.is_some() && content_width != full_width {
        state
            .preview_markdown
            .as_ref()
            .unwrap()
            .with_wrapped_lines(content_width, |w| w.lines.len())
    } else {
        total_at_full
    };

    state.preview_total_lines = total;
    state.preview_scrollbar_area = sb_area;
    state.preview_scroll = state.preview_scroll.min(total.saturating_sub(visible));
    let scroll = state.preview_scroll;

    state
        .preview_markdown
        .as_ref()
        .unwrap()
        .with_wrapped_lines(content_width, |wrapped| {
            for (row, line_idx) in (scroll..total.min(scroll + visible)).enumerate() {
                let y = area.y + row as u16;
                buf.set_line_safe(area.x, y, &wrapped.lines[line_idx], content_width as u16);
            }
        });

    render_scrollbar(
        buf,
        sb_area,
        sat_u16(total),
        sat_u16(visible),
        sat_u16(scroll),
        false,
    );
}

pub fn handle_memory_key(state: &mut MemoryModalState, key: &KeyEvent) -> InputOutcome {
    if key.kind == KeyEventKind::Release {
        return InputOutcome::Unchanged;
    }

    // ConfirmingDelete has an "any key cancel" contract — handle it first
    // so no other handler can silently swallow the key.
    if let MemoryModalMode::ConfirmingDelete { idx } = state.mode {
        if key.code == KeyCode::Char('x') {
            let filtered = state.filtered_indices();
            if let Some(&orig_idx) = filtered.get(idx) {
                let path = state.entries[orig_idx].path.clone();
                state.entries.remove(orig_idx);
                state.mode = MemoryModalMode::Browse;
                state.invalidate_filter();
                state.clamp_selected();
                let _ = std::fs::remove_file(&path);
                return InputOutcome::Changed;
            }
        }
        state.mode = MemoryModalMode::Browse;
        return InputOutcome::Changed;
    }

    match state.mode {
        MemoryModalMode::ConfirmingDelete { .. } => unreachable!("handled above"),
        MemoryModalMode::FilterFocused => handle_filter_focused(state, key),
        MemoryModalMode::Browse => handle_browse(state, key),
    }
}

pub fn handle_memory_paste(state: &mut MemoryModalState, text: &str) -> InputOutcome {
    if state.mode != MemoryModalMode::FilterFocused {
        return InputOutcome::Unchanged;
    }
    let outcome = state.query.insert_paste(text);
    finish_filter_edit(state, outcome)
}

/// Saturating cast from `usize` to `u16` (caps at `u16::MAX`).
fn sat_u16(v: usize) -> u16 {
    v.min(u16::MAX as usize) as u16
}

/// After a list scrollbar jump, select the first visible non-header entry
/// so that `render_file_list`'s scroll-clamping doesn't snap `scroll_offset`
/// back to the old selection.
fn select_first_visible(state: &mut MemoryModalState) {
    let filtered = state.filtered_indices();
    let visible_start = state.scroll_offset;
    let visible_end = filtered
        .len()
        .min(visible_start + state.list_area.height.saturating_sub(1) as usize);
    for (i, &orig) in filtered
        .iter()
        .enumerate()
        .take(visible_end)
        .skip(visible_start)
    {
        if !state.entries[orig].is_header {
            state.selected = i;
            state.load_preview();
            return;
        }
    }
}

/// Apply a scrollbar click/drag at `screen_row` within `sb_area`, updating `offset`.
fn apply_scrollbar_jump(
    screen_row: u16,
    sb_area: Rect,
    total_lines: u16,
    viewport_lines: u16,
    offset: &mut usize,
) {
    let cell_index = screen_row.saturating_sub(sb_area.y);
    let result = scrollbar_click_to_offset(cell_index, sb_area.height, total_lines, viewport_lines);
    let max_scroll = (total_lines as usize).saturating_sub(viewport_lines as usize);
    match result {
        ScrollbarClickResult::Top => *offset = 0,
        ScrollbarClickResult::Bottom => *offset = max_scroll,
        ScrollbarClickResult::Offset(o) => *offset = o.min(max_scroll),
    }
}

/// Handle mouse events for the memory modal content area.
///
/// Supports:
/// - Left-click on a file list row to select it
/// - Scroll wheel over the file list to scroll the list
/// - Scroll wheel over the preview pane to scroll the preview
/// - Click/drag on list scrollbar to jump-scroll the list
/// - Click/drag on preview scrollbar to jump-scroll the preview
pub fn handle_memory_mouse(
    state: &mut MemoryModalState,
    kind: MouseEventKind,
    column: u16,
    row: u16,
) -> InputOutcome {
    let in_rect = |r: Rect| -> bool {
        r.width > 0
            && r.height > 0
            && column >= r.x
            && column < r.x + r.width
            && row >= r.y
            && row < r.y + r.height
    };

    let on_list = in_rect(state.list_area);
    let on_preview = in_rect(state.preview_area);
    let on_list_sb = state.list_scrollbar_area.is_some_and(&in_rect);
    let on_preview_sb = state.preview_scrollbar_area.is_some_and(&in_rect);

    match kind {
        MouseEventKind::Down(crossterm::event::MouseButton::Left)
        | MouseEventKind::Drag(crossterm::event::MouseButton::Left) => {
            // Click/drag on list scrollbar → jump-scroll + select nearest entry.
            if on_list_sb {
                let sb = state.list_scrollbar_area.unwrap();
                let total = sat_u16(state.filtered_indices().len());
                let visible = state.list_area.height.saturating_sub(1);
                apply_scrollbar_jump(row, sb, total, visible, &mut state.scroll_offset);
                select_first_visible(state);
                return InputOutcome::Changed;
            }

            // Click/drag on preview scrollbar → jump-scroll.
            if on_preview_sb {
                let sb = state.preview_scrollbar_area.unwrap();
                let total = sat_u16(state.preview_total_lines);
                let visible = state.preview_area.height;
                apply_scrollbar_jump(row, sb, total, visible, &mut state.preview_scroll);
                return InputOutcome::Changed;
            }

            // Click on a file list row to select it (not on drag).
            if matches!(kind, MouseEventKind::Down(_)) && on_list {
                let entries_start_y = state.list_area.y + 1;
                if row >= entries_start_y {
                    let clicked_row = (row - entries_start_y) as usize;
                    let filt_idx = state.scroll_offset + clicked_row;
                    if state.select_at(filt_idx) {
                        return InputOutcome::Changed;
                    }
                }
            }

            InputOutcome::Unchanged
        }

        MouseEventKind::ScrollDown => {
            if on_list || on_list_sb {
                let mut moved = false;
                for _ in 0..3 {
                    moved |= state.advance_next();
                }
                if moved {
                    state.load_preview();
                }
                return InputOutcome::Changed;
            }
            if on_preview || on_preview_sb {
                let max = state
                    .preview_total_lines
                    .saturating_sub(state.preview_area.height as usize);
                state.preview_scroll = state.preview_scroll.saturating_add(3).min(max);
                return InputOutcome::Changed;
            }
            InputOutcome::Unchanged
        }

        MouseEventKind::ScrollUp => {
            if on_list || on_list_sb {
                let mut moved = false;
                for _ in 0..3 {
                    moved |= state.advance_prev();
                }
                if moved {
                    state.load_preview();
                }
                return InputOutcome::Changed;
            }
            if on_preview || on_preview_sb {
                state.preview_scroll = state.preview_scroll.saturating_sub(3);
                return InputOutcome::Changed;
            }
            InputOutcome::Unchanged
        }

        _ => InputOutcome::Unchanged,
    }
}

/// Keys while the filter input is focused: all chars go to the filter,
/// only Escape exits filter mode. Arrow keys still navigate the list.
fn handle_filter_focused(state: &mut MemoryModalState, key: &KeyEvent) -> InputOutcome {
    match key.code {
        KeyCode::Esc => {
            state.mode = MemoryModalMode::Browse;
            InputOutcome::Changed
        }
        KeyCode::Down => {
            state.select_next();
            InputOutcome::Changed
        }
        KeyCode::Up => {
            state.select_prev();
            InputOutcome::Changed
        }
        _ => {
            let outcome = state.query.handle_key(key);

            finish_filter_edit(state, outcome)
        }
    }
}

fn finish_filter_edit(state: &mut MemoryModalState, outcome: LineEditOutcome) -> InputOutcome {
    match outcome {
        LineEditOutcome::TextChanged => {
            state.invalidate_filter();
            state.clamp_selected();
            InputOutcome::Changed
        }
        LineEditOutcome::CursorChanged | LineEditOutcome::HandledNoChange => InputOutcome::Changed,
        LineEditOutcome::Unhandled => InputOutcome::Unchanged,
    }
}

/// Keys in normal browse mode: single-char hotkeys active, `/` enters
/// filter mode.
fn handle_browse(state: &mut MemoryModalState, key: &KeyEvent) -> InputOutcome {
    if key.code == KeyCode::Char('f') && key.modifiers.contains(KeyModifiers::CONTROL) {
        state.fullscreen = !state.fullscreen;
        return InputOutcome::Action(Action::PersistMemoryFullscreen(state.fullscreen));
    }
    match key.code {
        KeyCode::Down | KeyCode::Char('j') => {
            state.select_next();
            InputOutcome::Changed
        }
        KeyCode::Up | KeyCode::Char('k') => {
            state.select_prev();
            InputOutcome::Changed
        }
        KeyCode::PageDown => {
            let mut moved = false;
            for _ in 0..10 {
                moved |= state.advance_next();
            }
            if moved {
                state.load_preview();
            }
            InputOutcome::Changed
        }
        KeyCode::PageUp => {
            let mut moved = false;
            for _ in 0..10 {
                moved |= state.advance_prev();
            }
            if moved {
                state.load_preview();
            }
            InputOutcome::Changed
        }
        KeyCode::Char('x') if key.modifiers.is_empty() => {
            if let Some(entry) = state.selected_entry()
                && !entry.is_header
                && entry.source == "session"
            {
                state.mode = MemoryModalMode::ConfirmingDelete {
                    idx: state.selected,
                };
                return InputOutcome::Changed;
            }
            InputOutcome::Unchanged
        }
        KeyCode::Char('y') if key.modifiers.is_empty() => {
            if let Some(entry) = state.selected_entry()
                && !entry.is_header
            {
                let _ = crate::clipboard::copy_text(&entry.path.to_string_lossy());
                return InputOutcome::Changed;
            }
            InputOutcome::Unchanged
        }
        KeyCode::Char('t') if key.modifiers.is_empty() => {
            let cmd = if state.memory_enabled {
                "/memory off"
            } else {
                "/memory on"
            };
            // Optimistic update — the shortcut bar reflects the new state
            // immediately. The shell re-syncs via the next MemoryFiles
            // notification if the command fails.
            state.memory_enabled = !state.memory_enabled;
            InputOutcome::Action(Action::SendSlashCommandPreservingDraft(cmd.to_owned()))
        }
        // `i` aliases `/` (vim-nav "press i to search").
        KeyCode::Char('/') | KeyCode::Char('i') if key.modifiers.is_empty() => {
            state.mode = MemoryModalMode::FilterFocused;
            InputOutcome::Changed
        }
        KeyCode::Backspace => {
            if state.query.delete_last_grapheme() == LineEditOutcome::TextChanged {
                state.invalidate_filter();
                state.clamp_selected();
                InputOutcome::Changed
            } else {
                InputOutcome::Unchanged
            }
        }
        _ => InputOutcome::Unchanged,
    }
}

fn build_shortcuts(
    mode: &MemoryModalMode,
    memory_enabled: bool,
    fullscreen: bool,
) -> Vec<Shortcut<'static>> {
    match mode {
        MemoryModalMode::Browse => {
            let toggle_label = if memory_enabled {
                "t toggle (on)"
            } else {
                "t toggle (off)"
            };
            let mut shortcuts = vec![
                Shortcut {
                    label: "\u{2191}/\u{2193} nav",
                    clickable: false,
                    id: 0,
                },
                Shortcut {
                    label: "/ search",
                    clickable: false,
                    id: 0,
                },
                Shortcut {
                    label: "y copy path",
                    clickable: false,
                    id: 0,
                },
                Shortcut {
                    label: "x delete",
                    clickable: false,
                    id: 0,
                },
                Shortcut {
                    label: toggle_label,
                    clickable: false,
                    id: 0,
                },
                Shortcut {
                    label: if fullscreen {
                        "^F normal"
                    } else {
                        "^F fullscreen"
                    },
                    clickable: false,
                    id: 0,
                },
                Shortcut {
                    label: "Esc close",
                    clickable: false,
                    id: 0,
                },
            ];
            // Browse is nav mode (filter inactive), so append `i search` last
            // (matching the shared pickers).
            modal_window::push_vim_nav_search_hint(&mut shortcuts, false);
            shortcuts
        }
        MemoryModalMode::FilterFocused => vec![
            Shortcut {
                label: "type to filter",
                clickable: false,
                id: 0,
            },
            Shortcut {
                label: "Esc exit filter",
                clickable: false,
                id: 0,
            },
        ],
        MemoryModalMode::ConfirmingDelete { .. } => vec![
            Shortcut {
                label: "x confirm delete",
                clickable: false,
                id: 0,
            },
            Shortcut {
                label: "any key cancel",
                clickable: false,
                id: 0,
            },
        ],
    }
}

/// Truncate a string to fit within `max_width` display columns.
/// Delegates to `render::line_utils::byte_offset_at_width` to avoid
/// duplicating the Unicode-width scanning logic.
fn truncate_to_width(s: &str, max_width: usize) -> &str {
    let offset = crate::render::line_utils::byte_offset_at_width(s, max_width);
    &s[..offset]
}

fn file_label(path: &str) -> String {
    let p = std::path::Path::new(path);
    p.file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

fn format_modified(epoch_secs: Option<u64>, now_secs: u64) -> String {
    let Some(modified) = epoch_secs else {
        return "unknown".to_string();
    };
    if now_secs <= modified {
        return "just now".to_string();
    }
    let delta = now_secs - modified;
    if delta < 60 {
        return "just now".to_string();
    }
    if delta < 3600 {
        let mins = delta / 60;
        return format!("{mins}m ago");
    }
    if delta < 86400 {
        let hours = delta / 3600;
        return format!("{hours}h ago");
    }
    let days = delta / 86400;
    format!("{days}d ago")
}

fn load_fullscreen_pref() -> bool {
    let path = echo_agent_tools::util::echo_agent_home::echo_agent_home().join("config.toml");
    let Some(doc) = crate::config_toml_edit::read_config_document_for_edit(&path) else {
        return false;
    };
    doc.get("hints")
        .and_then(|h| h.get("memory_modal_fullscreen"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_modified_relative() {
        let now = 1_700_000_000u64;
        assert_eq!(format_modified(None, now), "unknown");
        assert_eq!(format_modified(Some(now), now), "just now");
        assert_eq!(format_modified(Some(now - 30), now), "just now");
        assert_eq!(format_modified(Some(now - 120), now), "2m ago");
        assert_eq!(format_modified(Some(now - 7200), now), "2h ago");
        assert_eq!(format_modified(Some(now - 172800), now), "2d ago");
    }

    #[test]
    fn file_label_extracts_filename() {
        assert_eq!(
            file_label("/home/user/.echo-agent/memory/MEMORY.md"),
            "MEMORY.md"
        );
        assert_eq!(
            file_label("/workspace/.echo-agent/memory/sessions/2026-01-15-fix-bug.md"),
            "2026-01-15-fix-bug.md"
        );
    }

    #[test]
    fn build_entries_groups_by_source() {
        use echo_agent_runtime::extensions::notification::MemoryFileInfo;

        let files = vec![
            MemoryFileInfo {
                path: "/global/MEMORY.md".into(),
                source: "global".into(),
                size_bytes: 100,
                modified_epoch_secs: Some(1_700_000_000),
            },
            MemoryFileInfo {
                path: "/workspace/MEMORY.md".into(),
                source: "workspace".into(),
                size_bytes: 200,
                modified_epoch_secs: Some(1_700_000_000),
            },
            MemoryFileInfo {
                path: "/sessions/log1.md".into(),
                source: "session".into(),
                size_bytes: 50,
                modified_epoch_secs: None,
            },
        ];

        let entries = build_entries(files);
        assert_eq!(entries.len(), 6);
        assert!(entries[0].is_header);
        assert_eq!(entries[0].label, "Global");
        assert!(!entries[1].is_header);
        assert!(entries[2].is_header);
        assert_eq!(entries[2].label, "Workspace");
        assert!(entries[4].is_header);
        assert_eq!(entries[4].label, "Sessions");
    }

    #[test]
    fn new_selects_first_non_header() {
        let entries = build_test_entries();
        let state = MemoryModalState::new(entries);
        // Index 0 is a header; selected should skip to index 1.
        assert_eq!(state.selected, 1);
        let sel = state.selected_entry().unwrap();
        assert!(!sel.is_header);
    }

    #[test]
    fn filtered_indices_preserves_headers_for_matching_entries() {
        let entries = build_test_entries();
        let mut state = MemoryModalState::new(entries);
        state.set_query("memory");
        state.invalidate_filter();

        let indices = state.filtered_indices();
        assert_eq!(indices.len(), 2);
        assert_eq!(indices[0], 0); // Global header
        assert_eq!(indices[1], 1); // MEMORY.md
    }

    #[test]
    fn delete_only_allowed_for_session_entries() {
        let entries = build_test_entries();
        let mut state = MemoryModalState::new(entries);
        // Select the global entry (index 1 in filtered, which is "MEMORY.md" source="global")
        state.selected = 1;
        // Try to delete — should NOT enter confirming mode because source is "global".
        let key = crossterm::event::KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
        let outcome = handle_memory_key(&mut state, &key);
        assert!(matches!(outcome, InputOutcome::Unchanged));
        assert_eq!(state.mode, MemoryModalMode::Browse);

        // Select the session entry (index 3 in filtered, which is "session-log.md" source="session")
        state.selected = 3;
        let outcome = handle_memory_key(&mut state, &key);
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(matches!(
            state.mode,
            MemoryModalMode::ConfirmingDelete { .. }
        ));
    }

    /// `i` aliases `/` without modifiers: from Browse it enters FilterFocused
    /// exactly like `/` (vim-nav "press i to search").
    #[test]
    fn i_key_enters_filter_like_slash() {
        let mut state = MemoryModalState::new(build_test_entries());
        assert_eq!(state.mode, MemoryModalMode::Browse);
        let i = crossterm::event::KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE);
        assert!(matches!(
            handle_memory_key(&mut state, &i),
            InputOutcome::Changed
        ));
        assert_eq!(state.mode, MemoryModalMode::FilterFocused);
    }

    /// The `modifiers.is_empty()` guard: Ctrl+i / Alt+i must NOT enter filter.
    #[test]
    fn modified_i_does_not_enter_filter() {
        for mods in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
            let mut state = MemoryModalState::new(build_test_entries());
            let k = crossterm::event::KeyEvent::new(KeyCode::Char('i'), mods);
            assert!(matches!(
                handle_memory_key(&mut state, &k),
                InputOutcome::Unchanged
            ));
            assert_eq!(state.mode, MemoryModalMode::Browse);
        }
    }

    /// Wiring check: the Browse footer carries the shared `i search` hint under
    /// vim nav mode. The gate is covered centrally by `modal_window`'s
    /// `vim_nav_search_hint_only_in_vim_nav_mode`. The explicit `set_vim_mode`
    /// pin (a thread-local that, once set, blocks disk-seeding) keeps this
    /// independent of the dev's on-disk `[ui].vim_mode`; reset afterward since
    /// libtest reuses worker threads.
    #[test]
    fn browse_footer_advertises_i_search_under_vim() {
        crate::appearance::cache::set_vim_mode(true);
        let vim = build_shortcuts(&MemoryModalMode::Browse, true, false);
        assert!(
            vim.iter().any(|s| s.label == "i search"),
            "vim-mode Browse footer must advertise `i search`"
        );
        crate::appearance::cache::set_vim_mode(false);
    }

    #[test]
    fn truncate_to_width_handles_ascii() {
        assert_eq!(truncate_to_width("hello world", 5), "hello");
        assert_eq!(truncate_to_width("hello", 10), "hello");
        assert_eq!(truncate_to_width("", 5), "");
    }

    #[test]
    fn truncate_to_width_handles_multibyte() {
        // CJK characters are 2 columns wide.
        let s = "\u{4F60}\u{597D}world"; // 你好world — 2+2+5 = 9 cols
        assert_eq!(truncate_to_width(s, 4), "\u{4F60}\u{597D}"); // 2+2 = 4
        assert_eq!(truncate_to_width(s, 3), "\u{4F60}"); // 2, next char is 2 → exceeds 3
        assert_eq!(truncate_to_width(s, 9), s); // fits
    }

    #[test]
    fn cached_filter_updates_on_invalidate() {
        let entries = build_test_entries();
        let mut state = MemoryModalState::new(entries);
        assert_eq!(state.filtered_indices().len(), 4); // all entries

        state.set_query("session");
        state.invalidate_filter();
        // Only the Sessions header + session-log.md should match.
        assert_eq!(state.filtered_indices().len(), 2);

        state.set_query("");
        state.invalidate_filter();
        assert_eq!(state.filtered_indices().len(), 4);
    }

    #[test]
    fn select_at_skips_headers_and_updates() {
        let entries = build_test_entries();
        let mut state = MemoryModalState::new(entries);
        // Initial selection is index 1 (first non-header).
        assert_eq!(state.selected, 1);

        // Select the session entry at filtered index 3.
        assert!(state.select_at(3));
        assert_eq!(state.selected, 3);
        let sel = state.selected_entry().unwrap();
        assert_eq!(sel.label, "session-log.md");

        // Selecting a header should fail.
        assert!(!state.select_at(0));
        assert_eq!(state.selected, 3);

        // Selecting the same index again should return false.
        assert!(!state.select_at(3));
    }

    #[test]
    fn select_at_out_of_bounds() {
        let entries = build_test_entries();
        let mut state = MemoryModalState::new(entries);
        assert!(!state.select_at(100));
        assert_eq!(state.selected, 1);
    }

    #[test]
    fn mouse_click_selects_file() {
        let entries = build_test_entries();
        let mut state = MemoryModalState::new(entries);
        // Simulate a rendered list area.
        state.list_area = Rect::new(5, 10, 30, 20);
        state.scroll_offset = 0;

        // Click on row corresponding to filtered index 3 (entries_start_y = 11, row 3 → y=14).
        let result = handle_memory_mouse(
            &mut state,
            MouseEventKind::Down(crossterm::event::MouseButton::Left),
            10,
            14,
        );
        assert!(matches!(result, InputOutcome::Changed));
        assert_eq!(state.selected, 3);
    }

    #[test]
    fn mouse_click_on_header_unchanged() {
        let entries = build_test_entries();
        let mut state = MemoryModalState::new(entries);
        state.list_area = Rect::new(5, 10, 30, 20);
        state.scroll_offset = 0;

        // Click on row 0 (header "Global" at y=11).
        let result = handle_memory_mouse(
            &mut state,
            MouseEventKind::Down(crossterm::event::MouseButton::Left),
            10,
            11,
        );
        assert!(matches!(result, InputOutcome::Unchanged));
        assert_eq!(state.selected, 1);
    }

    #[test]
    fn mouse_scroll_up_down_on_list() {
        let entries = build_test_entries();
        let mut state = MemoryModalState::new(entries);
        state.list_area = Rect::new(0, 0, 30, 10);

        // Scroll down on the list area.
        let result = handle_memory_mouse(&mut state, MouseEventKind::ScrollDown, 5, 3);
        assert!(matches!(result, InputOutcome::Changed));

        // Scroll up on the list area.
        let result = handle_memory_mouse(&mut state, MouseEventKind::ScrollUp, 5, 3);
        assert!(matches!(result, InputOutcome::Changed));
    }

    #[test]
    fn mouse_scroll_on_preview() {
        let entries = build_test_entries();
        let mut state = MemoryModalState::new(entries);
        state.preview_area = Rect::new(40, 0, 30, 10);
        state.preview_total_lines = 100;
        state.preview_scroll = 5;

        // Scroll down on the preview area.
        let result = handle_memory_mouse(&mut state, MouseEventKind::ScrollDown, 50, 3);
        assert!(matches!(result, InputOutcome::Changed));
        assert_eq!(state.preview_scroll, 8);

        // Scroll up on the preview area.
        let result = handle_memory_mouse(&mut state, MouseEventKind::ScrollUp, 50, 3);
        assert!(matches!(result, InputOutcome::Changed));
        assert_eq!(state.preview_scroll, 5);
    }

    #[test]
    fn mouse_outside_both_panes_unchanged() {
        let entries = build_test_entries();
        let mut state = MemoryModalState::new(entries);
        state.list_area = Rect::new(0, 0, 30, 10);
        state.preview_area = Rect::new(40, 0, 30, 10);

        // Click outside both areas.
        let result = handle_memory_mouse(
            &mut state,
            MouseEventKind::Down(crossterm::event::MouseButton::Left),
            35,
            5,
        );
        assert!(matches!(result, InputOutcome::Unchanged));
    }

    #[test]
    fn ctrl_d_u_no_longer_scrolls_preview() {
        let entries = build_test_entries();
        let mut state = MemoryModalState::new(entries);
        state.preview_scroll = 5;

        // Ctrl+D should NOT scroll preview (removed hotkey).
        let key = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);
        let result = handle_memory_key(&mut state, &key);
        assert!(matches!(result, InputOutcome::Unchanged));
        assert_eq!(state.preview_scroll, 5);

        // Ctrl+U should NOT scroll preview (removed hotkey).
        let key = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL);
        let result = handle_memory_key(&mut state, &key);
        assert!(matches!(result, InputOutcome::Unchanged));
        assert_eq!(state.preview_scroll, 5);
    }

    #[test]
    fn filter_text_changes_recompute_preview_but_cursor_moves_do_not() {
        let mut state = MemoryModalState::new(build_test_entries());
        state.mode = MemoryModalMode::FilterFocused;
        state.preview_scroll = 7;

        let outcome = handle_memory_key(
            &mut state,
            &KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE),
        );
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(state.query(), "m");
        assert_eq!(state.preview_scroll, 0);
        let filtered = state.filtered_indices().to_vec();

        state.preview_scroll = 7;
        let outcome = handle_memory_key(
            &mut state,
            &KeyEvent::new(KeyCode::Left, KeyModifiers::NONE),
        );
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(state.query(), "m");
        assert_eq!(state.query_cursor_byte(), 0);
        assert_eq!(state.filtered_indices(), filtered);
        assert_eq!(state.preview_scroll, 7);
    }

    #[test]
    fn filter_paste_recomputes_once_and_consumes_empty_input() {
        let mut state = MemoryModalState::new(build_test_entries());
        state.mode = MemoryModalMode::FilterFocused;
        state.preview_scroll = 7;
        let outcome = handle_memory_paste(&mut state, "mem\r\n");
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(state.query(), "mem");
        assert_eq!(state.preview_scroll, 0);

        state.preview_scroll = 7;
        let outcome = handle_memory_paste(&mut state, "\r\n");
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(state.query(), "mem");
        assert_eq!(state.preview_scroll, 7);

        state.mode = MemoryModalMode::Browse;
        let outcome = handle_memory_paste(&mut state, "ignored");
        assert!(matches!(outcome, InputOutcome::Unchanged));
        assert_eq!(state.query(), "mem");
    }

    #[test]
    fn filter_escape_preserves_query_and_enter_stays_focused() {
        let mut state = MemoryModalState::new(build_test_entries());
        state.mode = MemoryModalMode::FilterFocused;
        state.set_query("memory");

        let outcome = handle_memory_key(
            &mut state,
            &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );
        assert!(matches!(outcome, InputOutcome::Unchanged));
        assert_eq!(state.mode, MemoryModalMode::FilterFocused);
        assert_eq!(state.query(), "memory");

        let outcome =
            handle_memory_key(&mut state, &KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(state.mode, MemoryModalMode::Browse);
        assert_eq!(state.query(), "memory");
    }

    #[test]
    fn filter_uses_canonical_word_and_grapheme_editing() {
        for key in [
            KeyEvent::new(KeyCode::Left, KeyModifiers::ALT),
            KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT),
            KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL),
        ] {
            let mut state = MemoryModalState::new(build_test_entries());
            state.mode = MemoryModalMode::FilterFocused;
            state.set_query("hello-world");
            let outcome = handle_memory_key(&mut state, &key);
            assert!(matches!(outcome, InputOutcome::Changed));
            assert_eq!(state.query(), "hello-world");
            assert_eq!(state.query_cursor_byte(), "hello-".len());
        }

        let grapheme = "👩🏽\u{200d}💻";
        let mut state = MemoryModalState::new(build_test_entries());
        state.mode = MemoryModalMode::FilterFocused;
        state.set_query(format!("a{grapheme}b"));
        let _ = state.set_query_cursor_byte(1);
        let outcome = handle_memory_key(
            &mut state,
            &KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE),
        );
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(state.query(), "ab");
        assert_eq!(state.query_cursor_byte(), 1);
    }

    #[test]
    fn browse_backspace_deletes_trailing_grapheme_independent_of_cursor_and_modifiers() {
        let mut state = MemoryModalState::new(build_test_entries());
        let grapheme = "👩🏽\u{200d}💻";
        state.set_query(format!("a{grapheme}"));
        let _ = state.set_query_cursor_byte(0);
        let outcome = handle_memory_key(
            &mut state,
            &KeyEvent::new(KeyCode::Backspace, KeyModifiers::CONTROL),
        );
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(state.query(), "a");
        assert_eq!(state.query_cursor_byte(), 1);
    }

    #[test]
    fn filter_render_keeps_unicode_query_and_cursor_visible() {
        let mut state = MemoryModalState::new(build_test_entries());
        state.mode = MemoryModalMode::FilterFocused;
        let grapheme = "👩🏽\u{200d}💻";
        let text = format!("123456789012中e\u{301}{grapheme}z");
        state.set_query(&text);
        let _ = state.set_query_cursor_byte(text.len() - 1);
        let area = Rect::new(0, 0, 12, 3);
        let theme = Theme::current();
        let mut buffer = Buffer::empty(area);
        let viewport = state.query_viewport(area.width as usize);
        let visible = &state.query()[viewport.visible_byte_range.clone()];
        assert!(visible.contains('中'));
        assert!(visible.contains("e\u{301}"));
        assert!(visible.contains(grapheme));

        render_file_list(&mut buffer, area, &mut state, &theme);
        let cursor_x = viewport.cursor_display_column as u16;
        assert_eq!(buffer[(cursor_x, 0)].bg, theme.text_primary);
    }

    #[test]
    fn apply_scrollbar_jump_edges() {
        let mut offset = 50;
        // Top click → offset 0.
        apply_scrollbar_jump(10, Rect::new(0, 10, 1, 20), 100, 10, &mut offset);
        assert_eq!(offset, 0);

        // Bottom click → max offset.
        apply_scrollbar_jump(29, Rect::new(0, 10, 1, 20), 100, 10, &mut offset);
        assert_eq!(offset, 90);
    }

    fn build_test_entries() -> Vec<MemoryFileEntry> {
        vec![
            MemoryFileEntry {
                path: PathBuf::new(),
                source: String::new(),
                meta_display: String::new(),
                label: "Global".to_string(),
                is_header: true,
            },
            MemoryFileEntry {
                path: PathBuf::from("/test/MEMORY.md"),
                source: "global".into(),
                meta_display: "1KB \u{00B7} 1d ago".into(),
                label: "MEMORY.md".to_string(),
                is_header: false,
            },
            MemoryFileEntry {
                path: PathBuf::new(),
                source: String::new(),
                meta_display: String::new(),
                label: "Sessions".to_string(),
                is_header: true,
            },
            MemoryFileEntry {
                path: PathBuf::from("/test/session.md"),
                source: "session".into(),
                meta_display: "500B \u{00B7} 2d ago".into(),
                label: "session-log.md".to_string(),
                is_header: false,
            },
        ]
    }
}
