//! Product NFS worktree create latency sampler.
//!
//! Measures `WorktreeBuilder::create` with [`NfsWorktreeOpts`]: the product
//! path, including unbounded `mdutil`/`tmutil`. That number is
//! `NFS_WT_CREATE_PRODUCT_MS`. It is **not** grove's library
//! `NFS_WT_CREATE_MS` (prepare + read-only finish_mount).
//!
//! `--stamp` is release-only and refuses a non-`nfs` strategy. Debug builds
//! print a warning and refuse `--stamp`. Not run in CI.
//!
//! macOS + a live grove daemon. Linux compiles this bin and exits 2.

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;

#[derive(Parser)]
#[command(name = "nfs-create-latency-bench")]
#[command(
    about = "Product NFS create latency sampler (NFS_WT_CREATE_PRODUCT_MS, not NFS_WT_CREATE_MS)"
)]
struct Cli {
    /// Source repository (ignored when --synthetic-files is set)
    #[arg(long, default_value = ".")]
    source: PathBuf,

    /// Build a clean committed repo with this many tracked files and use it
    #[arg(long)]
    synthetic_files: Option<usize>,

    #[arg(long, default_value = "3")]
    iterations: usize,

    /// Grove control socket (default: $GROVE_CONTROL_SOCK / $XDG_RUNTIME_DIR/grove/control.sock)
    #[arg(long)]
    control_sock: Option<PathBuf>,

    #[arg(long)]
    data_dir: Option<PathBuf>,

    #[arg(long)]
    runtime_dir: Option<PathBuf>,

    /// Fail if dispatch did not adopt NFS (default: true)
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    require_nfs: bool,

    /// Also time the clonefile/copy arm for comparison (not an NFS number)
    #[arg(long)]
    copy_compare: bool,

    /// Print `GROVE_BASELINE_NFS_WT_CREATE_PRODUCT_MS=... n=... release=yes host=...`
    #[arg(long)]
    stamp: bool,

    #[arg(long)]
    json: bool,
}

#[cfg(not(target_os = "macos"))]
fn main() -> Result<()> {
    let _ = Cli::parse();
    eprintln!("nfs-create-latency-bench: NFS worktrees are macOS-only.");
    eprintln!("This binary compiles on Linux so CI typechecks; it does not sample NFS_WT_*.");
    std::process::exit(2);
}

#[cfg(target_os = "macos")]
fn main() -> Result<()> {
    mac::run()
}

#[cfg(target_os = "macos")]
mod mac {
    use super::Cli;
    use anyhow::{Context, Result, bail};
    use clap::Parser;
    use echo_agent_fast_worktree::create_latency_stamp::{
        LIBRARY_CREATE_ENV, format_create_p50, format_create_stamp,
    };
    use echo_agent_fast_worktree::{
        CreationMode, NfsWorktreeOpts, WorkingTreeMode, WorktreeBuilder, remove_worktree,
    };
    use std::ffi::CString;
    use std::fs;
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
    use std::time::Instant;

    /// Signal-safe dest path. Handler only loads the pointer and calls
    /// `unmount`/`umount` + `_exit` (no Mutex, no spawn).
    static LIVE_DEST: AtomicPtr<libc::c_char> = AtomicPtr::new(std::ptr::null_mut());
    static TEARDOWN_FAILED: AtomicBool = AtomicBool::new(false);

    fn nfs_mount_count() -> usize {
        let mut cmd = Command::new("mount");
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        echo_agent_tty_utils::detach_std_command(&mut cmd);
        let Ok(out) = cmd.output() else {
            return 0;
        };
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| l.contains(" nfs") || l.contains("(nfs"))
            .count()
    }

    fn store_live_dest(dest: &Path) {
        let c = CString::new(dest.as_os_str().as_bytes()).unwrap_or_default();
        let p = c.into_raw();
        let old = LIVE_DEST.swap(p, Ordering::SeqCst);
        if !old.is_null() {
            // SAFETY: `old` came from `CString::into_raw` in this module.
            unsafe {
                drop(CString::from_raw(old));
            }
        }
    }

    fn clear_live_dest() {
        let old = LIVE_DEST.swap(std::ptr::null_mut(), Ordering::SeqCst);
        if !old.is_null() {
            // SAFETY: `old` came from `CString::into_raw` in this module.
            unsafe {
                drop(CString::from_raw(old));
            }
        }
    }

    fn unmount_c_path(p: *const libc::c_char) {
        if p.is_null() {
            return;
        }
        #[cfg(target_os = "macos")]
        // SAFETY: `p` is a live CString from `store_live_dest` or null-checked.
        unsafe {
            libc::unmount(p, 0);
        }
        #[cfg(not(target_os = "macos"))]
        // SAFETY: `p` is a live CString from `store_live_dest` or null-checked.
        unsafe {
            libc::umount(p);
        }
    }

    /// Last-resort unmount for a dest `remove_worktree` left mounted.
    /// Signal-safe body is the same syscall; this wrapper also clears the
    /// pointer so `TempDir` drop cannot rmdir a live NFS dest.
    fn emergency_unmount_live() {
        let p = LIVE_DEST.load(Ordering::SeqCst);
        unmount_c_path(p);
        clear_live_dest();
    }

    /// Drops before the scratch `TempDir` so a failed DestGuard teardown
    /// still unmounts on every `run()` exit (Ok, Err, panic).
    struct UnmountLiveOnDrop;
    impl Drop for UnmountLiveOnDrop {
        fn drop(&mut self) {
            if !LIVE_DEST.load(Ordering::SeqCst).is_null() {
                emergency_unmount_live();
            }
        }
    }

    struct DestGuard {
        dest: PathBuf,
    }

    impl DestGuard {
        fn arm(dest: PathBuf) -> anyhow::Result<Self> {
            // Do not swap LIVE_DEST while a prior dest is still armed: swap
            // frees the old CString and SIGINT can no longer unmount the leak.
            if TEARDOWN_FAILED.load(Ordering::SeqCst) || !LIVE_DEST.load(Ordering::SeqCst).is_null()
            {
                anyhow::bail!(
                    "previous dest still armed / teardown failed; refusing next iteration"
                );
            }
            store_live_dest(&dest);
            Ok(Self { dest })
        }
    }

    impl Drop for DestGuard {
        fn drop(&mut self) {
            // Keep LIVE_DEST armed until unmount succeeds so SIGINT can still
            // unmount a leaked NFS dest. Clearing on failure disarms the
            // handler and hides the leak.
            if let Err(e) = remove_worktree(&self.dest) {
                eprintln!(
                    "ERROR: remove_worktree({}) failed: {e}",
                    self.dest.display()
                );
                TEARDOWN_FAILED.store(true, Ordering::SeqCst);
                return;
            }
            clear_live_dest();
        }
    }

    extern "C" fn handle_sigint(_: libc::c_int) {
        unmount_c_path(LIVE_DEST.load(Ordering::SeqCst));
        // SAFETY: `_exit` is async-signal-safe and skips Rust dtors on purpose.
        unsafe { libc::_exit(130) };
    }

    fn install_sigint_guard() {
        // SAFETY: handler only calls async-signal-safe unmount + `_exit`.
        unsafe {
            libc::signal(
                libc::SIGINT,
                handle_sigint as *const () as libc::sighandler_t,
            );
            libc::signal(
                libc::SIGTERM,
                handle_sigint as *const () as libc::sighandler_t,
            );
        }
    }

    #[derive(Debug, Clone)]
    struct Iter {
        total_ms: f64,
        strategy: String,
    }

    fn git(cwd: &Path, args: &[&str]) -> Result<()> {
        let mut cmd = grove_git::hermetic_git_command().context("hermetic git")?;
        cmd.args(args)
            .current_dir(cwd)
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let out = cmd.output().with_context(|| format!("git {args:?}"))?;
        if !out.status.success() {
            bail!(
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }

    fn seed_clean_repo(root: &Path, n: usize) -> Result<PathBuf> {
        let src = root.join("src");
        fs::create_dir_all(&src)?;
        git(&src, &["init", "-b", "main"])?;
        git(&src, &["config", "user.email", "grove@test"])?;
        git(&src, &["config", "user.name", "Grove"])?;
        git(&src, &["config", "core.untrackedCache", "true"])?;
        let dirs = n.clamp(1, 100);
        for d in 0..dirs {
            fs::create_dir_all(src.join(format!("d{d:02}")))?;
        }
        for i in 0..n {
            let d = i % dirs;
            fs::write(src.join(format!("d{d:02}/f{i:05}.txt")), format!("{i}\n"))?;
        }
        git(&src, &["add", "-A"])?;
        git(&src, &["commit", "-qm", "seed"])?;
        Ok(src)
    }

    fn assert_clean_shape(src: &Path, expect: Option<usize>) -> Result<usize> {
        let tracked = echo_agent_fast_worktree::count_tracked_files(src)
            .with_context(|| format!("count_tracked_files {}", src.display()))?;
        if let Some(n) = expect
            && tracked != n
        {
            bail!("fixture tracked {tracked} != --synthetic-files {n}");
        }
        if tracked == 0 {
            bail!("source has 0 tracked files");
        }
        let mut cmd = grove_git::hermetic_git_command().context("hermetic git")?;
        cmd.args(["status", "--porcelain", "-z"]).current_dir(src);
        let out = cmd.output().context("git status --porcelain")?;
        if !out.stdout.is_empty() {
            bail!(
                "source {} is dirty; product sampler requires a clean porcelain tree",
                src.display()
            );
        }
        Ok(tracked)
    }

    fn nfs_opts(cli: &Cli) -> NfsWorktreeOpts {
        let mut opts = NfsWorktreeOpts {
            enabled: true,
            ..NfsWorktreeOpts::default()
        };
        opts.control_sock = cli.control_sock.clone();
        opts.data_dir = cli.data_dir.clone();
        opts.runtime_dir = cli.runtime_dir.clone();
        opts
    }

    fn create_once(source: &Path, dest: &Path, nfs: Option<NfsWorktreeOpts>) -> Result<Iter> {
        let mut b = WorktreeBuilder::new(source, dest)
            .creation_mode(CreationMode::Linked)
            .working_tree_mode(WorkingTreeMode::PreserveWorkingTree);
        if let Some(opts) = nfs {
            b = b.nfs_worktree(opts);
        }
        let t0 = Instant::now();
        let report = b.create().context("WorktreeBuilder::create")?;
        let total_ms = t0.elapsed().as_secs_f64() * 1000.0;
        Ok(Iter {
            total_ms,
            strategy: report.resolved_strategy.to_owned(),
        })
    }

    fn mean_ms(iters: &[Iter]) -> f64 {
        match iters.len() {
            0 => 0.0,
            n => iters.iter().map(|i| i.total_ms).sum::<f64>() / n as f64,
        }
    }

    fn median_ms(iters: &[Iter]) -> f64 {
        if iters.is_empty() {
            return 0.0;
        }
        let mut v: Vec<f64> = iters.iter().map(|i| i.total_ms).collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mid = v.len() / 2;
        if v.len() % 2 == 1 {
            v[mid]
        } else {
            (v[mid - 1] + v[mid]) / 2.0
        }
    }

    fn stamp_host() -> String {
        let mut cmd = Command::new("sw_vers");
        cmd.arg("-productVersion");
        echo_agent_tty_utils::detach_std_command(&mut cmd);
        if let Ok(out) = cmd.output() {
            let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !v.is_empty() {
                return format!("{v}-{}", std::env::consts::ARCH);
            }
        }
        format!("macos-{}", std::env::consts::ARCH)
    }

    pub(super) fn run() -> Result<()> {
        let cli = Cli::parse();
        install_sigint_guard();
        if cfg!(debug_assertions) {
            eprintln!(
                "WARNING: debug build — do not stamp {LIBRARY_CREATE_ENV} or product create from this run."
            );
            if cli.stamp {
                bail!("--stamp requires `cargo run --release`");
            }
        }

        let scratch = tempfile::Builder::new()
            .prefix("nfs-create-latency-")
            .tempdir()
            .context("tempdir")?;
        // Constructed after scratch so it drops first and unmounts before rmdir.
        let _unmount_live = UnmountLiveOnDrop;
        let (source, expect_n) = if let Some(n) = cli.synthetic_files {
            if n == 0 {
                bail!("--synthetic-files must be > 0");
            }
            (seed_clean_repo(scratch.path(), n)?, Some(n))
        } else {
            let src = dunce::canonicalize(&cli.source).context("source")?;
            (src, None)
        };
        let tracked = assert_clean_shape(&source, expect_n)?;

        if !cli.json {
            eprintln!("source={} tracked={tracked}", source.display());
            eprintln!(
                "iterations={} require_nfs={} copy_compare={} release={} (product window, not {})",
                cli.iterations,
                cli.require_nfs,
                cli.copy_compare,
                !cfg!(debug_assertions),
                LIBRARY_CREATE_ENV
            );
        }

        let mounts_before = nfs_mount_count();
        if cli.stamp && mounts_before != 0 {
            bail!("--stamp requires zero leftover NFS mounts at start (got {mounts_before})");
        }
        let mut nfs_iters = Vec::new();
        for i in 0..cli.iterations {
            let dest = scratch.path().join(format!("nfs-{i}"));
            let _guard = DestGuard::arm(dest.clone())?;
            let iter = create_once(&source, &dest, Some(nfs_opts(&cli)))?;
            if cli.require_nfs && iter.strategy != "nfs" {
                bail!(
                    "expected resolved_strategy=nfs, got {} (daemon down / declined / copy fallback). \
                     Refusing to report a copy-path number as NFS_WT_CREATE_PRODUCT_MS.",
                    iter.strategy
                );
            }
            if !cli.json {
                eprintln!(
                    "  product iter {} strategy={} {:.1} ms",
                    i + 1,
                    iter.strategy,
                    iter.total_ms
                );
            }
            nfs_iters.push(iter);
        }

        let all_nfs = !nfs_iters.is_empty() && nfs_iters.iter().all(|i| i.strategy == "nfs");
        let nfs_mean = mean_ms(&nfs_iters);
        let nfs_p50 = median_ms(&nfs_iters);
        let mut copy_mean = None;
        let mut copy_p50 = None;
        if cli.copy_compare {
            let mut copy_iters = Vec::new();
            for i in 0..cli.iterations {
                let dest = scratch.path().join(format!("copy-{i}"));
                let _guard = DestGuard::arm(dest.clone())?;
                let iter = create_once(&source, &dest, None)?;
                if !cli.json {
                    eprintln!(
                        "  copy iter {} strategy={} {:.1} ms",
                        i + 1,
                        iter.strategy,
                        iter.total_ms
                    );
                }
                copy_iters.push(iter);
            }
            copy_mean = Some(mean_ms(&copy_iters));
            copy_p50 = Some(median_ms(&copy_iters));
        }

        // Do not key the stamp / NFS mean label off the first iteration when
        // later samples fell back to copy (`--require-nfs false`).
        let strategy = if all_nfs {
            "nfs"
        } else if nfs_iters.iter().any(|i| i.strategy == "nfs") {
            "mixed"
        } else {
            nfs_iters
                .first()
                .map(|i| i.strategy.as_str())
                .unwrap_or("unknown")
        };
        let p50_line = format_create_p50(strategy, nfs_p50, tracked, nfs_iters.len());
        if cli.json {
            println!("{{");
            println!("  \"tracked_files\": {tracked},");
            println!("  \"release\": {},", !cfg!(debug_assertions));
            println!("  \"strategy\": {strategy:?},");
            if !all_nfs {
                println!("  \"product_create_p50_ms\": null,");
                println!("  \"product_create_mean_ms\": null,");
            } else {
                println!("  \"product_create_p50_ms\": {nfs_p50:.3},");
                println!("  \"product_create_mean_ms\": {nfs_mean:.3},");
            }
            match copy_p50 {
                Some(c) => println!("  \"copy_create_p50_ms\": {c:.3},"),
                None => println!("  \"copy_create_p50_ms\": null,"),
            }
            match copy_mean {
                Some(c) => println!("  \"copy_create_mean_ms\": {c:.3},"),
                None => println!("  \"copy_create_mean_ms\": null,"),
            }
            println!("  \"iterations\": [");
            for (i, it) in nfs_iters.iter().enumerate() {
                let comma = if i + 1 < nfs_iters.len() { "," } else { "" };
                println!(
                    "    {{ \"strategy\": {:?}, \"total_ms\": {:.3} }}{comma}",
                    it.strategy, it.total_ms
                );
            }
            println!("  ]");
            println!("}}");
        } else {
            println!("{p50_line}");
            if let Some(c) = copy_p50 {
                println!("copy compare p50={c:.3} ms (not an NFS number)");
            }
        }

        let mounts_after = nfs_mount_count();
        if TEARDOWN_FAILED.load(Ordering::SeqCst)
            || !LIVE_DEST.load(Ordering::SeqCst).is_null()
            || mounts_after > mounts_before
        {
            bail!(
                "bench leaked NFS dest/mount: teardown_failed={} live_dest_armed={} mounts {mounts_before}->{mounts_after}",
                TEARDOWN_FAILED.load(Ordering::SeqCst),
                !LIVE_DEST.load(Ordering::SeqCst).is_null()
            );
        }

        if cli.stamp {
            if !all_nfs {
                bail!(
                    "--stamp requires every product iteration to be strategy=nfs \
                     (got {strategy}; --require-nfs already defaults to true — \
                     do not pass --require-nfs false)"
                );
            }
            let line = format_create_stamp(
                strategy,
                nfs_p50,
                tracked,
                !cfg!(debug_assertions),
                &stamp_host(),
            )
            .map_err(|e| anyhow::anyhow!("{e}"))?;
            // Stamp is labelled text, not JSON. Keep stdout parseable when --json.
            if cli.json {
                eprintln!("{line}");
            } else {
                println!("{line}");
            }
        }

        Ok(())
    }
}
