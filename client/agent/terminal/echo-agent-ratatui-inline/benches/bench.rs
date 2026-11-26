use std::hint::black_box;

use colored_json::ToColoredJson;
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::{TerminalOptions, Viewport};
use serde_json::json;

use echo_agent_ratatui_inline::{LinkSpan, Terminal, split_into_line_segments};

fn generate_colored_json_content() -> String {
    // Create a complex JSON structure with 50+ lines when pretty printed
    let data = json!({
        "users": (0..10).map(|i| json!({
            "id": i,
            "name": format!("User {}", i),
            "email": format!("user{}@example.com", i),
            "active": i % 2 == 0,
            "roles": ["admin", "user", "moderator"],
            "metadata": {
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-12-01T00:00:00Z",
                "last_login": "2024-12-15T12:00:00Z",
                "preferences": {
                    "theme": if i % 2 == 0 { "dark" } else { "light" },
                    "language": "en",
                    "notifications": true
                }
            }
        })).collect::<Vec<_>>(),
        "settings": {
            "app_name": "Test Application",
            "version": "1.2.3",
            "features": {
                "feature_a": true,
                "feature_b": false,
                "feature_c": true,
                "feature_d": {
                    "enabled": true,
                    "config": {
                        "param1": 100,
                        "param2": "value",
                        "param3": [1, 2, 3, 4, 5]
                    }
                }
            }
        },
        "logs": (0..5).map(|i| json!({
            "timestamp": format!("2024-12-01T{:02}:00:00Z", i),
            "level": if i % 3 == 0 { "ERROR" } else if i % 2 == 0 { "WARN" } else { "INFO" },
            "message": format!("Log entry number {}", i),
            "context": {
                "request_id": format!("req-{:04}", i * 100),
                "user_id": i % 10,
                "action": "process_request"
            }
        })).collect::<Vec<_>>()
    });

    // Convert to colored JSON string - this will have LOTS of ANSI color codes
    serde_json::to_string_pretty(&data)
        .unwrap()
        .to_colored_json_auto()
        .unwrap()
}

fn bench_split_into_line_segments(c: &mut Criterion) {
    let colored_json = generate_colored_json_content();

    // Also create a plain text version for comparison
    let plain_text = colored_json
        .chars()
        .filter(|c| *c != '\x1b')
        .collect::<String>()
        .replace("[0m", "")
        .replace("[31m", "")
        .replace("[32m", "")
        .replace("[33m", "")
        .replace("[34m", "")
        .replace("[35m", "")
        .replace("[36m", "")
        .replace("[37m", "")
        .replace("[90m", "")
        .replace("[1m", "")
        .replace("[22m", "");

    let mut group = c.benchmark_group("text_splitting");

    // Benchmark colored JSON
    group.bench_function("colored_json_80_cols", |b| {
        b.iter(|| split_into_line_segments(black_box(&colored_json), black_box(80)));
    });

    // Benchmark plain text
    group.bench_function("plain_text_80_cols", |b| {
        b.iter(|| split_into_line_segments(black_box(&plain_text), black_box(80)));
    });

    group.finish();
}

/// Fill rows `[rows]` of the current frame with `ch`.
fn fill_rows(
    t: &mut Terminal<CrosstermBackend<Vec<u8>>>,
    ch: char,
    rows: std::ops::Range<u16>,
    width: u16,
) {
    let line: String = ch.to_string().repeat(width as usize);
    let mut frame = t.get_frame();
    let buf = frame.buffer_mut();
    for y in rows {
        buf.set_string(0, y, &line, Style::default());
    }
}

/// Build a terminal in a "ready to flush" state: a previous full-screen frame,
/// then a partial-redraw current frame (a few changed rows, like streaming
/// output) with `num_links` hyperlinks set. The returned terminal is cloned per
/// benchmark iteration so the measured call is just the flush.
fn dirty_terminal(
    width: u16,
    height: u16,
    num_links: usize,
) -> Terminal<CrosstermBackend<Vec<u8>>> {
    let area = Rect::new(0, 0, width, height);
    let mut t = Terminal::with_options(
        CrosstermBackend::new(Vec::<u8>::new()),
        TerminalOptions {
            viewport: Viewport::Fixed(area),
        },
    )
    .unwrap();

    // Previous frame: full screen of 'a'.
    fill_rows(&mut t, 'a', 0..height, width);
    t.set_frame_links(&[]);
    let _ = t.flush_with_links();
    t.swap_buffers();

    // Current frame: mostly unchanged ('a'), a few changed rows ('b') — a
    // realistic partial redraw. The diff still visits every cell, which is where
    // the per-cell link resolution cost lives.
    fill_rows(&mut t, 'a', 0..height, width);
    fill_rows(&mut t, 'b', 0..height.min(3), width);

    let spans: Vec<LinkSpan> = (0..num_links)
        .map(|i| {
            let row = (i as u16) % height;
            LinkSpan {
                row,
                col_start: 0,
                col_end: width.min(24),
                url: "https://example.com/some/path".into(),
                id: Some(i as u32),
            }
        })
        .collect();
    t.set_frame_links(&spans);
    t
}

/// Benchmarks the OSC 8 hyperlink render path on a 256x100 viewport: the plain
/// `flush` baseline, `flush_with_links` with no links (early-exit fast path),
/// and `flush_with_links` with 50 links (the link-aware diff + emit).
fn bench_flush_with_links(c: &mut Criterion) {
    const W: u16 = 256;
    const H: u16 = 100;

    let mut group = c.benchmark_group("hyperlink_flush");

    group.bench_function("flush_baseline_no_links", |b| {
        b.iter_batched(
            || dirty_terminal(W, H, 0),
            |mut t| black_box(t.flush()),
            BatchSize::SmallInput,
        );
    });

    group.bench_function("flush_with_links_no_links", |b| {
        b.iter_batched(
            || dirty_terminal(W, H, 0),
            |mut t| black_box(t.flush_with_links()),
            BatchSize::SmallInput,
        );
    });

    group.bench_function("flush_with_links_50_links", |b| {
        b.iter_batched(
            || dirty_terminal(W, H, 50),
            |mut t| black_box(t.flush_with_links()),
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_split_into_line_segments,
    bench_flush_with_links
);
criterion_main!(benches);
