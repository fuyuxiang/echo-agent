use std::io::{ErrorKind, Read, Write};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// Why a git probe did not yield usable output.
pub(crate) enum ProbeError {
    /// The process could not be spawned or timed out.
    DidNotRun(std::io::Error),
    /// The process ran but exited non-zero; carries stderr for the caller's error.
    Failed { stderr: Vec<u8> },
}

/// Run a git probe `command` in `dir` with `stdin`, warning on failure. Shared
/// run→check-status→warn skeleton behind the safety/reclaim git probes.
pub(crate) fn run_probe(
    dir: &Path,
    command: Command,
    stdin: Vec<u8>,
    what: &str,
) -> Result<Output, ProbeError> {
    match run_with_timeout(command, stdin, PROBE_TIMEOUT) {
        Ok(output) if output.status.success() => Ok(output),
        Ok(output) => {
            tracing::warn!(
                path = %dir.display(),
                what,
                code = ?output.status.code(),
                stderr = %String::from_utf8_lossy(&output.stderr),
                "git probe exited non-zero"
            );
            Err(ProbeError::Failed {
                stderr: output.stderr,
            })
        }
        Err(error) => {
            tracing::warn!(path = %dir.display(), what, %error, "git probe failed to run");
            Err(ProbeError::DidNotRun(error))
        }
    }
}

/// One newline-terminated OID per line, capacity pre-sized for `rev-list --stdin`.
pub(crate) fn oids_to_stdin<I>(ids: I) -> Vec<u8>
where
    I: IntoIterator<Item = gix::ObjectId>,
    I::IntoIter: ExactSizeIterator,
{
    const LINE: usize = gix::hash::Kind::Sha1.len_in_hex() + 1;
    let ids = ids.into_iter();
    let mut buf = Vec::with_capacity(ids.len() * LINE);
    for id in ids {
        let _ = writeln!(buf, "{id}");
    }
    buf
}

const INHERITED_GIT_ENVIRONMENT: &[&str] = &[
    "GIT_LITERAL_PATHSPECS",
    "GIT_GLOB_PATHSPECS",
    "GIT_NOGLOB_PATHSPECS",
    "GIT_ICASE_PATHSPECS",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_REPLACE_REF_BASE",
    "GIT_SHALLOW_FILE",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG_NOSYSTEM",
    "GIT_CEILING_DIRECTORIES",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    "GIT_QUARANTINE_PATH",
];

pub(crate) fn forget_inherited_git_environment(command: &mut Command) {
    for name in INHERITED_GIT_ENVIRONMENT {
        command.env_remove(name);
    }
    for (name, _) in std::env::vars_os().filter(|(name, _)| {
        let name = name.to_string_lossy();
        name.starts_with("GIT_CONFIG_KEY_") || name.starts_with("GIT_CONFIG_VALUE_")
    }) {
        command.env_remove(name);
    }
}

const POLL_INTERVAL: Duration = Duration::from_millis(20);

const DRAIN_GRACE: Duration = Duration::from_secs(5);

#[derive(Clone, Copy)]
enum Stream {
    Stdout,
    Stderr,
}

fn read_pipe<R: Read + Send + 'static>(
    stream: Stream,
    pipe: Option<R>,
    sent: &std::sync::mpsc::Sender<(Stream, Vec<u8>)>,
) -> usize {
    let Some(mut pipe) = pipe else {
        return 0;
    };
    let sent = sent.clone();
    std::thread::spawn(move || {
        let mut read = Vec::new();
        let _ = pipe.read_to_end(&mut read);
        let _ = sent.send((stream, read));
    });
    1
}

#[cfg(unix)]
fn own_process_group(child: &std::process::Child) -> Option<libc::pid_t> {
    let pid = libc::pid_t::try_from(child.id()).ok()?;
    // SAFETY: reads the group of a child this process started.
    (unsafe { libc::getpgid(pid) } == pid).then_some(pid)
}

#[cfg(not(unix))]
fn own_process_group(_child: &std::process::Child) -> Option<i32> {
    None
}

#[cfg(unix)]
fn kill_group(group: libc::pid_t) -> std::io::Result<()> {
    // SAFETY: a signal to a group this process started, with nothing borrowed.
    if unsafe { libc::kill(-group, libc::SIGKILL) } == -1 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error);
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn kill_group(_group: i32) -> std::io::Result<()> {
    Ok(())
}

pub(crate) fn kill_process_group(child: &mut std::process::Child) -> std::io::Result<()> {
    match own_process_group(child) {
        Some(group) => kill_group(group),
        None => child.kill(),
    }
}

pub(crate) fn run_with_timeout(
    mut command: Command,
    input: Vec<u8>,
    timeout: Duration,
) -> std::io::Result<Output> {
    let stdin = if input.is_empty() {
        Stdio::null()
    } else {
        Stdio::piped()
    };
    command
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // spawn (not output) so the run can be bounded: git_command() already
    // detached the child, and the timeout path below kills its process group.
    #[allow(clippy::disallowed_methods)]
    let mut child = command.spawn()?;
    let writer = child.stdin.take().map(|mut pipe| {
        std::thread::spawn(move || {
            let _ = pipe.write_all(&input);
        })
    });
    let (sent, drained) = std::sync::mpsc::channel();
    let mut reading = 0;
    reading += read_pipe(Stream::Stdout, child.stdout.take(), &sent);
    reading += read_pipe(Stream::Stderr, child.stderr.take(), &sent);
    let deadline = Instant::now() + timeout;
    let mut failed = None;
    let mut group = None;
    loop {
        group = group.or_else(|| own_process_group(&child));
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(error) => {
                let _ = kill_process_group(&mut child);
                let _ = child.wait();
                failed = Some(error);
                break;
            }
        }
        if Instant::now() >= deadline {
            if let Err(error) = kill_process_group(&mut child) {
                tracing::warn!(%error, "failed to kill timed-out probe group");
            }
            if let Err(error) = child.wait() {
                tracing::warn!(%error, "failed to reap the timed-out probe");
            }
            failed = Some(std::io::Error::new(
                ErrorKind::TimedOut,
                format!("probe did not finish within {timeout:?}"),
            ));
            break;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    if let Some(error) = failed {
        return Err(error);
    }
    drop(writer);
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let (mut read_stdout, drained_by) = (false, Instant::now() + DRAIN_GRACE);
    for _ in 0..reading {
        match drained.recv_timeout(drained_by.saturating_duration_since(Instant::now())) {
            Ok((Stream::Stdout, read)) => (stdout, read_stdout) = (read, true),
            Ok((Stream::Stderr, read)) => stderr = read,
            Err(_) => {
                // The child exited but a grandchild still holds the pipes.
                // Signalling the pgid is safe: that live grandchild keeps the
                // group non-empty, so the group id cannot have been recycled.
                if let Some(group) = group
                    && let Err(error) = kill_group(group)
                {
                    tracing::warn!(%error, "failed to kill process group still holding the probe's pipes");
                }
                if !read_stdout {
                    return Err(std::io::Error::new(
                        ErrorKind::TimedOut,
                        format!("the probe's output did not drain within {DRAIN_GRACE:?}"),
                    ));
                }
                tracing::warn!(
                    ?DRAIN_GRACE,
                    "the probe's stderr did not drain; reporting none"
                );
                break;
            }
        }
    }
    Ok(Output {
        status: child.wait()?,
        stdout,
        stderr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn a_probe_that_times_out_takes_what_it_started_with_it() {
        let scratch = tempfile::tempdir().unwrap();
        let marker = scratch.path().join("outlived-the-probe");
        let mut command = Command::new("sh");
        echo_agent_tty_utils::detach_std_command(&mut command);
        command.arg("-c").arg(format!(
            "(sleep 1 && touch '{}') & sleep 30",
            marker.display()
        ));

        let error = run_with_timeout(command, Vec::new(), Duration::from_millis(200)).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::TimedOut);
        std::thread::sleep(Duration::from_secs(3));
        assert!(!marker.exists(), "something the probe started outlived it");
    }

    #[cfg(unix)]
    #[test]
    fn the_ambient_git_environment_does_not_reach_a_command() {
        let mut command = Command::new("sh");
        command
            .env("GIT_CONFIG_GLOBAL", "/somewhere/else")
            .env("GIT_DIR", "/somewhere/else")
            .arg("-c")
            .arg("echo \"[$GIT_CONFIG_GLOBAL][$GIT_DIR]\"");
        forget_inherited_git_environment(&mut command);

        let output = run_with_timeout(command, Vec::new(), Duration::from_secs(30)).unwrap();

        assert_eq!(output.stdout, b"[][]\n");
    }

    #[cfg(unix)]
    #[test]
    fn output_stands_when_only_stderr_is_still_held() {
        let mut command = Command::new("sh");
        echo_agent_tty_utils::detach_std_command(&mut command);
        command
            .arg("-c")
            .arg("echo hello; { sleep 30; } >/dev/null & exit 0");

        let start = Instant::now();
        let output = run_with_timeout(command, Vec::new(), Duration::from_secs(300)).unwrap();

        assert_eq!(output.stdout, b"hello\n");
        assert!(output.stderr.is_empty());
        assert!(
            start.elapsed() < DRAIN_GRACE * 2,
            "waited past the grace: {:?}",
            start.elapsed()
        );
    }

    #[cfg(unix)]
    #[test]
    fn output_a_lingering_child_still_holds_does_not_wait_forever() {
        let mut command = Command::new("sh");
        echo_agent_tty_utils::detach_std_command(&mut command);
        command.arg("-c").arg("sleep 30 & exit 0");

        let start = Instant::now();
        let error = run_with_timeout(command, Vec::new(), Duration::from_secs(300)).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::TimedOut);
        assert!(
            start.elapsed() < DRAIN_GRACE * 2,
            "the read waited past the grace: {:?}",
            start.elapsed()
        );
    }
}
