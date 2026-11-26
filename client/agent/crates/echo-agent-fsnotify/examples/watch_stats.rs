//! Watch-footprint benchmark harness.
//!
//! Measures what a live `FsEventSource` costs the OS: watch count (crate
//! accounting + `/proc/self/fdinfo` inotify ground truth on Linux) and
//! startup latency, under either strategy (`ECHO_AGENT_FSNOTIFY_PER_DIR=0|1`).
//!
//! ```bash
//! # Generate a synthetic tree, then measure both strategies against it:
//! cargo run --release -p echo-agent-fsnotify --example watch_stats -- gen js /tmp/js-repo
//! ECHO_AGENT_FSNOTIFY_PER_DIR=0 cargo run --release -p echo-agent-fsnotify --example watch_stats -- run /tmp/js-repo 5
//! ECHO_AGENT_FSNOTIFY_PER_DIR=1 cargo run --release -p echo-agent-fsnotify --example watch_stats -- run /tmp/js-repo 5
//! ```
//!
//! Tree shapes are scaled replicas of synthetic large-repo measurements:
//! - `js`: a JS/turbo monorepo where `node_modules/` trees nested below the
//!   top level dominate the directory count (the shape behind the original
//!   "echoagent holds 55k inotify watches" report).
//! - `large`: a wide multi-language monorepo — 44 top-level dirs, ~52k
//!   non-ignored dirs, ~7k nested-ignored, a large top-level `target/`, and
//!   a `.git` with 13k+ internal dirs (objects/modules/logs/refs-remotes).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use echo_agent_fsnotify::{FsConfig, FsEventSource};

fn make_dirs(base: &Path, count: usize, fanout: usize) {
    for i in 0..count {
        let dir = base.join(format!("g{}", i / fanout)).join(format!("d{i}"));
        fs::create_dir_all(&dir).unwrap();
    }
}

fn make_git_dir(root: &Path, objects: usize, logs: usize, remotes: usize, modules: usize) {
    let gd = root.join(".git");
    fs::create_dir_all(gd.join("refs/heads")).unwrap();
    fs::create_dir_all(gd.join("refs/tags")).unwrap();
    fs::write(gd.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    fs::write(gd.join("index"), "").unwrap();
    for i in 0..objects {
        fs::create_dir_all(gd.join(format!("objects/{i:02x}"))).unwrap();
    }
    for i in 0..remotes {
        fs::create_dir_all(gd.join(format!("refs/remotes/origin/user{}/f{i}", i / 40))).unwrap();
    }
    for i in 0..logs {
        fs::create_dir_all(gd.join(format!("logs/refs/remotes/origin/user{}/f{i}", i / 40)))
            .unwrap();
    }
    for i in 0..modules {
        fs::create_dir_all(gd.join(format!("modules/sub{}/objects/{:02x}", i / 30, i % 256)))
            .unwrap();
    }
}

/// JS monorepo: 3 apps + 12 packages of sources (~3.2k non-ignored dirs) and
/// nested `node_modules/` holding ~29k dirs — ignored, below the top level.
fn gen_js(root: &Path) {
    fs::write(root.join(".gitignore"), "node_modules/\ndist/\n").unwrap();
    make_git_dir(root, 256, 400, 120, 0);
    for app in 0..3 {
        let a = root.join(format!("apps/app{app}"));
        make_dirs(&a.join("src"), 150, 12);
        make_dirs(&a.join("node_modules"), 4500, 15);
        make_dirs(&a.join("dist"), 300, 20);
    }
    for pkg in 0..12 {
        let p = root.join(format!("packages/pkg{pkg}"));
        make_dirs(&p.join("src"), 120, 10);
        make_dirs(&p.join("node_modules"), 1200, 15);
    }
    // Root node_modules: the hoisted bulk.
    make_dirs(&root.join("node_modules"), 12000, 15);
    fs::write(root.join("package.json"), "{}").unwrap();
}

/// Wide multi-language monorepo shape (scaled synthetic distribution).
fn gen_large(root: &Path) {
    fs::write(root.join(".gitignore"), "target/\nnode_modules/\n.venv/\n").unwrap();
    make_git_dir(root, 256, 9000, 2500, 800);
    // 44 top-level dirs; weights exercise a realistic wide fan-out.
    let weights: &[(&str, usize)] = &[
        ("apps", 23_000),
        ("services", 6_600),
        ("crates", 5_100),
        ("frontend", 4_600),
        ("python", 3_400),
        ("tools", 1_800),
        ("libs", 1_800),
        ("infra", 1_500),
    ];
    for (name, dirs) in weights {
        make_dirs(&root.join(name), *dirs, 40);
    }
    for i in 0..36 {
        make_dirs(&root.join(format!("misc{i}")), 100, 20);
    }
    // Nested ignored trees (~7.4k dirs): per-language build/dep dirs.
    make_dirs(&root.join("frontend/node_modules"), 4_000, 15);
    make_dirs(&root.join("python/common/.venv"), 2_000, 20);
    make_dirs(&root.join("crates/foo/target"), 1_400, 30);
    // Top-level ignored target/ (~34k dirs): fan-out already skips it; the
    // recursive-root fallback (>64 top-level dirs) would not.
    make_dirs(&root.join("target"), 34_000, 50);
}

/// Ground truth: total inotify watches held by this process (Linux).
fn inotify_watches() -> usize {
    #[cfg(target_os = "linux")]
    {
        let Ok(fds) = fs::read_dir("/proc/self/fdinfo") else {
            return 0;
        };
        fds.flatten()
            .filter_map(|e| fs::read_to_string(e.path()).ok())
            .map(|s| s.lines().filter(|l| l.starts_with("inotify wd:")).count())
            .sum()
    }
    #[cfg(not(target_os = "linux"))]
    {
        0
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("gen") => {
            let kind = args.get(2).expect("gen <js|large> <path>");
            let path = PathBuf::from(args.get(3).expect("gen <js|large> <path>"));
            fs::create_dir_all(&path).unwrap();
            let t = Instant::now();
            match kind.as_str() {
                "js" => gen_js(&path),
                "large" => gen_large(&path),
                other => panic!("unknown tree kind: {other}"),
            }
            let total = walkdir_count(&path);
            println!(
                "generated {kind} tree at {} ({total} dirs) in {:?}",
                path.display(),
                t.elapsed()
            );
        }
        Some("run") => {
            let path = PathBuf::from(args.get(2).expect("run <path> [iters]"));
            let iters: usize = args.get(3).map_or(3, |s| s.parse().unwrap());
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let _guard = rt.enter();

            let strategy = std::env::var("ECHO_AGENT_FSNOTIFY_PER_DIR").unwrap_or_default();
            let mut ready_ms = Vec::new();
            let mut armed_ms = Vec::new();
            let mut counts = (0usize, 0usize);
            for _ in 0..iters {
                let t = Instant::now();
                let source =
                    FsEventSource::start(path.clone(), FsConfig::default()).expect("watcher start");
                ready_ms.push(t.elapsed().as_secs_f64() * 1e3);
                // Steady state: background arming (per-dir mode on big trees)
                // is done once the kernel watch count stops moving.
                let mut last = inotify_watches();
                let deadline = Instant::now() + std::time::Duration::from_secs(120);
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(250));
                    let n = inotify_watches();
                    if n == last || Instant::now() > deadline {
                        break;
                    }
                    last = n;
                }
                armed_ms.push(t.elapsed().as_secs_f64() * 1e3);
                counts = (source.os_watch_count(), inotify_watches());
                drop(source);
                // Let the watcher thread release its watches before re-measuring.
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            ready_ms.sort_by(|a, b| a.total_cmp(b));
            armed_ms.sort_by(|a, b| a.total_cmp(b));
            println!(
                "per_dir_env={strategy:?} tree={} watches_crate={} watches_kernel={} ready_ms_median={:.0} fully_armed_ms_median~{:.0} (n={iters})",
                path.display(),
                counts.0,
                counts.1,
                ready_ms[ready_ms.len() / 2],
                armed_ms[armed_ms.len() / 2],
            );
        }
        _ => eprintln!("usage: watch_stats gen <js|large> <path> | run <path> [iters]"),
    }
}

fn walkdir_count(root: &Path) -> usize {
    let mut n = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        if let Ok(rd) = fs::read_dir(&d) {
            for e in rd.flatten() {
                if e.file_type().is_ok_and(|t| t.is_dir()) {
                    n += 1;
                    stack.push(e.path());
                }
            }
        }
    }
    n
}
