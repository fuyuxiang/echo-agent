//! Bash request-level execution risk: argv flags that spawn programs, and ambient
//! local/worktree git config. Flag floors run inline; ambient git2 uses
//! `spawn_blocking` from the permission actor.

use std::path::{Path, PathBuf};

use crate::permission::bash_command_splitting::{
    MAX_TRANSPARENT_PREFIX_DEPTH, MAX_WRAPPER_DEPTH, TransparentPrefixPeel,
    peel_transparent_prefixes, unwrap_wrappers_checked,
};

#[cfg(test)]
use crate::permission::bash_command_splitting::{
    try_parse_shell, try_parse_word_only_commands_sequence,
};

/// Shared peel budget for nested `command env …` chains; remaining peelable layers fail closed.
const MAX_NORMALIZE_ROUNDS: usize = MAX_WRAPPER_DEPTH + MAX_TRANSPARENT_PREFIX_DEPTH;

enum NormalizedArgv<'a> {
    Ready(&'a [String]),
    FailClosed,
}

/// Alternate canonical wrappers and transparent prefixes until fixed point.
fn normalize_for_exec_risk(words: &[String]) -> NormalizedArgv<'_> {
    let mut current = words;
    for _ in 0..MAX_NORMALIZE_ROUNDS {
        let before = current;
        let checked = unwrap_wrappers_checked(current);
        if checked.exhausted || checked.has_split_string || checked.has_chdir {
            return NormalizedArgv::FailClosed;
        }
        let after_wrap = checked.words;
        let after_trans = match peel_transparent_prefixes(after_wrap) {
            TransparentPrefixPeel::Ambiguous => return NormalizedArgv::FailClosed,
            TransparentPrefixPeel::Ready(inner) => inner,
        };
        if std::ptr::eq(after_trans.as_ptr(), before.as_ptr()) && after_trans.len() == before.len()
        {
            return NormalizedArgv::Ready(after_trans);
        }
        if std::ptr::eq(after_trans.as_ptr(), after_wrap.as_ptr())
            && after_trans.len() == after_wrap.len()
        {
            return NormalizedArgv::Ready(after_trans);
        }
        current = after_trans;
    }
    NormalizedArgv::FailClosed
}

/// `min_len` is the shortest unique stem vs sibling options (e.g. sort `--co` vs `--check`).
fn is_accepted_long_option_prefix(flag: &str, full: &str, min_len: usize) -> bool {
    flag.starts_with("--")
        && flag.len() >= min_len
        && full.starts_with(flag)
        && flag.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn normalized_program_name(words: &[String]) -> Option<String> {
    let raw = words.first()?;
    let base = raw.rsplit(['/', '\\']).next().unwrap_or(raw.as_str());
    if base.is_empty() {
        return None;
    }
    let mut name = base.to_ascii_lowercase();
    if let Some(stem) = name.strip_suffix(".exe") {
        name = stem.to_owned();
    }
    Some(name)
}

fn is_git_program(words: &[String]) -> bool {
    normalized_program_name(words).as_deref() == Some("git")
}

fn is_sort_program(words: &[String]) -> bool {
    normalized_program_name(words).as_deref() == Some("sort")
}

fn normalized_token_basename(token: &str) -> String {
    let base = token.rsplit(['/', '\\']).next().unwrap_or(token);
    let mut name = base.to_ascii_lowercase();
    if let Some(stem) = name.strip_suffix(".exe") {
        name = stem.to_owned();
    }
    name
}

/// GNU `sort --compress-program`; stops at `--`. Min stem `--co` vs `--check`.
fn sort_has_compress_program_flag(words: &[String]) -> bool {
    for w in words.iter().skip(1) {
        if w == "--" {
            break;
        }
        if w == "--compress-program" || w.starts_with("--compress-program=") {
            return true;
        }
        let flag = w.split_once('=').map(|(f, _)| f).unwrap_or(w.as_str());
        if is_accepted_long_option_prefix(flag, "--compress-program", 4) {
            return true;
        }
    }
    false
}

fn is_git_config_env_flag(tok: &str) -> bool {
    if tok == "--config-env" || tok.starts_with("--config-env=") {
        return true;
    }
    let flag = tok.split_once('=').map(|(f, _)| f).unwrap_or(tok);
    // Sole git global `--config*`; min stem `--co` (len 4).
    is_accepted_long_option_prefix(flag, "--config-env", 4)
}

/// Presence fails closed — these retarget which config git reads.
fn is_git_repo_retarget_flag(tok: &str) -> bool {
    if tok == "--git-dir"
        || tok.starts_with("--git-dir=")
        || tok == "--work-tree"
        || tok.starts_with("--work-tree=")
    {
        return true;
    }
    let flag = tok.split_once('=').map(|(f, _)| f).unwrap_or(tok);
    // git.c globals: `--gi` unique vs `--glob-pathspecs`; `--wor` sole `--wor*`.
    is_accepted_long_option_prefix(flag, "--git-dir", 4)
        || is_accepted_long_option_prefix(flag, "--work-tree", 4)
}

fn is_attached_git_config_c(tok: &str) -> bool {
    tok.starts_with("-c") && tok.len() > 2 && !tok.starts_with("--")
}

fn attached_git_c_path(tok: &str) -> Option<&str> {
    tok.strip_prefix("-C")
        .filter(|rest| !rest.is_empty() && !tok.starts_with("--"))
}

fn git_global_option_takes_value(tok: &str) -> bool {
    matches!(
        tok,
        "-C" | "-c"
            | "--git-dir"
            | "--work-tree"
            | "--namespace"
            | "--super-prefix"
            | "--exec-path"
            | "--list-cmds"
            | "--attr-source"
            | "--config-env"
    ) || is_accepted_long_option_prefix(tok, "--config-env", 4)
        || is_accepted_long_option_prefix(tok, "--git-dir", 4)
        || is_accepted_long_option_prefix(tok, "--work-tree", 4)
        || is_accepted_long_option_prefix(tok, "--namespace", 7)
        || is_accepted_long_option_prefix(tok, "--super-prefix", 8)
        || is_accepted_long_option_prefix(tok, "--exec-path", 7)
        || is_accepted_long_option_prefix(tok, "--list-cmds", 7)
        || is_accepted_long_option_prefix(tok, "--attr-source", 8)
}

/// Pre-subcommand only; missing values fail closed. Post-subcommand `git log -c` is not scanned.
pub(crate) fn git_has_exec_risk_global(words: &[String]) -> bool {
    let mut i = 1;
    while i < words.len() {
        let tok = words[i].as_str();
        if tok == "--" {
            return false;
        }
        if !tok.starts_with('-') || tok == "-" {
            return false;
        }
        if tok == "-c"
            || is_attached_git_config_c(tok)
            || is_git_config_env_flag(tok)
            || is_git_repo_retarget_flag(tok)
        {
            return true;
        }
        // `-Cpath` is cwd-only (ambient); skip so it is not treated as the subcommand.
        if attached_git_c_path(tok).is_some() {
            i += 1;
            continue;
        }
        if !tok.contains('=')
            && git_global_option_takes_value(tok)
            && words
                .get(i + 1)
                .is_some_and(|n| !n.starts_with('-') || n == "-")
        {
            i += 1;
        }
        i += 1;
    }
    false
}

pub(crate) fn segment_has_exec_risk_flag(words: &[String]) -> bool {
    if is_sort_program(words) {
        return sort_has_compress_program_flag(words);
    }
    if is_git_program(words) {
        return git_has_exec_risk_global(words);
    }
    false
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SegmentExecFacts {
    pub exec_risk: bool,
    pub has_git: bool,
}

/// Normalize raw segment words, then inspect git/sort. Unmodeled peels fail closed.
pub(crate) fn segment_exec_facts(words: &[String]) -> SegmentExecFacts {
    match normalize_for_exec_risk(words) {
        NormalizedArgv::FailClosed => SegmentExecFacts {
            exec_risk: true,
            has_git: false,
        },
        NormalizedArgv::Ready(inner) => SegmentExecFacts {
            exec_risk: segment_has_exec_risk_flag(inner),
            has_git: is_git_program(inner),
        },
    }
}

/// Read-only git query verbs. SINGLE SOURCE for every git allow decision:
/// [`git_words_are_read_only_query`] (the manager safe lists and the auto-mode
/// routine heuristic both call it) and the `alias.<verb> = !cmd` shadowing
/// check in the ambient config scan below. Add a new read-only verb here and
/// every consumer inherits it — do not grow per-consumer prefix lists.
pub(crate) const SAFE_GIT_SUBCOMMANDS: &[&str] = &[
    "status",
    "branch",
    "log",
    "diff",
    "ls-files",
    "show",
    "rev-parse",
    "blame",
    "grep",
    "describe",
    "merge-base",
    "check-ignore",
    "check-attr",
    "cat-file",
    "ls-tree",
    "show-ref",
    "for-each-ref",
    "rev-list",
    "name-rev",
    "count-objects",
    "shortlog",
];

/// Options that make an otherwise read-only git verb run repo-configured
/// content drivers or write arbitrary paths — one table applied to EVERY
/// [`SAFE_GIT_SUBCOMMANDS`] verb, so a new safe verb inherits the policy:
/// `--filters`/`--textconv` run `filter.*.smudge` / `diff.*.textconv`
/// (`filter.*.smudge` is outside the ambient local-config exec scan),
/// `--ext-diff` runs the external diff driver, `--output` writes an arbitrary
/// file, and `--open-files-in-pager` executes a pager command (`git grep`'s
/// short-attached `-O<cmd>` form is guarded in
/// [`git_words_have_unsafe_query_option`]).
const GIT_QUERY_UNSAFE_OPTIONS: &[&str] = &[
    "--filters",
    "--textconv",
    "--output",
    "--ext-diff",
    "--open-files-in-pager",
];

/// Git accepts uniquely-abbreviated long options, so any `--` word (pre-`=`,
/// ≥3 chars) that prefixes a table entry fails closed — including
/// abbreviations a specific verb would resolve to a benign sibling
/// (`git grep --text` collides with `--textconv` and prompts).
fn git_query_option_is_unsafe(word: &str) -> bool {
    let flag = word.split('=').next().unwrap_or(word);
    flag.len() > 2
        && GIT_QUERY_UNSAFE_OPTIONS
            .iter()
            .any(|full| full.starts_with(flag))
}

/// Resolve the subcommand index, skipping only modeled-benign globals:
/// `-C <path>` / `-C<path>` (the ambient config scan tracks the retargeted
/// cwd) and `--no-pager` / `-P`. Every other pre-subcommand option fails
/// closed (`None`) — `-c`, `--config-env`, `--git-dir`, `--work-tree`,
/// `--exec-path`, `--paginate`, `--attr-source`, … can change what executes
/// or which config a query reads.
fn git_safe_query_verb_index(words: &[String]) -> Option<usize> {
    let mut i = 1;
    loop {
        let tok = words.get(i).map(String::as_str)?;
        if tok == "-" || tok == "--" {
            return None;
        }
        if !tok.starts_with('-') {
            return Some(i);
        }
        if tok == "-C" {
            i += 2;
            continue;
        }
        if attached_git_c_path(tok).is_some() || tok == "--no-pager" || tok == "-P" {
            i += 1;
            continue;
        }
        return None;
    }
}

/// True when a `git` invocation carries an option from the shared unsafe
/// table ([`GIT_QUERY_UNSAFE_OPTIONS`], plus `git grep`'s short-attached
/// `-O<cmd>`), whatever the verb. Used on its own to keep a session
/// whitelist-prefix grant from riding over a driver/write flag, and by
/// [`git_words_are_read_only_query`].
pub(crate) fn git_words_have_unsafe_query_option(words: &[String]) -> bool {
    if words.first().map(String::as_str) != Some("git") {
        return false;
    }
    if words.iter().skip(1).any(|w| git_query_option_is_unsafe(w)) {
        return true;
    }
    // `git grep -O<cmd>` / `-O <cmd>` executes <cmd>; the short-attached form
    // is not a long-option abbreviation, so guard it verb-specifically.
    matches!(git_safe_query_verb_index(words), Some(i) if words[i] == "grep")
        && words.iter().skip(1).any(|w| w.starts_with("-O"))
}

/// Single decision point for auto-approvable read-only `git` queries, shared
/// by the manager safe lists and the auto-mode routine heuristic so verb
/// policy and flag policy live in one place:
/// 1. resolve the subcommand via [`git_safe_query_verb_index`] (benign
///    globals skipped, config/retarget/unknown globals fail closed — plus a
///    redundant [`git_has_exec_risk_global`] belt for odd `-C` value shapes);
/// 2. allow only [`SAFE_GIT_SUBCOMMANDS`] verbs;
/// 3. reject the shared unsafe-option table
///    ([`git_words_have_unsafe_query_option`]).
///
/// Callers pass wrapper-peeled words; `words[0]` must be literally `git`
/// (path-qualified or case-variant "git" binaries fail closed — a different
/// binary of the same basename must not ride the allowlist).
pub(crate) fn git_words_are_read_only_query(words: &[String]) -> bool {
    if words.first().map(String::as_str) != Some("git") {
        return false;
    }
    if git_has_exec_risk_global(words) {
        return false;
    }
    let Some(verb_idx) = git_safe_query_verb_index(words) else {
        return false;
    };
    if !SAFE_GIT_SUBCOMMANDS.contains(&words[verb_idx].as_str()) {
        return false;
    }
    !git_words_have_unsafe_query_option(words)
}

fn local_git_config_entry_is_exec(name: &str, value: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let value = value.trim();
    if value.is_empty() {
        return false;
    }
    if name == "core.fsmonitor" {
        return git2::Config::parse_bool(value).is_err();
    }
    if name == "diff.external" {
        return true;
    }
    if let Some(rest) = name.strip_prefix("diff.")
        && (rest.ends_with(".command")
            || rest.ends_with(".textconv")
            || rest.ends_with(".external"))
    {
        return true;
    }
    if let Some(alias) = name.strip_prefix("alias.")
        && SAFE_GIT_SUBCOMMANDS.contains(&alias)
        && value.starts_with('!')
    {
        return true;
    }
    false
}

fn path_unreadable(path: &Path) -> bool {
    // Directories open on Linux, so require a readable regular file after following symlinks.
    match std::fs::File::open(path) {
        Ok(f) => match f.metadata() {
            Ok(meta) => !meta.is_file(),
            Err(_) => true,
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => true,
    }
}

/// Local/worktree only via libgit2 (include/includeIf). Fail closed on read errors.
pub(crate) fn local_repo_config_has_exec_risk(cwd: &Path) -> bool {
    let repo = match git2::Repository::discover(cwd) {
        Ok(repo) => repo,
        Err(e)
            if e.code() == git2::ErrorCode::NotFound
                && e.class() == git2::ErrorClass::Repository =>
        {
            return false;
        }
        Err(_) => return true,
    };
    // `repo.config()` can still open global levels when local is unreadable.
    let git_dir = repo.path();
    let common = repo.commondir();
    if path_unreadable(&common.join("config"))
        || path_unreadable(&git_dir.join("config"))
        || path_unreadable(&git_dir.join("config.worktree"))
    {
        return true;
    }
    let config = match repo.config() {
        Ok(c) => c,
        Err(_) => return true,
    };
    let mut entries = match config.entries(None) {
        Ok(e) => e,
        Err(_) => return true,
    };
    while let Some(entry) = entries.next() {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => return true,
        };
        match entry.level() {
            git2::ConfigLevel::Local | git2::ConfigLevel::Worktree => {}
            _ => continue,
        }
        let name = entry.name().unwrap_or("");
        let value = entry.value().unwrap_or("");
        if local_git_config_entry_is_exec(name, value) {
            return true;
        }
    }
    false
}

fn is_static_path_operand(p: &str) -> bool {
    !p.is_empty()
        && p != "-"
        && !p.starts_with('-')
        && !p.as_bytes().contains(&b'$')
        && !p.as_bytes().contains(&b'`')
        && !p.contains("$(")
}

fn join_cwd(base: &Path, operand: &str) -> PathBuf {
    let p = Path::new(operand);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    }
}

fn apply_literal_chdir(cwd: &Path, words: &[String]) -> Option<PathBuf> {
    let mut args = words.iter().skip(1).map(String::as_str);
    let mut target = None;
    while let Some(tok) = args.next() {
        if tok == "--" {
            target = args.next();
            break;
        }
        if tok.starts_with('-') {
            return None;
        }
        if target.is_some() {
            return None;
        }
        target = Some(tok);
    }
    let target = target?;
    if !is_static_path_operand(target) {
        return None;
    }
    Some(join_cwd(cwd, target))
}

/// Pre-subcommand `git -C` / `-Cpath` chains. `None` if path unmodeled or retarget global.
fn git_effective_cwd(words: &[String], start_cwd: &Path) -> Option<PathBuf> {
    let mut cwd = start_cwd.to_path_buf();
    let mut i = 1;
    while i < words.len() {
        let tok = words[i].as_str();
        if tok == "--" || !tok.starts_with('-') || tok == "-" {
            break;
        }
        if is_git_repo_retarget_flag(tok) {
            return None;
        }
        if tok == "-C" {
            let path = words.get(i + 1).map(String::as_str)?;
            if !is_static_path_operand(path) {
                return None;
            }
            cwd = join_cwd(&cwd, path);
            i += 2;
            continue;
        }
        if let Some(path) = attached_git_c_path(tok) {
            if !is_static_path_operand(path) {
                return None;
            }
            cwd = join_cwd(&cwd, path);
            i += 1;
            continue;
        }
        if !tok.contains('=')
            && git_global_option_takes_value(tok)
            && words
                .get(i + 1)
                .is_some_and(|n| !n.starts_with('-') || n == "-")
        {
            i += 1;
        }
        i += 1;
    }
    Some(cwd)
}

#[derive(Debug, Clone)]
pub(crate) enum AmbientScanPlan {
    FailClosed,
    CheckDirs(Vec<PathBuf>),
}

/// Same normalization as [`segment_exec_facts`], then track cd/git cwd.
pub(crate) fn ambient_scan_plan_from_segments(
    raw_segments: &[Vec<String>],
    session_cwd: &Path,
) -> AmbientScanPlan {
    let mut cwd = session_cwd.to_path_buf();
    let mut git_cwds = Vec::new();
    for raw in raw_segments {
        let words = match normalize_for_exec_risk(raw) {
            NormalizedArgv::FailClosed => return AmbientScanPlan::FailClosed,
            NormalizedArgv::Ready(inner) => inner,
        };
        match normalized_program_name(words).as_deref() {
            Some("cd") | Some("pushd") => match apply_literal_chdir(&cwd, words) {
                Some(next) => cwd = next,
                None => return AmbientScanPlan::FailClosed,
            },
            Some("popd") => return AmbientScanPlan::FailClosed,
            Some("git") => match git_effective_cwd(words, &cwd) {
                Some(effective) => git_cwds.push(effective),
                None => return AmbientScanPlan::FailClosed,
            },
            _ => {}
        }
    }
    if git_cwds.is_empty() {
        AmbientScanPlan::FailClosed
    } else {
        AmbientScanPlan::CheckDirs(git_cwds)
    }
}

pub(crate) fn ambient_exec_risk_from_plan(plan: &AmbientScanPlan) -> bool {
    match plan {
        AmbientScanPlan::FailClosed => true,
        AmbientScanPlan::CheckDirs(dirs) => dirs.iter().any(|c| local_repo_config_has_exec_risk(c)),
    }
}

#[cfg(test)]
pub(crate) fn ambient_scan_plan_from_cmd(cmd: &str, session_cwd: &Path) -> Option<AmbientScanPlan> {
    let tree = try_parse_shell(cmd)?;
    let segments = try_parse_word_only_commands_sequence(&tree, cmd)?;
    let raw: Vec<Vec<String>> = segments.iter().map(|s| s.words().to_vec()).collect();
    Some(ambient_scan_plan_from_segments(&raw, session_cwd))
}

/// Token probe for unparseable scripts. Bare `git` tokens fail closed (e.g. `echo git $(true)`).
pub(crate) fn script_may_invoke_git(cmd: &str) -> bool {
    for token in cmd.split(|c: char| {
        c.is_whitespace() || matches!(c, '|' | '&' | ';' | '(' | ')' | '`' | '\n' | '<' | '>')
    }) {
        let trimmed = token.trim_matches(|c| matches!(c, '\'' | '"' | '`'));
        if trimmed.is_empty() {
            continue;
        }
        if normalized_token_basename(trimmed) == "git" {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(cmd: &str) -> Vec<String> {
        cmd.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn sort_compress_program_flags() {
        for cmd in [
            "sort --compress-program=tools/x in",
            "sort --compress-program tools/x in",
            "sort --compress-prog=tools/x in",
            "sort --co=tools/x in",
            "/usr/bin/sort --compress-program=/tmp/pwn in",
            "SORT.EXE --compress-program=/tmp/pwn in",
        ] {
            assert!(segment_has_exec_risk_flag(&words(cmd)), "{cmd}");
        }
        for cmd in [
            "sort in.csv",
            "sort --check big.csv",
            "sort -- --compress-program=foo",
        ] {
            assert!(!segment_has_exec_risk_flag(&words(cmd)), "{cmd}");
        }
    }

    #[test]
    fn git_exec_risk_globals() {
        for cmd in [
            "git -c core.fsmonitor=/tmp/pwn status",
            "git -ccore.fsmonitor=/tmp/pwn status",
            "git --config-env=core.fsmonitor=EVIL status",
            "git --config-env core.fsmonitor=EVIL status",
            "git --config-e=core.fsmonitor=EVIL status",
            "git -c status",
            "git -C /tmp -c core.fsmonitor=/tmp/pwn status",
            "git --git-dir=/evil/.git status",
            "git --git-dir /evil/.git status",
            "git --work-tree=/evil status",
            "git --work-tree /evil status",
            "git --gi=/evil/.git status",
            "git --wor=/evil status",
            "/usr/bin/git -c core.fsmonitor=/tmp/pwn status",
            "Git -c core.fsmonitor=/tmp/pwn status",
            r"C:\Git\cmd\git.exe -c core.fsmonitor=/tmp/pwn status",
        ] {
            assert!(segment_has_exec_risk_flag(&words(cmd)), "{cmd}");
        }
        for cmd in [
            "git log -c",
            "git status",
            "git -C /tmp status",
            "git -C/tmp status",
        ] {
            assert!(!segment_has_exec_risk_flag(&words(cmd)), "{cmd}");
        }
    }

    #[test]
    fn read_only_git_queries() {
        // Safe verbs, benign globals, ordinary flags.
        for cmd in [
            "git status",
            "git -C sub status",
            "git -C/abs/path log --oneline --graph",
            "git --no-pager diff --stat",
            "git -P show HEAD",
            "git cat-file -p HEAD:src/main.rs",
            "git grep --only-matching pattern",
            "git grep --or -e a -e b",
            "git rev-parse --show-toplevel",
            "git shortlog -sn",
        ] {
            assert!(git_words_are_read_only_query(&words(cmd)), "{cmd}");
        }
        // Non-query verbs, unmodeled/exec globals, non-bare git.
        for cmd in [
            "git push --force",
            "git checkout main",
            "git",
            "git -C sub",
            "git -c core.fsmonitor=/x status",
            "git --git-dir=/evil/.git status",
            "git --exec-path=/evil status",
            "git -p status",
            "git --paginate status",
            "git --attr-source=evil check-attr -a f",
            "git -- status",
            "/usr/bin/git status",
            "Git status",
            "rm -rf /",
        ] {
            assert!(!git_words_are_read_only_query(&words(cmd)), "{cmd}");
        }
    }

    #[test]
    fn unsafe_query_options_apply_to_every_verb() {
        // Driver / write flags block on any verb, abbreviations fail closed.
        for cmd in [
            "git cat-file --filters HEAD:data.bin",
            "git cat-file --textconv HEAD:data.bin",
            "git cat-file --filt HEAD:data.bin",
            "git show --textconv HEAD:data.bin",
            "git log --textconv -p",
            "git log --ext-diff",
            "git show --output=/tmp/out HEAD",
            "git log --output /tmp/out",
            "git grep -Ovim TODO",
            "git grep -O touch-evil TODO",
            "git grep --open-files-in-pager=sh TODO",
            "git grep --op=sh TODO",
        ] {
            assert!(git_words_have_unsafe_query_option(&words(cmd)), "{cmd}");
            assert!(!git_words_are_read_only_query(&words(cmd)), "{cmd}");
        }
        for cmd in [
            "git cat-file -p HEAD:src/main.rs",
            "git log --oneline",
            "git log --only-matching",
            "git grep --or -e a -e b",
            "git show --stat HEAD",
        ] {
            assert!(!git_words_have_unsafe_query_option(&words(cmd)), "{cmd}");
        }
        // `-O` outside `git grep` is not the pager flag.
        assert!(!git_words_have_unsafe_query_option(&words(
            "git log -O/tmp/orderfile"
        )));
    }

    #[test]
    fn interleaved_wrapper_transparent_facts() {
        let f = segment_exec_facts(&words("command git status"));
        assert!(f.has_git && !f.exec_risk);
        let f = segment_exec_facts(&words("exec sort --compress-program=/tmp/pwn in"));
        assert!(f.exec_risk && !f.has_git);
        let f = segment_exec_facts(&words("builtin git -c core.fsmonitor=/tmp/pwn status"));
        assert!(f.exec_risk && f.has_git);

        for cmd in [
            "command env git status",
            "exec env RUST_LOG=debug git status",
            "command timeout 1 git status",
            "timeout 1 command env git status",
            "command exec env git status",
            "/usr/bin/command env /usr/bin/git status",
            "command env command env command env git status",
        ] {
            let f = segment_exec_facts(&words(cmd));
            assert!(f.has_git || f.exec_risk, "{cmd} → {f:?}");
        }
        assert!(
            segment_exec_facts(&words("command env sort --compress-program=/tmp/pwn in")).exec_risk
        );
        assert!(
            segment_exec_facts(&words(
                "command timeout 1 env git -c core.fsmonitor=/x status"
            ))
            .exec_risk
        );
        assert!(segment_exec_facts(&words("command env -C /evil git status")).exec_risk);
        assert!(segment_exec_facts(&words("command --unknown git status")).exec_risk);
        assert!(segment_exec_facts(&words("command exec git status")).has_git);
    }

    #[test]
    fn interleaved_ambient_plans() {
        let root = tempfile::tempdir().unwrap();
        let base = root.path();
        for cmd in [
            "command env git -C sub status",
            "command timeout 1 git -C sub status",
            "timeout 1 command env git -C sub status",
        ] {
            match ambient_scan_plan_from_cmd(cmd, base).unwrap() {
                AmbientScanPlan::CheckDirs(d) => assert_eq!(d, vec![base.join("sub")], "{cmd}"),
                other => panic!("{cmd}: expected CheckDirs, got {other:?}"),
            }
        }
        assert!(matches!(
            ambient_scan_plan_from_cmd("command env -C /evil git status", base).unwrap(),
            AmbientScanPlan::FailClosed
        ));
        assert!(matches!(
            ambient_scan_plan_from_cmd("command --unknown git status", base).unwrap(),
            AmbientScanPlan::FailClosed
        ));
    }

    #[test]
    fn attached_and_chained_c_paths() {
        let plan = |cmd: &str, cwd: &Path| ambient_scan_plan_from_cmd(cmd, cwd).unwrap();
        let root = tempfile::tempdir().unwrap();
        let base = root.path();
        match plan("git -C sub status", base) {
            AmbientScanPlan::CheckDirs(d) => {
                assert_eq!(d, vec![base.join("sub")]);
            }
            other => panic!("expected CheckDirs, got {other:?}"),
        }
        match plan("git -C/abs/path status", base) {
            AmbientScanPlan::CheckDirs(d) => {
                assert_eq!(d, vec![PathBuf::from("/abs/path")]);
            }
            other => panic!("expected CheckDirs, got {other:?}"),
        }
        match plan("git -C a -C b status", base) {
            AmbientScanPlan::CheckDirs(d) => {
                assert_eq!(d, vec![base.join("a").join("b")]);
            }
            other => panic!("expected CheckDirs, got {other:?}"),
        }
        assert!(matches!(
            plan("git --git-dir=evil/.git status", base),
            AmbientScanPlan::FailClosed
        ));
    }

    #[test]
    fn ambient_config_fixtures() {
        for (cfg, should_flag) in [
            ("[core]\n\tfsmonitor = /tmp/pwn\n", true),
            ("[diff \"evil\"]\n\tcommand = /tmp/pwn\n", true),
            ("[diff \"evil\"]\n\ttextconv = /tmp/pwn\n", true),
            ("[alias]\n\tstatus = !/tmp/pwn\n", true),
            (
                "[core]\n\trepositoryformatversion = 0\n\tfsmonitor = true\n\
                 [filter \"lfs\"]\n\tclean = git-lfs clean -- %f\n\
                 \tsmudge = git-lfs smudge -- %f\n\
                 \tprocess = git-lfs filter-process\n\
                 [alias]\n\tst = status\n",
                false,
            ),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            git2::Repository::init(tmp.path()).unwrap();
            std::fs::write(tmp.path().join(".git/config"), cfg).unwrap();
            assert_eq!(
                local_repo_config_has_exec_risk(tmp.path()),
                should_flag,
                "cfg={cfg:?}"
            );
        }

        // Plain include.
        {
            let tmp = tempfile::tempdir().unwrap();
            git2::Repository::init(tmp.path()).unwrap();
            std::fs::write(
                tmp.path().join(".git/extra"),
                "[core]\nfsmonitor = /tmp/pwn\n",
            )
            .unwrap();
            std::fs::write(
                tmp.path().join(".git/config"),
                "[core]\n\trepositoryformatversion = 0\n\
                 [include]\n\tpath = extra\n",
            )
            .unwrap();
            assert!(local_repo_config_has_exec_risk(tmp.path()));
        }

        // includeIf.gitdir: exact absolute gitdir (no trailing slash).
        // libgit2 appends `**` when the pattern ends with `/`, and wildmatch
        // `dir/**` does not match `dir` itself — so trailing-slash patterns fail.
        // Use repo.path() as libgit2 reports it (not a re-canonicalized twin).
        {
            let tmp = tempfile::tempdir().unwrap();
            let repo = git2::Repository::init(tmp.path()).unwrap();
            let gitdir_pat = repo
                .path()
                .to_string_lossy()
                .trim_end_matches(['/', '\\'])
                .to_owned();
            std::fs::write(
                tmp.path().join(".git/extra-if"),
                "[core]\nfsmonitor = /tmp/pwn\n",
            )
            .unwrap();
            std::fs::write(
                tmp.path().join(".git/config"),
                format!(
                    "[core]\n\trepositoryformatversion = 0\n\
                     [includeIf \"gitdir:{gitdir_pat}\"]\n\tpath = extra-if\n"
                ),
            )
            .unwrap();
            assert!(
                local_repo_config_has_exec_risk(tmp.path()),
                "includeIf.gitdir must be honored"
            );
        }

        // Config path is a directory (opens on Linux) → not a regular file → fail closed.
        {
            let tmp = tempfile::tempdir().unwrap();
            git2::Repository::init(tmp.path()).unwrap();
            let cfg = tmp.path().join(".git/config");
            std::fs::remove_file(&cfg).unwrap();
            std::fs::create_dir(&cfg).unwrap();
            assert!(
                local_repo_config_has_exec_risk(tmp.path()),
                "unopenable config path must fail closed"
            );
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let tmp = tempfile::tempdir().unwrap();
            git2::Repository::init(tmp.path()).unwrap();
            let cfg = tmp.path().join(".git/config");
            let mut perms = std::fs::metadata(&cfg).unwrap().permissions();
            perms.set_mode(0o000);
            std::fs::set_permissions(&cfg, perms).unwrap();
            // Root and CAP_DAC_OVERRIDE can still open mode 000 files.
            if std::fs::File::open(&cfg).is_err() {
                assert!(
                    local_repo_config_has_exec_risk(tmp.path()),
                    "unreadable config must fail closed"
                );
            }
            let mut perms = std::fs::metadata(&cfg).unwrap().permissions();
            perms.set_mode(0o644);
            std::fs::set_permissions(&cfg, perms).unwrap();
        }
    }

    #[test]
    fn script_may_invoke_git_probe() {
        assert!(script_may_invoke_git("git status $(true)"));
        assert!(script_may_invoke_git("/usr/bin/git status"));
        assert!(script_may_invoke_git("cd x && git diff"));
        assert!(script_may_invoke_git(r"C:\Git\cmd\git.exe status $(true)"));
        assert!(!script_may_invoke_git("echo hello"));
        assert!(!script_may_invoke_git("mygit status"));
        // Fail-closed false positive: bare `git` token in unparseable script.
        assert!(script_may_invoke_git("echo git $(true)"));
    }

    #[test]
    fn ambient_cd_and_git_c() {
        let root = tempfile::tempdir().unwrap();
        let clean = root.path().join("clean");
        let evil = root.path().join("evil");
        std::fs::create_dir_all(&clean).unwrap();
        std::fs::create_dir_all(&evil).unwrap();
        git2::Repository::init(&clean).unwrap();
        git2::Repository::init(&evil).unwrap();
        std::fs::write(evil.join(".git/config"), "[core]\nfsmonitor = /tmp/pwn\n").unwrap();

        let plan = ambient_scan_plan_from_cmd("git -C evil status", &clean).unwrap();
        assert!(ambient_exec_risk_from_plan(&plan));

        let plan = ambient_scan_plan_from_cmd("cd evil && git status", &clean).unwrap();
        assert!(ambient_exec_risk_from_plan(&plan));

        // `$HOME` expansion is rejected by word-only parse → ambient plan is
        // unavailable (`None`). Production maps that to fail-closed via
        // `unparseable_exec_risk` → `script_may_invoke_git` (do not invent a
        // word-only plan that weakens the expansion boundary).
        let expansion = "cd \"$HOME\" && git status";
        assert!(
            ambient_scan_plan_from_cmd(expansion, &clean).is_none(),
            "expansion must stay outside word-only ambient planning"
        );
        assert!(
            script_may_invoke_git(expansion),
            "unparseable git-bearing script must fail closed"
        );

        let plan = ambient_scan_plan_from_cmd("git status", &clean).unwrap();
        assert!(!ambient_exec_risk_from_plan(&plan));
    }

    #[test]
    fn worktree_common_and_config_worktree() {
        let main = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(main.path()).unwrap();
        let sig = git2::Signature::now("t", "t@t").unwrap();
        {
            let mut index = repo.index().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = repo.find_tree(tree_id).unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
                .unwrap();
        }
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("wt-branch", &head, false).unwrap();
        let wt_dir = main.path().join("wt");
        let mut opts = git2::WorktreeAddOptions::new();
        let branch = repo
            .find_branch("wt-branch", git2::BranchType::Local)
            .unwrap();
        opts.reference(Some(branch.get()));
        repo.worktree("wt", &wt_dir, Some(&opts)).unwrap();

        std::fs::write(
            main.path().join(".git/config"),
            "[core]\n\trepositoryformatversion = 0\n\tfsmonitor = /tmp/pwn\n",
        )
        .unwrap();
        assert!(local_repo_config_has_exec_risk(&wt_dir));

        std::fs::write(
            main.path().join(".git/config"),
            "[core]\n\trepositoryformatversion = 0\n\tfsmonitor = true\n\
             [extensions]\n\tworktreeConfig = true\n",
        )
        .unwrap();
        std::fs::write(
            main.path().join(".git/worktrees/wt/config.worktree"),
            "[core]\nfsmonitor = /tmp/pwn\n",
        )
        .unwrap();
        assert!(local_repo_config_has_exec_risk(&wt_dir));
    }
}
