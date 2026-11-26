//! Bootstrap-cost regression tier, in its OWN binary: the waterfall sink
//! latches on first use, so it cannot share a process with the stderr-mode
//! sweep tests. Knobs: `ECHO_AGENT_BOOTSTRAP_REGRESSION_SOFT=1` reports without
//! asserting; `ECHO_AGENT_SWEEP_LOG` adjusts tracing.

#[allow(dead_code)]
#[path = "acp_harness/mod.rs"]
mod acp_harness;
#[path = "perf_harness/mod.rs"]
mod perf_harness;
#[path = "subagent_sweep_support/mod.rs"]
mod support;
use echo_agent_runtime::waterfall;
use support::*;
use tempfile::TempDir;

#[test]
#[ignore = "perf regression tier; drives real subagent bursts; run with --ignored --nocapture"]
fn regression_bootstrap_cost() {
    let _sweep = sweep_lock();
    const REPS: usize = 3;
    // Floors keep a near-zero N=1 baseline from turning noise into a ratio.
    // Post-fix measurements under an 8-core cgroup quota (debug): sessboot
    // ratio 2.3-2.7, bridge 1.1-3.6, comfortably inside 3x / 5x.
    const SESSBOOT_MAX_RATIO: f64 = 3.0;
    const SESSBOOT_N1_FLOOR_MS: f64 = 20.0;
    const BRIDGE_MAX_RATIO: f64 = 5.0;
    const BRIDGE_N1_FLOOR_MS: f64 = 2.0;

    let env = sweep_env_init();
    let marks_dir = TempDir::new().expect("marks dir");
    let marks_path = marks_dir.path().join("waterfall.log");
    std::fs::write(&marks_path, "").expect("seed marks file");
    // SAFETY: still before any agent threads; routes marks to the file sink.
    unsafe {
        std::env::set_var(waterfall::ENV, &marks_path);
    }

    let mut offset = 0usize;
    let mut sessboot = (Vec::new(), Vec::new()); // (n1, n25) per-rep p50s
    let mut bridge = (Vec::new(), Vec::new());
    for _rep in 0..REPS {
        for &n in &[1usize, 25] {
            let outcome = burst_on_fresh_mock(&env, n, "none");
            assert_eq!(outcome.failures, 0, "burst n={n} had failed subagents");
            let (marks, new_offset) = parse_waterfall_marks(&marks_path, offset);
            offset = new_offset;
            let boot = segment_p50_ms(
                &marks,
                waterfall::stage::SESSION_SPAWN,
                waterfall::stage::SESSION_UP,
            )
            .expect("sessboot marks missing");
            let seed = segment_p50_ms(
                &marks,
                waterfall::stage::SB_BUILDER_DONE,
                waterfall::stage::SB_AGENT_BUILT,
            )
            .expect("bridge_seed marks missing");
            if n == 1 {
                sessboot.0.push(boot);
                bridge.0.push(seed);
            } else {
                sessboot.1.push(boot);
                bridge.1.push(seed);
            }
        }
    }

    let (boot_n1, boot_n25) = (median_f64(&sessboot.0), median_f64(&sessboot.1));
    let (seed_n1, seed_n25) = (median_f64(&bridge.0), median_f64(&bridge.1));
    let boot_ratio = boot_n25 / boot_n1.max(SESSBOOT_N1_FLOOR_MS);
    let seed_ratio = seed_n25 / seed_n1.max(BRIDGE_N1_FLOOR_MS);
    eprintln!(
        "REGRESSION sessboot_n1={boot_n1:.1} sessboot_n25={boot_n25:.1} sessboot_ratio={boot_ratio:.1} \
         bridge_n1={seed_n1:.1} bridge_n25={seed_n25:.1} bridge_ratio={seed_ratio:.1}"
    );
    if std::env::var_os("ECHO_AGENT_BOOTSTRAP_REGRESSION_SOFT").is_some() {
        return;
    }
    if boot_ratio > SESSBOOT_MAX_RATIO || seed_ratio > BRIDGE_MAX_RATIO {
        eprintln!(
            "per-rep p50s (ms): sessboot n1={:?} n25={:?} bridge n1={:?} n25={:?}",
            sessboot.0, sessboot.1, bridge.0, bridge.1
        );
        eprintln!(
            "reproduce: cargo test -p echo-agent-runtime --test subagent_bootstrap_regression -- --exact regression_bootstrap_cost --ignored --nocapture"
        );
    }
    assert!(
        boot_ratio <= SESSBOOT_MAX_RATIO,
        "sessboot p50 @N=25 ({boot_n25:.1}ms) exceeds {SESSBOOT_MAX_RATIO}x @N=1 ({boot_n1:.1}ms): ratio {boot_ratio:.1}"
    );
    assert!(
        seed_ratio <= BRIDGE_MAX_RATIO,
        "bridge_seed p50 @N=25 ({seed_n25:.1}ms) exceeds {BRIDGE_MAX_RATIO}x @N=1 ({seed_n1:.1}ms): ratio {seed_ratio:.1}"
    );
}
