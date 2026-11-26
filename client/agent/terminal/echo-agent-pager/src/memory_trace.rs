//! Process memory tracing: durable JSONL evidence for memory investigations.
//!
//! The pager's footprint problems (see `memory_release`) historically had to
//! be diagnosed live with `vmmap` on whatever process happened to still be
//! running — and vmmap mislabels the jemalloc heap as "CoreMedia Capture
//! Data" (jemalloc tags its mmaps `VM_MAKE_TAG(101)` = `VM_MEMORY_CM_REGWARP`),
//! so post-hoc analysis was blind. This module records what actually
//! happened, when, attributed to which code path:
//!
//! - **Samples**: footprint/RSS + allocator gauges every
//!   `ECHO_AGENT_MEMTRACE_INTERVAL_SECS` (default 30s) from a detached thread.
//! - **Purges**: every `memory_release` invocation, tagged with the memory
//!   cliff that triggered it (`reason`), with before/after footprint and
//!   duration — so over- or under-purging is visible per call site.
//! - **Thresholds**: when the physical footprint crosses a bucket
//!   (`ECHO_AGENT_MEMTRACE_THRESHOLD_MB`, default 1 GiB, then doubling), a full
//!   allocator stats dump (jemalloc `malloc_stats_print`) is written next to
//!   the trace and the *threshold hook* fires — the seam the GCS trace-upload
//!   pipeline plugs into (see below). Buckets re-arm once the footprint
//!   halves, so a long-lived process can evidence repeated growth cycles.
//!
//! ## Files
//!
//! `$ECHO_AGENT_HOME/memtrace/<start-ts>-<pid>.jsonl` (+ `.1` after 4 MiB
//! rotation) and `<stem>-jemalloc-<seq>.txt` threshold dumps. Files are
//! created lazily on the first event so short-lived CLI invocations leave no
//! debris. Traces contain **process memory numbers only** — no user content —
//! so they are safe to ship for analysis.
//!
//! ## Seams (composition-root pattern, mirrors `memory_release`)
//!
//! The lib cannot depend on jemalloc; `echo-agent-pager-bin` installs:
//! - [`install_allocator_stats_provider`] — cheap mallctl gauge reads
//! - [`install_allocator_dump_provider`] — full `malloc_stats_print` text
//! - [`install_threshold_hook`] — `(trace_path, crossed_bytes)`; the
//!   GCS upload pipeline (WIP) attaches here to ship the trace + dump when a
//!   process gets big enough to care about. Absent a hook, crossing is still
//!   recorded locally and surfaced via `tracing::warn!`.
//!
//! Everything is inert until [`start`] runs (tests use scoped sinks).

use std::io::Write as _;
use std::path::{Path, PathBuf};
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[path = "memory_trace_wait.rs"]
mod memory_trace_wait;
use memory_trace_wait::wait_full_interval;

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
#[path = "memory_trace_signal_topology_tests.rs"]
mod memory_trace_signal_topology_tests;

/// Test-only: `pthread_t` of the live `echo-agent-memtrace` sampler, published when
/// the thread starts so the isolated SIGCHLD topology test can `pthread_kill`
/// that waiter (errno is thread-local; process-wide `kill` can miss it).
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
static SAMPLER_PTHREAD: AtomicUsize = AtomicUsize::new(0);

/// Test-only accessor for [`SAMPLER_PTHREAD`]. `None` until the sampler thread
/// has entered its entry point after [`start`].
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(super) fn test_only_sampler_pthread() -> Option<usize> {
    let value = SAMPLER_PTHREAD.load(Ordering::SeqCst);
    (value != 0).then_some(value)
}

pub use echo_agent_tty_utils::{ProcessResources, sample_process_memory};

/// Allocator gauges sampled from jemalloc (`stats.*` mallctls). All bytes.
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct AllocatorStats {
    /// Bytes in live application allocations.
    pub allocated: u64,
    /// Bytes in active pages (allocated + internal fragmentation).
    pub active: u64,
    /// Bytes in physically resident data pages mapped by the allocator.
    pub resident: u64,
    /// Bytes in active extents mapped by the allocator.
    pub mapped: u64,
    /// Bytes in virtual memory mappings retained rather than returned.
    pub retained: u64,
    /// Allocator metadata bytes.
    pub metadata: u64,
}

/// One JSONL trace line. `kind` discriminates which optional fields apply.
#[derive(serde::Serialize)]
struct TraceEvent<'a> {
    /// Unix millis.
    ts_ms: u64,
    /// "start" | "sample" | "purge" | "threshold" | "crash".
    kind: &'a str,
    /// macOS `phys_footprint` (resident dirty + compressed + swapped); the
    /// number that matches Activity Monitor "Memory" and `vmmap`'s
    /// "Physical footprint". `None` where unavailable (Linux).
    #[serde(skip_serializing_if = "Option::is_none")]
    footprint_bytes: Option<u64>,
    /// Resident set size.
    #[serde(skip_serializing_if = "Option::is_none")]
    rss_bytes: Option<u64>,
    /// Live OS thread count, recorded for offline analysis (the threshold
    /// buckets key on footprint only). A count scaling with work done
    /// (background tasks, subagents) instead of holding a flat baseline is
    /// a leak.
    #[serde(skip_serializing_if = "Option::is_none")]
    threads: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    alloc: Option<AllocatorStats>,
    /// Purge: which memory cliff triggered it (call-site tag).
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'a str>,
    /// Purge: whether an allocator release hook was installed (a purge
    /// without a hook is a no-op — evidence the binary is misconfigured).
    #[serde(skip_serializing_if = "Option::is_none")]
    hook_installed: Option<bool>,
    /// Purge: the memory gauge before the purge — physical footprint where
    /// available (macOS), else RSS (Linux). Pair with the event's
    /// `footprint_bytes`/`rss_bytes` (same precedence, sampled after) for
    /// the released delta.
    #[serde(skip_serializing_if = "Option::is_none")]
    gauge_before_bytes: Option<u64>,
    /// Purge duration in microseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    purge_us: Option<u64>,
    /// Threshold: the bucket that fired.
    #[serde(skip_serializing_if = "Option::is_none")]
    threshold_bytes: Option<u64>,
    /// Threshold: relative path of the allocator dump written next to the
    /// trace, if a dump provider is installed.
    #[serde(skip_serializing_if = "Option::is_none")]
    dump_file: Option<&'a str>,
    /// Start: pid + binary version, for joining traces to sessions/hosts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<&'a str>,
}

// ─── Seams (installed by the composition-root binary) ────────────────────

static STATS_PROVIDER: OnceLock<fn() -> Option<AllocatorStats>> = OnceLock::new();
static DUMP_PROVIDER: OnceLock<fn() -> String> = OnceLock::new();
static THRESHOLD_HOOK: OnceLock<fn(&Path, u64)> = OnceLock::new();

/// Install the allocator gauge provider (jemalloc `stats.*` reads).
/// Idempotent; first caller wins.
pub fn install_allocator_stats_provider(provider: fn() -> Option<AllocatorStats>) {
    let _ = STATS_PROVIDER.set(provider);
}

/// Install the full allocator dump provider (`malloc_stats_print` text),
/// invoked only on threshold crossings. Idempotent; first caller wins.
pub fn install_allocator_dump_provider(provider: fn() -> String) {
    let _ = DUMP_PROVIDER.set(provider);
}

/// Install the threshold hook: `(jsonl_trace_path, crossed_threshold_bytes)`.
/// This is the attachment point for the GCS trace-upload pipeline: it fires
/// at most once per bucket per growth cycle (buckets re-arm after the
/// footprint halves). Idempotent; first caller wins.
pub fn install_threshold_hook(hook: fn(&Path, u64)) {
    let _ = THRESHOLD_HOOK.set(hook);
}

// ─── Threshold state (pure; unit-tested) ──────────────────────────────────

/// Exactly-once-per-growth-cycle threshold buckets. A bucket fires when the
/// footprint reaches it while armed, then stays disarmed until the footprint
/// drops below half the bucket (hysteresis, so purge/regrow cycles near a
/// boundary can't spam).
struct Thresholds {
    buckets: Vec<u64>,
    armed: Vec<bool>,
}

impl Thresholds {
    fn new(first_bytes: u64, count: usize) -> Self {
        let mut buckets = Vec::with_capacity(count);
        let mut b = first_bytes.max(64 << 20); // floor: 64 MiB
        for _ in 0..count {
            buckets.push(b);
            b = b.saturating_mul(2);
        }
        let armed = vec![true; buckets.len()];
        Self { buckets, armed }
    }

    /// Feed a footprint observation; returns the buckets that fire on it.
    fn observe(&mut self, footprint: u64) -> Vec<u64> {
        let mut fired = Vec::new();
        for (i, &bucket) in self.buckets.iter().enumerate() {
            if self.armed[i] && footprint >= bucket {
                self.armed[i] = false;
                fired.push(bucket);
            } else if !self.armed[i] && footprint < bucket / 2 {
                self.armed[i] = true;
            }
        }
        fired
    }
}

// ─── Sink ──────────────────────────────────────────────────────────────────

const ROTATE_BYTES_DEFAULT: u64 = 4 << 20; // 4 MiB, then one .1 rotation.
static DUMP_SEQ: AtomicU64 = AtomicU64::new(0);

struct Sink {
    /// Lazily-created JSONL file; `None` until the first event.
    file: Mutex<Option<std::fs::File>>,
    path: PathBuf,
    bytes_written: AtomicU64,
    rotate_bytes: u64,
    thresholds: Mutex<Thresholds>,
}

/// Process-global sink. `RwLock` (not `OnceLock`) so tests can install
/// scoped sinks; production installs exactly once via [`start`].
static SINK: RwLock<Option<std::sync::Arc<Sink>>> = RwLock::new(None);

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Sink {
    fn new(path: PathBuf, rotate_bytes: u64, first_threshold: u64) -> Self {
        Self {
            file: Mutex::new(None),
            path,
            bytes_written: AtomicU64::new(0),
            rotate_bytes,
            thresholds: Mutex::new(Thresholds::new(first_threshold, 6)),
        }
    }

    fn write_line(&self, line: &str) {
        let mut guard = match self.file.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if guard.is_none() {
            if let Some(parent) = self.path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
            {
                Ok(f) => *guard = Some(f),
                Err(_) => return, // Tracing must never break the pager.
            }
        }
        if let Some(f) = guard.as_mut()
            && f.write_all(line.as_bytes()).is_ok()
        {
            let _ = f.write_all(b"\n");
            let total = self
                .bytes_written
                .fetch_add(line.len() as u64 + 1, Ordering::Relaxed);
            if total > self.rotate_bytes {
                // Rotate: current → .1 (replacing any previous .1), and
                // reopen the live file eagerly so a reader between
                // events never observes a missing trace.
                let mut rotated = self.path.clone().into_os_string();
                rotated.push(".1");
                let _ = std::fs::rename(&self.path, PathBuf::from(rotated));
                self.bytes_written.store(0, Ordering::Relaxed);
                *guard = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.path)
                    .ok();
            }
        }
    }

    fn emit(&self, event: &TraceEvent<'_>) {
        if let Ok(line) = serde_json::to_string(event) {
            self.write_line(&line);
        }
    }

    /// Record gauges + run threshold logic. Shared by samples and purges.
    fn record(&self, kind: &str, purge: Option<PurgeInfo<'_>>) {
        let mem = sample_process_memory();
        let alloc = STATS_PROVIDER.get().and_then(|p| p());
        let (reason, hook_installed, before, purge_us) = match purge {
            Some(p) => (
                Some(p.reason),
                Some(p.hook_installed),
                p.gauge_before,
                Some(p.duration.as_micros() as u64),
            ),
            None => (None, None, None, None),
        };
        self.emit(&TraceEvent {
            ts_ms: now_ms(),
            kind,
            footprint_bytes: mem.footprint_bytes,
            rss_bytes: mem.rss_bytes,
            threads: mem.threads,
            alloc,
            reason,
            hook_installed,
            gauge_before_bytes: before,
            purge_us,
            threshold_bytes: None,
            dump_file: None,
            pid: None,
            version: None,
        });
        // Threshold gauge: physical footprint where available (macOS);
        // otherwise RSS (Linux) — without the fallback, Linux thresholds
        // would never fire.
        if let Some(gauge) = mem.footprint_bytes.or(mem.rss_bytes) {
            let fired = match self.thresholds.lock() {
                Ok(mut t) => t.observe(gauge),
                Err(p) => p.into_inner().observe(gauge),
            };
            for bucket in fired {
                self.fire_threshold(bucket, gauge);
            }
        }
    }

    fn fire_threshold(&self, bucket: u64, footprint: u64) {
        // Full allocator dump next to the trace (rare: once per bucket per
        // growth cycle), so the upload has arena-level detail to analyze.
        let dump_rel = DUMP_PROVIDER.get().map(|dump| {
            let seq = DUMP_SEQ.fetch_add(1, Ordering::Relaxed);
            let stem = self
                .path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "memtrace".to_owned());
            let name = format!("{stem}-jemalloc-{seq}.txt");
            let dump_path = self.path.with_file_name(&name);
            let _ = std::fs::write(&dump_path, dump());
            name
        });
        self.emit(&TraceEvent {
            ts_ms: now_ms(),
            kind: "threshold",
            footprint_bytes: Some(footprint),
            rss_bytes: None,
            threads: None,
            alloc: None,
            reason: None,
            hook_installed: None,
            gauge_before_bytes: None,
            purge_us: None,
            threshold_bytes: Some(bucket),
            dump_file: dump_rel.as_deref(),
            pid: None,
            version: None,
        });
        tracing::warn!(
            target: "memtrace",
            footprint_bytes = footprint,
            threshold_bytes = bucket,
            trace = %self.path.display(),
            "process footprint crossed memory threshold"
        );
        if let Some(hook) = THRESHOLD_HOOK.get() {
            hook(&self.path, bucket);
        }
    }
}

struct PurgeInfo<'a> {
    reason: &'a str,
    hook_installed: bool,
    gauge_before: Option<u64>,
    duration: Duration,
}

fn with_sink(f: impl FnOnce(&Sink)) {
    let guard = match SINK.read() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    if let Some(sink) = guard.as_ref() {
        f(sink);
    }
}

/// Whether a trace sink is installed ([`start`] ran and `ECHO_AGENT_MEMTRACE` is
/// not disabled, or a test sink is scoped in). Lets callers skip gauge
/// sampling entirely when tracing is off.
pub(crate) fn is_active() -> bool {
    match SINK.read() {
        Ok(g) => g.is_some(),
        Err(p) => p.into_inner().is_some(),
    }
}

/// Record a completed purge, attributed to its memory cliff. Called by
/// `memory_release`; no-op until [`start`].
pub(crate) fn record_purge(
    reason: &'static str,
    hook_installed: bool,
    gauge_before: Option<u64>,
    duration: Duration,
) {
    with_sink(|s| {
        s.record(
            "purge",
            Some(PurgeInfo {
                reason,
                hook_installed,
                gauge_before,
                duration,
            }),
        );
    });
}

// ─── Startup ───────────────────────────────────────────────────────────────

/// Env: disable with `ECHO_AGENT_MEMTRACE=0|false|off`.
fn enabled_by_env() -> bool {
    !matches!(
        std::env::var("ECHO_AGENT_MEMTRACE").ok().as_deref(),
        Some("0") | Some("false") | Some("off")
    )
}

fn interval_from_env() -> Duration {
    let secs = std::env::var("ECHO_AGENT_MEMTRACE_INTERVAL_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(30);
    // Production floors at 5s so accidental tiny values don't spam samples.
    // Test builds allow 1s so isolated regressions can finish quickly.
    let min_secs = if cfg!(test) { 1 } else { 5 };
    Duration::from_secs(secs.max(min_secs))
}

fn first_threshold_from_env() -> u64 {
    std::env::var("ECHO_AGENT_MEMTRACE_THRESHOLD_MB")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(1024)
        .saturating_mul(1 << 20)
}

/// Start memory tracing: install the process-global sink under
/// `dir` (e.g. `$ECHO_AGENT_HOME/memtrace/`) and spawn the detached sampler
/// thread. Call once from the composition-root binary, AFTER the
/// short-lived-child intercepts (mermaid render worker) so helper processes
/// don't trace. Inert when `ECHO_AGENT_MEMTRACE=0`.
///
/// The trace file is created lazily on the first event; the first sample is
/// taken after one full interval, so short-lived CLI invocations
/// (`echoagent --version`, `echoagent trace …`) leave no files behind.
pub fn start(dir: PathBuf) {
    if !enabled_by_env() {
        return;
    }
    {
        let mut guard = match SINK.write() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if guard.is_some() {
            return; // Already started.
        }
        let start_ts = now_ms() / 1000;
        let path = dir.join(format!("{start_ts}-{}.jsonl", std::process::id()));
        *guard = Some(std::sync::Arc::new(Sink::new(
            path,
            ROTATE_BYTES_DEFAULT,
            first_threshold_from_env(),
        )));
    }
    let interval = interval_from_env();
    // Detached sampler: holds no locks across waits, and the JoinHandle is
    // dropped so nothing else can unpark this thread (see memory_trace_wait).
    // Named for `sample`/Instruments visibility; dies with the process.
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    SAMPLER_PTHREAD.store(0, Ordering::SeqCst);
    let _ = std::thread::Builder::new()
        .name("echo-agent-memtrace".into())
        .spawn(move || {
            #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
            {
                // SAFETY: pthread_self returns this thread's valid pthread_t.
                SAMPLER_PTHREAD.store(unsafe { libc::pthread_self() as usize }, Ordering::SeqCst);
            }
            let mut wrote_start = false;
            loop {
                wait_full_interval(interval);
                with_sink(|s| {
                    if !wrote_start {
                        wrote_start = true;
                        s.emit(&TraceEvent {
                            ts_ms: now_ms(),
                            kind: "start",
                            footprint_bytes: None,
                            rss_bytes: None,
                            threads: None,
                            alloc: None,
                            reason: None,
                            hook_installed: None,
                            gauge_before_bytes: None,
                            purge_us: None,
                            threshold_bytes: None,
                            dump_file: None,
                            pid: Some(std::process::id()),
                            version: Some(env!("CARGO_PKG_VERSION")),
                        });
                    }
                    s.record("sample", None);
                });
            }
        });
}

/// Appends without taking the sink's write lock, so a panic raised while that
/// lock is held cannot hang the hook before the process reports the panic.
pub fn record_crash_sample() {
    let Ok(sink) = SINK.try_read() else {
        return;
    };
    let Some(path) = sink.as_ref().map(|s| s.path.clone()) else {
        return;
    };
    drop(sink);

    let mem = sample_process_memory();
    let Ok(line) = serde_json::to_string(&TraceEvent {
        ts_ms: now_ms(),
        kind: "crash",
        footprint_bytes: mem.footprint_bytes,
        rss_bytes: mem.rss_bytes,
        threads: mem.threads,
        alloc: STATS_PROVIDER.get().and_then(|p| p()),
        reason: None,
        hook_installed: None,
        gauge_before_bytes: None,
        purge_us: None,
        threshold_bytes: None,
        dump_file: None,
        pid: None,
        version: None,
    }) else {
        return;
    };

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = file.write_all(format!("{line}\n").as_bytes());
    }
}

pub fn default_dir() -> PathBuf {
    echo_agent_runtime::util::echo_agent_home::echo_agent_home().join("memtrace")
}

#[derive(Debug, PartialEq, Eq)]
pub struct ExportedTrace {
    pub name: String,
    pub data: Vec<u8>,
}

pub struct ExportLimits {
    pub max_files: usize,
    pub max_total_bytes: u64,
}

impl Default for ExportLimits {
    fn default() -> Self {
        Self {
            max_files: 32,
            max_total_bytes: 16 << 20,
        }
    }
}

struct Candidate {
    stem: String,
    name: String,
    path: PathBuf,
    len: u64,
    modified: SystemTime,
    is_dump: bool,
    dump_seq: u64,
}

fn parse_trace_name(name: &str) -> Option<(String, bool, u64)> {
    let (stem, is_dump, dump_seq) = match name.rsplit_once("-jemalloc-") {
        Some((stem, seq)) => (stem, true, seq.strip_suffix(".txt")?.parse().ok()?),
        None => {
            let stem = name
                .strip_suffix(".jsonl")
                .or_else(|| name.strip_suffix(".jsonl.1"))?;
            (stem, false, 0)
        }
    };
    let (start_ts, pid) = stem.split_once('-')?;
    start_ts.parse::<u64>().ok()?;
    pid.parse::<u32>().ok()?;
    Some((stem.to_owned(), is_dump, dump_seq))
}

pub fn collect_for_export(dir: &Path, limits: ExportLimits) -> Vec<ExportedTrace> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut candidates: Vec<Candidate> = entries
        .flatten()
        .filter_map(|entry| {
            if !entry.file_type().ok()?.is_file() {
                return None;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let (stem, is_dump, dump_seq) = parse_trace_name(&name)?;
            let meta = entry.metadata().ok()?;
            Some(Candidate {
                stem,
                name,
                path: entry.path(),
                len: meta.len(),
                modified: meta.modified().unwrap_or(UNIX_EPOCH),
                is_dump,
                dump_seq,
            })
        })
        .collect();

    let mut last_activity: std::collections::HashMap<String, SystemTime> =
        std::collections::HashMap::new();
    for candidate in &candidates {
        let seen = last_activity
            .entry(candidate.stem.clone())
            .or_insert(candidate.modified);
        *seen = (*seen).max(candidate.modified);
    }

    candidates.sort_by_cached_key(|c| {
        (
            std::cmp::Reverse(last_activity.get(&c.stem).copied().unwrap_or(c.modified)),
            c.stem.clone(),
            c.is_dump,
            std::cmp::Reverse(c.dump_seq),
            c.name.clone(),
        )
    });

    let mut exported = Vec::new();
    let mut total_bytes: u64 = 0;
    for candidate in candidates {
        if exported.len() >= limits.max_files {
            break;
        }
        if total_bytes.saturating_add(candidate.len) > limits.max_total_bytes {
            continue;
        }
        let Ok(data) = std::fs::read(&candidate.path) else {
            continue;
        };
        total_bytes = total_bytes.saturating_add(data.len() as u64);
        exported.push(ExportedTrace {
            name: candidate.name,
            data,
        });
    }
    exported
}

// ─── Test support ──────────────────────────────────────────────────────────

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// Install a scoped sink writing to `path` (tiny rotation cap, high
    /// thresholds). Returns a guard restoring the previous sink on drop.
    /// Tests using this must serialize on the `MEMTRACE_SINK` serial key —
    /// the sink is process-global.
    pub(crate) struct SinkGuard(Option<std::sync::Arc<Sink>>);

    impl Drop for SinkGuard {
        fn drop(&mut self) {
            let mut guard = match SINK.write() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            *guard = self.0.take();
        }
    }

    pub(crate) fn install_test_sink(path: PathBuf, rotate_bytes: u64) -> SinkGuard {
        let mut guard = match SINK.write() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let prev = guard.take();
        *guard = Some(std::sync::Arc::new(Sink::new(
            path,
            rotate_bytes,
            u64::MAX >> 1, // never fire thresholds unless a test asks
        )));
        SinkGuard(prev)
    }

    pub(crate) fn record_sample_for_tests() {
        with_sink(|s| s.record("sample", None));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thresholds_fire_once_and_rearm_after_halving() {
        let mut t = Thresholds::new(1 << 30, 3); // 1 GiB, 2 GiB, 4 GiB
        assert!(t.observe(512 << 20).is_empty(), "below first bucket");
        assert_eq!(t.observe(1 << 30), vec![1 << 30], "first crossing fires");
        assert!(
            t.observe(1200 << 20).is_empty(),
            "staying above must not re-fire"
        );
        assert_eq!(
            t.observe(5 << 30),
            vec![2 << 30, 4 << 30],
            "one observation can cross several buckets"
        );
        // Drop below half of 1 GiB → re-arms only that bucket.
        assert!(t.observe(400 << 20).is_empty());
        assert_eq!(
            t.observe(1 << 30),
            vec![1 << 30],
            "re-armed bucket fires again on the next growth cycle"
        );
    }

    #[test]
    fn thresholds_floor_prevents_degenerate_buckets() {
        let mut t = Thresholds::new(0, 2);
        assert!(
            t.observe(1 << 20).is_empty(),
            "buckets are floored at 64 MiB, tiny footprints never fire"
        );
        assert_eq!(t.observe(64 << 20), vec![64 << 20]);
    }

    #[test]
    #[serial_test::serial(MEMTRACE_SINK)]
    fn sample_events_are_valid_jsonl_and_rotate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let _guard = test_support::install_test_sink(path.clone(), 256);

        // Enough samples to exceed the 256-byte cap at least twice; the
        // post-rotation file is created lazily by the NEXT write, so the
        // final sample guarantees both files exist.
        for _ in 0..16 {
            test_support::record_sample_for_tests();
        }
        // Rotation happened at the tiny cap: a `.1` exists and both files
        // hold only valid JSON lines with the expected shape.
        let rotated = dir.path().join("t.jsonl.1");
        assert!(rotated.exists(), "rotation must produce a .1 file");
        assert!(path.exists(), "rotation reopens the live file eagerly");
        for p in [&path, &rotated] {
            let body = std::fs::read_to_string(p).unwrap();
            for line in body.lines() {
                let v: serde_json::Value = serde_json::from_str(line).expect("valid JSON line");
                assert_eq!(v["kind"], "sample");
                assert!(v["ts_ms"].as_u64().unwrap() > 0);
            }
        }
    }

    #[test]
    #[serial_test::serial(MEMTRACE_SINK)]
    fn purge_events_carry_cliff_attribution() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.jsonl");
        let _guard = test_support::install_test_sink(path.clone(), 1 << 20);

        crate::memory_release::release_retained_memory("unit-test-cliff");

        let body = std::fs::read_to_string(&path).unwrap();
        let purge_line = body
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .find(|v| v["kind"] == "purge" && v["reason"] == "unit-test-cliff")
            .expect("a purge event tagged with the calling cliff");
        assert!(purge_line["purge_us"].as_u64().is_some());
        assert!(purge_line["hook_installed"].as_bool().is_some());
        // The before-gauge must exist on every supported platform (footprint
        // on macOS, RSS fallback on Linux) or purge deltas are uncomputable.
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        assert!(
            purge_line["gauge_before_bytes"].as_u64().unwrap_or(0) > 0,
            "purge events must carry a before-gauge for delta analysis"
        );
    }

    fn write_aged(dir: &Path, name: &str, len: usize, age_secs: u64) {
        let path = dir.join(name);
        std::fs::write(&path, vec![b'x'; len]).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(age_secs))
            .unwrap();
    }

    fn names(exported: &[ExportedTrace]) -> Vec<&str> {
        exported.iter().map(|t| t.name.as_str()).collect()
    }

    #[test]
    #[serial_test::serial(MEMTRACE_SINK)]
    fn crash_sample_lands_even_when_the_trace_directory_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent").join("t.jsonl");
        let _guard = test_support::install_test_sink(path.clone(), 1 << 20);

        record_crash_sample();

        let body = std::fs::read_to_string(&path).unwrap();
        let event: serde_json::Value = serde_json::from_str(body.trim()).unwrap();
        assert_eq!(event["kind"], "crash");
    }

    #[test]
    fn export_groups_each_process_and_leads_with_its_timeline() {
        let dir = tempfile::tempdir().unwrap();
        write_aged(dir.path(), "100-1.jsonl", 8, 60);
        write_aged(dir.path(), "100-1-jemalloc-2.txt", 8, 300);
        write_aged(dir.path(), "100-1-jemalloc-10.txt", 8, 120);
        write_aged(dir.path(), "100-2.jsonl", 8, 5);
        write_aged(dir.path(), "100-2-jemalloc-0.txt", 8, 30);

        let exported = collect_for_export(dir.path(), ExportLimits::default());

        assert_eq!(
            names(&exported),
            [
                "100-2.jsonl",
                "100-2-jemalloc-0.txt",
                "100-1.jsonl",
                "100-1-jemalloc-10.txt",
                "100-1-jemalloc-2.txt"
            ]
        );
    }

    #[test]
    #[cfg(unix)]
    fn export_ignores_files_it_did_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = dir.path().join("credentials");
        std::fs::write(&elsewhere, "secret").unwrap();
        write_aged(dir.path(), "100-1.jsonl", 8, 0);
        std::fs::write(dir.path().join("notes.txt"), "x").unwrap();
        std::os::unix::fs::symlink(&elsewhere, dir.path().join("200-2.jsonl")).unwrap();

        let exported = collect_for_export(dir.path(), ExportLimits::default());

        assert_eq!(names(&exported), ["100-1.jsonl"]);
    }

    #[test]
    fn export_skips_an_over_budget_trace_and_keeps_reading() {
        let dir = tempfile::tempdir().unwrap();
        write_aged(dir.path(), "200-1.jsonl", 128, 0);
        write_aged(dir.path(), "100-1.jsonl", 32, 60);

        let exported = collect_for_export(
            dir.path(),
            ExportLimits {
                max_files: usize::MAX,
                max_total_bytes: 64,
            },
        );

        assert_eq!(
            exported,
            vec![ExportedTrace {
                name: "100-1.jsonl".into(),
                data: vec![b'x'; 32],
            }]
        );
    }
}
