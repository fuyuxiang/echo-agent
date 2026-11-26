//! Git marketplace source support.
//!
//! Provides persistent caching of git marketplace repos.
//! Cache root: `~/.echo-agent/marketplace-cache/<url-hash>/`

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use fs2::FileExt;

/// Default TTL for marketplace cache freshness (5 minutes).
const CACHE_TTL: Duration = Duration::from_secs(5 * 60);
const LOCK_TIMEOUT: Duration = Duration::from_secs(30);
const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// Hard cap for clone/fetch so a bad marketplace URL cannot hang list/refresh.
const NETWORK_OP_TIMEOUT: Duration = Duration::from_secs(15);
const STDERR_DIAGNOSTIC_CAP: usize = 64 * 1024;
const STDERR_DIAGNOSTIC_TAIL_CAP: usize = STDERR_DIAGNOSTIC_CAP / 2;
const STDERR_TRUNCATION_MARKER: &[u8] = b"\n[... git stderr truncated ...]\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncMode {
    UseTtl,
    Force,
}

pub struct SourceCacheLease {
    pub path: PathBuf,
    lock_file: File,
}

impl Drop for SourceCacheLease {
    fn drop(&mut self) {
        let _ = self.lock_file.unlock();
    }
}

/// Sync a git marketplace source to the persistent cache.
///
/// Returns the path to the cached repo on success.
pub fn sync_source_cache(
    url: &str,
    branch: Option<&str>,
    cache_root: &Path,
) -> Result<PathBuf, String> {
    let lease = sync_source_cache_with_mode(url, branch, cache_root, SyncMode::UseTtl)?;
    Ok(lease.path.clone())
}

pub fn force_sync_source_cache(
    url: &str,
    branch: Option<&str>,
    cache_root: &Path,
) -> Result<PathBuf, String> {
    let lease = sync_source_cache_with_mode(url, branch, cache_root, SyncMode::Force)?;
    Ok(lease.path.clone())
}

pub fn sync_source_cache_with_mode(
    url: &str,
    branch: Option<&str>,
    cache_root: &Path,
    mode: SyncMode,
) -> Result<SourceCacheLease, String> {
    let url = echo_agent_core::plugins::git_install::validate_git_url(url)?;
    let branch = branch
        .map(echo_agent_core::plugins::git_install::validate_git_ref)
        .transpose()?;
    let hash = cache_hash(url);
    let cache_dir = cache_root.join(&hash);
    let start = Instant::now();

    std::fs::create_dir_all(cache_root).map_err(|e| format!("failed to create cache root: {e}"))?;
    let lock_file = acquire_cache_lock(&cache_root.join(format!("{hash}.lock")), LOCK_TIMEOUT)?;

    let result = sync_cache_locked(url, branch, &cache_dir, mode);
    match &result {
        Ok(()) => {
            tracing::debug!(mode = ?mode, elapsed_ms = start.elapsed().as_millis(), "marketplace cache sync complete")
        }
        Err(error) => {
            tracing::warn!(mode = ?mode, elapsed_ms = start.elapsed().as_millis(), error = %error, "marketplace cache sync failed")
        }
    }
    result?;

    Ok(SourceCacheLease {
        path: cache_dir,
        lock_file,
    })
}

fn sync_cache_locked(
    url: &str,
    branch: Option<&str>,
    cache_dir: &Path,
    mode: SyncMode,
) -> Result<(), String> {
    let url = echo_agent_core::plugins::git_install::validate_git_url(url)?;
    let branch = branch
        .map(echo_agent_core::plugins::git_install::validate_git_ref)
        .transpose()?;
    if cache_dir.join(".git").exists() {
        if mode == SyncMode::UseTtl && is_cache_fresh(cache_dir) {
            return Ok(());
        }
        fetch_reset_cached_repo(cache_dir, branch).or_else(|e| {
            tracing::warn!(error = %e, "git fetch/reset failed, re-cloning marketplace cache");
            reclone_repo(url, branch, cache_dir)
        })
    } else {
        clone_repo(url, branch, cache_dir)
    }
}

fn acquire_cache_lock(lock_path: &Path, timeout: Duration) -> Result<File, String> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)
        .map_err(|e| format!("failed to open cache lock {}: {e}", lock_path.display()))?;
    let deadline = Instant::now() + timeout;
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(file),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(format!(
                        "cache lock timeout after {}s for {}",
                        timeout.as_secs(),
                        lock_path.display()
                    ));
                }
                std::thread::sleep(LOCK_POLL_INTERVAL);
            }
            Err(e) => return Err(format!("failed to lock cache {}: {e}", lock_path.display())),
        }
    }
}

/// Check if the cache was fetched recently enough to skip fetching.
fn is_cache_fresh(cache_dir: &Path) -> bool {
    let fetch_head = cache_dir.join(".git").join("FETCH_HEAD");
    match std::fs::metadata(&fetch_head) {
        Ok(meta) => meta
            .modified()
            .ok()
            .and_then(|mtime| mtime.elapsed().ok())
            .is_some_and(|age| age < CACHE_TTL),
        Err(_) => false,
    }
}

/// Get the default cache root directory.
pub fn default_cache_root() -> PathBuf {
    echo_agent_config::echo_agent_home().join("marketplace-cache")
}

/// Deterministic hash for a URL (used as cache directory name).
fn cache_hash(url: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    url.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Clone a git repo with depth 1.
///
/// Uses the git CLI (not libgit2): a libgit2 clone cannot be killed on
/// timeout, so a hung remote would pin a thread forever.
fn clone_repo(url: &str, branch: Option<&str>, dest: &Path) -> Result<(), String> {
    let url = echo_agent_core::plugins::git_install::validate_git_url(url)?;
    let branch = branch
        .map(echo_agent_core::plugins::git_install::validate_git_ref)
        .transpose()?;
    let mut cmd = clone_cli_command(url, branch, dest);
    run_git_timed(&mut cmd, "clone", NETWORK_OP_TIMEOUT).inspect_err(|_| {
        let _ = std::fs::remove_dir_all(dest);
    })
}

fn reclone_repo(url: &str, branch: Option<&str>, dest: &Path) -> Result<(), String> {
    let parent = dest
        .parent()
        .ok_or_else(|| format!("cache path has no parent: {}", dest.display()))?;
    let name = dest
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("cache path has no file name: {}", dest.display()))?;
    let suffix = format!("{}-{}", std::process::id(), unique_reclone_suffix());
    let temp_dest = parent.join(format!(".{name}.reclone-{suffix}"));
    let backup_dest = parent.join(format!(".{name}.backup-{suffix}"));

    let _ = std::fs::remove_dir_all(&temp_dest);
    let _ = std::fs::remove_dir_all(&backup_dest);

    clone_repo(url, branch, &temp_dest).inspect_err(|_| {
        let _ = std::fs::remove_dir_all(&temp_dest);
    })?;

    let had_existing = dest.exists();
    if had_existing {
        std::fs::rename(dest, &backup_dest)
            .map_err(|e| format!("failed to move existing cache aside: {e}"))?;
    }

    match std::fs::rename(&temp_dest, dest) {
        Ok(()) => {
            if had_existing {
                let _ = std::fs::remove_dir_all(&backup_dest);
            }
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&temp_dest);
            if had_existing && let Err(restore_err) = std::fs::rename(&backup_dest, dest) {
                return Err(format!(
                    "failed to install recloned cache: {e}; failed to restore original cache: {restore_err}; original cache preserved at {}",
                    backup_dest.display()
                ));
            }
            Err(format!("failed to install recloned cache: {e}"))
        }
    }
}

fn unique_reclone_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

pub use echo_agent_tty_utils::{GIT_AUTH_SUPPRESSION_ENVS, git_command, git_command_locking};

fn clone_cli_command(url: &str, branch: Option<&str>, dest: &Path) -> std::process::Command {
    let mut cmd = git_command();
    cmd.args(["clone", "--depth", "1"]);
    if let Some(b) = branch {
        cmd.args(["--branch", b]);
    }
    cmd.arg("--").arg(url).arg(dest.as_os_str());
    cmd
}

/// Probe whether `url` is a reachable git repository via a timed
/// `git ls-remote`, without touching any cache. Used to reject non-git URLs
/// (e.g. MCP endpoints) at add time instead of persisting a source that
/// fails on every scan.
pub fn probe_git_remote(url: &str) -> Result<(), String> {
    let url = echo_agent_core::plugins::git_install::validate_git_url(url)?;
    let mut cmd = git_command();
    cmd.args(["ls-remote", "--", url, "HEAD"]);
    run_git_timed(&mut cmd, "ls-remote", NETWORK_OP_TIMEOUT)
}

fn fetch_cli_command(repo_dir: &Path, branch: Option<&str>) -> std::process::Command {
    let mut cmd = git_command();
    cmd.current_dir(repo_dir).args([
        "fetch",
        "--depth",
        "1",
        "--",
        "origin",
        branch.unwrap_or("HEAD"),
    ]);
    cmd
}

/// Run a git command, wait up to `timeout`, kill+reap on hang. Errors on
/// timeout or non-zero exit; `what` names the operation in error messages.
fn run_git_timed(cmd: &mut Command, what: &str, timeout: Duration) -> Result<(), String> {
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::piped());
    #[allow(clippy::disallowed_methods)] // enrolled before any waiter thread starts
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to run git {what}: {e}"))?;
    let group = match echo_agent_tty_utils::global_process_scope().enroll_std(&child) {
        Ok(group) => group,
        Err(error) => {
            let _ = child.kill();
            if !matches!(
                echo_agent_tty_utils::wait_child_bounded(
                    &mut child,
                    echo_agent_tty_utils::KILL_REAP_TIMEOUT,
                ),
                Ok(Some(_))
            ) {
                transfer_git_cleanup(
                    what,
                    GitReaperOwners {
                        child: Some((child, None)),
                        stderr: None,
                    },
                );
            }
            return Err(format!(
                "failed to enroll git {what} process group: {error}"
            ));
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => match spawn_stderr_reader(stderr) {
            Ok(reader) => reader,
            Err(error) => {
                kill_git_child(what, child, group, None, CleanupIdentity::Certain);
                return Err(format!("failed to start git {what} stderr reader: {error}"));
            }
        },
        None => {
            kill_git_child(what, child, group, None, CleanupIdentity::Certain);
            return Err(format!("failed to capture git {what} stderr"));
        }
    };

    match echo_agent_tty_utils::wait_child_bounded(&mut child, timeout) {
        Ok(Some(status)) => {
            drop(group);
            finish_git_status(what, status, stderr)
        }
        Ok(None) => {
            let stderr = kill_git_child(what, child, group, Some(stderr), CleanupIdentity::Certain);
            Err(git_message_with_stderr(
                format!("git {what} timed out after {}s", timeout.as_secs()),
                what,
                stderr.as_deref(),
            ))
        }
        Err(error) => {
            let identity = if echo_agent_tty_utils::is_child_wait_identity_uncertain(&error) {
                CleanupIdentity::Uncertain
            } else {
                CleanupIdentity::Certain
            };
            let stderr = kill_git_child(what, child, group, Some(stderr), identity);
            Err(git_message_with_stderr(
                format!("failed to wait for git {what}: {error}"),
                what,
                stderr.as_deref(),
            ))
        }
    }
}

fn finish_git_status(what: &str, status: ExitStatus, stderr: StderrReader) -> Result<(), String> {
    // A successful git exit must not become Err: clone_repo's inspect_err deletes dest.
    let stderr = finish_stderr_or_reap(what, stderr).unwrap_or_default();
    if status.success() {
        return Ok(());
    }
    tracing::debug!(
        operation = what,
        stderr = %String::from_utf8_lossy(&stderr),
        "git command failed"
    );
    Err(git_failure_message(what, &stderr))
}

// Inherited writers (persistent ssh) never EOF; bound the drain and reap a still-blocked reader.
fn finish_stderr_or_reap(what: &str, mut stderr: StderrReader) -> Option<Vec<u8>> {
    match stderr.finish_bounded(echo_agent_tty_utils::KILL_REAP_TIMEOUT) {
        Some(Ok(bytes)) => Some(bytes),
        Some(Err(error)) => {
            tracing::debug!(
                operation = what,
                error = %error,
                "git stderr reader failed"
            );
            None
        }
        None => {
            tracing::debug!(operation = what, "git stderr reader timed out");
            transfer_git_cleanup(
                what,
                GitReaperOwners {
                    child: None,
                    stderr: Some(stderr),
                },
            );
            None
        }
    }
}

#[derive(Clone, Copy)]
enum CleanupIdentity {
    Certain,
    Uncertain,
}

fn kill_git_child(
    what: &str,
    mut child: Child,
    group: Arc<echo_agent_tty_utils::ProcessGroup>,
    stderr: Option<StderrReader>,
    identity: CleanupIdentity,
) -> Option<Vec<u8>> {
    // ECHILD makes numeric group identity unsafe; dropping its owner prevents
    // later scope cleanup from signaling a recycled group.
    let group = match identity {
        CleanupIdentity::Certain => {
            if let Err(group_error) = group.kill()
                && let Err(child_error) = child.kill()
            {
                tracing::warn!(error = %group_error, fallback_error = %child_error, operation = what, "git group and direct-child kill failed");
            }
            Some(group)
        }
        CleanupIdentity::Uncertain => {
            tracing::error!(
                operation = what,
                "git wait returned ECHILD; numeric cleanup forbidden"
            );
            drop(group);
            None
        }
    };
    let child = match echo_agent_tty_utils::wait_child_bounded(
        &mut child,
        echo_agent_tty_utils::KILL_REAP_TIMEOUT,
    ) {
        Ok(Some(_)) => {
            drop(group);
            None
        }
        Ok(None) => Some((child, group)),
        Err(error) => {
            tracing::warn!(error = %error, operation = what, "git bounded reap failed");
            Some((child, group))
        }
    };
    if child.is_some() {
        transfer_git_cleanup(what, GitReaperOwners { child, stderr });
        return None;
    }
    stderr.and_then(|stderr| finish_stderr_or_reap(what, stderr))
}

struct GitReaperOwners {
    child: Option<(Child, Option<Arc<echo_agent_tty_utils::ProcessGroup>>)>,
    stderr: Option<StderrReader>,
}

impl GitReaperOwners {
    fn finish(mut self) {
        if let Some((mut child, group)) = self.child.take() {
            let _ = child.wait();
            drop(group);
        }
        if let Some(stderr) = self.stderr.take() {
            let _ = stderr.finish();
        }
    }
}

fn transfer_git_cleanup(what: &str, mut owners: GitReaperOwners) {
    if owners.stderr.is_none()
        && let Some((child, group)) = owners.child.take()
    {
        if let Err((error, child, group)) =
            echo_agent_tty_utils::spawn_child_reaper("marketplace-git-reaper", child, group)
        {
            tracing::error!(error = %error, operation = what, child_id = child.id(), has_group = group.is_some(), "git cleanup bounded abandonment: reaper thread spawn failed");
        }
        return;
    }
    let owners = Arc::new(std::sync::Mutex::new(Some(owners)));
    let thread_owners = Arc::clone(&owners);
    if let Err(error) = std::thread::Builder::new()
        .name("marketplace-git-reaper".to_owned())
        .spawn(move || {
            let owners = thread_owners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some(owners) = owners {
                owners.finish();
            }
        })
    {
        let owners = owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        tracing::error!(error = %error, operation = what, has_child = owners.as_ref().is_some_and(|owners| owners.child.is_some()), has_reader = owners.as_ref().is_some_and(|owners| owners.stderr.is_some()), "git cleanup bounded abandonment: reaper thread spawn failed");
    }
}

struct StderrReader {
    receiver: Receiver<io::Result<Vec<u8>>>,
    thread: Option<JoinHandle<()>>,
}

impl StderrReader {
    fn finish(mut self) -> io::Result<Vec<u8>> {
        let result = self.receiver.recv().unwrap_or_else(|error| {
            Err(io::Error::other(format!(
                "stderr reader disconnected: {error}"
            )))
        });
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        result
    }

    fn finish_bounded(&mut self, timeout: Duration) -> Option<io::Result<Vec<u8>>> {
        match self.receiver.recv_timeout(timeout) {
            Ok(result) => {
                if let Some(thread) = self.thread.take() {
                    let _ = thread.join();
                }
                Some(result)
            }
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => {
                let _ = self.thread.take().map(JoinHandle::join);
                Some(Err(io::Error::other("stderr reader disconnected")))
            }
        }
    }
}

fn spawn_stderr_reader(mut stderr: ChildStderr) -> io::Result<StderrReader> {
    let (sender, receiver) = mpsc::sync_channel(1);
    let thread = std::thread::Builder::new()
        .name("marketplace-git-stderr".to_string())
        .spawn(move || {
            let mut diagnostic = CappedStderr::new();
            let mut buffer = [0_u8; 8192];
            let result = loop {
                match stderr.read(&mut buffer) {
                    Ok(0) => break Ok(diagnostic.finish()),
                    Ok(read) => diagnostic.push(&buffer[..read]),
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => break Err(error),
                }
            };
            let _ = sender.send(result);
        })?;
    Ok(StderrReader {
        receiver,
        thread: Some(thread),
    })
}

struct CappedStderr {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    is_truncated: bool,
}

impl CappedStderr {
    fn new() -> Self {
        Self {
            head: Vec::new(),
            tail: VecDeque::with_capacity(STDERR_DIAGNOSTIC_TAIL_CAP),
            is_truncated: false,
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        let head_cap = STDERR_DIAGNOSTIC_CAP - STDERR_DIAGNOSTIC_TAIL_CAP;
        let head_remaining = head_cap.saturating_sub(self.head.len());
        let (head, tail) = bytes.split_at(bytes.len().min(head_remaining));
        self.head.extend_from_slice(head);
        if tail.is_empty() {
            return;
        }
        self.is_truncated |=
            self.tail.len().saturating_add(tail.len()) > STDERR_DIAGNOSTIC_TAIL_CAP;
        let keep_start = tail.len().saturating_sub(STDERR_DIAGNOSTIC_TAIL_CAP);
        let tail = &tail[keep_start..];
        let discard = self
            .tail
            .len()
            .saturating_add(tail.len())
            .saturating_sub(STDERR_DIAGNOSTIC_TAIL_CAP);
        self.tail.drain(..discard);
        self.tail.extend(tail);
    }

    fn finish(mut self) -> Vec<u8> {
        if self.is_truncated {
            self.head.extend_from_slice(STDERR_TRUNCATION_MARKER);
        }
        self.head.extend(self.tail);
        self.head
    }
}

/// Condense git stderr into a user-facing failure message. git writes
/// progress ("Cloning into ...") to stderr alongside real errors, so keep
/// only `fatal:`/`error:` lines, and translate the prompts-disabled auth
/// failure (we set GIT_TERMINAL_PROMPT=0 / ssh BatchMode) out of git-speak.
fn git_failure_message(what: &str, stderr: impl AsRef<[u8]>) -> String {
    const AUTH_PATTERNS: [&str; 3] = [
        "could not read Username",
        "could not read Password",
        "Authentication failed",
    ];
    let prefix = format!("git {what} failed: ");
    let message_cap = prefix.len().saturating_add(STDERR_DIAGNOSTIC_CAP);
    let stderr = String::from_utf8_lossy(stderr.as_ref());
    let detail = if AUTH_PATTERNS.iter().any(|pattern| stderr.contains(pattern)) {
        "authentication required or not a git repository (check the URL)".to_owned()
    } else {
        let salient: Vec<&str> = stderr
            .lines()
            .filter(|line| line.starts_with("fatal:") || line.starts_with("error:"))
            .collect();
        if salient.is_empty() {
            stderr.trim().to_owned()
        } else {
            salient.join("; ")
        }
    };
    let mut message = prefix + &detail;
    if message.len() > message_cap {
        let mut end = message_cap;
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        message.truncate(end);
    }
    message
}

fn git_message_with_stderr(prefix: String, what: &str, stderr: Option<&[u8]>) -> String {
    let Some(stderr) = stderr.filter(|bytes| !bytes.is_empty()) else {
        return prefix;
    };
    let failure = git_failure_message(what, stderr);
    let detail_prefix = format!("git {what} failed: ");
    match failure
        .strip_prefix(&detail_prefix)
        .filter(|detail| !detail.is_empty())
    {
        Some(detail) => format!("{prefix}: {detail}"),
        None => prefix,
    }
}

fn fetch_reset_cached_repo(repo_dir: &Path, branch: Option<&str>) -> Result<(), String> {
    let branch = branch
        .map(echo_agent_core::plugins::git_install::validate_git_ref)
        .transpose()?;
    run_git_timed(
        &mut fetch_cli_command(repo_dir, branch),
        "fetch",
        NETWORK_OP_TIMEOUT,
    )?;

    let mut checkout_cmd = git_command();
    checkout_cmd
        .current_dir(repo_dir)
        .args(["checkout", "--detach", "FETCH_HEAD"]);
    run_git_timed(&mut checkout_cmd, "checkout", NETWORK_OP_TIMEOUT)?;

    let mut reset_cmd = git_command();
    reset_cmd
        .current_dir(repo_dir)
        .args(["reset", "--hard", "FETCH_HEAD"]);
    run_git_timed(&mut reset_cmd, "reset", NETWORK_OP_TIMEOUT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_hash_is_deterministic() {
        let url = "https://github.com/fuyuxiang/echo-agent-desktop.git";
        let h1 = cache_hash(url);
        let h2 = cache_hash(url);
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 16);
    }

    #[test]
    fn cache_hash_differs_for_different_urls() {
        let h1 = cache_hash("https://github.com/a/b.git");
        let h2 = cache_hash("https://github.com/c/d.git");
        assert_ne!(h1, h2);
    }

    #[test]
    fn default_cache_root_under_echo_agent() {
        let root = default_cache_root();
        assert!(root.to_string_lossy().contains("marketplace-cache"));
    }

    #[test]
    fn cli_git_args_terminate_options_before_operands() {
        let clone_cmd = clone_cli_command("repo", Some("main"), Path::new("dest"));
        let clone_args: Vec<_> = clone_cmd
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect();
        assert_eq!(
            clone_args,
            [
                "--no-optional-locks",
                "clone",
                "--depth",
                "1",
                "--branch",
                "main",
                "--",
                "repo",
                "dest",
            ]
        );

        let fetch_cmd = fetch_cli_command(Path::new("repo"), Some("main"));
        let fetch_args: Vec<_> = fetch_cmd
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect();
        assert_eq!(
            fetch_args,
            [
                "--no-optional-locks",
                "fetch",
                "--depth",
                "1",
                "--",
                "origin",
                "main",
            ]
        );
    }

    #[test]
    fn invalid_cache_operands_fail_before_cache_root_creation() {
        for (url, branch) in [
            ("--upload-pack=cmd", Some("main")),
            ("https://example.com/repo.git", Some("--upload-pack=cmd")),
        ] {
            let parent = tempfile::tempdir().unwrap();
            let cache_root = parent.path().join("cache");
            assert!(sync_source_cache(url, branch, &cache_root).is_err());
            assert!(!cache_root.exists());
        }
    }

    #[test]
    fn sync_source_cache_uses_ttl_by_default() {
        if !git_available() {
            eprintln!("skipping git-dependent test: git binary not available");
            return;
        }
        let remote = tempfile::tempdir().unwrap();
        init_remote_repo(remote.path());
        let cache_root = tempfile::tempdir().unwrap();
        let url = remote.path().to_string_lossy();

        let cache_dir = sync_source_cache(&url, Some("main"), cache_root.path()).unwrap();
        let fetch_head = cache_dir.join(".git").join("FETCH_HEAD");
        std::fs::write(&fetch_head, "ttl-sentinel").unwrap();
        let second_cache_dir = sync_source_cache(&url, Some("main"), cache_root.path()).unwrap();
        assert_eq!(second_cache_dir, cache_dir);
        assert_eq!(
            std::fs::read_to_string(&fetch_head).unwrap(),
            "ttl-sentinel"
        );
    }

    #[test]
    fn force_sync_source_cache_ignores_fresh_fetch_head() {
        if !git_available() {
            eprintln!("skipping git-dependent test: git binary not available");
            return;
        }
        let remote = tempfile::tempdir().unwrap();
        init_remote_repo(remote.path());
        let cache_root = tempfile::tempdir().unwrap();
        let url = remote.path().to_string_lossy();

        let cache_dir = sync_source_cache(&url, Some("main"), cache_root.path()).unwrap();
        let first_head = current_head(&cache_dir);
        add_commit(remote.path(), "second.txt", "second");

        let forced_cache_dir =
            force_sync_source_cache(&url, Some("main"), cache_root.path()).unwrap();
        assert_eq!(forced_cache_dir, cache_dir);
        assert_ne!(current_head(&cache_dir), first_head);
    }

    #[test]
    fn capped_stderr_separates_gap_and_caps_invalid_utf8_rendering() {
        let mut diagnostic = CappedStderr::new();
        diagnostic.push(b"Authentication ");
        diagnostic.push(&vec![b'x'; STDERR_DIAGNOSTIC_CAP * 2]);
        diagnostic.push(b"failed\nfatal: late\n");
        let stderr = diagnostic.finish();
        assert!(
            stderr
                .windows(STDERR_TRUNCATION_MARKER.len())
                .any(|window| window == STDERR_TRUNCATION_MARKER)
        );
        let message = git_failure_message("clone", &stderr);
        assert_eq!(message, "git clone failed: fatal: late");

        let invalid = vec![0xff; STDERR_DIAGNOSTIC_CAP];
        let rendered = git_failure_message("fetch", invalid);
        assert!(rendered.len() <= "git fetch failed: ".len() + STDERR_DIAGNOSTIC_CAP);
    }

    #[test]
    fn git_failure_message_maps_auth_prompt_to_plain_language() {
        let stderr = "Cloning into '/tmp/x'...\nfatal: could not read Username for 'https://mcp.linear.app': terminal prompts disabled\n";
        assert_eq!(
            git_failure_message("clone", stderr),
            "git clone failed: authentication required or not a git repository (check the URL)"
        );
    }

    #[test]
    fn git_failure_message_keeps_only_fatal_and_error_lines() {
        let stderr =
            "Cloning into '/tmp/x'...\nfatal: repository 'https://example.com/x.git/' not found\n";
        assert_eq!(
            git_failure_message("clone", stderr),
            "git clone failed: fatal: repository 'https://example.com/x.git/' not found"
        );
    }

    #[test]
    fn git_failure_message_falls_back_to_raw_stderr() {
        assert_eq!(
            git_failure_message("fetch", "something unusual\n"),
            "git fetch failed: something unusual"
        );
    }

    #[test]
    fn finish_git_status_success_ignores_reader_failure() {
        let (sender, receiver) = mpsc::sync_channel(1);
        drop(sender);
        let reader = StderrReader {
            receiver,
            thread: None,
        };
        let status = {
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                ExitStatus::from_raw(0)
            }
            #[cfg(windows)]
            {
                use std::os::windows::process::ExitStatusExt;
                ExitStatus::from_raw(0)
            }
        };
        finish_git_status("clone", status, reader).unwrap();
    }

    #[test]
    fn git_message_with_stderr_appends_salient_detail() {
        assert_eq!(
            git_message_with_stderr(
                "git clone timed out after 15s".to_owned(),
                "clone",
                Some(b"Cloning into x...\nfatal: unable to access 'https://example.com/'\n"),
            ),
            "git clone timed out after 15s: fatal: unable to access 'https://example.com/'"
        );
        assert_eq!(
            git_message_with_stderr(
                "git clone timed out after 15s".to_owned(),
                "clone",
                Some(b""),
            ),
            "git clone timed out after 15s"
        );
    }

    #[test]
    fn probe_git_remote_accepts_git_repo() {
        if !git_available() {
            eprintln!("skipping git-dependent test: git binary not available");
            return;
        }
        let remote = tempfile::tempdir().unwrap();
        init_remote_repo(remote.path());
        let url = remote.path().to_string_lossy().to_string();
        probe_git_remote(&url).unwrap();
    }

    #[test]
    fn probe_git_remote_rejects_non_repo() {
        if !git_available() {
            eprintln!("skipping git-dependent test: git binary not available");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let url = dir.path().to_string_lossy().to_string();
        let err = probe_git_remote(&url).unwrap_err();
        assert!(err.contains("ls-remote failed"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn run_git_timed_drains_more_than_a_pipe_buffer_without_false_timeout() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "head -c 262144 /dev/zero >&2"]);
        echo_agent_tty_utils::detach_std_command(&mut cmd);

        run_git_timed(&mut cmd, "stderr-flood", Duration::from_secs(3)).expect("git command");
    }

    #[cfg(unix)]
    #[test]
    fn run_git_timed_kills_hung_process_tree() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("descendant-finished");
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg(format!("(sleep 2; touch {}) & wait", marker.display()));
        echo_agent_tty_utils::detach_std_command(&mut cmd);

        let err = run_git_timed(&mut cmd, "sleep", Duration::from_millis(100)).unwrap_err();
        assert!(err.contains("timed out"), "{err}");
        std::thread::sleep(Duration::from_millis(2200));
        assert!(!marker.exists(), "timeout must kill detached descendants");
    }

    #[cfg(unix)]
    #[test]
    fn run_git_timed_timeout_includes_captured_stderr() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "printf 'fatal: still cloning\\n' >&2; exec sleep 30"]);
        echo_agent_tty_utils::detach_std_command(&mut cmd);

        let err = run_git_timed(&mut cmd, "clone", Duration::from_secs(1)).unwrap_err();
        assert!(err.contains("timed out"), "{err}");
        assert!(err.contains("fatal: still cloning"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn run_git_timed_preserves_late_salient_output_after_the_cap() {
        let mut cmd = Command::new("sh");
        cmd.args([
            "-c",
            "head -c 262144 /dev/zero >&2; printf '\\nfatal: late failure\\n' >&2; exit 1",
        ]);
        echo_agent_tty_utils::detach_std_command(&mut cmd);

        let err = run_git_timed(&mut cmd, "fetch", Duration::from_secs(3)).unwrap_err();
        assert!(err.contains("fatal: late failure"), "{err}");
    }

    #[test]
    fn cache_lease_blocks_concurrent_reclone_during_scan() {
        let cache_root = tempfile::tempdir().unwrap();
        let url = "https://example.com/repo.git";
        let hash = cache_hash(url);
        std::fs::create_dir_all(cache_root.path()).unwrap();
        let lock_path = cache_root.path().join(format!("{hash}.lock"));
        let lease = SourceCacheLease {
            path: cache_root.path().join(&hash),
            lock_file: acquire_cache_lock(&lock_path, Duration::from_millis(1)).unwrap(),
        };

        let start = Instant::now();
        let err = acquire_cache_lock(&lock_path, Duration::from_millis(50)).unwrap_err();
        assert!(err.contains("cache lock timeout"));
        assert!(start.elapsed() >= Duration::from_millis(50));
        drop(lease);
        let _lock = acquire_cache_lock(&lock_path, Duration::from_millis(1)).unwrap();
    }

    #[test]
    fn force_sync_source_cache_preserves_cache_when_reclone_fails() {
        if !git_available() {
            eprintln!("skipping git-dependent test: git binary not available");
            return;
        }
        let remote = tempfile::tempdir().unwrap();
        init_remote_repo(remote.path());
        let cache_root = tempfile::tempdir().unwrap();
        let url = remote.path().to_string_lossy();

        let cache_dir = sync_source_cache(&url, Some("main"), cache_root.path()).unwrap();
        std::fs::remove_dir_all(cache_dir.join(".git").join("objects")).unwrap();
        std::fs::remove_dir_all(remote.path()).unwrap();

        let result = force_sync_source_cache(&url, Some("main"), cache_root.path());
        assert!(result.is_err());
        assert!(cache_dir.exists());
        assert_eq!(
            std::fs::read_to_string(cache_dir.join("file.txt")).unwrap(),
            "initial"
        );
    }

    #[test]
    fn force_sync_source_cache_reclones_corrupt_cache() {
        if !git_available() {
            eprintln!("skipping git-dependent test: git binary not available");
            return;
        }
        let remote = tempfile::tempdir().unwrap();
        init_remote_repo(remote.path());
        let cache_root = tempfile::tempdir().unwrap();
        let url = remote.path().to_string_lossy();

        let cache_dir = sync_source_cache(&url, Some("main"), cache_root.path()).unwrap();
        std::fs::remove_dir_all(cache_dir.join(".git").join("objects")).unwrap();

        let forced_cache_dir =
            force_sync_source_cache(&url, Some("main"), cache_root.path()).unwrap();
        assert_eq!(forced_cache_dir, cache_dir);
        assert!(cache_dir.join(".git").join("objects").exists());
        assert_eq!(
            std::fs::read_to_string(cache_dir.join("file.txt")).unwrap(),
            "initial"
        );
    }

    fn init_remote_repo(path: &Path) {
        run_git(path, &["init", "--initial-branch", "main"]);
        run_git(path, &["config", "user.email", "test@example.com"]);
        run_git(path, &["config", "user.name", "Test User"]);
        add_commit(path, "file.txt", "initial");
    }

    fn add_commit(repo: &Path, file: &str, contents: &str) {
        std::fs::write(repo.join(file), contents).unwrap();
        run_git(repo, &["add", file]);
        run_git(repo, &["commit", "-m", file]);
    }

    fn current_head(repo: &Path) -> String {
        let output = git_command()
            .current_dir(repo)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    fn git_available() -> bool {
        let git_bin = std::env::var("GIT_BIN_PATH").unwrap_or_else(|_| "git".to_string());
        std::process::Command::new(git_bin)
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .output()
            .is_ok_and(|output| output.status.success())
    }

    fn run_git(dir: &Path, args: &[&str]) {
        let git_bin = std::env::var("GIT_BIN_PATH").unwrap_or_else(|_| "git".to_string());
        let output = std::process::Command::new(git_bin)
            .current_dir(dir)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_ASKPASS", "")
            .env("GIT_LFS_SKIP_SMUDGE", "1")
            .env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
