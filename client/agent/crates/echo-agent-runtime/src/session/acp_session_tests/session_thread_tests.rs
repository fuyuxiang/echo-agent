use super::*;

/// Poll `t.is_finished()` until it returns `true` or the deadline elapses.
/// Returns `true` if the thread finished in time. Used in place of a fixed
/// `sleep` so these tests don't flake under heavy CPU contention (e.g.
/// `bazel test --runs_per_test 50`).
fn wait_for_finish(t: &SessionThread, timeout: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while !t.is_finished() {
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    true
}

const FINISH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[test]
fn session_thread_detects_normal_exit() {
    let t = SessionThread::from_handle(std::thread::spawn(|| {}));
    assert!(
        wait_for_finish(&t, FINISH_TIMEOUT),
        "thread did not finish within {FINISH_TIMEOUT:?}"
    );
}

#[test]
#[ignore]
fn session_thread_detects_panic() {
    let t = SessionThread::from_handle(std::thread::spawn(|| {
        panic!("intentional test panic");
    }));
    assert!(
        wait_for_finish(&t, FINISH_TIMEOUT),
        "thread did not finish within {FINISH_TIMEOUT:?}"
    );
}

#[test]
fn session_thread_not_finished_while_running() {
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let t = SessionThread::from_handle(std::thread::spawn(move || {
        let _ = rx.recv(); // block until signaled
    }));
    assert!(!t.is_finished());
    drop(tx); // signal thread to exit
    assert!(
        wait_for_finish(&t, FINISH_TIMEOUT),
        "thread did not finish within {FINISH_TIMEOUT:?} after dropping tx"
    );
}

/// Regression test: per-session threads run independently.
///
/// Spawns two session threads, each with their own tokio runtime + LocalSet.
/// Thread A blocks for 3 seconds (simulating a long tool call). Thread B
/// completes a quick task. Asserts that B finishes within 1 second — proving
/// A's blocking work does not stall B.
///
/// On the old single-LocalSet architecture, both tasks would share one thread
/// and B would be blocked until A's sleep yields. With per-session threads,
/// they run on separate OS threads with true parallelism.
#[test]
fn sessions_on_separate_threads_do_not_block_each_other() {
    let (result_tx, result_rx) = std::sync::mpsc::channel::<&str>();

    let result_tx_a = result_tx.clone();
    let _thread_a = SessionThread::from_handle(std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        local.block_on(&rt, async {
            // Simulate a long-running tool call (e.g., bash sleep)
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            let _ = result_tx_a.send("A done");
        });
    }));

    let result_tx_b = result_tx;
    let _thread_b = SessionThread::from_handle(std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        local.block_on(&rt, async {
            // Quick task — should complete immediately
            tokio::task::yield_now().await;
            let _ = result_tx_b.send("B done");
        });
    }));

    // B should finish well before A's 3-second sleep.
    let first = result_rx
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("Neither session completed within 1 second — threads may be blocked");
    assert_eq!(
        first, "B done",
        "Expected B to finish first (A sleeps 3s), but got: {first}"
    );
}
