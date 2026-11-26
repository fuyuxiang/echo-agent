//! Load environment variables from a directory's `.envrc`: `direnv export
//! json` when available, else bash with direnv stubs.
//!
//! Every wait `Command::output()` used to hide is bounded here — exit wait,
//! pipe drain, buffer cap, reap — because one blocked read froze session
//! load. The output sentinel proves a capture is complete (not that a
//! concurrent descendant kept quiet), so timing can only cost the
//! environment, never install a truncated one.

use std::collections::HashMap;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const ENVRC_LOAD_TIMEOUT: Duration = Duration::from_secs(10);
const ENVRC_TIMEOUT_ENV: &str = "ECHO_AGENT_ENVRC_TIMEOUT_SECS"; // seconds; 0 disables
const MAX_TIMEOUT: Duration = Duration::from_secs(3600);
const POLL_INTERVAL: Duration = Duration::from_millis(25);
const PIPE_DRAIN_GRACE: Duration = Duration::from_millis(250); // of silence
const PIPE_DRAIN_CAP: Duration = Duration::from_secs(2);
const MAX_DRAIN_BYTES: usize = 4 * 1024 * 1024;

/// Prefix only; each run appends a nonce so no env entry can forge the anchor.
const OUTPUT_SENTINEL: &str = "__ECHO_AGENT_ENVRC_COMPLETE__";

/// Loader-side slack over the evaluator deadline (covers a wedged stat).
pub const JOIN_SLACK: Duration = Duration::from_secs(10);

pub fn effective_timeout() -> Duration {
    timeout_from(std::env::var(ENVRC_TIMEOUT_ENV).ok().as_deref())
}

/// Total budget callers should allow an in-flight load.
pub fn loader_budget() -> Duration {
    effective_timeout() + JOIN_SLACK
}

/// A `.envrc` evaluation in flight on a dedicated thread, never the
/// caller's; the deadline anchors at spawn, not at the join.
pub struct EnvrcLoad {
    rx: Option<tokio::sync::oneshot::Receiver<HashMap<String, String>>>,
    deadline: tokio::time::Instant,
}

pub fn spawn_envrc_load(cwd: std::path::PathBuf, trusted: bool) -> EnvrcLoad {
    let deadline = tokio::time::Instant::now() + loader_budget();
    if !trusted {
        return EnvrcLoad { rx: None, deadline };
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name("envrc-load".into())
        .spawn(move || {
            let _ = tx.send(load_envrc_or_empty(&cwd));
        });
    match spawned {
        Ok(_) => EnvrcLoad {
            rx: Some(rx),
            deadline,
        },
        Err(e) => {
            tracing::warn!(?e, "failed to spawn envrc loader thread");
            EnvrcLoad { rx: None, deadline }
        }
    }
}

impl EnvrcLoad {
    pub async fn join(self) -> HashMap<String, String> {
        let Some(rx) = self.rx else {
            return Default::default();
        };
        match tokio::time::timeout_at(self.deadline, rx).await {
            Ok(Ok(env)) => env,
            Ok(Err(_)) => {
                tracing::warn!("envrc loader thread died without a result");
                Default::default()
            }
            Err(_) => {
                tracing::warn!("envrc loader exceeded its budget; continuing without .envrc");
                Default::default()
            }
        }
    }
}

fn timeout_from(overriding: Option<&str>) -> Duration {
    let trimmed = overriding.map(str::trim).unwrap_or_default();
    if trimmed.is_empty() {
        return ENVRC_LOAD_TIMEOUT;
    }
    match trimmed.parse::<u64>() {
        Ok(secs) => {
            let capped = Duration::from_secs(secs).min(MAX_TIMEOUT);
            if capped.as_secs() < secs {
                tracing::warn!(secs, "clamping {ENVRC_TIMEOUT_ENV} to one hour");
            }
            capped
        }
        Err(_) => {
            tracing::warn!(value = trimmed, "ignoring unparseable {ENVRC_TIMEOUT_ENV}");
            ENVRC_LOAD_TIMEOUT
        }
    }
}

/// Stub implementations of common direnv helper functions.
/// These are prepended to the .envrc before execution when direnv is not available.
const DIRENV_STUBS: &str = r#"
# Stub direnv helper functions
source_up_if_exists() { :; }
source_up() { :; }
source_env_if_exists() {
    if [ -f "$1" ]; then
        . "$1"
    fi
}
source_env() {
    if [ -f "$1" ]; then
        . "$1"
    fi
}
PATH_add() {
    export PATH="$PWD/$1:$PATH"
}
path_add() {
    PATH_add "$@"
}
layout() { :; }
use() { :; }
watch_file() { :; }
"#;

pub fn load_envrc(dir: &Path) -> Option<HashMap<String, String>> {
    load_envrc_with_timeout(dir, effective_timeout())
}

pub fn load_envrc_or_empty(dir: &Path) -> HashMap<String, String> {
    load_envrc(dir).unwrap_or_default()
}

fn load_envrc_with_timeout(dir: &Path, timeout: Duration) -> Option<HashMap<String, String>> {
    if timeout.is_zero() {
        tracing::info!(".envrc evaluation disabled by zero {ENVRC_TIMEOUT_ENV}");
        return None;
    }
    // Anchored before the stat: a wedged stat eats the budget instead of
    // granting evaluation a fresh one after the caller gave up.
    let deadline = Instant::now() + timeout;
    let envrc_path = dir.join(".envrc");
    // Opening a FIFO for read blocks until a writer appears.
    match std::fs::metadata(&envrc_path) {
        Err(_) => {
            tracing::debug!(?dir, ".envrc not found");
            return None;
        }
        Ok(m) if !m.is_file() => {
            tracing::warn!(?envrc_path, "refusing to evaluate non-regular .envrc");
            return None;
        }
        Ok(_) => {}
    }
    if Instant::now() >= deadline {
        tracing::warn!(?envrc_path, ".envrc stat consumed the evaluation budget");
        return None;
    }

    match try_direnv_export(dir, deadline) {
        DirenvExport::Env(env) => Some(env),
        DirenvExport::TimedOut => None,
        DirenvExport::SideEffectsRan => None,
        DirenvExport::Unavailable => load_envrc_via_bash(dir, deadline),
    }
}

enum DirenvExport {
    Env(HashMap<String, String>),
    /// Deadline hit; bash would block on the same file.
    TimedOut,
    /// Ran but output was unusable; bash must not re-run the side effects.
    SideEffectsRan,
    /// direnv missing or produced nothing; bash may evaluate.
    Unavailable,
}

fn try_direnv_export(dir: &Path, deadline: Instant) -> DirenvExport {
    let mut cmd = Command::new("direnv");
    cmd.args(["export", "json"]).current_dir(dir);
    let output = match run_with_deadline(cmd, deadline, "direnv") {
        RunOutcome::Completed {
            output,
            truncated: false,
        } => output,
        RunOutcome::Completed {
            truncated: true, ..
        } => {
            tracing::warn!(?dir, "direnv output capture incomplete; skipping .envrc");
            return DirenvExport::SideEffectsRan;
        }
        RunOutcome::TimedOut => return DirenvExport::TimedOut,
        RunOutcome::Failed => return DirenvExport::Unavailable,
    };

    if !output.status.success() {
        // direnv not allowed, or other error
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.contains("not allowed") {
            tracing::debug!(?dir, %stderr, "direnv export failed");
        }
        return DirenvExport::Unavailable;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.trim().is_empty() {
        // No changes from direnv
        return DirenvExport::Unavailable;
    }

    // Parse JSON output: {"VAR": "value", ...}
    // Note: direnv also outputs null for vars to unset, but we ignore those
    match serde_json::from_str::<HashMap<String, serde_json::Value>>(&stdout) {
        Ok(json) => {
            let env: HashMap<String, String> = json
                .into_iter()
                .filter_map(|(k, v)| {
                    if let serde_json::Value::String(s) = v {
                        Some((k, s))
                    } else {
                        None // Skip null values (unset)
                    }
                })
                .collect();

            if env.is_empty() {
                DirenvExport::Unavailable
            } else {
                tracing::info!(?dir, count = env.len(), "Loaded environment via direnv");
                DirenvExport::Env(env)
            }
        }
        Err(e) => {
            tracing::warn!(?dir, ?e, "Failed to parse direnv JSON output");
            DirenvExport::SideEffectsRan
        }
    }
}

/// Load environment by running .envrc in a bash subshell.
/// This is the fallback when direnv is not installed.
fn load_envrc_via_bash(dir: &Path, deadline: Instant) -> Option<HashMap<String, String>> {
    if Instant::now() >= deadline {
        return None;
    }
    let envrc_path = dir.join(".envrc");

    // Build a script that:
    // 1. Includes direnv stubs
    // 2. Sources the .envrc
    // 3. Outputs all env vars as KEY=VALUE pairs (null-separated for safety)
    let sentinel = format!("{OUTPUT_SENTINEL}{}", uuid::Uuid::new_v4().simple());
    let script = format!(
        r#"
set -e
cd "{dir}"
{stubs}
. "{envrc}"
# Output all environment variables, null-separated, then prove completeness
env -0
printf '%s' '{sentinel}'
"#,
        dir = dir.display(),
        stubs = DIRENV_STUBS,
        envrc = envrc_path.display(),
    );

    // Capture baseline environment (before running .envrc)
    let baseline: HashMap<String, String> = std::env::vars().collect();

    // Run the script and capture output
    let mut bash_cmd = Command::new("/bin/bash");
    bash_cmd.arg("-c").arg(&script).current_dir(dir);
    let output = match run_with_deadline(bash_cmd, deadline, "bash") {
        // `truncated` is ignored here: the sentinel below is strictly
        // stronger evidence of completeness.
        RunOutcome::Completed { output, .. } if !output.status.success() => {
            tracing::warn!(?envrc_path, "Failed to execute .envrc via bash");
            return None;
        }
        RunOutcome::Completed { output, .. } => output,
        RunOutcome::TimedOut => return None,
        RunOutcome::Failed => {
            tracing::warn!(?envrc_path, "Failed to run bash for .envrc");
            return None;
        }
    };

    // The NUL anchor rejects a capture cut on an entry boundary; bytes after
    // the sentinel (a descendant still writing) are ignored.
    let mut anchored = Vec::with_capacity(sentinel.len() + 1);
    anchored.push(0);
    anchored.extend_from_slice(sentinel.as_bytes());
    let stdout_bytes = match output
        .stdout
        .windows(anchored.len())
        .position(|window| window == anchored.as_slice())
    {
        Some(sentinel_at) => &output.stdout[..sentinel_at],
        None if output.stdout.starts_with(sentinel.as_bytes()) => {
            tracing::debug!(?envrc_path, ".envrc produced no output");
            return None;
        }
        None => {
            tracing::warn!(?envrc_path, ".envrc output capture incomplete; discarding");
            return None;
        }
    };

    if stdout_bytes.is_empty() {
        tracing::debug!(?envrc_path, ".envrc produced no output");
        return None;
    }

    // Parse the null-separated KEY=VALUE pairs
    let stdout = String::from_utf8_lossy(stdout_bytes);
    let mut result: HashMap<String, String> = HashMap::new();

    for entry in stdout.split('\0') {
        if entry.is_empty() {
            continue;
        }
        if let Some((key, value)) = entry.split_once('=') {
            // Skip internal/noise variables
            let ignored_keys = ["_", "SHLVL", "PWD", "OLDPWD"];
            if ignored_keys.contains(&key) {
                continue;
            }
            // Only include vars that are new or changed from baseline
            match baseline.get(key) {
                Some(baseline_value) if baseline_value == value => {
                    // Unchanged, skip
                }
                _ => {
                    // New or changed
                    result.insert(key.to_string(), value.to_string());
                }
            }
        }
    }

    if result.is_empty() {
        tracing::debug!(?envrc_path, "No environment changes from .envrc");
        None
    } else {
        tracing::info!(
            ?envrc_path,
            count = result.len(),
            "Loaded environment from .envrc via bash"
        );
        Some(result)
    }
}

enum RunOutcome {
    Completed { output: Output, truncated: bool },
    TimedOut,
    Failed,
}

/// Run an evaluator until `deadline`, killing its process group on expiry.
fn run_with_deadline(mut cmd: Command, deadline: Instant, label: &str) -> RunOutcome {
    let budget = deadline.saturating_duration_since(Instant::now());
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    echo_agent_tools::util::detach_std_command(&mut cmd);
    #[allow(clippy::disallowed_methods)] // best-effort enrolled in the global ProcessScope below
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            tracing::debug!(label, ?e, "failed to spawn .envrc evaluator");
            return RunOutcome::Failed;
        }
    };

    let mut group = echo_agent_tools::util::ProcessGroup::new().ok();
    if let Some(g) = group.as_mut()
        && g.attach_std(&child).is_err()
    {
        group = None;
    }
    let group = group.map(Arc::new);
    // Kill before reaping so the group id cannot be recycled.
    let kill_and_reap = |child: &mut std::process::Child| {
        let group_killed = group.as_ref().is_some_and(|g| g.kill().is_ok());
        if !group_killed {
            let _ = child.kill();
        }
        reap_with_timeout(child, label);
    };
    // Refused registration = scope closed; don't trust its best-effort kill.
    if let Some(g) = &group
        && !echo_agent_tools::util::global_process_scope().register(g)
    {
        kill_and_reap(&mut child);
        return RunOutcome::Failed;
    }

    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        kill_and_reap(&mut child);
        return RunOutcome::Failed;
    };
    let stdout = PipeDrain::start(stdout);
    let stderr = PipeDrain::start(stderr);

    let remaining = deadline.saturating_duration_since(Instant::now());
    let status = match wait_timeout::ChildExt::wait_timeout(&mut child, remaining) {
        Ok(Some(status)) => status,
        Ok(None) => {
            tracing::warn!(
                label,
                budget_ms = budget.as_millis() as u64,
                "`.envrc` evaluation timed out; continuing without its environment \
                 (set {ENVRC_TIMEOUT_ENV} to extend)"
            );
            kill_and_reap(&mut child);
            return RunOutcome::TimedOut;
        }
        Err(e) => {
            tracing::warn!(label, ?e, "failed to wait for .envrc evaluator");
            kill_and_reap(&mut child);
            return RunOutcome::Failed;
        }
    };

    // Drop the enrollment at reap (the PID-reuse contract); descendants
    // deliberately survive on Unix, die with the Job Object on Windows.
    drop(group);

    let cap = Instant::now() + PIPE_DRAIN_CAP;
    let (stdout, stdout_cut) = stdout.finish(cap);
    let (stderr, _) = stderr.finish(cap);
    RunOutcome::Completed {
        output: Output {
            status,
            stdout,
            stderr,
        },
        truncated: stdout_cut,
    }
}

/// Captures a pipe on a helper thread; output survives a pipe a descendant
/// holds open past EOF. Unix reads are non-blocking (`done` cancels the
/// thread); elsewhere a blocked reader detaches and the Job Object closes
/// the pipe on drop.
struct PipeDrain {
    buf: Arc<Mutex<Vec<u8>>>,
    done: Arc<AtomicBool>,
    truncated: Arc<AtomicBool>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl PipeDrain {
    #[cfg(unix)]
    fn start(mut pipe: impl std::io::Read + std::os::fd::AsRawFd + Send + 'static) -> Self {
        let fd = pipe.as_raw_fd();
        // Our read end only; the child's write end stays blocking.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
        Self::spawn_reader(move |stop, sink, cut| {
            let mut chunk = [0u8; 8192];
            loop {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let mut pollfd = libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                };
                let ready = unsafe { libc::poll(&mut pollfd, 1, POLL_INTERVAL.as_millis() as i32) };
                if ready <= 0 {
                    continue; // timeout or EINTR; re-check `stop`
                }
                match pipe.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        let mut buf = lock_ignore_poison(&sink);
                        if buf.len() + n > MAX_DRAIN_BYTES {
                            cut.store(true, Ordering::Relaxed);
                            break;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                    }
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            || e.kind() == std::io::ErrorKind::Interrupted =>
                    {
                        continue;
                    }
                    Err(_) => break,
                }
            }
        })
    }

    #[cfg(not(unix))]
    fn start(mut pipe: impl std::io::Read + Send + 'static) -> Self {
        Self::spawn_reader(move |stop, sink, cut| {
            let mut chunk = [0u8; 8192];
            loop {
                match pipe.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if stop.load(Ordering::Relaxed) {
                            cut.store(true, Ordering::Relaxed);
                            break;
                        }
                        let mut buf = lock_ignore_poison(&sink);
                        if buf.len() + n > MAX_DRAIN_BYTES {
                            cut.store(true, Ordering::Relaxed);
                            break;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                    }
                }
            }
        })
    }

    fn spawn_reader(
        read_loop: impl FnOnce(Arc<AtomicBool>, Arc<Mutex<Vec<u8>>>, Arc<AtomicBool>) + Send + 'static,
    ) -> Self {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let done = Arc::new(AtomicBool::new(false));
        let truncated = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&done);
        let sink = Arc::clone(&buf);
        let cut = Arc::clone(&truncated);
        let reader = std::thread::Builder::new()
            .name("envrc-pipe".into())
            .spawn(move || read_loop(stop, sink, cut))
            .ok();
        Self {
            buf,
            done,
            truncated,
            reader,
        }
    }

    /// Take what arrived by EOF, `cap`, or [`PIPE_DRAIN_GRACE`] of silence;
    /// missing EOF alone is not truncation.
    fn finish(mut self, cap: Instant) -> (Vec<u8>, bool) {
        let spawned = self.reader.is_some();
        if let Some(reader) = &self.reader {
            let mut quiet_since = Instant::now();
            let mut last_len = lock_ignore_poison(&self.buf).len();
            while !reader.is_finished()
                && Instant::now() < cap
                && quiet_since.elapsed() < PIPE_DRAIN_GRACE
            {
                std::thread::sleep(POLL_INTERVAL);
                let len = lock_ignore_poison(&self.buf).len();
                if len != last_len {
                    last_len = len;
                    quiet_since = Instant::now();
                }
            }
        }
        self.done.store(true, Ordering::Relaxed);
        #[cfg(unix)]
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        let buf = std::mem::take(&mut *lock_ignore_poison(&self.buf));
        let truncated = self.truncated.load(Ordering::Relaxed) || !spawned;
        (buf, truncated)
    }
}

impl Drop for PipeDrain {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Relaxed);
    }
}

/// Reap a killed child; abandon a D-state corpse (the zombie pins its pid).
fn reap_with_timeout(child: &mut std::process::Child, label: &str) {
    if let Ok(None) =
        wait_timeout::ChildExt::wait_timeout(child, echo_agent_tty_utils::KILL_REAP_TIMEOUT)
    {
        tracing::warn!(
            label,
            pid = child.id(),
            "abandoning unreapable .envrc evaluator"
        );
    }
}

fn lock_ignore_poison(buf: &Mutex<Vec<u8>>) -> std::sync::MutexGuard<'_, Vec<u8>> {
    buf.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_simple_export() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(".envrc"), "export FOO=bar\n").unwrap();

        let env = load_envrc_with_timeout(dir.path(), Duration::from_secs(10)).unwrap();
        assert_eq!(env.get("FOO"), Some(&"bar".to_string()));
    }

    #[test]
    fn test_no_envrc() {
        let dir = TempDir::new().unwrap();
        assert!(load_envrc_with_timeout(dir.path(), Duration::from_secs(10)).is_none());
    }

    #[test]
    fn test_path_add() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(".envrc"), "PATH_add bin\n").unwrap();

        let env = load_envrc_with_timeout(dir.path(), Duration::from_secs(10)).unwrap();
        let path = env.get("PATH").unwrap();
        assert!(path.contains(&format!("{}/bin", dir.path().display())));
    }

    /// Hangs if the evaluator wait becomes unbounded again.
    #[test]
    fn hung_evaluation_fails_open_at_the_deadline() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(".envrc"), "sleep 300\n").unwrap();

        let started = Instant::now();
        assert!(load_envrc_with_timeout(dir.path(), Duration::from_millis(500)).is_none());
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn zero_timeout_disables_evaluation() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(".envrc"), "export FOO=bar\n").unwrap();

        let started = Instant::now();
        assert!(load_envrc_with_timeout(dir.path(), Duration::ZERO).is_none());
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn timeout_override_parses_and_clamps() {
        assert_eq!(timeout_from(None), ENVRC_LOAD_TIMEOUT);
        assert_eq!(timeout_from(Some("")), ENVRC_LOAD_TIMEOUT);
        assert_eq!(timeout_from(Some("  ")), ENVRC_LOAD_TIMEOUT);
        assert_eq!(timeout_from(Some(" 3 ")), Duration::from_secs(3));
        assert_eq!(timeout_from(Some("0")), Duration::ZERO);
        assert_eq!(timeout_from(Some("not-a-number")), ENVRC_LOAD_TIMEOUT);
        assert_eq!(timeout_from(Some(&u64::MAX.to_string())), MAX_TIMEOUT);
    }

    /// A descendant holding the pipe open must not cost the environment.
    #[cfg(unix)]
    #[test]
    fn background_child_does_not_discard_env() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(".envrc"), "export FOO=bar\nsleep 5 &\n").unwrap();

        let env = load_envrc_with_timeout(dir.path(), Duration::from_secs(10)).unwrap();
        assert_eq!(env.get("FOO"), Some(&"bar".to_string()));
    }

    /// Descendant output after the sentinel must be ignored, not treated as
    /// an incomplete capture.
    #[cfg(unix)]
    #[test]
    fn chatty_background_child_does_not_discard_env() {
        let dir = TempDir::new().unwrap();
        // The leading sleep keeps the noise out of the race with `env -0`
        // and the sentinel; the writes land during the drain window.
        fs::write(
            dir.path().join(".envrc"),
            "export FOO=bar\n( sleep 0.25; for _ in 1 2 3 4; do echo noise; sleep 0.1; done ) &\n",
        )
        .unwrap();

        let env = load_envrc_with_timeout(dir.path(), Duration::from_secs(10)).unwrap();
        assert_eq!(env.get("FOO"), Some(&"bar".to_string()));
    }

    /// An env entry named like the sentinel must not truncate the capture.
    #[cfg(unix)]
    #[test]
    fn sentinel_named_variable_does_not_truncate_env() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(".envrc"),
            "export __ECHO_AGENT_ENVRC_COMPLETE__=decoy\nexport ZZ_AFTER_DECOY=survives\n",
        )
        .unwrap();

        let env = load_envrc_with_timeout(dir.path(), Duration::from_secs(10)).unwrap();
        assert_eq!(
            env.get("__ECHO_AGENT_ENVRC_COMPLETE__"),
            Some(&"decoy".to_string())
        );
        assert_eq!(env.get("ZZ_AFTER_DECOY"), Some(&"survives".to_string()));
    }

    /// The guard must refuse a FIFO without opening it.
    #[cfg(unix)]
    #[test]
    fn fifo_envrc_is_refused() {
        let dir = TempDir::new().unwrap();
        assert!(
            std::process::Command::new("mkfifo")
                .arg(dir.path().join(".envrc"))
                .status()
                .unwrap()
                .success()
        );

        let started = Instant::now();
        assert!(load_envrc_with_timeout(dir.path(), Duration::from_secs(10)).is_none());
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
