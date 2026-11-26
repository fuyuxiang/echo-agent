use std::time::{Duration, Instant};

use super::{StallActivity, StallRollup, input_wait};

const WINDOW: Duration = Duration::from_secs(10);

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn idle() -> StallActivity {
    StallActivity::default()
}

#[test]
fn max_stall_is_worst_and_count_sums_batches() {
    let base = Instant::now();
    let mut rollup = StallRollup::new(WINDOW);

    assert_eq!(rollup.observe(ms(120), idle(), 2, base), None);
    assert_eq!(rollup.observe(ms(1000), idle(), 3, base + ms(10)), None);
    assert_eq!(rollup.observe(ms(30), idle(), 4, base + ms(20)), None);

    let flushed = rollup
        .observe(ms(40), idle(), 1, base + WINDOW)
        .expect("crossing the window boundary must flush");
    assert_eq!(flushed.max_stall_ms, 1000);
    assert_eq!(flushed.events_handled, 10);
}

#[test]
fn deadline_event_folds_into_the_elapsed_window_instead_of_splitting() {
    let base = Instant::now();
    let mut rollup = StallRollup::new(WINDOW);

    assert_eq!(rollup.observe(ms(100), idle(), 1, base), None);

    let busy = StallActivity {
        compaction_active: false,
        subagents_active: 3,
        mcp_servers_connected: 2,
    };
    let flushed = rollup
        .observe(ms(900), busy, 1, base + WINDOW)
        .expect("an event observed at the elapsed deadline flushes the open window");
    assert_eq!(flushed.window_ms, 10_000);
    assert_eq!(flushed.events_handled, 2);
    assert_eq!(flushed.max_stall_ms, 900);
    assert_eq!(flushed.activity, busy);

    assert_eq!(
        rollup.take_if_elapsed(base + WINDOW),
        None,
        "the boundary stall must not open a second window"
    );
}

#[test]
fn input_wait_floors_pre_loop_arrival_at_loop_entry() {
    let loop_entry = Instant::now();
    let captured_before_loop = loop_entry - ms(5000);
    assert_eq!(
        input_wait(captured_before_loop, loop_entry + ms(10), loop_entry),
        ms(10)
    );
    let arrived_live = loop_entry + ms(2000);
    assert_eq!(
        input_wait(arrived_live, arrived_live + ms(30), loop_entry),
        ms(30)
    );
}
