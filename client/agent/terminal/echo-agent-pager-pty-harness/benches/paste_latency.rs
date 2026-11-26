//! `paste-latency` — clipboard paste latency benchmark for `echo-agent-pager`.
//!
//! Spawns the real pager binary in a PTY and measures the Ctrl+V (raw 0x16)
//! paste path against the REAL macOS pasteboard (`pbcopy` / `osascript`):
//!
//! - `text` mode: `pbcopy` a sentinel, inject 0x16, measure inject → sentinel
//!   visible on screen.
//! - `image` mode (agent surface only): put a PNG on the pasteboard, inject
//!   0x16 followed immediately by a typed burst, and measure both
//!   responsiveness (inject → burst visible; stays flat when the clipboard
//!   read/persist is off the UI thread) and chip latency (inject →
//!   `Image #` chip visible).
//!
//! macOS-only at runtime (the real-clipboard Ctrl+V path only exists there);
//! it compiles everywhere so `cargo bench --no-run` stays green on CI hosts.
//! Only PTY input injection + screen scraping are used, so `--binary` can
//! point at OLD pager artifacts for before/after comparisons.
//!
//! WARNING: overwrites the host clipboard. Prior TEXT contents are restored
//! best-effort on exit; a prior image clipboard cannot be restored.
//!
//! ## Typical use
//!
//! ```bash
//! cargo bench -p echo-agent-pager-pty-harness --bench paste_latency -- --iterations 10
//!
//! # Compare an old release artifact, text mode only, JSON to a file:
//! cargo bench -p echo-agent-pager-pty-harness --bench paste_latency -- \
//!   --binary ~/Downloads/echo-agent-old --mode text --json /tmp/paste-old.json
//! ```

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::Parser as ClapParser;
use echo_agent_pager_pty_harness::{
    ContentController, PtyHarness,
    host_clipboard::{HostClipboardTextGuard, pbcopy, set_clipboard_png, write_fixture_png},
    pager_binary,
    results::percentile,
};

/// Ctrl+V byte; a plain PTY delivers it as the Ctrl+V paste chord.
const CTRL_V: u8 = 0x16;

/// Ctrl+\ as CSI-u (code 92, modifier 5) — opens the dashboard.
const CTRL_BACKSLASH: &[u8] = b"\x1b[92;5u";

/// Response sentinel for the initial session turn each surface needs.
const TURN_SENTINEL: &str = "PASTEBENCHTURNDONE";

#[derive(ClapParser, Debug)]
#[command(
    name = "paste-latency",
    about = "Clipboard paste latency benchmark for echo-agent-pager (macOS, real pasteboard)",
    long_about = None,
)]
struct Cli {
    /// Path to the pager binary. Defaults to auto-resolve (PAGER_BINARY env
    /// or a locally-built debug binary). Works against old artifacts too.
    #[arg(long)]
    binary: Option<PathBuf>,

    /// Paste payload kind to measure.
    #[arg(long, value_enum, default_value_t = Mode::All)]
    mode: Mode,

    /// Paste surface to measure. `dashboard-dispatch` is text-only.
    #[arg(long, value_enum, default_value_t = Surface::Agent)]
    surface: Surface,

    /// Paste iterations per (surface, mode) cell.
    #[arg(long, default_value_t = 10)]
    iterations: usize,

    /// Terminal rows.
    #[arg(long, default_value_t = 50)]
    rows: u16,

    /// Terminal columns.
    #[arg(long, default_value_t = 120)]
    cols: u16,

    /// Also write the results JSON to this path (pretty JSON always goes to
    /// stdout).
    #[arg(long, value_name = "PATH")]
    json: Option<PathBuf>,

    /// Accepted for `cargo bench` compatibility (libtest-style argument).
    /// We ignore it — this isn't a libtest harness.
    #[arg(long, hide = true)]
    #[allow(dead_code)]
    bench: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum Mode {
    Text,
    Image,
    All,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum Surface {
    Agent,
    DashboardDispatch,
    All,
}

impl Surface {
    fn as_str(self) -> &'static str {
        match self {
            Surface::Agent => "agent",
            Surface::DashboardDispatch => "dashboard-dispatch",
            Surface::All => "all",
        }
    }
}

/// Aggregated latency stats for one (surface, mode) cell. For `image` the
/// primary p50/p95/max track the chip (end-to-end attach) latency; the burst
/// responsiveness and chip p50s are also broken out explicitly.
#[derive(Debug, serde::Serialize)]
struct PasteLatencyResult {
    surface: &'static str,
    mode: &'static str,
    iterations: usize,
    p50_ms: f64,
    p95_ms: f64,
    max_ms: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    responsiveness_p50_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    chip_p50_ms: Option<f64>,
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();

    match run().await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("paste-latency failed: {e:#}");
            ExitCode::from(2)
        }
    }
}

async fn run() -> Result<ExitCode> {
    let cli = Cli::parse();

    if !cfg!(target_os = "macos") {
        bail!(
            "paste-latency drives the REAL macOS pasteboard (pbcopy/osascript) through the \
             pager's Ctrl+V path, which only exists on macOS — run it on a Mac"
        );
    }

    let binary = match cli.binary {
        Some(b) => b,
        None => pager_binary().context("resolve pager binary")?,
    };

    let surfaces: Vec<Surface> = match cli.surface {
        Surface::All => vec![Surface::Agent, Surface::DashboardDispatch],
        s => vec![s],
    };
    let modes: Vec<Mode> = match cli.mode {
        Mode::All => vec![Mode::Text, Mode::Image],
        m => vec![m],
    };

    tracing::info!(
        binary = %binary.display(),
        rows = cli.rows,
        cols = cli.cols,
        iterations = cli.iterations,
        "starting paste-latency run"
    );

    // Restore the prior TEXT clipboard on exit, best effort (images can't be).
    let _restore = HostClipboardTextGuard::save();

    let mut results: Vec<PasteLatencyResult> = Vec::new();
    for &surface in &surfaces {
        for &mode in &modes {
            if surface == Surface::DashboardDispatch && mode == Mode::Image {
                // Image pastes are an agent-prompt workflow; the dashboard cell
                // only tracks text latency.
                tracing::info!("skipping dashboard-dispatch image cell (text-only surface)");
                continue;
            }
            let res = bench_cell(&binary, cli.rows, cli.cols, surface, mode, cli.iterations).await;
            match res {
                Ok(r) => results.push(r),
                // Dashboard driving is best-effort: report and keep the agent numbers.
                Err(e) if surface == Surface::DashboardDispatch => {
                    tracing::warn!(error = %e, "dashboard-dispatch cell failed; dropping surface");
                }
                Err(e) => return Err(e),
            }
        }
    }

    // An all-skipped/dropped run measured nothing; fail loudly instead of
    // handing downstream tooling an empty [] with exit 0.
    if results.is_empty() {
        eprintln!(
            "paste-latency: no cells ran — every requested surface/mode combination was skipped or dropped"
        );
        return Ok(ExitCode::from(1));
    }

    let json = serde_json::to_string_pretty(&results).context("serialize results")?;
    println!("{json}");
    if let Some(path) = cli.json {
        std::fs::write(&path, &json)
            .with_context(|| format!("write results JSON to {}", path.display()))?;
        eprintln!("wrote results to {}", path.display());
    }

    Ok(ExitCode::SUCCESS)
}

/// Run one (surface, mode) cell: one spawned pager + session reused across all
/// iterations, cleared between pastes (with a respawn fallback if a clear
/// fails to take).
async fn bench_cell(
    binary: &Path,
    rows: u16,
    cols: u16,
    surface: Surface,
    mode: Mode,
    iterations: usize,
) -> Result<PasteLatencyResult> {
    let mode_str = match mode {
        Mode::Text => "text",
        Mode::Image => "image",
        Mode::All => unreachable!("cells are per concrete mode"),
    };
    tracing::info!(surface = surface.as_str(), mode = mode_str, "running cell");

    let content = ContentController::start()
        .await
        .context("start ContentController")?;
    content.set_response(format!("{TURN_SENTINEL} initial bench turn."));
    let mut harness = spawn_ready(binary, rows, cols, &content, surface)?;

    // Image mode keeps one PNG on the pasteboard for the whole cell (the
    // pager re-reads the unchanged clipboard on every Ctrl+V).
    let tmp = tempfile::tempdir().context("tempdir for the clipboard PNG")?;
    if mode == Mode::Image {
        let png = write_fixture_png(tmp.path())?;
        set_clipboard_png(&png)?;
    }

    let nonce = std::process::id();
    let mut primary_ms: Vec<f64> = Vec::with_capacity(iterations);
    let mut responsiveness_ms: Vec<f64> = Vec::with_capacity(iterations);

    for i in 0..iterations {
        match mode {
            Mode::Text => {
                let sentinel = format!("PASTELATENCY{i} N{nonce}");
                pbcopy(&sentinel)?;
                let start = Instant::now();
                harness.inject_keys(&[CTRL_V]).context("inject Ctrl+V")?;
                wait_visible(&mut harness, &sentinel, Duration::from_secs(10))?;
                let ms = start.elapsed().as_secs_f64() * 1000.0;
                eprintln!(
                    "  [{}/{mode_str}] iter {i}: paste {ms:.1} ms",
                    surface.as_str()
                );
                primary_ms.push(ms);
                clear_input_or_respawn(
                    binary,
                    rows,
                    cols,
                    &content,
                    surface,
                    &mut harness,
                    &[&sentinel],
                )?;
            }
            Mode::Image => {
                let burst = format!("R{i}Z");
                let mut keys = vec![CTRL_V];
                keys.extend_from_slice(burst.as_bytes());
                let start = Instant::now();
                harness
                    .inject_keys(&keys)
                    .context("inject Ctrl+V + typed burst")?;
                wait_visible(&mut harness, &burst, Duration::from_secs(10))?;
                let resp = start.elapsed().as_secs_f64() * 1000.0;
                wait_visible(&mut harness, "Image #", Duration::from_secs(30))?;
                let chip = start.elapsed().as_secs_f64() * 1000.0;
                eprintln!(
                    "  [{}/{mode_str}] iter {i}: responsiveness {resp:.1} ms, chip {chip:.1} ms",
                    surface.as_str()
                );
                responsiveness_ms.push(resp);
                primary_ms.push(chip);
                clear_input_or_respawn(
                    binary,
                    rows,
                    cols,
                    &content,
                    surface,
                    &mut harness,
                    &["Image #", &burst],
                )?;
            }
            Mode::All => unreachable!(),
        }
    }

    let _ = harness.quit();

    let (p50, p95, max) = stats(&mut primary_ms);
    let responsiveness_p50 =
        (!responsiveness_ms.is_empty()).then(|| stats(&mut responsiveness_ms).0);
    let result = PasteLatencyResult {
        surface: surface.as_str(),
        mode: mode_str,
        iterations,
        p50_ms: p50,
        p95_ms: p95,
        max_ms: max,
        responsiveness_p50_ms: responsiveness_p50,
        chip_p50_ms: (mode == Mode::Image).then_some(p50),
    };
    eprintln!(
        "cell {}/{}: p50 {:.1} ms, p95 {:.1} ms, max {:.1} ms{}",
        result.surface,
        result.mode,
        result.p50_ms,
        result.p95_ms,
        result.max_ms,
        result
            .responsiveness_p50_ms
            .map(|r| format!(", responsiveness p50 {r:.1} ms"))
            .unwrap_or_default()
    );
    Ok(result)
}

/// Spawn the pager and drive it to a ready paste surface: welcome → one
/// completed turn (idle session prompt); for the dashboard, Ctrl+\ on top.
fn spawn_ready(
    binary: &Path,
    rows: u16,
    cols: u16,
    content: &ContentController,
    surface: Surface,
) -> Result<PtyHarness> {
    let mut harness = PtyHarness::spawn_with_content(binary, rows, cols, content, &[])
        .context("spawn pager PTY harness")?;
    harness
        .wait_for_text("Quit", Duration::from_secs(20))
        .context("welcome screen")?;
    harness
        .inject_keys(b"go\r")
        .context("submit initial turn")?;
    harness
        .wait_for_text(TURN_SENTINEL, Duration::from_secs(30))
        .context("initial turn response")?;
    // Let post-turn animation settle so paste frames aren't queued behind it.
    harness.update(Duration::from_secs(1));

    if surface == Surface::DashboardDispatch {
        harness
            .inject_keys(CTRL_BACKSLASH)
            .context("Ctrl+\\ open dashboard")?;
        harness
            .wait_for_text("+ New Agent", Duration::from_secs(10))
            .context("dashboard list")?;
        // Opening from a session lands with the LIST focused (footer offers
        // "Tab:input"); Tab moves focus to the dispatch input so the clear
        // keys between iterations reach it. Ctrl+V itself is focus-agnostic.
        harness.update(Duration::from_millis(300));
        if harness.contains_text("Tab:input") {
            harness
                .inject_keys(b"\t")
                .context("Tab focus dispatch input")?;
            if !wait_absent(&mut harness, &["Tab:input"], Duration::from_secs(3)) {
                bail!(
                    "dashboard dispatch input did not take focus (footer still offers Tab:input)\nscreen:\n{}",
                    harness.screen_contents()
                );
            }
        }
    }
    Ok(harness)
}

/// Clear the pasted content between iterations; if the input refuses to clear
/// (needles still on screen), fall back to a full respawn so the next
/// iteration starts clean.
fn clear_input_or_respawn(
    binary: &Path,
    rows: u16,
    cols: u16,
    content: &ContentController,
    surface: Surface,
    harness: &mut PtyHarness,
    stale: &[&str],
) -> Result<()> {
    // Ctrl+U kills to line start, removing pasted text and chip elements.
    harness.inject_keys(b"\x15").context("Ctrl+U clear")?;
    if wait_absent(harness, stale, Duration::from_secs(2)) {
        return Ok(());
    }
    // Backspace spam (the dashboard consumes Ctrl+U as half-page scroll
    // before its dispatch input sees it); chips delete atomically, so 64
    // presses cover any bench payload.
    harness
        .inject_keys(&[0x7f; 64])
        .context("Backspace clear")?;
    if wait_absent(harness, stale, Duration::from_secs(2)) {
        return Ok(());
    }
    if surface == Surface::Agent {
        // Drafted-prompt Esc arms press-again-to-clear; the second press wipes
        // the whole input. Only safe while a draft remains (an empty-prompt
        // Esc-Esc opens the rewind picker), which the absent-checks ruled out.
        harness.inject_keys(b"\x1b").context("Esc arm clear")?;
        harness.update(Duration::from_millis(250));
        harness.inject_keys(b"\x1b").context("Esc confirm clear")?;
        if wait_absent(harness, stale, Duration::from_secs(2)) {
            return Ok(());
        }
    }
    tracing::warn!(
        surface = surface.as_str(),
        "input did not clear; respawning harness"
    );
    let _ = harness.quit();
    *harness = spawn_ready(binary, rows, cols, content, surface)?;
    Ok(())
}

/// Tight-poll (5 ms slices, vs `wait_for_text`'s 50 ms) until `needle` is on
/// screen, for low measurement quantization.
fn wait_visible(harness: &mut PtyHarness, needle: &str, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if harness.contains_text(needle) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "timed out after {timeout:?} waiting for {needle:?}\nscreen:\n{}",
                harness.screen_contents()
            );
        }
        harness.update(Duration::from_millis(5));
    }
}

/// Pump output until every needle is gone or `timeout` elapses.
fn wait_absent(harness: &mut PtyHarness, needles: &[&str], timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if needles.iter().all(|n| !harness.contains_text(n)) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        harness.update(Duration::from_millis(50));
    }
}

/// (p50, p95, max) over `samples`, reusing the shared percentile helper.
fn stats(samples: &mut [f64]) -> (f64, f64, f64) {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    (
        percentile(samples, 50.0),
        percentile(samples, 95.0),
        samples.last().copied().unwrap_or(0.0),
    )
}
