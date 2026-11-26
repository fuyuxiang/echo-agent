//! Targeted fetch of specific commit object ids for session restore.
//!
//! Unbounded `git fetch origin` on a shallow monorepo clone unshallows millions
//! of objects. Restore fetches only snapshot HEAD / public base:
//! `git fetch --no-tags [--depth=1] origin <full-sha>`.
//! `--depth=1` is used only when the destination is already shallow, so a full
//! clone (or a linked worktree sharing one) is not converted to shallow.
//!
//! Head and base share [`RESTORE_FETCH_BUDGET`]. Head is capped so at least
//! [`RESTORE_FETCH_BASE_RESERVE`] remains for public-base after head's
//! wait-timeout **and** TERM/KILL/stderr teardown when that oid is a distinct
//! missing object. The child process group is torn down with SIGTERM, then
//! SIGKILL, then a bounded wait so a detached `setsid` fetch cannot outlive
//! the restore attempt.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use echo_agent_tty_utils::{ProcessGroup, git_command, git_command_locking, global_process_scope};
use wait_timeout::ChildExt;

/// Shared wall-clock budget for head+base targeted fetches.
pub(crate) const RESTORE_FETCH_BUDGET: Duration = Duration::from_secs(60);

/// Minimum slice reserved for a distinct missing public-base after the head attempt.
pub(crate) const RESTORE_FETCH_BASE_RESERVE: Duration = Duration::from_secs(10);

/// Extra time for `spawn_blocking` join after the sync fetch budget + teardown.
pub(crate) const RESTORE_FETCH_JOIN_SLACK: Duration = Duration::from_secs(15);

const FETCH_TERM_GRACE: Duration = Duration::from_secs(2);
const FETCH_KILL_WAIT: Duration = Duration::from_secs(2);
const FETCH_ABANDON_REAP_WAIT: Duration = Duration::from_secs(5);
const STDERR_JOIN_WAIT: Duration = Duration::from_secs(2);

/// Wall-clock `wait_success` may still spend after a timed-out `wait_timeout`
/// (TERM grace + KILL wait + stderr join) plus a small scheduling slack.
/// Subtracted from the head slice so a hung head fetch cannot eat the base reserve.
pub(crate) const RESTORE_FETCH_TEARDOWN_RESERVE: Duration = Duration::from_secs(
    FETCH_TERM_GRACE.as_secs() + FETCH_KILL_WAIT.as_secs() + STDERR_JOIN_WAIT.as_secs() + 2,
);
pub(crate) const MAX_FETCH_STDERR_BYTES: usize = 8 * 1024;

/// Result of ensuring snapshot commits are locally present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnsureCommitsOutcome {
    AlreadyPresent,
    Fetched,
    SkippedInvalidOid,
}

/// Result of a single-oid fetch attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FetchCommitOutcome {
    AlreadyPresent,
    Fetched,
    SkippedInvalidOid,
}

/// True when `value` is a full lowercase hex object id (SHA-1 or SHA-256).
pub(crate) fn is_full_object_id(value: &str) -> bool {
    let len = value.len();
    (len == 40 || len == 64) && value.as_bytes().iter().all(is_lowercase_hex)
}

fn is_lowercase_hex(b: &u8) -> bool {
    matches!(b, b'0'..=b'9' | b'a'..=b'f')
}

/// Ref names safe to pass as a single `git fetch origin <spec>` argument.
pub(crate) fn is_safe_git_ref(value: &str) -> bool {
    if value.is_empty() || value.starts_with('-') {
        return false;
    }
    if value.contains([':', '*', '?', '[', '\\', '\0', ' ', '\t', '\n', '\r']) {
        return false;
    }
    if value.contains("..") || value.contains("@{") {
        return false;
    }
    true
}

pub(crate) fn is_safe_fetch_refspec(value: &str) -> bool {
    if is_full_object_id(value) {
        return true;
    }
    is_safe_git_ref(value) && !is_abbreviated_object_id(value)
}

/// Hex string that looks like an abbreviated object id rather than a ref name.
/// `git fetch origin <short-sha>` is not a remote refspec (unlike a full oid).
fn is_abbreviated_object_id(value: &str) -> bool {
    let len = value.len();
    let plausible_abbrev = (4..40).contains(&len) || (41..64).contains(&len);
    plausible_abbrev && value.as_bytes().iter().all(u8::is_ascii_hexdigit)
}

/// Map a checkout name to a `git fetch origin <spec>` source.
///
/// Local remote-tracking names (`origin/foo`, `refs/remotes/origin/foo`) are
/// valid `git checkout` targets but not origin fetch sources. Fetch the
/// corresponding remote branch instead. Full object ids and other simple refs
/// (`main`, `refs/heads/…`, `refs/tags/…`) pass through unchanged. Abbreviated
/// SHAs are rejected — they checkout locally when present but cannot be fetched.
pub(crate) fn origin_fetch_spec_for_checkout_target(target: &str) -> Option<&str> {
    if is_full_object_id(target) {
        return Some(target);
    }
    if !is_safe_git_ref(target) || is_abbreviated_object_id(target) {
        return None;
    }
    for prefix in ["refs/remotes/origin/", "origin/"] {
        if let Some(rest) = target.strip_prefix(prefix) {
            return is_safe_git_ref(rest).then_some(rest);
        }
    }
    Some(target)
}

/// Local git object lookup and origin fetch used by restore.
///
/// Implementors must only fetch a caller-supplied full object id — never a bare
/// `origin` fetch or `--unshallow`.
pub(crate) trait RestoreGit {
    fn has_object(&self, repo: &Path, oid: &str) -> bool;
    fn fetch_oid(&self, repo: &Path, oid: &str, timeout: Duration) -> Result<()>;
}

pub(crate) struct LocalGit;

impl RestoreGit for LocalGit {
    fn has_object(&self, repo: &Path, oid: &str) -> bool {
        git_object_exists(repo, oid)
    }

    fn fetch_oid(&self, repo: &Path, oid: &str, timeout: Duration) -> Result<()> {
        fetch_oid_from_origin(repo, oid, timeout)
    }
}

/// Fetch `head`, then `public_base` if it is still missing, from `origin`.
///
/// Invalid oids are not passed to git. Fetch failures are returned after both
/// attempts so the caller can log and continue; missing objects are left for
/// checkout-strategy selection.
///
/// # Errors
///
/// Spawn failure, timeout/teardown, or non-zero git exit. An error does **not**
/// imply the objects are unreachable — re-check before aborting restore.
pub fn ensure_commits_reachable(
    repo: &Path,
    head: &str,
    public_base: &str,
) -> Result<EnsureCommitsOutcome> {
    let deadline = Instant::now() + RESTORE_FETCH_BUDGET;
    ensure_commits_reachable_with(repo, head, public_base, &LocalGit, deadline)
}

/// Fetch `oid` from origin if it is a full object id and not already local.
///
/// # Errors
///
/// Same as [`ensure_commits_reachable`].
pub(crate) fn fetch_commit_if_missing(repo: &Path, oid: &str) -> Result<FetchCommitOutcome> {
    fetch_if_missing(repo, oid, &LocalGit, RESTORE_FETCH_BUDGET)
}

pub(crate) fn ensure_commits_reachable_with<G: RestoreGit>(
    repo: &Path,
    head: &str,
    public_base: &str,
    git: &G,
    deadline: Instant,
) -> Result<EnsureCommitsOutcome> {
    // Reserve only when a later base fetch is actually expected. A present or
    // identical base would otherwise shrink the head attempt (the common case).
    let reserve_for_base =
        is_full_object_id(public_base) && public_base != head && !git.has_object(repo, public_base);
    let head_timeout = if reserve_for_base {
        remaining(deadline)
            .saturating_sub(RESTORE_FETCH_BASE_RESERVE)
            .saturating_sub(RESTORE_FETCH_TEARDOWN_RESERVE)
    } else {
        remaining(deadline)
    };
    let head_result = fetch_if_missing(repo, head, git, head_timeout);
    let base_result = fetch_if_missing(repo, public_base, git, remaining(deadline));
    match (head_result, base_result) {
        (Err(head_err), Err(base_err)) => Err(head_err.context(format!(
            "public-base {} fetch also failed: {base_err}",
            short_oid(public_base)
        ))),
        (Err(err), Ok(_)) | (Ok(_), Err(err)) => Err(err),
        (Ok(head_out), Ok(base_out)) => Ok(combine_outcomes(head_out, base_out)),
    }
}

fn short_oid(oid: &str) -> &str {
    let mut end = oid.len().min(8);
    while end > 0 && !oid.is_char_boundary(end) {
        end -= 1;
    }
    &oid[..end]
}

fn remaining(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

fn combine_outcomes(head: FetchCommitOutcome, base: FetchCommitOutcome) -> EnsureCommitsOutcome {
    use FetchCommitOutcome::{AlreadyPresent, Fetched, SkippedInvalidOid};
    match (head, base) {
        (Fetched, _) | (_, Fetched) => EnsureCommitsOutcome::Fetched,
        (AlreadyPresent, AlreadyPresent)
        | (AlreadyPresent, SkippedInvalidOid)
        | (SkippedInvalidOid, AlreadyPresent) => EnsureCommitsOutcome::AlreadyPresent,
        (SkippedInvalidOid, SkippedInvalidOid) => EnsureCommitsOutcome::SkippedInvalidOid,
    }
}

pub(crate) fn fetch_if_missing<G: RestoreGit>(
    repo: &Path,
    oid: &str,
    git: &G,
    timeout: Duration,
) -> Result<FetchCommitOutcome> {
    if !is_full_object_id(oid) {
        tracing::warn!(oid = %oid, "restore_fetch: skipping non-oid refspec");
        return Ok(FetchCommitOutcome::SkippedInvalidOid);
    }
    if git.has_object(repo, oid) {
        return Ok(FetchCommitOutcome::AlreadyPresent);
    }
    if timeout.is_zero() {
        bail!("git fetch origin {oid} skipped: restore fetch budget exhausted");
    }
    git.fetch_oid(repo, oid, timeout)?;
    Ok(FetchCommitOutcome::Fetched)
}

/// Whether `git cat-file -t` resolves `spec` in `repo`.
pub fn git_object_exists(repo: &Path, spec: &str) -> bool {
    let output = git_command()
        .current_dir(repo)
        .args(["cat-file", "-t", spec])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output();
    matches!(output, Ok(o) if o.status.success())
}

/// Fetch a checkout target (full oid or simple ref) if it is not already local.
///
/// # Errors
///
/// Unsafe/unsupported spec, spawn failure, timeout, or non-zero git exit.
pub(crate) fn fetch_checkout_target_if_missing(
    repo: &Path,
    target: &str,
) -> Result<FetchCommitOutcome> {
    let Some(spec) = origin_fetch_spec_for_checkout_target(target) else {
        bail!("unsupported checkout target (need a full commit oid or a simple git ref): {target}");
    };
    if is_full_object_id(spec) {
        return fetch_commit_if_missing(repo, spec);
    }
    fetch_refspec_from_origin(repo, spec, RESTORE_FETCH_BUDGET)?;
    Ok(FetchCommitOutcome::Fetched)
}

fn is_shallow_repository(repo: &Path) -> bool {
    let output = git_command()
        .current_dir(repo)
        .args(["rev-parse", "--is-shallow-repository"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    matches!(
        output,
        Ok(o) if o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true"
    )
}

pub(crate) fn targeted_fetch_args(spec: &str, is_shallow: bool) -> Vec<String> {
    let mut args = vec!["fetch".to_owned(), "--no-tags".to_owned()];
    if is_shallow {
        args.push("--depth=1".to_owned());
    }
    args.push("origin".to_owned());
    // `--no-tags` still fetches the object into FETCH_HEAD only. A dst
    // refspec materializes the local tag so `git checkout refs/tags/…` works.
    if spec.starts_with("refs/tags/") && is_safe_git_ref(spec) {
        args.push(format!("{spec}:{spec}"));
    } else {
        args.push(spec.to_owned());
    }
    args
}

pub(crate) fn targeted_fetch_command(
    repo: &Path,
    oid: &str,
    is_shallow: bool,
) -> std::process::Command {
    let mut cmd = git_command_locking();
    cmd.current_dir(repo)
        .args(targeted_fetch_args(oid, is_shallow))
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    cmd
}

fn fetch_oid_from_origin(repo: &Path, oid: &str, timeout: Duration) -> Result<()> {
    fetch_refspec_from_origin(repo, oid, timeout)
}

fn fetch_refspec_from_origin(repo: &Path, spec: &str, timeout: Duration) -> Result<()> {
    if !is_safe_fetch_refspec(spec) {
        bail!("refusing unsafe fetch refspec");
    }
    let is_shallow = is_shallow_repository(repo);
    tracing::info!(
        spec = %spec,
        is_shallow,
        timeout_secs = timeout.as_secs(),
        "restore_fetch: targeted fetch"
    );

    let mut child = FetchChild::spawn(targeted_fetch_command(repo, spec, is_shallow))?;
    let result = child.wait_success(timeout, spec);
    if result.is_err() {
        warn_leftover_git_locks(repo);
    }
    result
}

struct FetchChild {
    child: Option<Child>,
    group: Arc<ProcessGroup>,
    stderr: Option<Receiver<String>>,
    abandoned: bool,
}

impl FetchChild {
    fn spawn(mut cmd: std::process::Command) -> Result<Self> {
        #[allow(clippy::disallowed_methods)] // enrolled in ProcessScope / Drop teardown below
        let mut child = cmd.spawn().context("spawning git fetch")?;
        let stderr = spawn_stderr_reader(child.stderr.take());

        let mut group = match ProcessGroup::new() {
            Ok(group) => group,
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait_timeout(FETCH_KILL_WAIT);
                return Err(err).context("creating fetch process group");
            }
        };
        if let Err(err) = group.attach_std(&child) {
            let _ = child.kill();
            let _ = child.wait_timeout(FETCH_KILL_WAIT);
            return Err(err).context("attaching fetch to process group");
        }
        let group = Arc::new(group);
        if !global_process_scope().register(&group) {
            let mut spawned = Self {
                child: Some(child),
                group,
                stderr: Some(stderr),
                abandoned: false,
            };
            let shutdown = spawned.shutdown();
            bail!(
                "process scope already closed; fetch killed{}",
                format_shutdown_suffix(shutdown.as_ref())
            );
        }
        Ok(Self {
            child: Some(child),
            group,
            stderr: Some(stderr),
            abandoned: false,
        })
    }

    fn wait_success(&mut self, timeout: Duration, spec: &str) -> Result<()> {
        let child = self.child.as_mut().context("fetch child already reaped")?;
        match child.wait_timeout(timeout) {
            Ok(Some(status)) => {
                self.child.take();
                let stderr = self.take_stderr();
                if status.success() {
                    Ok(())
                } else {
                    bail!("git fetch --no-tags origin {spec} failed ({status}): {stderr}");
                }
            }
            Ok(None) => {
                let shutdown = self.shutdown();
                let stderr = self.take_stderr();
                bail!(
                    "git fetch --no-tags origin {spec} timed out after {}s{}{}",
                    timeout.as_secs(),
                    format_shutdown_suffix(shutdown.as_ref()),
                    format_stderr_suffix(&stderr)
                );
            }
            Err(err) => {
                let shutdown = self.shutdown();
                let stderr = self.take_stderr();
                Err(err).context(format!(
                    "waiting for git fetch origin {spec}{}{}",
                    format_shutdown_suffix(shutdown.as_ref()),
                    format_stderr_suffix(&stderr)
                ))
            }
        }
    }

    fn shutdown(&mut self) -> Option<anyhow::Error> {
        if self.abandoned {
            return None;
        }
        let child = self.child.as_mut()?;
        let result = escalate_and_reap(child, &self.group);
        if result.leader_reaped {
            self.child.take();
        } else if let Some(child) = self.child.take() {
            // Hold enrollment until a detached reaper finishes a bounded wait,
            // then drop the Arc so kill_all cannot later hit a recycled pgid.
            spawn_abandon_reaper(child, Arc::clone(&self.group));
            self.abandoned = true;
        }
        result.error
    }

    fn take_stderr(&mut self) -> String {
        match self.stderr.take() {
            Some(rx) => rx.recv_timeout(STDERR_JOIN_WAIT).unwrap_or_default(),
            None => String::new(),
        }
    }
}

impl Drop for FetchChild {
    fn drop(&mut self) {
        if self.abandoned {
            return;
        }
        if self.child.is_some() {
            let _ = self.shutdown();
            let _ = self.take_stderr();
        }
    }
}

struct EscalateResult {
    error: Option<anyhow::Error>,
    leader_reaped: bool,
}

fn escalate_and_reap(child: &mut Child, group: &ProcessGroup) -> EscalateResult {
    let mut errors = Vec::new();
    if let Err(err) = group.terminate() {
        errors.push(format!("SIGTERM failed: {err}"));
    }
    let mut leader_reaped = match child.wait_timeout(FETCH_TERM_GRACE) {
        Ok(Some(_)) => true,
        Ok(None) => false,
        Err(err) => {
            errors.push(format!("wait after SIGTERM: {err}"));
            false
        }
    };
    if group.has_live_members() != Some(false)
        && let Err(err) = group.kill()
    {
        errors.push(format!("SIGKILL failed: {err}"));
    }
    if !leader_reaped {
        match child.wait_timeout(FETCH_KILL_WAIT) {
            Ok(Some(_)) => leader_reaped = true,
            Ok(None) => errors.push("fetch leader still alive after SIGKILL".to_owned()),
            Err(err) => errors.push(format!("wait after SIGKILL: {err}")),
        }
    }
    #[cfg(unix)]
    if group.has_live_members() != Some(false) {
        errors.push("fetch process group still has live members after teardown".to_owned());
    }
    EscalateResult {
        error: join_errors(errors),
        leader_reaped,
    }
}

fn join_errors(errors: Vec<String>) -> Option<anyhow::Error> {
    if errors.is_empty() {
        None
    } else {
        Some(anyhow::anyhow!(errors.join("; ")))
    }
}

fn format_shutdown_suffix(err: Option<&anyhow::Error>) -> String {
    err.map(|e| format!("; {e}")).unwrap_or_default()
}

fn format_stderr_suffix(stderr: &str) -> String {
    if stderr.is_empty() {
        String::new()
    } else {
        format!(": {stderr}")
    }
}

fn spawn_abandon_reaper(mut child: Child, group: Arc<ProcessGroup>) {
    std::thread::spawn(move || {
        let pid = child.id();
        let leader_reaped = matches!(child.wait_timeout(FETCH_ABANDON_REAP_WAIT), Ok(Some(_)));
        // killpg after a successful reap of an empty group can hit a recycled pgid.
        if !leader_reaped || group.has_live_members() != Some(false) {
            let _ = group.kill();
            if !leader_reaped {
                match child.wait_timeout(FETCH_ABANDON_REAP_WAIT) {
                    Ok(Some(_)) => {}
                    Ok(None) => {
                        tracing::warn!(
                            pid,
                            "restore_fetch: abandoned fetch still alive after SIGKILL wait"
                        );
                    }
                    Err(err) => {
                        tracing::warn!(
                            pid,
                            error = %err,
                            "restore_fetch: wait after abandon SIGKILL failed"
                        );
                    }
                }
            }
        }
        drop(group);
    });
}

fn spawn_stderr_reader(stderr: Option<ChildStderr>) -> Receiver<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(read_stderr_capped(stderr));
    });
    rx
}

fn read_stderr_capped(stderr: Option<ChildStderr>) -> String {
    let Some(mut reader) = stderr else {
        return String::new();
    };
    let mut chunk = [0u8; 1024];
    let mut collected = Vec::new();
    let mut truncated = false;
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                if collected.len() < MAX_FETCH_STDERR_BYTES {
                    let room = MAX_FETCH_STDERR_BYTES - collected.len();
                    collected.extend_from_slice(&chunk[..n.min(room)]);
                    if n > room {
                        truncated = true;
                    }
                } else {
                    truncated = true;
                }
            }
            Err(_) => break,
        }
    }
    let mut text = String::from_utf8_lossy(&collected).trim().to_string();
    if truncated {
        text.push_str("…(truncated)");
    }
    text
}

fn warn_leftover_git_locks(repo: &Path) {
    let mut dirs = Vec::new();
    for flag in ["--git-dir", "--git-common-dir"] {
        if let Some(dir) = git_rev_parse_path(repo, flag)
            && !dirs.contains(&dir)
        {
            dirs.push(dir);
        }
    }
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().ends_with(".lock") {
                tracing::warn!(
                    path = %entry.path().display(),
                    "restore_fetch: leftover git lock after fetch teardown"
                );
            }
        }
    }
}

fn git_rev_parse_path(repo: &Path, flag: &str) -> Option<PathBuf> {
    let output = git_command()
        .current_dir(repo)
        .args(["rev-parse", flag])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if raw.is_empty() {
        return None;
    }
    let path = PathBuf::from(raw);
    Some(if path.is_absolute() {
        path
    } else {
        repo.join(path)
    })
}

#[cfg(test)]
#[path = "restore_fetch_tests.rs"]
mod tests;
