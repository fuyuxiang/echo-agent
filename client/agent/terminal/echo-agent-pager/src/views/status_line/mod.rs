//! Status row rendering: reserving the height, painting the lines, and handing
//! back the link spans. [`sanitize`] turns a script's bytes into those lines;
//! [`segments`] builds the `builtin` row's own text.

use std::borrow::Cow;
use std::sync::Arc;

use echo_agent_ratatui_inline::LinkSpan;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::theme::Theme;

mod sanitize;
mod segments;

use sanitize::CommandLink;
pub use sanitize::{MAX_STATUS_LINE_LINES, SanitizedText};
pub use segments::{SEGMENT_SEPARATOR, SegmentTone, StatusSegment, compose_builtin};

/// The clusters ratatui paints; it drops control-bearing and zero-width ones.
fn painted_clusters(text: &str) -> impl Iterator<Item = (&str, usize)> {
    text.graphemes(true)
        .filter(|cluster| !cluster.contains(char::is_control))
        .map(|cluster| (cluster, UnicodeWidthStr::width(cluster)))
        .filter(|(_, columns)| *columns > 0)
}

fn painted_width(text: &str) -> usize {
    painted_clusters(text).map(|(_, columns)| columns).sum()
}

/// The longest prefix fitting `budget` painted columns, the columns used, and
/// whether anything was left behind.
fn take_columns(text: &str, budget: usize) -> (String, usize, bool) {
    let mut used = 0usize;
    let mut kept = String::new();
    for (cluster, columns) in painted_clusters(text) {
        if used + columns > budget {
            return (kept, used, true);
        }
        used += columns;
        kept.push_str(cluster);
    }
    (kept, used, false)
}

/// The longest prefix fitting `columns`, ending in `…` when cut, or empty when
/// there is no room for even the marker.
fn fit_columns(text: &str, columns: usize) -> Cow<'_, str> {
    if painted_width(text) <= columns {
        return Cow::Borrowed(text);
    }
    if columns == 0 {
        return Cow::Borrowed("");
    }
    let (kept, _, _) = take_columns(text, columns - 1);
    Cow::Owned(format!("{kept}\u{2026}"))
}

/// Returns the line to paint and the columns of the original it kept, which is
/// what a link has to be clipped to: a link whose text was elided away would
/// otherwise land on the ellipsis and make it clickable.
fn elide<'a>(line: &Line<'a>, width: usize) -> (Line<'a>, usize) {
    if painted_line_width(line) <= width {
        return (line.clone(), width);
    }
    if width == 0 {
        return (Line::default(), 0);
    }
    let budget = width - 1;
    let mut used = 0usize;
    let mut spans: Vec<Span<'a>> = Vec::with_capacity(line.spans.len() + 1);
    for span in &line.spans {
        let remaining = budget - used;
        if remaining == 0 {
            break;
        }
        let (text, columns, truncated) = take_columns(span.content.as_ref(), remaining);
        used += columns;
        if !text.is_empty() {
            spans.push(Span::styled(text, span.style));
        }
        // A span after a cut one would drop the glyphs in between silently.
        if truncated {
            break;
        }
    }
    debug_assert!(used <= budget, "spent {used} of {budget} columns");
    // The marker replaces text, so it takes that text's style.
    let marker = spans.last().map_or(line.style, |span| span.style);
    spans.push(Span::styled("…", marker));
    (
        Line {
            spans,
            style: line.style,
            alignment: line.alignment,
        },
        used,
    )
}

#[derive(Debug, Clone, PartialEq)]
pub enum StatusLineDisplay {
    Segments(Vec<StatusSegment>),
    Text(SanitizedText),
}

/// The space one frame gives the row, and what goes in it. Three states so
/// the height and the paint cannot disagree about whether the row is there.
#[derive(Debug, Clone, Default)]
pub enum StatusLineFrame {
    #[default]
    Off,
    /// Not answered for yet. Holds a line so the first result does not shove
    /// the input box up.
    Reserved { padding: u16 },
    On {
        display: Arc<StatusLineDisplay>,
        padding: u16,
    },
}

impl StatusLineFrame {
    pub fn height(&self) -> u16 {
        match self {
            StatusLineFrame::Off => 0,
            StatusLineFrame::Reserved { .. } => 1,
            StatusLineFrame::On { display, .. } => display.line_count(),
        }
    }

    /// The inset, for a row that takes space at all. A reserved row has one
    /// too: it sizes a script's first run.
    pub fn padding(&self) -> Option<u16> {
        match self {
            StatusLineFrame::Off => None,
            StatusLineFrame::Reserved { padding } | StatusLineFrame::On { padding, .. } => {
                Some(*padding)
            }
        }
    }

    pub fn display(&self) -> Option<&Arc<StatusLineDisplay>> {
        match self {
            StatusLineFrame::Off | StatusLineFrame::Reserved { .. } => None,
            StatusLineFrame::On { display, .. } => Some(display),
        }
    }
}

impl StatusLineDisplay {
    /// What the row asks the layout to reserve. A frame with less space to give
    /// paints fewer.
    pub fn line_count(&self) -> u16 {
        match self {
            StatusLineDisplay::Segments(_) => 1,
            StatusLineDisplay::Text(output) => output.line_count(),
        }
    }
}

#[must_use]
pub fn render_status_line(
    buf: &mut Buffer,
    area: Rect,
    display: &StatusLineDisplay,
    padding: u16,
    theme: &Theme,
) -> Vec<LinkSpan> {
    if area.width == 0 || area.height == 0 {
        return Vec::new();
    }
    // The fill covers the padding too, so the row reads as one band.
    buf.set_style(area, Style::default().bg(theme.bg_base));
    let Some(width) = inner_width(area.width, padding) else {
        return Vec::new();
    };
    let area = Rect {
        x: area.x + padding,
        width,
        ..area
    };

    let (lines, links) = styled_lines(display, theme);
    let mut kept = Vec::with_capacity(lines.len());
    for (offset, line) in lines.iter().take(area.height as usize).enumerate() {
        // `set_line` clips silently, turning `100% ctx` into `10`, so mark the cut.
        let (line, columns) = elide(line, width as usize);
        kept.push(columns);
        buf.set_line(area.x, area.y + offset as u16, &line, width);
    }
    project_links(area, links, &kept)
}

/// The row a `command` script is told it has, in `COLUMNS` and `LINES`. Named
/// fields rather than a tuple: the pair crosses three modules, and a silent
/// transposition sizes the script to one column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowSize {
    pub cols: u16,
    pub lines: u16,
}

impl RowSize {
    /// What a script is told when the row has never painted. A script divides
    /// by its width, so this is a plausible terminal rather than zero.
    pub const FALLBACK: Self = Self { cols: 80, lines: 1 };
}

/// The columns left for the row's own content, or `None` when the padding
/// leaves none. A script's `COLUMNS` is this same number.
pub(crate) fn inner_width(width: u16, padding: u16) -> Option<u16> {
    let inset = padding.saturating_mul(2);
    (inset < width).then(|| width - inset)
}

fn styled_lines<'a>(
    display: &'a StatusLineDisplay,
    theme: &Theme,
) -> (Vec<Line<'a>>, Vec<CommandLink>) {
    match display {
        StatusLineDisplay::Segments(segs) => {
            let separator = theme.dim();
            let mut spans: Vec<Span<'_>> = Vec::with_capacity(segs.len() * 2);
            for (idx, seg) in segs.iter().enumerate() {
                if idx > 0 {
                    spans.push(Span::styled(SEGMENT_SEPARATOR, separator));
                }
                let style = match seg.tone {
                    SegmentTone::Dim => theme.muted(),
                    SegmentTone::Warn => theme.fg(theme.warning),
                };
                spans.push(Span::styled(seg.text.as_str(), style));
            }
            (vec![Line::from(spans)], Vec::new())
        }
        StatusLineDisplay::Text(output) => (
            output
                .lines
                .iter()
                .map(|line| themed(line, theme))
                .collect(),
            output.links.clone(),
        ),
    }
}

fn themed<'a>(line: &'a Line<'static>, theme: &Theme) -> Line<'a> {
    // Muted, matching the `builtin` row: chrome reads below the conversation,
    // but no quieter than the shortcut hints under it. Text that wants a colour
    // carries one.
    let base = theme.muted();
    let spans = line.spans.iter().map(|span| {
        let mut style = span.style;
        if matches!(style.fg, None | Some(ratatui::style::Color::Reset)) {
            style = style.patch(base);
        }
        // Strip blink and hidden: a row that blinks or hides its own text is
        // one the user cannot read.
        style = style.remove_modifier(
            ratatui::style::Modifier::SLOW_BLINK
                | ratatui::style::Modifier::RAPID_BLINK
                | ratatui::style::Modifier::HIDDEN,
        );
        // Quantized like every other ANSI-bearing surface, so a script's colour
        // obeys `NO_COLOR` and a terminal locked to 16 colours. Before the
        // background fallback, which supplies a theme colour that already is.
        style.fg = style.fg.map(echo_agent_pager_render::theme::quantize);
        style.bg = style.bg.map(echo_agent_pager_render::theme::quantize);
        // `\x1b[0m`, which every powerline script emits, parses to an explicit
        // `Color::Reset` background that seams against the bar below.
        if matches!(style.bg, None | Some(ratatui::style::Color::Reset)) {
            style = style.bg(theme.bg_base);
        }
        Span::styled(span.content.as_ref(), style)
    });
    Line::from(spans.collect::<Vec<_>>()).style(line.style)
}

/// The columns ratatui paints. `Line::width` counts 1 for a control character
/// the painter drops, measuring CRLF output a column too wide.
fn painted_line_width(line: &Line<'_>) -> usize {
    line.spans
        .iter()
        .map(|span| painted_width(span.content.as_ref()))
        .sum()
}

fn project_links(area: Rect, links: Vec<CommandLink>, kept: &[usize]) -> Vec<LinkSpan> {
    let max_x = area.x.saturating_add(area.width);
    links
        .into_iter()
        .filter(|l| l.line < area.height)
        .filter_map(|l| {
            // Clipped to the text the elision kept, not to the row's width: the
            // ellipsis sits inside the width and is not part of any link.
            let kept = u16::try_from(*kept.get(l.line as usize)?).unwrap_or(u16::MAX);
            let col_start = area.x.saturating_add(l.col_start.min(kept));
            let col_end = area.x.saturating_add(l.col_end.min(kept)).min(max_x);
            (col_start < max_x && col_end > col_start).then(|| LinkSpan {
                row: area.y.saturating_add(l.line),
                col_start,
                col_end,
                url: Arc::clone(&l.url),
                id: None,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests;
