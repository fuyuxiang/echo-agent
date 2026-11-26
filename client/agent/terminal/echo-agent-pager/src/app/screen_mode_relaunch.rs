//! Rebuild process argv and re-exec the pager into a different screen mode.
//!
//! Fallback only — `/minimal`/`/fullscreen` switch in process by default
//! (`super::mode_switch`); this remains for the startup env override, the
//! `ECHO_AGENT_SCREEN_MODE_SWITCH=exec` escape hatch, and unrecoverable transitions.
//!
//! On the exec path the event loop quits, the terminal is restored, then this
//! module replaces the process image with the same binary pointed at the
//! active session under the requested render mode. Unix `exec` keeps the
//! PTY/fd identity so PTY harness tests can observe the transition without
//! re-spawning; Windows emulates exec by spawning the child on the inherited
//! console and parking the parent in `wait` (see
//! [`exec_screen_mode_relaunch`]) so the launching shell never gets the
//! console back mid-session.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::sync::OnceLock;

/// Env var that forces screen-mode resolution regardless of CLI flag / config.
///
/// Set only on the re-exec path so a config `[terminal] minimal = true` cannot
/// keep a `/fullscreen` relaunch stuck in minimal, and vice-versa. Consumed
/// (read **and removed**) exactly once at startup by
/// [`take_screen_mode_env_override`]; not a public user interface.
pub(crate) const ECHO_AGENT_SCREEN_MODE_ENV: &str = "ECHO_AGENT_SCREEN_MODE";

/// Argv tokens (`--long`, `-s`, and their aliases) of [`super::cli::PagerArgs`]
/// flags that consume a following value token when not written as
/// `--flag=value`.
///
/// Derived from the clap definition itself (via [`clap::CommandFactory`]) so
/// the classification can never drift from the CLI: a stale hand-maintained
/// list would silently misclassify a new flag's value as the bare positional
/// prompt and drop it from the relaunch argv. Boolean switches contribute
/// nothing — a bare word following one is the positional prompt and must be
/// dropped on resume.
///
/// Only `PagerArgs` flags matter here: the argv being rebuilt was already
/// parsed by `PagerArgs` at startup, so no other flags can appear in it.
fn value_taking_flag_tokens() -> &'static HashSet<String> {
    static TOKENS: OnceLock<HashSet<String>> = OnceLock::new();
    TOKENS.get_or_init(|| {
        use clap::CommandFactory;
        let cmd = super::cli::PagerArgs::command();
        let mut tokens = HashSet::new();
        for arg in cmd.get_arguments() {
            if !arg.get_action().takes_values() {
                continue;
            }
            if let Some(long) = arg.get_long() {
                tokens.insert(format!("--{long}"));
            }
            for alias in arg.get_all_aliases().unwrap_or_default() {
                tokens.insert(format!("--{alias}"));
            }
            if let Some(short) = arg.get_short() {
                tokens.insert(format!("-{short}"));
            }
            for alias in arg.get_all_short_aliases().unwrap_or_default() {
                tokens.insert(format!("-{alias}"));
            }
        }
        tokens
    })
}

fn flag_takes_value(flag: &str) -> bool {
    value_taking_flag_tokens().contains(flag)
}

/// Rebuild argv (without the binary name) for reopening `session_id` in the
/// requested screen mode.
///
/// Strips prior session-selection / mode flags, one-shot session-creation
/// directives, and any bare positional prompt so a cold-start
/// `echoagent "do the thing"` does not re-submit on resume. Keeps everything else
/// (e.g. `--no-leader`, `--model`, endpoint overrides) intact, including the
/// value token that follows value-taking flags.
///
/// One-shot startup directives must not survive into the rebuilt argv:
/// `--session-id` combined with the appended `--resume` (without
/// `--fork-session`) is rejected at startup (`SessionIdRequiresFork`), so the
/// relaunched process would exit immediately; a kept `--worktree` /
/// `--worktree-ref` would create a *second* worktree on relaunch; a kept
/// `--restore-code` would re-checkout the original session commit. All of
/// them already did their job in the process being replaced.
pub(crate) fn build_screen_mode_relaunch_args(
    current_args: impl IntoIterator<Item = impl AsRef<OsStr>>,
    session_id: &str,
    want_minimal: bool,
) -> Vec<OsString> {
    let mut iter = current_args
        .into_iter()
        .map(|a| a.as_ref().to_os_string())
        .peekable();
    // Drop argv[0] (binary path) — caller supplies the exe separately.
    let _ = iter.next();

    let mut out: Vec<OsString> = Vec::new();
    while let Some(arg) = iter.next() {
        let s = arg.to_string_lossy();

        // `--` ends flag parsing: everything after it is positional, i.e. the
        // prompt, which must not re-fire on resume. Drop the separator too —
        // keeping it would make the appended `--resume <id>` positional.
        if s == "--" {
            break;
        }

        // Boolean / no-value flags to drop. `--restore-code` is a one-shot
        // resume directive (checkout already happened in the old process).
        // `--worktree` (optional-value) is handled below with the
        // value-taking drops. Both screen-mode flags go: the right one is
        // re-appended below, and a stale opposite would either trip the clap
        // `--minimal`/`--fullscreen` conflict or fight the requested mode.
        if matches!(
            s.as_ref(),
            "--minimal"
                | "--fullscreen"
                | "--continue"
                | "-c"
                | "--fork-session"
                | "--restore-code"
        ) {
            continue;
        }

        // `--flag=value` forms of the dropped value-taking flags.
        if s.starts_with("--resume=")
            || s.starts_with("--load=")
            || s.starts_with("--session-id=")
            || s.starts_with("-s=")
            || s.starts_with("--worktree=")
            || s.starts_with("--worktree-ref=")
            || s.starts_with("--ref=")
        {
            continue;
        }

        // Session-selection / one-shot session-creation flags with an
        // optional/required following value — drop flag and value; we rebind
        // via a fresh `--resume <id>` below. `--session-id` would make the
        // appended `--resume` an invalid combo (SessionIdRequiresFork) and
        // kill the relaunch at startup; `--worktree`/`--worktree-ref` would
        // create a second worktree.
        if matches!(
            s.as_ref(),
            "--resume"
                | "-r"
                | "--load"
                | "--session-id"
                | "-s"
                | "--worktree"
                | "-w"
                | "--worktree-ref"
                | "--ref"
        ) {
            if let Some(next) = iter.peek() {
                let ns = next.to_string_lossy();
                if !ns.starts_with('-') {
                    let _ = iter.next();
                }
            }
            continue;
        }

        // Keep flags (and their value tokens when space-separated).
        if s.starts_with('-') {
            let takes_value = !s.contains('=') && flag_takes_value(s.as_ref());
            out.push(arg);
            if takes_value && let Some(next) = iter.peek() {
                let ns = next.to_string_lossy();
                if !ns.starts_with('-') {
                    out.push(iter.next().expect("peeked value present"));
                }
            }
            continue;
        }

        // Bare positional prompt (e.g. `echoagent "fix the bug"`). Must not re-fire
        // on resume. Clap positionals never start with `-`. Values for earlier
        // flags were already consumed above, so any remaining bare word here is
        // the prompt.
        continue;
    }

    out.push(OsString::from("--resume"));
    out.push(OsString::from(session_id));
    // Keep a CLI mode flag for hand-pasted resume hints that omit ECHO_AGENT_SCREEN_MODE.
    if want_minimal {
        out.push(OsString::from("--minimal"));
    } else {
        out.push(OsString::from("--fullscreen"));
    }
    out
}

/// `ECHO_AGENT_SCREEN_MODE_SWITCH=exec` forces the legacy re-exec switch.
pub(crate) const SCREEN_MODE_SWITCH_ENV: &str = "ECHO_AGENT_SCREEN_MODE_SWITCH";

pub(crate) fn exec_switch_forced() -> bool {
    std::env::var(SCREEN_MODE_SWITCH_ENV).is_ok_and(|v| v.trim().eq_ignore_ascii_case("exec"))
}

/// Env value written for a screen-mode relaunch (`minimal` / `fullscreen`).
pub(crate) fn screen_mode_env_value(want_minimal: bool) -> &'static str {
    if want_minimal {
        "minimal"
    } else {
        "fullscreen"
    }
}

/// Pasteable shell command when auto re-exec fails (env + flag + `--resume`).
pub(crate) fn screen_mode_relaunch_resume_hint(session_id: &str, want_minimal: bool) -> String {
    let mode = screen_mode_env_value(want_minimal);
    let flag = if want_minimal {
        "--minimal"
    } else {
        "--fullscreen"
    };
    format!("{ECHO_AGENT_SCREEN_MODE_ENV}={mode} echoagent {flag} --resume {session_id}")
}

/// Replace the current process with a relaunch into the requested screen mode.
///
/// On success this function never returns: Unix `exec`s; Windows emulates
/// `exec` by spawning the child on the same console, **waiting** for it, and
/// exiting with its code. On failure it returns the IO error so the caller can
/// fall back to a resume hint.
pub(crate) fn exec_screen_mode_relaunch(session_id: &str, want_minimal: bool) -> io::Result<()> {
    let exe = std::env::current_exe()?;
    let args = build_screen_mode_relaunch_args(std::env::args_os(), session_id, want_minimal);

    let mut cmd = std::process::Command::new(&exe);
    cmd.args(&args);
    // Force mode resolution even when config.toml has the opposite preference.
    cmd.env(
        ECHO_AGENT_SCREEN_MODE_ENV,
        screen_mode_env_value(want_minimal),
    );

    let mode_label = screen_mode_env_value(want_minimal);
    let reverse = if want_minimal {
        "/fullscreen"
    } else {
        "/minimal"
    };
    eprintln!("Reopening session in {mode_label} mode… (switch back with {reverse})");
    let _ = io::stdout().flush();
    let _ = io::stderr().flush();

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = cmd.exec();
        // `exec` only returns on failure.
        Err(io::Error::other(format!("failed to exec relaunch: {err}")))
    }

    #[cfg(windows)]
    {
        // Windows has no exec(2); emulate it the way cargo/rustup do — spawn
        // the child on the inherited console, park this parent in `wait`, and
        // exit with the child's code.
        //
        // Spawning and then `exit(0)`-ing immediately (the first shipped
        // version) is catastrophic: the launching shell sees its child (this
        // parent) exit, prints a prompt, and starts reading console input
        // *concurrently* with the still-running detached TUI — duplicated UI,
        // interleaved output, and two consumers of every keystroke. Waiting
        // keeps the shell parked behind this process exactly as it would be
        // behind an exec'd image, and preserves the console/PTY identity the
        // harness tests observe.
        cmd.stdin(std::process::Stdio::inherit())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit());
        // Route Ctrl+C / Ctrl+Break to the child alone for the rest of this
        // (now inert) parent's life: a console Ctrl event is delivered to
        // every process attached to the console, and the default handler
        // would kill this parent mid-`wait` — dropping the shell back onto
        // the console while the child TUI still runs (the same double-reader
        // mess the wait exists to prevent).
        //
        // SAFETY: FFI with a null handler pointer, documented by the Console
        // API to mean "ignore Ctrl+C in this process"; called once, before
        // the child exists, from the only surviving thread of a quiesced
        // event loop.
        unsafe {
            windows_sys::Win32::System::Console::SetConsoleCtrlHandler(None, 1);
        }
        // The event loop's input reader thread exits within one poll cycle
        // (POLL_TIMEOUT = 100ms in `event_loop::run`) of the loop dropping its
        // receiver. Unix `exec` kills that thread atomically; here the parent
        // survives, so give the reader a full cycle to park before the child
        // attaches to the same console input buffer — a still-polling parent
        // reader competes with the child for console records and swallows its
        // first keystrokes.
        std::thread::sleep(std::time::Duration::from_millis(150));
        #[allow(clippy::disallowed_methods)] // the parent waits and exits with its status
        let mut child = cmd.spawn()?;
        let status = child.wait()?;
        std::process::exit(status.code().unwrap_or(0));
    }

    #[cfg(not(any(unix, windows)))]
    {
        Err(io::Error::other(
            "screen-mode relaunch unsupported on this platform",
        ))
    }
}

/// Parse a [`ECHO_AGENT_SCREEN_MODE_ENV`] or config `[ui] screen_mode` value
/// (pure; unit-tested directly).
///
/// Case- and whitespace-insensitive for the known tokens, matching
/// [`crate::settings::canonical_screen_mode`] so a hand-edited
/// `Minimal` / `FULLSCREEN` is honored at startup the same way settings
/// displays it. Unlike the settings canonicalizer, unknown / absent /
/// legacy values (`default`, `auto`, empty) return `None` so soft
/// defaults (mouse-leak, pager.toml) still apply.
///
/// | Value | Mode |
/// |---|---|
/// | `minimal` | [`super::ScreenMode::Minimal`] |
/// | `fullscreen` / `full` | [`super::ScreenMode::Fullscreen`] |
/// | anything else / absent | `None` — normal resolution continues |
pub(crate) fn parse_screen_mode(value: Option<&str>) -> Option<super::ScreenMode> {
    let raw = value?.trim();
    if raw.is_empty() {
        return None;
    }
    if raw.eq_ignore_ascii_case("minimal") {
        Some(super::ScreenMode::Minimal)
    } else if raw.eq_ignore_ascii_case("fullscreen") || raw.eq_ignore_ascii_case("full") {
        Some(super::ScreenMode::Fullscreen)
    } else {
        None
    }
}

/// Consume the one-shot screen-mode override env (see [`ECHO_AGENT_SCREEN_MODE_ENV`]).
///
/// Reads **and removes** the variable so the override is truly one-shot: it
/// must not linger in this process's environment where every spawned child
/// (tool shells, workers, nested `echoagent` invocations) would inherit a forced
/// screen mode the user never asked for.
///
/// When set, the returned mode **wins** over CLI flags (`--minimal`,
/// `--no-alt-screen`), config (`[terminal] minimal`, `alt_screen`), and
/// auto-inline environment heuristics — see [`resolve_screen_mode`]. That way
/// `/fullscreen` always reopens in alt-screen fullscreen — not inline — even
/// under Zellij, `alt_screen = never`, or a preserved `--no-alt-screen`.
///
/// Call once, early in [`crate::app::run`].
pub(crate) fn take_screen_mode_env_override() -> Option<super::ScreenMode> {
    let raw = std::env::var_os(ECHO_AGENT_SCREEN_MODE_ENV);
    if raw.is_some() {
        // SAFETY: called once during pager startup, before the event loop and
        // before this process spawns threads that read the environment. Any
        // set value is removed (even an unparseable one) so children never
        // inherit the override.
        unsafe { std::env::remove_var(ECHO_AGENT_SCREEN_MODE_ENV) };
    }
    parse_screen_mode(raw.as_deref().and_then(OsStr::to_str))
}

/// CLI > `[ui] screen_mode` > pager.toml `[terminal] minimal` > no preference.
///
/// `Some(true)` = minimal, `Some(false)` = not minimal (explicit fullscreen).
/// `None` = no sticky preference — caller may apply soft defaults (JediTerm
/// mouse-leak auto-minimal). Settings UI still *displays* Fullscreen when the
/// key is unset; choosing Fullscreen writes an explicit value so that soft
/// default no longer applies.
pub(crate) fn effective_minimal_preference(
    cli_minimal: bool,
    cli_fullscreen: bool,
    config_screen_mode: Option<&str>,
    pager_toml_minimal: bool,
) -> Option<bool> {
    if cli_minimal {
        return Some(true);
    }
    if cli_fullscreen {
        return Some(false);
    }
    match parse_screen_mode(config_screen_mode) {
        Some(super::ScreenMode::Minimal) => Some(true),
        Some(_) => Some(false),
        None if pager_toml_minimal => Some(true),
        None => None,
    }
}

/// Env override > minimal flag/config > alt-screen policy.
pub(crate) fn resolve_screen_mode(
    env_override: Option<super::ScreenMode>,
    minimal_cli_or_config: bool,
    alt_screen_wants_fullscreen: bool,
) -> super::ScreenMode {
    if let Some(forced) = env_override {
        return forced;
    }
    if minimal_cli_or_config {
        super::ScreenMode::Minimal
    } else if alt_screen_wants_fullscreen {
        super::ScreenMode::Fullscreen
    } else {
        super::ScreenMode::Inline
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(parts: &[&str]) -> Vec<OsString> {
        parts.iter().map(|s| OsString::from(*s)).collect()
    }

    fn as_strs(v: &[OsString]) -> Vec<String> {
        v.iter().map(|s| s.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn value_taking_flag_tokens_derived_from_clap() {
        let tokens = value_taking_flag_tokens();
        // Value-taking flags (long, short, alias forms) are classified.
        for flag in [
            "--model",
            "-m",
            "--cwd",
            "--leader-socket",
            "--resume",
            "-r",
            "--load",
        ] {
            assert!(tokens.contains(flag), "expected value-taking flag {flag}");
        }
        // Boolean switches are not — a bare word after one is the prompt.
        for flag in [
            "--minimal",
            "--fullscreen",
            "--no-leader",
            "--continue",
            "-c",
            "--fork-session",
        ] {
            assert!(!tokens.contains(flag), "boolean flag misclassified: {flag}");
        }
    }

    #[test]
    fn adds_minimal_and_resume() {
        let out = build_screen_mode_relaunch_args(args(&["echoagent", "--no-leader"]), "abc", true);
        assert_eq!(
            as_strs(&out),
            vec!["--no-leader", "--resume", "abc", "--minimal"]
        );
    }

    /// The fullscreen direction appends an explicit `--fullscreen` so mode
    /// resolution still works without the env override.
    #[test]
    fn adds_fullscreen_and_resume() {
        let out =
            build_screen_mode_relaunch_args(args(&["echoagent", "--no-leader"]), "abc", false);
        assert_eq!(
            as_strs(&out),
            vec!["--no-leader", "--resume", "abc", "--fullscreen"]
        );
    }

    /// `--session-id` must not survive the rebuild: combined with the appended
    /// `--resume` (and no `--fork-session`) startup rejects the combo
    /// (`SessionIdRequiresFork`), so the relaunched process would exit
    /// immediately instead of reopening the session.
    #[test]
    fn strips_session_id_flag() {
        let out = build_screen_mode_relaunch_args(
            args(&[
                "echoagent",
                "--session-id",
                "11111111-1111-1111-1111-111111111111",
                "--no-leader",
            ]),
            "new",
            true,
        );
        assert_eq!(
            as_strs(&out),
            vec!["--no-leader", "--resume", "new", "--minimal"]
        );

        let out = build_screen_mode_relaunch_args(
            args(&["echoagent", "-s", "11111111-1111-1111-1111-111111111111"]),
            "new",
            false,
        );
        assert_eq!(as_strs(&out), vec!["--resume", "new", "--fullscreen"]);
    }

    /// One-shot session-creation directives (`--worktree`, `--worktree-ref`,
    /// `--restore-code`) already did their job in the process being replaced;
    /// keeping them would create a second worktree / re-checkout on relaunch.
    #[test]
    fn strips_worktree_and_restore_code() {
        let out = build_screen_mode_relaunch_args(
            args(&[
                "echoagent",
                "-w",
                "feature-x",
                "--worktree-ref",
                "main",
                "--restore-code",
                "--resume",
                "old",
                "--no-leader",
            ]),
            "new",
            false,
        );
        assert_eq!(
            as_strs(&out),
            vec!["--no-leader", "--resume", "new", "--fullscreen"]
        );
    }

    /// `--flag=value` spellings of the dropped one-shot flags.
    #[test]
    fn strips_eq_forms_of_one_shot_flags() {
        let out = build_screen_mode_relaunch_args(
            args(&[
                "echoagent",
                "--session-id=u1",
                "--worktree=wt",
                "--worktree-ref=main",
                "--ref=main",
                "--no-leader",
            ]),
            "new",
            false,
        );
        assert_eq!(
            as_strs(&out),
            vec!["--no-leader", "--resume", "new", "--fullscreen"]
        );
    }

    /// Bare `--worktree` (optional value omitted) is dropped without eating a
    /// following flag token.
    #[test]
    fn strips_bare_worktree_without_eating_next_flag() {
        let out = build_screen_mode_relaunch_args(
            args(&["echoagent", "--worktree", "--no-leader"]),
            "new",
            false,
        );
        assert_eq!(
            as_strs(&out),
            vec!["--no-leader", "--resume", "new", "--fullscreen"]
        );
    }

    #[test]
    fn strips_prior_minimal_and_resume() {
        let out = build_screen_mode_relaunch_args(
            args(&["echoagent", "--minimal", "--resume", "old", "--no-leader"]),
            "new",
            false,
        );
        assert_eq!(
            as_strs(&out),
            vec!["--no-leader", "--resume", "new", "--fullscreen"]
        );
        assert!(!as_strs(&out).iter().any(|s| s == "--minimal"));
    }

    /// A `--fullscreen` from a prior `/fullscreen` relaunch must not survive a
    /// `/minimal` rebuild: clap declares the two flags conflicting, so a stale
    /// `--fullscreen` next to the appended `--minimal` would kill the relaunch
    /// at arg parsing.
    #[test]
    fn strips_prior_fullscreen_flag() {
        let out = build_screen_mode_relaunch_args(
            args(&[
                "echoagent",
                "--fullscreen",
                "--resume",
                "old",
                "--no-leader",
            ]),
            "new",
            true,
        );
        assert_eq!(
            as_strs(&out),
            vec!["--no-leader", "--resume", "new", "--minimal"]
        );
        assert!(!as_strs(&out).iter().any(|s| s == "--fullscreen"));
    }

    #[test]
    fn strips_short_resume_and_continue() {
        let out = build_screen_mode_relaunch_args(
            args(&["echoagent", "-r", "old", "-c", "--no-leader"]),
            "sid",
            true,
        );
        // `-c` and `-r old` gone; `--minimal --resume sid` added; `--no-leader` kept.
        assert_eq!(
            as_strs(&out),
            vec!["--no-leader", "--resume", "sid", "--minimal"]
        );
    }

    #[test]
    fn strips_resume_equals_form() {
        let out = build_screen_mode_relaunch_args(
            args(&["echoagent", "--resume=old-id", "--no-leader"]),
            "sid",
            false,
        );
        assert_eq!(
            as_strs(&out),
            vec!["--no-leader", "--resume", "sid", "--fullscreen"]
        );
    }

    #[test]
    fn strips_positional_prompt() {
        let out = build_screen_mode_relaunch_args(
            args(&["echoagent", "--no-leader", "fix the bug"]),
            "sid",
            true,
        );
        assert_eq!(
            as_strs(&out),
            vec!["--no-leader", "--resume", "sid", "--minimal"]
        );
        assert!(!as_strs(&out).iter().any(|s| s.contains("fix")));
    }

    #[test]
    fn double_dash_and_following_positionals_dropped() {
        // `echoagent --no-leader -- "fix the bug"`: everything after `--` is the
        // prompt. The separator itself must go too, or the appended
        // `--resume <id>` would be parsed as positional prompt words.
        let out = build_screen_mode_relaunch_args(
            args(&["echoagent", "--no-leader", "--", "fix the bug"]),
            "sid",
            false,
        );
        assert_eq!(
            as_strs(&out),
            vec!["--no-leader", "--resume", "sid", "--fullscreen"]
        );
    }

    #[test]
    fn keeps_value_tokens_for_common_flags() {
        // Space-separated values must travel with their flags — not be treated
        // as the bare positional prompt (regression: relaunch argv drops flag values).
        let out = build_screen_mode_relaunch_args(
            args(&[
                "echoagent",
                "--model",
                "echo-agent-4",
                "--cwd",
                "/tmp/proj",
                "--leader-socket",
                "/tmp/leader.sock",
                "--debug-file",
                "/tmp/debug.log",
                "--no-leader",
                "fix the bug",
            ]),
            "sid",
            true,
        );
        assert_eq!(
            as_strs(&out),
            vec![
                "--model",
                "echo-agent-4",
                "--cwd",
                "/tmp/proj",
                "--leader-socket",
                "/tmp/leader.sock",
                "--debug-file",
                "/tmp/debug.log",
                "--no-leader",
                "--resume",
                "sid",
                "--minimal",
            ]
        );
        assert!(!as_strs(&out).iter().any(|s| s.contains("fix")));
    }

    #[test]
    fn keeps_equals_form_and_short_model_flag() {
        let out = build_screen_mode_relaunch_args(
            args(&[
                "echoagent",
                "-m",
                "echo-agent-4",
                "--cwd=/tmp/proj",
                "--no-leader",
            ]),
            "sid",
            false,
        );
        assert_eq!(
            as_strs(&out),
            vec![
                "-m",
                "echo-agent-4",
                "--cwd=/tmp/proj",
                "--no-leader",
                "--resume",
                "sid",
                "--fullscreen",
            ]
        );
    }

    #[test]
    fn boolean_flag_does_not_eat_following_positional() {
        // `--no-leader` is boolean; the bare word after it is the prompt and
        // must be dropped, not attached as a spurious value.
        let out = build_screen_mode_relaunch_args(
            args(&["echoagent", "--no-leader", "fix the bug"]),
            "sid",
            false,
        );
        assert_eq!(
            as_strs(&out),
            vec!["--no-leader", "--resume", "sid", "--fullscreen"]
        );
    }

    #[test]
    fn resume_without_value_then_flag_is_not_eaten() {
        // `echoagent --resume --no-leader` (resume most-recent; next token is a flag).
        let out = build_screen_mode_relaunch_args(
            args(&["echoagent", "--resume", "--no-leader"]),
            "sid",
            false,
        );
        assert_eq!(
            as_strs(&out),
            vec!["--no-leader", "--resume", "sid", "--fullscreen"]
        );
    }

    #[test]
    fn parse_screen_mode_values() {
        use super::super::ScreenMode;
        assert_eq!(
            parse_screen_mode(Some("minimal")),
            Some(ScreenMode::Minimal)
        );
        assert_eq!(
            parse_screen_mode(Some("fullscreen")),
            Some(ScreenMode::Fullscreen)
        );
        assert_eq!(
            parse_screen_mode(Some("full")),
            Some(ScreenMode::Fullscreen)
        );
        // Case / whitespace must match settings' case-insensitive path so a
        // hand-edited config.toml is not treated as unset at startup.
        assert_eq!(
            parse_screen_mode(Some("Minimal")),
            Some(ScreenMode::Minimal)
        );
        assert_eq!(
            parse_screen_mode(Some("  MINIMAL ")),
            Some(ScreenMode::Minimal)
        );
        assert_eq!(
            parse_screen_mode(Some("FULLSCREEN")),
            Some(ScreenMode::Fullscreen)
        );
        assert_eq!(
            parse_screen_mode(Some("Full")),
            Some(ScreenMode::Fullscreen)
        );
        assert_eq!(parse_screen_mode(Some("nope")), None);
        assert_eq!(parse_screen_mode(Some("inline")), None);
        assert_eq!(parse_screen_mode(Some("default")), None);
        assert_eq!(parse_screen_mode(Some("auto")), None);
        assert_eq!(parse_screen_mode(Some("")), None);
        assert_eq!(parse_screen_mode(Some("   ")), None);
        assert_eq!(parse_screen_mode(None), None);
    }

    #[test]
    fn take_env_override_consumes_the_variable() {
        // The override is one-shot: children of the relaunched process must not
        // inherit a forced screen mode. Sole test touching this env var.
        unsafe { std::env::set_var(ECHO_AGENT_SCREEN_MODE_ENV, "minimal") };
        assert_eq!(
            take_screen_mode_env_override(),
            Some(super::super::ScreenMode::Minimal)
        );
        assert!(
            std::env::var_os(ECHO_AGENT_SCREEN_MODE_ENV).is_none(),
            "env var must be removed after being read"
        );
        // Unparseable values are still removed (never leak to children).
        unsafe { std::env::set_var(ECHO_AGENT_SCREEN_MODE_ENV, "bogus") };
        assert_eq!(take_screen_mode_env_override(), None);
        assert!(std::env::var_os(ECHO_AGENT_SCREEN_MODE_ENV).is_none());
        // Absent stays absent.
        assert_eq!(take_screen_mode_env_override(), None);
    }

    #[test]
    fn resolve_forces_fullscreen_over_alt_screen_policy() {
        // `/fullscreen` must not reopen inline when env says fullscreen
        // but alt-screen policy would have chosen inline (Zellij, never, etc.).
        use super::super::ScreenMode;
        assert_eq!(
            resolve_screen_mode(
                Some(ScreenMode::Fullscreen),
                /*minimal*/ true,
                /*alt*/ false
            ),
            ScreenMode::Fullscreen
        );
        assert_eq!(
            resolve_screen_mode(Some(ScreenMode::Fullscreen), false, false),
            ScreenMode::Fullscreen
        );
        assert_eq!(
            resolve_screen_mode(Some(ScreenMode::Minimal), false, true),
            ScreenMode::Minimal
        );
    }

    #[test]
    fn resolve_without_env_follows_minimal_then_alt_screen() {
        use super::super::ScreenMode;
        assert_eq!(resolve_screen_mode(None, true, false), ScreenMode::Minimal);
        assert_eq!(
            resolve_screen_mode(None, false, true),
            ScreenMode::Fullscreen
        );
        assert_eq!(resolve_screen_mode(None, false, false), ScreenMode::Inline);
    }

    #[test]
    fn failed_relaunch_hint_includes_screen_mode_env() {
        // Recovery command must carry ECHO_AGENT_SCREEN_MODE so following the
        // hint after a failed `/fullscreen` does not reopen minimal/inline. The
        // explicit flag keeps the resume in the right mode if the env is dropped.
        assert_eq!(
            screen_mode_relaunch_resume_hint("abc-sid", false),
            "ECHO_AGENT_SCREEN_MODE=fullscreen echoagent --fullscreen --resume abc-sid"
        );
        assert_eq!(
            screen_mode_relaunch_resume_hint("abc-sid", true),
            "ECHO_AGENT_SCREEN_MODE=minimal echoagent --minimal --resume abc-sid"
        );
    }

    // ── effective_minimal_preference ─────────────────────────────────────

    #[test]
    fn preference_cli_flag_beats_config_and_legacy() {
        // `--minimal` wins over a config fullscreen and vice versa.
        assert_eq!(
            effective_minimal_preference(true, false, Some("fullscreen"), false),
            Some(true)
        );
        assert_eq!(
            effective_minimal_preference(false, true, Some("minimal"), false),
            Some(false)
        );
        // `--fullscreen` also beats the legacy pager.toml `[terminal] minimal`.
        assert_eq!(
            effective_minimal_preference(false, true, None, true),
            Some(false)
        );
    }

    #[test]
    fn preference_config_screen_mode_beats_legacy_pager_toml() {
        assert_eq!(
            effective_minimal_preference(false, false, Some("minimal"), false),
            Some(true)
        );
        assert_eq!(
            effective_minimal_preference(false, false, Some("fullscreen"), true),
            Some(false)
        );
        assert_eq!(
            effective_minimal_preference(false, false, Some("full"), true),
            Some(false)
        );
        // Case must match settings display/canonical path.
        assert_eq!(
            effective_minimal_preference(false, false, Some("Minimal"), false),
            Some(true)
        );
        assert_eq!(
            effective_minimal_preference(false, false, Some("FULLSCREEN"), true),
            Some(false)
        );
    }

    #[test]
    fn preference_unset_or_invalid_config_falls_back_to_legacy_or_none() {
        assert_eq!(
            effective_minimal_preference(false, false, None, true),
            Some(true)
        );
        // No sticky preference — caller may apply soft defaults (mouse-leak).
        assert_eq!(
            effective_minimal_preference(false, false, None, false),
            None
        );
        assert_eq!(
            effective_minimal_preference(false, false, Some("banana"), true),
            Some(true)
        );
        assert_eq!(
            effective_minimal_preference(false, false, Some("banana"), false),
            None
        );
        // Explicit fullscreen blocks soft defaults.
        assert_eq!(
            effective_minimal_preference(false, false, Some("fullscreen"), false),
            Some(false)
        );
    }
}
