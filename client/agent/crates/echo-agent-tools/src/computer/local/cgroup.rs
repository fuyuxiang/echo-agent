//! Cgroup v2 memory-high monitor for graceful OOM handling.
//!
//! Approach:
//!
//! 1. On startup we create a child cgroup under the current process's cgroup and
//!    configure:
//!    - `memory.high` = soft limit (the "desired" ceiling)
//!    - `memory.max`  = soft limit + headroom (hard OOM kill boundary)
//!
//! 2. Before each spawned command, we write the child PID into `cgroup.procs` to
//!    move it (and its entire process group) into the cgroup.
//!
//! 3. A background `MemoryHighMonitor` task watches `memory.events` via inotify.
//!    When the kernel increments the `high` counter (meaning a process touched
//!    `memory.high`), the monitor reads `memory.current` and — if RSS is still
//!    above 90 % of `memory.high` — sends a `MemoryHighEvent` through a
//!    `tokio::sync::watch` channel.
//!
//! 4. The terminal actor polls `monitor.try_recv()` on every tick.  When it
//!    receives an event it kills the offending process group with SIGKILL and
//!    reports exit-code **137** (128 + SIGKILL) with signal `"oom"`.
//!
//! The echo-agent-tools process itself is **never** inside this cgroup — only spawned
//! child commands are.  After the child exits the cgroup is empty until the next
//! command.
//!
//! ## Platform
//!
//! Everything compiles on all platforms, but the actual cgroup + inotify logic is
//! gated behind `#[cfg(target_os = "linux")]`.  On macOS / Windows the public
//! constructors return `None` / no-op stubs so callers don't need `#[cfg]`.

/// Exit code for processes killed due to memory pressure.
/// Matches the POSIX convention: 128 + signal-number (SIGKILL = 9).
pub const PROCESS_OOM_EXIT_CODE: i32 = 137;

// ============================================================================
// Public types (cross-platform)
// ============================================================================

/// Event emitted when `memory.high` is breached and RSS is still above
/// the 90 % buffer threshold.
#[derive(Debug, Clone)]
pub struct MemoryHighEvent {
    /// Current cgroup memory usage in bytes when the event fired.
    pub memory_current: u64,
    /// The configured `memory.high` threshold in bytes.
    pub memory_high_threshold: u64,
}

/// Configuration for cgroup memory limits.
#[derive(Debug, Clone)]
pub struct CgroupMemoryConfig {
    /// Soft memory limit (`memory.high`).  When a process inside the cgroup
    /// exceeds this, the monitor fires.
    pub memory_high_bytes: u64,
    /// Extra headroom above `memory.high` before the kernel hard-kills
    /// (`memory.max = memory_high_bytes + headroom_bytes`).
    /// A reasonable default is 256 MiB.
    pub headroom_bytes: u64,
}

impl CgroupMemoryConfig {
    /// memory.max = memory.high + headroom
    #[cfg(target_os = "linux")]
    fn memory_max(&self) -> u64 {
        self.memory_high_bytes.saturating_add(self.headroom_bytes)
    }
}

// ============================================================================
// Linux implementation
// ============================================================================

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::PathBuf;
    use tokio::io::Interest;
    use tokio::io::unix::AsyncFd;
    use tokio::sync::watch;

    // ── inotify FFI ──────────────────────────────────────────────────────

    unsafe fn inotify_init1(flags: libc::c_int) -> std::io::Result<i32> {
        #[allow(clippy::cast_possible_truncation)]
        let ret = unsafe { libc::syscall(libc::SYS_inotify_init1, flags) as libc::c_int };
        if ret < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(ret)
    }

    unsafe fn inotify_add_watch(
        fd: libc::c_int,
        pathname: *const libc::c_char,
        mask: u32,
    ) -> std::io::Result<i32> {
        #[allow(clippy::cast_possible_truncation)]
        let ret = unsafe {
            libc::syscall(libc::SYS_inotify_add_watch, fd, pathname, mask) as libc::c_int
        };
        if ret < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(ret)
    }

    const IN_MODIFY: u32 = 0x0000_0002;

    // ── Inotify wrapper ──────────────────────────────────────────────────

    struct Inotify {
        fd: AsyncFd<OwnedFd>,
    }

    impl Inotify {
        fn new() -> std::io::Result<Self> {
            let raw = unsafe { inotify_init1(libc::O_NONBLOCK | libc::O_CLOEXEC) }?;
            let owned = unsafe { OwnedFd::from_raw_fd(raw) };
            let fd = AsyncFd::with_interest(owned, Interest::READABLE)?;
            Ok(Inotify { fd })
        }

        fn add_watch(&self, path: &std::path::Path) -> std::io::Result<i32> {
            let mut bytes = path.as_os_str().as_bytes().to_vec();
            bytes.push(0); // NUL-terminate
            let wd = unsafe {
                inotify_add_watch(
                    self.fd.get_ref().as_raw_fd(),
                    bytes.as_ptr().cast(),
                    IN_MODIFY,
                )
            }?;
            Ok(wd)
        }

        async fn wait_and_drain(&self) -> std::io::Result<()> {
            let mut guard = self.fd.readable().await?;
            // Drain all pending inotify events
            let mut buf = [0u8; 4096];
            loop {
                let n = unsafe {
                    libc::read(
                        guard.get_inner().as_raw_fd(),
                        buf.as_mut_ptr().cast(),
                        buf.len(),
                    )
                };
                if n > 0 {
                    continue;
                }
                break;
            }
            guard.clear_ready();
            Ok(())
        }
    }

    // ── Cgroup helpers ───────────────────────────────────────────────────

    /// Read `/proc/self/cgroup` to find our own cgroup path (cgroup v2 unified).
    fn read_self_cgroup() -> std::io::Result<String> {
        let contents = std::fs::read_to_string("/proc/self/cgroup")?;
        // In cgroupv2 unified hierarchy, the line is "0::<path>"
        for line in contents.lines() {
            if let Some(rest) = line.strip_prefix("0::") {
                return Ok(rest.to_owned());
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Could not find cgroupv2 entry in /proc/self/cgroup",
        ))
    }

    /// Parse the `high <N>` counter from `memory.events` contents.
    fn parse_memory_events_high(contents: &str) -> Option<u64> {
        for line in contents.lines() {
            if let Some(value) = line.strip_prefix("high ") {
                return value.trim().parse::<u64>().ok();
            }
        }
        None
    }

    // ── CgroupHandle ─────────────────────────────────────────────────────

    /// Owns the lifecycle of a child cgroup directory.
    pub(crate) struct CgroupHandle {
        fs_path: PathBuf,
    }

    impl CgroupHandle {
        /// Create a child cgroup under the current process's cgroup and
        /// configure memory limits.
        pub(crate) async fn create(config: &CgroupMemoryConfig) -> std::io::Result<Self> {
            let self_cgroup = read_self_cgroup()?;
            let name = format!("echo-agent-tools-{}", uuid::Uuid::now_v7());
            let fs_path = PathBuf::from(format!("/sys/fs/cgroup{}/{}", self_cgroup, name));

            tokio::fs::create_dir_all(&fs_path).await?;

            // Enable memory + cpu controllers in the child cgroup's parent
            // (the parent's subtree_control must list the controllers).
            let parent = fs_path.parent().unwrap();
            let subtree_ctl = parent.join("cgroup.subtree_control");
            // Best-effort; may already be enabled or not permitted.
            let _ = tokio::fs::write(&subtree_ctl, "+memory +cpu").await;

            // Configure memory.high (soft limit)
            let memory_high_path = fs_path.join("memory.high");
            tokio::fs::write(&memory_high_path, config.memory_high_bytes.to_string()).await?;

            // Configure memory.max (hard limit = high + headroom)
            let memory_max_path = fs_path.join("memory.max");
            tokio::fs::write(&memory_max_path, config.memory_max().to_string()).await?;

            tracing::info!(
                cgroup = %fs_path.display(),
                memory_high = config.memory_high_bytes,
                memory_max = config.memory_max(),
                "Created cgroup with memory limits"
            );

            Ok(CgroupHandle { fs_path })
        }

        /// Move a process (by PID) into this cgroup.
        pub(crate) async fn add_process(&self, pid: u32) -> std::io::Result<()> {
            let procs_path = self.fs_path.join("cgroup.procs");
            tokio::fs::write(&procs_path, pid.to_string()).await
        }

        /// Read `memory.current` from this cgroup.
        #[allow(dead_code)]
        pub(crate) async fn memory_current(&self) -> std::io::Result<u64> {
            let s: String = tokio::fs::read_to_string(self.fs_path.join("memory.current")).await?;
            s.trim()
                .parse::<u64>()
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
        }

        /// Filesystem path to this cgroup.
        pub(crate) fn path(&self) -> &std::path::Path {
            &self.fs_path
        }
    }

    impl Drop for CgroupHandle {
        fn drop(&mut self) {
            let path = self.fs_path.clone();
            // Always use tokio::spawn: the cleanup future is Send and Drop
            // can fire after the LocalSet has shut down, making spawn_local
            // unsafe here.
            tokio::spawn(async move {
                let kill_path = path.join("cgroup.kill");
                let _ = tokio::fs::write(&kill_path, "1").await;
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                if let Err(e) = tokio::fs::remove_dir(&path).await {
                    tracing::debug!(
                        cgroup = %path.display(),
                        "Failed to remove cgroup dir on drop (may already be gone): {e}"
                    );
                }
            });
        }
    }

    // ── MemoryHighMonitor ────────────────────────────────────────────────

    /// Watches `memory.events` via inotify and signals when the `high`
    /// counter increments while RSS is still above 90% of the threshold.
    pub(crate) struct MemoryHighMonitor {
        rx: watch::Receiver<Option<MemoryHighEvent>>,
        /// Dropping this aborts the background task.
        _handle: tokio::task::JoinHandle<()>,
    }

    impl MemoryHighMonitor {
        /// Start monitoring the given cgroup for memory.high events.
        pub(crate) async fn start(
            cgroup_path: PathBuf,
            memory_high_threshold: u64,
            use_spawn_local: bool,
        ) -> std::io::Result<Self> {
            let (tx, rx) = watch::channel(None);

            let inotify = Inotify::new()?;
            let events_path = cgroup_path.join("memory.events");
            inotify.add_watch(&events_path)?;

            let monitor_fut = Self::monitor_loop(inotify, cgroup_path, memory_high_threshold, tx);
            let handle = if use_spawn_local {
                tokio::task::spawn_local(monitor_fut)
            } else {
                tokio::spawn(monitor_fut)
            };

            Ok(MemoryHighMonitor {
                rx,
                _handle: handle,
            })
        }

        /// Non-blocking check: returns `Some(event)` if memory.high was
        /// breached since the last call, `None` otherwise.
        pub(crate) fn try_recv(&mut self) -> Option<MemoryHighEvent> {
            if self.rx.has_changed().unwrap_or(false) {
                self.rx.borrow_and_update().clone()
            } else {
                None
            }
        }

        async fn monitor_loop(
            inotify: Inotify,
            cgroup_path: PathBuf,
            memory_high_threshold: u64,
            tx: watch::Sender<Option<MemoryHighEvent>>,
        ) {
            let events_path = cgroup_path.join("memory.events");
            let current_path = cgroup_path.join("memory.current");

            // Read baseline high counter
            let mut last_high_count = Self::read_high_counter(&events_path).await.unwrap_or(0);

            loop {
                // Block until inotify fires (memory.events was modified)
                if inotify.wait_and_drain().await.is_err() {
                    break;
                }

                // Read new high counter
                let current_high = Self::read_high_counter(&events_path).await.unwrap_or(0);
                if current_high <= last_high_count {
                    continue;
                }
                last_high_count = current_high;

                // Read current memory usage
                let memory_current = match tokio::fs::read_to_string(&current_path).await {
                    Ok(s) => {
                        let s: String = s;
                        s.trim().parse::<u64>().unwrap_or(0)
                    }
                    Err(_) => continue,
                };

                // Only fire if still above 90 % of threshold (avoids false
                // positives from transient spikes the kernel already handled).
                let buffer_threshold = memory_high_threshold.saturating_mul(9) / 10;
                if memory_current >= buffer_threshold {
                    let event = MemoryHighEvent {
                        memory_current,
                        memory_high_threshold,
                    };
                    if tx.send(Some(event)).is_err() {
                        break; // receiver dropped
                    }
                }
            }
        }

        async fn read_high_counter(events_path: &std::path::Path) -> Option<u64> {
            let contents = tokio::fs::read_to_string(events_path).await.ok()?;
            parse_memory_events_high(&contents)
        }
    }

    impl Drop for MemoryHighMonitor {
        fn drop(&mut self) {
            self._handle.abort();
        }
    }
}

// ============================================================================
// Cross-platform re-exports
// ============================================================================

/// Cgroup handle — owns the child cgroup's lifecycle.
///
/// On Linux, this creates a real cgroupv2 directory with memory limits.
/// On other platforms, this is a no-op.
pub struct CgroupGuard {
    #[cfg(target_os = "linux")]
    inner: Option<linux::CgroupHandle>,
}

impl CgroupGuard {
    /// Try to create a cgroup with the given memory config.
    /// Returns a guard that cleans up the cgroup on drop.
    ///
    /// On non-Linux platforms this always returns a no-op guard.
    /// On Linux, if cgroup creation fails (e.g., not running as root,
    /// cgroupv2 not available), it logs a warning and returns a no-op guard.
    pub async fn try_create(config: &CgroupMemoryConfig) -> Self {
        #[cfg(target_os = "linux")]
        {
            match linux::CgroupHandle::create(config).await {
                Ok(handle) => CgroupGuard {
                    inner: Some(handle),
                },
                Err(e) => {
                    tracing::warn!("Failed to create cgroup (falling back to no limits): {e}");
                    CgroupGuard { inner: None }
                }
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = config;
            CgroupGuard {}
        }
    }

    /// No-op guard with no backing cgroup.
    pub fn noop() -> Self {
        #[cfg(target_os = "linux")]
        {
            CgroupGuard { inner: None }
        }
        #[cfg(not(target_os = "linux"))]
        {
            CgroupGuard {}
        }
    }

    /// Move a process into this cgroup by PID.
    /// No-op if cgroup was not created.
    pub async fn add_process(&self, _pid: u32) -> std::io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            if let Some(ref handle) = self.inner {
                return handle.add_process(_pid).await;
            }
        }
        Ok(())
    }

    /// Returns the cgroup filesystem path, if available.
    #[allow(dead_code)]
    pub fn path(&self) -> Option<&std::path::Path> {
        #[cfg(target_os = "linux")]
        {
            if let Some(ref handle) = self.inner {
                return Some(handle.path());
            }
        }
        None
    }

    /// Returns true if this guard has a real cgroup backing it.
    pub fn is_active(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            self.inner.is_some()
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    }
}

/// Memory-high monitor — watches for memory pressure events.
///
/// On Linux, uses inotify on `memory.events`.
/// On other platforms, this is a no-op that never fires.
pub struct MemoryMonitor {
    #[cfg(target_os = "linux")]
    inner: Option<linux::MemoryHighMonitor>,
}

impl MemoryMonitor {
    /// Start monitoring the given cgroup guard for memory.high events.
    /// Returns a no-op monitor if the guard has no backing cgroup.
    pub async fn start(
        guard: &CgroupGuard,
        config: &CgroupMemoryConfig,
        use_spawn_local: bool,
    ) -> Self {
        #[cfg(target_os = "linux")]
        {
            if let Some(ref handle) = guard.inner {
                match linux::MemoryHighMonitor::start(
                    handle.path().to_path_buf(),
                    config.memory_high_bytes,
                    use_spawn_local,
                )
                .await
                {
                    Ok(monitor) => {
                        return MemoryMonitor {
                            inner: Some(monitor),
                        };
                    }
                    Err(e) => {
                        tracing::warn!("Failed to start memory monitor: {e}");
                    }
                }
            }
            MemoryMonitor { inner: None }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (guard, config, use_spawn_local);
            MemoryMonitor {}
        }
    }

    /// No-op monitor that never fires.
    pub fn noop() -> Self {
        #[cfg(target_os = "linux")]
        {
            MemoryMonitor { inner: None }
        }
        #[cfg(not(target_os = "linux"))]
        {
            MemoryMonitor {}
        }
    }

    /// Non-blocking poll: returns `Some(event)` if memory.high was breached
    /// since the last call, `None` otherwise.
    pub fn try_recv(&mut self) -> Option<MemoryHighEvent> {
        #[cfg(target_os = "linux")]
        {
            if let Some(ref mut monitor) = self.inner {
                return monitor.try_recv();
            }
        }
        None
    }
}
