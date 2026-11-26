//! Shared subprocess lifecycle ownership for echo-agent-build test harnesses.
//!
//! [`TestProcess`] is the Tokio-child owner used by ACP, leader, and headless
//! harnesses. [`TestProcessTree`] is the narrower process-tree guard used when
//! a dependency (notably `portable-pty`) owns the concrete child handle.

use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::io::{self, ErrorKind};
use std::pin::Pin;
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt as _, ReadBuf};
use tokio::task::JoinHandle;

use crate::sandbox::TestSandbox;

const DEFAULT_TAIL_BYTES: usize = 64 * 1024;
const DEFAULT_GRACE_PERIOD: Duration = Duration::from_millis(500);
const DEFAULT_KILL_WAIT: Duration = Duration::from_secs(5);
const CAPTURE_DRAIN_WAIT: Duration = Duration::from_secs(1);
const DROP_REAP_WAIT: Duration = Duration::from_millis(250);

/// How the test child receives stdin.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TestStdin {
    /// No inherited terminal and immediate EOF.
    #[default]
    Null,
    /// A pipe retrievable with [`TestProcess::take_stdin`].
    Piped,
}

/// How one output stream is consumed.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TestOutput {
    /// Drain the stream in the background while retaining only a bounded tail.
    #[default]
    Capture,
    /// Let the caller consume the stream through a tail-capturing reader.
    Piped,
}

/// Spawn and shutdown policy for [`TestProcess`].
#[derive(Debug)]
pub struct TestProcessConfig {
    label: Option<String>,
    stdin: TestStdin,
    stdout: TestOutput,
    stderr: TestOutput,
    tail_bytes: usize,
    grace_period: Duration,
    kill_wait: Duration,
    env: Vec<(OsString, OsString)>,
}

impl TestProcessConfig {
    pub fn new() -> Self {
        Self::default()
    }

    /// Diagnostic name; command arguments and environment are not included.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn stdin(mut self, policy: TestStdin) -> Self {
        self.stdin = policy;
        self
    }

    pub fn stdout(mut self, policy: TestOutput) -> Self {
        self.stdout = policy;
        self
    }

    pub fn stderr(mut self, policy: TestOutput) -> Self {
        self.stderr = policy;
        self
    }

    pub fn tail_bytes(mut self, bytes: usize) -> Self {
        self.tail_bytes = bytes.max(1);
        self
    }

    pub fn grace_period(mut self, grace_period: Duration) -> Self {
        self.grace_period = grace_period;
        self
    }

    pub fn kill_wait(mut self, kill_wait: Duration) -> Self {
        self.kill_wait = kill_wait;
        self
    }

    /// Apply a child-environment override after the sandbox baseline.
    pub fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.env
            .push((key.as_ref().to_owned(), value.as_ref().to_owned()));
        self
    }

    pub fn envs<I, K, V>(mut self, env: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        self.env.extend(
            env.into_iter()
                .map(|(key, value)| (key.as_ref().to_owned(), value.as_ref().to_owned())),
        );
        self
    }
}

impl Default for TestProcessConfig {
    fn default() -> Self {
        Self {
            label: None,
            stdin: TestStdin::Null,
            stdout: TestOutput::Capture,
            stderr: TestOutput::Capture,
            tail_bytes: DEFAULT_TAIL_BYTES,
            grace_period: DEFAULT_GRACE_PERIOD,
            kill_wait: DEFAULT_KILL_WAIT,
            env: Vec::new(),
        }
    }
}

/// Why the process owner initiated or observed termination.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TestProcessTermination {
    NaturalExit,
    GracefulTerminate,
    HardKill,
    HardKillAfterGrace,
    DropCleanup,
}

/// Current process state for diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TestProcessState {
    Running,
    Exited,
}

/// A bounded output-tail snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TestOutputSnapshot {
    pub text: String,
    pub bytes_seen: u64,
    pub truncated: bool,
    pub read_error: Option<String>,
}

#[derive(Debug)]
struct TailState {
    bytes: Vec<u8>,
    capacity: usize,
    bytes_seen: u64,
    truncated: bool,
    read_error: Option<String>,
}

impl TailState {
    fn new(capacity: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(capacity),
            capacity,
            bytes_seen: 0,
            truncated: false,
            read_error: None,
        }
    }

    fn append(&mut self, bytes: &[u8]) {
        self.bytes_seen = self.bytes_seen.saturating_add(bytes.len() as u64);
        if bytes.len() >= self.capacity {
            self.bytes.clear();
            self.bytes
                .extend_from_slice(&bytes[bytes.len() - self.capacity..]);
            self.truncated = true;
            return;
        }

        let overflow = self
            .bytes
            .len()
            .saturating_add(bytes.len())
            .saturating_sub(self.capacity);
        if overflow > 0 {
            self.bytes.drain(..overflow);
            self.truncated = true;
        }
        self.bytes.extend_from_slice(bytes);
    }

    fn snapshot(&self) -> TestOutputSnapshot {
        TestOutputSnapshot {
            text: String::from_utf8_lossy(&self.bytes).into_owned(),
            bytes_seen: self.bytes_seen,
            truncated: self.truncated,
            read_error: self.read_error.clone(),
        }
    }
}

#[derive(Clone, Debug)]
struct OutputTail(Arc<Mutex<TailState>>);

impl OutputTail {
    fn new(capacity: usize) -> Self {
        Self(Arc::new(Mutex::new(TailState::new(capacity))))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, TailState> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn append(&self, bytes: &[u8]) {
        self.lock().append(bytes);
    }

    fn record_error(&self, error: &io::Error) {
        self.lock().read_error = Some(error.to_string());
    }

    fn snapshot(&self) -> TestOutputSnapshot {
        self.lock().snapshot()
    }
}

macro_rules! captured_reader {
    ($name:ident, $inner:ty) => {
        /// A child-output reader that updates its owning [`TestProcess`]'s
        /// bounded diagnostic tail as the caller consumes bytes.
        pub struct $name {
            inner: $inner,
            tail: OutputTail,
        }

        impl AsyncRead for $name {
            fn poll_read(
                self: Pin<&mut Self>,
                cx: &mut Context<'_>,
                buf: &mut ReadBuf<'_>,
            ) -> Poll<io::Result<()>> {
                let this = self.get_mut();
                let before = buf.filled().len();
                match Pin::new(&mut this.inner).poll_read(cx, buf) {
                    Poll::Ready(Ok(())) => {
                        this.tail.append(&buf.filled()[before..]);
                        Poll::Ready(Ok(()))
                    }
                    Poll::Ready(Err(error)) => {
                        this.tail.record_error(&error);
                        Poll::Ready(Err(error))
                    }
                    Poll::Pending => Poll::Pending,
                }
            }
        }
    };
}

captured_reader!(TestProcessStdout, tokio::process::ChildStdout);
captured_reader!(TestProcessStderr, tokio::process::ChildStderr);

/// Synchronous process-tree owner for wrappers that retain their child handle.
pub struct TestProcessTree {
    pid: u32,
    label: String,
    group: Option<echo_agent_tty_utils::ProcessGroup>,
    attachment_error: Option<String>,
}

impl TestProcessTree {
    /// Attach to a child already spawned as its own session/process group.
    pub fn try_attach(pid: u32, label: impl Into<String>) -> io::Result<Self> {
        let label = label.into();
        let mut group = echo_agent_tty_utils::ProcessGroup::new()?;
        group.attach_pid(pid)?;
        Ok(Self::from_group(pid, label, group))
    }

    pub fn attach(pid: u32, label: impl Into<String>) -> Self {
        let label = label.into();
        let (group, attachment_error) = match echo_agent_tty_utils::ProcessGroup::new() {
            Ok(mut group) => match group.attach_pid(pid) {
                Ok(()) => (Some(group), None),
                Err(error) => (None, Some(error.to_string())),
            },
            Err(error) => (None, Some(error.to_string())),
        };
        Self {
            pid,
            label,
            group,
            attachment_error,
        }
    }

    fn from_group(pid: u32, label: String, group: echo_agent_tty_utils::ProcessGroup) -> Self {
        Self {
            pid,
            label,
            group: Some(group),
            attachment_error: None,
        }
    }

    #[cfg(windows)]
    fn from_attachment_error(pid: u32, label: String, error: io::Error) -> Self {
        Self {
            pid,
            label,
            group: None,
            attachment_error: Some(format!(
                "best-effort Windows Job attachment failed: {error}"
            )),
        }
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn is_attached(&self) -> bool {
        self.group.is_some()
    }

    pub fn attachment_error(&self) -> Option<&str> {
        self.attachment_error.as_deref()
    }

    pub fn supports_graceful_termination(&self) -> bool {
        cfg!(unix) && self.group.is_some()
    }

    pub fn terminate(&self) -> io::Result<()> {
        match &self.group {
            Some(group) => group.terminate(),
            None => Err(self.unavailable_error("terminate")),
        }
    }

    pub fn kill(&self) -> io::Result<()> {
        match &self.group {
            Some(group) => group.kill(),
            None => Err(self.unavailable_error("kill")),
        }
    }

    /// Stop owning the process-group/job handle after the concrete child owner
    /// has reaped the child and torn down any remaining descendants.
    pub fn release(&mut self) {
        self.group = None;
    }

    pub fn diagnostic_summary(&self) -> String {
        format!(
            "tree_label={:?} pid={} tree_attached={} tree_attach_error={:?}",
            self.label,
            self.pid,
            self.is_attached(),
            self.attachment_error,
        )
    }

    fn unavailable_error(&self, operation: &str) -> io::Error {
        let detail = self
            .attachment_error
            .as_deref()
            .unwrap_or("process-tree handle already released");
        io::Error::other(format!(
            "cannot {operation} process tree {} (pid {}): {detail}",
            self.label, self.pid
        ))
    }
}

impl Drop for TestProcessTree {
    fn drop(&mut self) {
        if let Some(group) = &self.group {
            let _ = group.kill();
        }
    }
}

impl std::fmt::Debug for TestProcessTree {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TestProcessTree")
            .field("pid", &self.pid)
            .field("label", &self.label)
            .field("attached", &self.is_attached())
            .field("attachment_error", &self.attachment_error)
            .finish()
    }
}

/// Owns one detached Tokio child, its process tree, and bounded output tails.
pub struct TestProcess {
    child: tokio::process::Child,
    tree: TestProcessTree,
    stdin: Option<tokio::process::ChildStdin>,
    stdout: Option<tokio::process::ChildStdout>,
    stderr: Option<tokio::process::ChildStderr>,
    stdout_tail: OutputTail,
    stderr_tail: OutputTail,
    stdout_capture: Option<JoinHandle<()>>,
    stderr_capture: Option<JoinHandle<()>>,
    status: Option<ExitStatus>,
    termination: Option<TestProcessTermination>,
    lifecycle_errors: Vec<String>,
    grace_period: Duration,
    kill_wait: Duration,
    redactions: Vec<String>,
}

impl TestProcess {
    /// Spawn from the [`TestSandbox`] baseline with detached, piped stdio and
    /// test-owned process-tree cleanup.
    ///
    /// Unix detachment establishes the child's session/process group before
    /// exec. Windows preserves `CREATE_NO_WINDOW`; Job attachment uses the
    /// pre-existing post-spawn API, so very short-lived descendants can escape
    /// before enrollment and cleanup remains best effort.
    pub fn spawn(
        mut cmd: tokio::process::Command,
        sandbox: &TestSandbox,
        config: TestProcessConfig,
    ) -> io::Result<Self> {
        let program = cmd.as_std().get_program().to_owned();
        let label = config.label.unwrap_or_else(|| {
            std::path::Path::new(&program)
                .file_name()
                .unwrap_or(program.as_os_str())
                .to_string_lossy()
                .into_owned()
        });

        sandbox.apply_to_tokio_command(&mut cmd);
        cmd.envs(echo_agent_tty_utils::pager_env())
            .envs(config.env)
            .stdin(match config.stdin {
                TestStdin::Null => Stdio::null(),
                TestStdin::Piped => Stdio::piped(),
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        echo_agent_tty_utils::detach_command(&mut cmd);

        let mut group = echo_agent_tty_utils::ProcessGroup::new()?;
        #[allow(clippy::disallowed_methods)]
        let mut child = cmd.spawn().map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("failed to spawn owned test process {label:?}: {error}"),
            )
        })?;
        let pid = child.id().ok_or_else(|| {
            io::Error::other(format!("spawned test process {label:?} has no pid"))
        })?;
        let tree = match group.attach(&child) {
            Ok(()) => TestProcessTree::from_group(pid, label, group),
            #[cfg(unix)]
            Err(error) => {
                let _ = child.start_kill();
                let cleanup_deadline = std::time::Instant::now() + DROP_REAP_WAIT;
                while std::time::Instant::now() < cleanup_deadline {
                    if child.try_wait().ok().flatten().is_some() {
                        break;
                    }
                    std::thread::yield_now();
                }
                return Err(io::Error::new(
                    error.kind(),
                    format!("failed to attach owned test process {label:?}: {error}"),
                ));
            }
            #[cfg(windows)]
            Err(error) => TestProcessTree::from_attachment_error(pid, label, error),
        };

        let stdin = child.stdin.take();
        let stdout_tail = OutputTail::new(config.tail_bytes);
        let stderr_tail = OutputTail::new(config.tail_bytes);
        let child_stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("test child stdout pipe missing"))?;
        let child_stderr = child
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("test child stderr pipe missing"))?;

        let (stdout, stdout_capture) = match config.stdout {
            TestOutput::Capture => (None, Some(spawn_capture(child_stdout, stdout_tail.clone()))),
            TestOutput::Piped => (Some(child_stdout), None),
        };
        let (stderr, stderr_capture) = match config.stderr {
            TestOutput::Capture => (None, Some(spawn_capture(child_stderr, stderr_tail.clone()))),
            TestOutput::Piped => (Some(child_stderr), None),
        };

        Ok(Self {
            child,
            tree,
            stdin,
            stdout,
            stderr,
            stdout_tail,
            stderr_tail,
            stdout_capture,
            stderr_capture,
            status: None,
            termination: None,
            lifecycle_errors: Vec::new(),
            grace_period: config.grace_period,
            kill_wait: config.kill_wait,
            redactions: sandbox.diagnostic_redactions(),
        })
    }

    /// Direct-child PID while it is live.
    pub fn pid(&self) -> Option<u32> {
        (self.status.is_none() && self.child.id().is_some()).then_some(self.tree.pid())
    }

    pub fn state(&self) -> TestProcessState {
        if self.status.is_some() {
            TestProcessState::Exited
        } else {
            TestProcessState::Running
        }
    }

    pub fn status(&self) -> Option<ExitStatus> {
        self.status
    }

    pub fn termination_reason(&self) -> Option<TestProcessTermination> {
        self.termination
    }

    pub fn stdout_tail(&self) -> TestOutputSnapshot {
        self.stdout_tail.snapshot()
    }

    pub fn stderr_tail(&self) -> TestOutputSnapshot {
        self.stderr_tail.snapshot()
    }

    pub fn take_stdin(&mut self) -> Option<tokio::process::ChildStdin> {
        self.stdin.take()
    }

    pub fn take_stdout(&mut self) -> Option<TestProcessStdout> {
        self.stdout.take().map(|inner| TestProcessStdout {
            inner,
            tail: self.stdout_tail.clone(),
        })
    }

    pub fn take_stderr(&mut self) -> Option<TestProcessStderr> {
        self.stderr.take().map(|inner| TestProcessStderr {
            inner,
            tail: self.stderr_tail.clone(),
        })
    }

    /// Send a whole-tree graceful termination signal without waiting.
    pub fn start_terminate(&mut self) -> io::Result<()> {
        if self.status.is_some() {
            return Ok(());
        }
        if self.tree.supports_graceful_termination() {
            self.termination = Some(TestProcessTermination::GracefulTerminate);
            self.tree.terminate()
        } else {
            self.request_hard_kill(TestProcessTermination::HardKill);
            Ok(())
        }
    }

    /// Start a whole-tree hard kill without waiting.
    pub fn start_kill(&mut self) {
        if self.status.is_none() {
            self.request_hard_kill(TestProcessTermination::HardKill);
        }
    }

    /// Poll once and cache the exit status. Unix observes exit without reaping,
    /// kills any remaining descendants while the PGID is still reserved by the
    /// zombie leader, and only then consumes the direct child's wait status.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if let Some(status) = self.status {
            return Ok(Some(status));
        }
        #[cfg(unix)]
        let status = {
            if !process_has_exited_without_reap(self.tree.pid(), &self.tree.label)? {
                return Ok(None);
            }
            self.cleanup_descendants_before_reap();
            self.child.try_wait()?.ok_or_else(|| {
                io::Error::other("observed child exit was not reapable by Tokio child owner")
            })?
        };
        #[cfg(windows)]
        let status = {
            let Some(status) = self.child.try_wait()? else {
                return Ok(None);
            };
            // Windows process and Job handles stay stable after direct-child
            // reap, unlike Unix PGIDs, so descendant cleanup can follow here.
            self.cleanup_descendants_before_reap();
            status
        };
        self.record_reaped(status);
        Ok(Some(status))
    }

    pub fn is_running(&mut self) -> io::Result<bool> {
        self.try_wait().map(|status| status.is_none())
    }

    /// Wait for direct-child exit up to `deadline`. `Ok(None)` means the child
    /// is still owned and running; no implicit kill occurs.
    pub async fn wait_with_deadline(
        &mut self,
        deadline: Duration,
    ) -> io::Result<Option<ExitStatus>> {
        if let Some(status) = self.status {
            self.finish_capture_tasks().await;
            return Ok(Some(status));
        }

        let deadline = tokio::time::Instant::now() + deadline;
        loop {
            if let Some(status) = self.try_wait()? {
                self.finish_capture_tasks().await;
                return Ok(Some(status));
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            tokio::time::sleep(Duration::from_millis(10).min(remaining)).await;
        }
    }

    /// Request whole-tree graceful termination, then escalate to a hard tree
    /// kill if the child has not exited within the configured grace period.
    pub async fn close(&mut self) -> io::Result<ExitStatus> {
        if let Some(status) = self.try_wait()? {
            self.finish_capture_tasks().await;
            return Ok(status);
        }

        if !self.tree.supports_graceful_termination() {
            self.request_hard_kill(TestProcessTermination::HardKill);
            return self.wait_after_kill().await;
        }

        self.termination = Some(TestProcessTermination::GracefulTerminate);
        if let Err(error) = self.tree.terminate() {
            self.push_lifecycle_error("graceful tree terminate", error);
        }
        if let Some(status) = self.wait_with_deadline(self.grace_period).await? {
            return Ok(status);
        }

        self.request_hard_kill(TestProcessTermination::HardKillAfterGrace);
        self.wait_after_kill().await
    }

    /// Hard-kill the whole tree immediately and wait a bounded time for the
    /// direct child to be reaped.
    pub async fn kill(&mut self) -> io::Result<ExitStatus> {
        if let Some(status) = self.try_wait()? {
            self.finish_capture_tasks().await;
            return Ok(status);
        }
        self.request_hard_kill(TestProcessTermination::HardKill);
        self.wait_after_kill().await
    }

    /// Sanitized state and bounded output-tail diagnostics.
    pub fn diagnostic_summary(&self) -> String {
        let stdout = self.stdout_tail();
        let stderr = self.stderr_tail();
        let stdout_text = sanitize_output(&stdout.text, &self.redactions);
        let stderr_text = sanitize_output(&stderr.text, &self.redactions);
        let mut summary = format!(
            "{} state={:?} status={:?} termination={:?} \
             stdout_bytes={} stdout_truncated={} stdout_read_error={:?} stdout_tail={:?} \
             stderr_bytes={} stderr_truncated={} stderr_read_error={:?} stderr_tail={:?}",
            self.tree.diagnostic_summary(),
            self.state(),
            self.status,
            self.termination,
            stdout.bytes_seen,
            stdout.truncated,
            stdout.read_error,
            stdout_text,
            stderr.bytes_seen,
            stderr.truncated,
            stderr.read_error,
            stderr_text,
        );
        if !self.lifecycle_errors.is_empty() {
            let _ = write!(summary, " lifecycle_errors={:?}", self.lifecycle_errors);
        }
        summary
    }

    fn cleanup_descendants_before_reap(&mut self) {
        if let Err(error) = self.tree.kill() {
            self.push_lifecycle_error("pre-reap descendant cleanup", error);
        }
    }

    fn record_reaped(&mut self, status: ExitStatus) {
        self.tree.release();
        self.status = Some(status);
        self.termination
            .get_or_insert(TestProcessTermination::NaturalExit);
    }

    fn request_hard_kill(&mut self, reason: TestProcessTermination) {
        self.termination = Some(reason);
        if let Err(error) = self.tree.kill() {
            self.push_lifecycle_error("hard tree kill", error);
        }
        if let Err(error) = self.child.start_kill() {
            self.push_lifecycle_error("direct-child hard-kill fallback", error);
        }
    }

    fn push_lifecycle_error(&mut self, operation: &str, error: io::Error) {
        if !is_missing_process_error(&error) {
            self.lifecycle_errors.push(format!("{operation}: {error}"));
        }
    }

    async fn wait_after_kill(&mut self) -> io::Result<ExitStatus> {
        match self.wait_with_deadline(self.kill_wait).await? {
            Some(status) => Ok(status),
            None => Err(io::Error::new(
                ErrorKind::TimedOut,
                format!(
                    "test process did not exit within {:?} after hard kill: {}",
                    self.kill_wait,
                    self.diagnostic_summary()
                ),
            )),
        }
    }

    async fn finish_capture_tasks(&mut self) {
        finish_capture_task(self.stdout_capture.take()).await;
        finish_capture_task(self.stderr_capture.take()).await;
    }
}

impl std::fmt::Debug for TestProcess {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TestProcess")
            .field("pid", &self.pid())
            .field("state", &self.state())
            .field("status", &self.status)
            .field("termination", &self.termination)
            .finish_non_exhaustive()
    }
}

impl Drop for TestProcess {
    fn drop(&mut self) {
        if self.status.is_none() {
            self.termination = Some(TestProcessTermination::DropCleanup);
            let _ = self.tree.kill();
            let _ = self.child.start_kill();
            // Bound synchronous reaping because async cleanup may not run during
            // runtime teardown.
            let deadline = std::time::Instant::now() + DROP_REAP_WAIT;
            while std::time::Instant::now() < deadline {
                #[cfg(unix)]
                match process_has_exited_without_reap(self.tree.pid(), &self.tree.label) {
                    Ok(true) => {
                        self.cleanup_descendants_before_reap();
                        if let Ok(Some(status)) = self.child.try_wait() {
                            self.record_reaped(status);
                        }
                        break;
                    }
                    Ok(false) => std::thread::yield_now(),
                    Err(_) => break,
                }
                #[cfg(windows)]
                match self.child.try_wait() {
                    Ok(Some(status)) => {
                        self.cleanup_descendants_before_reap();
                        self.record_reaped(status);
                        break;
                    }
                    Ok(None) => std::thread::yield_now(),
                    Err(_) => break,
                }
            }
        }
        if let Some(task) = self.stdout_capture.take() {
            task.abort();
        }
        if let Some(task) = self.stderr_capture.take() {
            task.abort();
        }
    }
}

fn is_missing_process_error(error: &io::Error) -> bool {
    #[cfg(unix)]
    {
        matches!(error.raw_os_error(), Some(code) if code == libc::ESRCH || code == libc::ECHILD)
    }
    #[cfg(windows)]
    {
        error.raw_os_error() == Some(1168)
    }
}

/// Observe an owned Unix child exit without consuming its wait status.
///
/// The caller must own the direct child identified by `pid`. `ECHILD` is
/// returned unchanged when another waiter already consumed the status, so
/// lifecycle owners can distinguish expected recovery races from liveness.
#[cfg(unix)]
pub fn process_has_exited_without_reap(pid: u32, label: &str) -> io::Result<bool> {
    if pid == 0 || pid > i32::MAX as u32 {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            format!("{label} pid {pid} is not a valid Unix child pid"),
        ));
    }

    // SAFETY: waitid writes one initialized siginfo_t, P_PID restricts the
    // query to the caller-owned direct child, and WNOWAIT preserves its status.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result != 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ECHILD) {
            return Err(error);
        }
        return Err(io::Error::new(
            error.kind(),
            format!("failed to observe {label} pid {pid} without reaping: {error}"),
        ));
    }
    // SAFETY: a successful waitid initialized siginfo_t; zero is WNOHANG.
    Ok(unsafe { info.si_pid() } != 0)
}

fn spawn_capture<R>(mut reader: R, tail: OutputTail) -> JoinHandle<()>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut buffer = [0_u8; 8192];
        loop {
            match reader.read(&mut buffer).await {
                Ok(0) => break,
                Ok(read) => tail.append(&buffer[..read]),
                Err(error) => {
                    tail.record_error(&error);
                    break;
                }
            }
        }
    })
}

async fn finish_capture_task(task: Option<JoinHandle<()>>) {
    let Some(mut task) = task else {
        return;
    };
    if tokio::time::timeout(CAPTURE_DRAIN_WAIT, &mut task)
        .await
        .is_err()
    {
        task.abort();
        let _ = task.await;
    }
}

fn sanitize_output(output: &str, redactions: &[String]) -> String {
    let mut output = output.to_owned();
    for secret in redactions {
        if secret.len() >= 4 {
            output = output.replace(secret, "<redacted>");
        }
    }
    output
        .lines()
        .map(|line| {
            let lower = line.to_ascii_lowercase();
            if [
                "authorization",
                "api_key",
                "apikey",
                "password",
                "passwd",
                "secret",
                "bearer ",
                "token=",
                "token:",
                "token\"",
                "cookie",
                "credential",
            ]
            .iter()
            .any(|marker| lower.contains(marker))
            {
                "<redacted sensitive output line>"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn shell(script: &str) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.args(["-c", script]);
        cmd
    }

    #[cfg(unix)]
    fn pid_is_alive(pid: u32) -> bool {
        // SAFETY: signal 0 performs an existence/permission check only.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }

    #[cfg(unix)]
    async fn wait_for_pid_file(path: &std::path::Path) -> u32 {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            if let Ok(raw) = tokio::fs::read_to_string(path).await
                && let Ok(pid) = raw.trim().parse()
            {
                return pid;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for pid file {}",
                path.display()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[cfg(unix)]
    async fn wait_until_gone(pid: u32) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while pid_is_alive(pid) && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!pid_is_alive(pid), "pid {pid} leaked");
    }

    #[cfg(unix)]
    #[test]
    fn non_reaping_exit_observation_validates_pid() {
        let error = process_has_exited_without_reap(0, "invalid fixture")
            .expect_err("zero pid must be rejected");
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert!(error.to_string().contains("invalid fixture pid 0"));
    }

    #[cfg(unix)]
    #[test]
    fn non_reaping_exit_observation_preserves_wait_status() {
        let mut command = std::process::Command::new("/bin/sh");
        command.args(["-c", "exit 23"]);
        echo_agent_tty_utils::detach_std_command(&mut command);
        #[allow(clippy::disallowed_methods)] // test fixture; the test reaps it
        let mut child = command.spawn().expect("spawn observation fixture");
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !process_has_exited_without_reap(child.id(), "observation fixture")
            .expect("observe child")
        {
            assert!(
                std::time::Instant::now() < deadline,
                "observation fixture did not exit"
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        assert_eq!(child.wait().expect("reap observed child").code(), Some(23));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn direct_exit_captures_status_and_output_tails() {
        let sandbox = TestSandbox::new();
        let mut process = TestProcess::spawn(
            shell("printf 'stdout-final'; printf 'stderr-final' >&2; exit 7"),
            &sandbox,
            TestProcessConfig::new().label("direct-exit"),
        )
        .expect("spawn direct child");

        let status = process
            .wait_with_deadline(Duration::from_secs(3))
            .await
            .expect("wait direct child")
            .expect("direct child timed out");

        assert_eq!(status.code(), Some(7));
        assert_eq!(process.status(), Some(status));
        assert_eq!(
            process.termination_reason(),
            Some(TestProcessTermination::NaturalExit)
        );
        assert_eq!(process.stdout_tail().text, "stdout-final");
        assert_eq!(process.stderr_tail().text, "stderr-final");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn wait_timeout_then_hard_kill_reaps_grandchild_tree() {
        let sandbox = TestSandbox::new();
        let pid_file = sandbox.temp_dir().join("grandchild.pid");
        let mut process = TestProcess::spawn(
            shell("sleep 1000 & echo $! > \"$PID_FILE\"; wait"),
            &sandbox,
            TestProcessConfig::new()
                .label("grandchild-timeout")
                .env("PID_FILE", &pid_file),
        )
        .expect("spawn child tree");
        let grandchild_pid = wait_for_pid_file(&pid_file).await;

        assert!(
            process
                .wait_with_deadline(Duration::from_millis(50))
                .await
                .expect("deadline wait")
                .is_none()
        );
        process.kill().await.expect("hard-kill child tree");

        wait_until_gone(grandchild_pid).await;
        assert_eq!(
            process.termination_reason(),
            Some(TestProcessTermination::HardKill)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn graceful_termination_exits_without_escalation() {
        let sandbox = TestSandbox::new();
        let ready_file = sandbox.temp_dir().join("term-ready.pid");
        let mut process = TestProcess::spawn(
            shell("trap 'exit 0' TERM; echo $$ > \"$READY_FILE\"; while :; do sleep 1; done"),
            &sandbox,
            TestProcessConfig::new()
                .label("handle-term")
                .env("READY_FILE", &ready_file),
        )
        .expect("spawn TERM-handling child");
        wait_for_pid_file(&ready_file).await;

        let status = process.close().await.expect("graceful close");
        assert!(status.success());
        assert_eq!(
            process.termination_reason(),
            Some(TestProcessTermination::GracefulTerminate)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn graceful_termination_escalates_when_sigterm_is_ignored() {
        let sandbox = TestSandbox::new();
        let ready_file = sandbox.temp_dir().join("ignore-term-ready.pid");
        let mut process = TestProcess::spawn(
            shell("trap '' TERM; echo $$ > \"$READY_FILE\"; while :; do sleep 1; done"),
            &sandbox,
            TestProcessConfig::new()
                .label("ignore-term")
                .env("READY_FILE", &ready_file)
                .grace_period(Duration::from_millis(100)),
        )
        .expect("spawn TERM-resistant child");
        wait_for_pid_file(&ready_file).await;

        process.close().await.expect("close with hard fallback");
        assert_eq!(
            process.termination_reason(),
            Some(TestProcessTermination::HardKillAfterGrace)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bounded_tail_and_diagnostics_report_truncation_without_secrets() {
        let mut sandbox = TestSandbox::new();
        sandbox.set_env("CUSTOM_TOKEN", "do-not-print-this-token");
        let mut process = TestProcess::spawn(
            shell(
                "printf '%0256d' 0; printf 'stdout-final'; \
                 printf 'CUSTOM_TOKEN=%s\\nstderr-final' \"$CUSTOM_TOKEN\" >&2",
            ),
            &sandbox,
            TestProcessConfig::new()
                .label("bounded-output")
                .tail_bytes(64),
        )
        .expect("spawn bounded-output child");
        process
            .wait_with_deadline(Duration::from_secs(3))
            .await
            .expect("wait bounded-output child")
            .expect("bounded-output child timed out");

        let stdout = process.stdout_tail();
        assert!(stdout.truncated);
        assert!(stdout.bytes_seen > 64);
        assert!(stdout.text.ends_with("stdout-final"));
        let diagnostics = process.diagnostic_summary();
        assert!(diagnostics.contains("stdout_truncated=true"));
        assert!(diagnostics.contains("stdout-final"));
        assert!(diagnostics.contains("stderr-final"));
        assert!(!diagnostics.contains("do-not-print-this-token"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn panic_like_owner_drop_kills_direct_child_and_grandchild() {
        let sandbox = TestSandbox::new();
        let pid_file = sandbox.temp_dir().join("drop-grandchild.pid");
        let process = TestProcess::spawn(
            shell("sleep 1000 & echo $! > \"$PID_FILE\"; wait"),
            &sandbox,
            TestProcessConfig::new()
                .label("panic-drop")
                .env("PID_FILE", &pid_file),
        )
        .expect("spawn panic-drop child tree");
        let direct_pid = process.pid().expect("live direct child pid");
        let grandchild_pid = wait_for_pid_file(&pid_file).await;
        let mut owner = Some(process);

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let process_owner = owner.take().expect("process owner");
            drop(process_owner);
            panic!("simulated owner panic");
        }));
        assert!(result.is_err());

        wait_until_gone(direct_pid).await;
        wait_until_gone(grandchild_pid).await;
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_job_kill_reaps_spawned_grandchild() {
        let sandbox = TestSandbox::new();
        let pid_file = sandbox.temp_dir().join("windows-grandchild.pid");
        let mut cmd = tokio::process::Command::new("powershell.exe");
        cmd.args([
            "-NoProfile",
            "-Command",
            "$p = Start-Process powershell.exe -ArgumentList '-NoProfile','-Command','Start-Sleep 1000' -PassThru; Set-Content -NoNewline -Path $env:PID_FILE -Value $p.Id; Wait-Process -Id $p.Id",
        ]);
        let mut process = TestProcess::spawn(
            cmd,
            &sandbox,
            TestProcessConfig::new()
                .label("windows-job-tree")
                .env("PID_FILE", &pid_file),
        )
        .expect("spawn Windows job tree");
        if !process.tree.is_attached() {
            eprintln!(
                "SKIP: Windows Job attachment is best effort: {}",
                process.diagnostic_summary()
            );
            process
                .kill()
                .await
                .expect("clean unattached Windows child");
            return;
        }

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let grandchild_pid = loop {
            if let Ok(raw) = tokio::fs::read_to_string(&pid_file).await
                && let Ok(pid) = raw.trim().parse::<u32>()
            {
                break pid;
            }
            assert!(tokio::time::Instant::now() < deadline, "pid file timeout");
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        process.kill().await.expect("kill Windows job tree");

        let mut verify = tokio::process::Command::new("powershell.exe");
        verify.args([
            "-NoProfile",
            "-Command",
            "if (Get-Process -Id $env:CHILD_PID -ErrorAction SilentlyContinue) { exit 1 }",
        ]);
        let mut verify = TestProcess::spawn(
            verify,
            &sandbox,
            TestProcessConfig::new()
                .label("verify-windows-job")
                .env("CHILD_PID", grandchild_pid.to_string()),
        )
        .expect("spawn Windows job verifier");
        let status = verify
            .wait_with_deadline(Duration::from_secs(5))
            .await
            .expect("wait Windows job verifier")
            .expect("Windows job verifier timed out");
        assert!(status.success(), "grandchild survived Job Object kill");
    }
}
