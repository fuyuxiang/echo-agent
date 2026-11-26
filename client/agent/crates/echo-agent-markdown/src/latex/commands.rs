//! Core renderer: sequences, commands, scripts, fractions, accents.

use std::fmt::Write as _;

use super::cursor::Cursor;
use super::environments::render_environment;
use super::math_box::MathBox;
use super::symbols::{
    map_mathbb, map_mathbf, map_mathcal, map_mathfrak, symbol, to_subscript, to_superscript,
};
use super::{MAX_DEPTH, Mode};

/// Render an atom's source to a flat (single-line) Unicode string.
///
/// Atoms are arguments to commands (fraction sides, script bodies, accent
/// targets); they always render flat — multi-row content inside them joins
/// with `; `.
pub(super) fn render_atom(atom: &str, depth: usize, mode: Mode) -> String {
    let mut cursor = Cursor::new(atom);
    let mut out = MathBox::new(true);
    render_sequence(&mut cursor, &mut out, depth + 1, mode, None);
    out.into_lines().concat()
}

/// Core renderer: walks `cursor`, appending Unicode to `out`.
///
/// `stop_at` optionally terminates the sequence at an unbalanced `}` (used
/// when rendering inside a group whose `{` was consumed by the caller).
pub(super) fn render_sequence(
    cursor: &mut Cursor<'_>,
    out: &mut MathBox,
    depth: usize,
    mode: Mode,
    stop_at: Option<char>,
) {
    while let Some(ch) = cursor.peek() {
        if Some(ch) == stop_at {
            cursor.bump();
            return;
        }
        match ch {
            '\\' => {
                cursor.bump();
                render_command(cursor, out, depth, mode);
            }
            '{' => {
                cursor.bump();
                if depth >= MAX_DEPTH {
                    // Too deep: render the group body flat, without recursing.
                    out.push_str(cursor.read_group_body());
                } else {
                    // Render the group body into the same box so environments
                    // inside groups keep their 2D layout.
                    let body = cursor.read_group_body();
                    let mut sub = Cursor::new(body);
                    render_sequence(&mut sub, out, depth + 1, mode, None);
                }
            }
            '}' => {
                // Unbalanced closing brace: drop it.
                cursor.bump();
            }
            '^' => {
                cursor.bump();
                render_script(cursor, out, depth, mode, Script::Super);
            }
            '_' => {
                cursor.bump();
                render_script(cursor, out, depth, mode, Script::Sub);
            }
            '~' => {
                cursor.bump();
                out.push(' ');
            }
            '&' => {
                // Alignment marker outside an environment: drop.
                cursor.bump();
            }
            '$' => {
                // Stray math delimiter inside math: drop.
                cursor.bump();
            }
            '-' if mode == Mode::Math => {
                cursor.bump();
                out.push('−');
            }
            '\'' if mode == Mode::Math => {
                cursor.bump();
                out.push('′');
            }
            c if c.is_whitespace() => {
                cursor.skip_ws();
                // TeX collapses whitespace runs (including newlines) to
                // nothing semantically; keep a single space for readability.
                if !out.at_line_start() && !out.ends_with_space() {
                    out.push(' ');
                }
            }
            c => {
                cursor.bump();
                out.push(c);
            }
        }
    }
}

/// Which script position is being rendered.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Script {
    Super,
    Sub,
}

/// Render `^atom` / `_atom` using Unicode script chars when every char of
/// the rendered atom has a script form; otherwise `^x` / `^(...)` fallback.
///
/// Word-like atoms take the fallback even when fully mappable: labels such as
/// `p_{\text{torso}}` or `x_{max}` would otherwise become long modifier-letter
/// runs (`pₜₒᵣₛₒ`) that are hard to read and render with visible gaps in
/// terminal fonts lacking those glyphs. Index-like atoms (`x_{ij}`,
/// `T_{i+1}`, `n^{th}`) keep the compact Unicode form.
fn render_script(
    cursor: &mut Cursor<'_>,
    out: &mut MathBox,
    depth: usize,
    mode: Mode,
    kind: Script,
) {
    let Some(atom) = cursor.read_atom() else {
        out.push(match kind {
            Script::Super => '^',
            Script::Sub => '_',
        });
        return;
    };
    let rendered = render_atom(atom, depth, mode);
    let mapped: Option<String> = if script_atom_is_wordlike(atom, &rendered) {
        None
    } else {
        rendered
            .chars()
            .map(|c| match kind {
                Script::Super => to_superscript(c),
                Script::Sub => to_subscript(c),
            })
            .collect()
    };
    match mapped {
        Some(s) if !s.is_empty() => out.push_str(&s),
        _ => {
            out.push(match kind {
                Script::Super => '^',
                Script::Sub => '_',
            });
            if rendered.chars().count() > 1 {
                let _ = write!(out, "({rendered})");
            } else {
                out.push_str(&rendered);
            }
        }
    }
}

/// `true` if a script atom is a word-like label rather than indices.
///
/// Two signals, checked on the atom *source* and its rendered form:
///
/// - the source routes through a text-family command (`\text{…}`, `\mathrm{…}`,
///   `\operatorname{…}`, …): the author explicitly marked the content as a
///   word;
/// - the rendered form contains a run of 3+ ASCII letters: multi-letter runs
///   read as words (`max`, `torso`), while 1–2 letter runs are index
///   juxtapositions (`ij`, `th`) that stay compact.
fn script_atom_is_wordlike(atom: &str, rendered: &str) -> bool {
    // `\text` also catches `\textrm`/`\textbf`/`\textit`/`\textsf`/`\texttt`/
    // `\textnormal` by prefix; `\math…` variants and box commands likewise.
    const TEXT_MARKERS: [&str; 8] = [
        "\\text",
        "\\mathrm",
        "\\mathsf",
        "\\mathtt",
        "\\mathit",
        "\\operatorname",
        "\\mbox",
        "\\hbox",
    ];
    if TEXT_MARKERS.iter().any(|m| atom.contains(m)) {
        return true;
    }
    let mut run = 0usize;
    for c in rendered.chars() {
        if c.is_ascii_alphabetic() {
            run += 1;
            if run >= 3 {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

/// Render a `\command` whose backslash was already consumed.
fn render_command(cursor: &mut Cursor<'_>, out: &mut MathBox, depth: usize, mode: Mode) {
    let name = cursor.read_command_name();
    match name {
        // ── Structure ────────────────────────────────────────────────────
        "" => out.push('\\'),
        "\\" => out.push('\n'),
        "begin" => render_environment(cursor, out, depth, mode),
        "end" => {
            // Stray \end without matching \begin: drop its argument.
            let _ = take_brace_arg(cursor);
        }
        "left" | "right" => {
            // Keep the delimiter that follows; `.` means "no delimiter".
            cursor.skip_ws();
            match cursor.peek() {
                Some('.') => {
                    cursor.bump();
                }
                Some('\\') => {
                    cursor.bump();
                    render_command(cursor, out, depth, mode);
                }
                Some(c) => {
                    cursor.bump();
                    out.push(c);
                }
                None => {}
            }
        }

        // ── Fractions / binomials / roots ────────────────────────────────
        "frac" | "dfrac" | "tfrac" | "cfrac" => {
            let num = take_brace_arg(cursor).map(|a| render_atom(a, depth, mode));
            let den = take_brace_arg(cursor).map(|a| render_atom(a, depth, mode));
            match (num, den) {
                (Some(n), Some(d)) => out.push_str(&format_fraction(&n, &d)),
                (Some(n), None) => out.push_str(&n),
                _ => {}
            }
        }
        "binom" | "tbinom" | "dbinom" => {
            let n = take_brace_arg(cursor).map(|a| render_atom(a, depth, mode));
            let k = take_brace_arg(cursor).map(|a| render_atom(a, depth, mode));
            if let (Some(n), Some(k)) = (n, k) {
                let _ = write!(out, "C({n}, {k})");
            }
        }
        "sqrt" => {
            cursor.skip_ws();
            let index = if cursor.peek() == Some('[') {
                cursor.bump();
                let start = cursor.pos;
                while let Some(c) = cursor.peek() {
                    if c == ']' {
                        break;
                    }
                    cursor.bump();
                }
                let idx = &cursor.src[start..cursor.pos];
                cursor.bump(); // consume `]`
                Some(render_atom(idx, depth, mode))
            } else {
                None
            };
            let radical = match index.as_deref() {
                None | Some("2") => "√",
                Some("3") => "∛",
                Some("4") => "∜",
                Some(other) => {
                    // ⁿ√ style prefix for other indices.
                    let sup: Option<String> = other.chars().map(to_superscript).collect();
                    out.push_str(&sup.unwrap_or_else(|| format!("({other})")));
                    "√"
                }
            };
            out.push_str(radical);
            if let Some(arg) = cursor.read_atom() {
                let rendered = render_atom(arg, depth, mode);
                // Parenthesize any multi-char radicand: `√ab` would read as
                // `(√a)b`.
                if rendered.chars().count() > 1 {
                    let _ = write!(out, "({rendered})");
                } else {
                    out.push_str(&rendered);
                }
            }
        }

        // ── Boxes (frame dropped; content preserved) ─────────────────────
        "boxed" => {
            if let Some(arg) = take_brace_arg(cursor) {
                out.push_str(&render_atom(arg, depth, mode));
            }
        }
        "fbox" | "framebox" => {
            if let Some(arg) = take_brace_arg(cursor) {
                out.push_str(&render_atom(arg, depth, Mode::Text));
            }
        }

        // ── Text / alphabets ─────────────────────────────────────────────
        "text" | "textrm" | "textit" | "textbf" | "textsf" | "texttt" | "textnormal" | "mbox"
        | "hbox" => {
            if let Some(arg) = take_brace_arg(cursor) {
                out.push_str(&render_atom(arg, depth, Mode::Text));
            }
        }
        "mathrm" | "operatorname" | "mathit" | "mathsf" | "mathtt" | "mathnormal" => {
            if let Some(arg) = take_brace_arg(cursor) {
                out.push_str(&render_atom(arg, depth, Mode::Text));
            }
        }
        "mathbb" => render_mapped_alphabet(cursor, out, depth, mode, map_mathbb),
        "mathcal" | "mathscr" => render_mapped_alphabet(cursor, out, depth, mode, map_mathcal),
        "mathfrak" => render_mapped_alphabet(cursor, out, depth, mode, map_mathfrak),
        "mathbf" | "boldsymbol" | "bm" | "bold" => {
            render_mapped_alphabet(cursor, out, depth, mode, map_mathbf)
        }

        // ── Accents (combining marks) ────────────────────────────────────
        "hat" | "widehat" => render_accent(cursor, out, depth, mode, '\u{0302}'),
        "bar" | "overline" => render_accent(cursor, out, depth, mode, '\u{0304}'),
        "tilde" | "widetilde" => render_accent(cursor, out, depth, mode, '\u{0303}'),
        "vec" => render_accent(cursor, out, depth, mode, '\u{20D7}'),
        "dot" => render_accent(cursor, out, depth, mode, '\u{0307}'),
        "ddot" => render_accent(cursor, out, depth, mode, '\u{0308}'),
        "check" => render_accent(cursor, out, depth, mode, '\u{030C}'),
        "breve" => render_accent(cursor, out, depth, mode, '\u{0306}'),
        "acute" => render_accent(cursor, out, depth, mode, '\u{0301}'),
        "grave" => render_accent(cursor, out, depth, mode, '\u{0300}'),
        "mathring" => render_accent(cursor, out, depth, mode, '\u{030A}'),
        "underline" => render_accent(cursor, out, depth, mode, '\u{0332}'),

        // ── Negation ─────────────────────────────────────────────────────
        "not" => {
            if let Some(atom) = cursor.read_atom() {
                let rendered = render_atom(atom, depth, mode);
                match rendered.as_str() {
                    "∈" => out.push('∉'),
                    "=" => out.push('≠'),
                    "<" => out.push('≮'),
                    ">" => out.push('≯'),
                    "≡" => out.push('≢'),
                    "⊂" => out.push('⊄'),
                    "⊆" => out.push('⊈'),
                    "∃" => out.push('∄'),
                    other => {
                        out.push_str(other);
                        // Combining long solidus overlay on the last char.
                        if !other.is_empty() {
                            out.push('\u{0338}');
                        }
                    }
                }
            }
        }

        // ── Decorations rendered as base + script ────────────────────────
        "overset" | "stackrel" => {
            let over = take_brace_arg(cursor).map(|a| render_atom(a, depth, mode));
            let base = take_brace_arg(cursor).map(|a| render_atom(a, depth, mode));
            if let (Some(over), Some(base)) = (over, base) {
                out.push_str(&base);
                let sup: Option<String> = over.chars().map(to_superscript).collect();
                match sup {
                    Some(s) if !s.is_empty() => out.push_str(&s),
                    _ => {}
                }
            }
        }
        "underset" => {
            let under = take_brace_arg(cursor).map(|a| render_atom(a, depth, mode));
            let base = take_brace_arg(cursor).map(|a| render_atom(a, depth, mode));
            if let (Some(under), Some(base)) = (under, base) {
                out.push_str(&base);
                let sub: Option<String> = under.chars().map(to_subscript).collect();
                match sub {
                    Some(s) if !s.is_empty() => out.push_str(&s),
                    _ => {}
                }
            }
        }

        // ── Modular arithmetic ───────────────────────────────────────────
        "pmod" => {
            if let Some(arg) = take_brace_arg(cursor) {
                if !out.at_line_start() && !out.ends_with_space() {
                    out.push(' ');
                }
                let _ = write!(out, "(mod {})", render_atom(arg, depth, mode));
            }
        }
        "bmod" => {
            if !out.at_line_start() && !out.ends_with_space() {
                out.push(' ');
            }
            out.push_str("mod ");
        }

        // ── Spacing ──────────────────────────────────────────────────────
        "," | ";" | ":" | ">" | " " | "space" | "thinspace" | "medspace" | "thickspace"
        | "enspace" => {
            if !out.at_line_start() && !out.ends_with_space() {
                out.push(' ');
            }
        }
        "quad" => out.push_str("  "),
        "qquad" => out.push_str("    "),
        "!" | "negthinspace" | "negmedspace" | "negthickspace" => {}

        // ── No-ops (sizing/styling/structure hints) ──────────────────────
        "limits" | "nolimits" | "displaystyle" | "textstyle" | "scriptstyle"
        | "scriptscriptstyle" | "big" | "Big" | "bigg" | "Bigg" | "bigl" | "Bigl" | "biggl"
        | "Biggl" | "bigr" | "Bigr" | "biggr" | "Biggr" | "bigm" | "Bigm" | "biggm" | "Biggm"
        | "mathstrut" | "strut" | "allowbreak" | "nonumber" | "notag" | "mathopen"
        | "mathclose" | "mathbin" | "mathrel" | "mathord" | "mathpunct" | "mathinner"
        | "mathop" | "ensuremath" | "label" | "tag" => {
            // \label/\tag carry non-visual arguments: drop them.
            if matches!(name, "label" | "tag") {
                let _ = take_brace_arg(cursor);
            }
        }

        // ── Symbol table ─────────────────────────────────────────────────
        _ => {
            if let Some(sym) = symbol(name) {
                out.push_str(sym);
            } else {
                // Unknown command: keep its name as plain text.
                out.push_str(name);
            }
        }
    }
}

/// Consume `{...}` (after optional whitespace) and return the body source.
pub(super) fn take_brace_arg<'a>(cursor: &mut Cursor<'a>) -> Option<&'a str> {
    cursor.skip_ws();
    if cursor.peek() == Some('{') {
        cursor.bump();
        Some(cursor.read_group_body())
    } else {
        None
    }
}

/// `true` if a fraction/root operand needs parentheses for readability.
fn needs_parens(s: &str) -> bool {
    s.chars().count() > 1 && s.contains([' ', '+', '−', '-', '=', '/'])
}

/// Format `num/den`, mapping common numeric fractions to vulgar fractions.
fn format_fraction(num: &str, den: &str) -> String {
    let vulgar = match (num, den) {
        ("1", "2") => Some('½'),
        ("1", "3") => Some('⅓'),
        ("2", "3") => Some('⅔'),
        ("1", "4") => Some('¼'),
        ("3", "4") => Some('¾'),
        ("1", "5") => Some('⅕'),
        ("2", "5") => Some('⅖'),
        ("3", "5") => Some('⅗'),
        ("4", "5") => Some('⅘'),
        ("1", "6") => Some('⅙'),
        ("5", "6") => Some('⅚'),
        ("1", "7") => Some('⅐'),
        ("1", "8") => Some('⅛'),
        ("3", "8") => Some('⅜'),
        ("5", "8") => Some('⅝'),
        ("7", "8") => Some('⅞'),
        ("1", "9") => Some('⅑'),
        ("1", "10") => Some('⅒'),
        _ => None,
    };
    if let Some(v) = vulgar {
        return v.to_string();
    }
    let n = if needs_parens(num) {
        format!("({num})")
    } else {
        num.to_string()
    };
    let d = if needs_parens(den) {
        format!("({den})")
    } else {
        den.to_string()
    };
    format!("{n}/{d}")
}

/// Render an alphabet-mapping command (`\mathbb{R}` etc.): map chars that
/// have a styled form, keep the rest as rendered.
fn render_mapped_alphabet(
    cursor: &mut Cursor<'_>,
    out: &mut MathBox,
    depth: usize,
    mode: Mode,
    map: fn(char) -> Option<char>,
) {
    let Some(atom) = cursor.read_atom() else {
        return;
    };
    let rendered = render_atom(atom, depth, mode);
    for c in rendered.chars() {
        out.push(map(c).unwrap_or(c));
    }
}

/// Render an accent command by appending a combining mark to each char of
/// the argument.
fn render_accent(
    cursor: &mut Cursor<'_>,
    out: &mut MathBox,
    depth: usize,
    mode: Mode,
    combining: char,
) {
    let Some(atom) = cursor.read_atom() else {
        return;
    };
    let rendered = render_atom(atom, depth, mode);
    for c in rendered.chars() {
        out.push(c);
        if !c.is_whitespace() {
            out.push(combining);
        }
    }
}
