//! Minimal shell-token syntax for completion: find the token under the
//! cursor and re-quote completed components to match how it was typed.
//! Consumed by the file provider; the natural home for the $PATH provider's
//! segmentation too, once it learns quoting.
//!
//! ## Scope (deliberately minimal)
//!
//! [`parse_current_token`] understands just enough POSIX-shell syntax to
//! find the token under the cursor: double/single quotes, backslash
//! escapes, whitespace, the segment separators `|`/`;`/`&`, and the
//! redirection operators `<`/`>`. It does NOT model the full grammar:
//! no subshells or `$(…)`, no here-docs, no brace/glob expansion, no
//! `~user` home lookup.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum QuoteStyle {
    None,
    Double,
    Single,
}

/// The shell token under the cursor plus the segment facts completion needs.
#[derive(Debug)]
pub(super) struct CurrentToken {
    /// Byte offset in the request text where the token starts (an opening
    /// quote is part of the token) — the start of the replace range.
    pub(super) start: usize,
    /// Unquoted/unescaped value typed so far.
    pub(super) value: String,
    /// `value` length right after its last `/` (`None`: no slash).
    pub(super) dir_value_len: Option<usize>,
    /// Byte offset just after the last raw `/` (== `start` without one).
    pub(super) dir_raw_end: usize,
    /// Quote state at the cursor.
    pub(super) quote: QuoteStyle,
    /// Byte offset of the still-open quote (meaningful when `quote != None`).
    pub(super) open_quote_idx: usize,
    /// Quote structure of the REPLACED component when no quote is open at
    /// the cursor: the style of a quote whose closer sits inside the
    /// component (at/after `dir_raw_end`), plus whether its opener does too
    /// (and so needs re-emitting). `None`: quote-free component.
    pub(super) closed_quote: Option<(QuoteStyle, bool)>,
    /// Byte-aligned with `value`: `true` where the char was consumed
    /// unquoted and unescaped — the only spellings the shell expands
    /// `~`/`$` in (`'$HOME'`, `\$HOME`, and `"~/…` are literal to it).
    pub(super) plain_mask: Vec<bool>,
    /// Completed tokens before this one in the current segment.
    pub(super) tokens_before: usize,
    /// The segment's command word (first non-redirect-target token value).
    pub(super) command: Option<String>,
    /// Token directly follows `<`/`>` — always a file argument.
    pub(super) after_redirect: bool,
}

#[derive(Debug, Default)]
struct TokenBuild {
    start: usize,
    value: String,
    dir_value_len: Option<usize>,
    dir_raw_end: usize,
    after_redirect: bool,
    /// Last quote closed within this token: `(open_idx, close_idx, style)`.
    last_close: Option<(usize, usize, QuoteStyle)>,
    plain_mask: Vec<bool>,
}

impl TokenBuild {
    /// `plain`: the char reached `value` unquoted and unescaped — the only
    /// provenance the shell expands `~`/`$` in.
    fn push(&mut self, i: usize, c: char, plain: bool) {
        self.value.push(c);
        self.plain_mask
            .extend(std::iter::repeat_n(plain, c.len_utf8()));
        if c == '/' {
            self.dir_value_len = Some(self.value.len());
            self.dir_raw_end = i + 1;
        }
    }
}

fn ensure_token<'a>(
    cur: &'a mut Option<TokenBuild>,
    i: usize,
    pending_redirect: &mut bool,
) -> &'a mut TokenBuild {
    cur.get_or_insert_with(|| TokenBuild {
        start: i,
        dir_raw_end: i,
        after_redirect: std::mem::take(pending_redirect),
        ..TokenBuild::default()
    })
}

fn finish_token(
    cur: &mut Option<TokenBuild>,
    tokens_before: &mut usize,
    command: &mut Option<String>,
) {
    if let Some(t) = cur.take() {
        *tokens_before += 1;
        if command.is_none() && !t.after_redirect {
            *command = Some(t.value);
        }
    }
}

/// Scan the cursor prefix and return the token being typed. Quotes hide
/// separators (`echo "a | b` is one segment), backslashes escape the next
/// char, and `|`/`;`/`&` reset the segment. A dangling backslash at the
/// cursor contributes no value char but stays inside the token extent.
pub(super) fn parse_current_token(prefix: &str) -> CurrentToken {
    let mut cur: Option<TokenBuild> = None;
    let mut quote = QuoteStyle::None;
    let mut open_quote_idx = 0usize;
    let mut escape = false;
    let mut tokens_before = 0usize;
    let mut command: Option<String> = None;
    let mut pending_redirect = false;

    // Escape/quote state implies a token exists (`ensure_token` ran when the
    // state was entered), so the `ensure_token` calls below are no-op
    // lookups on valid input — but this parses arbitrary wire text, and a
    // mis-tokenized line must degrade, never panic the agent.
    for (i, c) in prefix.char_indices() {
        if escape {
            escape = false;
            let t = ensure_token(&mut cur, i, &mut pending_redirect);
            match quote {
                // `\X` outside quotes: literal X.
                QuoteStyle::None => t.push(i, c, false),
                // Inside double quotes `\` only escapes `"` `\` `$` `` ` ``.
                QuoteStyle::Double => {
                    if !matches!(c, '"' | '\\' | '$' | '`') {
                        t.push(i, '\\', false);
                    }
                    t.push(i, c, false);
                }
                // No escapes exist inside single quotes; keep the char.
                QuoteStyle::Single => t.push(i, c, false),
            }
            continue;
        }
        match quote {
            QuoteStyle::Single => match c {
                '\'' => {
                    quote = QuoteStyle::None;
                    if let Some(t) = cur.as_mut() {
                        t.last_close = Some((open_quote_idx, i, QuoteStyle::Single));
                    }
                }
                _ => ensure_token(&mut cur, i, &mut pending_redirect).push(i, c, false),
            },
            QuoteStyle::Double => match c {
                '"' => {
                    quote = QuoteStyle::None;
                    if let Some(t) = cur.as_mut() {
                        t.last_close = Some((open_quote_idx, i, QuoteStyle::Double));
                    }
                }
                '\\' => escape = true,
                _ => ensure_token(&mut cur, i, &mut pending_redirect).push(i, c, false),
            },
            QuoteStyle::None => match c {
                '\\' => {
                    ensure_token(&mut cur, i, &mut pending_redirect);
                    escape = true;
                }
                '\'' => {
                    ensure_token(&mut cur, i, &mut pending_redirect);
                    quote = QuoteStyle::Single;
                    open_quote_idx = i;
                }
                '"' => {
                    ensure_token(&mut cur, i, &mut pending_redirect);
                    quote = QuoteStyle::Double;
                    open_quote_idx = i;
                }
                c if c.is_whitespace() => {
                    finish_token(&mut cur, &mut tokens_before, &mut command);
                }
                '|' | ';' | '&' => {
                    finish_token(&mut cur, &mut tokens_before, &mut command);
                    tokens_before = 0;
                    command = None;
                    pending_redirect = false;
                }
                '<' | '>' => {
                    finish_token(&mut cur, &mut tokens_before, &mut command);
                    pending_redirect = true;
                }
                _ => ensure_token(&mut cur, i, &mut pending_redirect).push(i, c, true),
            },
        }
    }

    let (start, value, dir_value_len, dir_raw_end, after_redirect, last_close, plain_mask) =
        match cur {
            Some(t) => (
                t.start,
                t.value,
                t.dir_value_len,
                t.dir_raw_end,
                t.after_redirect,
                t.last_close,
                t.plain_mask,
            ),
            // Cursor sits after a separator: a fresh empty token starts here.
            None => (
                prefix.len(),
                String::new(),
                None,
                prefix.len(),
                pending_redirect,
                None,
                Vec::new(),
            ),
        };
    // A closure before the component boundary is raw-dir-internal
    // (balanced, kept verbatim) — only closers the component consumed
    // constrain how it re-renders.
    let closed_quote = last_close.and_then(|(open, close, style)| {
        (close >= dir_raw_end).then_some((style, open >= dir_raw_end))
    });
    CurrentToken {
        start,
        value,
        dir_value_len,
        dir_raw_end,
        quote,
        open_quote_idx,
        closed_quote,
        plain_mask,
        tokens_before,
        command,
        after_redirect,
    }
}

// ── Insert-token construction (quoting) ─────────────────────────────────

/// Build the replacement for the whole token: the user's verbatim directory
/// prefix plus the completed component escaped for the quote context at the
/// cursor. Files close an open quote; directories keep it open (and get the
/// trailing `/`) so the next Tab drills down, bash-style.
pub(super) fn build_insert_token(
    tok: &CurrentToken,
    raw_dir: &str,
    name: &str,
    is_dir: bool,
) -> String {
    let mut out = String::with_capacity(raw_dir.len() + name.len() + 4);
    out.push_str(raw_dir);
    // A completed component starting with `-` would otherwise insert a
    // flag-looking argument (`rm ` + Tab → `rm -rf`, invisible when the
    // single-candidate insta-accept skips the dropdown); quoting wouldn't
    // help (`rm "-rf"` is still a flag to rm). Anchor bare names as
    // explicit paths — deliberately stricter than bash.
    if raw_dir.is_empty() && name.starts_with('-') {
        out.push_str("./");
    }
    // The quote context the component renders in: the quote still open at
    // the cursor, or one the component CLOSED (`cat "My Dir/fi"` — raw_dir
    // keeps the dangling opener, so dropping the closer would emit an
    // unbalanced line). A quote opened INSIDE the component (after the
    // last `/`) is not part of `raw_dir` — re-emit it.
    let (style, reopen) = match tok.quote {
        QuoteStyle::None => tok.closed_quote.unwrap_or((QuoteStyle::None, false)),
        open => (open, tok.open_quote_idx >= tok.dir_raw_end),
    };
    match style {
        QuoteStyle::None => out.push_str(&escape_unquoted(name)),
        QuoteStyle::Double => {
            if reopen {
                out.push('"');
            }
            out.push_str(&escape_double_quoted(name));
            if !is_dir {
                out.push('"');
            }
        }
        QuoteStyle::Single => {
            if reopen {
                out.push('\'');
            }
            out.push_str(&escape_single_quoted(name));
            if !is_dir {
                out.push('\'');
            }
        }
    }
    if is_dir {
        out.push('/');
    }
    out
}

/// Bash-ish set of characters that need a backslash outside quotes.
/// Deliberately generous: over-escaping is harmless to the shell,
/// under-escaping breaks the command.
fn needs_backslash(c: char) -> bool {
    matches!(
        c,
        ' ' | '\t'
            | '"'
            | '\''
            | '\\'
            | '$'
            | '`'
            | '&'
            | '|'
            | ';'
            | '('
            | ')'
            | '<'
            | '>'
            | '*'
            | '?'
            | '['
            | ']'
            | '#'
            | '!'
            | '{'
            | '}'
            | '~'
    )
}

fn escape_unquoted(name: &str) -> String {
    // Control chars (newlines…) can't be backslash-escaped portably (`\` +
    // newline is a line continuation) — single-quote the whole component.
    if name.chars().any(char::is_control) {
        return format!("'{}'", escape_single_quoted(name));
    }
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if needs_backslash(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn escape_double_quoted(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if matches!(c, '"' | '\\' | '$' | '`') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn escape_single_quoted(name: &str) -> String {
    // `'` cannot appear inside single quotes: close, escape, reopen.
    name.replace('\'', "'\\''")
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- parse_current_token ---

    #[test]
    fn parse_after_pipe_and_semicolon() {
        let tok = parse_current_token("echo hi | cat foo");
        assert_eq!(tok.value, "foo");
        assert_eq!(tok.start, 14);
        assert_eq!(tok.command.as_deref(), Some("cat"));

        let tok = parse_current_token("cd /tmp; ls ");
        assert_eq!(tok.value, "");
        assert_eq!(tok.start, 12);
        assert_eq!(tok.command.as_deref(), Some("ls"));
    }

    #[test]
    fn parse_after_double_ampersand() {
        let tok = parse_current_token("make && cat foo");
        assert_eq!(tok.value, "foo");
        assert_eq!(tok.start, 12);
    }

    /// Quotes hide segment separators: the pipe is data, not a new command.
    #[test]
    fn parse_quoted_pipe_is_one_token() {
        let tok = parse_current_token("echo \"a | b");
        assert_eq!(tok.value, "a | b");
        assert_eq!(tok.start, 5);
        assert_eq!(tok.quote, QuoteStyle::Double);
        assert_eq!(tok.command.as_deref(), Some("echo"));
    }

    #[test]
    fn parse_open_double_quote_token() {
        let tok = parse_current_token("cat \"My Fi");
        assert_eq!(tok.value, "My Fi");
        assert_eq!(tok.start, 4);
        assert_eq!(tok.quote, QuoteStyle::Double);
        assert_eq!(tok.open_quote_idx, 4);
    }

    #[test]
    fn parse_backslash_escaped_space_token() {
        let tok = parse_current_token("cat My\\ Fi");
        assert_eq!(tok.value, "My Fi");
        assert_eq!(tok.start, 4);
        assert_eq!(tok.quote, QuoteStyle::None);
    }

    #[test]
    fn parse_open_single_quote_token() {
        let tok = parse_current_token("cat 'sing le");
        assert_eq!(tok.value, "sing le");
        assert_eq!(tok.quote, QuoteStyle::Single);
    }

    /// Closed-quote token: quote state returns to None at the cursor and the
    /// raw dir keeps the user's quoting verbatim.
    #[test]
    fn parse_closed_quote_dir_prefix() {
        let tok = parse_current_token("cat \"My Dir\"/fi");
        assert_eq!(tok.value, "My Dir/fi");
        assert_eq!(tok.quote, QuoteStyle::None);
        assert_eq!(tok.dir_raw_end, 13);
        assert_eq!(tok.dir_value_len, Some(7));
    }

    /// `<`/`>` end the preceding token and flag the next as a redirect
    /// target without resetting the segment (the command survives).
    #[test]
    fn parse_redirect_sets_flag_and_boundary() {
        let tok = parse_current_token("echo hi > lo");
        assert_eq!(tok.value, "lo");
        assert!(tok.after_redirect);
        assert_eq!(tok.command.as_deref(), Some("echo"));

        let tok = parse_current_token("> lo");
        assert_eq!(tok.value, "lo");
        assert!(tok.after_redirect);
        assert_eq!(tok.tokens_before, 0);
    }

    #[test]
    fn parse_multibyte_whitespace() {
        let tok = parse_current_token("cat\u{3000}foo");
        assert_eq!(tok.value, "foo");
    }

    /// The mask records per-byte quote/escape provenance: quoted and escaped
    /// chars are not `plain` (the shell would not expand `~`/`$` there).
    #[test]
    fn parse_plain_mask_tracks_quote_and_escape_provenance() {
        let tok = parse_current_token("cat '$A'/b\\$c");
        assert_eq!(tok.value, "$A/b$c");
        assert_eq!(tok.plain_mask, [false, false, true, true, false, true]);

        let tok = parse_current_token("cat \"~/do");
        assert_eq!(tok.value, "~/do");
        assert!(tok.plain_mask.iter().all(|p| !p));

        let tok = parse_current_token("cat ~/do");
        assert!(tok.plain_mask.iter().all(|p| *p));
    }

    #[test]
    fn parse_dangling_backslash_keeps_token() {
        let tok = parse_current_token("cat Notes\\");
        assert_eq!(tok.value, "Notes");
        assert_eq!(tok.start, 4);
    }

    #[test]
    fn parse_multiple_args_takes_last() {
        let tok = parse_current_token("cp src/a.txt dst/b");
        assert_eq!(tok.value, "dst/b");
        assert_eq!(tok.start, 13);
    }

    // --- escaping / insert-token construction ---

    #[test]
    fn escape_unquoted_space_and_specials() {
        assert_eq!(escape_unquoted("My File.txt"), "My\\ File.txt");
        assert_eq!(escape_unquoted("a\"b'c"), "a\\\"b\\'c");
        assert_eq!(escape_unquoted("a$b"), "a\\$b");
        assert_eq!(escape_unquoted("plain.txt"), "plain.txt");
    }

    #[test]
    fn escape_unquoted_control_chars_single_quote_fallback() {
        assert_eq!(escape_unquoted("a\nb"), "'a\nb'");
    }

    #[test]
    fn escape_double_quoted_minimal_set() {
        assert_eq!(escape_double_quoted("My File.txt"), "My File.txt");
        assert_eq!(escape_double_quoted("a\"b$c"), "a\\\"b\\$c");
    }

    #[test]
    fn escape_single_quoted_embedded_quote() {
        assert_eq!(escape_single_quoted("it's"), "it'\\''s");
    }

    #[test]
    fn insert_token_backslash_style_dir_stays_open() {
        let tok = parse_current_token("cat No");
        assert_eq!(
            build_insert_token(&tok, "", "Notes Archive", true),
            "Notes\\ Archive/"
        );
        assert_eq!(
            build_insert_token(&tok, "", "My File.txt", false),
            "My\\ File.txt"
        );
    }

    /// Open double quote: files close it, directories keep it open for
    /// drill-down (bash behavior). Slashless tokens have an empty raw dir —
    /// the still-open quote sits inside the replaced component (reopen path).
    #[test]
    fn insert_token_preserves_open_double_quote() {
        let tok = parse_current_token("cat \"My Fi");
        assert_eq!(
            build_insert_token(&tok, "", "My File.txt", false),
            "\"My File.txt\""
        );
        let tok = parse_current_token("cat \"No");
        assert_eq!(
            build_insert_token(&tok, "", "Notes Archive", true),
            "\"Notes Archive/"
        );
    }

    /// A quote opened after the last `/` sits inside the replaced component
    /// and must be re-emitted.
    #[test]
    fn insert_token_reopens_quote_after_slash() {
        let tok = parse_current_token("cat dir/\"fi");
        // raw_dir covers `dir/`; the quote reopened inside the component.
        assert_eq!(tok.dir_raw_end, 8);
        assert_eq!(tok.open_quote_idx, 8);
        assert_eq!(
            build_insert_token(&tok, "dir/", "file name.txt", false),
            "dir/\"file name.txt\""
        );
    }

    #[test]
    fn insert_token_single_quote_style() {
        let tok = parse_current_token("cat 'My Fi");
        assert_eq!(
            build_insert_token(&tok, "", "My File.txt", false),
            "'My File.txt'"
        );
    }

    /// THE closed-at-cursor case: the closer sits inside the replaced
    /// component while `raw_dir` keeps the opener — the rebuilt insert must
    /// still close it (files) or keep drilling (dirs), never emit
    /// `"My Dir/file.txt` with a dangling opener.
    #[test]
    fn insert_token_quote_closed_at_cursor_keeps_closer() {
        let tok = parse_current_token("cat \"My Dir/fi\"");
        assert_eq!(tok.quote, QuoteStyle::None);
        assert_eq!(tok.closed_quote, Some((QuoteStyle::Double, false)));
        assert_eq!(
            build_insert_token(&tok, "\"My Dir/", "file.txt", false),
            "\"My Dir/file.txt\""
        );
        assert_eq!(
            build_insert_token(&tok, "\"My Dir/", "subdir", true),
            "\"My Dir/subdir/"
        );

        let tok = parse_current_token("cat 'My Dir/fi'");
        assert_eq!(
            build_insert_token(&tok, "'My Dir/", "file.txt", false),
            "'My Dir/file.txt'"
        );
    }

    /// Cursor still INSIDE the quotes (closer not part of the prefix): the
    /// open-quote path is unchanged by the closed-quote tracking.
    #[test]
    fn insert_token_cursor_inside_quotes_unchanged() {
        let tok = parse_current_token("cat \"My Dir/fi");
        assert_eq!(tok.quote, QuoteStyle::Double);
        assert_eq!(tok.closed_quote, None);
        assert_eq!(
            build_insert_token(&tok, "\"My Dir/", "file.txt", false),
            "\"My Dir/file.txt\""
        );
    }

    /// Balanced quotes entirely inside a slashless component are replaced
    /// wholesale: the insert re-opens AND closes them.
    #[test]
    fn insert_token_balanced_slashless_quotes_reopen_and_close() {
        let tok = parse_current_token("cat \"fi\"");
        assert_eq!(tok.closed_quote, Some((QuoteStyle::Double, true)));
        assert_eq!(
            build_insert_token(&tok, "", "file.txt", false),
            "\"file.txt\""
        );
    }

    /// A quote closed BEFORE the last `/` is raw-dir-internal (kept
    /// verbatim) and must not force quote rendering on the component.
    #[test]
    fn insert_token_quote_closed_in_raw_dir_stays_plain() {
        let tok = parse_current_token("cat \"My Dir\"/fi");
        assert_eq!(tok.closed_quote, None);
        assert_eq!(
            build_insert_token(&tok, "\"My Dir\"/", "file.txt", false),
            "\"My Dir\"/file.txt"
        );
    }

    /// Dash-leading names anchor as `./`-relative paths so a completed bare
    /// component can never parse as a flag (`rm ` + Tab must not become
    /// `rm -rf`); quoting alone would not help. Directory-prefixed
    /// components are already anchored.
    #[test]
    fn insert_token_anchors_dash_leading_names() {
        let tok = parse_current_token("rm ");
        assert_eq!(build_insert_token(&tok, "", "-rf", false), "./-rf");
        assert_eq!(
            build_insert_token(&tok, "", "-flag dir", true),
            "./-flag\\ dir/"
        );
        let tok = parse_current_token("rm \"");
        assert_eq!(build_insert_token(&tok, "", "-rf", false), "./\"-rf\"");
        let tok = parse_current_token("rm sub/");
        assert_eq!(build_insert_token(&tok, "sub/", "-rf", false), "sub/-rf");
    }
}
