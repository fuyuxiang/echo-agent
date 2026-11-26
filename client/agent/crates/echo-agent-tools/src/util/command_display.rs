//! Display-only helpers for shell command chrome (activity titles, execute headers).
//!
//! Path equality for peel is **lexical** (segment-wise on `/` and `\`), not
//! `canonicalize`d — so `/var` vs `/private/var` or symlink roots miss peel
//! rather than false-peel. Callers should store session cwd in the same string
//! form agents embed in `cd` tokens when possible.
//!
//! [`paths_equal_for_display`] is only meaningful **after** the absolute-shaped
//! gate in [`peel_cd_prefix`]; segment equality alone would treat `proj` and
//! `/proj` as equal, which must never drive a peel on its own.

use std::borrow::Cow;
use std::path::Path;

/// Peel a leading `cd <session_cwd> &&|;` (or Windows `cd /d`) when the target
/// equals session cwd so TUI chrome shows the real command first.
///
/// Only absolute-shaped path tokens are considered (Unix `/…`, Windows `X:\` /
/// `X:/`, or `\\` UNC) so relative `cd proj` cannot false-match `/proj`.
/// Fail-closed on ambiguous quotes, empty remainder, pipes-only, or path mismatch.
/// Does not canonicalize; works with Windows-shaped paths on any host OS.
/// Single outer `(cd … &&|;) …)` is supported; nested parens are not peeled.
pub fn strip_redundant_session_cd<'a>(command: &'a str, session_cwd: &Path) -> Cow<'a, str> {
    let trimmed = command.trim_start();
    let inner = trim_wrapping_parens(trimmed).unwrap_or(trimmed);
    // `remainder` is always a sub-slice of `command`, so borrow in every case.
    match peel_cd_prefix(inner, session_cwd) {
        Some(remainder) => Cow::Borrowed(remainder),
        None => Cow::Borrowed(command),
    }
}

/// Path equality for display peel: segment-wise with `/` and `\` as separators,
/// trailing-separator tolerant, case-insensitive for Windows-shaped drive paths.
/// No canonicalize; works for Windows fixtures on Unix hosts.
///
/// **Not general path equality.** Call only after both sides are known
/// absolute-shaped (`is_absolute_shaped_path_token`); otherwise `proj` and
/// `/proj` compare equal by segments alone. Peel enforces that gate in
/// [`peel_cd_prefix`] before invoking this helper.
fn paths_equal_for_display(a: &Path, b: &Path) -> bool {
    let a_str = a.to_string_lossy();
    let b_str = b.to_string_lossy();
    debug_assert!(
        is_absolute_shaped_path_token(&a_str) && is_absolute_shaped_path_token(&b_str),
        "paths_equal_for_display is only meaningful after absolute-shaped gate \
         (got {a_str:?} vs {b_str:?})"
    );
    let a_win = is_windows_shaped_str(&a_str);
    let b_win = is_windows_shaped_str(&b_str);
    let case_insensitive = a_win || b_win;

    let a_segs = path_segments(&a_str);
    let b_segs = path_segments(&b_str);
    if a_segs.len() != b_segs.len() {
        return false;
    }
    for (as_, bs) in a_segs.iter().zip(b_segs.iter()) {
        if case_insensitive {
            if !as_.eq_ignore_ascii_case(bs) {
                return false;
            }
        } else if as_ != bs {
            return false;
        }
    }
    true
}

/// Absolute-shaped for peel: leading `/`, `X:` drive, or `\\` UNC.
fn is_absolute_shaped_path_token(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    if bytes[0] == b'/' {
        return true;
    }
    is_windows_shaped_str(s)
}

fn is_windows_shaped_str(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return true;
    }
    bytes.len() >= 2 && bytes[0] == b'\\' && bytes[1] == b'\\'
}

/// Split on `/` or `\`, drop empty segments (trailing sep / root markers).
fn path_segments(s: &str) -> Vec<&str> {
    let trimmed = s.trim_end_matches(['/', '\\']);
    trimmed
        .split(['/', '\\'])
        .filter(|p| !p.is_empty())
        .collect()
}

/// Trim a single outer `( … )` pair if the closing paren is the last non-ws char.
fn trim_wrapping_parens(s: &str) -> Option<&str> {
    let t = s.trim();
    if !t.starts_with('(') || !t.ends_with(')') || t.len() < 2 {
        return None;
    }
    let inner = &t[1..t.len() - 1];
    if inner.contains('(') || inner.contains(')') {
        return None;
    }
    Some(inner.trim())
}

fn peel_cd_prefix<'a>(command: &'a str, session_cwd: &Path) -> Option<&'a str> {
    let s = command.trim_start();
    if s.is_empty() || s.contains('\n') || s.contains('\r') {
        return None;
    }

    let mut rest = s;

    // Only `cd` (not PowerShell Set-Location/sl/chdir).
    let (w, after) = take_shell_word(rest)?;
    if !w.eq_ignore_ascii_case("cd") {
        return None;
    }
    rest = after;

    // Windows cmd: `cd /d <path>`
    if let Some((flag, after_flag)) = take_shell_word(rest)
        && (flag == "/d" || flag == "/D")
    {
        rest = after_flag;
    }

    let (path_token, after_path) = take_path_token(rest)?;
    if path_token.contains('\n') || path_token.contains('\r') {
        return None;
    }

    let path_unquoted = unquote_path_token(path_token)?;
    if path_unquoted == "-" || path_unquoted == ".." {
        return None;
    }
    // Relative tokens must not peel (avoids `cd proj` matching session `/proj`).
    if !is_absolute_shaped_path_token(path_unquoted) {
        return None;
    }

    // Fail closed (not panic via the absolute-shaped debug_assert) when the
    // session cwd itself isn't absolute-shaped.
    if !is_absolute_shaped_path_token(&session_cwd.to_string_lossy()) {
        return None;
    }
    let cmd_path = Path::new(path_unquoted);
    if !paths_equal_for_display(cmd_path, session_cwd) {
        return None;
    }

    let after = after_path.trim_start();
    let remainder = if let Some(stripped) = after.strip_prefix("&&") {
        stripped.trim_start()
    } else if let Some(stripped) = after.strip_prefix(';') {
        stripped.trim_start()
    } else {
        return None;
    };
    if remainder.is_empty() {
        return None;
    }
    Some(remainder)
}

/// First shell word: unquoted run of non-whitespace, or fail on leading quote.
fn take_shell_word(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start();
    if s.is_empty() {
        return None;
    }
    let first = s.chars().next()?;
    if first == '\'' || first == '"' {
        return None;
    }
    let end = s
        .char_indices()
        .find(|(_, c)| c.is_whitespace())
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    if end == 0 {
        return None;
    }
    Some((&s[..end], &s[end..]))
}

/// Path token: quoted string or unquoted until whitespace / separator start.
fn take_path_token(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start();
    if s.is_empty() {
        return None;
    }
    let bytes = s.as_bytes();
    match bytes[0] {
        b'\'' | b'"' => {
            let quote = bytes[0];
            let mut i = 1;
            while i < bytes.len() {
                if bytes[i] == quote {
                    return Some((&s[..=i], &s[i + 1..]));
                }
                if bytes[i] == b'\\' && quote == b'"' && i + 1 < bytes.len() {
                    i += 2;
                    continue;
                }
                i += 1;
            }
            None
        }
        _ => {
            let mut end = 0;
            let chars: Vec<(usize, char)> = s.char_indices().collect();
            for (idx, (byte_i, ch)) in chars.iter().enumerate() {
                if ch.is_whitespace() || *ch == ';' || *ch == '|' {
                    end = *byte_i;
                    break;
                }
                if *ch == '&' && chars.get(idx + 1).is_some_and(|(_, n)| *n == '&') {
                    end = *byte_i;
                    break;
                }
                end = byte_i + ch.len_utf8();
            }
            if end == 0 {
                return None;
            }
            Some((&s[..end], &s[end..]))
        }
    }
}

fn unquote_path_token(token: &str) -> Option<&str> {
    let t = token.trim();
    if t.len() >= 2 {
        let b = t.as_bytes();
        if (b[0] == b'\'' && b[t.len() - 1] == b'\'') || (b[0] == b'"' && b[t.len() - 1] == b'"') {
            return Some(&t[1..t.len() - 1]);
        }
        if b[0] == b'\'' || b[0] == b'"' {
            return None;
        }
    }
    Some(t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn cwd(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    fn expect_peel(command: &str, session: &str, want: &str) {
        let got = strip_redundant_session_cd(command, &cwd(session));
        assert_eq!(
            got.as_ref(),
            want,
            "peel failed for command={command:?} cwd={session:?}"
        );
    }

    fn expect_no_peel(command: &str, session: &str) {
        let got = strip_redundant_session_cd(command, &cwd(session));
        assert_eq!(
            got.as_ref(),
            command,
            "unexpected peel for command={command:?} cwd={session:?} got={got:?}"
        );
    }

    #[test]
    fn matrix_happy_path_unix() {
        expect_peel(r#"cd /proj && python -c "x""#, "/proj", r#"python -c "x""#);
        expect_peel("cd /proj; ls -la", "/proj", "ls -la");
        expect_peel("  cd /proj && make", "/proj", "make");
        expect_peel("cd /proj && cd sub && make", "/proj", "cd sub && make");
        expect_peel("cd '/proj with spaces' && ls", "/proj with spaces", "ls");
        expect_peel(r#"cd "/proj with spaces" && ls"#, "/proj with spaces", "ls");
        expect_peel("(cd /proj && cargo test)", "/proj", "cargo test");
        expect_peel("(cd /proj; cargo test)", "/proj", "cargo test");
        expect_peel("cd /proj/ && pytest", "/proj", "pytest");
        expect_peel(
            "cd /Users/u/code/my-project && python -c \"print(1)\"",
            "/Users/u/code/my-project",
            "python -c \"print(1)\"",
        );
    }

    #[test]
    fn matrix_windows_shaped_peel_on_any_host() {
        let win = r"C:\Users\a\proj";
        expect_peel(r"cd C:\Users\a\proj && cargo test", win, "cargo test");
        expect_peel("cd C:/Users/a/proj && cargo test", win, "cargo test");
        expect_peel(r"cd /d C:\Users\a\proj && cargo test", win, "cargo test");
        expect_peel(r"cd /d C:\Users\a\proj; dir", win, "dir");
        expect_peel(r"cd c:\users\a\proj && cargo test", win, "cargo test");
        expect_peel(
            r#"cd "C:\Users\a\My Project" && msbuild"#,
            r"C:\Users\a\My Project",
            "msbuild",
        );
        expect_peel(r"(cd C:\Users\a\proj && cargo test)", win, "cargo test");
    }

    #[test]
    fn matrix_no_peel_fail_closed() {
        let proj = "/proj";
        let win = r"C:\Users\a\proj";
        expect_no_peel("cd /other && ls", proj);
        expect_no_peel("cd /proj", proj);
        expect_no_peel(r#"python -c "x""#, proj);
        expect_no_peel("cd subdir && ls", proj);
        // Relative basename must not match absolute session cwd segments.
        expect_no_peel("cd proj && ls", proj);
        expect_no_peel("cd proj && ls", "/other/proj");
        expect_no_peel("cd /proj | wc -l", proj);
        expect_no_peel("cde /proj && ls", proj);
        expect_no_peel(r"cd D:\other && cargo test", win);
        expect_no_peel(r"cd /d D:\other && cargo test", win);
        expect_no_peel("cd '/proj && ls", proj);
        expect_no_peel("cd /proj &&", proj);
        expect_no_peel("cd /proj\n&& ls", proj);
        expect_no_peel("Push-Location /proj; ls", proj);
        expect_no_peel("Set-Location /proj; Get-ChildItem", proj);
        expect_no_peel("sl /proj && ls", proj);
        expect_no_peel("chdir /proj && ls", proj);
        expect_no_peel("cd - && ls", proj);
        expect_no_peel("cd .. && ls", "/proj/sub");
    }

    #[test]
    fn matrix_adversarial_and_model_noise() {
        let proj = "/proj";
        expect_peel("cd    /proj    &&    ls", proj, "ls");
        expect_no_peel("cd /proj || true", proj);
        expect_peel(
            "cd /proj && echo done && cd /tmp && true",
            proj,
            "echo done && cd /tmp && true",
        );
        let long = format!("cd {} && true", "/".to_string() + &"a".repeat(200));
        let long_cwd = "/".to_string() + &"a".repeat(200);
        expect_peel(&long, &long_cwd, "true");
        expect_peel("cd /proj && cd /proj && true", proj, "cd /proj && true");
    }

    #[test]
    fn paths_equal_slash_and_trailing() {
        assert!(paths_equal_for_display(
            Path::new(r"C:\Users\a\proj"),
            Path::new("C:/Users/a/proj")
        ));
        assert!(paths_equal_for_display(
            Path::new("/proj/"),
            Path::new("/proj")
        ));
        assert!(paths_equal_for_display(
            Path::new(r"C:\Users\a\proj\"),
            Path::new(r"C:\Users\a\proj")
        ));
        assert!(!paths_equal_for_display(
            Path::new("/proj"),
            Path::new("/other")
        ));
        // Relative vs absolute segment-"equality" is intentionally *not*
        // asserted here (would trip the absolute-shaped debug_assert). Peel
        // fail-closed coverage for `cd proj` vs session `/proj` lives in
        // `matrix_no_peel_fail_closed`.
    }
}
