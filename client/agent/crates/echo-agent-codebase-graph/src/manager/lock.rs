//! Workspace-level locking for index operations.
//!
//! Provides both in-memory (same-process) and file-based (cross-process)
//! coordination to prevent redundant index operations on the same workspace.
//!
//! ## Design
//!
//! - **In-memory locks**: Fast path for same-process deduplication using a global registry
//! - **File locks**: Cross-process coordination using lock files with PID and timestamp
//! - **Stale detection**: Locks are considered stale if the holding process is dead or timeout exceeded
//!
//! ## Lock Types
//!
//! - **Shared (Load)**: Multiple readers allowed, blocked during exclusive operations
//! - **Exclusive (Save/Build/Refresh)**: Single writer, blocks all other operations

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dashmap::DashMap;
use once_cell::sync::Lazy;

const LOAD_STALE_DURATION_SEC: u64 = 120;
const SAVE_STALE_DURATION_SEC: u64 = 120;
const BUILD_STALE_DURATION_SEC: u64 = 600;
const BG_REFRESH_STALE_DURATION_SEC: u64 = 300;

/// Operations that require locking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexOperation {
    /// Loading index from cache (shared/read lock).
    Load,
    /// Saving index to cache (exclusive).
    Save,
    /// Building index from scratch (exclusive).
    Build,
    /// Background validation and refresh (exclusive).
    BackgroundRefresh,
}

impl IndexOperation {
    /// String representation for lock file.
    fn as_str(&self) -> &'static str {
        match self {
            Self::Load => "load",
            Self::Save => "save",
            Self::Build => "build",
            Self::BackgroundRefresh => "background_refresh",
        }
    }

    /// Whether this operation requires exclusive access.
    pub fn is_exclusive(&self) -> bool {
        match self {
            Self::Load => false, // Shared/read access
            Self::Save | Self::Build | Self::BackgroundRefresh => true,
        }
    }

    /// Timeout after which a lock is considered stale.
    fn stale_timeout(&self) -> Duration {
        match self {
            Self::Load => Duration::from_secs(LOAD_STALE_DURATION_SEC),
            Self::Save => Duration::from_secs(SAVE_STALE_DURATION_SEC),
            Self::Build => Duration::from_secs(BUILD_STALE_DURATION_SEC),
            Self::BackgroundRefresh => Duration::from_secs(BG_REFRESH_STALE_DURATION_SEC),
        }
    }
}

impl std::fmt::Display for IndexOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// In-memory lock state for same-process deduplication.
struct InMemoryLockState {
    operation: IndexOperation,
    readers: usize,  // Count for shared locks
    exclusive: bool, // Whether an exclusive lock is held
}

/// Global registry of in-memory locks (same process).
/// Uses DashMap for lock-free concurrent access.
static IN_MEMORY_LOCKS: Lazy<DashMap<PathBuf, InMemoryLockState>> = Lazy::new(DashMap::new);

/// A guard that releases the lock when dropped.
pub struct WorkspaceLockGuard {
    workspace: PathBuf,
    lock_file_path: PathBuf,
    operation: IndexOperation,
}

impl Drop for WorkspaceLockGuard {
    fn drop(&mut self) {
        // Release in-memory lock
        release_in_memory_lock(&self.workspace, self.operation);

        // Release file lock (only for exclusive operations)
        if self.operation.is_exclusive()
            && let Err(e) = std::fs::remove_file(&self.lock_file_path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(
                path = %self.lock_file_path.display(),
                error = %e,
                "Failed to remove lock file"
            );
        }
    }
}

/// Result of trying to acquire a lock.
pub enum LockResult {
    /// Lock acquired successfully.
    Acquired(WorkspaceLockGuard),
    /// Another operation is in progress.
    Busy {
        /// Description of the blocking operation.
        operation: String,
        /// PID of the process holding the lock (if known).
        holder_pid: Option<u32>,
    },
}

impl LockResult {
    /// Returns true if the lock was acquired.
    pub fn is_acquired(&self) -> bool {
        matches!(self, Self::Acquired(_))
    }

    /// Unwrap the guard, panicking if busy.
    pub fn unwrap(self) -> WorkspaceLockGuard {
        match self {
            Self::Acquired(guard) => guard,
            Self::Busy { operation, .. } => {
                panic!("Lock was busy: {}", operation)
            }
        }
    }
}

/// Try to acquire a lock for an index operation on a workspace.
///
/// Returns `LockResult::Acquired` with a guard if successful, or `LockResult::Busy`
/// if another operation is in progress.
///
/// # Arguments
///
/// * `workspace` - The workspace root path
/// * `operation` - The type of operation to perform
///
/// # Example
///
/// ```ignore
/// use echo_agent_codebase_graph::manager::lock::{try_lock, IndexOperation, LockResult};
///
/// let workspace = Path::new("/path/to/workspace");
/// match try_lock(workspace, IndexOperation::Build) {
///     LockResult::Acquired(guard) => {
///         // Do work...
///         // Lock is released when guard is dropped
///     }
///     LockResult::Busy { operation, holder_pid } => {
///         println!("Busy: {} by pid {:?}", operation, holder_pid);
///     }
/// }
/// ```
pub fn try_lock(workspace: &Path, operation: IndexOperation) -> LockResult {
    let workspace = canonicalize_workspace(workspace);
    let lock_file_path = get_lock_file_path(&workspace);

    // Step 1: Check/acquire in-memory lock (fast path for same process)
    if !try_acquire_in_memory_lock(&workspace, operation) {
        tracing::debug!(
            workspace = %workspace.display(),
            operation = %operation,
            "In-memory lock busy"
        );
        return LockResult::Busy {
            operation: format!("{} (same process)", operation),
            holder_pid: Some(std::process::id()),
        };
    }

    // Step 2: For exclusive operations, also acquire file lock (cross-process)
    if operation.is_exclusive() {
        match try_acquire_file_lock(&lock_file_path, operation) {
            Ok(()) => {
                tracing::debug!(
                    workspace = %workspace.display(),
                    operation = %operation,
                    lock_file = %lock_file_path.display(),
                    "Acquired exclusive lock"
                );
            }
            Err((op, pid)) => {
                // Release in-memory lock since we failed to get file lock
                release_in_memory_lock(&workspace, operation);
                tracing::debug!(
                    workspace = %workspace.display(),
                    operation = %operation,
                    blocking_op = %op,
                    blocking_pid = ?pid,
                    "File lock busy"
                );
                return LockResult::Busy {
                    operation: op,
                    holder_pid: pid,
                };
            }
        }
    }

    LockResult::Acquired(WorkspaceLockGuard {
        workspace,
        lock_file_path,
        operation,
    })
}

/// Check if an operation is currently in progress for a workspace.
///
/// This is a non-blocking check that doesn't acquire any locks.
pub fn is_operation_in_progress(workspace: &Path, operation: IndexOperation) -> bool {
    let workspace = canonicalize_workspace(workspace);

    // Check in-memory first
    if let Some(state) = IN_MEMORY_LOCKS.get(&workspace) {
        if operation.is_exclusive() {
            if state.readers > 0 || state.exclusive {
                return true;
            }
        } else if state.exclusive {
            return true;
        }
    }

    // Check file lock for exclusive operations
    if operation.is_exclusive()
        && let Ok(contents) = std::fs::read_to_string(get_lock_file_path(&workspace))
        && let Some((_, pid, started)) = parse_lock_file(&contents)
    {
        let age = SystemTime::now()
            .duration_since(started)
            .unwrap_or(Duration::ZERO);
        if age < operation.stale_timeout() && is_process_alive(pid) {
            return true;
        }
    }

    false
}

/// Canonicalize workspace path for consistent lock keys.
fn canonicalize_workspace(workspace: &Path) -> PathBuf {
    // Try to canonicalize, fall back to the original path
    dunce::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf())
}

/// Get the lock file path for a workspace.
fn get_lock_file_path(workspace: &Path) -> PathBuf {
    // Use the cache directory (same as where .goto_index.bin is stored)
    let cache_path = super::get_cache_path(workspace);
    cache_path.with_extension("lock")
}

/// Try to acquire an in-memory lock for same-process deduplication.
fn try_acquire_in_memory_lock(workspace: &Path, operation: IndexOperation) -> bool {
    // Use entry API for atomic check-and-modify
    match IN_MEMORY_LOCKS.entry(workspace.to_path_buf()) {
        dashmap::mapref::entry::Entry::Occupied(mut entry) => {
            let state = entry.get_mut();
            if operation.is_exclusive() {
                // Exclusive operation - must have no readers or existing exclusive
                if state.readers > 0 || state.exclusive {
                    return false;
                }
                state.exclusive = true;
                state.operation = operation;
            } else {
                // Shared operation (Load) - OK if no exclusive lock
                if state.exclusive {
                    return false;
                }
                state.readers += 1;
            }
        }
        dashmap::mapref::entry::Entry::Vacant(entry) => {
            // No existing lock - create one
            entry.insert(InMemoryLockState {
                operation,
                readers: if operation.is_exclusive() { 0 } else { 1 },
                exclusive: operation.is_exclusive(),
            });
        }
    }
    true
}

/// Release an in-memory lock.
fn release_in_memory_lock(workspace: &Path, operation: IndexOperation) {
    // Use entry API for atomic check-and-modify
    if let dashmap::mapref::entry::Entry::Occupied(mut entry) =
        IN_MEMORY_LOCKS.entry(workspace.to_path_buf())
    {
        let should_remove = {
            let state = entry.get_mut();
            if operation.is_exclusive() {
                state.exclusive = false;
            } else {
                state.readers = state.readers.saturating_sub(1);
            }
            // Check if we should remove the entry
            !state.exclusive && state.readers == 0
        };

        if should_remove {
            entry.remove();
        }
    }
}

/// Try to acquire a file-based lock for cross-process coordination.
fn try_acquire_file_lock(
    lock_path: &Path,
    operation: IndexOperation,
) -> Result<(), (String, Option<u32>)> {
    // Check if existing lock file is valid
    if let Ok(contents) = std::fs::read_to_string(lock_path)
        && let Some((op, pid, started)) = parse_lock_file(&contents)
    {
        // Check if lock is stale
        let age = SystemTime::now()
            .duration_since(started)
            .unwrap_or(Duration::ZERO);

        if age < operation.stale_timeout() && is_process_alive(pid) {
            return Err((op, Some(pid)));
        }
        // Lock is stale - we can take over
        tracing::debug!(
            lock_path = %lock_path.display(),
            stale_op = %op,
            stale_pid = pid,
            age_secs = age.as_secs(),
            "Taking over stale lock"
        );
    }

    // Create parent directory if needed
    if let Some(parent) = lock_path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        tracing::warn!(
            path = %parent.display(),
            error = %e,
            "Failed to create lock directory"
        );
    }

    // Write our lock file
    let contents = format!(
        "operation={}\npid={}\nstarted={}\nworkspace={}\n",
        operation.as_str(),
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
        lock_path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
    );

    std::fs::write(lock_path, contents).map_err(|e| (format!("io_error: {}", e), None))
}

/// Parse a lock file's contents.
fn parse_lock_file(contents: &str) -> Option<(String, u32, SystemTime)> {
    let mut operation = None;
    let mut pid = None;
    let mut started = None;

    for line in contents.lines() {
        if let Some(val) = line.strip_prefix("operation=") {
            operation = Some(val.to_string());
        } else if let Some(val) = line.strip_prefix("pid=") {
            pid = val.parse().ok();
        } else if let Some(val) = line.strip_prefix("started=")
            && let Ok(secs) = val.parse::<u64>()
        {
            started = Some(UNIX_EPOCH + Duration::from_secs(secs));
        }
    }

    match (operation, pid, started) {
        (Some(op), Some(p), Some(s)) => Some((op, p, s)),
        _ => None,
    }
}

/// Check if a process is still alive.
#[cfg(unix)]
fn is_process_alive(pid: u32) -> bool {
    // kill with signal 0 checks if process exists without sending a signal
    // Returns 0 if process exists and we have permission to send signals
    // Returns -1 with ESRCH if process doesn't exist
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(not(unix))]
fn is_process_alive(_pid: u32) -> bool {
    // On non-Unix platforms, rely on timeout-based stale detection
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_exclusive_lock_blocks_exclusive() {
        let dir = tempdir().unwrap();
        let workspace = dir.path();

        // Acquire first exclusive lock
        let guard1 = try_lock(workspace, IndexOperation::Build);
        assert!(guard1.is_acquired());

        // Second exclusive lock should fail
        let result2 = try_lock(workspace, IndexOperation::Build);
        assert!(!result2.is_acquired());

        // Drop first lock
        drop(guard1);

        // Now second should succeed
        let guard3 = try_lock(workspace, IndexOperation::Build);
        assert!(guard3.is_acquired());
    }

    #[test]
    fn test_shared_locks_coexist() {
        let dir = tempdir().unwrap();
        let workspace = dir.path();

        // Multiple shared locks should work
        let guard1 = try_lock(workspace, IndexOperation::Load);
        assert!(guard1.is_acquired());

        let guard2 = try_lock(workspace, IndexOperation::Load);
        assert!(guard2.is_acquired());

        let guard3 = try_lock(workspace, IndexOperation::Load);
        assert!(guard3.is_acquired());
    }

    #[test]
    fn test_exclusive_blocks_shared() {
        let dir = tempdir().unwrap();
        let workspace = dir.path();

        // Acquire exclusive lock
        let guard1 = try_lock(workspace, IndexOperation::Build);
        assert!(guard1.is_acquired());

        // Shared lock should fail
        let result2 = try_lock(workspace, IndexOperation::Load);
        assert!(!result2.is_acquired());
    }

    #[test]
    fn test_shared_blocks_exclusive() {
        let dir = tempdir().unwrap();
        let workspace = dir.path();

        // Acquire shared lock
        let guard1 = try_lock(workspace, IndexOperation::Load);
        assert!(guard1.is_acquired());

        // Exclusive lock should fail
        let result2 = try_lock(workspace, IndexOperation::Build);
        assert!(!result2.is_acquired());

        // Drop shared lock
        drop(guard1);

        // Now exclusive should succeed
        let guard3 = try_lock(workspace, IndexOperation::Build);
        assert!(guard3.is_acquired());
    }

    #[test]
    fn test_different_workspaces_independent() {
        let dir1 = tempdir().unwrap();
        let dir2 = tempdir().unwrap();

        // Locks on different workspaces should be independent
        let guard1 = try_lock(dir1.path(), IndexOperation::Build);
        assert!(guard1.is_acquired());

        let guard2 = try_lock(dir2.path(), IndexOperation::Build);
        assert!(guard2.is_acquired());
    }

    #[test]
    fn test_lock_file_created_for_exclusive() {
        let dir = tempdir().unwrap();
        let workspace = dir.path();

        let lock_file = get_lock_file_path(workspace);

        // No lock file initially
        assert!(!lock_file.exists());

        // Acquire exclusive lock
        let guard = try_lock(workspace, IndexOperation::Build);
        assert!(guard.is_acquired());

        // Lock file should exist
        assert!(lock_file.exists());

        // Check contents
        let contents = std::fs::read_to_string(&lock_file).unwrap();
        assert!(contents.contains("operation=build"));
        assert!(contents.contains(&format!("pid={}", std::process::id())));

        // Drop guard
        drop(guard);

        // Lock file should be removed
        assert!(!lock_file.exists());
    }
}
