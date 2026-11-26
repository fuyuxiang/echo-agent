//! Pool performance benchmark — emulates the A/B worktree pool lifecycle.
//!
//! Exercises the exact same primitives the production pool uses:
//!   1. Create worktree (GitCheckout mode, like the pool fill task)
//!   2. Warm git caches (git status, like the pool does before marking ready)
//!   3. Sync (git reset --hard + git clean + dirty state copy, like acquire())
//!   4. Simulate use (git status in the synced worktree)
//!   5. Release (git reset --hard + git clean, like release())
//!   6. Cleanup (git worktree remove, like shutdown/schedule_cleanup)
//!
//! Runs against a REAL repo (defaults to the current directory).
//! Designed to be run from a large repo root to get realistic timings.
//!
//! Usage:
//!   cargo run --release --bin pool-perf-bench -- [--source /path/to/repo] [--iterations 3]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use anyhow::{Context, Result};
use clap::Parser;

use echo_agent_fast_worktree::{CreationMode, WorktreeBuilder, WorktreeSync, remove_worktree};

// ============================================================================
// CLI
// ============================================================================

#[derive(Parser)]
#[command(name = "pool-perf-bench")]
#[command(about = "Benchmark worktree pool lifecycle (create/warm/sync/release/cleanup)")]
struct Cli {
    /// Source repository path (default: current directory)
    #[arg(long, default_value = ".")]
    source: PathBuf,

    /// Number of full lifecycle iterations
    #[arg(long, default_value = "3")]
    iterations: usize,

    /// Number of parallel checkout workers (0 = auto)
    #[arg(long, default_value = "0")]
    parallelism: usize,

    /// Whether to copy dirty state during sync
    #[arg(long)]
    copy_dirty: bool,

    /// Enable verbose tracing output
    #[arg(short, long)]
    verbose: bool,

    /// Run in A/B mode: create 2 worktrees concurrently, sync both, release both
    #[arg(long)]
    ab: bool,

    /// Output results as JSON (for programmatic consumption)
    #[arg(long)]
    json: bool,
}

// ============================================================================
// Timing structs
// ============================================================================

#[derive(Debug, Clone)]
struct PhaseTiming {
    name: String,
    duration_ms: f64,
    detail: String,
}

#[derive(Debug, Clone)]
struct IterationResult {
    iteration: usize,
    phases: Vec<PhaseTiming>,
    total_ms: f64,
}

#[derive(Debug, Clone)]
struct BenchmarkResult {
    source: String,
    tracked_files: usize,
    iterations: Vec<IterationResult>,
    ab_mode: bool,
    summary: BenchmarkSummary,
}

#[derive(Debug, Clone)]
struct BenchmarkSummary {
    /// Per-phase averages across all iterations
    phase_averages: Vec<(String, f64)>,
    /// Total average
    total_avg_ms: f64,
    /// Slowest phase name and average
    bottleneck: (String, f64),
}

// ============================================================================
// Phase runners
// ============================================================================

/// Phase 1: Create a linked worktree via GitCheckout mode (what the pool fill task does)
fn phase_create(source: &Path, dest: &Path, parallelism: usize) -> Result<PhaseTiming> {
    let start = Instant::now();

    WorktreeBuilder::new(source, dest)
        .creation_mode(CreationMode::GitCheckout)
        .parallelism(parallelism)
        .create()
        .context("WorktreeBuilder::create failed")?;

    let ms = start.elapsed().as_secs_f64() * 1000.0;
    Ok(PhaseTiming {
        name: "create (GitCheckout)".into(),
        duration_ms: ms,
        detail: format!("git worktree add --detach with checkout.workers={parallelism}"),
    })
}

/// Phase 2: Warm git caches (git status --porcelain) — populates fsmonitor, untracked cache
fn phase_warm_caches(worktree: &Path) -> Result<PhaseTiming> {
    let start = Instant::now();

    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(worktree)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .context("git status for cache warming")?;

    let ms = start.elapsed().as_secs_f64() * 1000.0;
    let detail = if output.status.success() {
        "git status --porcelain (populates fsmonitor + untracked cache)".into()
    } else {
        format!(
            "git status FAILED: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
    };

    Ok(PhaseTiming {
        name: "warm caches".into(),
        duration_ms: ms,
        detail,
    })
}

/// Phase 2b: Second git status — measures the warm-cache speed (should be much faster)
fn phase_warm_caches_2nd(worktree: &Path) -> Result<PhaseTiming> {
    let start = Instant::now();

    Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(worktree)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .context("git status (2nd run)")?;

    let ms = start.elapsed().as_secs_f64() * 1000.0;
    Ok(PhaseTiming {
        name: "warm caches (2nd, hot)".into(),
        duration_ms: ms,
        detail: "git status --porcelain (should be fast with warm caches)".into(),
    })
}

/// Phase 3: Sync — reset to source HEAD + clean + copy dirty state
fn phase_sync(
    source: &Path,
    worktree: &Path,
    copy_dirty: bool,
    skip_clean: bool,
) -> Result<PhaseTiming> {
    let start = Instant::now();

    let sync = WorktreeSync::new(source, worktree);
    let report = sync
        .sync_worktree_opts(copy_dirty, skip_clean)
        .context("sync_worktree failed")?;

    let ms = start.elapsed().as_secs_f64() * 1000.0;
    Ok(PhaseTiming {
        name: if skip_clean {
            "sync (skip_clean)".into()
        } else {
            "sync".into()
        },
        duration_ms: ms,
        detail: format!(
            "head_moved={} dirty_copied={} deleted={} staged={} clean_skipped={}",
            report.head_moved,
            report.dirty_files_copied,
            report.files_deleted,
            report.staged_entries,
            report.clean_skipped
        ),
    })
}

/// Phase 3b: Measure git status speed AFTER sync (validates stat caches are intact)
fn phase_post_sync_status(worktree: &Path) -> Result<PhaseTiming> {
    let start = Instant::now();

    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(worktree)
        .output()
        .context("post-sync git status")?;

    let ms = start.elapsed().as_secs_f64() * 1000.0;
    let line_count = output
        .stdout
        .split(|&b| b == b'\n')
        .filter(|l| !l.is_empty())
        .count();
    Ok(PhaseTiming {
        name: "post-sync git status".into(),
        duration_ms: ms,
        detail: format!("{line_count} dirty entries reported"),
    })
}

/// Phase 4: Simulate use — run git diff --stat (what an agent would do)
fn phase_simulate_use(worktree: &Path) -> Result<PhaseTiming> {
    let start = Instant::now();

    Command::new("git")
        .args(["diff", "--stat"])
        .current_dir(worktree)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .context("git diff --stat")?;

    let ms = start.elapsed().as_secs_f64() * 1000.0;
    Ok(PhaseTiming {
        name: "simulate use (git diff --stat)".into(),
        duration_ms: ms,
        detail: "simulates agent reading diff".into(),
    })
}

/// Phase 5: Release — git reset --hard + git clean -fdx (what pool.release() does)
fn phase_release(worktree: &Path) -> Result<PhaseTiming> {
    let start = Instant::now();

    let r1 = Command::new("git")
        .args(["reset", "--hard", "HEAD"])
        .current_dir(worktree)
        .output()
        .context("git reset --hard")?;

    let r2 = Command::new("git")
        .args(["clean", "-fdx"])
        .current_dir(worktree)
        .output()
        .context("git clean -fdx")?;

    let ms = start.elapsed().as_secs_f64() * 1000.0;
    Ok(PhaseTiming {
        name: "release (reset+clean)".into(),
        duration_ms: ms,
        detail: format!(
            "reset_ok={} clean_ok={}",
            r1.status.success(),
            r2.status.success()
        ),
    })
}

/// Phase 5b: Re-warm caches after release (what the pool does before marking .ready)
fn phase_post_release_warm(worktree: &Path) -> Result<PhaseTiming> {
    let start = Instant::now();

    Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(worktree)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .context("post-release git status")?;

    let ms = start.elapsed().as_secs_f64() * 1000.0;
    Ok(PhaseTiming {
        name: "post-release warm".into(),
        duration_ms: ms,
        detail: "git status after release to re-warm caches".into(),
    })
}

/// Phase 6: Cleanup — rm -rf + deregister (fast) instead of git worktree remove (slow)
fn phase_cleanup(_source: &Path, worktree: &Path) -> Result<PhaseTiming> {
    let start = Instant::now();

    let report = remove_worktree(worktree).context("remove_worktree failed")?;

    let ms = start.elapsed().as_secs_f64() * 1000.0;
    Ok(PhaseTiming {
        name: "cleanup (rm -rf + deregister)".into(),
        duration_ms: ms,
        detail: format!("btrfs={}", report.used_btrfs_delete),
    })
}

// ============================================================================
// A/B mode: two worktrees concurrently
// ============================================================================

fn run_ab_iteration(
    source: &Path,
    base_dir: &Path,
    iteration: usize,
    parallelism: usize,
    copy_dirty: bool,
) -> Result<IterationResult> {
    let mut phases = Vec::new();
    let iter_start = Instant::now();

    let wt_a = base_dir.join(format!("bench_wt_a_{iteration}"));
    let wt_b = base_dir.join(format!("bench_wt_b_{iteration}"));

    // Phase 1: Create both worktrees (sequentially, like the fill task does)
    eprintln!("  [A/B] Creating worktree A...");
    {
        let mut p = phase_create(source, &wt_a, parallelism)?;
        p.name = "create A (GitCheckout)".into();
        phases.push(p);
    }

    eprintln!("  [A/B] Creating worktree B...");
    {
        let mut p = phase_create(source, &wt_b, parallelism)?;
        p.name = "create B (GitCheckout)".into();
        phases.push(p);
    }

    // Phase 2: Warm caches on both
    eprintln!("  [A/B] Warming caches A...");
    {
        let mut p = phase_warm_caches(&wt_a)?;
        p.name = "warm caches A".into();
        phases.push(p);
    }
    eprintln!("  [A/B] Warming caches B...");
    {
        let mut p = phase_warm_caches(&wt_b)?;
        p.name = "warm caches B".into();
        phases.push(p);
    }

    // Phase 2b: Second warm (hot cache measurement)
    {
        let mut p = phase_warm_caches_2nd(&wt_a)?;
        p.name = "warm caches A (2nd, hot)".into();
        phases.push(p);
    }
    {
        let mut p = phase_warm_caches_2nd(&wt_b)?;
        p.name = "warm caches B (2nd, hot)".into();
        phases.push(p);
    }

    // Phase 3: Sync both (this is what acquire() does after claim)
    eprintln!("  [A/B] Syncing A...");
    {
        let mut p = phase_sync(source, &wt_a, copy_dirty, /* skip_clean */ true)?;
        p.name = "sync A (skip_clean)".into();
        phases.push(p);
    }
    eprintln!("  [A/B] Syncing B...");
    {
        let mut p = phase_sync(source, &wt_b, copy_dirty, /* skip_clean */ true)?;
        p.name = "sync B (skip_clean)".into();
        phases.push(p);
    }

    // Phase 3b: Post-sync git status (validates stat caches)
    {
        let mut p = phase_post_sync_status(&wt_a)?;
        p.name = "post-sync status A".into();
        phases.push(p);
    }
    {
        let mut p = phase_post_sync_status(&wt_b)?;
        p.name = "post-sync status B".into();
        phases.push(p);
    }

    // Phase 4: Simulate use
    {
        let mut p = phase_simulate_use(&wt_a)?;
        p.name = "use A (git diff)".into();
        phases.push(p);
    }
    {
        let mut p = phase_simulate_use(&wt_b)?;
        p.name = "use B (git diff)".into();
        phases.push(p);
    }

    // Phase 5: Release both
    eprintln!("  [A/B] Releasing A...");
    {
        let mut p = phase_release(&wt_a)?;
        p.name = "release A".into();
        phases.push(p);
    }
    eprintln!("  [A/B] Releasing B...");
    {
        let mut p = phase_release(&wt_b)?;
        p.name = "release B".into();
        phases.push(p);
    }

    // Phase 5b: Post-release warm
    {
        let mut p = phase_post_release_warm(&wt_a)?;
        p.name = "post-release warm A".into();
        phases.push(p);
    }
    {
        let mut p = phase_post_release_warm(&wt_b)?;
        p.name = "post-release warm B".into();
        phases.push(p);
    }

    // Phase 6: Cleanup both
    eprintln!("  [A/B] Cleaning up A...");
    {
        let mut p = phase_cleanup(source, &wt_a)?;
        p.name = "cleanup A".into();
        phases.push(p);
    }
    eprintln!("  [A/B] Cleaning up B...");
    {
        let mut p = phase_cleanup(source, &wt_b)?;
        p.name = "cleanup B".into();
        phases.push(p);
    }

    let total_ms = iter_start.elapsed().as_secs_f64() * 1000.0;
    Ok(IterationResult {
        iteration,
        phases,
        total_ms,
    })
}

fn run_single_iteration(
    source: &Path,
    base_dir: &Path,
    iteration: usize,
    parallelism: usize,
    copy_dirty: bool,
) -> Result<IterationResult> {
    let mut phases = Vec::new();
    let iter_start = Instant::now();

    let wt = base_dir.join(format!("bench_wt_{iteration}"));

    eprintln!("  Creating worktree...");
    phases.push(phase_create(source, &wt, parallelism)?);

    eprintln!("  Warming caches (1st)...");
    phases.push(phase_warm_caches(&wt)?);

    eprintln!("  Warming caches (2nd, hot)...");
    phases.push(phase_warm_caches_2nd(&wt)?);

    eprintln!("  Syncing (skip_clean=true, pool mode)...");
    phases.push(phase_sync(
        source, &wt, copy_dirty, /* skip_clean */ true,
    )?);

    eprintln!("  Post-sync git status...");
    phases.push(phase_post_sync_status(&wt)?);

    eprintln!("  Simulating use...");
    phases.push(phase_simulate_use(&wt)?);

    eprintln!("  Releasing...");
    phases.push(phase_release(&wt)?);

    eprintln!("  Post-release warm...");
    phases.push(phase_post_release_warm(&wt)?);

    eprintln!("  Cleaning up...");
    phases.push(phase_cleanup(source, &wt)?);

    let total_ms = iter_start.elapsed().as_secs_f64() * 1000.0;
    Ok(IterationResult {
        iteration,
        phases,
        total_ms,
    })
}

// ============================================================================
// Helpers
// ============================================================================

fn count_tracked_files(source: &Path) -> Result<usize> {
    echo_agent_fast_worktree::count_tracked_files(source)
}

fn compute_summary(iterations: &[IterationResult]) -> BenchmarkSummary {
    if iterations.is_empty() {
        return BenchmarkSummary {
            phase_averages: vec![],
            total_avg_ms: 0.0,
            bottleneck: ("(none)".into(), 0.0),
        };
    }

    // Collect all unique phase names in order from the first iteration
    let phase_names: Vec<String> = iterations[0]
        .phases
        .iter()
        .map(|p| p.name.clone())
        .collect();

    let mut phase_averages = Vec::new();
    let mut max_phase = ("(none)".to_string(), 0.0f64);

    for name in &phase_names {
        let mut sum = 0.0;
        let mut count = 0;
        for iter in iterations {
            for phase in &iter.phases {
                if &phase.name == name {
                    sum += phase.duration_ms;
                    count += 1;
                }
            }
        }
        let avg = if count > 0 { sum / count as f64 } else { 0.0 };
        if avg > max_phase.1 {
            max_phase = (name.clone(), avg);
        }
        phase_averages.push((name.clone(), avg));
    }

    let total_avg = iterations.iter().map(|i| i.total_ms).sum::<f64>() / iterations.len() as f64;

    BenchmarkSummary {
        phase_averages,
        total_avg_ms: total_avg,
        bottleneck: max_phase,
    }
}

// ============================================================================
// Output
// ============================================================================

fn print_iteration(result: &IterationResult) {
    println!();
    println!(
        "  ┌─ Iteration {} ─────────────────────────────────────────────────",
        result.iteration + 1
    );
    for phase in &result.phases {
        let bar_len = (phase.duration_ms / 100.0).min(50.0) as usize;
        let bar: String = "█".repeat(bar_len);
        println!(
            "  │ {:>8.1}ms  {:<35} {}",
            phase.duration_ms, phase.name, bar
        );
        if !phase.detail.is_empty() {
            println!("  │            └─ {}", phase.detail);
        }
    }
    println!(
        "  └─ Total: {:.1}ms ──────────────────────────────────────────────",
        result.total_ms
    );
}

fn print_summary(result: &BenchmarkResult) {
    println!();
    println!("╔══════════════════════════════════════════════════════════════════╗");
    println!("║                    BENCHMARK SUMMARY                            ║");
    println!("╠══════════════════════════════════════════════════════════════════╣");
    println!(
        "║ Source: {:<55} ║",
        &result.source[..result.source.len().min(55)]
    );
    println!("║ Tracked files: {:<48} ║", result.tracked_files);
    println!(
        "║ Mode: {:<57} ║",
        if result.ab_mode {
            "A/B (2 worktrees)"
        } else {
            "Single worktree"
        }
    );
    println!("║ Iterations: {:<51} ║", result.iterations.len());
    println!("╠══════════════════════════════════════════════════════════════════╣");
    println!("║ Phase Averages:                                                 ║");
    for (name, avg) in &result.summary.phase_averages {
        let pct = if result.summary.total_avg_ms > 0.0 {
            avg / result.summary.total_avg_ms * 100.0
        } else {
            0.0
        };
        let marker = if name == &result.summary.bottleneck.0 {
            " ◄ BOTTLENECK"
        } else {
            ""
        };
        println!("║   {:>8.1}ms ({:>4.1}%)  {:<35}{}", avg, pct, name, marker);
    }
    println!("╠══════════════════════════════════════════════════════════════════╣");
    println!(
        "║ Total average: {:.1}ms{:>47} ║",
        result.summary.total_avg_ms, ""
    );
    println!(
        "║ Bottleneck: {} ({:.1}ms){} ║",
        result.summary.bottleneck.0,
        result.summary.bottleneck.1,
        " ".repeat(
            63usize
                .saturating_sub(result.summary.bottleneck.0.len())
                .saturating_sub(format!("{:.1}", result.summary.bottleneck.1).len())
                .saturating_sub(15)
        )
    );
    println!("╚══════════════════════════════════════════════════════════════════╝");
}

fn print_json(result: &BenchmarkResult) {
    println!("{{");
    println!("  \"source\": {:?},", result.source);
    println!("  \"tracked_files\": {},", result.tracked_files);
    println!("  \"ab_mode\": {},", result.ab_mode);
    println!("  \"iterations\": [");
    for (i, iter) in result.iterations.iter().enumerate() {
        println!("    {{");
        println!("      \"iteration\": {},", iter.iteration);
        println!("      \"total_ms\": {:.2},", iter.total_ms);
        println!("      \"phases\": [");
        for (j, phase) in iter.phases.iter().enumerate() {
            let comma = if j + 1 < iter.phases.len() { "," } else { "" };
            println!(
                "        {{ \"name\": {:?}, \"duration_ms\": {:.2}, \"detail\": {:?} }}{}",
                phase.name, phase.duration_ms, phase.detail, comma
            );
        }
        println!("      ]");
        let comma = if i + 1 < result.iterations.len() {
            ","
        } else {
            ""
        };
        println!("    }}{comma}");
    }
    println!("  ],");
    println!("  \"summary\": {{");
    println!("    \"total_avg_ms\": {:.2},", result.summary.total_avg_ms);
    println!(
        "    \"bottleneck\": {{ \"name\": {:?}, \"avg_ms\": {:.2} }},",
        result.summary.bottleneck.0, result.summary.bottleneck.1
    );
    println!("    \"phase_averages\": [");
    for (i, (name, avg)) in result.summary.phase_averages.iter().enumerate() {
        let comma = if i + 1 < result.summary.phase_averages.len() {
            ","
        } else {
            ""
        };
        println!(
            "      {{ \"name\": {:?}, \"avg_ms\": {:.2} }}{}",
            name, avg, comma
        );
    }
    println!("    ]");
    println!("  }}");
    println!("}}");
}

// ============================================================================
// Main
// ============================================================================

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Init tracing
    if cli.verbose {
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_target(false)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .with_target(false)
            .init();
    }

    let source = dunce::canonicalize(&cli.source).context("source path not found")?;
    let tracked = count_tracked_files(&source).unwrap_or(0);

    if !cli.json {
        eprintln!("╔══════════════════════════════════════════════════════════════════╗");
        eprintln!("║             Pool Performance Benchmark                          ║");
        eprintln!("╠══════════════════════════════════════════════════════════════════╣");
        eprintln!("║ Source: {}", source.display());
        eprintln!("║ Tracked files: {tracked}");
        eprintln!(
            "║ Mode: {}",
            if cli.ab {
                "A/B (2 worktrees)"
            } else {
                "Single worktree"
            }
        );
        eprintln!("║ Iterations: {}", cli.iterations);
        eprintln!(
            "║ Parallelism: {}",
            if cli.parallelism == 0 {
                "auto".to_string()
            } else {
                cli.parallelism.to_string()
            }
        );
        eprintln!("║ Copy dirty: {}", cli.copy_dirty);
        eprintln!("╚══════════════════════════════════════════════════════════════════╝");
        eprintln!();
    }

    // Create a temporary base directory for worktrees
    let bench_dir = tempfile::Builder::new()
        .prefix("pool-perf-bench-")
        .tempdir()
        .context("failed to create temp dir")?;

    if !cli.json {
        eprintln!("Bench dir: {}", bench_dir.path().display());
    }

    // Enable git perf features on source (like the pool does)
    if !cli.json {
        eprintln!("Enabling git perf features on source...");
    }
    for (key, val) in [("core.fsmonitor", "true"), ("core.untrackedCache", "true")] {
        Command::new("git")
            .args(["config", key, val])
            .current_dir(&source)
            .output()
            .ok();
    }

    let mut iterations = Vec::new();

    for i in 0..cli.iterations {
        if !cli.json {
            eprintln!("\n━━━ Iteration {}/{} ━━━", i + 1, cli.iterations);
        }

        let result = if cli.ab {
            run_ab_iteration(
                &source,
                bench_dir.path(),
                i,
                cli.parallelism,
                cli.copy_dirty,
            )?
        } else {
            run_single_iteration(
                &source,
                bench_dir.path(),
                i,
                cli.parallelism,
                cli.copy_dirty,
            )?
        };

        if !cli.json {
            print_iteration(&result);
        }

        iterations.push(result);
    }

    let summary = compute_summary(&iterations);

    let bench_result = BenchmarkResult {
        source: source.to_string_lossy().to_string(),
        tracked_files: tracked,
        iterations,
        ab_mode: cli.ab,
        summary,
    };

    if cli.json {
        print_json(&bench_result);
    } else {
        print_summary(&bench_result);
    }

    Ok(())
}
