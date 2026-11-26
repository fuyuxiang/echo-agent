//! PTY: Esc mirror of `auto_wake_cancel_preserves_queued_user_prompt` (see
//! that file's header for the failure chain). Also gates on the wake stop
//! affordance while the pane is idle, which exists only with wake-turn
//! cancel support.
#[allow(unused_imports)]
use super::common::*;

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn auto_wake_cancel_via_esc_preserves_queued_user_prompt() {
    use super::auto_wake_cancel_preserves_queued_user_prompt::{
        WakeCancelGesture, run_wake_cancel_scenario,
    };
    run_wake_cancel_scenario(WakeCancelGesture::Esc, "auto_wake_esc").await;
}
