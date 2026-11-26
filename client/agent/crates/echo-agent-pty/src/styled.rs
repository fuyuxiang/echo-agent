//! Styled output formatters — styled JSON and HTML rendering.
//!
//! Converts the terminal grid into structured representations that preserve
//! color and text attribute information for LLM consumption.

use alacritty_terminal::grid::Row;
use alacritty_terminal::index::Column;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::vte::ansi::{Color, NamedColor};

use crate::term::{CursorPosition, ScreenOpts, TerminalSize};

/// A single styled run of text with uniform attributes.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StyledRun {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fg: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bg: Option<String>,
    #[serde(skip_serializing_if = "is_false")]
    pub bold: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub italic: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub underline: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub strikeout: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub dim: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub inverse: bool,
}

fn is_false(v: &bool) -> bool {
    !v
}

/// A single line of styled content.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StyledLine {
    pub line: usize,
    pub runs: Vec<StyledRun>,
}

/// Style attributes for a cell, used for run-length coalescing.
#[derive(Debug, Clone, PartialEq)]
struct CellStyle {
    fg: Option<String>,
    bg: Option<String>,
    bold: bool,
    italic: bool,
    underline: bool,
    strikeout: bool,
    dim: bool,
    inverse: bool,
}

impl CellStyle {
    fn from_cell(cell: &Cell) -> Self {
        let flags = cell.flags;
        Self {
            fg: color_to_css(&cell.fg),
            bg: color_to_css(&cell.bg),
            bold: flags.contains(Flags::BOLD),
            italic: flags.contains(Flags::ITALIC),
            underline: flags.intersects(Flags::ALL_UNDERLINES),
            strikeout: flags.contains(Flags::STRIKEOUT),
            dim: flags.contains(Flags::DIM),
            inverse: flags.contains(Flags::INVERSE),
        }
    }

    fn to_run(&self, text: String) -> StyledRun {
        StyledRun {
            text,
            fg: self.fg.clone(),
            bg: self.bg.clone(),
            bold: self.bold,
            italic: self.italic,
            underline: self.underline,
            strikeout: self.strikeout,
            dim: self.dim,
            inverse: self.inverse,
        }
    }
}

/// Extract a styled line from a grid row by coalescing consecutive cells
/// with identical style into runs.
pub fn extract_styled_line(
    row: &Row<Cell>,
    col_start: usize,
    col_end: usize,
    line_number: usize,
    cursor: &CursorPosition,
    opts: &ScreenOpts,
) -> StyledLine {
    let mut runs = Vec::new();
    let mut current_text = String::new();
    let mut current_style: Option<CellStyle> = None;

    for col_idx in col_start..col_end {
        let cell = &row[Column(col_idx)];

        // Skip wide char spacers.
        if cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        {
            continue;
        }

        let style = CellStyle::from_cell(cell);

        // Determine the character to emit.
        let is_cursor = cursor.row == line_number && cursor.col == col_idx + 1;
        let ch = if is_cursor && let Some(cursor_char) = opts.cursor_char {
            cursor_char
        } else {
            cell.c
        };

        // If style changed, flush the current run.
        if let Some(ref cur) = current_style {
            if *cur != style {
                if !current_text.is_empty() {
                    runs.push(cur.to_run(std::mem::take(&mut current_text)));
                }
                current_style = Some(style);
            }
        } else {
            current_style = Some(style);
        }

        current_text.push(ch);
        if let Some(zw) = cell.zerowidth() {
            for &c in zw {
                current_text.push(c);
            }
        }
    }

    // Flush remaining.
    if !current_text.is_empty()
        && let Some(ref style) = current_style
    {
        runs.push(style.to_run(current_text));
    }

    // Trim trailing whitespace-only runs with default style.
    while runs
        .last()
        .is_some_and(|r| r.text.trim().is_empty() && r.fg.is_none() && r.bg.is_none() && !r.bold)
    {
        runs.pop();
    }

    StyledLine {
        line: line_number,
        runs,
    }
}

/// Render styled lines as an HTML document.
pub fn render_html(lines: &[StyledLine], _cursor: &CursorPosition, _size: &TerminalSize) -> String {
    let mut html = String::new();
    html.push_str("<!DOCTYPE html>\n<html>\n<head>\n");
    html.push_str("<meta charset=\"utf-8\">\n");
    html.push_str("<style>\n");
    html.push_str("body { background: #1e1e1e; margin: 0; padding: 16px; }\n");
    html.push_str("pre { font-family: 'Menlo', 'Monaco', 'Courier New', monospace; ");
    html.push_str("font-size: 14px; line-height: 1.4; color: #d4d4d4; margin: 0; }\n");
    html.push_str(".cursor { background: #d4d4d4; color: #1e1e1e; }\n");
    html.push_str(".bold { font-weight: bold; }\n");
    html.push_str(".italic { font-style: italic; }\n");
    html.push_str(".underline { text-decoration: underline; }\n");
    html.push_str(".strikeout { text-decoration: line-through; }\n");
    html.push_str(".dim { opacity: 0.5; }\n");
    html.push_str("</style>\n</head>\n<body>\n<pre>");

    for styled_line in lines {
        html.push_str("<div class=\"line\">");
        for run in &styled_line.runs {
            let mut classes = Vec::new();
            let mut styles = Vec::new();

            if run.bold {
                classes.push("bold");
            }
            if run.italic {
                classes.push("italic");
            }
            if run.underline {
                classes.push("underline");
            }
            if run.strikeout {
                classes.push("strikeout");
            }
            if run.dim {
                classes.push("dim");
            }

            if let Some(ref fg) = run.fg {
                styles.push(format!("color:{fg}"));
            }
            if let Some(ref bg) = run.bg {
                styles.push(format!("background:{bg}"));
            }

            if classes.is_empty() && styles.is_empty() {
                html.push_str(&html_escape(&run.text));
            } else {
                html.push_str("<span");
                if !classes.is_empty() {
                    html.push_str(&format!(" class=\"{}\"", classes.join(" ")));
                }
                if !styles.is_empty() {
                    html.push_str(&format!(" style=\"{}\"", styles.join(";")));
                }
                html.push('>');
                html.push_str(&html_escape(&run.text));
                html.push_str("</span>");
            }
        }
        html.push_str("</div>\n");
    }

    html.push_str("</pre>\n</body>\n</html>\n");
    html
}

/// Convert a terminal `Color` to a CSS color string.
fn color_to_css(color: &Color) -> Option<String> {
    match color {
        Color::Spec(rgb) => Some(format!("#{:02x}{:02x}{:02x}", rgb.r, rgb.g, rgb.b)),
        Color::Named(name) => named_color_to_css(name),
        Color::Indexed(idx) => Some(indexed_color_to_css(*idx)),
    }
}

/// Map named ANSI colors to CSS hex values (standard xterm palette).
fn named_color_to_css(name: &NamedColor) -> Option<String> {
    let hex = match name {
        NamedColor::Black => "#000000",
        NamedColor::Red => "#cd0000",
        NamedColor::Green => "#00cd00",
        NamedColor::Yellow => "#cdcd00",
        NamedColor::Blue => "#0000ee",
        NamedColor::Magenta => "#cd00cd",
        NamedColor::Cyan => "#00cdcd",
        NamedColor::White => "#e5e5e5",
        NamedColor::BrightBlack => "#7f7f7f",
        NamedColor::BrightRed => "#ff0000",
        NamedColor::BrightGreen => "#00ff00",
        NamedColor::BrightYellow => "#ffff00",
        NamedColor::BrightBlue => "#5c5cff",
        NamedColor::BrightMagenta => "#ff00ff",
        NamedColor::BrightCyan => "#00ffff",
        NamedColor::BrightWhite => "#ffffff",
        NamedColor::Foreground | NamedColor::Background | NamedColor::Cursor => return None,
        _ => return None,
    };
    Some(hex.to_string())
}

/// Map 256-color palette index to CSS hex.
fn indexed_color_to_css(idx: u8) -> String {
    match idx {
        0 => "#000000".into(),
        1 => "#cd0000".into(),
        2 => "#00cd00".into(),
        3 => "#cdcd00".into(),
        4 => "#0000ee".into(),
        5 => "#cd00cd".into(),
        6 => "#00cdcd".into(),
        7 => "#e5e5e5".into(),
        8 => "#7f7f7f".into(),
        9 => "#ff0000".into(),
        10 => "#00ff00".into(),
        11 => "#ffff00".into(),
        12 => "#5c5cff".into(),
        13 => "#ff00ff".into(),
        14 => "#00ffff".into(),
        15 => "#ffffff".into(),
        // 216 color cube (indices 16-231).
        16..=231 => {
            let idx = idx - 16;
            let r = idx / 36;
            let g = (idx % 36) / 6;
            let b = idx % 6;
            let to_rgb = |v: u8| if v == 0 { 0u8 } else { 55 + 40 * v };
            format!("#{:02x}{:02x}{:02x}", to_rgb(r), to_rgb(g), to_rgb(b))
        }
        // Grayscale ramp (indices 232-255).
        232..=255 => {
            let v = 8 + 10 * (idx - 232);
            format!("#{:02x}{:02x}{:02x}", v, v, v)
        }
    }
}

/// Escape HTML special characters.
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}
