//! Benchmark for comparing git CLI vs git2 file listing.
//!
//! Usage: cargo run --bin bench_file_listing --release -- [path] [cli|git2|git2-index|both]

use std::path::Path;
use std::process::Command;
use std::time::Instant;

use echo_agent_codebase_graph::LanguageRegistry;
use git2::{Repository, StatusOptions};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path_str = if let Some(p) = args.get(1) {
        p.clone()
    } else if let Ok(p) =
        std::env::var("BENCH_REPO_ROOT").or_else(|_| std::env::var("ECHO_AGENT_ROOT"))
    {
        p
    } else {
        eprintln!("Usage: bench_file_listing <path> [cli|git2|git2-index|both]");
        eprintln!("Or set BENCH_REPO_ROOT to a large checkout to bench against");
        std::process::exit(1);
    };
    let mode = args.get(2).map(|s| s.as_str()).unwrap_or("both");

    let root_path = Path::new(&path_str);
    let registry = LanguageRegistry::new();

    match mode {
        "cli" => {
            let start = Instant::now();
            let files = collect_files_cli(root_path, &registry);
            let elapsed = start.elapsed();
            println!("CLI: {} files in {:?}", files.len(), elapsed);
        }
        "git2" => {
            let start = Instant::now();
            let files = collect_files_git2(root_path, &registry);
            let elapsed = start.elapsed();
            println!("git2: {} files in {:?}", files.len(), elapsed);
        }
        "git2-index" => {
            let start = Instant::now();
            let files = collect_files_git2_index_only(root_path, &registry);
            let elapsed = start.elapsed();
            println!("git2 (index only): {} files in {:?}", files.len(), elapsed);
        }
        _ => {
            // Run all three methods multiple times for comparison
            println!("Benchmarking file listing for: {}", root_path.display());
            println!();

            let iterations = 5;

            // Warm up
            let _ = collect_files_cli(root_path, &registry);
            let _ = collect_files_git2(root_path, &registry);
            let _ = collect_files_git2_index_only(root_path, &registry);

            // CLI benchmark
            let mut cli_times = Vec::with_capacity(iterations);
            let mut cli_count = 0;
            for _ in 0..iterations {
                let start = Instant::now();
                let files = collect_files_cli(root_path, &registry);
                cli_times.push(start.elapsed());
                cli_count = files.len();
            }

            // git2 benchmark (with untracked)
            let mut git2_times = Vec::with_capacity(iterations);
            let mut git2_count = 0;
            for _ in 0..iterations {
                let start = Instant::now();
                let files = collect_files_git2(root_path, &registry);
                git2_times.push(start.elapsed());
                git2_count = files.len();
            }

            // git2 index-only benchmark
            let mut git2_index_times = Vec::with_capacity(iterations);
            let mut git2_index_count = 0;
            for _ in 0..iterations {
                let start = Instant::now();
                let files = collect_files_git2_index_only(root_path, &registry);
                git2_index_times.push(start.elapsed());
                git2_index_count = files.len();
            }

            // Print results
            let cli_avg = cli_times.iter().sum::<std::time::Duration>() / iterations as u32;
            let git2_avg = git2_times.iter().sum::<std::time::Duration>() / iterations as u32;
            let git2_index_avg =
                git2_index_times.iter().sum::<std::time::Duration>() / iterations as u32;

            println!("Results ({} iterations):", iterations);
            println!(
                "  CLI:               {} files, avg {:?}",
                cli_count, cli_avg
            );
            println!(
                "  git2 (+ untracked): {} files, avg {:?}",
                git2_count, git2_avg
            );
            println!(
                "  git2 (index only):  {} files, avg {:?}",
                git2_index_count, git2_index_avg
            );
            println!();

            let speedup = cli_avg.as_secs_f64() / git2_index_avg.as_secs_f64();
            if speedup > 1.0 {
                println!("git2 (index only) is {:.2}x faster than CLI", speedup);
            } else {
                println!("CLI is {:.2}x faster than git2 (index only)", 1.0 / speedup);
            }
        }
    }
}

/// Collect files using git CLI (original approach)
fn collect_files_cli(root_path: &Path, registry: &LanguageRegistry) -> Vec<std::path::PathBuf> {
    // Get tracked files
    let tracked_output = Command::new("git")
        .args(["ls-files"])
        .current_dir(root_path)
        .output();

    let tracked_output = match tracked_output {
        Ok(o) if o.status.success() => o,
        _ => return vec![],
    };

    // Get untracked files
    let untracked_output = Command::new("git")
        .args(["ls-files", "--others", "--exclude-standard"])
        .current_dir(root_path)
        .output()
        .ok();

    let tracked_str = String::from_utf8_lossy(&tracked_output.stdout);
    let mut files: Vec<std::path::PathBuf> = tracked_str
        .lines()
        .filter(|line| registry.is_supported(Path::new(line)))
        .map(|line| root_path.join(line))
        .collect();

    if let Some(output) = untracked_output
        && output.status.success()
    {
        let untracked_str = String::from_utf8_lossy(&output.stdout);
        let untracked_files: Vec<std::path::PathBuf> = untracked_str
            .lines()
            .filter(|line| registry.is_supported(Path::new(line)))
            .map(|line| root_path.join(line))
            .collect();
        files.extend(untracked_files);
    }

    files
}

/// Collect files using git2 (new approach)
fn collect_files_git2(root_path: &Path, registry: &LanguageRegistry) -> Vec<std::path::PathBuf> {
    let repo = match Repository::open(root_path) {
        Ok(r) => r,
        Err(_) => return vec![],
    };

    let index = match repo.index() {
        Ok(i) => i,
        Err(_) => return vec![],
    };

    let mut files: Vec<std::path::PathBuf> = index
        .iter()
        .filter_map(|entry| {
            let path_str = std::str::from_utf8(&entry.path).ok()?;
            if registry.is_supported(Path::new(path_str)) {
                Some(root_path.join(path_str))
            } else {
                None
            }
        })
        .collect();

    // Get untracked files
    let mut status_opts = StatusOptions::new();
    status_opts
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .exclude_submodules(true);

    if let Ok(statuses) = repo.statuses(Some(&mut status_opts)) {
        for status_entry in statuses.iter() {
            if status_entry.status().is_wt_new()
                && let Some(path_str) = status_entry.path()
                && registry.is_supported(Path::new(path_str))
            {
                files.push(root_path.join(path_str));
            }
        }
    }

    files
}

/// Collect files using git2 index only (tracked files only, no untracked)
fn collect_files_git2_index_only(
    root_path: &Path,
    registry: &LanguageRegistry,
) -> Vec<std::path::PathBuf> {
    let repo = match Repository::open(root_path) {
        Ok(r) => r,
        Err(_) => return vec![],
    };

    let index = match repo.index() {
        Ok(i) => i,
        Err(_) => return vec![],
    };

    index
        .iter()
        .filter_map(|entry| {
            let path_str = std::str::from_utf8(&entry.path).ok()?;
            if registry.is_supported(Path::new(path_str)) {
                Some(root_path.join(path_str))
            } else {
                None
            }
        })
        .collect()
}
