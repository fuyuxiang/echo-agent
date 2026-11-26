//! File operation lock manager — serializes concurrent file operations.
//!
//!   diagnostics for each file. Multiple reads for *different* paths can proceed
//!   concurrently; reads for the *same* path are serialized.
//! - **Exclusive lock** (`wait_for_exclusive_lock`): used by `Write` and
//!   `StrReplace` before mutating files. Blocks all per-path locks and
//!   vice-versa.
//!
//! The queue is FIFO with priority inversion avoidance: per-path waiters
//! will not jump ahead of a queued exclusive waiter, preventing writer
//! starvation.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use tokio::sync::{Mutex, oneshot};

/// Shared file operation lock manager stored in tool shared resources.
#[derive(Clone)]
pub struct FileOperationLockManager {
    inner: Arc<Mutex<LockInner>>,
}

struct LockInner {
    locked_files: HashSet<String>,
    exclusive_lock_active: bool,
    wait_queue: VecDeque<QueuedWaiter>,
}

enum QueuedWaiter {
    File {
        path: String,
        tx: oneshot::Sender<()>,
    },
    Exclusive {
        tx: oneshot::Sender<()>,
    },
}

impl FileOperationLockManager {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(LockInner {
                locked_files: HashSet::new(),
                exclusive_lock_active: false,
                wait_queue: VecDeque::new(),
            })),
        }
    }

    /// Acquire a per-path lock. Blocks if:
    /// - An exclusive lock is active, OR
    /// - The same path is already locked, OR
    /// - An exclusive waiter is ahead in the queue.
    ///
    /// Returns a guard that releases the lock on drop.
    pub async fn wait_for_lock(&self, path: &str) -> FileOperationLockGuard {
        let rx = {
            let mut inner = self.inner.lock().await;
            let needs_wait = inner.exclusive_lock_active
                || inner.locked_files.contains(path)
                || inner.has_exclusive_waiter_ahead();

            if needs_wait {
                let (tx, rx) = oneshot::channel();
                inner.wait_queue.push_back(QueuedWaiter::File {
                    path: path.to_string(),
                    tx,
                });
                Some(rx)
            } else {
                inner.locked_files.insert(path.to_string());
                None
            }
        };

        if let Some(rx) = rx {
            // Wait for our turn (ignore error — sender dropped means lock manager was dropped).
            let _ = rx.await;
        }

        FileOperationLockGuard {
            manager: self.clone(),
            kind: LockKind::File(path.to_string()),
        }
    }

    /// Acquire an exclusive lock. Blocks until all per-path locks are released
    /// and no other exclusive lock is active.
    ///
    /// Returns a guard that releases the lock on drop.
    pub async fn wait_for_exclusive_lock(&self) -> FileOperationLockGuard {
        let rx = {
            let mut inner = self.inner.lock().await;
            let needs_wait = inner.exclusive_lock_active || !inner.locked_files.is_empty();

            if needs_wait {
                let (tx, rx) = oneshot::channel();
                inner.wait_queue.push_back(QueuedWaiter::Exclusive { tx });
                Some(rx)
            } else {
                inner.exclusive_lock_active = true;
                None
            }
        };

        if let Some(rx) = rx {
            let _ = rx.await;
        }

        FileOperationLockGuard {
            manager: self.clone(),
            kind: LockKind::Exclusive,
        }
    }
}

impl Default for FileOperationLockManager {
    fn default() -> Self {
        Self::new()
    }
}

enum LockKind {
    File(String),
    Exclusive,
}

/// RAII guard that releases the lock when dropped.
pub struct FileOperationLockGuard {
    manager: FileOperationLockManager,
    kind: LockKind,
}

impl Drop for FileOperationLockGuard {
    fn drop(&mut self) {
        let manager = self.manager.clone();
        let kind = std::mem::replace(&mut self.kind, LockKind::Exclusive);
        // Use `spawn` to release asynchronously — `drop` can't be async.
        tokio::spawn(async move {
            let mut inner = manager.inner.lock().await;
            match kind {
                LockKind::File(path) => {
                    inner.locked_files.remove(&path);
                }
                LockKind::Exclusive => {
                    inner.exclusive_lock_active = false;
                }
            }
            inner.process_queue();
        });
    }
}

impl LockInner {
    fn has_exclusive_waiter_ahead(&self) -> bool {
        self.wait_queue
            .iter()
            .any(|w| matches!(w, QueuedWaiter::Exclusive { .. }))
    }

    /// Process the wait queue, granting locks to eligible waiters.
    ///
    /// If a waiter's receiver has been dropped (task cancelled), the send
    /// will fail. In that case, we undo the lock grant and continue to the
    /// next waiter. This prevents phantom locks from cancelled tool calls.
    fn process_queue(&mut self) {
        while let Some(front) = self.wait_queue.front() {
            match front {
                QueuedWaiter::Exclusive { .. } => {
                    if !self.locked_files.is_empty() || self.exclusive_lock_active {
                        break;
                    }
                    if let Some(QueuedWaiter::Exclusive { tx }) = self.wait_queue.pop_front() {
                        self.exclusive_lock_active = true;
                        if tx.send(()).is_err() {
                            // Receiver dropped (cancelled) — undo the grant.
                            self.exclusive_lock_active = false;
                            continue;
                        }
                    }
                    // Exclusive lock granted — stop processing.
                    break;
                }
                QueuedWaiter::File { path, .. } => {
                    if self.exclusive_lock_active || self.locked_files.contains(path) {
                        break;
                    }
                    let path = path.clone();
                    if let Some(QueuedWaiter::File { tx, .. }) = self.wait_queue.pop_front() {
                        self.locked_files.insert(path.clone());
                        if tx.send(()).is_err() {
                            // Receiver dropped (cancelled) — undo the grant.
                            self.locked_files.remove(&path);
                            continue;
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn serializes_same_path() {
        let mgr = FileOperationLockManager::new();
        let order = Arc::new(Mutex::new(Vec::new()));

        let guard1 = mgr.wait_for_lock("a.ts").await;
        let order2 = order.clone();
        let mgr2 = mgr.clone();
        let handle = tokio::spawn(async move {
            order2.lock().await.push("2-waiting");
            let _guard2 = mgr2.wait_for_lock("a.ts").await;
            order2.lock().await.push("2-acquired");
        });

        // Give spawned task time to queue.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        order.lock().await.push("1-releasing");
        drop(guard1);

        handle.await.unwrap();
        let log = order.lock().await;
        assert_eq!(log.as_slice(), &["2-waiting", "1-releasing", "2-acquired"]);
    }

    #[tokio::test]
    async fn allows_different_paths_concurrently() {
        let mgr = FileOperationLockManager::new();
        let _guard_a = mgr.wait_for_lock("a.ts").await;

        // Different path should acquire immediately.
        let mgr2 = mgr.clone();
        let handle = tokio::spawn(async move {
            let _guard_b = mgr2.wait_for_lock("b.ts").await;
            true
        });

        let result = tokio::time::timeout(std::time::Duration::from_millis(100), handle)
            .await
            .expect("should not timeout")
            .expect("should not panic");
        assert!(result);
    }

    #[tokio::test]
    async fn exclusive_blocks_file_locks() {
        let mgr = FileOperationLockManager::new();
        let exclusive = mgr.wait_for_exclusive_lock().await;
        let acquired = Arc::new(Mutex::new(false));

        let mgr2 = mgr.clone();
        let acquired2 = acquired.clone();
        let handle = tokio::spawn(async move {
            let _guard = mgr2.wait_for_lock("a.ts").await;
            *acquired2.lock().await = true;
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!*acquired.lock().await);

        drop(exclusive);
        handle.await.unwrap();
        assert!(*acquired.lock().await);
    }
}
