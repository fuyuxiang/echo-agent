//! Markdown renderer - transforms parsed markdown buffers into styled output.
//!
//! After parsing with `MarkdownParser`, use `ParsedMarkdown` to render
//! to either ratatui Lines or ANSI strings.

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::fmt::Write as FmtWrite;
use std::ops::Range;

use anstyle::{Effects, Reset, Style};
use ratatui::text::{Line, Span};
use syntect::highlighting::Style as SyntectStyle;

use crate::buffers::{MarkdownBuffers, RenderEvent, RenderEventKind, unicode_display_width};
use crate::checkpoint::Checkpoint;
use crate::colors::adapt_style;
use crate::hyperlinks::{ChunkLinkRange, chunk_link_offsets, emit_segment_hyperlinks};
use crate::output::{HyperlinkTarget, MarkdownRenderOutput};
use crate::parse::ParsedMarkdown;
use crate::source_map::SourceMap;
use crate::style::{all_hidden, merge_styles};

/// Trait for converting anstyle to ratatui style.
trait StyleInto<T> {
    fn style_into(self) -> T;
}

impl StyleInto<ratatui::style::Style> for Style {
    fn style_into(self) -> ratatui::style::Style {
        use ratatui::style::{Modifier, Style as RStyle};

        let mut style = RStyle::default();

        if let Some(fg) = self.get_fg_color() {
            style = style.fg(anstyle_to_ratatui_color(fg));
        }
        if let Some(bg) = self.get_bg_color() {
            style = style.bg(anstyle_to_ratatui_color(bg));
        }

        let effects = self.get_effects();
        let mut modifiers = Modifier::empty();
        if effects.contains(Effects::BOLD) {
            modifiers |= Modifier::BOLD;
        }
        if effects.contains(Effects::DIMMED) {
            modifiers |= Modifier::DIM;
        }
        if effects.contains(Effects::ITALIC) {
            modifiers |= Modifier::ITALIC;
        }
        if effects.contains(Effects::UNDERLINE) {
            modifiers |= Modifier::UNDERLINED;
        }
        if effects.contains(Effects::STRIKETHROUGH) {
            modifiers |= Modifier::CROSSED_OUT;
        }
        if effects.contains(Effects::HIDDEN) {
            modifiers |= Modifier::HIDDEN;
        }

        style.add_modifier(modifiers)
    }
}

fn anstyle_to_ratatui_color(color: anstyle::Color) -> ratatui::style::Color {
    use ratatui::style::Color;
    match color {
        anstyle::Color::Ansi(ansi) => match ansi {
            anstyle::AnsiColor::Black => Color::Black,
            anstyle::AnsiColor::Red => Color::Red,
            anstyle::AnsiColor::Green => Color::Green,
            anstyle::AnsiColor::Yellow => Color::Yellow,
            anstyle::AnsiColor::Blue => Color::Blue,
            anstyle::AnsiColor::Magenta => Color::Magenta,
            anstyle::AnsiColor::Cyan => Color::Cyan,
            anstyle::AnsiColor::White => Color::Gray,
            anstyle::AnsiColor::BrightBlack => Color::DarkGray,
            anstyle::AnsiColor::BrightRed => Color::LightRed,
            anstyle::AnsiColor::BrightGreen => Color::LightGreen,
            anstyle::AnsiColor::BrightYellow => Color::LightYellow,
            anstyle::AnsiColor::BrightBlue => Color::LightBlue,
            anstyle::AnsiColor::BrightMagenta => Color::LightMagenta,
            anstyle::AnsiColor::BrightCyan => Color::LightCyan,
            anstyle::AnsiColor::BrightWhite => Color::White,
        },
        anstyle::Color::Ansi256(idx) => Color::Indexed(idx.index()),
        anstyle::Color::Rgb(rgb) => Color::Rgb(rgb.0, rgb.1, rgb.2),
    }
}

/// Render raw highlighted spans to an ANSI string.
fn render_replace_ansi(highlighted: &[Vec<(SyntectStyle, String)>]) -> String {
    let mut out = String::new();
    for line_spans in highlighted {
        for (style, text) in line_spans {
            if text.is_empty() {
                continue;
            }
            let full_style = anstyle_syntect::to_anstyle(*style);
            let fg_only = full_style.bg_color(None);
            let adapted = adapt_style(fg_only);
            if adapted != Style::new() {
                write!(out, "{adapted}{text}\x1b[0m").ok();
            } else {
                out.push_str(text);
            }
        }
    }
    out
}

/// Stylize trait for ANSI rendering.
trait Stylize {
    fn astyle(&self, style: Style) -> StyledStr<'_>;
}

impl Stylize for str {
    fn astyle(&self, style: Style) -> StyledStr<'_> {
        StyledStr { text: self, style }
    }
}

struct StyledStr<'a> {
    text: &'a str,
    style: Style,
}

impl<'a> std::fmt::Display for StyledStr<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.style.is_plain() {
            write!(f, "{}", self.text)
        } else {
            write!(f, "{}{}\x1b[0m", self.style, self.text)
        }
    }
}

impl<'a, 'b> ParsedMarkdown<'a, 'b> {
    fn apply_transforms<'t>(&self, text: &'t str, start: usize, pretty: bool) -> Cow<'t, str> {
        if self.buffers.transforms.is_empty() {
            return Cow::Borrowed(text);
        }
        // Raw mode applies only `force` transforms (e.g. soft-break collapse).
        if !pretty && !self.buffers.transforms.iter().any(|t| t.force) {
            return Cow::Borrowed(text);
        }

        let end = start + text.len();
        let mut result = String::new();
        let mut pos = start;
        let mut applied = false;

        for transform in &self.buffers.transforms {
            if transform.range.end <= start || transform.range.start >= end {
                continue;
            }
            if !pretty && !transform.force {
                continue;
            }
            applied = true;
            // Clamp transform range to our text range
            let t_start = transform.range.start.max(start);
            let t_end = transform.range.end.min(end);

            // Copy text before transform
            if t_start > pos {
                let before = &text[(pos - start)..(t_start - start)];
                result.push_str(before);
            }

            // Apply transform
            result.push_str(&transform.to);

            pos = t_end;
        }

        if !applied {
            Cow::Borrowed(text)
        } else {
            // Copy remaining text
            if pos < end {
                result.push_str(&text[(pos - start)..]);
            }
            Cow::Owned(result)
        }
    }

    /// Build sorted render events into the provided Vec.
    fn build_render_events_into(&self, events: &mut Vec<RenderEvent>) {
        events.clear();
        let capacity = self.buffers.highlights.len() * 2
            + self.buffers.replaces.len() * 2
            + self.buffers.table_replaces.len() * 2
            + self.buffers.mermaid_replaces.len() * 2;
        events.reserve(capacity);

        for (i, hl) in self.buffers.highlights.iter().enumerate() {
            events.push(RenderEvent {
                pos: hl.range.start,
                kind: RenderEventKind::Highlight,
                index: i,
                is_end: false,
            });
            events.push(RenderEvent {
                pos: hl.range.end,
                kind: RenderEventKind::Highlight,
                index: i,
                is_end: true,
            });
        }
        for (i, r) in self.buffers.replaces.iter().enumerate() {
            events.push(RenderEvent {
                pos: r.range.start,
                kind: RenderEventKind::Replace,
                index: i,
                is_end: false,
            });
            events.push(RenderEvent {
                pos: r.range.end,
                kind: RenderEventKind::Replace,
                index: i,
                is_end: true,
            });
        }
        for (i, t) in self.buffers.table_replaces.iter().enumerate() {
            events.push(RenderEvent {
                pos: t.range.start,
                kind: RenderEventKind::Table,
                index: i,
                is_end: false,
            });
            events.push(RenderEvent {
                pos: t.range.end,
                kind: RenderEventKind::Table,
                index: i,
                is_end: true,
            });
        }
        for (i, m) in self.buffers.mermaid_replaces.iter().enumerate() {
            events.push(RenderEvent {
                pos: m.range.start,
                kind: RenderEventKind::Mermaid,
                index: i,
                is_end: false,
            });
            events.push(RenderEvent {
                pos: m.range.end,
                kind: RenderEventKind::Mermaid,
                index: i,
                is_end: true,
            });
        }
        events.sort_unstable();
    }

    /// Build sorted render events into a new Vec.
    fn build_render_events(&self) -> Vec<RenderEvent> {
        let mut events = Vec::new();
        self.build_render_events_into(&mut events);
        events
    }

    /// Render to ANSI-styled string.
    ///
    /// If `pretty` is true, syntax markers are hidden.
    /// Returns the rendered string and a source map for copy-paste support.
    pub fn render_ansi(&mut self, pretty: bool) -> (String, SourceMap) {
        let events = self.build_render_events();

        // Apply force transforms in place over a copy of `self.text` so
        // the ANSI path picks them up without restructuring `push`. See
        // `Transform::force` for the byte-length invariant.
        let text_owned: Option<String> = if self.buffers.transforms.iter().any(|t| t.force) {
            let mut bytes = self.text.as_bytes().to_vec();
            for t in &self.buffers.transforms {
                if !t.force {
                    continue;
                }
                debug_assert_eq!(
                    t.to.len(),
                    t.range.end - t.range.start,
                    "force transforms must preserve byte length",
                );
                debug_assert!(
                    self.text.is_char_boundary(t.range.start)
                        && self.text.is_char_boundary(t.range.end),
                    "force transform range must align with char boundaries",
                );
                bytes[t.range.clone()].copy_from_slice(t.to.as_bytes());
            }
            Some(String::from_utf8(bytes).expect("force transforms preserve UTF-8"))
        } else {
            None
        };
        let view_text: &str = text_owned.as_deref().unwrap_or(self.text);

        let mut out = String::with_capacity(view_text.len() * 2);
        let mut source_map = SourceMap::new();
        let mut rendered_offset = 0;
        let mut hl_ids = BTreeSet::<usize>::new();
        let mut last_pos = 0;
        let mut replace: Option<usize> = None;
        let mut table_replace: Option<usize> = None;
        let mut mermaid_replace: Option<usize> = None;
        let mut current = (0..0, Style::new());

        fn push(
            out: &mut String,
            current: &mut (Range<usize>, Style),
            text: &str,
            range: Range<usize>,
            style: Style,
            source_map: &mut SourceMap,
            rendered_offset: &mut usize,
        ) {
            let (crange, cstyle) = current;
            let ctext = &text[crange.clone()];
            if !range.is_empty() && style == *cstyle {
                if !ctext.is_empty() {
                    debug_assert_eq!(crange.end, range.start);
                }
                crange.end = range.end;
                return;
            }
            if !ctext.is_empty() {
                source_map.add(*rendered_offset, crange.clone());
                *rendered_offset += ctext.len();

                if cstyle.is_plain() {
                    out.push_str(ctext);
                } else {
                    out.push_str(&ctext.astyle(*cstyle).to_string());
                }
            }
            *crange = range;
            *cstyle = style;
        }

        for ev in &events {
            if replace.is_none()
                && table_replace.is_none()
                && mermaid_replace.is_none()
                && ev.pos > last_pos
            {
                let should_skip =
                    pretty && all_hidden(hl_ids.iter().map(|&i| self.buffers.highlights[i].style));

                if should_skip {
                    push(
                        &mut out,
                        &mut current,
                        view_text,
                        ev.pos..ev.pos,
                        Style::new(),
                        &mut source_map,
                        &mut rendered_offset,
                    );
                } else {
                    let mut style =
                        merge_styles(hl_ids.iter().map(|&i| self.buffers.highlights[i].style));
                    let text = &view_text[last_pos..ev.pos];
                    let is_invert = style.get_effects().contains(Effects::INVERT);
                    if text.as_bytes().iter().all(|&ch| ch == b'\n')
                        || (text.as_bytes().iter().all(u8::is_ascii_whitespace)
                            && ((!is_invert && style.get_bg_color().is_none())
                                || (is_invert && style.get_fg_color().is_none())))
                    {
                        style = Style::new();
                    }
                    push(
                        &mut out,
                        &mut current,
                        view_text,
                        last_pos..ev.pos,
                        style,
                        &mut source_map,
                        &mut rendered_offset,
                    );
                }
                last_pos = ev.pos;
            }

            match ev.kind {
                RenderEventKind::Replace => {
                    if ev.is_end && replace == Some(ev.index) {
                        replace = None;
                        out.push_str(&Reset.to_string());
                    } else if !ev.is_end && replace.is_none() && table_replace.is_none() {
                        replace = Some(ev.index);
                        push(
                            &mut out,
                            &mut current,
                            view_text,
                            ev.pos..ev.pos,
                            Style::new(),
                            &mut source_map,
                            &mut rendered_offset,
                        );
                        out.push_str(&Reset.to_string());

                        let repl = &self.buffers.replaces[ev.index];
                        let ansi_content = render_replace_ansi(&repl.highlighted);

                        let replace_text_len: usize = repl
                            .highlighted
                            .iter()
                            .flat_map(|line| line.iter().map(|(_, t)| t.len()))
                            .sum();
                        source_map.add(rendered_offset, repl.range.clone());
                        rendered_offset += replace_text_len;

                        out.push_str(&ansi_content);
                        last_pos = repl.range.end;
                    }
                }
                RenderEventKind::Table => {
                    if ev.is_end && table_replace == Some(ev.index) {
                        table_replace = None;
                    } else if !ev.is_end && table_replace.is_none() && pretty {
                        table_replace = Some(ev.index);
                        push(
                            &mut out,
                            &mut current,
                            view_text,
                            ev.pos..ev.pos,
                            Style::new(),
                            &mut source_map,
                            &mut rendered_offset,
                        );

                        let trepl = &self.buffers.table_replaces[ev.index];
                        // Block lines must start at a line boundary; a
                        // display-math replacement can occur mid-paragraph.
                        // Styled chunks end with a reset sequence after the
                        // newline, so check both forms.
                        let at_line_start =
                            out.is_empty() || out.ends_with('\n') || out.ends_with("\n\x1b[0m");
                        if !at_line_start {
                            out.push('\n');
                            rendered_offset += 1;
                        }
                        for line in &trepl.lines {
                            out.push_str(line);
                            out.push('\n');
                            rendered_offset += line.len() + 1;
                        }
                        last_pos = trepl.range.end;
                        // Advance `current` past the table so trailing text
                        // doesn't merge back to the pre-table position.
                        current.0 = trepl.range.end..trepl.range.end;
                    }
                }
                RenderEventKind::Mermaid => {
                    if ev.is_end && mermaid_replace == Some(ev.index) {
                        mermaid_replace = None;
                    } else if !ev.is_end && mermaid_replace.is_none() && pretty {
                        mermaid_replace = Some(ev.index);
                        push(
                            &mut out,
                            &mut current,
                            view_text,
                            ev.pos..ev.pos,
                            Style::new(),
                            &mut source_map,
                            &mut rendered_offset,
                        );

                        let mrepl = &self.buffers.mermaid_replaces[ev.index];
                        for line in &mrepl.lines {
                            out.push_str(line);
                            out.push('\n');
                            rendered_offset += line.len() + 1;
                        }
                        last_pos = mrepl.range.end;
                        current.0 = mrepl.range.end..mrepl.range.end;
                    }
                }
                RenderEventKind::Highlight => {
                    if ev.is_end {
                        hl_ids.remove(&ev.index);
                    } else {
                        hl_ids.insert(ev.index);
                    }
                }
            }
        }

        let len = view_text.len();
        if last_pos < len {
            push(
                &mut out,
                &mut current,
                view_text,
                last_pos..len,
                Style::new(),
                &mut source_map,
                &mut rendered_offset,
            );
        }
        push(
            &mut out,
            &mut current,
            view_text,
            len..len,
            Style::new(),
            &mut source_map,
            &mut rendered_offset,
        );
        (out, source_map)
    }

    /// Render to ratatui Lines.
    ///
    /// If `pretty` is true, syntax markers are hidden.
    /// Returns rendered lines, line source map, and optional checkpoint.
    pub fn render_ratatui(&mut self, pretty: bool) -> (MarkdownRenderOutput, Option<Checkpoint>) {
        // Build render events
        let render_events = self.build_render_events();

        self.buffers.current_spans.clear();
        self.buffers.active_highlights.clear();

        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut line_source_map: Vec<usize> = Vec::new();
        let mut hyperlinks: Vec<HyperlinkTarget> = Vec::new();

        let mut last_pos = 0;
        let mut replace: Option<usize> = None;
        let mut table_replace: Option<usize> = None;
        let mut mermaid_replace: Option<usize> = None;
        let mut skip_leading_newline = false;
        let mut in_hidden_code_block = false;
        let mut next_link_idx: usize = 0;
        // Running display-column tracker for the in-progress line.
        let mut cur_col_in_line: usize = 0;

        let checkpoint_info = self.last_checkpoint;
        let mut checkpoint_output_lines: Option<usize> = None;

        // Style already adapted - no need to call adapt_style again
        let code_bg_style: ratatui::style::Style = self.ms.code_background.style_into();

        let in_untagged_code = |pos: usize, buffers: &MarkdownBuffers| -> bool {
            buffers
                .untagged_code_ranges
                .iter()
                .any(|range| pos >= range.start && pos < range.end)
        };

        let mut current_source_line = 0usize;
        let mut last_line_count_pos = 0usize;
        let mut pending_line_is_code = false;

        let count_newlines_in_range = |from: usize, to: usize, text: &str| -> usize {
            if to <= from {
                return 0;
            }
            let to = to.min(text.len());
            let from = from.min(to);
            // Use as_bytes() to avoid panicking on non-char-boundary offsets.
            // This is safe because '\n' (0x0A) is a single-byte ASCII value
            // that can never appear as a UTF-8 continuation byte (0x80..0xBF).
            text.as_bytes()[from..to]
                .iter()
                .filter(|&&b| b == b'\n')
                .count()
        };

        for ev in &render_events {
            if replace.is_none()
                && table_replace.is_none()
                && mermaid_replace.is_none()
                && ev.pos > last_pos
            {
                // Check if we need to split text processing at the checkpoint boundary.
                // If last_pos < cp_byte <= ev.pos, we process in two parts:
                // 1. Process [last_pos..cp_byte], capture lines.len(), process [cp_byte..ev.pos]
                let split_at_checkpoint = checkpoint_output_lines.is_none()
                    && checkpoint_info
                        .map(|(_, cp_byte)| last_pos < cp_byte && cp_byte <= ev.pos)
                        .unwrap_or(false);

                let cp_byte = checkpoint_info.map(|(_, cp)| cp).unwrap_or(0);

                // Snap cp_byte to the nearest char boundary.  Checkpoint byte
                // offsets come from pulldown-cmark event ranges which should
                // always be char-aligned, but in edge cases (e.g., thematic
                // breaks followed by headings with multi-byte chars) the
                // position can land mid-character.  Snapping forward is safe
                // because it only affects where we split the text for line
                // counting — a few extra or fewer newlines in the first vs
                // second range doesn't change the total count.
                let cp_byte = {
                    let mut b = cp_byte;
                    while b < self.text.len() && !self.text.is_char_boundary(b) {
                        b += 1;
                    }
                    b
                };

                // Determine ranges to process
                let ranges: &[(usize, usize)] = if split_at_checkpoint {
                    // Process in two parts, capturing checkpoint between them
                    &[(last_pos, cp_byte), (cp_byte, ev.pos)]
                } else {
                    // Process as single range
                    &[(last_pos, ev.pos)]
                };

                for (range_idx, &(range_start, range_end)) in ranges.iter().enumerate() {
                    // After processing the first range when splitting, capture checkpoint.
                    // Flush any pending spans to `lines` first — content like a thematic
                    // break (`───`) may sit in `current_spans` without a trailing newline
                    // to flush it.  Without this flush, the checkpoint's `output_lines`
                    // count would be too low, causing the line to vanish on re-render.
                    if split_at_checkpoint && range_idx == 1 {
                        if !self.buffers.current_spans.is_empty() {
                            line_source_map.push(current_source_line);
                            let line = Line::from(std::mem::take(&mut self.buffers.current_spans));
                            lines.push(line);
                            cur_col_in_line = 0;
                        }
                        checkpoint_output_lines = Some(lines.len());
                    }

                    if range_end <= range_start {
                        continue;
                    }

                    // Update source line counter
                    if range_start > last_line_count_pos {
                        current_source_line +=
                            count_newlines_in_range(last_line_count_pos, range_start, self.text);
                        last_line_count_pos = range_start;
                    }

                    let is_hidden = pretty
                        && all_hidden(
                            self.buffers
                                .active_highlights
                                .iter()
                                .map(|&i| self.buffers.highlights[i].style),
                        );

                    if is_hidden {
                        let at_line_start = range_start == 0
                            || self.text.as_bytes().get(range_start - 1) == Some(&b'\n');
                        if at_line_start {
                            // Check if this hidden block is a code fence (``` or ~~~).
                            // Only code fences need separator handling — heading markers
                            // (#) are also hidden at line start but are unpaired.
                            let hidden_text = self.text[range_start..range_end].trim_start();
                            let is_code_fence =
                                hidden_text.starts_with("```") || hidden_text.starts_with("~~~");

                            if is_code_fence {
                                // Emit a blank separator before an OPENING fence (not
                                // closing). Prevents adjacent blocks (e.g., list → code)
                                // from collapsing their visual boundary when the hidden
                                // fence markers are removed in pretty mode.
                                if !in_hidden_code_block
                                    && lines.last().is_some_and(|l| l.width() > 0)
                                {
                                    line_source_map.push(current_source_line);
                                    lines.push(Line::default());
                                    cur_col_in_line = 0;
                                }
                                in_hidden_code_block = !in_hidden_code_block;
                            }
                            skip_leading_newline = true;
                        }
                    } else {
                        let mut text = &self.text[range_start..range_end];
                        let mut text_start = range_start;

                        if skip_leading_newline && text.starts_with('\n') {
                            text = &text[1..];
                            text_start += 1;
                        }
                        skip_leading_newline = false;

                        if !text.is_empty() {
                            let style = merge_styles(
                                self.buffers
                                    .active_highlights
                                    .iter()
                                    .map(|&i| self.buffers.highlights[i].style),
                            );

                            let transformed = self.apply_transforms(text, range_start, pretty);
                            let ratatui_style: ratatui::style::Style = style.style_into();

                            let chunk_src_start = text_start;
                            let chunk_src_end = text_start + text.len();

                            // Advance the cursor past links that ended before
                            // this chunk starts, then check if any remaining
                            // link overlaps the chunk.  Skip all hyperlink
                            // bookkeeping when none does — keeps the no-link
                            // hot path identical to the pre-feature renderer.
                            while next_link_idx < self.buffers.link_targets.len()
                                && self.buffers.link_targets[next_link_idx].source_range.end
                                    <= chunk_src_start
                            {
                                next_link_idx += 1;
                            }
                            let chunk_has_links = next_link_idx < self.buffers.link_targets.len()
                                && self.buffers.link_targets[next_link_idx].source_range.start
                                    < chunk_src_end;

                            let chunk_links: Vec<ChunkLinkRange> = if chunk_has_links {
                                chunk_link_offsets(
                                    &self.buffers.link_targets,
                                    next_link_idx,
                                    chunk_src_start,
                                    chunk_src_end,
                                    pretty,
                                    &self.buffers.transforms,
                                )
                            } else {
                                Vec::new()
                            };

                            let mut byte_offset = text_start;
                            let mut seg_x_offset: usize = 0;
                            let is_in_code = in_untagged_code(text_start, self.buffers);
                            pending_line_is_code = is_in_code;
                            for (idx, segment) in transformed.split('\n').enumerate() {
                                if idx > 0 {
                                    line_source_map.push(current_source_line);
                                    let line =
                                        Line::from(std::mem::take(&mut self.buffers.current_spans));
                                    lines.push(if is_in_code {
                                        line.style(code_bg_style)
                                    } else {
                                        line
                                    });
                                    if byte_offset > last_line_count_pos {
                                        current_source_line += count_newlines_in_range(
                                            last_line_count_pos,
                                            byte_offset,
                                            self.text,
                                        );
                                        last_line_count_pos = byte_offset;
                                    }
                                    cur_col_in_line = 0;
                                }

                                if !chunk_links.is_empty() {
                                    emit_segment_hyperlinks(
                                        &chunk_links,
                                        &self.buffers.link_targets,
                                        segment,
                                        seg_x_offset,
                                        cur_col_in_line,
                                        lines.len(),
                                        &mut hyperlinks,
                                    );
                                }

                                if !segment.is_empty() {
                                    self.buffers
                                        .current_spans
                                        .push(Span::styled(segment.to_string(), ratatui_style));
                                    cur_col_in_line += unicode_display_width(segment);
                                }
                                byte_offset += segment.len() + 1;
                                seg_x_offset += segment.len() + 1;
                            }
                        }
                    }
                }
                last_pos = ev.pos;
            }

            match ev.kind {
                RenderEventKind::Replace => {
                    if ev.is_end && replace == Some(ev.index) {
                        replace = None;
                    } else if !ev.is_end && replace.is_none() && table_replace.is_none() {
                        replace = Some(ev.index);
                        let repl = &self.buffers.replaces[ev.index];

                        // Update source line to code start
                        if repl.range.start > last_line_count_pos {
                            current_source_line += count_newlines_in_range(
                                last_line_count_pos,
                                repl.range.start,
                                self.text,
                            );
                        }
                        let code_start_source_line = current_source_line;

                        for (line_idx, line_spans) in repl.highlighted.iter().enumerate() {
                            current_source_line = code_start_source_line + line_idx;

                            for (syn_style, text) in line_spans {
                                let full_style = anstyle_syntect::to_anstyle(*syn_style);
                                let with_bg =
                                    full_style.bg_color(self.ms.code_background.get_bg_color());
                                // This is the only legitimate inline adapt_style call
                                // for dynamically created syntect+background combo
                                let adapted = adapt_style(with_bg);
                                let ratatui_style: ratatui::style::Style = adapted.style_into();

                                for (idx, segment) in text.split('\n').enumerate() {
                                    if idx > 0 {
                                        line_source_map.push(current_source_line);
                                        let line = Line::from(std::mem::take(
                                            &mut self.buffers.current_spans,
                                        ))
                                        .style(code_bg_style);
                                        lines.push(line);
                                        current_source_line += 1;
                                        cur_col_in_line = 0;
                                    }
                                    if !segment.is_empty() {
                                        self.buffers
                                            .current_spans
                                            .push(Span::styled(segment.to_string(), ratatui_style));
                                        cur_col_in_line += unicode_display_width(segment);
                                    }
                                }
                            }

                            if !self.buffers.current_spans.is_empty() {
                                line_source_map.push(current_source_line);
                                let line =
                                    Line::from(std::mem::take(&mut self.buffers.current_spans))
                                        .style(code_bg_style);
                                lines.push(line);
                                cur_col_in_line = 0;
                            }
                        }

                        last_pos = repl.range.end;
                        let newlines_in_code =
                            count_newlines_in_range(repl.range.start, repl.range.end, self.text);
                        current_source_line = code_start_source_line + newlines_in_code;
                        last_line_count_pos = repl.range.end;

                        if checkpoint_output_lines.is_none()
                            && let Some((_, cp_byte)) = checkpoint_info
                            && last_pos >= cp_byte
                        {
                            checkpoint_output_lines = Some(lines.len());
                        }
                    }
                }
                RenderEventKind::Table => {
                    if ev.is_end && table_replace == Some(ev.index) {
                        table_replace = None;
                    } else if !ev.is_end && table_replace.is_none() && pretty {
                        table_replace = Some(ev.index);
                        let trepl = &self.buffers.table_replaces[ev.index];

                        // Flush any in-progress inline spans first. Tables
                        // always start at a line boundary (no-op), but a
                        // display-math block replacement can occur
                        // mid-paragraph (`text $$x$$ more`): without the
                        // flush, the pending "text " spans would be emitted
                        // AFTER the block lines.
                        if !self.buffers.current_spans.is_empty() {
                            line_source_map.push(current_source_line);
                            lines.push(Line::from(std::mem::take(&mut self.buffers.current_spans)));
                            // cur_col_in_line is reset unconditionally after
                            // the block lines are emitted below.
                        }

                        // Update source line to table start
                        if trepl.range.start > last_line_count_pos {
                            current_source_line += count_newlines_in_range(
                                last_line_count_pos,
                                trepl.range.start,
                                self.text,
                            );
                        }
                        let table_start_source_line = current_source_line;
                        let table_base_line = lines.len();

                        for (line_idx, styled_line) in trepl.styled_lines.iter().enumerate() {
                            let offset = trepl
                                .line_source_offsets
                                .get(line_idx)
                                .copied()
                                .unwrap_or(0);
                            current_source_line = table_start_source_line + offset;
                            line_source_map.push(current_source_line);
                            lines.push(styled_line.clone());
                        }
                        // Translate table-local hyperlink coordinates into
                        // absolute line indices and append to the global list.
                        for link in &trepl.hyperlinks {
                            hyperlinks.push(HyperlinkTarget {
                                line_index: table_base_line + link.line_offset,
                                column_range: link.column_range.clone(),
                                url: link.url.clone(),
                                id: link.id,
                            });
                        }
                        // Table emits whole pre-rendered lines; reset col so
                        // any subsequent inline content starts at column 0.
                        cur_col_in_line = 0;

                        last_pos = trepl.range.end;
                        let newlines_in_table =
                            count_newlines_in_range(trepl.range.start, trepl.range.end, self.text);
                        current_source_line = table_start_source_line + newlines_in_table;
                        last_line_count_pos = trepl.range.end;

                        if checkpoint_output_lines.is_none()
                            && let Some((_, cp_byte)) = checkpoint_info
                            && last_pos >= cp_byte
                        {
                            checkpoint_output_lines = Some(lines.len());
                        }
                    }
                }
                RenderEventKind::Mermaid => {
                    if ev.is_end && mermaid_replace == Some(ev.index) {
                        mermaid_replace = None;
                    } else if !ev.is_end && mermaid_replace.is_none() && pretty {
                        mermaid_replace = Some(ev.index);
                        let mrepl = &self.buffers.mermaid_replaces[ev.index];

                        if mrepl.range.start > last_line_count_pos {
                            current_source_line += count_newlines_in_range(
                                last_line_count_pos,
                                mrepl.range.start,
                                self.text,
                            );
                        }
                        let start_source_line = current_source_line;

                        for styled_line in &mrepl.styled_lines {
                            line_source_map.push(start_source_line);
                            lines.push(styled_line.clone());
                        }
                        cur_col_in_line = 0;

                        last_pos = mrepl.range.end;
                        let newlines =
                            count_newlines_in_range(mrepl.range.start, mrepl.range.end, self.text);
                        current_source_line = start_source_line + newlines;
                        last_line_count_pos = mrepl.range.end;

                        if checkpoint_output_lines.is_none()
                            && let Some((_, cp_byte)) = checkpoint_info
                            && last_pos >= cp_byte
                        {
                            checkpoint_output_lines = Some(lines.len());
                        }
                    }
                }
                RenderEventKind::Highlight => {
                    if ev.is_end {
                        self.buffers.active_highlights.retain(|&x| x != ev.index);
                    } else {
                        self.buffers.active_highlights.push(ev.index);
                    }
                }
            }
        }

        // Handle remaining text
        let len = self.text.len();
        if last_pos < len {
            // Apply force transforms only; non-force transforms have
            // never been applied in this trailing path and force
            // transforms preserve byte length so source offsets below
            // stay valid.
            let raw = &self.text[last_pos..len];
            let transformed = self.apply_transforms(raw, last_pos, false);
            debug_assert_eq!(transformed.len(), raw.len());
            let text: &str = &transformed;
            let is_only_whitespace = text.as_bytes().iter().all(u8::is_ascii_whitespace);

            if !(pretty && is_only_whitespace) {
                if last_pos > last_line_count_pos {
                    current_source_line +=
                        count_newlines_in_range(last_line_count_pos, last_pos, self.text);
                    last_line_count_pos = last_pos;
                }
                let chunk_src_start = last_pos;
                let chunk_src_end = last_pos + text.len();

                // Same cursor-skip pattern as the main path: keep the no-link
                // hot path identical to the pre-feature renderer.
                while next_link_idx < self.buffers.link_targets.len()
                    && self.buffers.link_targets[next_link_idx].source_range.end <= chunk_src_start
                {
                    next_link_idx += 1;
                }
                let chunk_has_links = next_link_idx < self.buffers.link_targets.len()
                    && self.buffers.link_targets[next_link_idx].source_range.start < chunk_src_end;

                // Trailing text bypasses apply_transforms (it's emitted raw),
                // so transformed offsets equal source offsets within the chunk.
                let chunk_links: Vec<ChunkLinkRange> = if chunk_has_links {
                    chunk_link_offsets(
                        &self.buffers.link_targets,
                        next_link_idx,
                        chunk_src_start,
                        chunk_src_end,
                        false,
                        &[],
                    )
                } else {
                    Vec::new()
                };

                let mut byte_offset = last_pos;
                let mut seg_x_offset: usize = 0;
                let is_in_code = in_untagged_code(last_pos, self.buffers);
                pending_line_is_code = is_in_code;

                for (idx, segment) in text.split('\n').enumerate() {
                    if idx > 0 {
                        line_source_map.push(current_source_line);
                        let line = Line::from(std::mem::take(&mut self.buffers.current_spans));
                        lines.push(if is_in_code {
                            line.style(code_bg_style)
                        } else {
                            line
                        });
                        if byte_offset > last_line_count_pos {
                            current_source_line += count_newlines_in_range(
                                last_line_count_pos,
                                byte_offset,
                                self.text,
                            );
                            last_line_count_pos = byte_offset;
                        }
                        cur_col_in_line = 0;
                    }

                    if !chunk_links.is_empty() {
                        emit_segment_hyperlinks(
                            &chunk_links,
                            &self.buffers.link_targets,
                            segment,
                            seg_x_offset,
                            cur_col_in_line,
                            lines.len(),
                            &mut hyperlinks,
                        );
                    }

                    if !segment.is_empty() {
                        self.buffers
                            .current_spans
                            .push(Span::raw(segment.to_string()));
                        cur_col_in_line += unicode_display_width(segment);
                    }
                    byte_offset += segment.len() + 1;
                    seg_x_offset += segment.len() + 1;
                }
            }
        }

        // Emit final line. Use the membership of the chunk that produced these spans:
        // an unterminated bare fence ends its range exactly at last_pos (EOF) and the
        // range check is end-exclusive, so recomputing here would drop the code bg.
        if !self.buffers.current_spans.is_empty() {
            line_source_map.push(current_source_line);
            let final_is_code = pending_line_is_code;
            let line = Line::from(std::mem::take(&mut self.buffers.current_spans));
            lines.push(if final_is_code {
                line.style(code_bg_style)
            } else {
                line
            });
        }

        // If checkpoint wasn't captured during event processing, compute it based on
        // the number of newlines in the text up to checkpoint byte.
        // This handles cases where there are no events past the checkpoint (e.g., incomplete list items).
        if checkpoint_output_lines.is_none()
            && let Some((_, cp_byte)) = checkpoint_info
        {
            // Count newlines in text before the checkpoint byte.
            // Each newline ENDS a line, so N newlines = N complete lines.
            // However, we need to account for blank lines that are absorbed
            // into the block separator. The checkpoint is at the start of
            // the NEXT block, so lines from the frozen content should not
            // include any content that starts at or after cp_byte.
            //
            // More precise approach: count how many output lines have their
            // content entirely before cp_byte. This is tricky without tracking
            // each line's byte range.
            //
            // Logic:
            // - Each newline ENDS a line
            // - Use line_source_map to find output lines before checkpoint
            // - line_source_map[i] is the source line at which output line i was created
            // - source_line_at_cp is the source line containing cp_byte
            // - Output lines with source_line < source_line_at_cp are complete before checkpoint

            let source_line_at_cp = self.text[..cp_byte.min(self.text.len())]
                .bytes()
                .filter(|&b| b == b'\n')
                .count();

            // When the checkpoint is at or past the end of the text, ALL output
            // lines belong to the frozen content (the entire input was consumed
            // by the checkpointed block).  Otherwise, output lines created at
            // source lines strictly before the checkpoint source line are frozen.
            let complete_lines = if cp_byte >= self.text.len() {
                lines.len()
            } else {
                line_source_map
                    .iter()
                    .take_while(|&&src_line| src_line < source_line_at_cp)
                    .count()
            };

            checkpoint_output_lines = Some(complete_lines.min(lines.len()));
        }

        let checkpoint = match (checkpoint_info, checkpoint_output_lines) {
            (Some((kind, source_bytes)), Some(output_lines)) => Some(Checkpoint {
                source_bytes,
                output_lines,
                kind,
            }),
            _ => None,
        };

        // Now that `line_source_map` is final, map each parsed code block's
        // body onto its rendered (pre-wrap) line range.
        let text = self.text;
        let code_blocks = crate::output::build_code_block_spans(
            text,
            &line_source_map,
            std::mem::take(&mut self.buffers.code_blocks),
        );

        (
            MarkdownRenderOutput {
                lines,
                line_source_map,
                hyperlinks,
                code_blocks,
            },
            checkpoint,
        )
    }
}

#[cfg(test)]
mod tests {
    use crate::render_markdown_ratatui_full;
    use crate::style::test_style;

    fn lines_to_text(lines: &[ratatui::text::Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    /// A fenced `mermaid` block renders as a diagram in pretty mode.
    #[test]
    fn test_mermaid_block_renders_diagram() {
        let md = "```mermaid\ngraph TD\n  A[Start] --> B[End]\n```\n";
        let (output, _) = render_markdown_ratatui_full(md, test_style::STYLE, true, None);
        let text = lines_to_text(&output.lines).join("\n");
        assert!(
            text.contains('┌') || text.contains('╭'),
            "expected box-drawing, got:\n{text}"
        );
        assert!(text.contains("Start") && text.contains("End"), "{text}");
        assert!(text.contains('▼'), "expected an arrowhead, got:\n{text}");
        assert!(!text.contains("```"), "fences should be hidden:\n{text}");
    }

    /// A mermaid fence with trailing info tokens still renders a diagram.
    #[test]
    fn test_mermaid_block_with_info_extras_renders() {
        let md = "```mermaid theme=dark\ngraph TD\n  A[X] --> B[Y]\n```\n";
        let (output, _) = render_markdown_ratatui_full(md, test_style::STYLE, true, None);
        let text = lines_to_text(&output.lines).join("\n");
        assert!(
            text.contains('▼'),
            "info extras should still draw a diagram:\n{text}"
        );
    }

    /// Raw mode shows the mermaid source instead of the diagram.
    #[test]
    fn test_mermaid_block_raw_mode_shows_source() {
        let md = "```mermaid\ngraph TD\n  A[Start] --> B[End]\n```\n";
        let (output, _) = render_markdown_ratatui_full(md, test_style::STYLE, false, None);
        let text = lines_to_text(&output.lines).join("\n");
        assert!(text.contains("graph TD"), "raw should show source:\n{text}");
        assert!(
            !text.contains('▼'),
            "raw should not draw a diagram:\n{text}"
        );
    }

    /// Pretty mode must remove the opening `[` from `[text](url)` links.
    /// Regression test: apply_transforms treated replace-with-empty-string
    /// as "no transform applied" because it checked `result.is_empty()`.
    #[test]
    fn test_pretty_link_bracket_removed() {
        let text = "Here is a [link](https://example.com) in text.\n\n";
        let (output, _) = render_markdown_ratatui_full(text, test_style::STYLE, true, None);
        let lines = lines_to_text(&output.lines);

        assert!(
            !lines[0].contains("[link"),
            "Pretty mode should remove '[' from link. Got: {:?}",
            lines[0]
        );
        assert!(
            lines[0].contains("link (https://example.com)"),
            "Pretty mode should render 'link (url)'. Got: {:?}",
            lines[0]
        );
    }

    /// Same regression for images: `![img](src)` should not show `[img`.
    #[test]
    fn test_pretty_image_bracket_removed() {
        let text = "An ![image](src.png) here.\n\n";
        let (output, _) = render_markdown_ratatui_full(text, test_style::STYLE, true, None);
        let lines = lines_to_text(&output.lines);

        let img_line = &lines[0];
        assert!(
            !img_line.contains("[image"),
            "Pretty mode should remove '[' from image. Got: {:?}",
            img_line
        );
    }

    /// Regression: `count_newlines_in_range` panics when a checkpoint byte
    /// offset from a thematic break falls inside a multi-byte character in
    /// subsequent content (e.g., a 4-byte emoji like 📐).
    ///
    /// Minimal repro: thematic break `---` followed by heading with emoji.
    /// The checkpoint creates a byte offset that lands mid-emoji when used
    /// to slice `self.text` in `text[from..to]`.
    /// Nested blockquote with paragraph break and list inside inner quote.
    #[test]
    fn test_nested_blockquote_with_list() {
        let md = "> Foo\n>\n> > Bar\n> >\n> > - Baz\n";

        let (raw_output, _) = render_markdown_ratatui_full(md, test_style::STYLE, false, None);
        assert_eq!(
            lines_to_text(&raw_output.lines),
            vec!["> Foo", ">", "> > Bar", "> >", "> > - Baz"],
            "raw mode",
        );

        let (pretty_output, _) = render_markdown_ratatui_full(md, test_style::STYLE, true, None);
        assert_eq!(
            lines_to_text(&pretty_output.lines),
            vec!["│ Foo", "│", "│ │ Bar", "│ │", "│ │ • Baz"],
            "pretty mode",
        );
    }

    #[test]
    fn test_emoji_after_thematic_break_does_not_panic() {
        // "---\n\n## 📐 H\n\n" — 📐 is at bytes 8..12, checkpoint offset
        // lands at byte 10 (inside the emoji), causing a panic in
        // count_newlines_in_range which does text[from..to].
        let md = "---\n\n## 📐 H\n\n";
        let (_output, _cp) = render_markdown_ratatui_full(md, test_style::STYLE, true, None);
    }

    #[test]
    fn test_list_followed_by_code_block_has_separator() {
        // A list item followed by a code block should have a blank line between them
        let md = "1. Hello\n```python\nworld\n```\n";
        let (output, _) = render_markdown_ratatui_full(md, test_style::STYLE, true, None);
        let text = lines_to_text(&output.lines);
        eprintln!("Lines: {text:#?}");

        // Find the list item line and the code block line
        let hello_idx = text.iter().position(|l| l.contains("Hello")).unwrap();
        let world_idx = text.iter().position(|l| l.contains("world")).unwrap();

        // There should be at least one blank line between them
        assert!(
            world_idx - hello_idx >= 2,
            "Expected blank line between list item and code block. \
             hello at {hello_idx}, world at {world_idx}. Lines: {text:#?}"
        );
    }

    #[test]
    fn test_code_block_empty_line_has_bg() {
        use ratatui::style::Color;

        // Create a style with a visible code_background
        let mut style = test_style::STYLE;
        style.code_background = anstyle::Style::new()
            .bg_color(Some(anstyle::Color::Rgb(anstyle::RgbColor(30, 30, 46))));

        let md = "```\nline1\n\nline3\n```\n";
        let (output, _) = render_markdown_ratatui_full(md, style, true, None);

        let expected_bg = Color::Rgb(30, 30, 46);

        // All lines inside the code block should have the bg set
        for (i, line) in output.lines.iter().enumerate() {
            assert_eq!(
                line.style.bg,
                Some(expected_bg),
                "Line {i} ({:?}) should have code_background, got {:?}",
                lines_to_text(std::slice::from_ref(line))[0],
                line.style.bg,
            );
        }
    }

    /// Regression: an unterminated bare fence with no trailing newline (the tail of a
    /// streamed message) must keep code_background on its final line.
    #[test]
    fn test_unterminated_untagged_fence_final_line_has_bg() {
        use ratatui::style::Color;

        let mut style = test_style::STYLE;
        style.code_background = anstyle::Style::new()
            .bg_color(Some(anstyle::Color::Rgb(anstyle::RgbColor(30, 30, 46))));

        let md = "```\nline1\n\nfinal line";
        let (output, _) = render_markdown_ratatui_full(md, style, true, None);

        let texts = lines_to_text(&output.lines);
        assert!(
            texts.last().is_some_and(|l| l.contains("final line")),
            "expected the newline-less final line in output: {texts:#?}"
        );
        let expected_bg = Color::Rgb(30, 30, 46);
        for (i, line) in output.lines.iter().enumerate() {
            assert_eq!(
                line.style.bg,
                Some(expected_bg),
                "Line {i} ({:?}) should have code_background",
                texts[i],
            );
        }
    }

    /// Tables wider than max_table_width should be constrained to fit.
    #[test]
    fn test_table_constrained_to_max_width() {
        use unicode_width::UnicodeWidthStr;

        let md = "| Column A | Column B | Column C |\n|----------|----------|----------|\n| value 1  | value 2  | value 3  |\n\n";

        // Render without constraint — table uses natural widths
        let (output_full, _) = render_markdown_ratatui_full(md, test_style::STYLE, true, None);
        let full_lines = lines_to_text(&output_full.lines);
        let full_max_width = full_lines.iter().map(|l| l.width()).max().unwrap_or(0);

        // Render with narrow constraint
        let narrow = 30;
        assert!(
            full_max_width > narrow,
            "Table should be wider than {narrow} naturally"
        );

        let mut buffers = crate::MarkdownBuffers::new();
        let (output_narrow, _) = crate::render_markdown_ratatui_with_buffers_width(
            md,
            test_style::STYLE,
            true,
            &mut buffers,
            None,
            Some(narrow),
        );
        let narrow_lines = lines_to_text(&output_narrow.lines);
        let narrow_max_width = narrow_lines.iter().map(|l| l.width()).max().unwrap_or(0);

        assert!(
            narrow_max_width <= narrow,
            "Constrained table should fit within {narrow} columns, got {narrow_max_width}. Lines: {narrow_lines:#?}"
        );

        // All table lines should still have consistent widths
        let table_widths: Vec<usize> = narrow_lines.iter().map(|l| l.width()).collect();
        let first_width = table_widths[0];
        for (i, &w) in table_widths.iter().enumerate() {
            assert_eq!(
                w, first_width,
                "Table line {i} has width {w}, expected {first_width}"
            );
        }
    }

    /// When columns are shrunk, long cell content should be wrapped within the cell.
    #[test]
    fn test_table_cell_wrapping() {
        let md = "| Very Long Column Name |\n|-----------------------|\n| Short |\n\n";

        let mut buffers = crate::MarkdownBuffers::new();
        let (output, _) = crate::render_markdown_ratatui_with_buffers_width(
            md,
            test_style::STYLE,
            true,
            &mut buffers,
            None,
            Some(15),
        );
        let text = lines_to_text(&output.lines);
        eprintln!("Wrapped table: {text:#?}");

        // The header "Very Long Column Name" should be wrapped across multiple lines
        // since it doesn't fit in the constrained column width.
        // All content should still be present (no truncation).
        let all_text: String = text.join("");
        assert!(
            all_text.contains("Very") && all_text.contains("Long") && all_text.contains("Name"),
            "All header words should be present (wrapped, not truncated). Got: {text:#?}"
        );
    }

    /// Cell wrapping should break at punctuation/symbols, not mid-word.
    /// Punct chars attach to whichever side gives a smaller max segment.
    #[test]
    fn test_table_cell_wraps_at_punctuation() {
        use crate::parse::cell_word_separator;

        fn words(s: &str) -> Vec<String> {
            cell_word_separator(s)
                .map(|w| format!("[{}|{}]", w.word, w.whitespace))
                .collect()
        }

        // Equal-length sides: tie goes to attach-left
        assert_eq!(words("foo/bar"), vec!["[foo/|]", "[bar|]"]);

        // Break at space after comma (whitespace break, no attachment choice)
        assert_eq!(words("hello, world"), vec!["[hello,| ]", "[world|]"]);

        // Plain words only break on spaces
        assert_eq!(words("hello world"), vec!["[hello| ]", "[world|]"]);

        // Single-char segments separated by hyphens
        assert_eq!(words("a-b-c"), vec!["[a-|]", "[b-|]", "[c|]"]);

        // Unequal sides: punct attaches to shorter side to minimize max
        // ABCD-EFG: left gives max(5,3)=5, right gives max(4,4)=4 → right
        assert_eq!(words("ABCD-EFG"), vec!["[ABCD|]", "[-EFG|]"]);

        // Comma and dot between digits stay together (number formatting)
        assert_eq!(words("$145,000"), vec!["[$145,000|]"]);
        assert_eq!(words("3.14"), vec!["[3.14|]"]);
        assert_eq!(words("1.0.2"), vec!["[1.0.2|]"]);

        // Hyphens between digits are breakable (phones, dates, IDs)
        // Attachment is chosen to minimize max segment width.
        // 2019-03-15: right gives max(4,3,3)=4 < left max(5,3,2)=5
        assert_eq!(words("2019-03-15"), vec!["[2019|]", "[-03|]", "[-15|]"]);
        // 555-0101: right gives max(3,5)=5 vs left max(4,4)=4 → left
        assert_eq!(words("555-0101"), vec!["[555-|]", "[0101|]"]);
        // Verify a full phone number breaks correctly
        let phone = words("+44-20-7555-0118");
        // All segments should be present, phone is breakable
        assert!(phone.len() > 1, "phone number should be breakable");
        assert_eq!(
            words("(415) 555-0101"),
            vec!["[(415)| ]", "[555-|]", "[0101|]"]
        );
        // EMP-1001: no digit before `-`, and `1` after is not alphabetic →
        // stays together (it's an ID, not digit-punct-digit)
        assert_eq!(words("EMP-1001"), vec!["[EMP-1001|]"]);
    }

    /// URLs should be treated as unbreakable words so that terminal
    /// Cmd+Click detection works when table cells wrap.
    #[test]
    fn test_table_cell_url_not_broken() {
        use crate::parse::cell_word_separator;

        fn words(s: &str) -> Vec<String> {
            cell_word_separator(s)
                .map(|w| format!("[{}|{}]", w.word, w.whitespace))
                .collect()
        }

        // A URL should be a single unbreakable word
        assert_eq!(
            words("https://example.com/path/to/page"),
            vec!["[https://example.com/path/to/page|]"]
        );

        // URL with text before and after breaks at spaces, URL stays intact
        assert_eq!(
            words("see https://example.com/foo for details"),
            vec![
                "[see| ]",
                "[https://example.com/foo| ]",
                "[for| ]",
                "[details|]"
            ]
        );

        // http:// URLs are also preserved
        assert_eq!(
            words("http://example.com/a-b/c"),
            vec!["[http://example.com/a-b/c|]"]
        );

        // Multiple URLs in the same cell
        assert_eq!(
            words("https://a.com/x https://b.com/y"),
            vec!["[https://a.com/x| ]", "[https://b.com/y|]"]
        );

        // URL with query params and fragments
        assert_eq!(
            words("https://example.com/search?q=hello&lang=en#results"),
            vec!["[https://example.com/search?q=hello&lang=en#results|]"]
        );

        // Non-http schemes (ftp, ssh, etc.) are also preserved
        assert_eq!(
            words("ftp://files.example.com/pub/data"),
            vec!["[ftp://files.example.com/pub/data|]"]
        );
        assert_eq!(
            words("ssh://git@github.com/org/repo"),
            vec!["[ssh://git@github.com/org/repo|]"]
        );
    }

    /// Inline formatting (bold, italic, code) should be preserved per-span
    /// when table cells are wrapped across multiple visual lines.
    #[test]
    fn test_table_preserves_inline_formatting() {
        // Table with inline code in a cell
        let md = "| A | B |\n|---|---|\n| 1 | hello world `abc` |\n\n";

        let mut buffers = crate::MarkdownBuffers::new();
        let (output, _) = crate::render_markdown_ratatui_with_buffers_width(
            md,
            test_style::STYLE,
            true,
            &mut buffers,
            None,
            Some(30), // narrow enough to force wrapping in column B
        );

        // Find the lines that contain "abc" — they should have a styled span
        // with the code style, not just plain text.
        let mut found_code_span = false;
        for line in &output.lines {
            for span in &line.spans {
                if span.content.contains("abc") {
                    // Inline code should have some style applied (not default)
                    let default_style = ratatui::style::Style::default();
                    assert_ne!(
                        span.style, default_style,
                        "Inline code `abc` should have code formatting, got default style"
                    );
                    found_code_span = true;
                }
            }
        }
        assert!(
            found_code_span,
            "Should find a span containing 'abc' with code formatting"
        );
    }

    /// Regression: table cells containing multi-byte UTF-8 characters (em-dash '—',
    /// CJK, emoji, etc.) could panic with "byte index N is not a char boundary"
    /// when cell wrapping causes `prev_len` (sum of wrapped-line byte lengths) to
    /// land inside a multi-byte character sequence.
    #[test]
    fn test_table_cell_with_multibyte_chars_does_not_panic() {
        // Em-dash '—' is 3 bytes (0xE2 0x80 0x94). Force wrapping so the
        // prev_len calculation for the second visual line can land mid-char.
        let md = "| A |\n|---|\n| hello world — goodbye world |\n\n";
        let mut buffers = crate::MarkdownBuffers::new();
        let (output, _) = crate::render_markdown_ratatui_with_buffers_width(
            md,
            test_style::STYLE,
            true,
            &mut buffers,
            None,
            Some(20), // narrow enough to force wrapping around the em-dash
        );
        let text = lines_to_text(&output.lines);
        let all_text: String = text.join("");
        // All content should still be present (no truncation or crash).
        assert!(
            all_text.contains("hello") && all_text.contains("goodbye"),
            "All cell words should be present after wrapping. Got: {text:#?}"
        );
    }

    /// Same regression for CJK and emoji characters in table cells.
    #[test]
    fn test_table_cell_with_cjk_and_emoji_does_not_panic() {
        // Mix CJK (3 bytes each), emoji (4 bytes), and ASCII to stress char boundaries.
        let md = "| Col |\n|-----|\n| \u{4F60}\u{597D}\u{4E16}\u{754C} hello \u{1F680}\u{1F30D} world |\n\n";
        let mut buffers = crate::MarkdownBuffers::new();
        let (output, _) = crate::render_markdown_ratatui_with_buffers_width(
            md,
            test_style::STYLE,
            true,
            &mut buffers,
            None,
            Some(15),
        );
        let text = lines_to_text(&output.lines);
        let all_text: String = text.join("");
        assert!(
            all_text.contains("hello") && all_text.contains("world"),
            "ASCII words should survive wrapping with CJK/emoji. Got: {text:#?}"
        );
    }

    /// Split rendered table lines into logical rows of per-column cell text,
    /// concatenating wrapped fragments *without* inserting spaces so that
    /// hard-split tokens reconstruct exactly.
    fn reconstruct_table_cells(lines: &[String]) -> Vec<Vec<String>> {
        let mut rows: Vec<Vec<String>> = Vec::new();
        let mut current: Option<Vec<String>> = None;
        for line in lines {
            if let Some(inner) = line.strip_prefix('│').and_then(|l| l.strip_suffix('│')) {
                let cells: Vec<&str> = inner.split('│').collect();
                let row = current.get_or_insert_with(|| vec![String::new(); cells.len()]);
                for (acc, fragment) in row.iter_mut().zip(cells) {
                    acc.push_str(fragment.trim());
                }
            } else if let Some(row) = current.take() {
                // Border or separator line closes the logical row.
                rows.push(row);
            }
        }
        if let Some(row) = current.take() {
            rows.push(row);
        }
        rows
    }

    /// A six-column table of unbreakable tokens must reflow inside cells and
    /// grow taller — never exceed the width budget or lose the right border.
    #[test]
    fn test_table_six_col_unbreakable_tokens_fit_width_50_40_30() {
        use unicode_width::UnicodeWidthStr;

        let md = "| Alpha | Bravo | Ident | DeptName | RoleName | Amount |\n\
                  |---|---|---|---|---|---|\n\
                  | LongalphaToken | TokenTwo | ID-AA1001 | EngineeringOps | ManagerRole | $145,000 |\n\n";

        let (output_full, _) = render_markdown_ratatui_full(md, test_style::STYLE, true, None);
        let full_lines = lines_to_text(&output_full.lines);
        let full_max_width = full_lines.iter().map(|l| l.width()).max().unwrap_or(0);

        for narrow in [50usize, 40, 30] {
            assert!(
                full_max_width > narrow,
                "fixture must be naturally wider than {narrow}, got {full_max_width}"
            );

            let mut buffers = crate::MarkdownBuffers::new();
            let (output, _) = crate::render_markdown_ratatui_with_buffers_width(
                md,
                test_style::STYLE,
                true,
                &mut buffers,
                None,
                Some(narrow),
            );
            let lines = lines_to_text(&output.lines);
            assert!(!lines.is_empty(), "table should render at width {narrow}");

            let first_width = lines.first().map(|l| l.width()).unwrap_or(0);
            for (i, line) in lines.iter().enumerate() {
                let w = line.width();
                assert!(w <= narrow, "line {i} width {w} exceeds {narrow}: {line:?}");
                assert_eq!(
                    w, first_width,
                    "line {i} width {w} != {first_width}: {line:?}"
                );
                let right_edge = line.trim_end().chars().last();
                assert!(
                    matches!(right_edge, Some('│' | '┐' | '┘' | '┤')),
                    "line {i} lost its right border at width {narrow}: {line:?}"
                );
            }

            // Width pressure must add visual lines (taller rows), not clip.
            let content_lines = lines.iter().filter(|l| l.starts_with('│')).count();
            assert!(
                content_lines > 2,
                "cells should wrap onto extra visual lines at width {narrow}, got {content_lines}"
            );
            assert!(
                lines.len() > full_lines.len(),
                "table should grow taller at width {narrow}: {} vs {} lines",
                lines.len(),
                full_lines.len()
            );

            // Every token must survive the reflow in its own column — each
            // body cell is a single token, so reconstruction is exact.
            let rows = reconstruct_table_cells(&lines);
            assert_eq!(rows.len(), 2, "header + one body row, got {rows:#?}");
            assert_eq!(
                rows[1],
                [
                    "LongalphaToken",
                    "TokenTwo",
                    "ID-AA1001",
                    "EngineeringOps",
                    "ManagerRole",
                    "$145,000",
                ],
                "body cells must reconstruct exactly at width {narrow}"
            );
        }
    }

    /// Hard splits must fall on grapheme boundaries — CJK, VS16 emoji, and
    /// ZWJ clusters stay intact and every line fits the assigned width.
    /// Fixtures are single unbreakable words so the word separator never
    /// contributes break points of its own.
    #[test]
    fn test_wrap_cell_text_grapheme_hard_split_stays_within_width() {
        use unicode_segmentation::UnicodeSegmentation;
        use unicode_width::UnicodeWidthStr;

        for text in [
            "你好世界⚠\u{FE0F}",
            "编码测试👩\u{200D}🚀",
            "你好世👩\u{200D}🚀。",
        ] {
            let widest = text.graphemes(true).map(|g| g.width()).max().unwrap_or(0);
            // Representable width: at least the widest single grapheme.
            let width = widest.max(4);

            let lines = crate::parse::MarkdownParser::wrap_cell_text(text, width);
            assert!(!lines.is_empty(), "wrap must never return an empty vec");
            assert!(
                lines.len() > 1,
                "fixture should force a hard split: {text:?} at width {width}"
            );
            for line in &lines {
                assert!(
                    line.width() <= width,
                    "line {line:?} wider than {width} for {text:?}"
                );
            }
            assert_eq!(
                lines.concat(),
                text,
                "no graphemes may be dropped or reordered"
            );

            // Every split point must be a grapheme boundary of the source.
            let boundaries: std::collections::HashSet<usize> = text
                .grapheme_indices(true)
                .map(|(i, _)| i)
                .chain(std::iter::once(text.len()))
                .collect();
            let mut offset = 0usize;
            for line in &lines {
                offset += line.len();
                assert!(
                    boundaries.contains(&offset),
                    "split at byte {offset} falls inside a grapheme of {text:?}"
                );
            }
        }
    }

    /// A markdown link hard-wrapped across visual lines keeps one shared
    /// hyperlink id + url, in-bounds column ranges, and link styling on
    /// every fragment.
    #[test]
    fn test_table_hard_wrapped_styled_link_keeps_id_and_bounds() {
        use unicode_width::UnicodeWidthStr;

        let md = "| L |\n|---|\n| [clickmenowplease](https://example.com) |\n\n";
        let narrow = 12;

        let mut buffers = crate::MarkdownBuffers::new();
        let (output, _) = crate::render_markdown_ratatui_with_buffers_width(
            md,
            test_style::STYLE,
            true,
            &mut buffers,
            None,
            Some(narrow),
        );
        let lines = lines_to_text(&output.lines);

        for (i, line) in lines.iter().enumerate() {
            assert!(
                line.width() <= narrow,
                "line {i} exceeds width {narrow}: {line:?}"
            );
        }

        // All label characters survive the hard wrap (no inserted spaces).
        let rows = reconstruct_table_cells(&lines);
        assert!(
            rows.iter().any(|r| r.concat().contains("clickmenowplease")),
            "label must reconstruct intact: {rows:#?}"
        );

        let links: Vec<_> = output
            .hyperlinks
            .iter()
            .filter(|h| h.url == "https://example.com")
            .collect();
        assert!(
            links.len() >= 2,
            "wrapped label should produce multiple link fragments: {links:#?}"
        );
        let first_id = links[0].id;
        for link in &links {
            assert_eq!(link.id, first_id, "fragments must share one link id");
            let line = &lines[link.line_index];
            assert!(
                link.column_range.end <= line.width(),
                "range {:?} exceeds line {} width {}: {line:?}",
                link.column_range,
                link.line_index,
                line.width()
            );
        }

        // Label fragments keep link styling instead of default spans.
        let default_style = ratatui::style::Style::default();
        let styled_fragments = output
            .lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .filter(|span| {
                let content = span.content.trim();
                !content.is_empty()
                    && "clickmenowplease".contains(content)
                    && span.style != default_style
            })
            .count();
        assert!(
            styled_fragments >= 2,
            "wrapped label fragments must keep link styling"
        );
    }

    /// Hard-split fragments must project source spans from a monotonic
    /// cursor: a linked run followed by a plain run of the same substring
    /// ("aa") must not re-match earlier bytes — that leaks link style and
    /// hyperlink ranges into the plain fragment.
    #[test]
    fn test_table_hard_split_adjacent_link_and_plain_spans_stay_separate() {
        // Cell "x aaaaaa" wraps at content width 2 into fragments
        // "x" / "aa" / "aa" / "aa": two linked, then one plain.
        let md = "| A |\n|---|\n| x [aaaa](https://example.com)aa |\n\n";

        let mut buffers = crate::MarkdownBuffers::new();
        let (output, _) = crate::render_markdown_ratatui_with_buffers_width(
            md,
            test_style::STYLE,
            true,
            &mut buffers,
            None,
            Some(6),
        );
        let lines = lines_to_text(&output.lines);

        // Each content line renders its fragment as exactly one span — a
        // mis-projected fragment straddles two source spans and splits.
        let fragment_spans: Vec<Vec<&str>> = output
            .lines
            .iter()
            .zip(&lines)
            .filter(|(_, text)| text.starts_with('│'))
            .map(|(line, _)| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .filter(|c| !c.contains('│') && !c.trim().is_empty())
                    .collect()
            })
            .collect();
        assert_eq!(
            fragment_spans,
            [["A"], ["x"], ["aa"], ["aa"], ["aa"]],
            "fragments must not straddle span boundaries: {lines:#?}"
        );

        // Only the two linked fragments carry hyperlinks, sharing one id and
        // covering exactly the linked text on their lines.
        let links: Vec<_> = output
            .hyperlinks
            .iter()
            .filter(|h| h.url == "https://example.com")
            .collect();
        assert_eq!(links.len(), 2, "plain fragment must not link: {links:#?}");
        assert_eq!(links[0].id, links[1].id, "fragments must share one link id");
        for link in &links {
            // All chars on these lines are one display cell wide.
            let line = &lines[link.line_index];
            let covered: String = line
                .chars()
                .skip(link.column_range.start)
                .take(link.column_range.len())
                .collect();
            assert_eq!(
                covered, "aa",
                "hyperlink range must cover exactly the linked text: {line:?}"
            );
        }

        // The trailing plain "aa" must render unstyled (no link leak).
        let last_link_line = links.iter().map(|l| l.line_index).max().unwrap_or(0);
        let plain_line = &output.lines[last_link_line + 1];
        let default_style = ratatui::style::Style::default();
        for span in &plain_line.spans {
            let content = span.content.trim();
            if !content.is_empty() && !span.content.contains('│') {
                assert_eq!(content, "aa", "plain fragment content: {plain_line:?}");
                assert_eq!(
                    span.style, default_style,
                    "plain fragment must not inherit link styling"
                );
            }
        }
    }

    /// Table source map: rendered line numbers must not exceed the table's
    /// actual source line count, and must map to the correct source lines.
    #[test]
    fn test_table_source_map_stays_within_bounds() {
        // 4 source lines: header (0), separator (1), row1 (2), row2 (3)
        let md = "| A | B |\n|---|---|\n| x | y |\n| w | z |\n\n";

        let table_start_line = 0usize;
        let table_source_lines = 4usize; // header + separator + 2 rows

        let (output, _) = render_markdown_ratatui_full(md, test_style::STYLE, true, None);

        for (i, &src_line) in output.line_source_map.iter().enumerate() {
            assert!(
                src_line < table_start_line + table_source_lines,
                "Rendered line {i} maps to source line {src_line}, \
                 but table only has {table_source_lines} source lines \
                 (0..{}). Source map: {:?}",
                table_start_line + table_source_lines,
                output.line_source_map,
            );
        }
    }

    /// Table source map: header, separator, and body rows map to correct offsets.
    #[test]
    fn test_table_source_map_correct_offsets() {
        let md = "| H1 | H2 |\n|----|----|\n| r1 | r2 |\n| r3 | r4 |\n\n";

        let (output, _) = render_markdown_ratatui_full(md, test_style::STYLE, true, None);
        let text = lines_to_text(&output.lines);
        let map = &output.line_source_map;

        // Find which rendered lines contain table content.
        // Source offsets: header=0, separator=1, row1=2, row2=3
        for (i, line_text) in text.iter().enumerate() {
            let src = map[i];
            if line_text.contains("H1") || line_text.contains("H2") {
                assert_eq!(
                    src, 0,
                    "Header content line {i} should map to source 0, got {src}"
                );
            }
            if line_text.contains("r1") || line_text.contains("r2") {
                assert_eq!(
                    src, 2,
                    "Row 1 content line {i} should map to source 2, got {src}"
                );
            }
            if line_text.contains("r3") || line_text.contains("r4") {
                assert_eq!(
                    src, 3,
                    "Row 2 content line {i} should map to source 3, got {src}"
                );
            }
        }
    }

    /// Table source map with cell wrapping: wrapped continuation lines must
    /// map to the same source line as the first visual line of that row.
    #[test]
    fn test_table_source_map_with_cell_wrapping() {
        let md = "| Name | Description |\n|------|-------------|\n| short | A very long description that will wrap |\n\n";

        let mut buffers = crate::MarkdownBuffers::new();
        let (output, _) = crate::render_markdown_ratatui_with_buffers_width(
            md,
            test_style::STYLE,
            true,
            &mut buffers,
            None,
            Some(30),
        );

        let table_source_lines = 3; // header + separator + 1 row
        for (i, &src_line) in output.line_source_map.iter().enumerate() {
            assert!(
                src_line < table_source_lines,
                "Wrapped table line {i} maps to source {src_line}, \
                 exceeds table source lines ({table_source_lines}). Map: {:?}",
                output.line_source_map,
            );
        }
    }

    /// Fenced block with `lineStart:lineEnd:path` (citation-style) uses the file
    /// extension for syntect, same as a ` ```rust` block.
    #[test]
    fn test_citation_code_fence_highlights_as_rust() {
        let syntect = crate::syntax::test_syntect();
        let code = "const DEFAULT_READ_LIMIT: usize = 2000;\n";
        let md_cite = format!("```37:65:crates/x/read.rs\n{code}```\n\n");
        let md_rust = format!("```rust\n{code}```\n\n");

        let (out_cite, _) =
            render_markdown_ratatui_full(&md_cite, test_style::STYLE, true, Some(syntect));
        let (out_rust, _) =
            render_markdown_ratatui_full(&md_rust, test_style::STYLE, true, Some(syntect));

        fn const_line_span_count(out: &crate::MarkdownRenderOutput) -> usize {
            out.lines
                .iter()
                .find(|l| l.spans.iter().any(|s| s.content.as_ref().contains("const")))
                .expect("line with 'const' should exist")
                .spans
                .len()
        }

        let s_cite = const_line_span_count(&out_cite);
        let s_rust = const_line_span_count(&out_rust);
        assert_eq!(
            s_cite, s_rust,
            "citation fence should match ```rust highlight shape"
        );
        assert!(
            s_cite > 1,
            "const line should have multiple styled spans, got {s_cite}"
        );
    }

    /// InlineHtml (e.g. `<PathBuf>`) inside a table cell must not leak raw
    /// text below the rendered table. Regression for the Replace-inside-table bug.
    #[test]
    fn test_table_inline_html_no_raw_text_leak_ratatui() {
        let md = "| Col A | Col B |\n|-------|-------|\n| Arc<PathBuf> | optimization |\n| normal | row |\n\n";

        let (output, _) = render_markdown_ratatui_full(md, test_style::STYLE, true, None);
        let text = lines_to_text(&output.lines);
        let joined = text.join("\n");

        // The raw markdown pipe syntax must not appear in rendered output
        assert!(
            !joined.contains("| normal"),
            "Raw markdown table syntax leaked below rendered table. Lines: {text:#?}"
        );
        assert!(
            !joined.contains("| optimization"),
            "Raw table cell content leaked as plain text. Lines: {text:#?}"
        );
    }

    /// ANSI render path: same regression — InlineHtml Replace must not
    /// corrupt `last_pos` and re-emit table content as raw text.
    #[test]
    fn test_table_inline_html_no_raw_text_leak_ansi() {
        let md = "| Col A | Col B |\n|-------|-------|\n| Arc<PathBuf> | optimization |\n| normal | row |\n\n";

        let (output, _) = crate::render_markdown(md, test_style::STYLE, true, None);

        assert!(
            !output.contains("| normal"),
            "Raw markdown table syntax leaked in ANSI output. Got: {output}"
        );
        assert!(
            !output.contains("| optimization"),
            "Raw table cell content leaked in ANSI output. Got: {output}"
        );
    }

    /// InlineHtml content must be captured into table cells so it appears
    /// in the formatted table, not silently dropped.
    #[test]
    fn test_table_inline_html_captured_in_cell() {
        let md = "| Type |\n|------|\n| Arc<PathBuf> |\n\n";

        let (output, _) = render_markdown_ratatui_full(md, test_style::STYLE, true, None);
        let text = lines_to_text(&output.lines);
        let all_text: String = text.join("");

        assert!(
            all_text.contains("<PathBuf>"),
            "InlineHtml content should appear in formatted table cell. Got: {text:#?}"
        );
        assert!(
            all_text.contains("Arc"),
            "Text before InlineHtml should appear in cell. Got: {text:#?}"
        );
    }

    /// Multiple HTML-like tags across different cells and rows must all
    /// render correctly without leaking.
    #[test]
    fn test_table_multiple_inline_html_tags() {
        let md = "| Input | Output |\n|-------|--------|\n| Vec<String> | Option<i32> |\n| Box<dyn Trait> | Result<T> |\n\n";

        let (output, _) = render_markdown_ratatui_full(md, test_style::STYLE, true, None);
        let text = lines_to_text(&output.lines);
        let joined = text.join("\n");

        // No raw pipe-delimited rows should leak
        assert!(
            !joined.contains("| Vec"),
            "Raw table syntax leaked with multiple HTML tags. Lines: {text:#?}"
        );
        assert!(
            !joined.contains("| Box"),
            "Raw table syntax leaked with multiple HTML tags. Lines: {text:#?}"
        );

        // Cell content should be present in the table
        let all_text: String = text.join("");
        assert!(
            all_text.contains("Vec"),
            "Vec should appear in table. Got: {text:#?}"
        );
        assert!(
            all_text.contains("Box"),
            "Box should appear in table. Got: {text:#?}"
        );
    }

    /// ANSI render path: multiple HTML-like tags across cells and rows
    /// must not leak raw text via the `current` accumulator merge logic.
    #[test]
    fn test_table_multiple_inline_html_tags_ansi() {
        let md = "| Input | Output |\n|-------|--------|\n| Vec<String> | Option<i32> |\n| Box<dyn Trait> | Result<T> |\n\n";

        let (output, _) = crate::render_markdown(md, test_style::STYLE, true, None);

        assert!(
            !output.contains("| Vec"),
            "Raw table syntax leaked in ANSI multi-tag output. Got: {output}"
        );
        assert!(
            !output.contains("| Box"),
            "Raw table syntax leaked in ANSI multi-tag output. Got: {output}"
        );
    }

    /// Leading content before a table exercises the `push` flush at the
    /// Table Start event followed by the `current.0` reset after rendering.
    #[test]
    fn test_table_with_leading_text_ansi() {
        let md = "Hello world\n\n| Col |\n|-----|\n| Arc<PathBuf> |\n\n";

        let (output, _) = crate::render_markdown(md, test_style::STYLE, true, None);

        assert!(
            output.contains("Hello world"),
            "Leading text should be present. Got: {output}"
        );
        assert!(
            !output.contains("| Col"),
            "Raw table syntax leaked after leading text in ANSI output. Got: {output}"
        );
    }

    #[test]
    fn test_table_br_tag_becomes_line_break() {
        let md = "| Col |\n|-----|\n| hello<br>world |\n\n";

        let (output, _) = render_markdown_ratatui_full(md, test_style::STYLE, true, None);
        let text = lines_to_text(&output.lines);
        let joined = text.join("\n");

        assert!(!joined.contains("<br>"), "literal <br> leaked: {joined}");
        assert!(
            joined.contains("hello") && joined.contains("world"),
            "cell content missing: {joined}"
        );
        assert!(
            !text
                .iter()
                .any(|l| l.contains("hello") && l.contains("world")),
            "hello and world must be on separate visual lines: {joined}"
        );
    }

    #[test]
    fn test_table_br_tag_variants() {
        let md = "| Col |\n|-----|\n| a<BR>b<br/>c<br />d |\n\n";

        let (output, _) = render_markdown_ratatui_full(md, test_style::STYLE, true, None);
        let text = lines_to_text(&output.lines);
        let joined = text.join("\n");

        for tag in ["<BR>", "<br/>", "<br />"] {
            assert!(!joined.contains(tag), "literal {tag} leaked: {joined}");
        }
        for ch in ['a', 'b', 'c', 'd'] {
            assert!(
                text.iter().any(|l| l.contains(ch)),
                "segment '{ch}' missing: {joined}"
            );
        }
    }

    #[test]
    fn test_table_br_tag_ansi() {
        let md = "| Col |\n|-----|\n| hello<br>world |\n\n";

        let (output, _) = crate::render_markdown(md, test_style::STYLE, true, None);
        assert!(
            !output.contains("<br>"),
            "literal <br> in ANSI output: {output}"
        );
    }

    #[test]
    fn test_br_tag_outside_table() {
        let md = "hello<br>world\n\n";

        let (output, _) = render_markdown_ratatui_full(md, test_style::STYLE, true, None);
        let text = lines_to_text(&output.lines);
        let joined = text.join("\n");

        assert!(
            !joined.contains("<br>"),
            "literal <br> outside table: {joined}"
        );
    }

    // CommonMark soft breaks collapse to a single space inside a plain
    // paragraph; hard breaks and block-container continuations (list
    // items, blockquotes) still split into separate visual lines.

    #[test]
    fn test_soft_break_plain_paragraph_collapses_to_space() {
        let md = "Foo bar\nbaz qux.";
        for pretty in [false, true] {
            let (output, _) = render_markdown_ratatui_full(md, test_style::STYLE, pretty, None);
            let text = lines_to_text(&output.lines);
            assert_eq!(text, vec!["Foo bar baz qux."], "pretty={pretty}: {text:?}");
        }
    }

    #[test]
    fn test_soft_break_original_bug_repro() {
        let md = "- Tiny emit guard in pretty.rs: empty-reflowed KDoc output with no \"<decl>\n\" pollution).";
        let (output, _) = render_markdown_ratatui_full(md, test_style::STYLE, true, None);
        let text = lines_to_text(&output.lines);
        assert_eq!(text.len(), 1, "got: {text:?}");
        assert!(
            text[0].contains("no \"<decl> \" pollution)."),
            "got: {text:?}"
        );
    }

    #[test]
    fn test_soft_break_multiple_consecutive() {
        let md = "alpha\nbeta\ngamma";
        let (output, _) = render_markdown_ratatui_full(md, test_style::STYLE, false, None);
        let text = lines_to_text(&output.lines);
        assert_eq!(text, vec!["alpha beta gamma"], "got: {text:?}");
    }

    #[test]
    fn test_soft_break_around_inline_html_decl_tag() {
        // <decl> arrives as Event::InlineHtml; the following `\n` is the soft break.
        let md = "Foo bar <decl>\nbaz qux.";
        let (output, _) = render_markdown_ratatui_full(md, test_style::STYLE, true, None);
        let text = lines_to_text(&output.lines);
        assert_eq!(text.len(), 1, "got: {text:?}");
        assert!(text[0].contains("<decl> baz qux."), "got: {text:?}");
    }

    #[test]
    fn test_soft_break_ansi_render_path_no_mid_sentence_newline() {
        // render_ansi has its own `split('\n')` loop; verify the parser fix reaches it.
        let md = "Foo bar\nbaz qux.";
        let (output, _) = crate::render_markdown(md, test_style::STYLE, false, None);
        let body = output.trim_end_matches('\n');
        assert!(!body.contains('\n'), "{output:?}");
        assert!(body.contains("Foo bar baz qux."), "{output:?}");
    }

    #[test]
    fn test_hard_break_two_trailing_spaces_still_breaks() {
        let md = "Foo bar  \nbaz qux.";
        let (output, _) = render_markdown_ratatui_full(md, test_style::STYLE, false, None);
        let text = lines_to_text(&output.lines);
        assert_eq!(text.len(), 2, "got: {text:?}");
        assert_eq!(text[0].trim_end(), "Foo bar");
        assert_eq!(text[1], "baz qux.");
    }

    #[test]
    fn test_hard_break_backslash_still_breaks() {
        let md = "Foo bar\\\nbaz qux.";
        let (output, _) = render_markdown_ratatui_full(md, test_style::STYLE, false, None);
        let text = lines_to_text(&output.lines);
        assert_eq!(text.len(), 2, "got: {text:?}");
        assert!(text[0].starts_with("Foo bar"), "got: {text:?}");
        assert_eq!(text[1], "baz qux.");
    }

    #[test]
    fn test_inline_br_tag_still_breaks() {
        let md = "Foo bar<br>baz qux.";
        let (output, _) = render_markdown_ratatui_full(md, test_style::STYLE, true, None);
        let text = lines_to_text(&output.lines);
        let joined = text.join("\n");
        assert!(!joined.contains("<br>"), "{joined:?}");
        assert!(text.len() >= 2, "{text:?}");
    }

    #[test]
    fn test_code_block_internal_newlines_still_break() {
        let md = "```rust\nfn foo() {}\nfn bar() {}\n```\n";
        let (output, _) = render_markdown_ratatui_full(md, test_style::STYLE, false, None);
        let text = lines_to_text(&output.lines);
        let foo_idx = text.iter().position(|l| l.contains("fn foo() {}"));
        let bar_idx = text.iter().position(|l| l.contains("fn bar() {}"));
        assert!(
            foo_idx.is_some() && bar_idx.is_some() && foo_idx != bar_idx,
            "got: {text:?}",
        );
    }

    #[test]
    fn test_inline_code_with_real_newline_still_splits() {
        // Inline code's `\n` is part of the Event::Code source slice; no
        // SoftBreak fires, so the fix must not over-collapse it.
        let md = "foo `bar\nbaz` qux";
        let (output, _) = render_markdown_ratatui_full(md, test_style::STYLE, false, None);
        let text = lines_to_text(&output.lines);
        assert!(text.len() >= 2, "got: {text:?}");
        let bar_idx = text.iter().position(|l| l.contains("bar"));
        let baz_idx = text.iter().position(|l| l.contains("baz"));
        assert!(
            bar_idx.is_some() && baz_idx.is_some() && bar_idx != baz_idx,
            "got: {text:?}",
        );
    }

    #[test]
    fn test_soft_break_in_bullet_list_item_preserves_lines() {
        // Lazy continuation inside a list item is a soft break, but the
        // continuation indent belongs to a new visual line; collapsing
        // would leave stray indent whitespace mid-line.
        let md = "- first line\n  second line\n";
        let (output, _) = render_markdown_ratatui_full(md, test_style::STYLE, true, None);
        let text = lines_to_text(&output.lines);
        assert_eq!(text.len(), 2, "got: {text:?}");
        assert!(text[0].contains("first line"), "got: {text:?}");
        assert!(text[1].contains("second line"), "got: {text:?}");
    }

    #[test]
    fn test_soft_break_in_blockquote_preserves_lines() {
        // Continuation `>` markers belong to new visual lines; collapsing
        // would leak a stray `│` (pretty) or `>` (raw) mid-paragraph.
        let md = "> first line\n> second line\n";
        let (output, _) = render_markdown_ratatui_full(md, test_style::STYLE, true, None);
        let text = lines_to_text(&output.lines);
        assert_eq!(text.len(), 2, "got: {text:?}");
        assert!(text[0].contains("first line"), "got: {text:?}");
        assert!(text[1].contains("second line"), "got: {text:?}");
        assert!(
            !text[0].contains("second line") && !text[1].contains("first line"),
            "lines must not collapse: {text:?}",
        );
    }

    #[test]
    fn test_soft_break_crlf_range_preserves_length() {
        // pulldown emits SoftBreak with a 2-byte range for CRLF; the
        // transform must replace both bytes to keep the byte-length
        // invariant force transforms rely on in render_ansi.
        let md = "Foo bar\r\nbaz qux.";
        let (output, _) = render_markdown_ratatui_full(md, test_style::STYLE, false, None);
        let text = lines_to_text(&output.lines);
        assert_eq!(text, vec!["Foo bar  baz qux."], "got: {text:?}");

        let (ansi, _) = crate::render_markdown(md, test_style::STYLE, false, None);
        assert!(!ansi.trim_end_matches('\n').contains('\n'), "{ansi:?}");
        assert!(ansi.contains("Foo bar  baz qux."), "{ansi:?}");
    }

    #[test]
    fn test_source_map_preserved_for_soft_break_collapse() {
        let md = "Foo bar\nbaz qux.";
        let (output, _) = render_markdown_ratatui_full(md, test_style::STYLE, false, None);
        assert_eq!(output.lines.len(), 1);
        assert_eq!(output.line_source_map.len(), 1);
        assert!(
            output.line_source_map[0] <= 1,
            "got {}",
            output.line_source_map[0]
        );
    }

    #[test]
    fn test_source_map_preserved_for_hard_break() {
        let md = "Foo bar  \nbaz qux.";
        let (output, _) = render_markdown_ratatui_full(md, test_style::STYLE, false, None);
        assert_eq!(output.lines.len(), 2, "lines: {:?}", output.lines);
        assert_eq!(output.line_source_map, vec![0, 1]);
    }

    // Soft-break inside a markdown link is covered by
    // `hyperlinks::hyperlink_tests::soft_break_inside_link_text_preserves_column_range`.

    /// An indented fenced code block (common when an LLM nests code under a
    /// list, or simply indents the fence) must render the same as a
    /// non-indented one: pulldown-cmark strips the indentation from the
    /// content, and the renderer must hide the indentation on the opening
    /// fence line too. Regression test for the bug where the first content
    /// line kept its leading indentation and a spurious blank line was
    /// appended.
    #[test]
    fn test_indented_fenced_code_block_strips_indentation() {
        let syn = crate::syntax::test_syntect();
        let indented = "  ```cpp\n  cellContChargeLimits_S cellContChargeLimits;\n  cellChargeTables_S     cellChargeTables;\n  ```\n";

        let (output, _) =
            render_markdown_ratatui_full(indented, test_style::STYLE, true, Some(syn));
        let text = lines_to_text(&output.lines);

        assert_eq!(
            text,
            vec![
                "cellContChargeLimits_S cellContChargeLimits;",
                "cellChargeTables_S     cellChargeTables;",
            ],
            "indented code block should render dedented with no spurious blank line: {text:#?}",
        );
    }

    /// Indented and non-indented code blocks must produce identical pretty
    /// output (the indentation is purely structural).
    #[test]
    fn test_indented_code_block_matches_non_indented() {
        let syn = crate::syntax::test_syntect();
        let non_indented = "```rust\nfn main() {\n    let x = 1;\n}\n```\n";
        let indented = "   ```rust\n   fn main() {\n       let x = 1;\n   }\n   ```\n";

        let (out_plain, _) =
            render_markdown_ratatui_full(non_indented, test_style::STYLE, true, Some(syn));
        let (out_indent, _) =
            render_markdown_ratatui_full(indented, test_style::STYLE, true, Some(syn));

        assert_eq!(
            lines_to_text(&out_plain.lines),
            lines_to_text(&out_indent.lines),
            "indented fenced code block should match non-indented output",
        );
    }

    /// A fenced code block nested inside a list item renders dedented, with a
    /// single blank separator before the code and no leading indentation
    /// leaking onto the first code line.
    #[test]
    fn test_code_block_in_list_strips_indentation() {
        let syn = crate::syntax::test_syntect();
        let in_list = "1. Do this:\n   ```cpp\n   int x = 1;\n   int y = 2;\n   ```\n";

        let (output, _) = render_markdown_ratatui_full(in_list, test_style::STYLE, true, Some(syn));
        let text = lines_to_text(&output.lines);

        // No rendered line should begin with leftover indentation.
        let x_idx = text.iter().position(|l| l.contains("int x = 1;")).unwrap();
        let y_idx = text.iter().position(|l| l.contains("int y = 2;")).unwrap();
        assert_eq!(text[x_idx], "int x = 1;", "first code line: {text:#?}");
        assert_eq!(text[y_idx], "int y = 2;", "second code line: {text:#?}");
        assert!(
            text.last().is_some_and(|l| !l.is_empty()),
            "no spurious trailing blank line: {text:#?}",
        );
    }
}

/// Integration tests for LaTeX math rendering across all four delimiter
/// forms (`$...$`, `$$...$$`, `\(...\)`, `\[...\]`).
#[cfg(test)]
mod math_tests {
    use crate::style::test_style;
    use crate::{render_markdown, render_markdown_ratatui_full};

    fn lines_to_text(lines: &[ratatui::text::Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    fn pretty_lines(text: &str) -> Vec<String> {
        let (output, _) = render_markdown_ratatui_full(text, test_style::STYLE, true, None);
        lines_to_text(&output.lines)
    }

    #[test]
    fn dollar_inline_math_renders_unicode() {
        let lines = pretty_lines("Energy is $E = mc^2$ here.\n\n");
        assert_eq!(lines[0], "Energy is E = mc² here.", "got: {lines:#?}");
    }

    #[test]
    fn dollar_inline_math_hides_delimiters_in_pretty_mode() {
        let lines = pretty_lines("So $x_1 + x_2$ holds.\n\n");
        assert!(!lines[0].contains('$'), "got: {lines:#?}");
        assert!(lines[0].contains("x₁ + x₂"), "got: {lines:#?}");
    }

    #[test]
    fn raw_mode_preserves_inline_math_source() {
        let text = "Energy is $E = mc^2$ here.\n\n";
        let (output, _) = render_markdown_ratatui_full(text, test_style::STYLE, false, None);
        let lines = lines_to_text(&output.lines);
        assert!(lines[0].contains("$E = mc^2$"), "got: {lines:#?}");
    }

    #[test]
    fn paren_inline_math_renders_unicode() {
        let lines = pretty_lines("Sum \\(\\alpha + \\beta\\) end.\n\n");
        assert_eq!(lines[0], "Sum α + β end.", "got: {lines:#?}");
    }

    #[test]
    fn padded_paren_inline_math_renders_unicode() {
        // Regression: whitespace just inside `\( … \)` made the normalized
        // `$ … $` violate pulldown's dollar-math flanking rule, so it used to
        // render as raw `$ … $`. The normalizer now trims that padding.
        let lines = pretty_lines("Sum \\( x+y \\) end.\n\n");
        assert_eq!(lines[0], "Sum x+y end.", "got: {lines:#?}");
        assert!(
            !lines[0].contains('$'),
            "delimiters must be gone: {lines:#?}"
        );
    }

    #[test]
    fn padded_paren_inline_math_with_braces_renders() {
        let lines = pretty_lines("Set \\( S = \\{ x : x > 0 \\} \\) defined.\n\n");
        let joined = lines.join("\n");
        assert!(joined.contains("x : x > 0"), "got: {lines:#?}");
        assert!(!joined.contains('$'), "no raw dollar math: {lines:#?}");
    }

    #[test]
    fn paren_inline_math_in_list_item() {
        let lines = pretty_lines("- implies \\(p \\to q\\)\n- plain\n\n");
        assert!(lines[0].contains("implies p → q"), "got: {lines:#?}");
    }

    #[test]
    fn paren_inline_math_in_heading() {
        let lines = pretty_lines("## About \\(\\pi^2\\)\n\n");
        assert!(lines[0].contains("About π²"), "got: {lines:#?}");
    }

    #[test]
    fn dollar_inline_math_in_heading() {
        let lines = pretty_lines("# Energy $E=mc^2$\n\n");
        assert!(lines[0].contains("Energy E=mc²"), "got: {lines:#?}");
    }

    #[test]
    fn bracket_display_math_in_heading() {
        // pulldown-cmark keeps heading content inside a `Heading` block (no
        // wrapping paragraph), so the `\[...\]` source scan must also run on
        // heading end. `$$...$$` in the same position already converts via
        // `Event::DisplayMath`.
        let lines = pretty_lines("## Identity \\[x^2 + y^2 = z^2\\]\n\nAfter.\n\n");
        let joined = lines.join("\n");
        assert!(joined.contains("x² + y² = z²"), "got: {lines:#?}");
        assert!(!joined.contains("\\["), "got: {lines:#?}");
    }

    #[test]
    fn escaped_backslash_paren_is_not_math() {
        // `\\(` is a literal backslash followed by a paren — not a math open.
        let lines = pretty_lines("Literal \\\\(x\\\\) here.\n\n");
        let joined = lines.join("\n");
        // Pulldown renders the escapes; no Unicode conversion should occur
        // and the parens must survive.
        assert!(joined.contains("(x"), "got: {lines:#?}");
    }

    #[test]
    fn emphasis_inside_paren_math_falls_back() {
        // `*nope*` becomes emphasis, splitting the text events, so the span
        // is not converted; content must still render.
        let lines = pretty_lines("a \\(*nope*\\) b\n\n");
        let joined = lines.join("\n");
        assert!(joined.contains("nope"), "got: {lines:#?}");
        assert!(!joined.contains('→'), "got: {lines:#?}");
    }

    #[test]
    fn display_math_dollar_renders_block() {
        let lines =
            pretty_lines("Before.\n\n$$\n\\int_0^1 x \\, dx = \\frac{1}{2}\n$$\n\nAfter.\n\n");
        let math_line = lines
            .iter()
            .find(|l| l.contains('∫'))
            .expect("math block line");
        assert_eq!(math_line.trim(), "∫₀¹ x dx = ½", "got: {lines:#?}");
        // Block lines are indented.
        assert!(math_line.starts_with("  "), "got: {lines:#?}");
    }

    #[test]
    fn display_math_dollar_inline_form_renders_block() {
        let lines = pretty_lines("text $$x^2 + y^2 = z^2$$ more\n\n");
        let idx_text = lines.iter().position(|l| l.contains("text")).unwrap();
        let idx_math = lines
            .iter()
            .position(|l| l.contains("x² + y² = z²"))
            .unwrap();
        let idx_more = lines.iter().position(|l| l.contains("more")).unwrap();
        assert!(idx_text < idx_math, "text before math: {lines:#?}");
        assert!(idx_math < idx_more, "math before trailing text: {lines:#?}");
    }

    #[test]
    fn display_math_bracket_renders_block() {
        let text = "The AM-GM inequality:\n\n\\[\n\\frac{a+b}{2} \\ge \\sqrt{ab}\n\\]\n\nDone.\n\n";
        let lines = pretty_lines(text);
        let math_line = lines
            .iter()
            .find(|l| l.contains('≥'))
            .expect("math block line");
        assert_eq!(math_line.trim(), "(a+b)/2 ≥ √(ab)", "got: {lines:#?}");
        assert!(!lines.join("\n").contains("\\["), "got: {lines:#?}");
    }

    #[test]
    fn display_math_bracket_single_line_renders_block() {
        let lines = pretty_lines("\\[E = mc^2\\]\n\nAfter.\n\n");
        let math_line = lines.iter().find(|l| l.contains("mc²")).expect("math line");
        assert_eq!(math_line.trim(), "E = mc²", "got: {lines:#?}");
    }

    #[test]
    fn display_math_bracket_in_raw_mode_shows_canonical_dollars() {
        // The delimiter normalizer rewrites `\[…\]` → `$$…$$` before parsing, so
        // raw mode shows the canonical `$$` form (the math→Unicode conversion is
        // still a pretty-only overlay, so the TeX body itself is preserved).
        let text = "\\[E = mc^2\\]\n\n";
        let (output, _) = render_markdown_ratatui_full(text, test_style::STYLE, false, None);
        let joined = lines_to_text(&output.lines).join("\n");
        assert!(joined.contains("$$E = mc^2$$"), "got: {joined:?}");
        assert!(!joined.contains("\\["), "got: {joined:?}");
    }

    #[test]
    fn display_math_with_lone_equals_line_renders_block() {
        // Symptom 1: a lone `=` line inside a display span is a
        // CommonMark setext underline; unjoined, the first line became an H1
        // and the math rendered as raw TeX.
        let text = "The loss:\n\n\\[\n\\boxed{\n\\mathcal{L}_{\\text{MTP}}\n=\n\\sum_{i=0}^{2}\n\\gamma^{i}\\,\n\\mathbb{E}_{\\text{positions, mask}}\n\\Big[\n\\mathrm{KL}\\big(\n  \\mathrm{softmax}(z_{\\text{torso}}^{(s_i)})\n  \\;\\big\\|\\;\n  \\mathrm{softmax}(z_{\\text{draft}}^{(i)})\n\\big)\n\\Big]\n}\n\\]\n\nAfter.\n\n";
        let lines = pretty_lines(text);
        let joined = lines.join("\n");
        let math_line = lines
            .iter()
            .find(|l| l.contains('ℒ'))
            .expect("math block line");
        assert!(math_line.contains("ℒ_(MTP) = ∑ᵢ₌₀²"), "got: {lines:#?}");
        assert!(joined.contains("softmax(z_(torso)"), "got: {lines:#?}");
        assert!(!joined.contains('$'), "no raw delimiters: {lines:#?}");
        assert!(!joined.contains("\\["), "got: {lines:#?}");
        assert!(!joined.contains("boxed"), "got: {lines:#?}");
    }

    #[test]
    fn dollar_display_math_with_lone_equals_line_renders_block() {
        let lines = pretty_lines("$$\nx\n=\ny\n$$\n\nAfter.\n\n");
        let math_line = lines
            .iter()
            .find(|l| l.contains("x = y"))
            .expect("math block line");
        assert!(math_line.starts_with("  "), "block indent: {lines:#?}");
        assert!(!lines.join("\n").contains('$'), "got: {lines:#?}");
    }

    #[test]
    fn text_subscript_in_table_cell_renders_readable() {
        // Symptom 2: `p_{\text{torso}}` in a table cell became the
        // modifier-letter run `pₜₒᵣₛₒ`, which renders with visible gaps in
        // fonts lacking those glyphs.
        let text = "| Who | Soft-teacher |\n|-----|--------------|\n| **Torso** | \\(p_{\\text{torso}}(\\cdot \\mid T_0,\\ldots,T_i)\\) |\n\n";
        let lines = pretty_lines(text);
        let joined = lines.join("\n");
        assert!(joined.contains("p_(torso)(⋅ ∣ T₀,…,Tᵢ)"), "got: {lines:#?}");
        assert!(!joined.contains('ₜ'), "no modifier-letter runs: {lines:#?}");
    }

    #[test]
    fn aligned_environment_renders_multiple_lines() {
        let text =
            "\\[\n\\begin{aligned}\nf(x) &= x^2 \\\\\ng(x) &= 2x\n\\end{aligned}\n\\]\n\nEnd.\n\n";
        let lines = pretty_lines(text);
        let idx_f = lines.iter().position(|l| l.contains("f(x) = x²")).unwrap();
        let idx_g = lines.iter().position(|l| l.contains("g(x) = 2x")).unwrap();
        assert_eq!(idx_g, idx_f + 1, "consecutive block lines: {lines:#?}");
    }

    #[test]
    fn cases_environment_renders_brace_column() {
        let text = "$$\n|x| = \\begin{cases} x & x \\ge 0 \\\\ -x & x < 0 \\end{cases}\n$$\n\n";
        let lines = pretty_lines(text);
        let joined = lines.join("\n");
        assert!(joined.contains('⎧'), "got: {lines:#?}");
        assert!(joined.contains('⎩'), "got: {lines:#?}");
    }

    #[test]
    fn inline_math_in_table_cell_renders_unicode() {
        let text = "| Col | Math |\n|-----|------|\n| a | $x^2 + 1$ |\n\n";
        let lines = pretty_lines(text);
        let joined = lines.join("\n");
        assert!(joined.contains("x² + 1"), "got: {lines:#?}");
        assert!(!joined.contains('$'), "got: {lines:#?}");
    }

    #[test]
    fn paren_inline_math_in_table_cell_renders_unicode() {
        // `\(…\)` inside a table cell must convert. Previously the
        // backslash-form scanner was disabled inside tables, leaving raw TeX.
        // Normalization rewrites `\(…\)` → `$…$` before parsing, so the existing
        // in-cell `$` path converts it.
        let text = "| Mode | Metric |\n|------|--------|\n| Rate | \\(\\alpha + \\beta\\) |\n\n";
        let lines = pretty_lines(text);
        let joined = lines.join("\n");
        assert!(joined.contains("α + β"), "got: {lines:#?}");
        assert!(
            !joined.contains("\\("),
            "raw TeX must not survive: {lines:#?}"
        );
        assert!(!joined.contains('$'), "delimiters hidden: {lines:#?}");
    }

    #[test]
    fn bracket_display_math_in_table_cell_renders_unicode() {
        // `\[…\]` inside a cell renders single-line (no room for a block).
        let text = "| Col | Math |\n|-----|------|\n| a | \\[x^2\\] |\n\n";
        let lines = pretty_lines(text);
        let joined = lines.join("\n");
        assert!(joined.contains("x²"), "got: {lines:#?}");
        assert!(!joined.contains("\\["), "got: {lines:#?}");
    }

    #[test]
    fn paren_inline_math_in_blockquote_renders_unicode() {
        let lines = pretty_lines("> energy \\(E = mc^2\\) noted\n\n");
        let joined = lines.join("\n");
        assert!(joined.contains("E = mc²"), "got: {lines:#?}");
        assert!(!joined.contains("\\("), "got: {lines:#?}");
    }

    #[test]
    fn equation_environment_converts_to_block() {
        let text = "Before.\n\n\\begin{equation}\nE = mc^2\n\\end{equation}\n\nAfter.\n\n";
        let lines = pretty_lines(text);
        let joined = lines.join("\n");
        assert!(joined.contains("E = mc²"), "got: {lines:#?}");
        assert!(!joined.contains("\\begin"), "got: {lines:#?}");
    }

    #[test]
    fn latex_in_code_span_left_verbatim() {
        // Code spans are verbatim: `\(…\)` inside backticks must NOT convert.
        let lines = pretty_lines("inline `\\(x\\)` code\n\n");
        let joined = lines.join("\n");
        assert!(joined.contains("\\(x\\)"), "code must stay raw: {lines:#?}");
    }

    #[test]
    fn display_math_in_blockquote_renders() {
        let lines = pretty_lines("> Einstein: $$E = mc^2$$\n\n");
        let joined = lines.join("\n");
        assert!(joined.contains("E = mc²"), "got: {lines:#?}");
    }

    #[test]
    fn oversized_inline_math_falls_back_to_code_styling() {
        let body = "x".repeat(crate::latex::MAX_MATH_SOURCE_LEN + 10);
        let text = format!("Big ${body}$ end.\n\n");
        let lines = pretty_lines(&text);
        let joined = lines.join("\n");
        // Content is preserved verbatim (code-style fallback), delimiters
        // hidden in pretty mode.
        assert!(joined.contains(&body), "fallback must keep raw content");
    }

    #[test]
    fn bracket_math_inside_link_label_keeps_link_target() {
        // Option A normalizes `\[x\]` → `$$x$$` everywhere outside code, so (like
        // a literal `$$…$$`) display math inside a link label now converts. This
        // construct — display math inside a link label — is degenerate and
        // exceedingly rare in model output; the invariant we keep is that the
        // link target survives.
        let lines = pretty_lines("See [\\[x\\] notes](https://example.com) now.\n\n");
        let joined = lines.join("\n");
        assert!(
            joined.contains("https://example.com"),
            "link must survive: {lines:#?}"
        );
    }

    #[test]
    fn unclosed_math_renders_without_panic() {
        for text in [
            "open $a + b\n\n",
            "open $$a + b\n\n",
            "open \\(a + b\n\n",
            "open \\[a + b\n\n",
            "$$\n\\frac{1}{\n\n",
            "\\]\n\n",
            "\\)\n\n",
        ] {
            let _ = pretty_lines(text);
            let _ = render_markdown(text, test_style::STYLE, true, None);
            let _ = render_markdown(text, test_style::STYLE, false, None);
        }
    }

    #[test]
    fn ansi_render_includes_math_block_lines() {
        let (out, _) = render_markdown("before $$x^2$$ after\n\n", test_style::STYLE, true, None);
        assert!(out.contains("x²"), "got: {out:?}");
        // Block content starts on its own line.
        let plain = out.replace("\x1b[0m", "");
        let math_line = plain
            .lines()
            .find(|l| l.contains("x²"))
            .expect("math line in ANSI output");
        assert!(math_line.trim_start().starts_with("x²"), "got: {out:?}");
    }

    #[test]
    fn multiple_inline_math_spans_in_one_paragraph() {
        let lines = pretty_lines("Both $a^2$ and \\(b_1\\) and $c \\ne d$ work.\n\n");
        assert_eq!(
            lines[0], "Both a² and b₁ and c ≠ d work.",
            "got: {lines:#?}"
        );
    }

    #[test]
    fn greek_and_symbols_inline() {
        let lines =
            pretty_lines("Rate $\\lambda \\approx 0.5$ and set $S \\subseteq \\mathbb{R}^n$.\n\n");
        assert_eq!(lines[0], "Rate λ ≈ 0.5 and set S ⊆ ℝⁿ.", "got: {lines:#?}");
    }
}

/// Tests for HTML character-entity decoding in prose (`&lt;` → `<`, etc.).
#[cfg(test)]
mod entity_tests {
    use crate::style::test_style;
    use crate::{render_markdown, render_markdown_ratatui_full};

    fn lines_to_text(lines: &[ratatui::text::Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    fn pretty_lines(text: &str) -> Vec<String> {
        let (output, _) = render_markdown_ratatui_full(text, test_style::STYLE, true, None);
        lines_to_text(&output.lines)
    }

    fn raw_lines(text: &str) -> Vec<String> {
        let (output, _) = render_markdown_ratatui_full(text, test_style::STYLE, false, None);
        lines_to_text(&output.lines)
    }

    #[test]
    fn lt_gt_amp_decoded_in_prose() {
        let lines = pretty_lines("Use &lt;tag&gt; with a &amp; b.\n\n");
        assert_eq!(lines[0], "Use <tag> with a & b.", "got: {lines:#?}");
    }

    #[test]
    fn multiple_entities_one_paragraph() {
        let lines = pretty_lines("1 &lt; 2 &amp;&amp; 3 &gt; 2\n\n");
        assert_eq!(lines[0], "1 < 2 && 3 > 2", "got: {lines:#?}");
    }

    #[test]
    fn quote_and_apostrophe_entities() {
        let lines = pretty_lines("&quot;hello&quot; &amp; &#39;world&#39;\n\n");
        assert_eq!(lines[0], "\"hello\" & 'world'", "got: {lines:#?}");
    }

    #[test]
    fn numeric_decimal_and_hex_entities() {
        // &#60; = '<', &#x3e; = '>'
        let lines = pretty_lines("a &#60;b&#x3e; c\n\n");
        assert_eq!(lines[0], "a <b> c", "got: {lines:#?}");
    }

    #[test]
    fn full_html5_named_entities_decoded() {
        // Beyond the XML core set: these must decode in prose just like they
        // already do in table cells (via pulldown), keeping the two consistent.
        let lines = pretty_lines("&mdash; &copy; &hellip; &rarr; &times;\n\n");
        assert_eq!(lines[0], "— © … → ×", "got: {lines:#?}");
    }

    #[test]
    fn nbsp_decodes_to_no_break_space() {
        let lines = pretty_lines("a&nbsp;b\n\n");
        assert_eq!(lines[0], "a\u{a0}b", "got: {lines:#?}");
    }

    #[test]
    fn control_char_entities_are_not_injected() {
        // ESC / BEL / NUL / CR must never be substituted into terminal output;
        // the source stays literal instead.
        for (src, literal) in [
            ("x &#27; y\n\n", "&#27;"),
            ("x &#x1b; y\n\n", "&#x1b;"),
            ("x &#7; y\n\n", "&#7;"),
            ("x &#0; y\n\n", "&#0;"),
        ] {
            let lines = pretty_lines(src);
            let joined = lines.join("\n");
            assert!(
                joined.contains(literal),
                "control entity must stay literal: src={src:?} got={lines:#?}"
            );
            assert!(
                !joined.chars().any(|c| c.is_control() && c != '\n'),
                "no control char injected: src={src:?} got={lines:#?}"
            );
        }
    }

    #[test]
    fn entity_inside_link_text_decodes_and_keeps_link() {
        let lines = pretty_lines("See [a &lt; b](https://example.com) end.\n\n");
        let joined = lines.join("\n");
        assert!(joined.contains("a < b"), "link text decoded: {lines:#?}");
        assert!(
            joined.contains("https://example.com"),
            "link url survives: {lines:#?}"
        );
        assert!(!joined.contains("&lt;"), "no literal entity: {lines:#?}");
    }

    #[test]
    fn entity_inside_inline_math_does_not_corrupt() {
        // The entity sits inside a `\(...\)` math span; the math transform owns
        // those bytes, so the entity scan must not add an overlapping transform.
        let lines = pretty_lines("eq \\(a &lt; b\\) end\n\n");
        let joined = lines.join("\n");
        assert!(joined.contains("end"), "trailing text intact: {lines:#?}");
        // No doubled fragments from overlapping transforms.
        assert!(!joined.contains("endend"), "no double emit: {lines:#?}");
    }

    #[test]
    fn raw_mode_preserves_entity_source() {
        let lines = raw_lines("Use &lt;tag&gt; here.\n\n");
        assert!(
            lines[0].contains("&lt;tag&gt;"),
            "raw mode must keep source: {lines:#?}"
        );
    }

    #[test]
    fn entities_decoded_inside_emphasis_and_heading() {
        let bold = pretty_lines("**a &lt; b**\n\n");
        assert_eq!(bold[0], "a < b", "got: {bold:#?}");
        let heading = pretty_lines("## Compare &lt;T&gt;\n\n");
        assert!(
            heading.iter().any(|l| l.contains("Compare <T>")),
            "got: {heading:#?}"
        );
    }

    #[test]
    fn entities_left_literal_in_code() {
        // Inline code and fenced blocks are intentionally verbatim.
        let inline = pretty_lines("call `vec&lt;i32&gt;` now.\n\n");
        assert!(
            inline.iter().any(|l| l.contains("vec&lt;i32&gt;")),
            "inline code stays literal: {inline:#?}"
        );
        let fenced = pretty_lines("```\nGeneric&lt;T&gt;\n```\n\n");
        assert!(
            fenced.iter().any(|l| l.contains("Generic&lt;T&gt;")),
            "code block stays literal: {fenced:#?}"
        );
    }

    #[test]
    fn unknown_or_bare_ampersand_untouched() {
        // No semicolon, unknown name, and a lone `&` must all pass through.
        let lines = pretty_lines("Tom &amp Jerry &unknown; plain & text\n\n");
        assert_eq!(
            lines[0], "Tom &amp Jerry &unknown; plain & text",
            "got: {lines:#?}"
        );
    }

    #[test]
    fn entity_in_table_cell_still_decodes() {
        // Regression guard: the table cell path already decoded entities; this
        // must keep working alongside the new prose path.
        let lines = pretty_lines("| H |\n|---|\n| a &lt; b |\n\n");
        let joined = lines.join("\n");
        assert!(joined.contains("a < b"), "got: {lines:#?}");
    }

    #[test]
    fn no_panic_on_entity_edge_cases() {
        for text in [
            "&\n\n",
            "&;\n\n",
            "&#;\n\n",
            "&#x;\n\n",
            "&#0;\n\n",
            "&#27;\n\n",
            "&#x1b;\n\n",
            "trailing &lt",
            "&lt;&gt;&amp;",
            "&#xZZ;\n\n",
            "&CounterClockwiseContourIntegral;\n\n",
            // Multi-byte UTF-8 mixed with `&` in various positions: the inner
            // loop only advances over ASCII bytes, so it must not slice
            // through a multi-byte sequence.
            "& é &lt; ñ\n\n",
            "café &lt; thé\n\n",
            "🦀 & 🦀\n\n",
            "&amp;🦀&lt;\n\n",
            // Repeated `&` runs (worst case for the O(n²) bound).
            "&&&&&&&&&&&&\n\n",
            &("&".repeat(200) + "\n\n"),
        ] {
            let _ = pretty_lines(text);
            let _ = render_markdown(text, test_style::STYLE, true, None);
            let _ = render_markdown(text, test_style::STYLE, false, None);
        }
    }
}
