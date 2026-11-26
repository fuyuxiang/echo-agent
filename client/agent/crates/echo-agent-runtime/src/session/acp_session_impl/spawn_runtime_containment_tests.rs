//! Containment tests for [`super::build_session_runtime`].
//!
//! Cases run in a re-exec'd child (the `echo-agent-gix-status` pattern) so
//! parallel tests are unaffected. Stdout markers distinguish skip
//! (unenforceable environment) from pass/fail.

use super::build_session_runtime;
use echo_agent_tty_utils::runtime::MAX_BLOCKING_THREADS;

/// Env marker dispatching the re-exec'd test binary into child logic.
const CHILD_ENV: &str = "ECHO_AGENT_RUNTIME_RUNTIME_CONTAINMENT_CHILD";
const BLOCKING_POOL_CHILD_ENV: &str = "ECHO_AGENT_RUNTIME_BLOCKING_POOL_CONTAINMENT_CHILD";
const PASS_MARK: &str = "runtime-build-contained:";
const BLOCKING_PASS_MARK: &str = "blocking-pool-contained:";
const SKIP_MARK: &str = "skip-child:";

fn reexec_child(test_name: &str, env: &str) -> std::process::Output {
    // module_path!() includes the crate name; libtest filters do not.
    let filter = module_path!()
        .split_once("::")
        .map(|(_, rest)| rest)
        .unwrap_or_default();
    let exe = std::env::current_exe().expect("current_exe");
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--exact")
        .arg(format!("{filter}::{test_name}"))
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env(env, "1")
        .stdin(std::process::Stdio::null());
    echo_agent_tty_utils::detach_std_command(&mut cmd);
    cmd.output().expect("spawn child test process")
}

fn assert_child_contained(out: &std::process::Output, pass_mark: &str) {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        out.status.success() && !stderr.contains("panicked at"),
        "child aborted/panicked instead of containing the failure \
         (status: {:?})\nstdout:\n{stdout}\nstderr:\n{stderr}",
        out.status
    );
    if stdout.contains(SKIP_MARK) {
        eprintln!("skipped: {stdout}");
        return;
    }
    assert!(
        stdout.contains(pass_mark),
        "no pass/skip marker (filter matched nothing?)\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

/// Child: lower RLIMIT_NOFILE, fill the fd table, assert `Err`.
fn run_child() -> ! {
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit writes only into local `lim`.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) } != 0 {
        println!("{SKIP_MARK} getrlimit failed");
        std::process::exit(0);
    }
    lim.rlim_cur = 64.min(lim.rlim_max);
    // SAFETY: lowers only this process's soft limit.
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &lim) } != 0 {
        println!("{SKIP_MARK} setrlimit failed");
        std::process::exit(0);
    }
    // Fill remaining headroom empirically; immune to fds the test
    // harness already holds.
    let mut held = Vec::new();
    loop {
        // SAFETY: dup(0) yields a held fd or fails with EMFILE when full.
        let fd = unsafe { libc::dup(0) };
        if fd < 0 {
            break;
        }
        held.push(fd);
        if held.len() > 4096 {
            println!("{SKIP_MARK} fd limit not enforced");
            std::process::exit(0);
        }
    }

    match build_session_runtime() {
        Err(e) => {
            println!("{PASS_MARK} {e}");
            std::process::exit(0);
        }
        // The contract under test ("failure is an Err") was not
        // exercised; skip rather than fail.
        Ok(_) => {
            println!("{SKIP_MARK} runtime built despite full fd table");
            std::process::exit(0);
        }
    }
}

fn thread_count() -> Option<u64> {
    echo_agent_tty_utils::sample_process_resources().threads
}

/// Child: session runtime must not 16-wide pre-warm (cap proof lives in
/// `echo-agent-tty-utils`). `spawn_blocking` must still run.
fn run_blocking_pool_child() -> ! {
    let Some(before) = thread_count() else {
        println!("{SKIP_MARK} thread count unavailable");
        std::process::exit(0);
    };

    let rt = match build_session_runtime() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("build_session_runtime failed: {e}");
            std::process::exit(1);
        }
    };

    let Some(after_build) = thread_count() else {
        println!("{SKIP_MARK} thread count unavailable after build");
        std::process::exit(0);
    };
    let grown = after_build.saturating_sub(before);
    if grown >= MAX_BLOCKING_THREADS as u64 {
        eprintln!(
            "session runtime must not 16-wide pre-warm: {before} -> {after_build} (delta {grown})"
        );
        std::process::exit(1);
    }

    rt.block_on(async {
        tokio::task::spawn_blocking(|| {})
            .await
            .expect("spawn_blocking");
    });
    println!("{BLOCKING_PASS_MARK} grew {grown}");
    std::process::exit(0);
}

/// Doubles as the child entry point when `CHILD_ENV` is set.
#[test]
fn child_entry_runtime_build_under_fd_exhaustion() {
    if std::env::var_os(CHILD_ENV).is_some() {
        run_child();
    }
}

/// Doubles as the child entry point when `BLOCKING_POOL_CHILD_ENV` is set.
#[test]
fn child_entry_capped_blocking_pool() {
    if std::env::var_os(BLOCKING_POOL_CHILD_ENV).is_some() {
        run_blocking_pool_child();
    }
}

#[test]
fn runtime_build_failure_is_contained() {
    let out = reexec_child("child_entry_runtime_build_under_fd_exhaustion", CHILD_ENV);
    assert_child_contained(&out, PASS_MARK);
}

#[test]
fn capped_runtime_spawn_blocking_is_contained() {
    let out = reexec_child("child_entry_capped_blocking_pool", BLOCKING_POOL_CHILD_ENV);
    assert_child_contained(&out, BLOCKING_PASS_MARK);
}
