//! Empty-composer Enter force-sends the top mid-turn queued follow-up.
//!
//! ```bash
//! cargo test -p echo-agent-pager-pty-harness --test empty_enter_send_now -- --ignored --nocapture
//! ```

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore] // opt-in: real pager binary in a PTY (CI runs with --ignored)
async fn empty_enter_force_sends_top_queued() {
    echo_agent_pager_pty_harness::scenarios::empty_enter_send_now::assert_empty_enter_force_sends_top_queued()
        .await
        .expect("empty Enter mid-turn must force-send the top queued follow-up");
}
