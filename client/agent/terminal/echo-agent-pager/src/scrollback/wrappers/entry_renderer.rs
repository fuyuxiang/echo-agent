//! EntryRenderer - renders a ScrollbackEntry using composed wrappers.

use std::borrow::Cow;
use std::cell::OnceCell;
use std::path::Path;

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;

use crate::appearance::AppearanceConfig;
use crate::render::color::blend_color;
use crate::render::{Renderable, SafeBuf};
use crate::scrollback::BlockOutput;
use crate::scrollback::block::{BlockContent, RenderBlock};
use crate::scrollback::entry::ScrollbackEntry;
use crate::scrollback::layout::HorizontalLayout;
use crate::scrollback::types::{AccentStyle, BlockBackground, DisplayMode, Selectable};
use crate::theme::{self, Theme};

/// Animation speed for running blocks (radians per tick).
/// ~0.15 gives a nice smooth wave that travels the block in ~40 ticks.
const WAVE_SPEED: f32 = 0.15;

pub struct EntryRenderer<'a> {
    entry: &'a ScrollbackEntry,
    theme: &'a Theme,
    /// Deliberately NOT eagerly `AppearanceConfig::default()`: that conversion
    /// reads `Theme::current()` and quantizes every color, and profiling a
    /// resize showed it running once per entry only to be overwritten.
    appearance: OnceCell<Cow<'a, AppearanceConfig>>,
    tick: u64,
    /// Number of rows to skip from the top of the entry.
    ///
    /// When non-zero, the renderer acts as if the entry starts `skip_rows`
    /// rows lower — it omits that many top rows (vpad, then content lines)
    /// and renders the remainder into `area`. This eliminates the need for
    /// scratch-buffer rendering of partially-visible entries.
    skip_rows: u16,
    /// Whether this entry's block is groupable (participates in dense groups).
    /// When true AND display_mode == Collapsed, the bullet is dimmed.
    groupable: bool,
    /// Whether this entry is currently selected in the scrollback.
    is_selected: bool,
    /// Mouse position for timestamp hover detection.
    mouse_pos: Option<(u16, u16)>,
    /// When non-zero, this entry renders as a group truncation header
    /// ("╶╶ N more") instead of its normal block content.
    group_header_count: u16,
    /// When true, this is a collapse header for an expanded group
    /// ("▾ N tool calls" instead of "╶╶ N more").
    group_collapse_header: bool,
    /// Aggregated group-header label; when set, group-header rows render it
    /// instead of the plain "N more" / "N tool calls" text. The variant
    /// picks the chrome: verb-run headers wear running/error accents from
    /// the run state, truncation headers keep the dimmed fold chrome.
    group_header_label: Option<&'a crate::scrollback::state::verb_group::GroupHeaderLabel>,
    /// When true, suppress the block's background band (force
    /// `BlockBackground::None`) and per-line "panel" bands
    /// ([`BlockLine::background_is_panel`] — tool result preview boxes) so the
    /// entry blends with the terminal's own background. Used by minimal mode,
    /// which prints into native scrollback where a fixed-color band (e.g. the
    /// user-message `bg_light` or a tool preview's `bg_dark`) clashes with the
    /// user's real terminal background. Semantic per-line backgrounds
    /// (code-block syntax shading, diff insert/delete rows) and the accent
    /// column are unaffected.
    flat_background: bool,
    /// When true, reclaim the accent column for content (chrome width drops by [`HorizontalLayout::ACCENT`]).
    /// Minimal mode pairs this with zeroed `block_pad_{left,right}`, so content starts at column 0, aligned with the welcome card.
    hide_accent: bool,
    /// Paint the accent bar with [`Modifier::DIM`] on top of its color, so a
    /// rail that resolved to `Color::Reset` reads as chrome rather than
    /// full-brightness content.
    dim_accent: bool,
    /// Session/worktree cwd (`AgentSession.cwd`) for Expanded tool paths.
    cwd: Option<&'a Path>,
}

impl<'a> EntryRenderer<'a> {
    pub fn new(entry: &'a ScrollbackEntry, theme: &'a Theme) -> Self {
        Self {
            entry,
            theme,
            appearance: OnceCell::new(),
            tick: 0,
            skip_rows: 0,
            groupable: false,
            is_selected: false,
            mouse_pos: None,
            group_header_count: 0,
            group_collapse_header: false,
            group_header_label: None,
            flat_background: false,
            hide_accent: false,
            dim_accent: false,
            cwd: None,
        }
    }

    pub fn with_cwd(mut self, cwd: Option<&'a Path>) -> Self {
        self.cwd = cwd;
        self
    }

    /// Suppress the block background band so the entry blends with the
    /// terminal's own background (minimal mode). See [`Self::flat_background`].
    pub fn with_flat_background(mut self, flat: bool) -> Self {
        self.flat_background = flat;
        self
    }

    /// Reclaim the accent column for content (minimal mode's flush-left look). See [`Self::hide_accent`].
    pub fn with_hide_accent(mut self, hide: bool) -> Self {
        self.hide_accent = hide;
        self
    }

    /// See [`Self::dim_accent`]. Height-neutral — `chrome_width` is unchanged.
    pub fn with_dim_accent(mut self, dim: bool) -> Self {
        self.dim_accent = dim;
        self
    }

    /// Background to paint where the block itself has none (accent column,
    /// gutter, bullets). In flat mode this is `Color::Reset` — the terminal's
    /// own default background — so the entry inherits terminal transparency
    /// instead of an opaque `bg_base` strip; otherwise it is `bg_base`.
    fn fallback_bg(&self) -> ratatui::style::Color {
        if self.flat_background {
            ratatui::style::Color::Reset
        } else {
            self.theme.bg_base
        }
    }

    /// The block's rail style, resolved through a full context so selection and width reach it.
    fn accent(&self, content_width: u16) -> Option<AccentStyle> {
        let mut ctx = self
            .entry
            .context(content_width, self.appearance(), self.cwd);
        ctx.is_selected = self.is_selected;
        self.entry.block.accent(&ctx)
    }

    /// Shared by every rail branch so it cannot be dim in one running state and bright in another.
    fn accent_paint_style(&self, color: ratatui::style::Color) -> Style {
        let style = Style::default().fg(color);
        if self.dim_accent {
            style.add_modifier(ratatui::style::Modifier::DIM)
        } else {
            style
        }
    }

    pub fn with_appearance(mut self, appearance: AppearanceConfig) -> Self {
        self.appearance = OnceCell::from(Cow::Owned(appearance));
        self
    }

    ///
    /// Preferred inside the O(history) layout loops, which would otherwise
    /// clone the config once per entry.
    pub fn with_appearance_ref(mut self, appearance: &'a AppearanceConfig) -> Self {
        self.appearance = OnceCell::from(Cow::Borrowed(appearance));
        self
    }

    fn appearance(&self) -> &AppearanceConfig {
        self.appearance
            .get_or_init(|| Cow::Owned(AppearanceConfig::default()))
    }

    pub fn with_tick(mut self, tick: u64) -> Self {
        self.tick = tick;
        self
    }

    /// Skip the first `n` rows of the entry when rendering.
    ///
    /// The skipped rows consume vpad first, then content lines. This allows
    /// partially-visible entries to be rendered directly into the output buffer
    /// without a scratch buffer intermediate.
    pub fn with_skip_rows(mut self, n: u16) -> Self {
        self.skip_rows = n;
        self
    }

    /// Mark this entry as groupable, which dims its bullet when collapsed.
    pub fn with_groupable(mut self, groupable: bool) -> Self {
        self.groupable = groupable;
        self
    }

    /// Mark this entry as selected in the scrollback.
    pub fn with_selected(mut self, selected: bool) -> Self {
        self.is_selected = selected;
        self
    }

    /// Set the mouse position for timestamp hover detection.
    pub fn with_mouse_pos(mut self, pos: Option<(u16, u16)>) -> Self {
        self.mouse_pos = pos;
        self
    }

    /// Set the group header count for group truncation rendering.
    ///
    /// When non-zero, the renderer draws a compact "╶╶ N more" line
    /// instead of the block's normal content.
    pub fn with_group_header_count(mut self, count: u16) -> Self {
        self.group_header_count = count;
        self
    }

    pub fn with_group_collapse_header(mut self, collapse: bool) -> Self {
        self.group_collapse_header = collapse;
        self
    }

    /// Set the aggregated label for a group-header row (either fold family).
    pub fn with_group_header_label(
        mut self,
        label: Option<&'a crate::scrollback::state::verb_group::GroupHeaderLabel>,
    ) -> Self {
        self.group_header_label = label;
        self
    }

    /// Render a compact "╶╶ N more" group header line.
    ///
    /// Uses the collapsed accent char for the accent column, and renders the
    /// header text with dimmed styling to visually separate it from real entries.
    /// Avoids the full `self.accent()` path (which allocates a `BlockContext`)
    /// by reading the accent color directly from the theme.
    fn render_group_header(&self, area: Rect, buf: &mut Buffer) {
        use crate::scrollback::state::verb_group::GroupHeaderLabel;

        let layout_cfg = &self.appearance().scrollback.layout;
        let accent_w = if self.hide_accent {
            0
        } else {
            HorizontalLayout::ACCENT
        };
        let [accent_area, _left_pad, content_area, _right_pad] = Layout::horizontal([
            Constraint::Length(accent_w),
            Constraint::Length(layout_cfg.block_pad_left),
            Constraint::Min(1),
            Constraint::Length(layout_cfg.block_pad_right),
        ])
        .areas(area);

        let bg = self.theme.bg_base;

        // The column is reserved but never painted here, and an unowned cell survives the frame diff,
        // so whatever glyph the previous frame left at this position would stay.
        fill_bg_spaces(buf, accent_area, bg);

        // Verb-group header: aggregated "Verb N noun" label whose diamond takes the run-state color, so an active group's glyph
        // animates with the same wave as a running tool row's bullet.
        if let Some(GroupHeaderLabel::VerbRun(vg)) = self.group_header_label {
            use unicode_width::UnicodeWidthStr;

            let glyph_color = if vg.failed {
                self.theme.accent_error
            } else if vg.running {
                let brightness = theme::wave_brightness(
                    self.tick,
                    self.skip_rows,
                    self.appearance().animation.wave_rows,
                    WAVE_SPEED,
                );
                blend_color(bg, self.theme.accent_tool, brightness)
                    .unwrap_or(self.theme.accent_tool)
            } else {
                self.theme.gray
            };

            // Diamond chrome in BOTH states, same family as the "N more" headers.
            // The selection caret (the expandable indicator in scrollback_pane.rs) overdraws the diamond on
            // the selected row and flips `›`/`⌄` with the group's fold state.
            let prefix = group_header_chrome_prefix();
            let mut spans = vec![ratatui::text::Span::styled(
                prefix.clone(),
                Style::default().fg(glyph_color),
            )];
            let hook_start = vg
                .line
                .spans
                .iter()
                .position(|span| span.content.starts_with("  [hooks: "));
            if let Some(hook_start) = hook_start {
                let suffix_width: usize = vg.line.spans[hook_start..]
                    .iter()
                    .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
                    .sum();
                let label_budget = usize::from(content_area.width)
                    .saturating_sub(UnicodeWidthStr::width(prefix.as_str()))
                    .saturating_sub(suffix_width);
                let label = ratatui::text::Line::from(vg.line.spans[..hook_start].to_vec());
                spans.extend(crate::render::line_utils::truncate_line(label, label_budget).spans);
                spans.extend(vg.line.spans[hook_start..].iter().cloned());
            } else {
                spans.extend(vg.line.spans.iter().cloned());
            }
            let line = ratatui::text::Line::from(spans);
            // Group-header content is registered selectable (GROUP_HEADER_RANGE_ID)
            // and its selection maps visual columns, so it must paint visual too.
            buf.set_line_safe_bidi(content_area.x, content_area.y, &line, content_area.width);
            return;
        }

        // Render header: ◈ (dimmed) + text (brighter, stands out). The
        // aggregated label describes the hidden rows through the shared
        // bucket vocabulary ("Ran 6 commands"); when the caller supplied
        // none (the render loop owns the reasons) the plain count remains.
        let n = self.group_header_count;
        let diamond_style = Style::default().fg(self.theme.gray);
        let text_style = Style::default()
            .fg(self.theme.gray_bright)
            .add_modifier(ratatui::style::Modifier::BOLD);
        let mut spans = vec![ratatui::text::Span::styled(
            group_header_chrome_prefix(),
            diamond_style,
        )];
        // The VerbRun variant returned above, so only a truncation label can
        // reach this row's span assembly.
        if let Some(GroupHeaderLabel::Truncation(label)) = self.group_header_label {
            spans.extend(label.line.spans.iter().cloned());
        } else {
            let label = if self.group_collapse_header {
                format!("{n} tool calls & thoughts")
            } else {
                format!("{n} more")
            };
            spans.push(ratatui::text::Span::styled(label, text_style));
        }
        let line = ratatui::text::Line::from(spans);
        // Group-header content is registered selectable (GROUP_HEADER_RANGE_ID)
        // and its selection maps visual columns, so it must paint visual too.
        buf.set_line_safe_bidi(content_area.x, content_area.y, &line, content_area.width);
    }

    /// Get the chrome width (accent + padding) for this renderer's appearance.
    ///
    /// When [`Self::hide_accent`] is set the accent column is reclaimed, so
    /// chrome is just the block pads (typically zeroed in minimal mode).
    pub fn chrome_width(&self) -> u16 {
        let pads = self.appearance().scrollback.layout.block_pad_left
            + self.appearance().scrollback.layout.block_pad_right;
        if self.hide_accent {
            pads
        } else {
            HorizontalLayout::ACCENT + pads
        }
    }

    /// Legacy fixed chrome-width estimate for callers with no appearance to
    /// borrow (off-screen mermaid sizing); the live value is `chrome_width()`.
    pub const CHROME_WIDTH: u16 = 1 + 2 + 1; // accent + left_pad + right_pad (legacy)

    /// Whether this entry should display a timestamp on the first content line.
    ///
    /// Timestamps are shown for user and agent messages (including /btw responses
    /// and mid-turn interjections) but NOT for thinking traces, tool calls, or
    /// system messages.
    fn should_show_timestamp(&self) -> bool {
        matches!(
            self.entry.block,
            RenderBlock::UserPrompt(_) | RenderBlock::AgentMessage(_) | RenderBlock::Btw(_)
        )
    }

    /// Width reserved for the timestamp on the right side of content lines.
    ///
    /// When > 0, content is wrapped at `content_width - reserved` so text
    /// never collides with the timestamp overlay.
    fn timestamp_reserved(&self) -> u16 {
        if self.appearance().show_timestamps && self.should_show_timestamp() {
            10 // max short format: "  12:30 PM"
        } else {
            0
        }
    }

    /// Thinking entries take no rows when the Appearance toggle is off.
    fn thinking_hidden(&self) -> bool {
        self.entry
            .is_hidden_thinking(crate::appearance::cache::load_show_thinking_blocks())
    }

    /// Compute height as if displayed in Truncated mode.
    ///
    /// This avoids cloning the entry just to compute truncated height.
    /// Used by layout cache to precompute sticky header heights.
    ///
    /// Goes through the entry's truncated-height cache so repeated layout
    /// rebuilds don't re-run `block.output()` (which is expensive — full
    /// syntect highlighting for Edit blocks, full word-wrap for Markdown).
    pub fn compute_truncated_height(&self, width: u16) -> u16 {
        if self.thinking_hidden() {
            return 0;
        }
        let content_width = width
            .saturating_sub(self.chrome_width())
            .saturating_sub(self.timestamp_reserved());
        self.entry
            .ensure_truncated_height_cached(content_width, self.appearance(), self.cwd)
    }

    /// Extra rows to reserve for inline media preview (images/video poster).
    /// Returns 0 on non-graphics terminals or when the block has no media.
    fn inline_media_rows(&self, content_width: u16) -> u16 {
        use crate::terminal::image::scrollback_inline_overlay_active;

        if !scrollback_inline_overlay_active() {
            return 0;
        }
        let Some(media) = self.entry.block.inline_media() else {
            return 0;
        };
        // Shared with `inline_media_placements` so the reserved rows match the
        // painted placement exactly.
        let (_image_rows, total_rows) =
            crate::inline_media_ffmpeg::inline_media_reserved_rows(&media, content_width);
        total_rows
    }

    /// Cheap height ESTIMATE that avoids a markdown render / word-wrap.
    ///
    /// Mirrors `desired_height` but derives the content line count from the block's
    /// raw source text (`searchable_text`) instead of laying it out. Lets the layout
    /// cache size off-screen entries on a bulk load (`echoagent -r`) without word-wrapping
    /// and markdown-rendering every entry (O(history)). On-screen entries get their
    /// EXACT `desired_height`, so visible content is never estimated.
    pub fn estimate_height(&self, width: u16) -> u16 {
        if self.thinking_hidden() {
            return 0;
        }
        let content_width = width
            .saturating_sub(self.chrome_width())
            .saturating_sub(self.timestamp_reserved());
        let content_lines = self.estimate_content_lines(content_width);
        // `inline_media_rows` (in `assemble_height`) covers trailing tool media.
        // `estimate_extra_rows` adds the Mermaid treatment rows (one affordance
        // row / fallback caption per diagram) that live inside `output()` and are
        // invisible to this source-based estimate, so it never under-reserves.
        self.assemble_height(content_width, content_lines)
            .saturating_add(self.entry.block.estimate_extra_rows())
    }

    /// Estimate the rendered content-line count without laying the block out.
    ///
    /// Memoized per content width on the entry (cleared by `invalidate_cache`)
    /// so a full layout rebuild at the same width — e.g. the rebuild after a
    /// fold / group-expand — doesn't re-clone every entry's source text.
    fn estimate_content_lines(&self, content_width: u16) -> u16 {
        if let Some(lines) = self.entry.cached_estimate_lines(content_width) {
            return lines;
        }
        // Collapsed / Truncated foldable entries render a compact ~1-line header,
        // NOT their (often huge) hidden body. Use the ENTRY-level foldability
        // (`block.is_foldable()` OR attached hooks), matching the fold path, so a
        // hook-only-foldable collapsed entry isn't over-counted.
        let lines = if self.entry.display_mode != DisplayMode::Expanded && self.entry.is_foldable()
        {
            1
        } else {
            self.entry.estimate_source_lines(content_width)
        };
        self.entry.store_estimate_lines(content_width, lines);
        lines
    }

    /// Combine a content-line count with this entry's vpad + inline-media rows.
    /// Saturating throughout so a multi-MB block whose line count hits the u16
    /// ceiling can't overflow and corrupt `virtual_y` / `total_height`. Shared by
    /// `desired_height` and `estimate_height` so the assembly stays canonical.
    fn assemble_height(&self, content_width: u16, content_lines: u16) -> u16 {
        let vpad: u16 = if self.entry.block.has_vpad_for(self.appearance()) {
            2
        } else {
            0
        };
        content_lines
            .saturating_add(vpad)
            .saturating_add(self.inline_media_rows(content_width))
    }

    /// Rendered-row offset (from the entry top, including any top vpad row) of
    /// each logical (newline-delimited) line's start, plus the entry's final
    /// rendered content row, at viewport `width`.
    ///
    /// The SINGLE source of truth for the logical-line ↔ rendered-row mapping, so
    /// the forward ([`rendered_row_of_logical_line`]) and inverse
    /// ([`logical_line_of_rendered_row`]) helpers below derive from one predicate
    /// and can't drift. A logical-line start is a SELECTABLE hard-break row —
    /// matching the search index's `plain_text_from_output`; `Selectable::None`
    /// decoration rows (e.g. the Thinking header/blank) and soft-wrap
    /// continuations (`joiner.is_some()`) are not starts but still occupy rows
    /// (so the full-enumeration index is used). The returned starts are strictly
    /// ascending. Inline-media rows are not accounted for.
    ///
    /// Returns `(starts, last_content_row)`. `last_content_row` is the entry's
    /// final rendered row, used to bound the last logical line (which has no
    /// following start) and to clamp out-of-range lookups.
    pub(crate) fn logical_line_start_rows(&self, width: u16) -> (Vec<u16>, u16) {
        let content_width = width
            .saturating_sub(self.chrome_width())
            .saturating_sub(self.timestamp_reserved());
        // Compute vpad and populate the cache before borrowing the cached output:
        // `ensure_cached` takes the RefCell mutably on a miss, so the `Ref` from
        // `cached_output_ref` must come after it.
        let ctx = self
            .entry
            .context(content_width, self.appearance(), self.cwd);
        let vpad_top: u16 = if self.entry.block.has_vpad(&ctx) {
            1
        } else {
            0
        };
        self.entry
            .ensure_cached(content_width, self.appearance(), false, self.cwd);
        let output = self.entry.cached_output_ref();
        let starts = output
            .lines
            .iter()
            .enumerate()
            .filter(|(_, line)| {
                !matches!(line.selectable, Selectable::None) && line.joiner.is_none()
            })
            .map(|(idx, _)| vpad_top.saturating_add(u16::try_from(idx).unwrap_or(u16::MAX)))
            .collect();
        let last_content_row = vpad_top.saturating_add(
            u16::try_from(output.lines.len().saturating_sub(1)).unwrap_or(u16::MAX),
        );
        (starts, last_content_row)
    }

    /// Rendered-row offset from the entry's top (including any top vpad row) at
    /// which the search index's `logical_line`-th logical (newline-delimited)
    /// line begins, at viewport `width`.
    ///
    /// EXACT for blocks whose searchable text mirrors their selectable rendered
    /// lines (plain source blocks, markdown/thinking bodies); a best-effort
    /// estimate for field-joined source (Subagent/BgTask/CreditLimit), kept on
    /// screen by the caller's entry-height clamp. Past the last logical line,
    /// clamps to the final content row.
    pub fn rendered_row_of_logical_line(&self, width: u16, logical_line: usize) -> u16 {
        let (starts, last_content_row) = self.logical_line_start_rows(width);
        starts
            .get(logical_line)
            .copied()
            .unwrap_or(last_content_row)
    }

    /// Inverse of [`rendered_row_of_logical_line`]: the logical (newline-
    /// delimited) line index whose start lies at or before rendered-row offset
    /// `row` (from the entry top, including vpad), at viewport `width`.
    ///
    /// A display-row offset into a word-wrapped entry is not stable across a
    /// width change, but the logical line it sits on is — so scroll re-anchoring
    /// across a resize captures the logical line with this, then re-resolves its
    /// row at the new width via `rendered_row_of_logical_line`. Shares
    /// `logical_line_start_rows` with that method so the two provably round-trip.
    pub fn logical_line_of_rendered_row(&self, width: u16, row: u16) -> usize {
        let (starts, _) = self.logical_line_start_rows(width);
        // `starts` is ascending: the count of starts at or before `row`, minus 1,
        // is the 0-based index of the logical line containing `row`.
        starts
            .partition_point(|&start| start <= row)
            .saturating_sub(1)
    }
}

/// Diamond chrome prefix every group header draws before its text — verb-run
/// labels, truncation labels, and plain counts alike, in both fold states.
/// Selection geometry for labeled headers derives from this same string (see
/// [`group_header_chrome_prefix_width`]) so render and hitbox can't drift.
pub(crate) fn group_header_chrome_prefix() -> String {
    format!("{} ", crate::glyphs::diamond_dotted())
}

/// Display width of [`group_header_chrome_prefix`].
pub(crate) fn group_header_chrome_prefix_width() -> u16 {
    unicode_width::UnicodeWidthStr::width(group_header_chrome_prefix().as_str()) as u16
}

/// Fill a region with background-styled spaces so the frame owns these cells;
/// the cell diff won't repaint an unowned cell, so a stale glyph would persist.
fn fill_bg_spaces(buf: &mut Buffer, rect: Rect, bg: ratatui::style::Color) {
    let style = Style::default().bg(bg);
    for y in rect.y..rect.bottom() {
        for x in rect.x..rect.right() {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_char(' ');
                cell.set_style(style);
            }
        }
    }
}

impl Renderable for EntryRenderer<'_> {
    fn desired_height(&self, width: u16) -> u16 {
        if self.thinking_hidden() {
            return 0;
        }
        let content_width = width
            .saturating_sub(self.chrome_width())
            .saturating_sub(self.timestamp_reserved());
        // Use cached output for height calculation. The is_selected flag only
        // affects styling (e.g., UserPrompt prefix color), not line count, so
        // the non-selected cached output gives the correct height.
        self.entry
            .ensure_cached(content_width, self.appearance(), false, self.cwd);
        // Clamp the line count: a pathologically large block could exceed u16.
        let content_lines = self.entry.cached_output_ref().len().min(u16::MAX as usize) as u16;
        self.assemble_height(content_width, content_lines)
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        let chrome = self.chrome_width();
        if area.width < chrome + 1 || area.height == 0 || self.thinking_hidden() {
            return;
        }

        // Expand header ("╶╶ N more"): replaces block content entirely.
        // Only for truncated (non-expanded) groups.
        if self.group_header_count > 0 && !self.group_collapse_header {
            self.render_group_header(area, buf);
            return;
        }

        // Collapse header ("▾ N tool calls & thoughts"): standalone header
        // entry (height=1) that replaces the first group entry's content.
        // The remaining group entries keep their normal heights, giving the
        // header its own selectable index with independent interaction.
        let (area, skip_rows) = if self.group_collapse_header {
            if self.skip_rows == 0 {
                // Header is visible — render it in the first row
                let header_area = Rect::new(area.x, area.y, area.width, 1);
                self.render_group_header(header_area, buf);
                let remaining_height = area.height.saturating_sub(1);
                if remaining_height == 0 {
                    return;
                }
                let remaining = Rect::new(area.x, area.y + 1, area.width, remaining_height);
                (remaining, 0u16)
            } else {
                // Header is scrolled off — reduce skip_rows by 1 for content
                (area, self.skip_rows - 1)
            }
        } else {
            (area, self.skip_rows)
        };

        let layout_cfg = &self.appearance().scrollback.layout;
        // Minimal (`hide_accent`): reclaim the accent column so content is
        // flush-left. Fullscreen keeps the 1-col gutter even when a block has
        // no painted accent (so columns stay aligned across entry types).
        let accent_w = if self.hide_accent {
            0
        } else {
            HorizontalLayout::ACCENT
        };
        let [accent_area, left_pad, content_area, right_pad] = Layout::horizontal([
            Constraint::Length(accent_w),
            Constraint::Length(layout_cfg.block_pad_left),
            Constraint::Min(1),
            Constraint::Length(layout_cfg.block_pad_right),
        ])
        .areas(area);

        // Build context directly — avoid calling effective_output() here since
        // we only need the context for background/accent decisions, not the output.
        // This eliminates a full block.output() call (including syntax highlighting
        // for edit blocks) that was previously thrown away.
        let mut ctx = self
            .entry
            .context(content_area.width, self.appearance(), self.cwd);
        ctx.is_selected = self.is_selected;
        // Minimal mode blends committed/tail blocks with the real terminal
        // background; suppress the block's own band (keeps accents + per-line
        // code shading).
        let bg = if self.flat_background {
            BlockBackground::None
        } else {
            self.entry.block.background(&ctx)
        };

        // Fill padding with background if block has a background
        let bg_color = match bg {
            BlockBackground::None => None,
            BlockBackground::Light => Some(self.theme.bg_light),
            BlockBackground::Dark => Some(self.theme.bg_dark),
        };

        if let Some(bg_color) = bg_color {
            let bg_style = Style::default().bg(bg_color);

            // Fill the entire visible area (already accounts for skip_rows via area sizing)
            let full_start_y = area.y;
            let full_end_y = area.y + area.height;

            // Fill accent area if block wants accent background
            if self.entry.block.accent_background(&ctx) {
                for y in full_start_y..full_end_y {
                    for x in accent_area.x..accent_area.x + accent_area.width {
                        if let Some(cell) = buf.cell_mut((x, y)) {
                            cell.set_style(bg_style);
                        }
                    }
                }
            }

            // Fill left padding
            for y in full_start_y..full_end_y {
                for x in left_pad.x..left_pad.x + left_pad.width {
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_style(bg_style);
                    }
                }
            }

            // Fill content area
            for y in full_start_y..full_end_y {
                for x in content_area.x..content_area.x + content_area.width {
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_style(bg_style);
                    }
                }
            }

            // Fill right padding
            for y in full_start_y..full_end_y {
                for x in right_pad.x..right_pad.x + right_pad.width {
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_style(bg_style);
                    }
                }
            }
        }

        // Briefly flashed when a tool call or thinking block finishes. Read/Search/Edit carry no accent
        // of their own, so the flash falls back to green.
        let recently_finished = !self.entry.is_running
            && self.entry.finished_at.is_some_and(|t| {
                t.elapsed().as_millis() < crate::scrollback::state::FINISH_FLASH_DURATION_MS as u128
            })
            && matches!(
                self.entry.block,
                RenderBlock::ToolCall(_) | RenderBlock::Thinking(_)
            );

        // A collapsed row is a one-line summary, so a rail beside it is noise, flash included. Truncated
        // counts as open: it shows real content, and thinking blocks rest in that state.
        let accent = (!self.hide_accent && self.entry.display_mode != DisplayMode::Collapsed)
            .then(|| {
                if recently_finished {
                    let color = self
                        .entry
                        .block
                        .accent_color()
                        .unwrap_or(self.theme.accent_success);
                    Some(AccentStyle::static_color(color))
                } else {
                    self.accent(content_area.width)
                }
            })
            .flatten();

        // The column stays reserved either way, so nothing reflows. Clear it or a previous frame bleeds through.
        if accent.is_none() {
            fill_bg_spaces(buf, accent_area, bg_color.unwrap_or(self.fallback_bg()));
        }

        if let Some(accent_style) = accent {
            let color = accent_style.color;

            if self.entry.is_pending_user_input && accent_style.animated {
                // Pending user input freezes the wave: a solid rail reads as "paused on you" without the spinner motion.
                let style = self.accent_paint_style(color);
                for y in accent_area.y..accent_area.y + accent_area.height {
                    buf.set_string_safe(accent_area.x, y, crate::glyphs::accent_bar(), style);
                }
            } else if accent_style.animated {
                let bg = bg_color.unwrap_or(self.fallback_bg());
                let wave_rows = self.appearance().animation.wave_rows;

                for row in 0..accent_area.height {
                    let y = accent_area.y + row;
                    let brightness =
                        theme::wave_brightness(self.tick, skip_rows + row, wave_rows, WAVE_SPEED);
                    let animated_color = blend_color(bg, color, brightness).unwrap_or(color);
                    let style = self.accent_paint_style(animated_color);
                    buf.set_string_safe(accent_area.x, y, crate::glyphs::accent_bar(), style);
                }
            } else {
                let style = self.accent_paint_style(color);
                for y in accent_area.y..accent_area.y + accent_area.height {
                    buf.set_string_safe(accent_area.x, y, crate::glyphs::accent_bar(), style);
                }
            }
        }

        // Render content using cache (keyed on is_selected for blocks like
        // UserPrompt that adjust styling based on selection state).
        // When timestamps are enabled, wrap content at a narrower width so
        // text never collides with the right-aligned timestamp.
        let ts_reserved = self.timestamp_reserved();
        let text_width = content_area.width.saturating_sub(ts_reserved);
        self.entry
            .ensure_cached(text_width, self.appearance(), self.is_selected, self.cwd);
        let cached_ref = self.entry.cached_output_ref();
        let output: &BlockOutput = &cached_ref;
        let has_vpad = self.entry.block.has_vpad(&ctx);
        // Determine how many rows of vpad/content to skip.
        // Layout is: [vpad_top?] [content lines...] [vpad_bottom?]
        let vpad_top = if has_vpad { 1u16 } else { 0 };
        let skip_remaining = skip_rows;

        // Skip vpad_top (0 or 1 row)
        let vpad_top_visible = if skip_remaining < vpad_top {
            // Partial skip into vpad — vpad is still visible
            // (vpad is 0 or 1 row, so this means skip_rows == 0)
            true
        } else {
            false
        };
        let content_skip = skip_remaining.saturating_sub(vpad_top);

        let mut row = content_area.y;
        let max_row = content_area.y + content_area.height;

        // Top vpad (only if not skipped)
        if vpad_top_visible && row < max_row {
            row += 1;
        }

        // Own the timestamp gutter per row (no-bg blocks skip the full-area fill)
        // so a wide glyph can't strand a ghost; per-row bg keeps code blocks whole.
        let own_gutter = ts_reserved > 0 && bg_color.is_none() && content_area.width > ts_reserved;

        // Content lines — skip the first `content_skip` lines
        for line in output.lines.iter().skip(content_skip as usize) {
            if row >= max_row {
                break;
            }

            // Apply line-specific background if set. Decorative panel bands
            // (tool result previews) are dropped in flat mode so they blend
            // with the terminal's own background (minimal mode); semantic
            // shading (diff rows, code-block fill) always paints.
            let line_bg = if self.flat_background && line.background_is_panel {
                None
            } else {
                line.background
            };
            if let Some(line_bg) = line_bg {
                let bg_x = content_area.x + line.bg_start_col;
                let bg_width = content_area.width.saturating_sub(line.bg_start_col);
                if bg_width > 0 {
                    let line_rect = Rect::new(bg_x, row, bg_width, 1);
                    buf.set_style(line_rect, Style::default().bg(line_bg));
                }
            }

            buf.set_line_safe_bidi(content_area.x, row, &line.content, content_area.width);

            if own_gutter {
                let gutter = Rect::new(content_area.x + text_width, row, ts_reserved, 1);
                fill_bg_spaces(buf, gutter, line_bg.unwrap_or(self.fallback_bg()));
            }

            row += 1;
        }

        // Overlay timestamp on the first content line for message blocks.
        // Short format (h:mm AM/PM) by default; expands to full format
        // (HH:mm:ss | MMM DD) when the mouse hovers over the timestamp area.
        // Gated on appearance.show_timestamps (toggled via /timestamps).
        if self.appearance().show_timestamps
            && content_skip == 0
            && !output.is_empty()
            && self.should_show_timestamp()
            && let Some(ts) = self.entry.created_at
        {
            let first_content_y = content_area.y + if vpad_top_visible { 1 } else { 0 };
            // Check if mouse is hovering the timestamp zone (rightmost 10 cols
            // of the first content row).
            let ts_hovered = self.mouse_pos.is_some_and(|(mx, my)| {
                my == first_content_y
                    && mx >= content_area.x + content_area.width.saturating_sub(10)
                    && mx < content_area.x + content_area.width
            });
            let ts_str = if ts_hovered {
                ts.format("  %H:%M:%S | %b %d").to_string()
            } else {
                ts.format("  %-I:%M %p").to_string()
            };
            let ts_width = ts_str.len() as u16;
            if content_area.width > ts_width + 1 && first_content_y < max_row {
                let ts_x = content_area.x + content_area.width - ts_width;
                let ts_style = Style::default().fg(self.theme.gray);
                buf.set_string_safe(ts_x, first_content_y, &ts_str, ts_style);
            }
        }

        // Post-pass: adjust bullet color based on block state.
        //
        // Three cases (in priority order):
        // 1. Pending user input (permission / question) → keep the bullet
        //    glyph but force a static accent color, skipping the running
        //    wave so the entry reads as "paused on you", not "loading".
        // 2. Running block with animated bullet → wave animation on bullet char
        // 3. Collapsed groupable block with colored bullet → dim the bullet color
        //
        // The bullet is the first character on the first content row.
        if skip_rows == 0 && self.entry.block.has_bullet(&ctx) {
            let bullet_style = self.entry.block.bullet(&ctx);
            let bullet_y = content_area.y + if has_vpad { 1 } else { 0 };

            if bullet_y >= max_row {
                // bullet not visible — skip post-pass
            } else if self.entry.is_pending_user_input {
                // Pending user input: leave the bullet glyph alone, just
                // freeze its color at the block's bullet color (falling
                // back to `accent_user` for blocks that supply no bullet
                // style, e.g. Collapsed tool calls — they otherwise render
                // in default gray and lose the cue entirely). The fallback
                // intentionally matches the turn-status diamond and the
                // drain-blocked diamond so every "your turn" cue reads in
                // the same hue across the scrollback and status line.
                let color = bullet_style
                    .map(|s| s.color)
                    .unwrap_or(self.theme.accent_user);
                if let Some(cell) = buf.cell_mut((content_area.x, bullet_y)) {
                    cell.fg = color;
                }
            } else if let Some(style) = bullet_style {
                if style.animated {
                    // Animated bullet: wave effect synced with accent
                    let bg = bg_color.unwrap_or(self.fallback_bg());
                    let wave_rows = self.appearance().animation.wave_rows;
                    let brightness = theme::wave_brightness(self.tick, 0, wave_rows, WAVE_SPEED);
                    let animated_color =
                        blend_color(bg, style.color, brightness).unwrap_or(style.color);
                    if let Some(cell) = buf.cell_mut((content_area.x, bullet_y)) {
                        cell.fg = animated_color;
                    }
                } else if self.groupable
                    && self.entry.display_mode == DisplayMode::Collapsed
                    && !self.is_selected
                {
                    // Collapsed groupable with colored bullet: dim it.
                    // When selected, keep the bullet at its original full
                    // color so the selection reads as undimmed.
                    let bg = bg_color.unwrap_or(self.fallback_bg());
                    let dim = self.appearance().scrollback.display.dim_accent;
                    let dimmed = blend_color(bg, style.color, dim).unwrap_or(style.color);
                    if let Some(cell) = buf.cell_mut((content_area.x, bullet_y)) {
                        cell.fg = dimmed;
                    }
                }
                // Static + not collapsed+groupable: bullet already has correct color
                // from prepend_bullet(), no post-pass needed.
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scrollback::RenderBlock;
    use crate::theme::cache::pin_theme;
    use pretty_assertions::assert_eq;
    use ratatui::style::Color;

    #[test]
    fn test_entry_renderer_height() {
        let theme = Theme::current();
        let entry = ScrollbackEntry::new(RenderBlock::stub("Hello", Color::Blue));
        let renderer = EntryRenderer::new(&entry, &theme);

        // StubBlock: 1 line + 2 vpad = 3
        // Width 80 - chrome(4) = 76 for content
        assert_eq!(renderer.desired_height(80), 3);
    }

    #[test]
    fn rendered_row_of_logical_line_follows_word_wrap() {
        let _theme = pin_theme();
        let theme = Theme::current();
        // First logical line is long enough to wrap into several rows at a
        // narrow width; the second logical line then starts well past row 1.
        let text = format!("{}\nsecond", "word ".repeat(40));
        let entry = ScrollbackEntry::new(RenderBlock::user_prompt(text));
        let renderer = EntryRenderer::new(&entry, &theme);

        // Narrow entry-area width (chrome is subtracted internally) forces wrap.
        let row0 = renderer.rendered_row_of_logical_line(30, 0);
        let row1 = renderer.rendered_row_of_logical_line(30, 1);

        // Line 0 starts on the first content row (after any top vpad row).
        assert!(
            row0 <= 1,
            "first logical line starts at the top content row"
        );
        // Line 1 begins after every wrapped row of line 0, so it lands more than
        // one row below it — proof the mapping follows the word wrap.
        assert!(
            row1 > row0 + 1,
            "wrapped first line must push the second logical line past row {}",
            row0 + 1
        );
    }

    /// A collapsed row drops the rail and keeps the column, so content must not shift with it.
    #[test]
    fn a_collapsed_entry_drops_the_rail_but_keeps_the_column() {
        let theme = Theme::current();
        let entry = ScrollbackEntry::new(RenderBlock::stub("Test", Color::Blue))
            .with_display_mode(DisplayMode::Collapsed);

        let area = Rect::new(0, 0, 20, 3);
        let mut buf = Buffer::empty(area);
        EntryRenderer::new(&entry, &theme).render(area, &mut buf);

        assert_eq!(buf.cell((0, 1)).unwrap().symbol(), " ", "no rail");
        assert_eq!(
            buf.cell((3, 1)).unwrap().symbol(),
            "T",
            "content must not reflow"
        );
    }

    #[test]
    fn test_entry_renderer_layout() {
        let theme = Theme::current();
        let entry = ScrollbackEntry::new(RenderBlock::stub("Test", Color::Blue));
        let renderer = EntryRenderer::new(&entry, &theme);

        // Area: 20 chars wide, 3 rows
        // Layout: accent(1) + left_pad(2) + content(16) + right_pad(1) = 20
        let area = Rect::new(0, 0, 20, 3);
        let mut buf = Buffer::empty(area);
        renderer.render(area, &mut buf);

        // A stub is Expanded, so it keeps its rail. Collapsed rows are the ones that go bare.
        assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "┃");
        assert_eq!(buf.cell((0, 1)).unwrap().symbol(), "┃");
        assert_eq!(buf.cell((0, 2)).unwrap().symbol(), "┃");

        // Left padding at columns 1-2 (empty space)
        assert_eq!(buf.cell((1, 1)).unwrap().symbol(), " ");
        assert_eq!(buf.cell((2, 1)).unwrap().symbol(), " ");

        // Content starts at column 3
        // Row 0 = vpad (empty), row 1 = content "Test", row 2 = vpad
        assert_eq!(buf.cell((3, 1)).unwrap().symbol(), "T");
        assert_eq!(buf.cell((4, 1)).unwrap().symbol(), "e");
        assert_eq!(buf.cell((5, 1)).unwrap().symbol(), "s");
        assert_eq!(buf.cell((6, 1)).unwrap().symbol(), "t");
    }

    #[test]
    fn test_chrome_width_constant() {
        assert_eq!(EntryRenderer::CHROME_WIDTH, 4); // 1 + 2 + 1
    }

    #[test]
    fn pending_user_input_keeps_diamond_bullet_with_static_color() {
        // While a tool is blocked on a permission / question we keep the
        // default Diamond bullet but freeze its color — no character swap
        // and no wave brightness animation. The bullet must read the same
        // glyph at every tick so the eye sees "paused on you", not the
        // running-wave loading cue.
        use crate::scrollback::blocks::tool::{OtherToolCallBlock, ToolCallBlock};

        let theme = Theme::current();
        let mut entry = ScrollbackEntry::running(RenderBlock::ToolCall(ToolCallBlock::Other(
            OtherToolCallBlock::new("ask_user_question", "ask user"),
        )));
        entry.is_pending_user_input = true;

        let area = Rect::new(0, 0, 60, 3);
        let mut first_fg: Option<ratatui::style::Color> = None;
        for tick in [0u64, 13, 27, 41, 55] {
            let mut buf = Buffer::empty(area);
            let renderer = EntryRenderer::new(&entry, &theme).with_tick(tick);
            renderer.render(area, &mut buf);
            // Default layout: accent(1) + left_pad(2) + content starts at 3.
            // Tool call header has no vpad, so bullet sits on row 0.
            let cell = buf.cell((3, 0)).unwrap();
            assert_eq!(
                cell.symbol(),
                "◆",
                "pending tool must keep the Diamond bullet at tick {tick}"
            );
            match first_fg {
                None => first_fg = Some(cell.fg),
                Some(prev) => assert_eq!(
                    cell.fg, prev,
                    "pending bullet color must be static across ticks (tick {tick})"
                ),
            }
        }
    }

    #[test]
    fn non_pending_running_tool_keeps_default_bullet() {
        // Sanity check: when is_pending_user_input is false we leave the
        // normal Diamond bullet alone so the running wave animation runs
        // on top of it as before.
        use crate::scrollback::blocks::tool::{OtherToolCallBlock, ToolCallBlock};

        let theme = Theme::current();
        let entry = ScrollbackEntry::running(RenderBlock::ToolCall(ToolCallBlock::Other(
            OtherToolCallBlock::new("ask_user_question", "ask user"),
        )));
        // is_pending_user_input is false by default.

        let area = Rect::new(0, 0, 60, 3);
        let mut buf = Buffer::empty(area);
        let renderer = EntryRenderer::new(&entry, &theme).with_tick(7);
        renderer.render(area, &mut buf);

        assert_eq!(buf.cell((3, 0)).unwrap().symbol(), "◆");
    }

    /// Collect the symbols from a row range in the buffer into a String.
    fn collect_row_symbols(buf: &Buffer, y: u16, x_start: u16, x_end: u16) -> String {
        (x_start..x_end)
            .filter_map(|x| buf.cell((x, y)).map(|c| c.symbol().to_string()))
            .collect()
    }

    /// The reserved timestamp gutter (rightmost `ts_reserved` content columns)
    /// for `width`, derived from the renderer geometry so tests self-adjust to
    /// the default layout padding instead of hard-coding column numbers.
    fn gutter_band(renderer: &EntryRenderer, width: u16) -> std::ops::Range<u16> {
        let content_right = width - renderer.appearance().scrollback.layout.block_pad_right;
        (content_right - renderer.timestamp_reserved())..content_right
    }

    /// Check that a right-aligned timestamp ending with "AM" or "PM" exists on a row.
    /// Only scans the rightmost 16 columns to avoid false positives from content text.
    fn has_ampm_timestamp(buf: &Buffer, y: u16, x_end: u16) -> bool {
        let x_start = x_end.saturating_sub(16);
        let text = collect_row_symbols(buf, y, x_start, x_end);
        text.contains("AM") || text.contains("PM")
    }

    #[test]
    fn test_timestamp_short_format_for_user_prompt() {
        let theme = Theme::current();
        let entry = ScrollbackEntry::new(RenderBlock::user_prompt("hello"));
        // Not selected → short format "h:mm AM/PM"
        let renderer = EntryRenderer::new(&entry, &theme);

        let width: u16 = 80;
        let height = renderer.desired_height(width);
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        renderer.render(area, &mut buf);

        // UserPrompt has vpad=true, first content row is y=1.
        let expected = entry.created_at.unwrap().format("%-I:%M %p").to_string();
        let ts_width = expected.len() as u16;
        let ts_x = width - 2 - ts_width;
        let content_row = 1u16;

        let rendered = collect_row_symbols(&buf, content_row, ts_x, ts_x + ts_width);
        assert_eq!(
            rendered, expected,
            "Expected short timestamp '{expected}' at row {content_row}"
        );
    }

    #[test]
    fn test_timestamp_short_format_for_agent_message() {
        let theme = Theme::current();
        let entry = ScrollbackEntry::new(RenderBlock::agent_message("hello"));
        let renderer = EntryRenderer::new(&entry, &theme);

        let width: u16 = 80;
        let height = renderer.desired_height(width);
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        renderer.render(area, &mut buf);

        // AgentMessage has vpad=false, first content row is y=0.
        let expected = entry.created_at.unwrap().format("%-I:%M %p").to_string();
        let ts_width = expected.len() as u16;
        let ts_x = width - 2 - ts_width;

        let rendered = collect_row_symbols(&buf, 0, ts_x, ts_x + ts_width);
        assert_eq!(
            rendered, expected,
            "Expected short timestamp '{expected}' at row 0"
        );
    }

    #[test]
    fn test_timestamp_expands_on_mouse_hover() {
        let theme = Theme::current();
        let entry = ScrollbackEntry::new(RenderBlock::agent_message("hello"));
        let width: u16 = 80;

        // AgentMessage has no vpad → first content row at y=0.
        // Hover the rightmost 10 cols of that row to trigger expansion.
        let hover_x = width - 2 - 5; // inside the timestamp zone
        let renderer = EntryRenderer::new(&entry, &theme).with_mouse_pos(Some((hover_x, 0)));

        let height = renderer.desired_height(width);
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        renderer.render(area, &mut buf);

        // Compare against the entry's captured timestamp, not a fresh
        // Local::now() — re-sampling the clock here races the second boundary
        // (%H:%M:%S) and made this test flaky.
        let expected = entry
            .created_at
            .unwrap()
            .format("%H:%M:%S | %b %d")
            .to_string();
        let ts_width = expected.len() as u16;
        let ts_x = width - 2 - ts_width;

        let rendered = collect_row_symbols(&buf, 0, ts_x, ts_x + ts_width);
        assert_eq!(
            rendered, expected,
            "Mouse-hovered timestamp should show expanded format '{expected}'"
        );
    }

    #[test]
    fn test_timestamp_collapses_when_mouse_away() {
        let theme = Theme::current();
        let entry = ScrollbackEntry::new(RenderBlock::agent_message("hello"));
        let width: u16 = 80;

        // Render with mouse hovering timestamp → expanded
        let hover_x = width - 2 - 3;
        let renderer = EntryRenderer::new(&entry, &theme).with_mouse_pos(Some((hover_x, 0)));
        let height = renderer.desired_height(width);
        let area = Rect::new(0, 0, width, height);
        let mut buf_hover = Buffer::empty(area);
        renderer.render(area, &mut buf_hover);

        // Render with mouse far from timestamp → short
        let renderer = EntryRenderer::new(&entry, &theme).with_mouse_pos(Some((5, 0)));
        let mut buf_away = Buffer::empty(area);
        renderer.render(area, &mut buf_away);

        // Hovered should have pipe separator (expanded format)
        let row_hover = collect_row_symbols(&buf_hover, 0, 0, width);
        assert!(
            row_hover.contains('|'),
            "Hovered should show expanded format with '|'"
        );

        // Away should have AM/PM but NO pipe
        let row_away = collect_row_symbols(&buf_away, 0, 0, width);
        assert!(
            has_ampm_timestamp(&buf_away, 0, width),
            "Non-hovered should show short AM/PM timestamp"
        );
        assert!(
            !row_away.contains('|'),
            "Non-hovered should NOT show '|' separator"
        );
    }

    #[test]
    fn test_no_timestamp_for_thinking_block() {
        crate::appearance::cache::set_show_thinking_blocks(true);
        let theme = Theme::current();
        let entry = ScrollbackEntry::new(RenderBlock::thinking("deep thoughts"));
        let renderer = EntryRenderer::new(&entry, &theme);

        let width: u16 = 80;
        let height = renderer.desired_height(width);
        assert!(height > 0);
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        renderer.render(area, &mut buf);

        for y in 0..height {
            assert!(
                !has_ampm_timestamp(&buf, y, width),
                "Thinking block should not have a timestamp on row {y}"
            );
        }
    }

    #[test]
    fn test_no_timestamp_for_tool_call() {
        let theme = Theme::current();
        let entry = ScrollbackEntry::new(RenderBlock::tool_call("Read", "src/main.rs", true));
        let renderer = EntryRenderer::new(&entry, &theme);

        let width: u16 = 80;
        let height = renderer.desired_height(width);
        assert!(height > 0);
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        renderer.render(area, &mut buf);

        for y in 0..height {
            assert!(
                !has_ampm_timestamp(&buf, y, width),
                "Tool call block should not have a timestamp on row {y}"
            );
        }
    }

    #[test]
    fn test_should_show_timestamp_returns_correct_values() {
        use crate::scrollback::blocks::BtwBlock;

        let theme = Theme::current();
        let width: u16 = 80;

        // Blocks that SHOULD show timestamps
        let positive_blocks = vec![
            ("UserPrompt", RenderBlock::user_prompt("test"), 1u16),
            ("AgentMessage", RenderBlock::agent_message("test"), 0),
            ("Btw", RenderBlock::Btw(BtwBlock::new("q", "a")), 0),
        ];

        for (name, block, content_row) in positive_blocks {
            let entry = ScrollbackEntry::new(block);
            let renderer = EntryRenderer::new(&entry, &theme);
            let height = renderer.desired_height(width);
            let area = Rect::new(0, 0, width, height);
            let mut buf = Buffer::empty(area);
            renderer.render(area, &mut buf);

            assert!(
                has_ampm_timestamp(&buf, content_row, width),
                "{name} should show AM/PM timestamp on row {content_row}"
            );
        }

        // Blocks that should NOT show timestamps
        let negative_blocks = vec![
            ("Thinking", RenderBlock::thinking("think")),
            ("ToolCall", RenderBlock::tool_call("Read", "file.rs", true)),
            ("System", RenderBlock::system("sys msg")),
            ("Stub", RenderBlock::stub("stub", Color::Blue)),
        ];

        for (name, block) in negative_blocks {
            let entry = ScrollbackEntry::new(block);
            let renderer = EntryRenderer::new(&entry, &theme);
            let height = renderer.desired_height(width);
            let area = Rect::new(0, 0, width, height);
            let mut buf = Buffer::empty(area);
            renderer.render(area, &mut buf);

            let mut found = false;
            for y in 0..height {
                if has_ampm_timestamp(&buf, y, width) {
                    found = true;
                    break;
                }
            }
            assert!(!found, "{name} should NOT show timestamp");
        }
    }

    #[test]
    fn test_timestamp_not_rendered_when_narrow() {
        let theme = Theme::current();
        let entry = ScrollbackEntry::new(RenderBlock::user_prompt("hi"));
        let renderer = EntryRenderer::new(&entry, &theme);

        // Short timestamp "h:mm AM" is 7-8 chars; code requires content_width > ts_width + 1.
        // Width 12: chrome(5 actual) leaves content_width = 7, just barely not enough
        // for 8-char timestamp + 1. Suppress.
        let width: u16 = 12;
        let height = renderer.desired_height(width);
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        renderer.render(area, &mut buf);

        for y in 0..height {
            assert!(
                !has_ampm_timestamp(&buf, y, width),
                "Narrow width should suppress timestamp on row {y}"
            );
        }
    }

    // ── timestamp gutter ownership (ghost-glyph regression) ──

    #[test]
    fn gutter_cleared_on_non_first_content_row_without_background() {
        // Regression: nothing writes the timestamp gutter on rows past the
        // first, so a glyph stranded there (e.g. a wide table cell past
        // `text_width`) used to persist. Every content row must now clear it.
        let theme = Theme::current();
        // Long enough to wrap into several content rows at width 80.
        let entry = ScrollbackEntry::new(RenderBlock::agent_message("word ".repeat(80)));
        let renderer = EntryRenderer::new(&entry, &theme);

        let width: u16 = 80;
        let height = renderer.desired_height(width);
        assert!(height >= 3, "need multiple content rows, got {height}");
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);

        // A column inside the gutter band (past `text_width`), so content never
        // writes it; only the gutter clear can.
        let ghost_x = gutter_band(&renderer, width).start + 2;
        buf.set_string(ghost_x, 1, "X", Style::default());
        buf.set_string(ghost_x, 2, "X", Style::default());

        renderer.render(area, &mut buf);

        // Without the fix these cells stay "X" (nothing repaints them).
        assert_eq!(
            buf.cell((ghost_x, 1)).unwrap().symbol(),
            " ",
            "gutter ghost on row 1 must be cleared"
        );
        assert_eq!(
            buf.cell((ghost_x, 2)).unwrap().symbol(),
            " ",
            "gutter ghost on row 2 must be cleared"
        );
    }

    #[test]
    fn gutter_clear_preserves_first_row_timestamp() {
        // The gutter clear runs before the timestamp overlay, so it must not
        // wipe the first-row timestamp.
        let theme = Theme::current();
        let entry = ScrollbackEntry::new(RenderBlock::agent_message("hello"));
        let renderer = EntryRenderer::new(&entry, &theme);

        let width: u16 = 80;
        let height = renderer.desired_height(width);
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        // Seed a ghost in the gutter on the first content row too.
        let ghost_x = gutter_band(&renderer, width).start + 2;
        buf.set_string(ghost_x, 0, "X", Style::default());
        renderer.render(area, &mut buf);

        // AgentMessage has no vpad → first content row is y=0.
        let expected = entry.created_at.unwrap().format("%-I:%M %p").to_string();
        let ts_width = expected.len() as u16;
        let ts_x = width - 2 - ts_width;
        let rendered = collect_row_symbols(&buf, 0, ts_x, ts_x + ts_width);
        assert_eq!(
            rendered, expected,
            "timestamp must survive the gutter clear on the first row"
        );
    }

    #[test]
    fn background_block_gutter_uses_block_background_fill() {
        // Background blocks own the gutter via the existing full-area fill, so
        // the no-bg clear must not run for them. Concrete theme so bg_light !=
        // bg_base (Theme::current() quantizes both to Reset in the test env).
        let theme = Theme::echo_agent_night();
        assert_ne!(
            theme.bg_light, theme.bg_base,
            "test premise: block bg must differ from base bg"
        );
        let entry = ScrollbackEntry::new(RenderBlock::user_prompt("hello"));
        let renderer = EntryRenderer::new(&entry, &theme);

        let width: u16 = 80;
        let height = renderer.desired_height(width);
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        renderer.render(area, &mut buf);

        // Gutter cell carries the block background, proving the block fill
        // (not the bg_base clear) owns it. UserPrompt has vpad → content on row 1.
        let gutter_x = gutter_band(&renderer, width).start + 2;
        let gutter_cell = buf.cell((gutter_x, 1)).unwrap();
        assert_eq!(
            gutter_cell.bg, theme.bg_light,
            "background block gutter must use the block bg fill"
        );
    }

    #[test]
    fn gutter_keeps_code_block_background_on_no_background_block() {
        // A code block's per-line bg is painted across the full width (gutter
        // included); the per-row clear must reuse that bg, not bg_base, or the
        // code rectangle gets a notch. Concrete theme so the two bgs differ.
        //
        // Requires color support: under `NO_COLOR` the global markdown style
        // carries no code background while this test's local `echo_agent_night()`
        // theme still has RGB — a mismatch impossible in production.
        // (Historically this passed under NO_COLOR only because the md_style
        // Reset→silver fallback bug painted a concrete bg despite the opt-out.)
        if !crate::theme::color_support::detect().has_color() {
            return;
        }
        let theme = Theme::echo_agent_night();
        let mut entry = ScrollbackEntry::new(RenderBlock::agent_message("```\nZZZZ\n```\n"));
        // The code block's only content row is the first content row, which is
        // also where the timestamp overlay lands. Drop `created_at` so the
        // overlay is skipped (it would otherwise paint the right-aligned clock
        // into the gutter band and clobber the ghost-clear assertion at the
        // current wall-clock time). `timestamp_reserved()` ignores `created_at`,
        // so the per-row gutter-ownership path under test still runs.
        entry.created_at = None;
        let renderer = EntryRenderer::new(&entry, &theme);

        let width: u16 = 80;
        let height = renderer.desired_height(width);
        let area = Rect::new(0, 0, width, height);
        let content_left =
            HorizontalLayout::ACCENT + renderer.appearance().scrollback.layout.block_pad_left;
        let ghost_x = gutter_band(&renderer, width).start + 2;

        // Clean render: find the code row by its token, capture its background.
        let mut buf = Buffer::empty(area);
        renderer.render(area, &mut buf);
        let code_row = (0..height)
            .find(|&y| collect_row_symbols(&buf, y, content_left, width).contains("ZZZZ"))
            .expect("code block row with ZZZZ must render");
        let code_bg = buf.cell((content_left, code_row)).unwrap().bg;
        assert_ne!(
            code_bg, theme.bg_base,
            "test premise: code-block bg must differ from bg_base"
        );

        // (b) Gutter keeps the code-block background (full width) — on the
        //     unfixed `bg_base` fill this cell would be `theme.bg_base != code_bg`.
        assert_eq!(
            buf.cell((ghost_x, code_row)).unwrap().bg,
            code_bg,
            "code-row gutter must keep the code-block background, not bg_base"
        );

        // (a) A stranded glyph on the code row's gutter is still cleared.
        let mut buf2 = Buffer::empty(area);
        buf2.set_string(ghost_x, code_row, "X", Style::default());
        renderer.render(area, &mut buf2);
        assert_eq!(
            buf2.cell((ghost_x, code_row)).unwrap().symbol(),
            " ",
            "code-row gutter ghost must be cleared"
        );
    }

    // ── estimate_height (lazy off-screen sizing) ──

    #[test]
    fn estimate_matches_exact_for_plain_single_line() {
        let _theme = pin_theme();
        // For plain single-line content the cheap estimate must equal the exact
        // rendered height — this is why total_height stays correct for the many
        // simple entries that don't wrap or use markdown structure.
        let theme = Theme::current();
        let entry = ScrollbackEntry::new(RenderBlock::agent_message("hello world"));
        let r = EntryRenderer::new(&entry, &theme);
        assert_eq!(r.estimate_height(80), r.desired_height(80));
    }

    #[test]
    fn estimate_matches_exact_for_plain_multiline_stub() {
        let _theme = pin_theme();
        let theme = Theme::current();
        let entry = ScrollbackEntry::new(RenderBlock::stub("a\nb\nc", Color::Blue));
        let r = EntryRenderer::new(&entry, &theme);
        assert_eq!(r.estimate_height(80), r.desired_height(80));
    }

    #[test]
    fn estimate_collapsed_tool_call_matches_exact() {
        let _theme = pin_theme();
        // A collapsed tool call renders a one-line header; the estimate must NOT
        // count the (large) hidden body, and must equal the exact height so the
        // scroll math is correct for sessions full of collapsed calls.
        let theme = Theme::current();
        let mut entry = ScrollbackEntry::new(RenderBlock::tool_call_with_details(
            "Bash",
            "ls -la",
            true,
            "line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8",
        ));
        entry.set_display_mode(DisplayMode::Collapsed);
        let r = EntryRenderer::new(&entry, &theme);
        assert_eq!(
            r.estimate_height(80),
            r.desired_height(80),
            "collapsed tool-call estimate must equal exact (compact header)"
        );
    }

    #[test]
    fn estimate_collapsed_thinking_matches_exact() {
        let _theme = pin_theme();
        let theme = Theme::current();
        let mut entry = ScrollbackEntry::new(RenderBlock::thinking_with_time(
            "deep\nmulti\nline\nthought",
            1500,
        ));
        entry.set_display_mode(DisplayMode::Collapsed);
        let r = EntryRenderer::new(&entry, &theme);
        assert_eq!(
            r.estimate_height(80),
            r.desired_height(80),
            "collapsed thinking estimate must equal exact (one summary line)"
        );
    }

    #[test]
    fn estimate_trailing_newline_does_not_add_a_row() {
        let _theme = pin_theme();
        // `str::lines()` (and the renderers) drop a single trailing newline, so
        // the estimate must strip it too — otherwise trailing-`\n` source would
        // estimate one row too many and break estimate == exact.
        let theme = Theme::current();
        let with_nl = ScrollbackEntry::new(RenderBlock::stub("a\nb\nc\n", Color::Blue));
        let without_nl = ScrollbackEntry::new(RenderBlock::stub("a\nb\nc", Color::Blue));
        let h_with = EntryRenderer::new(&with_nl, &theme).estimate_height(80);
        let h_without = EntryRenderer::new(&without_nl, &theme).estimate_height(80);
        assert_eq!(h_with, h_without, "trailing newline must not add a row");
        assert_eq!(
            h_with,
            EntryRenderer::new(&with_nl, &theme).desired_height(80),
            "estimate with trailing newline still equals exact"
        );
    }

    #[test]
    fn estimate_uses_entry_level_foldability_for_collapsed_shortcut() {
        let _theme = pin_theme();
        // An AgentMessage block is NOT block-foldable, but attaching hooks makes
        // the ENTRY foldable (matching the fold path). A Collapsed foldable entry
        // takes the compact ~1-line shortcut; a non-foldable one estimates its body.
        use crate::scrollback::blocks::tool::hook::{
            HookRunEntry, HookRunStatus, ToolCallHookData,
        };
        let theme = Theme::current();
        // AgentMessage renders as markdown (single newlines collapse to spaces), so
        // force a multi-row body with length, not line count.
        let body = "word ".repeat(60);

        // No hooks → not foldable → a Collapsed entry estimates its body, not the
        // 1-line fold shortcut.
        let mut plain = ScrollbackEntry::new(RenderBlock::agent_message(body.as_str()));
        plain.set_display_mode(DisplayMode::Collapsed);
        let plain_est = EntryRenderer::new(&plain, &theme).estimate_height(80);
        assert!(
            plain_est > 1,
            "non-foldable collapsed entry estimates its body, not the shortcut (got {plain_est})"
        );

        // With hooks → entry-level foldable → compact shortcut (1 line, no vpad).
        let mut hooked = ScrollbackEntry::new(RenderBlock::agent_message(body.as_str()));
        hooked.set_display_mode(DisplayMode::Collapsed);
        hooked.hook_data = Some(ToolCallHookData {
            pre_hooks: vec![HookRunEntry {
                name: "fmt".into(),
                status: HookRunStatus::Success {
                    elapsed: std::time::Duration::from_millis(1),
                },
                output: None,
            }],
            ..Default::default()
        });
        let hooked_est = EntryRenderer::new(&hooked, &theme).estimate_height(80);
        assert_eq!(
            hooked_est, 1,
            "hook-foldable collapsed entry uses the compact shortcut"
        );
        assert!(
            plain_est > hooked_est,
            "entry-level foldability must change the collapsed estimate \
             (plain {plain_est} vs hooked {hooked_est})"
        );
    }

    #[test]
    fn estimate_differs_from_exact_for_wrapping_text() {
        let _theme = pin_theme();
        // The estimate is a cheap char-ceil that ignores word boundaries, so for
        // word-heavy content at a narrow width the exact word-wrapped height is
        // larger. This proves the estimate is genuinely an approximation (so the
        // viewport-exact measurement path is actually doing work).
        let theme = Theme::current();
        let appearance = AppearanceConfig {
            show_timestamps: false,
            ..Default::default()
        };
        let entry = ScrollbackEntry::new(RenderBlock::agent_message(
            "msg aaaaaaaaaa bbbbbbbbbb cccccccccc dddddddddd eeeeeeeeee ffffffffff",
        ));
        let r = EntryRenderer::new(&entry, &theme).with_appearance(appearance);
        let est = r.estimate_height(20);
        let exact = r.desired_height(20);
        assert!(
            est < exact,
            "word-wrap height (exact={exact}) should exceed char-ceil estimate (est={est})"
        );
    }

    #[test]
    fn estimate_wrapped_line_count_saturates_at_u16_max() {
        // > u16::MAX source lines must cap, not overflow the running total.
        let entry = ScrollbackEntry::new(RenderBlock::user_prompt("\n".repeat(70_000)));
        assert_eq!(entry.estimate_source_lines(80), u16::MAX);
    }

    #[test]
    fn estimate_height_saturates_instead_of_overflowing() {
        let _theme = pin_theme();
        // A pathologically tall block (multi-MB source) hits the line-count cap;
        // adding vpad on top must SATURATE, not overflow (debug panic / release
        // wrap → corrupt virtual_y). Stub has vpad, so this exercises the
        // saturating_add in `assemble_height`.
        let theme = Theme::current();
        let entry = ScrollbackEntry::new(RenderBlock::stub("\n".repeat(70_000), Color::Blue));
        let r = EntryRenderer::new(&entry, &theme);
        assert_eq!(r.estimate_height(80), u16::MAX);
    }

    #[test]
    fn estimate_uses_display_width_not_byte_length() {
        let _theme = pin_theme();
        // Wide (CJK) glyphs are 2 display columns but 3 UTF-8 bytes each. The
        // estimate must wrap on DISPLAY width, not byte length, so a 10-glyph
        // wide string (display 20, bytes 30) sizes like 20 ascii cols (bytes 20)
        // and NOT like 30 ascii cols (bytes 30).
        let theme = Theme::current();
        // Narrow width so 20 vs 30 display columns wrap to different counts.
        let height = |text: &str| {
            let entry = ScrollbackEntry::new(RenderBlock::stub(text, Color::Blue));
            EntryRenderer::new(&entry, &theme).estimate_height(14)
        };
        let wide = height("一二三四五六七八九十"); // display 20, bytes 30
        let ascii_same_display = height("xxxxxxxxxxxxxxxxxxxx"); // display 20, bytes 20
        let ascii_same_bytes = height("xxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"); // display 30, bytes 30
        assert_eq!(
            wide, ascii_same_display,
            "wraps by display width: wide(20 cols) == ascii(20 cols)"
        );
        assert_ne!(
            wide, ascii_same_bytes,
            "must NOT wrap by byte length: wide(30 bytes) != ascii(30 cols)"
        );
    }

    // ── diagram affordance-row reservation ──

    /// A diagram under `auto`/`on` reserves exactly one extra row — the
    /// affordance row — and the off-screen estimate accounts for it so a bulk
    /// load never under-reserves. Rendering is lazy, so the row is always present
    /// regardless of any on-click render.
    #[test]
    fn affordance_row_adds_one_row_per_diagram() {
        let _theme = pin_theme();
        let theme = Theme::current();
        let md = "intro line\n\n```mermaid\nA-->B\n```\n\nafterword line\n";

        // Baselines with the setting OFF (no treatment row). Both the exact and
        // the source-based estimate are captured in this window because
        // `estimate_extra_rows`/`output()` read the *current* global setting.
        crate::appearance::cache::set_render_mermaid(crate::appearance::RenderMermaid::Off);
        let off_entry = ScrollbackEntry::new(RenderBlock::agent_message(md));
        let h_off = EntryRenderer::new(&off_entry, &theme).desired_height(80);
        let est_off = EntryRenderer::new(&off_entry, &theme).estimate_height(80);

        // Engine on → exactly one extra (affordance) row over the plain block.
        crate::appearance::cache::set_render_mermaid(crate::appearance::RenderMermaid::On);
        let on_entry = ScrollbackEntry::new(RenderBlock::agent_message(md));
        let r = EntryRenderer::new(&on_entry, &theme);
        let h_on = r.desired_height(80);
        assert_eq!(h_on, h_off + 1, "the affordance row adds exactly one row");

        // The off-screen estimate accounts for the affordance row (one per
        // diagram) and so never under-reserves vs the realized height — the
        // invariant a bulk load (`echoagent -r`) relies on to avoid clipping.
        let est_on = r.estimate_height(80);
        assert_eq!(
            est_on,
            est_off + 1,
            "estimate adds one row per diagram when the engine is on",
        );
        assert!(
            est_on >= h_on,
            "estimate must not under-reserve: est {est_on} >= desired {h_on}",
        );
    }

    #[test]
    fn thinking_entry_height_zero_when_show_thinking_blocks_off() {
        let theme = Theme::current();
        let entry = ScrollbackEntry::new(RenderBlock::thinking("reason step by step"));
        crate::appearance::cache::set_show_thinking_blocks(true);
        let on = EntryRenderer::new(&entry, &theme);
        let h_on = on.desired_height(80);
        let est_on = on.estimate_height(80);
        let trunc_on = on.compute_truncated_height(80);
        assert!(h_on > 0, "thinking must occupy rows when shown");
        assert!(est_on > 0);
        assert!(trunc_on > 0);

        crate::appearance::cache::set_show_thinking_blocks(false);
        let off = EntryRenderer::new(&entry, &theme);
        assert_eq!(off.desired_height(80), 0);
        assert_eq!(off.estimate_height(80), 0);
        assert_eq!(off.compute_truncated_height(80), 0);

        // Restore process default (off).
        crate::appearance::cache::set_show_thinking_blocks(false);
    }

    // ── flat_background × per-line backgrounds (minimal mode) ──

    /// Whether any cell in the buffer carries `bg` as its background.
    fn buf_has_bg(buf: &Buffer, bg: Color) -> bool {
        let area = *buf.area();
        (area.y..area.bottom())
            .any(|y| (area.x..area.right()).any(|x| buf.cell((x, y)).unwrap().bg == bg))
    }

    fn render_to_buf(renderer: &EntryRenderer<'_>, width: u16) -> Buffer {
        let height = renderer.desired_height(width);
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        renderer.render(area, &mut buf);
        buf
    }

    /// Fixed injected line-bg color for the flat-background tests. A raw RGB
    /// (not a theme color): the stub injects it directly into its output, so
    /// the tests are independent of `Theme::current()` — whose process-global
    /// kind / color-level state other tests mutate in a parallel run.
    const LINE_BG: Color = Color::Rgb(12, 34, 56);

    #[test]
    fn flat_background_suppresses_panel_line_bg() {
        // Read/Search/etc. tool previews paint a decorative per-line panel
        // band (marked `background_is_panel`). In minimal mode
        // (flat_background) that fixed-color band clashes with the terminal's
        // own background, so it must be dropped — the block-level suppression
        // alone doesn't cover it because those blocks declare
        // `BlockBackground::None` and shade per line. (That Read/Search mark
        // their previews as panel is pinned by block-side tests.)
        use crate::scrollback::block::StubBlock;

        let theme = Theme::echo_agent_night();
        let entry = ScrollbackEntry::new(RenderBlock::Stub(
            StubBlock::new("alpha\nbravo", Color::Blue).with_line_bg(LINE_BG, true),
        ));

        // Control: the default (alt-screen) render paints the panel band.
        let renderer = EntryRenderer::new(&entry, &theme);
        assert!(
            buf_has_bg(&render_to_buf(&renderer, 40), LINE_BG),
            "test premise: non-flat render must paint the panel band"
        );

        // Flat (minimal mode): the panel must not paint anywhere.
        let flat = EntryRenderer::new(&entry, &theme).with_flat_background(true);
        assert!(
            !buf_has_bg(&render_to_buf(&flat, 40), LINE_BG),
            "flat render must suppress panel line backgrounds"
        );
    }

    #[test]
    fn flat_background_keeps_semantic_line_bg() {
        // Semantic per-line backgrounds — diff insert/delete rows and
        // markdown code-block fill (NOT marked `background_is_panel`) — must
        // survive flat mode: they carry meaning, unlike the decorative
        // tool-preview panels.
        use crate::scrollback::block::StubBlock;

        let theme = Theme::echo_agent_night();
        let entry = ScrollbackEntry::new(RenderBlock::Stub(
            StubBlock::new("alpha\nbravo", Color::Blue).with_line_bg(LINE_BG, false),
        ));

        let flat = EntryRenderer::new(&entry, &theme).with_flat_background(true);
        assert!(
            buf_has_bg(&render_to_buf(&flat, 40), LINE_BG),
            "flat render must keep semantic (non-panel) line backgrounds"
        );
    }
}
