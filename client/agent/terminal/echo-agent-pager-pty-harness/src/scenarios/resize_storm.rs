//! `resize_storm` — resize the PTY many times in quick succession; assert
//! no crash and measure recovery.
//!
//! What it stresses: `prepare_layout` Case 1 (full width-change rebuild),
//! wrap-cache misses across every entry, resize-debounce in `event_loop.rs`.

use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};

use super::{BenchResults, ContentController, PtyHarness, wait_for_welcome};

const RESIZES: usize = 25;
const RESIZE_INTERVAL: Duration = Duration::from_millis(40);

pub async fn run(harness: &mut PtyHarness, _content: &ContentController) -> Result<BenchResults> {
    wait_for_welcome(harness).await?;
    harness.reset_timing();

    let start = Instant::now();
    // Oscillate between a narrow and a wide layout.
    for i in 0..RESIZES {
        let (rows, cols) = if i % 2 == 0 { (35, 100) } else { (55, 160) };
        harness.resize(rows, cols)?;
        harness.update(RESIZE_INTERVAL);
        if !harness.is_running()? {
            return Err(anyhow!("pager exited during resize_storm at iter {i}"));
        }
    }
    // Let the pager settle.
    harness.update(Duration::from_millis(500));
    let wall_time = start.elapsed();

    if harness.contains_text("panicked") {
        return Err(anyhow!(
            "pager rendered 'panicked' during resize_storm\nscreen:\n{}",
            harness.screen_contents()
        ));
    }

    Ok(harness.bench_results("resize_storm", wall_time))
}
