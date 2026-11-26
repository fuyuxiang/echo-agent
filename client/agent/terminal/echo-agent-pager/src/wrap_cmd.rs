//! `echoagent wrap` — run any command in a local PTY that forwards its clipboard.
//!
//! Generalizes the `echoagent ssh` wrapper: spawns an arbitrary command inside a
//! local pseudo-terminal, intercepts OSC 52 clipboard escape sequences from
//! its output, and writes their payload to the local system clipboard. Useful
//! for containerized or remote shells (`docker exec`, `kubectl exec`, ...)
//! whose clipboard cannot otherwise reach the user — especially in terminals
//! that do not handle OSC 52 themselves (for example Apple Terminal). Also
//! stamps `LC_ECHO_AGENT_APPEARANCE` from the local OS theme so a remote
//! `theme = "auto"` can resolve over SSH + tmux.
//!
//! Resolvable programs spawn directly. On Unix, commands a direct spawn cannot
//! run — a single shell-quoted string (`echoagent wrap "mycli ssh host"`) or a shell
//! alias — are handed to `$SHELL -i -c` instead, so the user's own shell does
//! the word-splitting and alias expansion. The exec fallback (a non-TTY
//! session, or PTY setup failure) keeps the same route but drops `-i` to
//! avoid job-control noise without our PTY.
//!
//! The PTY/OSC 52/resize engine lives in [`crate::pty_wrap::run_wrapped_command`].

use anyhow::Result;

use crate::app::WrapArgs;

/// Run the `echoagent wrap` command.
///
/// On Unix interactive sessions the command runs inside a local PTY so its
/// OSC 52 clipboard sequences can be intercepted and written to the local
/// clipboard. Otherwise the command is executed directly (no wrapping).
pub fn run(args: &WrapArgs) -> Result<()> {
    // `command` is `required` in clap, so it always has at least one element.
    let program = args
        .command
        .first()
        .ok_or_else(|| anyhow::anyhow!("echoagent wrap: no command given"))?;

    // Unix: derive both spawn plans up front from one env snapshot so the PTY
    // attempt and its fallback route consistently. The wrapped run uses
    // `$SHELL -i` when routing through the shell (rc files load, aliases
    // expand — safe because it runs inside our PTY); the exec fallback drops
    // `-i` because an interactive shell without our PTY risks job-control
    // noise.
    #[cfg(unix)]
    let (wrapped, fallback) = {
        let shell = user_shell();
        let in_path = echo_agent_config::shell::is_command_available(program);
        (
            derive_spawn(&args.command, &shell, in_path, ShellMode::Interactive),
            derive_spawn(&args.command, &shell, in_path, ShellMode::Plain),
        )
    };
    #[cfg(not(unix))]
    let (wrapped, fallback) = {
        let direct = SpawnPlan {
            program: program.clone(),
            args: args.command[1..].to_vec(),
        };
        (direct.clone(), direct)
    };

    if should_wrap() {
        match crate::pty_wrap::run_wrapped_command(&wrapped.program, &wrapped.args) {
            Ok(code) => std::process::exit(code),
            Err(e) => {
                // PTY setup failed; keep the chosen route without our PTY so
                // the command still works (just without clipboard forwarding).
                eprintln!("echoagent wrap: wrapped mode failed, running without PTY wrapping: {e}");
                exec_command(&fallback.program, &fallback.args)
            }
        }
    } else {
        exec_command(&fallback.program, &fallback.args)
    }
}

/// The program and argv `echoagent wrap` will actually spawn.
#[derive(Clone)]
struct SpawnPlan {
    program: String,
    args: Vec<String>,
}

/// Whether a shell-routed command runs `$SHELL -i -c` or plain `$SHELL -c`.
#[cfg(unix)]
#[derive(Clone, Copy)]
enum ShellMode {
    /// PTY-wrapped run: rc files source and aliases expand.
    Interactive,
    /// Exec fallback without our PTY: plain `-c` avoids job-control noise.
    Plain,
}

/// Decide how to launch the wrapped command: direct spawn, or via the user's
/// shell when a direct spawn cannot work.
///
/// Pure so it is testable without the real environment: `program_in_path` is
/// the caller's `PATH` lookup result for `command[0]`.
#[cfg(unix)]
fn derive_spawn(
    command: &[String],
    shell: &str,
    program_in_path: bool,
    mode: ShellMode,
) -> SpawnPlan {
    let via_shell = |cmdline: String| {
        let args = match mode {
            ShellMode::Interactive => vec!["-i".to_string(), "-c".to_string(), cmdline],
            ShellMode::Plain => vec!["-c".to_string(), cmdline],
        };
        SpawnPlan {
            program: shell.to_string(),
            args,
        }
    };

    // A single argument containing whitespace is a shell-quoted command line
    // (`echoagent wrap "mycli ssh host"`), not a program name: hand it to the shell
    // verbatim so it does word-splitting, alias expansion, pipes, etc.
    if command.len() == 1 && command[0].contains(char::is_whitespace) {
        return via_shell(command[0].clone());
    }

    // A bare program name that PATH cannot resolve is usually a shell alias
    // (`alias mycli=remote`); only a shell can expand it. Explicit paths
    // (containing `/`) spawn directly so their errors stay precise, as do
    // empty and whitespace-containing first words — neither can be an alias
    // name, and an empty one (`echoagent wrap "$PROG" ...` with `$PROG` unset)
    // must keep failing fast instead of silently running the tail.
    if !command[0].is_empty()
        && !command[0].contains('/')
        && !command[0].contains(char::is_whitespace)
        && !program_in_path
    {
        return via_shell(join_command_line(command));
    }

    SpawnPlan {
        program: command[0].clone(),
        args: command[1..].to_vec(),
    }
}

/// Join argv back into one shell command line, leaving the first word bare:
/// shells only expand aliases on unquoted words, so quoting it would break
/// the very case this route exists for. Every following word is quoted.
#[cfg(unix)]
fn join_command_line(command: &[String]) -> String {
    let mut line = command[0].clone();
    for word in &command[1..] {
        line.push(' ');
        line.push_str(&quote_word(word));
    }
    line
}

/// Single-quote `word` for a POSIX shell. Always quotes: a hand-maintained
/// bare-word safe set rots per shell (zsh alone expands bare `=word`), and
/// only the shell ever reads the composed line.
#[cfg(unix)]
fn quote_word(word: &str) -> String {
    // Single quotes are literal in POSIX shells except `'` itself, which must
    // close the quote, escape, and reopen: `'` → `'\''`.
    format!("'{}'", word.replace('\'', "'\\''"))
}

/// The user's shell from `$SHELL` when it points at an existing file,
/// otherwise `/bin/sh`. `$SHELL` is preferred because the user typed the
/// wrapped command in their own shell's dialect (aliases live there);
/// deliberately not `echo_agent_config::shell::unix_shell_path`, which coerces
/// to bash/zsh and would drop fish/other-shell aliases.
#[cfg(unix)]
fn user_shell() -> String {
    resolve_shell(std::env::var("SHELL").ok().as_deref())
}

/// Pure core of [`user_shell`], split from the env read so tests never touch
/// the process environment. `shell` is the raw `$SHELL` value, if any.
#[cfg(unix)]
fn resolve_shell(shell: Option<&str>) -> String {
    match shell {
        Some(s) if !s.is_empty() && std::path::Path::new(s).is_file() => s.to_string(),
        _ => "/bin/sh".to_string(),
    }
}

/// Returns true when the command should be wrapped in a local PTY.
///
/// Unlike `echoagent ssh`, `echoagent wrap` does not gate on the terminal brand: the user
/// has explicitly asked to forward the clipboard, and interception works
/// regardless of whether the outer terminal supports OSC 52 (the payload is
/// written to the local clipboard directly).
///
/// It requires a platform `portable-pty` can drive — Unix (`openpty`) or
/// Windows (ConPTY) — and an interactive (TTY) session. Wrapping a
/// non-interactive pipe would make the child think it has a terminal and has no
/// clipboard destination anyway. On Windows the OSC 52 → clipboard bridge works
/// the same; only the live outer→inner resize is not forwarded (see
/// [`crate::pty_wrap::run_wrapped_command`]).
fn should_wrap() -> bool {
    // PTY wrapping requires native pseudo-terminal APIs (Unix openpty / Windows
    // ConPTY), both of which `portable-pty` supports.
    if !cfg!(any(unix, windows)) {
        return false;
    }

    use std::io::IsTerminal;
    std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
        && std::io::stderr().is_terminal()
}

/// Replace the current process with `program <args...>` (no PTY wrapping).
#[cfg(unix)]
fn exec_command(program: &str, args: &[String]) -> Result<()> {
    use std::os::unix::process::CommandExt;

    let err = std::process::Command::new(program).args(args).exec();

    // exec() only returns on error.
    Err(anyhow::anyhow!("failed to exec {program}: {err}"))
}

/// On non-Unix platforms, spawn and wait.
#[cfg(not(unix))]
fn exec_command(program: &str, args: &[String]) -> Result<()> {
    let status = std::process::Command::new(program)
        .args(args)
        .status()
        .map_err(|e| anyhow::anyhow!("failed to run {program}: {e}"))?;

    std::process::exit(status.code().unwrap_or(1));
}

#[cfg(all(test, unix))]
#[path = "wrap_cmd_tests.rs"]
mod tests;
