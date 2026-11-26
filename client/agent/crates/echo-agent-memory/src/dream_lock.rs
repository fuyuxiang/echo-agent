//! Dream lock file and session counting infrastructure.
//!
//! Coordination primitives for background memory consolidation ("dream"):
//! - [`DreamLock`]: PID-based lock file with mtime tracking
//! - [`sessions_since`]: counts session files modified after a given timestamp

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const LOCK_FILE_NAME: &str = ".dream-lock";

/// Whether a process with the given PID is alive.
///
/// Local copy kept dependency-free of `crate::util` so the memory subsystem
/// can be extracted into its own crate. Mirrors `crate::util::is_process_alive`.
#[cfg(unix)]
fn is_process_alive(pid: u32) -> bool {
    use nix::errno::Errno;
    use nix::sys::signal::kill;
    use nix::unistd::Pid;

    // Signal 0 probes existence; EPERM means alive under a different UID.
    match kill(Pid::from_raw(pid as i32), None) {
        Ok(()) => true,
        Err(Errno::ESRCH) => false,
        Err(_) => true,
    }
}

#[cfg(windows)]
fn is_process_alive(pid: u32) -> bool {
    use windows::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };

    // SAFETY: OpenProcess returns Err on absence/permission failure;
    // PROCESS_SYNCHRONIZE is the minimum right needed for WaitForSingleObject.
    let Ok(handle) = (unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }) else {
        return false;
    };

    // SAFETY: handle is valid; timeout 0 means "poll, don't block."
    let wait_result = unsafe { WaitForSingleObject(handle, 0) };
    // SAFETY: handle is owned by us; close regardless of wait result.
    let _ = unsafe { CloseHandle(handle) };

    wait_result == WAIT_TIMEOUT
}

pub struct DreamLock {
    path: PathBuf,
}

impl DreamLock {
    pub fn new(workspace_dir: &Path) -> Self {
        Self {
            path: workspace_dir.join(LOCK_FILE_NAME),
        }
    }

    /// Read the last consolidation timestamp (lock file mtime).
    /// Returns `None` if the lock file doesn't exist.
    pub fn last_consolidated_at(&self) -> io::Result<Option<SystemTime>> {
        match fs::metadata(&self.path) {
            Ok(meta) => Ok(Some(meta.modified()?)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Try to acquire the lock for consolidation.
    ///
    /// Returns `Ok(Some(prior))` on success, where `prior` is the previous mtime
    /// (`None` if the file didn't exist). Pass to [`Self::rollback`] on failure.
    /// Returns `Ok(None)` if held by a live, non-stale process.
    ///
    /// Reclaims stale locks when the holder PID is dead or age exceeds `stale_secs`.
    ///
    /// Note: this is best-effort coordination, not mutual exclusion. The
    /// write-then-verify protocol reduces but cannot eliminate races — two
    /// processes may rarely both believe they acquired. Callers must tolerate
    /// duplicate consolidation (dream is idempotent).
    pub fn try_acquire(&self, stale_secs: u64) -> io::Result<Option<Option<SystemTime>>> {
        let prior = match fs::metadata(&self.path) {
            Ok(meta) => {
                let mtime = meta.modified()?;
                if let Ok(content) = fs::read_to_string(&self.path)
                    && let Ok(pid) = content.trim().parse::<u32>()
                {
                    let age = SystemTime::now()
                        .duration_since(mtime)
                        .unwrap_or_default()
                        .as_secs();
                    if age < stale_secs && is_process_alive(pid) {
                        return Ok(None);
                    }
                }
                Some(mtime)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };

        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let our_pid = std::process::id();
        fs::write(&self.path, our_pid.to_string())?;

        // Re-read to verify we won the race
        let content = fs::read_to_string(&self.path)?;
        if content.trim().parse::<u32>().ok() == Some(our_pid) {
            Ok(Some(prior))
        } else {
            Ok(None)
        }
    }

    /// Restore lock state after a failed dream.
    /// If `prior` is `None` (no prior file), deletes the lock file.
    pub fn rollback(&self, prior: Option<SystemTime>) -> io::Result<()> {
        match prior {
            None => {
                if let Err(e) = fs::remove_file(&self.path)
                    && e.kind() != io::ErrorKind::NotFound
                {
                    return Err(e);
                }
                Ok(())
            }
            Some(mtime) => {
                // Clear the PID body so our alive PID doesn't block future reclaimers.
                fs::write(&self.path, "")?;
                let file = fs::File::options().write(true).open(&self.path)?;
                file.set_times(fs::FileTimes::new().set_modified(mtime))
            }
        }
    }

    /// Stamp the lock file with the current time to record a consolidation.
    pub fn record_consolidation(&self) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&self.path, std::process::id().to_string())
    }
}

/// Count session files modified after `since`, excluding the current session.
///
/// Returns sorted file stems of matching `.md` files in `sessions_dir`.
pub fn sessions_since(
    sessions_dir: &Path,
    since: SystemTime,
    exclude_session_id: Option<&str>,
) -> io::Result<Vec<String>> {
    let entries = match fs::read_dir(sessions_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };

    let mut result = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();

        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }

        if let Some(exclude) = exclude_session_id
            && path
                .file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(|stem| crate::storage::session_log_belongs_to(stem, exclude))
        {
            continue;
        }

        if entry.metadata()?.modified()? > since
            && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
        {
            result.push(stem.to_owned());
        }
    }

    result.sort();
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use filetime::FileTime;
    use std::time::Duration;
    use tempfile::TempDir;

    #[cfg(unix)]
    #[test]
    fn sessions_since_ignores_symlinked_summaries() {
        let dir = TempDir::new().unwrap();
        let sessions = dir.path().join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        let outside = dir.path().join("outside.md");
        fs::write(&outside, "private text").unwrap();
        std::os::unix::fs::symlink(&outside, sessions.join("linked.md")).unwrap();
        assert!(
            sessions_since(&sessions, SystemTime::UNIX_EPOCH, None)
                .unwrap()
                .is_empty()
        );
    }

    // --- DreamLock tests ---

    #[test]
    fn no_file_means_no_prior_consolidation() {
        let dir = TempDir::new().unwrap();
        let lock = DreamLock::new(dir.path());
        assert!(lock.last_consolidated_at().unwrap().is_none());
    }

    #[test]
    fn acquire_on_empty_dir_writes_pid() {
        let dir = TempDir::new().unwrap();
        let lock = DreamLock::new(dir.path());

        let prior = lock.try_acquire(300).unwrap().expect("should acquire");
        assert!(prior.is_none(), "no prior file existed");

        let content = fs::read_to_string(&lock.path).unwrap();
        assert_eq!(content, std::process::id().to_string());
        assert!(lock.last_consolidated_at().unwrap().is_some());
    }

    #[test]
    fn rollback_none_deletes_file() {
        let dir = TempDir::new().unwrap();
        let lock = DreamLock::new(dir.path());

        let prior = lock.try_acquire(300).unwrap().unwrap();
        assert!(lock.path.exists());

        lock.rollback(prior).unwrap();
        assert!(!lock.path.exists());
        assert!(lock.last_consolidated_at().unwrap().is_none());
    }

    #[test]
    fn rollback_restores_prior_mtime() {
        let dir = TempDir::new().unwrap();
        let lock = DreamLock::new(dir.path());

        let old_time = SystemTime::now() - Duration::from_secs(7200);
        fs::write(&lock.path, "4000000000").unwrap(); // dead PID
        filetime::set_file_mtime(&lock.path, FileTime::from_system_time(old_time)).unwrap();

        let prior = lock
            .try_acquire(300)
            .unwrap()
            .expect("should reclaim dead PID");
        let prior_mtime = prior.expect("prior file existed");

        // mtime after acquire is fresh (from fs::write)
        let fresh = lock.last_consolidated_at().unwrap().unwrap();
        let fresh_age = SystemTime::now().duration_since(fresh).unwrap_or_default();
        assert!(fresh_age.as_secs() < 5);

        // Rollback restores old mtime
        lock.rollback(Some(prior_mtime)).unwrap();
        let restored = lock.last_consolidated_at().unwrap().unwrap();
        let drift = restored
            .duration_since(old_time)
            .or_else(|_| old_time.duration_since(restored))
            .unwrap();
        assert!(drift.as_secs() < 2, "mtime should be restored");
    }

    #[test]
    fn dead_pid_is_reclaimed() {
        let dir = TempDir::new().unwrap();
        let lock = DreamLock::new(dir.path());

        fs::write(&lock.path, "4000000000").unwrap();
        assert!(
            lock.try_acquire(300).unwrap().is_some(),
            "dead PID should be reclaimable"
        );

        let content = fs::read_to_string(&lock.path).unwrap();
        assert_eq!(content, std::process::id().to_string());
    }

    #[test]
    fn live_pid_blocks_acquisition() {
        let dir = TempDir::new().unwrap();
        let lock = DreamLock::new(dir.path());

        assert!(lock.try_acquire(300).unwrap().is_some(), "first acquire");
        assert!(
            lock.try_acquire(300).unwrap().is_none(),
            "second acquire should be blocked by live PID"
        );
    }

    #[test]
    fn stale_age_allows_reclaim_even_if_alive() {
        let dir = TempDir::new().unwrap();
        let lock = DreamLock::new(dir.path());

        fs::write(&lock.path, std::process::id().to_string()).unwrap();
        let old = SystemTime::now() - Duration::from_secs(600);
        filetime::set_file_mtime(&lock.path, FileTime::from_system_time(old)).unwrap();

        // stale_secs=300, age=600 → stale, should reclaim
        assert!(
            lock.try_acquire(300).unwrap().is_some(),
            "stale lock should be reclaimable"
        );
    }

    #[test]
    fn record_consolidation_creates_file() {
        let dir = TempDir::new().unwrap();
        let lock = DreamLock::new(dir.path());

        lock.record_consolidation().unwrap();
        assert!(lock.path.exists());

        let age = SystemTime::now()
            .duration_since(lock.last_consolidated_at().unwrap().unwrap())
            .unwrap_or_default();
        assert!(age.as_secs() < 5);
    }

    #[test]
    fn record_consolidation_updates_mtime() {
        let dir = TempDir::new().unwrap();
        let lock = DreamLock::new(dir.path());

        fs::write(&lock.path, "12345").unwrap();
        let old = SystemTime::now() - Duration::from_secs(7200);
        filetime::set_file_mtime(&lock.path, FileTime::from_system_time(old)).unwrap();

        lock.record_consolidation().unwrap();

        let age = SystemTime::now()
            .duration_since(lock.last_consolidated_at().unwrap().unwrap())
            .unwrap_or_default();
        assert!(age.as_secs() < 5, "mtime should be ~now");
    }

    #[test]
    fn full_lifecycle_acquire_consolidate_blocks_reacquire() {
        let dir = TempDir::new().unwrap();
        let lock = DreamLock::new(dir.path());

        let prior = lock.try_acquire(300).unwrap().unwrap();
        assert!(prior.is_none());

        lock.record_consolidation().unwrap();

        assert!(
            lock.try_acquire(300).unwrap().is_none(),
            "fresh consolidation should block re-acquire"
        );
    }

    #[test]
    fn rollback_on_nonexistent_file_is_noop() {
        let dir = TempDir::new().unwrap();
        let lock = DreamLock::new(dir.path());
        lock.rollback(None).unwrap(); // no file to delete, should be fine
    }

    #[test]
    fn corrupted_lock_body_is_reclaimable() {
        let dir = TempDir::new().unwrap();
        let lock = DreamLock::new(dir.path());

        fs::write(&lock.path, "not-a-pid").unwrap();
        assert!(
            lock.try_acquire(300).unwrap().is_some(),
            "unparseable PID should be reclaimable"
        );
    }

    #[test]
    fn empty_lock_body_is_reclaimable() {
        let dir = TempDir::new().unwrap();
        let lock = DreamLock::new(dir.path());

        fs::write(&lock.path, "").unwrap();
        assert!(
            lock.try_acquire(300).unwrap().is_some(),
            "empty body should be reclaimable"
        );
    }

    // --- sessions_since tests ---

    fn write_session(dir: &Path, name: &str, age_secs: u64) {
        fs::create_dir_all(dir).unwrap();
        let path = dir.join(format!("{name}.md"));
        fs::write(&path, "test").unwrap();
        let t = SystemTime::now() - Duration::from_secs(age_secs);
        filetime::set_file_mtime(&path, FileTime::from_system_time(t)).unwrap();
    }

    #[test]
    fn filters_by_mtime() {
        let dir = TempDir::new().unwrap();
        let sessions = dir.path().join("sessions");
        let cutoff = SystemTime::now() - Duration::from_secs(3600);

        write_session(&sessions, "2026-01-01-proj-aaa11111", 1800); // 30min ago, after cutoff
        write_session(&sessions, "2025-12-31-proj-bbb22222", 7200); // 2h ago, before cutoff

        let result = sessions_since(&sessions, cutoff, None).unwrap();
        assert_eq!(result, vec!["2026-01-01-proj-aaa11111"]);
    }

    #[test]
    fn mtime_at_exact_cutoff_is_excluded() {
        let dir = TempDir::new().unwrap();
        let sessions = dir.path().join("sessions");
        let cutoff = SystemTime::now() - Duration::from_secs(3600);

        // Set mtime to the exact cutoff value (not strictly after)
        fs::create_dir_all(&sessions).unwrap();
        let path = sessions.join("2026-01-01-proj-exact000.md");
        fs::write(&path, "test").unwrap();
        filetime::set_file_mtime(&path, FileTime::from_system_time(cutoff)).unwrap();

        let result = sessions_since(&sessions, cutoff, None).unwrap();
        assert!(
            result.is_empty(),
            "mtime == cutoff should be excluded (strict >)"
        );
    }

    #[test]
    fn excludes_current_session() {
        let dir = TempDir::new().unwrap();
        let sessions = dir.path().join("sessions");
        let cutoff = SystemTime::now() - Duration::from_secs(86400);

        write_session(&sessions, "2026-01-01-proj-aaa11111", 100);
        write_session(&sessions, "2026-01-01-proj-bbb22222", 100);

        let result = sessions_since(&sessions, cutoff, Some("bbb22222")).unwrap();
        assert_eq!(result, vec!["2026-01-01-proj-aaa11111"]);
    }

    #[test]
    fn empty_dir_returns_empty_vec() {
        let dir = TempDir::new().unwrap();
        let sessions = dir.path().join("sessions");
        fs::create_dir_all(&sessions).unwrap();

        let result = sessions_since(&sessions, SystemTime::UNIX_EPOCH, None).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn nonexistent_dir_returns_empty_vec() {
        let dir = TempDir::new().unwrap();
        let sessions = dir.path().join("nonexistent");

        let result = sessions_since(&sessions, SystemTime::UNIX_EPOCH, None).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn ignores_non_md_files() {
        let dir = TempDir::new().unwrap();
        let sessions = dir.path().join("sessions");
        fs::create_dir_all(&sessions).unwrap();

        write_session(&sessions, "2026-01-01-proj-aaa11111", 0);
        fs::write(sessions.join("notes.txt"), "not a session").unwrap();
        fs::write(sessions.join("data.json"), "{}").unwrap();

        let result = sessions_since(&sessions, SystemTime::UNIX_EPOCH, None).unwrap();
        assert_eq!(result, vec!["2026-01-01-proj-aaa11111"]);
    }

    #[test]
    fn returns_sorted_stems() {
        let dir = TempDir::new().unwrap();
        let sessions = dir.path().join("sessions");

        write_session(&sessions, "zzz-session", 0);
        write_session(&sessions, "aaa-session", 0);
        write_session(&sessions, "mmm-session", 0);

        let result = sessions_since(&sessions, SystemTime::UNIX_EPOCH, None).unwrap();
        assert_eq!(result, vec!["aaa-session", "mmm-session", "zzz-session"]);
    }
}
