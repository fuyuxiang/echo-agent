//! Turning a script's bytes into the lines the frame paints. Separate from
//! `echo-agent-pager-render`'s `vte::Perform`, which discards OSC and quantizes
//! colour; this row needs OSC 8 spans in absolute screen columns.

use std::borrow::Cow;
use std::sync::Arc;

use ansi_to_tui::IntoText;
use ratatui::text::{Line, Span};

use super::painted_width;

pub const MAX_STATUS_LINE_LINES: u16 = 5;

/// Sanitized on arrival rather than per frame: tabs expanded, escapes dropped,
/// targets checked, lines cut. The frame still applies theme and width.
#[derive(Debug, Clone, PartialEq)]
pub struct SanitizedText {
    pub(super) lines: Vec<Line<'static>>,
    pub(super) links: Vec<CommandLink>,
}

/// Bounds the scan, not the paint: each link is measured from its line start,
/// so many links cost quadratic time. Characters, escapes included, never
/// columns.
const MAX_SANITIZED_CHARS: usize = 1024;

impl SanitizedText {
    #[must_use]
    pub fn new(text: &str) -> Self {
        let expanded = echo_agent_pager_render::appearance::expand_tabs(text);
        let (clean, mut links) = extract_osc8_links(&clamp_lines(&expanded));
        let mut lines = match clean.as_str().into_text() {
            Ok(parsed) if !parsed.lines.is_empty() => parsed.lines,
            _ => vec![Line::from(Span::raw(clean))],
        };
        lines.truncate(MAX_STATUS_LINE_LINES as usize);
        links.retain(|link| usize::from(link.line) < lines.len());
        Self { lines, links }
    }

    pub(super) fn line_count(&self) -> u16 {
        self.lines.len().max(1) as u16
    }
}

/// Drops lines beyond [`MAX_STATUS_LINE_LINES`] and cuts each survivor to
/// [`MAX_SANITIZED_CHARS`] characters. The byte length gates the work, since a
/// string under the cap in bytes is under it in characters too.
fn clamp_lines(text: &str) -> Cow<'_, str> {
    if text.len() <= MAX_SANITIZED_CHARS {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len().min(MAX_SANITIZED_CHARS * 8));
    for (index, line) in text
        .split('\n')
        .take(MAX_STATUS_LINE_LINES as usize)
        .enumerate()
    {
        if index > 0 {
            out.push('\n');
        }
        match line.char_indices().nth(MAX_SANITIZED_CHARS) {
            Some((cut, _)) => out.push_str(&line[..cut]),
            None => out.push_str(line),
        }
    }
    Cow::Owned(out)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CommandLink {
    pub(super) line: u16,
    pub(super) col_start: u16,
    pub(super) col_end: u16,
    pub(super) url: Arc<str>,
}

/// An open OSC 8 link. The offset is into the visible text, so its columns come
/// from painted glyphs.
struct OpenLink {
    line: u16,
    byte_start: usize,
    url: Arc<str>,
}

fn extract_osc8_links(text: &str) -> (String, Vec<CommandLink>) {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    // `out` keeps the styling escapes for `ansi_to_tui`. A CSI is not glyphs,
    // so only `visible` is measured.
    let mut visible = String::with_capacity(text.len());
    let mut links: Vec<CommandLink> = Vec::new();
    let mut open: Option<OpenLink> = None;
    let mut line: u16 = 0;
    let mut line_start = 0usize;
    let mut i = 0;

    // A newline closes any open link, so a link never outlives its line.
    let close_link = |visible: &str,
                      links: &mut Vec<CommandLink>,
                      open: &mut Option<OpenLink>,
                      line_start: usize,
                      line: u16| {
        let Some(link) = open.take() else { return };
        // Both columns are measured from `line_start`, which locates the link only
        // if it opened on this line.
        debug_assert_eq!(link.line, line, "a link outlived the line it opened on");
        let columns = |text: &str| u16::try_from(painted_width(text)).unwrap_or(u16::MAX);
        let col_start = columns(&visible[line_start..link.byte_start]);
        let col_end = columns(&visible[line_start..]);
        if col_end > col_start {
            links.push(CommandLink {
                line: link.line,
                col_start,
                col_end,
                url: link.url,
            });
        }
    };

    while i < chars.len() {
        let c = chars[i];
        if c == '\x1b' {
            match chars.get(i + 1).copied() {
                // Every OSC but a link is dropped: `ansi_to_tui` ends one at BEL only, so an
                // ST-terminated notification would eat the rest of the line.
                Some(']') => {
                    let (body_end, next_i) = string_sequence(&chars, i + 2);
                    let body: String = chars[i + 2..body_end].iter().collect();
                    if let Some(rest) = body.strip_prefix("8;") {
                        let uri = rest.split_once(';').map(|(_, u)| u).unwrap_or("");
                        close_link(&visible, &mut links, &mut open, line_start, line);
                        if let Some(url) = safe_link_target(uri) {
                            open = Some(OpenLink {
                                line,
                                byte_start: visible.len(),
                                url,
                            });
                        }
                    }
                    i = next_i;
                }
                Some('[') => {
                    let mut j = i + 2;
                    let mut final_byte = None;
                    while j < chars.len() {
                        let cc = chars[j];
                        // An unterminated sequence ends at the line: swallowing
                        // the newline counts a later link onto the wrong line.
                        if cc == '\n' {
                            break;
                        }
                        j += 1;
                        // ECMA-48 final byte. Stopping at an alphabetic
                        // swallows the text after `ESC [ 3 ~`.
                        if matches!(cc, '\u{40}'..='\u{7e}') {
                            final_byte = Some(cc);
                            break;
                        }
                    }
                    // Only SGR reaches the parser: `ansi_to_tui` eats any other CSI up to the
                    // next ASCII letter and that letter too, dropping a counted glyph.
                    if final_byte == Some('m') {
                        out.extend(&chars[i..j]);
                    }
                    i = j;
                }
                // DCS, SOS, PM and APC address the terminal, not the screen.
                // Passing one through paints its payload across the row.
                Some('P' | 'X' | '^' | '_') => i = string_sequence(&chars, i + 2).1,
                // Charset selection and two-character escapes: `tput sgr0`
                // emits `ESC ( B ESC [ m`; an `ESC (` left here paints `(B`.
                _ => {
                    let mut j = i + 1;
                    while j < chars.len() && matches!(chars[j], '\u{20}'..='\u{2f}') {
                        j += 1;
                    }
                    if j < chars.len() && matches!(chars[j], '\u{30}'..='\u{7e}') {
                        j += 1;
                    }
                    i = j;
                }
            }
            continue;
        }
        if c == '\n' {
            close_link(&visible, &mut links, &mut open, line_start, line);
            out.push('\n');
            visible.push('\n');
            line = line.saturating_add(1);
            line_start = visible.len();
            i += 1;
            continue;
        }
        out.push(c);
        visible.push(c);
        i += 1;
    }
    close_link(&visible, &mut links, &mut open, line_start, line);
    (out, links)
}

/// The target to hand the terminal, or `None` when the scheme is not allowed.
/// Trimmed first, so the checked and emitted strings are one string.
fn safe_link_target(uri: &str) -> Option<Arc<str>> {
    let uri = uri.trim();
    if uri.is_empty() || uri.contains(char::is_whitespace) || uri.contains(char::is_control) {
        return None;
    }
    if !crate::app::link_opener::is_safe_to_open(
        uri,
        crate::terminal::hyperlinks::SchemeFilter::Standard,
    ) {
        return None;
    }
    // The shared gate stops at the scheme, enough for EchoAgent's own text but not a
    // script's: `http://` with no host passes it and opens a browser on nothing.
    let web = uri.split_once("://").is_some_and(|(scheme, _)| {
        scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
    });
    if web && !url::Url::parse(uri).is_ok_and(|parsed| parsed.host_str().is_some()) {
        return None;
    }
    Some(Arc::from(uri))
}

/// Where a string sequence's body ends and scanning resumes. BEL and ST close
/// one; a newline ends an unterminated one at the line.
fn string_sequence(chars: &[char], start: usize) -> (usize, usize) {
    let mut i = start;
    while i < chars.len() {
        match chars[i] {
            '\x07' => return (i, i + 1),
            '\x1b' if chars.get(i + 1) == Some(&'\\') => return (i, i + 2),
            '\n' => return (i, i),
            _ => i += 1,
        }
    }
    (chars.len(), chars.len())
}

#[cfg(test)]
#[path = "sanitize_tests.rs"]
mod tests;
