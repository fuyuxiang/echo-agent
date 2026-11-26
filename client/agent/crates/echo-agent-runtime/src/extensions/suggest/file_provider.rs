//! Filesystem completion for the shell token under the cursor: any
//! command's file arguments (plus path-like first tokens and redirection
//! targets), with fuzzy matching, shell quoting, and `~`/`$VAR` awareness.
//!
//! Token *syntax* — the minimal tokenizer and the re-quoting rules, with
//! their documented limits — lives in [`super::shell_token`]. This module
//! owns the completion *policy*: which tokens complete, directory
//! listing/ranking, and `~`/`$VAR` expansion. Expansion only picks the
//! directory to LIST (quotes do not suppress it: `'$HOME'/x` lists like
//! `$HOME/x`); the inserted completion always preserves the user's typed
//! prefix verbatim.

use std::path::{Path, PathBuf};

use nucleo::pattern::{Atom, AtomKind, CaseMatching, Normalization};

use super::shell_token::{CurrentToken, build_insert_token, parse_current_token};
use super::{RankedSuggestion, SuggestContext, SuggestionSource, splice_token_into_line};

/// Ranked results returned per request. The dropdown renders 6 rows and
/// scrolls; ranking happens BEFORE this cap so directories and the best
/// fuzzy matches survive it.
const MAX_RESULTS: usize = 50;

/// Directory-scan cap guarding pathological directories (the same guard the
/// `/export` path completer uses).
const SCAN_CAP: usize = 1000;

/// Symlink-classification stat budget per scan: a directory of up to
/// [`SCAN_CAP`] symlinks would otherwise serialize that many `stat`s
/// (hundreds of ms locally, worse on network filesystems). Past the budget
/// a symlink classifies as a file — worst case a symlinked directory loses
/// its trailing `/` and dirs-first ranking.
const SYMLINK_STAT_BUDGET: usize = 64;

/// Commands whose file arguments get a small ranking BOOST — not a gate:
/// any command's arguments file-complete. Must stay sorted (binary_search).
const FILE_COMMANDS: &[&str] = &[
    "awk", "bat", "cat", "cd", "chmod", "chown", "code", "cp", "diff", "file", "find", "grep",
    "head", "less", "ln", "ls", "mkdir", "mv", "nano", "nvim", "rm", "sed", "sort", "source",
    "stat", "tail", "touch", "vi", "vim", "wc",
];

/// Priority bump for candidates when the segment's command is a known file
/// consumer: above $PATH rows (priority 0) AND above the history tail
/// (history base decays to 1 by list position — boosted file rows
/// deliberately displace the weakest history matches); below mid/top
/// history rows (base up to 10, +30 exact).
///
/// Every candidate in one response carries the SAME priority: ordering
/// within the response is provider-internal (tier → score → dirs-first →
/// name) and survives to the wire only because `aggregate`'s sort is
/// STABLE (see `mod.rs`).
const FILE_CMD_BOOST: i32 = 2;

pub(crate) struct FilePathProvider;

impl FilePathProvider {
    pub(crate) async fn suggest(&self, ctx: &SuggestContext) -> Vec<RankedSuggestion> {
        // shell_token quoting is POSIX-only: cmd/pwsh would misparse the
        // escaped line, so Windows serves no deterministic completions.
        if cfg!(windows) {
            return Vec::new();
        }
        let tok = match extract_file_context(ctx.prefix()) {
            Some(t) => t,
            None => return Vec::new(),
        };
        // Completions replace the whole quoted/escaped token up to the cursor.
        let arg_range = (tok.start, ctx.prefix().len());
        let split = split_token(
            &tok,
            &ctx.text,
            &ctx.cwd,
            echo_agent_config::discovery_home_dir().as_deref(),
            |name| std::env::var(name).ok(),
        );
        let (entries, truncated) = list_ranked_entries(&split.list_dir, split.match_prefix).await;
        let boost = file_command_boost(tok.command.as_deref());

        let mut results: Vec<RankedSuggestion> = entries
            .into_iter()
            .map(|e| RankedSuggestion {
                // Token replacement for the arg range: the completed path,
                // re-quoted to match how the user opened the token.
                insert_text: build_insert_token(&tok, &split.raw_dir, &e.name, e.is_dir),
                display: if e.is_dir {
                    format!("{}/", e.name)
                } else {
                    e.name
                },
                description: if e.is_dir {
                    "directory".into()
                } else {
                    String::new()
                },
                source: SuggestionSource::File,
                priority: boost,
                is_ghost_candidate: false,
                replace_range: Some(arg_range),
                token_text: None,
                truncated,
            })
            .collect();
        splice_token_into_line(&mut results, &ctx.text, arg_range);
        results
    }
}

/// Decide whether the token under the cursor file-completes:
/// - flag-looking tokens (`-x`, `--foo`) never do;
/// - any command's arguments (non-first tokens) and redirection targets do;
/// - a first token only when path-like (`./script.sh`, `/bin/…`, `~`) —
///   plain first words are the $PATH provider's turf.
fn extract_file_context(prefix: &str) -> Option<CurrentToken> {
    let tok = parse_current_token(prefix);
    if tok.value.starts_with('-') {
        return None;
    }
    if tok.tokens_before == 0 && !tok.after_redirect && !is_path_like(&tok.value) {
        return None;
    }
    Some(tok)
}

fn is_path_like(s: &str) -> bool {
    s.contains('/') || s == "~"
}

// ── Directory/prefix split + `~`/`$VAR` expansion (listing only) ────────

struct SplitToken<'a> {
    /// Expanded directory to list (absolute, or joined onto the cwd).
    list_dir: PathBuf,
    /// Final path component typed so far (unquoted) — the match needle.
    match_prefix: &'a str,
    /// Verbatim request-text slice kept in front of every completion, so
    /// the user's own quoting/escapes/`~`/`$VAR` spellings survive.
    raw_dir: String,
}

fn split_token<'a>(
    tok: &'a CurrentToken,
    text: &str,
    cwd: &str,
    home: Option<&Path>,
    lookup: impl Fn(&str) -> Option<String>,
) -> SplitToken<'a> {
    // Bare `~` completes as `~/…`: list the home directory. With NO
    // resolvable home, `~` stays literal — exactly what the shell's own
    // failed tilde expansion does — so list `cwd/~` (usually nothing) like
    // the `~/x` arm below. Falling back to listing the cwd itself would
    // show files the accepted `~/…` insert can never name. A quoted or
    // escaped `~` is shell-literal — the general path matches it against
    // cwd entries instead.
    if tok.value == "~" && tok.dir_value_len.is_none() && tok.plain_mask.first() == Some(&true) {
        return SplitToken {
            list_dir: home.map_or_else(|| Path::new(cwd).join("~"), Path::to_path_buf),
            match_prefix: "",
            raw_dir: "~/".to_owned(),
        };
    }
    let split_at = tok.dir_value_len.unwrap_or(0);
    let expanded = expand_for_listing(
        &tok.value[..split_at],
        &tok.plain_mask[..split_at],
        home,
        lookup,
    );
    let list_dir = if expanded.as_os_str().is_empty() {
        PathBuf::from(cwd)
    } else if expanded.is_absolute() {
        expanded
    } else {
        Path::new(cwd).join(expanded)
    };
    SplitToken {
        list_dir,
        match_prefix: &tok.value[split_at..],
        raw_dir: text[tok.start..tok.dir_raw_end].to_owned(),
    }
}

/// Expand `~/` and `$VAR`/`${VAR}` in the directory part, for LISTING only
/// and only where the shell itself would: `plain` (byte-aligned with
/// `dir_value`) marks chars typed unquoted and unescaped, so `'$HOME'/x`,
/// `\$HOME/x`, and `"~/x` stay literal. Deliberately conservative:
/// double-quoted `$VAR`, which bash would expand, stays literal too.
/// Unset variables and `~user` forms stay literal (the listing just comes
/// up empty); the inserted text never contains the expansion.
fn expand_for_listing(
    dir_value: &str,
    plain: &[bool],
    home: Option<&Path>,
    lookup: impl Fn(&str) -> Option<String>,
) -> PathBuf {
    // Tilde first (always at the word start), vars on the remainder — with
    // its own mask slice, so provenance stays byte-aligned after the home
    // prefix replaces `~`.
    if let (Some(rest), Some(h)) = (dir_value.strip_prefix("~/"), home)
        && plain.first() == Some(&true)
    {
        return PathBuf::from(format!(
            "{}/{}",
            h.to_string_lossy(),
            expand_vars(rest, &plain[2..], lookup)
        ));
    }
    PathBuf::from(expand_vars(dir_value, plain, lookup))
}

/// Replace `$NAME` / `${NAME}` with `lookup(NAME)` when set; anything else
/// (unset vars, a lone `$`, `$1`-style digits, or a `$` the user quoted or
/// escaped — `plain` is byte-aligned with `s`) stays literal.
fn expand_vars(s: &str, plain: &[bool], lookup: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find('$') {
        out.push_str(&rest[..pos]);
        if plain.get(s.len() - rest.len() + pos) != Some(&true) {
            out.push('$');
            rest = &rest[pos + 1..];
            continue;
        }
        let after = &rest[pos + 1..];
        // `(name, token_len)`: bytes consumed starting at the `$`.
        let (name, token_len) = match after.strip_prefix('{') {
            Some(inner) => match inner.find('}') {
                Some(end) => (&inner[..end], 2 + end + 1),
                None => ("", 1),
            },
            None => {
                let end = after
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .unwrap_or(after.len());
                (&after[..end], 1 + end)
            }
        };
        let valid = !name.is_empty()
            && !name.starts_with(|c: char| c.is_ascii_digit())
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !valid {
            out.push('$');
            rest = &rest[pos + 1..];
            continue;
        }
        match lookup(name) {
            Some(v) => out.push_str(&v),
            None => out.push_str(&rest[pos..pos + token_len]),
        }
        rest = &rest[pos + token_len..];
    }
    out.push_str(rest);
    out
}

// ── Matching + ranking ──────────────────────────────────────────────────

struct ScoredEntry {
    name: String,
    is_dir: bool,
    /// 0 = exact prefix, 1 = case-insensitive prefix, 2 = fuzzy.
    tier: u8,
    score: u32,
}

/// List `dir` and rank matches: exact-prefix, then case-insensitive prefix,
/// then nucleo fuzzy — score descending, directories first, name ascending
/// within ties. Ranking happens BEFORE the [`MAX_RESULTS`] cap so an
/// alphabetical scan order can never crowd directories or better matches
/// out. Hidden entries only list when the typed prefix starts with `.`.
///
/// The second return is `truncated`: the scan hit [`SCAN_CAP`] or the
/// ranked matches exceeded [`MAX_RESULTS`] — the returned set may be
/// incomplete, so the pager must not conclude from it (no insta-accept or
/// LCP fill over rows a hidden entry could disprove).
async fn list_ranked_entries(dir: &Path, match_prefix: &str) -> (Vec<ScoredEntry>, bool) {
    let mut read_dir = match tokio::fs::read_dir(dir).await {
        Ok(rd) => rd,
        Err(_) => return (Vec::new(), false),
    };

    let show_hidden = match_prefix.starts_with('.');
    let prefix_lower = match_prefix.to_lowercase();
    let atom = (!match_prefix.is_empty()).then(|| {
        Atom::new(
            match_prefix,
            CaseMatching::Ignore,
            Normalization::Smart,
            AtomKind::Fuzzy,
            false,
        )
    });
    let mut config = nucleo::Config::DEFAULT;
    config.prefer_prefix = true;
    let mut matcher = nucleo::Matcher::new(config);
    let mut buf = Vec::new();

    let mut entries: Vec<ScoredEntry> = Vec::new();
    let mut scanned = 0usize;
    let mut symlink_stats = 0usize;
    let mut truncated = false;
    loop {
        scanned += 1;
        if scanned > SCAN_CAP {
            truncated = true;
            break;
        }
        let entry = match read_dir.next_entry().await {
            Ok(Some(entry)) => entry,
            Ok(None) => break,
            Err(_) => continue,
        };

        let name = match entry.file_name().into_string() {
            Ok(n) => n,
            Err(_) => continue,
        };
        if name.starts_with('.') && !show_hidden {
            continue;
        }

        let (tier, score) = match match_prefix.is_empty() {
            true => (0, 0),
            false => {
                let score = atom
                    .as_ref()
                    .and_then(|a| a.score(nucleo::Utf32Str::new(&name, &mut buf), &mut matcher))
                    .map(u32::from);
                if name.starts_with(match_prefix) {
                    (0, score.unwrap_or(0))
                } else if ci_starts_with(&name, &prefix_lower) {
                    (1, score.unwrap_or(0))
                } else {
                    match score {
                        Some(s) => (2, s),
                        None => continue,
                    }
                }
            }
        };

        // `file_type()` is free on most Unix (it comes from the dirent);
        // only symlinks need the full `stat` — following them keeps the
        // trailing `/` on symlinked directories — and those stats are
        // budgeted (see [`SYMLINK_STAT_BUDGET`]).
        let is_dir = match entry.file_type().await {
            Ok(ft) if ft.is_symlink() && symlink_stats < SYMLINK_STAT_BUDGET => {
                symlink_stats += 1;
                tokio::fs::metadata(entry.path())
                    .await
                    .map(|m| m.is_dir())
                    .unwrap_or(false)
            }
            Ok(ft) => ft.is_dir(),
            Err(_) => false,
        };

        entries.push(ScoredEntry {
            name,
            is_dir,
            tier,
            score,
        });
    }

    entries.sort_unstable_by(|a, b| {
        a.tier
            .cmp(&b.tier)
            .then_with(|| b.score.cmp(&a.score))
            .then_with(|| b.is_dir.cmp(&a.is_dir))
            .then_with(|| a.name.cmp(&b.name))
    });
    truncated |= entries.len() > MAX_RESULTS;
    entries.truncate(MAX_RESULTS);
    (entries, truncated)
}

fn file_command_boost(command: Option<&str>) -> i32 {
    let Some(cmd) = command else { return 0 };
    let base = cmd.rsplit('/').next().unwrap_or(cmd);
    if FILE_COMMANDS.binary_search(&base).is_ok() {
        FILE_CMD_BOOST
    } else {
        0
    }
}

/// Case-insensitive prefix test without a per-entry `to_lowercase`
/// allocation (`prefix_lower` is lowered once per request). Char-fold
/// equivalent of `name.to_lowercase().starts_with(prefix_lower)`.
fn ci_starts_with(name: &str, prefix_lower: &str) -> bool {
    let mut folded = name.chars().flat_map(char::to_lowercase);
    prefix_lower.chars().all(|p| folded.next() == Some(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- extract_file_context (completion decision) ---

    #[test]
    fn context_file_cmd_with_arg() {
        let tok = extract_file_context("cat foo").unwrap();
        assert_eq!(tok.value, "foo");
        assert_eq!(tok.start, 4);
        assert_eq!(tok.tokens_before, 1);
        assert_eq!(tok.command.as_deref(), Some("cat"));
    }

    /// ANY command's argument file-completes — FILE_COMMANDS is only a
    /// ranking boost, never a gate.
    #[test]
    fn context_unknown_cmd_plain_arg_completes() {
        let tok = extract_file_context("git status").unwrap();
        assert_eq!(tok.value, "status");
        assert_eq!(tok.start, 4);
    }

    #[test]
    fn context_flag_tokens_never_complete() {
        assert!(extract_file_context("ls -l").is_none());
        assert!(extract_file_context("git --ver").is_none());
        assert!(extract_file_context("cat -").is_none());
    }

    #[test]
    fn context_trailing_space_any_cmd() {
        let tok = extract_file_context("cat ").unwrap();
        assert_eq!(tok.value, "");
        assert_eq!(tok.start, 4);
        let tok = extract_file_context("git ").unwrap();
        assert_eq!(tok.start, 4);
    }

    #[test]
    fn context_empty_and_whitespace_only() {
        assert!(extract_file_context("").is_none());
        assert!(extract_file_context("   ").is_none());
    }

    #[test]
    fn context_first_token_plain_word_no_activate() {
        assert!(extract_file_context("hello").is_none());
        assert!(extract_file_context("cat").is_none());
    }

    #[test]
    fn context_first_token_path_like_completes() {
        let tok = extract_file_context("./src/main").unwrap();
        assert_eq!(tok.value, "./src/main");
        assert_eq!(tok.start, 0);
        assert!(extract_file_context("/usr/bin/ca").is_some());
        assert!(extract_file_context("~").is_some());
    }

    /// Redirection targets are file arguments even at command position
    /// (the parse mechanics live in shell_token's redirect test).
    #[test]
    fn context_redirect_target_completes() {
        assert!(extract_file_context("echo hi > lo").is_some());
        assert!(extract_file_context("> lo").is_some());
    }

    // --- is_path_like ---

    #[test]
    fn path_like_matrix() {
        assert!(is_path_like("/usr/bin"));
        assert!(is_path_like("./src"));
        assert!(is_path_like("../lib"));
        assert!(is_path_like("src/main.rs"));
        assert!(is_path_like("~"));
        assert!(!is_path_like("hello"));
        assert!(!is_path_like(""));
        assert!(!is_path_like("."));
        assert!(!is_path_like(".."));
        assert!(!is_path_like("~user"));
    }

    // --- split_token + expansion ---

    fn no_vars(_: &str) -> Option<String> {
        None
    }

    fn token_for(text: &str) -> CurrentToken {
        extract_file_context(text).expect("token")
    }

    #[test]
    fn split_absolute_path() {
        let tok = token_for("cat /usr/bin/gi");
        let s = split_token(&tok, "cat /usr/bin/gi", "/home", None, no_vars);
        assert_eq!(s.list_dir, PathBuf::from("/usr/bin/"));
        assert_eq!(s.match_prefix, "gi");
        assert_eq!(s.raw_dir, "/usr/bin/");
    }

    #[test]
    fn split_relative_path() {
        let tok = token_for("cat src/main");
        let s = split_token(&tok, "cat src/main", "/home/user", None, no_vars);
        assert_eq!(s.list_dir, PathBuf::from("/home/user/src/"));
        assert_eq!(s.match_prefix, "main");
        assert_eq!(s.raw_dir, "src/");
    }

    #[test]
    fn split_no_slash_uses_cwd() {
        let tok = token_for("cat foo");
        let s = split_token(&tok, "cat foo", "/tmp", None, no_vars);
        assert_eq!(s.list_dir, PathBuf::from("/tmp"));
        assert_eq!(s.match_prefix, "foo");
        assert_eq!(s.raw_dir, "");
    }

    #[test]
    fn split_trailing_slash_lists_that_dir() {
        let tok = token_for("cat src/");
        let s = split_token(&tok, "cat src/", "/home", None, no_vars);
        assert_eq!(s.list_dir, PathBuf::from("/home/src/"));
        assert_eq!(s.match_prefix, "");
        assert_eq!(s.raw_dir, "src/");
    }

    #[test]
    fn split_tilde_expands_home_for_listing_only() {
        let tok = token_for("cat ~/Do");
        let s = split_token(
            &tok,
            "cat ~/Do",
            "/ignored",
            Some(Path::new("/home/me")),
            no_vars,
        );
        assert_eq!(s.list_dir, PathBuf::from("/home/me/"));
        assert_eq!(s.match_prefix, "Do");
        // The insert prefix keeps the user's `~/`, never the expansion.
        assert_eq!(s.raw_dir, "~/");
    }

    #[test]
    fn split_bare_tilde_lists_home_as_tilde_slash() {
        let tok = token_for("cat ~");
        let s = split_token(
            &tok,
            "cat ~",
            "/ignored",
            Some(Path::new("/home/me")),
            no_vars,
        );
        assert_eq!(s.list_dir, PathBuf::from("/home/me"));
        assert_eq!(s.match_prefix, "");
        assert_eq!(s.raw_dir, "~/");
    }

    /// With NO resolvable home, bare `~` stays literal — the shell's own
    /// failed tilde expansion leaves the word unchanged — so the listing is
    /// `cwd/~` (usually empty), NEVER the cwd itself: cwd entries would be
    /// unreachable through the `~/…` insert. Degrades identically to the
    /// `~/x` arm, whose listing is pinned alongside.
    #[test]
    fn split_bare_tilde_without_home_stays_literal() {
        let tok = token_for("cat ~");
        let s = split_token(&tok, "cat ~", "/work", None, no_vars);
        assert_eq!(s.list_dir, PathBuf::from("/work/~"));
        assert_eq!(s.match_prefix, "");
        assert_eq!(s.raw_dir, "~/");

        let tok = token_for("cat ~/Do");
        let s = split_token(&tok, "cat ~/Do", "/work", None, no_vars);
        assert_eq!(s.list_dir, PathBuf::from("/work/~"), "same literal dir");
        assert_eq!(s.match_prefix, "Do");
        assert_eq!(s.raw_dir, "~/");
    }

    #[test]
    fn split_var_expands_for_listing_keeps_raw_insert() {
        let tok = token_for("cat $MYDIR/fi");
        let lookup = |name: &str| (name == "MYDIR").then(|| "/data/stuff".to_owned());
        let s = split_token(&tok, "cat $MYDIR/fi", "/ignored", None, lookup);
        assert_eq!(s.list_dir, PathBuf::from("/data/stuff/"));
        assert_eq!(s.match_prefix, "fi");
        assert_eq!(s.raw_dir, "$MYDIR/");
    }

    #[test]
    fn split_quoted_dir_keeps_verbatim_raw() {
        let tok = token_for("cat \"My Dir\"/fi");
        let s = split_token(&tok, "cat \"My Dir\"/fi", "/home", None, no_vars);
        assert_eq!(s.list_dir, PathBuf::from("/home/My Dir/"));
        assert_eq!(s.match_prefix, "fi");
        assert_eq!(s.raw_dir, "\"My Dir\"/");
    }

    /// Quoted/escaped `$VAR` spellings are shell-literal: the listing must
    /// target the literal path (never the expansion) — otherwise the
    /// accepted raw-spelling insert names a different file than the one
    /// shown.
    #[test]
    fn split_quoted_and_escaped_var_stay_literal() {
        let lookup = |name: &str| (name == "HOME").then(|| "/home/me".to_owned());

        let tok = token_for("cat '$HOME'/fi");
        let s = split_token(&tok, "cat '$HOME'/fi", "/work", None, lookup);
        assert_eq!(s.list_dir, PathBuf::from("/work/$HOME/"));
        assert_eq!(s.match_prefix, "fi");
        assert_eq!(s.raw_dir, "'$HOME'/");

        let tok = token_for("cat \\$HOME/fi");
        let s = split_token(&tok, "cat \\$HOME/fi", "/work", None, lookup);
        assert_eq!(s.list_dir, PathBuf::from("/work/$HOME/"));
        assert_eq!(s.raw_dir, "\\$HOME/");
    }

    /// Quoted `~` is shell-literal: no home listing — the same literal
    /// `cwd/~` degradation as the no-home case; a bare quoted `~` skips the
    /// home fast path and matches cwd entries literally.
    #[test]
    fn split_quoted_tilde_stays_literal() {
        let home = Some(Path::new("/home/me"));

        let tok = token_for("cat \"~/do");
        let s = split_token(&tok, "cat \"~/do", "/work", home, no_vars);
        assert_eq!(s.list_dir, PathBuf::from("/work/~"));
        assert_eq!(s.match_prefix, "do");
        assert_eq!(s.raw_dir, "\"~/");

        let tok = token_for("cat '~'");
        let s = split_token(&tok, "cat '~'", "/work", home, no_vars);
        assert_eq!(s.list_dir, PathBuf::from("/work"));
        assert_eq!(s.match_prefix, "~");
        assert_eq!(s.raw_dir, "");
    }

    fn all_plain(s: &str) -> Vec<bool> {
        vec![true; s.len()]
    }

    #[test]
    fn expand_vars_matrix() {
        let lookup = |name: &str| match name {
            "HOME" => Some("/home/me".to_owned()),
            "A_1" => Some("x".to_owned()),
            _ => None,
        };
        let ev = |s: &str| expand_vars(s, &all_plain(s), lookup);
        assert_eq!(ev("$HOME/docs"), "/home/me/docs");
        assert_eq!(ev("${HOME}/docs"), "/home/me/docs");
        assert_eq!(ev("$A_1/z"), "x/z");
        // Unset vars stay literal.
        assert_eq!(ev("$NOPE/docs"), "$NOPE/docs");
        // Lone `$`, digits, and malformed braces stay literal.
        assert_eq!(ev("a$"), "a$");
        assert_eq!(ev("$1/x"), "$1/x");
        assert_eq!(ev("${/x"), "${/x");
    }

    /// A `$` whose byte is not plain (quoted/escaped provenance) stays
    /// literal even when the variable is set.
    #[test]
    fn expand_vars_skips_non_plain_dollar() {
        let lookup = |name: &str| (name == "HOME").then(|| "/home/me".to_owned());
        let s = "$HOME/$HOME";
        let mut plain = all_plain(s);
        plain[0] = false;
        assert_eq!(expand_vars(s, &plain, lookup), "$HOME//home/me");
    }

    // --- file_command_boost ---

    #[test]
    fn boost_for_known_file_commands_only() {
        assert_eq!(file_command_boost(Some("cat")), FILE_CMD_BOOST);
        assert_eq!(file_command_boost(Some("/bin/cat")), FILE_CMD_BOOST);
        assert_eq!(file_command_boost(Some("git")), 0);
        assert_eq!(file_command_boost(None), 0);
    }

    /// `file_command_boost` binary-searches: a misplaced insert would
    /// silently drop boosts for everything after it.
    #[test]
    fn file_commands_is_sorted() {
        assert!(FILE_COMMANDS.is_sorted());
    }

    #[test]
    fn ci_starts_with_matches_lowercase_semantics() {
        assert!(ci_starts_with("Notes Archive", "no"));
        assert!(ci_starts_with("notes.md", "no"));
        assert!(!ci_starts_with("anode", "no"));
        assert!(!ci_starts_with("N", "no"));
        // Multibyte fold: uppercase e-acute lowers to the 2-byte e-acute.
        assert!(ci_starts_with("\u{c9}tude", "\u{e9}t"));
    }

    // --- list_ranked_entries ---

    #[tokio::test]
    async fn rank_exact_then_ci_prefix_then_fuzzy() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("note.md"), "").unwrap();
        std::fs::write(tmp.path().join("Nope.txt"), "").unwrap();
        std::fs::write(tmp.path().join("anode.txt"), "").unwrap();

        let names: Vec<String> = list_ranked_entries(tmp.path(), "no")
            .await
            .0
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, ["note.md", "Nope.txt", "anode.txt"]);
    }

    #[tokio::test]
    async fn rank_case_insensitive_prefix_matches() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(tmp.path().join("Notes Archive")).unwrap();

        let (entries, _) = list_ranked_entries(tmp.path(), "no").await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "Notes Archive");
        assert_eq!(entries[0].tier, 1);
    }

    #[tokio::test]
    async fn rank_fuzzy_subsequence_matches() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("notes.md"), "").unwrap();
        std::fs::write(tmp.path().join("zzz.bin"), "").unwrap();

        let (entries, _) = list_ranked_entries(tmp.path(), "nts").await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "notes.md");
        assert_eq!(entries[0].tier, 2);
    }

    #[tokio::test]
    async fn rank_no_match_is_empty() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("alpha"), "").unwrap();
        assert!(list_ranked_entries(tmp.path(), "qqq").await.0.is_empty());
    }

    /// Directories survive the result cap even when the scan yields them
    /// after `MAX_RESULTS` files (ranking happens before truncation).
    #[tokio::test]
    async fn rank_dirs_first_before_cap() {
        let tmp = tempfile::TempDir::new().unwrap();
        for i in 0..60 {
            std::fs::write(tmp.path().join(format!("file_{i:03}")), "").unwrap();
        }
        // `z`-names sort after every file alphabetically.
        std::fs::create_dir(tmp.path().join("zdir_a")).unwrap();
        std::fs::create_dir(tmp.path().join("zdir_b")).unwrap();

        let (entries, truncated) = list_ranked_entries(tmp.path(), "").await;
        assert_eq!(entries.len(), MAX_RESULTS);
        assert!(truncated, "result-capped scans are not exhaustive");
        assert!(entries[0].is_dir && entries[1].is_dir);
        assert_eq!(entries[0].name, "zdir_a");
        assert_eq!(entries[1].name, "zdir_b");
        assert!(entries[2..].iter().all(|e| !e.is_dir));
    }

    #[tokio::test]
    async fn rank_deterministic_name_order_within_tier() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("zebra"), "").unwrap();
        std::fs::write(tmp.path().join("alpha"), "").unwrap();
        std::fs::write(tmp.path().join("mango"), "").unwrap();

        let names: Vec<String> = list_ranked_entries(tmp.path(), "")
            .await
            .0
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, ["alpha", "mango", "zebra"]);
    }

    #[tokio::test]
    async fn rank_skips_hidden_without_dot_prefix() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join(".hidden"), "").unwrap();
        std::fs::write(tmp.path().join("visible"), "").unwrap();

        let (entries, truncated) = list_ranked_entries(tmp.path(), "").await;
        assert_eq!(entries.len(), 1);
        assert!(!truncated, "an exhaustively scanned dir is not truncated");
        assert_eq!(entries[0].name, "visible");
    }

    #[tokio::test]
    async fn rank_shows_hidden_with_dot_prefix() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join(".config"), "").unwrap();
        std::fs::write(tmp.path().join(".hidden"), "").unwrap();
        std::fs::write(tmp.path().join("visible"), "").unwrap();

        let (entries, _) = list_ranked_entries(tmp.path(), ".").await;
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|e| e.name.starts_with('.')));
    }

    #[tokio::test]
    async fn rank_nonexistent_and_empty_dirs() {
        assert!(
            list_ranked_entries(Path::new("/no/such/dir"), "")
                .await
                .0
                .is_empty()
        );
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(list_ranked_entries(tmp.path(), "").await.0.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rank_follows_symlinked_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let real_dir = tmp.path().join("real");
        std::fs::create_dir(&real_dir).unwrap();
        std::os::unix::fs::symlink(&real_dir, tmp.path().join("link")).unwrap();

        let (entries, _) = list_ranked_entries(tmp.path(), "li").await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "link");
        assert!(entries[0].is_dir);
    }

    /// A self-referential symlink makes the follow-up `metadata()` fail
    /// (ELOOP) — the entry still lists, as a non-directory.
    #[cfg(unix)]
    #[tokio::test]
    async fn rank_self_symlink_falls_back_to_file_type() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::os::unix::fs::symlink("selfie", tmp.path().join("selfie")).unwrap();

        let (entries, _) = list_ranked_entries(tmp.path(), "se").await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "selfie");
        assert!(!entries[0].is_dir);
    }

    /// A scan that hits [`SCAN_CAP`] reports truncation: entries may remain
    /// unread, so the returned rows must not be treated as exhaustive.
    #[tokio::test]
    async fn rank_scan_cap_marks_truncated() {
        let tmp = tempfile::TempDir::new().unwrap();
        for i in 0..=SCAN_CAP {
            std::fs::write(tmp.path().join(format!("f_{i:04}")), "").unwrap();
        }

        let (entries, truncated) = list_ranked_entries(tmp.path(), "").await;
        assert!(truncated);
        assert_eq!(entries.len(), MAX_RESULTS);
    }

    // --- end-to-end via suggest() ---

    fn ctx(text: &str, cwd: &Path) -> SuggestContext {
        SuggestContext::new(
            text.to_owned(),
            text.len(),
            cwd.to_string_lossy().into_owned(),
        )
    }

    #[tokio::test]
    async fn suggest_end_to_end_file_cmd() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("hello.txt"), "").unwrap();
        std::fs::create_dir(tmp.path().join("subdir")).unwrap();

        let results = FilePathProvider.suggest(&ctx("cat hel", tmp.path())).await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].display, "hello.txt");
        assert_eq!(results[0].insert_text, "cat hello.txt");
        assert_eq!(results[0].token_text.as_deref(), Some("hello.txt"));
        assert_eq!(results[0].replace_range, Some((4, 7)));
        assert_eq!(results[0].source, SuggestionSource::File);
        assert!(!results[0].is_ghost_candidate);
        assert!(!results[0].truncated);
        assert_eq!(results[0].priority, FILE_CMD_BOOST);
    }

    /// A capped scan stamps `truncated` on every row — the pager must not
    /// insta-accept a "sole" match a hidden entry could disprove.
    #[tokio::test]
    async fn suggest_capped_scan_stamps_truncated_rows() {
        let tmp = tempfile::TempDir::new().unwrap();
        for i in 0..=MAX_RESULTS {
            std::fs::write(tmp.path().join(format!("note_{i:03}")), "").unwrap();
        }

        let results = FilePathProvider.suggest(&ctx("cat no", tmp.path())).await;
        assert_eq!(results.len(), MAX_RESULTS);
        assert!(results.iter().all(|s| s.truncated));
    }

    /// THE closed-quote round trip (cursor right after the closer): the
    /// completed insert keeps the closing quote instead of dropping it.
    #[tokio::test]
    async fn suggest_quote_closed_at_cursor_keeps_closer() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("My Dir");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("file.txt"), "").unwrap();

        let text = "cat \"My Dir/fi\"";
        let results = FilePathProvider.suggest(&ctx(text, tmp.path())).await;
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].token_text.as_deref(),
            Some("\"My Dir/file.txt\"")
        );
        assert_eq!(results[0].insert_text, "cat \"My Dir/file.txt\"");
        assert_eq!(results[0].replace_range, Some((4, text.len())));
    }

    /// Unknown commands complete their args too — at priority 0, no boost.
    #[tokio::test]
    async fn suggest_end_to_end_any_command() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("notes.md"), "").unwrap();

        let results = FilePathProvider
            .suggest(&ctx("foobar no", tmp.path()))
            .await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].insert_text, "foobar notes.md");
        assert_eq!(results[0].priority, 0);
    }

    #[tokio::test]
    async fn suggest_flag_token_returns_nothing() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("-weird"), "").unwrap();
        assert!(
            FilePathProvider
                .suggest(&ctx("git -w", tmp.path()))
                .await
                .is_empty()
        );
    }

    /// Arg-token range with text after the cursor: the range ends at the
    /// cursor so the tail (`| wc -l`) is out of the replaced span, and the
    /// compat whole-line `insert_text` keeps that tail too.
    #[tokio::test]
    async fn suggest_arg_range_ends_at_cursor_mid_text() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("hello.txt"), "").unwrap();

        let text = "cat hel | wc -l";
        let ctx = SuggestContext::new(text.into(), 7, tmp.path().to_string_lossy().into_owned());
        let results = FilePathProvider.suggest(&ctx).await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].insert_text, "cat hello.txt | wc -l");
        assert_eq!(results[0].token_text.as_deref(), Some("hello.txt"));
        assert_eq!(results[0].replace_range, Some((4, 7)));
    }

    /// THE quoting round-trip: `cat "My Fi` completes `My File.txt` with the
    /// token extent covering the opening quote and the insert closing it.
    #[tokio::test]
    async fn suggest_open_quote_completes_spaced_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("My File.txt"), "").unwrap();

        let text = "cat \"My Fi";
        let results = FilePathProvider.suggest(&ctx(text, tmp.path())).await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].token_text.as_deref(), Some("\"My File.txt\""));
        assert_eq!(results[0].replace_range, Some((4, text.len())));
        assert_eq!(results[0].insert_text, "cat \"My File.txt\"");
    }

    /// Unquoted completion of a spaced name backslash-escapes it; the next
    /// request tokenizes that insert back to the same directory (round-trip).
    #[tokio::test]
    async fn suggest_unquoted_spaced_dir_escapes_and_drills_down() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("Notes Archive");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("inner.txt"), "").unwrap();

        let results = FilePathProvider.suggest(&ctx("cat No", tmp.path())).await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].token_text.as_deref(), Some("Notes\\ Archive/"));
        assert_eq!(results[0].insert_text, "cat Notes\\ Archive/");
        assert_eq!(results[0].description, "directory");

        // Drill-down: the accepted text re-completes inside the directory.
        let text = "cat Notes\\ Archive/";
        let results = FilePathProvider.suggest(&ctx(text, tmp.path())).await;
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].token_text.as_deref(),
            Some("Notes\\ Archive/inner.txt")
        );
        assert_eq!(results[0].replace_range, Some((4, text.len())));
        assert_eq!(results[0].insert_text, "cat Notes\\ Archive/inner.txt");
    }

    /// Same drill-down through an open double quote: the dir insert keeps
    /// the quote open; the file completion inside closes it.
    #[tokio::test]
    async fn suggest_quoted_dir_drill_down_closes_quote_on_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("Notes Archive");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("inner.txt"), "").unwrap();

        let results = FilePathProvider.suggest(&ctx("cat \"No", tmp.path())).await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].token_text.as_deref(), Some("\"Notes Archive/"));

        let text = "cat \"Notes Archive/";
        let results = FilePathProvider.suggest(&ctx(text, tmp.path())).await;
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].token_text.as_deref(),
            Some("\"Notes Archive/inner.txt\"")
        );
        assert_eq!(results[0].replace_range, Some((4, text.len())));
    }

    #[tokio::test]
    async fn suggest_nested_dir_range_and_insert() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sub = tmp.path().join("src");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("main.rs"), "").unwrap();

        let results = FilePathProvider
            .suggest(&ctx("cat src/m", tmp.path()))
            .await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].token_text.as_deref(), Some("src/main.rs"));
        assert_eq!(results[0].replace_range, Some((4, 9)));
    }

    #[tokio::test]
    async fn suggest_end_to_end_path_like_first_token() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sub = tmp.path().join("src");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("main.rs"), "").unwrap();

        let text = format!("{}/src/m", tmp.path().display());
        let cursor = text.len();
        let ctx = SuggestContext::new(text, cursor, "/ignored".into());
        let results = FilePathProvider.suggest(&ctx).await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].display, "main.rs");
        assert_eq!(results[0].replace_range, Some((0, cursor)));
        assert!(
            results[0]
                .token_text
                .as_deref()
                .unwrap()
                .ends_with("main.rs")
        );
    }

    #[tokio::test]
    async fn suggest_exact_prefix_ranks_before_case_insensitive() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("notes.md"), "").unwrap();
        std::fs::create_dir(tmp.path().join("Notes Archive")).unwrap();

        let results = FilePathProvider.suggest(&ctx("cat no", tmp.path())).await;
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].display, "notes.md");
        assert_eq!(results[1].display, "Notes Archive/");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn suggest_var_prefix_lists_expansion_inserts_raw() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("notes.md"), "").unwrap();

        let _env =
            echo_agent_test_support::EnvGuard::set("ECHO_AGENT_SUGGEST_TEST_DIR", tmp.path());
        let ctx = SuggestContext::new(
            "cat $ECHO_AGENT_SUGGEST_TEST_DIR/no".into(),
            "cat $ECHO_AGENT_SUGGEST_TEST_DIR/no".len(),
            "/ignored".into(),
        );
        let results = FilePathProvider.suggest(&ctx).await;
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].token_text.as_deref(),
            Some("$ECHO_AGENT_SUGGEST_TEST_DIR/notes.md")
        );
    }

    /// THE provenance case: a quoted `$VAR` is literal to the shell, so the
    /// listing targets a directory literally named `$HOME` — the accepted
    /// candidate names exactly the file shown (no env expansion, whatever
    /// the real `$HOME` is).
    #[tokio::test]
    async fn suggest_quoted_var_lists_literal_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let lit = tmp.path().join("$HOME");
        std::fs::create_dir(&lit).unwrap();
        std::fs::write(lit.join("file.txt"), "").unwrap();

        let results = FilePathProvider
            .suggest(&ctx("cat '$HOME'/fi", tmp.path()))
            .await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].token_text.as_deref(), Some("'$HOME'/file.txt"));
        assert_eq!(results[0].insert_text, "cat '$HOME'/file.txt");
    }

    /// A dash-leading candidate inserts `./`-anchored, so single-candidate
    /// insta-accept can never silently write a flag (`rm ` + Tab must not
    /// become `rm -rf`).
    #[tokio::test]
    async fn suggest_dash_leading_candidate_anchored_as_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("-rf"), "").unwrap();

        let results = FilePathProvider.suggest(&ctx("rm ", tmp.path())).await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].display, "-rf");
        assert_eq!(results[0].token_text.as_deref(), Some("./-rf"));
        assert_eq!(results[0].insert_text, "rm ./-rf");
    }

    #[tokio::test]
    async fn suggest_first_token_plain_word_no_results() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("hello"), "").unwrap();
        assert!(
            FilePathProvider
                .suggest(&ctx("hel", tmp.path()))
                .await
                .is_empty()
        );
    }
}
