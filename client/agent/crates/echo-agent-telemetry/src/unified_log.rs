//! Centralized unified log for cross-component session observability.
//!
//! Shell writes directly via [`emit()`]. Pager and desktop forward entries
//! over ACP (`echo.agent/log` notifications); shell receives them in
//! [`ingest_client_entries()`] and writes on their behalf.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, Write};
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use echo_agent_config::echo_agent_home;

/// Binary version stamped into every log entry. Set once at startup via
/// [`set_version()`]; entries emitted before that get `None`.
static VERSION: OnceLock<String> = OnceLock::new();

/// Register the binary version (e.g. shell's `CARGO_PKG_VERSION`).
/// Call once at startup; subsequent calls are no-ops.
pub fn set_version(ver: &str) {
    let _ = VERSION.set(ver.to_owned());
}

pub const LOG_DIR: &str = "logs";
const LOG_FILE: &str = "unified.jsonl";
pub const MAX_SIZE: u64 = 5 * 1024 * 1024; // 5 MB

/// ACP method name for unified log notifications.
pub const LOG_METHOD: &str = "echo.agent/log";

// ---------------------------------------------------------------------------
// Log entry types
// ---------------------------------------------------------------------------

/// Log level for a unified log entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display, Serialize, Deserialize)]
#[strum(serialize_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
}

/// Component that produced a log entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display, Serialize, Deserialize)]
pub enum LogSource {
    #[strum(serialize = "shell")]
    #[serde(rename = "shell")]
    Shell,
    #[strum(serialize = "echo-agent-pager")]
    #[serde(rename = "echo-agent-pager")]
    EchoAgentPager,
    #[strum(serialize = "echo-agent-desktop")]
    #[serde(rename = "echo-agent-desktop")]
    EchoAgentDesktop,
}

/// A single unified log entry, written as one JSONL line.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    /// RFC 3339 timestamp (millisecond precision, UTC).
    pub ts: String,
    /// Component that produced the entry.
    pub src: LogSource,
    /// OS process id of the producer. Critical for cross-process trace
    /// reconstruction because shell/pager/desktop all append to the same
    /// `unified.jsonl`, so multiple shell processes' lines interleave
    /// indistinguishably without it.
    ///
    /// `Option<u32>` is for wire compatibility only -- shell, pager, and
    /// desktop all stamp `Some(std::process::id())` at emit time. A
    /// `None` here means the entry came from an older client/server that
    /// predates this field; current code never emits one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// Binary version (e.g. `"0.1.211"`). Stamped by [`set_version()`]
    /// at startup so stale zombie processes are identifiable in logs.
    /// `None` for entries from older binaries that predate this field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ver: Option<String>,
    /// Log level.
    pub lvl: LogLevel,
    /// Session ID, if one exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sid: Option<String>,
    /// Human-readable message.
    pub msg: String,
    /// Structured context fields.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ctx: Option<serde_json::Value>,
}

/// Wire format for the `echo.agent/log` ACP notification params.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogNotificationParams {
    /// Source component identifier.
    pub src: LogSource,
    pub entries: Vec<ClientLogEntry>,
}

/// Entry as sent by a client (no `src` field -- shell stamps it).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientLogEntry {
    pub ts: String,
    /// Client process id. Stamped by the client when the entry is
    /// created; preserved through ACP forwarding so the on-disk log
    /// reflects the originating process.
    ///
    /// Optional only for wire compatibility with clients that predate
    /// this field; in-tree clients always populate it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// Binary version. Optional for wire compatibility with older clients.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ver: Option<String>,
    pub lvl: LogLevel,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sid: Option<String>,
    pub msg: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ctx: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Writer
// ---------------------------------------------------------------------------

/// How often a writer re-checks that its handle still refers to the file at
/// `path`, and that the file is still under [`MAX_SIZE`].
///
/// Time-based rather than byte-based so a low-volume process detects a stale
/// handle just as fast as a chatty one — a process logging one line a minute
/// is precisely the one that would otherwise write into an unlinked inode for
/// hours without noticing.
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(2);

struct LogWriter {
    file: File,
    path: PathBuf,
    /// Identity of the inode this handle refers to, re-checked against the
    /// path on the maintenance cadence. `None` on platforms with no cheap
    /// stable file id, where only disappearance is detectable.
    identity: Option<FileIdentity>,
    last_maintenance: Instant,
    /// Set when `path` stopped resolving to our inode **and** reopening it
    /// failed. Writes are dropped while it is set.
    ///
    /// Continuing to append to the old descriptor would be the exact failure
    /// this module was changed to end: bytes land in a file no reader can
    /// find and no process will ever trim. Dropping them is not a loss —
    /// those bytes were already unreadable — and it avoids growing an
    /// invisible file on a disk that is quite possibly full, which is one of
    /// the few ways the reopen fails in the first place. Cleared by the next
    /// successful reopen, retried on the maintenance cadence.
    detached: bool,
}

/// `(dev, ino)` on Unix. Enough to notice that the path now resolves to a
/// different inode than the one we hold open.
type FileIdentity = (u64, u64);

static WRITER: LazyLock<Mutex<Option<LogWriter>>> = LazyLock::new(|| Mutex::new(open_writer()));

/// See [`redirect_to_temp_for_tests`].
static TEST_REDIRECT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Redirect all subsequent unified-log writes **and** snapshot reads to a
/// per-process file under the system temp directory, so test binaries stop
/// writing synthetic events into the developer's real
/// `~/.echo-agent/logs/unified.jsonl` (those bursts inflate exactly the counters
/// an incident responder greps for). Runtime-activated rather than a cargo
/// feature: Bazel compiles production and test targets with one shared
/// feature set, so a feature gate would leak into production builds.
///
/// Idempotent and safe at any point: an already-open writer is re-pointed,
/// so an emit that precedes the redirect cannot pin the real path. Test
/// binaries install it pre-main via `#[ctor]`.
pub fn redirect_to_temp_for_tests() {
    TEST_REDIRECT.store(true, std::sync::atomic::Ordering::Relaxed);
    if let Ok(mut guard) = WRITER.lock() {
        *guard = open_writer();
    }
}

fn log_path() -> PathBuf {
    if TEST_REDIRECT.load(std::sync::atomic::Ordering::Relaxed) {
        return test_log_dir().join(LOG_FILE);
    }
    echo_agent_home().join(LOG_DIR).join(LOG_FILE)
}

/// Owner-only (0o700), freshly-created directory for the test redirect.
///
/// The stream carries path metadata and credential tail fragments, and the
/// system temp dir is world-writable on Linux: a pre-planted directory or
/// symlink would let another local user read the file — or make the writer
/// and [`trim_file`] operate through a symlink onto a victim file. The
/// non-recursive `create` fails on any pre-existing path instead of
/// adopting it, and the nanos component makes the name unpredictable.
/// Panicking on failure is deliberate: this branch only runs in test
/// binaries, and silently falling back would reopen the hole via
/// `open_writer_at`'s `create_dir_all`.
fn test_log_dir() -> &'static PathBuf {
    static TEST_LOG_DIR: OnceLock<PathBuf> = OnceLock::new();
    TEST_LOG_DIR.get_or_init(|| {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "echo-agent-unified-log-test-{}-{nanos}",
            std::process::id()
        ));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(&dir)
            .expect("create private unified-log test dir");
        dir
    })
}

pub fn file_size(path: &std::path::Path) -> u64 {
    fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// Identity of whatever file currently lives at `path`, or `None` if nothing
/// does. Compared against the identity captured at open time to detect that
/// our descriptor has been orphaned by a rename or an unlink.
#[cfg(unix)]
fn path_identity(path: &std::path::Path) -> Option<FileIdentity> {
    use std::os::unix::fs::MetadataExt;
    let meta = fs::metadata(path).ok()?;
    Some((meta.dev(), meta.ino()))
}

/// Windows has no comparably cheap stable id from a path stat, so this
/// degrades to presence detection: a deleted log is still healed, a replaced
/// one is not.
#[cfg(not(unix))]
fn path_identity(path: &std::path::Path) -> Option<FileIdentity> {
    fs::metadata(path).ok().map(|_| (0, 0))
}

fn open_writer() -> Option<LogWriter> {
    open_writer_at(log_path())
}

/// Open (creating if needed) a writer for an explicit path.
///
/// Split from [`open_writer`] so a writer re-points at **its own** path when
/// healing a stale handle rather than re-resolving `$ECHO_AGENT_HOME` — which also
/// makes the healing path testable against a temp directory.
fn open_writer_at(path: PathBuf) -> Option<LogWriter> {
    if let Some(parent) = path.parent()
        && let Err(e) = fs::create_dir_all(parent)
    {
        tracing::warn!("[unified_log] failed to create log dir: {e}");
        return None;
    }

    if file_size(&path) >= MAX_SIZE {
        trim_file(&path);
    }

    match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(file) => Some(LogWriter {
            file,
            identity: path_identity(&path),
            path,
            last_maintenance: Instant::now(),
            detached: false,
        }),
        Err(e) => {
            tracing::warn!("[unified_log] failed to open log file: {e}");
            None
        }
    }
}

impl LogWriter {
    /// Re-point at the live file if ours was replaced or removed, and trim if
    /// the file has grown past [`MAX_SIZE`].
    ///
    /// The size check reads the **real** file rather than a per-process byte
    /// counter. A counter only sees this process's own writes, so several
    /// writers sharing one log each believed they were far below the cap while
    /// the file sailed past it — orphaned writers observed at 8.5 MB against a
    /// 5 MB cap.
    ///
    /// Returns whether the handle is safe to write to: `false` once the file
    /// has been replaced or removed and reopening it did not work, so the
    /// caller drops the entry instead of appending it somewhere unreadable.
    fn maintain(&mut self) -> bool {
        if self.last_maintenance.elapsed() < MAINTENANCE_INTERVAL {
            return !self.detached;
        }
        self.last_maintenance = Instant::now();

        if path_identity(&self.path) != self.identity {
            let Some(reopened) = open_writer_at(self.path.clone()) else {
                // Warn on entering the state, not once per tick: a broken log
                // directory would otherwise flood the diagnostic output an
                // operator is trying to read.
                if !self.detached {
                    tracing::warn!(
                        path = %self.path.display(),
                        "[unified_log] log file replaced or removed and reopen failed; \
                         dropping entries until it can be reopened"
                    );
                    self.detached = true;
                }
                return false;
            };
            *self = reopened;
            return true;
        }

        // The path resolves to our inode again — either it always did, or a
        // transient stat failure cleared.
        self.detached = false;

        if file_size(&self.path) >= MAX_SIZE {
            let _ = self.file.flush();
            trim_file(&self.path);
        }
        true
    }
}

fn write_lines(lines: &[u8]) {
    let Ok(mut guard) = WRITER.lock() else { return };
    let writer = match guard.as_mut() {
        Some(w) => w,
        None => return,
    };
    if !writer.maintain() {
        return;
    }

    if let Err(e) = writer.file.write_all(lines) {
        tracing::warn!("[unified_log] write failed: {e}");
    }
}

fn write_entry(entry: &LogEntry) {
    let Ok(mut line) = serde_json::to_vec(entry) else {
        return;
    };
    line.push(b'\n');
    write_lines(&line);
}

/// Drop the oldest lines from the file, keeping roughly the last half,
/// **preserving the inode**.
///
/// Rewrites the retained tail at offset 0 and truncates to match. This must
/// not go through temp + rename: every other process holds an `O_APPEND`
/// descriptor on this inode, and swapping a fresh file in underneath them
/// leaves each one appending to an unlinked inode that nothing can read and
/// nothing will ever trim. That failure was silent and unbounded — a single
/// developer machine accumulated roughly 26 MB across six orphaned inodes,
/// several of them past the 5 MB cap, while the visible log held only what
/// the most recent trimming process happened to write. The unified log was
/// therefore blind during the incident it exists to explain.
///
/// Truncating in place trades the rename's crash-atomicity for the far more
/// valuable property that concurrent writers keep working. A crash between
/// the write and the `set_len` leaves the tail followed by stale bytes; for a
/// line-delimited diagnostic log that costs at most a few garbled lines,
/// against losing every sibling's output indefinitely.
///
/// A sibling appending *during* the rewrite may lose that one line to the
/// truncation. The previous implementation lost every line written after the
/// rename, forever.
///
/// The whole read-modify-write is held under an exclusive advisory lock on
/// the log itself, because trimming in place is only safe for one process at
/// a time — see the comment in the body.
///
/// Known limitation: a single line longer than half the file leaves no
/// newline to cut at, and the trim is skipped rather than split that line.
/// The log then stays over its cap until a shorter line arrives.
pub fn trim_file(path: &std::path::Path) {
    // One trimmer at a time, across processes. Writers decide on the real
    // on-disk size, so when the log crosses the cap every process reaches
    // this function inside the same maintenance window. Two of them
    // interleaving a multi-megabyte rewrite at offset 0 would splice one
    // tail into the other; worse, a trimmer that reads while another is
    // mid-rewrite sees new-tail-over-old-head and computes its own tail from
    // that. Temp + rename was no safer — every process used the same
    // `unified.jsonl.tmp` — it was just rarer, because the old per-process
    // byte counter meant one process did essentially all the trimming.
    //
    // `try_lock`, not `lock`: a contended trim is one somebody else is
    // already doing, so there is nothing to wait for, and waiting would park
    // this process's writer mutex on a foreign process's I/O.
    //
    // A trimmer that decided to trim just before another one finished will
    // find a freshly halved file and halve it again. Losing another half of
    // an over-budget diagnostic log is a far cheaper outcome than interleaved
    // rewrites, so the size is deliberately not re-checked here: callers
    // trim on their own terms and the unit tests trim small files directly.
    let Ok(mut file) = OpenOptions::new().read(true).write(true).open(path) else {
        return;
    };
    if file.try_lock().is_err() {
        return;
    }

    let mut data = Vec::new();
    if let Err(e) = file.read_to_end(&mut data) {
        tracing::warn!("[unified_log] trim read failed: {e}");
        return;
    }
    let half = data.len() / 2;
    // Find the first newline after the halfway point so we don't split a line.
    let start = match data[half..].iter().position(|&b| b == b'\n') {
        Some(pos) => half + pos + 1,
        None => return,
    };
    let tail = &data[start..];

    // Rewind rather than truncate-on-open: the tail is laid down over the
    // head first, and only then is the file shortened, so the retained bytes
    // are never absent from disk.
    if file.rewind().is_err() {
        return;
    }
    if let Err(e) = file.write_all(tail) {
        tracing::warn!("[unified_log] trim rewrite failed: {e}");
        return;
    }
    let _ = file.set_len(tail.len() as u64);
    let _ = file.flush();
    // The lock is released when `file` drops.
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Return a new timestamp string in the unified log format.
fn now_ts() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Emit a log entry from shell itself.
pub fn emit(lvl: LogLevel, msg: &str, sid: Option<&str>, ctx: Option<serde_json::Value>) {
    let entry = LogEntry {
        ts: now_ts(),
        src: LogSource::Shell,
        pid: Some(std::process::id()),
        ver: VERSION.get().cloned(),
        lvl,
        sid: sid.map(Into::into),
        msg: msg.into(),
        ctx,
    };
    write_entry(&entry);
}

/// Ingest a batch of log entries from a client (pager or desktop).
///
/// Called by the `echo.agent/log` notification handler. Entries from
/// [`LogSource::Shell`] are rejected to prevent spoofing.
pub fn ingest_client_entries(src: LogSource, entries: &[ClientLogEntry]) {
    if matches!(src, LogSource::Shell) || entries.is_empty() {
        return;
    }
    // Serialize all entries up front, then write in a single lock acquisition.
    let mut buf = Vec::new();
    for client_entry in entries {
        let entry = LogEntry {
            ts: client_entry.ts.clone(),
            src,
            pid: client_entry.pid,
            ver: client_entry.ver.clone(),
            lvl: client_entry.lvl,
            sid: client_entry.sid.clone(),
            msg: client_entry.msg.clone(),
            ctx: client_entry.ctx.clone(),
        };
        if let Ok(mut line) = serde_json::to_vec(&entry) {
            line.push(b'\n');
            buf.extend_from_slice(&line);
        }
    }
    if !buf.is_empty() {
        write_lines(&buf);
    }
}

/// Convenience: emit an info-level entry from shell.
pub fn info(msg: &str, sid: Option<&str>, ctx: Option<serde_json::Value>) {
    emit(LogLevel::Info, msg, sid, ctx);
}

/// Convenience: emit a warn-level entry from shell.
pub fn warn(msg: &str, sid: Option<&str>, ctx: Option<serde_json::Value>) {
    emit(LogLevel::Warn, msg, sid, ctx);
}

/// Convenience: emit an error-level entry from shell.
pub fn error(msg: &str, sid: Option<&str>, ctx: Option<serde_json::Value>) {
    emit(LogLevel::Error, msg, sid, ctx);
}

/// Convenience: emit a debug-level entry from shell.
pub fn debug(msg: &str, sid: Option<&str>, ctx: Option<serde_json::Value>) {
    emit(LogLevel::Debug, msg, sid, ctx);
}

/// The resolved log path, for error messages that point the user here.
pub fn path() -> PathBuf {
    log_path()
}

/// Read the current unified log file and return its contents.
///
/// Returns `None` if the log file doesn't exist or can't be read.
/// Used by diagnostic uploads to capture the log state at a point in time.
pub fn snapshot_log() -> Option<Vec<u8>> {
    let path = log_path();
    // Flush pending writes before reading.
    if let Ok(mut guard) = WRITER.lock()
        && let Some(ref mut w) = *guard
    {
        let _ = w.file.flush();
    }
    // Lock released intentionally — snapshot is approximate.
    match fs::read(&path) {
        Ok(data) if !data.is_empty() => Some(data),
        _ => None,
    }
}

/// Read the unified log and return only entries belonging to the given session.
///
/// Parses each JSONL line, keeps entries where `"sid"` matches `session_id`,
/// and returns the filtered lines as JSONL bytes. Returns `None` if the log
/// is empty or contains no entries for this session.
pub fn snapshot_session_log(session_id: &str) -> Option<Vec<u8>> {
    let path = log_path();
    if let Ok(mut guard) = WRITER.lock()
        && let Some(ref mut w) = *guard
    {
        let _ = w.file.flush();
    }
    let data = match fs::read(&path) {
        Ok(d) if !d.is_empty() => d,
        _ => return None,
    };
    let mut out = Vec::new();
    for line in data.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_slice::<serde_json::Value>(line)
            && entry.get("sid").and_then(|v| v.as_str()) == Some(session_id)
        {
            out.extend_from_slice(line);
            out.push(b'\n');
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pre-main, so no test in this binary can race the lazily-opened
    /// writer onto the developer's real `~/.echo-agent/logs/unified.jsonl`.
    #[ctor::ctor]
    fn redirect_for_tests() {
        redirect_to_temp_for_tests();
    }

    /// The redirect must cover both the writer and the snapshot readers:
    /// an emit lands in a per-process temp file, never under `echo_agent_home()`.
    #[test]
    fn redirect_routes_writes_and_snapshots_to_process_temp_file() {
        info(
            "unified-log redirect probe",
            Some("redirect-probe-sid"),
            None,
        );
        let snapshot = snapshot_log().expect("snapshot after emit");
        assert!(
            String::from_utf8_lossy(&snapshot).contains("unified-log redirect probe"),
            "snapshot must read the same redirected file the writer appended to"
        );
        assert!(
            log_path().starts_with(std::env::temp_dir()),
            "the shared file must live under the temp dir, not echo_agent_home(): {}",
            log_path().display()
        );
    }

    #[test]
    fn log_entry_serializes_minimal() {
        let entry = LogEntry {
            ts: "2025-07-14T10:30:00.123Z".into(),
            src: LogSource::Shell,
            pid: None,
            ver: None,
            lvl: LogLevel::Info,
            sid: None,
            msg: "test".into(),
            ctx: None,
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(!json.contains("sid"));
        assert!(!json.contains("ctx"));
        assert!(!json.contains("pid"));
        assert!(!json.contains("ver"));
        assert!(json.contains("\"src\":\"shell\""));
    }

    #[test]
    fn log_entry_serializes_full() {
        let entry = LogEntry {
            ts: "2025-07-14T10:30:00.123Z".into(),
            src: LogSource::EchoAgentPager,
            pid: Some(4242),
            ver: Some("0.1.211".into()),
            lvl: LogLevel::Warn,
            sid: Some("abc123".into()),
            msg: "connection lost".into(),
            ctx: Some(serde_json::json!({"retry": 3})),
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"sid\":\"abc123\""));
        assert!(json.contains("\"retry\":3"));
        assert!(json.contains("\"pid\":4242"));
        assert!(json.contains("\"ver\":\"0.1.211\""));
    }

    #[test]
    fn client_entry_round_trip() {
        let wire = r#"{"ts":"2025-07-14T10:30:00.123Z","lvl":"info","msg":"hello"}"#;
        let entry: ClientLogEntry = serde_json::from_str(wire).unwrap();
        assert_eq!(entry.msg, "hello");
        assert!(entry.sid.is_none());
        assert!(entry.ctx.is_none());
    }

    /// The reason this incident was undiagnosable: `trim_file` used to
    /// temp+rename, which swaps the inode out from under every other process
    /// holding an `O_APPEND` descriptor. Their writes then land in an
    /// unlinked inode that no reader can ever see.
    #[cfg(unix)]
    #[test]
    fn trim_file_preserves_the_inode_so_open_handles_survive() {
        use std::os::unix::fs::MetadataExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");
        let mut content = String::new();
        for i in 0..10 {
            content.push_str(&format!("line {i}\n"));
        }
        fs::write(&path, &content).unwrap();
        let before = fs::metadata(&path).unwrap().ino();

        trim_file(&path);

        assert_eq!(
            fs::metadata(&path).unwrap().ino(),
            before,
            "trim must rewrite in place; replacing the inode strands every \
             sibling process's open log handle",
        );
    }

    /// End-to-end version of the same property: a writer that opened the file
    /// *before* a trim must still be able to append to the file a reader sees
    /// afterwards.
    #[cfg(unix)]
    #[test]
    fn writes_from_a_handle_opened_before_trim_remain_visible() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");
        let mut content = String::new();
        for i in 0..10 {
            content.push_str(&format!("line {i}\n"));
        }
        fs::write(&path, &content).unwrap();

        // A sibling process's writer, opened before the trim happens.
        let mut sibling = OpenOptions::new().append(true).open(&path).unwrap();

        trim_file(&path);

        sibling.write_all(b"after trim\n").unwrap();
        sibling.flush().unwrap();

        let visible = fs::read_to_string(&path).unwrap();
        assert!(
            visible.contains("after trim"),
            "a handle opened before the trim must keep writing to the live \
             file, got: {visible:?}",
        );
    }

    /// `maintain` heals a writer whose file was replaced or deleted behind its
    /// back — an older binary still doing temp+rename, an external `rm`, or a
    /// `$TMPDIR` reaper.
    #[cfg(unix)]
    #[test]
    fn maintain_reopens_after_the_file_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");
        fs::write(&path, b"original\n").unwrap();

        let mut writer = LogWriter {
            file: OpenOptions::new().append(true).open(&path).unwrap(),
            identity: path_identity(&path),
            path: path.clone(),
            // Force the maintenance cadence to fire on the next call.
            last_maintenance: Instant::now() - MAINTENANCE_INTERVAL,
            detached: false,
        };
        let original_identity = writer.identity;

        // Simulate an older binary's rename-based trim from another process.
        let replacement = dir.path().join("replacement.jsonl");
        fs::write(&replacement, b"replaced\n").unwrap();
        fs::rename(&replacement, &path).unwrap();
        assert_ne!(
            path_identity(&path),
            original_identity,
            "test setup: the path must now resolve to a new inode",
        );

        assert!(
            writer.maintain(),
            "a writer that successfully re-pointed at the live file is writable",
        );
        writer.file.write_all(b"after replacement\n").unwrap();
        writer.file.flush().unwrap();

        let visible = fs::read_to_string(&path).unwrap();
        assert!(
            visible.contains("after replacement"),
            "a writer whose file was replaced must re-point at the live file \
             instead of writing into the orphaned inode, got: {visible:?}",
        );
        assert_eq!(
            writer.identity,
            path_identity(&path),
            "the healed writer must track the new inode",
        );
    }

    /// The same healing path for outright deletion, which is how a
    /// `$TMPDIR` reaper (or a stray `rm`) silences a long-lived agent.
    #[cfg(unix)]
    #[test]
    fn maintain_reopens_after_the_file_is_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");
        fs::write(&path, b"original\n").unwrap();

        let mut writer = LogWriter {
            file: OpenOptions::new().append(true).open(&path).unwrap(),
            identity: path_identity(&path),
            path: path.clone(),
            last_maintenance: Instant::now() - MAINTENANCE_INTERVAL,
            detached: false,
        };

        fs::remove_file(&path).unwrap();

        assert!(
            writer.maintain(),
            "a writer that successfully re-pointed at the live file is writable",
        );
        writer.file.write_all(b"after deletion\n").unwrap();
        writer.file.flush().unwrap();

        let visible = fs::read_to_string(&path).expect("log must be recreated");
        assert!(
            visible.contains("after deletion"),
            "a deleted log must be recreated rather than written into the \
             void, got: {visible:?}",
        );
    }

    /// The trim decision must read the real file, not a per-process counter:
    /// with several writers sharing one log, each one's own byte count stays
    /// far below the cap while the file sails past it.
    #[cfg(unix)]
    #[test]
    fn maintain_trims_growth_this_process_did_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");

        let mut writer = LogWriter {
            file: OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .unwrap(),
            identity: path_identity(&path),
            path: path.clone(),
            last_maintenance: Instant::now() - MAINTENANCE_INTERVAL,
            detached: false,
        };

        // Someone else fills the log past the cap; this writer wrote nothing.
        let line = "x".repeat(1023);
        let mut bulk = String::new();
        while bulk.len() as u64 <= MAX_SIZE {
            bulk.push_str(&line);
            bulk.push('\n');
        }
        fs::write(&path, &bulk).unwrap();
        // Rewriting the path in place keeps the inode, so the handle is fine.
        assert_eq!(path_identity(&path), writer.identity);
        assert!(file_size(&path) >= MAX_SIZE);

        assert!(
            writer.maintain(),
            "trimming does not detach the writer; its handle stays usable",
        );

        assert!(
            file_size(&path) < MAX_SIZE,
            "a writer must trim on observed file size, not on its own \
             write counter; size is now {}",
            file_size(&path),
        );
    }

    /// Trimming in place is only safe for one process at a time, and deciding
    /// on the real file size means every writer reaches [`trim_file`] in the
    /// same maintenance window once the log crosses the cap. A trimmer that
    /// finds the log already being rewritten must leave it alone rather than
    /// interleave a second rewrite at offset 0.
    #[test]
    fn trim_file_yields_to_a_concurrent_trimmer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");
        let mut content = String::new();
        for i in 0..10 {
            content.push_str(&format!("line {i}\n"));
        }
        fs::write(&path, &content).unwrap();

        // Stand in for another process midway through its own trim.
        let holder = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        holder.lock().expect("test setup: exclusive lock");

        trim_file(&path);

        // Release before reading: the lock is mandatory on Windows.
        drop(holder);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            content,
            "a contended trim must be skipped, not interleaved with the \
             rewrite already in progress",
        );

        // And it is only deferred, not lost: the next trim proceeds.
        trim_file(&path);
        assert!(fs::read_to_string(&path).unwrap().len() < content.len());
    }

    /// The reopen can itself fail — a log directory replaced by a file, a full
    /// disk, exhausted descriptors. Appending to the old handle anyway would
    /// reproduce the orphaning this module was changed to end, so the writer
    /// drops entries until it can reach the real file again.
    #[cfg(unix)]
    #[test]
    fn maintain_stops_writing_when_the_file_cannot_be_reopened() {
        let dir = tempfile::tempdir().unwrap();
        let log_dir = dir.path().join("logs");
        fs::create_dir_all(&log_dir).unwrap();
        let path = log_dir.join("test.jsonl");
        fs::write(&path, b"original\n").unwrap();

        let mut writer = LogWriter {
            file: OpenOptions::new().append(true).open(&path).unwrap(),
            identity: path_identity(&path),
            path: path.clone(),
            last_maintenance: Instant::now() - MAINTENANCE_INTERVAL,
            detached: false,
        };

        // Wipe the log's directory and put a regular file in its place, so
        // the path no longer resolves to our inode *and* cannot be reopened.
        fs::remove_dir_all(&log_dir).unwrap();
        fs::write(&log_dir, b"not a directory\n").unwrap();

        assert!(
            !writer.maintain(),
            "a writer that cannot reach the real log must report itself \
             unwritable instead of appending into the orphaned inode",
        );
        assert!(
            !writer.maintain(),
            "and must stay unwritable between maintenance ticks, not just on \
             the tick that discovered the problem",
        );

        // Healing: once the directory is back, the next tick reopens.
        fs::remove_file(&log_dir).unwrap();
        writer.last_maintenance = Instant::now() - MAINTENANCE_INTERVAL;
        assert!(
            writer.maintain(),
            "the writer must recover as soon as the path is usable again",
        );

        writer.file.write_all(b"after recovery\n").unwrap();
        writer.file.flush().unwrap();
        let visible = fs::read_to_string(&path).unwrap();
        assert!(
            visible.contains("after recovery"),
            "the recovered writer must be attached to the visible file, \
             got: {visible:?}",
        );
    }

    #[test]
    fn trim_file_keeps_recent_half() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");
        let mut content = String::new();
        for i in 0..10 {
            content.push_str(&format!("line {i}\n"));
        }
        fs::write(&path, &content).unwrap();
        trim_file(&path);
        let result = fs::read_to_string(&path).unwrap();
        // Should keep roughly the second half, starting at a line boundary.
        assert!(!result.contains("line 0"));
        assert!(result.contains("line 9"));
        assert!(result.len() < content.len());
        // Every line should be complete (no partial lines).
        for line in result.lines() {
            assert!(line.starts_with("line "));
        }
    }

    #[test]
    fn trim_file_no_newline_in_second_half_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");
        let content = "single-line-no-newline";
        fs::write(&path, content).unwrap();
        trim_file(&path);
        assert_eq!(fs::read_to_string(&path).unwrap(), content);
    }

    #[test]
    fn trim_file_missing_file_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.jsonl");
        trim_file(&path);
        assert!(!path.exists());
    }

    #[test]
    fn ingest_rejects_shell_src() {
        ingest_client_entries(
            LogSource::Shell,
            &[ClientLogEntry {
                ts: "2025-01-01T00:00:00.000Z".into(),
                pid: None,
                ver: None,
                lvl: LogLevel::Info,
                sid: None,
                msg: "sneaky".into(),
                ctx: None,
            }],
        );
    }

    #[test]
    fn unknown_src_rejected_at_deserialization() {
        for bad in &[
            r#"{"src":"evil","entries":[]}"#,
            r#"{"src":"","entries":[]}"#,
            r#"{"src":"ECHO_AGENT-PAGER","entries":[]}"#,
        ] {
            assert!(serde_json::from_str::<LogNotificationParams>(bad).is_err());
        }
    }

    #[test]
    fn notification_params_round_trip() {
        let params = LogNotificationParams {
            src: LogSource::EchoAgentPager,
            entries: vec![
                ClientLogEntry {
                    ts: "2025-07-14T10:30:00.123Z".into(),
                    pid: Some(1234),
                    ver: None,
                    lvl: LogLevel::Info,
                    sid: Some("s1".into()),
                    msg: "first".into(),
                    ctx: None,
                },
                ClientLogEntry {
                    ts: "2025-07-14T10:30:00.456Z".into(),
                    pid: Some(1234),
                    ver: Some("0.1.211".into()),
                    lvl: LogLevel::Error,
                    sid: None,
                    msg: "second".into(),
                    ctx: Some(serde_json::json!({"code": 42})),
                },
            ],
        };
        let json = serde_json::to_string(&params).unwrap();
        let parsed: LogNotificationParams = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.entries.len(), 2);
        assert_eq!(parsed.entries[0].msg, "first");
        assert_eq!(parsed.entries[1].msg, "second");
    }
}
