//! `mixed_interaction` — scroll while streaming. The real-world worst case.
//!
//! What it stresses: simultaneous cache invalidation (from streaming) and
//! full viewport re-render (from scrolling). Surfaces `dirty_heights` /
//! scroll-offset interactions.

use std::time::{Duration, Instant};

use anyhow::Result;

use super::{BenchResults, ContentController, PtyHarness, wait_for_welcome};
use crate::keys;

const TARGET_WORDS: usize = 400;
const SCROLL_KEYS: usize = 80;
const KEY_INTERVAL: Duration = Duration::from_millis(40);

pub async fn run(harness: &mut PtyHarness, content: &ContentController) -> Result<BenchResults> {
    // Same payload shape as streaming_render, but we scroll through it
    // while it's still arriving.
    let mut body = String::from("mixed-bench ");
    for i in 0..TARGET_WORDS {
        body.push_str("tok");
        body.push_str(&i.to_string());
        body.push(' ');
    }
    content.set_response(body);

    wait_for_welcome(harness).await?;
    harness.inject_keys(b"go\r")?;
    harness.wait_for_text("mixed-bench", Duration::from_secs(20))?;
    harness.reset_timing();

    let start = Instant::now();
    for _ in 0..SCROLL_KEYS {
        harness.inject_keys(keys::J)?;
        harness.update(KEY_INTERVAL);
        if !harness.is_running()? {
            break;
        }
    }
    harness.update(Duration::from_millis(250));
    let wall_time = start.elapsed();

    Ok(harness.bench_results("mixed_interaction", wall_time))
}
