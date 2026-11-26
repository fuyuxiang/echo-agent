//! Shared modal window chrome component.
//!
//! Provides a single `ModalWindow` that handles the visual frame (border,
//! title, close button, optional tab bar, footer shortcuts) and common
//! input routing (Esc to close, tab switching, shortcut clicks). Each
//! popup modal in the pager becomes an instance of `ModalWindow` with
//! different features enabled via [`ModalWindowConfig`].
//!
//! The visual style follows the import-claude modal's design: accent-colored
//! square border, bold title on top border, generous inner padding, inline
//! centered footer shortcuts with hover highlights, and a `Clear` background.

use crossterm::event::{KeyCode, KeyEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, Borders, Clear, Widget};

use unicode_width::UnicodeWidthStr;

use crate::render::line_utils::byte_offset_at_width;
use crate::theme::Theme;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

pub use crate::modal_window_state::{ModalWindowState, ShortcutHitArea};

use std::sync::atomic::{AtomicBool, Ordering};

/// When set, [`render_modal_window`] renders **borderless** ("embedded"): no
/// centered popup box, no border, no close button — the modal's content fills
/// the given `area` directly. Minimal mode sets this once at startup so it never
/// shows a floating modal frame (design: "never render modals in minimal mode");
/// the full TUI leaves it off and keeps the bordered popup.
static EMBEDDED: AtomicBool = AtomicBool::new(false);

/// Enable/disable borderless ("embedded") modal rendering. Set `true` for
/// minimal mode (called once at terminal init).
pub fn set_embedded(on: bool) {
    EMBEDDED.store(on, Ordering::Relaxed);
}

/// Whether modals should render borderless (minimal mode). See [`set_embedded`].
pub fn embedded() -> bool {
    EMBEDDED.load(Ordering::Relaxed)
}

/// Resolved list-row styling for the embedded ("resume-list") look shared by
/// the dropdown / picker widgets: rows stay transparent and the selected row
/// recolors its text with the selection accent (fzf-style) — a painted
/// selection band is not expressible on the terminal-native palette.
#[derive(Clone, Copy)]
pub struct EmbeddedRowStyle {
    /// Row background: always transparent (`Color::Reset`).
    pub bg: Color,
    /// True when this row is the selected row.
    pub selected: bool,
    selected_fg: Color,
}

impl EmbeddedRowStyle {
    /// Foreground for text/glyphs on this row: the selection accent on the
    /// selected row, otherwise the caller's `normal` color.
    pub fn fg(&self, normal: Color) -> Color {
        if self.selected {
            self.selected_fg
        } else {
            normal
        }
    }
}

/// Embedded (minimal) list-row styling, or `None` in the full TUI — in which
/// case the caller keeps its own selected / hovered / normal styling. See
/// [`embedded`] and [`EmbeddedRowStyle`].
pub fn embedded_row_style(theme: &Theme, is_selected: bool) -> Option<EmbeddedRowStyle> {
    embedded().then_some(EmbeddedRowStyle {
        bg: Color::Reset,
        selected: is_selected,
        selected_fg: theme.fuzzy_accent,
    })
}

/// Per-render configuration for a modal window (rebuilt each frame).
pub struct ModalWindowConfig<'a> {
    /// Title displayed bold on the top border.
    pub title: &'a str,
    /// Tab labels. `None` = no tab bar.
    pub tabs: Option<&'a [&'a str]>,
    /// Footer shortcuts to render inline at the bottom.
    pub shortcuts: &'a [Shortcut<'a>],
    /// Sizing parameters.
    pub sizing: ModalSizing,
    /// Fold state of the currently focused entry. When provided,
    /// Left/Right/h/l return specific fold outcomes instead of
    /// [`ModalWindowOutcome::Unhandled`].
    pub fold_info: Option<FoldInfo>,
}

/// Sizing parameters for the modal popup.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModalSizing {
    /// Fraction of screen width to use (0.0..=1.0). Default: 0.9.
    pub width_pct: f32,
    /// Maximum width in columns. Default: 140.
    pub max_width: u16,
    /// Minimum width in columns. Default: 60.
    pub min_width: u16,
    /// Vertical margin (top and bottom) in rows. Default: 7.
    pub v_margin: u16,
    /// Horizontal inner padding in columns (applied on both sides). Default: 2.
    pub h_pad: u16,
    /// Vertical inner padding above the content area (below the tab bar,
    /// if present). Not applied at the bottom — the footer occupies that
    /// space. Default: 2.
    pub v_pad: u16,
    /// Lines reserved at the bottom for footer shortcuts. Default: 2.
    pub footer_lines: u16,
}

impl Default for ModalSizing {
    fn default() -> Self {
        Self {
            width_pct: 0.9,
            max_width: 140,
            min_width: 60,
            v_margin: 7,
            h_pad: 2,
            v_pad: 2,
            footer_lines: 2,
        }
    }
}

impl ModalSizing {
    /// Medium popup: ~60% width, standard padding. Good for picker lists.
    /// Used by: cloud_modal and other pickers (verified: values match exactly).
    pub fn medium() -> Self {
        Self {
            width_pct: 0.60,
            max_width: 120,
            min_width: 44,
            v_margin: 4,
            h_pad: 2,
            v_pad: 1,
            footer_lines: 2,
        }
    }

    /// Large popup: ~90% width, generous padding. Good for forms/detail views.
    /// Used by: import_claude_modal. Same as Default.
    pub fn large() -> Self {
        Self::default()
    }

    /// Returns adjusted sizing for compact mode with *very little margins*.
    ///
    /// Goal: maximize usable content area inside every popup (command palette,
    /// /resume sessions, plugins/hooks/mcps, import-claude, docs, etc.).
    pub fn with_compact(mut self, compact: bool) -> Self {
        if compact {
            // Almost no outer centering margin — the popup can nearly touch
            // the top and bottom of the terminal.
            self.v_margin = 0;
            // Keep 1 column so the left accent line + selection border have room.
            self.h_pad = 1;
            // No extra vertical breathing room above the first content row
            // (search bar, tab content, or first picker row).
            self.v_pad = 0;
        }
        self
    }
}

/// Fold state of the currently focused entry, provided by the caller
/// so [`handle_modal_key`] can make fold decisions generically.
///
/// When present in [`ModalWindowConfig`], Left/Right/h/l keys return
/// specific fold outcomes ([`ModalWindowOutcome::CollapseGroup`], etc.)
/// instead of the default [`ModalWindowOutcome::Unhandled`]. Note that
/// `Unhandled` is still returned when no fold action applies (e.g.
/// Left on a top-level collapsed header with no parent).
#[derive(Debug, Clone, Copy)]
pub struct FoldInfo {
    /// Whether the focused entry is a collapsible group header.
    pub collapsible: bool,
    /// Whether the focused entry is currently expanded (children visible).
    pub expanded: bool,
    /// Whether the focused entry has expandable detail fields (e.g. leaf
    /// items with description lines or config fields).
    pub has_details: bool,
    /// Whether detail fields are currently shown.
    pub details_expanded: bool,
    /// Index of the parent group header. `None` for top-level entries.
    pub parent_index: Option<usize>,
}

/// A single footer shortcut definition.
pub struct Shortcut<'a> {
    /// Display label (e.g. "Enter import 3" or "Esc cancel").
    pub label: &'a str,
    /// Whether clicking this shortcut dispatches `ShortcutActivated`.
    /// All shortcuts get the same visual style and hover highlights
    /// regardless of this flag.
    pub clickable: bool,
    /// Caller-defined identifier, returned in `ModalWindowOutcome::ShortcutActivated`.
    pub id: usize,
}

/// Append an `i search` footer hint when a vim-nav picker is in NAV mode
/// (vim-mode on and search not yet active), so users discover how to start
/// typing. No-op otherwise. Shared by the picker modals' footer builders.
pub fn push_vim_nav_search_hint<'a>(shortcuts: &mut Vec<Shortcut<'a>>, search_active: bool) {
    if !search_active && crate::appearance::cache::load_vim_mode() {
        shortcuts.push(Shortcut {
            label: "i search",
            clickable: false,
            id: 0,
        });
    }
}

/// The content area returned by [`render_modal_window`] so the caller
/// knows where to draw their domain-specific content.
pub struct ModalContentArea {
    /// Rect for the main content (between top padding and footer),
    /// inset by `h_pad` on each side.
    pub content: Rect,
    /// Rect for the footer shortcut row.
    pub footer: Rect,
    /// Full inner width (border to border, no h_pad). Use this for
    /// rendering full-width dividers.
    pub inner_x: u16,
    pub inner_width: u16,
}

/// Outcome of a modal window processing an input event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModalWindowOutcome {
    /// Chrome consumed the event (hover update, etc.).
    Handled,
    /// Close requested (Esc, click `[✗]`, or click outside).
    CloseRequested,
    /// Tab changed to the given index.
    TabChanged(usize),
    /// A clickable footer shortcut was activated (by caller-defined ID).
    ShortcutActivated(usize),
    /// Collapse the focused group header (Left on expanded collapsible
    /// header).
    CollapseGroup,
    /// Expand the focused group header (Right on collapsed collapsible
    /// header).
    ExpandGroup,
    /// Collapse detail fields on the focused leaf (Left on expanded
    /// details).
    CollapseDetails,
    /// Expand detail fields on the focused leaf (Right on collapsed
    /// details).
    ExpandDetails,
    /// Jump focus to the parent group header at the given index (Left on
    /// collapsed header or leaf without expanded details).
    JumpToParent(usize),
    /// Event not consumed by the chrome -- caller should handle content
    /// interaction (row clicks, scroll, key navigation, etc.).
    Unhandled,
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Render the modal window chrome and return the content area for the
/// caller to render into.
///
/// Draws:
/// 1. `Clear` over the popup rect (no dimming).
/// 2. Accent-colored square border with bold title on the top border.
/// 3. `[✗]` close button on the top-right border with hover brightening.
/// 4. Optional tab bar below the top border.
/// 5. Inner padding (`h_pad` x `v_pad`).
/// 6. Footer shortcut row (inline centered, clickable bold, hints dim,
///    hover highlight).
///
/// Returns `None` if the area is too small to render a meaningful popup.
pub fn render_modal_window(
    buf: &mut Buffer,
    area: Rect,
    state: &mut ModalWindowState,
    config: &ModalWindowConfig<'_>,
    theme: &Theme,
) -> Option<ModalContentArea> {
    let sizing = &config.sizing;
    let is_embedded = embedded();

    // Popup rect: embedded (minimal) fills the given area; otherwise a centered
    // popup box.
    let (modal_width, modal_height) = if is_embedded {
        (area.width, area.height)
    } else {
        compute_modal_dims(area, sizing)
    };

    if modal_width < 20 || modal_height < 6 {
        // Clear stale hit-test rects so mouse handlers don't act on
        // positions from a previous (larger) render.
        state.popup_area = None;
        state.close_button_rect = None;
        state.shortcut_hits.clear();
        state.tab_rects.clear();
        return None;
    }

    let modal_area = if is_embedded {
        area
    } else {
        Rect {
            x: area.x + (area.width.saturating_sub(modal_width)) / 2,
            y: area.y + (area.height.saturating_sub(modal_height)) / 2,
            width: modal_width,
            height: modal_height,
        }
    };
    state.popup_area = Some(modal_area);

    // Clear cells under the modal so content behind doesn't bleed through.
    Clear.render(modal_area, buf);

    // The decorative ─ dashes around the title use the border color (gray_dim)
    // so they blend with the border, while the title text itself stays bold.
    let border_style = Style::default().fg(theme.gray_dim).bg(theme.bg_base);
    let title_style = Style::default()
        .fg(theme.text_primary)
        .bg(theme.bg_base)
        .add_modifier(Modifier::BOLD);

    let inner = if is_embedded {
        // Borderless (minimal): no popup box, border, or close button — the
        // modal must not look like a floating window. An optional bold title
        // takes the first row; content fills the full width below it.
        if config.title.is_empty() {
            modal_area
        } else {
            // Background-free title (minimal renders every element transparent).
            let embedded_title_style = Style::default()
                .fg(theme.text_primary)
                .add_modifier(Modifier::BOLD);
            let title = ratatui::text::Line::from(Span::styled(config.title, embedded_title_style));
            buf.set_line(
                modal_area.x + sizing.h_pad,
                modal_area.y,
                &title,
                modal_area.width.saturating_sub(sizing.h_pad),
            );
            Rect {
                x: modal_area.x,
                y: modal_area.y + 1,
                width: modal_area.width,
                height: modal_area.height.saturating_sub(1),
            }
        }
    } else {
        // Border block, optionally with a bold title on the top border. An empty
        // title suppresses the decoration so the top border is a continuous line.
        let mut block = Block::default()
            .borders(Borders::ALL)
            .style(Style::default().bg(theme.bg_base).fg(theme.text_primary))
            .border_style(border_style);
        if !config.title.is_empty() {
            let title = ratatui::text::Line::from(vec![
                Span::styled("\u{2500} ", border_style),
                Span::styled(config.title, title_style),
                Span::styled(" \u{2500}", border_style),
            ]);
            block = block.title(title);
        }
        let inner = block.inner(modal_area);
        block.render(modal_area, buf);
        // Top-right [✗] close button rendered ON the border.
        render_close_button(buf, modal_area, state, theme);
        inner
    };

    // Optional tab bar below the top border, with a full-width divider
    // separating tabs from content. Tab bar wraps onto multiple rows
    // when tabs don't fit on a single line.
    let tab_bar_height;
    let tab_divider_height;
    if let Some(tabs) = config.tabs {
        tab_bar_height = render_tab_bar(buf, inner, state, tabs, theme);
        tab_divider_height = 1;
        // Full-width divider below tab bar.
        let div_y = inner.y + tab_bar_height;
        if div_y < inner.y + inner.height {
            let div_bg = if is_embedded {
                ratatui::style::Color::Reset
            } else {
                theme.bg_base
            };
            let div_style = Style::default().fg(theme.gray_dim).bg(div_bg);
            let line: String = std::iter::repeat_n('\u{2500}', inner.width as usize).collect();
            buf.set_string(inner.x, div_y, &line, div_style);
        }
    } else {
        tab_bar_height = 0;
        tab_divider_height = 0;
    }

    // Compute content area (inside padding, above footer).
    // When tabs are present, the divider replaces vertical padding between
    // tabs and content (so content starts right after the divider).
    let effective_v_pad = if config.tabs.is_some() {
        0
    } else {
        sizing.v_pad
    };

    // Footer height: at least footer_lines, but expands if shortcuts
    // need more rows at the current width.
    let footer_width = inner.width.saturating_sub(sizing.h_pad * 2);
    let needed_footer = shortcuts_rows_needed(config.shortcuts, footer_width);
    let footer_lines = sizing.footer_lines.max(needed_footer);

    let content_top = inner.y + effective_v_pad + tab_bar_height + tab_divider_height;
    let content_height = inner
        .height
        .saturating_sub(effective_v_pad + tab_bar_height + tab_divider_height + footer_lines);
    let content_area = Rect {
        x: inner.x + sizing.h_pad,
        y: content_top,
        width: inner.width.saturating_sub(sizing.h_pad * 2),
        height: content_height,
    };

    // Footer: spans all footer rows at the bottom. Shortcuts render
    // bottom-aligned within this area (single row stays on the last
    // line, additional rows wrap upward).
    let footer_height = footer_lines.min(inner.height);
    let footer_y = inner.y + inner.height.saturating_sub(footer_height);
    let footer_area = Rect {
        x: inner.x + sizing.h_pad,
        y: footer_y,
        width: footer_width,
        height: footer_height,
    };

    // Render footer shortcuts.
    state.shortcut_hits = render_modal_shortcuts(
        buf,
        footer_area,
        config.shortcuts,
        state.hovered_shortcut,
        theme,
    );

    Some(ModalContentArea {
        content: content_area,
        footer: footer_area,
        inner_x: inner.x,
        inner_width: inner.width,
    })
}

/// Render the `[✗]` close button on the top-right border of the modal.
pub(crate) fn render_close_button(
    buf: &mut Buffer,
    modal_area: Rect,
    state: &mut ModalWindowState,
    theme: &Theme,
) {
    // Padding spaces around a bracketed close mark (`✗`, or `x` on legacy
    // ConHost where the Dingbats glyph renders as tofu). Each cell is one
    // column, so the 5-cell width is platform-independent.
    let close_cells: [&str; 5] = [" ", "[", crate::glyphs::ballot_x(), "]", " "];
    let close_width = close_cells.len() as u16;
    let close_rect = Rect {
        x: modal_area.x + modal_area.width.saturating_sub(close_width + 2),
        y: modal_area.y,
        width: close_width,
        height: 1,
    };
    for (offset, sym) in close_cells.iter().enumerate() {
        let col = close_rect.x + offset as u16;
        if let Some(cell) = buf.cell_mut((col, close_rect.y)) {
            cell.set_symbol(sym);
            // On hover, brighten the bracketed mark so it reads as a click
            // target (the padding spaces stay unstyled).
            if !sym.trim().is_empty() && state.close_hovered {
                let mut s = cell.style();
                s.fg = Some(theme.text_primary);
                s = s.add_modifier(Modifier::BOLD);
                cell.set_style(s);
            }
        }
    }
    state.close_button_rect = Some(close_rect);
}

/// Render a tab bar below the top border. Returns the number of rows used.
///
/// Tabs wrap onto additional rows when they don't fit on a single line.
fn render_tab_bar(
    buf: &mut Buffer,
    inner: Rect,
    state: &mut ModalWindowState,
    tabs: &[&str],
    theme: &Theme,
) -> u16 {
    state.tab_count = tabs.len();
    state.tab_rects = vec![None; tabs.len()];

    let left_margin = 2u16;
    let available = (inner.width as usize).saturating_sub(left_margin as usize);
    if available == 0 {
        return 0;
    }

    let separator = "  ";
    let sep_w = separator.width();

    // Break tabs into rows greedily.
    let mut rows: Vec<Vec<usize>> = vec![vec![]];
    let mut cur_row_w: usize = 0;
    for (i, label) in tabs.iter().enumerate() {
        let label_w = label.width();
        let needed = if rows.last().unwrap().is_empty() {
            label_w
        } else {
            cur_row_w + sep_w + label_w
        };

        if needed > available && !rows.last().unwrap().is_empty() {
            rows.push(vec![i]);
            cur_row_w = label_w;
        } else {
            rows.last_mut().unwrap().push(i);
            cur_row_w = needed;
        }
    }

    let right_edge = inner.x + inner.width;
    let num_rows = rows.len() as u16;

    for (row_idx, row_indices) in rows.iter().enumerate() {
        let y = inner.y + row_idx as u16;
        if y >= inner.y + inner.height {
            // Mark remaining tabs as not rendered.
            for &tab_idx in row_indices {
                state.tab_rects[tab_idx] = None;
            }
            break;
        }

        let mut cur_x = inner.x + left_margin;
        for (local_idx, &tab_idx) in row_indices.iter().enumerate() {
            let label = tabs[tab_idx];
            let remaining = right_edge.saturating_sub(cur_x) as usize;
            if remaining == 0 {
                state.tab_rects[tab_idx] = None;
                continue;
            }

            let is_active = tab_idx == state.active_tab;
            // Minimal renders every element background-free, so the focused
            // active tab uses accent text instead of a highlight band.
            let is_embedded = embedded();
            let display = &label[..byte_offset_at_width(label, remaining)];
            let label_w = display.width();
            // Inactive tab labels use `theme.gray` (secondary-text tier),
            // not `theme.gray_dim`. At ANSI16 `gray_dim` collapses to the
            // softer slot (silver on White) which leaves text at ~1.2:1
            // contrast — fine for the modal frame's one-cell border line
            // but unreadable as text glyphs on echo-agent-day.
            let style = if is_active {
                if state.tabs_focused && !is_embedded {
                    Style::default()
                        .fg(theme.text_primary)
                        .bg(theme.bg_visual)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                        .fg(theme.accent_user)
                        .add_modifier(Modifier::BOLD)
                }
            } else {
                Style::default().fg(theme.gray)
            };
            buf.set_string(cur_x, y, display, style);
            state.tab_rects[tab_idx] = Some(Rect {
                x: cur_x,
                y,
                width: label_w as u16,
                height: 1,
            });
            cur_x += label_w as u16;
            if local_idx + 1 < row_indices.len() {
                let sep_remaining = right_edge.saturating_sub(cur_x) as usize;
                if sep_remaining > 0 {
                    let sep_display = &separator[..byte_offset_at_width(separator, sep_remaining)];
                    buf.set_string(cur_x, y, sep_display, Style::default().fg(theme.gray));
                    cur_x += sep_display.width() as u16;
                }
            }
        }
    }

    num_rows
}

/// Predict footer shortcut row count at the given area + sizing.
/// Returns 0 when the modal is too small to render.
pub(crate) fn predict_shortcut_rows(
    area: Rect,
    sizing: &ModalSizing,
    shortcuts: &[Shortcut<'_>],
) -> u16 {
    let (modal_width, modal_height) = compute_modal_dims(area, sizing);
    if modal_width < 20 || modal_height < 6 {
        return 0;
    }
    // Border on each side eats 1 column → inner_width = modal_width - 2.
    let inner_width = modal_width.saturating_sub(2);
    let footer_width = inner_width.saturating_sub(sizing.h_pad * 2);
    shortcuts_rows_needed(shortcuts, footer_width)
}

/// Compute the modal's `(width, height)`. Shared by `render_modal_window`
/// and `predict_shortcut_rows`.
fn compute_modal_dims(area: Rect, sizing: &ModalSizing) -> (u16, u16) {
    let max_width = area.width.saturating_sub(4).min(sizing.max_width);
    let preferred_width = (area.width as f32 * sizing.width_pct) as u16;
    // Clamp to the buffer last: the min_width floor can exceed a narrow terminal, sending Clear/Block out of bounds.
    let modal_width = preferred_width
        .min(max_width)
        .max(sizing.min_width)
        .min(area.width);
    let modal_height = area.height.saturating_sub(sizing.v_margin * 2);
    (modal_width, modal_height)
}

/// Compute how many rows a set of shortcuts needs at a given width.
///
/// Uses the same greedy line-breaking logic as [`render_modal_shortcuts`]
/// so the layout prediction matches the actual render.
pub(crate) fn shortcuts_rows_needed(shortcuts: &[Shortcut<'_>], width: u16) -> u16 {
    if width == 0 || shortcuts.is_empty() {
        return 0;
    }
    let avail = width as usize;
    let sep_w = "  |  ".width(); // 5
    let mut rows = 1u16;
    let mut cur_row_w: usize = 0;
    for shortcut in shortcuts {
        let label_w = shortcut.label.width();
        let needed = if cur_row_w == 0 {
            label_w
        } else {
            cur_row_w + sep_w + label_w
        };
        if needed > avail && cur_row_w > 0 {
            rows += 1;
            cur_row_w = label_w;
        } else {
            cur_row_w = needed;
        }
    }
    rows
}

/// Split a shortcut label into its `(key, label)` parts at the first
/// ASCII space character.
///
/// Returns `(full_label, "")` for single-token labels (no space).
/// The leading space stays attached to the label half so the gap
/// between key and label is rendered with the label style — this keeps
/// the spacing visually contiguous when the label has a hover background.
///
/// We intentionally split on ASCII `' '` rather than `char::is_whitespace`
/// so that interpolated labels containing tabs, NBSP (`\u{00A0}`), or
/// other Unicode whitespace don't produce a surprising split point that
/// would style part of the label text as the bold "key".
fn split_shortcut_label(label: &str) -> (&str, &str) {
    match label.find(' ') {
        Some(i) => label.split_at(i),
        None => (label, ""),
    }
}

/// Render inline centered footer shortcuts with multi-row wrapping.
///
/// Returns hit-test areas for all rendered shortcuts (both clickable
/// and hint-only). When shortcuts don't fit on a single row, they
/// wrap onto additional rows within the `area` height. Rows are
/// placed bottom-aligned so a single row sits on the last line.
///
/// Each shortcut label is split at the first whitespace: the leading
/// token (the "key", e.g. `Esc`, `Enter`, `↑/↓`) is rendered bold in
/// `text_secondary`, and the trailing label text (e.g. `cancel`,
/// `import 3`) is rendered in `gray` (the codebase's tertiary text
/// shade — see `shortcuts_bar.rs` for the same hierarchy).
/// Single-token labels render entirely as the key. Hovered shortcuts
/// get a `bg_highlight` underlay across the whole label.
pub fn render_modal_shortcuts(
    buf: &mut Buffer,
    area: Rect,
    shortcuts: &[Shortcut<'_>],
    hovered: Option<usize>,
    theme: &Theme,
) -> Vec<ShortcutHitArea> {
    if area.width == 0 || area.height == 0 || shortcuts.is_empty() {
        return Vec::new();
    }

    let avail = area.width as usize;
    let separator = "  |  ";
    let sep_w = separator.width();

    // Break shortcuts into rows greedily: each row holds as many
    // shortcuts as fit within the available width.
    let mut rows: Vec<Vec<usize>> = vec![vec![]];
    let mut cur_row_w: usize = 0;
    for (idx, shortcut) in shortcuts.iter().enumerate() {
        let label_w = shortcut.label.width();
        let needed = if rows.last().unwrap().is_empty() {
            label_w
        } else {
            cur_row_w + sep_w + label_w
        };

        if needed > avail && !rows.last().unwrap().is_empty() {
            rows.push(vec![idx]);
            cur_row_w = label_w;
        } else {
            rows.last_mut().unwrap().push(idx);
            cur_row_w = needed;
        }
    }

    // Limit to available height.
    rows.truncate(area.height as usize);

    // Render rows bottom-aligned: last row at the bottom of the area.
    let num_rows = rows.len() as u16;
    let mut hits = Vec::new();
    let row_end = area.x + area.width;

    for (row_idx, row_indices) in rows.iter().enumerate() {
        let y = area.y + area.height - num_rows + row_idx as u16;

        // Compute this row's total width for centering.
        let row_total: usize = row_indices
            .iter()
            .map(|&i| shortcuts[i].label.width())
            .sum::<usize>()
            + sep_w * row_indices.len().saturating_sub(1);
        let start_x = if row_total > avail {
            area.x
        } else {
            area.x + (area.width.saturating_sub(row_total as u16)) / 2
        };

        let mut cur_x = start_x;

        for (local_idx, &shortcut_idx) in row_indices.iter().enumerate() {
            let shortcut = &shortcuts[shortcut_idx];
            let remaining = row_end.saturating_sub(cur_x) as usize;
            if remaining == 0 {
                break;
            }

            let display = &shortcut.label[..byte_offset_at_width(shortcut.label, remaining)];
            let visible_w = display.width() as u16;
            let is_hovered = hovered == Some(shortcut_idx);

            // Underlay: fill cell bg with bg_highlight on hover.
            if is_hovered {
                let hover_bg = Style::default().bg(theme.bg_highlight);
                for x in cur_x..cur_x + visible_w {
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_style(hover_bg);
                    }
                }
            }

            // Split the label at the first whitespace: the leading token
            // is the "key" (rendered bold in text_secondary) and the rest
            // is the descriptive label (rendered in gray, the tertiary
            // shade). Single-token labels render entirely as the key.
            let (key_part, label_part) = split_shortcut_label(display);

            let mut key_style = Style::default()
                .fg(theme.text_secondary)
                .add_modifier(Modifier::BOLD);
            if is_hovered {
                key_style = key_style.bg(theme.bg_highlight);
            }
            buf.set_string(cur_x, y, key_part, key_style);

            if !label_part.is_empty() {
                let key_w = key_part.width() as u16;
                let mut label_style = Style::default().fg(theme.gray);
                if is_hovered {
                    label_style = label_style.bg(theme.bg_highlight);
                }
                buf.set_string(cur_x + key_w, y, label_part, label_style);
            }

            hits.push(ShortcutHitArea {
                rect: Rect {
                    x: cur_x,
                    y,
                    width: visible_w,
                    height: 1,
                },
                id: shortcut.id,
                shortcuts_idx: shortcut_idx,
                clickable: shortcut.clickable,
            });

            cur_x += visible_w;
            // Separator after every shortcut except the last in this row.
            if local_idx + 1 < row_indices.len() {
                let sep_remaining = row_end.saturating_sub(cur_x) as usize;
                if sep_remaining == 0 {
                    break;
                }
                let sep_display = &separator[..byte_offset_at_width(separator, sep_remaining)];
                buf.set_string(cur_x, y, sep_display, Style::default().fg(theme.gray_dim));
                cur_x += sep_display.width() as u16;
            }
        }
    }

    hits
}

// ---------------------------------------------------------------------------
// Centered tip footer (Settings / How-to Guides)
// ---------------------------------------------------------------------------

/// First candidate that fits `width`, else truncate the last.
pub(crate) fn fit_tip_line<'a>(candidates: &[&'a str], width: usize) -> std::borrow::Cow<'a, str> {
    if width == 0 {
        return std::borrow::Cow::Borrowed("");
    }
    for &c in candidates {
        if c.width() <= width {
            return std::borrow::Cow::Borrowed(c);
        }
    }
    match candidates.last().copied().filter(|s| !s.is_empty()) {
        Some(last) => std::borrow::Cow::Owned(crate::render::line_utils::truncate_str(last, width)),
        None => std::borrow::Cow::Borrowed(""),
    }
}

pub(crate) fn render_centered_tip_footer(buf: &mut Buffer, area: Rect, theme: &Theme, text: &str) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let style = Style::default()
        .fg(theme.gray_dim)
        .bg(theme.bg_base)
        .add_modifier(Modifier::ITALIC);
    buf.set_style(area, Style::default().bg(theme.bg_base));
    let area_w = area.width as usize;
    let rendered: std::borrow::Cow<'_, str> = if text.width() <= area_w {
        std::borrow::Cow::Borrowed(text)
    } else {
        std::borrow::Cow::Owned(crate::render::line_utils::truncate_str(text, area_w))
    };
    let width = (rendered.width() as u16).min(area.width);
    let start_x = area.x + (area.width.saturating_sub(width) / 2);
    buf.set_span(
        start_x,
        area.y,
        &Span::styled(rendered.as_ref(), style),
        width,
    );
}

/// No tip if height < 3; blank gap above tip when height >= 6.
pub(crate) fn split_content_for_tip_footer(content: Rect) -> (Rect, Option<Rect>) {
    if content.height < 3 {
        return (content, None);
    }
    let gap = u16::from(content.height >= 6);
    let body = Rect {
        height: content.height - 1 - gap,
        ..content
    };
    let tip = Rect {
        y: content.y + content.height - 1,
        height: 1,
        ..content
    };
    (body, Some(tip))
}

/// Reserve a blank chrome row between tip and shortcut hints.
pub(crate) fn footer_lines_with_tip_gap(
    area: Rect,
    sizing: &ModalSizing,
    shortcuts: &[Shortcut<'_>],
) -> u16 {
    predict_shortcut_rows(area, sizing, shortcuts)
        .saturating_add(1)
        .max(2)
}

// ---------------------------------------------------------------------------
// Fold indicator
// ---------------------------------------------------------------------------

/// Render a fold indicator glyph at position `(x, y)`.
///
/// Draws `▶ ` (collapsed) or `▼ ` (expanded) in `gray_dim` with optional
/// hover brightening (bold + `text_primary`). The style matches both the
/// import-claude modal's header rows and the picker's expandable rows.
///
/// - `collapsed`: which glyph to show.
/// - `hovered`: brighten the glyph for hover feedback.
/// - `bg`: optional background color (e.g. `bg_highlight` for focused rows).
///
/// Returns a [`Span`] containing the indicator and trailing space (2 columns
/// wide). The caller is responsible for placing it via `buf.set_span` or
/// including it in a [`Line`].
pub fn fold_indicator_span(
    collapsed: bool,
    hovered: bool,
    bg: Option<ratatui::style::Color>,
    theme: &Theme,
) -> Span<'static> {
    let glyph = if collapsed {
        format!("{} ", crate::glyphs::chevron())
    } else {
        format!("{} ", crate::glyphs::diamond_filled())
    };
    let fg = if hovered {
        theme.text_primary
    } else {
        theme.gray_dim
    };
    let mods = if hovered {
        Modifier::BOLD
    } else {
        Modifier::empty()
    };
    let mut style = Style::default().fg(fg).add_modifier(mods);
    if let Some(c) = bg {
        style = style.bg(c);
    }
    Span::styled(glyph, style)
}

/// Render a fold indicator directly into a buffer at `(x, y)`.
///
/// Convenience wrapper around [`fold_indicator_span`] that writes the
/// glyph into `buf` and returns the number of columns consumed (always 2).
pub fn render_fold_indicator(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    collapsed: bool,
    hovered: bool,
    bg: Option<ratatui::style::Color>,
    theme: &Theme,
) -> u16 {
    let span = fold_indicator_span(collapsed, hovered, bg, theme);
    let width = 2u16;
    buf.set_span(x, y, &span, width);
    width
}

// ---------------------------------------------------------------------------
// Input handling
// ---------------------------------------------------------------------------

/// Process a key event against the modal chrome.
///
/// Returns:
/// - `CloseRequested` for Esc.
/// - When tab bar is focused (`state.tabs_focused`): Left/Right/h/l return
///   `Unhandled` (picker/caller handles tab switching).
/// - When `config.fold_info` is `Some` and tabs not focused: Left/Right/h/l
///   return specific fold outcomes based on the focused entry's state.
/// - Otherwise `Unhandled` for Left/Right etc so the caller can handle.
/// - `Unhandled` for everything else.
pub fn handle_modal_key(
    state: &mut ModalWindowState,
    key: &KeyEvent,
    config: &ModalWindowConfig<'_>,
) -> ModalWindowOutcome {
    match key.code {
        KeyCode::Esc => ModalWindowOutcome::CloseRequested,
        KeyCode::Left | KeyCode::Char('h') => {
            if state.tabs_focused {
                ModalWindowOutcome::Unhandled
            } else if let Some(ref fold) = config.fold_info {
                if fold.collapsible && fold.expanded {
                    ModalWindowOutcome::CollapseGroup
                } else if fold.has_details && fold.details_expanded {
                    ModalWindowOutcome::CollapseDetails
                } else if let Some(parent) = fold.parent_index {
                    ModalWindowOutcome::JumpToParent(parent)
                } else {
                    ModalWindowOutcome::Unhandled
                }
            } else {
                ModalWindowOutcome::Unhandled
            }
        }
        KeyCode::Right | KeyCode::Char('l') => {
            if state.tabs_focused {
                ModalWindowOutcome::Unhandled
            } else if let Some(ref fold) = config.fold_info {
                if fold.collapsible && !fold.expanded {
                    ModalWindowOutcome::ExpandGroup
                } else if fold.has_details && !fold.details_expanded {
                    ModalWindowOutcome::ExpandDetails
                } else {
                    ModalWindowOutcome::Unhandled
                }
            } else {
                ModalWindowOutcome::Unhandled
            }
        }
        _ => ModalWindowOutcome::Unhandled,
    }
}

/// Process a mouse event against the modal chrome. Returns `Unhandled` if
/// the event should be passed to the caller's content handler.
pub fn handle_modal_mouse(
    state: &mut ModalWindowState,
    kind: MouseEventKind,
    column: u16,
    row: u16,
) -> ModalWindowOutcome {
    let in_rect = |r: Rect| -> bool {
        column >= r.x && column < r.x + r.width && row >= r.y && row < r.y + r.height
    };

    let on_close = state.close_button_rect.is_some_and(&in_rect);

    // Check if on a tab.
    let on_tab: Option<usize> = state
        .tab_rects
        .iter()
        .enumerate()
        .find_map(|(i, r)| r.filter(|r| in_rect(*r)).map(|_| i));

    // Check if on a clickable shortcut (for click dispatch).
    let on_shortcut: Option<usize> = state
        .shortcut_hits
        .iter()
        .find(|hit| hit.clickable && in_rect(hit.rect))
        .map(|hit| hit.id);

    // Check which shortcut index is hovered (for hover state tracking).
    // Uses `shortcuts_idx` (the index into the full shortcuts slice) so
    // the hover index matches the render loop's `idx` in
    // `render_modal_shortcuts`.
    let shortcut_hover_idx: Option<usize> = state
        .shortcut_hits
        .iter()
        .find(|hit| in_rect(hit.rect))
        .map(|hit| hit.shortcuts_idx);

    match kind {
        MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
            if on_close {
                return ModalWindowOutcome::CloseRequested;
            }
            if let Some(tab_idx) = on_tab {
                if tab_idx != state.active_tab {
                    state.active_tab = tab_idx;
                    return ModalWindowOutcome::TabChanged(tab_idx);
                }
                return ModalWindowOutcome::Handled;
            }
            if let Some(id) = on_shortcut {
                return ModalWindowOutcome::ShortcutActivated(id);
            }
            // Click outside the popup area = close.
            if let Some(popup) = state.popup_area
                && !in_rect(popup)
            {
                return ModalWindowOutcome::CloseRequested;
            }
            ModalWindowOutcome::Unhandled
        }
        MouseEventKind::Moved => {
            let mut changed = false;
            if state.close_hovered != on_close {
                state.close_hovered = on_close;
                changed = true;
            }
            if state.hovered_shortcut != shortcut_hover_idx {
                state.hovered_shortcut = shortcut_hover_idx;
                changed = true;
            }
            if on_close || shortcut_hover_idx.is_some() || on_tab.is_some() {
                // Cursor is on chrome -- consume the event so the caller
                // doesn't interpret it as content interaction.
                return ModalWindowOutcome::Handled;
            }
            if changed {
                // Chrome hover state changed (e.g. cursor left a shortcut)
                // but is now in the content area. Return Handled to ensure
                // a redraw clears the stale highlight. The content hover
                // will update on the next mouse move.
                return ModalWindowOutcome::Handled;
            }
            ModalWindowOutcome::Unhandled
        }
        _ => ModalWindowOutcome::Unhandled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};

    fn dummy_config<'a>() -> ModalWindowConfig<'a> {
        ModalWindowConfig {
            title: "Test",
            tabs: None,
            shortcuts: &[],
            sizing: ModalSizing::default(),
            fold_info: None,
        }
    }

    /// The `i search` footer hint appears only in vim NAV mode (vim-mode on,
    /// search inactive) — not when typing, not when vim-mode is off.
    #[test]
    fn vim_nav_search_hint_only_in_vim_nav_mode() {
        crate::appearance::cache::set_vim_mode(true);
        let mut nav: Vec<Shortcut<'static>> = vec![];
        push_vim_nav_search_hint(&mut nav, false);
        assert!(
            nav.iter().any(|s| s.label == "i search"),
            "vim + nav must surface the i hint"
        );

        let mut searching: Vec<Shortcut<'static>> = vec![];
        push_vim_nav_search_hint(&mut searching, true);
        assert!(searching.is_empty(), "no i hint while already searching");

        crate::appearance::cache::set_vim_mode(false);
        let mut off: Vec<Shortcut<'static>> = vec![];
        push_vim_nav_search_hint(&mut off, false);
        assert!(off.is_empty(), "no i hint when vim-mode is off");
    }

    /// Embedded mode (minimal) fills the given area with no centered popup box;
    /// the default (full TUI) renders a smaller, centered popup.
    #[test]
    #[serial_test::serial]
    fn embedded_fills_area_without_centering() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;

        let theme = Theme::current();
        let area = Rect::new(0, 0, 80, 24);
        let config = dummy_config();

        // Default (full TUI): a centered popup strictly smaller than the area.
        set_embedded(false);
        let mut buf = Buffer::empty(area);
        let mut state = ModalWindowState::new();
        let centered =
            render_modal_window(&mut buf, area, &mut state, &config, &theme).expect("renders");
        assert_ne!(
            state.popup_area,
            Some(area),
            "full TUI centers a smaller popup"
        );

        // Embedded (minimal): fills the full area, borderless — so content is
        // wider than the centered popup's content.
        set_embedded(true);
        let mut buf = Buffer::empty(area);
        let mut state = ModalWindowState::new();
        let embedded =
            render_modal_window(&mut buf, area, &mut state, &config, &theme).expect("renders");
        assert_eq!(
            state.popup_area,
            Some(area),
            "embedded fills the whole area (no centered box)"
        );
        assert!(
            embedded.content.width > centered.content.width,
            "embedded content ({}) spans wider than the centered popup ({})",
            embedded.content.width,
            centered.content.width
        );

        // Reset the process-global flag for other tests.
        set_embedded(false);
    }

    #[test]
    fn centered_tip_footer_centers_and_clips() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;

        let theme = Theme::current();
        const TEXT: &str = "Tip · Ask EchoAgent about the docs";
        let row_text = |width: u16| {
            let area = Rect {
                x: 0,
                y: 0,
                width,
                height: 1,
            };
            let mut buf = Buffer::empty(area);
            render_centered_tip_footer(&mut buf, area, &theme, TEXT);
            (0..width)
                .filter_map(|x| buf.cell((x, 0)).map(|c| c.symbol().to_string()))
                .collect::<String>()
        };

        let wide = row_text(80);
        let start = wide.find("Tip").expect("Tip");
        let trailing = wide.chars().rev().take_while(|c| *c == ' ').count();
        assert!(start.abs_diff(trailing) <= 1, "not centered: {wide:?}");
        assert!(wide.contains("Ask EchoAgent about the docs"));

        let tiny = row_text(10);
        assert!(tiny.contains("Tip"));
        assert!(tiny.trim_end().chars().count() <= 10);
    }

    #[test]
    fn split_content_for_tip_footer_thresholds() {
        let (body, tip) = split_content_for_tip_footer(Rect {
            x: 2,
            y: 3,
            width: 40,
            height: 8,
        });
        assert_eq!(body.height, 6);
        assert_eq!(tip.expect("tip").y, 3 + 8 - 1);

        let (body, tip) = split_content_for_tip_footer(Rect {
            x: 0,
            y: 0,
            width: 20,
            height: 4,
        });
        assert_eq!(body.height, 3);
        assert!(tip.is_some());

        let tiny = Rect {
            x: 0,
            y: 0,
            width: 20,
            height: 2,
        };
        let (body, tip) = split_content_for_tip_footer(tiny);
        assert!(tip.is_none());
        assert_eq!(body, tiny);
    }

    #[test]
    fn fit_tip_line_picks_first_fit_then_truncates() {
        assert_eq!(fit_tip_line(&["abcdef", "xy"], 10).as_ref(), "abcdef");
        assert_eq!(fit_tip_line(&["abcdef", "xy"], 4).as_ref(), "xy");
        assert!(fit_tip_line(&["abcdef", "xy"], 1).as_ref().width() <= 1);
    }

    // -- ModalSizing::with_compact tests --

    #[test]
    fn modal_sizing_with_compact_reduces_margins_aggressively() {
        let base = ModalSizing {
            width_pct: 0.8,
            max_width: 120,
            min_width: 50,
            v_margin: 5,
            h_pad: 3,
            v_pad: 2,
            footer_lines: 2,
        };

        // Default (compact=false) keeps original values
        assert_eq!(base.v_margin, 5);
        assert_eq!(base.h_pad, 3);
        assert_eq!(base.v_pad, 2);

        let compact = base.with_compact(true);
        assert_eq!(
            compact.v_margin, 0,
            "v_margin must be 0 in compact mode for max height"
        );
        assert_eq!(
            compact.h_pad, 1,
            "h_pad clamped to MIN 1 for accent/selection border"
        );
        assert_eq!(
            compact.v_pad, 0,
            "v_pad must be 0 so search bar / first row sits tight"
        );
        // Other fields unchanged
        assert_eq!(compact.width_pct, 0.8);
        assert_eq!(compact.max_width, 120);

        // Calling with false is a no-op
        let unchanged = base.with_compact(false);
        assert_eq!(unchanged.v_margin, 5);
        assert_eq!(unchanged.h_pad, 3);
    }

    // -- compute_modal_dims --

    #[test]
    fn modal_width_never_exceeds_narrow_terminal() {
        // Regression: the min_width floor used to re-inflate the modal past a narrow buffer.
        for sizing in [ModalSizing::medium(), ModalSizing::large()] {
            for width in 0..=70 {
                let (w, _) = compute_modal_dims(Rect::new(0, 0, width, 85), &sizing);
                assert!(w <= width, "modal width {w} overflows {width}-col terminal");
            }
        }
    }

    // -- ModalWindowState construction --

    #[test]
    fn new_defaults() {
        let s = ModalWindowState::new();
        assert!(!s.close_hovered);
        assert_eq!(s.close_button_rect, None);
        assert_eq!(s.popup_area, None);
        assert_eq!(s.active_tab, 0);
        assert_eq!(s.tab_count, 0);
        assert!(s.tab_rects.is_empty());
        assert!(s.shortcut_hits.is_empty());
        assert_eq!(s.hovered_shortcut, None);
    }

    #[test]
    fn with_tabs_initialises_rects() {
        let s = ModalWindowState::with_tabs(3);
        assert_eq!(s.tab_count, 3);
        assert_eq!(s.tab_rects.len(), 3);
        assert!(s.tab_rects.iter().all(|r| r.is_none()));
    }

    #[test]
    fn default_matches_new() {
        let a = ModalWindowState::new();
        let b = ModalWindowState::default();
        assert_eq!(a.tab_count, b.tab_count);
        assert_eq!(a.close_hovered, b.close_hovered);
    }

    // -- handle_modal_key --

    #[test]
    fn key_esc_returns_close_requested() {
        let mut state = ModalWindowState::new();
        let config = dummy_config();
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(
            handle_modal_key(&mut state, &esc, &config),
            ModalWindowOutcome::CloseRequested
        );
    }

    #[test]
    fn key_other_returns_unhandled() {
        let mut state = ModalWindowState::new();
        let config = dummy_config();
        let j = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(
            handle_modal_key(&mut state, &j, &config),
            ModalWindowOutcome::Unhandled
        );
    }

    #[test]
    fn key_left_without_fold_info_returns_unhandled() {
        let mut state = ModalWindowState::new();
        let config = dummy_config();
        let left = KeyEvent::new(KeyCode::Left, KeyModifiers::NONE);
        assert_eq!(
            handle_modal_key(&mut state, &left, &config),
            ModalWindowOutcome::Unhandled
        );
    }

    #[test]
    fn key_h_without_fold_info_returns_unhandled() {
        let mut state = ModalWindowState::new();
        let config = dummy_config();
        let h = KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE);
        assert_eq!(
            handle_modal_key(&mut state, &h, &config),
            ModalWindowOutcome::Unhandled
        );
    }

    #[test]
    fn key_right_without_fold_info_returns_unhandled() {
        let mut state = ModalWindowState::new();
        let config = dummy_config();
        let right = KeyEvent::new(KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(
            handle_modal_key(&mut state, &right, &config),
            ModalWindowOutcome::Unhandled
        );
    }

    #[test]
    fn key_l_without_fold_info_returns_unhandled() {
        let mut state = ModalWindowState::new();
        let config = dummy_config();
        let l = KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE);
        assert_eq!(
            handle_modal_key(&mut state, &l, &config),
            ModalWindowOutcome::Unhandled
        );
    }

    // -- handle_modal_key with FoldInfo --

    fn config_with_fold<'a>(fold_info: FoldInfo) -> ModalWindowConfig<'a> {
        ModalWindowConfig {
            title: "Test",
            tabs: None,
            shortcuts: &[],
            sizing: ModalSizing::default(),
            fold_info: Some(fold_info),
        }
    }

    #[test]
    fn left_on_expanded_collapsible_returns_collapse_group() {
        let mut state = ModalWindowState::new();
        let config = config_with_fold(FoldInfo {
            collapsible: true,
            expanded: true,
            has_details: false,
            details_expanded: false,
            parent_index: None,
        });
        let left = KeyEvent::new(KeyCode::Left, KeyModifiers::NONE);
        assert_eq!(
            handle_modal_key(&mut state, &left, &config),
            ModalWindowOutcome::CollapseGroup
        );
    }

    #[test]
    fn right_on_collapsed_collapsible_returns_expand_group() {
        let mut state = ModalWindowState::new();
        let config = config_with_fold(FoldInfo {
            collapsible: true,
            expanded: false,
            has_details: false,
            details_expanded: false,
            parent_index: None,
        });
        let right = KeyEvent::new(KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(
            handle_modal_key(&mut state, &right, &config),
            ModalWindowOutcome::ExpandGroup
        );
    }

    #[test]
    fn left_on_collapsed_collapsible_with_parent_returns_jump() {
        let mut state = ModalWindowState::new();
        let config = config_with_fold(FoldInfo {
            collapsible: true,
            expanded: false,
            has_details: false,
            details_expanded: false,
            parent_index: Some(3),
        });
        let left = KeyEvent::new(KeyCode::Left, KeyModifiers::NONE);
        assert_eq!(
            handle_modal_key(&mut state, &left, &config),
            ModalWindowOutcome::JumpToParent(3)
        );
    }

    #[test]
    fn left_on_collapsed_collapsible_without_parent_returns_unhandled() {
        let mut state = ModalWindowState::new();
        let config = config_with_fold(FoldInfo {
            collapsible: true,
            expanded: false,
            has_details: false,
            details_expanded: false,
            parent_index: None,
        });
        let left = KeyEvent::new(KeyCode::Left, KeyModifiers::NONE);
        assert_eq!(
            handle_modal_key(&mut state, &left, &config),
            ModalWindowOutcome::Unhandled
        );
    }

    #[test]
    fn left_on_expanded_details_returns_collapse_details() {
        let mut state = ModalWindowState::new();
        let config = config_with_fold(FoldInfo {
            collapsible: false,
            expanded: false,
            has_details: true,
            details_expanded: true,
            parent_index: Some(0),
        });
        let left = KeyEvent::new(KeyCode::Left, KeyModifiers::NONE);
        assert_eq!(
            handle_modal_key(&mut state, &left, &config),
            ModalWindowOutcome::CollapseDetails
        );
    }

    #[test]
    fn right_on_collapsed_details_returns_expand_details() {
        let mut state = ModalWindowState::new();
        let config = config_with_fold(FoldInfo {
            collapsible: false,
            expanded: false,
            has_details: true,
            details_expanded: false,
            parent_index: Some(0),
        });
        let right = KeyEvent::new(KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(
            handle_modal_key(&mut state, &right, &config),
            ModalWindowOutcome::ExpandDetails
        );
    }

    #[test]
    fn left_on_leaf_without_details_returns_jump_to_parent() {
        let mut state = ModalWindowState::new();
        let config = config_with_fold(FoldInfo {
            collapsible: false,
            expanded: false,
            has_details: false,
            details_expanded: false,
            parent_index: Some(5),
        });
        let left = KeyEvent::new(KeyCode::Left, KeyModifiers::NONE);
        assert_eq!(
            handle_modal_key(&mut state, &left, &config),
            ModalWindowOutcome::JumpToParent(5)
        );
    }

    #[test]
    fn right_on_expanded_collapsible_returns_unhandled() {
        let mut state = ModalWindowState::new();
        let config = config_with_fold(FoldInfo {
            collapsible: true,
            expanded: true,
            has_details: false,
            details_expanded: false,
            parent_index: None,
        });
        let right = KeyEvent::new(KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(
            handle_modal_key(&mut state, &right, &config),
            ModalWindowOutcome::Unhandled
        );
    }

    #[test]
    fn h_key_uses_fold_info_same_as_left() {
        let mut state = ModalWindowState::new();
        let config = config_with_fold(FoldInfo {
            collapsible: true,
            expanded: true,
            has_details: false,
            details_expanded: false,
            parent_index: None,
        });
        let h = KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE);
        assert_eq!(
            handle_modal_key(&mut state, &h, &config),
            ModalWindowOutcome::CollapseGroup
        );
    }

    #[test]
    fn l_key_uses_fold_info_same_as_right() {
        let mut state = ModalWindowState::new();
        let config = config_with_fold(FoldInfo {
            collapsible: true,
            expanded: false,
            has_details: false,
            details_expanded: false,
            parent_index: None,
        });
        let l = KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE);
        assert_eq!(
            handle_modal_key(&mut state, &l, &config),
            ModalWindowOutcome::ExpandGroup
        );
    }

    // -- FoldInfo precedence & edge cases --

    #[test]
    fn left_collapse_group_wins_over_collapse_details() {
        // When both collapsible+expanded AND has_details+details_expanded
        // are true, CollapseGroup takes priority (group collapse first).
        let mut state = ModalWindowState::new();
        let config = config_with_fold(FoldInfo {
            collapsible: true,
            expanded: true,
            has_details: true,
            details_expanded: true,
            parent_index: Some(0),
        });
        let left = KeyEvent::new(KeyCode::Left, KeyModifiers::NONE);
        assert_eq!(
            handle_modal_key(&mut state, &left, &config),
            ModalWindowOutcome::CollapseGroup
        );
    }

    #[test]
    fn right_expand_group_wins_over_expand_details() {
        // When both collapsible+collapsed AND has_details+!details_expanded,
        // ExpandGroup takes priority.
        let mut state = ModalWindowState::new();
        let config = config_with_fold(FoldInfo {
            collapsible: true,
            expanded: false,
            has_details: true,
            details_expanded: false,
            parent_index: Some(0),
        });
        let right = KeyEvent::new(KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(
            handle_modal_key(&mut state, &right, &config),
            ModalWindowOutcome::ExpandGroup
        );
    }

    #[test]
    fn right_on_expanded_collapsible_with_unexpanded_details_returns_expand_details() {
        // Expanded group header still has unexpanded detail fields:
        // Right falls through to ExpandDetails.
        let mut state = ModalWindowState::new();
        let config = config_with_fold(FoldInfo {
            collapsible: true,
            expanded: true,
            has_details: true,
            details_expanded: false,
            parent_index: None,
        });
        let right = KeyEvent::new(KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(
            handle_modal_key(&mut state, &right, &config),
            ModalWindowOutcome::ExpandDetails
        );
    }

    #[test]
    fn left_on_bare_leaf_no_parent_returns_unhandled() {
        // Leaf with no details and no parent: nothing to do.
        let mut state = ModalWindowState::new();
        let config = config_with_fold(FoldInfo {
            collapsible: false,
            expanded: false,
            has_details: false,
            details_expanded: false,
            parent_index: None,
        });
        let left = KeyEvent::new(KeyCode::Left, KeyModifiers::NONE);
        assert_eq!(
            handle_modal_key(&mut state, &left, &config),
            ModalWindowOutcome::Unhandled
        );
    }

    #[test]
    fn right_on_fully_expanded_details_returns_unhandled() {
        // Details already expanded, not collapsible: nothing to expand.
        let mut state = ModalWindowState::new();
        let config = config_with_fold(FoldInfo {
            collapsible: false,
            expanded: false,
            has_details: true,
            details_expanded: true,
            parent_index: Some(0),
        });
        let right = KeyEvent::new(KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(
            handle_modal_key(&mut state, &right, &config),
            ModalWindowOutcome::Unhandled
        );
    }

    #[test]
    fn esc_with_fold_info_returns_close_requested() {
        let mut state = ModalWindowState::new();
        let config = config_with_fold(FoldInfo {
            collapsible: true,
            expanded: true,
            has_details: false,
            details_expanded: false,
            parent_index: None,
        });
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(
            handle_modal_key(&mut state, &esc, &config),
            ModalWindowOutcome::CloseRequested
        );
    }

    // -- handle_modal_mouse --

    #[test]
    fn click_on_close_button_returns_close_requested() {
        let mut state = ModalWindowState::new();
        state.close_button_rect = Some(Rect {
            x: 90,
            y: 5,
            width: 5,
            height: 1,
        });
        state.popup_area = Some(Rect {
            x: 10,
            y: 5,
            width: 100,
            height: 30,
        });
        let outcome =
            handle_modal_mouse(&mut state, MouseEventKind::Down(MouseButton::Left), 92, 5);
        assert_eq!(outcome, ModalWindowOutcome::CloseRequested);
    }

    #[test]
    fn click_outside_popup_returns_close_requested() {
        let mut state = ModalWindowState::new();
        state.popup_area = Some(Rect {
            x: 10,
            y: 5,
            width: 80,
            height: 30,
        });
        let outcome = handle_modal_mouse(&mut state, MouseEventKind::Down(MouseButton::Left), 5, 5);
        assert_eq!(outcome, ModalWindowOutcome::CloseRequested);
    }

    #[test]
    fn click_inside_popup_no_chrome_returns_unhandled() {
        let mut state = ModalWindowState::new();
        state.popup_area = Some(Rect {
            x: 10,
            y: 5,
            width: 80,
            height: 30,
        });
        let outcome =
            handle_modal_mouse(&mut state, MouseEventKind::Down(MouseButton::Left), 50, 20);
        assert_eq!(outcome, ModalWindowOutcome::Unhandled);
    }

    #[test]
    fn click_on_shortcut_returns_shortcut_activated() {
        let mut state = ModalWindowState::new();
        state.popup_area = Some(Rect {
            x: 10,
            y: 5,
            width: 80,
            height: 30,
        });
        state.shortcut_hits = vec![ShortcutHitArea {
            rect: Rect {
                x: 30,
                y: 32,
                width: 10,
                height: 1,
            },
            id: 42,
            shortcuts_idx: 3,
            clickable: true,
        }];
        let outcome =
            handle_modal_mouse(&mut state, MouseEventKind::Down(MouseButton::Left), 35, 32);
        assert_eq!(outcome, ModalWindowOutcome::ShortcutActivated(42));
    }

    #[test]
    fn hover_over_close_sets_hovered_and_returns_handled() {
        let mut state = ModalWindowState::new();
        state.close_button_rect = Some(Rect {
            x: 90,
            y: 5,
            width: 5,
            height: 1,
        });
        state.popup_area = Some(Rect {
            x: 10,
            y: 5,
            width: 100,
            height: 30,
        });
        assert!(!state.close_hovered);
        let outcome = handle_modal_mouse(&mut state, MouseEventKind::Moved, 92, 5);
        assert_eq!(outcome, ModalWindowOutcome::Handled);
        assert!(state.close_hovered);
    }

    #[test]
    fn hover_leaving_shortcut_returns_handled_for_redraw() {
        let mut state = ModalWindowState::new();
        state.popup_area = Some(Rect {
            x: 10,
            y: 5,
            width: 80,
            height: 30,
        });
        state.shortcut_hits = vec![ShortcutHitArea {
            rect: Rect {
                x: 30,
                y: 32,
                width: 10,
                height: 1,
            },
            id: 1,
            shortcuts_idx: 4,
            clickable: true,
        }];
        state.hovered_shortcut = Some(4);
        let outcome = handle_modal_mouse(&mut state, MouseEventKind::Moved, 50, 20);
        assert_eq!(outcome, ModalWindowOutcome::Handled);
        assert_eq!(state.hovered_shortcut, None);
    }

    #[test]
    fn hover_no_change_returns_unhandled() {
        let mut state = ModalWindowState::new();
        state.popup_area = Some(Rect {
            x: 10,
            y: 5,
            width: 80,
            height: 30,
        });
        let outcome = handle_modal_mouse(&mut state, MouseEventKind::Moved, 50, 20);
        assert_eq!(outcome, ModalWindowOutcome::Unhandled);
    }

    #[test]
    fn hover_shortcut_uses_shortcuts_idx_not_position_in_hits() {
        // Regression: when non-clickable hint shortcuts precede clickable
        // ones, the hover index must match the full shortcuts-array index
        // (shortcuts_idx), not the position within shortcut_hits. Otherwise
        // the renderer's `hovered == Some(idx)` comparison never matches
        // and hover highlights never appear.
        let mut state = ModalWindowState::new();
        state.popup_area = Some(Rect {
            x: 10,
            y: 5,
            width: 80,
            height: 30,
        });
        // Simulate 3 non-clickable hints before this clickable shortcut.
        // The clickable shortcut is at shortcuts_idx=3 in the full array
        // but at position 0 in shortcut_hits.
        state.shortcut_hits = vec![ShortcutHitArea {
            rect: Rect {
                x: 40,
                y: 32,
                width: 8,
                height: 1,
            },
            id: 99,
            shortcuts_idx: 3,
            clickable: true,
        }];
        let outcome = handle_modal_mouse(&mut state, MouseEventKind::Moved, 44, 32);
        assert_eq!(outcome, ModalWindowOutcome::Handled);
        // hovered_shortcut must be 3 (shortcuts_idx), not 0 (position in hits)
        assert_eq!(state.hovered_shortcut, Some(3));
    }

    // -- ModalSizing presets --

    #[test]
    fn modal_sizing_medium_has_expected_values() {
        let m = ModalSizing::medium();
        assert_eq!(m.width_pct, 0.60);
        assert_eq!(m.max_width, 120);
        assert_eq!(m.min_width, 44);
        assert_eq!(m.v_margin, 4);
        assert_eq!(m.h_pad, 2);
        assert_eq!(m.v_pad, 1);
        assert_eq!(m.footer_lines, 2);
    }

    #[test]
    fn modal_sizing_large_matches_default() {
        assert_eq!(ModalSizing::large(), ModalSizing::default());
    }

    // -- split_shortcut_label --

    #[test]
    fn split_shortcut_label_basic_ascii() {
        assert_eq!(split_shortcut_label("Esc cancel"), ("Esc", " cancel"));
        assert_eq!(split_shortcut_label("Enter select"), ("Enter", " select"));
    }

    #[test]
    fn split_shortcut_label_multi_word_label() {
        // Only the first whitespace splits — remaining spaces stay in label.
        assert_eq!(
            split_shortcut_label("Enter import 3"),
            ("Enter", " import 3")
        );
        assert_eq!(
            split_shortcut_label("x confirm delete"),
            ("x", " confirm delete")
        );
    }

    #[test]
    fn split_shortcut_label_unicode_arrows() {
        // Arrow characters are multi-byte but split_at uses the byte
        // offset returned by char_indices, so the slice is valid UTF-8.
        let (k, l) = split_shortcut_label("\u{2191}/\u{2193} nav");
        assert_eq!(k, "\u{2191}/\u{2193}");
        assert_eq!(l, " nav");

        let (k, l) = split_shortcut_label("\u{2191}\u{2193} nav");
        assert_eq!(k, "\u{2191}\u{2193}");
        assert_eq!(l, " nav");
    }

    #[test]
    fn split_shortcut_label_no_whitespace_is_key_only() {
        assert_eq!(split_shortcut_label("Esc"), ("Esc", ""));
        assert_eq!(split_shortcut_label(""), ("", ""));
    }

    #[test]
    fn split_shortcut_label_only_splits_on_ascii_space() {
        // Tabs, NBSP, and other non-space whitespace must NOT split — only
        // ASCII ' ' is a separator. This keeps interpolated labels safe
        // from accidental key/label boundary surprises.
        assert_eq!(split_shortcut_label("Esc\tcancel"), ("Esc\tcancel", ""));
        assert_eq!(
            split_shortcut_label("Esc\u{00A0}cancel"),
            ("Esc\u{00A0}cancel", "")
        );
        // ASCII space takes precedence even when other whitespace appears later.
        assert_eq!(
            split_shortcut_label("Esc cancel\twith\ttabs"),
            ("Esc", " cancel\twith\ttabs")
        );
    }
}
