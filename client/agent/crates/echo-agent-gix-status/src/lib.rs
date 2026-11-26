//! Shared helpers for `gix` status scans.
//!
//! `gix-features` `in_parallel` does `spawn_scoped(...).expect("valid name")`.
//! Under `panic=abort` and a tight `RLIMIT_NPROC`, a failed spawn aborts the
//! whole process instead of becoming a recoverable `JoinError`. Cap
//! `index_worktree_options.thread_limit` so produce workers stay within
//! headroom. `Some(0)` means unlimited in gix — never pass 0.

/// Past 8 produce workers a status scan gains no speed, only spawn pressure.
const HARD_CAP: usize = 8;
/// Reserve for non-gix threads; nproc tests use `used + OUTER_RESERVE - 2`.
pub(crate) const OUTER_RESERVE: usize = 8;

const ENV_THREADS: &str = "ECHO_AGENT_GIX_STATUS_THREADS";

/// Pure produce-worker budget. Always `n >= 1`. Caps at 8; shrinks under tight
/// soft nproc headroom (`headroom < 2` → 1).
pub fn compute_gix_status_thread_limit_from(
    cores: usize,
    soft_nproc: Option<usize>,
    threads_used: usize,
) -> usize {
    let cores = cores.max(1);
    let mut limit = cores.min(HARD_CAP);
    if let Some(soft) = soft_nproc {
        let headroom = soft
            .saturating_sub(threads_used)
            .saturating_sub(OUTER_RESERVE);
        if headroom < 2 {
            limit = 1;
        } else {
            limit = limit.min(headroom);
        }
    }
    limit.max(1)
}

/// Production budget (`n >= 1`). Honours `ECHO_AGENT_GIX_STATUS_THREADS=N` for `N >= 1`
/// (forced dial; bypasses nproc). Else cores + soft nproc + thread usage.
pub fn compute_gix_status_thread_limit() -> usize {
    if let Ok(raw) = std::env::var(ENV_THREADS)
        && let Some(n) = parse_env_thread_override(&raw)
    {
        return n;
    }
    let cores = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1);
    compute_gix_status_thread_limit_from(cores, soft_nproc_limit(), threads_used())
}

/// `N >= 1` only; reject `0` and garbage.
fn parse_env_thread_override(raw: &str) -> Option<usize> {
    raw.parse::<usize>().ok().filter(|&n| n >= 1)
}

/// Test helper: `None` = uncapped, `Some(n)` with `n >= 1`. Never `Some(0)`.
fn apply_thread_limit<'repo, P>(
    platform: gix::status::Platform<'repo, P>,
    limit: Option<usize>,
) -> gix::status::Platform<'repo, P>
where
    P: gix::Progress + 'static,
{
    debug_assert!(
        !matches!(limit, Some(0)),
        "Some(0) is unlimited in gix — never pass 0"
    );
    platform.index_worktree_options_mut(|opts| {
        opts.thread_limit = limit;
    })
}

/// Apply [`compute_gix_status_thread_limit`] as `Some(n)` on the status platform.
pub fn with_budgeted_thread_limit<'repo, P>(
    platform: gix::status::Platform<'repo, P>,
) -> gix::status::Platform<'repo, P>
where
    P: gix::Progress + 'static,
{
    apply_thread_limit(platform, Some(compute_gix_status_thread_limit()))
}

#[cfg(unix)]
fn soft_nproc_limit() -> Option<usize> {
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit writes only into local `lim`.
    if unsafe { libc::getrlimit(libc::RLIMIT_NPROC, &mut lim) } != 0 {
        return None;
    }
    if lim.rlim_cur == libc::RLIM_INFINITY {
        return None;
    }
    Some(
        lim.rlim_cur
            .min(usize::MAX as libc::rlim_t)
            .try_into()
            .unwrap_or(usize::MAX),
    )
}

#[cfg(not(unix))]
fn soft_nproc_limit() -> Option<usize> {
    None
}

fn threads_used() -> usize {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        status
            .lines()
            .find_map(|line| {
                line.strip_prefix("Threads:")
                    .and_then(|rest| rest.trim().parse().ok())
            })
            .unwrap_or(1)
    }
    #[cfg(not(target_os = "linux"))]
    {
        1
    }
}

/// Test scan: `Ok(true)` iff a dirty entry ending in `want_suffix` is seen.
/// Errors carry the gix `Debug` form so a `SpawnThread` failure stays
/// distinguishable from a genuinely missed dirty file.
#[cfg(test)]
fn status_finds_suffix(
    repo_path: &std::path::Path,
    thread_limit: Option<usize>,
    want_suffix: &str,
) -> Result<bool, String> {
    use gix::bstr::BString;

    let repo = gix::discover(repo_path).map_err(|e| format!("discover: {e:?}"))?;
    let index_path = repo.git_dir().join("index");
    if index_path.metadata().map_or(true, |m| m.len() == 0) {
        return Err("missing or empty index".into());
    }
    let status = repo
        .status(gix::progress::Discard)
        .map_err(|e| format!("status platform: {e:?}"))?;
    let status = apply_thread_limit(status, thread_limit);
    let iter = status
        .into_index_worktree_iter(Vec::<BString>::new())
        .map_err(|e| format!("into_index_worktree_iter: {e:?}"))?;
    for item in iter {
        let item = item.map_err(|e| format!("status item: {e:?}"))?;
        let path = match &item {
            gix::status::index_worktree::Item::Modification { rela_path, .. } => {
                rela_path.to_string()
            }
            gix::status::index_worktree::Item::DirectoryContents { entry, .. } => {
                entry.rela_path.to_string()
            }
            gix::status::index_worktree::Item::Rewrite { dirwalk_entry, .. } => {
                dirwalk_entry.rela_path.to_string()
            }
        };
        if std::path::Path::new(&path).ends_with(want_suffix) {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent_test_utils::git::run_git;
    use std::path::PathBuf;

    #[test]
    fn compute_from_table() {
        // (cores, soft_nproc, used, expected)
        let cases: &[(usize, Option<usize>, usize, usize)] = &[
            (16, None, 1, 8),
            (4, None, 1, 4),
            (1, None, 1, 1),
            (0, None, 1, 1),
            // headroom = soft - used - OUTER_RESERVE
            (16, Some(100), 1, 8),
            (16, Some(20), 10, 2),
            (16, Some(19), 10, 1),
            (16, Some(18), 10, 1),
            (16, Some(10), 10, 1),
            (16, Some(8), 0, 1),
            (16, Some(9), 0, 1),
            (16, Some(10), 0, 2),
            (32, Some(25), 5, 8),
            (3, Some(25), 5, 3),
        ];
        for &(cores, soft, used, want) in cases {
            let got = compute_gix_status_thread_limit_from(cores, soft, used);
            assert_eq!(got, want, "cores={cores} soft={soft:?} used={used}");
            assert!(got >= 1);
        }
    }

    #[test]
    fn parse_env_thread_override_table() {
        let cases: &[(&str, Option<usize>)] = &[
            ("", None),
            ("0", None),
            ("00", None),
            ("1", Some(1)),
            ("8", Some(8)),
            ("16", Some(16)),
            ("abc", None),
            ("-1", None),
            ("1.5", None),
            (" 1", None),
            ("1 ", None),
        ];
        for &(raw, want) in cases {
            assert_eq!(parse_env_thread_override(raw), want, "raw={raw:?}");
        }
    }

    #[test]
    fn production_compute_always_ge_one() {
        assert!(compute_gix_status_thread_limit() >= 1);
    }

    pub(super) fn temp_repo_with_dirty_file() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        run_git(&root, &["init"]);
        // Sequential index decode: with threads gix-index hard-expects an
        // extension-load spawn, which would skew the nproc children's slot budget.
        run_git(&root, &["config", "index.threads", "1"]);
        // 64 entries keeps gix's computed status worker count >= 2 on any
        // 2+ core host, so the uncapped child reaches the in_parallel workers.
        for i in 0..64 {
            let rel = format!("f{i:02}.txt");
            std::fs::write(root.join(&rel), format!("base {i}\n")).unwrap();
        }
        run_git(&root, &["add", "."]);
        run_git(&root, &["commit", "-m", "seed"]);
        std::fs::write(root.join("f00.txt"), "dirty\n").unwrap();
        (tmp, root)
    }

    #[test]
    fn serial_status_finds_dirty() {
        let (_tmp, root) = temp_repo_with_dirty_file();
        assert!(
            status_finds_suffix(&root, Some(1), "f00.txt").expect("serial scan"),
            "serial status should find dirty f00.txt"
        );
    }

    #[test]
    fn uncapped_status_finds_dirty_on_healthy_host() {
        let (_tmp, root) = temp_repo_with_dirty_file();
        assert!(
            status_finds_suffix(&root, None, "f00.txt").expect("uncapped scan"),
            "uncapped status should find dirty f00.txt on a healthy host"
        );
    }
}

/// Unix subprocess tests that lower `RLIMIT_NPROC` in a **child** process.
/// Parent never touches rlimits (would flake parallel cargo test).
/// Linux is the fidelity target; abort assert is Linux-only.
#[cfg(all(test, unix))]
mod nproc_tests {
    use super::tests::temp_repo_with_dirty_file;
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    const CHILD_ENV: &str = "ECHO_AGENT_GIX_STATUS_NPROC_CHILD";
    const REPO_ENV: &str = "ECHO_AGENT_GIX_STATUS_NPROC_REPO";

    /// Child exit protocol; 0 means the scan survived and saw the dirty file.
    const EXIT_SKIP: i32 = 2;
    const EXIT_BAD_MODE: i32 = 3;
    const EXIT_MISSED_DIRTY: i32 = 4;
    const EXIT_SCAN_ERROR: i32 = 5;
    /// Child stderr markers the parent matches on.
    const SKIP_MARK: &str = "skip-child:";
    const NPROC_HIT_MARK: &str = "nproc-hit:";
    const SCAN_ERROR_MARK: &str = "scan-error:";
    /// Ceiling headroom above the child's own threads; must exceed
    /// `SCAFFOLD_SLOTS` so the fill proves enforcement before refunding.
    const HOLDER_SLACK: u64 = 32;
    /// Fill-loop bound; filling this far past `HOLDER_SLACK` without EAGAIN
    /// means the rlimit is not enforced (CAP_SYS_RESOURCE, macOS semantics).
    const MAX_HOLDERS: usize = 512;
    /// Soft `map_err(SpawnThread)` scaffolding spawns between the fill and the
    /// first hard `expect("valid name")` spawn; see refund comments below.
    /// Verified against vendored gix 0.77.0 / gix-status 0.24.0.
    const SCAFFOLD_SLOTS: usize = 3;

    /// Parked thread pinning one RLIMIT_NPROC slot until released.
    struct Holder {
        release: Arc<AtomicBool>,
        handle: std::thread::JoinHandle<()>,
    }

    impl Holder {
        fn spawn() -> std::io::Result<Holder> {
            let release = Arc::new(AtomicBool::new(false));
            let flag = Arc::clone(&release);
            let handle = std::thread::Builder::new().spawn(move || {
                // Spurious unparks are fine; only the flag ends the hold.
                while !flag.load(Ordering::Acquire) {
                    std::thread::park();
                }
            })?;
            Ok(Holder { release, handle })
        }

        /// Free this holder's slot; join guarantees the kernel task is gone.
        fn release(self) {
            self.release.store(true, Ordering::Release);
            self.handle.thread().unpark();
            let _ = self.handle.join();
        }
    }

    fn set_nproc_limit(max_threads: u64) -> Result<(), String> {
        let mut lim = libc::rlimit {
            rlim_cur: max_threads,
            rlim_max: max_threads,
        };
        // SAFETY: setrlimit only touches the local rlimit for this process.
        let rc = unsafe { libc::setrlimit(libc::RLIMIT_NPROC, &lim) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        // SAFETY: getrlimit writes into local lim only.
        let rc = unsafe { libc::getrlimit(libc::RLIMIT_NPROC, &mut lim) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        if lim.rlim_cur != max_threads {
            return Err(format!(
                "setrlimit soft={} want={max_threads}",
                lim.rlim_cur
            ));
        }
        Ok(())
    }

    /// Child entry: tighten nproc, run status, exit 0 if survived with dirty found.
    fn run_child(mode: &str, repo: &Path) -> ! {
        let cores = std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1);
        if cores < 2 {
            eprintln!("{SKIP_MARK} available_parallelism={cores} < 2");
            std::process::exit(EXIT_SKIP);
        }

        let used = threads_used() as u64;
        // Holders must stay alive through the status call.
        let (thread_limit, _holders) = match mode {
            // Fill to the nproc ceiling, refund only the scan's scaffolding,
            // then run uncapped status so a produce-worker spawn hits the
            // hard expect("valid name") — the regression RCA.
            "uncapped" => {
                let limit = used.saturating_add(HOLDER_SLACK);
                if let Err(e) = set_nproc_limit(limit) {
                    eprintln!("{SKIP_MARK} setrlimit failed: {e}");
                    std::process::exit(EXIT_SKIP);
                }
                // Fill empirically: the kernel checks RLIMIT_NPROC against the
                // real UID's TOTAL task count across all processes, so no
                // arithmetic on this process's own thread count can find the
                // ceiling (that assumption exit-0'd this test twice on CI).
                let mut holders = Vec::new();
                let mut hit_limit = false;
                for _ in 0..MAX_HOLDERS {
                    match Holder::spawn() {
                        Ok(h) => holders.push(h),
                        Err(e) => {
                            eprintln!(
                                "{NPROC_HIT_MARK} holders={} threads={} limit={limit} err={e}",
                                holders.len(),
                                threads_used()
                            );
                            hit_limit = true;
                            break;
                        }
                    }
                }
                if !hit_limit {
                    eprintln!(
                        "{SKIP_MARK} RLIMIT_NPROC not enforced (spawned {} holders under limit {limit})",
                        holders.len()
                    );
                    std::process::exit(EXIT_SKIP);
                }
                // Refund one slot per soft scaffolding spawn on the
                // into_index_worktree_iter path so those succeed and the NEXT
                // spawn to fail is `gitoxide.in_parallel.produce.N`, whose
                // expect("valid name") panics on `gix_status::index_as_worktree`:
                //   slot 1 funds gix::status::index_worktree::producer (soft map_err(SpawnThread))
                //   slot 2 funds gix_status::dirwalk (soft map_err(SpawnThread))
                //   slot 3 funds gix_status::index_as_worktree (soft map_err(SpawnThread))
                // Same-UID churn may eat refunds first; the parent then insists
                // the resulting scan error is itself a spawn failure.
                for _ in 0..SCAFFOLD_SLOTS {
                    if let Some(holder) = holders.pop() {
                        holder.release();
                    }
                }
                (None, holders)
            }
            // soft = used + OUTER_RESERVE - 2 ⇒ headroom < 2 ⇒ budgeted shrinks to 1.
            "serial" | "budgeted" => {
                let limit = used.saturating_add((OUTER_RESERVE as u64).saturating_sub(2));
                if let Err(e) = set_nproc_limit(limit) {
                    eprintln!("{SKIP_MARK} setrlimit failed: {e}");
                    std::process::exit(EXIT_SKIP);
                }
                // Same-UID tasks may already exceed the new ceiling; probe one
                // spawn and skip (not fail), mirroring the uncapped fill probe.
                match std::thread::Builder::new().spawn(|| {}) {
                    Ok(probe) => {
                        let _ = probe.join();
                    }
                    Err(e) => {
                        eprintln!("{SKIP_MARK} no spawn headroom under limit {limit}: {e}");
                        std::process::exit(EXIT_SKIP);
                    }
                }
                let limit_opt = if mode == "serial" {
                    Some(1usize)
                } else {
                    Some(compute_gix_status_thread_limit())
                };
                (limit_opt, Vec::new())
            }
            other => {
                eprintln!("unknown child mode {other}");
                std::process::exit(EXIT_BAD_MODE);
            }
        };

        match status_finds_suffix(repo, thread_limit, "f00.txt") {
            Ok(true) => std::process::exit(0),
            Ok(false) => {
                eprintln!("scan completed but missed the dirty file");
                std::process::exit(EXIT_MISSED_DIRTY);
            }
            Err(e) => {
                eprintln!("{SCAN_ERROR_MARK} {e}");
                std::process::exit(EXIT_SCAN_ERROR);
            }
        }
    }

    fn spawn_child(mode: &str, repo: &Path) -> std::process::Output {
        let exe = std::env::current_exe().expect("current_exe");
        Command::new(&exe)
            .env(CHILD_ENV, mode)
            .env(REPO_ENV, repo)
            // An inherited forced dial would bypass the budget under test.
            .env_remove(ENV_THREADS)
            .args(["--exact", "nproc_tests::child_entry", "--nocapture"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("spawn child")
    }

    /// Parent-side boilerplate: guard re-entry, require 2+ cores, build the
    /// repo, run the child, and turn child-side skips into parent-side skips.
    fn spawn_child_or_skip(mode: &str) -> Option<std::process::Output> {
        if std::env::var_os(CHILD_ENV).is_some() {
            // Never fork further children from inside a child.
            return None;
        }
        let cores = std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1);
        if cores < 2 {
            eprintln!("skip: available_parallelism={cores} < 2");
            return None;
        }
        let (_tmp, root) = temp_repo_with_dirty_file();
        let out = spawn_child(mode, &root);
        if String::from_utf8_lossy(&out.stderr).contains(SKIP_MARK) {
            eprintln!("skip: {}", String::from_utf8_lossy(&out.stderr));
            return None;
        }
        Some(out)
    }

    #[test]
    fn child_entry() {
        let Some(mode) = std::env::var_os(CHILD_ENV) else {
            return;
        };
        let mode = mode.to_string_lossy();
        let repo = std::env::var_os(REPO_ENV).expect("repo env in child");
        run_child(&mode, Path::new(&repo));
    }

    #[test]
    fn uncapped_gix_status_aborts_under_tight_nproc() {
        // Linux RLIMIT_NPROC counts threads (LWPs); macOS counts processes only.
        if !cfg!(target_os = "linux") {
            eprintln!("skip: abort repro is Linux-only (RLIMIT_NPROC semantics)");
            return;
        }
        let Some(out) = spawn_child_or_skip("uncapped") else {
            return;
        };
        let code = out.status.code();
        let signal = out.status.signal();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        // Precondition, separate from the outcome: the child must have proven
        // the ceiling is enforced before it scanned.
        assert!(
            stderr.contains(NPROC_HIT_MARK),
            "child never hit the nproc ceiling; \
             code={code:?} signal={signal:?}\nstdout={stdout}\nstderr={stderr}"
        );
        assert_ne!(
            code,
            Some(0),
            "uncapped status must not complete under tight nproc; \
             code={code:?} signal={signal:?}\nstdout={stdout}\nstderr={stderr}"
        );
        // cargo builds test targets with panic=unwind (the workspace
        // panic=abort applies to non-test profiles), so the expect("valid
        // name") panic surfaces as stderr markers plus a nonzero exit; the
        // SIGABRT arm covers abort-configured harnesses.
        let aborted = signal == Some(libc::SIGABRT)
            || stderr.contains("gix_status::index_as_worktree")
            || stderr.contains("valid name");
        // Refund race: same-UID processes may consume the refunded slots, so a
        // soft scaffolding spawn fails first — accept only if the printed gix
        // error is itself a thread-spawn failure.
        let soft_spawn_failure = code == Some(EXIT_SCAN_ERROR)
            && stderr.contains(SCAN_ERROR_MARK)
            && (stderr.contains("SpawnThread") || stderr.contains("Failed to spawn"));
        assert!(
            aborted || soft_spawn_failure,
            "expected the in_parallel produce-worker expect(\"valid name\") \
             or a spawn-failure scan error after the refund race; \
             code={code:?} signal={signal:?}\nstdout={stdout}\nstderr={stderr}"
        );
    }

    #[test]
    fn production_budgeted_gix_status_survives_tight_nproc() {
        let Some(out) = spawn_child_or_skip("budgeted") else {
            return;
        };
        assert!(
            out.status.success(),
            "budgeted status must survive tight nproc; code={:?} signal={:?}\nstdout={}\nstderr={}",
            out.status.code(),
            out.status.signal(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
    }

    #[test]
    fn serial_gix_status_survives_tight_nproc() {
        let Some(out) = spawn_child_or_skip("serial") else {
            return;
        };
        assert!(
            out.status.success(),
            "serial (thread_limit=1) status must survive tight nproc; \
             code={:?} signal={:?}\nstdout={}\nstderr={}",
            out.status.code(),
            out.status.signal(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
    }
}
