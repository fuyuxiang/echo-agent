//! Layer 1: PTY management — spawn, inject keys, resize, drain output.

use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result};
use echo_agent_test_support::{TestProcessTree, TestSandbox, process_has_exited_without_reap};
use portable_pty::{CommandBuilder, ExitStatus, PtySize, native_pty_system};

const PTY_DROP_REAP_TIMEOUT: Duration = Duration::from_millis(250);
const PTY_REAP_POLL: Duration = Duration::from_millis(10);
const PENDING_STATUS_ERROR: &str = "exit observed but status unavailable";

/// Raw key byte constants for terminal input injection.
pub mod keys {
    pub const J: &[u8] = b"j";
    pub const K: &[u8] = b"k";
    pub const Q: &[u8] = b"q";
    pub const DOWN: &[u8] = b"\x1b[B";
    pub const UP: &[u8] = b"\x1b[A";
    pub const RIGHT: &[u8] = b"\x1b[C";
    pub const PGDN: &[u8] = b"\x1b[6~";
    pub const PGUP: &[u8] = b"\x1b[5~";
    pub const ENTER: &[u8] = b"\r";
    pub const CTRL_C: &[u8] = b"\x03";
    /// Ctrl+R (0x12) — prompt history search / scrollback mouse-reporting toggle.
    pub const CTRL_R: &[u8] = b"\x12";
    pub const ESC: &[u8] = b"\x1b";
    /// F2 (SS3 `ESC O Q`, the xterm encoding crossterm parses) — opens the settings modal.
    pub const F2: &[u8] = b"\x1bOQ";
}

/// One explicit environment mutation applied after the TestSandbox baseline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnvOp<'a> {
    Set(&'a OsStr, &'a OsStr),
    Remove(&'a OsStr),
}

impl<'a> EnvOp<'a> {
    pub fn set(key: &'a str, value: &'a str) -> Self {
        Self::Set(OsStr::new(key), OsStr::new(value))
    }

    pub const fn set_os(key: &'a OsStr, value: &'a OsStr) -> Self {
        Self::Set(key, value)
    }

    pub fn remove(key: &'a str) -> Self {
        Self::Remove(OsStr::new(key))
    }

    pub const fn remove_os(key: &'a OsStr) -> Self {
        Self::Remove(key)
    }
}

#[derive(Debug)]
pub(crate) enum PtyRead {
    Chunk(Vec<u8>),
    Timeout,
    Closed,
}

/// Low-level PTY controller: spawns a child process inside a PTY and provides
/// methods to inject input, resize, and drain output.
pub struct PtyController {
    child: Box<dyn portable_pty::Child + Send>,
    process_tree: Option<TestProcessTree>,
    exit_status: Option<ExitStatus>,
    exit_observed: bool,
    spawn_pid: Option<u32>,
    // portable-pty's Unix kill may reap and cache status through Child::try_wait.
    #[cfg(unix)]
    portable_kill_may_have_reaped: bool,
    #[cfg(test)]
    status_cache_count: usize,
    #[cfg(test)]
    tree_release_count: usize,
    writer: Box<dyn Write + Send>,
    reader_rx: mpsc::Receiver<Vec<u8>>,
    #[allow(dead_code)] // Kept alive to hold the PTY open; used by resize().
    master: Box<dyn portable_pty::MasterPty + Send>,
}

impl PtyController {
    /// Inherit the parent environment for terminal-brand probes, echo-agent-wrap
    /// tests, and other fixtures that test inherited host env. Content-backed
    /// pager launches must use [`Self::spawn_in_sandbox`].
    pub fn spawn_inherited_env(
        binary: &Path,
        size: PtySize,
        args: &[&str],
        env: &[(&str, &str)],
        cwd: Option<&Path>,
    ) -> Result<Self> {
        let operations = set_operations(env);
        Self::spawn_inner(binary, size, args, &operations, cwd, None)
    }

    /// Spawn from a [`TestSandbox`] baseline plus typed per-process Set/Remove
    /// operations. The sandbox remains owned by the caller.
    pub fn spawn_in_sandbox(
        binary: &Path,
        size: PtySize,
        args: &[&str],
        sandbox: &TestSandbox,
        env: &[EnvOp<'_>],
        cwd: Option<&Path>,
    ) -> Result<Self> {
        Self::spawn_inner(binary, size, args, env, cwd, Some(sandbox))
    }

    fn spawn_inner(
        binary: &Path,
        size: PtySize,
        args: &[&str],
        env: &[EnvOp<'_>],
        cwd: Option<&Path>,
        sandbox: Option<&TestSandbox>,
    ) -> Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(size)?;

        let mut cmd = CommandBuilder::new(binary);
        for arg in args {
            cmd.arg(*arg);
        }
        if let Some(dir) = cwd {
            cmd.cwd(dir);
        }
        apply_child_env(&mut cmd, sandbox, env);

        // portable-pty calls setsid on Unix. Windows Job enrollment is a
        // best-effort post-spawn attachment, so a very short-lived descendant
        // may escape before enrollment; diagnostics preserve that downgrade.
        #[allow(clippy::disallowed_methods)]
        let child = pair.slave.spawn_command(cmd)?;
        #[cfg(unix)]
        let process_pid = child
            .process_id()
            .or_else(|| pair.master.process_group_leader().map(|pid| pid as u32));
        #[cfg(windows)]
        let process_pid = child.process_id();
        let process_tree =
            process_pid.map(|pid| TestProcessTree::attach(pid, "echoagent PTY child"));
        // Attachment failures remain recorded by TestProcessTree and are
        // surfaced through process_tree_diagnostics() on every harness timeout.
        // Drop the slave so we get EOF when the child exits.
        drop(pair.slave);

        let reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let reader_rx = spawn_reader(reader);

        Ok(Self {
            child,
            process_tree,
            exit_status: None,
            exit_observed: false,
            spawn_pid: process_pid,
            #[cfg(unix)]
            portable_kill_may_have_reaped: false,
            #[cfg(test)]
            status_cache_count: 0,
            #[cfg(test)]
            tree_release_count: 0,
            writer,
            reader_rx,
            master: pair.master,
        })
    }

    /// Write raw key bytes into the PTY stdin.
    pub fn inject_keys(&mut self, keys: &[u8]) -> Result<()> {
        self.writer
            .write_all(keys)
            .context("failed to write to PTY stdin")
    }

    /// Resize the PTY (sends SIGWINCH to the child). Arguments are `(rows, cols)`.
    pub fn resize(&self, rows: u16, cols: u16) -> Result<()> {
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("failed to resize PTY")
    }

    /// Drain the reader channel, collecting all available chunks within `timeout`.
    pub fn drain_output(&self, timeout: Duration) -> Vec<Vec<u8>> {
        let mut chunks = Vec::new();
        let deadline = std::time::Instant::now() + timeout;

        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match self.reader_rx.recv_timeout(remaining) {
                Ok(chunk) => chunks.push(chunk),
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        chunks
    }

    /// Send 'q' to trigger the pager's quit handler and wait for exit.
    ///
    /// Key injection is best-effort — the child may have already exited
    /// (e.g. no ACP server), so write failures are silently ignored.
    pub fn quit(&mut self) -> Result<()> {
        let _ = self.inject_keys(keys::Q);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if is_quit_complete(self.poll_exit_code())? {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                self.cleanup_descendants();
                self.kill_portable_child()?;
                self.wait_child_bounded(Duration::from_secs(1))
                    .context("failed to wait for pager child after kill")?
                    .context("pager child did not exit within 1s after kill")?;
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Receive one chunk, distinguishing timeout from reader EOF.
    ///
    /// Processing each chunk inline preserves inter-chunk timing.
    pub(crate) fn recv_chunk(&self, timeout: Duration) -> PtyRead {
        match self.reader_rx.recv_timeout(timeout) {
            Ok(chunk) => PtyRead::Chunk(chunk),
            Err(mpsc::RecvTimeoutError::Timeout) => PtyRead::Timeout,
            Err(mpsc::RecvTimeoutError::Disconnected) => PtyRead::Closed,
        }
    }

    /// Return true only while the child is live; pending status is non-running.
    pub fn is_running(&mut self) -> Result<bool> {
        self.poll_exit_code()
            .map(|state| state == PtyExitPoll::Running)
    }

    /// Poll once without collapsing pending status, liveness, or query errors.
    /// Repeated calls return cached exit status without querying a reaped child.
    pub fn poll_exit_code(&mut self) -> Result<PtyExitPoll<u32>> {
        let poll = self
            .poll_exit_status()
            .map(|status| status.map(|status| status.exit_code()));
        classify_exit_poll(poll, self.exit_observed)
    }

    /// Poll until exit or `timeout` without collapsing lifecycle states.
    /// Returns [`PtyExitPoll::PendingStatus`] immediately because the child is
    /// already non-running; [`PtyExitPoll::Running`] is returned only when the
    /// deadline expires while the child remains live.
    pub fn wait_exit_code(&mut self, timeout: Duration) -> Result<PtyExitPoll<u32>> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Some(state) =
                resolve_wait_poll(self.poll_exit_code(), std::time::Instant::now() >= deadline)?
            {
                return Ok(state);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Child PID while the direct child is live. Once reaped, returns `None` so
    /// callers cannot signal a recycled PID.
    pub fn child_pid(&self) -> Option<u32> {
        (!self.exit_observed && self.exit_status.is_none())
            .then_some(self.spawn_pid)
            .flatten()
    }

    /// Deliver a signal directly to the child (unix), bypassing the PTY line
    /// discipline. Exercises the real SIGINT/SIGTERM/SIGHUP paths (distinct from
    /// injected Ctrl+C key bytes, which are key events under raw mode).
    /// Call before the child is reaped: a reaped pid can be reused.
    #[cfg(unix)]
    pub fn send_signal(&self, signal: i32) -> Result<()> {
        let pid = self.child_pid().context("no child pid to signal")?;
        // SAFETY: libc::kill has no memory-safety preconditions, and child_pid()
        // only yields a positive live-child pid (never the kill(0)/kill(-1) broadcast).
        let rc = unsafe { libc::kill(pid as libc::pid_t, signal) };
        if rc != 0 {
            return Err(anyhow::Error::from(std::io::Error::last_os_error())).context("libc::kill");
        }
        Ok(())
    }

    fn poll_exit_status(&mut self) -> Result<Option<ExitStatus>> {
        if let Some(status) = self.exit_status.clone() {
            return Ok(Some(status));
        }
        #[cfg(unix)]
        {
            if let Some(pid) = self.spawn_pid {
                match observe_exit_before_reap(
                    process_has_exited_without_reap(pid, "PTY child"),
                    self.exit_observed,
                    self.portable_kill_may_have_reaped,
                ) {
                    Ok(ExitObservation::Running) => return Ok(None),
                    Ok(ExitObservation::Exited) => self.observe_exit_and_cleanup_tree(),
                    Ok(ExitObservation::StatusAlreadyConsumed) => {
                        self.observe_exit_and_cleanup_tree();
                        return self.recover_consumed_status();
                    }
                    Err(error) => {
                        return Err(error).context("failed to observe PTY child exit");
                    }
                }
            }
        }
        self.try_wait_and_cache()
    }

    fn try_wait_and_cache(&mut self) -> Result<Option<ExitStatus>> {
        let status = self
            .child
            .try_wait()
            .context("failed to query PTY child status")?;
        if let Some(status) = status {
            #[cfg(windows)]
            self.cleanup_descendants();
            self.cache_reaped_status(status.clone());
            return Ok(Some(status));
        }
        Ok(None)
    }

    fn cache_reaped_status(&mut self, status: ExitStatus) {
        if self.exit_status.is_none() {
            self.release_process_tree();
            cache_exit_status(
                &mut self.exit_status,
                &mut self.exit_observed,
                &mut self.spawn_pid,
                status,
            );
            #[cfg(test)]
            {
                self.status_cache_count += 1;
            }
        }
    }

    #[cfg(unix)]
    fn observe_exit_and_cleanup_tree(&mut self) {
        if !self.exit_observed {
            self.exit_observed = true;
            self.cleanup_descendants();
        }
    }

    #[cfg(unix)]
    fn recover_consumed_status(&mut self) -> Result<Option<ExitStatus>> {
        let status = recover_consumed_status(self.child.try_wait())
            .context("failed to recover PTY child status after it was consumed")?;
        self.cache_reaped_status(status.clone());
        Ok(Some(status))
    }

    fn kill_portable_child(&mut self) -> io::Result<()> {
        #[cfg(unix)]
        {
            self.portable_kill_may_have_reaped = true;
        }
        self.child.kill()
    }

    /// Process-group/job enrollment state.
    pub fn process_tree_diagnostics(&self) -> String {
        self.process_tree
            .as_ref()
            .map(TestProcessTree::diagnostic_summary)
            .unwrap_or_else(|| "tree_unavailable=true".to_owned())
    }

    fn kill_tree_best_effort(&self) {
        if let Some(tree) = &self.process_tree {
            let _ = tree.kill();
        }
    }

    fn release_process_tree(&mut self) {
        if let Some(mut tree) = self.process_tree.take() {
            tree.release();
            #[cfg(test)]
            {
                self.tree_release_count += 1;
            }
        }
    }

    fn cleanup_descendants(&mut self) {
        self.kill_tree_best_effort();
        self.release_process_tree();
    }

    fn wait_child_bounded(&mut self, timeout: Duration) -> Result<Option<ExitStatus>> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Some(status) = self.poll_exit_status()? {
                return Ok(Some(status));
            }
            if std::time::Instant::now() >= deadline {
                if self.exit_observed {
                    anyhow::bail!(PENDING_STATUS_ERROR);
                }
                return Ok(None);
            }
            std::thread::sleep(PTY_REAP_POLL);
        }
    }
}

impl Drop for PtyController {
    fn drop(&mut self) {
        if self.exit_status.is_none() {
            self.cleanup_descendants();
            let _ = self.kill_portable_child();
            let _ = self.wait_child_bounded(PTY_DROP_REAP_TIMEOUT);
        }
        self.release_process_tree();
    }
}

#[cfg(unix)]
#[derive(Debug, Eq, PartialEq)]
enum ExitObservation {
    Running,
    Exited,
    StatusAlreadyConsumed,
}

#[cfg(unix)]
fn observe_exit_before_reap(
    observation: io::Result<bool>,
    exit_observed: bool,
    portable_kill_may_have_reaped: bool,
) -> io::Result<ExitObservation> {
    match observation {
        Ok(false) => Ok(ExitObservation::Running),
        Ok(true) => Ok(ExitObservation::Exited),
        Err(error)
            if error.raw_os_error() == Some(libc::ECHILD)
                && (exit_observed || portable_kill_may_have_reaped) =>
        {
            Ok(ExitObservation::StatusAlreadyConsumed)
        }
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn recover_consumed_status(status: io::Result<Option<ExitStatus>>) -> io::Result<ExitStatus> {
    status?.ok_or_else(|| io::Error::other("PTY child status was consumed without being cached"))
}

/// Typed result of polling a PTY child's lifecycle.
///
/// Only [`Self::Running`] means the process is live. [`Self::PendingStatus`]
/// means exit was already observed, descendants were cleaned, and the PID was
/// hidden, but portable-pty has not yet yielded the final status.
#[must_use = "PTY exit state and poll errors must be handled explicitly"]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PtyExitPoll<T> {
    /// The child exited and its cached terminal status is available.
    Exited(T),
    /// The child is non-running, but its portable-pty status is not yet available.
    PendingStatus,
    /// The child is still live.
    Running,
}

fn classify_exit_poll<T, E>(
    poll: std::result::Result<Option<T>, E>,
    exit_observed: bool,
) -> std::result::Result<PtyExitPoll<T>, E> {
    match poll {
        Ok(Some(status)) => Ok(PtyExitPoll::Exited(status)),
        Ok(None) if exit_observed => Ok(PtyExitPoll::PendingStatus),
        Ok(None) => Ok(PtyExitPoll::Running),
        Err(error) => Err(error),
    }
}

fn resolve_wait_poll<T, E>(
    poll: std::result::Result<PtyExitPoll<T>, E>,
    deadline_reached: bool,
) -> std::result::Result<Option<PtyExitPoll<T>>, E> {
    match poll? {
        PtyExitPoll::Running if !deadline_reached => Ok(None),
        state => Ok(Some(state)),
    }
}

fn is_quit_complete<T, E>(
    poll: std::result::Result<PtyExitPoll<T>, E>,
) -> std::result::Result<bool, E> {
    match poll? {
        PtyExitPoll::Exited(_) | PtyExitPoll::PendingStatus => Ok(true),
        PtyExitPoll::Running => Ok(false),
    }
}

fn cache_exit_status(
    exit_status: &mut Option<ExitStatus>,
    exit_observed: &mut bool,
    spawn_pid: &mut Option<u32>,
    status: ExitStatus,
) {
    *exit_status = Some(status);
    *exit_observed = true;
    *spawn_pid = None;
}

const CLIPBOARD_SINK_ENV_VARS: &[&str] = &["ECHO_AGENT_OSC52_SINK", "LC_ECHO_AGENT_OSC52_SINK"];

/// Host / wrap appearance hints that would make `theme=auto` non-deterministic
/// in PTY tests (layout depends on the resolved palette).
const APPEARANCE_ENV_VARS: &[&str] = &[
    "ECHO_AGENT_APPEARANCE",
    "LC_ECHO_AGENT_APPEARANCE",
    "ECHO_AGENT_THEME",
    "LC_ECHO_AGENT_THEME",
    "COLORFGBG",
];

/// Host terminal identity markers stripped from the child environment.
///
/// The pager's terminal detection
/// (`echo-agent-pager-render/src/terminal/mod.rs`:
/// `detect_terminal_brand_from_env` / `detect_byobu_from_env` /
/// `detect_multiplexer_from_env` / `detect_tmux_meta_from_env`, plus
/// `embedded_editor.rs`'s `embedded_editor_from_env`) reads all of these,
/// so any one leaking from the harness's own host terminal reclassifies the
/// child: a dev running tests inside tmux leaks `TMUX` (every cell becomes
/// the remuxed profile), inside Cursor leaks `CURSOR_TRACE_ID` (checked
/// *before* `TERM_PROGRAM`, so it overrides even a test-injected brand),
/// inside nvim's `:terminal` leaks `NVIM` (clipboard OSC 52 wrapping).
/// Keep this list in sync with the detection source above.
const HOST_TERMINAL_ENV_VARS: &[&str] = &[
    // Brand chain (detect_terminal_brand_from_env), in detection order.
    "CURSOR_TRACE_ID",
    "VSCODE_GIT_ASKPASS_MAIN",
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "TERMINAL_EMULATOR",
    "WEZTERM_VERSION",
    "ITERM_SESSION_ID",
    "ITERM_PROFILE",
    "LC_TERMINAL",
    "LC_TERMINAL_VERSION",
    "TERM_SESSION_ID",
    "KITTY_WINDOW_ID",
    "ALACRITTY_SOCKET",
    "TERMINATOR_UUID",
    "VTE_VERSION",
    "WT_SESSION",
    // Multiplexer / Byobu markers (detect_multiplexer_from_env,
    // detect_byobu_from_env, detect_tmux_meta_from_env).
    "TMUX",
    "TMUX_PANE",
    "ZELLIJ",
    "ZELLIJ_SESSION_NAME",
    "STY",
    "BYOBU_BACKEND",
    "BYOBU_CONFIG_DIR",
    "BYOBU_DISTRO",
    "CMUX_SOCKET_PATH",
    "CMUX_PANEL_ID",
    "CMUX_BUNDLE_ID",
    "HERDR_ENV",
    // Embedded editor markers (embedded_editor_from_env).
    "NVIM",
    "NVIM_LISTEN_ADDRESS",
    "VIM_TERMINAL",
    "INSIDE_EMACS",
];

fn set_operations<'a>(env: &'a [(&'a str, &'a str)]) -> Vec<EnvOp<'a>> {
    env.iter()
        .map(|(key, value)| EnvOp::set(key, value))
        .collect()
}

/// Prepare the child environment. Content-backed callers provide a
/// [`echo_agent_test_support::TestSandbox`], which always clears inheritance.
/// The explicitly named inherited-env path is reserved for terminal probing and
/// echo-agent-wrap fixtures. Caller overrides are always applied last.
fn apply_child_env(cmd: &mut CommandBuilder, sandbox: Option<&TestSandbox>, env: &[EnvOp<'_>]) {
    if let Some(sandbox) = sandbox {
        cmd.env_clear();
        sandbox.apply_to_command_builder(cmd);
    }
    // Set TERM so the pager renders with full color support.
    cmd.env("TERM", "xterm-256color");
    // Strip inherited color opt-outs/overrides for the same reason: a
    // leaked NO_COLOR (common in agent/CI shells) renders the pager
    // colorless, making style-sensitive assertions (e.g. the selection
    // highlight color swap) silently untestable on some hosts. Tests may
    // re-set these via the `env` list (applied after this).
    for color_var in ["NO_COLOR", "CLICOLOR", "CLICOLOR_FORCE"] {
        cmd.env_remove(color_var);
    }
    // Strip SSH vars inherited from the parent: the harness PTY is a
    // local terminal, but `SSH_CONNECTION`/`SSH_TTY` leaking through
    // makes the pager's terminal detector report SSH and disable the
    // drag-drop image classifier (see
    // `try_handle_dropped_paths_paste` in `agent_view.rs`).
    for ssh_var in ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY", "SSH_AUTH_SOCK"] {
        cmd.env_remove(ssh_var);
    }
    // A harness launched under `echoagent wrap` must not silently confirm clipboard
    // delivery for no-sink scenarios. Explicit sink tests re-inject a marker
    // through `env` after this hygiene pass.
    for sink_var in CLIPBOARD_SINK_ENV_VARS {
        cmd.env_remove(sink_var);
    }
    for appearance_var in APPEARANCE_ENV_VARS {
        cmd.env_remove(appearance_var);
    }
    // Neutralize parent-terminal identity bleed: agent hosts often export
    // TERM_PROGRAM=ghostty/iTerm/etc. (and mux/editor markers) which make
    // the child pager adopt that host's key/modifier/clipboard quirks even
    // though we only set TERM above.
    for term_var in HOST_TERMINAL_ENV_VARS {
        cmd.env_remove(term_var);
    }
    for operation in env {
        match operation {
            EnvOp::Set(key, value) => cmd.env(key, value),
            EnvOp::Remove(key) => cmd.env_remove(key),
        }
    }
}

/// Spawn a background thread that reads from the PTY master and sends
/// chunks over an `mpsc` channel. The reader is blocking (WezTerm pattern),
/// so it must live on its own thread.
fn spawn_reader(mut reader: Box<dyn Read + Send>) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("pty-reader".into())
        .spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        })
        .expect("failed to spawn pty-reader thread");
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_poll_distinguishes_pending_running_and_errors() {
        assert_eq!(
            classify_exit_poll::<u32, &'static str>(Ok(None), true),
            Ok(PtyExitPoll::PendingStatus)
        );
        assert_eq!(
            classify_exit_poll::<u32, &'static str>(Ok(None), false),
            Ok(PtyExitPoll::Running)
        );
        assert_eq!(
            classify_exit_poll::<u32, &'static str>(Err("poll failed"), false),
            Err("poll failed")
        );
    }

    #[test]
    fn wait_deadline_preserves_pending_running_and_errors() {
        assert_eq!(
            resolve_wait_poll::<u32, &'static str>(Ok(PtyExitPoll::PendingStatus), false),
            Ok(Some(PtyExitPoll::PendingStatus))
        );
        assert_eq!(
            resolve_wait_poll::<u32, &'static str>(Ok(PtyExitPoll::PendingStatus), true),
            Ok(Some(PtyExitPoll::PendingStatus))
        );
        assert_eq!(
            resolve_wait_poll::<u32, &'static str>(Ok(PtyExitPoll::Running), false),
            Ok(None)
        );
        assert_eq!(
            resolve_wait_poll::<u32, &'static str>(Ok(PtyExitPoll::Running), true),
            Ok(Some(PtyExitPoll::Running))
        );
        assert_eq!(
            resolve_wait_poll::<u32, &'static str>(Err("poll failed"), true),
            Err("poll failed")
        );
    }

    #[test]
    fn quit_completion_accepts_non_running_states_and_propagates_errors() {
        assert_eq!(
            is_quit_complete::<u32, &'static str>(Ok(PtyExitPoll::Exited(0))),
            Ok(true)
        );
        assert_eq!(
            is_quit_complete::<u32, &'static str>(Ok(PtyExitPoll::PendingStatus)),
            Ok(true)
        );
        assert_eq!(
            is_quit_complete::<u32, &'static str>(Ok(PtyExitPoll::Running)),
            Ok(false)
        );
        assert_eq!(
            is_quit_complete::<u32, &'static str>(Err("poll failed")),
            Err("poll failed")
        );
    }

    #[cfg(unix)]
    #[test]
    fn observed_exit_echild_is_typed_only_after_portable_reap_capability() {
        let echild = || io::Error::from_raw_os_error(libc::ECHILD);
        let unrelated = io::Error::other("unrelated poll failure");

        assert_eq!(
            observe_exit_before_reap(Err(echild()), false, true).unwrap(),
            ExitObservation::StatusAlreadyConsumed
        );
        assert_eq!(
            observe_exit_before_reap(Err(echild()), true, false).unwrap(),
            ExitObservation::StatusAlreadyConsumed
        );
        assert_eq!(
            observe_exit_before_reap(Err(echild()), false, false)
                .unwrap_err()
                .raw_os_error(),
            Some(libc::ECHILD)
        );
        assert_eq!(
            observe_exit_before_reap(Err(unrelated), true, true)
                .unwrap_err()
                .to_string(),
            "unrelated poll failure"
        );
    }

    #[cfg(unix)]
    #[test]
    fn consumed_status_recovery_requires_a_cached_status() {
        let status = ExitStatus::with_exit_code(0);
        assert_eq!(
            recover_consumed_status(Ok(Some(status.clone())))
                .unwrap()
                .exit_code(),
            0
        );
        assert!(recover_consumed_status(Ok(None)).is_err());
        assert_eq!(
            recover_consumed_status(Err(io::Error::other("real status failure")))
                .unwrap_err()
                .to_string(),
            "real status failure"
        );
    }

    #[cfg(unix)]
    #[test]
    fn observed_exit_then_echild_recovers_cached_status_once() {
        let sandbox = TestSandbox::new();
        let mut controller = PtyController::spawn_in_sandbox(
            Path::new("/bin/sh"),
            PtySize {
                rows: 8,
                cols: 40,
                pixel_width: 0,
                pixel_height: 0,
            },
            &["-c", "exit 7"],
            &sandbox,
            &[],
            None,
        )
        .expect("spawn PTY exit fixture");
        let pid = controller.child_pid().expect("live child pid");
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !process_has_exited_without_reap(pid, "PTY exit fixture").expect("observe child exit")
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }

        controller.observe_exit_and_cleanup_tree();
        controller
            .kill_portable_child()
            .expect("portable kill consumes the exited child status");
        assert!(controller.portable_kill_may_have_reaped);
        assert_eq!(controller.tree_release_count, 1);
        assert_eq!(
            process_has_exited_without_reap(pid, "PTY exit fixture")
                .expect_err("consumed status must produce ECHILD")
                .raw_os_error(),
            Some(libc::ECHILD)
        );

        assert_eq!(
            controller
                .poll_exit_status()
                .expect("recover cached portable status")
                .expect("cached status")
                .exit_code(),
            7
        );
        assert_eq!(controller.status_cache_count, 1);
        assert_eq!(controller.tree_release_count, 1);
        assert_eq!(
            controller.poll_exit_status().unwrap().unwrap().exit_code(),
            7
        );
        assert_eq!(controller.status_cache_count, 1);
        assert_eq!(controller.tree_release_count, 1);
        assert_eq!(controller.child_pid(), None);
    }

    #[cfg(unix)]
    #[test]
    fn pty_waits_are_idempotent_and_pid_is_hidden_after_reap() {
        let sandbox = TestSandbox::new();
        let mut controller = PtyController::spawn_in_sandbox(
            Path::new("/bin/sh"),
            PtySize {
                rows: 8,
                cols: 40,
                pixel_width: 0,
                pixel_height: 0,
            },
            &["-c", "exit 7"],
            &sandbox,
            &[],
            None,
        )
        .expect("spawn PTY exit fixture");
        assert!(controller.child_pid().is_some());
        assert_eq!(
            controller.wait_exit_code(Duration::from_secs(2)).unwrap(),
            PtyExitPoll::Exited(7)
        );
        assert_eq!(
            controller.wait_exit_code(Duration::ZERO).unwrap(),
            PtyExitPoll::Exited(7)
        );
        assert_eq!(controller.poll_exit_code().unwrap(), PtyExitPoll::Exited(7));
        assert!(!controller.is_running().unwrap());
        assert_eq!(controller.status_cache_count, 1);
        assert_eq!(controller.tree_release_count, 1);
        assert_eq!(controller.child_pid(), None);
        assert!(controller.send_signal(libc::SIGTERM).is_err());
    }

    #[cfg(unix)]
    fn pid_is_alive(pid: u32) -> bool {
        // SAFETY: signal 0 performs an existence/permission check only.
        let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
        result == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    #[cfg(unix)]
    #[test]
    fn pty_drop_tree_cleanup_is_bounded_and_reaps_grandchild() {
        let sandbox = TestSandbox::new();
        let pid_file = sandbox.temp_dir().join("pty-grandchild.pid");
        let pid_path = pid_file.to_string_lossy().into_owned();
        let controller = PtyController::spawn_in_sandbox(
            Path::new("/bin/sh"),
            PtySize {
                rows: 8,
                cols: 40,
                pixel_width: 0,
                pixel_height: 0,
            },
            &["-c", "sleep 1000 & echo $! > \"$PID_FILE\"; wait"],
            &sandbox,
            &[EnvOp::set("PID_FILE", &pid_path)],
            None,
        )
        .expect("spawn PTY tree fixture");
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let grandchild_pid = loop {
            if let Ok(raw) = std::fs::read_to_string(&pid_file)
                && let Ok(pid) = raw.trim().parse::<u32>()
            {
                break pid;
            }
            assert!(std::time::Instant::now() < deadline, "pid file timeout");
            std::thread::sleep(Duration::from_millis(10));
        };

        let started = std::time::Instant::now();
        drop(controller);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "PTY Drop exceeded its bounded wait"
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while pid_is_alive(grandchild_pid) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !pid_is_alive(grandchild_pid),
            "PTY grandchild leaked after controller Drop"
        );
    }

    #[cfg(unix)]
    #[test]
    fn pty_tree_diagnostics_surface_enrollment_state() {
        let tree = TestProcessTree::attach(u32::MAX, "invalid PTY fixture");
        let diagnostics = tree.diagnostic_summary();
        assert!(diagnostics.contains("tree_label=\"invalid PTY fixture\""));
        assert!(diagnostics.contains("tree_attached=false"));
        assert!(diagnostics.contains("tree_attach_error=Some"));
    }

    /// Every host-terminal marker the pager's detection chain reads must be
    /// stripped from the child env — polluted entries are seeded via
    /// `cmd.env` (same `CommandBuilder` map that inherited base-env entries
    /// live in, so `env_remove` takes the identical path) rather than
    /// process-global `set_var` (racy under parallel tests).
    #[test]
    fn apply_child_env_strips_all_host_terminal_markers() {
        let mut cmd = CommandBuilder::new("true");
        for var in HOST_TERMINAL_ENV_VARS {
            cmd.env(var, "polluted");
        }
        for ssh_var in ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY", "SSH_AUTH_SOCK"] {
            cmd.env(ssh_var, "polluted");
        }
        for color_var in ["NO_COLOR", "CLICOLOR", "CLICOLOR_FORCE"] {
            cmd.env(color_var, "polluted");
        }
        for sink_var in CLIPBOARD_SINK_ENV_VARS {
            cmd.env(sink_var, "polluted");
        }
        for appearance_var in APPEARANCE_ENV_VARS {
            cmd.env(appearance_var, "polluted");
        }
        // Sandboxed launches remove unrelated inherited variables before
        // re-applying the baseline and explicit overrides.
        cmd.env("ECHO_AGENT_SCROLL_LOG", "/tmp/scroll.jsonl");
        let sandbox = TestSandbox::new();

        apply_child_env(&mut cmd, Some(&sandbox), &[]);

        for var in HOST_TERMINAL_ENV_VARS {
            assert!(
                cmd.get_env(var).is_none(),
                "host terminal marker {var} leaked into the child env"
            );
        }
        for ssh_var in ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY", "SSH_AUTH_SOCK"] {
            assert!(
                cmd.get_env(ssh_var).is_none(),
                "SSH marker {ssh_var} leaked into the child env"
            );
        }
        for color_var in ["NO_COLOR", "CLICOLOR", "CLICOLOR_FORCE"] {
            assert!(
                cmd.get_env(color_var).is_none(),
                "color override {color_var} leaked into the child env"
            );
        }
        for sink_var in CLIPBOARD_SINK_ENV_VARS {
            assert!(
                cmd.get_env(sink_var).is_none(),
                "clipboard sink marker {sink_var} leaked into the child env"
            );
        }
        for appearance_var in APPEARANCE_ENV_VARS {
            assert!(
                cmd.get_env(appearance_var).is_none(),
                "appearance hint {appearance_var} leaked into the child env"
            );
        }
        assert_eq!(
            cmd.get_env("TERM").and_then(|v| v.to_str()),
            Some("xterm-256color")
        );
        assert_eq!(
            cmd.get_env("ECHO_AGENT_SCROLL_LOG")
                .and_then(|v| v.to_str()),
            None,
            "hermetic baseline must remove unrelated inherited vars"
        );
        assert_eq!(
            cmd.get_env("ECHO_AGENT_HOME").and_then(|v| v.to_str()),
            sandbox.echo_agent_home().to_str()
        );
    }

    #[test]
    fn apply_child_env_uses_sandbox_baseline() {
        let sandbox = TestSandbox::new();
        let mut cmd = CommandBuilder::new("true");

        apply_child_env(&mut cmd, Some(&sandbox), &[]);

        assert_eq!(
            cmd.get_env("HOME").and_then(|v| v.to_str()),
            sandbox.home().to_str()
        );
        assert_eq!(
            cmd.get_env("ECHO_AGENT_HOME").and_then(|v| v.to_str()),
            sandbox.echo_agent_home().to_str()
        );
        assert_eq!(cmd.get_env("ECHO_AGENT_LEADER_SOCKET"), None);
    }

    #[test]
    fn apply_child_env_remove_deletes_sandbox_credential() {
        let sandbox = TestSandbox::builder()
            .mock_url("http://127.0.0.1:43123/v1")
            .build();
        let mut cmd = CommandBuilder::new("true");

        apply_child_env(
            &mut cmd,
            Some(&sandbox),
            &[EnvOp::remove("ECHO_AGENT_API_KEY")],
        );

        assert_eq!(cmd.get_env("ECHO_AGENT_API_KEY"), None);
        assert_eq!(
            cmd.get_env("ECHO_AGENT_API_BASE_URL")
                .and_then(|v| v.to_str()),
            Some("http://127.0.0.1:43123/v1")
        );
    }

    #[test]
    fn inherited_env_projection_is_set_only_and_preserves_unrelated_ambient_vars() {
        let operations = set_operations(&[("EXPLICIT_MARKER", "set")]);
        assert_eq!(operations, [EnvOp::set("EXPLICIT_MARKER", "set")]);

        let mut cmd = CommandBuilder::new("true");
        cmd.env("AMBIENT_MARKER", "inherited");
        apply_child_env(&mut cmd, None, &operations);

        assert_eq!(
            cmd.get_env("AMBIENT_MARKER")
                .and_then(|value| value.to_str()),
            Some("inherited")
        );
        assert_eq!(
            cmd.get_env("EXPLICIT_MARKER")
                .and_then(|value| value.to_str()),
            Some("set")
        );
    }

    #[test]
    fn apply_child_env_caller_env_overrides_survive_strips() {
        let mut cmd = CommandBuilder::new("true");
        cmd.env("TMUX", "/tmp/host-tmux,999,0");
        cmd.env("CURSOR_TRACE_ID", "host-cursor");

        apply_child_env(
            &mut cmd,
            None,
            &[
                EnvOp::set("TERM_PROGRAM", "vscode"),
                EnvOp::set("NVIM", "/tmp/fake-nvim.sock"),
                EnvOp::set("TERM", "xterm-kitty"),
                EnvOp::set("ECHO_AGENT_OSC52_SINK", "1"),
            ],
        );

        // Host pollution is gone…
        assert!(cmd.get_env("TMUX").is_none());
        assert!(cmd.get_env("CURSOR_TRACE_ID").is_none());
        // …but caller-injected markers (applied after strips) survive,
        // including overriding the harness's own TERM default.
        assert_eq!(
            cmd.get_env("TERM_PROGRAM").and_then(|v| v.to_str()),
            Some("vscode")
        );
        assert_eq!(
            cmd.get_env("NVIM").and_then(|v| v.to_str()),
            Some("/tmp/fake-nvim.sock")
        );
        assert_eq!(
            cmd.get_env("TERM").and_then(|v| v.to_str()),
            Some("xterm-kitty")
        );
        assert_eq!(
            cmd.get_env("ECHO_AGENT_OSC52_SINK")
                .and_then(|v| v.to_str()),
            Some("1"),
            "explicit sink scenarios must be able to re-inject the marker"
        );
    }
}
