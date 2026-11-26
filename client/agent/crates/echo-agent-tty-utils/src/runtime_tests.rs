use super::{park_blocking_workers, release_parked_workers, *};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

fn nz(n: usize) -> NonZeroUsize {
    NonZeroUsize::new(n).unwrap()
}

#[test]
fn cap_is_identity_at_or_below_max() {
    assert_eq!(cap_worker_threads(nz(1)), nz(1));
    assert_eq!(cap_worker_threads(nz(4)), nz(4));
    assert_eq!(cap_worker_threads(nz(8)), nz(8));
}

#[test]
fn cap_clamps_many_core_hosts() {
    assert_eq!(cap_worker_threads(nz(9)), nz(8));
    assert_eq!(cap_worker_threads(nz(360)), nz(8));
}

#[test]
fn capped_worker_threads_stays_in_bounds() {
    let n = capped_worker_threads();
    assert!(n <= MAX_WORKER_THREADS, "got {n}");
}

#[test]
fn keep_alive_overflows_instant_add() {
    assert!(
        Instant::now()
            .checked_add(BLOCKING_THREAD_KEEP_ALIVE)
            .is_none(),
        "Tokio/parking_lot treat checked_add overflow as wait-forever"
    );
}

#[test]
fn apply_blocking_pool_builds_and_prewarms() {
    let mut builder = tokio::runtime::Builder::new_current_thread();
    let rt = build_with_blocking_pool(&mut builder).expect("runtime build");
    rt.block_on(async {
        tokio::task::spawn_blocking(|| {})
            .await
            .expect("spawn_blocking");
    });
}

#[test]
fn prewarm_times_out_and_releases_workers() {
    let mut builder = tokio::runtime::Builder::new_current_thread();
    builder.max_blocking_threads(1);
    let rt = builder.build().expect("runtime build");
    let err = prewarm_blocking_pool_n(rt.handle(), MAX_BLOCKING_THREADS, Duration::from_millis(80))
        .expect_err("cap 1 cannot start 16 overlapping workers");
    assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    let msg = err.to_string();
    assert!(
        msg.contains("stalled after") && msg.contains(&format!("of {MAX_BLOCKING_THREADS}")),
        "unexpected timeout message: {msg}"
    );

    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        drop(rt);
        let _ = done_tx.send(());
    });
    assert!(
        done_rx.recv_timeout(Duration::from_secs(2)).is_ok(),
        "Runtime drop hung — parked pre-warm workers were not released"
    );
}

#[cfg(target_os = "linux")]
fn thread_count() -> Option<u64> {
    crate::sample_process_resources().threads
}

#[cfg(target_os = "linux")]
const BEHAVIOR_CHILD_ENV: &str = "ECHO_AGENT_TTY_UTILS_BLOCKING_POOL_CHILD";
#[cfg(target_os = "linux")]
const BEHAVIOR_PASS_MARK: &str = "blocking-pool-behavior:";

#[cfg(target_os = "linux")]
fn run_behavior_child() -> ! {
    let before = thread_count().expect("thread count");
    let mut builder = tokio::runtime::Builder::new_current_thread();
    let rt = build_with_blocking_pool(&mut builder).expect("runtime build");
    let after_prewarm = thread_count().expect("thread count after pre-warm");
    let grown = after_prewarm.saturating_sub(before);
    assert!(
        grown >= MAX_BLOCKING_THREADS as u64,
        "pre-warm did not create the pool: {before} -> {after_prewarm} (delta {grown})"
    );

    let release = Arc::new(AtomicBool::new(false));
    let held = park_blocking_workers(
        rt.handle(),
        MAX_BLOCKING_THREADS,
        &release,
        Duration::from_secs(5),
    )
    .expect("occupy");

    let busy = thread_count().expect("thread count while busy");
    let extra = MAX_BLOCKING_THREADS;
    let (tx, rx) = std::sync::mpsc::channel();
    for _ in 0..extra {
        let tx = tx.clone();
        rt.handle().spawn_blocking(move || {
            let _ = tx.send(());
        });
    }
    drop(tx);

    let deadline = Instant::now() + Duration::from_millis(300);
    loop {
        let now = thread_count().expect("thread count under load");
        assert!(
            now <= busy,
            "blocking pool grew under load: {busy} -> {now} (cap is {MAX_BLOCKING_THREADS})"
        );
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    release_parked_workers(&release, &held);
    let drain_deadline = Instant::now() + Duration::from_secs(5);
    let mut got = 0usize;
    while got < extra {
        let remaining = drain_deadline.saturating_duration_since(Instant::now());
        rx.recv_timeout(remaining)
            .expect("queued spawn_blocking should run after release");
        got += 1;
    }
    println!("{BEHAVIOR_PASS_MARK} prewarmed +{grown} then queued {got}");
    std::process::exit(0);
}

#[cfg(target_os = "linux")]
#[test]
fn child_entry_blocking_pool_behavior() {
    if std::env::var_os(BEHAVIOR_CHILD_ENV).is_some() {
        run_behavior_child();
    }
}

#[cfg(target_os = "linux")]
#[test]
fn prewarm_creates_cap_and_extra_spawn_blocking_queues() {
    if std::env::var_os(BEHAVIOR_CHILD_ENV).is_some() {
        return;
    }
    let filter = module_path!()
        .split_once("::")
        .map(|(_, rest)| rest)
        .unwrap_or_default();
    let exe = std::env::current_exe().expect("current_exe");
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--exact")
        .arg(format!("{filter}::child_entry_blocking_pool_behavior"))
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env(BEHAVIOR_CHILD_ENV, "1")
        .stdin(std::process::Stdio::null());
    crate::detach_std_command(&mut cmd);
    let out = cmd.output().expect("spawn child");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && !stderr.contains("panicked at"),
        "child aborted/panicked (status: {:?})\nstdout:\n{stdout}\nstderr:\n{stderr}",
        out.status
    );
    assert!(
        stdout.contains(BEHAVIOR_PASS_MARK),
        "no pass marker (filter matched nothing?)\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
