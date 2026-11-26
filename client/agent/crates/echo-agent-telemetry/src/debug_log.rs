//! Reusable non-blocking file-logging tracing layers for the `--debug` firehose.
//!
//! Two install modes, chosen by env precedence (see `resolve_debug_target_inner`):
//! - PerSession (`ECHO_AGENT_DEBUG_LOG=1`): a routing layer fans each session's
//!   firehose to `~/.echo-agent/debug/<session_id>.txt` (one file per session), with a
//!   `<role>-<pid>.txt` catch-all for events fired outside any session span, and
//!   a `latest.txt` symlink pointing at the most-recently-opened session file.
//! - SingleFile (explicit path via `ECHO_AGENT_LOG_FILE` or `ECHO_AGENT_DEBUG_LOG=<path>`):
//!   one flat `fmt` file, routing bypassed. Disk IO stays off the tracing hot
//!   path via `tracing_appender`'s non-blocking writer in both modes.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use tracing::Subscriber;
use tracing::field::{Field, Visit};
use tracing_appender::non_blocking::NonBlocking;
use tracing_subscriber::filter::{EnvFilter, LevelFilter};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;

use crate::session_ctx::SESSION_ID_FIELD;
use echo_agent_config::echo_agent_home;

/// Which env var requested a single-file debug log (drives filter and diagnostics).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DebugSource {
    EchoAgentLogFile,
    EchoAgentDebugLog,
}

impl DebugSource {
    fn label(self) -> &'static str {
        match self {
            Self::EchoAgentLogFile => "ECHO_AGENT_LOG_FILE",
            Self::EchoAgentDebugLog => "ECHO_AGENT_DEBUG_LOG",
        }
    }
}

/// Target for the pager's always-on compact ACP update summary line
/// (kind, ids, status, payload sizes).
///
/// Lives here (not in `echo-agent-pager`) so the firehose directives below and
/// the pager's own filter are built from the same constants — a rename can't
/// silently desync them into a no-op directive.
pub const ACP_UPDATE_TARGET: &str = "acp_update";

/// Target for the pager's full ACP update payload dump (plain JSON).
///
/// Off in the pager's release filter; the firehose is the always-available
/// subscriber for full payloads, and it writes to disk, where the volume is
/// safe. See `echo-agent-pager/src/tracing.rs` for the consumer side.
pub const ACP_UPDATE_PAYLOAD_TARGET: &str = "acp_update_payload";

/// Module path of rmcp 2.1's per-reconnect SSE warn (`sse stream error: ...`),
/// which subscribers demote to `error` to drop the flood. Re-check on rmcp bump.
pub const RMCP_SSE_NOISE_TARGET: &str = "rmcp::transport::common::client_side_sse";

// Broad firehose filter for the routing/ECHO_AGENT_DEBUG_LOG sources: capture our
// crates at debug regardless of a narrowing RUST_LOG, with deps at info so they
// don't flood. Curated first-party allowlist: new echoagent crates default to `info`
// until added here.
const FIREHOSE_BASE_DIRECTIVES: &str = "info,echo_agent_pager=debug,echo_agent_runtime=debug,echo_agent_tools=debug,echo_agent_telemetry=debug,echo_agent_core=debug,echo_agent_mcp=debug,echo_agent_session_search=debug,echo_agent_acp=debug,sampling_log=off";

// Full firehose directives: the curated crate list plus the pager's ACP
// update target (built from the constant above, not a literal).
fn firehose_directives() -> String {
    format!("{FIREHOSE_BASE_DIRECTIVES},{ACP_UPDATE_TARGET}=debug")
}

// The broad firehose filter, used by both the routing layer and the
// ECHO_AGENT_DEBUG_LOG single-file source (mirrors `default_file_filter`).
fn firehose_filter() -> EnvFilter {
    EnvFilter::new(firehose_directives())
}

// RUST_LOG-respecting filter for the ECHO_AGENT_LOG_FILE source: DEBUG default, honor
// RUST_LOG, silence sampling_log (preserves ECHO_AGENT_LOG_FILE back-compat).
fn default_file_filter() -> EnvFilter {
    EnvFilter::builder()
        .with_default_directive(LevelFilter::DEBUG.into())
        .from_env_lossy()
        .add_directive(
            "sampling_log=off"
                .parse()
                .expect("static directive is valid"),
        )
}

// Open `path` as a non-blocking flat `fmt` layer with `filter`; ansi off, target on.
fn build_file_layer<S>(path: &Path, filter: EnvFilter) -> std::io::Result<impl Layer<S>>
where
    S: Subscriber + for<'span> LookupSpan<'span>,
{
    let non_blocking = crate::appender::non_blocking_file_writer(path)?;
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_ansi(false)
        .with_writer(non_blocking)
        .with_filter(filter);
    Ok(fmt_layer)
}

// ── Per-session routing layer ───────────────────────────────────────────────

/// Filesystem-safe session key. Sanitized once at capture (`on_new_span`) and
/// stashed in the span's tracing extensions, so events fired anywhere under the
/// span route to the right file without re-sanitizing on the hot path.
#[derive(Clone)]
struct SessionId(String);

/// Visits span attributes to pull out the `session_id` field. Production records
/// it via `%` (Display → `record_debug`, no quotes); like `EventVisitor`, the
/// single `record_debug` impl captures every field type (the other recorders
/// default to it).
#[derive(Default)]
struct SessionIdVisitor(Option<String>);

impl Visit for SessionIdVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == SESSION_ID_FIELD {
            self.0 = Some(format!("{value:?}"));
        }
    }
}

/// Renders an event's message + remaining fields into plain strings. All field
/// types funnel through `record_debug` (the trait's other recorders default to
/// it), so this one impl captures everything.
#[derive(Default)]
struct EventVisitor {
    message: String,
    fields: String,
}

impl Visit for EventVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write as _;
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            let _ = write!(self.fields, " {}={:?}", field.name(), value);
        }
    }
}

// Format one compact, ANSI-free firehose line. Intentionally NOT byte-identical
// to `fmt::Layer`: its `FormatEvent` can't be reused from another layer and a
// `MakeWriter` can't see span context, so we render here. Span context is
// omitted on purpose — the file name already carries the session id.
fn format_event(event: &tracing::Event<'_>) -> String {
    let meta = event.metadata();
    let mut visitor = EventVisitor::default();
    event.record(&mut visitor);
    let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
    let level = meta.level();
    let target = meta.target();
    // Skip the message gap when there's no `message` field so a field-only event
    // renders "target: k=v" (each field already carries a leading space), not
    // "target:  k=v" with a dangling double space.
    if visitor.message.is_empty() {
        format!("{ts} {level} {target}:{}\n", visitor.fields)
    } else {
        format!(
            "{ts} {level} {target}: {}{}\n",
            visitor.message, visitor.fields
        )
    }
}

// Keep per-session file names filesystem-safe. A session id is normally a UUID,
// but never let an unexpected value (path separators, `..`) escape the dir.
fn sanitize_key(id: &str) -> String {
    let safe: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    // Map empty / dot-only keys ("", ".", "..", "...") to a constant: those are
    // filesystem-special, and relying on the `.txt` suffix to neutralize them is
    // incidental. Make the safety explicit instead.
    if safe.is_empty() || safe.bytes().all(|b| b == b'.') {
        return "_".to_owned();
    }
    safe
}

// `latest.txt` link + swap-temp name parts, shared by `update_latest_symlink`
// (create/rename) and `prune_old_logs` (spare rule + orphan cleanup) so the
// sites can never drift. Tests pin the literals on purpose: orphans created by
// already-shipped binaries must stay reapable across a rename of these consts.
const LATEST_LINK_NAME: &str = "latest.txt";
const LATEST_TMP_PREFIX: &str = ".latest.";
const LATEST_TMP_SUFFIX: &str = ".tmp";

/// Repoint `<dir>/latest.txt` at `target` (a sibling session file) for
/// `tail -f`. Best-effort and Unix-only; the relative target keeps the link
/// valid regardless of the dir's absolute path.
#[cfg(unix)]
fn update_latest_symlink(dir: &Path, target: &Path) {
    let Some(name) = target.file_name() else {
        return;
    };
    // Atomic swap: symlink a unique temp then rename it over latest.txt (rename
    // is atomic on POSIX), so a racing `tail -f` never sees latest.txt missing.
    // The temp name is keyed by the target file so concurrent opens of different
    // sessions don't collide on it.
    let tmp = dir.join(format!(
        "{LATEST_TMP_PREFIX}{}{LATEST_TMP_SUFFIX}",
        name.to_string_lossy()
    ));
    let _ = std::fs::remove_file(&tmp);
    if std::os::unix::fs::symlink(name, &tmp).is_ok()
        && std::fs::rename(&tmp, dir.join(LATEST_LINK_NAME)).is_err()
    {
        // Rename failed: remove the temp symlink now rather than leaving an
        // orphan for prune to reap only after LOG_RETENTION.
        let _ = std::fs::remove_file(&tmp);
    }
}

#[cfg(not(unix))]
fn update_latest_symlink(_dir: &Path, _target: &Path) {}

/// Per-session sinks plus a single fallback sink, all behind the routing layer's
/// mutex. There is no cap or eviction: each distinct session id opens one file +
/// non-blocking worker + parked guard that persist for the process lifetime
/// (reclaimed only when the process/leader restarts). That is acceptable for an
/// opt-in, debug-only firehose; a long-lived `--debug` leader holds one fd per
/// session it logs. The central guard parking (`appender`) is what lets
/// `flush()` drain these at exit, so we do not reclaim per session.
#[derive(Default)]
struct SinkMap {
    sessions: HashMap<String, NonBlocking>,
    fallback: Option<NonBlocking>,
}

/// Routes the firehose per session: events under a `session` span go to
/// `<dir>/<session_id>.txt`; everything else to `<dir>/<role>-<pid>.txt`.
struct RoutingLayer {
    dir: PathBuf,
    role: String,
    pid: u32,
    // The lock is scoped to map access ONLY — file opens (fs + a worker-thread
    // spawn + the appender's own mutex) run OUTSIDE it, so a tracing event
    // emitted on the open path can't re-enter and deadlock this non-reentrant
    // Mutex. Lock-on-write is otherwise fine: the firehose is opt-in/debug-only.
    sinks: Mutex<SinkMap>,
}

impl RoutingLayer {
    fn new(dir: PathBuf, role: String, pid: u32) -> Self {
        Self {
            dir,
            role,
            pid,
            sinks: Mutex::new(SinkMap::default()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, SinkMap> {
        self.sinks.lock().unwrap_or_else(|p| p.into_inner())
    }

    // Append `line` to the session's file. `key` is already sanitized. Opens (and
    // points `latest.txt` at) the file on first use; open failures degrade to a
    // no-op for that file.
    fn write_session(&self, key: &str, line: &[u8]) {
        // Fast path: writer already open. Hold the lock only for the lookup + the
        // (non-blocking, channel-only) write.
        {
            let mut map = self.lock();
            if let Some(writer) = map.sessions.get_mut(key) {
                let _ = writer.write_all(line);
                return;
            }
        }
        // First event for this session: open OUTSIDE the lock.
        let path = self.dir.join(format!("{key}.txt"));
        let Ok(mut writer) = crate::appender::non_blocking_file_writer(&path) else {
            return;
        };
        update_latest_symlink(&self.dir, &path);
        let _ = writer.write_all(line);
        let mut map = self.lock();
        // If a concurrent event opened it first, keep that one and drop ours (the
        // line we wrote already reached the file via our worker).
        map.sessions.entry(key.to_owned()).or_insert(writer);
    }

    // Append `line` to the `<role>-<pid>.txt` catch-all, opening it on first use.
    fn write_fallback(&self, line: &[u8]) {
        {
            let mut map = self.lock();
            if let Some(writer) = map.fallback.as_mut() {
                let _ = writer.write_all(line);
                return;
            }
        }
        let path = self.dir.join(format!("{}-{}.txt", self.role, self.pid));
        let Ok(mut writer) = crate::appender::non_blocking_file_writer(&path) else {
            return;
        };
        let _ = writer.write_all(line);
        let mut map = self.lock();
        if map.fallback.is_none() {
            map.fallback = Some(writer);
        }
    }
}

impl<S> Layer<S> for RoutingLayer
where
    S: Subscriber + for<'span> LookupSpan<'span>,
{
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        ctx: Context<'_, S>,
    ) {
        let mut visitor = SessionIdVisitor::default();
        attrs.record(&mut visitor);
        if let Some(sid) = visitor.0
            && let Some(span) = ctx.span(id)
        {
            // Sanitize once at capture so the stored key is always filesystem-safe
            // and `on_event` never re-sanitizes on the hot path.
            span.extensions_mut().insert(SessionId(sanitize_key(&sid)));
        }
    }

    fn on_event(&self, event: &tracing::Event<'_>, ctx: Context<'_, S>) {
        // Nearest enclosing span (leaf→root) carrying a session id wins. The key
        // is already sanitized (stored at `on_new_span`).
        let session_key = ctx.event_scope(event).and_then(|scope| {
            scope
                .into_iter()
                .find_map(|span| span.extensions().get::<SessionId>().map(|s| s.0.clone()))
        });
        let line = format_event(event);
        match session_key {
            Some(key) => self.write_session(&key, line.as_bytes()),
            None => self.write_fallback(line.as_bytes()),
        }
    }
}

// ── Install + lifecycle ──────────────────────────────────────────────────────

/// Resolve the requested debug target and install the matching firehose layer on
/// `registry`, then init the subscriber.
///
/// PerSession installs the routing layer (firehose filter, RUST_LOG-immune) and
/// prunes old session logs; SingleFile installs a flat `fmt` file picking the
/// filter by source (ECHO_AGENT_LOG_FILE respects RUST_LOG). Open failures warn AFTER
/// init in the single-file case; routing open failures are per-file at write
/// time and degrade gracefully. `role` names the per-pid fallback file.
pub fn install_firehose<S>(registry: S, role: &str)
where
    S: Subscriber + for<'span> LookupSpan<'span> + Send + Sync + 'static,
{
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;

    match resolve_debug_target() {
        Some(DebugTarget::PerSession { dir }) => {
            let layer = RoutingLayer::new(dir, role.to_owned(), std::process::id())
                .with_filter(firehose_filter());
            registry.with(layer).init();
            // Tie pruning to actually routing a firehose, not to the flag.
            sweep_old_logs();
        }
        Some(DebugTarget::SingleFile { path, src }) => {
            let filter = match src {
                DebugSource::EchoAgentLogFile => default_file_filter(),
                DebugSource::EchoAgentDebugLog => firehose_filter(),
            };
            match build_file_layer::<S>(&path, filter) {
                Ok(layer) => registry.with(layer).init(),
                Err(e) => {
                    registry.init();
                    tracing::warn!("failed to open {} {path:?}: {e}", src.label());
                }
            }
        }
        None => registry.init(),
    }
}

/// Flush parked firehose writers at process exit (no-op when none installed).
pub fn flush() {
    crate::appender::flush_file_log_guards();
}

/// Where the firehose should go, if anywhere.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DebugTarget {
    /// `ECHO_AGENT_DEBUG_LOG=1` → route per session into `<dir>` (`~/.echo-agent/debug`).
    PerSession { dir: PathBuf },
    /// An explicit path → one flat `fmt` file, routing bypassed.
    SingleFile { path: PathBuf, src: DebugSource },
}

/// Resolve the debug target, honoring precedence: explicit ECHO_AGENT_LOG_FILE wins
/// (single file, RUST_LOG filter); else ECHO_AGENT_DEBUG_LOG — a truthy bool routes
/// per session into `~/.echo-agent/debug`, an explicit path writes a single file.
///
/// Read via `var_os` (not `var`) so a non-UTF-8 path isn't silently dropped.
pub(crate) fn resolve_debug_target() -> Option<DebugTarget> {
    let echo_agent_log_file = std::env::var_os("ECHO_AGENT_LOG_FILE");
    let echo_agent_debug_log = std::env::var_os("ECHO_AGENT_DEBUG_LOG");
    resolve_debug_target_inner(
        echo_agent_log_file.as_deref(),
        echo_agent_debug_log.as_deref(),
        &echo_agent_home().join("debug"),
    )
}

// Empty / whitespace (when valid UTF-8) counts as unset; a non-UTF-8 value is
// never blank.
fn is_blank(v: &OsStr) -> bool {
    v.to_str().is_some_and(|s| s.trim().is_empty())
}

// Build a path from an env value: trim surrounding whitespace when it is valid
// UTF-8, and preserve the raw bytes otherwise (non-UTF-8 paths must survive).
fn os_path(v: &OsStr) -> PathBuf {
    match v.to_str() {
        Some(s) => PathBuf::from(s.trim()),
        None => PathBuf::from(v),
    }
}

// Env-free precedence core so the resolution rules are unit-testable. The role
// and pid are no longer part of resolution: the routing layer owns fallback
// naming, so resolution only decides routing-dir vs single-file-path. Takes
// `OsStr` so non-UTF-8 paths round-trip; only the bool-vs-path discrimination
// needs UTF-8 (a non-UTF-8 value can't be a bool keyword, so it's a path).
fn resolve_debug_target_inner(
    echo_agent_log_file: Option<&OsStr>,
    echo_agent_debug_log: Option<&OsStr>,
    debug_dir: &Path,
) -> Option<DebugTarget> {
    if let Some(raw) = echo_agent_log_file
        && !is_blank(raw)
    {
        return Some(DebugTarget::SingleFile {
            path: os_path(raw),
            src: DebugSource::EchoAgentLogFile,
        });
    }
    let raw = echo_agent_debug_log?;
    match raw.to_str().map(str::trim) {
        Some("" | "0" | "false" | "off" | "no") => None,
        Some("1" | "true" | "on" | "yes") => Some(DebugTarget::PerSession {
            dir: debug_dir.to_path_buf(),
        }),
        // Any other UTF-8 value, or a non-UTF-8 value (`None`), is an explicit path.
        _ => Some(DebugTarget::SingleFile {
            path: os_path(raw),
            src: DebugSource::EchoAgentDebugLog,
        }),
    }
}

/// Retention window for firehose debug logs: files older than this are pruned.
const LOG_RETENTION: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 60 * 60);

/// Prune `*.txt` firehose files (and orphaned `latest.txt` swap temps) under
/// `~/.echo-agent/debug` older than [`LOG_RETENTION`] so the dir doesn't grow
/// unbounded. Age-based (not count-based) so a still-open log from a concurrent
/// process is never unlinked mid-write; best-effort, ignore errors.
pub(crate) fn sweep_old_logs() {
    prune_old_logs(&echo_agent_home().join("debug"), LOG_RETENTION);
}

// Pure prune core: remove `*.txt` files and orphaned `latest.txt` swap temps in
// `dir` older than `max_age`. Age-based so a recently-written (active) log is
// never deleted; spares the `latest.txt` symlink (a stale link is harmless and
// never an active file); best-effort so cleanup never fails logging setup;
// testable against a tempdir.
fn prune_old_logs(dir: &Path, max_age: std::time::Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let is_log = name.ends_with(".txt") && name != LATEST_LINK_NAME;
        // Swap temps matching this shape that survive the age gate below are
        // orphans of a crash between `update_latest_symlink`'s create and rename.
        let is_latest_swap_tmp =
            name.starts_with(LATEST_TMP_PREFIX) && name.ends_with(LATEST_TMP_SUFFIX);
        if !is_log && !is_latest_swap_tmp {
            continue;
        }
        // `DirEntry::metadata` does not follow symlinks, so a dangling orphaned
        // temp still yields its own mtime here.
        let Ok(modified) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        if now.duration_since(modified).is_ok_and(|age| age > max_age) {
            let _ = std::fs::remove_file(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Routing tests drive real non-blocking writers whose worker guards are
    // parked in a process-lifetime static; flushing drains ALL of them. Serialize
    // such tests so a concurrent `cargo test` thread can't clear another's guards
    // before it reads. (nextest already isolates each test in its own process.)
    fn flush_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    #[test]
    fn build_file_layer_creates_parent_dir_and_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("debug.log");
        assert!(!path.parent().unwrap().exists());

        let layer = build_file_layer::<tracing_subscriber::Registry>(&path, default_file_filter());

        assert!(layer.is_ok());
        assert!(path.parent().unwrap().exists());
        assert!(path.exists());
    }

    #[test]
    fn build_file_layer_errors_when_open_fails() {
        // Opening an existing directory in append mode fails, exercising the Err path.
        let dir = tempfile::tempdir().unwrap();
        let layer =
            build_file_layer::<tracing_subscriber::Registry>(dir.path(), default_file_filter());
        assert!(layer.is_err());
    }

    #[test]
    fn resolve_target_unset_is_none() {
        assert!(resolve_debug_target_inner(None, None, Path::new("/debug")).is_none());
    }

    #[test]
    fn resolve_target_debug_log_disabled_is_none() {
        for v in ["0", "false", "off", "no", "", "  "] {
            assert!(
                resolve_debug_target_inner(None, Some(OsStr::new(v)), Path::new("/debug"))
                    .is_none(),
                "expected None for ECHO_AGENT_DEBUG_LOG={v:?}"
            );
        }
    }

    #[test]
    fn resolve_target_debug_log_enabled_is_per_session_dir() {
        for v in ["1", "true", "on", "yes"] {
            let target =
                resolve_debug_target_inner(None, Some(OsStr::new(v)), Path::new("/debug")).unwrap();
            assert_eq!(
                target,
                DebugTarget::PerSession {
                    dir: PathBuf::from("/debug")
                },
                "expected PerSession for ECHO_AGENT_DEBUG_LOG={v:?}"
            );
        }
    }

    #[test]
    fn resolve_target_debug_log_custom_path_is_single_file() {
        let target = resolve_debug_target_inner(
            None,
            Some(OsStr::new("/tmp/custom.log")),
            Path::new("/debug"),
        )
        .unwrap();
        assert_eq!(
            target,
            DebugTarget::SingleFile {
                path: PathBuf::from("/tmp/custom.log"),
                src: DebugSource::EchoAgentDebugLog,
            }
        );
    }

    #[test]
    fn resolve_target_log_file_wins_over_debug_log() {
        let target = resolve_debug_target_inner(
            Some(OsStr::new("/tmp/explicit.log")),
            Some(OsStr::new("1")),
            Path::new("/debug"),
        )
        .unwrap();
        assert_eq!(
            target,
            DebugTarget::SingleFile {
                path: PathBuf::from("/tmp/explicit.log"),
                src: DebugSource::EchoAgentLogFile,
            }
        );
    }

    #[test]
    fn resolve_target_empty_log_file_falls_through_to_debug_log() {
        // Empty / whitespace ECHO_AGENT_LOG_FILE is treated as unset (mirrors ECHO_AGENT_DEBUG_LOG).
        for blank in ["", "   "] {
            let target = resolve_debug_target_inner(
                Some(OsStr::new(blank)),
                Some(OsStr::new("1")),
                Path::new("/debug"),
            )
            .unwrap();
            assert_eq!(
                target,
                DebugTarget::PerSession {
                    dir: PathBuf::from("/debug")
                }
            );
        }
        assert!(
            resolve_debug_target_inner(Some(OsStr::new("")), None, Path::new("/debug")).is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolve_target_non_utf8_debug_log_path_is_single_file() {
        use std::os::unix::ffi::OsStrExt;

        // A non-UTF-8 ECHO_AGENT_DEBUG_LOG value is a path, not a bool keyword, and its
        // bytes must round-trip (not be silently dropped).
        let raw = OsStr::from_bytes(b"/tmp/\xff/fire.txt");
        let target = resolve_debug_target_inner(None, Some(raw), Path::new("/debug")).unwrap();
        match target {
            DebugTarget::SingleFile { path, src } => {
                assert_eq!(src, DebugSource::EchoAgentDebugLog);
                assert_eq!(path.as_os_str(), raw);
            }
            other => panic!("expected SingleFile for non-UTF-8 path, got {other:?}"),
        }
    }

    #[test]
    fn firehose_directives_parse() {
        // Guard against the const rotting: every directive must parse strictly.
        for d in firehose_directives().split(',') {
            d.parse::<tracing_subscriber::filter::Directive>()
                .unwrap_or_else(|e| panic!("invalid directive {d:?}: {e}"));
        }
    }

    #[test]
    fn firehose_directives_include_acp_update_targets() {
        let directives = firehose_directives();
        assert!(directives.contains(&format!("{ACP_UPDATE_TARGET}=debug")));
        assert!(!directives.contains(&format!("{ACP_UPDATE_PAYLOAD_TARGET}=debug")));
    }

    #[test]
    fn sanitize_key_replaces_path_separators_and_dot_only() {
        assert_eq!(sanitize_key("01923-abcd-EF"), "01923-abcd-EF");
        assert_eq!(sanitize_key("../escape"), ".._escape");
        assert_eq!(sanitize_key("a/b\\c"), "a_b_c");
        // Dot-only / empty keys collapse to a safe constant.
        for dotty in ["", ".", "..", "..."] {
            assert_eq!(sanitize_key(dotty), "_", "expected '_' for {dotty:?}");
        }
    }

    #[test]
    fn layer_routes_event_by_session_span() {
        use tracing_subscriber::layer::SubscriberExt as _;

        let _lock = flush_test_lock();
        let dir = tempfile::tempdir().unwrap();
        let layer = RoutingLayer::new(dir.path().to_path_buf(), "agent".to_owned(), 4242);
        let subscriber = tracing_subscriber::registry().with(layer);

        tracing::subscriber::with_default(subscriber, || {
            // `%` mirrors production's `info_span!("session", session_id = %...)`.
            tracing::info_span!("session", session_id = %"sess-xyz").in_scope(|| {
                tracing::info!(target: "echo_agent_runtime", "inside session");
            });
            tracing::info!(target: "echo_agent_runtime", "outside session");
        });
        crate::appender::flush_file_log_guards();

        let session_file = std::fs::read_to_string(dir.path().join("sess-xyz.txt")).unwrap();
        assert!(
            session_file.contains("inside session"),
            "session file: {session_file:?}"
        );
        assert!(!session_file.contains("outside session"));

        let fallback = std::fs::read_to_string(dir.path().join("agent-4242.txt")).unwrap();
        assert!(
            fallback.contains("outside session"),
            "fallback file: {fallback:?}"
        );
        assert!(!fallback.contains("inside session"));
    }

    #[test]
    fn layer_routes_under_real_firehose_filter() {
        use tracing_subscriber::layer::SubscriberExt as _;

        let _lock = flush_test_lock();
        let dir = tempfile::tempdir().unwrap();
        // Exactly the production wrapper: routing layer behind FIREHOSE_DIRECTIVES.
        // Pins the linchpin invariant — the `session` span (INFO, target
        // `echo_agent_telemetry::session_ctx`) survives the real filter so
        // `event_scope` still finds it — at the unit level.
        let layer = RoutingLayer::new(dir.path().to_path_buf(), "agent".to_owned(), 7)
            .with_filter(firehose_filter());
        let subscriber = tracing_subscriber::registry().with(layer);

        tracing::subscriber::with_default(subscriber, || {
            tracing::info_span!(
                target: "echo_agent_telemetry::session_ctx",
                "session",
                session_id = %"sid-real"
            )
            .in_scope(|| {
                tracing::debug!(target: "echo_agent_runtime", "filtered routing works");
            });
        });
        crate::appender::flush_file_log_guards();

        let session_file = std::fs::read_to_string(dir.path().join("sid-real.txt")).unwrap();
        assert!(
            session_file.contains("filtered routing works"),
            "session file under real filter: {session_file:?}"
        );
        // Must route to the session file, NOT silently fall back to per-pid.
        assert!(
            !dir.path().join("agent-7.txt").exists(),
            "event must route to the session file, not the fallback"
        );
    }

    #[test]
    fn layer_routes_two_sessions_to_distinct_files() {
        use tracing_subscriber::layer::SubscriberExt as _;

        let _lock = flush_test_lock();
        let dir = tempfile::tempdir().unwrap();
        let layer = RoutingLayer::new(dir.path().to_path_buf(), "agent".to_owned(), 1);
        let subscriber = tracing_subscriber::registry().with(layer);

        tracing::subscriber::with_default(subscriber, || {
            tracing::info_span!("session", session_id = %"sid-one").in_scope(|| {
                // Two events in one session also prove within-session accumulation.
                tracing::info!(target: "echo_agent_runtime", "one first");
                tracing::info!(target: "echo_agent_runtime", "one second");
            });
            tracing::info_span!("session", session_id = %"sid-two").in_scope(|| {
                tracing::info!(target: "echo_agent_runtime", "two only");
            });
        });
        crate::appender::flush_file_log_guards();

        let one = std::fs::read_to_string(dir.path().join("sid-one.txt")).unwrap();
        let two = std::fs::read_to_string(dir.path().join("sid-two.txt")).unwrap();
        assert!(
            one.contains("one first") && one.contains("one second") && !one.contains("two only"),
            "sid-one.txt: {one:?}"
        );
        assert!(
            two.contains("two only") && !two.contains("one first"),
            "sid-two.txt: {two:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn opening_session_sink_points_latest_symlink() {
        let _lock = flush_test_lock();
        let dir = tempfile::tempdir().unwrap();
        let layer = RoutingLayer::new(dir.path().to_path_buf(), "agent".to_owned(), 1);

        layer.write_session("sess-1", b"x\n");
        let link = dir.path().join("latest.txt");
        assert_eq!(std::fs::read_link(&link).unwrap(), Path::new("sess-1.txt"));

        // Opening a second session repoints latest.txt at it.
        layer.write_session("sess-2", b"y\n");
        assert_eq!(std::fs::read_link(&link).unwrap(), Path::new("sess-2.txt"));

        crate::appender::flush_file_log_guards();
    }

    #[test]
    fn prune_old_logs_removes_old_keeps_recent_and_spares_nonmatching_and_latest() {
        use std::time::{Duration, SystemTime};

        let dir = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        let max_age = Duration::from_secs(7 * 24 * 60 * 60);

        let old = std::fs::File::create(dir.path().join("old-session.txt")).unwrap();
        old.set_modified(now - Duration::from_secs(8 * 24 * 60 * 60))
            .unwrap();
        let recent = std::fs::File::create(dir.path().join("recent-session.txt")).unwrap();
        recent.set_modified(now).unwrap();
        // A non-.txt file must be left untouched even if it is old.
        let other = std::fs::File::create(dir.path().join("unified.jsonl")).unwrap();
        other
            .set_modified(now - Duration::from_secs(30 * 24 * 60 * 60))
            .unwrap();
        // An old `latest.txt` must be spared (harmless stale link / sentinel).
        let latest = std::fs::File::create(dir.path().join("latest.txt")).unwrap();
        latest
            .set_modified(now - Duration::from_secs(30 * 24 * 60 * 60))
            .unwrap();
        // Orphaned `latest.txt` swap temps follow the same age rule: old reaped,
        // recent spared. Regular files here so mtimes are settable cross-platform;
        // the symlink-specific path is covered by the Unix-gated test below.
        let old_tmp =
            std::fs::File::create(dir.path().join(".latest.old-session.txt.tmp")).unwrap();
        old_tmp
            .set_modified(now - Duration::from_secs(8 * 24 * 60 * 60))
            .unwrap();
        let recent_tmp =
            std::fs::File::create(dir.path().join(".latest.recent-session.txt.tmp")).unwrap();
        recent_tmp.set_modified(now).unwrap();

        prune_old_logs(dir.path(), max_age);

        assert!(!dir.path().join("old-session.txt").exists());
        assert!(dir.path().join("recent-session.txt").exists());
        assert!(dir.path().join("unified.jsonl").exists());
        assert!(dir.path().join("latest.txt").exists());
        assert!(!dir.path().join(".latest.old-session.txt.tmp").exists());
        assert!(dir.path().join(".latest.recent-session.txt.tmp").exists());
    }

    #[cfg(unix)]
    #[test]
    fn prune_old_logs_reaps_dangling_orphaned_latest_tmp_symlink() {
        use std::time::{Duration, SystemTime};

        // Models the real orphan: a crash between `update_latest_symlink`'s
        // create and rename leaves the temp symlink, and its target session file
        // may itself be pruned later — so the link is dangling. Literal name (not
        // the consts) so renaming the scheme can't silently strand orphans
        // created by already-shipped binaries.
        let dir = tempfile::tempdir().unwrap();
        let max_age = Duration::from_secs(7 * 24 * 60 * 60);
        // `filetime` ages the link itself; std's `set_modified` follows it (and a
        // dangling link can't even be opened).
        let old = filetime::FileTime::from_system_time(
            SystemTime::now() - Duration::from_secs(8 * 24 * 60 * 60),
        );

        let tmp = dir.path().join(".latest.gone-session.txt.tmp");
        std::os::unix::fs::symlink("gone-session.txt", &tmp).unwrap();
        filetime::set_symlink_file_times(&tmp, old, old).unwrap();
        // A just-created (mid-swap) temp must be spared by age.
        let fresh_tmp = dir.path().join(".latest.live-session.txt.tmp");
        std::os::unix::fs::symlink("live-session.txt", &fresh_tmp).unwrap();
        // `latest.txt` must stay spared by name even as an old dangling symlink.
        let latest = dir.path().join("latest.txt");
        std::os::unix::fs::symlink("gone-session.txt", &latest).unwrap();
        filetime::set_symlink_file_times(&latest, old, old).unwrap();

        prune_old_logs(dir.path(), max_age);

        // `Path::exists` follows symlinks (false for dangling links either way),
        // so assert on the links themselves via `symlink_metadata`.
        assert!(
            std::fs::symlink_metadata(&tmp).is_err(),
            "old orphaned dangling temp symlink must be pruned"
        );
        assert!(
            std::fs::symlink_metadata(&fresh_tmp).is_ok(),
            "fresh mid-swap temp must be spared by age"
        );
        assert!(
            std::fs::symlink_metadata(&latest).is_ok(),
            "latest.txt must be spared"
        );
    }

    #[cfg(unix)]
    #[test]
    fn update_latest_symlink_failed_rename_removes_temp() {
        // Sanity: prove the temp symlink is creatable here, so the helper's
        // symlink step must succeed and the post-call absence below can only
        // come from the rename-failure cleanup branch.
        let dir = tempfile::tempdir().unwrap();
        let tmp = dir.path().join(".latest.sess.txt.tmp");
        std::os::unix::fs::symlink("sess.txt", &tmp).unwrap();
        std::fs::remove_file(&tmp).unwrap();
        // Force the rename to fail: a non-empty directory at `latest.txt` makes
        // rename(2) of a non-directory over it error (EISDIR/ENOTEMPTY).
        let blocker = dir.path().join("latest.txt");
        std::fs::create_dir(&blocker).unwrap();
        std::fs::File::create(blocker.join("occupant.txt")).unwrap();

        update_latest_symlink(dir.path(), &dir.path().join("sess.txt"));

        assert!(
            std::fs::symlink_metadata(&tmp).is_err(),
            "failed swap must remove the temp symlink, not orphan it"
        );
        assert!(
            blocker.join("occupant.txt").exists(),
            "rename must have failed, leaving the blocker dir untouched"
        );
    }

    #[test]
    fn prune_old_logs_spares_active_logs_regardless_of_count() {
        use std::time::{Duration, SystemTime};

        // Guards the reported bug: a concurrent process's still-open (recently
        // written) log must never be unlinked, however many newer logs exist.
        let dir = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        for i in 0..25 {
            let f = std::fs::File::create(dir.path().join(format!("cli-{i}.txt"))).unwrap();
            f.set_modified(now).unwrap();
        }

        prune_old_logs(dir.path(), Duration::from_secs(7 * 24 * 60 * 60));

        let count = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_str().is_some_and(|n| n.ends_with(".txt")))
            .count();
        assert_eq!(count, 25);
    }

    #[test]
    fn prune_old_logs_missing_dir_is_noop() {
        // Best-effort: a nonexistent debug dir must not panic.
        prune_old_logs(
            Path::new("/no/such/echoagent/debug/dir"),
            std::time::Duration::from_secs(1),
        );
    }
}
