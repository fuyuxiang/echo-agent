use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use echo_agent_workspace::util::is_lock_contended;
use fs2::FileExt;

use crate::util::echo_agent_home::echo_agent_home;

/// Compute a short hash suffix from a WS URL for differentiating leader instances.
/// Returns empty string for the default/production URL.
pub fn compute_ws_url_suffix(ws_url: &str) -> String {
    use std::hash::{Hash, Hasher};

    if ws_url.is_empty() {
        return String::new();
    }

    // Default production URL doesn't need a suffix.
    // `echo_agent_ws_url` is always the *relay* endpoint (see
    // [`crate::env::PROD_RELAY_WS_URL`]); the gateway URL never reaches
    // the leader-lock path-derivation code.
    if ws_url == crate::env::PROD_RELAY_WS_URL {
        return String::new();
    }

    // Compute a short hash of the URL
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    ws_url.hash(&mut hasher);
    let hash = hasher.finish();
    format!("-{:08x}", hash as u32)
}

/// Env var that overrides the leader socket path (and, by extension, the lock
/// path — the sibling `.lock`). Set by the `--leader-socket` flag, or exported
/// directly. Lets a developer sandbox a leader instance away from the default
/// `~/.echo-agent/leader.sock` — e.g. run a local branch build's leader without
/// colliding with an installed stable leader on the same machine. Honored by
/// BOTH the client (`connect_or_spawn`) and the leader (`run_leader`), and
/// inherited by the spawned leader subprocess, so all parties bind the same
/// path. When set, the WS-URL-derived suffix (`compute_ws_url_suffix`) is
/// bypassed entirely.
pub const LEADER_SOCKET_ENV: &str = "ECHO_AGENT_LEADER_SOCKET";

/// The explicit socket-path override, if [`LEADER_SOCKET_ENV`] is set and
/// non-empty.
fn leader_socket_override() -> Option<PathBuf> {
    std::env::var_os(LEADER_SOCKET_ENV)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// The lock path paired with a given socket path: the sibling file with a
/// `.lock` extension (`/x/leader-foo.sock` → `/x/leader-foo.lock`). Matches the
/// default `leader.sock`/`leader.lock` pairing so the two never disagree.
fn lock_path_for_socket(socket: &Path) -> PathBuf {
    socket.with_extension("lock")
}

/// Resolve the socket path: the explicit override wins, else the WS-URL-derived
/// default under `root`. Pure (the override is passed in) so it is unit-testable
/// without touching process env.
fn resolve_socket_path(override_socket: Option<PathBuf>, root: &Path, ws_url: &str) -> PathBuf {
    override_socket.unwrap_or_else(|| socket_path_for_ws_url_in(root, ws_url))
}

/// Resolve the lock path: the sibling `.lock` of the override socket if set,
/// else the WS-URL-derived default under `root`. Pure (see
/// [`resolve_socket_path`]).
fn resolve_lock_path(override_socket: Option<PathBuf>, root: &Path, ws_url: &str) -> PathBuf {
    match override_socket {
        Some(socket) => lock_path_for_socket(&socket),
        None => lock_path_for_ws_url_in(root, ws_url),
    }
}

pub fn lock_path_for_ws_url_in(root: &Path, ws_url: &str) -> PathBuf {
    let suffix = compute_ws_url_suffix(ws_url);
    root.join(format!("leader{}.lock", suffix))
}

pub fn lock_path_for_ws_url(ws_url: &str) -> PathBuf {
    resolve_lock_path(leader_socket_override(), &echo_agent_home(), ws_url)
}

pub fn socket_path_for_ws_url_in(root: &Path, ws_url: &str) -> PathBuf {
    let suffix = compute_ws_url_suffix(ws_url);
    root.join(format!("leader{}.sock", suffix))
}

pub fn socket_path_for_ws_url(ws_url: &str) -> PathBuf {
    resolve_socket_path(leader_socket_override(), &echo_agent_home(), ws_url)
}

pub fn ws_url_suffix_from_paths(lock_path: &Path, socket_path: &Path) -> Option<String> {
    let lock_name = lock_path.file_name()?.to_str()?;
    let socket_name = socket_path.file_name()?.to_str()?;
    let lock_suffix = lock_name
        .strip_prefix("leader")?
        .strip_suffix(".lock")?
        .to_string();
    let socket_suffix = socket_name
        .strip_prefix("leader")?
        .strip_suffix(".sock")?
        .to_string();

    if lock_suffix == socket_suffix {
        Some(lock_suffix)
    } else {
        None
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),
    #[error("Lock held by another process")]
    AlreadyLocked,
    #[error("Timed out waiting to acquire lock after {0:?}")]
    Timeout(Duration),
}

/// Lock manager for the leader process using OS-level file locking (flock).
///
/// The lock file serves two purposes:
/// 1. Exclusive lock indicates who is the leader (or who is spawning)
/// 2. File contents store the leader's PID for diagnostics
///
/// Lock semantics:
/// - Leader holds exclusive lock for its entire lifetime
/// - Clients use try_lock to check if leader exists and coordinate spawning
///
/// Cleanup behavior:
/// - If lock is held when dropped (crash/exit), files are cleaned up
/// - If `release()` is called before drop, files are NOT cleaned up (handoff to leader)
#[derive(Debug)]
pub struct LeaderLock {
    lock_path: PathBuf,
    sock_path: PathBuf,
    lock_file: Option<File>,
    /// Tracks if we should clean up files on drop.
    /// Set to true when lock is acquired, set to false when explicitly released.
    /// This ensures cleanup happens if we crash while holding the lock,
    /// but NOT if we explicitly hand off to another process via release().
    was_leader: bool,
}

impl LeaderLock {
    /// Create a new LeaderLock using the default paths in echoagent home.
    /// If ws_url differs from the default production URL, a hash suffix is added
    /// to the lock and socket file names to differentiate leader instances.
    pub fn new(ws_url: &str) -> Self {
        Self {
            lock_path: lock_path_for_ws_url(ws_url),
            sock_path: socket_path_for_ws_url(ws_url),
            lock_file: None,
            was_leader: false,
        }
    }

    pub fn socket_path(&self) -> &PathBuf {
        &self.sock_path
    }

    pub fn lock_path(&self) -> &PathBuf {
        &self.lock_path
    }

    /// Open (or create) the lock file for subsequent locking operations.
    fn open_lock_file(&self) -> Result<File, LockError> {
        Ok(OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.lock_path)?)
    }

    /// Record a successful lock acquisition in our state.
    fn mark_acquired(&mut self, file: File) {
        self.lock_file = Some(file);
        self.was_leader = true;
    }

    /// Try to acquire exclusive lock without blocking.
    ///
    /// Returns `Ok(true)` if lock acquired, `Ok(false)` if already held by another process.
    /// After acquiring, call `write_pid()` to record the leader's PID.
    pub fn try_acquire(&mut self) -> Result<bool, LockError> {
        let file = self.open_lock_file()?;

        match file.try_lock_exclusive() {
            Ok(()) => {
                self.mark_acquired(file);
                Ok(true)
            }
            Err(e) if is_lock_contended(&e) => Ok(false),
            Err(e) => Err(LockError::Io(e)),
        }
    }

    /// Acquire exclusive lock with a bounded wait, re-opening the lock-file path
    /// on every attempt.
    ///
    /// Polls `try_lock_exclusive()` every 200ms until acquired or the timeout
    /// elapses (`LockError::Timeout`). The re-open is load-bearing on the leader
    /// path: an old-flow client's `Drop` unlinks the lock file on its timeout, so
    /// the winner must acquire on the freshly re-created inode — a single held fd
    /// would keep polling the stale, unlinked inode forever.
    ///
    /// Async so the 200ms poll yields to the Tokio runtime instead of blocking a
    /// worker thread — `run_leader` calls this on the multi-thread runtime.
    pub(crate) async fn acquire_reopen_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<(), LockError> {
        let deadline = Instant::now() + timeout;
        let poll_interval = Duration::from_millis(200);

        loop {
            // Re-open each attempt: the inode may have been replaced since the last poll.
            let file = self.open_lock_file()?;
            match file.try_lock_exclusive() {
                Ok(()) => {
                    self.mark_acquired(file);
                    return Ok(());
                }
                Err(e) if is_lock_contended(&e) => {
                    drop(file); // release the fd before sleeping; re-open next poll
                    if Instant::now() >= deadline {
                        return Err(LockError::Timeout(timeout));
                    }
                    tokio::time::sleep(poll_interval).await;
                }
                Err(e) => return Err(LockError::Io(e)),
            }
        }
    }

    /// Write our PID to the lock file. Call after acquiring lock.
    pub fn write_pid(&mut self) -> Result<(), LockError> {
        if let Some(ref mut file) = self.lock_file {
            file.set_len(0)?;
            write!(file, "{}", std::process::id())?;
            file.sync_all()?;
        }
        Ok(())
    }

    /// Read PID from lock file (for diagnostics).
    pub fn read_pid(&self) -> Option<u32> {
        Self::read_pid_from_path(&self.lock_path)
    }

    pub(crate) fn read_pid_from_path(path: &Path) -> Option<u32> {
        let mut content = String::new();
        File::open(path)
            .and_then(|mut f| f.read_to_string(&mut content))
            .ok()?;
        content.trim().parse().ok()
    }

    /// Delete the socket file. Call while holding the lock.
    pub(crate) fn cleanup_socket(&self) -> io::Result<()> {
        match fs::remove_file(&self.sock_path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Release the lock explicitly. `Drop` will NOT clean up files afterward.
    pub fn release(&mut self) -> io::Result<()> {
        // Clear FIRST: even if `unlock()` errors, `Drop` must not delete the live
        // child leader's socket.
        self.was_leader = false;
        if let Some(file) = self.lock_file.take() {
            file.unlock()?;
        }
        Ok(())
    }

    /// Check if we currently hold the lock.
    pub fn is_held(&self) -> bool {
        self.lock_file.is_some()
    }
}

#[cfg(test)]
impl LeaderLock {
    /// Bind a lock to explicit paths for tests running outside the default home.
    pub(crate) fn from_paths(lock_path: PathBuf, sock_path: PathBuf) -> Self {
        Self {
            lock_path,
            sock_path,
            lock_file: None,
            was_leader: false,
        }
    }
}

impl Drop for LeaderLock {
    fn drop(&mut self) {
        // Lock is automatically released when file is closed.
        // We only clean up files if was_leader is true, which means:
        // - We acquired the lock AND
        // - We did NOT call release() (which clears was_leader)
        // This ensures the spawner doesn't delete files when handing off to the leader.
        if self.was_leader {
            let _ = fs::remove_file(&self.lock_path);
            let _ = fs::remove_file(&self.sock_path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_lock(temp: &TempDir) -> LeaderLock {
        LeaderLock::from_paths(
            temp.path().join("leader.lock"),
            temp.path().join("leader.sock"),
        )
    }

    #[test]
    fn override_socket_path_wins_over_ws_url_derivation() {
        let root = Path::new("/home/u/.echo-agent");
        let override_sock = PathBuf::from("/home/u/.echo-agent/leader-branch.sock");

        // With an override, the path is taken verbatim and the WS-URL suffix is
        // ignored (a non-default ws_url would otherwise add a hash suffix).
        assert_eq!(
            resolve_socket_path(Some(override_sock.clone()), root, "wss://custom.example/ws"),
            override_sock
        );
        // The lock is the sibling `.lock`, NOT a ws-url-derived name.
        assert_eq!(
            resolve_lock_path(Some(override_sock), root, "wss://custom.example/ws"),
            PathBuf::from("/home/u/.echo-agent/leader-branch.lock")
        );
    }

    #[test]
    fn no_override_falls_back_to_ws_url_derivation() {
        let root = Path::new("/home/u/.echo-agent");
        // Default (empty) ws_url → bare leader.sock / leader.lock under root.
        assert_eq!(
            resolve_socket_path(None, root, ""),
            root.join("leader.sock")
        );
        assert_eq!(resolve_lock_path(None, root, ""), root.join("leader.lock"));
    }

    #[test]
    fn lock_path_for_socket_swaps_extension() {
        assert_eq!(
            lock_path_for_socket(Path::new("/x/leader-foo.sock")),
            PathBuf::from("/x/leader-foo.lock")
        );
        // A socket path without an extension still gets a `.lock` sibling.
        assert_eq!(
            lock_path_for_socket(Path::new("/x/myleader")),
            PathBuf::from("/x/myleader.lock")
        );
    }

    #[test]
    fn try_acquire_succeeds_when_unlocked() {
        let temp = TempDir::new().unwrap();
        let mut lock = test_lock(&temp);

        assert!(lock.try_acquire().unwrap());
        assert!(lock.is_held());
    }

    #[test]
    fn try_acquire_fails_when_held() {
        let temp = TempDir::new().unwrap();
        let mut lock1 = test_lock(&temp);
        let mut lock2 = test_lock(&temp);

        assert!(lock1.try_acquire().unwrap());
        assert!(!lock2.try_acquire().unwrap()); // Should return false, not error
    }

    #[test]
    fn write_and_read_pid() {
        let temp = TempDir::new().unwrap();
        let mut lock = test_lock(&temp);

        lock.try_acquire().unwrap();
        lock.write_pid().unwrap();

        let pid = lock.read_pid().unwrap();
        assert_eq!(pid, std::process::id());
        assert_eq!(
            LeaderLock::read_pid_from_path(lock.lock_path()),
            Some(std::process::id())
        );
    }

    #[test]
    fn derived_lock_and_socket_paths_match_suffix() {
        let ws_url = "wss://relay.staging.example/ws/code-agent";
        let lock_path = lock_path_for_ws_url(ws_url);
        let socket_path = socket_path_for_ws_url(ws_url);

        assert_eq!(
            ws_url_suffix_from_paths(&lock_path, &socket_path),
            Some(compute_ws_url_suffix(ws_url))
        );
    }

    #[test]
    fn cleanup_socket_removes_file() {
        let temp = TempDir::new().unwrap();
        let mut lock = test_lock(&temp);

        // Create socket file
        fs::write(&lock.sock_path, "").unwrap();
        assert!(lock.sock_path.exists());

        lock.try_acquire().unwrap();
        lock.cleanup_socket().unwrap();

        assert!(!lock.sock_path.exists());
    }

    #[test]
    fn cleanup_socket_ok_if_missing() {
        let temp = TempDir::new().unwrap();
        let mut lock = test_lock(&temp);

        lock.try_acquire().unwrap();
        // Should not error even if socket doesn't exist
        lock.cleanup_socket().unwrap();
    }

    #[test]
    fn release_allows_reacquisition() {
        let temp = TempDir::new().unwrap();
        let mut lock1 = test_lock(&temp);
        let mut lock2 = test_lock(&temp);

        assert!(lock1.try_acquire().unwrap());
        lock1.release().unwrap();

        assert!(lock2.try_acquire().unwrap());
    }

    #[test]
    fn drop_releases_lock() {
        let temp = TempDir::new().unwrap();
        let mut lock2 = test_lock(&temp);

        {
            let mut lock1 = test_lock(&temp);
            assert!(lock1.try_acquire().unwrap());
            // lock1 dropped here
        }

        // lock2 should be able to acquire now
        assert!(lock2.try_acquire().unwrap());
    }

    #[test]
    fn release_prevents_file_cleanup_on_drop() {
        let temp = TempDir::new().unwrap();
        let mut lock = test_lock(&temp);

        // Create socket file (simulating leader binding)
        fs::write(&lock.sock_path, "").unwrap();
        assert!(lock.sock_path.exists());

        // Acquire and then release (simulating spawner handoff)
        assert!(lock.try_acquire().unwrap());
        lock.release().unwrap();

        // Drop should NOT delete the socket file
        drop(lock);

        // Socket file should still exist (leader would still be using it)
        assert!(
            temp.path().join("leader.sock").exists(),
            "Socket file should NOT be deleted after release()"
        );
    }

    #[test]
    fn drop_without_release_cleans_up_files() {
        let temp = TempDir::new().unwrap();

        {
            let mut lock = test_lock(&temp);

            // Create socket file
            fs::write(&lock.sock_path, "").unwrap();
            assert!(lock.sock_path.exists());

            // Acquire but do NOT release (simulating crash/normal exit)
            assert!(lock.try_acquire().unwrap());
            // lock dropped here without release()
        }

        // Socket file should be deleted
        assert!(
            !temp.path().join("leader.sock").exists(),
            "Socket file SHOULD be deleted when dropped without release()"
        );
    }

    #[test]
    fn read_pid_returns_none_for_missing_lock_file() {
        let temp = TempDir::new().unwrap();
        let lock = test_lock(&temp);
        // Lock file doesn't exist yet
        assert!(lock.read_pid().is_none());
        assert!(LeaderLock::read_pid_from_path(lock.lock_path()).is_none());
    }

    #[test]
    fn read_pid_returns_none_for_empty_lock_file() {
        let temp = TempDir::new().unwrap();
        let lock = test_lock(&temp);
        fs::write(lock.lock_path(), "").unwrap();
        assert!(lock.read_pid().is_none());
    }

    #[test]
    fn read_pid_returns_none_for_non_numeric_content() {
        let temp = TempDir::new().unwrap();
        let lock = test_lock(&temp);
        fs::write(lock.lock_path(), "not-a-pid").unwrap();
        assert!(lock.read_pid().is_none());
    }

    #[tokio::test]
    async fn acquire_reopen_timeout_succeeds_when_unlocked() {
        let temp = TempDir::new().unwrap();
        let mut lock = test_lock(&temp);

        lock.acquire_reopen_timeout(Duration::from_secs(1))
            .await
            .unwrap();
        assert!(lock.is_held());
    }

    #[tokio::test]
    async fn acquire_reopen_timeout_returns_timeout_when_held() {
        let temp = TempDir::new().unwrap();
        let mut lock1 = test_lock(&temp);
        let mut lock2 = test_lock(&temp);

        assert!(lock1.try_acquire().unwrap());

        let result = lock2
            .acquire_reopen_timeout(Duration::from_millis(500))
            .await;
        assert!(
            matches!(result, Err(LockError::Timeout(_))),
            "Expected Timeout error, got {:?}",
            result
        );
        assert!(!lock2.is_held());
    }

    /// The re-open is load-bearing: while `lock1` holds the flock on the ORIGINAL
    /// (now-unlinked) inode for the whole test, re-opening the path each poll lets
    /// the waiter acquire on a fresh inode. A single-fd waiter would time out here.
    #[tokio::test]
    async fn acquire_reopen_timeout_tolerates_unlinked_recreated_lock_file() {
        let temp = TempDir::new().unwrap();
        let mut lock1 = test_lock(&temp);
        let mut lock2 = test_lock(&temp);

        assert!(lock1.try_acquire().unwrap()); // inode A, held for the whole test
        let lock_path = lock1.lock_path().clone();

        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            // Simulate the old-flow client's Drop unlinking the lock file while it
            // still holds the (now-anonymous) inode.
            fs::remove_file(&lock_path).unwrap();
            lock1 // return to keep inode A flock-held until the waiter has acquired
        });

        lock2
            .acquire_reopen_timeout(Duration::from_secs(5))
            .await
            .unwrap();
        assert!(lock2.is_held());

        let _lock1 = handle.join().unwrap();
    }

    /// Mirrors `run_leader`'s lock-then-socket guard: only the flock winner
    /// binds the socket; a loser returns `false` without touching it.
    fn try_start_leader(lock: &mut LeaderLock, socket_contents: &str) -> bool {
        match lock.try_acquire() {
            Ok(true) => {
                lock.cleanup_socket().unwrap();
                fs::write(lock.socket_path(), socket_contents).unwrap();
                true
            }
            Ok(false) | Err(_) => false,
        }
    }

    /// Single-leader invariant: a racing would-be leader that loses the flock
    /// must not touch the socket.
    #[test]
    fn racing_leader_without_flock_cannot_clobber_socket() {
        let temp = TempDir::new().unwrap();
        let mut leader1 = test_lock(&temp);
        let mut leader2 = test_lock(&temp);

        assert!(try_start_leader(&mut leader1, "leader1-socket"));
        assert!(!try_start_leader(&mut leader2, "leader2-socket"));

        // Leader 1's socket survives untouched.
        assert!(leader1.socket_path().exists());
        assert_eq!(
            fs::read_to_string(leader1.socket_path()).unwrap(),
            "leader1-socket"
        );
    }

    /// The leader holds the flock continuously for its lifetime (released only on
    /// `Drop`), so no second leader can acquire it while the leader is alive.
    #[test]
    fn flock_held_continuously_blocks_second_leader_until_drop() {
        let temp = TempDir::new().unwrap();
        let mut contender = test_lock(&temp);

        {
            let mut leader = test_lock(&temp);
            assert!(leader.try_acquire().unwrap());
            leader.write_pid().unwrap();

            assert!(!contender.try_acquire().unwrap());
            assert!(!contender.try_acquire().unwrap());
            // leader dropped here (simulating exit) → flock released, files cleaned
        }

        assert!(contender.try_acquire().unwrap());
    }

    #[tokio::test]
    async fn acquire_reopen_timeout_succeeds_after_release() {
        let temp = TempDir::new().unwrap();
        let mut lock1 = test_lock(&temp);
        let mut lock2 = test_lock(&temp);

        assert!(lock1.try_acquire().unwrap());

        // Release lock1 in a background thread after a short delay
        let lock_path = lock1.lock_path.clone();
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            lock1.release().unwrap();
            lock_path // keep the path for verification, lock1 is consumed
        });

        // lock2 should acquire within the timeout because lock1 is released after 200ms
        lock2
            .acquire_reopen_timeout(Duration::from_secs(5))
            .await
            .unwrap();
        assert!(lock2.is_held());

        handle.join().unwrap();
    }
}
