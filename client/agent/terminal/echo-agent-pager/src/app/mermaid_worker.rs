//! Off-draw-thread Mermaid render worker, per-session disk cache, and the
//! [`AgentView`] lazy render-on-click glue (both `[Open]` and `[Copy path]`).
//!
//! Mirrors the existing inline-video worker model (`std::thread::spawn` +
//! `std::sync::mpsc`, polled each tick via `try_recv`) rather than tokio. A
//! single worker thread renders a requested diagram (in a short-lived child
//! process — see below), writes the PNG to the session's `mermaid/` dir, and
//! reports back only the on-disk path. Diagrams are never displayed inline; the
//! path is what the affordance row's `[Open]`/`[Copy path]` actions target.
//!
//! Rendering is **lazy**: nothing renders until the user clicks `[Open]` or
//! `[Copy path]`, which renders the diagram at the *live* theme/width (so it
//! always matches the current theme, including auto day/night) and then runs the
//! requested action. A small lock-free [`PendingMermaidAction`] list records what
//! the user asked for so the tick can complete it when the matching render result
//! arrives — mirroring how inline video loads via `mpsc` + poll.
//!
//! # Crash isolation under `panic = "abort"` — out of process
//!
//! The shipped CLI profiles build with `panic = "abort"`, so the `catch_unwind`
//! inside [`echo_agent_mermaid::render_checked`] is a no-op there: a panic in the
//! layout engine over untrusted model output would abort the whole pager, and a
//! synchronous in-process render could not be killed on timeout. The render
//! therefore runs **out of process**, in a short-lived child:
//!
//! 1. The pager re-execs itself as `echo-agent-pager __mermaid-render` (see
//!    [`maybe_run_render_subprocess`], intercepted at the very top of `main`
//!    before any TUI/agent/runtime init). The child reads the source from stdin
//!    and the theme/width/height from argv, renders source → SVG → PNG, writes
//!    the PNG atomically to the out-path, and exits 0; any error exits non-zero.
//! 2. The worker spawns that child with a wall-clock budget ([`RENDER_TIMEOUT`])
//!    via [`echo_agent_mermaid::run_with_timeout`], which **kills and reaps** the
//!    child (real process kill, not a soft signal) on timeout. A child panic
//!    (abort), non-zero exit, or timeout is contained to the child and surfaces
//!    as `Failed` → the existing code-block fallback; the pager survives.
//! 3. The child applies the same caps as in-process would: the source-size limit
//!    ([`echo_agent_mermaid::RenderLimits`]) and the raster megapixel/height caps
//!    (see `echo-agent-mermaid`). The parent also rejects obviously-oversized
//!    source before spawning, to avoid launching a doomed child.
//!
//! Because untrusted source now renders fully isolated, the feature is on by
//! default (no cargo gate); the `appearance.render_mermaid` setting still gates
//! it at runtime.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use echo_agent_mermaid::{
    MermaidTheme, RenderLimits, RenderParams, RenderedDiagram, SubprocessError, default_engine,
    render_checked, run_with_timeout,
};
use indexmap::IndexMap;

use crate::app::agent_view::AgentView;
use crate::scrollback::blocks::mermaid_content::{
    MermaidCacheKey, MermaidRenderQuality, theme_is_dark,
};
use crate::theme::ThemeKind;

/// Hidden subcommand the pager re-execs itself with to render one diagram in an
/// isolated child process. Intercepted by [`maybe_run_render_subprocess`] at the
/// very top of `main`, before any TUI/agent/tokio init.
pub const MERMAID_RENDER_SUBCOMMAND: &str = "__mermaid-render";

/// Tracing target for diagram render observability (mirrors
/// `PROMPT_IMAGES_TRACING_TARGET`). Filter with `RUST_LOG=mermaid=debug`.
pub const MERMAID_TRACING_TARGET: &str = "mermaid";

/// Per-session on-disk cap, swept once at session load. A pure cache, so the
/// oldest PNGs are dropped first when the directory grows past this.
const SESSION_DISK_CAP_BYTES: u64 = 200 * 1024 * 1024;

/// Approximate terminal cell width in pixels, used to turn a content-column
/// budget into a render pixel budget.
const APPROX_CELL_W_PX: u32 = 8;
/// HiDPI oversample so diagram text stays crisp after the terminal scales the
/// PNG down to cells.
const RENDER_SCALE: u32 = 2;
/// Clamp for the derived render width so a tiny/huge viewport still produces a
/// sensibly-sized PNG.
const MIN_TARGET_WIDTH_PX: u32 = 320;
const MAX_TARGET_WIDTH_PX: u32 = 1600;
/// Hard ceiling on render height so a tall diagram can't dominate memory; the
/// raster stage also caps total megapixels.
const MAX_TARGET_HEIGHT_PX: u32 = 2400;

/// Open-tier (OS viewer / copy path): minimum raster width so small diagrams
/// stay readable on HiDPI displays. Large SVGs use 2× intrinsic size instead
/// (see [`RenderParams::for_os_viewer`]).
const OPEN_MIN_WIDTH_PX: u32 = 2560;
/// Open-tier height headroom before the crate-wide megapixel/axis caps apply.
const OPEN_MAX_HEIGHT_PX: u32 = 8192;

/// Wall-clock budget per render. Enforced against the out-of-process child: on
/// timeout the worker kills and reaps the child (a real process kill) and
/// surfaces `Failed`, so one pathological diagram neither stalls the worker nor
/// leaks work.
const RENDER_TIMEOUT: Duration = Duration::from_millis(3000);

/// Re-sweep the per-session on-disk cache after this many fresh PNG writes, so a
/// long session that keeps generating diagrams stays bounded between loads
/// (complements the at-load sweep). Runs on the worker thread, off the draw path.
const SWEEP_EVERY_N_WRITES: u32 = 8;

/// What a finished on-click render should do with the rendered PNG. `[Copy
/// source]` needs no render, so it is not represented here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MermaidClickAction {
    /// Open the rendered PNG in the OS default app.
    Open,
    /// Copy the rendered PNG's filesystem path to the clipboard.
    CopyPath,
}

impl MermaidClickAction {
    fn log_label(self) -> &'static str {
        match self {
            Self::Open => "open_image",
            Self::CopyPath => "copy_image_path",
        }
    }
}

/// An on-click render the user requested whose worker result hasn't arrived yet.
///
/// Recorded when a click misses the disk cache and dispatches a render; the tick
/// completes the `action` when a result for `key` arrives (mirrors the inline
/// video `mpsc` + poll pattern). The painter also consults the keys to show the
/// transient `rendering…` hint on the matching diagram's row.
struct PendingMermaidAction {
    /// Cache key of the in-flight render (matched against the worker result).
    key: MermaidCacheKey,
    /// What to do once the render lands.
    action: MermaidClickAction,
}

/// A unit of work for the render worker.
///
/// Carries owned data so the worker thread is self-contained; the pager-local
/// render parameters (`theme_dark`, `target_width_px`) are translated to
/// [`RenderParams`] inside the worker, keeping the channel types independent of
/// the engine crate's surface. The worker coalesces queued jobs by [`key`], so a
/// burst of identical requests renders once.
///
/// [`key`]: MermaidJob::key
pub struct MermaidJob {
    /// Cache key being rendered (the coalescing key, echoed back on the result
    /// so the tick can match it to the pending action that requested it).
    pub key: MermaidCacheKey,
    /// The diagram source (owned; moved into the worker).
    pub source: String,
    /// Per-session destination PNG path.
    pub out_path: PathBuf,
    /// Whether to render for a dark surface.
    pub theme_dark: bool,
    /// Target raster width in pixels (terminal tier). Ignored for
    /// [`MermaidRenderQuality::Open`] (auto-scale from SVG + min width).
    pub target_width_px: u32,
    /// Terminal-budget vs OS-viewer quality tier (also part of the cache key).
    pub quality: MermaidRenderQuality,
}

/// The result of one render attempt.
pub enum MermaidOutcome {
    /// Render (or disk-cache load) succeeded; the diagram PNG is on disk at
    /// `path`, ready for the requesting `[Open]`/`[Copy path]` action.
    Ready {
        /// On-disk PNG path.
        path: PathBuf,
    },
    /// Render failed (parse/layout/raster/panic/oversize/timeout); the requested
    /// action surfaces an error toast.
    Failed,
}

/// A render result returned over the worker channel.
pub struct MermaidResult {
    /// The key that was rendered (matched against the pending action's key so the
    /// tick runs the right action).
    pub key: MermaidCacheKey,
    /// Render outcome.
    pub outcome: MermaidOutcome,
}

/// How the worker turns one job's source into an on-disk PNG at `job.out_path`.
///
/// Returns `Ok(())` when a fresh, decodable PNG was written; `Err(reason)` with a
/// source-free category otherwise. In production this is the out-of-process
/// child ([`render_via_subprocess`]); the worker's own unit tests swap in an
/// in-process renderer (a child re-exec can't work under the test harness
/// binary), so the cargo-`test` build resolves [`default_render_fn`] to that.
type RenderFn = dyn Fn(&MermaidJob, Duration) -> Result<(), &'static str> + Send + Sync;

/// The render function the worker uses: out-of-process in production, in-process
/// under the crate's own `cargo test` (the test binary is not the pager and so
/// cannot re-exec the `__mermaid-render` subcommand — the real subprocess path is
/// covered end to end by the integration test against the built binary).
fn default_render_fn() -> Arc<RenderFn> {
    #[cfg(test)]
    {
        Arc::new(render_in_process_for_tests)
    }
    #[cfg(not(test))]
    {
        match std::env::current_exe() {
            Ok(exe) => Arc::new(move |job: &MermaidJob, timeout: Duration| {
                render_via_subprocess(
                    &exe,
                    &job.source,
                    job.theme_dark,
                    job.target_width_px,
                    job.quality,
                    &job.out_path,
                    timeout,
                )
            }),
            Err(e) => {
                tracing::warn!(target: MERMAID_TRACING_TARGET, error = %e, "current_exe() failed; mermaid rendering disabled");
                Arc::new(|_: &MermaidJob, _: Duration| Err("no_exe"))
            }
        }
    }
}

/// Spawn the single render worker thread and return its job/result channels.
///
/// One thread (not a pool) keeps the receiver lock-free, matching the
/// "avoid locks unless necessary" guidance; the worker coalesces any queued
/// burst by [`MermaidCacheKey`] (latest wins) before rendering, so duplicate
/// requests for the same diagram never pile up. Each render runs in a short-lived
/// child process (out-of-process crash isolation; see the module docs).
pub fn spawn_worker() -> (Sender<MermaidJob>, Receiver<MermaidResult>) {
    let (job_tx, job_rx) = std::sync::mpsc::channel::<MermaidJob>();
    let (result_tx, result_rx) = std::sync::mpsc::channel::<MermaidResult>();

    let render = default_render_fn();
    std::thread::Builder::new()
        .name("mermaid-render".to_string())
        .spawn(move || {
            // Fresh PNG writes since the last incremental sweep, per session dir.
            let mut writes_since_sweep: u32 = 0;
            while let Ok(first) = job_rx.recv() {
                // Coalesce the whole queued burst by cache key so duplicate
                // requests for the same diagram render once.
                let pending = drain_coalesced(first, &job_rx);
                for (_, job) in pending {
                    let (outcome, wrote) = render_job(render.as_ref(), &job, RENDER_TIMEOUT);
                    if wrote {
                        writes_since_sweep += 1;
                        if writes_since_sweep >= SWEEP_EVERY_N_WRITES {
                            writes_since_sweep = 0;
                            if let Some(dir) = job.out_path.parent() {
                                // Off the draw path (worker thread). Keeps the
                                // session's mermaid/ dir bounded within a session.
                                sweep_session_cache(dir, SESSION_DISK_CAP_BYTES);
                            }
                        }
                    }
                    let result = MermaidResult {
                        key: job.key,
                        outcome,
                    };
                    if result_tx.send(result).is_err() {
                        return; // Receiver dropped — the view is gone.
                    }
                }
            }
        })
        .expect("spawn mermaid-render thread");

    (job_tx, result_rx)
}

/// Coalesce `first` plus every job already queued on `rx` into a
/// per-[`MermaidCacheKey`] map where the latest job for each key wins. `IndexMap`
/// keeps FIFO order across distinct keys, so independent diagrams still render in
/// order.
fn drain_coalesced(
    first: MermaidJob,
    rx: &Receiver<MermaidJob>,
) -> IndexMap<MermaidCacheKey, MermaidJob> {
    let mut pending: IndexMap<MermaidCacheKey, MermaidJob> = IndexMap::new();
    pending.insert(first.key.clone(), first);
    while let Ok(job) = rx.try_recv() {
        pending.insert(job.key.clone(), job);
    }
    pending
}

/// Whether `path` holds a valid cached diagram PNG (a disk-cache hit).
///
/// Refuses a symlinked final component (a model-predictable per-session path
/// shouldn't be followed through a symlink) and requires the file to actually
/// decode as an image, so a short/corrupt file is treated as a miss and
/// re-rendered, mirroring prompt-image handling.
fn read_cached_png(path: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if meta.file_type().is_symlink() {
        return false;
    }
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    crate::prompt_images::decode_image_dimensions(&bytes).is_some()
}

/// Render one job's source to its `out_path` PNG in a short-lived child process.
///
/// Re-execs `exe` (the running pager) as `exe __mermaid-render …`, passing the
/// theme/quality/width and the wall-clock deadline on argv and the source on
/// stdin, with a wall-clock budget. On timeout the child (and its group) is
/// killed and reaped — a real process kill, so a panic (abort) or runaway render
/// is contained. Returns `Ok(())` only when the child exits 0 and a decodable
/// PNG is present; otherwise a source-free failure category. Shared by the
/// worker (production) and the end-to-end subprocess integration test.
pub fn render_via_subprocess(
    exe: &Path,
    source: &str,
    theme_dark: bool,
    target_width_px: u32,
    quality: MermaidRenderQuality,
    out_path: &Path,
    timeout: Duration,
) -> Result<(), &'static str> {
    let mut cmd = Command::new(exe);
    cmd.arg(MERMAID_RENDER_SUBCOMMAND)
        .arg("--out")
        .arg(out_path)
        .arg("--theme")
        .arg(if theme_dark { "dark" } else { "light" })
        .arg("--quality")
        .arg(match quality {
            MermaidRenderQuality::Terminal => "terminal",
            MermaidRenderQuality::Open => "open",
        })
        .arg("--width")
        .arg(target_width_px.to_string())
        // Forward the parent's budget so the child derives its self-watchdog
        // deadline from it (always strictly after the parent's kill) rather than
        // a fixed value that a slow render under a larger budget could trip.
        .arg("--deadline-ms")
        .arg(timeout.as_millis().to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .envs(echo_agent_tty_utils::pager_env());
    // The child runs under an `RLIMIT_AS` cap (see `cap_child_address_space`),
    // but jemalloc — the default global allocator — pre-reserves virtual address
    // space that scales with its arena count (default ~4×ncpus), which on a
    // many-core box can approach the cap at startup and abort every render. The
    // child is short-lived and allocates from one thread, so pinning it to a
    // single arena keeps the reservation well under the cap regardless of host
    // core count. Scoped to the child; the parent pager keeps default arenas.
    // `_RJEM_MALLOC_CONF` matches tikv-jemalloc-sys's `_rjem_` symbol prefix and
    // `MALLOC_CONF` its unprefixed build — both inert if unread. Linux-gated like
    // the cap; the Linux e2e lane is the runtime gate (this host is macOS).
    #[cfg(target_os = "linux")]
    {
        cmd.env("_RJEM_MALLOC_CONF", "narenas:1")
            .env("MALLOC_CONF", "narenas:1");
    }
    // setsid/console detach via the sanctioned helper (never a raw pre_exec).
    echo_agent_tty_utils::detach_std_command(&mut cmd);

    run_render_command(cmd, source.as_bytes(), out_path, timeout)
}

/// Run a fully-built render `cmd` (with `source` piped to its stdin) under
/// `timeout` and map the outcome to a source-free failure category. Split from
/// [`render_via_subprocess`] so the production parent-side path (spawn + wall-
/// clock timeout + outcome mapping) is unit-testable against a stub child,
/// without re-execing the pager binary (which the worker's `#[cfg(test)]` path
/// cannot do).
fn run_render_command(
    cmd: Command,
    source: &[u8],
    out_path: &Path,
    timeout: Duration,
) -> Result<(), &'static str> {
    map_run_result(run_with_timeout(cmd, Some(source), timeout), out_path)
}

/// Map a subprocess outcome to the worker's source-free failure category. A zero
/// exit only counts as success when a decodable PNG is actually present, so a
/// truncated/garbage file is treated as a failure rather than a false `Ready`.
fn map_run_result(
    result: Result<(), SubprocessError>,
    out_path: &Path,
) -> Result<(), &'static str> {
    match result {
        Ok(()) if read_cached_png(out_path) => Ok(()),
        Ok(()) => Err("no_output"),
        Err(SubprocessError::Timeout) => Err("timeout"),
        // Split the non-zero exit so a containment event is observable in
        // telemetry rather than masquerading as an ordinary render failure: a
        // signal-terminated child (the `RLIMIT_AS` allocation abort, a
        // panic-under-`panic=abort`, or a SIGKILL) and the child's own watchdog
        // self-destruct each get a distinct reason. All still degrade to the same
        // user-visible code-block fallback.
        Err(SubprocessError::NonZeroExit(status)) => match status.code() {
            None => Err("child_crashed"),
            Some(CHILD_WATCHDOG_EXIT_CODE) => Err("child_watchdog"),
            Some(_) => Err("child_error"),
        },
        Err(SubprocessError::Spawn(_)) => Err("spawn"),
        Err(SubprocessError::Wait(_)) => Err("wait"),
    }
}

/// If this process was re-exec'd as the hidden mermaid render child, render the
/// requested diagram and return `Some(exit_code)`; otherwise `None` (it is a
/// normal pager invocation).
///
/// Intercepted at the very top of `main`, before any TUI/agent/tokio/sentry
/// init, so the child stays minimal and a panic (abort) or runaway render is
/// contained to this short-lived process. Reads the source from stdin and the
/// theme/width/height from argv, renders source → SVG → PNG, writes the PNG
/// atomically to the out-path, and exits 0; any error exits non-zero.
pub fn maybe_run_render_subprocess() -> Option<i32> {
    let argv: Vec<std::ffi::OsString> = std::env::args_os().collect();
    if !is_render_subcommand(&argv) {
        return None;
    }
    // Skip argv[0] (binary) and argv[1] (subcommand); the rest are flags.
    Some(match render_child(argv.into_iter().skip(2)) {
        Ok(()) => 0,
        // Any failure is the caller's signal to fall back to the code block; the
        // parent's stderr is null, so there is nothing to print.
        Err(_) => 1,
    })
}

/// Whether `argv` (the full process argv, incl. argv[0]) invokes the hidden
/// render child — i.e. argv[1] is [`MERMAID_RENDER_SUBCOMMAND`]. Pure so the
/// dispatch decision is unit-testable without mutating the process's real args.
fn is_render_subcommand(argv: &[std::ffi::OsString]) -> bool {
    argv.get(1).and_then(|a| a.to_str()) == Some(MERMAID_RENDER_SUBCOMMAND)
}

/// Slack added to the parent's forwarded wall-clock budget for the render
/// child's self-watchdog. The parent's kill normally fires first; the watchdog
/// only matters when the parent died abruptly (SIGKILL / `panic = "abort"` /
/// quit) and so cannot kill the child itself. Generous enough that a healthy
/// render (well under the budget) never trips it.
const CHILD_WATCHDOG_SLACK: Duration = Duration::from_secs(3);

/// Exit code the child's self-watchdog hard-exits with when a render outlives its
/// budget. Distinct from the child's normal failure exit (1) so the parent can
/// tell a watchdog self-destruct apart from an ordinary render error
/// ([`map_run_result`]).
const CHILD_WATCHDOG_EXIT_CODE: i32 = 2;

/// The child's self-watchdog deadline for a given forwarded parent budget:
/// always strictly *after* the parent's wall-clock kill (`budget + slack`), so
/// the self-destruct only ever fires once the parent is already gone. Pure so
/// the derivation is unit-testable without arming a real watchdog thread.
fn child_watchdog_deadline(forwarded_budget: Duration) -> Duration {
    forwarded_budget + CHILD_WATCHDOG_SLACK
}

/// Upper bound on the render child's address space (Linux). Bounds a pathological
/// dagre dummy-node explosion over a crafted ≤64 KiB flowchart so it cannot spike
/// host memory inside the wall-clock window: a render that hits the cap aborts on
/// allocation failure → non-zero exit → contained, instead of growing toward
/// host OOM. Generous vs the real ceiling (a 32 MP pixmap is ~128 MB) so a
/// legitimate large diagram never trips it. The cap measures *virtual* address
/// space, which jemalloc over-reserves in proportion to its arena count, so
/// [`render_via_subprocess`] pins the child to one arena to keep this cap safe
/// regardless of host core count.
#[cfg(target_os = "linux")]
const CHILD_ADDRESS_SPACE_CAP_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Install the render child's containment backstops, arming the watchdog for the
/// `forwarded_budget` (the parent's wall-clock kill deadline). Called only from
/// [`render_child`] (itself reached only via the real subcommand entry
/// [`maybe_run_render_subprocess`]); the worker/child unit tests drive the
/// smaller seams ([`parse_render_args`], [`render_and_write`]) directly, so they
/// never arm the watchdog.
///
/// Belt-and-suspenders behind the parent's wall-clock kill:
///  * a portable watchdog thread that hard-exits if a render outlives
///    [`child_watchdog_deadline`] (so an abruptly-dead parent cannot leave a
///    CPU-spinning orphan);
///  * on Linux, an address-space cap (bounds a memory-explosion render) and a
///    parent-death signal (the child dies with the parent immediately).
fn install_child_backstops(forwarded_budget: Duration) {
    #[cfg(target_os = "linux")]
    {
        cap_child_address_space();
        install_parent_death_signal();
    }

    let deadline = child_watchdog_deadline(forwarded_budget);
    // Best-effort: if the thread can't spawn, the parent's kill still bounds us.
    let _ = std::thread::Builder::new()
        .name("mermaid-render-watchdog".to_string())
        .spawn(move || {
            std::thread::sleep(deadline);
            // Still alive well past the budget → a runaway render or a dead
            // parent left us spinning. Hard-exit so we can't become an
            // uncontainable orphan (a healthy render exits long before this).
            std::process::exit(CHILD_WATCHDOG_EXIT_CODE);
        });
}

/// Cap the child's address space so a memory-explosion render aborts instead of
/// growing toward host OOM. Best-effort and only ever *lowers* the soft limit
/// (raising the hard limit needs privilege). Linux-only: macOS over-reserves
/// virtual address space, so an `RLIMIT_AS` this size would false-positive on a
/// normal process, and Windows has no equivalent here — both rely on the wall-
/// clock kill plus the watchdog above.
#[cfg(target_os = "linux")]
fn cap_child_address_space() {
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: get/setrlimit read+write the local `lim`; no aliasing, no other
    // memory effects.
    unsafe {
        if libc::getrlimit(libc::RLIMIT_AS, &mut lim) != 0 {
            return;
        }
        // Respect any existing (lower) hard limit; never raise it. `rlim_t` is
        // `u64` on Linux, so the cap const needs no cast.
        let cap = if lim.rlim_max == libc::RLIM_INFINITY {
            CHILD_ADDRESS_SPACE_CAP_BYTES
        } else {
            CHILD_ADDRESS_SPACE_CAP_BYTES.min(lim.rlim_max)
        };
        if lim.rlim_cur == libc::RLIM_INFINITY || lim.rlim_cur > cap {
            lim.rlim_cur = cap;
            let _ = libc::setrlimit(libc::RLIMIT_AS, &lim);
        }
    }
}

/// Ask the kernel to SIGKILL this child if its parent (the pager) dies, so an
/// abruptly-killed parent doesn't strand the child. Complements the watchdog,
/// which covers the race where the parent died before this call. Linux-only.
#[cfg(target_os = "linux")]
fn install_parent_death_signal() {
    // SAFETY: prctl(PR_SET_PDEATHSIG, …) only sets the calling process's
    // parent-death signal; it reads/writes no caller memory.
    unsafe {
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL as libc::c_ulong);
    }
}

/// The parsed `__mermaid-render` child argv.
struct RenderArgs {
    out: PathBuf,
    theme_dark: bool,
    width: u32,
    quality: MermaidRenderQuality,
    /// The parent's forwarded wall-clock budget, from which the child derives its
    /// self-watchdog deadline ([`child_watchdog_deadline`]). Defaults to
    /// [`RENDER_TIMEOUT`] when `--deadline-ms` is absent.
    deadline: Duration,
}

/// Parse the child's argv (`--out`, `--theme`, `--quality`, `--width`,
/// `--deadline-ms`). Separated from stdin reading + rendering so the many
/// malformed-argv cases are unit-testable without touching the process's real
/// stdin. Legacy `--max-height` is accepted and ignored (height comes from quality).
fn parse_render_args(
    mut args: impl Iterator<Item = std::ffi::OsString>,
) -> Result<RenderArgs, String> {
    let mut out: Option<PathBuf> = None;
    let mut theme_dark = false;
    let mut width: u32 = 0;
    let mut quality = MermaidRenderQuality::Terminal;
    let mut deadline = RENDER_TIMEOUT;
    while let Some(flag) = args.next() {
        let flag = flag.to_str().ok_or("non-utf8 flag")?.to_string();
        let mut value = || args.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--out" => out = Some(PathBuf::from(value()?)),
            "--theme" => {
                theme_dark = match value()?.to_str() {
                    Some("dark") => true,
                    Some("light") => false,
                    other => return Err(format!("invalid --theme {other:?}")),
                }
            }
            "--width" => width = parse_u32_arg(&flag, value()?)?,
            // Legacy: height is derived from --quality now; accept and ignore.
            "--max-height" => {
                let _ = parse_u32_arg(&flag, value()?)?;
            }
            "--quality" => {
                quality = match value()?.to_str() {
                    Some("terminal") | Some("t") => MermaidRenderQuality::Terminal,
                    Some("open") | Some("o") => MermaidRenderQuality::Open,
                    other => return Err(format!("invalid --quality {other:?}")),
                }
            }
            "--deadline-ms" => deadline = parse_millis_arg(&flag, value()?)?,
            other => return Err(format!("unknown flag {other}")),
        }
    }
    Ok(RenderArgs {
        out: out.ok_or("missing --out")?,
        theme_dark,
        width,
        quality,
        deadline,
    })
}

/// Render `source` for the parsed `args` and write the PNG atomically. Split
/// from stdin reading so the render→write path is unit-testable with an
/// in-memory source; the end-to-end stdin path is covered by the subprocess
/// integration test.
fn render_and_write(args: &RenderArgs, source: &str) -> Result<(), String> {
    let diagram = render_source_to_png(source, args.theme_dark, args.width, args.quality)
        .map_err(|e| e.to_string())?;
    write_png_atomic(&args.out, &diagram.png).map_err(|e| e.to_string())
}

/// Parse the child's argv, arm the containment backstops, read the source from
/// stdin (capped), render it, and write the PNG atomically.
fn render_child(args: impl Iterator<Item = std::ffi::OsString>) -> Result<(), String> {
    let parsed = parse_render_args(args)?;
    // We ARE the short-lived render child: arm the containment backstops (the
    // watchdog deadline derived from the forwarded parent budget) before reading
    // the untrusted source, so a runaway render or an abruptly-dead parent can't
    // leave a CPU-spinning orphan. The parent's wall-clock kill is the primary
    // bound; these cover the parent-died / abrupt-exit cases.
    install_child_backstops(parsed.deadline);
    let source = read_stdin_capped(RenderLimits::default().max_source_bytes)?;
    render_and_write(&parsed, &source)
}

/// Parse a `u32` CLI value, mapping the flag name into the error for context.
fn parse_u32_arg(flag: &str, value: std::ffi::OsString) -> Result<u32, String> {
    value
        .to_str()
        .and_then(|s| s.parse::<u32>().ok())
        .ok_or_else(|| format!("invalid value for {flag}"))
}

/// Parse a milliseconds CLI value into a [`Duration`], mapping the flag name into
/// the error for context.
fn parse_millis_arg(flag: &str, value: std::ffi::OsString) -> Result<Duration, String> {
    value
        .to_str()
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_millis)
        .ok_or_else(|| format!("invalid value for {flag}"))
}

/// Read at most `max + 1` bytes from stdin (so an oversized payload is detected,
/// not slurped unbounded) and decode as UTF-8. The extra byte lets
/// [`render_checked`]'s size cap reject an over-limit source rather than
/// silently truncating it.
fn read_stdin_capped(max: usize) -> Result<String, String> {
    use std::io::Read as _;
    let mut buf = Vec::new();
    std::io::stdin()
        .take(max as u64 + 1)
        .read_to_end(&mut buf)
        .map_err(|e| e.to_string())?;
    String::from_utf8(buf).map_err(|e| format!("source is not valid UTF-8: {e}"))
}

/// Build [`RenderParams`] for a theme + quality tier (and terminal target width).
fn render_params_for(
    theme_dark: bool,
    target_width_px: u32,
    quality: MermaidRenderQuality,
) -> RenderParams {
    let theme = if theme_dark {
        MermaidTheme::Dark
    } else {
        MermaidTheme::Light
    };
    match quality {
        MermaidRenderQuality::Open => {
            RenderParams::for_os_viewer(theme, OPEN_MIN_WIDTH_PX, OPEN_MAX_HEIGHT_PX)
        }
        MermaidRenderQuality::Terminal => RenderParams {
            theme,
            target_width_px,
            max_height_px: MAX_TARGET_HEIGHT_PX,
            scale: 1.0,
            min_width_px: 0,
            background: Some(theme.surface_background()),
        },
    }
}

/// The shared render core: source + theme/size → checked PNG. Used by the child
/// process and (under `cargo test`) the in-process worker stand-in. Applies the
/// source-size cap via [`render_checked`] and the raster caps via the engine.
fn render_source_to_png(
    source: &str,
    theme_dark: bool,
    target_width_px: u32,
    quality: MermaidRenderQuality,
) -> Result<RenderedDiagram, echo_agent_mermaid::MermaidError> {
    let params = render_params_for(theme_dark, target_width_px, quality);
    render_checked(
        default_engine().as_ref(),
        source,
        &params,
        &RenderLimits::default(),
    )
}

/// In-process render used only by this crate's own `cargo test` (see
/// [`default_render_fn`]): the test harness binary cannot re-exec the
/// `__mermaid-render` subcommand, so the worker plumbing tests render the
/// diagram directly. The real out-of-process path is covered by the integration
/// test against the built pager binary.
#[cfg(test)]
fn render_in_process_for_tests(job: &MermaidJob, _timeout: Duration) -> Result<(), &'static str> {
    match render_source_to_png(
        &job.source,
        job.theme_dark,
        job.target_width_px,
        job.quality,
    ) {
        Ok(diagram) => write_png_atomic(&job.out_path, &diagram.png)
            .map(|_| ())
            .map_err(|_| "write"),
        Err(_) => Err("render"),
    }
}

/// Render (or load from disk) one job. Runs only on the worker thread. Returns
/// the outcome and whether it wrote a fresh PNG (so the worker can sweep).
fn render_job(render: &RenderFn, job: &MermaidJob, timeout: Duration) -> (MermaidOutcome, bool) {
    let started = Instant::now();

    // Disk hit: the diagram PNG is already on disk and decodes.
    if read_cached_png(&job.out_path) {
        tracing::debug!(
            target: MERMAID_TRACING_TARGET,
            theme_kind = ?job.key.theme,
            width_bucket = job.key.width_bucket,
            cache_hit = true,
            "mermaid disk-cache hit",
        );
        return (
            MermaidOutcome::Ready {
                path: job.out_path.clone(),
            },
            false,
        );
    }

    // Reject obviously-oversized source before paying for a child process; the
    // child re-enforces the same cap (defense in depth, since it is the
    // isolation boundary over untrusted input).
    if job.source.len() > RenderLimits::default().max_source_bytes {
        tracing::warn!(
            target: MERMAID_TRACING_TARGET,
            source_len = job.source.len(),
            fallback_reason = "oversize",
            "mermaid render skipped (oversized source)",
        );
        return (MermaidOutcome::Failed, false);
    }

    match render(job, timeout) {
        Ok(()) => {
            tracing::info!(
                target: MERMAID_TRACING_TARGET,
                theme_kind = ?job.key.theme,
                width_bucket = job.key.width_bucket,
                source_len = job.source.len(),
                theme_dark = job.theme_dark,
                target_px = job.target_width_px,
                duration_ms = started.elapsed().as_millis() as u64,
                cache_hit = false,
                engine = "mermaid-to-svg",
                "mermaid render completed",
            );
            (
                MermaidOutcome::Ready {
                    path: job.out_path.clone(),
                },
                true,
            )
        }
        Err(reason) => {
            // `fallback_reason` is a source-free category; safe to log at warn.
            tracing::warn!(
                target: MERMAID_TRACING_TARGET,
                theme_kind = ?job.key.theme,
                width_bucket = job.key.width_bucket,
                source_len = job.source.len(),
                duration_ms = started.elapsed().as_millis() as u64,
                fallback_reason = reason,
                "mermaid render failed",
            );
            (MermaidOutcome::Failed, false)
        }
    }
}

/// Atomically write `png` to `path` (write to a sibling temp, then rename) so a
/// crash mid-write never leaves a partial file the next disk-hit would load.
fn write_png_atomic(path: &Path, png: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("png.tmp");
    std::fs::write(&tmp, png)?;
    std::fs::rename(&tmp, path)
}

/// Sweep a session's `mermaid/` dir down to `max_bytes`, dropping the oldest
/// PNGs (and any decode-corrupt ones) first. A pure cache, so deletion is safe;
/// repopulated on demand. Intended to run off the render path (e.g. via
/// `spawn_blocking` at session load), never per render.
pub fn sweep_session_cache(dir: &Path, max_bytes: u64) {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return; // No cache dir yet — nothing to sweep.
    };

    // (modified_time, size, path) for each PNG; corrupt files are deleted now.
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = Vec::new();
    let mut total: u64 = 0;
    for entry in read_dir.flatten() {
        let path = entry.path();
        // Reclaim orphaned atomic-write temps from a killed/crashed child
        // (`write_png_atomic` writes `*.png.tmp` then renames). They never become
        // a cache hit, so dropping them on sweep keeps the dir from accreting.
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(".png.tmp"))
        {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("png") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let len = meta.len();
        // Drop obviously-truncated PNGs (shorter than the 8-byte signature).
        if len < 8 {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        let modified = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
        total += len;
        files.push((modified, len, path));
    }

    if total <= max_bytes {
        return;
    }

    // Oldest first; delete until under budget.
    files.sort_by_key(|(modified, _, _)| *modified);
    for (_, len, path) in files {
        if total <= max_bytes {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(len);
        }
    }
}

/// Per-[`AgentView`] lazy render runtime: the worker channels plus the on-click
/// renders awaiting their result. Created on the first *cache-missing*
/// `[Open]`/`[Copy path]` click (a disk hit completes without a worker); `None`
/// until then.
pub struct MermaidRuntime {
    tx: Sender<MermaidJob>,
    rx: Receiver<MermaidResult>,
    /// On-click renders whose result hasn't arrived yet. The tick runs each
    /// action when its key lands; the painter consults these keys for the
    /// transient `rendering…` hint. A short `Vec` (clicks are human-paced) keeps
    /// the worker lock-free — no map or mutex needed.
    ///
    /// No independent timeout is needed. The worker coalesces queued jobs by
    /// `MermaidCacheKey`, so a burst for one diagram (e.g. an `[Open]` and a
    /// `[Copy path]` on the same key) collapses to a single `MermaidResult` for
    /// that key rather than one result per job. When it lands,
    /// [`poll_mermaid_results`] drains *every* pending action awaiting that key
    /// via [`take_pending_for`] (and a render that times out / fails in its child
    /// process becomes `Failed`), so no pending entry is ever stranded. The only
    /// way the worker stops sending is its own thread dying, which under the
    /// shipped `panic = "abort"` profile aborts the whole pager rather than
    /// leaking a pending entry — and the render itself runs out of process, so a
    /// diagram that aborts only kills its short-lived child.
    pending: Vec<PendingMermaidAction>,
    /// Whether the per-session disk-cap sweep has run (once per view, off the
    /// render path, the first time a render is dispatched).
    swept: bool,
}

impl MermaidRuntime {
    fn new() -> Self {
        let (tx, rx) = spawn_worker();
        Self {
            tx,
            rx,
            pending: Vec::new(),
            swept: false,
        }
    }

    /// Whether an on-click render for `key` + `action` is already outstanding, so
    /// a repeat click neither re-dispatches nor double-records it.
    fn has_pending(&self, key: &MermaidCacheKey, action: MermaidClickAction) -> bool {
        self.pending
            .iter()
            .any(|p| p.key == *key && p.action == action)
    }
}

/// Representative content-column budget for diagram render sizing. The exact
/// per-entry width does not matter (the cache key buckets width), so a single
/// viewport-derived value drives the render request.
fn representative_content_cols(terminal_width: u16) -> u16 {
    const CHROME: u16 = crate::scrollback::wrappers::EntryRenderer::CHROME_WIDTH;
    const TIMESTAMP_RESERVE: u16 = 10;
    terminal_width
        .saturating_sub(CHROME)
        .saturating_sub(TIMESTAMP_RESERVE)
        .max(20)
}

/// Translate a content-column budget into a render pixel width.
fn target_width_px(content_cols: u16) -> u32 {
    (content_cols as u32 * APPROX_CELL_W_PX * RENDER_SCALE)
        .clamp(MIN_TARGET_WIDTH_PX, MAX_TARGET_WIDTH_PX)
}

/// Remove and return every pending action awaiting `key` (usually one, but the
/// same diagram can have both an `[Open]` and a `[Copy path]` queued). Pure so
/// the result → action matching is unit-testable without an [`AgentView`].
fn take_pending_for(
    pending: &mut Vec<PendingMermaidAction>,
    key: &MermaidCacheKey,
) -> Vec<MermaidClickAction> {
    let mut taken = Vec::new();
    pending.retain(|p| {
        if p.key == *key {
            taken.push(p.action);
            false
        } else {
            true
        }
    });
    taken
}

#[cfg(test)]
thread_local! {
    /// Per-test override for the session `mermaid/` cache dir. View-side tests
    /// set this to a private tempdir so [`AgentView::mermaid_out_path`] resolves
    /// a hermetic, writable cache dir *without* mutating the process-global
    /// `ECHO_AGENT_HOME` (whose `echo_agent_home()` value is cached first-write-wins, an
    /// isolation hazard under the full parallel suite — PNGs could land in the
    /// real `~/.echo-agent`). Thread-local, so each parallel test is independent; the
    /// `TempDir` guard lives here so the dir outlives the view. Mirrors the
    /// `subagent::REPLAY_ECHO_AGENT_HOME` test seam. Production never sets this.
    static TEST_MERMAID_DIR: std::cell::RefCell<Option<tempfile::TempDir>> =
        const { std::cell::RefCell::new(None) };
}

impl AgentView {
    /// Cheap predicate for the event loop: keep ticking only while an on-click
    /// render is outstanding, so the worker's result (plain mpsc, no waker) is
    /// polled promptly. Lazy rendering does no background scanning, so there is
    /// nothing else to drive.
    pub fn mermaid_needs_tick(&self) -> bool {
        self.mermaid
            .as_ref()
            .is_some_and(|rt| !rt.pending.is_empty())
    }

    /// Representative content columns for diagram render sizing this frame.
    fn mermaid_content_cols(&self) -> u16 {
        representative_content_cols(self.last_terminal_size.0)
    }

    /// Drive the lazy mermaid lifecycle for one tick: poll the worker for
    /// finished on-click renders and run each requesting action. Returns `true`
    /// when a redraw is warranted. A no-op until a click is in flight.
    pub fn mermaid_tick(&mut self) -> bool {
        self.poll_mermaid_results()
    }

    /// Lazily create the render runtime (and spawn the worker) on first need.
    fn ensure_mermaid_runtime(&mut self) -> &mut MermaidRuntime {
        if self.mermaid.is_none() {
            self.mermaid = Some(MermaidRuntime::new());
        }
        self.mermaid.as_mut().expect("just created")
    }

    /// Per-session destination path for a diagram's PNG, or `None` until session
    /// identity is known (no on-disk cache before then).
    fn mermaid_out_path(&self, key: &MermaidCacheKey) -> Option<PathBuf> {
        // Test seam: a hermetic per-test cache dir (no `ECHO_AGENT_HOME` mutation).
        #[cfg(test)]
        if let Some(path) = TEST_MERMAID_DIR.with(|d| {
            d.borrow()
                .as_ref()
                .map(|tmp| tmp.path().join(key.cache_filename()))
        }) {
            return Some(path);
        }
        let dir = crate::prompt_images::session_mermaid_dir(
            self.session.session_id.as_ref(),
            &self.session.cwd,
        )?;
        Some(dir.join(key.cache_filename()))
    }

    /// The single cache-key + on-disk-PNG-path derivation, shared by the click
    /// dispatch and its disk-hit check. `None` until the session dir is known.
    fn mermaid_render_target(
        &self,
        source: &str,
        theme: ThemeKind,
        cols: u16,
        quality: MermaidRenderQuality,
    ) -> Option<(MermaidCacheKey, PathBuf)> {
        let key = MermaidCacheKey::derive(source, theme, cols, quality);
        let out_path = self.mermaid_out_path(&key)?;
        Some((key, out_path))
    }

    /// Whether the diagram with `source` currently has an on-click render in
    /// flight — drives the affordance row's transient `rendering…` hint. Matched
    /// by the source hash (not the full cache key) so the hint persists until the
    /// render completes even if the live theme/width changes mid-render (the
    /// in-flight render still targets its click-time key). Cheap: short-circuits
    /// before hashing when nothing is pending.
    pub(crate) fn mermaid_is_rendering(&self, source: &str) -> bool {
        let Some(rt) = self.mermaid.as_ref() else {
            return false;
        };
        if rt.pending.is_empty() {
            return false;
        }
        let source_hash = crate::scrollback::blocks::mermaid_content::hash_source(source);
        rt.pending.iter().any(|p| p.key.source_hash == source_hash)
    }

    /// Run a finished/cached on-click `action` against the rendered PNG `path`.
    /// Shared by the disk-hit fast path and the worker-result completion path.
    fn complete_mermaid_action(&mut self, action: MermaidClickAction, path: &Path) {
        let ok = match action {
            MermaidClickAction::Open => self.open_media_natively(path),
            MermaidClickAction::CopyPath => self
                .copy_to_clipboard(&path.display().to_string())
                .success(),
        };
        if !ok {
            crate::unified_log::error(
                "mermaid.action.failed",
                self.session.session_id.as_ref().map(|s| s.0.as_ref()),
                Some(serde_json::json!({
                    "action": action.log_label(),
                    "path": path.display().to_string(),
                })),
            );
        }
    }

    /// Handle a click on a Mermaid `[Open]`/`[Copy path]` button: render the
    /// diagram at the *live* theme and open-tier auto-scale (sharp OS viewer
    /// PNGs, independent of terminal width) if it isn't cached, then run
    /// `action`.
    ///
    /// Disk hit at the current theme/quality tier → run the action immediately.
    /// Miss → dispatch a render job and record a [`PendingMermaidAction`] so the
    /// tick runs the action when the result lands (a brief `rendering…` toast
    /// covers the gap; a failed render shows an error toast). `source` is moved
    /// into the job, never cloned.
    pub(crate) fn request_mermaid_render(&mut self, source: String, action: MermaidClickAction) {
        let theme = crate::theme::cache::current_kind();
        let cols = self.mermaid_content_cols();
        // Open Image / Copy Image Path always use the open tier so Preview etc.
        // get a high-res PNG; cache key is tier-separated from any terminal budget.
        let quality = MermaidRenderQuality::Open;
        let Some((key, out_path)) = self.mermaid_render_target(&source, theme, cols, quality)
        else {
            // No session dir yet → nowhere to cache the PNG.
            crate::unified_log::warn(
                "mermaid.render.not_ready",
                self.session.session_id.as_ref().map(|s| s.0.as_ref()),
                Some(serde_json::json!({ "action": action.log_label() })),
            );
            self.show_toast("Diagram not ready yet");
            return;
        };

        // A render for this exact key+action is already in flight → just wait
        // for it. Checked BEFORE the disk-cache fast path so a second click that
        // lands after the worker writes the PNG (but before the tick polls the
        // result) doesn't also take the disk-hit path and run the action twice.
        if self
            .mermaid
            .as_ref()
            .is_some_and(|rt| rt.has_pending(&key, action))
        {
            self.show_toast("Rendering diagram\u{2026}");
            return;
        }

        // Disk hit at the current theme/quality → run the action now, no render.
        if read_cached_png(&out_path) {
            self.complete_mermaid_action(action, &out_path);
            return;
        }

        self.ensure_mermaid_runtime();
        self.maybe_sweep_session_cache(&out_path);
        let job = MermaidJob {
            key: key.clone(),
            source,
            out_path,
            theme_dark: theme_is_dark(theme),
            // Terminal width is unused for open tier but kept for job symmetry /
            // future terminal-tier callers.
            target_width_px: target_width_px(cols),
            quality,
        };
        tracing::debug!(
            target: MERMAID_TRACING_TARGET,
            source_len = job.source.len(),
            theme_kind = ?theme,
            target_px = job.target_width_px,
            ?quality,
            "mermaid on-click render dispatched",
        );
        let sent = self
            .mermaid
            .as_ref()
            .is_some_and(|rt| rt.tx.send(job).is_ok());
        if sent {
            if let Some(rt) = self.mermaid.as_mut() {
                rt.pending.push(PendingMermaidAction { key, action });
            }
            self.show_toast("Rendering diagram\u{2026}");
        } else {
            tracing::warn!(
                target: MERMAID_TRACING_TARGET,
                "mermaid worker unavailable; cannot render",
            );
            crate::unified_log::error(
                "mermaid.render.worker_unavailable",
                self.session.session_id.as_ref().map(|s| s.0.as_ref()),
                Some(serde_json::json!({ "action": action.log_label() })),
            );
            self.show_toast("Could not render diagram");
        }
    }

    /// Drain finished renders from the worker and run each one's requesting
    /// action(s): `[Open]` opens the PNG, `[Copy path]` copies its path, and a
    /// failed render shows an error toast. Returns `true` if any action ran (so
    /// the transient `rendering…` hint clears and the toast shows).
    fn poll_mermaid_results(&mut self) -> bool {
        let mut results = Vec::new();
        if let Some(rt) = self.mermaid.as_ref() {
            while let Ok(result) = rt.rx.try_recv() {
                results.push(result);
            }
        }
        if results.is_empty() {
            return false;
        }

        let mut changed = false;
        for MermaidResult { key, outcome } in results {
            // Take every pending action that asked for this key (usually one).
            let actions = match self.mermaid.as_mut() {
                Some(rt) => take_pending_for(&mut rt.pending, &key),
                None => Vec::new(),
            };
            for action in actions {
                changed = true;
                match &outcome {
                    MermaidOutcome::Ready { path } => self.complete_mermaid_action(action, path),
                    MermaidOutcome::Failed => {
                        crate::unified_log::warn(
                            "mermaid.render.failed",
                            self.session.session_id.as_ref().map(|s| s.0.as_ref()),
                            Some(serde_json::json!({ "action": action.log_label() })),
                        );
                        self.show_toast("Could not render diagram");
                    }
                }
            }
        }
        changed
    }

    /// Run the per-session disk-cap sweep once per view, off the render path.
    /// Prefers a tokio blocking task; falls back to inline when no runtime is
    /// present (e.g. unit tests) so scheduling never panics outside a runtime.
    fn maybe_sweep_session_cache(&mut self, out_path: &Path) {
        let Some(rt) = self.mermaid.as_mut() else {
            return;
        };
        if rt.swept {
            return;
        }
        rt.swept = true;
        let Some(dir) = out_path.parent().map(Path::to_path_buf) else {
            return;
        };
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn_blocking(move || sweep_session_cache(&dir, SESSION_DISK_CAP_BYTES));
            }
            Err(_) => sweep_session_cache(&dir, SESSION_DISK_CAP_BYTES),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ThemeKind;

    fn key(source: &str) -> MermaidCacheKey {
        MermaidCacheKey::derive(
            source,
            ThemeKind::EchoAgentNight,
            80,
            MermaidRenderQuality::Terminal,
        )
    }

    fn job(source: &str, out_path: &str) -> MermaidJob {
        MermaidJob {
            key: key(source),
            source: source.to_string(),
            out_path: PathBuf::from(out_path),
            theme_dark: true,
            target_width_px: 1024,
            quality: MermaidRenderQuality::Terminal,
        }
    }

    #[test]
    fn drain_coalesced_keeps_latest_per_key() {
        let (tx, rx) = std::sync::mpsc::channel::<MermaidJob>();
        // Two requests for the same diagram (same key) plus one for a different
        // diagram, all queued.
        tx.send(job("same", "/tmp/old.png")).unwrap();
        tx.send(job("other", "/tmp/other.png")).unwrap();
        tx.send(job("same", "/tmp/new.png")).unwrap();
        let first = rx.recv().unwrap();
        let pending = drain_coalesced(first, &rx);
        assert_eq!(pending.len(), 2, "two distinct cache keys");
        // The repeated key coalesced to its latest job (newest out_path wins).
        assert_eq!(
            pending[&key("same")].out_path,
            PathBuf::from("/tmp/new.png")
        );
        assert_eq!(
            pending[&key("other")].out_path,
            PathBuf::from("/tmp/other.png")
        );
    }

    #[test]
    fn take_pending_for_takes_all_matching_and_leaves_the_rest() {
        let k1 = key("one");
        let k2 = key("two");
        let mut pending = vec![
            PendingMermaidAction {
                key: k1.clone(),
                action: MermaidClickAction::Open,
            },
            PendingMermaidAction {
                key: k2.clone(),
                action: MermaidClickAction::Open,
            },
            PendingMermaidAction {
                key: k1.clone(),
                action: MermaidClickAction::CopyPath,
            },
        ];
        // Both actions awaiting k1 (an Open and a CopyPath) are taken; k2 stays.
        let taken = take_pending_for(&mut pending, &k1);
        assert_eq!(
            taken,
            vec![MermaidClickAction::Open, MermaidClickAction::CopyPath]
        );
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].key, k2);
        // A key with nothing pending takes nothing and leaves the list intact.
        assert!(take_pending_for(&mut pending, &key("absent")).is_empty());
        assert_eq!(pending.len(), 1);
    }

    #[test]
    fn runtime_has_pending_dedupes_by_key_and_action() {
        let mut rt = MermaidRuntime::new();
        let k = key("d");
        assert!(!rt.has_pending(&k, MermaidClickAction::Open));
        rt.pending.push(PendingMermaidAction {
            key: k.clone(),
            action: MermaidClickAction::Open,
        });
        assert!(rt.has_pending(&k, MermaidClickAction::Open));
        // Same key, different action → not a duplicate (both can be queued).
        assert!(!rt.has_pending(&k, MermaidClickAction::CopyPath));
        // Different key → not pending.
        assert!(!rt.has_pending(&key("other"), MermaidClickAction::Open));
    }

    #[test]
    fn target_width_px_is_clamped() {
        assert_eq!(target_width_px(1), MIN_TARGET_WIDTH_PX);
        assert_eq!(target_width_px(10_000), MAX_TARGET_WIDTH_PX);
        // Mid-range scales by cell width × oversample.
        let mid = target_width_px(60);
        assert!(mid > MIN_TARGET_WIDTH_PX && mid < MAX_TARGET_WIDTH_PX);
    }

    #[test]
    fn representative_cols_subtracts_chrome_and_timestamp() {
        // Exactly chrome (4) + timestamp (10) reserved off the viewport width.
        let chrome = crate::scrollback::wrappers::EntryRenderer::CHROME_WIDTH;
        assert_eq!(representative_content_cols(100), 100 - chrome - 10);
        // Tiny viewport never underflows below the floor.
        assert_eq!(representative_content_cols(0), 20);
        assert_eq!(representative_content_cols(5), 20, "saturates to the floor");
    }

    #[test]
    fn sweep_enforces_cap_and_drops_corrupt_keeping_non_png() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // Three valid 16-byte PNGs (total 48).
        for n in 0..3 {
            std::fs::write(root.join(format!("p{n}.png")), vec![0u8; 16]).unwrap();
        }
        // A truncated PNG (< 8 bytes) is always dropped, regardless of the cap.
        let corrupt = root.join("corrupt.png");
        std::fs::write(&corrupt, vec![0u8; 4]).unwrap();
        // A non-PNG is ignored entirely.
        let other = root.join("keep.txt");
        std::fs::write(&other, vec![0u8; 64]).unwrap();

        // Cap below the three PNGs' total → some are dropped to meet it.
        sweep_session_cache(root, 20);
        assert!(!corrupt.exists(), "truncated PNG dropped");
        assert!(other.exists(), "non-PNG untouched");
        let remaining_png: u64 = std::fs::read_dir(root)
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("png"))
            .map(|e| e.metadata().unwrap().len())
            .sum();
        assert!(
            remaining_png <= 20,
            "PNG total swept under the cap: {remaining_png}"
        );
    }

    #[test]
    fn sweep_under_cap_keeps_everything() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.png"), vec![0u8; 16]).unwrap();
        sweep_session_cache(root, 1024);
        assert!(root.join("a.png").exists(), "files under the cap are kept");
    }

    #[test]
    fn sweep_missing_dir_is_noop() {
        // Must not panic and must not create the missing directory.
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        sweep_session_cache(&missing, 100);
        assert!(!missing.exists(), "sweep must not create the dir");
    }

    #[test]
    fn sweep_reclaims_orphaned_png_tmp() {
        // `write_png_atomic` writes `*.png.tmp` then renames; a killed/crashed
        // child can leave the temp behind. The sweep reclaims it (it never
        // becomes a cache hit) while keeping the real PNG, even under the cap.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let orphan = root.join("d.png.tmp");
        std::fs::write(&orphan, vec![0u8; 16]).unwrap();
        let real = root.join("d.png");
        std::fs::write(&real, vec![0u8; 16]).unwrap();

        sweep_session_cache(root, 1024);
        assert!(!orphan.exists(), "orphaned *.png.tmp is reclaimed");
        assert!(real.exists(), "the real PNG is kept");
    }

    fn render_job_for(source: &str, out: &Path) -> MermaidJob {
        MermaidJob {
            key: key(source),
            source: source.to_string(),
            out_path: out.to_path_buf(),
            theme_dark: true,
            target_width_px: 512,
            quality: MermaidRenderQuality::Terminal,
        }
    }

    /// Liveness: the worker thread renders autonomously —
    /// a `Ready` result arrives on the channel with **no external event** after
    /// the single `send` (the view side only has to keep polling, which
    /// `mermaid_needs_tick` ensures while an on-click action is pending).
    #[test]
    fn worker_autonomously_produces_ready() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("d.png");
        let (tx, rx) = spawn_worker();
        tx.send(render_job_for("flowchart LR\nA-->B", &out))
            .unwrap();
        // No further input: the worker renders on its own and sends the result.
        let result = rx
            .recv_timeout(Duration::from_secs(20))
            .expect("worker produces a result without any external poke");
        match result.outcome {
            MermaidOutcome::Ready { path } => {
                assert_eq!(path, out, "Ready reports the on-disk PNG path");
                assert!(out.exists(), "PNG persisted to the per-session cache");
                // The reported path is a real, decodable PNG (no inline bytes
                // are carried — the affordance row opens this file).
                let bytes = std::fs::read(&out).unwrap();
                assert!(
                    crate::prompt_images::decode_image_dimensions(&bytes).is_some(),
                    "rendered a decodable PNG",
                );
            }
            MermaidOutcome::Failed => panic!("a simple flowchart must render"),
        }
    }

    /// Oversized source: rejected by the worker's source-size cap (before a
    /// child is even spawned) → `Failed`, and **no** PNG is written. The child
    /// re-enforces the same cap as defense in depth.
    #[test]
    fn worker_oversized_source_fails_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("huge.png");
        let limit = RenderLimits::default().max_source_bytes;
        let huge = format!("flowchart LR\n{}", "A-->B\n".repeat(limit / 6 + 64));
        assert!(huge.len() > limit, "source exceeds the engine's size cap");
        let (tx, rx) = spawn_worker();
        tx.send(render_job_for(&huge, &out)).unwrap();
        let result = rx
            .recv_timeout(Duration::from_secs(20))
            .expect("worker reports a result");
        assert!(
            matches!(result.outcome, MermaidOutcome::Failed),
            "oversized source must fall back to Failed",
        );
        assert!(!out.exists(), "a failed render writes no PNG");
    }

    /// A re-render under the worker's coalescing still produces a fresh PNG: a
    /// disk-hit on the second send returns `Ready` from the cached file.
    #[test]
    fn worker_second_render_is_a_disk_hit() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("hit.png");
        let (tx, rx) = spawn_worker();
        tx.send(render_job_for("flowchart LR\nA-->B", &out))
            .unwrap();
        let first = rx.recv_timeout(Duration::from_secs(20)).unwrap();
        assert!(matches!(first.outcome, MermaidOutcome::Ready { .. }));
        // Second identical job: the PNG already exists → disk-hit Ready.
        tx.send(render_job_for("flowchart LR\nA-->B", &out))
            .unwrap();
        let second = rx.recv_timeout(Duration::from_secs(20)).unwrap();
        assert!(
            matches!(second.outcome, MermaidOutcome::Ready { .. }),
            "second render is served from the on-disk cache",
        );
    }

    /// A render-step failure (the out-of-process child timed out / exited
    /// non-zero / could not be spawned) maps to `Failed`, writes no PNG, and the
    /// reported reason never reaches the result (only the coarse outcome does).
    #[test]
    fn render_job_render_failure_is_failed() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("fail.png");
        let job = render_job_for("flowchart LR\nA-->B", &out);
        // Stand in for the child timing out (the real kill/timeout is covered by
        // the subprocess unit tests and the integration test).
        let render = |_: &MermaidJob, _: Duration| Err("timeout");

        let (outcome, wrote) = render_job(&render, &job, Duration::from_millis(20));
        assert!(matches!(outcome, MermaidOutcome::Failed));
        assert!(!wrote, "a failed render reports no fresh PNG write");
        assert!(!out.exists(), "no PNG is persisted on failure");
    }

    /// A successful render-step yields `Ready` + `wrote = true` (so the worker's
    /// sweep counter advances) and reports the on-disk path.
    #[test]
    fn render_job_success_reports_ready_and_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("ok.png");
        let job = render_job_for("flowchart LR\nA-->B", &out);
        let render = |job: &MermaidJob, _: Duration| {
            image::RgbaImage::from_pixel(3, 3, image::Rgba([5, 6, 7, 255]))
                .save(&job.out_path)
                .map(|_| ())
                .map_err(|_| "write")
        };

        let (outcome, wrote) = render_job(&render, &job, RENDER_TIMEOUT);
        assert!(matches!(outcome, MermaidOutcome::Ready { .. }));
        assert!(wrote, "a fresh render reports a PNG write");
        assert!(out.exists(), "the PNG persisted");
    }

    /// A disk hit short-circuits before the render step even runs (the render fn
    /// here panics if invoked).
    #[test]
    fn render_job_disk_hit_skips_render() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("hit.png");
        image::RgbaImage::from_pixel(2, 2, image::Rgba([1, 2, 3, 255]))
            .save(&out)
            .unwrap();
        let job = render_job_for("flowchart LR\nA-->B", &out);
        let render = |_: &MermaidJob, _: Duration| -> Result<(), &'static str> {
            panic!("disk hit must not invoke the render step")
        };

        let (outcome, wrote) = render_job(&render, &job, RENDER_TIMEOUT);
        assert!(matches!(outcome, MermaidOutcome::Ready { .. }));
        assert!(!wrote, "a disk hit is not a fresh write");
    }

    /// Oversized source is rejected before the render step (no child spawned):
    /// the render fn panics if reached.
    #[test]
    fn render_job_oversized_source_skips_render() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("huge.png");
        let limit = RenderLimits::default().max_source_bytes;
        let huge = format!("flowchart LR\n{}", "A-->B\n".repeat(limit / 6 + 64));
        let job = render_job_for(&huge, &out);
        let render = |_: &MermaidJob, _: Duration| -> Result<(), &'static str> {
            panic!("oversized source must be rejected before the render step")
        };

        let (outcome, wrote) = render_job(&render, &job, RENDER_TIMEOUT);
        assert!(matches!(outcome, MermaidOutcome::Failed));
        assert!(!wrote);
        assert!(!out.exists(), "no PNG written for oversized source");
    }

    /// The disk-cache hit predicate: a real PNG is a hit; a missing, corrupt, or
    /// symlinked entry is a miss (so it re-renders and never follows a symlink
    /// planted at the model-predictable path).
    #[test]
    fn read_cached_png_accepts_valid_rejects_corrupt_symlink_missing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // Missing file → miss.
        assert!(!read_cached_png(&root.join("absent.png")));

        // A real, decodable PNG → hit.
        let good = root.join("good.png");
        image::RgbaImage::from_pixel(2, 2, image::Rgba([1, 2, 3, 255]))
            .save(&good)
            .unwrap();
        assert!(read_cached_png(&good), "a decodable PNG is a cache hit");

        // A short/undecodable file → miss (forces a re-render).
        let corrupt = root.join("corrupt.png");
        std::fs::write(&corrupt, b"not a png").unwrap();
        assert!(!read_cached_png(&corrupt), "a corrupt file is not a hit");

        // A symlink (even to a valid PNG) → refused.
        #[cfg(unix)]
        {
            let link = root.join("link.png");
            std::os::unix::fs::symlink(&good, &link).unwrap();
            assert!(
                !read_cached_png(&link),
                "a symlinked cache entry is refused"
            );
        }
    }

    /// A render whose PNG cannot be written to disk is fatal: with no inline
    /// display the on-disk file is the only artifact, so a diagram with no file
    /// has nothing for `[Open]`/`[Copy path]` to target. Uses the real
    /// (in-process, under `cargo test`) render fn so the engine renders fine but
    /// the unwritable out-path makes the write fail.
    #[test]
    fn render_job_write_failure_is_fatal_without_a_png() {
        let dir = tempfile::tempdir().unwrap();
        // A regular file where a directory component is expected, so the
        // out_path's parent can't be created and the PNG write fails.
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, b"x").unwrap();
        let out = blocker.join("sub").join("d.png");

        let render = default_render_fn();
        let job = render_job_for("flowchart LR\nA-->B", &out);
        let (outcome, wrote) = render_job(render.as_ref(), &job, RENDER_TIMEOUT);

        assert!(
            matches!(outcome, MermaidOutcome::Failed),
            "an unwritable PNG path falls back to Failed",
        );
        assert!(!wrote, "a failed write reports no fresh PNG");
        assert!(!out.exists(), "no PNG is persisted on write failure");
    }

    /// The shared render core (also used by the child process) renders a cyclic
    /// login-flow — whose back-edge routes back into the cycle, the tricky
    /// flowchart-routing case — to a decodable PNG without panicking.
    #[test]
    fn render_source_to_png_handles_cyclic_login_flow() {
        let source = "flowchart TD\n\
            Start([User visits login page]) --> Enter[Enter username & password]\n\
            Enter --> Submit[Submit credentials]\n\
            Submit --> Validate{Credentials valid?}\n\
            Validate -->|No| Fail[Show error message]\n\
            Fail --> Attempts{Too many failed attempts?}\n\
            Attempts -->|Yes| Lock[Lock account]\n\
            Attempts -->|No| Enter\n\
            Validate -->|Yes| Session[Create session]";
        let diagram = render_source_to_png(source, false, 1024, MermaidRenderQuality::Terminal)
            .expect("cyclic login-flow renders");
        assert!(diagram.width_px > 0 && diagram.height_px > 0);
        assert!(image::load_from_memory(&diagram.png).is_ok());
    }

    fn os_args(v: &[&str]) -> std::vec::IntoIter<std::ffi::OsString> {
        v.iter()
            .map(std::ffi::OsString::from)
            .collect::<Vec<_>>()
            .into_iter()
    }

    /// The child arg-parser accepts a full valid invocation and rejects every
    /// malformed argv shape the `__mermaid-render` child can receive — without
    /// spawning a process or touching stdin.
    #[test]
    fn parse_render_args_accepts_valid_and_rejects_malformed() {
        // Valid: every flag parses into the expected struct.
        let ok = parse_render_args(os_args(&[
            "--out",
            "/tmp/x.png",
            "--theme",
            "dark",
            "--quality",
            "open",
            "--width",
            "640",
            "--max-height",
            "900",
        ]))
        .expect("valid argv parses");
        assert_eq!(ok.out, PathBuf::from("/tmp/x.png"));
        assert!(ok.theme_dark);
        assert_eq!(ok.width, 640);
        assert_eq!(ok.quality, MermaidRenderQuality::Open);

        // light theme → not dark; omitted width defaults to 0; quality defaults terminal.
        let light =
            parse_render_args(os_args(&["--out", "/tmp/x.png", "--theme", "light"])).unwrap();
        assert!(!light.theme_dark);
        assert_eq!(light.width, 0);
        assert_eq!(light.quality, MermaidRenderQuality::Terminal);

        // Each malformed shape is rejected (not silently mis-parsed).
        assert!(
            parse_render_args(os_args(&["--theme", "light"])).is_err(),
            "missing --out",
        );
        assert!(
            parse_render_args(os_args(&["--out", "/tmp/x.png", "--bogus"])).is_err(),
            "unknown flag",
        );
        assert!(
            parse_render_args(os_args(&["--out", "/tmp/x.png", "--width", "wide"])).is_err(),
            "non-numeric width",
        );
        assert!(
            parse_render_args(os_args(&["--out", "/tmp/x.png", "--theme", "purple"])).is_err(),
            "invalid --theme value",
        );
        assert!(
            parse_render_args(os_args(&["--out", "/tmp/x.png", "--theme"])).is_err(),
            "flag missing its value",
        );
        assert!(
            parse_render_args(os_args(&["--out"])).is_err(),
            "--out missing its value at end of argv",
        );
    }

    /// A non-UTF-8 flag is rejected, not panicked-on or mis-parsed.
    #[cfg(unix)]
    #[test]
    fn parse_render_args_rejects_non_utf8_flag() {
        use std::os::unix::ffi::OsStringExt as _;
        let bad = std::ffi::OsString::from_vec(vec![0xff, 0xfe]);
        assert!(
            parse_render_args(vec![bad].into_iter()).is_err(),
            "non-utf8 flag must error",
        );
    }

    /// The child's render→write core turns a real source into a decodable PNG on
    /// disk — the happy path the `__mermaid-render` child takes after reading
    /// stdin (the stdin half is covered by the subprocess integration test).
    #[test]
    fn render_and_write_produces_decodable_png() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("child.png");
        let args = RenderArgs {
            out: out.clone(),
            theme_dark: true,
            width: 640,
            quality: MermaidRenderQuality::Terminal,
            deadline: RENDER_TIMEOUT,
        };
        render_and_write(&args, "flowchart LR\nA-->B").expect("render+write succeeds");
        assert!(out.exists(), "the child wrote the PNG");
        let bytes = std::fs::read(&out).unwrap();
        assert!(image::load_from_memory(&bytes).is_ok(), "PNG decodes");
    }

    /// An empty source (e.g. empty stdin) fails to render and writes no PNG.
    #[test]
    fn render_and_write_empty_source_fails_without_png() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("empty.png");
        let args = RenderArgs {
            out: out.clone(),
            theme_dark: false,
            width: 320,
            quality: MermaidRenderQuality::Terminal,
            deadline: RENDER_TIMEOUT,
        };
        assert!(
            render_and_write(&args, "").is_err(),
            "empty source must error"
        );
        assert!(!out.exists(), "no PNG for an unrenderable source");
    }

    /// The subcommand gate recognizes `__mermaid-render` only as argv[1] — the
    /// dispatch that routes a re-exec into the render child.
    #[test]
    fn is_render_subcommand_matches_only_argv1() {
        let argv = |v: &[&str]| v.iter().map(std::ffi::OsString::from).collect::<Vec<_>>();
        assert!(is_render_subcommand(&argv(&[
            "echoagent",
            MERMAID_RENDER_SUBCOMMAND
        ])));
        assert!(is_render_subcommand(&argv(&[
            "echoagent",
            MERMAID_RENDER_SUBCOMMAND,
            "--out",
            "/tmp/x.png",
        ])));
        // Normal invocations are not the render child.
        assert!(!is_render_subcommand(&argv(&["echoagent"])));
        assert!(!is_render_subcommand(&argv(&["echoagent", "chat"])));
        assert!(!is_render_subcommand(&argv(&[])));
        // The subcommand only counts as argv[1], not deeper in the args.
        assert!(!is_render_subcommand(&argv(&[
            "echoagent",
            "chat",
            MERMAID_RENDER_SUBCOMMAND,
        ])));
    }

    /// `map_run_result` maps every subprocess outcome to the right source-free
    /// category, and treats a zero exit with no decodable PNG as a failure (not
    /// a false `Ready`).
    #[test]
    fn map_run_result_covers_every_outcome() {
        let dir = tempfile::tempdir().unwrap();
        // Zero exit + a real PNG on disk → Ok.
        let good = dir.path().join("good.png");
        image::RgbaImage::from_pixel(2, 2, image::Rgba([1, 2, 3, 255]))
            .save(&good)
            .unwrap();
        assert!(map_run_result(Ok(()), &good).is_ok());
        // Zero exit but no PNG → no_output.
        let missing = dir.path().join("missing.png");
        assert_eq!(map_run_result(Ok(()), &missing), Err("no_output"));
        // Each subprocess error maps to its own category.
        assert_eq!(
            map_run_result(Err(SubprocessError::Timeout), &good),
            Err("timeout"),
        );
        assert_eq!(
            map_run_result(
                Err(SubprocessError::Spawn(std::io::Error::other("x"))),
                &good
            ),
            Err("spawn"),
        );
        assert_eq!(
            map_run_result(
                Err(SubprocessError::Wait(std::io::Error::other("x"))),
                &good
            ),
            Err("wait"),
        );
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt as _;
            // Wait-status with exit code 1 (bits 8-15) → non-success.
            let nonzero = std::process::ExitStatus::from_raw(1 << 8);
            assert_eq!(
                map_run_result(Err(SubprocessError::NonZeroExit(nonzero)), &good),
                Err("child_error"),
            );
        }
    }

    /// A non-zero exit is split so a containment event is observable: a real
    /// signal-terminated child (the `RLIMIT_AS` abort / panic-under-abort shape),
    /// the watchdog's exit code, and an ordinary render-failure exit each map to a
    /// distinct reason. Drives real stub children so the `ExitStatus` is genuine
    /// (a signal carries no exit code; an `exit n` does).
    #[cfg(unix)]
    #[test]
    fn map_run_result_distinguishes_crash_watchdog_from_render_error() {
        // Only the `Ok(())` branch reads the path; every case here is an `Err`.
        let out = Path::new("/nonexistent/out.png");

        // Signal-terminated (SIGABRT) → no exit code → child_crashed.
        let crashed = Command::new("sh")
            .args(["-c", "kill -ABRT $$"])
            .status()
            .expect("spawn abort stub");
        assert!(
            crashed.code().is_none(),
            "a signal-terminated child carries no exit code",
        );
        assert_eq!(
            map_run_result(Err(SubprocessError::NonZeroExit(crashed)), out),
            Err("child_crashed"),
        );

        // The watchdog's exact self-destruct code → child_watchdog.
        let watchdog = Command::new("sh")
            .arg("-c")
            .arg(format!("exit {CHILD_WATCHDOG_EXIT_CODE}"))
            .status()
            .expect("spawn watchdog stub");
        assert_eq!(watchdog.code(), Some(CHILD_WATCHDOG_EXIT_CODE));
        assert_eq!(
            map_run_result(Err(SubprocessError::NonZeroExit(watchdog)), out),
            Err("child_watchdog"),
        );

        // An ordinary render failure (the child's exit 1) → child_error.
        let render_fail = Command::new("sh")
            .args(["-c", "exit 1"])
            .status()
            .expect("spawn render-fail stub");
        assert_eq!(render_fail.code(), Some(1));
        assert_eq!(
            map_run_result(Err(SubprocessError::NonZeroExit(render_fail)), out),
            Err("child_error"),
        );
    }

    /// The child's self-watchdog deadline is the forwarded parent budget plus the
    /// fixed slack, so the self-destruct always lands strictly *after* the
    /// parent's own wall-clock kill — the invariant that keeps the e2e's 30s
    /// budget from racing a slow debug-binary render against a fixed 6s
    /// watchdog.
    #[test]
    fn child_watchdog_deadline_trails_the_forwarded_budget() {
        // e2e budget: 30s forwarded → 33s watchdog (strictly later than the
        // parent's 30s kill, so it never fires while the parent still waits).
        assert_eq!(
            child_watchdog_deadline(Duration::from_secs(30)),
            Duration::from_secs(33),
        );
        assert_eq!(
            child_watchdog_deadline(RENDER_TIMEOUT),
            RENDER_TIMEOUT + CHILD_WATCHDOG_SLACK,
        );
        // For any budget, the watchdog strictly trails the parent's kill.
        for ms in [1u64, 100, 1_500, 30_000] {
            let budget = Duration::from_millis(ms);
            assert!(
                child_watchdog_deadline(budget) > budget,
                "watchdog must trail the parent budget of {ms}ms",
            );
        }
    }

    /// `--deadline-ms` parses into the forwarded budget, defaults to
    /// [`RENDER_TIMEOUT`] when absent, and rejects a missing / non-numeric value.
    #[test]
    fn parse_render_args_handles_deadline_ms() {
        // Present + valid → the exact forwarded budget.
        let parsed = parse_render_args(os_args(&["--out", "/tmp/x.png", "--deadline-ms", "30000"]))
            .expect("valid deadline parses");
        assert_eq!(parsed.deadline, Duration::from_millis(30_000));

        // Absent → defaults to the production render budget.
        let default = parse_render_args(os_args(&["--out", "/tmp/x.png"])).unwrap();
        assert_eq!(
            default.deadline, RENDER_TIMEOUT,
            "an absent --deadline-ms defaults to RENDER_TIMEOUT",
        );

        // Missing value (flag at end of argv) → error.
        assert!(
            parse_render_args(os_args(&["--out", "/tmp/x.png", "--deadline-ms"])).is_err(),
            "--deadline-ms with no value is rejected",
        );
        // Non-numeric value → error.
        assert!(
            parse_render_args(os_args(&["--out", "/tmp/x.png", "--deadline-ms", "soon"])).is_err(),
            "a non-numeric --deadline-ms is rejected",
        );
    }

    /// The production parent path (`run_render_command`: spawn + stdin pipe +
    /// wall-clock timeout + outcome mapping) against stub children — the path the
    /// worker swaps out under `#[cfg(test)]`, so it would otherwise run only in
    /// the `#[ignore]` e2e.
    #[cfg(unix)]
    #[test]
    fn run_render_command_maps_stub_child_outcomes() {
        fn stub(script: &str) -> Command {
            let mut cmd = Command::new("sh");
            cmd.arg("-c")
                .arg(script)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            echo_agent_tty_utils::detach_std_command(&mut cmd);
            cmd
        }

        let dir = tempfile::tempdir().unwrap();

        // Success: the piped stdin (a real PNG) is drained to the out-path, which
        // then decodes → Ok. Proves the scoped stdin writer + zero-exit path.
        let png = dir.path().join("src.png");
        image::RgbaImage::from_pixel(3, 3, image::Rgba([9, 8, 7, 255]))
            .save(&png)
            .unwrap();
        let png_bytes = std::fs::read(&png).unwrap();
        let ok_out = dir.path().join("ok.png");
        let ok_script = format!("cat > '{}'", ok_out.display());
        assert!(
            run_render_command(
                stub(&ok_script),
                &png_bytes,
                &ok_out,
                Duration::from_secs(10)
            )
            .is_ok(),
            "a draining stub that writes a decodable PNG maps to Ok",
        );

        // Zero exit but no PNG written → no_output.
        let none_out = dir.path().join("none.png");
        assert_eq!(
            run_render_command(
                stub("cat >/dev/null"),
                b"x",
                &none_out,
                Duration::from_secs(10)
            ),
            Err("no_output"),
        );

        // Non-zero exit → child_error.
        let err_out = dir.path().join("err.png");
        assert_eq!(
            run_render_command(
                stub("cat >/dev/null; exit 7"),
                b"x",
                &err_out,
                Duration::from_secs(10),
            ),
            Err("child_error"),
        );

        // Slow child + tight budget → timeout, returned promptly (real kill).
        let slow_out = dir.path().join("slow.png");
        let started = Instant::now();
        assert_eq!(
            run_render_command(
                stub("sleep 30"),
                b"x",
                &slow_out,
                Duration::from_millis(100)
            ),
            Err("timeout"),
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "timeout must return at the deadline, not after the child finishes",
        );

        // Missing binary → spawn.
        let spawn_out = dir.path().join("spawn.png");
        let mut missing = Command::new("definitely-not-a-real-binary-9f8a7b6c5d4e");
        missing.stdin(Stdio::piped());
        assert_eq!(
            run_render_command(missing, b"x", &spawn_out, Duration::from_secs(5)),
            Err("spawn"),
        );
    }

    /// `parse_u32_arg` accepts a decimal and rejects junk.
    #[test]
    fn parse_u32_arg_validates() {
        use std::ffi::OsString;
        assert_eq!(
            parse_u32_arg("--width", OsString::from("800")).unwrap(),
            800
        );
        assert!(parse_u32_arg("--width", OsString::from("-1")).is_err());
        assert!(parse_u32_arg("--width", OsString::from("x")).is_err());
    }

    /// Two themes derive two cache keys (hence two filenames), so a dark and a
    /// light render of the *same* source land in separate decodable PNGs — the
    /// artifact-level basis for "switching theme opens the correct per-theme
    /// PNG" (the lazy click derives the out-path from the live theme's key).
    #[test]
    fn per_theme_renders_land_in_distinct_files() {
        let dir = tempfile::tempdir().unwrap();
        let src = "flowchart LR\nA-->B";
        let dark_key = MermaidCacheKey::derive(
            src,
            ThemeKind::EchoAgentNight,
            80,
            MermaidRenderQuality::Terminal,
        );
        let light_key = MermaidCacheKey::derive(
            src,
            ThemeKind::EchoAgentDay,
            80,
            MermaidRenderQuality::Terminal,
        );
        assert_ne!(
            dark_key.cache_filename(),
            light_key.cache_filename(),
            "theme is part of the cache filename",
        );
        let dark_out = dir.path().join(dark_key.cache_filename());
        let light_out = dir.path().join(light_key.cache_filename());

        let (tx, rx) = spawn_worker();
        tx.send(MermaidJob {
            key: dark_key,
            source: src.to_string(),
            out_path: dark_out.clone(),
            theme_dark: true,
            target_width_px: 512,
            quality: MermaidRenderQuality::Terminal,
        })
        .unwrap();
        tx.send(MermaidJob {
            key: light_key,
            source: src.to_string(),
            out_path: light_out.clone(),
            theme_dark: false,
            target_width_px: 512,
            quality: MermaidRenderQuality::Terminal,
        })
        .unwrap();
        for _ in 0..2 {
            let r = rx.recv_timeout(Duration::from_secs(20)).unwrap();
            assert!(matches!(r.outcome, MermaidOutcome::Ready { .. }));
        }
        assert!(dark_out.exists(), "dark-theme PNG persisted");
        assert!(light_out.exists(), "light-theme PNG persisted");
        assert_ne!(dark_out, light_out, "distinct per-theme files");
    }

    // -- View-side lazy glue (hermetic per-test cache dir) -------------------
    //
    // These drive the click → render → poll → action path through a real
    // `AgentView` whose on-disk cache dir is a private tempdir, which the unit
    // tests above (pure helpers) and the `make_agent`-based view tests (no
    // session dir) can't.

    /// Point this test's session `mermaid/` cache dir at a private tempdir —
    /// hermetic, with no process-global `ECHO_AGENT_HOME` mutation. The `TempDir` lives
    /// in the [`TEST_MERMAID_DIR`] thread-local for the test thread's lifetime
    /// (so the dir outlives the view), and each parallel test gets its own dir,
    /// so there is no cross-test contamination and no `echo_agent_home()` cache race.
    fn use_test_mermaid_dir() {
        let tmp = tempfile::tempdir().expect("tempdir creation");
        TEST_MERMAID_DIR.with(|d| *d.borrow_mut() = Some(tmp));
    }

    /// An [`AgentView`] whose session `mermaid/` dir is a private per-test
    /// tempdir and with a fixed terminal width (so the render width bucket —
    /// hence the cache key — is deterministic).
    fn agent_with_session(name: &str) -> AgentView {
        use_test_mermaid_dir();
        let cwd = PathBuf::from("/echo-agent-mermaid-test").join(name);
        let mut agent = crate::app::agent_view::test_agent_view(Some(name), cwd);
        agent.last_terminal_size = (100, 40);
        agent
    }

    /// Drive `mermaid_tick` until `done` (the autonomous worker has no waker, so
    /// the view must keep polling) or a deadline, without a fixed sleep up front.
    fn pump_until(agent: &mut AgentView, mut done: impl FnMut(&AgentView) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            agent.mermaid_tick();
            if done(agent) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "mermaid render did not settle before the deadline",
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn toast_of(agent: &AgentView) -> String {
        agent
            .toast
            .as_ref()
            .map(|(m, _)| m.clone())
            .unwrap_or_default()
    }

    /// Cache miss: the click dispatches a render keyed by the LIVE theme/width,
    /// records a pending action (so the view keeps ticking), and — once the
    /// worker lands the result — runs the action and drains `pending` so
    /// `mermaid_needs_tick()` flips back to false (the settle invariant).
    #[test]
    fn mermaid_view_miss_dispatches_then_settles() {
        let mut agent = agent_with_session("miss");
        let src = "flowchart LR\nA-->B\n".to_string();

        agent.request_mermaid_render(src.clone(), MermaidClickAction::CopyPath);
        assert!(
            agent.mermaid_needs_tick(),
            "a miss records a pending action"
        );
        assert!(
            toast_of(&agent).contains("Rendering"),
            "a miss shows the transient rendering toast",
        );

        // The pending key was hashed at click time from the live theme + width.
        let theme = crate::theme::cache::current_kind();
        let cols = agent.mermaid_content_cols();
        let (want_key, out_path) = agent
            .mermaid_render_target(&src, theme, cols, MermaidRenderQuality::Open)
            .unwrap();
        assert!(
            agent
                .mermaid
                .as_ref()
                .unwrap()
                .pending
                .iter()
                .any(|p| p.key == want_key),
            "pending entry carries the click-time (live theme/width) key",
        );

        // The worker renders autonomously; ticking drains the result, runs the
        // CopyPath action, and clears `pending` (settle).
        pump_until(&mut agent, |a| !a.mermaid_needs_tick());
        assert!(
            !agent.mermaid_needs_tick(),
            "pending is drained once the render lands",
        );
        assert!(
            out_path.exists(),
            "the on-click render wrote the PNG to the session cache",
        );
    }

    /// Disk hit at the live theme/width: the click runs the action immediately,
    /// dispatches no render, and never even spins up the worker runtime.
    #[test]
    fn mermaid_view_disk_hit_runs_action_without_dispatch() {
        let mut agent = agent_with_session("hit");
        let src = "flowchart LR\nA-->B\n".to_string();
        let theme = crate::theme::cache::current_kind();
        let cols = agent.mermaid_content_cols();
        let (_key, out_path) = agent
            .mermaid_render_target(&src, theme, cols, MermaidRenderQuality::Open)
            .unwrap();
        std::fs::create_dir_all(out_path.parent().unwrap()).unwrap();
        image::RgbaImage::from_pixel(2, 2, image::Rgba([1, 2, 3, 255]))
            .save(&out_path)
            .unwrap();

        agent.request_mermaid_render(src, MermaidClickAction::CopyPath);
        // CopyPath ran now (clipboard toast), no render pending, no runtime built.
        let toast = toast_of(&agent);
        assert!(
            toast.starts_with("Copied")
                || toast.starts_with("Copy sent")
                || toast.starts_with("Clipboard unreachable")
                || toast.starts_with("Copy failed"),
            "a disk hit runs the copy action immediately, got {toast:?}",
        );
        assert!(
            !agent.mermaid_needs_tick(),
            "a disk hit dispatches no render"
        );
        assert!(
            agent.mermaid.is_none(),
            "a disk hit needs no worker runtime (created only on a cache miss)",
        );
    }

    /// Regression: when the PNG lands on disk while an action is still
    /// pending, a second identical click must take the `has_pending` guard (not
    /// the disk-hit fast path) — otherwise it would run the action now AND again
    /// when the poll resolves the pending entry (two opens / two copies).
    #[test]
    fn mermaid_view_pending_dedup_wins_over_disk_hit_race() {
        let mut agent = agent_with_session("race");
        let src = "flowchart LR\nA-->B\n".to_string();

        // First click misses → dispatch + exactly one pending CopyPath.
        agent.request_mermaid_render(src.clone(), MermaidClickAction::CopyPath);
        assert_eq!(agent.mermaid.as_ref().unwrap().pending.len(), 1);

        // Simulate the worker having written the PNG before the result is polled.
        let theme = crate::theme::cache::current_kind();
        let cols = agent.mermaid_content_cols();
        let (_key, out_path) = agent
            .mermaid_render_target(&src, theme, cols, MermaidRenderQuality::Open)
            .unwrap();
        std::fs::create_dir_all(out_path.parent().unwrap()).unwrap();
        image::RgbaImage::from_pixel(2, 2, image::Rgba([9, 9, 9, 255]))
            .save(&out_path)
            .unwrap();

        // Second identical click: must dedupe against the in-flight render.
        agent.toast = None;
        agent.request_mermaid_render(src, MermaidClickAction::CopyPath);
        let toast = toast_of(&agent);
        assert!(
            toast.contains("Rendering"),
            "the in-flight render dedups the repeat click (no immediate disk-hit run): {toast:?}",
        );
        assert_eq!(
            agent.mermaid.as_ref().unwrap().pending.len(),
            1,
            "still exactly one pending action — not double-fired",
        );
    }

    /// A failed render (oversized source rejected by the engine's size cap)
    /// resolves the pending action as an error toast, and the view settles.
    #[test]
    fn mermaid_view_failed_render_shows_error_toast() {
        let mut agent = agent_with_session("fail");
        let limit = RenderLimits::default().max_source_bytes;
        let huge = format!("flowchart LR\n{}", "A-->B\n".repeat(limit / 6 + 64));

        agent.request_mermaid_render(huge, MermaidClickAction::CopyPath);
        assert!(
            agent.mermaid_needs_tick(),
            "even a doomed render is dispatched + pending first",
        );
        pump_until(&mut agent, |a| !a.mermaid_needs_tick());
        assert_eq!(
            toast_of(&agent),
            "Could not render diagram",
            "a failed render surfaces the error toast",
        );
    }

    /// Regression: the transient `rendering…` hint matches a pending
    /// render by SOURCE hash, not the full click-time cache key — so it survives
    /// a theme/width switch that happens mid-render (the in-flight render still
    /// targets its click-time key, but the hint must stay on the diagram). A
    /// regression to full-key matching would fail the persistence assertion.
    #[test]
    fn mermaid_is_rendering_matches_by_source_across_theme_width_change() {
        let mut agent = crate::app::agent_view::test_agent_view(None, PathBuf::from("/x"));
        let src = "flowchart LR\nA-->B";

        // An on-click render in flight, keyed at the click-time theme + width.
        let click_key = MermaidCacheKey::derive(
            src,
            ThemeKind::EchoAgentNight,
            80,
            MermaidRenderQuality::Open,
        );
        let mut rt = MermaidRuntime::new();
        rt.pending.push(PendingMermaidAction {
            key: click_key.clone(),
            action: MermaidClickAction::Open,
        });
        agent.mermaid = Some(rt);

        // A later (live) theme + width derives a DIFFERENT full key for the same
        // source — full-key matching would no longer find the pending render...
        let live_key = MermaidCacheKey::derive(
            src,
            ThemeKind::EchoAgentDay,
            240,
            MermaidRenderQuality::Open,
        );
        assert_ne!(
            click_key, live_key,
            "a theme/width change alters the full cache key",
        );
        assert_eq!(
            click_key.source_hash, live_key.source_hash,
            "...but the source hash is theme/width-independent",
        );

        // ...yet the hint still matches, because it keys on the source hash.
        assert!(
            agent.mermaid_is_rendering(src),
            "the rendering hint persists across a mid-render theme/width switch",
        );
        // A different diagram is never considered rendering.
        assert!(
            !agent.mermaid_is_rendering("flowchart LR\nC-->D"),
            "a different source does not match the pending render",
        );
    }
}
