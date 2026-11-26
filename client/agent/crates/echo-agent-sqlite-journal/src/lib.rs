//! Filesystem-aware SQLite journal-mode selection.
//!
//! WAL keeps its wal-index in an mmap'd `-shm` file and relies on coherent
//! shared memory plus reliable POSIX locks — guarantees network filesystems
//! do not provide. When `$HOME` (and thus `~/.echo-agent`) is NFS-mounted on
//! several machines at once, a peer host truncating/rebuilding the `-shm`
//! during WAL recovery or close rips the backing out from under our mapping
//! and the next wal-index read dies with SIGBUS. On such mounts we use a
//! rollback journal instead (SQLite's documented "WAL does not work over a
//! network filesystem" limitation), and each host opens its own per-host DB
//! file (see [`JournalMode::effective_db_path`]) so no peer — including
//! pre-fix binaries that would flip a shared DB back to WAL — ever shares
//! the file.

use std::path::{Path, PathBuf};

/// Wait for peers' locks instead of failing instantly; matches what every
/// consumer historically set.
const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(5000);

/// One budget for riding out `SQLITE_BUSY`. Inner retries share the caller's
/// deadline, so two timers cannot stack into twice the advertised wait.
pub const BUSY_RETRY_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

/// Journal mode chosen for a SQLite database based on where it lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JournalMode {
    /// Write-ahead logging — the historical default, local filesystems only.
    Wal,
    /// Rollback journal truncated (not unlinked) at commit — safe on network
    /// filesystems, and cheaper there than DELETE mode: no per-commit
    /// create/unlink namespace round-trips and no NFS `.nfsXXXX`
    /// silly-rename litter.
    Truncate,
}

impl JournalMode {
    /// Pick the journal mode for a database at `db_path`.
    ///
    /// Classifies the parent directory (the DB file itself may not exist
    /// yet), so callers must create it first. `ECHO_AGENT_SQLITE_JOURNAL_MODE`
    /// (`wal`|`truncate`) overrides detection as a field kill-switch.
    pub fn for_db_path(db_path: &Path) -> Self {
        let env = std::env::var("ECHO_AGENT_SQLITE_JOURNAL_MODE").ok();
        match mode_from_env(env.as_deref()) {
            EnvOverride::Mode(mode) => {
                // Loud so field flips of the kill-switch are greppable in logs.
                tracing::info!(
                    db = %db_path.display(),
                    mode = mode.as_str(),
                    source = "env",
                    "sqlite journal mode forced by ECHO_AGENT_SQLITE_JOURNAL_MODE"
                );
                return mode;
            }
            EnvOverride::Invalid => {
                // A typo in the emergency kill-switch must be loud, not silently ignored.
                tracing::warn!(
                    value = env.as_deref().unwrap_or_default(),
                    "invalid ECHO_AGENT_SQLITE_JOURNAL_MODE (accepted: wal, truncate); using detection"
                );
            }
            EnvOverride::Unset => {}
        }
        let dir = match db_path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            // Bare filename (or root): classify the CWD it resolves against.
            _ => Path::new("."),
        };
        let mode = if is_network_fs(dir) {
            Self::Truncate
        } else {
            Self::Wal
        };
        tracing::debug!(
            db = %db_path.display(),
            mode = mode.as_str(),
            source = "statfs",
            "sqlite journal mode"
        );
        mode
    }

    /// The `PRAGMA journal_mode` value for this mode.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Wal => "WAL",
            Self::Truncate => "TRUNCATE",
        }
    }

    /// The path actually opened for `db_path` under this mode.
    ///
    /// `Wal` (local): unchanged. `Truncate` (network): a per-host sibling
    /// (`worktrees.db` → `worktrees.h-<host>.db`). Journal mode is a
    /// database-wide property, so a live pre-fix binary on a peer host (or
    /// this host) can flip a *shared* DB back to WAL at any time and our
    /// long-lived connections would silently adopt it, re-creating the
    /// mmap'd `-shm`. Old binaries never know the per-host name, so the
    /// no-WAL invariant — and the end of cross-host sharing, the root
    /// hazard — holds by construction. These DBs are all rebuildable
    /// indexes/caches, so each host starting fresh is acceptable.
    ///
    /// Idempotent (an already-suffixed path is returned unchanged) so
    /// callers may pre-resolve the path for sidecar file operations. Falls
    /// back to `db_path` unchanged (still TRUNCATE) if no hostname is
    /// available.
    pub fn effective_db_path(self, db_path: &Path) -> PathBuf {
        if self != Self::Truncate {
            return db_path.to_path_buf();
        }
        let Some(host) = host_discriminator() else {
            return db_path.to_path_buf();
        };
        let Some(name) = db_path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
        else {
            return db_path.to_path_buf();
        };
        let tag = format!(".h-{host}");
        // Idempotent: pre-resolved paths pass through unchanged.
        if name.ends_with(&tag) || name.contains(&format!("{tag}.")) {
            return db_path.to_path_buf();
        }
        // Insert before the final extension ("worktrees.db" -> "worktrees.h-x.db"),
        // else append; rsplit keeps dotfile names like ".hidden" on the append arm.
        let new_name = match name.rsplit_once('.') {
            Some((stem, ext)) if !stem.is_empty() => format!("{stem}{tag}.{ext}"),
            _ => format!("{name}{tag}"),
        };
        db_path.with_file_name(new_name)
    }

    /// Open (or create) a read-write connection with this journal mode
    /// applied, at [`Self::effective_db_path`] (per-host on network mounts).
    pub fn open(self, db_path: &Path) -> rusqlite::Result<rusqlite::Connection> {
        let conn = rusqlite::Connection::open(self.effective_db_path(db_path))?;
        self.apply_with_retry(&conn)?;
        Ok(conn)
    }

    /// Open a connection for read-only use, safely for this mode's
    /// filesystem, at [`Self::effective_db_path`] (per-host on network
    /// mounts — until a read-write open creates that file, this errors and
    /// callers fall back to their defaults). Errors if the database file
    /// does not exist (never creates it).
    ///
    /// `Wal` (local): a plain read-only open — the historical behavior.
    ///
    /// `Truncate` (network): reading a legacy WAL-stamped DB read-only would
    /// mmap its `-shm` (the SIGBUS), and the read-only escape hatch does not
    /// exist: EXCLUSIVE locking's heap wal-index takes an exclusive file lock,
    /// and POSIX forbids write-locking an O_RDONLY fd (`SQLITE_IOERR_LOCK`).
    /// So open read-write (without CREATE) and run the idempotent conversion
    /// (needed e.g. after remote sync drops in a WAL-stamped file). The fd
    /// stays writable (conversion and hot-journal rollback need it), but
    /// `query_only` is then set so SQL writes are rejected on this arm too —
    /// both arms honor the name.
    pub fn open_readonly(self, db_path: &Path) -> rusqlite::Result<rusqlite::Connection> {
        self.open_readonly_until(db_path, std::time::Instant::now() + BUSY_RETRY_BUDGET)
    }

    pub fn open_readonly_until(
        self,
        db_path: &Path,
        deadline: std::time::Instant,
    ) -> rusqlite::Result<rusqlite::Connection> {
        use rusqlite::OpenFlags;
        let flags = match self {
            Self::Wal => OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            // Read-write (no CREATE): the conversion needs a writable fd.
            Self::Truncate => OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        };
        let conn = rusqlite::Connection::open_with_flags(self.effective_db_path(db_path), flags)?;
        // Readers can hit SQLITE_BUSY too (peer recovery, a rollback writer's
        // exclusive window), and the conversion below requires a busy handler.
        conn.busy_timeout(BUSY_TIMEOUT)?;
        if let Self::Truncate = self {
            self.apply_with_retry_until(&conn, deadline)?;
            // Make the name true: reject SQL writes (statement-level) while
            // the fd stays writable for lock/rollback purposes.
            conn.pragma_update(None, "query_only", true)?;
        }
        Ok(conn)
    }

    /// Apply this journal mode to a freshly opened read-write connection.
    ///
    /// Busy semantics (single source of truth): converting a database between
    /// WAL and rollback journaling takes a brief exclusive lock whose
    /// acquisition only PARTIALLY honors the busy handler — some lock paths
    /// wait out `busy_timeout`, others (e.g. a peer connection holding a WAL
    /// read-mark) fail fast with `SQLITE_BUSY`. Callers must set
    /// `busy_timeout` first, or use [`Self::apply_with_retry`] (as
    /// [`Self::open`] does), which also rides out the fail-fast paths.
    pub fn apply(self, conn: &rusqlite::Connection) -> rusqlite::Result<()> {
        match self {
            Self::Wal => conn.pragma_update(None, "journal_mode", "WAL"),
            Self::Truncate => {
                // EXCLUSIVE locking keeps the wal-index in heap memory, so
                // converting an already-WAL-stamped DB never mmaps a `-shm`
                // that a peer NFS client may be rebuilding (SIGBUS).
                conn.pragma_update(None, "locking_mode", "EXCLUSIVE")?;
                conn.pragma_update(None, "journal_mode", "TRUNCATE")?;
                // Legal only after leaving WAL; the exclusive lock is released
                // by the caller's next database access (e.g. schema init).
                conn.pragma_update(None, "locking_mode", "NORMAL")
            }
        }
    }

    /// [`Self::apply`] with a busy retry bounded by [`BUSY_RETRY_BUDGET`] and
    /// a 1s per-attempt lock wait. Leaves `busy_timeout` at the 5s default.
    pub fn apply_with_retry(self, conn: &rusqlite::Connection) -> rusqlite::Result<()> {
        self.apply_with_retry_until(conn, std::time::Instant::now() + BUSY_RETRY_BUDGET)
    }

    pub fn apply_with_retry_until(
        self,
        conn: &rusqlite::Connection,
        deadline: std::time::Instant,
    ) -> rusqlite::Result<()> {
        let result = self.apply_with_retry_inner(conn, deadline);
        let restore = conn.busy_timeout(BUSY_TIMEOUT);
        if result.is_err() { result } else { restore }
    }

    fn apply_with_retry_inner(
        self,
        conn: &rusqlite::Connection,
        deadline: std::time::Instant,
    ) -> rusqlite::Result<()> {
        use rusqlite::ErrorCode;
        use std::time::{Duration, Instant};
        const RETRY_PAUSE: Duration = Duration::from_millis(20);
        let start = Instant::now();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            conn.busy_timeout(remaining.clamp(RETRY_PAUSE, Duration::from_millis(1000)))?;
            match self.apply(conn) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    let is_busy = matches!(
                        &e,
                        rusqlite::Error::SqliteFailure(f, _)
                            if matches!(f.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
                    );
                    if !is_busy || Instant::now() + RETRY_PAUSE >= deadline {
                        let elapsed_ms = start.elapsed().as_millis();
                        let message = format!(
                            "failed to set journal mode {} after {elapsed_ms}ms: {e}",
                            self.as_str()
                        );
                        return Err(match e {
                            rusqlite::Error::SqliteFailure(f, _) => {
                                rusqlite::Error::SqliteFailure(f, Some(message))
                            }
                            other => other,
                        });
                    }
                    std::thread::sleep(RETRY_PAUSE);
                }
            }
        }
    }
}

/// Parse result of the `ECHO_AGENT_SQLITE_JOURNAL_MODE` kill-switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EnvOverride {
    Unset,
    /// Set to something unrecognized — observable (warned) but non-fatal.
    Invalid,
    Mode(JournalMode),
}

/// `ECHO_AGENT_SQLITE_JOURNAL_MODE` override parsing (pure for testability).
fn mode_from_env(value: Option<&str>) -> EnvOverride {
    // Set-but-empty counts as unset: a deliberate blank, not a typo.
    match value {
        None | Some("") => EnvOverride::Unset,
        Some(v) if v.eq_ignore_ascii_case("wal") => EnvOverride::Mode(JournalMode::Wal),
        Some(v) if v.eq_ignore_ascii_case("truncate") => EnvOverride::Mode(JournalMode::Truncate),
        Some(_) => EnvOverride::Invalid,
    }
}

/// Short per-host discriminator for per-host DB filenames (lowercased
/// alphanumeric hostname, other bytes mapped to `-`, capped at 24 chars).
/// `None` when no hostname is available. Sanitization collisions across
/// hosts only degrade to plain shared-TRUNCATE behavior, never to WAL.
fn host_discriminator() -> Option<String> {
    let raw = hostname_raw()?;
    let mut s: String = raw
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    s.truncate(24);
    let s = s.trim_matches('-');
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

#[cfg(unix)]
fn hostname_raw() -> Option<String> {
    let mut buf = [0u8; 256];
    // SAFETY: buf is a valid out-buffer of the given length; gethostname
    // NUL-terminates on success (position() guards the not-terminated case).
    if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } != 0 {
        return None;
    }
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8(buf[..len].to_vec()).ok()
}

#[cfg(windows)]
fn hostname_raw() -> Option<String> {
    std::env::var("COMPUTERNAME").ok()
}

#[cfg(not(any(unix, windows)))]
fn hostname_raw() -> Option<String> {
    None
}

/// Best-effort: whether `path` lives on a network/remote filesystem.
///
/// Any detection failure returns `false` (treat as local) so unclassifiable
/// filesystems keep the historical WAL behavior.
pub fn is_network_fs(path: &Path) -> bool {
    imp::is_network_fs(path)
}

/// Classify a Linux `statfs(2)` `f_type` as a network/remote filesystem.
///
/// Magic values from `include/uapi/linux/magic.h` (Lustre's from its module
/// sources). Only the low 32 bits are compared: `f_type` is a signed word
/// whose width varies by architecture, so 32-bit kernels sign-extend magics
/// with the high bit set (e.g. CIFS 0xFF534D42).
#[cfg(any(target_os = "linux", test))]
fn is_network_fs_magic(f_type: u64) -> bool {
    const NFS_SUPER_MAGIC: u64 = 0x6969;
    const SMB_SUPER_MAGIC: u64 = 0x517B;
    const SMB2_SUPER_MAGIC: u64 = 0xFE53_4D42;
    const CIFS_SUPER_MAGIC: u64 = 0xFF53_4D42;
    const V9FS_MAGIC: u64 = 0x0102_1997;
    const CODA_SUPER_MAGIC: u64 = 0x7375_7245;
    const AFS_SUPER_MAGIC: u64 = 0x5346_414F;
    const AFS_FS_MAGIC: u64 = 0x6B41_4653; // in-kernel kAFS client
    const CEPH_SUPER_MAGIC: u64 = 0x00C3_6400;
    const LUSTRE_SUPER_MAGIC: u64 = 0x0BD0_0BD0;
    const GFS2_MAGIC: u64 = 0x0116_1970;
    const GPFS_SUPER_MAGIC: u64 = 0x4750_4653; // "GPFS" (Spectrum Scale)
    const OCFS2_SUPER_MAGIC: u64 = 0x7461_636F;
    // WekaFS: parallel filesystem sometimes used for network-mounted home
    // directories. Its magic is not in linux/magic.h — the value is confirmed
    // empirically from `statfs` on wekafs mounts (`findmnt` reports
    // FSTYPE=wekafs; coreutils `stat -f` still prints it as UNKNOWN). Like NFS
    // it offers no coherent cross-host shared memory, so WAL's mmap'd `-shm`
    // SIGBUSes when a peer host rebuilds it.
    const WEKAFS_SUPER_MAGIC: u64 = 0x1803_1977;
    // FUSE is deliberately treated as network: sshfs/s3fs/gluster and other
    // FUSE-backed homes cannot guarantee coherent mmap across writers, and
    // rollback journaling costs little there.
    const FUSE_SUPER_MAGIC: u64 = 0x6573_5546;

    matches!(
        f_type & 0xFFFF_FFFF,
        NFS_SUPER_MAGIC
            | SMB_SUPER_MAGIC
            | SMB2_SUPER_MAGIC
            | CIFS_SUPER_MAGIC
            | V9FS_MAGIC
            | CODA_SUPER_MAGIC
            | AFS_SUPER_MAGIC
            | AFS_FS_MAGIC
            | CEPH_SUPER_MAGIC
            | LUSTRE_SUPER_MAGIC
            | GFS2_MAGIC
            | GPFS_SUPER_MAGIC
            | OCFS2_SUPER_MAGIC
            | WEKAFS_SUPER_MAGIC
            | FUSE_SUPER_MAGIC
    )
}

/// Mirrors macOS `libc::MNT_LOCAL` so the pure classifier is testable on
/// non-mac hosts; pinned to libc by a macOS-only test.
#[cfg(any(target_os = "macos", test))]
const MNT_LOCAL: u32 = 0x0000_1000;

/// Classify a macOS `statfs(2)` result as a network/remote filesystem.
///
/// Absence of `MNT_LOCAL` in `f_flags` is the authoritative remote signal
/// (covers unknown/future remote fs types). The `f_fstypename` allowlist is
/// kept as a conservative extra trigger for remote-backed mounts that still
/// set MNT_LOCAL (e.g. FUSE bridges).
#[cfg(any(target_os = "macos", test))]
fn is_network_fs_mac(f_flags: u32, fstype: &str) -> bool {
    (f_flags & MNT_LOCAL) == 0 || is_network_fs_name(fstype)
}

/// Classify a macOS `statfs(2)` `f_fstypename` as a network/remote filesystem.
#[cfg(any(target_os = "macos", test))]
fn is_network_fs_name(fstype: &str) -> bool {
    // macfuse/osxfuse mirror Linux's FUSE-is-network stance (sshfs etc.);
    // fuse-t needs no entry — its mounts already surface as "nfs".
    fstype.eq_ignore_ascii_case("nfs")
        || fstype.eq_ignore_ascii_case("smbfs")
        || fstype.eq_ignore_ascii_case("cifs")
        || fstype.eq_ignore_ascii_case("afpfs")
        || fstype.eq_ignore_ascii_case("webdav")
        || fstype.eq_ignore_ascii_case("macfuse")
        || fstype.eq_ignore_ascii_case("osxfuse")
}

#[cfg(target_os = "linux")]
mod imp {
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    pub(crate) fn is_network_fs(path: &Path) -> bool {
        let Ok(cpath) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: statfs is zero-initializable POD; cpath is NUL-terminated
        // and st is a valid out-pointer for the duration of the call.
        let mut st: libc::statfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statfs(cpath.as_ptr(), &mut st) } != 0 {
            return false;
        }
        // Cast through u64: f_type is i64 on 64-bit targets, i32 on some
        // 32-bit ones; the classifier masks to the meaningful low 32 bits.
        super::is_network_fs_magic(st.f_type as u64)
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    pub(crate) fn is_network_fs(path: &Path) -> bool {
        let Ok(cpath) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: statfs is zero-initializable POD; cpath is NUL-terminated
        // and st is a valid out-pointer for the duration of the call.
        let mut st: libc::statfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statfs(cpath.as_ptr(), &mut st) } != 0 {
            return false;
        }
        // Stack-copy the fixed array to u8 (c_char is i8 here), no heap;
        // take up to the first NUL.
        let bytes = st.f_fstypename.map(|c| c as u8);
        let len = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        let name = std::str::from_utf8(&bytes[..len]).unwrap_or("");
        super::is_network_fs_mac(st.f_flags, name)
    }
}

/// Classify a Windows path string as UNC (network) — `\\server\share` or
/// `\\?\UNC\server\share`; the `\\.\` and `\\?\C:\` device/verbatim-local
/// forms are not network. Pure for testability; mapped drives are caught by
/// the `GetDriveTypeW` probe instead.
#[cfg(any(windows, test))]
fn is_windows_unc(path: &str) -> bool {
    let Some(rest) = path.strip_prefix(r"\\") else {
        return false;
    };
    if let Some(verbatim) = rest.strip_prefix(r"?\") {
        return verbatim
            .get(..4)
            .is_some_and(|p| p.eq_ignore_ascii_case(r"UNC\"));
    }
    !rest.starts_with(r".\")
}

#[cfg(windows)]
mod imp {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetVolumePathNameW};
    use windows_sys::Win32::System::WindowsProgramming::DRIVE_REMOTE;

    pub(crate) fn is_network_fs(path: &Path) -> bool {
        if super::is_windows_unc(&path.to_string_lossy()) {
            return true;
        }
        // Mapped drives (e.g. Z: on SMB): resolve the volume root and ask
        // for its drive type; any failure → local (historical behavior).
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut root = [0u16; 261];
        // SAFETY: wide is NUL-terminated; root is a valid out-buffer whose
        // length is passed in characters, as the API requires.
        unsafe {
            if GetVolumePathNameW(wide.as_ptr(), root.as_mut_ptr(), root.len() as u32) == 0 {
                return false;
            }
            GetDriveTypeW(root.as_ptr()) == DRIVE_REMOTE
        }
    }
}

// Anything else: no cheap, reliable remote-FS probe and no field reports of
// the WAL-over-network crash there — keep the WAL default.
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod imp {
    use std::path::Path;

    pub(crate) fn is_network_fs(_path: &Path) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn journal_mode(conn: &rusqlite::Connection) -> String {
        conn.query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn network_magics_classify_as_network() {
        for magic in [
            0x6969u64,   // NFS
            0x517B,      // SMB
            0xFE53_4D42, // SMB2
            0xFF53_4D42, // CIFS
            0x0102_1997, // 9p
            0x7375_7245, // CODA
            0x5346_414F, // AFS
            0x6B41_4653, // kAFS
            0x00C3_6400, // CEPH
            0x0BD0_0BD0, // Lustre
            0x0116_1970, // GFS2
            0x4750_4653, // GPFS
            0x7461_636F, // OCFS2
            0x1803_1977, // wekafs
            0x6573_5546, // FUSE
        ] {
            assert!(is_network_fs_magic(magic), "magic {magic:#x}");
        }
    }

    #[test]
    fn local_magics_classify_as_local() {
        for magic in [
            0xEF53u64,   // ext2/3/4
            0x0102_1994, // tmpfs
            0x9123_683E, // btrfs
            0x5846_5342, // XFS
            0x794C_7630, // overlayfs
            0x2FC1_2FC1, // zfs
            0x0,
        ] {
            assert!(!is_network_fs_magic(magic), "magic {magic:#x}");
        }
    }

    #[test]
    fn sign_extended_magic_still_matches() {
        // A 32-bit kernel reports CIFS's 0xFF534D42 as a negative f_type.
        assert!(is_network_fs_magic(0xFFFF_FFFF_FF53_4D42));
    }

    #[test]
    fn fstypenames_classify() {
        for name in [
            "nfs", "smbfs", "cifs", "afpfs", "webdav", "NFS", "macfuse", "osxfuse",
        ] {
            assert!(is_network_fs_name(name), "{name}");
        }
        for name in ["apfs", "hfs", "tmpfs", "devfs", ""] {
            assert!(!is_network_fs_name(name), "{name}");
        }
    }

    #[test]
    fn apply_with_retry_rides_out_transient_exclusive_lock() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("retry.db");

        // EXCLUSIVE locking holds the lock until the connection closes.
        let holder = rusqlite::Connection::open(&path).unwrap();
        holder
            .pragma_update(None, "locking_mode", "EXCLUSIVE")
            .unwrap();
        holder
            .execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES (1);")
            .unwrap();

        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            drop(holder);
        });

        let conn = JournalMode::Wal
            .open(&path)
            .expect("open must ride out a transiently held exclusive lock");
        assert_eq!(journal_mode(&conn), "wal");
        release.join().unwrap();
    }

    #[test]
    fn env_override_parses() {
        use EnvOverride::{Invalid, Mode, Unset};
        assert_eq!(mode_from_env(Some("wal")), Mode(JournalMode::Wal));
        assert_eq!(mode_from_env(Some("WAL")), Mode(JournalMode::Wal));
        assert_eq!(mode_from_env(Some("truncate")), Mode(JournalMode::Truncate));
        assert_eq!(mode_from_env(Some("TRUNCATE")), Mode(JournalMode::Truncate));
        // Typos are Invalid (warned), not silently treated as unset.
        assert_eq!(mode_from_env(Some("delete")), Invalid);
        assert_eq!(mode_from_env(Some("wall")), Invalid);
        assert_eq!(mode_from_env(Some("")), Unset);
        assert_eq!(mode_from_env(None), Unset);
    }

    #[test]
    fn mac_classifier_uses_mnt_local_and_name_override() {
        // Unknown remote type without MNT_LOCAL → network.
        assert!(is_network_fs_mac(0, "somefutfs"));
        assert!(is_network_fs_mac(0, "macfuse_sshfs"));
        // Plain local APFS → local.
        assert!(!is_network_fs_mac(MNT_LOCAL, "apfs"));
        // Allowlisted name wins even when the mount claims MNT_LOCAL.
        assert!(is_network_fs_mac(MNT_LOCAL, "smbfs"));
        assert!(is_network_fs_mac(MNT_LOCAL, "macfuse"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mnt_local_matches_libc() {
        // libc::MNT_LOCAL is c_int; ours is u32 to match statfs.f_flags.
        assert_eq!(u64::from(MNT_LOCAL), libc::MNT_LOCAL as u64);
    }

    #[test]
    fn windows_unc_classifies() {
        assert!(is_windows_unc(r"\\server\share\echoagent"));
        assert!(is_windows_unc(r"\\?\UNC\server\share\echoagent"));
        assert!(is_windows_unc(r"\\?\unc\server\share"));
        assert!(!is_windows_unc(r"\\?\C:\Users\x"));
        assert!(!is_windows_unc(r"\\.\pipe\echoagent"));
        assert!(!is_windows_unc(r"C:\Users\x"));
        assert!(!is_windows_unc("/home/x"));
    }

    #[test]
    fn local_and_error_paths_are_not_network() {
        let tmp = TempDir::new().unwrap();
        // CI runners use plain local filesystems (ext4/tmpfs/APFS).
        assert!(!is_network_fs(tmp.path()));
        assert!(!is_network_fs(&tmp.path().join("does-not-exist")));
    }

    #[test]
    fn for_db_path_defaults_to_wal_on_local_fs() {
        // Ambient override would invalidate the assertion; skip if set.
        if std::env::var("ECHO_AGENT_SQLITE_JOURNAL_MODE").is_ok() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        let mode = JournalMode::for_db_path(&tmp.path().join("x.sqlite"));
        assert_eq!(mode, JournalMode::Wal);
    }

    #[test]
    fn effective_db_path_is_per_host_only_in_truncate_mode() {
        let p = Path::new("/tmp/dir/worktrees.db");
        // Local mode: untouched.
        assert_eq!(JournalMode::Wal.effective_db_path(p), p);

        // Network mode: deterministic per-host sibling in the same dir.
        let a = JournalMode::Truncate.effective_db_path(p);
        let b = JournalMode::Truncate.effective_db_path(p);
        assert_eq!(a, b);
        assert_ne!(a, p, "CI hosts always have a hostname");
        assert_eq!(a.parent(), p.parent());
        let name = a.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("worktrees.h-"), "{name}");
        assert!(name.ends_with(".db"), "{name}");

        // Idempotent: pre-resolved paths (for sidecar file ops) don't re-suffix.
        assert_eq!(JournalMode::Truncate.effective_db_path(&a), a);

        // Extension-less names get the suffix appended.
        let bare = JournalMode::Truncate.effective_db_path(Path::new("/tmp/dir/state"));
        let bare_name = bare.file_name().unwrap().to_string_lossy().into_owned();
        assert!(bare_name.starts_with("state.h-"), "{bare_name}");
    }

    #[test]
    fn network_mode_survives_legacy_wal_flip_back() {
        // Journal mode is database-wide: a live old binary can flip a SHARED
        // DB back to WAL underneath us. The per-host file makes that
        // impossible by construction — old binaries never open it.
        let tmp = TempDir::new().unwrap();
        let legacy = tmp.path().join("db.sqlite");

        let a = JournalMode::Truncate.open(&legacy).unwrap();
        a.execute_batch("CREATE TABLE t (v TEXT); INSERT INTO t VALUES ('new');")
            .unwrap();
        assert_eq!(journal_mode(&a), "truncate");

        // Simulated old binary: opens the legacy path, stamps WAL, writes.
        let b = rusqlite::Connection::open(&legacy).unwrap();
        b.pragma_update(None, "journal_mode", "WAL").unwrap();
        b.execute_batch("CREATE TABLE t (v TEXT); INSERT INTO t VALUES ('old');")
            .unwrap();

        // A is unaffected: still truncate, reads and writes fine, and its
        // own file never grows WAL sidecars.
        assert_eq!(journal_mode(&a), "truncate");
        a.execute("INSERT INTO t VALUES ('more')", []).unwrap();
        let n: i64 = a
            .query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2);

        let eff = JournalMode::Truncate.effective_db_path(&legacy);
        assert_ne!(eff, legacy);
        let eff_base = eff.display().to_string();
        assert!(!std::fs::exists(format!("{eff_base}-wal")).unwrap());
        assert!(!std::fs::exists(format!("{eff_base}-shm")).unwrap());
        // The legacy file really is in WAL with live sidecars — the two
        // connections diverged onto different files.
        let legacy_base = legacy.display().to_string();
        assert!(std::fs::exists(format!("{legacy_base}-wal")).unwrap());
        drop(b);
    }

    #[test]
    fn apply_sets_requested_mode() {
        let tmp = TempDir::new().unwrap();

        let wal = rusqlite::Connection::open(tmp.path().join("wal.sqlite")).unwrap();
        JournalMode::Wal.apply(&wal).unwrap();
        assert_eq!(journal_mode(&wal), "wal");

        let trunc = rusqlite::Connection::open(tmp.path().join("trunc.sqlite")).unwrap();
        JournalMode::Truncate.apply(&trunc).unwrap();
        assert_eq!(journal_mode(&trunc), "truncate");
    }

    #[test]
    fn wal_stamped_db_converts_to_truncate() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("db.sqlite");

        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            JournalMode::Wal.apply(&conn).unwrap();
            conn.execute_batch("CREATE TABLE t (v TEXT); INSERT INTO t VALUES ('keep');")
                .unwrap();
        }

        let conn = rusqlite::Connection::open(&path).unwrap();
        JournalMode::Truncate.apply(&conn).unwrap();
        assert_eq!(journal_mode(&conn), "truncate");
        let v: String = conn.query_row("SELECT v FROM t", [], |r| r.get(0)).unwrap();
        assert_eq!(v, "keep");
        // Exercise the locking-mode downgrade: the next write transaction
        // must acquire and release locks normally.
        conn.execute("INSERT INTO t VALUES ('more')", []).unwrap();
        drop(conn);

        let base = path.display().to_string();
        assert!(!std::fs::exists(format!("{base}-wal")).unwrap());
        assert!(!std::fs::exists(format!("{base}-shm")).unwrap());
    }

    #[test]
    fn open_applies_mode() {
        let tmp = TempDir::new().unwrap();

        let wal = JournalMode::Wal
            .open(&tmp.path().join("wal.sqlite"))
            .unwrap();
        assert_eq!(journal_mode(&wal), "wal");

        let trunc = JournalMode::Truncate
            .open(&tmp.path().join("trunc.sqlite"))
            .unwrap();
        assert_eq!(journal_mode(&trunc), "truncate");
    }

    #[test]
    fn open_readonly_missing_db_errors_and_creates_nothing() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("missing.sqlite");
        assert!(JournalMode::Wal.open_readonly(&path).is_err());
        assert!(JournalMode::Truncate.open_readonly(&path).is_err());
        // The Truncate arm opens read-write but must never create a file —
        // neither the given path nor its per-host sibling.
        assert!(!std::fs::exists(&path).unwrap());
        assert!(!std::fs::exists(JournalMode::Truncate.effective_db_path(&path)).unwrap());
    }

    #[test]
    fn open_readonly_wal_rejects_writes() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("db.sqlite");
        {
            let conn = JournalMode::Wal.open(&path).unwrap();
            conn.execute_batch("CREATE TABLE t (v TEXT); INSERT INTO t VALUES ('ro');")
                .unwrap();
        }

        let conn = JournalMode::Wal.open_readonly(&path).unwrap();
        let v: String = conn.query_row("SELECT v FROM t", [], |r| r.get(0)).unwrap();
        assert_eq!(v, "ro");
        assert!(conn.execute("INSERT INTO t VALUES ('nope')", []).is_err());
    }

    #[test]
    fn open_readonly_truncate_rejects_writes() {
        // The network arm's fd is read-write (conversion needs it), but
        // query_only must make the connection honor the name.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("db.sqlite");
        {
            let conn = JournalMode::Truncate.open(&path).unwrap();
            conn.execute_batch("CREATE TABLE t (v TEXT); INSERT INTO t VALUES ('ro');")
                .unwrap();
        }

        let conn = JournalMode::Truncate.open_readonly(&path).unwrap();
        assert_eq!(journal_mode(&conn), "truncate");
        let v: String = conn.query_row("SELECT v FROM t", [], |r| r.get(0)).unwrap();
        assert_eq!(v, "ro");
        assert!(conn.execute("INSERT INTO t VALUES ('nope')", []).is_err());
    }

    #[test]
    fn open_readonly_truncate_never_opens_legacy_file() {
        // A legacy (possibly WAL-poisoned) file at the shared path must not
        // be touched: network-mode opens resolve to the per-host sibling,
        // which doesn't exist yet → error → callers use their defaults.
        let tmp = TempDir::new().unwrap();
        let legacy = tmp.path().join("db.sqlite");
        {
            let conn = rusqlite::Connection::open(&legacy).unwrap();
            JournalMode::Wal.apply(&conn).unwrap();
            conn.execute_batch("CREATE TABLE t (v TEXT); INSERT INTO t VALUES ('legacy');")
                .unwrap();
        }

        assert!(JournalMode::Truncate.open_readonly(&legacy).is_err());
        // The legacy file stays WAL-stamped and unconverted.
        let conn = rusqlite::Connection::open(&legacy).unwrap();
        assert_eq!(journal_mode(&conn), "wal");
    }
}
