//! Glob deny entries: detection, the macOS Seatbelt-regex translation, and the
//! Linux launch-time expansion. A deny entry is a GLOB iff it contains a glob
//! metacharacter; macOS emits an anchored runtime regex (covers files created
//! after launch), Linux expands to concrete existing matches at bwrap launch
//! (best-effort).
//!
//! Parity invariant: `validate_deny_glob` accepts/rejects identically on both
//! platforms, and the accepted subset translates the SAME on both — asserted by
//! the `macos_regex_matches_globset_property` cross-product test.

#[cfg(all(feature = "enforce", unix))]
use nono::CapabilitySet;
#[cfg(all(feature = "enforce", target_os = "linux"))]
use std::collections::BTreeSet;
#[cfg(all(feature = "enforce", unix))]
use std::path::{Path, PathBuf};
#[cfg(all(feature = "enforce", target_os = "linux"))]
use std::sync::Mutex;
// macOS regex translation reuses the parent module's alias + write-deny helpers.
use super::is_glob;
#[cfg(all(feature = "enforce", target_os = "macos"))]
use super::{emit_seatbelt_deny, macos_deny_aliases};

/// Split a profile's raw deny entries into exact paths (handled by the literal /
/// subpath kernel-deny flow) and glob patterns. Non-glob entries are returned
/// unchanged so their exact-path enforcement is preserved with no regression.
#[cfg(all(feature = "enforce", unix))]
pub(crate) fn partition_deny_entries(deny: &[PathBuf]) -> (Vec<PathBuf>, Vec<String>) {
    let mut exact = Vec::new();
    let mut globs = Vec::new();
    for entry in deny {
        match entry.to_str() {
            Some(s) if is_glob(s) => globs.push(s.to_string()),
            _ => exact.push(entry.clone()),
        }
    }
    (exact, globs)
}

/// Split a glob into its literal root and the tail from the first glob
/// component (`secrets/**` -> `<workspace>/secrets` + `**`; `/home/**/.ssh` ->
/// `/home` + `**/.ssh`). Root plus tail always re-joins to the original
/// pattern, so the macOS regex body is unchanged; the alias set follows the
/// (possibly deeper) root.
#[cfg(all(feature = "enforce", unix))]
fn split_glob_root(workspace: &Path, glob: &str) -> (PathBuf, String) {
    let (mut root, rest) = match glob.strip_prefix('/') {
        Some(absolute) => (PathBuf::from("/"), absolute),
        None => (workspace.to_path_buf(), glob),
    };
    let segments: Vec<&str> = rest.split('/').collect();
    let Some(first_glob_index) = segments.iter().position(|segment| is_glob(segment)) else {
        return (root, rest.to_string());
    };
    for segment in &segments[..first_glob_index] {
        if !segment.is_empty() {
            root.push(segment);
        }
    }
    (root, segments[first_glob_index..].join("/"))
}

/// Validate a deny glob on BOTH platforms so a given pattern is interpreted
/// IDENTICALLY everywhere or rejected everywhere (never silently under-enforced
/// on macOS). Two checks, run before the macOS regex translation and the Linux
/// globset expansion alike:
///
/// 1. Reject `{`/`}`/`\`: globset honors brace alternation and backslash-escapes,
///    but Seatbelt's runtime regex (sourced from globset's own `.regex()` mis-
///    enforces `**/` for root-level paths, so we hand-roll the regex instead and
///    cannot faithfully reproduce those forms — rejecting them on both platforms
///    keeps the two backends in agreement. A user wanting alternation writes
///    separate deny entries.
/// 2. Compile through `globset` (the Linux matcher) so a malformed glob (`a**b`,
///    unterminated `[`) fails closed identically on both platforms.
#[cfg(all(feature = "enforce", unix))]
pub(crate) fn validate_deny_glob(glob: &str) -> anyhow::Result<()> {
    if let Some(c) = glob.chars().find(|&c| matches!(c, '{' | '}' | '\\')) {
        anyhow::bail!(
            "deny glob {glob:?} uses unsupported metacharacter '{c}' \
             (brace alternation and backslash-escapes are not supported; \
             use separate deny entries)"
        );
    }
    // `**` must be a whole path component (gitignore semantics). A non-component
    // `**` (e.g. `a**b`) would translate to `.*` on macOS but collapse to `*` in
    // globset — reject it on both platforms so they never diverge. Empty
    // segments (`a//*`) drift the same way: globset keeps `//` literally while
    // the macOS regex collapses it.
    for (index, segment) in glob.split('/').enumerate() {
        if segment.is_empty() && !(index == 0 && glob.starts_with('/')) {
            anyhow::bail!(
                "deny glob {glob:?}: empty path segment (a doubled '//' or \
                 trailing '/'); remove the extra slash in sandbox.toml"
            );
        }
        // `.`/`..` would let a relative glob scan outside the workspace on
        // Linux while the macOS regex stays dead; reject on both platforms.
        if segment == "." || segment == ".." {
            anyhow::bail!(
                "deny glob {glob:?}: `.` and `..` segments are not supported; \
                 write the path without them (use an absolute path to deny \
                 files outside the workspace)"
            );
        }
        if segment.contains("**") && segment != "**" {
            anyhow::bail!(
                "deny glob {glob:?}: `**` must be its own path segment (got {segment:?}); \
                 write it as `**/` or `/**`, e.g. `a/**/b`"
            );
        }
    }
    // Char classes: support only the simple subset that translates identically to
    // globset. Reject a literal `]`-first member (`[]a]`) and any nested `[` —
    // which covers POSIX `[[:…:]]` — since globset and the hand-rolled regex parse
    // those differently. (A leading `!`/`^` negation IS supported.)
    let cc: Vec<char> = glob.chars().collect();
    let mut i = 0;
    while i < cc.len() {
        if cc[i] != '[' {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        if matches!(cc.get(j), Some('!') | Some('^')) {
            j += 1;
        }
        if cc.get(j) == Some(&']') {
            anyhow::bail!("deny glob {glob:?}: a literal ']' as first class member is unsupported");
        }
        while j < cc.len() && cc[j] != ']' {
            if cc[j] == '[' {
                anyhow::bail!(
                    "deny glob {glob:?}: nested '[' / POSIX '[[:…:]]' classes are unsupported"
                );
            }
            j += 1;
        }
        // Unterminated class: let the globset build below report it uniformly.
        i = if j < cc.len() { j + 1 } else { cc.len() };
    }
    globset::GlobBuilder::new(glob)
        .literal_separator(true)
        .build()
        .map_err(|e| anyhow::anyhow!("invalid deny glob {glob:?}: {e}"))?;
    Ok(())
}

/// Push `c` as a regex literal, escaping it when it is a regex metacharacter.
#[cfg(all(feature = "enforce", target_os = "macos"))]
fn push_escaped_regex_literal(out: &mut String, c: char) {
    if matches!(
        c,
        '.' | '+' | '*' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '^' | '$' | '|' | '\\'
    ) {
        out.push('\\');
    }
    out.push(c);
}

/// Regex-escape every character of a literal path segment.
#[cfg(all(feature = "enforce", target_os = "macos"))]
fn escape_regex_literal_str(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        push_escaped_regex_literal(&mut out, c);
    }
    out
}

/// Translate a gitignore-style glob tail into an (unanchored) Seatbelt regex
/// body. Dialect: `**/`->`(.*/)?`, `**`->`.*`, `*`->`[^/]*`, `?`->`[^/]`,
/// `[...]` classes copied with a leading `!`/`^` -> regex negation `[^…]`, all
/// other literal text regex-escaped. Only the class subset `validate_deny_glob`
/// accepts reaches here, so it always matches globset.
#[cfg(all(feature = "enforce", target_os = "macos"))]
fn glob_tail_to_regex(tail: &str) -> String {
    let mut out = String::new();
    let mut chars = tail.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' => {
                if chars.peek() == Some(&'*') {
                    chars.next();
                    if chars.peek() == Some(&'/') {
                        chars.next();
                        out.push_str("(.*/)?"); // `**/` spans zero or more dirs
                    } else {
                        out.push_str(".*"); // `**` spans anything, incl. `/`
                    }
                } else {
                    out.push_str("[^/]*"); // `*` stops at a path separator
                }
            }
            '?' => out.push_str("[^/]"),
            '[' => {
                out.push('[');
                // globset treats a leading `!` OR `^` as negation -> regex `[^…]`
                // (validate_deny_glob has rejected the class forms that would drift).
                if matches!(chars.peek(), Some('!') | Some('^')) {
                    chars.next();
                    out.push('^');
                }
                while let Some(cc) = chars.next() {
                    if cc == ']' {
                        break;
                    }
                    // Backslash-escapes are rejected by validate_deny_glob; this
                    // passthrough stays defensive.
                    if cc == '\\' {
                        out.push('\\');
                        if let Some(n) = chars.next() {
                            out.push(n);
                        }
                    } else {
                        out.push(cc);
                    }
                }
                out.push(']');
            }
            c => push_escaped_regex_literal(&mut out, c),
        }
    }
    out
}

/// Canonicalize the nearest existing ancestor and re-append the rest; a glob
/// root whose prefix does not exist yet must keep the workspace's aliases.
#[cfg(all(feature = "enforce", target_os = "macos"))]
fn canonicalize_existing_ancestor(root: &Path) -> PathBuf {
    let mut ancestor = root;
    let mut rest = Vec::new();
    loop {
        if let Ok(canonical) = dunce::canonicalize(ancestor) {
            return rest
                .iter()
                .rev()
                .fold(canonical, |acc, comp| acc.join(comp));
        }
        let Some(name) = ancestor.file_name() else {
            return root.to_path_buf();
        };
        rest.push(name);
        let Some(parent) = ancestor.parent() else {
            return root.to_path_buf();
        };
        ancestor = parent;
    }
}

/// Anchored Seatbelt regex bodies for one glob, one per alias form of its root
/// (the `/private` firmlink must not bypass the deny). A symlinked prefix also
/// anchors the deny at its resolved target, matching the Linux masking.
#[cfg(all(feature = "enforce", target_os = "macos"))]
fn glob_to_seatbelt_regexes(workspace: &Path, glob: &str) -> Vec<String> {
    let (root, tail) = split_glob_root(workspace, glob);
    let tail_regex = glob_tail_to_regex(&tail);
    let canonical_root = canonicalize_existing_ancestor(&root);
    let mut regexes = Vec::new();
    for form in macos_deny_aliases(&root, &canonical_root) {
        let Some(form_str) = form.to_str() else {
            continue;
        };
        let escaped_root = escape_regex_literal_str(form_str);
        // Avoid a double slash when the root is `/`.
        let sep = if escaped_root.ends_with('/') { "" } else { "/" };
        regexes.push(format!("^{escaped_root}{sep}{tail_regex}$"));
    }
    regexes
}

/// Wrap a finished regex body in a Seatbelt `(regex #"…")` filter, escaping the
/// SBPL string delimiter and rejecting control chars. Fail-closed: returns
/// `None` for an inexpressible pattern so the caller errors rather than emitting
/// a rule that silently targets the wrong path.
#[cfg(all(feature = "enforce", target_os = "macos"))]
fn seatbelt_regex_filter(regex: &str) -> Option<String> {
    if regex.chars().any(|c| c.is_control()) {
        return None;
    }
    let escaped = regex.replace('"', "\\\"");
    Some(format!("(regex #\"{escaped}\")"))
}

/// Apply kernel-level deny rules for glob patterns.
///
/// On macOS, translate each glob to an anchored Seatbelt regex and emit the same
/// read + per-write-sub-action denies as the exact-path flow (so `mv x y && cat y`
/// stays closed), covering files created after launch. On Linux this is a no-op:
/// a mount namespace can't match a regex at runtime, so globs are expanded to
/// concrete paths and bound over at bwrap re-exec (see [`expand_deny_globs`]).
///
/// Unlike the exact-path flow, this does NOT call `remove_exact_file_caps_for_paths`
/// (a glob can't enumerate the file caps it collides with); glob denies rely on
/// Seatbelt last-match ordering — the deny platform rules are emitted after the
/// read/write allows, so the regex deny wins. The e2e is the contract.
#[cfg(all(feature = "enforce", unix))]
pub(crate) fn apply_deny_globs_to_capability_set(
    caps: &mut CapabilitySet,
    workspace: &Path,
    globs: &[String],
) -> anyhow::Result<()> {
    if globs.is_empty() {
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    {
        for glob in globs {
            // Fail CLOSED on any glob that isn't expressible identically on both
            // platforms (braces/backslash) or is malformed — same check Linux runs.
            validate_deny_glob(glob)?;
            let regexes = glob_to_seatbelt_regexes(workspace, glob);
            if regexes.is_empty() {
                // Fail CLOSED: a glob we can't anchor would be silently
                // unprotected while the sandbox still reports active.
                anyhow::bail!("cannot translate deny glob {glob:?} to a Seatbelt regex");
            }
            for regex in regexes {
                let Some(filter) = seatbelt_regex_filter(&regex) else {
                    anyhow::bail!("cannot express deny glob {glob:?} as a Seatbelt regex filter");
                };
                emit_seatbelt_deny(caps, &filter)?;
            }
        }
        tracing::info!(
            count = globs.len(),
            "Applied Seatbelt deny regexes for sandbox deny globs"
        );
    }

    #[cfg(target_os = "linux")]
    {
        let _ = (caps, workspace);
        tracing::debug!(
            count = globs.len(),
            "Linux deny globs are expanded and bound over at bwrap re-exec (launch-time)"
        );
    }

    Ok(())
}

/// Launch-time expansion caps: a mount namespace can't glob at runtime, so
/// matches are enumerated once at launch. Exceeding a cap fails closed.
#[cfg(all(feature = "enforce", target_os = "linux"))]
#[derive(Clone, Copy)]
pub(crate) struct DenyGlobCaps {
    /// Directory depth; a directory at the cap could hide deeper matches.
    pub depth: usize,
    /// Each match becomes one `--ro-bind`; kept under bwrap's argv budget.
    pub matches: usize,
    /// Visited-entry budget bounding launch latency; the walk includes
    /// gitignored/hidden trees, so ordinary repos reach hundreds of thousands.
    pub entries: usize,
}

#[cfg(all(feature = "enforce", target_os = "linux"))]
pub(crate) const DENY_GLOB_CAPS: DenyGlobCaps = DenyGlobCaps {
    depth: 64,
    matches: 4096,
    entries: 2_000_000,
};

/// A permission error is skippable (the agent is equally denied by the OS);
/// any other walk error could hide a readable match, so it fails closed.
#[cfg(all(feature = "enforce", target_os = "linux"))]
fn deny_glob_walk_error_is_fatal(err: &ignore::Error) -> bool {
    match err.io_error() {
        Some(io) => io.kind() != std::io::ErrorKind::PermissionDenied,
        None => true,
    }
}

/// Insert a match plus its canonical target when the path involves a symlink;
/// masking only the logical path leaves the content readable at its real
/// location. Canonicalize failures keep just the logical path.
#[cfg(all(feature = "enforce", target_os = "linux"))]
fn insert_match(
    matches: &Mutex<BTreeSet<String>>,
    path: &Path,
    globs: &[String],
    max_matches: usize,
) -> Result<(), String> {
    let mut paths = vec![path.to_path_buf()];
    if let Ok(canonical) = dunce::canonicalize(path)
        && canonical.as_path() != path
    {
        paths.push(canonical);
    }
    let mut matched = matches.lock().expect("deny-glob matches mutex poisoned");
    for candidate in paths {
        // A non-UTF8 path can't be bound as a string; fail closed.
        let Some(text) = candidate.to_str() else {
            return Err(format!(
                "deny-glob match has a non-UTF8 path: {candidate:?}"
            ));
        };
        matched.insert(text.to_owned());
        if matched.len() > max_matches {
            return Err(format!(
                "the deny globs {globs:?} matched over {max_matches} files; \
                 use narrower globs, or deny a parent directory as an exact \
                 path instead"
            ));
        }
    }
    Ok(())
}

/// Expand deny globs into the concrete existing paths to bind over. Fails
/// closed on an invalid glob, an exceeded cap, or a non-permission walk error.
/// Only files that exist at launch are covered; macOS enforces the same globs
/// as runtime Seatbelt regexes.
#[cfg(all(feature = "enforce", target_os = "linux"))]
pub(crate) fn expand_deny_globs(
    workspace: &Path,
    globs: &[String],
    caps: DenyGlobCaps,
) -> Result<Vec<String>, String> {
    use globset::{GlobBuilder, GlobSetBuilder};
    use ignore::{WalkBuilder, WalkState};
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // A lossy workspace pattern would silently match nothing; fail closed.
    let Some(workspace_str) = workspace.to_str() else {
        return Err(format!("workspace path is not UTF-8: {workspace:?}"));
    };
    let mut builder = GlobSetBuilder::new();
    let mut roots: BTreeSet<PathBuf> = BTreeSet::new();
    for glob in globs {
        // Same validation as macOS, so both platforms fail closed identically.
        validate_deny_glob(glob).map_err(|err| err.to_string())?;
        // Relative globs get the escaped workspace prefix; `literal_separator`
        // keeps `*`/`?` from crossing `/`, matching the macOS translation.
        let pattern = if glob.starts_with('/') {
            glob.clone()
        } else {
            format!("{}/{}", globset::escape(workspace_str), glob)
        };
        let compiled = GlobBuilder::new(&pattern)
            .literal_separator(true)
            .build()
            .map_err(|_| format!("could not compile glob {glob:?}"))?;
        builder.add(compiled);
        roots.insert(split_glob_root(workspace, glob).0);
    }
    let glob_set = builder
        .build()
        .map_err(|_| "could not build glob set".to_string())?;

    let matches: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());
    // First fail-closed cause wins; parallel walkers race to `Quit`.
    let failure: OnceLock<String> = OnceLock::new();
    let visited = AtomicUsize::new(0);
    // Overlapping globs share one walk. A nested root is covered by its
    // parent's walk only when the segment below the parent holds no symlinks
    // (the walk does not descend them): its canonical path then equals the
    // parent's canonical path plus that segment.
    let mut walked: Vec<&Path> = Vec::new();
    for root in &roots {
        if !root.exists() {
            continue;
        }
        let covered = walked.iter().any(|walked_root| {
            let Ok(relative) = root.strip_prefix(walked_root) else {
                return false;
            };
            match (dunce::canonicalize(walked_root), dunce::canonicalize(root)) {
                (Ok(parent), Ok(child)) => child == parent.join(relative),
                _ => false,
            }
        });
        if covered {
            continue;
        }
        walked.push(root.as_path());
        // Include hidden/gitignored files (denied secrets usually are both);
        // never descend symlinked dirs, though a named root resolves through one.
        WalkBuilder::new(root)
            .max_depth(Some(caps.depth))
            .standard_filters(false)
            .follow_links(false)
            .build_parallel()
            .run(|| {
                Box::new(|entry| {
                    let entry = match entry {
                        Ok(entry) => entry,
                        Err(err) if deny_glob_walk_error_is_fatal(&err) => {
                            let _ = failure
                                .set(format!("walk error while expanding deny globs: {err}"));
                            return WalkState::Quit;
                        }
                        Err(err) => {
                            tracing::warn!(error = %err, "skipping unreadable entry during deny-glob walk");
                            return WalkState::Continue;
                        }
                    };
                    if visited.fetch_add(1, Ordering::Relaxed) >= caps.entries {
                        let _ = failure.set(format!(
                            "expanding the deny globs {globs:?} visited over {} entries \
                             across their roots (stopped in {} at {}; gitignored and \
                             hidden files are included in the scan). Use narrower globs \
                             with a literal prefix, or deny exact paths; see the sandbox \
                             guide (~/.echo-agent/docs/user-guide/18-sandbox.md)",
                            caps.entries,
                            root.display(),
                            entry.path().display(),
                        ));
                        return WalkState::Quit;
                    }
                    // A directory at the depth cap may hide deeper matches; fail closed.
                    if entry.depth() >= caps.depth
                        && entry.file_type().is_some_and(|file_type| file_type.is_dir())
                    {
                        let _ = failure.set(format!(
                            "{} is deeper than the {}-level scan limit for the deny \
                             globs {globs:?}, so deeper matches could be hidden; deny \
                             a parent directory as an exact path instead",
                            entry.path().display(),
                            caps.depth,
                        ));
                        return WalkState::Quit;
                    }
                    if glob_set.is_match(entry.path())
                        && let Err(reason) =
                            insert_match(&matches, entry.path(), globs, caps.matches)
                    {
                        let _ = failure.set(reason);
                        return WalkState::Quit;
                    }
                    WalkState::Continue
                })
            });
        if let Some(reason) = failure.get() {
            return Err(reason.clone());
        }
    }
    Ok(matches
        .into_inner()
        .expect("deny-glob matches mutex poisoned")
        .into_iter()
        .collect())
}

#[cfg(test)]
mod tests {
    // All tests here exercise enforce+unix paths; without the gate `super::*`
    // is unused on `--no-default-features`.
    #[cfg(all(feature = "enforce", unix))]
    use super::*;

    #[test]
    #[cfg(all(feature = "enforce", unix))]
    fn is_glob_detects_metacharacters() {
        assert!(is_glob("**/.env"));
        assert!(is_glob("**/*.pem"));
        assert!(is_glob("secrets/**"));
        assert!(is_glob("a?b"));
        assert!(is_glob("[abc].txt"));
        // Exact paths must NOT be treated as globs (no regression in literal deny).
        assert!(!is_glob(".env"));
        assert!(!is_glob("src/server.pem"));
        assert!(!is_glob("/etc/shadow"));
    }

    #[test]
    #[cfg(all(feature = "enforce", unix))]
    fn partition_separates_globs_from_exact_paths() {
        let deny = vec![
            PathBuf::from(".env"),
            PathBuf::from("**/*.pem"),
            PathBuf::from("/etc/shadow"),
            PathBuf::from("secrets/**"),
        ];
        let (exact, globs) = partition_deny_entries(&deny);
        assert_eq!(
            exact,
            vec![PathBuf::from(".env"), PathBuf::from("/etc/shadow")]
        );
        assert_eq!(
            globs,
            vec!["**/*.pem".to_string(), "secrets/**".to_string()]
        );
    }

    #[test]
    #[cfg(all(feature = "enforce", unix))]
    fn split_glob_root_relative_vs_absolute() {
        let workspace = Path::new("/ws");
        assert_eq!(
            split_glob_root(workspace, "**/.env"),
            (PathBuf::from("/ws"), "**/.env".to_string())
        );
        assert_eq!(
            split_glob_root(workspace, "/home/**/.ssh"),
            (PathBuf::from("/home"), "**/.ssh".to_string())
        );
        assert_eq!(
            split_glob_root(workspace, "secrets/**"),
            (PathBuf::from("/ws/secrets"), "**".to_string())
        );
        assert_eq!(
            split_glob_root(workspace, "a/b/*.pem"),
            (PathBuf::from("/ws/a/b"), "*.pem".to_string())
        );
    }

    #[test]
    #[cfg(all(feature = "enforce", unix))]
    fn validate_deny_glob_accepts_subset_rejects_rest() {
        // Supported subset (`*`, `?`, `**`, `[...]` incl. `[!a]`/`[^a]` negation).
        for glob in [
            "**/*.pem",
            "**/.env",
            "secrets/**",
            "[abc].txt",
            "[a-z].rs",
            "[!a]b",
            "[^a]b",
            "a?b",
            "/home/**/.ssh",
        ] {
            assert!(
                validate_deny_glob(glob).is_ok(),
                "{glob} should be supported"
            );
        }
        // Braces + backslash drift macOS vs globset -> rejected (fail closed) on BOTH.
        for glob in ["**/*.{pem,key}", "a\\*b", "{a,b}"] {
            assert!(
                validate_deny_glob(glob).is_err(),
                "{glob} should be rejected"
            );
        }
        // Char-class forms that parse differently in globset vs the regex engine.
        for glob in ["[]a]", "[[:alpha:]]", "[a[b]"] {
            assert!(
                validate_deny_glob(glob).is_err(),
                "{glob} unsupported char class should be rejected"
            );
        }
        // Malformed globs fail closed identically to the Linux globset backend.
        for glob in ["a**b", "**a", "[abc"] {
            assert!(
                validate_deny_glob(glob).is_err(),
                "{glob} should be rejected as malformed"
            );
        }
        for glob in ["a//b", "secrets//**", "a/b/"] {
            assert!(
                validate_deny_glob(glob).is_err(),
                "{glob} with an empty segment should be rejected"
            );
        }
        // `.`/`..` would scan outside the workspace on Linux only.
        for glob in ["../up/**", "./x/*", "a/../b/*"] {
            assert!(
                validate_deny_glob(glob).is_err(),
                "{glob} with a dot segment should be rejected"
            );
        }
    }

    #[test]
    #[cfg(all(feature = "enforce", target_os = "macos"))]
    fn glob_to_regex_doubles_private_aliased_root() {
        // A workspace under /tmp (firmlinked to /private/tmp) must emit a deny
        // regex for BOTH alias roots, else the broad read-allow leaks via the alias.
        let regexes = glob_to_seatbelt_regexes(Path::new("/tmp/projalias"), "**/.env");
        assert!(
            regexes.contains(&"^/tmp/projalias/(.*/)?\\.env$".to_string()),
            "{regexes:?}"
        );
        assert!(
            regexes.contains(&"^/private/tmp/projalias/(.*/)?\\.env$".to_string()),
            "{regexes:?}"
        );
    }

    #[test]
    #[cfg(all(feature = "enforce", target_os = "macos"))]
    fn macos_regex_matches_globset_property() {
        // PARITY GUARD (cross-product): for EVERY pattern `validate_deny_glob`
        // accepts, the hand-rolled macOS regex must match a path IFF globset (the
        // Linux backend) matches it. Generated from building blocks (literals,
        // regex-metachar literal, `*`, `?`, `**`, char classes incl. both
        // negations, ranges, class-content edge cases, and a `]` outside a class)
        // crossed with sample paths, so any future dialect drift fails
        // mechanically. Rejected forms are asserted to fail closed.
        let segs = [
            "a", "x.y", "*", "?", "**", "[abc]", "[a-z]", "[!a]", "[^a]", "[.]", "[*]", "[a^]",
            "[a-]", "[-a]", "*]",
        ];
        let paths = [
            "a",
            "ab",
            "x",
            "x.y",
            "xay",
            "^",
            "!",
            ".",
            "-",
            "*",
            "b",
            "a/b",
            "ab/cd",
            "sub/a",
            "sub/dir/a",
            ".env",
            "sub/.env",
            "k.pem",
            "foo.env",
            ".envrc",
            "secrets/x",
            "]",
            "a]",
        ];
        // Build single- and two-segment patterns from the blocks.
        let mut patterns: Vec<String> = Vec::new();
        for a in segs {
            patterns.push(a.to_string());
            for b in segs {
                patterns.push(format!("{a}/{b}"));
            }
        }
        for p in &patterns {
            if validate_deny_glob(p).is_err() {
                continue; // rejected patterns aren't enforced on either platform
            }
            let regexes = glob_to_seatbelt_regexes(Path::new("/ws"), p);
            assert_eq!(regexes.len(), 1, "expected one regex for {p:?}");
            let re = regex::Regex::new(&regexes[0]).unwrap_or_else(|e| panic!("{p:?}: {e}"));
            let gs = globset::GlobBuilder::new(&format!("/ws/{p}"))
                .literal_separator(true)
                .build()
                .unwrap()
                .compile_matcher();
            for path in paths {
                let abs = format!("/ws/{path}");
                assert_eq!(
                    re.is_match(&abs),
                    gs.is_match(&abs),
                    "DRIFT for pattern {p:?} on {abs:?}: macos={}, globset={}",
                    re.is_match(&abs),
                    gs.is_match(&abs)
                );
            }
        }
        // Forms that MUST fail closed on both platforms (cannot translate identically).
        for bad in [
            "[]a]",
            "[[:alpha:]]",
            "{a,b}",
            "**/*.{pem,key}",
            "a**b",
            "**a",
            "[abc",
            "a//*",
        ] {
            assert!(validate_deny_glob(bad).is_err(), "{bad} must be rejected");
        }
    }

    #[test]
    #[cfg(all(feature = "enforce", target_os = "macos"))]
    fn glob_regex_keeps_workspace_aliases_for_missing_prefix() {
        // A missing literal prefix must not drop the workspace's /private alias.
        let regexes = glob_to_seatbelt_regexes(Path::new("/tmp/projalias"), "newdir/**");
        assert!(
            regexes.contains(&"^/tmp/projalias/newdir/.*$".to_string()),
            "{regexes:?}"
        );
        assert!(
            regexes.contains(&"^/private/tmp/projalias/newdir/.*$".to_string()),
            "{regexes:?}"
        );
    }

    #[test]
    #[cfg(all(feature = "enforce", target_os = "macos"))]
    fn glob_to_regex_roots_absolute_at_literal_prefix() {
        let regexes =
            glob_to_seatbelt_regexes(Path::new("/ws-does-not-exist-xyz"), "/nope-xyz/**/.ssh");
        assert_eq!(regexes, vec!["^/nope-xyz/(.*/)?\\.ssh$".to_string()]);
    }

    #[test]
    #[cfg(all(feature = "enforce", target_os = "macos"))]
    fn seatbelt_regex_filter_wraps_and_rejects_control_chars() {
        assert_eq!(
            seatbelt_regex_filter("^/workspace/(.*/)?\\.env$").unwrap(),
            "(regex #\"^/workspace/(.*/)?\\.env$\")"
        );
        assert!(seatbelt_regex_filter("a\u{07}b").is_none());
    }

    #[test]
    #[cfg(all(feature = "enforce", target_os = "macos"))]
    fn apply_deny_globs_emits_nono_accepted_rules() {
        // Every emitted `(deny … (regex …))` must pass nono's validate_platform_rule.
        let mut caps = CapabilitySet::new();
        let globs = vec![
            "**/*.pem".to_string(),
            "**/.env".to_string(),
            "secrets/**".to_string(),
        ];
        apply_deny_globs_to_capability_set(&mut caps, Path::new("/ws-does-not-exist-xyz"), &globs)
            .expect("emitted Seatbelt deny regexes must be accepted by nono");
    }

    // Linux launch-time expansion + its fail-closed caps. Gated to enforce+linux,
    // so they run on the Linux CI lane (not on macOS).
    #[cfg(all(feature = "enforce", target_os = "linux"))]
    mod linux_expand {
        use super::*;

        struct TmpTree(PathBuf);
        impl Drop for TmpTree {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        fn tmp_tree(tag: &str) -> PathBuf {
            let p = std::env::temp_dir().join(format!(
                "deny-glob-ut-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&p).unwrap();
            // A symlinked temp dir would double every insert (logical +
            // canonical) and skew the cap-boundary assertions.
            dunce::canonicalize(&p).unwrap()
        }
        fn caps(depth: usize, matches: usize, entries: usize) -> DenyGlobCaps {
            DenyGlobCaps {
                depth,
                matches,
                entries,
            }
        }

        #[test]
        fn production_entries_cap_covers_large_workspaces() {
            // 200k refused startup on ordinary large workspaces.
            const { assert!(DENY_GLOB_CAPS.entries >= 2_000_000) };
        }

        #[test]
        fn matches_nested_pem_and_dotenv_excludes_control() {
            let workspace = tmp_tree("match");
            let _workspace_guard = TmpTree(workspace.clone());
            std::fs::create_dir_all(workspace.join("sub/dir")).unwrap();
            std::fs::write(workspace.join("sub/dir/key.pem"), "x").unwrap();
            std::fs::write(workspace.join(".env"), "x").unwrap(); // hidden + usually gitignored
            std::fs::write(workspace.join("readable.txt"), "x").unwrap();
            let globs = vec!["**/*.pem".to_string(), "**/.env".to_string()];
            let expanded =
                expand_deny_globs(&workspace, &globs, DENY_GLOB_CAPS).expect("should expand");
            assert!(
                expanded.iter().any(|p| p.ends_with("sub/dir/key.pem")),
                "{expanded:?}"
            );
            assert!(
                expanded.iter().any(|p| p.ends_with("/.env")),
                "{expanded:?}"
            );
            assert!(
                !expanded.iter().any(|p| p.ends_with("readable.txt")),
                "{expanded:?}"
            );
        }

        #[test]
        fn empty_when_nothing_matches() {
            let workspace = tmp_tree("empty");
            let _workspace_guard = TmpTree(workspace.clone());
            std::fs::write(workspace.join("a.txt"), "x").unwrap();
            let expanded =
                expand_deny_globs(&workspace, &["**/*.pem".to_string()], DENY_GLOB_CAPS).unwrap();
            assert!(expanded.is_empty(), "{expanded:?}");
        }

        #[test]
        fn fails_closed_on_match_cap() {
            let workspace = tmp_tree("matchcap");
            let _workspace_guard = TmpTree(workspace.clone());
            for i in 0..5 {
                std::fs::write(workspace.join(format!("k{i}.pem")), "x").unwrap();
            }
            let err =
                expand_deny_globs(&workspace, &["**/*.pem".to_string()], caps(64, 2, 200_000))
                    .unwrap_err();
            assert!(err.contains("matched over 2 files"), "{err}");
            assert!(
                expand_deny_globs(&workspace, &["**/*.pem".to_string()], caps(64, 5, 200_000))
                    .is_ok()
            );
        }

        #[test]
        fn fails_closed_on_depth_cap() {
            let workspace = tmp_tree("depthcap");
            let _workspace_guard = TmpTree(workspace.clone());
            std::fs::create_dir_all(workspace.join("a/b/c")).unwrap();
            std::fs::write(workspace.join("a/b/c/k.pem"), "x").unwrap();
            let err = expand_deny_globs(
                &workspace,
                &["**/*.pem".to_string()],
                caps(1, 4096, 200_000),
            )
            .unwrap_err();
            assert!(err.contains("scan limit"), "{err}");
        }

        #[test]
        fn fails_closed_on_entries_cap_and_names_the_stop_point() {
            let workspace = tmp_tree("entriescap");
            let _workspace_guard = TmpTree(workspace.clone());
            for i in 0..10 {
                std::fs::write(workspace.join(format!("f{i}.txt")), "x").unwrap();
            }
            let err = expand_deny_globs(&workspace, &["**/*.pem".to_string()], caps(64, 4096, 3))
                .unwrap_err();
            assert!(err.contains("visited over 3 entries"), "{err}");
            assert!(err.contains(workspace.to_str().unwrap()), "{err}");
            // Exactly at the budget (root dir + 10 files) still succeeds.
            assert!(
                expand_deny_globs(&workspace, &["**/*.pem".to_string()], caps(64, 4096, 11))
                    .is_ok()
            );
        }

        #[test]
        fn entries_budget_is_shared_across_roots() {
            let workspace = tmp_tree("sharedbudget");
            let _workspace_guard = TmpTree(workspace.clone());
            let other = tmp_tree("sharedbudget-other");
            let _outside_guard = TmpTree(other.clone());
            for i in 0..4 {
                std::fs::write(workspace.join(format!("a{i}.txt")), "x").unwrap();
                std::fs::write(other.join(format!("b{i}.txt")), "x").unwrap();
            }
            // Each root alone stays under the budget; together they exceed it.
            let globs = vec![
                "**/*.pem".to_string(),
                format!("{}/**/*.pem", other.to_str().unwrap()),
            ];
            assert!(expand_deny_globs(&workspace, &globs, caps(64, 4096, 6)).is_err());
            assert!(expand_deny_globs(&workspace, &globs, caps(64, 4096, 200_000)).is_ok());
        }

        #[test]
        fn literal_prefix_narrows_the_walk_root() {
            let workspace = tmp_tree("narrow");
            let _workspace_guard = TmpTree(workspace.clone());
            std::fs::create_dir_all(workspace.join("big")).unwrap();
            std::fs::create_dir_all(workspace.join("sub")).unwrap();
            for i in 0..20 {
                std::fs::write(workspace.join(format!("big/f{i}.txt")), "x").unwrap();
            }
            std::fs::write(workspace.join("sub/key.pem"), "x").unwrap();
            // `sub/**` walks only `<workspace>/sub`; the budget is smaller than `big/`.
            let expanded =
                expand_deny_globs(&workspace, &["sub/**".to_string()], caps(64, 4096, 5))
                    .expect("narrowed walk stays under the budget");
            assert!(
                expanded.iter().any(|p| p.ends_with("sub/key.pem")),
                "{expanded:?}"
            );
        }

        #[test]
        fn rejects_unsupported_glob() {
            let workspace = tmp_tree("reject");
            let _workspace_guard = TmpTree(workspace.clone());
            // Braces are rejected identically to macOS (fail closed).
            assert!(
                expand_deny_globs(&workspace, &["**/*.{pem,key}".to_string()], DENY_GLOB_CAPS)
                    .is_err()
            );
        }

        #[test]
        fn symlinked_nested_root_keeps_its_own_walk() {
            let workspace = tmp_tree("nestedsym");
            let _workspace_guard = TmpTree(workspace.clone());
            let outside = tmp_tree("nestedsym-target");
            let _outside_guard = TmpTree(outside.clone());
            std::fs::write(outside.join("key.pem"), "x").unwrap();
            std::os::unix::fs::symlink(&outside, workspace.join("secrets")).unwrap();
            // The workspace walk skips the symlink; the narrow root walks itself.
            let globs = vec!["**/*.pem".to_string(), "secrets/**".to_string()];
            let expanded = expand_deny_globs(&workspace, &globs, DENY_GLOB_CAPS).unwrap();
            assert!(
                expanded.iter().any(|p| p.ends_with("secrets/key.pem")),
                "{expanded:?}"
            );
        }

        #[test]
        fn nested_root_under_symlinked_parent_is_not_rewalked() {
            let outside = tmp_tree("symparent-target");
            let _workspace_guard = TmpTree(outside.clone());
            std::fs::create_dir_all(outside.join("sub")).unwrap();
            std::fs::write(outside.join("sub/key.pem"), "x").unwrap();
            let holder = tmp_tree("symparent");
            let _outside_guard = TmpTree(holder.clone());
            let link = holder.join("link");
            std::os::unix::fs::symlink(&outside, &link).unwrap();
            let link_str = link.to_str().unwrap();
            let globs = vec![format!("{link_str}/**/*.pem"), format!("{link_str}/sub/**")];
            // The nested root resolves to the parent's target plus `sub`, so
            // the parent's walk covers it; a one-walk budget suffices.
            let workspace = tmp_tree("symparent-workspace");
            let _extra_guard = TmpTree(workspace.clone());
            let expanded =
                expand_deny_globs(&workspace, &globs, caps(64, 4096, 3)).expect("one shared walk");
            assert!(
                expanded.iter().any(|p| p.ends_with("sub/key.pem")),
                "{expanded:?}"
            );
        }

        #[test]
        fn does_not_descend_symlinked_dir() {
            let workspace = tmp_tree("symlink");
            let _workspace_guard = TmpTree(workspace.clone());
            let outside = tmp_tree("symlink-outside");
            let _outside_guard = TmpTree(outside.clone());
            std::fs::write(outside.join("secret.pem"), "x").unwrap();
            std::os::unix::fs::symlink(&outside, workspace.join("link")).unwrap();
            // follow_links(false): the symlink must not smuggle its target in.
            let expanded =
                expand_deny_globs(&workspace, &["**/*.pem".to_string()], DENY_GLOB_CAPS).unwrap();
            assert!(
                !expanded.iter().any(|p| p.contains("secret.pem")),
                "symlinked dir must not be descended: {expanded:?}"
            );
        }

        #[test]
        fn symlinked_match_masks_canonical_target() {
            let workspace = tmp_tree("symlink-match");
            let _workspace_guard = TmpTree(workspace.clone());
            let outside = tmp_tree("symlink-match-target");
            let _outside_guard = TmpTree(outside.clone());
            std::fs::write(outside.join("real.pem"), "x").unwrap();
            std::os::unix::fs::symlink(outside.join("real.pem"), workspace.join("link.pem"))
                .unwrap();
            let expanded =
                expand_deny_globs(&workspace, &["**/*.pem".to_string()], DENY_GLOB_CAPS).unwrap();
            assert!(
                expanded.iter().any(|p| p.ends_with("link.pem")),
                "{expanded:?}"
            );
            let canonical = dunce::canonicalize(outside.join("real.pem")).unwrap();
            assert!(
                expanded.iter().any(|p| Path::new(p) == canonical),
                "canonical target of a matched symlink must be masked too: {expanded:?}"
            );
        }

        #[test]
        fn canonical_targets_count_toward_the_match_cap() {
            let workspace = tmp_tree("symlink-cap");
            let _workspace_guard = TmpTree(workspace.clone());
            let outside = tmp_tree("symlink-cap-target");
            let _outside_guard = TmpTree(outside.clone());
            std::fs::write(outside.join("real.pem"), "x").unwrap();
            std::os::unix::fs::symlink(outside.join("real.pem"), workspace.join("link.pem"))
                .unwrap();
            // One matched symlink yields two bind paths: logical + canonical.
            assert!(
                expand_deny_globs(&workspace, &["**/*.pem".to_string()], caps(64, 1, 200_000))
                    .is_err()
            );
            assert!(
                expand_deny_globs(&workspace, &["**/*.pem".to_string()], caps(64, 2, 200_000))
                    .is_ok()
            );
        }

        #[test]
        fn walk_error_permission_skipped_others_fatal() {
            use std::io;
            let perm = ignore::Error::from(io::Error::from(io::ErrorKind::PermissionDenied));
            assert!(!deny_glob_walk_error_is_fatal(&perm));
            let other = ignore::Error::from(io::Error::other("boom"));
            assert!(deny_glob_walk_error_is_fatal(&other));
            let loop_err = ignore::Error::Loop {
                ancestor: PathBuf::from("/a"),
                child: PathBuf::from("/a/b"),
            };
            assert!(deny_glob_walk_error_is_fatal(&loop_err));
        }

        #[test]
        fn fails_closed_on_non_utf8_workspace() {
            use std::os::unix::ffi::OsStrExt;
            let base = tmp_tree("nonutf8-workspace");
            let _workspace_guard = TmpTree(base.clone());
            let workspace = base.join(std::ffi::OsStr::from_bytes(b"workspace\xFF"));
            std::fs::create_dir_all(&workspace).unwrap();
            let err = expand_deny_globs(&workspace, &["**/*.pem".to_string()], DENY_GLOB_CAPS)
                .unwrap_err();
            assert!(err.contains("not UTF-8"), "{err}");
        }

        #[test]
        fn fails_closed_on_non_utf8_match() {
            use std::os::unix::ffi::OsStrExt;
            let workspace = tmp_tree("nonutf8");
            let _workspace_guard = TmpTree(workspace.clone());
            let name = std::ffi::OsStr::from_bytes(b"secret\xFF.pem");
            std::fs::write(workspace.join(name), "x").unwrap();
            let err = expand_deny_globs(&workspace, &["**/*.pem".to_string()], DENY_GLOB_CAPS)
                .unwrap_err();
            assert!(err.contains("non-UTF8"), "{err}");
        }
    }
}
