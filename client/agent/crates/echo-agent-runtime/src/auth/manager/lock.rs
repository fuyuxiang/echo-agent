//! Advisory `auth.json.lock` handling. The lock file is never deleted and a held
//! flock is never broken — an unlinked lock lets two processes spend the same
//! refresh token. Staleness resolves in place on the live lock, via [`flock_wait`].

#[path = "lock/flock_wait.rs"]
mod flock_wait;

#[cfg(test)]
#[path = "lock_tests.rs"]
mod tests;

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, Write};
use std::path::Path;
use std::time::Duration as StdDuration;

use fs2::FileExt;

use echo_agent_telemetry::events::{AuthLockTimeout, AuthLockWait};
use echo_agent_telemetry::session_ctx::log_event;

use crate::auth::storage::AuthFileLock;
use crate::unified_log;

pub(crate) const LOCK_FILE_NAME: &str = "auth.json.lock";

/// Older binaries break locks whose holder info ages past this; heartbeats stay under it.
const STALE_LOCK_TIMEOUT_SECS: u64 = 60;

const LOCK_HEARTBEAT_INTERVAL: StdDuration = StdDuration::from_secs(5);

const ACQUIRE_ERROR_BACKOFF: StdDuration = StdDuration::from_millis(50);

const _: () = assert!(
    LOCK_HEARTBEAT_INTERVAL.as_secs() < STALE_LOCK_TIMEOUT_SECS,
    "a heartbeating holder must never age past the stale threshold"
);

// TODO: delete once the token endpoint tolerates racing refreshes AND unlink-recovery
// binaries have aged out of the fleet; the heartbeat only placates their staleness check.
/// Re-dates the lock file's holder info while the lock is held.
pub(crate) struct LockHeartbeat {
    stop: std::sync::mpsc::Sender<()>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl LockHeartbeat {
    fn spawn(mut file: File, interval: StdDuration) -> Self {
        let (stop, ticks) = std::sync::mpsc::channel::<()>();
        let handle = std::thread::Builder::new()
            .name("auth-lock-heartbeat".into())
            .spawn(move || {
                while let Err(std::sync::mpsc::RecvTimeoutError::Timeout) =
                    ticks.recv_timeout(interval)
                {
                    if let Err(e) = write_holder_info(&mut file) {
                        tracing::debug!(error = %e, "auth lock: heartbeat rewrite failed");
                    }
                }
            })
            .inspect_err(|e| {
                unified_log::warn(
                    &format!("auth lock: failed to spawn heartbeat thread: {e}"),
                    /*sid*/ None,
                    /*ctx*/ None,
                );
            })
            .ok();
        Self { stop, handle }
    }
}

impl Drop for LockHeartbeat {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

// TODO: the re-dating half dies with `LockHeartbeat`; the stamp stays for holder telemetry.
/// Writes `PID:UNIX_TS` stamped now into the lock file so waiters can identify the holder.
fn write_holder_info(file: &mut File) -> io::Result<()> {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    write_holder_info_at(file, ts)
}

/// Writes `PID:ts` into the lock file, replacing any prior holder line. The sole
/// production writer of the on-disk holder stamp format.
fn write_holder_info_at(file: &mut File, ts: u64) -> io::Result<()> {
    let pid = std::process::id();
    file.set_len(0)?;
    file.seek(io::SeekFrom::Start(0))?;
    write!(file, "{pid}:{ts}")?;
    file.sync_all()?;
    Ok(())
}

fn parse_holder_info(content: &str) -> Option<(u32, u64)> {
    let (pid_str, ts_str) = content.trim().split_once(':')?;
    Some((pid_str.parse().ok()?, ts_str.parse().ok()?))
}

#[cfg(unix)]
fn is_process_alive(pid: u32) -> bool {
    let pid_i = match i32::try_from(pid) {
        Ok(p) if p > 0 => p,
        Ok(_) | Err(_) => return false,
    };
    // SAFETY: `kill(pid, 0)` sends no signal; it only tests for existence.
    let ret = unsafe { libc::kill(pid_i as libc::pid_t, 0) };
    // EPERM still means the process exists; only ESRCH means it is gone.
    ret == 0 || io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

#[cfg(not(unix))]
fn is_process_alive(_pid: u32) -> bool {
    true
}

// TODO: unlink-tolerance cluster (with `LockAttempt::InodeChanged`); dies with `LockHeartbeat`.
#[cfg(unix)]
fn inodes_match(file: &File, path: &Path) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let fd_meta = file.metadata()?;
    let path_meta = std::fs::metadata(path)?;
    Ok(fd_meta.ino() == path_meta.ino() && fd_meta.dev() == path_meta.dev())
}

#[cfg(not(unix))]
fn inodes_match(_file: &File, _path: &Path) -> io::Result<bool> {
    Ok(true)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HolderState {
    Dead,
    StuckLive,
    Alive,
}

impl HolderState {
    /// Stable label emitted in telemetry.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Dead => "dead",
            Self::StuckLive => "stuck_live",
            Self::Alive => "alive",
        }
    }
}

/// Telemetry-only snapshot of the current lock holder; never a break input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LockHolder {
    pub(crate) state: HolderState,
    pub(crate) pid: Option<u32>,
    pub(crate) age_secs: Option<u64>,
}

/// An unidentifiable holder classifies by file mtime: fresh may be a holder mid-write.
fn read_holder(file: &mut File) -> LockHolder {
    let mut content = String::new();
    let parsed =
        if file.seek(io::SeekFrom::Start(0)).is_ok() && file.read_to_string(&mut content).is_ok() {
            parse_holder_info(&content)
        } else {
            None
        };

    let Some((pid, ts)) = parsed else {
        let age_secs = file
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .map(|modified| modified.elapsed().unwrap_or_default().as_secs());
        let state = if age_secs.is_some_and(|age| age > STALE_LOCK_TIMEOUT_SECS) {
            HolderState::StuckLive
        } else {
            HolderState::Alive
        };
        return LockHolder {
            state,
            pid: None,
            age_secs,
        };
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let age = now.saturating_sub(ts);
    let state = if !is_process_alive(pid) {
        HolderState::Dead
    } else if age > STALE_LOCK_TIMEOUT_SECS {
        HolderState::StuckLive
    } else {
        HolderState::Alive
    };
    LockHolder {
        state,
        pid: Some(pid),
        age_secs: Some(age),
    }
}

enum LockAttempt {
    Acquired(File),
    Busy,
    InodeChanged,
    Failed(io::Error),
}

fn try_acquire_once(lock_path: &Path) -> LockAttempt {
    let mut file = match OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)
    {
        Ok(f) => f,
        Err(e) => {
            unified_log::warn(
                &format!("auth lock: failed to open {}: {e}", lock_path.display()),
                /*sid*/ None,
                /*ctx*/ None,
            );
            return LockAttempt::Failed(e);
        }
    };

    match file.try_lock_exclusive() {
        Ok(()) => {
            let pid = std::process::id();
            if let Err(e) = write_holder_info(&mut file) {
                unified_log::warn(
                    &format!("auth lock: failed to write holder info: {e}"),
                    /*sid*/ None,
                    Some(serde_json::json!({ "pid": pid })),
                );
            }

            match inodes_match(&file, lock_path) {
                Ok(true) => {
                    unified_log::debug(
                        &format!("auth lock: acquired (pid={pid})"),
                        /*sid*/ None,
                        Some(
                            serde_json::json!({ "pid": pid, "path": lock_path.display().to_string() }),
                        ),
                    );
                    LockAttempt::Acquired(file)
                }
                Ok(false) => {
                    unified_log::debug(
                        &format!("auth lock: inode changed after acquire (pid={pid}), retrying"),
                        /*sid*/ None,
                        /*ctx*/ None,
                    );
                    LockAttempt::InodeChanged
                }
                Err(e) => {
                    unified_log::debug(
                        &format!("auth lock: path gone after acquire (pid={pid}): {e}"),
                        /*sid*/ None,
                        /*ctx*/ None,
                    );
                    LockAttempt::InodeChanged
                }
            }
        }

        Err(e) if e.kind() == io::ErrorKind::WouldBlock => LockAttempt::Busy,

        Err(e) => {
            unified_log::warn(
                &format!("auth lock: flock failed: {e}"),
                /*sid*/ None,
                /*ctx*/ None,
            );
            LockAttempt::Failed(e)
        }
    }
}

/// Parks in the kernel until the flock is free; fails if the file was replaced meanwhile.
fn blocking_acquire(lock_path: &Path) -> io::Result<File> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)?;

    loop {
        match file.lock_exclusive() {
            Ok(()) => break,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => {
                unified_log::warn(
                    &format!("auth lock: blocking flock failed: {e}"),
                    /*sid*/ None,
                    /*ctx*/ None,
                );
                return Err(e);
            }
        }
    }

    let pid = std::process::id();
    if let Err(e) = write_holder_info(&mut file) {
        unified_log::warn(
            &format!("auth lock: failed to write holder info: {e}"),
            /*sid*/ None,
            Some(serde_json::json!({ "pid": pid })),
        );
    }

    match inodes_match(&file, lock_path) {
        Ok(true) => {
            unified_log::debug(
                &format!("auth lock: acquired via blocking flock (pid={pid})"),
                /*sid*/ None,
                Some(serde_json::json!({ "pid": pid, "path": lock_path.display().to_string() })),
            );
            Ok(file)
        }
        Ok(false) => Err(io::Error::other(
            "inode changed during blocking flock (concurrent unlink+recreate)",
        )),
        Err(e) => Err(io::Error::other(format!(
            "path gone after blocking flock: {e}"
        ))),
    }
}

/// Takes the flock iff it is free right now; never waits.
pub(crate) fn try_lock_auth_file_nonblocking(auth_json_path: &Path) -> Option<AuthFileLock> {
    let lock_path = auth_json_path.with_file_name(LOCK_FILE_NAME);
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .ok()?;

    file.try_lock_exclusive().ok()?;

    if let Err(e) = write_holder_info(&mut file) {
        unified_log::warn(
            &format!("auth lock: failed to write holder info (non-blocking): {e}"),
            /*sid*/ None,
            Some(serde_json::json!({ "pid": std::process::id() })),
        );
    }
    if !inodes_match(&file, &lock_path).unwrap_or(false) {
        return None;
    }
    Some(lock_guard(file, Heartbeat::Skip))
}

#[derive(Clone, Copy)]
pub(crate) enum Heartbeat {
    /// The hold may span an IdP exchange; keep the holder info fresh.
    Attach,
    /// Millisecond-scale advisory holds don't warrant a thread each.
    Skip,
}

fn lock_guard(file: File, heartbeat: Heartbeat) -> AuthFileLock {
    let heartbeat = match heartbeat {
        Heartbeat::Skip => None,
        Heartbeat::Attach => match file.try_clone() {
            Ok(clone) => Some(LockHeartbeat::spawn(clone, LOCK_HEARTBEAT_INTERVAL)),
            Err(e) => {
                unified_log::warn(
                    &format!("auth lock: failed to clone FD for heartbeat: {e}"),
                    /*sid*/ None,
                    /*ctx*/ None,
                );
                None
            }
        },
    };
    AuthFileLock { heartbeat, file }
}

#[must_use]
pub(crate) enum LockAcquire {
    Acquired(AuthFileLock),
    /// Budget expired on a held flock; `holder` is the snapshot at the deadline.
    TimedOut {
        holder: Option<LockHolder>,
    },
    /// The lock file could not be opened or flocked at all; nothing was waited on.
    Failed {
        error: io::Error,
    },
}

impl LockAcquire {
    #[must_use]
    pub(crate) fn into_guard(self) -> Option<AuthFileLock> {
        match self {
            Self::Acquired(guard) => Some(guard),
            Self::TimedOut { .. } | Self::Failed { .. } => None,
        }
    }
}

/// Instant non-blocking try, then the shared blocking wait bounded by `timeout`.
/// A timed-out waiter leaves the holder alone and reports it via `holder`.
pub(crate) async fn try_lock_auth_file_async(
    auth_json_path: &Path,
    timeout: StdDuration,
    heartbeat: Heartbeat,
) -> LockAcquire {
    let lock_path = auth_json_path.with_file_name(LOCK_FILE_NAME);

    unified_log::debug(
        &format!(
            "auth lock: attempting acquire (timeout={}ms)",
            timeout.as_millis()
        ),
        /*sid*/ None,
        Some(
            serde_json::json!({ "path": lock_path.display().to_string(), "timeout_ms": timeout.as_millis() as u64 }),
        ),
    );

    match try_acquire_once(&lock_path) {
        LockAttempt::Acquired(file) => {
            return LockAcquire::Acquired(lock_guard(file, heartbeat));
        }
        LockAttempt::Failed(error) => return LockAcquire::Failed { error },
        LockAttempt::InodeChanged | LockAttempt::Busy => {}
    }

    let deadline = tokio::time::Instant::now() + timeout;
    let contended_at = std::time::Instant::now();
    let late_ticket = loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining == StdDuration::ZERO {
            break None;
        }

        let ticket = flock_wait::join(&lock_path);
        match tokio::time::timeout(remaining, ticket.claim()).await {
            Ok(Some(Ok(file))) => {
                log_event(AuthLockWait {
                    wait_ms: contended_at.elapsed().as_millis() as u64,
                    budget_ms: timeout.as_millis() as u64,
                });
                return LockAcquire::Acquired(lock_guard(file, heartbeat));
            }
            Ok(Some(Err(e))) => {
                tracing::debug!(error = %e, "auth lock: shared wait deposited an error");
                tokio::time::sleep(
                    ACQUIRE_ERROR_BACKOFF
                        .min(deadline.saturating_duration_since(tokio::time::Instant::now())),
                )
                .await;
                continue;
            }
            Ok(None) => continue,
            // The ticket outlives the salvage so a late deposit is claimed, not dropped.
            Err(_elapsed) => break Some(ticket),
        }
    };

    let salvaged = salvage_at_deadline(late_ticket.as_ref(), &lock_path);
    drop(late_ticket);
    let late_acquire = match salvaged {
        Ok(file) => file,
        Err(error) => return LockAcquire::Failed { error },
    };
    if let Some(file) = late_acquire {
        log_event(AuthLockWait {
            wait_ms: contended_at.elapsed().as_millis() as u64,
            budget_ms: timeout.as_millis() as u64,
        });
        unified_log::info(
            &format!(
                "auth lock: acquired after deadline race ({}ms budget already exhausted)",
                timeout.as_millis()
            ),
            /*sid*/ None,
            Some(
                serde_json::json!({ "path": lock_path.display().to_string(), "timeout_ms": timeout.as_millis() as u64 }),
            ),
        );
        return LockAcquire::Acquired(lock_guard(file, heartbeat));
    }

    let holder = OpenOptions::new()
        .read(true)
        .open(&lock_path)
        .ok()
        .map(|mut file| read_holder(&mut file));
    unified_log::warn(
        &format!(
            "auth lock: wait budget exhausted after {}ms; holder left in place",
            timeout.as_millis()
        ),
        /*sid*/ None,
        Some(serde_json::json!({
            "path": lock_path.display().to_string(),
            "timeout_ms": timeout.as_millis() as u64,
            "holder_pid": holder.and_then(|h| h.pid),
            "holder_state": holder.map(|h| h.state.label()),
            "holder_age_secs": holder.and_then(|h| h.age_secs),
        })),
    );
    log_event(AuthLockTimeout {
        budget_ms: timeout.as_millis() as u64,
        holder_state: holder.map(|h| h.state.label()),
    });
    LockAcquire::TimedOut { holder }
}

/// One last claim-or-acquire pass at the deadline; a deposited failure surfaces, not a timeout.
fn salvage_at_deadline(
    ticket: Option<&flock_wait::Ticket>,
    lock_path: &Path,
) -> Result<Option<File>, io::Error> {
    let mut deposit_error = None;
    if let Some(ticket) = ticket {
        match ticket.try_claim() {
            Some(Ok(file)) => return Ok(Some(file)),
            Some(Err(e)) => deposit_error = Some(e),
            None => {}
        }
    }
    match try_acquire_once(lock_path) {
        LockAttempt::Acquired(file) => return Ok(Some(file)),
        LockAttempt::Busy => {
            if let Some(ticket) = ticket {
                match ticket.try_claim() {
                    Some(Ok(file)) => return Ok(Some(file)),
                    Some(Err(e)) => deposit_error = Some(e),
                    None => {}
                }
            }
        }
        LockAttempt::InodeChanged | LockAttempt::Failed(_) => {}
    }
    match deposit_error {
        Some(e) => Err(e),
        None => Ok(None),
    }
}

pub(crate) fn read_holder_at(auth_json_path: &Path) -> Option<LockHolder> {
    OpenOptions::new()
        .read(true)
        .open(auth_json_path.with_file_name(LOCK_FILE_NAME))
        .ok()
        .map(|mut file| read_holder(&mut file))
}

#[cfg(all(test, unix))]
pub(crate) mod test_support {
    use super::*;

    const STALE_HOLDER_AGE: u64 = STALE_LOCK_TIMEOUT_SECS + 60;

    #[must_use]
    pub(crate) fn hold_backdated_stale_lock(lock_path: &Path) -> File {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(lock_path)
            .expect("create auth.json.lock");
        file.try_lock_exclusive()
            .expect("uncontended flock in test");
        let backdated = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .saturating_sub(STALE_HOLDER_AGE);
        write_holder_info_at(&mut file, backdated).expect("write backdated holder info");
        file
    }
}
