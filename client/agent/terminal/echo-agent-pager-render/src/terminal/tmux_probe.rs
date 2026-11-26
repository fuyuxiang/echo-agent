//! Shared tmux command protocol and result parsing.

use std::process::{Command, Stdio};
use std::time::Duration;

const TMUX_QUERY_TIMEOUT: Duration = Duration::from_secs(2);
/// After the leader exits, allow this much additional time for process-group
/// teardown and concurrent pipe drains so a near-deadline success is not turned
/// into a drain timeout. The main process wait still uses only
/// [`TMUX_QUERY_TIMEOUT`].
const POST_EXIT_CLEANUP_GRACE: Duration = Duration::from_millis(300);
/// How long a signalled process group may take to empty before it is killed.
const GROUP_EXIT_GRACE: Duration = Duration::from_millis(100);
const GROUP_EXIT_POLL: Duration = Duration::from_millis(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TmuxCommand<'a> {
    Version,
    OptionValue(&'a str),
    OptionSupport(&'a str),
    ControlMode,
    ClientFeatures,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TmuxCommandOutput {
    status_success: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

trait TmuxCommandRunner {
    fn run(&self, command: TmuxCommand<'_>) -> Result<TmuxCommandOutput, String>;
}

struct LiveTmuxCommandRunner;

impl TmuxCommandRunner for LiveTmuxCommandRunner {
    fn run(&self, command: TmuxCommand<'_>) -> Result<TmuxCommandOutput, String> {
        run_tmux_bounded(command, TMUX_QUERY_TIMEOUT)
    }
}

fn run_tmux_bounded(
    command: TmuxCommand<'_>,
    timeout: Duration,
) -> Result<TmuxCommandOutput, String> {
    let mut command = build_tmux_command(command);
    #[allow(clippy::disallowed_methods)] // bounded probe, waited on with a timeout
    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to run tmux: {error}"))?;
    let group = echo_agent_tty_utils::ProcessGroup::new()
        .and_then(|mut group| {
            group.attach_std(&child)?;
            Ok(group)
        })
        .map_err(|error| {
            let _ = child.kill();
            let _ = child.wait();
            format!("failed to own tmux process tree: {error}")
        })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "tmux stdout pipe was not captured".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "tmux stderr pipe was not captured".to_owned())?;
    let stdout = spawn_pipe_drain(stdout, "stdout");
    let stderr = spawn_pipe_drain(stderr, "stderr");
    let deadline = std::time::Instant::now() + timeout;

    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(15));
            }
            Ok(None) => {
                terminate_tmux_tree(&group, &mut child);
                return Err(format!("tmux query timed out after {timeout:?}"));
            }
            Err(error) => {
                terminate_tmux_tree(&group, &mut child);
                return Err(format!("failed to wait for tmux: {error}"));
            }
        }
    };

    // The leader may be reaped while descendants still exist or hold pipes.
    // Use a fresh post-exit bound so near-deadline success still drains; the
    // main process deadline is not extended for hung leaders.
    let cleanup_deadline = std::time::Instant::now() + POST_EXIT_CLEANUP_GRACE;
    terminate_owned_group(&group);
    let stdout = recv_pipe_drain(stdout, cleanup_deadline, "stdout")?;
    let stderr = recv_pipe_drain(stderr, cleanup_deadline, "stderr")?;
    Ok(TmuxCommandOutput {
        status_success: status.success(),
        stdout,
        stderr,
    })
}

fn spawn_pipe_drain(
    mut pipe: impl std::io::Read + Send + 'static,
    label: &'static str,
) -> std::sync::mpsc::Receiver<Result<Vec<u8>, String>> {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut output = Vec::new();
        let result = pipe
            .read_to_end(&mut output)
            .map(|_| output)
            .map_err(|error| format!("failed to read tmux {label}: {error}"));
        let _ = sender.send(result);
    });
    receiver
}

fn recv_pipe_drain(
    receiver: std::sync::mpsc::Receiver<Result<Vec<u8>, String>>,
    deadline: std::time::Instant,
    label: &'static str,
) -> Result<Vec<u8>, String> {
    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
    receiver
        .recv_timeout(remaining)
        .map_err(|_| format!("tmux {label} did not close before the query deadline"))?
}

fn terminate_tmux_tree(
    group: &echo_agent_tty_utils::ProcessGroup,
    child: &mut std::process::Child,
) {
    terminate_owned_group(group);
    let _ = child.wait();
}

/// SIGTERM the group, then escalate to SIGKILL only if it outlives the grace.
///
/// Callers reach this with the leader already reaped, so the group is usually
/// empty on the first check.
fn terminate_owned_group(group: &echo_agent_tty_utils::ProcessGroup) {
    let _ = group.terminate();
    let deadline = std::time::Instant::now() + GROUP_EXIT_GRACE;
    loop {
        if group.has_live_members() == Some(false) {
            // `return`, not `break`: the reaped leader's pid may already
            // belong to an unrelated group, so an empty group gets no SIGKILL.
            return;
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(GROUP_EXIT_POLL);
    }
    let _ = group.kill();
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TmuxQueryResult<T> {
    Available(T),
    Unsupported,
    Unavailable,
    Error(String),
}

impl<T> TmuxQueryResult<T> {
    pub fn into_option(self) -> Option<T> {
        match self {
            Self::Available(value) => Some(value),
            Self::Unsupported | Self::Unavailable | Self::Error(_) => None,
        }
    }
}

pub fn query_version() -> TmuxQueryResult<String> {
    query_version_with(&LiveTmuxCommandRunner)
}

fn query_version_with(runner: &dyn TmuxCommandRunner) -> TmuxQueryResult<String> {
    parse_value(runner.run(TmuxCommand::Version))
}

pub fn query_option(option: &str) -> TmuxQueryResult<String> {
    query_option_with(&LiveTmuxCommandRunner, option)
}

fn query_option_with(runner: &dyn TmuxCommandRunner, option: &str) -> TmuxQueryResult<String> {
    parse_value(runner.run(TmuxCommand::OptionValue(option)))
}

pub fn query_option_support(option: &str) -> TmuxQueryResult<()> {
    query_option_support_with(&LiveTmuxCommandRunner, option)
}

fn query_option_support_with(runner: &dyn TmuxCommandRunner, option: &str) -> TmuxQueryResult<()> {
    match runner.run(TmuxCommand::OptionSupport(option)) {
        Ok(output) if output.status_success => TmuxQueryResult::Available(()),
        Ok(output) if stderr_identifies_unknown_option(&output.stderr, option) => {
            TmuxQueryResult::Unsupported
        }
        Ok(_) => TmuxQueryResult::Unavailable,
        Err(error) => TmuxQueryResult::Error(error),
    }
}

/// The attached client's resolved terminal features, as a comma-separated list
/// (`RGB`, `clipboard`, `focus`, …).
///
/// tmux resolves this once per client at attach time from the outer terminal's
/// terminfo plus `terminal-features` / `terminal-overrides`, and it decides
/// whether 24-bit SGR survives the multiplexer. `COLORTERM` inside the pane
/// describes only what the pane's program emits, so it cannot answer that.
///
/// Empty output means the answer is unknown rather than negative: tmux before
/// 3.2 has no `terminal-features` and renders the unknown format as an empty
/// string, and a server with no attached client has nothing to report.
pub fn query_client_features() -> TmuxQueryResult<String> {
    query_client_features_with(&LiveTmuxCommandRunner)
}

fn query_client_features_with(runner: &dyn TmuxCommandRunner) -> TmuxQueryResult<String> {
    parse_value(runner.run(TmuxCommand::ClientFeatures))
}

pub fn query_control_mode() -> TmuxQueryResult<bool> {
    query_control_mode_with(&LiveTmuxCommandRunner)
}

fn query_control_mode_with(runner: &dyn TmuxCommandRunner) -> TmuxQueryResult<bool> {
    match runner.run(TmuxCommand::ControlMode) {
        Ok(output) if output.status_success => TmuxQueryResult::Available(
            String::from_utf8_lossy(&output.stdout).contains("control-mode"),
        ),
        Ok(_) => TmuxQueryResult::Unavailable,
        Err(error) => TmuxQueryResult::Error(error),
    }
}

fn build_tmux_command(command: TmuxCommand<'_>) -> Command {
    let mut cmd = Command::new("tmux");
    match command {
        TmuxCommand::Version => {
            cmd.arg("-V").stdout(Stdio::piped()).stderr(Stdio::piped());
        }
        TmuxCommand::OptionValue(option) => {
            cmd.args(["show-option", "-gqv", option])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
        }
        TmuxCommand::OptionSupport(option) => {
            cmd.args(["show-option", "-gv", option])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
        }
        TmuxCommand::ControlMode => {
            cmd.args(["display-message", "-p", "#{client_flags}"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
        }
        TmuxCommand::ClientFeatures => {
            cmd.args(["display-message", "-p", "#{client_termfeatures}"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
        }
    }
    cmd.stdin(Stdio::null())
        .envs(echo_agent_tty_utils::pager_env());
    echo_agent_tty_utils::detach_std_command(&mut cmd);
    cmd
}

fn parse_value(output: Result<TmuxCommandOutput, String>) -> TmuxQueryResult<String> {
    match output {
        Ok(output) if output.status_success => {
            let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if value.is_empty() {
                TmuxQueryResult::Unavailable
            } else {
                TmuxQueryResult::Available(value)
            }
        }
        Ok(_) => TmuxQueryResult::Unavailable,
        Err(error) => TmuxQueryResult::Error(error),
    }
}

fn stderr_identifies_unknown_option(stderr: &[u8], option: &str) -> bool {
    let invalid = format!("invalid option: {option}");
    let unknown = format!("unknown option: {option}");
    String::from_utf8_lossy(stderr)
        .lines()
        .any(|line| matches!(line.trim(), value if value == invalid || value == unknown))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::ffi::{OsStr, OsString};

    use super::*;

    struct FakeRunner {
        output: Result<TmuxCommandOutput, String>,
        calls: RefCell<Vec<String>>,
    }

    impl FakeRunner {
        fn output(status_success: bool, stdout: &[u8], stderr: &[u8]) -> Self {
            Self {
                output: Ok(TmuxCommandOutput {
                    status_success,
                    stdout: stdout.to_vec(),
                    stderr: stderr.to_vec(),
                }),
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl TmuxCommandRunner for FakeRunner {
        fn run(&self, command: TmuxCommand<'_>) -> Result<TmuxCommandOutput, String> {
            self.calls.borrow_mut().push(match command {
                TmuxCommand::Version => "version".to_owned(),
                TmuxCommand::OptionValue(option) => format!("value:{option}"),
                TmuxCommand::OptionSupport(option) => format!("support:{option}"),
                TmuxCommand::ControlMode => "control-mode".to_owned(),
                TmuxCommand::ClientFeatures => "client-features".to_owned(),
            });
            self.output.clone()
        }
    }

    #[test]
    fn command_protocol_uses_exact_argv_and_pager_env() {
        let cases = [
            (TmuxCommand::Version, vec!["-V"]),
            (
                TmuxCommand::OptionValue("set-clipboard"),
                vec!["show-option", "-gqv", "set-clipboard"],
            ),
            (
                TmuxCommand::OptionSupport("allow-passthrough"),
                vec!["show-option", "-gv", "allow-passthrough"],
            ),
            (
                TmuxCommand::ControlMode,
                vec!["display-message", "-p", "#{client_flags}"],
            ),
            (
                TmuxCommand::ClientFeatures,
                vec!["display-message", "-p", "#{client_termfeatures}"],
            ),
        ];
        for (request, args) in cases {
            let cmd = build_tmux_command(request);
            assert_eq!(cmd.get_program(), OsStr::new("tmux"));
            assert_eq!(cmd.get_args().collect::<Vec<_>>(), args);
            let actual: HashMap<OsString, Option<OsString>> = cmd
                .get_envs()
                .map(|(key, value)| (key.to_owned(), value.map(OsStr::to_owned)))
                .collect();
            let expected: HashMap<OsString, Option<OsString>> = echo_agent_tty_utils::pager_env()
                .into_iter()
                .map(|(key, value)| (key.into(), Some(value.into())))
                .collect();
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn value_and_control_queries_use_the_injected_runner() {
        let runner = FakeRunner::output(true, b" on\n", b"");
        assert_eq!(
            query_option_with(&runner, "set-clipboard"),
            TmuxQueryResult::Available("on".to_owned())
        );
        assert_eq!(runner.calls.into_inner(), ["value:set-clipboard"]);

        let runner = FakeRunner::output(true, b"control-mode,utf8\n", b"");
        assert_eq!(
            query_control_mode_with(&runner),
            TmuxQueryResult::Available(true)
        );
        assert_eq!(runner.calls.into_inner(), ["control-mode"]);
    }

    #[test]
    fn nonzero_and_execution_failure_remain_fail_open_facts() {
        let runner = FakeRunner::output(false, b"on", b"server unavailable");
        assert_eq!(
            query_option_with(&runner, "set-clipboard"),
            TmuxQueryResult::Unavailable
        );
        let runner = FakeRunner {
            output: Err("spawn failed".to_owned()),
            calls: RefCell::new(Vec::new()),
        };
        assert_eq!(
            query_version_with(&runner),
            TmuxQueryResult::Error("spawn failed".to_owned())
        );
    }

    #[test]
    fn support_query_accepts_both_known_spellings_only() {
        for stderr in [
            b"invalid option: allow-passthrough\n".as_slice(),
            b"unknown option: allow-passthrough\n".as_slice(),
        ] {
            let runner = FakeRunner::output(false, b"", stderr);
            assert_eq!(
                query_option_support_with(&runner, "allow-passthrough"),
                TmuxQueryResult::Unsupported
            );
        }
        let runner = FakeRunner::output(false, b"", b"no server running\n");
        assert_eq!(
            query_option_support_with(&runner, "allow-passthrough"),
            TmuxQueryResult::Unavailable
        );
    }

    /// A leader that exits successfully just under the process deadline must
    /// still return captured output: post-exit TERM grace + pipe drain use a
    /// separate bound and must not turn success into a drain timeout.
    ///
    /// A background descendant keeps the captured pipes open until process-group
    /// teardown so the drain cannot finish during the wait loop. That makes the
    /// post-exit cleanup window load-bearing once the main deadline is nearly
    /// exhausted.
    #[cfg(unix)]
    #[test]
    #[serial_test::serial(tmux_probe_path)]
    fn successful_near_deadline_exit_still_returns_captured_output() {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = tempfile::tempdir().unwrap();
        let bin = temp.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let tmux = bin.join("tmux");
        // Burn most of the process budget, then exit successfully while a
        // descendant still holds the pipes. Remaining main-deadline time is
        // intentionally below the fixed TERM grace sleep so a shared deadline
        // would fail the drain; the separate post-exit cleanup grace must keep
        // this a success. Perl select is used for subsecond precision.
        let timeout = Duration::from_millis(1500);
        std::fs::write(
            &tmux,
            "#!/bin/sh\n\
             /usr/bin/perl -e 'select(undef, undef, undef, 1.2)'\n\
             ( exec sleep 30 ) &\n\
             printf 'tmux 3.4\\n'\n\
             exit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o755)).unwrap();

        let previous_path = std::env::var_os("PATH");
        let mut path = OsString::from(bin.as_os_str());
        path.push(":");
        if let Some(existing) = &previous_path {
            path.push(existing);
        }
        // SAFETY: serialized on `tmux_probe_path`; restored before return.
        unsafe {
            std::env::set_var("PATH", &path);
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut last_timeout = None;
            for _ in 0..3 {
                match run_tmux_bounded(TmuxCommand::Version, timeout) {
                    Err(error) if error.starts_with("tmux query timed out") => {
                        // A heavily parallel test process can leave the shell
                        // unscheduled across the deliberately narrow deadline.
                        // Retry only that scheduler-sensitive outcome. Drain
                        // and teardown errors still expose regressions at once.
                        last_timeout = Some(error);
                    }
                    result => return result,
                }
            }
            Err(last_timeout.expect("each retry ended in a timeout"))
        }));
        match previous_path {
            Some(value) => unsafe {
                std::env::set_var("PATH", value);
            },
            None => unsafe {
                std::env::remove_var("PATH");
            },
        }
        let output = result
            .expect("near-deadline probe must not panic")
            .expect("near-deadline success must not become a drain error");
        assert!(output.status_success, "expected successful status");
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "tmux 3.4");
    }

    #[cfg(unix)]
    #[test]
    fn empty_group_teardown_does_not_wait_out_the_grace() {
        let group = echo_agent_tty_utils::ProcessGroup::new().expect("group");
        let started = std::time::Instant::now();
        terminate_owned_group(&group);
        let elapsed = started.elapsed();
        assert!(
            elapsed < GROUP_EXIT_GRACE / 2,
            "an empty group must not wait out the grace, took {elapsed:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn group_that_ignores_sigterm_waits_the_grace_and_is_killed() {
        let mut group = echo_agent_tty_utils::ProcessGroup::new().expect("group");
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg("trap '' TERM; sleep 1000")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        echo_agent_tty_utils::detach_std_command(&mut cmd);
        #[allow(clippy::disallowed_methods)] // test fixture; the test kills it
        let mut child = cmd.spawn().expect("spawn sigterm-ignoring child");
        group.attach_std(&child).expect("attach");
        // The shell installs its trap ~0.3ms after exec. Signal before that
        // and it dies to the default SIGTERM, leaving a zombie that still
        // reports live — the test then passes without exercising SIGKILL.
        std::thread::sleep(Duration::from_millis(250));

        let started = std::time::Instant::now();
        terminate_owned_group(&group);
        let elapsed = started.elapsed();

        assert!(
            elapsed >= GROUP_EXIT_GRACE,
            "an occupied group must still get the full grace, took {elapsed:?}"
        );

        // Bounded: without SIGKILL this fails in seconds rather than blocking
        // the run on `sleep 1000`.
        let reap_deadline = std::time::Instant::now() + Duration::from_secs(5);
        let status = loop {
            match child.try_wait().expect("poll child") {
                Some(status) => break status,
                None if std::time::Instant::now() < reap_deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                None => {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("the child survived teardown, so SIGKILL never escalated");
                }
            }
        };
        assert!(
            !status.success(),
            "the child must have been killed, got {status:?}"
        );
    }
}
