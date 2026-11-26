//! Isolated RSS test for incremental reindexing.
//!
//! This test lives in its own integration-test file (and therefore its own
//! Bazel `rust_test` target / process) so that its whole-process RSS samples
//! are not polluted by the other allocation-heavy tests in
//! `memory_integration.rs` (e.g. `test_fresh_build_rss`,
//! `test_build_batch_peak_rss_is_bounded`, `test_compact_reduces_rss_vs_uncompacted`).
//!
//! Background: `libtest` runs tests in a single binary concurrently across
//! `num_cpus` threads, and VmRSS is measured per-*process*. When this test
//! ran inside `memory_integration.rs` it observed allocator churn from the
//! other tests on the same process, intermittently pushing the measured
//! "incremental growth" delta over the 20 MB budget on aarch64 fastbuild CI
//! (`run_1_of_2` and `run_2_of_2` both failed at ~31 MB).
//!
//! Keep this file to a single test. If you need to add another RSS-sensitive
//! test, give it its own file too rather than reintroducing the
//! noisy-neighbor problem.

use echo_agent_codebase_graph::{FileEvent, IndexManager, IndexManagerConfig};
use std::fs;
use std::path::Path;
use tempfile::tempdir;

/// Read current process RSS in bytes. Supports Linux and macOS.
/// Returns `None` on unsupported platforms.
fn rss_bytes() -> Option<usize> {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        for line in status.lines() {
            if let Some(val) = line.strip_prefix("VmRSS:") {
                let kb: usize = val.trim().trim_end_matches(" kB").trim().parse().ok()?;
                return Some(kb * 1024);
            }
        }
        None
    }

    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        let output = Command::new("ps")
            .args(["-o", "rss=", "-p", &std::process::id().to_string()])
            .output()
            .ok()?;
        let kb: usize = String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .ok()?;
        Some(kb * 1024)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

fn rss_mb() -> Option<f64> {
    rss_bytes().map(|b| b as f64 / (1024.0 * 1024.0))
}

fn fmt_rss(rss: Option<f64>) -> String {
    rss.map_or("N/A".to_string(), |v| format!("{:.1}MB", v))
}

/// Create N Rust source files in `dir`, each with `defs_per_file` function defs.
fn create_rust_files(dir: &Path, count: usize, defs_per_file: usize) {
    for i in 0..count {
        let mut content = String::new();
        for d in 0..defs_per_file {
            content.push_str(&format!("fn func_{}_{}() {{}}\n", i, d));
        }
        fs::write(dir.join(format!("file_{}.rs", i)), &content).unwrap();
    }
}

#[test]
fn test_bulk_incremental_indexing_memory() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    create_rust_files(root, 500, 10);

    let rss_before = rss_mb();

    let config = IndexManagerConfig::new(root.to_path_buf())
        .without_cache_load()
        .without_cache_save();

    let handle = IndexManager::spawn(config);

    let stats = handle.get_stats().unwrap();
    let rss_after_build = rss_mb();

    println!(
        "Initial build: {} files, {} defs, {} refs",
        stats.files, stats.definitions, stats.references
    );
    println!(
        "RSS: {} before → {} after build",
        fmt_rss(rss_before),
        fmt_rss(rss_after_build)
    );

    assert_eq!(stats.files, 500);
    assert!(stats.definitions >= 5000);

    for i in 0..100 {
        let path = root.join(format!("file_{}.rs", i));
        fs::write(&path, "fn modified() {}\nfn also_modified() {}\n").unwrap();
        handle.send_event(FileEvent::modified(path)).unwrap();
    }

    let stats_after = handle.get_stats().unwrap();
    let rss_after_incremental = rss_mb();

    println!(
        "After 100 incremental reindexes: {} files, {} defs",
        stats_after.files, stats_after.definitions
    );
    println!("RSS after incremental: {}", fmt_rss(rss_after_incremental));

    // Incremental reindexing should not grow memory significantly.
    if let (Some(after_inc), Some(after_build)) = (rss_after_incremental, rss_after_build) {
        let growth = after_inc - after_build;
        assert!(
            growth < 20.0,
            "Incremental reindex grew RSS by {:.1}MB (expected <20MB)",
            growth
        );
    }

    handle.shutdown().unwrap();
}
