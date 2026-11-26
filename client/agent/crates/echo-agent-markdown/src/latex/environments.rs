//! `\\begin{env}...\\end{env}` environments: matrices, cases, alignments.

use crate::buffers::unicode_display_width;

use super::Mode;
use super::commands::{render_atom, take_brace_arg};
use super::cursor::Cursor;
use super::math_box::MathBox;

/// Render `\begin{env}...\end{env}`. The `\begin` name was already consumed.
pub(super) fn render_environment(
    cursor: &mut Cursor<'_>,
    out: &mut MathBox,
    depth: usize,
    mode: Mode,
) {
    let Some(env_name) = take_brace_arg(cursor) else {
        return;
    };
    let env_name = env_name.trim().trim_end_matches('*');

    // Capture body source until the matching `\end{name}`, tracking nesting
    // of same-named environments. Scans raw source from the cursor.
    let body_start = cursor.pos;
    let mut body_end = cursor.src.len();
    let mut resume = cursor.src.len();
    let mut nest = 0usize;
    let mut search = cursor.pos;
    while search < cursor.src.len() {
        let rest = &cursor.src[search..];
        let Some(rel) = rest.find('\\') else {
            break;
        };
        let bs_pos = search + rel;
        let after_bs = &cursor.src[bs_pos + 1..];
        let kw_len = if command_at(after_bs, "begin") {
            "begin".len()
        } else if command_at(after_bs, "end") {
            "end".len()
        } else {
            // Not begin/end: skip the backslash and the char after it (so
            // `\\` and `\{` never confuse the scan).
            let skip = after_bs.chars().next().map_or(0, char::len_utf8);
            search = bs_pos + 1 + skip.max(1);
            continue;
        };
        let is_begin = kw_len == "begin".len();
        let mut probe = Cursor {
            src: cursor.src,
            pos: bs_pos + 1 + kw_len,
        };
        let arg = take_brace_arg(&mut probe).map(|a| a.trim().trim_end_matches('*'));
        if arg == Some(env_name) {
            if is_begin {
                nest += 1;
            } else if nest == 0 {
                body_end = bs_pos;
                resume = probe.pos;
                break;
            } else {
                nest -= 1;
            }
        }
        search = probe.pos.max(bs_pos + 1 + kw_len);
    }
    cursor.pos = resume;
    let mut body = &cursor.src[body_start..body_end.min(cursor.src.len())];

    // Optional column spec for array environments: `\begin{array}{ll}`.
    if env_name == "array" || env_name == "alignat" {
        let mut probe = Cursor::new(body);
        probe.skip_ws();
        if probe.peek() == Some('{') {
            probe.bump();
            let _ = probe.read_group_body();
            body = &body[probe.pos..];
        }
    }
    let rows = env_rows_to_strings(body, env_name, out.flat, depth, mode);
    out.hcat_rows(rows);
}

/// `true` if `rest` starts with command word `word` NOT followed by another
/// ASCII letter (so `\endx` is not mistaken for `\end`).
fn command_at(rest: &str, word: &str) -> bool {
    rest.starts_with(word)
        && !rest[word.len()..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
}

/// Split an environment body into rows (`\\`) and cells (`&`) at brace and
/// environment depth 0, render each cell, then lay the rows out according to
/// the environment. Returns one string per visual row; the caller attaches
/// them as a box. In `flat` mode, matrix/cases environments render as a
/// single row with `; ` between matrix rows.
fn env_rows_to_strings(
    body: &str,
    env_name: &str,
    flat: bool,
    depth: usize,
    mode: Mode,
) -> Vec<String> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut cell_start = 0usize;
    let mut brace_depth = 0usize;
    let mut env_depth = 0usize;
    let bytes = body.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => {
                if bytes.get(i + 1) == Some(&b'\\') {
                    if brace_depth == 0 && env_depth == 0 {
                        row.push(body[cell_start..i].to_string());
                        rows.push(std::mem::take(&mut row));
                        i += 2;
                        cell_start = i;
                        continue;
                    }
                    i += 2;
                    continue;
                }
                let rest = &body[i + 1..];
                if command_at(rest, "begin") {
                    env_depth += 1;
                } else if command_at(rest, "end") {
                    env_depth = env_depth.saturating_sub(1);
                }
                // Skip the backslash plus the char after it so escaped
                // delimiters (`\&`, `\{`, `\}`) never affect depth/splits.
                let skip = rest.chars().next().map_or(0, char::len_utf8);
                i += 1 + skip.max(1);
                continue;
            }
            b'{' => brace_depth += 1,
            b'}' => brace_depth = brace_depth.saturating_sub(1),
            b'&' if brace_depth == 0 && env_depth == 0 => {
                row.push(body[cell_start..i].to_string());
                cell_start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    row.push(body[cell_start.min(bytes.len())..].to_string());
    rows.push(row);

    // Render each cell, drop fully-empty rows.
    let mut rendered_rows: Vec<Vec<String>> = rows
        .into_iter()
        .map(|cells| {
            cells
                .into_iter()
                .map(|c| render_atom(c.trim(), depth, mode).trim().to_string())
                .collect::<Vec<_>>()
        })
        .collect();
    rendered_rows.retain(|cells| cells.iter().any(|c| !c.is_empty()));
    if rendered_rows.is_empty() {
        return Vec::new();
    }

    let is_matrix = matches!(
        env_name,
        "matrix"
            | "pmatrix"
            | "bmatrix"
            | "Bmatrix"
            | "vmatrix"
            | "Vmatrix"
            | "smallmatrix"
            | "array"
    );
    let n_rows = rendered_rows.len();

    if is_matrix {
        // Flat (inline) mode: one row, single delimiter pair, rows joined
        // with `; ` — `(1  2; 3  4)`.
        if flat {
            let inner = rendered_rows
                .iter()
                .map(|cells| cells.join("  "))
                .collect::<Vec<_>>()
                .join("; ");
            // Single-row delimiter pair; plain `matrix` has none (' ').
            let (l, r) = matrix_delims(env_name, 0, 1);
            let mut s = String::new();
            if l != ' ' {
                s.push(l);
            }
            s.push_str(&inner);
            if r != ' ' {
                s.push(r);
            }
            return vec![s];
        }
        // Pad columns to equal width so rows align.
        let n_cols = rendered_rows.iter().map(Vec::len).max().unwrap_or(0);
        let mut widths = vec![0usize; n_cols];
        for cells in &rendered_rows {
            for (i, cell) in cells.iter().enumerate() {
                widths[i] = widths[i].max(unicode_display_width(cell));
            }
        }
        rendered_rows
            .iter()
            .enumerate()
            .map(|(row_idx, cells)| {
                let mut content = String::new();
                for (i, cell) in cells.iter().enumerate() {
                    if i > 0 {
                        content.push_str("  ");
                    }
                    content.push_str(cell);
                    if i + 1 < cells.len() {
                        let pad = widths[i].saturating_sub(unicode_display_width(cell));
                        content.push_str(&" ".repeat(pad));
                    }
                }
                let (l, r) = matrix_delims(env_name, row_idx, n_rows);
                format!("{l}{content}{r}")
            })
            .collect()
    } else if env_name == "cases" {
        if flat {
            let inner = rendered_rows
                .iter()
                .map(|cells| cells.join("  "))
                .collect::<Vec<_>>()
                .join("; ");
            return vec![format!("{{{inner}}}")];
        }
        rendered_rows
            .iter()
            .enumerate()
            .map(|(row_idx, cells)| {
                let brace = cases_brace(row_idx, n_rows);
                format!("{brace} {}", cells.join("  "))
            })
            .collect()
    } else {
        // aligned/align/gather/split/equation/…: `&` is an invisible
        // alignment marker; rejoin cells with a single space. One string per
        // row; the caller's box attachment (or flat `; ` join) handles the
        // rest.
        rendered_rows
            .iter()
            .map(|cells| {
                let mut s = cells
                    .iter()
                    .filter(|c| !c.is_empty())
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" ");
                // Collapse any double spaces introduced around markers.
                while s.contains("  ") {
                    s = s.replace("  ", " ");
                }
                s
            })
            .collect()
    }
}

/// Per-row delimiters for matrix-family environments.
fn matrix_delims(env: &str, row: usize, n_rows: usize) -> (char, char) {
    let single = n_rows == 1;
    let first = row == 0;
    let last = row + 1 == n_rows;
    match env {
        "pmatrix" => {
            if single {
                ('(', ')')
            } else if first {
                ('⎛', '⎞')
            } else if last {
                ('⎝', '⎠')
            } else {
                ('⎜', '⎟')
            }
        }
        "bmatrix" | "array" => {
            if single {
                ('[', ']')
            } else if first {
                ('⎡', '⎤')
            } else if last {
                ('⎣', '⎦')
            } else {
                ('⎢', '⎥')
            }
        }
        "Bmatrix" => {
            if single {
                ('{', '}')
            } else if first {
                ('⎧', '⎫')
            } else if last {
                ('⎩', '⎭')
            } else {
                ('⎨', '⎬')
            }
        }
        "vmatrix" | "Vmatrix" => ('│', '│'),
        _ => (' ', ' '),
    }
}

/// Left-brace column char for `cases` rows.
fn cases_brace(row: usize, n_rows: usize) -> char {
    if n_rows == 1 {
        '{'
    } else if row == 0 {
        '⎧'
    } else if row + 1 == n_rows {
        '⎩'
    } else if row == n_rows / 2 {
        '⎨'
    } else {
        '⎪'
    }
}
