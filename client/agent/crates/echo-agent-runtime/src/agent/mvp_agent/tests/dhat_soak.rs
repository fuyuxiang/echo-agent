//! Heap-leak test for the session lifecycle: create and remove many sessions,
//! then fail if heap memory grows per session. Run:
//!   cargo test -p echo-agent-runtime --features dhat-heap \
//!     leader_session_lifecycle_heap_steady_state -- --ignored --nocapture
use super::*;
use echo_agent_workspace::permission::PermissionEvent;

// Chosen between a healthy build (about zero retained allocations per
// session) and the smallest deliberately introduced leak (one per session);
// re-tune if healthy runs drift toward the limits.
const MAX_BLOCKS_PER_SESSION: f64 = 0.5;
const MAX_BYTES_PER_SESSION: f64 = 1024.0;

/// Creates the per-session state that `remove_session` must clean up, then
/// removes the session. A full `SessionHandle` would allocate so much
/// unrelated memory that a small leak would be lost in the noise.
fn populate_and_evict(agent: &MvpAgent, i: usize) {
    let sid = acp::SessionId::new(format!("soak-{i}"));

    // The same workspace binding `spawn_session_actor` creates; if
    // `remove_session` does not release it, the session map holds every
    // toolset for the life of the process.
    {
        let ops = agent.workspace_ops.borrow();
        let ops = ops.as_ref().expect("test installs workspace ops");
        let toolset = std::sync::Arc::new(
            echo_agent_tools::registry::types::FinalizedToolset::empty_for_test(),
        );
        ops.bind_local_session(
            sid.0.as_ref(),
            std::env::temp_dir(),
            echo_agent_hunk_tracker::HunkTrackerHandle::noop(),
            toolset,
            None,
        )
        .expect("bind_local_session must succeed");
    }

    let (_ptx, prx) = tokio::sync::mpsc::unbounded_channel::<PermissionEvent>();
    agent.session_registry.set_permission_receiver(&sid, prx);
    agent.set_turn_number(&sid, i as u64);
    agent
        .session_registry
        .set_unavailable_model(&sid, acp::ModelId::new(std::sync::Arc::from("gone-model")));
    agent.remove_session(&sid);
}

/// Waits for background tasks to finish before reading heap stats.
async fn quiesce() {
    const YIELD_ROUNDS: usize = 50;
    const SETTLE: std::time::Duration = std::time::Duration::from_millis(50);
    for _ in 0..YIELD_ROUNDS {
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(SETTLE).await;
    for _ in 0..YIELD_ROUNDS {
        tokio::task::yield_now().await;
    }
}

/// Creating and removing N sessions must not grow the heap.
///
/// Only one `dhat::Profiler` can exist at a time, and the test harness runs
/// tests in parallel, so keep this the only test that creates one.
#[test]
#[ignore = "heap soak; nightly only, needs --features dhat-heap"]
fn leader_session_lifecycle_heap_steady_state() {
    run_local_for_bridge_test(|| async {
        let agent = build_minimal_agent_for_tests();
        *agent.workspace_ops.borrow_mut() = Some(echo_agent_workspace::WorkspaceOps::for_test());
        let _profiler = dhat::Profiler::builder().testing().build();

        const WARMUP: usize = 16;
        const MEASURE: usize = 256;

        // The first runs fill caches and one-time allocations; do them before
        // the measured window so they do not count as growth.
        for i in 0..WARMUP {
            populate_and_evict(&agent, i);
        }
        quiesce().await;
        let before = dhat::HeapStats::get();

        for i in WARMUP..(WARMUP + MEASURE) {
            populate_and_evict(&agent, i);
        }
        quiesce().await;
        let after = dhat::HeapStats::get();

        let d_blocks = after.curr_blocks as i64 - before.curr_blocks as i64;
        let d_bytes = after.curr_bytes as i64 - before.curr_bytes as i64;
        let blocks_per = d_blocks as f64 / MEASURE as f64;
        let bytes_per = d_bytes as f64 / MEASURE as f64;
        // Printed before the asserts so failing runs still show the numbers.
        eprintln!(
            "DHAT_SOAK_SUMMARY {}",
            serde_json::json!({
                "warmup_sessions": WARMUP,
                "measured_sessions": MEASURE,
                "before_blocks": before.curr_blocks,
                "before_bytes": before.curr_bytes,
                "after_blocks": after.curr_blocks,
                "after_bytes": after.curr_bytes,
                "blocks_per_session": blocks_per,
                "bytes_per_session": bytes_per,
                "max_blocks_per_session": MAX_BLOCKS_PER_SESSION,
                "max_bytes_per_session": MAX_BYTES_PER_SESSION,
                "pass": blocks_per < MAX_BLOCKS_PER_SESSION && bytes_per < MAX_BYTES_PER_SESSION
            })
        );

        assert!(
            blocks_per < MAX_BLOCKS_PER_SESSION,
            "block-count leak: {blocks_per:.3} blocks/session retained ({d_blocks} over {MEASURE} cycles) exceeds the {MAX_BLOCKS_PER_SESSION} gate"
        );
        assert!(
            bytes_per < MAX_BYTES_PER_SESSION,
            "byte leak: {bytes_per:.1} bytes/session retained ({d_bytes} over {MEASURE} cycles) exceeds the {MAX_BYTES_PER_SESSION} gate"
        );
    });
}
