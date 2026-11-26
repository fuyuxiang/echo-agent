//! `streaming_render` — stream a response and measure frame timing during
//! active streaming.
//!
//! What it stresses: streaming-chunk cache invalidation, `ensure_wrapped()`
//! cache misses every generation, wave animation during running turn.

use std::time::{Duration, Instant};

use anyhow::Result;

use super::{BenchResults, ContentController, PtyHarness, wait_for_welcome};

const TARGET_WORDS: usize = 400;
const STREAM_WINDOW: Duration = Duration::from_secs(4);

pub async fn run(harness: &mut PtyHarness, content: &ContentController) -> Result<BenchResults> {
    content.set_response(build_response(TARGET_WORDS));

    wait_for_welcome(harness).await?;

    // Kick off the streamed response.
    harness.inject_keys(b"stream\r")?;
    // Wait for first delta to hit the screen so we're measuring the
    // steady-state streaming path, not startup latency.
    harness.wait_for_text("stream-bench", Duration::from_secs(20))?;
    harness.reset_timing();

    // Collect frames during the streaming window — the mock server paces
    // itself naturally via HTTP/SSE; we just let the pipe drain.
    let start = Instant::now();
    while start.elapsed() < STREAM_WINDOW {
        harness.update(Duration::from_millis(100));
        if !harness.is_running()? {
            break;
        }
    }
    let wall_time = start.elapsed();

    Ok(harness.bench_results("streaming_render", wall_time))
}

fn build_response(words: usize) -> String {
    let mut s = String::with_capacity(words * 10);
    // Sentinel token that wait_for_text keys on — guaranteed to appear
    // near the very start of the stream.
    s.push_str("stream-bench ");
    for i in 0..words {
        s.push_str("word");
        s.push_str(&i.to_string());
        s.push(' ');
    }
    s
}
