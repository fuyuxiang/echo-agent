//! `idle_cost` — after content settles, measure frames for N seconds of
//! true idle. Catches the `needs_animation()` always-true bug: frames > 0
//! here means the pager is ticking when it shouldn't.

use std::time::{Duration, Instant};

use anyhow::Result;

use super::{BenchResults, ContentController, PtyHarness, wait_for_welcome};

const IDLE_WINDOW: Duration = Duration::from_secs(3);

pub async fn run(harness: &mut PtyHarness, _content: &ContentController) -> Result<BenchResults> {
    wait_for_welcome(harness).await?;
    // Let the splash screen animation settle (some of the pager's intro
    // screens do a brief fade/animation).
    harness.update(Duration::from_secs(1));
    harness.reset_timing();

    let start = Instant::now();
    while start.elapsed() < IDLE_WINDOW {
        harness.update(Duration::from_millis(100));
        if !harness.is_running()? {
            break;
        }
    }
    let wall_time = start.elapsed();

    Ok(harness.bench_results("idle_cost", wall_time))
}
