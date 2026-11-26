//! Runs a user's `command` row: payload in on stdin, the first
//! [`MAX_STATUS_LINE_LINES`] lines back, in its own process group.

use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use echo_agent_status_line::StatusLineContext;

use crate::views::status_line::{MAX_STATUS_LINE_LINES, RowSize};

use super::{RunId, RunOutcome, StatusLineRun, metrics};

pub(crate) const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

const MAX_COMMAND_OUTPUT_BYTES: u64 = 64 * 1024;

impl StatusLineRun {
    /// The id rides back with the row so a late result can be matched to the
    /// run that asked for it.
    pub(crate) async fn execute(self) -> (RunId, RunOutcome) {
        let outcome =
            run_status_command(&self.command, &self.ctx, self.term_size, COMMAND_TIMEOUT).await;
        (self.id, outcome)
    }
}

async fn run_status_command(
    command: &str,
    ctx: &StatusLineContext,
    term_size: RowSize,
    timeout: Duration,
) -> RunOutcome {
    let started = Instant::now();
    let result = run_command(command, ctx, term_size, timeout).await;
    let elapsed_ms = started.elapsed().as_millis() as u64;
    match result {
        Ok(line) => {
            metrics::global().record_ok(elapsed_ms);
            RunOutcome::Output(line)
        }
        Err(error) => {
            if matches!(error, RunError::TimedOut) {
                metrics::global().record_timed_out(elapsed_ms);
            } else {
                metrics::global().record_failed(elapsed_ms);
            }
            RunOutcome::Failed {
                text: format!("[status line: {error}]"),
                error: error.to_string(),
            }
        }
    }
}

#[derive(Debug)]
enum RunError {
    Spawn(std::io::Error),
    Wait(std::io::Error),
    Json(serde_json::Error),
    TimedOut,
    Exit(Option<i32>),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::Spawn(e) => write!(f, "could not start the script: {e}"),
            RunError::Wait(e) => write!(f, "could not wait for the script: {e}"),
            // EchoAgent's own bug rather than the script's, so it says whose it is.
            RunError::Json(e) => write!(f, "could not encode EchoAgent's payload: {e}"),
            RunError::TimedOut => f.write_str("timed out"),
            RunError::Exit(Some(code)) => write!(f, "exit {code}"),
            RunError::Exit(None) => f.write_str("killed by signal"),
        }
    }
}

/// Kills the run's process group unless the group is already empty. A group
/// with a surviving member cannot have its id recycled, so signalling one is
/// safe; an empty group is disarmed instead, which is the discipline `enroll`
/// asks for to keep a reaped leader's id from being signalled later.
struct GroupGuard(Option<std::sync::Arc<echo_agent_tty_utils::ProcessGroup>>);

impl GroupGuard {
    /// Whether anything is left to kill. `None` where the platform cannot say,
    /// which counts as alive.
    fn holds_survivors(&self) -> bool {
        self.0
            .as_ref()
            .is_none_or(|group| group.has_live_members() != Some(false))
    }

    fn disarm(mut self) {
        self.0 = None;
    }
}

impl Drop for GroupGuard {
    fn drop(&mut self) {
        if let Some(group) = &self.0 {
            let _ = group.kill();
        }
    }
}

/// Works the child's three pipes together and waits for it, returning its
/// status and the bytes stdout produced.
///
/// The wait races the read rather than following it. Stdout closes only when
/// every writer does, so a script that backgrounds a job without redirecting it
/// would otherwise hold the row until the deadline: the shell is long gone and
/// a grandchild owns the pipe.
async fn pump(
    child: &mut tokio::process::Child,
    json: &str,
) -> Result<(Option<std::process::ExitStatus>, Vec<u8>), RunError> {
    let mut stdin = child.stdin.take();
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let mut buf = Vec::new();

    let status = {
        // Alongside the reads: a script that fills its stdout before reading stdin
        // would block a write that ran first. Dropping the handle gives it EOF.
        let write_in = async {
            let Some(mut stdin) = stdin.take() else {
                return Ok(());
            };
            let written = stdin.write_all(json.as_bytes()).await;
            drop(stdin);
            match written {
                Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => Err(e),
                _ => Ok(()),
            }
        };
        let read_out = async {
            match stdout.as_mut() {
                // One byte past the cap, so a run that lands exactly on it is not
                // reported as a runaway.
                Some(out) => {
                    out.take(MAX_COMMAND_OUTPUT_BYTES + 1)
                        .read_to_end(&mut buf)
                        .await
                }
                None => Ok(0),
            }
        };
        // First 64 KiB logged, rest dropped: the pipe stays open and empty while the
        // script runs. Closing it at the cap is `SIGPIPE`; leaving it full blocks.
        let drain_err = async {
            let Some(mut stderr) = stderr.take() else {
                return;
            };
            let mut errors = Vec::new();
            let _ = (&mut stderr)
                .take(MAX_COMMAND_OUTPUT_BYTES)
                .read_to_end(&mut errors)
                .await;
            if !errors.is_empty() {
                tracing::debug!(
                    stderr = %String::from_utf8_lossy(&errors).trim_end(),
                    "status_line: the script wrote to stderr"
                );
            }
            let _ = tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await;
        };

        tokio::pin!(read_out, drain_err, write_in);
        let mut writing = true;
        let mut draining = true;
        let mut reading = true;
        loop {
            tokio::select! {
                biased;
                // Each flag retires its branch: `select!` resumes a pinned future
                // rather than restarting it, and polling a finished one panics.
                written = &mut write_in, if writing => {
                    writing = false;
                    if let Err(error) = written {
                        // Logged, not returned: the script may already have printed a
                        // row. Same rule as a read error and a non-zero exit.
                        tracing::debug!(%error, "status_line: sending the payload failed");
                    }
                }
                () = &mut drain_err, if draining => draining = false,
                read = &mut read_out, if reading => {
                    reading = false;
                    match read {
                        // The cap ends the run: a script still writing will never
                        // exit, so there is no status coming to wait for.
                        Ok(read) if read as u64 > MAX_COMMAND_OUTPUT_BYTES => break None,
                        Ok(_) => {}
                        Err(error) => {
                            // What arrived still paints; without this a truncated row
                            // and a short one are indistinguishable.
                            tracing::debug!(%error, "status_line: reading the script's output failed");
                        }
                    }
                }
                waited = child.wait() => break Some(waited.map_err(RunError::Wait)?),
            }
        }
    };

    if buf.len() as u64 > MAX_COMMAND_OUTPUT_BYTES {
        buf.truncate(MAX_COMMAND_OUTPUT_BYTES as usize);
        return Ok((None, buf));
    }
    Ok((status, buf))
}

/// Whether the kernel refused to execute the file at all, which is what an
/// executable script with no `#!` gets. Windows has no such answer.
fn is_shell_script(error: &std::io::Error) -> bool {
    #[cfg(unix)]
    {
        error.raw_os_error() == Some(libc::ENOEXEC)
    }
    #[cfg(not(unix))]
    {
        let _ = error;
        false
    }
}

async fn run_command(
    command: &str,
    ctx: &StatusLineContext,
    RowSize { cols, lines }: RowSize,
    timeout: Duration,
) -> Result<String, RunError> {
    use std::process::Stdio;

    use tokio::process::Command;

    // Newline-terminated so a script reading with `read -r` gets a complete
    // line rather than blocking for a terminator that never comes.
    let mut json = serde_json::to_string(ctx).map_err(RunError::Json)?;
    json.push('\n');

    let expanded = match command.strip_prefix("~/") {
        Some(rest) => match dirs::home_dir() {
            Some(home) => format!("{}/{rest}", home.display()),
            None => command.to_string(),
        },
        None => command.to_string(),
    };

    // `tokio::fs`, since `Path::is_dir` stats on the runtime thread. A deleted
    // directory fails here, and so does one whose name is not UTF-8, because the
    // payload carries the lossy form JSON can hold.
    let repo_root = ctx.workspace.repo_root.clone().unwrap_or_default();
    let mut local_cwd = None;
    for dir in [ctx.cwd.as_str(), repo_root.as_str()] {
        if !dir.is_empty()
            && tokio::fs::metadata(dir)
                .await
                .is_ok_and(|meta| meta.is_dir())
        {
            local_cwd = Some(dir.to_string());
            break;
        }
    }

    let configure = |cmd: &mut Command| {
        cmd.env_remove("BASH_ENV")
            .env_remove("ENV")
            .envs(echo_agent_tty_utils::pager_env())
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("COLUMNS", cols.to_string())
            .env("LINES", lines.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Piped, not inherited, or a script's stderr scribbles over the alternate
            // screen. Logged rather than painted, so `--debug` is where an author sees it.
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(cwd) = local_cwd.as_deref() {
            cmd.current_dir(cwd);
        }
        echo_agent_tty_utils::detach_command(cmd);
    };

    let mut direct = Command::new(&expanded);
    configure(&mut direct);
    #[allow(clippy::disallowed_methods)] // enrolled in the session scope below
    let mut child = match direct.spawn() {
        Ok(child) => child,
        // A shell line rather than a path, or a file `sh` may still read. Every other
        // failure surfaces: a permission error under `sh` reports a bogus `exit 126`.
        Err(error) if error.kind() != std::io::ErrorKind::NotFound && !is_shell_script(&error) => {
            return Err(RunError::Spawn(error));
        }
        Err(_) => {
            // Windows has no `sh` on PATH by default, and where Git Bash does
            // supply one it mangles native paths.
            #[cfg(unix)]
            let mut shell = {
                let mut c = Command::new("sh");
                c.args(["-c", expanded.as_str()]);
                c
            };
            // Which shell to use is `shell_command_argv`'s decision; the env it
            // sets is table-tested there over every Windows variant. What is
            // left here is the spawn, which no test on this platform reaches.
            #[cfg(not(unix))]
            let mut shell = {
                let inv = echo_agent_config::shell::shell_command_argv(&expanded);
                let mut c = Command::new(&inv.program);
                c.args(&inv.args).envs(inv.env);
                c
            };
            configure(&mut shell);
            shell.spawn().map_err(RunError::Spawn)?
        }
    };
    let group = match echo_agent_tty_utils::global_process_scope().enroll(&child) {
        Ok(group) => Some(group),
        Err(error) => {
            // No group means no teardown: a timeout kills the shell and
            // orphans anything it backgrounded.
            tracing::warn!(%error, "status_line: run left unenrolled, descendants may leak");
            None
        }
    };

    let guard = GroupGuard(group);
    let (status, out) = match tokio::time::timeout(timeout, pump(&mut child, &json)).await {
        Ok(pumped) => pumped?,
        Err(_) => return Err(RunError::TimedOut),
    };
    // An empty group is disarmed; one the script left populated is killed by
    // the guard's drop, so a row that runs three times a second cannot leak a
    // process tree per run.
    if status.is_some() && !guard.holds_survivors() {
        guard.disarm();
    }
    // Printed output takes precedence over the exit code: `printf …; [[ -n
    // $dirty ]]` is an ordinary way to end a shell script.
    if let Some(status) = status
        && !status.success()
        && out.is_empty()
    {
        return Err(RunError::Exit(status.code()));
    }

    // Only the first few lines are painted, so a runaway script must not carry
    // 64 KiB of unused lines into the row's state.
    let text = String::from_utf8_lossy(&out);
    let kept: Vec<&str> = text
        .trim_end_matches('\n')
        .split('\n')
        .take(MAX_STATUS_LINE_LINES as usize)
        .collect();
    Ok(kept.join("\n"))
}

#[cfg(all(test, unix))]
#[path = "command_tests.rs"]
mod tests;
