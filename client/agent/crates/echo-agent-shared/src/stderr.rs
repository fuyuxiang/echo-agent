//! Serialized access to the TUI's stderr writer.

use parking_lot::{Mutex, MutexGuard};
use std::sync::OnceLock;

static STDERR_OUTPUT_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn stderr_output_lock() -> &'static Mutex<()> {
    STDERR_OUTPUT_LOCK.get_or_init(|| Mutex::new(()))
}

pub fn stderr_lock() -> MutexGuard<'static, ()> {
    stderr_output_lock().lock()
}

/// Execute `f` with exclusive access to the TUI's stderr writer.
///
/// When [`echo_agent_tty_utils::redirect_native_stderr`] has been called, this
/// writes to the dup'd fd that points at the real terminal (bypassing the
/// `/dev/null` redirect on fd 2). Otherwise falls back to normal stderr.
pub fn with_locked_stderr<T>(f: impl FnOnce(&mut std::fs::File) -> T) -> T {
    let _guard = stderr_lock();
    let mut file = echo_agent_tty_utils::dup_tui_stderr().unwrap_or_else(|_| {
        // Fallback: try_clone stderr to get an independently-owned
        // File. This path is hit if redirect_native_stderr was never
        // called or fd dup fails.
        let stderr = std::io::stderr();
        let stderr_file: std::fs::File;
        #[cfg(unix)]
        {
            use std::os::unix::io::{AsRawFd, FromRawFd};
            // SAFETY: stderr fd (2) is valid; from_raw_fd takes ownership
            // of the dup'd copy, not the original.
            let fd = unsafe { libc::dup(stderr.as_raw_fd()) };
            stderr_file = unsafe { std::fs::File::from_raw_fd(fd) };
        }
        #[cfg(not(unix))]
        {
            use std::os::windows::io::{AsRawHandle, FromRawHandle};
            // SAFETY: stderr handle is valid; DuplicateHandle gives us
            // an independent copy.
            let handle = stderr.as_raw_handle();
            let temp = unsafe { std::fs::File::from_raw_handle(handle) };
            stderr_file = temp.try_clone().expect("dup stderr fallback");
            std::mem::forget(temp);
        }
        stderr_file
    });
    f(&mut file)
}
