//! Real-subagent burst latency sweep.
//!
//! Drives N real background `task` calls through the full production path
//! (coordinator admission → spawn pipeline → child session → echo-mock turn)
//! from an in-process agent over duplex ACP pipes; the measuring client runs
//! on the test thread. Run one N per process for clean resource numbers:
//!   cargo test --release -p echo-agent-runtime --test subagent_latency_sweep \
//!     -- --exact sweep_n25 --ignored --nocapture
//!
//! Output (grep-parseable): per-child `SUBAGENT n= id= spawn_ms= start_ms=
//! end_ms= task_ms= status= agent_ms=` (client-observed offsets from prompt
//! t0) and one `SWEEP` summary row per burst. The waterfall variants add
//! per-stage `WATERFALL id= stage= t_us=` marks.
//!
//! Knobs: ECHO_AGENT_SWEEP_NS, ECHO_AGENT_SWEEP_ISOLATION ("none"|"worktree"),
//! ECHO_AGENT_SWEEP_DEADLINE_S, ECHO_AGENT_SWEEP_REPO_FILES,
//! ECHO_AGENT_SWEEP_ASSERT_NO_FAILURES, ECHO_AGENT_SWEEP_LOG.

#[allow(dead_code)]
#[path = "acp_harness/mod.rs"]
mod acp_harness;
#[path = "perf_harness/mod.rs"]
mod perf_harness;
#[path = "subagent_sweep_support/mod.rs"]
mod support;
use echo_agent_runtime::waterfall;
use support::*;

#[test]
#[ignore = "perf sweep; drives real subagent bursts; run with --ignored --nocapture"]
fn subagent_burst_latency_sweep() {
    let _sweep = sweep_lock();
    let ns: Vec<usize> = std::env::var("ECHO_AGENT_SWEEP_NS")
        .unwrap_or_else(|_| "1,5,10,25,50".to_string())
        .split(',')
        .filter_map(|s| s.trim().parse::<usize>().ok())
        .filter(|&v| v > 0)
        .collect();
    let isolation =
        std::env::var("ECHO_AGENT_SWEEP_ISOLATION").unwrap_or_else(|_| "none".to_string());
    run_sweep(&ns, &isolation);
}

macro_rules! sweep_case {
    ($name:ident, $n:expr, $iso:expr) => {
        #[test]
        #[ignore = "perf sweep; drives real subagent bursts; run with --ignored --nocapture"]
        fn $name() {
            let _sweep = sweep_lock();
            run_sweep(&[$n], $iso);
        }
    };
}

sweep_case!(sweep_n1, 1, "none");
sweep_case!(sweep_n5, 5, "none");
sweep_case!(sweep_n10, 10, "none");
sweep_case!(sweep_n25, 25, "none");
sweep_case!(sweep_n50, 50, "none");
sweep_case!(sweep_n1_worktree, 1, "worktree");
sweep_case!(sweep_n10_worktree, 10, "worktree");
sweep_case!(sweep_n25_worktree, 25, "worktree");

macro_rules! waterfall_case {
    ($name:ident, $n:expr) => {
        #[test]
        #[ignore = "perf sweep; drives real subagent bursts; run with --ignored --nocapture"]
        fn $name() {
            let _sweep = sweep_lock();
            // SAFETY: serialized by SWEEP_LOCK; set before any agent threads exist.
            unsafe { std::env::set_var(waterfall::ENV, "1") };
            run_sweep(&[$n], "none");
        }
    };
}

waterfall_case!(waterfall_n1, 1);
waterfall_case!(waterfall_n10, 10);
waterfall_case!(waterfall_n25, 25);
