//! ContextInfoBlock — typed `/context` display rendered in scrollback.
//!
//! Holds the raw [`ContextInfo`] snapshot + active model name and rebuilds
//! the styled output on every `output()` call. This is the same pattern as
//! [`super::SessionEventBlock`]: keep typed data, format at render time. The
//! payoff is theme-reactivity — `Theme::current()` is re-resolved on every
//! redraw, so switching themes after running `/context` updates the colors
//! immediately instead of leaving stale baked-in values.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::appearance::AppearanceConfig;
use crate::render::wrapping::word_wrap_lines;
use crate::scrollback::block::BlockContent;
use crate::scrollback::types::{AccentStyle, BlockContext, BlockLine, BlockOutput};
use crate::theme::{Theme, quantize};
use echo_agent_runtime::session::{ContextInfo, count_detail};

/// Block that renders a `/context` snapshot in scrollback.
///
/// Layout (all left-aligned to column 0):
///
/// ```text
/// Context
///
/// 36.7k / 1.0m tokens (3.67%)
/// echo-agent-4
///
/// ◆ ◆ ◆ ◆ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇
/// ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇
/// ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇
/// ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇
/// ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇ ◇
///
/// ◆ System prompt     1.2k tokens  (0.1%)   (gray)
/// ◆ Messages         29.9k tokens    (3%)
/// ◇ Free              963k tokens   (96%)
///
/// ◈ Tool definitions  5.6k tokens  (0.6%) · 12 tools
/// ◈ Skills            2.4k tokens  (0.2%) · 21 skills
/// ◈ MCP servers        320 tokens  (0.1%) ·  4 servers
///
/// Auto-compact at 85% · ~812k tokens remaining
///
/// Turns: 5 · Tool calls: 12 · Compactions: 0
/// ```
///
/// The bar is a categorical breakdown: each cell uses its category's glyph
/// and color. System (gray ◆), messages (primary ◆), and reasoning/overhead
/// (violet ◆) fill left-to-right in legend order, and the remainder renders
/// as muted ◇ outlines for free capacity. The ◈ informational rows never
/// enter the bar.
#[derive(Debug, Clone)]
pub struct ContextInfoBlock {
    /// The captured context-window snapshot.
    pub snapshot: ContextInfo,
    /// Active model name at the time of capture (display-only).
    pub model: String,
}

/// Shape of the categorical bar — how the 100 cells are laid out.
///
/// Two layouts ship today:
///
/// - `WIDE`: 5 rows × 20 cells = 100 cells, ~39 columns wide. The
///   default when the terminal has room.
/// - `NARROW`: 10 rows × 10 cells = 100 cells, ~19 columns wide. Used
///   when terminal width drops below [`BarLayout::NARROW_BREAKPOINT`]
///   so the bar still fits on narrow terminals (tmux split panes,
///   small terminal windows, embedded shells).
///
/// Both shapes hold the same 100 cells, so the visual breakdown
/// (which categories occupy which share of the bar) is identical —
/// only the aspect ratio changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BarLayout {
    /// Cells per row.
    row_len: usize,
    /// Number of rows.
    rows: usize,
}

impl BarLayout {
    /// 5 rows × 20 cells. Renders in ~39 columns (20 glyphs + 19
    /// separator spaces).
    const WIDE: Self = Self {
        row_len: 20,
        rows: 5,
    };

    /// 10 rows × 10 cells. Renders in ~19 columns (10 glyphs + 9
    /// separator spaces).
    const NARROW: Self = Self {
        row_len: 10,
        rows: 10,
    };

    /// Terminal width (in columns) at which the bar switches from
    /// [`Self::WIDE`] to [`Self::NARROW`]. The wide layout needs 39
    /// columns just for the bar; 50 leaves ~11 columns of margin and
    /// is also roughly where the legend rows
    /// (e.g. `◈ Tool definitions  5.6k tokens   (0.6%) · 12 tools`)
    /// start to word-wrap, so the breakpoint is consistent with the
    /// rest of the block's responsive behavior.
    const NARROW_BREAKPOINT: u16 = 50;

    /// Choose a layout that fits the available terminal width.
    fn for_width(width: u16) -> Self {
        if width < Self::NARROW_BREAKPOINT {
            Self::NARROW
        } else {
            Self::WIDE
        }
    }

    /// Total cells in the bar. Always 100 for both shipped layouts;
    /// kept as a method so future layouts with a different total cell
    /// count don't silently misalign with the legend percentages.
    const fn total(self) -> usize {
        self.row_len * self.rows
    }
}

/// One legend or informational row, before column formatting.
struct LegendRow {
    glyph: &'static str,
    color: Color,
    label: String,
    tokens: u64,
    detail: Option<String>,
}

/// Column widths for the legend and informational rows, measured from the
/// rows that actually render so token counts, percentages, and detail
/// counts stay aligned no matter the labels or magnitudes.
struct RowLayout {
    label_width: usize,
    tokens_width: usize,
    percent_width: usize,
    count_width: usize,
}

impl RowLayout {
    /// Measure column widths over every row that will render. Widths are
    /// in codepoints, not bytes.
    fn measure<'a>(rows: impl Iterator<Item = &'a LegendRow> + Clone, total: u64) -> Self {
        Self {
            label_width: rows
                .clone()
                .map(|r| r.label.chars().count())
                .max()
                .unwrap_or(0)
                + 1,
            tokens_width: rows
                .clone()
                .map(|r| fmt_tok(r.tokens).chars().count())
                .max()
                .unwrap_or(0),
            percent_width: rows
                .clone()
                .map(|r| Self::percent(r.tokens, total).chars().count())
                .max()
                .unwrap_or(0),
            count_width: rows
                .filter_map(|r| r.detail.as_deref())
                .filter_map(|d| d.split(' ').next().map(|n| n.chars().count()))
                .max()
                .unwrap_or(0),
        }
    }

    /// The parenthesized share of the window, e.g. `"(0.6%)"`.
    fn percent(tokens: u64, total: u64) -> String {
        format!("({})", percent_of_window(tokens, total))
    }

    /// The row's numeric cells: tokens and percent, each right-aligned.
    fn cells(&self, tokens: u64, total: u64) -> String {
        format!(
            "{:>tokens_width$} tokens   {:>percent_width$}",
            fmt_tok(tokens),
            Self::percent(tokens, total),
            tokens_width = self.tokens_width,
            percent_width = self.percent_width,
        )
    }

    /// The detail suffix with the leading count right-aligned so the nouns
    /// line up: `" · 25 tools"` over `" ·  1 server"`. Details follow the
    /// `TokenUsageCategory::detail` count-then-noun convention; text with
    /// no space renders unaligned.
    fn detail_suffix(&self, detail: &str) -> String {
        match detail.split_once(' ') {
            Some((count, rest)) => {
                format!(
                    " \u{00b7} {count:>count_width$} {rest}",
                    count_width = self.count_width
                )
            }
            None => format!(" \u{00b7} {detail}"),
        }
    }

    /// Render one row. WIDE: a single column-aligned line. NARROW: glyph
    /// and label on the first line, numeric data indented by one space on
    /// the second so it clusters under the label.
    fn render(
        &self,
        row: &LegendRow,
        bar: BarLayout,
        total: u64,
        label_style: Style,
        muted: Style,
    ) -> Vec<Line<'static>> {
        let glyph = Span::styled(format!("{} ", row.glyph), Style::default().fg(row.color));
        let suffix = row.detail.as_deref().map(|d| self.detail_suffix(d));
        if bar == BarLayout::NARROW {
            let first = Line::from(vec![glyph, Span::styled(row.label.clone(), label_style)]);
            let mut second = vec![
                Span::raw(" "),
                Span::styled(
                    format!(
                        "{} tokens   {}",
                        fmt_tok(row.tokens),
                        Self::percent(row.tokens, total)
                    ),
                    muted,
                ),
            ];
            if let Some(extra) = suffix {
                second.push(Span::styled(extra, muted));
            }
            vec![first, Line::from(second)]
        } else {
            let mut spans = vec![
                glyph,
                Span::styled(
                    format!(
                        "{:<label_width$}",
                        row.label,
                        label_width = self.label_width
                    ),
                    label_style,
                ),
                Span::raw(" "),
                Span::styled(self.cells(row.tokens, total), muted),
            ];
            if let Some(extra) = suffix {
                spans.push(Span::styled(extra, muted));
            }
            vec![Line::from(spans)]
        }
    }
}

impl ContextInfoBlock {
    /// Create a new context-info block.
    pub fn new(snapshot: ContextInfo, model: impl Into<String>) -> Self {
        Self {
            snapshot,
            model: model.into(),
        }
    }

    /// Build the styled lines for an arbitrary content width. Reused by the
    /// usage modal's "Context usage" tab so the modal and the minimal-mode
    /// scrollback block render the same breakdown.
    pub(crate) fn lines_for_width(&self, theme: &Theme, width: u16) -> Vec<Line<'static>> {
        self.build_lines(theme, BarLayout::for_width(width))
    }

    /// Build the styled lines using the supplied theme and bar layout.
    ///
    /// Called from `output()` on every redraw so theme switches take effect
    /// without re-running `/context`. The theme is passed in (rather than
    /// re-resolved here) so a single `Theme::current()` lookup in `output()`
    /// is shared with the `max_lines` truncation branch.
    ///
    /// `bar` controls the shape of the categorical bar — the wide layout
    /// (5×20) is the default; `output()` switches to the narrow layout
    /// (10×10) when terminal width drops below `BarLayout::NARROW_BREAKPOINT`
    /// so the bar still fits on column-constrained terminals.
    fn build_lines(&self, theme: &Theme, bar: BarLayout) -> Vec<Line<'static>> {
        let snapshot = &self.snapshot;
        let model = &self.model;

        let used = snapshot.used;
        let total = snapshot.total;
        let usage_pct = snapshot.usage_pct;
        let system_tokens = snapshot.system_prompt_tokens;
        let tool_tokens = snapshot.tool_definitions_tokens;
        let tool_count = snapshot.tool_definitions_count;
        let message_tokens = snapshot.message_tokens;
        let free_tokens = snapshot.free_tokens;
        let turn_count = snapshot.turn_count;
        let tool_call_count = snapshot.tool_call_count;
        let compaction_count = snapshot.compaction_count;
        let overhead_tokens = used.saturating_sub(system_tokens + message_tokens);

        let muted = theme.muted();
        let primary = Style::default()
            .fg(theme.text_primary)
            .add_modifier(Modifier::BOLD);

        // Per-category colors used in both the bar and the legend so the
        // two visualizations are scannable side-by-side. Messages get the
        // brightest treatment (primary) — they dominate the breakdown and
        // are the conversation the user is actually steering. System
        // prompt uses the same diamond glyph as messages but in muted
        // gray so it reads as a quiet base layer.
        let system_color = quantize(theme.gray_bright); // gray
        let tools_color = quantize(theme.accent_skill); // teal / skill accent
        let messages_color = quantize(theme.text_primary); // primary (white)
        let empty_color = quantize(theme.gray_dim); // free / outline
        let overhead_color = quantize(theme.accent_verify);

        // Categorical bar: 100 cells laid out as `bar.rows` rows of
        // `bar.row_len` cells with one space between cells. Each category
        // gets its own glyph + color so the bar reads as a stacked
        // breakdown at a glance.
        // Routed through `glyphs` so the diamonds degrade to CP437-safe
        // stand-ins (`◆`→`♦`, `◇`→`○`) on legacy Windows consoles that
        // can't render the U+25Cx diamonds.
        let system_glyph = crate::glyphs::diamond_filled(); // ◆ (gray)
        let tools_glyph = crate::glyphs::diamond_dotted(); // ◈
        let messages_glyph = crate::glyphs::diamond_filled(); // ◆ (primary)
        let free_glyph = crate::glyphs::diamond_hollow(); // ◇
        let overhead_glyph = crate::glyphs::diamond_filled(); // ◆ (violet)

        let total_cells = bar.total();
        let cells_for = |tokens: u64| -> usize {
            if total == 0 {
                0
            } else {
                ((tokens as f64 / total as f64) * total_cells as f64).round() as usize
            }
        };
        let used_cells = cells_for(used).min(total_cells);
        let system_cells = cells_for(system_tokens).min(used_cells);
        let messages_cells = cells_for(message_tokens).min(used_cells - system_cells);
        let overhead_cells = used_cells - system_cells - messages_cells;
        let free_cells = total_cells - used_cells;

        let mut cells: Vec<(&'static str, Color)> = Vec::with_capacity(total_cells);
        for _ in 0..system_cells {
            cells.push((system_glyph, system_color));
        }
        for _ in 0..messages_cells {
            cells.push((messages_glyph, messages_color));
        }
        for _ in 0..overhead_cells {
            cells.push((overhead_glyph, overhead_color));
        }
        for _ in 0..free_cells {
            cells.push((free_glyph, empty_color));
        }
        debug_assert_eq!(cells.len(), total_cells);

        let mut bar_lines: Vec<Line<'static>> = Vec::with_capacity(bar.rows);
        for row_idx in 0..bar.rows {
            let start = row_idx * bar.row_len;
            let end = (start + bar.row_len).min(cells.len());
            let mut spans = Vec::with_capacity(bar.row_len * 2);
            for (i, (glyph, color)) in cells[start..end].iter().enumerate() {
                if i > 0 {
                    spans.push(Span::raw(" "));
                }
                spans.push(Span::styled(
                    (*glyph).to_string(),
                    Style::default().fg(*color),
                ));
            }
            bar_lines.push(Line::from(spans));
        }

        // Legend rows fill the bar; informational rows sit below it
        // because their tokens are already counted in its categories:
        // tool definitions surface in Reasoning/overhead, and the usage
        // categories overlap Messages.
        let mut legend_rows = vec![
            LegendRow {
                glyph: system_glyph,
                color: system_color,
                label: "System prompt".to_string(),
                tokens: system_tokens,
                detail: None,
            },
            LegendRow {
                glyph: messages_glyph,
                color: messages_color,
                label: "Messages".to_string(),
                tokens: message_tokens,
                detail: None,
            },
        ];
        if overhead_tokens > 0 {
            legend_rows.push(LegendRow {
                glyph: overhead_glyph,
                color: overhead_color,
                label: "Reasoning/overhead".to_string(),
                tokens: overhead_tokens,
                detail: None,
            });
        }
        legend_rows.push(LegendRow {
            glyph: free_glyph,
            color: empty_color,
            label: "Free".to_string(),
            tokens: free_tokens,
            detail: None,
        });
        let info_rows: Vec<LegendRow> = std::iter::once(LegendRow {
            glyph: tools_glyph,
            color: tools_color,
            label: "Tool definitions".to_string(),
            tokens: tool_tokens,
            detail: Some(count_detail(tool_count, "tool")),
        })
        .chain(snapshot.usage_categories.iter().map(|c| LegendRow {
            glyph: tools_glyph,
            color: tools_color,
            label: c.label.clone(),
            tokens: c.tokens,
            detail: c.detail.clone(),
        }))
        .collect();
        let layout = RowLayout::measure(legend_rows.iter().chain(info_rows.iter()), total);
        let label_style = Style::default().fg(theme.text_secondary);

        let mut lines: Vec<Line<'static>> = vec![
            // Header: bold white "Context"
            Line::from(Span::styled("Context", primary)),
            // Blank row between header and the at-a-glance summary
            Line::from(""),
            // Sub-header: token totals + percent. Uses `text_secondary` for
            // a touch more contrast than `muted` so the at-a-glance numbers
            // stand apart from the breakdown/footer rows. Switches to "m"
            // with one decimal place once a value reaches a million so wide
            // context windows (e.g. 1m / 2m / 4m) read naturally. The
            // percentage is recomputed from `used / total` so we get two
            // decimal places of precision (the `usage_pct: u8` field on
            // `ContextInfo` is pre-rounded to an integer).
            Line::from(Span::styled(
                format!(
                    "{} / {} tokens ({:.2}%)",
                    fmt_tok_big(used),
                    fmt_tok_big(total),
                    precise_usage_percent(used, total),
                ),
                Style::default().fg(theme.text_secondary),
            )),
            // Model name (one step dimmer than the tokens line so it reads
            // as a supporting caption rather than the primary number).
            Line::from(Span::styled(
                model.to_string(),
                Style::default().fg(theme.gray_bright),
            )),
            // Blank space before the bar
            Line::from(""),
        ];
        lines.extend(bar_lines);
        lines.push(Line::from(""));
        for row in &legend_rows {
            lines.extend(layout.render(row, bar, total, label_style, muted));
        }
        lines.push(Line::from(""));
        for row in &info_rows {
            lines.extend(layout.render(row, bar, total, label_style, muted));
        }
        lines.push(Line::from(""));

        // Auto-compact estimate: tokens until we hit the auto-compact
        // threshold. Uses the *live* value from the session snapshot
        // (routed from echo-agent-runtime's model config resolution). This makes
        // the “Auto-compact at X%” line and the tip band match exactly what
        // remote settings / user TOML / env have configured for the
        // current model (e.g. 65 for echo-agent-build).
        //
        // `threshold_tokens` uses `div_ceil` rather than truncating integer
        // division so it matches the rounded `usage_pct` from `ContextInfo`
        // (which uses `round()`). Without `div_ceil`, tiny totals could
        // produce `remaining == 0` while `usage_pct < threshold_percent`,
        // showing `~0 tokens remaining` for a context window that isn't
        // actually at the threshold.
        if total > 0 {
            let threshold_percent = snapshot.auto_compact_threshold_percent;
            let threshold_tokens = total.saturating_mul(threshold_percent as u64).div_ceil(100);
            let remaining = threshold_tokens.saturating_sub(used);
            let (text, style) = if usage_pct >= threshold_percent {
                (
                    format!("Auto-compact triggers next turn (at {threshold_percent}%)"),
                    Style::default().fg(quantize(theme.warning)),
                )
            } else {
                // Use `fmt_tok_big` (same as the header) so the remaining
                // count rolls over to `m` for wide context windows — a 4m
                // window at 60% reads `~1.0m tokens remaining`, not
                // `~1000k tokens remaining`.
                (
                    format!(
                        "Auto-compact at {threshold_percent}% \u{00b7} ~{} tokens remaining",
                        fmt_tok_big(remaining)
                    ),
                    muted,
                )
            };
            lines.push(Line::from(Span::styled(text, style)));
            lines.push(Line::from(""));
        }

        // Footer stats
        lines.push(Line::from(Span::styled(
            format!(
                "Turns: {turn_count} \u{00b7} Tool calls: {tool_call_count} \u{00b7} Compactions: {compaction_count}"
            ),
            muted,
        )));

        // Approaching-auto-compact tip: only show in the gap between the
        // "getting close" mark (80%) and the actual auto-compact threshold.
        // Above the threshold the "Auto-compact triggers next turn" line
        // already surfaces in warning style, so a second warning-styled tip
        // suggesting a manual `/compact` would just stack visually and
        // contradict itself (auto-compact is about to fire on its own).
        if (80..snapshot.auto_compact_threshold_percent).contains(&usage_pct) {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Tip: run /compact to free up context space.".to_string(),
                Style::default().fg(quantize(theme.warning)),
            )));
        }

        lines
    }
}

/// Format a token count compactly (`123`, `1.2k`, `999k`).
///
/// The cutover from `{:.1}k` to plain `{}k` happens at 99_500 (not 100_000)
/// to avoid a precision discontinuity: `{:.1}k` rounds `99.999` (n=99_999)
/// up to `"100.0k"` (6 chars), and the next bucket then emits `"100k"` (4
/// chars). The mismatch makes the value visually identical but two
/// characters wider, which knocks the column-aligned `tw` width in
/// `build_lines` off by one. Switching to integer-rounded `Nk` at 99_500
/// keeps the output stable: 99_499 → `"99.5k"`, 99_500 → `"100k"`.
fn fmt_tok(n: u64) -> String {
    if n >= 99_500 {
        // Round half-up to the nearest 1k; equivalent to `(n + 500) / 1000`
        // for u64, which avoids the f64 rounding artifact described above.
        format!("{}k", (n + 500) / 1000)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        format!("{n}")
    }
}

/// Compute `used / total * 100` as f64 with safe handling of `total == 0`.
fn precise_usage_percent(used: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        (used as f64 / total as f64) * 100.0
    }
}

/// Like [`fmt_tok`] but rolls over to `1.0m` at one million.
///
/// Used for the at-a-glance totals line so a 1M / 2M / 4M context window
/// reads naturally as `1.0m` rather than `1000k`. Per-category legend rows
/// stay on [`fmt_tok`] so a fractional-million breakdown still shows the
/// finer-grained `k` resolution.
fn fmt_tok_big(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}m", n as f64 / 1_000_000.0)
    } else {
        fmt_tok(n)
    }
}

/// Format a category's share of the total context window as a percentage.
fn percent_of_window(part: u64, total: u64) -> String {
    if total == 0 {
        return "-".to_string();
    }
    // Tiny nonzero shares floor at 0.1% so the column stays clean.
    let p = ((part as f64 / total as f64) * 100.0).max(if part > 0 { 0.1 } else { 0.0 });
    if p < 10.0 {
        format!("{p:.1}%")
    } else {
        format!("{p:.0}%")
    }
}

impl BlockContent for ContextInfoBlock {
    fn output(&self, ctx: &BlockContext) -> BlockOutput {
        let theme = Theme::current();
        // Responsive bar shape: narrow terminals get a 10×10 bar so the
        // visualization doesn't get clipped or pushed under the legend.
        let bar = BarLayout::for_width(ctx.width);
        let styled_lines = self.build_lines(&theme, bar);
        let wrapped = word_wrap_lines(styled_lines, ctx.width as usize);
        let all_lines: Vec<BlockLine> = wrapped
            .into_iter()
            .map(|line| BlockLine::styled(line).with_selection_range(Some(0)))
            .collect();

        // Apply max_lines budget if set
        let lines = if let Some(max) = ctx.max_lines {
            let max = max as usize;
            if all_lines.len() > max && max > 0 {
                let take_count = if max > 1 { max - 1 } else { 1 };
                let mut truncated: Vec<BlockLine> =
                    all_lines.into_iter().take(take_count).collect();
                if let Some(last) = truncated.last_mut() {
                    let content_end = last.content.spans.len();
                    last.content
                        .spans
                        .push(Span::styled(" \u{2026}".to_string(), theme.muted()));
                    last.selectable = crate::scrollback::types::Selectable::Spans(0..content_end);
                }
                truncated
            } else {
                all_lines
            }
        } else {
            all_lines
        };

        if lines.is_empty() {
            BlockOutput {
                lines: vec![BlockLine::styled(Line::from("")).with_selection_range(Some(0))],
            }
        } else {
            BlockOutput { lines }
        }
    }

    fn accent(&self, _ctx: &BlockContext) -> Option<AccentStyle> {
        None
    }

    fn has_vpad_for(&self, _appearance: &AppearanceConfig) -> bool {
        false // Compact like SystemMessageBlock
    }

    fn has_raw_mode(&self) -> bool {
        false
    }

    fn is_foldable(&self) -> bool {
        false
    }

    fn is_selectable(&self) -> bool {
        false
    }

    fn is_groupable(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent_runtime::session::TokenUsageCategory;

    fn snapshot() -> ContextInfo {
        ContextInfo {
            used: 36_700,
            total: 1_000_000,
            system_prompt_tokens: 1_200,
            tool_definitions_count: 12,
            tool_definitions_tokens: 5_600,
            compaction_count: 0,
            turn_count: 5,
            tool_call_count: 12,
            message_count: 8,
            message_tokens: 29_900,
            free_tokens: 963_300,
            usage_pct: 4,
            auto_compact_threshold_percent: 85,
            usage_categories: vec![],
        }
    }

    /// Theme handle used by the unit tests. `Theme::current()` is the same
    /// resolver `output()` calls; the active theme doesn't matter for these
    /// tests since they assert on text content / span counts, not colors.
    fn test_theme() -> Theme {
        Theme::current()
    }

    /// Render a block and collapse a single line's spans into a flat string.
    fn line_text(lines: &[Line<'static>], idx: usize) -> String {
        lines[idx]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect()
    }

    /// Render a block and collapse all spans into a flat newline-joined
    /// string, useful for `contains` assertions.
    fn all_text(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()).chain(["\n"]))
            .collect()
    }

    #[test]
    fn build_lines_contains_header_tokens_and_model() {
        let block = ContextInfoBlock::new(snapshot(), "echo-agent-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::WIDE);
        // Layout: Context / <blank> / tokens / model.
        assert_eq!(line_text(&lines, 0), "Context");
        assert_eq!(line_text(&lines, 1), "");
        let l2 = line_text(&lines, 2);
        assert!(l2.contains("tokens"));
        // Percent now shows 2 decimal places (36.7k / 1m = 3.67%).
        assert!(l2.contains("(3.67%)"), "got: {l2:?}");
        assert_eq!(line_text(&lines, 3), "echo-agent-4");
    }

    #[test]
    fn build_lines_contains_tokens_summary() {
        let block = ContextInfoBlock::new(snapshot(), "echo-agent-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::WIDE);
        let l2 = line_text(&lines, 2);
        assert!(l2.contains("tokens"));
        assert!(l2.contains("(3.67%)"));
    }

    #[test]
    fn precise_usage_percent_handles_zero_total() {
        assert_eq!(precise_usage_percent(100, 0), 0.0);
    }

    #[test]
    fn precise_usage_percent_two_decimal_formatting() {
        // 36_700 / 1_000_000 = 3.67 (exact to 2 decimals)
        let s = format!("{:.2}", precise_usage_percent(36_700, 1_000_000));
        assert_eq!(s, "3.67");
    }

    #[test]
    fn build_lines_includes_compaction_tip_in_warning_band() {
        // 80..85 is the band where the tip appears: auto-compact is close
        // enough to mention but not so close that the "triggers next turn"
        // line is also showing.
        let mut snap = snapshot();
        snap.usage_pct = 80;
        let block = ContextInfoBlock::new(snap, "echo-agent-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::WIDE);
        let last = line_text(&lines, lines.len() - 1);
        assert!(last.contains("/compact"), "expected tip line, got {last:?}");
    }

    #[test]
    fn build_lines_omits_tip_below_threshold() {
        let block = ContextInfoBlock::new(snapshot(), "echo-agent-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::WIDE);
        assert!(!all_text(&lines).contains("/compact"));
    }

    #[test]
    fn build_lines_omits_tip_at_or_above_threshold() {
        // At/above the auto-compact threshold the "triggers next turn" line
        // already says everything the tip would; the tip is suppressed to
        // avoid stacking two warning-styled lines that contradict each
        // other (manual /compact vs. auto-compact about to fire).
        let mut snap = snapshot();
        snap.usage_pct = 85; // the historical default (and value in snapshot() helper)
        let block = ContextInfoBlock::new(snap, "echo-agent-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::WIDE);
        assert!(!all_text(&lines).contains("/compact"));
    }

    #[test]
    fn build_lines_shows_auto_compact_estimate_below_threshold() {
        let block = ContextInfoBlock::new(snapshot(), "echo-agent-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::WIDE);
        let all = all_text(&lines);
        assert!(
            all.contains("Auto-compact at 85%") && all.contains("tokens remaining"),
            "expected `Auto-compact at 85% · ~X tokens remaining` line, got:\n{all}"
        );
    }

    #[test]
    fn build_lines_auto_compact_eta_uses_millions_for_wide_windows() {
        // 4M window at 0% used: remaining = ceil(4_000_000 * 85 / 100) = 3_400_000.
        // Should render via fmt_tok_big as "3.4m", not "3400k".
        let mut snap = snapshot();
        snap.total = 4_000_000;
        snap.used = 0;
        snap.usage_pct = 0;
        let block = ContextInfoBlock::new(snap, "echo-agent-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::WIDE);
        let all = all_text(&lines);
        assert!(
            all.contains("~3.4m tokens remaining"),
            "expected ETA to use millions, got:\n{all}"
        );
    }

    #[test]
    fn build_lines_auto_compact_eta_arithmetic_at_known_snapshot() {
        // 1M window, 36_700 used: ceil(850_000) - 36_700 = 813_300 → "813k".
        let block = ContextInfoBlock::new(snapshot(), "echo-agent-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::WIDE);
        let all = all_text(&lines);
        assert!(
            all.contains("~813k tokens remaining"),
            "expected `~813k tokens remaining`, got:\n{all}"
        );
    }

    #[test]
    fn fmt_tok_big_switches_to_millions_at_one_million() {
        assert_eq!(fmt_tok_big(999_999), fmt_tok(999_999)); // delegates below 1m
        assert_eq!(fmt_tok_big(1_000_000), "1.0m");
        assert_eq!(fmt_tok_big(1_500_000), "1.5m");
        assert_eq!(fmt_tok_big(2_345_678), "2.3m");
        assert_eq!(fmt_tok_big(10_000_000), "10.0m");
    }

    #[test]
    fn fmt_tok_boundaries() {
        assert_eq!(fmt_tok(0), "0");
        assert_eq!(fmt_tok(999), "999");
        assert_eq!(fmt_tok(1_000), "1.0k");
        // 99_499 is the last value below the `{:.1}k` cutover; it formats
        // with one decimal. 99_500 crosses into the integer-rounded branch
        // and emits `100k` (4 chars) rather than `{:.1}k`'s round-up of
        // `99.5` to `100.0k` (6 chars). See `fmt_tok` doc comment.
        assert_eq!(fmt_tok(99_499), "99.5k");
        assert_eq!(fmt_tok(99_500), "100k");
        assert_eq!(fmt_tok(99_999), "100k");
        assert_eq!(fmt_tok(100_000), "100k");
        assert_eq!(fmt_tok(999_999), "1000k");
    }

    #[test]
    fn build_lines_summary_uses_millions_for_total() {
        let mut snap = snapshot();
        snap.total = 2_000_000;
        snap.used = 36_700;
        let block = ContextInfoBlock::new(snap, "echo-agent-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::WIDE);
        let l2 = line_text(&lines, 2);
        assert!(
            l2.contains("/ 2.0m tokens"),
            "expected `/ 2.0m tokens` in summary line, got {l2:?}"
        );
    }

    #[test]
    fn build_lines_shows_imminent_auto_compact_at_threshold() {
        let mut snap = snapshot();
        snap.usage_pct = 85;
        let block = ContextInfoBlock::new(snap, "echo-agent-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::WIDE);
        let all = all_text(&lines);
        assert!(
            all.contains("Auto-compact triggers next turn"),
            "expected `Auto-compact triggers next turn` line, got:\n{all}"
        );
    }

    // -------------------------------------------------------------------
    // Bar partition tests
    //
    // The bar lives at line indices 5..(5+layout.rows) (after header /
    // blank / tokens / model / blank). Each row is rendered as `glyph`
    // spans separated by raw-space spans. To count cells per category,
    // we walk the bar lines for the given layout and count spans whose
    // content matches each category's glyph.
    // -------------------------------------------------------------------

    const SYSTEM_GLYPH_TEST: &str = "\u{25C6}";
    const TOOLS_GLYPH_TEST: &str = "\u{25C8}";
    const MESSAGES_GLYPH_TEST: &str = "\u{25C6}"; // same as system; distinguished by color in real render
    const FREE_GLYPH_TEST: &str = "\u{25C7}";

    /// `layout` tells the function how many bar rows to slice (5 for
    /// WIDE, 10 for NARROW); without it the slice would be wrong for
    /// the narrow layout and the assertions would fail spuriously.
    fn count_bar_glyphs(
        lines: &[Line<'static>],
        layout: BarLayout,
    ) -> (usize, usize, usize, usize) {
        let mut diamonds = 0usize;
        let mut tools = 0usize;
        let mut free = 0usize;
        let bar_start = 5; // header / blank / tokens / model / blank
        let bar_end = bar_start + layout.rows;
        for line in &lines[bar_start..bar_end] {
            for span in &line.spans {
                let c = span.content.as_ref();
                if c == SYSTEM_GLYPH_TEST || c == MESSAGES_GLYPH_TEST {
                    // SYSTEM_GLYPH_TEST == MESSAGES_GLYPH_TEST; counted together.
                    diamonds += 1;
                } else if c == TOOLS_GLYPH_TEST {
                    tools += 1;
                } else if c == FREE_GLYPH_TEST {
                    free += 1;
                }
            }
        }
        (diamonds, tools, free, diamonds + tools + free)
    }

    #[test]
    fn bar_used_band_does_not_overshoot_when_estimates_exceed_used() {
        let mut snap = snapshot();
        snap.total = 100_000;
        snap.used = 10_000;
        snap.system_prompt_tokens = 8_000;
        snap.message_tokens = 5_000;
        snap.tool_definitions_tokens = 0;
        snap.free_tokens = 90_000;
        snap.usage_pct = 10;
        let block = ContextInfoBlock::new(snap, "echo-agent-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::WIDE);
        let (diamonds, tools, free, total) = count_bar_glyphs(&lines, BarLayout::WIDE);
        assert_eq!(total, 100);
        assert_eq!(tools, 0);
        assert_eq!(
            diamonds, 10,
            "used band must equal cells_for(used)=10 even when estimates (13k) exceed used (10k)"
        );
        assert_eq!(free, 90);
    }

    #[test]
    fn bar_total_cells_always_sum_to_one_hundred() {
        let block = ContextInfoBlock::new(snapshot(), "echo-agent-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::WIDE);
        let (_, _, _, total) = count_bar_glyphs(&lines, BarLayout::WIDE);
        assert_eq!(total, 100, "bar must always render exactly 100 cells");
    }

    #[test]
    fn bar_all_free_when_total_is_zero() {
        let mut snap = snapshot();
        snap.total = 0;
        snap.used = 0;
        snap.system_prompt_tokens = 0;
        snap.tool_definitions_tokens = 0;
        snap.message_tokens = 0;
        snap.free_tokens = 0;
        let block = ContextInfoBlock::new(snap, "echo-agent-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::WIDE);
        let (diamonds, tools, free, total) = count_bar_glyphs(&lines, BarLayout::WIDE);
        assert_eq!(total, 100);
        assert_eq!(diamonds, 0, "no diamonds when total=0");
        assert_eq!(tools, 0, "no tools when total=0");
        assert_eq!(free, 100, "all cells free when total=0");
    }

    #[test]
    fn bar_all_used_when_completely_full() {
        // Fill the bar entirely with messages so we can count by glyph
        // (system+messages share the diamond; tools and free are distinct).
        let mut snap = snapshot();
        snap.total = 1_000;
        snap.used = 1_000;
        snap.system_prompt_tokens = 0;
        snap.tool_definitions_tokens = 0;
        snap.message_tokens = 1_000;
        snap.free_tokens = 0;
        snap.usage_pct = 100;
        let block = ContextInfoBlock::new(snap, "echo-agent-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::WIDE);
        let (diamonds, tools, free, total) = count_bar_glyphs(&lines, BarLayout::WIDE);
        assert_eq!(total, 100);
        assert_eq!(diamonds, 100, "messages should fill the entire bar");
        assert_eq!(tools, 0);
        assert_eq!(free, 0);
    }

    #[test]
    fn bar_used_band_excludes_tools_and_never_overshoots() {
        let mut snap = snapshot();
        snap.total = 1_000;
        snap.used = 1_000;
        snap.system_prompt_tokens = 495;
        snap.message_tokens = 10;
        snap.tool_definitions_tokens = 800;
        snap.free_tokens = 0;
        snap.usage_pct = 100;
        let block = ContextInfoBlock::new(snap, "echo-agent-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::WIDE);
        let (diamonds, tools, free, total) = count_bar_glyphs(&lines, BarLayout::WIDE);
        assert_eq!(total, 100, "bar must always render exactly 100 cells");
        assert_eq!(tools, 0, "tool definitions must never enter the bar");
        assert_eq!(diamonds, 100, "used band fills the bar at 100% usage");
        assert_eq!(free, 0, "no free cells at 100% usage");
    }

    #[test]
    fn bar_and_legend_reconcile_used_with_overhead_excluding_tools() {
        let snap = ContextInfo {
            used: 100_000,
            total: 500_000,
            system_prompt_tokens: 5_000,
            tool_definitions_count: 190,
            tool_definitions_tokens: 75_000,
            compaction_count: 0,
            turn_count: 1,
            tool_call_count: 0,
            message_count: 4,
            message_tokens: 25_000,
            free_tokens: 400_000,
            usage_pct: 20,
            auto_compact_threshold_percent: 65,
            usage_categories: vec![],
        };
        let block = ContextInfoBlock::new(snap, "echo-agent-build");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::WIDE);

        let all = all_text(&lines);
        assert!(
            all.contains("Reasoning/overhead") && all.contains("70.0k"),
            "overhead row (70.0k) missing:\n{all}"
        );
        assert!(
            all.contains("Tool definitions") && all.contains("190 tools"),
            "tools row must be shown with its count:\n{all}"
        );

        let (diamonds, tools, free, total) = count_bar_glyphs(&lines, BarLayout::WIDE);
        assert_eq!(total, 100);
        assert_eq!(tools, 0, "tool definitions excluded from the bar");
        assert_eq!(diamonds, 20, "used band must equal used/total");
        assert_eq!(free, 80);
    }

    #[test]
    fn build_lines_renders_usage_categories_with_details() {
        let mut snap = snapshot();
        snap.usage_categories = vec![
            TokenUsageCategory::skills_listing(&"x".repeat(9_600), 21),
            TokenUsageCategory::mcp_servers(&"y".repeat(1_200), 4),
        ];
        let block = ContextInfoBlock::new(snap, "echo-agent-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::WIDE);
        let all = all_text(&lines);
        assert!(
            all.contains("Skills") && all.contains("21 skills"),
            "skills row missing:\n{all}"
        );
        assert!(
            all.contains("MCP servers") && all.contains("4 servers"),
            "mcp row missing:\n{all}"
        );
        assert!(all.contains("\u{00b7} 12 tools"), "tools count:\n{all}");
        let (_, tools, _, total) = count_bar_glyphs(&lines, BarLayout::WIDE);
        assert_eq!(total, 100);
        assert_eq!(tools, 0, "usage categories must never enter the bar");

        // Token, percent, and count columns line up across all rows;
        // single-digit counts are right-aligned ("·  4 servers").
        let is_row = |l: &&str| {
            (l.starts_with('\u{25C6}') || l.starts_with('\u{25C8}') || l.starts_with('\u{25C7}'))
                && l.contains(" tokens ")
        };
        let cols = |needle: &str| -> Vec<usize> {
            all.lines()
                .filter(is_row)
                .filter_map(|l| l.find(needle))
                .collect()
        };
        for needle in [" tokens ", ")"] {
            let positions = cols(needle);
            assert!(
                positions.windows(2).all(|w| w[0] == w[1]),
                "{needle:?} column misaligned: {positions:?}\n{all}"
            );
        }
        assert!(
            all.contains("\u{00b7}  4 servers"),
            "single-digit count must be right-aligned:\n{all}"
        );
    }

    #[test]
    fn percent_of_window_returns_dash_for_zero_total() {
        // Exercised the `total == 0` early return — pct can't be computed.
        assert_eq!(percent_of_window(100, 0), "-");
        assert_eq!(percent_of_window(0, 0), "-");
    }

    #[test]
    fn percent_of_window_formatting() {
        assert_eq!(percent_of_window(1, 1_000_000), "0.1%");
        assert_eq!(percent_of_window(0, 1_000_000), "0.0%");
        assert_eq!(percent_of_window(50_000, 1_000_000), "5.0%");
        assert_eq!(percent_of_window(500_000, 1_000_000), "50%");
    }

    // -------------------------------------------------------------------
    // Responsive bar layout tests
    //
    // The bar's shape (5×20 vs 10×10) is chosen by `BarLayout::for_width`
    // based on terminal width, so narrow terminals get a square bar that
    // still fits in their column budget.
    // -------------------------------------------------------------------

    #[test]
    fn bar_layout_wide_is_5_rows_of_20() {
        assert_eq!(BarLayout::WIDE.rows, 5);
        assert_eq!(BarLayout::WIDE.row_len, 20);
        assert_eq!(BarLayout::WIDE.total(), 100);
    }

    #[test]
    fn bar_layout_narrow_is_10_rows_of_10() {
        assert_eq!(BarLayout::NARROW.rows, 10);
        assert_eq!(BarLayout::NARROW.row_len, 10);
        assert_eq!(BarLayout::NARROW.total(), 100);
    }

    #[test]
    fn bar_layout_for_width_picks_wide_at_breakpoint_and_above() {
        // At the breakpoint and above, the wide layout is selected.
        assert_eq!(
            BarLayout::for_width(BarLayout::NARROW_BREAKPOINT),
            BarLayout::WIDE
        );
        assert_eq!(BarLayout::for_width(80), BarLayout::WIDE);
        assert_eq!(BarLayout::for_width(200), BarLayout::WIDE);
        assert_eq!(BarLayout::for_width(u16::MAX), BarLayout::WIDE);
    }

    #[test]
    fn bar_layout_for_width_picks_narrow_below_breakpoint() {
        // Below the breakpoint, the narrow layout is selected so the bar
        // fits without being clipped.
        assert_eq!(
            BarLayout::for_width(BarLayout::NARROW_BREAKPOINT - 1),
            BarLayout::NARROW
        );
        assert_eq!(BarLayout::for_width(40), BarLayout::NARROW);
        assert_eq!(BarLayout::for_width(20), BarLayout::NARROW);
        assert_eq!(BarLayout::for_width(0), BarLayout::NARROW);
    }

    #[test]
    fn narrow_bar_renders_10_rows() {
        let block = ContextInfoBlock::new(snapshot(), "echo-agent-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::NARROW);
        // The bar starts at index 5 (header / blank / tokens / model /
        // blank). Each of the next 10 rows must be a non-empty bar row.
        for (offset, line) in lines[5..15].iter().enumerate() {
            assert!(
                !line.spans.is_empty(),
                "narrow bar row {offset} must be non-empty"
            );
        }
        // The line right after the bar should be the spacer blank.
        let after_bar: String = lines[15].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(after_bar, "", "expected blank line after narrow bar");
    }

    #[test]
    fn narrow_bar_total_cells_still_100() {
        let block = ContextInfoBlock::new(snapshot(), "echo-agent-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::NARROW);
        let (_, _, _, total) = count_bar_glyphs(&lines, BarLayout::NARROW);
        assert_eq!(total, 100, "narrow bar must still render exactly 100 cells");
    }

    #[test]
    fn narrow_bar_each_row_has_at_most_10_cells() {
        // Sanity: no single bar row should exceed the narrow row_len.
        // We count cell glyphs (not separator spaces) per row.
        let block = ContextInfoBlock::new(snapshot(), "echo-agent-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::NARROW);
        for (offset, line) in lines[5..15].iter().enumerate() {
            let cell_count = line
                .spans
                .iter()
                .filter(|s| {
                    let c = s.content.as_ref();
                    c == SYSTEM_GLYPH_TEST
                        || c == TOOLS_GLYPH_TEST
                        || c == MESSAGES_GLYPH_TEST
                        || c == FREE_GLYPH_TEST
                })
                .count();
            assert!(
                cell_count <= BarLayout::NARROW.row_len,
                "row {offset} has {cell_count} cells, expected <= {}",
                BarLayout::NARROW.row_len
            );
        }
    }

    #[test]
    fn wide_bar_each_row_has_at_most_20_cells() {
        let block = ContextInfoBlock::new(snapshot(), "echo-agent-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::WIDE);
        for (offset, line) in lines[5..10].iter().enumerate() {
            let cell_count = line
                .spans
                .iter()
                .filter(|s| {
                    let c = s.content.as_ref();
                    c == SYSTEM_GLYPH_TEST
                        || c == TOOLS_GLYPH_TEST
                        || c == MESSAGES_GLYPH_TEST
                        || c == FREE_GLYPH_TEST
                })
                .count();
            assert!(
                cell_count <= BarLayout::WIDE.row_len,
                "row {offset} has {cell_count} cells, expected <= {}",
                BarLayout::WIDE.row_len
            );
        }
    }

    // -------------------------------------------------------------------
    // Legend label color + responsive wrapping tests
    // -------------------------------------------------------------------

    /// Helper: find the legend row (or row 1, for the narrow layout)
    /// whose label text starts with `label_prefix`. Returns the matching
    /// `Line` so the caller can assert on its spans.
    fn find_legend_line<'a>(
        lines: &'a [Line<'static>],
        label_prefix: &str,
    ) -> Option<&'a Line<'static>> {
        lines.iter().find(|line| {
            line.spans
                .iter()
                .any(|s| s.content.as_ref().trim_start().starts_with(label_prefix))
        })
    }

    #[test]
    fn legend_label_uses_secondary_color_wide() {
        let block = ContextInfoBlock::new(snapshot(), "echo-agent-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::WIDE);
        let row = find_legend_line(&lines, "System prompt").expect("legend row");
        // Span layout for WIDE: [glyph+space, label, " ", tokens, ...].
        // The label span content begins with "System prompt".
        let label_span = row
            .spans
            .iter()
            .find(|s| s.content.as_ref().starts_with("System prompt"))
            .expect("label span");
        assert_eq!(
            label_span.style.fg,
            Some(theme.text_secondary),
            "label should use text_secondary, got {:?}",
            label_span.style.fg
        );
    }

    #[test]
    fn legend_label_uses_secondary_color_narrow() {
        let block = ContextInfoBlock::new(snapshot(), "echo-agent-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::NARROW);
        let row = find_legend_line(&lines, "System prompt").expect("legend row 1");
        // Span layout for NARROW row 1: [glyph+space, label].
        let label_span = row
            .spans
            .iter()
            .find(|s| s.content.as_ref() == "System prompt")
            .expect("label span");
        assert_eq!(
            label_span.style.fg,
            Some(theme.text_secondary),
            "narrow label should use text_secondary, got {:?}",
            label_span.style.fg
        );
    }

    #[test]
    fn narrow_legend_wraps_to_two_lines_per_category() {
        let block = ContextInfoBlock::new(snapshot(), "echo-agent-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::NARROW);
        let categories = [
            ("System prompt", "1.2k"),
            ("Messages", "29.9k"),
            ("Reasoning/overhead", "5.6k"),
            ("Free", "963k"),
        ];
        let mut idx = 16;
        for (label, expected_tokens) in categories {
            let row1: String = lines[idx]
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect();
            let row2: String = lines[idx + 1]
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect();
            assert!(
                row1.contains(label),
                "expected row {idx} to contain `{label}`, got: {row1:?}"
            );
            assert!(
                row2.contains(expected_tokens),
                "expected row {} to contain `{expected_tokens}` for `{label}`, got: {row2:?}",
                idx + 1
            );
            idx += 2;
        }
    }

    #[test]
    fn narrow_legend_data_row_starts_with_one_space_indent() {
        let block = ContextInfoBlock::new(snapshot(), "echo-agent-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::NARROW);
        // The data row for the first legend entry sits at index 17
        // (16 = "System prompt" header row, 17 = its data row).
        let data_row = &lines[17];
        let first = data_row
            .spans
            .first()
            .expect("data row should have a leading indent span");
        assert_eq!(
            first.content.as_ref(),
            " ",
            "narrow data row must start with exactly one space, got {:?}",
            first.content.as_ref()
        );
    }

    #[test]
    fn wide_legend_remains_single_line_per_category() {
        let block = ContextInfoBlock::new(snapshot(), "echo-agent-4");
        let theme = test_theme();
        let lines = block.build_lines(&theme, BarLayout::WIDE);
        let row_text =
            |i: usize| -> String { lines[i].spans.iter().map(|s| s.content.as_ref()).collect() };
        let l11 = row_text(11);
        assert!(
            l11.contains("System prompt") && l11.contains("1.2k"),
            "wide legend should keep label + tokens on one line, got: {l11:?}"
        );
        let l12 = row_text(12);
        assert!(
            l12.contains("Messages") && l12.contains("29.9k"),
            "wide legend should keep label + tokens on one line, got: {l12:?}"
        );
        let l13 = row_text(13);
        assert!(
            l13.contains("Reasoning/overhead") && l13.contains("5.6k"),
            "wide legend should show reasoning/overhead on one line, got: {l13:?}"
        );
        let l14 = row_text(14);
        assert!(
            l14.contains("Free") && l14.contains("963k"),
            "wide legend should keep label + tokens on one line, got: {l14:?}"
        );
    }
}
