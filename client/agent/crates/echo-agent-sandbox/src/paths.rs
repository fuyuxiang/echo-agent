//! Filesystem path tables for sandbox profiles.
//!
//! Collects device files, temp directories, and essential writable paths.

use std::path::{Path, PathBuf};

// ── EchoAgent state directory ────────────────────────────────────────────────────

/// EchoAgent state directory — always writable (`$ECHO_AGENT_HOME` or `~/.echo-agent`).
pub(crate) fn echo_agent_home() -> PathBuf {
    echo_agent_config::echo_agent_home()
}

// ── Device files & directories ──────────────────────────────────────────────

/// Device files that need write access for normal tool operation.
///
/// Without write access to these, common programs (git, curl, ssh, compilers)
/// break because they can't open `/dev/null` as an output sink, allocate PTYs,
/// or seed RNGs.
///
/// These are individual files (use `allow_file`, not `allow_path`).
/// Directory nodes under `/dev` belong in [`DEVICE_DIRS`].
#[cfg(all(feature = "enforce", unix))]
pub(crate) const DEVICE_FILES: &[&str] = &[
    "/dev/null",    // output sink — used by virtually every CLI tool
    "/dev/zero",    // zero source — used by memory allocators
    "/dev/random",  // entropy — used by crypto/TLS
    "/dev/urandom", // entropy — used by crypto/TLS
    "/dev/tty",     // controlling terminal — used by git, ssh, gpg
    "/dev/ptmx",    // PTY allocation — used by terminal spawning
];

/// Device directories that need write access (use `allow_path`, not `allow_file`).
#[cfg(all(feature = "enforce", unix))]
pub(crate) const DEVICE_DIRS: &[&str] = &[
    "/dev/pts", // PTY slaves (Linux)
    "/dev/fd",  // fd table (symlink to /proc/self/fd on Linux; a directory)
];

// ── Temporary directories ───────────────────────────────────────────────────

/// Temporary directories that need write access.
///
/// On Linux, `/tmp` is the standard temp directory.
/// On macOS, programs use both `/tmp` (symlink to `/private/tmp`) and
/// `/private/var/folders/` (the real `TMPDIR` / `NSTemporaryDirectory()`).
/// git, compilers, and other tools write temp files to `$TMPDIR` which
/// resolves to `/private/var/folders/xx/.../T/` on macOS.
pub(crate) fn temp_writable_paths() -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::from("/tmp"), PathBuf::from("/var/tmp")];

    // macOS: /tmp → /private/tmp, but the real TMPDIR is under /private/var/folders.
    // Also include /private/tmp since Seatbelt may resolve the symlink.
    if cfg!(target_os = "macos") {
        for p in ["/private/tmp", "/private/var/tmp", "/private/var/folders"] {
            let pb = PathBuf::from(p);
            if pb.exists() && pb.is_dir() {
                paths.push(pb);
            }
        }
    }

    // Respect $TMPDIR if it points somewhere else (e.g. custom Linux setups).
    if let Ok(tmpdir) = std::env::var("TMPDIR") {
        let pb = PathBuf::from(&tmpdir);
        if pb.exists() && pb.is_dir() && !paths.contains(&pb) {
            paths.push(pb);
        }
    }

    paths
}

// ── Essential writable paths ────────────────────────────────────────────────

/// Writable directory paths for profiles that allow workspace writes (workspace, devbox, strict).
/// Device files are handled separately via `allow_file` in `to_capability_set_with_config`.
pub(crate) fn essential_writable_paths(workspace: &Path) -> Vec<PathBuf> {
    let mut paths = vec![workspace.to_path_buf(), echo_agent_home()];
    paths.extend(temp_writable_paths());
    paths
}

/// Writable directory paths for the read-only profile (minimal: just ~/.echo-agent + temp).
/// Device files are handled separately via `allow_file` in `to_capability_set_with_config`.
pub(crate) fn essential_writable_paths_minimal() -> Vec<PathBuf> {
    let mut paths = vec![echo_agent_home()];
    paths.extend(temp_writable_paths());
    paths
}
