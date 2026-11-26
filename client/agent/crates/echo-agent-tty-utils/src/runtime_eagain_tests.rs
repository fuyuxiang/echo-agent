//! Proves the fault Ashok hit is gone: `pthread_create` returning `EAGAIN`
//! must queue the task, not abort the process.
//!
//! Tokio only panics on `EAGAIN` when the blocking pool holds zero threads
//! (`blocking/pool.rs`, `SpawnError::NoThreads`). Under `panic = abort` that
//! kills every session. The cap plus keep-alive plus pre-warm keeps the pool
//! non-empty, so the panic arm becomes unreachable.
//!
//! Two children run, and the control matters as much as the subject: if the
//! default runtime does not die, the harness did not reproduce the fault and
//! the pre-warmed result proves nothing.
//!
//! Privileges are dropped first. A process holding `CAP_SYS_RESOURCE` ignores
//! `RLIMIT_NPROC`, so as root this test would pass without testing anything.

use super::{MAX_BLOCKING_THREADS, build_with_blocking_pool};
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, Stdio};

const CHILD_ENV: &str = "ECHO_AGENT_TTY_UTILS_EAGAIN_CHILD";
const SURVIVED: &str = "eagain-survived:";
const SKIP: &str = "skip-child:";
const NOBODY: u32 = 65534;

fn thread_count() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("Threads:")?.trim().parse().ok())
        })
        .unwrap_or(1)
}

/// `RLIMIT_NPROC` does not apply to a privileged process.
fn drop_privileges() -> bool {
    // SAFETY: standard credential drop, no borrowed state.
    unsafe {
        if libc::geteuid() != 0 {
            return true;
        }
        libc::setgid(NOBODY) == 0 && libc::setuid(NOBODY) == 0
    }
}

fn pin_thread_limit() -> bool {
    let n = thread_count();
    let lim = libc::rlimit {
        rlim_cur: n,
        rlim_max: n,
    };
    // SAFETY: setrlimit reads one local struct.
    unsafe { libc::setrlimit(libc::RLIMIT_NPROC, &lim) == 0 }
}

/// The limit is worthless unless the kernel actually refuses a thread.
fn limit_bites() -> bool {
    std::thread::Builder::new()
        .spawn(|| {})
        .map(|h| {
            let _ = h.join();
        })
        .is_err()
}

fn run_child(prewarmed: bool) -> ! {
    if !drop_privileges() {
        println!("{SKIP} could not drop privileges");
        std::process::exit(0);
    }
    let mut builder = tokio::runtime::Builder::new_current_thread();
    builder.enable_all();
    let rt = if prewarmed {
        build_with_blocking_pool(&mut builder).expect("runtime build")
    } else {
        builder.build().expect("runtime build")
    };
    if !pin_thread_limit() {
        println!("{SKIP} setrlimit failed");
        std::process::exit(0);
    }
    if !limit_bites() {
        println!("{SKIP} RLIMIT_NPROC is not enforced here");
        std::process::exit(0);
    }
    // The default runtime has an empty pool, so this must create a thread and
    // take the panic arm. The pre-warmed runtime hands it to an idle worker.
    let got = rt.block_on(async { tokio::task::spawn_blocking(|| 42u32).await });
    println!("{SURVIVED} {got:?} pool={MAX_BLOCKING_THREADS}");
    std::process::exit(0);
}

#[test]
fn child_entry_eagain_under_thread_limit() {
    if let Ok(mode) = std::env::var(CHILD_ENV) {
        run_child(mode == "prewarmed");
    }
}

fn spawn_child(mode: &str) -> (bool, i32, String) {
    let filter = module_path!()
        .split_once("::")
        .map(|(_, rest)| rest)
        .unwrap_or_default();
    let out = Command::new(std::env::current_exe().expect("current_exe"))
        .arg("--exact")
        .arg(format!("{filter}::child_entry_eagain_under_thread_limit"))
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env(CHILD_ENV, mode)
        .stdin(Stdio::null())
        .output()
        .expect("spawn child");
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    let signal = out.status.signal().unwrap_or(0);
    (out.status.success(), signal, text)
}

#[test]
fn default_runtime_dies_and_prewarmed_runtime_survives() {
    let (ok_a, sig_a, text_a) = spawn_child("default");
    if text_a.contains(SKIP) {
        eprintln!("skipping: {}", text_a.trim());
        return;
    }

    // Control. Without it, a green subject means nothing.
    assert!(
        !ok_a,
        "control did not reproduce the fault: the default runtime survived EAGAIN.\n{text_a}"
    );
    assert!(
        text_a.contains("OS can't spawn worker thread") || sig_a != 0,
        "control died for the wrong reason (signal {sig_a}).\n{text_a}"
    );

    let (ok_b, sig_b, text_b) = spawn_child("prewarmed");
    if text_b.contains(SKIP) {
        eprintln!("skipping: {}", text_b.trim());
        return;
    }
    assert!(
        ok_b && text_b.contains(SURVIVED),
        "pre-warmed runtime still died under EAGAIN (signal {sig_b}).\n{text_b}"
    );
}
