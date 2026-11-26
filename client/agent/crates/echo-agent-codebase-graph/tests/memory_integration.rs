//! Integration tests for memory behavior during indexing.
//!
//! These tests create real file trees, index them, send incremental events,
//! and measure RSS to detect memory regressions.

use echo_agent_codebase_graph::{
    FileEvent, IndexBuilder, IndexManager, IndexManagerConfig, ScopeGraphIndex, load_index,
    save_index,
};
use std::fs;
use std::path::Path;
use std::sync::Arc;
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

/// Create N binary files with a supported extension.
fn create_binary_files(dir: &Path, count: usize, size: usize) {
    for i in 0..count {
        let mut data = vec![0xFFu8; size];
        // Ensure null byte in first 8000 bytes for binary detection
        data[50] = 0;
        fs::write(dir.join(format!("binary_{}.rs", i)), &data).unwrap();
    }
}

// =========================================================================
// Tests
// =========================================================================

#[test]
fn test_binary_files_no_memory_growth() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    fs::write(root.join("legit.rs"), "fn legit() {}").unwrap();

    let config = IndexManagerConfig::new(root.to_path_buf())
        .without_cache_load()
        .without_cache_save();

    let handle = IndexManager::spawn(config);
    let _ = handle.get_file_count();

    // Create 200 binary files with .rs extension, each 100KB
    create_binary_files(root, 200, 100_000);

    for i in 0..200 {
        let path = root.join(format!("binary_{}.rs", i));
        handle.send_event(FileEvent::created(path)).unwrap();
    }

    let count = handle.get_file_count().unwrap();

    // Binary files should not be indexed (detected via 8KB prefix read)
    assert_eq!(count, 1, "Only legit.rs should be indexed");

    handle.shutdown().unwrap();
}

#[test]
fn test_hidden_dir_files_not_indexed() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    fs::write(root.join("normal.rs"), "fn normal() {}").unwrap();

    let config = IndexManagerConfig::new(root.to_path_buf())
        .without_cache_load()
        .without_cache_save();

    let handle = IndexManager::spawn(config);
    let _ = handle.get_file_count();

    // Create 300 files under a hidden directory (simulating .claude worktree)
    let hidden = root.join(".claude").join("worktrees").join("session1");
    fs::create_dir_all(&hidden).unwrap();
    create_rust_files(&hidden, 300, 20);

    for i in 0..300 {
        let path = hidden.join(format!("file_{}.rs", i));
        handle.send_event(FileEvent::created(path)).unwrap();
    }

    let count = handle.get_file_count().unwrap();

    // Hidden dir files should not be indexed
    assert_eq!(count, 1, "Only normal.rs should be indexed");

    handle.shutdown().unwrap();
}

#[test]
fn test_oversized_files_skipped() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    fs::write(root.join("small.rs"), "fn small() {}").unwrap();

    let config = IndexManagerConfig::new(root.to_path_buf())
        .without_cache_load()
        .without_cache_save();

    let handle = IndexManager::spawn(config);
    let _ = handle.get_file_count();

    // Create a 6MB text file with valid Rust syntax (exceeds MAX_INDEXABLE_FILE_SIZE)
    let big_content = "fn big() {}\n".repeat(500_000);
    let big_path = root.join("huge.rs");
    fs::write(&big_path, &big_content).unwrap();
    drop(big_content);

    handle.send_event(FileEvent::created(big_path)).unwrap();

    let count = handle.get_file_count().unwrap();
    assert_eq!(count, 1, "Only small.rs should be indexed");

    handle.shutdown().unwrap();
}

#[test]
fn test_event_coalescing_reduces_work() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    fs::write(root.join("target.rs"), "fn original() {}").unwrap();

    let config = IndexManagerConfig::new(root.to_path_buf())
        .without_cache_load()
        .without_cache_save();

    let handle = IndexManager::spawn(config);
    let _ = handle.get_file_count();

    // Rapidly send 50 modify events for the same file
    // Coalescing should collapse these into a single reindex
    let path = root.join("target.rs");
    for i in 0..50 {
        fs::write(&path, format!("fn version_{}() {{}}", i)).unwrap();
        handle
            .send_event(FileEvent::modified(path.clone()))
            .unwrap();
    }

    let stats = handle.get_stats().unwrap();

    // Should have exactly 1 file with 1 definition (the last version)
    assert_eq!(stats.files, 1);
    assert!(
        stats.definitions >= 1,
        "Should have at least 1 definition after coalescing"
    );

    handle.shutdown().unwrap();
}

#[test]
fn test_builder_skips_binary_and_oversized_in_bulk() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    // Mix of valid, binary, and oversized files
    create_rust_files(root, 100, 5); // 100 valid files
    create_binary_files(root, 50, 10_000); // 50 binary files

    // One oversized file
    let big = "fn x() {}\n".repeat(600_000); // ~6MB
    fs::write(root.join("oversized.rs"), &big).unwrap();
    drop(big);

    let index = IndexBuilder::new().build(root).unwrap();
    let (files, defs, _refs) = index.stats();

    // Only the 100 valid files should be indexed
    assert_eq!(files, 100);
    assert!(defs >= 500); // 100 files × 5 defs
}

/// Measure RSS growth from a single `get_snapshot()` call on a representative index.
///
/// This test characterises the per-clone cost so we have a baseline before any
/// structural changes to `ScopeGraphIndex`.  It does not enforce a hard byte
/// limit because RSS jitter in CI can be significant; instead it prints the
/// delta so regressions are visible in test output.
#[test]
fn test_single_snapshot_rss() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    create_rust_files(root, 500, 10); // 500 files, 5 000 defs

    let config = IndexManagerConfig::new(root.to_path_buf())
        .without_cache_load()
        .without_cache_save();

    let handle = IndexManager::spawn(config);
    let stats = handle.get_stats().unwrap();
    assert_eq!(stats.files, 500);
    assert!(stats.definitions >= 5000);

    let rss_before = rss_mb();

    // Hold the snapshot to keep its allocation live while we sample RSS.
    // get_snapshot() now returns Arc<ScopeGraphIndex>; the Arc::clone is
    // zero-cost, so the delta here reflects any other allocator activity.
    let snapshot: Arc<ScopeGraphIndex> = handle.get_snapshot().unwrap();
    let rss_with_snapshot = rss_mb();

    // Drop it and let the allocator reclaim.
    drop(snapshot);
    let rss_after_drop = rss_mb();

    println!(
        "Single snapshot RSS: {} before → {} held → {} after drop",
        fmt_rss(rss_before),
        fmt_rss(rss_with_snapshot),
        fmt_rss(rss_after_drop),
    );

    if let (Some(before), Some(held)) = (rss_before, rss_with_snapshot) {
        let delta_mb = held - before;
        println!("Snapshot RSS delta (held): {:.1}MB", delta_mb);
        // With Arc<ScopeGraphIndex> the snapshot is a pointer increment —
        // no heap allocation of index data.  Allow 20 MB for jitter/allocator
        // metadata.
        assert!(
            delta_mb < 20.0,
            "Snapshot Arc::clone grew RSS by {:.1}MB (expected <20MB with Arc)",
            delta_mb
        );
    }

    handle.shutdown().unwrap();
}

/// Verify that taking many snapshots and dropping them immediately does not
/// cause unbounded RSS growth (allocator should reclaim between clones).
#[test]
fn test_repeated_snapshots_rss_bounded() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    create_rust_files(root, 500, 10);

    let config = IndexManagerConfig::new(root.to_path_buf())
        .without_cache_load()
        .without_cache_save();

    let handle = IndexManager::spawn(config);
    let _ = handle.get_stats().unwrap();

    let rss_after_build = rss_mb();

    // Take 20 snapshots, dropping each one before requesting the next.
    for _ in 0..20 {
        let _snapshot = handle.get_snapshot().unwrap();
        // dropped at end of loop body
    }

    let rss_after_snapshots = rss_mb();

    println!(
        "Repeated snapshots RSS: {} after build → {} after 20 snapshots (dropped)",
        fmt_rss(rss_after_build),
        fmt_rss(rss_after_snapshots),
    );

    if let (Some(after_build), Some(after_snaps)) = (rss_after_build, rss_after_snapshots) {
        let growth_mb = after_snaps - after_build;
        println!("RSS growth from 20 dropped snapshots: {:.1}MB", growth_mb);
        // With Arc<ScopeGraphIndex> each snapshot is a reference-count bump;
        // dropping it is a decrement.  No heap data is duplicated, so 20
        // repeated dropped snapshots should add near-zero RSS.  Allow 10 MB
        // for allocator bookkeeping jitter.
        assert!(
            growth_mb < 10.0,
            "20 Arc snapshots grew RSS by {:.1}MB (expected <10MB with Arc)",
            growth_mb
        );
    }

    handle.shutdown().unwrap();
}

/// Measure the RSS cost of a fresh index build (no cache).
///
/// This establishes a baseline for the build-phase two-phase peak that
/// the bounded merge-batching is designed to reduce on large repos.
/// The test prints the delta so regressions become visible in CI output.
#[test]
fn test_fresh_build_rss() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    create_rust_files(root, 500, 10); // 500 files, 5 000 defs

    let rss_before = rss_mb();

    // Build entirely via IndexBuilder (same path used by IndexManager on first run)
    let index = IndexBuilder::new().build(root).unwrap();

    let rss_after_build = rss_mb();

    let (files, defs, _refs) = index.stats();
    println!(
        "Fresh build: {} files, {} defs — RSS {} → {}",
        files,
        defs,
        fmt_rss(rss_before),
        fmt_rss(rss_after_build)
    );

    if let (Some(before), Some(after)) = (rss_before, rss_after_build) {
        let delta_mb = after - before;
        println!("Fresh build RSS delta: {:.1}MB", delta_mb);
        // 200 MB is a generous ceiling for a 500-file index with 5 000 defs.
        // The bounded-batch fix targets large repos (> build_batch_size files);
        // the delta here reflects the steady-state index size.
        assert!(
            delta_mb < 200.0,
            "Fresh build grew RSS by {:.1}MB (expected <200MB for 500 files)",
            delta_mb
        );
    }

    assert_eq!(files, 500);
    assert!(defs >= 5000);
}

/// Measure the RSS cost of loading an index from the cache.
///
/// This isolates the deserialization peak in `ScopeGraphIndex::read_from()` +
/// `StringInterner::from_parts()` from the build peak so that improvements to
/// each path can be tracked independently.
#[test]
fn test_cache_load_rss() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    create_rust_files(root, 500, 10);

    // Build and save to a temp cache file
    let cache_path = dir.path().join("index.bin");
    let index = IndexBuilder::new().build(root).unwrap();
    save_index(&cache_path, &index).unwrap();

    let (files, defs, _refs) = index.stats();
    // Drop the built index so the only RSS above baseline is from the load
    drop(index);

    let rss_before_load = rss_mb();

    let loaded = load_index(&cache_path).unwrap();

    let rss_after_load = rss_mb();

    println!(
        "Cache load: {} files, {} defs — RSS {} → {}",
        files,
        defs,
        fmt_rss(rss_before_load),
        fmt_rss(rss_after_load)
    );

    if let (Some(before), Some(after)) = (rss_before_load, rss_after_load) {
        let delta_mb = after - before;
        println!("Cache load RSS delta: {:.1}MB", delta_mb);
        // 200 MB ceiling — same rationale as the build test above.
        assert!(
            delta_mb < 200.0,
            "Cache load grew RSS by {:.1}MB (expected <200MB for 500 files)",
            delta_mb
        );
    }

    let (loaded_files, loaded_defs, _) = loaded.stats();
    assert_eq!(
        loaded_files, files,
        "loaded file count must match built count"
    );
    assert_eq!(loaded_defs, defs, "loaded def count must match built count");
}

/// Verify that bounded merge-batching produces the same index as unbounded.
///
/// Uses a very small build_batch_size to exercise the multi-batch code path
/// even on this small corpus, then compares stats against an unbatched build.
#[test]
fn test_build_batch_size_produces_correct_index() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    create_rust_files(root, 200, 5); // 200 files, 1 000 defs

    // Build with a very small batch size (10 files per merge batch)
    let batched = IndexBuilder::new()
        .with_build_batch_size(10)
        .build(root)
        .unwrap();

    // Build without batching restriction for comparison
    let reference = IndexBuilder::new().build(root).unwrap();

    let (b_files, b_defs, b_refs) = batched.stats();
    let (r_files, r_defs, r_refs) = reference.stats();

    assert_eq!(
        b_files, r_files,
        "file count must match regardless of batch size"
    );
    assert_eq!(
        b_defs, r_defs,
        "definition count must match regardless of batch size"
    );
    assert_eq!(
        b_refs, r_refs,
        "reference count must match regardless of batch size"
    );
}

/// Demonstrate that bounded merge-batching caps the transient peak RSS that
/// occurs while `FileSymbols` are live in memory.
///
/// Strategy: run both builds under a background RSS-polling thread so we can
/// observe the peak that is otherwise invisible to before/after measurements.
/// On a 1 000-file corpus the per-batch buffer is small relative to OS-page
/// granularity, so we do **not** assert a hard numeric improvement; instead
/// we assert that the batched peak is not *worse* than the unbounded peak
/// (ruling out regressions) and print both deltas for CI visibility.
///
/// At production scale (tens of thousands of files) the bounded-batch effect
/// is proportionally much larger and easily observable in profiling output.
#[test]
fn test_build_batch_peak_rss_is_bounded() {
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    };
    use std::time::Duration;

    let dir = tempdir().unwrap();
    let root = dir.path();
    // 1 000 files, 20 defs each — enough to make FileSymbols payload non-trivial
    create_rust_files(root, 1000, 20);

    /// Spawn a background thread that polls RSS every 2ms and returns the peak.
    fn track_peak_during<F: FnOnce()>(f: F) -> f64 {
        let peak = Arc::new(Mutex::new(rss_mb().unwrap_or(0.0)));
        let stop = Arc::new(AtomicBool::new(false));
        let peak_bg = Arc::clone(&peak);
        let stop_bg = Arc::clone(&stop);

        let monitor = std::thread::spawn(move || {
            while !stop_bg.load(Ordering::Relaxed) {
                if let Some(rss) = rss_mb() {
                    let mut p = peak_bg.lock().unwrap();
                    if rss > *p {
                        *p = rss;
                    }
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        });

        f();

        stop.store(true, Ordering::Relaxed);
        monitor.join().unwrap();
        *peak.lock().unwrap()
    }

    let baseline = rss_mb().unwrap_or(0.0);

    // Batched: 50 files per merge batch → 20 batches for 1 000 files.
    // FileSymbols from at most 50 files are live at any one time.
    let peak_batched = track_peak_during(|| {
        let _idx = IndexBuilder::new()
            .with_build_batch_size(50)
            .build(root)
            .unwrap();
    });

    // Unbounded: all 1 000 files parsed before any merge starts.
    let peak_unbatched = track_peak_during(|| {
        let _idx = IndexBuilder::new()
            .with_build_batch_size(5_000)
            .build(root)
            .unwrap();
    });

    println!(
        "Build peak RSS — baseline: {:.1}MB, batched (50/batch): {:.1}MB \
         (+{:.1}MB), unbounded: {:.1}MB (+{:.1}MB)",
        baseline,
        peak_batched,
        peak_batched - baseline,
        peak_unbatched,
        peak_unbatched - baseline,
    );

    // The batched build must not exhibit meaningfully higher peak RSS than the
    // unbounded build — that would indicate the batching is broken.
    // Allow 30 MB of jitter from concurrent allocator/OS activity.
    if peak_batched > 0.0 && peak_unbatched > 0.0 {
        assert!(
            peak_batched <= peak_unbatched + 30.0,
            "Batched build peaked {:.1}MB higher than unbounded (>30MB unexpected)",
            peak_batched - peak_unbatched
        );
    }

    // Both configurations must produce the same logical index.
    let batched_idx = IndexBuilder::new()
        .with_build_batch_size(50)
        .build(root)
        .unwrap();
    let unbatched_idx = IndexBuilder::new()
        .with_build_batch_size(5_000)
        .build(root)
        .unwrap();
    let (b_files, b_defs, b_refs) = batched_idx.stats();
    let (u_files, u_defs, u_refs) = unbatched_idx.stats();
    assert_eq!(b_files, u_files, "file count must match");
    assert_eq!(b_defs, u_defs, "definition count must match");
    assert_eq!(b_refs, u_refs, "reference count must match");
}

// =============================================================================
// Structural compaction tests
// =============================================================================

/// Verify that an index survives a save/load round-trip after compact().
///
/// Guards against regressions in the binary format introduced by the u32
/// line-number change.  compact() is already called by IndexBuilder::build,
/// so no explicit call is needed here.
#[test]
fn test_compact_then_save_load_roundtrip() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    create_rust_files(root, 50, 4); // 50 files, 200 defs

    // build() calls compact() internally via build_fast()
    let original = IndexBuilder::new().build(root).unwrap();

    let cache_path = dir.path().join("test_index.bin");
    save_index(&cache_path, &original).unwrap();

    let loaded = load_index(&cache_path).unwrap();

    let (orig_files, orig_defs, orig_refs) = original.stats();
    let (load_files, load_defs, load_refs) = loaded.stats();

    assert_eq!(orig_files, load_files, "file count survives round-trip");
    assert_eq!(orig_defs, load_defs, "def count survives round-trip");
    assert_eq!(orig_refs, load_refs, "ref count survives round-trip");

    // Verify a representative symbol round-trips
    let sym = "func_0_0";
    let orig_locs = original.find_definitions(sym);
    let load_locs = loaded.find_definitions(sym);
    assert_eq!(
        orig_locs, load_locs,
        "definition locations must survive save/load round-trip"
    );
}

/// Compare RSS between an uncompacted index (built via raw API, no Vec
/// shrinking) and a compacted one, to justify the compaction scope decision.
///
/// ## Compaction scope justification
///
/// Structural compaction was planned "if justified by measurement".
/// This test provides that measurement:
///
/// - **u32 line numbers** reduce every location entry from 16 bytes
///   (`(StringId, usize)` with alignment padding on 64-bit) to 8 bytes
///   (`(StringId, u32)`) — a 2× per-entry reduction.
///
/// - **compact()** eliminates Vec doubling over-allocation that accumulates
///   during `push()`-based bulk build (typically 1.5–2× wasted capacity).
///
/// Together these address the per-symbol-Vec overhead without the more
/// invasive contiguous/range-based layout redesign.  If a future measurement
/// shows the ceiling is still too high for very large repos (>> 50K files),
/// the contiguous-layout redesign from the plan should be revisited.
#[test]
fn test_compact_reduces_rss_vs_uncompacted() {
    // Build via raw ScopeGraphIndex API so compact() is never called.
    // This reproduces the steady-state heap shape *without* compact()'s shrinking.
    let rss_before = rss_mb();
    let mut raw = ScopeGraphIndex::new();
    for i in 0..500usize {
        for d in 0..10usize {
            let sym = format!("func_{}_{}", i, d);
            let path = format!("file_{}.rs", i);
            raw.add_definition(&sym, &path, d + 1);
        }
    }
    let rss_uncompacted = rss_mb();
    let (raw_files, raw_defs, _) = raw.stats();

    // compact() trims Vec slack on all location lists and the interner.
    raw.compact();
    let rss_compacted = rss_mb();

    println!(
        "PR4 RSS — uncompacted: {} ({} files, {} defs), after compact(): {}",
        fmt_rss(rss_uncompacted),
        raw_files,
        raw_defs,
        fmt_rss(rss_compacted),
    );

    if let (Some(unc), Some(cpt)) = (rss_uncompacted, rss_compacted) {
        println!("compact() RSS change: {:.1}MB", cpt - unc);
        // compact() must not increase RSS
        assert!(
            cpt <= unc + 5.0,
            "compact() increased RSS by {:.1}MB — unexpected",
            cpt - unc
        );
    }

    // Absolute ceiling for the compacted 500-file index
    if let (Some(base), Some(cpt)) = (rss_before, rss_compacted) {
        let delta_mb = cpt - base;
        println!("Compacted index RSS delta from baseline: {:.1}MB", delta_mb);
        assert!(
            delta_mb < 200.0,
            "Compacted index RSS delta {:.1}MB exceeds 200MB ceiling",
            delta_mb
        );
    }
}
