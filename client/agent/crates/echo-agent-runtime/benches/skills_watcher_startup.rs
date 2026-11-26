//! Skills file-watcher startup latency.
//!
//! Times OS watch registration for a project-tier `.claude` tree with a large
//! `worktrees/` subtree (Bazel-like fan-out). Compares:
//!
//! - **scoped** — current `SkillsFileWatcher::start_with_dirs` (vendor root
//!   non-recursive + skills/commands/workflows only)
//! - **recursive_control** — full `RecursiveMode::Recursive` on `.claude`
//!   (pre-fix project-tier behavior on Linux: one inotify wd per directory)
//!
//! Fixture sizes stay comparable across scenarios. Medians land under
//! `target/criterion/skills_watcher_startup/`.
//!
//! ```text
//! cargo bench -p echo-agent-runtime --bench skills_watcher_startup
//! # optional scale:
//! ECHO_AGENT_SKILLS_WATCHER_BENCH_DIRS=12000 cargo bench -p echo-agent-runtime --bench skills_watcher_startup
//! ```
//!
//! On macOS, recursive FSEvents is cheap so both arms may be close. On Linux
//! inotify, `recursive_control` scales with directory count; `scoped` stays flat.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use echo_agent_runtime::config::watcher::SkillsFileWatcher;
use notify::RecursiveMode;
use notify_debouncer_mini::new_debouncer;
use tempfile::TempDir;

/// Default dirs under `.claude/worktrees/` (override with env).
const DEFAULT_WORKTREE_DIRS: usize = 6_000;

fn worktree_dir_count() -> usize {
    std::env::var("ECHO_AGENT_SKILLS_WATCHER_BENCH_DIRS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_WORKTREE_DIRS)
}

/// Nested groups of 100 so the tree has width and depth.
fn make_nested_dirs(base: &Path, count: usize) {
    for i in 0..count {
        let dir = base.join(format!("g{}", i / 100)).join(format!("d{i}"));
        fs::create_dir_all(&dir).unwrap();
    }
}

struct Fixture {
    _root: TempDir,
    project: PathBuf,
    claude: PathBuf,
    echo_agent_home: PathBuf,
}

/// Project with a real skill and a fat `.claude/worktrees` tree.
fn build_fixture(worktree_dirs: usize) -> Fixture {
    let root = TempDir::new().unwrap();
    let project = root.path().join("project");
    let claude = project.join(".claude");
    let skills = claude.join("skills").join("alpha");
    fs::create_dir_all(&skills).unwrap();
    fs::write(skills.join("SKILL.md"), "# alpha\n").unwrap();

    let worktrees = claude.join("worktrees").join("wt1");
    make_nested_dirs(&worktrees, worktree_dirs);

    let echo_agent_home = root.path().join("echo-agent-home");
    fs::create_dir_all(&echo_agent_home).unwrap();

    Fixture {
        _root: root,
        project,
        claude,
        echo_agent_home,
    }
}

fn start_scoped(fixture: &Fixture) -> SkillsFileWatcher {
    let dirs = vec![fixture.claude.clone()];
    let (watcher, _rx) = SkillsFileWatcher::start_with_dirs(
        &dirs,
        &fixture.echo_agent_home,
        Some(fixture.project.as_path()),
    )
    .expect("scoped skills watcher should start");
    watcher
}

/// Pre-fix control: one recursive watch on the whole project `.claude`.
fn start_recursive_control(
    claude: &Path,
) -> notify_debouncer_mini::Debouncer<notify::RecommendedWatcher> {
    let mut debouncer = new_debouncer(Duration::from_secs(2), |_| {}).expect("debouncer");
    debouncer
        .watcher()
        .watch(claude, RecursiveMode::Recursive)
        .expect("recursive watch");
    debouncer
}

fn bench_skills_watcher_startup(c: &mut Criterion) {
    let n = worktree_dir_count();
    let fixture = build_fixture(n);

    eprintln!(
        "skills_watcher_startup fixture: project={:?} worktree_dirs={n}",
        fixture.project
    );

    let mut group = c.benchmark_group("skills_watcher_startup");
    group.sample_size(20);
    group.throughput(Throughput::Elements(n as u64));
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(8));

    group.bench_function(BenchmarkId::new("scoped", n), |b| {
        b.iter_batched(|| (), |()| start_scoped(&fixture), BatchSize::PerIteration);
    });

    group.bench_function(BenchmarkId::new("recursive_control", n), |b| {
        b.iter_batched(
            || (),
            |()| start_recursive_control(&fixture.claude),
            BatchSize::PerIteration,
        );
    });

    // Tiny tree: both arms should be similar (fixed overhead check).
    let tiny = build_fixture(0);
    group.throughput(Throughput::Elements(1));
    group.bench_function(BenchmarkId::new("scoped_tiny", 0), |b| {
        b.iter_batched(|| (), |()| start_scoped(&tiny), BatchSize::PerIteration);
    });
    group.bench_function(BenchmarkId::new("recursive_control_tiny", 0), |b| {
        b.iter_batched(
            || (),
            |()| start_recursive_control(&tiny.claude),
            BatchSize::PerIteration,
        );
    });

    group.finish();
}

criterion_group!(benches, bench_skills_watcher_startup);
criterion_main!(benches);
