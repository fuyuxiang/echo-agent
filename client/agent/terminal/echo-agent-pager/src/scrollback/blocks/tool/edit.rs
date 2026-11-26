//! EditToolCallBlock - displays file edit diffs with syntax highlighting.
//!
//! # Progressive highlight
//!
//! First paint uses per-hunk syntect (fast). When the post-edit file is available
//! and under size/line caps, a background worker upgrades to full-file-scoped
//! styles so mid-file multi-line scopes (e.g. closing `"""`) paint correctly.
//!
//! # Caps ([`EDIT_HL_MAX_BYTES`] / [`EDIT_HL_MAX_LINES`])
//!
//! Full-file HL costs up to **O(file lines)** in syntect, not O(hunk lines) —
//! the walk stops at the last hunk line, but a hunk near EOF pays for the whole
//! file. Caps keep background work bounded so a multi-megabyte monorepo dump
//! never freezes the worker or balloons the style map:
//!
//! | Gate | Default | On exceed |
//! |------|---------|-----------|
//! | File bytes | 2 MiB | stay [`EditHighlightPhase::HunkOnly`] |
//! | Line count | 50_000 | stay hunk-only |
//!
//! Cost magnitudes: see `benches/edit_highlight` (hunk-only first paint is
//! cheap; full-file is once-per-upgrade; naïve prefix-per-hunk is not shipped).

use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use similar::ChangeTag;
use syntect::easy::HighlightLines;
use syntect::highlighting::Style as SyntectStyle;

use super::TOOL_HEADER_RANGE;
use crate::appearance::AppearanceConfig;
use crate::scrollback::block::BlockContent;
use crate::scrollback::types::{
    AccentStyle, BlockBackground, BlockContext, BlockLine, BlockOutput, DisplayMode,
    RenderedBlockOutput, Selectable, SelectionBoundaries, SelectionBoundary,
    SelectionBoundaryEntry,
};
use crate::syntax::{Syntect, get_syntect};
use crate::theme::{Theme, ThemeKind};
use echo_agent_pager_diff::{DiffHunk, diff_hunks_to_patch};

/// Skip full-file HL when the post-edit file exceeds this size (2 MiB).
///
/// Full-file syntect on multi-MB sources is poor background work vs staying
/// hunk-only; see `benches/edit_highlight`.
pub const EDIT_HL_MAX_BYTES: u64 = 2 * 1024 * 1024;
/// Skip full-file HL when the post-edit file has more lines than this (50k).
///
/// Larger files stay hunk-only so the worker cannot unbounded-walk a dump.
pub const EDIT_HL_MAX_LINES: usize = 50_000;

/// Content spans for one source line: FG styles only (height-neutral).
pub type EditLineStyles = Vec<(Style, String)>;

/// Progressive syntax-highlight state for an edit block.
///
/// `HunkOnly` / `Pending` use per-hunk syntect; `FileScoped` maps full-file FG
/// styles onto Equal/Insert hunk text (Deletes keep per-hunk syntect). Clone via [`Arc`].
#[derive(Debug, Clone, Default)]
pub enum EditHighlightPhase {
    #[default]
    HunkOnly,
    /// Full-file job in flight; paint is still hunk-only.
    Pending { job_id: u64 },
    /// Precomputed full-file styles keyed by 1-based new-file line.
    FileScoped {
        by_new_line: Arc<HashMap<usize, EditLineStyles>>,
        /// Theme the styles were baked under; paint falls back to hunk-only
        /// on mismatch so a runtime theme flip never mixes palettes.
        theme: ThemeKind,
    },
}

/// Configuration for diff rendering (tweakable for dev testing).
#[derive(Debug, Clone)]
pub struct DiffRenderConfig {
    /// If true, add 2-char indent before line numbers.
    pub indent: bool,
    /// If true, background extends from line start (including gutter).
    /// If false (default), background only covers the content area.
    pub gutter_bg: bool,
    /// If true, skip indent columns in background (keep them clean).
    /// If false, include indent in background.
    pub indent_bg: bool,
    /// Separator string between hunks.
    /// Options: "───" (line), "…" (ellipsis), "⋯" (midline ellipsis), "" (none).
    pub hunk_separator: String,
    /// Show two line-number columns (old + new) like GitHub's unified diff.
    /// When false (default), show a single column with the new-file line number.
    pub dual_line_numbers: bool,
}

impl Default for DiffRenderConfig {
    fn default() -> Self {
        Self {
            indent: true,
            gutter_bg: false,
            indent_bg: false,
            hunk_separator: "…".to_string(),
            dual_line_numbers: false,
        }
    }
}

/// Layout constants.
const INDENT: &str = "  ";
const GUTTER_GAP: &str = " ";
const CONTENT_GAP: &str = "  ";

/// A rendered diff line with optional background color.
pub struct DiffLineOutput {
    pub line: Line<'static>,
    pub background: Option<Color>,
    /// Column where background starts (for partial background).
    pub content_start_col: u16,
    /// Number of gutter spans at the start of this line (for selection exclusion).
    pub gutter_span_count: usize,
    /// Raw content text for this line (excluding gutter).
    pub content_text: String,
    /// Joiner for soft-wrap reconstruction (None = hard break, Some = continuation).
    pub joiner: Option<String>,
    /// True for hunk separator lines (not selectable, break range continuity).
    pub is_separator: bool,
}

/// Render a diff hunk to lines with syntax highlighting.
///
/// Returns lines with optional background colors for insert/delete.
pub fn render_diff_hunk_highlighted(
    hunk: &DiffHunk,
    path: &Path,
    theme: &Theme,
    width: u16,
    config: &DiffRenderConfig,
) -> Vec<DiffLineOutput> {
    render_diff_hunks_core(std::slice::from_ref(hunk), path, None, theme, width, config)
}

/// Render multiple hunks with separators, returning lines with backgrounds.
pub fn render_diff_hunks_highlighted(
    hunks: &[DiffHunk],
    path: &Path,
    theme: &Theme,
    width: u16,
    config: &DiffRenderConfig,
) -> Vec<DiffLineOutput> {
    render_diff_hunks_core(hunks, path, None, theme, width, config)
}

/// The single hunk walker behind both public fronts, so gutters, backgrounds,
/// separators, and wrap behavior are identical across highlight phases by
/// construction. Every line renders its per-hunk syntect spans (keeping the
/// highlighter state exactly as in the hunk-only phase); when `by_new_line`
/// is given, matching Equal/Insert lines swap in the full-file styles.
fn render_diff_hunks_core(
    hunks: &[DiffHunk],
    path: &Path,
    by_new_line: Option<&HashMap<usize, EditLineStyles>>,
    theme: &Theme,
    width: u16,
    config: &DiffRenderConfig,
) -> Vec<DiffLineOutput> {
    let syntect = get_syntect();
    let mut lines = Vec::new();

    for (i, hunk) in hunks.iter().enumerate() {
        if i > 0 && !lines.is_empty() && !config.hunk_separator.is_empty() {
            // Add separator between hunks (no background)
            let indent = if config.indent { INDENT } else { "" };
            let sep_text = match hunk_gap_lines(&hunks[i - 1], hunk) {
                Some(1) => format!("{} 1 unchanged line", config.hunk_separator),
                Some(n) => format!("{} {n} unchanged lines", config.hunk_separator),
                None => config.hunk_separator.clone(),
            };
            let sep_line = Line::from(vec![
                Span::raw(indent),
                Span::styled(sep_text, theme.muted()),
            ]);
            lines.push(DiffLineOutput {
                line: sep_line,
                background: None,
                content_start_col: 0,
                gutter_span_count: 0,
                content_text: String::new(),
                joiner: None,
                is_separator: true,
            });
        }
        if hunk.is_empty() {
            continue;
        }
        let layout = gutter_layout(hunk, config);
        let indent_width = if config.indent { INDENT.len() } else { 0 };
        let content_width = (width as usize).saturating_sub(layout.total);
        // A diff interleaves two file versions; give each side its own highlighter
        // so a multi-line construct can't leak across sides. Equal lines render on
        // the new side and advance both.
        let mut old_highlighter = syntect.highlight_lines_by_file_path(path);
        let mut new_highlighter = syntect.highlight_lines_by_file_path(path);
        for line in hunk {
            let trimmed = line.text.trim_end_matches(['\r', '\n']);
            let text = echo_agent_pager_render::appearance::expand_tabs(trimmed);
            // Cold spans render unconditionally so Delete lines and any map
            // miss (text drift) paint exactly like the hunk-only phase.
            let mut content_spans = match line.tag {
                ChangeTag::Delete => {
                    render_content_spans(&text, line.tag, theme, &mut old_highlighter, syntect)
                }
                ChangeTag::Insert => {
                    render_content_spans(&text, line.tag, theme, &mut new_highlighter, syntect)
                }
                ChangeTag::Equal => {
                    let spans =
                        render_content_spans(&text, line.tag, theme, &mut new_highlighter, syntect);
                    advance_highlighter(&mut old_highlighter, &text, syntect);
                    spans
                }
            };
            if let Some(map) = by_new_line
                && let Some(spans) = map_spans_for_line(line, &text, map, theme)
            {
                content_spans = spans;
            }
            lines.extend(assemble_diff_line_outputs(
                line,
                content_spans,
                &layout,
                indent_width,
                content_width,
                theme,
                config,
            ));
        }
    }

    lines
}

/// Unchanged new-file lines hidden between two hunks, when computable.
///
/// Uses the `ln` of the new-file lines (Equal/Insert) bordering the gap.
/// `None` — a hunk with no new-file lines, or a non-positive gap — keeps the
/// bare separator. Non-monotonic `ln` happens on coalesced multi-call blocks
/// whose later edit landed above an earlier one (each call's hunks are
/// numbered against its own file snapshot), so a count would be wrong there.
fn hunk_gap_lines(prev: &DiffHunk, next: &DiffHunk) -> Option<usize> {
    let prev_last = prev.iter().rev().find(|l| l.tag != ChangeTag::Delete)?.ln;
    let next_first = next.iter().find(|l| l.tag != ChangeTag::Delete)?.ln;
    next_first
        .checked_sub(prev_last)
        .and_then(|d| d.checked_sub(1))
        .filter(|n| *n > 0)
}

/// Expanded (tabs → spaces) text for Equal/Insert hunk lines, keyed by 1-based ln.
fn hunk_new_line_texts(hunks: &[DiffHunk]) -> HashMap<usize, String> {
    let mut out = HashMap::new();
    for hunk in hunks {
        for line in hunk {
            if matches!(line.tag, ChangeTag::Equal | ChangeTag::Insert) && line.ln > 0 {
                let trimmed = line.text.trim_end_matches(['\r', '\n']);
                out.insert(
                    line.ln,
                    echo_agent_pager_render::appearance::expand_tabs(trimmed).into_owned(),
                );
            }
        }
    }
    out
}

/// Whether `file_text` is within the full-file HL caps (bytes + lines).
pub fn file_text_within_hl_caps(file_text: &str) -> bool {
    if file_text.len() as u64 > EDIT_HL_MAX_BYTES {
        return false;
    }
    let lines = if file_text.is_empty() {
        0
    } else {
        file_text.bytes().filter(|&b| b == b'\n').count() + usize::from(!file_text.ends_with('\n'))
    };
    lines <= EDIT_HL_MAX_LINES
}

/// Full-file HL once; keep new-side lines referenced by `hunks`.
///
/// Production upgrade path (edit-HL worker): one syntect walk over `file_text`
/// up to the last hunk line, retaining only Equal/Insert lines present in
/// `hunks`. Expands tabs before HL
/// (same as cold paint). Returns `None` if any needed disk line differs from
/// hunk text (so upgrade never rewrites displayed content). That refusal is also
/// the expected outcome for multi-edit blocks whose earlier hunks' `ln` were
/// shifted by a later edit above them, not a missed upgrade.
/// Caller enforces caps ([`file_text_within_hl_caps`]) and UTF-8.
pub fn compute_file_scoped_styles(
    path: &Path,
    file_text: &str,
    hunks: &[DiffHunk],
) -> Option<HashMap<usize, EditLineStyles>> {
    let expected = hunk_new_line_texts(hunks);
    if expected.is_empty() {
        return Some(HashMap::new());
    }

    let syntect = get_syntect();
    let mut highlighter = syntect.highlight_lines_by_file_path(path)?;

    // Lines past the last needed one cannot affect earlier syntect state.
    let max_needed = expected.keys().copied().max().unwrap_or(0);
    let mut out = HashMap::with_capacity(expected.len());
    for (idx, line) in file_text.lines().enumerate() {
        let ln = idx + 1;
        if ln > max_needed {
            break;
        }
        let expanded = echo_agent_pager_render::appearance::expand_tabs(line);
        let owned = format!("{expanded}\n");
        let ranges = highlighter
            .highlight_line(&owned, &syntect.syntax_set)
            .unwrap_or_default();
        let Some(expected_text) = expected.get(&ln) else {
            continue;
        };
        if expanded.as_ref() != expected_text.as_str() {
            return None;
        }
        let mut spans = Vec::new();
        for (style, segment) in ranges {
            let mut text = segment.to_owned();
            while text.ends_with('\n') || text.ends_with('\r') {
                text.pop();
            }
            if text.is_empty() {
                continue;
            }
            spans.push((syntect_to_ratatui_fg(style), text));
        }
        if spans.is_empty() {
            spans.push((Style::default(), String::new()));
        }
        out.insert(ln, spans);
    }
    if out.len() != expected.len() {
        return None;
    }
    Some(out)
}

/// Render hunks with precomputed full-file styles (FileScoped). A thin front
/// over [`render_diff_hunks_core`], so gutters/BG/wrap — and the Delete lines'
/// per-hunk syntect paint — match [`render_diff_hunks_highlighted`] by
/// construction; the map only overrides Equal/Insert foregrounds.
pub fn render_diff_hunks_with_styles(
    hunks: &[DiffHunk],
    path: &Path,
    by_new_line: &HashMap<usize, EditLineStyles>,
    theme: &Theme,
    width: u16,
    config: &DiffRenderConfig,
) -> Vec<DiffLineOutput> {
    render_diff_hunks_core(hunks, path, Some(by_new_line), theme, width, config)
}

/// Full-file map spans for one line, when they may override the cold spans:
/// Equal always; Insert only on banded themes (bandless paints changed lines
/// with a solid line FG); Delete never (it keeps the per-hunk syntect paint the
/// user already saw). `None` — missing line, text drift, or nothing visible —
/// keeps the cold spans.
fn map_spans_for_line(
    line: &echo_agent_pager_diff::DiffLine,
    expanded: &str,
    by_new_line: &HashMap<usize, EditLineStyles>,
    theme: &Theme,
) -> Option<Vec<Span<'static>>> {
    let overridable = match line.tag {
        ChangeTag::Equal => true,
        ChangeTag::Insert => !theme.diff_uses_line_fg(),
        ChangeTag::Delete => false,
    };
    if !overridable {
        return None;
    }
    let styles = by_new_line.get(&line.ln)?;
    let joined: String = styles.iter().map(|(_, t)| t.as_str()).collect();
    if joined != expanded {
        return None;
    }
    let spans: Vec<Span<'static>> = styles
        .iter()
        .filter(|(_, t)| !t.is_empty())
        .map(|(st, t)| Span::styled(t.clone(), *st))
        .collect();
    if spans.is_empty() {
        return None;
    }
    Some(spans)
}

/// Assemble gutter + content spans into one or more wrapped [`DiffLineOutput`]s.
fn assemble_diff_line_outputs(
    line: &echo_agent_pager_diff::DiffLine,
    content_spans: Vec<Span<'static>>,
    layout: &GutterLayout,
    indent_width: usize,
    content_width: usize,
    theme: &Theme,
    config: &DiffRenderConfig,
) -> Vec<DiffLineOutput> {
    let bg = match line.tag {
        ChangeTag::Equal => None,
        ChangeTag::Delete => Some(theme.diff_delete_bg),
        ChangeTag::Insert => Some(theme.diff_insert_bg),
    };

    let content_char_count: usize = content_spans
        .iter()
        .map(|s| s.content.chars().count())
        .sum();
    let raw_content: String = content_spans.iter().map(|s| s.content.as_ref()).collect();

    if content_width == 0 || content_char_count <= content_width {
        let mut spans = Vec::new();
        render_gutter(&mut spans, line, layout, theme, config);
        let gutter_count = spans.len();
        spans.extend(content_spans);
        let bg_start = compute_bg_start(config, layout.total, indent_width);
        return vec![DiffLineOutput {
            line: Line::from(spans),
            background: bg,
            content_start_col: bg_start,
            gutter_span_count: gutter_count,
            content_text: raw_content,
            joiner: None,
            is_separator: false,
        }];
    }

    // Wrap: project the precomputed styles onto the wrap segments; fall back
    // to a solid FG per segment when the projection cannot be aligned.
    let mut outputs = Vec::new();
    let gutter_padding = " ".repeat(layout.total);
    let wrapped_lines = wrap_text(&raw_content, content_width);
    let bg_start = compute_bg_start(config, layout.total, indent_width);
    let styled_rows = match project_styles_onto_wrap_segments(&content_spans, &wrapped_lines) {
        Some(rows) if rows.len() == wrapped_lines.len() => rows,
        _ => {
            let style = match line.tag {
                ChangeTag::Equal => Style::default().fg(theme.diff_equal_fg),
                ChangeTag::Delete | ChangeTag::Insert => {
                    if theme.diff_uses_line_fg() {
                        let fg = if line.tag == ChangeTag::Delete {
                            theme.diff_delete_fg
                        } else {
                            theme.diff_insert_fg
                        };
                        Style::default().fg(fg)
                    } else {
                        Style::default().fg(theme.text_primary)
                    }
                }
            };
            wrapped_lines
                .iter()
                .map(|wrapped| vec![Span::styled(wrapped.clone(), style)])
                .collect()
        }
    };
    for (i, (wrapped, row)) in wrapped_lines.iter().zip(styled_rows).enumerate() {
        let mut spans = Vec::new();
        let gutter_count = if i == 0 {
            render_gutter(&mut spans, line, layout, theme, config);
            spans.len()
        } else {
            spans.push(Span::raw(gutter_padding.clone()));
            1
        };
        spans.extend(row);
        outputs.push(DiffLineOutput {
            line: Line::from(spans),
            background: bg,
            content_start_col: bg_start,
            gutter_span_count: gutter_count,
            content_text: wrapped.clone(),
            joiner: if i == 0 { None } else { Some(String::new()) },
            is_separator: false,
        });
    }
    outputs
}

/// Compute where background starts based on config.
fn compute_bg_start(config: &DiffRenderConfig, gutter_width: usize, indent_width: usize) -> u16 {
    if config.gutter_bg {
        // Gutter bg is on - check if we should skip indent
        // indent_bg = true means skip indent (keep it clean)
        // indent_bg = false means include indent in background
        if config.indent && config.indent_bg {
            indent_width as u16
        } else {
            0
        }
    } else {
        // Gutter bg is off - color from content only
        gutter_width as u16
    }
}

/// Simple word-wrap implementation.
fn wrap_text(text: &str, max_width: usize) -> Vec<String> {
    if max_width == 0 || text.is_empty() {
        return vec![text.to_string()];
    }

    let mut lines = Vec::new();
    let mut current_line = String::new();
    let mut current_width = 0;

    for word in text.split_inclusive(|c: char| c.is_whitespace()) {
        let word_width = word.chars().count();

        if current_width + word_width > max_width && !current_line.is_empty() {
            lines.push(current_line);
            current_line = String::new();
            current_width = 0;
        }

        current_line.push_str(word);
        current_width += word_width;
    }

    if !current_line.is_empty() {
        lines.push(current_line);
    }

    if lines.is_empty() {
        lines.push(String::new());
    }

    lines
}

/// Project precomputed content-span styles onto [`wrap_text`] segments.
///
/// Walks the source spans with a monotonic cursor, splitting only at existing
/// span edges or wrap edges and copying each whole [`Style`] onto owned
/// substrings, so the concat of every returned row's text equals its wrap
/// segment. Returns `None` when the span text and the segments do not
/// partition the same bytes (the caller keeps its solid-FG wrap path).
fn project_styles_onto_wrap_segments(
    content_spans: &[Span<'static>],
    wrapped_segments: &[String],
) -> Option<Vec<Vec<Span<'static>>>> {
    let spans_text: String = content_spans.iter().map(|s| s.content.as_ref()).collect();
    if spans_text != wrapped_segments.concat() {
        return None;
    }

    let mut rows = Vec::with_capacity(wrapped_segments.len());
    let mut span_idx = 0;
    let mut span_byte = 0;
    for segment in wrapped_segments {
        let mut row = Vec::new();
        let mut remaining = segment.len();
        while remaining > 0 {
            let span = content_spans.get(span_idx)?;
            let available = span.content.len().checked_sub(span_byte)?;
            if available == 0 {
                span_idx += 1;
                span_byte = 0;
                continue;
            }
            let take = available.min(remaining);
            // `get` fails closed if either edge is not a char boundary.
            let piece = span.content.get(span_byte..span_byte + take)?;
            row.push(Span::styled(piece.to_owned(), span.style));
            span_byte += take;
            remaining -= take;
        }
        rows.push(row);
    }
    Some(rows)
}

/// Gutter layout computed from a hunk and config.
struct GutterLayout {
    /// Width of old-file column (0 in single mode).
    width_old: usize,
    /// Width of new-file column.
    width_new: usize,
    /// Total gutter width including indent and gaps.
    total: usize,
    /// Whether dual mode is active.
    dual: bool,
}

/// Calculate gutter layout for a hunk.
fn gutter_layout(hunk: &DiffHunk, config: &DiffRenderConfig) -> GutterLayout {
    let mut max_old = 1usize;
    let mut max_new = 1usize;

    for line in hunk {
        max_old = max_old.max(line.lo.max(1));
        max_new = max_new.max(line.ln.max(1));
    }

    let indent_width = if config.indent { INDENT.len() } else { 0 };

    if config.dual_line_numbers {
        let width_old = max_old.ilog10() as usize + 1;
        let width_new = max_new.ilog10() as usize + 1;
        let total = indent_width + width_old + GUTTER_GAP.len() + width_new + CONTENT_GAP.len();
        GutterLayout {
            width_old,
            width_new,
            total,
            dual: true,
        }
    } else {
        let max_num = max_old.max(max_new);
        let width_new = max_num.ilog10() as usize + 1;
        let total = indent_width + width_new + CONTENT_GAP.len();
        GutterLayout {
            width_old: 0,
            width_new,
            total,
            dual: false,
        }
    }
}

/// Render the line number gutter.
fn render_gutter(
    spans: &mut Vec<Span<'static>>,
    line: &echo_agent_pager_diff::DiffLine,
    layout: &GutterLayout,
    theme: &Theme,
    config: &DiffRenderConfig,
) {
    let gutter_style = Style::default().fg(theme.diff_gutter_fg);

    // Indent (configurable)
    if config.indent {
        spans.push(Span::raw(INDENT));
    }

    if layout.dual {
        // Dual mode: two columns (old + new) like GitHub unified diff
        let w_old = layout.width_old;
        let w_new = layout.width_new;
        match line.tag {
            ChangeTag::Equal => {
                spans.push(Span::styled(format!("{:>w_old$}", line.lo), gutter_style));
                spans.push(Span::raw(GUTTER_GAP));
                spans.push(Span::styled(format!("{:>w_new$}", line.ln), gutter_style));
            }
            ChangeTag::Delete => {
                spans.push(Span::styled(
                    format!("{:>w_old$}", line.lo),
                    Style::default().fg(theme.diff_delete_fg),
                ));
                spans.push(Span::raw(GUTTER_GAP));
                spans.push(Span::styled(" ".repeat(w_new), gutter_style));
            }
            ChangeTag::Insert => {
                spans.push(Span::styled(" ".repeat(w_old), gutter_style));
                spans.push(Span::raw(GUTTER_GAP));
                spans.push(Span::styled(
                    format!("{:>w_new$}", line.ln),
                    Style::default().fg(theme.diff_insert_fg),
                ));
            }
        }
    } else {
        // Single mode: one column with the relevant line number
        let w = layout.width_new;
        match line.tag {
            ChangeTag::Equal => {
                spans.push(Span::styled(format!("{:>w$}", line.ln), gutter_style));
            }
            ChangeTag::Delete => {
                spans.push(Span::styled(
                    format!("{:>w$}", line.lo),
                    Style::default().fg(theme.diff_delete_fg),
                ));
            }
            ChangeTag::Insert => {
                spans.push(Span::styled(
                    format!("{:>w$}", line.ln),
                    Style::default().fg(theme.diff_insert_fg),
                ));
            }
        }
    }

    // Gap between gutter and content
    spans.push(Span::raw(CONTENT_GAP));
}

/// Span with `style`; empty text paints a single space so the row keeps a
/// visible background band and stays selectable.
fn painted(text: &str, style: Style) -> Span<'static> {
    let text = if text.is_empty() { " " } else { text };
    Span::styled(text.to_string(), style)
}

fn advance_highlighter(
    highlighter: &mut Option<HighlightLines<'_>>,
    content: &str,
    syntect: &Syntect,
) {
    if let Some(hl) = highlighter.as_mut() {
        let _ = hl.highlight_line(&format!("{content}\n"), &syntect.syntax_set);
    }
}

/// Render content spans with syntax highlighting.
fn render_content_spans(
    content: &str,
    tag: ChangeTag,
    theme: &Theme,
    highlighter: &mut Option<HighlightLines<'_>>,
    syntect: &Syntect,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();

    if tag != ChangeTag::Equal && theme.diff_uses_line_fg() {
        let fg = match tag {
            ChangeTag::Delete => theme.diff_delete_fg,
            _ => theme.diff_insert_fg,
        };
        spans.push(painted(content, Style::default().fg(fg)));
        return spans;
    }

    // Try syntax highlighting
    if let Some(hl) = highlighter.as_mut()
        && let Ok(ranges) = hl.highlight_line(&format!("{content}\n"), &syntect.syntax_set)
    {
        let mut wrote = false;
        for (style, segment) in ranges {
            let mut text = segment.to_owned();
            while text.ends_with('\n') || text.ends_with('\r') {
                text.pop();
            }
            if text.is_empty() {
                continue;
            }
            let fg_style = syntect_to_ratatui_fg(style);
            spans.push(Span::styled(text, fg_style));
            wrote = true;
        }
        if wrote {
            return spans;
        }
    }

    // Fallback: plain text
    let style = match tag {
        ChangeTag::Equal => Style::default().fg(theme.diff_equal_fg),
        ChangeTag::Delete | ChangeTag::Insert => Style::default().fg(theme.text_primary),
    };
    spans.push(painted(content, style));

    spans
}

/// Convert syntect style to ratatui style (FG only, no BG).
fn syntect_to_ratatui_fg(style: SyntectStyle) -> Style {
    crate::syntax::syntect_to_ratatui_fg(style)
}

/// Edit tool call block - displays file edit with diff.
#[derive(Debug, Clone)]
pub struct EditToolCallBlock {
    /// File path being edited.
    pub path: String,
    /// Diff hunks.
    pub hunks: Vec<DiffHunk>,
    /// Number of edits (for multi-edit display).
    pub edit_count: usize,
    /// Error message if the tool call failed (None = success).
    pub error: Option<String>,
    /// When the tool started running (Phase 2: time tracking).
    pub started_at: Option<std::time::Instant>,
    /// Elapsed time in ms after completion (Phase 2: time tracking).
    pub elapsed_ms: Option<i64>,
    /// Header prefix (e.g. "Edit " or "Creating ").
    pub prefix: &'static str,
    pub display_name: Option<String>,
    /// One-liner summary can't be trusted: the call touched multiple files
    /// (apply_patch emits one Diff per file, only the first becomes hunks) or
    /// the path fell back to the tool title. Suppresses the diffstat suffix;
    /// `ScrollbackState`'s materialize policy keeps such blocks expanded.
    pub summary_untrusted: bool,
    /// Cached `(insertions, deletions)` count, computed eagerly from hunks.
    change_counts: (usize, usize),
    /// Hunk-only first paint; may upgrade to FileScoped via the edit-HL worker.
    pub highlight: EditHighlightPhase,
}

fn workflow_script_name(path: &str) -> Option<String> {
    let p = Path::new(path);
    if p.extension().is_none_or(|e| e != "rhai") {
        return None;
    }
    if !p
        .ancestors()
        .skip(1)
        .any(|a| a.file_name().is_some_and(|n| n == "workflows"))
    {
        return None;
    }
    Some(p.file_stem()?.to_string_lossy().into_owned())
}

impl EditToolCallBlock {
    /// Create a new edit block.
    ///
    /// Pre-completed blocks have no meaningful local timing — `started_at`
    /// is `None`. Timing is only set for blocks that enter a running UI
    /// state (via `set_last_running(true)` in `ScrollbackState`).
    pub fn new(path: impl Into<String>, hunks: Vec<DiffHunk>) -> Self {
        let path = path.into();
        let edit_count = hunks.len().max(1);
        let change_counts = Self::compute_changes(&hunks);
        let display_name = workflow_script_name(&path);
        Self {
            path,
            hunks,
            edit_count,
            error: None,
            started_at: None,
            elapsed_ms: None,
            prefix: if display_name.is_some() {
                "Editing workflow "
            } else {
                "Edit "
            },
            display_name,
            summary_untrusted: false,
            change_counts,
            highlight: EditHighlightPhase::HunkOnly,
        }
    }

    pub fn with_prefix(mut self, prefix: &'static str) -> Self {
        self.prefix = if self.display_name.is_some() && prefix == "Creating " {
            "Creating workflow "
        } else {
            prefix
        };
        self
    }

    /// Mark the one-liner summary as covering only part of the call.
    pub fn with_untrusted_summary(mut self) -> Self {
        self.summary_untrusted = true;
        self
    }

    /// Set error (marks as failed).
    pub fn with_error(mut self, error: impl Into<String>) -> Self {
        self.error = Some(error.into());
        self
    }

    /// Check if successful (no error).
    pub fn is_success(&self) -> bool {
        self.error.is_none()
    }

    /// Set error (mutable) — compute elapsed time if not already set (Phase 2).
    pub fn set_error(&mut self, error: Option<String>) {
        if self.elapsed_ms.is_none()
            && let Some(start) = self.started_at
        {
            self.elapsed_ms = Some(start.elapsed().as_millis() as i64);
        }
        self.error = error;
    }

    /// Finalize elapsed time from `started_at`.
    ///
    /// Idempotent: no-op if `started_at` is `None` (pre-completed block)
    /// or if `elapsed_ms` is already set (already finalized).
    pub fn finish(&mut self) {
        if self.elapsed_ms.is_some() {
            return;
        }
        if let Some(start) = self.started_at {
            self.elapsed_ms = Some(start.elapsed().as_millis() as i64);
        }
    }

    /// Get elapsed time in ms (Phase 2).
    pub fn elapsed_ms(&self) -> Option<i64> {
        match self.elapsed_ms {
            Some(ms) => Some(ms),
            None => self
                .started_at
                .map(|start| start.elapsed().as_millis() as i64),
        }
    }

    /// Set edit count explicitly.
    pub fn with_edit_count(mut self, count: usize) -> Self {
        self.edit_count = count;
        self
    }

    /// Get copyable text as a unified diff patch.
    ///
    /// Generates a patch format suitable for `git apply` or clipboard sharing.
    pub fn copy_text(&self) -> String {
        diff_hunks_to_patch(&self.path, &self.hunks)
    }

    /// Set hunks (mutable).
    pub fn set_hunks(&mut self, hunks: Vec<DiffHunk>) {
        self.edit_count = hunks.len().max(1);
        self.change_counts = Self::compute_changes(&hunks);
        self.hunks = hunks;
    }

    /// Count insertions and deletions across all hunks.
    fn compute_changes(hunks: &[DiffHunk]) -> (usize, usize) {
        let mut insertions = 0;
        let mut deletions = 0;
        for hunk in hunks {
            for line in hunk {
                match line.tag {
                    ChangeTag::Insert => insertions += 1,
                    ChangeTag::Delete => deletions += 1,
                    ChangeTag::Equal => {}
                }
            }
        }
        (insertions, deletions)
    }

    /// Get pre-computed insertion/deletion counts.
    fn count_changes(&self) -> (usize, usize) {
        self.change_counts
    }

    /// Header line: path painted for `surface` (Collapsed / Expanded / Fullscreen).
    #[allow(clippy::too_many_arguments)]
    fn header_line(
        &self,
        theme: &Theme,
        muted: bool,
        show_summary: bool,
        dim_details: bool,
        surface: crate::render::tool_paths::ToolPathSurface,
        cwd: Option<&Path>,
        width: Option<usize>,
    ) -> Line<'static> {
        let text_style = if muted {
            theme.muted()
        } else {
            theme.primary()
        };
        let bold_style = text_style.add_modifier(Modifier::BOLD);
        let path_style = if muted {
            theme.muted()
        } else {
            Style::default().fg(theme.path)
        };
        let detail_style = if dim_details {
            theme.dim()
        } else {
            theme.muted()
        };

        let prefix = self.prefix;

        // Build the suffix spans first so we can reserve space for them.
        // The suffix (diffstat / "(N edits)") renders only on the collapsed
        // one-liner: expanded and fullscreen surfaces show the hunks, so
        // their headers stay bare. Diffstat counts keep their diff colors
        // even when the header is muted; untrusted summaries (multi-file,
        // title-fallback path) never show counts that would only describe
        // the first diff.
        let collapsed = matches!(
            surface,
            crate::render::tool_paths::ToolPathSurface::Collapsed
        );
        let suffix_spans: Vec<Span<'static>> =
            if collapsed && show_summary && !self.hunks.is_empty() && !self.summary_untrusted {
                let (ins, del) = self.count_changes();
                if ins > 0 || del > 0 {
                    vec![
                        Span::styled(
                            format!(" +{ins}"),
                            Style::default().fg(theme.diff_insert_fg),
                        ),
                        Span::styled("/", detail_style),
                        Span::styled(format!("-{del}"), Style::default().fg(theme.diff_delete_fg)),
                    ]
                } else {
                    Vec::new()
                }
            } else if collapsed && self.edit_count > 1 {
                vec![Span::styled(
                    format!(" ({} edits)", self.edit_count),
                    detail_style,
                )]
            } else {
                Vec::new()
            };
        let suffix_width: usize = suffix_spans
            .iter()
            .map(|span| unicode_width::UnicodeWidthStr::width(span.content.as_ref()))
            .sum();

        let path = match &self.display_name {
            Some(name) => name.clone(),
            None => crate::render::tool_paths::path_for_tool_surface(
                &self.path,
                surface,
                cwd,
                width,
                prefix.len() + suffix_width,
            ),
        };

        let mut spans = vec![
            Span::styled(prefix, bold_style),
            Span::styled(path, path_style),
        ];
        spans.extend(suffix_spans);

        Line::from(spans)
    }

    fn path_link_target(&self, cwd: Option<&Path>) -> Option<crate::render::osc8::LinkTarget> {
        crate::render::osc8::tool_path_file_target(&self.path, cwd)
    }

    /// Render this block's hunks for its current highlight phase — the single
    /// dispatch point shared by scrollback `output()` and the fullscreen block
    /// viewer. FileScoped styles baked under a different theme than the live
    /// one are skipped (hunk-only paint) so a theme flip never mixes palettes.
    pub fn render_diff_lines(
        &self,
        theme: &Theme,
        width: u16,
        config: &DiffRenderConfig,
    ) -> Vec<DiffLineOutput> {
        let path = Path::new(&self.path);
        match &self.highlight {
            EditHighlightPhase::FileScoped {
                by_new_line,
                theme: baked,
            } if *baked == crate::theme::cache::current_kind() => render_diff_hunks_with_styles(
                &self.hunks,
                path,
                by_new_line.as_ref(),
                theme,
                width,
                config,
            ),
            _ => render_diff_hunks_highlighted(&self.hunks, path, theme, width, config),
        }
    }
}

struct WrappedEditHeaderLine {
    content: Line<'static>,
    joiner: Option<String>,
    path_spans: Option<Range<usize>>,
    selection_text: Option<String>,
    selection_boundary: Option<EditSelectionBoundary>,
}

#[derive(Default)]
struct EditSelectionBoundary {
    prefix: String,
    suffix: String,
}

fn split_joiner_by_path(
    joiner: &str,
    source_start: usize,
    path_range: &Range<usize>,
) -> (String, String) {
    let source_end = source_start.saturating_add(joiner.len());
    let owned_start = source_start.max(path_range.start);
    let owned_end = source_end.min(path_range.end);
    if owned_start >= owned_end {
        return (joiner.to_owned(), String::new());
    }

    let local_start = owned_start - source_start;
    let local_end = owned_end - source_start;
    let mut remainder = String::with_capacity(joiner.len() - (local_end - local_start));
    remainder.push_str(&joiner[..local_start]);
    remainder.push_str(&joiner[local_end..]);
    (remainder, joiner[local_start..local_end].to_owned())
}

fn wrap_edit_header(
    header: Line<'static>,
    width: usize,
    extra_indent: usize,
) -> Vec<WrappedEditHeaderLine> {
    let prefix_width = header
        .spans
        .first()
        .map(|span| unicode_width::UnicodeWidthStr::width(span.content.as_ref()))
        .unwrap_or(0);
    let total_indent = extra_indent + prefix_width;
    let wrap_width = width.saturating_sub(total_indent);
    let prefix_bytes = header.spans.first().map_or(0, |span| span.content.len());
    let path_bytes = header.spans.get(1).map_or(0, |span| span.content.len());
    let path_text = header
        .spans
        .get(1)
        .map_or_else(String::new, |span| span.content.to_string());
    let (wrapped, joiners, path_range, prefix_span) = if !header.spans.is_empty()
        && prefix_width > 0
    {
        let prefix_span = header.spans[0].clone();
        let content_line = Line::from(header.spans[1..].to_vec());
        let (wrapped, joiners) = crate::render::wrapping::word_wrap_lines_with_joiners(
            std::iter::once(content_line),
            wrap_width,
        );
        (wrapped, joiners, 0..path_bytes, Some(prefix_span))
    } else {
        let (wrapped, joiners) =
            crate::render::wrapping::word_wrap_lines_with_joiners(std::iter::once(header), width);
        (
            wrapped,
            joiners,
            prefix_bytes..prefix_bytes.saturating_add(path_bytes),
            None,
        )
    };

    let indent = " ".repeat(total_indent);
    let mut source_offset = 0usize;
    let mut rows: Vec<WrappedEditHeaderLine> = Vec::with_capacity(wrapped.len());
    let mut last_path_row: Option<usize> = None;
    for (row, (mut content, joiner)) in wrapped.into_iter().zip(joiners).enumerate() {
        let (original_joiner, non_path_joiner, path_joiner) = match joiner {
            Some(joiner) => {
                let (remainder, owned) = split_joiner_by_path(&joiner, source_offset, &path_range);
                source_offset = source_offset.saturating_add(joiner.len());
                (Some(joiner), Some(remainder), owned)
            }
            None => (None, None, String::new()),
        };
        let mut span_offset = source_offset;
        let mut path_span_start = None;
        let mut path_span_end = 0usize;
        for (span_idx, span) in content.spans.iter().enumerate() {
            let span_end = span_offset.saturating_add(span.content.len());
            if span_offset < path_range.end && path_range.start < span_end {
                path_span_start.get_or_insert(span_idx);
                path_span_end = span_idx + 1;
            }
            span_offset = span_end;
        }
        source_offset = span_offset;

        let selection_text = path_span_start.map(|start| {
            let mut selected = String::new();
            for span in &content.spans[start..path_span_end] {
                selected.push_str(span.content.as_ref());
            }
            selected
        });
        if selection_text.is_none()
            && !path_joiner.is_empty()
            && let Some(previous) = last_path_row.and_then(|index| rows.get_mut(index))
        {
            previous
                .selection_boundary
                .get_or_insert_with(EditSelectionBoundary::default)
                .suffix
                .push_str(&path_joiner);
        }

        let decoration_spans = if let Some(prefix) = &prefix_span {
            let decoration = if row == 0 {
                prefix.clone()
            } else {
                Span::raw(indent.clone())
            };
            content.spans.insert(0, decoration);
            1
        } else {
            0
        };
        let path_spans =
            path_span_start.map(|start| start + decoration_spans..path_span_end + decoration_spans);
        let has_path = path_spans.is_some();
        let first_path_row = has_path && last_path_row.is_none();
        let selection_boundary = first_path_row
            .then_some(path_joiner)
            .filter(|prefix| !prefix.is_empty())
            .map(|prefix| EditSelectionBoundary {
                prefix,
                suffix: String::new(),
            });
        let output_joiner = if has_path && !first_path_row {
            original_joiner
        } else {
            non_path_joiner
        };
        let output_index = rows.len();
        rows.push(WrappedEditHeaderLine {
            content,
            joiner: output_joiner,
            path_spans,
            selection_text,
            selection_boundary,
        });
        if has_path {
            last_path_row = Some(output_index);
        }
    }
    if source_offset < path_range.end
        && let Some(previous) = last_path_row.and_then(|index| rows.get_mut(index))
    {
        let suffix_start = source_offset.max(path_range.start) - path_range.start;
        previous
            .selection_boundary
            .get_or_insert_with(EditSelectionBoundary::default)
            .suffix
            .push_str(&path_text[suffix_start..]);
    }
    rows
}

impl EditToolCallBlock {
    pub(crate) fn rendered_output(&self, ctx: &BlockContext) -> RenderedBlockOutput {
        let theme = Theme::current();
        let edit_cfg = &ctx.appearance.scrollback.blocks.edit;
        let tool_cfg = &ctx.appearance.scrollback.blocks.tool;
        let muted_collapsed = ctx.mute_when_collapsed(tool_cfg.muted_collapsed);
        let dim_details = tool_cfg.dim_details;
        // Explicit pager.toml value wins; unset follows the shell-owned flag.
        let show_summary =
            edit_cfg.effective_line_summary(crate::appearance::cache::load_collapsed_edit_blocks());

        let cwd = ctx.cwd.as_deref();
        let link_target = self.path_link_target(cwd);

        match ctx.mode {
            DisplayMode::Collapsed => {
                // Collapsed: just header line. Path (span 1) is selectable.
                let line = self.header_line(
                    &theme,
                    muted_collapsed,
                    show_summary,
                    dim_details,
                    crate::render::tool_paths::ToolPathSurface::Collapsed,
                    cwd,
                    Some(ctx.content_width()),
                );
                // Spans: ["Edit ", path, optional suffix spans...]
                // Only the path span (index 1) is selectable.
                let path_end = if line.spans.len() > 2 {
                    2
                } else {
                    line.spans.len()
                };
                RenderedBlockOutput::from(BlockOutput {
                    lines: vec![BlockLine {
                        selectable: Selectable::Spans(1..path_end),
                        selection_range: Some(TOOL_HEADER_RANGE),
                        // Copy the painted path span (basename when collapsed).
                        content: line,
                        link_target,
                        ..Default::default()
                    }],
                })
            }
            DisplayMode::Truncated | DisplayMode::Expanded => {
                // Convert appearance config to render config
                let config = DiffRenderConfig {
                    indent: edit_cfg.indent,
                    gutter_bg: edit_cfg.gutter_bg,
                    indent_bg: edit_cfg.indent_bg,
                    hunk_separator: edit_cfg.hunk_separator.clone(),
                    dual_line_numbers: edit_cfg.dual_line_numbers,
                };
                // Header with word-wrap and hanging indent
                let header = self.header_line(
                    &theme,
                    false,
                    show_summary,
                    dim_details,
                    crate::render::tool_paths::ToolPathSurface::Expanded,
                    cwd,
                    None,
                );
                let bullet_indent = ctx
                    .appearance
                    .scrollback
                    .blocks
                    .tool
                    .bullet
                    .char()
                    .map(|c| unicode_width::UnicodeWidthStr::width(c) + 1)
                    .unwrap_or(0);
                let wrapped_header = wrap_edit_header(header, ctx.width as usize, bullet_indent);
                let mut lines: Vec<BlockLine> = Vec::with_capacity(wrapped_header.len());
                let mut boundary_entries = Vec::new();
                for (line_index, line) in wrapped_header.into_iter().enumerate() {
                    let has_path = line.path_spans.is_some();
                    if let Some(boundary) = line.selection_boundary {
                        boundary_entries.push(SelectionBoundaryEntry {
                            line_index,
                            boundary: Arc::new(SelectionBoundary::new(
                                boundary.prefix,
                                boundary.suffix,
                            )),
                        });
                    }
                    lines.push(BlockLine {
                        selectable: line.path_spans.map_or(Selectable::None, Selectable::Spans),
                        selection_range: Some(TOOL_HEADER_RANGE),
                        selection_text: line.selection_text,
                        joiner: line.joiner,
                        content: line.content,
                        link_target: if has_path { link_target.clone() } else { None },
                        ..Default::default()
                    });
                }

                // Error message (non-selectable decoration)
                if let Some(err) = &self.error {
                    lines.push(BlockLine::separator(Line::from("")));
                    for line in err.lines() {
                        lines.push(BlockLine::separator(Line::from(Span::styled(
                            line.to_string(),
                            theme.muted(),
                        ))));
                    }
                }

                // Diff content with syntax highlighting and full-width backgrounds
                if !self.hunks.is_empty() {
                    // Empty line after header (non-selectable)
                    lines.push(BlockLine::separator(Line::from("")));

                    let diff_lines = self.render_diff_lines(&theme, ctx.width, &config);
                    let mut diff_range_id: u16 = 1;
                    for output in diff_lines {
                        if output.is_separator {
                            lines.push(BlockLine::separator(output.line));
                            diff_range_id += 1;
                            continue;
                        }
                        let total = output.line.spans.len();
                        let gc = output.gutter_span_count;
                        let mut block_line = BlockLine {
                            selectable: if gc < total {
                                Selectable::Spans(gc..total)
                            } else {
                                Selectable::All
                            },
                            selection_range: Some(diff_range_id),
                            selection_text: Some(output.content_text),
                            joiner: output.joiner,
                            content: output.line,
                            ..Default::default()
                        };
                        if let Some(bg_color) = output.background {
                            block_line =
                                block_line.with_background_from(bg_color, output.content_start_col);
                        }
                        lines.push(block_line);
                    }
                }

                // Apply max_lines if set
                if let Some(max) = ctx.max_lines {
                    lines.truncate(max as usize);
                }

                boundary_entries.retain(|entry| entry.line_index < lines.len());
                RenderedBlockOutput {
                    output: BlockOutput { lines },
                    boundaries: SelectionBoundaries::from_entries(boundary_entries),
                }
            }
        }
    }
}

impl BlockContent for EditToolCallBlock {
    fn output(&self, ctx: &BlockContext) -> BlockOutput {
        self.rendered_output(ctx).output
    }

    fn accent(&self, ctx: &BlockContext) -> Option<AccentStyle> {
        // Edit blocks: use config accent color if set, otherwise no accent.
        // Note: errors no longer show red accent — they show red bullet instead.
        ctx.appearance
            .scrollback
            .blocks
            .edit
            .accent
            .map(AccentStyle::static_color)
    }

    fn bullet(&self, _ctx: &BlockContext) -> Option<AccentStyle> {
        // Failed edit: red bullet. Successful: default (gray/primary).
        if self.error.is_some() {
            let theme = Theme::current();
            Some(AccentStyle::static_color(theme.accent_error))
        } else {
            None
        }
    }

    fn accent_background(&self, ctx: &BlockContext) -> bool {
        ctx.appearance.scrollback.blocks.edit.accent_bg
    }

    fn has_vpad_for(&self, appearance: &AppearanceConfig) -> bool {
        appearance.scrollback.blocks.edit.vpad
    }

    fn background(&self, ctx: &BlockContext) -> BlockBackground {
        ctx.appearance.scrollback.blocks.edit.bg
    }

    fn has_raw_mode(&self) -> bool {
        false
    }

    fn is_foldable(&self) -> bool {
        // Foldable if there are hunks to show, or an error message to reveal.
        !self.hunks.is_empty() || self.error.is_some()
    }

    fn default_display_mode(&self) -> DisplayMode {
        // Context-free: the effective expanded default (pager.toml shape >
        // collapsed_edit_blocks flag) and the untrusted-summary escape live
        // in ScrollbackState's materialize policy (push / replace_tool_block).
        DisplayMode::Collapsed
    }

    fn finished_display_mode(&self) -> Option<DisplayMode> {
        if self.error.is_some() {
            Some(DisplayMode::Collapsed)
        } else {
            None // keep current mode (the config-aware default, or a user fold)
        }
    }

    fn next_fold_mode(&self, current: DisplayMode, _is_running: bool) -> DisplayMode {
        match current {
            DisplayMode::Collapsed => DisplayMode::Expanded,
            _ => DisplayMode::Collapsed,
        }
    }

    fn preamble(&self, ctx: &BlockContext) -> Option<Text<'static>> {
        let theme = Theme::current();
        let dim_details = ctx.appearance.scrollback.blocks.tool.dim_details;
        // Same effective toggle as `rendered_output` (moot here: the suffix
        // is collapsed-only and this is the Fullscreen surface).
        let show_summary = ctx
            .appearance
            .scrollback
            .blocks
            .edit
            .effective_line_summary(crate::appearance::cache::load_collapsed_edit_blocks());
        Some(Text::from(self.header_line(
            &theme,
            false,
            show_summary,
            dim_details,
            crate::render::tool_paths::ToolPathSurface::Fullscreen,
            ctx.cwd.as_deref(),
            None,
        )))
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::appearance::AppearanceConfig;
    use crate::scrollback::types::DisplayMode;
    use echo_agent_pager_diff::DiffLine;

    fn test_ctx() -> BlockContext {
        BlockContext {
            mode: DisplayMode::Expanded,
            is_running: false,
            width: 80,
            raw: false,
            max_lines: None,
            appearance: AppearanceConfig::default(),
            is_selected: false,
            cwd: None,
        }
    }

    fn make_hunk() -> DiffHunk {
        vec![
            DiffLine {
                text: "let x = 1;\n".into(),
                lo: 10,
                ln: 10,
                tag: ChangeTag::Equal,
            },
            DiffLine {
                text: "let y = 2;\n".into(),
                lo: 11,
                ln: 0,
                tag: ChangeTag::Delete,
            },
            DiffLine {
                text: "let y = 3;\n".into(),
                lo: 0,
                ln: 11,
                tag: ChangeTag::Insert,
            },
            DiffLine {
                text: "let z = 4;\n".into(),
                lo: 12,
                ln: 12,
                tag: ChangeTag::Equal,
            },
        ]
    }

    fn line_to_string(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn diff_outputs_to_string(outputs: &[DiffLineOutput]) -> String {
        outputs
            .iter()
            .map(|o| line_to_string(&o.line))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn test_gutter_layout_single() {
        let hunk = make_hunk();
        let config = DiffRenderConfig::default();
        let layout = gutter_layout(&hunk, &config);
        assert!(!layout.dual);
        assert_eq!(layout.width_new, 2);
    }

    #[test]
    fn test_gutter_layout_dual() {
        let hunk = make_hunk();
        let config = DiffRenderConfig {
            dual_line_numbers: true,
            ..Default::default()
        };
        let layout = gutter_layout(&hunk, &config);
        assert!(layout.dual);
        assert_eq!(layout.width_old, 2);
        assert_eq!(layout.width_new, 2);
    }

    use crate::render::tool_paths::ToolPathSurface;
    use std::path::Path;

    #[test]
    fn test_edit_block_header() {
        let block = EditToolCallBlock::new("src/main.rs", vec![]);
        let theme = Theme::current();
        let header = block.header_line(
            &theme,
            false,
            false,
            false,
            ToolPathSurface::Expanded,
            None,
            None,
        );
        let text: String = header.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "Edit src/main.rs");
    }

    #[test]
    fn test_edit_block_header_multi_edit() {
        // The "(N edits)" fallback is collapsed-only, like the diffstat.
        let block = EditToolCallBlock::new("src/main.rs", vec![]).with_edit_count(3);
        let theme = Theme::current();
        let header = block.header_line(
            &theme,
            false,
            false,
            false,
            ToolPathSurface::Collapsed,
            None,
            Some(80),
        );
        let text: String = header.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "Edit main.rs (3 edits)");
    }

    #[test]
    fn workflow_script_header_hides_rhai_path() {
        let theme = Theme::current();
        let block = EditToolCallBlock::new(".echo-agent/workflows/cc-deep-research.rhai", vec![]);
        let header = block.header_line(
            &theme,
            false,
            false,
            false,
            ToolPathSurface::Expanded,
            None,
            None,
        );
        let text: String = header.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "Editing workflow cc-deep-research");
        assert!(
            block
                .path_link_target(Some(Path::new("/repo")))
                .and_then(|target| crate::render::osc8::resolve_link_target(&target))
                .and_then(|resolved| resolved.osc8_url)
                .is_some_and(|u| u.contains("cc-deep-research.rhai")),
        );

        let block = EditToolCallBlock::new(".echo-agent/workflows/triage.rhai", vec![])
            .with_prefix("Creating ");
        let header = block.header_line(
            &theme,
            false,
            false,
            false,
            ToolPathSurface::Expanded,
            None,
            None,
        );
        let text: String = header.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "Creating workflow triage");

        let block = EditToolCallBlock::new("scripts/build.rhai", vec![]);
        assert_eq!(block.prefix, "Edit ");
        assert!(block.display_name.is_none());

        let block = EditToolCallBlock::new("workflows/wf_0199abc/script.rhai", vec![]);
        assert_eq!(block.display_name.as_deref(), Some("script"));
    }

    #[test]
    fn header_diffstat_spans_use_diff_colors() {
        let block = EditToolCallBlock::new("/Users/me/project/src/foo.rs", vec![make_hunk()]);
        let theme = Theme::current();
        let header = block.header_line(
            &theme,
            false,
            true,
            false,
            ToolPathSurface::Collapsed,
            None,
            Some(80),
        );
        // Spans: ["Edit ", basename, " +1", "/", "-1"] — path stays span 1 so
        // the collapsed arm's selection/link invariant holds. Sole pin of the
        // exact diffstat suffix format.
        assert_eq!(header.spans.len(), 5);
        assert_eq!(header.spans[0].content.as_ref(), "Edit ");
        assert_eq!(header.spans[1].content.as_ref(), "foo.rs");
        assert_eq!(header.spans[2].content.as_ref(), " +1");
        assert_eq!(header.spans[2].style.fg, Some(theme.diff_insert_fg));
        assert_eq!(header.spans[3].content.as_ref(), "/");
        assert_eq!(header.spans[4].content.as_ref(), "-1");
        assert_eq!(header.spans[4].style.fg, Some(theme.diff_delete_fg));

        // The suffix is collapsed-only: expanded and fullscreen headers stay
        // bare — the hunks/body carry the information there. Both suffix
        // shapes (diffstat, "(N edits)" fallback) are gated.
        let multi = block.clone().with_edit_count(3);
        for surface in [ToolPathSurface::Expanded, ToolPathSurface::Fullscreen] {
            let header = block.header_line(&theme, false, true, false, surface, None, None);
            assert_eq!(header.spans.len(), 2, "no suffix spans on {surface:?}");
            let header = multi.header_line(&theme, false, false, false, surface, None, None);
            assert_eq!(
                header.spans.len(),
                2,
                "no (N edits) fallback on {surface:?}"
            );
        }
    }

    #[test]
    fn untrusted_summary_suppresses_diffstat() {
        // Counts would only describe the first diff of a multi-file call, so
        // the suffix falls back to "(N edits)" / nothing.
        let block =
            EditToolCallBlock::new("src/foo.rs", vec![make_hunk()]).with_untrusted_summary();
        let theme = Theme::current();
        let header = block.header_line(
            &theme,
            false,
            true,
            false,
            ToolPathSurface::Collapsed,
            None,
            Some(80),
        );
        let text: String = header.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "Edit foo.rs");

        let block = block.with_edit_count(3);
        let header = block.header_line(
            &theme,
            false,
            true,
            false,
            ToolPathSurface::Collapsed,
            None,
            Some(80),
        );
        let text: String = header.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "Edit foo.rs (3 edits)");
    }

    #[test]
    fn collapsed_mode_renders_header_only() {
        // The block's context-free default is Collapsed (the effective
        // expanded default is applied by ScrollbackState's materialize
        // policy, pinned in state/mod.rs).
        let block = EditToolCallBlock::new("src/foo.rs", vec![make_hunk()]);
        let entry = crate::scrollback::entry::ScrollbackEntry::new(
            crate::scrollback::block::RenderBlock::ToolCall(
                crate::scrollback::blocks::tool::ToolCallBlock::Edit(block.clone()),
            ),
        );
        assert_eq!(entry.display_mode, DisplayMode::Collapsed);

        let mut ctx = test_ctx();
        ctx.mode = DisplayMode::Collapsed;
        let output = block.output(&ctx);
        assert_eq!(output.lines.len(), 1, "collapsed shows the header only");
        // Exact suffix format is pinned by header_diffstat_spans_use_diff_colors.
        let text = line_to_string(&output.lines[0].content);
        assert!(text.starts_with("Edit foo.rs"), "header line: {text:?}");
        assert!(!text.contains("let y"), "no diff body while collapsed");
    }

    #[test]
    fn expanded_shows_relative_when_under_cwd_preamble_absolute() {
        let abs = "/Users/me/project/src/foo.rs";
        let cwd = Path::new("/Users/me/project");
        let block = EditToolCallBlock::new(abs, vec![]);
        let theme = Theme::current();
        let header = block.header_line(
            &theme,
            false,
            false,
            false,
            ToolPathSurface::Expanded,
            Some(cwd),
            None,
        );
        let text: String = header.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "Edit src/foo.rs");

        let mut ctx = test_ctx();
        ctx.mode = DisplayMode::Expanded;
        ctx.cwd = Some(cwd.to_path_buf());
        let preamble = block.preamble(&ctx).unwrap();
        let preamble_text: String = preamble
            .lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(preamble_text, "Edit /Users/me/project/src/foo.rs");
    }

    #[test]
    fn collapsed_selection_matches_painted_basename() {
        use crate::scrollback::types::derive_selection_text;

        let block = EditToolCallBlock::new("/Users/me/project/src/foo.rs", vec![]);
        let mut ctx = test_ctx();
        ctx.mode = DisplayMode::Collapsed;
        let output = block.output(&ctx);
        let header = &output.lines[0];
        assert_eq!(header.content.spans[1].content.as_ref(), "foo.rs");
        assert_eq!(derive_selection_text(header), "foo.rs");
        assert!(header.selection_text.is_none());
    }

    #[test]
    fn header_link_target_is_absolute_file_for_all_surfaces() {
        let abs = "/Users/me/project/src/foo.rs";
        let cwd = Path::new("/Users/me/project");
        let block = EditToolCallBlock::new(abs, vec![]);
        let target = block.path_link_target(Some(cwd)).expect("file target");
        assert_eq!(
            target,
            crate::render::osc8::LinkTarget::File(Arc::from(Path::new(abs)))
        );
        assert_eq!(
            crate::render::osc8::resolve_link_target(&target)
                .unwrap()
                .osc8_url
                .unwrap()
                .as_ref(),
            "file:///Users/me/project/src/foo.rs"
        );

        let mut ctx = test_ctx();
        ctx.cwd = Some(cwd.to_path_buf());
        ctx.mode = DisplayMode::Collapsed;
        let collapsed = block.output(&ctx);
        assert_eq!(
            collapsed.lines[0].content.spans[1].content.as_ref(),
            "foo.rs"
        );
        assert_eq!(collapsed.lines[0].link_target.as_ref(), Some(&target));

        ctx.mode = DisplayMode::Expanded;
        let expanded = block.output(&ctx);
        assert_eq!(
            expanded.lines[0].content.spans[1].content.as_ref(),
            "src/foo.rs"
        );
        assert_eq!(expanded.lines[0].link_target.as_ref(), Some(&target));
    }

    #[test]
    fn wrapped_header_path_provenance_survives_all_wrap_shapes() {
        let cases = [
            "src/averyverylongfilename.rs",
            "dir with spaces/file name.rs",
            "src/界面/éclair_文件.rs",
            "   foo.rs",
            "foo.rs   ",
        ];
        let mut saw_mid_word = false;

        for path in cases {
            for width in 8..=48 {
                // Mirrors production: expanded headers are prefix + path only
                // (the diffstat suffix is collapsed-only, and collapsed
                // headers never reach wrap_edit_header).
                let header = Line::from(vec![Span::raw("Edit "), Span::raw(path.to_owned())]);
                let wrapped = wrap_edit_header(header, width, 2);
                let mut reassembled = String::new();

                for row in &wrapped {
                    let Some(fragment) = &row.selection_text else {
                        assert!(
                            row.path_spans.is_none(),
                            "non-selectable row retained path spans at width {width}"
                        );
                        continue;
                    };
                    assert!(row.path_spans.is_some());
                    if !reassembled.is_empty()
                        && let Some(joiner) = &row.joiner
                    {
                        reassembled.push_str(joiner);
                    }
                    if let Some(boundary) = &row.selection_boundary {
                        reassembled.push_str(&boundary.prefix);
                    }
                    reassembled.push_str(fragment);
                    if let Some(boundary) = &row.selection_boundary {
                        reassembled.push_str(&boundary.suffix);
                    }

                    saw_mid_word |= row.joiner.as_deref() == Some("");
                }

                assert_eq!(reassembled, path, "path mismatch at width {width}");
            }
        }

        assert!(saw_mid_word, "missing mid-word continuation coverage");
    }

    #[test]
    fn test_edit_block_output_line_count() {
        let block = EditToolCallBlock::new("src/lib.rs", vec![make_hunk()]);
        let ctx = test_ctx();
        let output = block.output(&ctx);
        assert_eq!(output.lines.len(), 6); // header + empty + 4 diff
    }

    #[test]
    fn test_edit_block_backgrounds() {
        let block = EditToolCallBlock::new("test.rs", vec![make_hunk()]);
        let ctx = test_ctx();
        let output = block.output(&ctx);

        assert_eq!(output.lines[0].background, None); // header
        assert_eq!(output.lines[2].background, None); // equal
        assert!(output.lines[3].background.is_some()); // delete
        assert!(output.lines[4].background.is_some()); // insert
        assert_eq!(output.lines[5].background, None); // equal

        // Insert/delete shading is semantic, NOT a decorative panel — it must
        // survive minimal mode's flat rendering (EntryRenderer::flat_background),
        // so it must never be marked `background_is_panel`.
        assert!(
            output.lines.iter().all(|l| !l.background_is_panel),
            "diff shading must not be marked panel"
        );
    }

    #[test]
    fn test_edit_block_no_accent() {
        let block = EditToolCallBlock::new("test.rs", vec![]);
        let ctx = test_ctx();
        assert!(block.accent(&ctx).is_none());
    }

    #[test]
    fn test_diff_line_exact_layout() {
        let hunk = vec![
            DiffLine {
                text: "old line\n".into(),
                lo: 5,
                ln: 0,
                tag: ChangeTag::Delete,
            },
            DiffLine {
                text: "new line\n".into(),
                lo: 0,
                ln: 5,
                tag: ChangeTag::Insert,
            },
        ];

        let theme = Theme::current();
        let config = DiffRenderConfig::default();
        let path = Path::new("test.txt");
        let outputs = render_diff_hunk_highlighted(&hunk, path, &theme, 80, &config);

        assert_eq!(outputs.len(), 2);
        assert_eq!(line_to_string(&outputs[0].line), "  5  old line");
        assert_eq!(line_to_string(&outputs[1].line), "  5  new line");
        assert_eq!(outputs[0].content_start_col, 5);
        assert_eq!(outputs[1].content_start_col, 5);
    }

    #[test]
    fn test_diff_line_exact_layout_two_digit() {
        let hunk = vec![
            DiffLine {
                text: "context\n".into(),
                lo: 10,
                ln: 10,
                tag: ChangeTag::Equal,
            },
            DiffLine {
                text: "deleted\n".into(),
                lo: 11,
                ln: 0,
                tag: ChangeTag::Delete,
            },
            DiffLine {
                text: "inserted\n".into(),
                lo: 0,
                ln: 11,
                tag: ChangeTag::Insert,
            },
            DiffLine {
                text: "more context\n".into(),
                lo: 12,
                ln: 12,
                tag: ChangeTag::Equal,
            },
        ];

        let theme = Theme::current();
        let config = DiffRenderConfig::default();
        let path = Path::new("test.txt");
        let outputs = render_diff_hunk_highlighted(&hunk, path, &theme, 80, &config);

        assert_eq!(outputs.len(), 4);
        assert_eq!(line_to_string(&outputs[0].line), "  10  context");
        assert_eq!(line_to_string(&outputs[1].line), "  11  deleted");
        assert_eq!(line_to_string(&outputs[2].line), "  11  inserted");
        assert_eq!(line_to_string(&outputs[3].line), "  12  more context");

        for output in &outputs {
            assert_eq!(output.content_start_col, 6);
        }
    }

    #[test]
    fn test_diff_gutter_bg_flag() {
        let hunk = vec![DiffLine {
            text: "inserted\n".into(),
            lo: 0,
            ln: 5,
            tag: ChangeTag::Insert,
        }];

        let theme = Theme::current();
        let path = Path::new("test.txt");

        // Default: indent=false, gutter_bg=false
        let config_default = DiffRenderConfig::default();
        let outputs = render_diff_hunk_highlighted(&hunk, path, &theme, 80, &config_default);
        // Without indent, gutter is narrower
        assert_eq!(outputs[0].background, Some(theme.diff_insert_bg));

        let config_gutter = DiffRenderConfig {
            indent: false,
            gutter_bg: true,
            indent_bg: true,
            ..Default::default()
        };
        let outputs = render_diff_hunk_highlighted(&hunk, path, &theme, 80, &config_gutter);
        assert_eq!(outputs[0].content_start_col, 0);
        assert_eq!(outputs[0].background, Some(theme.diff_insert_bg));
    }

    #[test]
    fn test_diff_reflow_wrapping() {
        let hunk = vec![DiffLine {
            text: "this is a very long line that should wrap when width is narrow\n".into(),
            lo: 1,
            ln: 1,
            tag: ChangeTag::Equal,
        }];

        let theme = Theme::current();
        let config = DiffRenderConfig::default();
        let path = Path::new("test.txt");
        let outputs = render_diff_hunk_highlighted(&hunk, path, &theme, 30, &config);

        assert!(outputs.len() > 1, "got {} lines", outputs.len());

        let first = line_to_string(&outputs[0].line);
        assert_eq!(&first[..5], "  1  ");

        let second = line_to_string(&outputs[1].line);
        assert_eq!(&second[..5], "     ");

        for output in &outputs {
            assert_eq!(output.content_start_col, 5);
            assert_eq!(output.background, None);
        }
    }

    /// Regression pin: wrapped continuation segments of a
    /// changed line used to fall back to `text_primary` (= Reset on bandless
    /// themes), reading like plain context.
    #[test]
    fn test_diff_reflow_keeps_change_fg_for_bandless_theme() {
        let theme = Theme::terminal_default();
        assert!(theme.diff_uses_line_fg(), "palette must be bandless");
        let config = DiffRenderConfig::default();
        let path = Path::new("test.txt");

        for (tag, expected_fg, label) in [
            (ChangeTag::Insert, theme.diff_insert_fg, "insert"),
            (ChangeTag::Delete, theme.diff_delete_fg, "delete"),
        ] {
            let hunk = vec![DiffLine {
                text: "this is a long changed line that needs to wrap around into \
                       several continuation segments\n"
                    .into(),
                lo: if tag == ChangeTag::Delete { 1 } else { 0 },
                ln: if tag == ChangeTag::Insert { 1 } else { 0 },
                tag,
            }];
            let outputs = render_diff_hunk_highlighted(&hunk, path, &theme, 30, &config);
            assert!(outputs.len() > 1, "{label}: expected wrapping");
            for (i, output) in outputs.iter().enumerate() {
                let content_fg = output
                    .line
                    .spans
                    .iter()
                    .skip(output.gutter_span_count)
                    .find_map(|s| s.style.fg)
                    .expect("content span with fg");
                assert_eq!(
                    content_fg, expected_fg,
                    "{label}: wrapped segment {i} must keep the change foreground"
                );
            }
        }
    }

    #[test]
    fn test_diff_reflow_preserves_background() {
        let hunk = vec![DiffLine {
            text: "this is a long inserted line that needs to wrap around\n".into(),
            lo: 0,
            ln: 1,
            tag: ChangeTag::Insert,
        }];

        let theme = Theme::current();
        let config = DiffRenderConfig::default();
        let path = Path::new("test.txt");
        let outputs = render_diff_hunk_highlighted(&hunk, path, &theme, 30, &config);

        assert!(outputs.len() > 1);
        for output in &outputs {
            assert_eq!(output.background, Some(theme.diff_insert_bg));
        }
    }

    /// Wrapped diff rows must keep the precomputed syntect / FileScoped styles
    /// instead of flattening to a solid foreground on banded themes.
    #[test]
    fn test_diff_reflow_preserves_banded_syntect_styles_across_wrap() {
        let _guard = pin_echo_agentnight_syntect();
        let theme = Theme::echo_agent_night();
        let config = DiffRenderConfig::default();
        let path = Path::new("probe.rs");
        let source = "let naïve = compute(input); // café comment stretching over wraps";

        fn style_stream(outputs: &[DiffLineOutput]) -> Vec<(char, Style)> {
            outputs
                .iter()
                .flat_map(|o| {
                    o.line.spans.iter().skip(o.gutter_span_count).flat_map(|s| {
                        let style = s.style;
                        s.content.chars().map(move |c| (c, style))
                    })
                })
                .collect()
        }

        for (tag, expected_bg, label) in [
            (ChangeTag::Insert, Some(theme.diff_insert_bg), "insert"),
            (ChangeTag::Equal, None, "equal"),
            (ChangeTag::Delete, Some(theme.diff_delete_bg), "delete"),
        ] {
            let hunk = vec![DiffLine {
                text: format!("{source}\n"),
                lo: if tag == ChangeTag::Insert { 0 } else { 1 },
                ln: if tag == ChangeTag::Delete { 0 } else { 1 },
                tag,
            }];
            let wide = render_diff_hunk_highlighted(&hunk, path, &theme, 120, &config);
            assert_eq!(wide.len(), 1, "{label}: wide render must not wrap");
            let narrow = render_diff_hunk_highlighted(&hunk, path, &theme, 30, &config);
            assert!(narrow.len() > 1, "{label}: narrow render must wrap");

            // Per-character styles survive the wrap, row 0 included.
            let narrow_stream = style_stream(&narrow);
            assert_eq!(
                narrow_stream,
                style_stream(&wide),
                "{label}: wrapped rows must keep the unwrapped styles"
            );

            // Non-vacuous fixture: the comment is italic, the code is not.
            assert!(
                narrow_stream
                    .iter()
                    .any(|(_, st)| st.add_modifier.contains(Modifier::ITALIC)),
                "{label}: comment chars must be italic"
            );
            assert!(
                narrow_stream
                    .iter()
                    .any(|(_, st)| !st.add_modifier.contains(Modifier::ITALIC)),
                "{label}: code chars must not be italic"
            );

            // Geometry is unchanged: joiners, content_text partition, painted
            // content after the gutter, background band on every row.
            assert_eq!(narrow[0].joiner, None, "{label}: row 0 joiner");
            assert!(
                narrow[1..].iter().all(|o| o.joiner == Some(String::new())),
                "{label}: continuation joiners"
            );
            let joined: String = narrow.iter().map(|o| o.content_text.as_str()).collect();
            assert_eq!(joined, source, "{label}: content_text partition");
            for (i, output) in narrow.iter().enumerate() {
                let painted: String = output.line.spans[output.gutter_span_count..]
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect();
                assert_eq!(
                    painted, output.content_text,
                    "{label}: row {i} painted text"
                );
                assert_eq!(
                    output.background, expected_bg,
                    "{label}: row {i} background"
                );
            }
        }

        // FileScoped styles flow through the same projection: a style edge
        // mid-word must survive wrapping at a different column.
        let fs_source = "abcdef ghijkl mnopqr stuvwx yzabcd efghij";
        let split = 17;
        let style_a = Style::default()
            .fg(Color::Rgb(10, 20, 30))
            .add_modifier(Modifier::BOLD);
        let style_b = Style::default()
            .fg(Color::Rgb(200, 100, 50))
            .add_modifier(Modifier::ITALIC);
        let mut map = HashMap::new();
        map.insert(
            1usize,
            vec![
                (style_a, fs_source[..split].to_string()),
                (style_b, fs_source[split..].to_string()),
            ],
        );
        let hunk = vec![DiffLine {
            text: format!("{fs_source}\n"),
            lo: 0,
            ln: 1,
            tag: ChangeTag::Insert,
        }];
        let outputs = render_diff_hunks_with_styles(&[hunk], path, &map, &theme, 30, &config);
        assert!(outputs.len() > 1, "file-scoped render must wrap");
        let expected: Vec<(char, Style)> = fs_source
            .chars()
            .enumerate()
            .map(|(i, c)| (c, if i < split { style_a } else { style_b }))
            .collect();
        assert_eq!(
            style_stream(&outputs),
            expected,
            "file-scoped styles must survive the wrap"
        );
    }

    /// A token wider than the content width still wraps into a single row and
    /// must keep its styles rather than flatten to `text_primary`.
    #[test]
    fn test_diff_reflow_keeps_overlong_token_styles() {
        let _guard = pin_echo_agentnight_syntect();
        let theme = Theme::echo_agent_night();
        let config = DiffRenderConfig::default();
        let path = Path::new("probe.rs");
        let source = format!("//{}", "x".repeat(40));
        let hunk = vec![DiffLine {
            text: format!("{source}\n"),
            lo: 0,
            ln: 1,
            tag: ChangeTag::Insert,
        }];

        let outputs = render_diff_hunk_highlighted(&hunk, path, &theme, 30, &config);
        assert_eq!(outputs.len(), 1, "overlong token must stay one row");
        assert_eq!(outputs[0].joiner, None);
        assert_eq!(outputs[0].content_text, source);
        let content = &outputs[0].line.spans[outputs[0].gutter_span_count..];
        let painted: String = content.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(painted, source);
        assert!(
            content
                .iter()
                .all(|s| s.style.add_modifier.contains(Modifier::ITALIC)),
            "comment styles must survive the wrap branch"
        );
    }

    /// The projection helper fails closed when the segments do not partition
    /// the span text exactly.
    #[test]
    fn test_diff_style_projection_rejects_mismatched_partition() {
        let spans = vec![Span::styled(
            "hello world".to_string(),
            Style::default().fg(Color::Red),
        )];
        // Extra byte.
        assert!(
            project_styles_onto_wrap_segments(
                &spans,
                &["hello ".to_string(), "world!".to_string()]
            )
            .is_none()
        );
        // Same length, different bytes.
        assert!(
            project_styles_onto_wrap_segments(&spans, &["hello ".to_string(), "wOrld".to_string()])
                .is_none()
        );
        // Exact partition succeeds.
        let rows =
            project_styles_onto_wrap_segments(&spans, &["hello ".to_string(), "world".to_string()])
                .expect("exact partition must project");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0].content.as_ref(), "hello ");
        assert_eq!(rows[1][0].content.as_ref(), "world");

        // Empty mid-spans must be skipped without underflow/panic.
        let with_empty = vec![
            Span::styled("ab".to_string(), Style::default().fg(Color::Red)),
            Span::raw(""),
            Span::styled("cd".to_string(), Style::default().fg(Color::Blue)),
        ];
        let rows =
            project_styles_onto_wrap_segments(&with_empty, &["a".to_string(), "bcd".to_string()])
                .expect("empty mid-span must project");
        assert_eq!(
            rows.iter()
                .map(|row| row.iter().map(|s| s.content.as_ref()).collect::<String>())
                .collect::<Vec<_>>(),
            ["a", "bcd"]
        );
    }

    #[test]
    fn test_diff_hunk_separator() {
        let hunk1 = vec![DiffLine {
            text: "line1\n".into(),
            lo: 1,
            ln: 1,
            tag: ChangeTag::Equal,
        }];
        let hunk2 = vec![DiffLine {
            text: "line2\n".into(),
            lo: 10,
            ln: 10,
            tag: ChangeTag::Equal,
        }];

        let theme = Theme::current();
        let config = DiffRenderConfig::default();
        let path = Path::new("test.txt");
        let outputs = render_diff_hunks_highlighted(&[hunk1, hunk2], path, &theme, 80, &config);

        // Lines 2..=9 sit between the hunks: computable gap of 8.
        assert_eq!(outputs.len(), 3);
        assert!(outputs[1].is_separator);
        assert_eq!(line_to_string(&outputs[1].line), "  … 8 unchanged lines");
        assert_eq!(outputs[1].background, None);
    }

    #[test]
    fn hunk_separator_singular_gap() {
        let hunk1 = vec![DiffLine {
            text: "line1\n".into(),
            lo: 1,
            ln: 1,
            tag: ChangeTag::Equal,
        }];
        let hunk2 = vec![DiffLine {
            text: "line3\n".into(),
            lo: 3,
            ln: 3,
            tag: ChangeTag::Equal,
        }];

        let theme = Theme::current();
        let config = DiffRenderConfig::default();
        let path = Path::new("test.txt");
        let outputs = render_diff_hunks_highlighted(&[hunk1, hunk2], path, &theme, 80, &config);

        assert_eq!(line_to_string(&outputs[1].line), "  … 1 unchanged line");
    }

    #[test]
    fn hunk_separator_bare_for_non_monotonic_or_adjacent() {
        let mk = |ln: usize| {
            vec![DiffLine {
                text: format!("line{ln}\n"),
                lo: ln,
                ln,
                tag: ChangeTag::Equal,
            }]
        };
        let theme = Theme::current();
        let config = DiffRenderConfig::default();
        let path = Path::new("test.txt");

        // Non-monotonic ln (a coalesced later edit above an earlier one):
        // never render a negative/zero count, keep the bare separator.
        let outputs = render_diff_hunks_highlighted(&[mk(20), mk(4)], path, &theme, 80, &config);
        assert_eq!(line_to_string(&outputs[1].line), "  …");

        // Adjacent hunks (no hidden lines) keep the bare separator too.
        let outputs = render_diff_hunks_highlighted(&[mk(5), mk(6)], path, &theme, 80, &config);
        assert_eq!(line_to_string(&outputs[1].line), "  …");

        // A hunk with no new-file lines (pure deletion) is not computable.
        let pure_delete = vec![DiffLine {
            text: "gone\n".into(),
            lo: 9,
            ln: 9,
            tag: ChangeTag::Delete,
        }];
        let outputs =
            render_diff_hunks_highlighted(&[mk(5), pure_delete], path, &theme, 80, &config);
        assert_eq!(line_to_string(&outputs[1].line), "  …");
    }

    #[test]
    fn snapshot_diff_basic() {
        let theme = Theme::current();
        let config = DiffRenderConfig::default();
        let path = Path::new("test.txt");
        let outputs = render_diff_hunk_highlighted(&make_hunk(), path, &theme, 80, &config);
        insta::assert_snapshot!("diff_basic", diff_outputs_to_string(&outputs));
    }

    #[test]
    fn snapshot_diff_three_digit_lines() {
        let hunk = vec![
            DiffLine {
                text: "context before\n".into(),
                lo: 99,
                ln: 99,
                tag: ChangeTag::Equal,
            },
            DiffLine {
                text: "old code\n".into(),
                lo: 100,
                ln: 0,
                tag: ChangeTag::Delete,
            },
            DiffLine {
                text: "new code\n".into(),
                lo: 0,
                ln: 100,
                tag: ChangeTag::Insert,
            },
            DiffLine {
                text: "context after\n".into(),
                lo: 101,
                ln: 101,
                tag: ChangeTag::Equal,
            },
        ];

        let theme = Theme::current();
        let config = DiffRenderConfig::default();
        let path = Path::new("test.txt");
        let outputs = render_diff_hunk_highlighted(&hunk, path, &theme, 80, &config);
        insta::assert_snapshot!("diff_three_digit_lines", diff_outputs_to_string(&outputs));
    }

    #[test]
    fn snapshot_diff_reflow() {
        let hunk = vec![
            DiffLine {
                text: "short line\n".into(),
                lo: 1,
                ln: 1,
                tag: ChangeTag::Equal,
            },
            DiffLine {
                text: "this is a very long line that will definitely wrap to multiple lines\n"
                    .into(),
                lo: 0,
                ln: 2,
                tag: ChangeTag::Insert,
            },
            DiffLine {
                text: "another short one\n".into(),
                lo: 2,
                ln: 3,
                tag: ChangeTag::Equal,
            },
        ];

        let theme = Theme::current();
        let config = DiffRenderConfig::default();
        let path = Path::new("test.txt");
        let outputs = render_diff_hunk_highlighted(&hunk, path, &theme, 40, &config);
        insta::assert_snapshot!("diff_reflow", diff_outputs_to_string(&outputs));
    }

    #[test]
    fn snapshot_diff_multiple_hunks() {
        let hunk1 = vec![
            DiffLine {
                text: "first hunk context\n".into(),
                lo: 5,
                ln: 5,
                tag: ChangeTag::Equal,
            },
            DiffLine {
                text: "deleted in first\n".into(),
                lo: 6,
                ln: 0,
                tag: ChangeTag::Delete,
            },
        ];
        let hunk2 = vec![
            DiffLine {
                text: "second hunk context\n".into(),
                lo: 50,
                ln: 49,
                tag: ChangeTag::Equal,
            },
            DiffLine {
                text: "inserted in second\n".into(),
                lo: 0,
                ln: 50,
                tag: ChangeTag::Insert,
            },
        ];

        let theme = Theme::current();
        let config = DiffRenderConfig::default();
        let path = Path::new("test.txt");
        let outputs = render_diff_hunks_highlighted(&[hunk1, hunk2], path, &theme, 80, &config);
        insta::assert_snapshot!("diff_multiple_hunks", diff_outputs_to_string(&outputs));
    }

    #[test]
    fn snapshot_diff_merged_hunks_gap_markers() {
        // Shape of a coalesced block: hunks from consecutive same-file edits
        // appended in completion order, monotonically increasing, so every
        // separator carries a computable gap count.
        let hunk1 = vec![
            DiffLine {
                text: "fn one() {\n".into(),
                lo: 3,
                ln: 3,
                tag: ChangeTag::Equal,
            },
            DiffLine {
                text: "old_one();\n".into(),
                lo: 4,
                ln: 0,
                tag: ChangeTag::Delete,
            },
            DiffLine {
                text: "new_one();\n".into(),
                lo: 0,
                ln: 4,
                tag: ChangeTag::Insert,
            },
        ];
        let hunk2 = vec![
            DiffLine {
                text: "ctx_two();\n".into(),
                lo: 12,
                ln: 12,
                tag: ChangeTag::Equal,
            },
            DiffLine {
                text: "add_two();\n".into(),
                lo: 0,
                ln: 13,
                tag: ChangeTag::Insert,
            },
        ];
        let hunk3 = vec![
            DiffLine {
                text: "ctx_three();\n".into(),
                lo: 19,
                ln: 20,
                tag: ChangeTag::Equal,
            },
            DiffLine {
                text: "add_three();\n".into(),
                lo: 0,
                ln: 21,
                tag: ChangeTag::Insert,
            },
        ];

        let theme = Theme::current();
        let config = DiffRenderConfig::default();
        let path = Path::new("test.txt");
        let outputs =
            render_diff_hunks_highlighted(&[hunk1, hunk2, hunk3], path, &theme, 80, &config);
        insta::assert_snapshot!(
            "diff_merged_hunks_gap_markers",
            diff_outputs_to_string(&outputs)
        );
    }

    // --- dual_line_numbers = true snapshots ---

    fn dual_config() -> DiffRenderConfig {
        DiffRenderConfig {
            dual_line_numbers: true,
            ..Default::default()
        }
    }

    #[test]
    fn snapshot_diff_basic_dual() {
        let theme = Theme::current();
        let config = dual_config();
        let path = Path::new("test.txt");
        let outputs = render_diff_hunk_highlighted(&make_hunk(), path, &theme, 80, &config);
        insta::assert_snapshot!("diff_basic_dual", diff_outputs_to_string(&outputs));
    }

    #[test]
    fn snapshot_diff_three_digit_lines_dual() {
        let hunk = vec![
            DiffLine {
                text: "context before\n".into(),
                lo: 99,
                ln: 99,
                tag: ChangeTag::Equal,
            },
            DiffLine {
                text: "old code\n".into(),
                lo: 100,
                ln: 0,
                tag: ChangeTag::Delete,
            },
            DiffLine {
                text: "new code\n".into(),
                lo: 0,
                ln: 100,
                tag: ChangeTag::Insert,
            },
            DiffLine {
                text: "context after\n".into(),
                lo: 101,
                ln: 101,
                tag: ChangeTag::Equal,
            },
        ];

        let theme = Theme::current();
        let config = dual_config();
        let path = Path::new("test.txt");
        let outputs = render_diff_hunk_highlighted(&hunk, path, &theme, 80, &config);
        insta::assert_snapshot!(
            "diff_three_digit_lines_dual",
            diff_outputs_to_string(&outputs)
        );
    }

    #[test]
    fn snapshot_diff_reflow_dual() {
        let hunk = vec![
            DiffLine {
                text: "short line\n".into(),
                lo: 1,
                ln: 1,
                tag: ChangeTag::Equal,
            },
            DiffLine {
                text: "this is a very long line that will definitely wrap to multiple lines\n"
                    .into(),
                lo: 0,
                ln: 2,
                tag: ChangeTag::Insert,
            },
            DiffLine {
                text: "another short one\n".into(),
                lo: 2,
                ln: 3,
                tag: ChangeTag::Equal,
            },
        ];

        let theme = Theme::current();
        let config = dual_config();
        let path = Path::new("test.txt");
        let outputs = render_diff_hunk_highlighted(&hunk, path, &theme, 40, &config);
        insta::assert_snapshot!("diff_reflow_dual", diff_outputs_to_string(&outputs));
    }

    #[test]
    fn snapshot_diff_multiple_hunks_dual() {
        let hunk1 = vec![
            DiffLine {
                text: "first hunk context\n".into(),
                lo: 5,
                ln: 5,
                tag: ChangeTag::Equal,
            },
            DiffLine {
                text: "deleted in first\n".into(),
                lo: 6,
                ln: 0,
                tag: ChangeTag::Delete,
            },
        ];
        let hunk2 = vec![
            DiffLine {
                text: "second hunk context\n".into(),
                lo: 50,
                ln: 49,
                tag: ChangeTag::Equal,
            },
            DiffLine {
                text: "inserted in second\n".into(),
                lo: 0,
                ln: 50,
                tag: ChangeTag::Insert,
            },
        ];

        let theme = Theme::current();
        let config = dual_config();
        let path = Path::new("test.txt");
        let outputs = render_diff_hunks_highlighted(&[hunk1, hunk2], path, &theme, 80, &config);
        insta::assert_snapshot!("diff_multiple_hunks_dual", diff_outputs_to_string(&outputs));
    }

    /// Three-line all-Insert Go hunk with a tab-indented body (new-file lines 1-3).
    fn go_tab_hunk() -> DiffHunk {
        vec![
            DiffLine {
                text: "func main() {\n".into(),
                lo: 0,
                ln: 1,
                tag: ChangeTag::Insert,
            },
            DiffLine {
                text: "\tfmt.Println(\"hello\")\n".into(),
                lo: 0,
                ln: 2,
                tag: ChangeTag::Insert,
            },
            DiffLine {
                text: "}\n".into(),
                lo: 0,
                ln: 3,
                tag: ChangeTag::Insert,
            },
        ]
    }

    #[test]
    fn tabs_expanded_in_diff_lines() {
        // Simulates creating a new file with tab-indented content (e.g. Go, Makefile).
        // All lines are Insert — tabs must be expanded to spaces so they're visible.
        let hunk = go_tab_hunk();

        let theme = Theme::current();
        let config = DiffRenderConfig::default();
        let path = Path::new("main.go");
        let outputs = render_diff_hunk_highlighted(&hunk, path, &theme, 80, &config);

        assert_eq!(outputs.len(), 3);

        // The tab on line 2 should be expanded to spaces (default tab_width=4).
        let line2 = line_to_string(&outputs[1].line);
        assert!(
            !line2.contains('\t'),
            "tab character should be expanded, got: {:?}",
            line2,
        );
        assert!(
            line2.contains("    fmt"),
            "tab should become 4 spaces, got: {:?}",
            line2,
        );

        // content_text should also have expanded tabs.
        assert!(
            !outputs[1].content_text.contains('\t'),
            "content_text should also have tabs expanded",
        );
    }

    // ── Edit syntax-highlight harness (triple-quote spill) ──
    //
    // Asserts use **raw syntect RGB** (not ratatui FG after quantize). Under
    // `NO_COLOR` quantize maps every RGB → Reset, which would make keyword vs
    // string asserts tautological / false.

    type Rgb = (u8, u8, u8);
    type SyntectSpans = Vec<(Rgb, String)>;

    /// Pins EchoAgentNight via the shared test-lock guard; hold it for the whole
    /// test so a concurrent theme flip can't skew the compared highlighter walks.
    fn pin_echo_agentnight_syntect() -> std::sync::MutexGuard<'static, ()> {
        let guard = crate::theme::cache::pin_theme();
        assert!(
            !Theme::echo_agent_night().diff_uses_line_fg(),
            "EchoAgentNight must be banded for Equal-line syntect path in render"
        );
        guard
    }

    fn rgb_of(style: SyntectStyle) -> Rgb {
        (style.foreground.r, style.foreground.g, style.foreground.b)
    }

    fn highlight_line_raw(line: &str, hl: &mut HighlightLines<'_>) -> SyntectSpans {
        let syntect = get_syntect();
        let owned = format!("{line}\n");
        let ranges = hl
            .highlight_line(&owned, &syntect.syntax_set)
            .unwrap_or_default();
        let mut spans = Vec::new();
        for (style, segment) in ranges {
            let mut text = segment.to_owned();
            while text.ends_with('\n') || text.ends_with('\r') {
                text.pop();
            }
            if text.is_empty() {
                continue;
            }
            spans.push((rgb_of(style), text));
        }
        spans
    }

    /// Hunk-only: fresh highlighter, only the given lines in order (prod path).
    /// Caller pins the theme via `pin_echo_agentnight_syntect` (lock is not reentrant).
    fn hunk_only_raw_styles(path: &Path, lines: &[&str]) -> Vec<SyntectSpans> {
        let syntect = get_syntect();
        let mut hl = syntect
            .highlight_lines_by_file_path(path)
            .expect("syntax for path");
        lines
            .iter()
            .map(|line| highlight_line_raw(line, &mut hl))
            .collect()
    }

    /// Full-file then slice: silent HL from line 1, return styles for all lines.
    /// Caller pins the theme via `pin_echo_agentnight_syntect` (lock is not reentrant).
    fn full_file_raw_styles(path: &Path, file_text: &str) -> Vec<SyntectSpans> {
        let syntect = get_syntect();
        let mut hl = syntect
            .highlight_lines_by_file_path(path)
            .expect("syntax for path");
        file_text
            .lines()
            .map(|line| highlight_line_raw(line, &mut hl))
            .collect()
    }

    /// Triple-quote-spill shape: synthetic file text + 1-based line of the
    /// closing `"""`. Content is fictional (layout stress only).
    fn fixture_python_parts() -> (String, usize) {
        let file = "\
class ProcessQueueItem(BaseModel):
    \"\"\"Request body for processing a single queue item.

    The item id is in the path; keep notes in the body.
    \"\"\"

    notes: str = Field(..., min_length=1)
    category_id: CategoryId = Field(default=DEFAULT_CATEGORY_ID)
"
        .to_string();
        let close_ln = file
            .lines()
            .enumerate()
            .find(|(_, l)| l.trim() == "\"\"\"")
            .map(|(i, _)| i + 1)
            .expect("closing triple-quote");
        (file, close_ln)
    }

    /// Control: self-contained Rust line → keyword RGB ≠ string RGB (raw syntect).
    #[test]
    fn syntax_highlight_splits_keyword_and_string_fg() {
        let _guard = pin_echo_agentnight_syntect();
        let path = Path::new("probe.rs");
        let lines = hunk_only_raw_styles(path, &["let x = \"hello\";"]);
        assert_eq!(lines.len(), 1);
        let styles = &lines[0];
        let let_rgb = styles
            .iter()
            .find(|(_, t)| t.contains("let"))
            .map(|(c, _)| *c)
            .expect("keyword span");
        let str_rgb = styles
            .iter()
            .find(|(_, t)| t.contains("hello"))
            .map(|(c, _)| *c)
            .expect("string span");
        assert_ne!(
            let_rgb, str_rgb,
            "keyword and string must not share syntect FG; styles={styles:?}"
        );
    }

    /// Regression: a `"""` opened on a removed line must not change how the
    /// added line highlights. The two diff sides are highlighted independently.
    #[test]
    fn delete_side_multiline_string_does_not_leak_into_insert() {
        let _guard = pin_echo_agentnight_syntect();
        let path = Path::new("probe.py");
        let config = DiffRenderConfig::default();
        let theme = Theme::echo_agent_night();

        // Content spans of the added `def` line, given the removed line above it.
        let added_def = |removed: &str| -> Vec<(ratatui::style::Color, String)> {
            let hunk = vec![
                DiffLine {
                    text: format!("{removed}\n"),
                    lo: 1,
                    ln: 0,
                    tag: ChangeTag::Delete,
                },
                DiffLine {
                    text: "def parse(x: str) -> int:\n".into(),
                    lo: 0,
                    ln: 1,
                    tag: ChangeTag::Insert,
                },
            ];
            let rows = render_diff_hunk_highlighted(&hunk, path, &theme, 120, &config);
            let insert = rows.last().expect("insert row");
            insert.line.spans[insert.gutter_span_count..]
                .iter()
                .map(|span| {
                    (
                        span.style.fg.unwrap_or(ratatui::style::Color::Reset),
                        span.content.to_string(),
                    )
                })
                .collect()
        };

        let after_open_docstring = added_def("    \"\"\"Old docstring opener");
        let after_plain_code = added_def("    x = 1");
        assert_eq!(
            after_open_docstring, after_plain_code,
            "added line must be highlighted independently of the removed side",
        );
        // Under the bug the added line is one string span; the fix keeps it code.
        assert!(
            after_open_docstring.len() > 1,
            "added def line should be syntax highlighted"
        );
    }

    /// Build a DiffHunk spanning the closing `"""` through end of the fixture.
    fn fixture_python_close_hunk() -> (String, DiffHunk, usize) {
        let (file, close_ln) = fixture_python_parts();
        let all: Vec<&str> = file.lines().collect();
        let start = close_ln - 1; // 0-based
        let mut hunk = DiffHunk::new();
        for (i, line) in all[start..].iter().enumerate() {
            let ln = start + i + 1; // 1-based
            hunk.push(DiffLine {
                text: format!("{line}\n"),
                lo: ln,
                ln,
                tag: ChangeTag::Equal,
            });
        }
        let field_offset = all[start..]
            .iter()
            .position(|l| l.contains("notes") || l.contains("category_id"))
            .expect("field line in hunk");
        (file, hunk, start + field_offset + 1) // 1-based field ln
    }

    /// Fix pin: file-scoped field styles match full-file raw styles
    /// (and differ from cold hunk-only spill) after a mid-file closing `"""`.
    #[test]
    fn file_scoped_matches_full_file_on_field_line() {
        let _guard = pin_echo_agentnight_syntect();
        let path = Path::new("queue_item.py");
        let (file, hunk, field_ln) = fixture_python_close_hunk();
        let close_ln = hunk[0].ln;
        let hunk_lines: Vec<&str> = hunk.iter().map(|l| l.text.trim_end_matches('\n')).collect();
        let field_offset = field_ln - close_ln;

        let full = full_file_raw_styles(path, &file);
        let cold = hunk_only_raw_styles(path, &hunk_lines);
        let field_full = &full[field_ln - 1];
        let close_full = &full[close_ln - 1];
        assert_ne!(
            field_full, close_full,
            "full-file HL must not paint field line like the docstring closer"
        );
        assert_ne!(
            cold[field_offset], *field_full,
            "cold hunk-only must still spill vs full-file on field line"
        );

        assert!(
            file_text_within_hl_caps(&file),
            "fixture must be under HL caps"
        );
        let map = compute_file_scoped_styles(path, &file, std::slice::from_ref(&hunk))
            .expect("matching file+hunk must yield styles");
        let field_styles = map
            .get(&field_ln)
            .expect("file-scoped map must include field line");

        // Compare span texts to full-file raw; styles come from the same HL walk.
        let field_text: String = field_styles.iter().map(|(_, t)| t.as_str()).collect();
        let full_text: String = field_full.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(
            field_text, full_text,
            "file-scoped field text must equal full-file slice text"
        );
        // Span segmentation must match full-file (same syntect walk with correct state).
        let scoped_segs: Vec<&str> = field_styles.iter().map(|(_, t)| t.as_str()).collect();
        let full_segs: Vec<&str> = field_full.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(
            scoped_segs, full_segs,
            "file-scoped segmentation must match full-file on field line; \
             scoped={scoped_segs:?} full={full_segs:?}"
        );

        // Paint uses hunk text (not a disk rewrite).
        let field_hunk_text = hunk_lines[field_offset].to_string();
        let mut block = EditToolCallBlock::new("queue_item.py", vec![hunk]);
        block.highlight = EditHighlightPhase::FileScoped {
            by_new_line: Arc::new(map),
            theme: crate::theme::cache::current_kind(),
        };
        let out = block.output(&test_ctx());
        let joined: String = out
            .lines
            .iter()
            .map(|l| {
                l.content
                    .spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains(field_hunk_text.trim()),
            "FileScoped output must include hunk field text; got:\n{joined}"
        );
    }

    /// Pins both directions of the `render_diff_lines` theme gate with one
    /// paintable map: under its baking theme it repaints (positive control);
    /// baked under another theme it must not paint — hunk-only output.
    #[test]
    fn file_scoped_stale_theme_falls_back_to_hunk_only() {
        let _guard = pin_echo_agentnight_syntect();
        let path = Path::new("queue_item.py");
        let (file, hunk, field_ln) = fixture_python_close_hunk();
        // Real computed map: paintable by construction (line keys and texts
        // match the hunk).
        let map = compute_file_scoped_styles(path, &file, std::slice::from_ref(&hunk))
            .expect("matching file+hunk must yield styles");
        assert!(
            map.contains_key(&field_ln),
            "computed map must cover the field line"
        );
        let by_new_line = Arc::new(map);

        let mut block = EditToolCallBlock::new("queue_item.py", vec![hunk]);
        let hunk_only = block.output(&test_ctx());

        let render = |out: &BlockOutput| -> Vec<String> {
            out.lines
                .iter()
                .map(|l| {
                    l.content
                        .spans
                        .iter()
                        .map(|s| format!("{:?}:{}", s.style.fg, s.content))
                        .collect::<String>()
                })
                .collect()
        };

        // Positive control: under its baking theme the map must repaint the
        // spilled field line, so the stale assert below cannot go vacuous.
        block.highlight = EditHighlightPhase::FileScoped {
            by_new_line: Arc::clone(&by_new_line),
            theme: crate::theme::cache::current_kind(),
        };
        let fresh_out = block.output(&test_ctx());
        assert_ne!(
            render(&hunk_only),
            render(&fresh_out),
            "fresh-theme FileScoped must paint the map (differ from hunk-only)"
        );

        // Stale: the SAME paintable map baked under another theme is skipped.
        let stale = ThemeKind::EchoAgentDay;
        assert_ne!(stale, crate::theme::cache::current_kind());
        block.highlight = EditHighlightPhase::FileScoped {
            by_new_line,
            theme: stale,
        };
        let stale_out = block.output(&test_ctx());
        assert_eq!(
            render(&hunk_only),
            render(&stale_out),
            "stale-theme FileScoped must paint exactly like hunk-only"
        );
    }

    /// Disk line ≠ hunk line → compute refuses upgrade (no content swap).
    #[test]
    fn compute_file_scoped_rejects_disk_hunk_mismatch() {
        let path = Path::new("probe.py");
        let file = "x = 1\ny = 2\n";
        let hunk = vec![DiffLine {
            text: "x = 999\n".into(), // differs from disk line 1
            lo: 1,
            ln: 1,
            tag: ChangeTag::Insert,
        }];
        assert!(
            compute_file_scoped_styles(path, file, std::slice::from_ref(&hunk)).is_none(),
            "mismatch must refuse FileScoped"
        );
    }

    /// Caps reject oversized file text so the worker stays HunkOnly.
    #[test]
    fn file_text_caps_reject_huge_input() {
        let huge = "x\n".repeat(EDIT_HL_MAX_LINES + 1);
        assert!(!file_text_within_hl_caps(&huge));
        let ok = "x\n".repeat(10);
        assert!(file_text_within_hl_caps(&ok));
    }

    /// FileScoped paint expands tabs like the cold path.
    #[test]
    fn tabs_expanded_in_file_scoped_paint() {
        let hunk = go_tab_hunk();
        let file = "func main() {\n\tfmt.Println(\"hello\")\n}\n";
        let path = Path::new("main.go");
        let map = compute_file_scoped_styles(path, file, std::slice::from_ref(&hunk))
            .expect("matching go fixture");
        let theme = Theme::current();
        let config = DiffRenderConfig::default();
        let outputs = render_diff_hunks_with_styles(&[hunk], path, &map, &theme, 80, &config);
        assert_eq!(outputs.len(), 3);
        let line2 = line_to_string(&outputs[1].line);
        assert!(
            !line2.contains('\t'),
            "FileScoped must expand tabs, got: {line2:?}"
        );
        assert!(
            line2.contains("    fmt") || line2.contains("fmt"),
            "tab-expanded content expected, got: {line2:?}"
        );
    }

    /// Runnable pin: cold hunk-only and full-file disagree on the field line
    /// after a mid-file closing `"""`.
    #[test]
    fn triple_quote_hunk_only_differs_from_full_file_today() {
        let _guard = pin_echo_agentnight_syntect();
        let path = Path::new("queue_item.py");
        let (file, hunk, field_ln) = fixture_python_close_hunk();
        let close_ln = hunk[0].ln;
        let hunk_lines: Vec<&str> = hunk.iter().map(|l| l.text.trim_end_matches('\n')).collect();
        let field_offset = field_ln - close_ln;

        let cold = hunk_only_raw_styles(path, &hunk_lines);
        let full = full_file_raw_styles(path, &file);

        // Full-file: field line is not painted like the closing """ string line.
        let close_styles = &full[close_ln - 1];
        let field_full = &full[field_ln - 1];
        assert_ne!(
            field_full, close_styles,
            "full-file HL must not paint field line like the docstring closer; \
             field={field_full:?} close={close_styles:?}"
        );

        assert_ne!(
            cold[field_offset], *field_full,
            "expected cold-start mismatch (hunk-only string spill vs full-file); \
             if equal, the bug may already be fixed or the fixture no longer triggers"
        );
    }
}
