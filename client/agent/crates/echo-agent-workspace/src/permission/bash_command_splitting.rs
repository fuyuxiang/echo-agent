use tree_sitter::{Node, Parser, Tree};
use tree_sitter_bash::LANGUAGE as BASH;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BashCommandHighlights {
    pub prefix: Vec<String>,
    // TODO: Whatever the words looked like,, we don't handle the words differently
    pub highlighted_words: Vec<String>,
    pub suffix: Vec<String>,
}

/// Internal representation of a parsed "plain" command:
/// - `words`: the actual command + args (env assignments stripped)
/// - `span_start` / `span_end`: byte range in the original script covering
///   the highlighted command part (command name + args).
#[derive(Debug, Clone)]
pub struct PlainCommand {
    words: Vec<String>,
    span_start: usize,
    span_end: usize,
}

impl PlainCommand {
    /// Returns the command words (command name + args, env assignments stripped).
    pub fn words(&self) -> &[String] {
        &self.words
    }

    /// Whether this command's highlighted span covers the entire script
    /// (ignoring surrounding whitespace). Only then can the dequoted word join
    /// stand in for the raw script string: a leading `FOO=…` assignment or a
    /// chained sibling would otherwise be silently dropped from the compare,
    /// letting an env-injected or extended script match a narrower grant.
    ///
    /// The spans were taken from the script this command was parsed from;
    /// `get` keeps a mismatched or shorter `script` panic-safe, and `false`
    /// is the conservative answer for one.
    pub(crate) fn spans_whole_script(&self, script: &str) -> bool {
        let (Some(before), Some(after)) =
            (script.get(..self.span_start), script.get(self.span_end..))
        else {
            return false;
        };
        before.trim().is_empty() && after.trim().is_empty()
    }
}

/// Parse the provided bash source using tree-sitter-bash, returning a Tree on
/// success or None if parsing failed.
pub fn try_parse_shell(src: &str) -> Option<Tree> {
    let lang = BASH.into();
    let mut parser = Parser::new();
    #[expect(clippy::expect_used)]
    parser.set_language(&lang).expect("load bash grammar");
    let old_tree: Option<&Tree> = None;
    parser.parse(src, old_tree)
}

/// Parse a script which may contain multiple simple commands joined only by
/// the safe logical/pipe/sequencing operators: `&&`, `||`, `;`, `|`.
///
/// Returns `Some(Vec<PlainCommand>)` if every command is a plain word-only
/// command and the parse tree does not contain disallowed constructs
/// (parentheses, redirections, substitutions, control flow, etc.). Otherwise
/// returns `None`.
pub fn try_parse_word_only_commands_sequence(tree: &Tree, src: &str) -> Option<Vec<PlainCommand>> {
    if tree.root_node().has_error() {
        return None;
    }

    // List of allowed (named) node kinds for a "word only commands sequence".
    // If we encounter a named node that is not in this list we reject.
    const ALLOWED_KINDS: &[&str] = &[
        // top level containers
        "program",
        "list",
        "pipeline",
        // commands & words
        "command",
        "command_name",
        "word",
        "string",
        "string_content",
        "raw_string",
        "number",
        "concatenation",
        // allow simple env var assignments before commands
        "variable_assignment",
        "variable_name",
        // allow redirections (e.g., 2>&1, > file, etc.)
        "redirected_statement",
        "file_redirect",
        "file_descriptor",
        // Comments never execute.
        "comment",
        // Heredoc bodies are stdin data to the (separately classified) head
        // command, not shell-executed text. An unquoted body surfaces
        // `$(...)`/`${...}` as named child nodes outside this allowlist, so
        // substitution smuggling still fails the parse; a `> file` on the same
        // statement stays visible to the write model as a file_redirect.
        // `declaration_command` (`export K=V`) is deliberately ABSENT: it is
        // not a `command` node, so ask-mode segment evaluation (which has no
        // env guard) would never see a PATH/LD_PRELOAD hijack.
        "heredoc_redirect",
        "heredoc_start",
        "heredoc_body",
        "heredoc_content",
        "heredoc_end",
    ];

    // Allow only safe punctuation / operator tokens; anything else causes reject.
    // We add "=" so that VAR=VALUE is allowed.
    // Redirection operators: >, >>, <, >&, &>, etc.
    const ALLOWED_PUNCT_TOKENS: &[&str] = &[
        "&&", "||", ";", "|", "\"", "'", "=", ">", ">>", "<", "<<", ">&", "&>", "&>>",
    ];

    let root = tree.root_node();
    let mut cursor = root.walk();
    let mut stack = vec![root];
    let mut command_nodes = Vec::new();

    while let Some(node) = stack.pop() {
        let kind = node.kind();

        if node.is_named() {
            if !ALLOWED_KINDS.contains(&kind) {
                return None;
            }
            if kind == "command" {
                command_nodes.push(node);
            }
        } else {
            // Reject any punctuation / operator tokens that are not explicitly allowed.
            if kind.chars().any(|c| "&;|".contains(c)) && !ALLOWED_PUNCT_TOKENS.contains(&kind) {
                return None;
            }
            if !(ALLOWED_PUNCT_TOKENS.contains(&kind) || kind.trim().is_empty()) {
                // If it's a quote token or operator it's allowed above; we also allow whitespace tokens.
                // Any other punctuation like parentheses, braces, redirects, backticks, etc are rejected.
                return None;
            }
        }

        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }

    // Walk uses a stack (LIFO), so re-sort by position to restore source order.
    command_nodes.sort_by_key(Node::start_byte);

    let mut commands = Vec::new();
    for node in command_nodes {
        if let Some(cmd) = parse_plain_command_from_node(node, src) {
            commands.push(cmd);
        } else {
            return None;
        }
    }
    Some(commands)
}

/// Classify "setup" or "wrapper" commands you want to skip when looking for the
/// "first important command" (cd/export/sleep/timeout/etc).
pub(crate) fn is_setup_command(cmd: &[String]) -> bool {
    if cmd.is_empty() {
        return true;
    }

    matches!(
        cmd[0].as_str(),
        "cd" | "pushd" | "popd" | "export" | "unset" | "set" | "sleep" | "timeout"
    )
}

/// A GNU `env` `NAME=VALUE` assignment operand (skipped during option scanning).
fn is_env_assignment(tok: &str) -> bool {
    let Some(eq) = tok.find('=') else {
        return false;
    };
    let name = &tok[..eq];
    !name.is_empty()
        && name
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

struct EnvScan<'a> {
    chdir: Option<&'a str>,
    /// GNU/BSD `env -S`/`--split-string` rewrites argv; never peel past it.
    has_split_string: bool,
    /// Unknown/value-taking option arity not modeled — refuse peel + Ask.
    options_uncertain: bool,
    /// High-confidence literal packed script for Bash deny recursion only.
    split_string_script: Option<String>,
    command_start: usize,
}

/// Decode expansion-free word/string/concatenation *source spelling* under shell
/// quote rules. For raw tree-sitter concatenations that still contain `'`/`"`.
/// Already-dequoted plain argv words must not use this (env-S metasyntax like
/// `\t` would be corrupted into shell escapes).
pub(crate) fn decode_shell_literal_spelling(raw: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = raw.chars().peekable();
    let mut quote = None;
    while let Some(ch) = chars.next() {
        match (quote, ch) {
            (None, '\'' | '"') => quote = Some(ch),
            (Some(open), close) if open == close => quote = None,
            (None, '\\') => out.push(chars.next()?),
            (Some('"'), '\\') => match chars.next()? {
                n @ ('$' | '`' | '"' | '\\' | '\n') => out.push(n),
                n => {
                    out.push('\\');
                    out.push(n);
                }
            },
            // Single-quoted text is literal, including backslashes.
            _ => out.push(ch),
        }
    }
    quote.is_none().then_some(out)
}

/// Option-token spelling only: shell-decode when quote chars remain (glued/equal
/// concatenations like `-S'cmd'` / `--split-string='cmd'`). Never fold bare
/// backslashes without quotes — those are env-S metasyntax on the payload side.
fn decode_env_option_token(raw: &str) -> std::borrow::Cow<'_, str> {
    if !raw.contains(['\'', '"']) {
        return std::borrow::Cow::Borrowed(raw);
    }
    match decode_shell_literal_spelling(raw) {
        Some(decoded) => std::borrow::Cow::Owned(decoded),
        // Unclosed quotes: keep raw so flag detection can still Ask.
        None => std::borrow::Cow::Borrowed(raw),
    }
}

/// Safe subset for recursing a packed `env -S` operand as a Bash script: no
/// env-S quotes/escapes/comments/expansions that could diverge under reparse.
/// Includes bare `\` so `\t`/`\n`/… stay non-extractable (Ask floor only).
fn is_high_confidence_env_s_payload(s: &str) -> bool {
    !s.is_empty()
        && !s.contains('\0')
        && !s
            .chars()
            .any(|c| matches!(c, '\'' | '"' | '\\' | '#' | '`' | '$' | '\n' | '\r'))
}

/// Safe-subset check + own. Callers must pass already-literal text (separate
/// argv word, or payload carved after option-token quote removal) — never raw
/// shell-escape sequences meant for env-S.
fn take_high_confidence_payload(raw: &str) -> Option<String> {
    is_high_confidence_env_s_payload(raw).then(|| raw.to_owned())
}

/// Classification of one short-option token for GNU/BSD `env` (minimal table).
enum EnvShort<'a> {
    SplitStringNext,
    SplitStringGlued(&'a str),
    /// Cluster `S` after modeled no-arg shorts: detect only.
    SplitStringDetect,
    NoArg,
    ArgNeedNext(char),
    /// Glued arg-taking short; `kind` is `u`/`C`/`P`/`a` (operand is rest of token).
    ArgGlued {
        kind: char,
    },
    Uncertain,
}

/// Walk one short-option token. Arg-taking `u`/`C`/`P`/`a` absorb the rest of
/// the token as their operand (do not treat `S` inside that operand as `-S`).
fn classify_env_short(tok: &str) -> EnvShort<'_> {
    if !tok.starts_with('-') || tok.starts_with("--") || tok == "-" {
        return EnvShort::Uncertain;
    }
    let body = &tok[1..];
    if body.is_empty() {
        return EnvShort::Uncertain;
    }
    if let Some(rest) = body.strip_prefix('S') {
        return if rest.is_empty() {
            EnvShort::SplitStringNext
        } else {
            EnvShort::SplitStringGlued(rest)
        };
    }
    let chars: Vec<char> = body.chars().collect();
    let mut idx = 0usize;
    while idx < chars.len() {
        match chars[idx] {
            'i' | 'v' | '0' => idx += 1,
            'S' => return EnvShort::SplitStringDetect,
            kind @ ('u' | 'C' | 'P' | 'a') => {
                let has_glued = body.char_indices().nth(idx + 1).is_some();
                return if has_glued {
                    EnvShort::ArgGlued { kind }
                } else {
                    EnvShort::ArgNeedNext(kind)
                };
            }
            _ => return EnvShort::Uncertain,
        }
    }
    EnvShort::NoArg
}

/// Consume a required option operand; missing → uncertain.
fn take_option_operand<'a>(cmd: &'a [String], i: &mut usize) -> Option<&'a str> {
    match cmd.get(*i + 1).map(String::as_str) {
        Some(v) => {
            *i += 2;
            Some(v)
        }
        None => None,
    }
}

/// Scan a leading `env`'s options/assignments → its `-C`/`--chdir` target (last
/// wins), whether a split-string rewrite is present, an optional high-confidence
/// packed script, and the index where the inner command starts. `env` permutes
/// options with `NAME=VALUE`. Minimal GNU/BSD table — unknown arity fails closed.
fn env_scan(cmd: &[String]) -> EnvScan<'_> {
    let mut chdir = None;
    let mut has_split_string = false;
    let mut options_uncertain = false;
    let mut split_string_script = None;
    let mut i = 1usize;
    while let Some(tok_raw) = cmd.get(i).map(String::as_str) {
        if tok_raw == "--" {
            i += 1;
            break;
        }
        // GNU: bare `-` is a synonym for `-i` (still an option, not the command).
        if tok_raw == "-" {
            i += 1;
            continue;
        }
        if !tok_raw.starts_with('-') {
            if is_env_assignment(tok_raw) {
                i += 1;
                continue;
            }
            break;
        }

        if tok_raw.starts_with("--") {
            if tok_raw == "--chdir" {
                match take_option_operand(cmd, &mut i) {
                    Some(dir) => chdir = Some(dir),
                    None => {
                        options_uncertain = true;
                        break;
                    }
                }
                continue;
            }
            if let Some(dir) = tok_raw.strip_prefix("--chdir=") {
                chdir = Some(dir);
                i += 1;
                continue;
            }
            // `--path` pairs with BSD-style `-P`; unmodeled longs (e.g. `--prefix`) uncertain.
            if tok_raw == "--unset" || tok_raw == "--path" || tok_raw == "--argv0" {
                if take_option_operand(cmd, &mut i).is_none() {
                    options_uncertain = true;
                    break;
                }
                continue;
            }
            if tok_raw.starts_with("--unset=")
                || tok_raw.starts_with("--path=")
                || tok_raw.starts_with("--argv0=")
            {
                i += 1;
                continue;
            }
            if matches!(
                tok_raw,
                "--ignore-environment" | "--null" | "--debug" | "--version" | "--help"
            ) {
                i += 1;
                continue;
            }
            if tok_raw == "--split-string" {
                // WHY: `-S` re-tokenizes a packed string; do not peel past it.
                has_split_string = true;
                if let Some(raw) = cmd.get(i + 1).map(String::as_str) {
                    split_string_script = take_high_confidence_payload(raw);
                }
                break;
            }
            if let Some(payload) = tok_raw.strip_prefix("--split-string=") {
                let decoded = decode_env_option_token(tok_raw);
                let payload = decoded
                    .as_ref()
                    .strip_prefix("--split-string=")
                    .unwrap_or(payload);
                has_split_string = true;
                split_string_script = take_high_confidence_payload(payload);
                break;
            }
            // WHY: unknown long may take a value and hide a later `-S`.
            options_uncertain = true;
            break;
        }

        let tok_decoded = decode_env_option_token(tok_raw);
        let tok = tok_decoded.as_ref();
        match classify_env_short(tok) {
            EnvShort::SplitStringNext => {
                has_split_string = true;
                if let Some(raw) = cmd.get(i + 1).map(String::as_str) {
                    split_string_script = take_high_confidence_payload(raw);
                }
                break;
            }
            EnvShort::SplitStringGlued(payload) => {
                has_split_string = true;
                split_string_script = take_high_confidence_payload(payload);
                break;
            }
            EnvShort::SplitStringDetect => {
                has_split_string = true;
                break;
            }
            EnvShort::NoArg => i += 1,
            EnvShort::ArgNeedNext(kind) => match take_option_operand(cmd, &mut i) {
                Some(operand) => {
                    if kind == 'C' {
                        chdir = Some(operand);
                    }
                }
                None => {
                    options_uncertain = true;
                    break;
                }
            },
            EnvShort::ArgGlued { kind } => {
                if kind == 'C' {
                    chdir = Some(
                        tok_raw
                            .strip_prefix("-C")
                            .filter(|d| !d.is_empty())
                            .unwrap_or(""),
                    );
                }
                i += 1;
            }
            EnvShort::Uncertain => {
                options_uncertain = true;
                break;
            }
        }
    }
    EnvScan {
        chdir,
        has_split_string,
        options_uncertain,
        split_string_script,
        command_start: i,
    }
}

/// High-confidence packed `env -S`/`--split-string` operand when `words` is an
/// env invocation stopped on a recoverable literal form. Bash deny only.
pub(crate) fn env_split_string_script(words: &[String]) -> Option<String> {
    if words.first()?.rsplit(['/', '\\']).next()? != "env" {
        return None;
    }
    let scan = env_scan(words);
    if !scan.has_split_string {
        return None;
    }
    scan.split_string_script
}

/// Strip a leading wrapper (`timeout`/`env`/`nice`/`stdbuf`/`ionice`/`chrt`) and
/// its args, returning the inner command (`None` if not a wrapper). Conservative:
/// under-strip when unsure so `timeout 30 rm -rf /` can't hide behind `timeout`.
pub(crate) fn strip_wrapper_command(cmd: &[String]) -> Option<&[String]> {
    // Basename so a path-qualified wrapper (e.g. `/usr/bin/env`) is still stripped.
    let head = cmd.first()?.rsplit(['/', '\\']).next()?;
    let mut i = 1usize;
    match head {
        // `timeout [OPTIONS] DURATION COMMAND [ARGS]`
        "timeout" => {
            while i < cmd.len() && cmd[i].starts_with('-') {
                if matches!(cmd[i].as_str(), "-k" | "-s" | "--kill-after" | "--signal") {
                    i += 2;
                } else {
                    i += 1;
                }
            }
            // mandatory DURATION token
            if i >= cmd.len() {
                return None;
            }
            i += 1;
        }
        // `nice [OPTIONS] [COMMAND]`
        "nice" => {
            while i < cmd.len() && cmd[i].starts_with('-') {
                if matches!(cmd[i].as_str(), "-n" | "--adjustment") {
                    i += 2;
                } else {
                    i += 1;
                }
            }
        }
        // `ionice [OPTIONS] [COMMAND]`
        "ionice" => {
            while i < cmd.len() && cmd[i].starts_with('-') {
                if matches!(
                    cmd[i].as_str(),
                    "-c" | "-n"
                        | "-p"
                        | "-P"
                        | "-u"
                        | "--class"
                        | "--classdata"
                        | "--pid"
                        | "--pgid"
                        | "--uid"
                ) {
                    i += 2;
                } else {
                    i += 1;
                }
            }
        }
        // `chrt [OPTIONS] PRIORITY COMMAND [ARGS]`
        "chrt" => {
            while i < cmd.len() && cmd[i].starts_with('-') {
                i += 1;
            }
            // mandatory PRIORITY token
            if i >= cmd.len() {
                return None;
            }
            i += 1;
        }
        // `stdbuf OPTIONS COMMAND [ARGS]`
        "stdbuf" => {
            while i < cmd.len() && cmd[i].starts_with('-') {
                if matches!(cmd[i].as_str(), "-i" | "-o" | "-e") {
                    i += 2;
                } else {
                    i += 1;
                }
            }
        }
        // `env [OPTIONS]/[NAME=VALUE] (interspersed) [COMMAND] [ARGS]`
        "env" => {
            let scan = env_scan(cmd);
            // WHY: split-string rewrites argv; uncertain option arity can hide it.
            if scan.has_split_string || scan.options_uncertain {
                return None;
            }
            i = scan.command_start;
        }
        _ => return None,
    }
    let inner = cmd.get(i..)?;
    if inner.is_empty() {
        return None;
    }
    Some(inner)
}

/// True if `words`' head is a command [`strip_wrapper_command`] would peel — i.e.
/// its basename is in the canonical wrapper set. Reports membership only (so bare
/// `env`, which `strip_wrapper_command` returns `None` for, still counts). Keep
/// the set in sync with `strip_wrapper_command`'s match arms.
pub(crate) fn is_wrapper_command(words: &[String]) -> bool {
    matches!(
        words.first().and_then(|w| w.rsplit(['/', '\\']).next()),
        Some("timeout" | "nice" | "ionice" | "chrt" | "stdbuf" | "env")
    )
}

pub(crate) const MAX_WRAPPER_DEPTH: usize = 8;
/// Independent budget for `shell -c` recursion (not wrapper peel layers).
pub(crate) const MAX_INLINE_SHELL_DEPTH: usize = 8;

pub(crate) struct CheckedWrapperPeel<'a> {
    pub(crate) words: &'a [String],
    pub(crate) has_chdir: bool,
    /// `env -S`/`--split-string` rewrite was seen; callers must Ask (floor).
    pub(crate) has_split_string: bool,
    /// Unmodeled env option arity; callers must Ask and must not peel.
    pub(crate) env_options_uncertain: bool,
    pub(crate) exhausted: bool,
}

/// Peel canonical wrappers while retaining cwd, split-string, and exhaustion facts.
pub(crate) fn unwrap_wrappers_checked(words: &[String]) -> CheckedWrapperPeel<'_> {
    let mut current = words;
    let mut has_chdir = false;
    let mut has_split_string = false;
    let mut env_options_uncertain = false;
    for _ in 0..MAX_WRAPPER_DEPTH {
        if current.first().and_then(|w| w.rsplit(['/', '\\']).next()) == Some("env") {
            let scan = env_scan(current);
            has_chdir |= scan.chdir.is_some();
            has_split_string |= scan.has_split_string;
            env_options_uncertain |= scan.options_uncertain;
        }
        match strip_wrapper_command(current) {
            Some(inner) => current = inner,
            None => {
                return CheckedWrapperPeel {
                    words: current,
                    has_chdir,
                    has_split_string,
                    env_options_uncertain,
                    exhausted: false,
                };
            }
        }
    }
    CheckedWrapperPeel {
        words: current,
        has_chdir,
        has_split_string,
        env_options_uncertain,
        exhausted: is_wrapper_command(current) && strip_wrapper_command(current).is_some(),
    }
}

/// Repeatedly strip wrapper commands (e.g. `timeout 30 nice -n 10 rm -rf /`).
/// Bounded to avoid pathological loops. Returns the original slice if no
/// wrapper is present.
pub(crate) fn unwrap_wrappers(words: &[String]) -> &[String] {
    unwrap_wrappers_checked(words).words
}

/// Peel wrapper commands (`timeout`, `nice`, `env`, …) from a command's words,
/// exposing the same normalization the permission enforcer applies before
/// matching session grants. The pager's "Always allow" pattern editor uses this
/// so its pre-fill and match preview agree with enforcement on wrapped commands.
pub fn unwrap_command_wrappers(words: &[String]) -> &[String] {
    unwrap_wrappers(words)
}

/// Result of peeling shell-transparent prefixes (`exec` / `command` / `builtin`).
/// Not part of the canonical wrapper set — used only by security gates.
pub(crate) enum TransparentPrefixPeel<'a> {
    Ready(&'a [String]),
    Ambiguous,
}

pub(crate) const MAX_TRANSPARENT_PREFIX_DEPTH: usize = 8;
/// Bound on alternating wrapper ↔ transparent normalize rounds.
pub(crate) const MAX_NORMALIZE_ROUNDS: usize = MAX_WRAPPER_DEPTH + MAX_TRANSPARENT_PREFIX_DEPTH;

/// Peel `exec`/`command`/`builtin` after canonical wrappers. Unknown options and
/// depth exhaustion (a ninth peelable transparent prefix remains) Ask.
pub(crate) fn peel_transparent_prefixes(words: &[String]) -> TransparentPrefixPeel<'_> {
    let mut current = words;
    for _ in 0..MAX_TRANSPARENT_PREFIX_DEPTH {
        match strip_transparent_prefix(current) {
            TransparentStrip::NotPrefix => return TransparentPrefixPeel::Ready(current),
            TransparentStrip::Peeled(inner) => current = inner,
            TransparentStrip::Ambiguous => return TransparentPrefixPeel::Ambiguous,
        }
    }
    // WHY: mirror wrapper exhaustion — a remaining peelable prefix is unmodeled.
    match strip_transparent_prefix(current) {
        TransparentStrip::NotPrefix => TransparentPrefixPeel::Ready(current),
        TransparentStrip::Peeled(_) | TransparentStrip::Ambiguous => {
            TransparentPrefixPeel::Ambiguous
        }
    }
}

/// Normalized argv after bounded alternation of wrapper and transparent peels.
pub(crate) struct NormalizedCommandPeel<'a> {
    pub(crate) words: &'a [String],
    pub(crate) has_chdir: bool,
    pub(crate) has_split_string: bool,
    pub(crate) env_options_uncertain: bool,
    pub(crate) exhausted: bool,
    pub(crate) ambiguous: bool,
}

/// Alternate canonical wrappers and transparent prefixes to a bounded fixed
/// point so shapes like `command timeout command env -S …` surface the pack.
/// Ambiguity, depth exhaustion, chdir, split-string, and uncertain env options
/// all fail closed (callers Ask).
pub(crate) fn normalize_command_words(words: &[String]) -> NormalizedCommandPeel<'_> {
    let mut current = words;
    let mut has_chdir = false;
    let mut has_split_string = false;
    let mut env_options_uncertain = false;
    let mut exhausted = false;
    let mut ambiguous = false;

    for _ in 0..MAX_NORMALIZE_ROUNDS {
        let start_len = current.len();
        let start_ptr = current.as_ptr();

        let wrapped = unwrap_wrappers_checked(current);
        has_chdir |= wrapped.has_chdir;
        has_split_string |= wrapped.has_split_string;
        env_options_uncertain |= wrapped.env_options_uncertain;
        exhausted |= wrapped.exhausted;
        current = wrapped.words;

        // Opaque pack / uncertain env options: do not peel further.
        if has_split_string || env_options_uncertain {
            break;
        }

        match peel_transparent_prefixes(current) {
            TransparentPrefixPeel::Ready(inner) => current = inner,
            TransparentPrefixPeel::Ambiguous => {
                ambiguous = true;
                break;
            }
        }

        if current.len() == start_len && std::ptr::eq(current.as_ptr(), start_ptr) {
            break;
        }
    }

    if !ambiguous && !has_split_string && !env_options_uncertain {
        // Remaining peelable head after the budget is unmodeled → fail closed.
        let still_wrapper = is_wrapper_command(current) && strip_wrapper_command(current).is_some();
        let still_transparent = !matches!(
            strip_transparent_prefix(current),
            TransparentStrip::NotPrefix
        );
        if still_wrapper || still_transparent {
            exhausted = true;
        }
    }

    NormalizedCommandPeel {
        words: current,
        has_chdir,
        has_split_string,
        env_options_uncertain,
        exhausted,
        ambiguous,
    }
}

enum TransparentStrip<'a> {
    NotPrefix,
    Peeled(&'a [String]),
    Ambiguous,
}

fn strip_transparent_prefix(cmd: &[String]) -> TransparentStrip<'_> {
    let head = match cmd.first().map(String::as_str) {
        Some(h) if !h.is_empty() && h != "\0" => h.rsplit(['/', '\\']).next().unwrap_or(h),
        _ => return TransparentStrip::NotPrefix,
    };
    match head {
        "exec" => strip_exec_prefix(cmd),
        "command" => strip_command_prefix(cmd),
        "builtin" => strip_builtin_prefix(cmd),
        _ => TransparentStrip::NotPrefix,
    }
}

/// `exec [-cl] [-a name] [command [arguments]]`
fn strip_exec_prefix(cmd: &[String]) -> TransparentStrip<'_> {
    let mut i = 1usize;
    while let Some(tok) = cmd.get(i).map(String::as_str) {
        if tok == "--" {
            i += 1;
            break;
        }
        if tok == "-" || !tok.starts_with('-') {
            break;
        }
        if matches!(tok, "-c" | "-l" | "-cl" | "-lc") {
            i += 1;
            continue;
        }
        if tok == "-a" {
            match cmd.get(i + 1).map(String::as_str) {
                Some(v) if v != "\0" && !v.is_empty() => i += 2,
                _ => return TransparentStrip::Ambiguous,
            }
            continue;
        }
        if let Some(rest) = tok.strip_prefix("-a").filter(|r| !r.is_empty()) {
            if rest == "\0" {
                return TransparentStrip::Ambiguous;
            }
            i += 1;
            continue;
        }
        // Combined shorts that include only c/l plus optional glued -a are rare;
        // anything else is unmodeled.
        if tok.starts_with('-')
            && !tok.starts_with("--")
            && tok.chars().skip(1).all(|c| matches!(c, 'c' | 'l'))
        {
            i += 1;
            continue;
        }
        return TransparentStrip::Ambiguous;
    }
    match cmd.get(i..) {
        Some(inner) if !inner.is_empty() => TransparentStrip::Peeled(inner),
        // Redirect-only `exec` has no command word; leave it for the AST redirect path.
        _ => TransparentStrip::NotPrefix,
    }
}

/// `command [-p] command [arguments]` — do not peel display forms `-v`/`-V`.
fn strip_command_prefix(cmd: &[String]) -> TransparentStrip<'_> {
    let mut i = 1usize;
    while let Some(tok) = cmd.get(i).map(String::as_str) {
        if tok == "--" {
            i += 1;
            break;
        }
        if tok == "-" || !tok.starts_with('-') {
            break;
        }
        if matches!(tok, "-v" | "-V" | "--help" | "--version")
            || (tok.starts_with('-')
                && !tok.starts_with("--")
                && tok.chars().skip(1).any(|c| c == 'v' || c == 'V'))
        {
            // Display/query mode is not an execute peel.
            return TransparentStrip::NotPrefix;
        }
        if tok == "-p"
            || (tok.starts_with('-')
                && !tok.starts_with("--")
                && tok.chars().skip(1).all(|c| c == 'p'))
        {
            i += 1;
            continue;
        }
        return TransparentStrip::Ambiguous;
    }
    match cmd.get(i..) {
        Some(inner) if !inner.is_empty() => TransparentStrip::Peeled(inner),
        _ => TransparentStrip::NotPrefix,
    }
}

/// `builtin [shell-builtin [arguments]]` — no options modeled.
fn strip_builtin_prefix(cmd: &[String]) -> TransparentStrip<'_> {
    match cmd.get(1).map(String::as_str) {
        None => TransparentStrip::NotPrefix,
        Some(tok) if tok.starts_with('-') && tok != "-" => TransparentStrip::Ambiguous,
        Some(_) => match cmd.get(1..) {
            Some(inner) if !inner.is_empty() => TransparentStrip::Peeled(inner),
            _ => TransparentStrip::NotPrefix,
        },
    }
}

/// Simple shell-like splitter that:
/// - splits on whitespace (outside of quotes)
/// - handles single and double quotes, removing the quotes
/// - handles backslash escapes in a basic way
fn sh_split_simple(s: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut chars = s.chars().peekable();
    let mut in_single = bool::default();
    let mut in_double = bool::default();

    while let Some(c) = chars.next() {
        match c {
            '\'' if !in_double => {
                in_single = !in_single;
            }
            '"' if !in_single => {
                in_double = !in_double;
            }
            '\\' => {
                // basic escape: take next char literally
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            c if c.is_whitespace() && !in_single && !in_double => {
                if !current.is_empty() {
                    result.push(current);
                    current = String::new();
                }
            }
            c => current.push(c),
        }
    }

    if !current.is_empty() {
        result.push(current);
    }

    result
}

/// Given a bash *script string* like:
///
/// ```bash
/// ECHO_AGENT_API_KEY='echo-agent-some-key' cargo run --bin echo-agent-pager
/// ```
///
/// returns the first "important" command as a `BashCommandHighlights` where:
/// - `prefix` = tokens before the highlighted command (env assignments, setup commands, operators)
/// - `highlighted_words` = the main command + args (like the old return)
/// - `suffix` = tokens after the highlighted command.
///
/// For the above example:
///   prefix: ["ECHO_AGENT_API_KEY=echo-agent-some-key"]
///   highlighted_words: ["cargo", "run", "--bin", "echo-agent-pager"]
///   suffix: []
pub fn primary_command_from_script(script: &str) -> Option<BashCommandHighlights> {
    let tree = try_parse_shell(script)?;
    let commands = try_parse_word_only_commands_sequence(&tree, script)?;

    // Peel wrappers before the setup check and before choosing the highlight:
    // enforcement matches grants against wrapper-peeled words (`evaluate_bash`
    // calls `unwrap_wrappers`), so an "Always allow" saved from unpeeled words
    // (`env FOO=1 …`) could never match. Peeling first also gives a script
    // that is only `timeout 30 cargo test` a primary command, so its prompt
    // keeps the always-allow rows.
    let primary = commands.into_iter().find_map(|c| {
        let peeled = unwrap_wrappers(&c.words);
        if peeled.is_empty() || is_setup_command(peeled) {
            return None;
        }
        let peeled_off = c.words.len() - peeled.len();
        let peeled_words = c.words[peeled_off..].to_vec();
        Some((c, peeled_off, peeled_words))
    });
    let (primary, peeled_off, peeled_words) = primary?;

    let prefix_str = &script[..primary.span_start];
    let suffix_str = &script[primary.span_end..];

    // Peeled wrapper words stay visible as prefix context so the rendered
    // command is still the full invocation; only the grant scope narrows.
    let mut prefix = sh_split_simple(prefix_str);
    prefix.extend_from_slice(&primary.words[..peeled_off]);

    Some(BashCommandHighlights {
        prefix,
        highlighted_words: peeled_words,
        suffix: sh_split_simple(suffix_str),
    })
}

/// Parse all commands from a bash script using tree-sitter.
///
/// Returns `Some(Vec<PlainCommand>)` with every command in source order
/// (including "setup" commands like `cd`, `sleep`, etc.), or `None` if
/// the script contains constructs that tree-sitter-bash cannot cleanly
/// decompose into plain word-only commands.
pub fn all_commands_from_script(script: &str) -> Option<Vec<PlainCommand>> {
    let tree = try_parse_shell(script)?;
    try_parse_word_only_commands_sequence(&tree, script)
}

/// Parse a single `command` node into a list of "words", rejecting anything
/// non-trivial (substitutions, complex strings, etc.), and also returning
/// the byte span for the *highlighted* portion (command + args, not env).
fn parse_plain_command_from_node(cmd: Node, src: &str) -> Option<PlainCommand> {
    if cmd.kind() != "command" {
        return None;
    }
    let mut words = Vec::new();
    let mut cursor = cmd.walk();

    let mut span_start: Option<usize> = None;
    let mut span_end: Option<usize> = None;

    for child in cmd.named_children(&mut cursor) {
        match child.kind() {
            // We ignore simple env var assignments in front of the command.
            // Safety of their contents is already enforced by the outer whitelist.
            "variable_assignment" => {
                // no-op, just skip for words & span
            }
            "command_name" => {
                let word_node = child.named_child(0)?;
                if word_node.kind() != "word" {
                    return None;
                }
                let text = word_node.utf8_text(src.as_bytes()).ok()?.to_owned();

                if span_start.is_none() {
                    span_start = Some(word_node.start_byte());
                }
                span_end = Some(word_node.end_byte());

                words.push(text);
            }
            "word" | "number" => {
                let text = child.utf8_text(src.as_bytes()).ok()?.to_owned();

                if span_start.is_none() {
                    span_start = Some(child.start_byte());
                }
                span_end = Some(child.end_byte());

                words.push(text);
            }
            "string"
                // Allow only simple double-quoted strings with plain content.
                if child.child_count() == 3
                    && child.child(0)?.kind() == "\""
                    && child.child(1)?.kind() == "string_content"
                    && child.child(2)?.kind() == "\""
                => {
                    let content_node = child.child(1)?;
                    let text = content_node.utf8_text(src.as_bytes()).ok()?.to_owned();

                    if span_start.is_none() {
                        // Highlight whole quoted string in the original script,
                        // so span uses the outer node.
                        span_start = Some(child.start_byte());
                    }
                    span_end = Some(child.end_byte());

                    words.push(text);
                }
            "raw_string" => {
                let raw_string = child.utf8_text(src.as_bytes()).ok()?;
                let stripped = raw_string
                    .strip_prefix('\'')
                    .and_then(|s| s.strip_suffix('\''));
                if let Some(s) = stripped {
                    if span_start.is_none() {
                        span_start = Some(child.start_byte());
                    }
                    span_end = Some(child.end_byte());

                    words.push(s.to_owned());
                } else {
                    return None;
                }
            }
            "concatenation" => {
                // Handle concatenation nodes (e.g., {} in find -exec)
                let text = child.utf8_text(src.as_bytes()).ok()?.to_owned();

                if span_start.is_none() {
                    span_start = Some(child.start_byte());
                }
                span_end = Some(child.end_byte());

                words.push(text);
            }
            _ => return None,
        }
    }

    let span_start = span_start.unwrap_or_else(|| cmd.start_byte());
    let span_end = span_end.unwrap_or_else(|| cmd.end_byte());

    Some(PlainCommand {
        words,
        span_start,
        span_end,
    })
}

// ── Display soft-breaks (permission UI / formatting) ─────────────────

/// Node kinds whose descendants are payload text, not shell control flow.
/// Operators that appear only as *characters* inside these are not real
/// soft-break points (e.g. `&&` in a heredoc body or double-quoted string).
const PAYLOAD_NODE_KINDS: &[&str] = &[
    // Heredoc body / content (not the redirect operator itself on the
    // command line — `cat <<EOF && true` still has a real list `&&`).
    "heredoc_body",
    "simple_heredoc_body",
    "heredoc_content",
    "heredoc_end",
    // Quoted / expansion payload
    "string",
    "raw_string",
    "string_content",
    "ansi_c_string",
    "translated_string",
    // Comments
    "comment",
];

/// True when `kind` is a shell list/pipeline operator we may soft-break after.
fn is_soft_break_operator_kind(kind: &str) -> bool {
    matches!(kind, "&&" | "||" | "|" | ";")
}

/// True when we must not descend into this node looking for operators.
fn is_payload_node_kind(kind: &str) -> bool {
    PAYLOAD_NODE_KINDS.contains(&kind)
}

/// Byte offsets into `script` **after** real shell list/pipeline operators
/// where a display soft-wrap is safe.
///
/// Uses tree-sitter-bash so `&&` / `||` / `|` / `;` that appear only inside
/// strings, heredoc bodies, or comments are **not** returned. The command-line
/// operator in `cat <<EOF && echo after` **is** returned (it is a real `list`
/// operator); the body's `foo && bar` is not.
///
/// Returns an empty vec when the script cannot be parsed at all (caller should
/// fall back to width-only word-wrap, not naive substring splits).
///
/// Offsets are sorted ascending and de-duplicated. Each offset is
/// `operator_node.end_byte()` — i.e. the split keeps the operator on the
/// preceding display row.
pub fn soft_break_offsets_after_operators(script: &str) -> Vec<usize> {
    let Some(tree) = try_parse_shell(script) else {
        return Vec::new();
    };

    let root = tree.root_node();
    // On a broken parse, tree-sitter can still expose `|` / `&&` / `;` nodes
    // that are *not* real shell control flow (e.g. fragments of unclosed
    // strings or half-parsed heredocs). Prefer no soft-breaks over wrong ones.
    if root.has_error() {
        return Vec::new();
    }

    let mut breaks: Vec<usize> = Vec::new();
    let mut stack: Vec<Node> = vec![root];

    while let Some(node) = stack.pop() {
        let kind = node.kind();

        // Do not walk into string / heredoc / comment payload — any operator
        // characters there are not shell syntax nodes we care about, and
        // skipping the whole subtree is cheaper and safer.
        if is_payload_node_kind(kind) {
            continue;
        }

        if is_soft_break_operator_kind(kind) {
            let end = node.end_byte();
            if end > 0 && end <= script.len() && script.is_char_boundary(end) {
                breaks.push(end);
            }
        }

        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        for child in children.into_iter().rev() {
            stack.push(child);
        }
    }

    breaks.sort_unstable();
    breaks.dedup();
    breaks
}

/// Byte ranges of heredoc *payload* (body / content), not the `<<WORD` opener
/// on the command line.
///
/// Used by the permission overlay so physical lines that are pure heredoc
/// body text are **not** soft-wrapped at spaces (they are free-form payload,
/// not shell syntax). Returns an empty vec when the script cannot be parsed
/// or the tree has errors (same policy as [`soft_break_offsets_after_operators`]:
/// error recovery can invent bogus heredoc spans).
pub fn heredoc_payload_byte_ranges(script: &str) -> Vec<(usize, usize)> {
    let Some(tree) = try_parse_shell(script) else {
        return Vec::new();
    };

    let root = tree.root_node();
    // Match soft-break policy: on a broken parse, tree-sitter error recovery can
    // invent or mis-bound `heredoc_body` nodes. Prefer no payload ranges (normal
    // soft-wrap / no false no-wrap) over wrong spans that overflow or skip wraps.
    if root.has_error() {
        return Vec::new();
    }

    const HEREDOC_PAYLOAD: &[&str] = &["heredoc_body", "simple_heredoc_body", "heredoc_content"];

    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut stack: Vec<Node> = vec![root];

    while let Some(node) = stack.pop() {
        let kind = node.kind();
        if HEREDOC_PAYLOAD.contains(&kind) {
            let start = node.start_byte();
            let end = node.end_byte();
            if start < end && end <= script.len() {
                ranges.push((start, end));
            }
            // Do not walk into children — the outer body/content range is enough.
            continue;
        }
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        for child in children.into_iter().rev() {
            stack.push(child);
        }
    }

    ranges.sort_unstable();
    ranges.dedup();
    ranges
}

/// True when the half-open byte range `[start, end)` lies entirely inside one
/// of the given (sorted) payload ranges.
pub fn range_fully_inside(start: usize, end: usize, ranges: &[(usize, usize)]) -> bool {
    if end < start {
        return false;
    }
    ranges.iter().any(|&(rs, re)| start >= rs && end <= re)
}

/// Split `line` (a physical line whose first byte is at `line_start` in the
/// full script that produced `breaks`) into contiguous slices at any soft
/// breaks that fall strictly inside the line.
///
/// When there are no applicable breaks, returns a single-element vec with
/// `line` unchanged.
pub fn split_physical_line_at_soft_breaks<'a>(
    line: &'a str,
    line_start: usize,
    breaks: &[usize],
) -> Vec<&'a str> {
    let line_end = line_start + line.len();
    let mut rel: Vec<usize> = breaks
        .iter()
        .copied()
        .filter(|&b| b > line_start && b < line_end)
        .map(|b| b - line_start)
        .filter(|&b| line.is_char_boundary(b))
        .collect();
    rel.dedup();
    if rel.is_empty() {
        return vec![line];
    }

    let mut chunks = Vec::with_capacity(rel.len() + 1);
    let mut start = 0usize;
    for b in rel {
        if b > start {
            chunks.push(&line[start..b]);
            start = b;
        }
    }
    if start < line.len() {
        chunks.push(&line[start..]);
    }
    if chunks.is_empty() {
        chunks.push(line);
    }
    chunks
}

/// Slice of `script` at each soft-break, for tests / debugging. Each slice
/// ends at an operator (inclusive); the final slice is the remainder.
pub fn soft_break_chunks(script: &str) -> Vec<&str> {
    let breaks = soft_break_offsets_after_operators(script);
    if breaks.is_empty() {
        return vec![script];
    }
    let mut out = Vec::with_capacity(breaks.len() + 1);
    let mut start = 0usize;
    for b in breaks {
        if b > start && b <= script.len() {
            out.push(&script[start..b]);
            start = b;
        }
    }
    if start < script.len() {
        out.push(&script[start..]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{
        BashCommandHighlights, all_commands_from_script, env_split_string_script,
        heredoc_payload_byte_ranges, normalize_command_words, primary_command_from_script,
        range_fully_inside, soft_break_chunks, soft_break_offsets_after_operators,
        split_physical_line_at_soft_breaks, try_parse_shell, unwrap_wrappers_checked,
    };

    #[test]
    fn env_split_string_scan_contract() {
        let words = |script: &str| {
            all_commands_from_script(script)
                .expect(script)
                .into_iter()
                .next()
                .expect(script)
                .words()
                .to_vec()
        };
        for script in [
            "env -S 'rm -rf x'",
            "env --split-string 'rm -rf x'",
            "env --split-string='rm -rf x'",
            "env -S'rm -rf x'",
        ] {
            let w = words(script);
            let peeled = unwrap_wrappers_checked(&w);
            assert!(peeled.has_split_string, "{script}");
            assert_eq!(
                env_split_string_script(&w).as_deref(),
                Some("rm -rf x"),
                "{script}"
            );
            assert_eq!(
                peeled.words.first().map(String::as_str),
                Some("env"),
                "{script}"
            );
        }
        for script in [
            "env -iS 'rm -rf x'",
            "env -vS 'rm -rf x'",
            "env -0S 'rm -rf x'",
        ] {
            let w = words(script);
            let peeled = unwrap_wrappers_checked(&w);
            assert!(peeled.has_split_string, "{script}");
            assert!(env_split_string_script(&w).is_none(), "{script}");
        }
        let unknown_cluster = words("env -xS 'rm -rf x'");
        let peeled = unwrap_wrappers_checked(&unknown_cluster);
        assert!(peeled.env_options_uncertain);
        assert!(!peeled.has_split_string);
        assert!(env_split_string_script(&unknown_cluster).is_none());
        for script in [
            "env -uS rm -rf x",
            "env -CS rm -rf x",
            "env -PSfoo rm -rf x",
        ] {
            let w = words(script);
            let peeled = unwrap_wrappers_checked(&w);
            assert!(!peeled.has_split_string, "{script}");
            assert!(env_split_string_script(&w).is_none(), "{script}");
        }
        for script in [
            "env -P /usr/bin -S 'rm -rf x'",
            "env --path /usr/bin -S 'rm -rf x'",
            "env --path=/usr/bin -S 'rm -rf x'",
            "env -a name -S 'rm -rf x'",
            "env -u NAME -S 'rm -rf x'",
            "env -C /tmp -S 'rm -rf x'",
            "env - -S 'rm -rf x'",
            "env -iv -S 'rm -rf x'",
        ] {
            let w = words(script);
            let peeled = unwrap_wrappers_checked(&w);
            assert!(peeled.has_split_string, "{script}");
            assert_eq!(
                env_split_string_script(&w).as_deref(),
                Some("rm -rf x"),
                "{script}"
            );
            assert_eq!(
                peeled.words.first().map(String::as_str),
                Some("env"),
                "{script}"
            );
        }
        for script in [
            r"env -S '\trm -rf x'",
            r"env -S '\nrm -rf x'",
            "env -S 'echo $HOME'",
            "env -S 'rm #x'",
            "env -S",
        ] {
            let w = words(script);
            let peeled = unwrap_wrappers_checked(&w);
            assert!(peeled.has_split_string, "{script}");
            assert!(env_split_string_script(&w).is_none(), "{script}");
        }
        let tabbed = words("env -S 'rm\t-rf x'");
        assert!(unwrap_wrappers_checked(&tabbed).has_split_string);
        assert_eq!(
            env_split_string_script(&tabbed).as_deref(),
            Some("rm\t-rf x")
        );
        let dashed = words("env -- -S 'rm -rf x'");
        let peeled = unwrap_wrappers_checked(&dashed);
        assert!(!peeled.has_split_string);
        assert!(env_split_string_script(&dashed).is_none());
        for script in [
            "env --block-signal SEGV -S 'rm -rf x'",
            "env -x foo -S 'rm -rf x'",
            "env --prefix /usr/bin -S 'rm -rf x'",
        ] {
            let w = words(script);
            let peeled = unwrap_wrappers_checked(&w);
            assert!(peeled.env_options_uncertain, "{script}");
            assert!(!peeled.has_split_string, "{script}");
        }
        let ordinary = words("env FOO=1 rm -rf x");
        let peeled = unwrap_wrappers_checked(&ordinary);
        assert!(!peeled.has_split_string);
        assert_eq!(
            peeled.words.iter().map(String::as_str).collect::<Vec<_>>(),
            vec!["rm", "-rf", "x"]
        );
        let prefix = words("env -P /usr/bin rm -rf x");
        let peeled = unwrap_wrappers_checked(&prefix);
        assert!(!peeled.has_split_string && !peeled.env_options_uncertain);
        assert_eq!(
            peeled.words.iter().map(String::as_str).collect::<Vec<_>>(),
            vec!["rm", "-rf", "x"]
        );
        let deep = words("command timeout 5 command env -S 'rm -rf x'");
        let norm = normalize_command_words(&deep);
        assert!(norm.has_split_string);
        assert_eq!(
            env_split_string_script(norm.words).as_deref(),
            Some("rm -rf x")
        );
    }

    #[test]
    fn test_parse_plain_commands_from_script() {
        let command = "python3 something.py";
        assert_eq!(
            primary_command_from_script(command),
            Some(BashCommandHighlights {
                prefix: vec![],
                highlighted_words: vec!["python3".to_owned(), "something.py".to_owned()],
                suffix: vec![],
            })
        );

        let environment_key_command =
            "ECHO_AGENT_API_KEY='echo-agent-some-key' cargo run --bin echo-agent-pager";
        assert_eq!(
            primary_command_from_script(environment_key_command),
            Some(BashCommandHighlights {
                prefix: vec!["ECHO_AGENT_API_KEY=echo-agent-some-key".to_owned()],
                highlighted_words: vec![
                    "cargo".to_owned(),
                    "run".to_owned(),
                    "--bin".to_owned(),
                    "echo-agent-pager".to_owned()
                ],
                suffix: vec![],
            })
        );

        let cd_directory_command = "cd something/interesting/wow && python3 -c \"some text\\\" here \" | blah function important";
        assert_eq!(
            primary_command_from_script(cd_directory_command),
            Some(BashCommandHighlights {
                prefix: vec![
                    "cd".to_owned(),
                    "something/interesting/wow".to_owned(),
                    "&&".to_owned()
                ],
                highlighted_words: vec![
                    "python3".to_owned(),
                    "-c".to_owned(),
                    "some text\\\" here ".to_owned()
                ],
                suffix: vec![
                    "|".to_owned(),
                    "blah".to_owned(),
                    "function".to_owned(),
                    "important".to_owned()
                ],
            })
        );

        let redirection_command = "cargo build --bin echo-agent-pager 2>&1";
        assert_eq!(
            primary_command_from_script(redirection_command),
            Some(BashCommandHighlights {
                prefix: vec![],
                highlighted_words: vec![
                    "cargo".to_owned(),
                    "build".to_owned(),
                    "--bin".to_owned(),
                    "echo-agent-pager".to_owned(),
                ],
                suffix: vec!["2>&1".to_owned(),],
            })
        );

        let long_command = "grep -r \"ToolKind\" ~/.cargo/registry/src/ 2>/dev/null | grep \"enum\" | head -5 || find ~/.cargo -name \"*.rs\" -exec grep -l \"enum ToolKind\" {} \\; 2>/dev/null | head -5";
        assert_eq!(
            primary_command_from_script(long_command),
            Some(BashCommandHighlights {
                prefix: vec![],
                highlighted_words: vec![
                    "grep".to_owned(),
                    "-r".to_owned(),
                    "ToolKind".to_owned(),
                    "~/.cargo/registry/src/".to_owned(),
                ],
                suffix: vec![
                    "2>/dev/null".to_owned(),
                    "|".to_owned(),
                    "grep".to_owned(),
                    "enum".to_owned(),
                    "|".to_owned(),
                    "head".to_owned(),
                    "-5".to_owned(),
                    "||".to_owned(),
                    "find".to_owned(),
                    "~/.cargo".to_owned(),
                    "-name".to_owned(),
                    "*.rs".to_owned(),
                    "-exec".to_owned(),
                    "grep".to_owned(),
                    "-l".to_owned(),
                    "enum ToolKind".to_owned(),
                    "{}".to_owned(),
                    ";".to_owned(),
                    "2>/dev/null".to_owned(),
                    "|".to_owned(),
                    "head".to_owned(),
                    "-5".to_owned(),
                ],
            })
        );

        let another_long_command = "cargo test --package echo-agent-runtime --lib -- permission::bash_command_splitting::tests::test_parse_plain_commands_from_script --exact --nocapture 2>&1";
        assert_eq!(
            primary_command_from_script(another_long_command),
            Some(BashCommandHighlights {
                prefix: vec![],
                highlighted_words: vec![
                    "cargo".to_owned(),
                    "test".to_owned(),
                    "--package".to_owned(),
                    "echo-agent-runtime".to_owned(),
                    "--lib".to_owned(),
                    "--".to_owned(),
                    "permission::bash_command_splitting::tests::test_parse_plain_commands_from_script".to_owned(),
                    "--exact".to_owned(),
                    "--nocapture".to_owned(),
                ],
                suffix: vec!["2>&1".to_owned(),],
            })
        );

        let another_long_command_python = r#"python -c "
        import os
        os.environ.get("something")
        ""#;
        assert_eq!(
            primary_command_from_script(another_long_command_python),
            Some(BashCommandHighlights {
                prefix: vec![],
                highlighted_words: vec![
                    "python".to_owned(),
                    "-c".to_owned(),
                    "\"\n        import os\n        os.environ.get(\"something\")\n        \""
                        .to_owned()
                ],
                suffix: vec![],
            })
        );
    }

    #[test]
    fn test_sleep_and_timeout_commands_skipped() {
        // sleep 5 && foo should extract "foo" as the primary command
        let sleep_command = "sleep 5 && foo --bar";
        assert_eq!(
            primary_command_from_script(sleep_command),
            Some(BashCommandHighlights {
                prefix: vec!["sleep".to_owned(), "5".to_owned(), "&&".to_owned()],
                highlighted_words: vec!["foo".to_owned(), "--bar".to_owned()],
                suffix: vec![],
            })
        );

        // timeout 60 && foo should extract "foo" as the primary command
        let timeout_command = "timeout 60 && foo --bar";
        assert_eq!(
            primary_command_from_script(timeout_command),
            Some(BashCommandHighlights {
                prefix: vec!["timeout".to_owned(), "60".to_owned(), "&&".to_owned()],
                highlighted_words: vec!["foo".to_owned(), "--bar".to_owned()],
                suffix: vec![],
            })
        );

        // sleep 5 && timeout 60 foo should extract "foo" as the primary command
        // Note: "timeout 60 foo" is parsed as one command where timeout takes 60 and foo as args
        // So the primary should skip sleep and get to the timeout command, but timeout is also skipped
        let sleep_timeout_command = "sleep 5 && timeout 60 && foo --bar";
        assert_eq!(
            primary_command_from_script(sleep_timeout_command),
            Some(BashCommandHighlights {
                prefix: vec![
                    "sleep".to_owned(),
                    "5".to_owned(),
                    "&&".to_owned(),
                    "timeout".to_owned(),
                    "60".to_owned(),
                    "&&".to_owned()
                ],
                highlighted_words: vec!["foo".to_owned(), "--bar".to_owned()],
                suffix: vec![],
            })
        );

        // Combined with cd: cd /path && sleep 5 && git status
        let cd_sleep_command = "cd /some/path && sleep 5 && git status";
        assert_eq!(
            primary_command_from_script(cd_sleep_command),
            Some(BashCommandHighlights {
                prefix: vec![
                    "cd".to_owned(),
                    "/some/path".to_owned(),
                    "&&".to_owned(),
                    "sleep".to_owned(),
                    "5".to_owned(),
                    "&&".to_owned()
                ],
                highlighted_words: vec!["git".to_owned(), "status".to_owned()],
                suffix: vec![],
            })
        );

        // Wrapped command: the wrapper peels into the prefix and the inner
        // program is the primary (see `primary_command_peels_wrappers_into_prefix`).
        let timeout_wrapped = "timeout 60 cargo test";
        assert_eq!(
            primary_command_from_script(timeout_wrapped),
            Some(BashCommandHighlights {
                prefix: vec!["timeout".to_owned(), "60".to_owned()],
                highlighted_words: vec!["cargo".to_owned(), "test".to_owned()],
                suffix: vec![],
            })
        );

        // But if we chain it properly with &&, we can extract the real command
        let timeout_chained = "timeout 60 && cargo test";
        assert_eq!(
            primary_command_from_script(timeout_chained),
            Some(BashCommandHighlights {
                prefix: vec!["timeout".to_owned(), "60".to_owned(), "&&".to_owned()],
                highlighted_words: vec!["cargo".to_owned(), "test".to_owned()],
                suffix: vec![],
            })
        );

        // Test sleep with no following command just returns None (no primary command)
        let sleep_only = "sleep 5";
        assert_eq!(primary_command_from_script(sleep_only), None);
    }

    /// The highlighted words must be the wrapper-peeled form — the exact words
    /// `evaluate_bash` compares grants against — or an "Always allow" saved
    /// from the highlight could never match at enforcement time.
    #[test]
    fn primary_command_peels_wrappers_into_prefix() {
        assert_eq!(
            primary_command_from_script("env FOO=1 cargo test"),
            Some(BashCommandHighlights {
                prefix: vec!["env".to_owned(), "FOO=1".to_owned()],
                highlighted_words: vec!["cargo".to_owned(), "test".to_owned()],
                suffix: vec![],
            })
        );
        assert_eq!(
            primary_command_from_script("nice -n 5 make -j8"),
            Some(BashCommandHighlights {
                prefix: vec!["nice".to_owned(), "-n".to_owned(), "5".to_owned()],
                highlighted_words: vec!["make".to_owned(), "-j8".to_owned()],
                suffix: vec![],
            })
        );
        // A bare wrapper with nothing inside is not a primary command.
        assert_eq!(primary_command_from_script("timeout 30"), None);
    }

    // ── soft_break_offsets_after_operators ────────────────────────────

    /// Helper: operators present at soft-break points (suffix of each prefix).
    fn break_operator_suffixes(script: &str) -> Vec<String> {
        soft_break_offsets_after_operators(script)
            .into_iter()
            .map(|b| {
                let prefix = &script[..b];
                // Take the trailing operator token (&&, ||, |, ;).
                if prefix.ends_with("&&") {
                    "&&".to_owned()
                } else if prefix.ends_with("||") {
                    "||".to_owned()
                } else if prefix.ends_with('|') {
                    "|".to_owned()
                } else if prefix.ends_with(';') {
                    ";".to_owned()
                } else {
                    prefix
                        .chars()
                        .rev()
                        .take(2)
                        .collect::<String>()
                        .chars()
                        .rev()
                        .collect()
                }
            })
            .collect()
    }

    #[test]
    fn soft_break_simple_and_and_or_pipe_semi() {
        let script = "git status && cargo test || true; echo done | cat";
        let ops = break_operator_suffixes(script);
        assert_eq!(ops, vec!["&&", "||", ";", "|"]);
        let chunks = soft_break_chunks(script);
        assert!(chunks[0].ends_with("&&"));
        assert!(chunks.last().unwrap().contains("cat"));
    }

    #[test]
    fn soft_break_ignores_and_inside_double_quotes() {
        let script = r#"echo "a && b" && echo real"#;
        let ops = break_operator_suffixes(script);
        // Only the real list operator after the closing quote.
        assert_eq!(ops, vec!["&&"]);
        let chunks = soft_break_chunks(script);
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].contains(r#""a && b""#));
        assert!(chunks[0].ends_with("&&"));
        assert!(chunks[1].contains("echo real"));
    }

    #[test]
    fn soft_break_ignores_and_inside_single_quotes() {
        let script = "echo 'x || y' || echo real";
        let ops = break_operator_suffixes(script);
        assert_eq!(ops, vec!["||"]);
    }

    #[test]
    fn soft_break_ignores_and_inside_heredoc_body() {
        let script = "cat <<EOF && echo after\nfoo && bar\nbaz || qux\nEOF";
        let ops = break_operator_suffixes(script);
        // Only the command-line list operator — not body `&&` / `||`.
        assert_eq!(
            ops,
            vec!["&&"],
            "body operators must not create soft-breaks; got {ops:?} for {script:?}"
        );
        // The single break must sit on the first physical line (after `&&`).
        let breaks = soft_break_offsets_after_operators(script);
        assert_eq!(breaks.len(), 1);
        let first_nl = script.find('\n').unwrap();
        assert!(
            breaks[0] <= first_nl,
            "break at {} should be on the opener line (nl at {first_nl})",
            breaks[0]
        );
        assert!(script[..breaks[0]].ends_with("&&"));
    }

    #[test]
    fn soft_break_ignores_and_in_heredoc_with_quoted_delimiter() {
        // Quoted delimiter still produces a heredoc body node.
        let script = "cat <<'END' && true\nkeep && this\nEND";
        let ops = break_operator_suffixes(script);
        assert_eq!(ops, vec!["&&"]);
    }

    #[test]
    fn soft_break_ignores_and_in_dash_heredoc_body() {
        // `<<-` (tab-stripping heredoc) must behave like `<<`: only the
        // command-line list operator is a break, not body operators.
        let script = "cat <<-EOF && echo after\n\tfoo && bar\n\tbaz | qux\n\tEOF";
        let ops = break_operator_suffixes(script);
        assert_eq!(
            ops,
            vec!["&&"],
            "dash-heredoc body operators must not break; got {ops:?}"
        );
    }

    #[test]
    fn soft_break_ignores_operators_in_comments() {
        let script = "echo hi # not && a break\necho real && echo also";
        let ops = break_operator_suffixes(script);
        assert_eq!(ops, vec!["&&"]);
        let breaks = soft_break_offsets_after_operators(script);
        // The break is on the second line.
        assert!(script[..breaks[0]].contains("echo real"));
    }

    #[test]
    fn soft_break_pipeline_only() {
        let script = "ps aux | grep foo | head -n 5";
        assert_eq!(break_operator_suffixes(script), vec!["|", "|"]);
    }

    #[test]
    fn soft_break_no_operators_returns_empty() {
        assert!(soft_break_offsets_after_operators("echo hello world").is_empty());
        assert_eq!(
            soft_break_chunks("echo hello world"),
            vec!["echo hello world"]
        );
    }

    #[test]
    fn soft_break_empty_script() {
        assert!(soft_break_offsets_after_operators("").is_empty());
    }

    #[test]
    fn soft_break_tight_operators_without_spaces() {
        // tree-sitter still emits `&&` nodes without surrounding spaces.
        let script = "true&&false||true";
        assert_eq!(break_operator_suffixes(script), vec!["&&", "||"]);
    }

    #[test]
    fn soft_break_does_not_split_on_ampersand_background() {
        // trailing `&` for background is not `&&` — must not produce a break.
        let script = "sleep 1 &";
        assert!(
            soft_break_offsets_after_operators(script).is_empty(),
            "background & is not a soft-break operator"
        );
    }

    #[test]
    fn split_physical_line_filters_breaks_to_line_range() {
        let script = "a && b\nc && d";
        let breaks = soft_break_offsets_after_operators(script);
        assert_eq!(breaks.len(), 2);

        let line0 = "a && b";
        let chunks0 = split_physical_line_at_soft_breaks(line0, 0, &breaks);
        assert_eq!(chunks0.len(), 2);
        assert!(chunks0[0].ends_with("&&"));
        assert_eq!(chunks0[1].trim(), "b");

        let line1_start = script.find('\n').unwrap() + 1;
        let line1 = "c && d";
        let chunks1 = split_physical_line_at_soft_breaks(line1, line1_start, &breaks);
        assert_eq!(chunks1.len(), 2);
        assert!(chunks1[0].ends_with("&&"));
        assert_eq!(chunks1[1].trim(), "d");
    }

    #[test]
    fn soft_break_nested_command_sub_still_sees_outer_list() {
        // Operators inside $() may or may not be soft-breaks depending on
        // whether we treat command_substitution as payload. We intentionally
        // still allow breaks inside $() (they're real shell ops for that
        // subshell) — only string/heredoc/comment are payload. Assert outer
        // list op is present either way.
        let script = "echo $(true && false) && echo outer";
        let ops = break_operator_suffixes(script);
        assert!(
            ops.iter().any(|o| o == "&&"),
            "expected at least the outer &&, got {ops:?}"
        );
        // Outer break: last one should be the list joining to `echo outer`.
        assert_eq!(ops.last().map(String::as_str), Some("&&"));
    }

    #[test]
    fn soft_break_multiline_backslash_continuation_with_and() {
        // Physical layout with `\` continuations; real `&&` between commands.
        let script = "cargo test \\\n  --all && \\\n  cargo clippy";
        let ops = break_operator_suffixes(script);
        assert_eq!(ops, vec!["&&"]);
    }

    #[test]
    fn soft_break_empty_on_parse_error() {
        // Broken scripts must not emit soft-breaks from half-parsed ops.
        // Unclosed quote / paren typically marks the tree as has_error().
        let broken = r#"echo "unclosed && true | false"#;
        let tree = try_parse_shell(broken).expect("tree-sitter still returns a tree");
        assert!(
            tree.root_node().has_error(),
            "expected error node for unclosed quote"
        );
        assert!(
            soft_break_offsets_after_operators(broken).is_empty(),
            "must not soft-break on a broken parse"
        );
    }

    #[test]
    fn heredoc_payload_empty_on_parse_error() {
        // Same policy as soft_break_empty_on_parse_error: error recovery must
        // not invent bogus heredoc payload spans.
        let broken = r#"echo "unclosed && true | false"#;
        let tree = try_parse_shell(broken).expect("tree-sitter still returns a tree");
        assert!(
            tree.root_node().has_error(),
            "expected error node for unclosed quote"
        );
        assert!(
            heredoc_payload_byte_ranges(broken).is_empty(),
            "must not emit heredoc payload ranges on a broken parse"
        );
        // Broken heredoc-looking input: unclosed quote + << can confuse recovery.
        let broken_heredocish = "cat <<EOF && echo \"unclosed\nbody\nEOF";
        if let Some(tree) = try_parse_shell(broken_heredocish)
            && tree.root_node().has_error()
        {
            assert!(
                heredoc_payload_byte_ranges(broken_heredocish).is_empty(),
                "broken heredoc-ish script must not invent payload ranges"
            );
        }
    }

    #[test]
    fn heredoc_payload_ranges_cover_body_not_opener() {
        let script = "cat <<EOF && echo after\nbody line with spaces here\nEOF";
        let ranges = heredoc_payload_byte_ranges(script);
        assert!(!ranges.is_empty(), "expected heredoc payload range");
        let body = "body line with spaces here";
        let body_start = script.find(body).unwrap();
        assert!(
            range_fully_inside(body_start, body_start + body.len(), &ranges),
            "body must be inside payload ranges: {ranges:?}"
        );
        // Opener command line is not fully inside payload.
        assert!(!range_fully_inside(0, script.find('\n').unwrap(), &ranges));
    }

    #[test]
    fn soft_break_gh_jq_pipeline_inside_single_quotes_is_not_a_break() {
        // Regression: `jq '.[] | ...'` must not soft-break at the `|` — it is
        // inside a single-quoted raw_string, not a shell pipeline.
        let script = r#"gh api user --jq '.login' && echo "---" && gh search prs --author=@me --sort=updated --limit=15 --json number,title,url,state,updatedAt,repository,isDraft --jq '.[] | "\(.state)\t#\(.number)\t\(.updatedAt)\t\(.repository.nameWithOwner)\t\(.title)\t\(.url)"'"#;
        let ops = break_operator_suffixes(script);
        assert_eq!(
            ops,
            vec!["&&", "&&"],
            "only the two real list operators; got {ops:?}"
        );
        assert!(
            !ops.iter().any(|o| *o == "|"),
            "jq `|` inside quotes must not be a soft-break"
        );
    }
}
