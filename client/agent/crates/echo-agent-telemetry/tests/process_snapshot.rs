//! Fresh-process pins; the assertions consume process-global state.

use std::time::{Duration, Instant};

use echo_agent_telemetry::events::ShellTrueNoop;
use echo_agent_telemetry::{process_metrics, session_ctx};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_gated_emit_takes_no_snapshot_and_the_second_snapshot_reports_cpu() {
    assert!(
        !echo_agent_telemetry::is_enabled(),
        "this binary must never install a telemetry client"
    );
    echo_agent_telemetry::log_event(ShellTrueNoop {
        tool_name: "bash".into(),
    });
    session_ctx::drain_pending(Duration::from_secs(5)).await;

    let first = process_metrics::snapshot();
    assert_eq!(
        first.cpu, None,
        "an emit without a client must not have taken the first snapshot"
    );
    #[cfg(unix)]
    assert!(
        first.cpu_time_ms.is_some(),
        "the cumulative counter must be readable on the first snapshot"
    );

    // Spin so both CPU time and wall clock advance before the second snapshot.
    let spin_until = Instant::now() + Duration::from_millis(20);
    let mut acc: u64 = 0;
    while Instant::now() < spin_until {
        acc = acc.wrapping_mul(31).wrapping_add(7);
    }
    std::hint::black_box(acc);

    let second = process_metrics::snapshot();
    #[cfg(unix)]
    {
        let window = second
            .cpu
            .expect("the second snapshot must derive a cpu window");
        assert!(
            window.share_percent.is_finite() && window.share_percent >= 0.0,
            "cpu share must be finite and non-negative, got {}",
            window.share_percent
        );
        assert!(
            window.window_ms >= 1,
            "a derived share must cover at least the minimum window"
        );
    }

    // A sub-floor read must not advance the baseline: the next derived
    // window spans back to the last DERIVED window, across the sub-floor
    // read, never just since the previous snapshot.
    #[cfg(unix)]
    {
        let mut last_derived_end = Instant::now();
        let deadline = Instant::now() + Duration::from_secs(5);
        let sub_floor_at = loop {
            let taken = Instant::now();
            if process_metrics::snapshot().cpu.is_none() {
                break taken;
            }
            last_derived_end = Instant::now();
            assert!(
                Instant::now() < deadline,
                "never observed a sub-floor snapshot"
            );
        };

        let spin_until = Instant::now() + Duration::from_millis(20);
        let mut acc: u64 = 0;
        while Instant::now() < spin_until {
            acc = acc.wrapping_mul(31).wrapping_add(7);
        }
        std::hint::black_box(acc);

        let before_final = Instant::now();
        let window = process_metrics::snapshot()
            .cpu
            .expect("20ms after a derived baseline must derive a window");
        assert!(
            window.window_ms >= before_final.duration_since(last_derived_end).as_millis() as u64,
            "the window must span back to the last derived baseline, not the sub-floor read {:?} after it",
            sub_floor_at.duration_since(last_derived_end),
        );
    }
}
