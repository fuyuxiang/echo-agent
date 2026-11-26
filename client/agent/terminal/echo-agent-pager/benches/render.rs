//! Criterion benchmarks for the echo-agent-pager rendering pipeline.
//!
//! Measures the per-frame cost of rendering a rich markdown document
//! into a ratatui `Buffer`.  This isolates the render hot path (entry
//! rendering, scratch buffer copies, layout computation) from one-time
//! setup (markdown parsing, syntax highlighting, word wrapping).

use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use echo_agent_pager::appearance::AppearanceConfig;
use echo_agent_pager::render::Renderable;
use echo_agent_pager::scrollback::entry::ScrollbackEntry;
use echo_agent_pager::scrollback::render::render_scrolled_entries_with_scratch;
use echo_agent_pager::scrollback::wrappers::EntryRenderer;
use echo_agent_pager::scrollback::{
    EntryId, EntryLayoutInfo, HorizontalLayout, RenderBlock, ScrollbackState,
};
use echo_agent_pager::theme::Theme;

static BENCH_MD: &str = include_str!("bench.md");

/// Viewport dimensions for the benchmark.
const VIEWPORT_WIDTH: u16 = 120;
const VIEWPORT_HEIGHT: u16 = 50;

/// How many lines to advance per step in full_scroll.
const SCROLL_STEP: u16 = 10;

/// Entry count for the reveal benchmarks. Approximates the ~3,200-entry
/// scrollback of a long real session (~5 MB of searchable text).
const REVEAL_ENTRIES: usize = 3000;

/// Build the entries used by every benchmark iteration.
///
/// This is called once in the setup closure so that markdown parsing,
/// syntax highlighting, and word-wrap caching are **not** measured.
fn build_entries() -> Vec<ScrollbackEntry> {
    vec![
        // A user prompt to exercise multi-entry layout
        ScrollbackEntry::new(RenderBlock::user_prompt(
            "Explain the rendering pipeline architecture in detail",
        )),
        // The main payload — a large markdown agent response
        ScrollbackEntry::new(RenderBlock::agent_message(BENCH_MD)),
    ]
}

/// Pre-compute entry layout info (same as prepare_layout does in production).
fn compute_layouts(
    entries: &[ScrollbackEntry],
    appearance: &AppearanceConfig,
) -> Vec<EntryLayoutInfo> {
    let theme = Theme::current();
    let viewport = Rect::new(0, 0, VIEWPORT_WIDTH, VIEWPORT_HEIGHT);
    let layout = HorizontalLayout::new(viewport, &appearance.scrollback.layout);
    let entry_width = layout.entry_content_area().width;

    entries
        .iter()
        .map(|e| {
            let height = EntryRenderer::new(e, &theme)
                .with_appearance(appearance.clone())
                .desired_height(entry_width);
            EntryLayoutInfo {
                height,
                gap_after: 1,
                group_header_count: 0,
                group_collapse_header: false,
                verb_group_header: false,
            }
        })
        .collect()
}

// ─── Benchmarks ────────────────────────────────────────────────────

/// Render a single frame at scroll offset 0 (top of document).
///
/// The per-frame baseline with no top clipping — only bottom-clipped.
fn bench_single_frame(c: &mut Criterion) {
    let entries = build_entries();
    let entry_refs: Vec<&ScrollbackEntry> = entries.iter().collect();
    let viewport = Rect::new(0, 0, VIEWPORT_WIDTH, VIEWPORT_HEIGHT);
    let theme = Theme::current();
    let appearance = AppearanceConfig::default();
    let layouts = compute_layouts(&entries, &appearance);

    // Prime the wrap cache
    {
        let mut buf = Buffer::empty(viewport);
        render_scrolled_entries_with_scratch(
            &mut buf,
            viewport,
            &entry_refs,
            0,
            None,
            &theme,
            &appearance,
            &layouts,
            0,
            None,
            None,
            None,
            0,
            0,
            &[],
            None,
            None,
        );
    }

    c.bench_function("render/single_frame", |b| {
        let mut buf = Buffer::empty(viewport);
        b.iter(|| {
            buf.reset();
            render_scrolled_entries_with_scratch(
                &mut buf,
                viewport,
                &entry_refs,
                0,
                None,
                &theme,
                &appearance,
                &layouts,
                0,
                None,
                None,
                None,
                0,
                0,
                &[],
                None,
                None,
            );
        });
    });
}

/// Scroll through the entire document, stepping by SCROLL_STEP lines.
///
/// Simulates a user paging through the document, exercising the full
/// render pipeline at many offsets including partial entry rendering.
fn bench_full_scroll(c: &mut Criterion) {
    let entries = build_entries();
    let entry_refs: Vec<&ScrollbackEntry> = entries.iter().collect();
    let viewport = Rect::new(0, 0, VIEWPORT_WIDTH, VIEWPORT_HEIGHT);
    let theme = Theme::current();
    let appearance = AppearanceConfig::default();
    let layouts = compute_layouts(&entries, &appearance);

    // usize: scroll offset is usize in the render path.
    let total: usize = layouts
        .iter()
        .map(|l| l.height as usize + l.gap_after as usize)
        .sum(); // heights + gaps
    let max_scroll = total.saturating_sub(VIEWPORT_HEIGHT as usize);

    // Prime the wrap cache
    {
        let mut buf = Buffer::empty(viewport);
        render_scrolled_entries_with_scratch(
            &mut buf,
            viewport,
            &entry_refs,
            0,
            None,
            &theme,
            &appearance,
            &layouts,
            0,
            None,
            None,
            None,
            0,
            0,
            &[],
            None,
            None,
        );
    }

    c.bench_function("render/full_scroll", |b| {
        let mut buf = Buffer::empty(viewport);
        b.iter(|| {
            let mut offset = 0usize;
            while offset <= max_scroll {
                buf.reset();
                render_scrolled_entries_with_scratch(
                    &mut buf,
                    viewport,
                    &entry_refs,
                    offset,
                    None,
                    &theme,
                    &appearance,
                    &layouts,
                    0,
                    None,
                    None,
                    None,
                    0,
                    0,
                    &[],
                    None,
                    None,
                );
                offset += SCROLL_STEP as usize;
            }
        });
    });
}

/// Scroll through a large scrollback the way production does: per step,
/// locate the paint window via `ScrollbackState::paint_window` (partition
/// point over the cached virtual-y prefix sum) and render only that slice
/// with `content_y0`/`entry_index_base` — mirroring
/// `ScrollbackPane::render_content`. `full_scroll` above measures the
/// renderer's full-list walk; this measures the shipped windowed path, so
/// regressions in the window computation show up here.
fn bench_windowed_scroll(c: &mut Criterion) {
    let (state, _think_id) = build_reveal_state();
    let viewport = Rect::new(0, 0, VIEWPORT_WIDTH, VIEWPORT_HEIGHT);
    let theme = Theme::current();
    let n = state.len();
    let virtual_y = state.get_cached_virtual_y().expect("layout cache");
    let layouts = state.get_cached_entry_layouts().expect("layout cache");
    let total: usize =
        virtual_y[n - 1] + layouts[n - 1].height as usize + layouts[n - 1].gap_after as usize;
    let max_scroll = total.saturating_sub(VIEWPORT_HEIGHT as usize);

    let mut g = c.benchmark_group("render");
    // Each iteration pages through the whole corpus; cap samples to keep the
    // run short (same treatment as reveal/navigate_rebuild).
    g.sample_size(10).warm_up_time(Duration::from_millis(500));
    g.bench_function("windowed_scroll", |b| {
        let mut buf = Buffer::empty(viewport);
        b.iter(|| {
            let mut offset = 0usize;
            while offset <= max_scroll {
                buf.reset();
                let (paint_range, content_y0) =
                    state.paint_window(0..n, offset, VIEWPORT_HEIGHT as usize);
                let window = state.entries_in_range(paint_range.clone());
                render_scrolled_entries_with_scratch(
                    &mut buf,
                    viewport,
                    &window,
                    offset,
                    None,
                    &theme,
                    state.appearance(),
                    &layouts[paint_range.clone()],
                    0,
                    None,
                    None,
                    None,
                    content_y0,
                    paint_range.start,
                    &[],
                    None,
                    None,
                );
                offset += SCROLL_STEP as usize;
            }
        });
    });
    g.finish();
}

// ─── Reveal (scrollback-search n/N navigation) ─────────────────────

/// One paragraph of lorem-style body per entry (~1.7 KB), so the
/// `REVEAL_ENTRIES`-entry corpus is on the order of the motivating session's
/// searchable text (a few MB; the exact size is logged).
fn reveal_body(i: usize) -> String {
    let lorem = "lorem ipsum dolor sit amet consectetur adipiscing elit sed do \
                 eiusmod tempor incididunt ut labore et dolore magna aliqua ut \
                 enim ad minim veniam quis nostrud exercitation ullamco laboris ";
    format!(
        "Entry {i}: the quick brown fox jumps over the lazy dog. {} done.",
        lorem.repeat(9)
    )
}

/// Build a large scrollback approximating a long search session, returning it
/// with the `EntryId` of a thinking block placed in the middle (the rebuild
/// bench dirties that entry's height to force reveal's rebuild branch).
fn build_reveal_state() -> (ScrollbackState, EntryId) {
    let mut state = ScrollbackState::new();
    let mut total_bytes = 0usize;
    let middle = REVEAL_ENTRIES / 2;
    let mut think_id = None;
    for i in 0..REVEAL_ENTRIES {
        if i == middle {
            let body = "reasoning about the matched entry and the nearby context";
            total_bytes += body.len();
            think_id = Some(state.push_block(RenderBlock::thinking(body)));
        } else {
            let body = reveal_body(i);
            total_bytes += body.len();
            state.push_block(RenderBlock::user_prompt(body));
        }
    }
    // ~1-2 KB prompts are foldable, so they default to Collapsed; expand them so
    // a reveal over an already-visible match doesn't change display state — the
    // steady n/N case the skip path optimizes. (Setup only; not measured.)
    state.expand_all();
    // Settle the layout cache and clear dirty heights so the measured reveals
    // start from a clean cache.
    state.prepare_layout(VIEWPORT_WIDTH, VIEWPORT_HEIGHT);
    eprintln!(
        "reveal corpus: {REVEAL_ENTRIES} entries, ~{:.1} MB text",
        total_bytes as f64 / (1024.0 * 1024.0)
    );
    (state, think_id.expect("thinking block pushed"))
}

/// New per-`n` cost: revealing an already-visible match reuses the settled
/// cache and only refreshes the cheap total-height sum — no O(history) rebuild.
fn bench_reveal_skip(c: &mut Criterion) {
    let (mut state, _think_id) = build_reveal_state();
    c.bench_function("reveal/navigate_skip", |b| {
        let mut i = 0usize;
        b.iter(|| {
            state.reveal_entry_line(i % REVEAL_ENTRIES, 0);
            i += 1;
        });
    });
}

/// Old per-`n` cost: the pre-fix reveal ran a full layout rebuild unconditionally.
/// The O(1) `push_chunk_to_thinking` dirties one entry's height, negligible next
/// to the O(N) `rebuild_layout` it forces reveal to take on every call.
fn bench_reveal_rebuild(c: &mut Criterion) {
    let (mut state, think_id) = build_reveal_state();
    let mut g = c.benchmark_group("reveal");
    // Each iteration rebuilds the whole layout (O(N)); cap samples so the run
    // doesn't take minutes.
    g.sample_size(20).warm_up_time(Duration::from_millis(500));
    g.bench_function("navigate_rebuild", |b| {
        let mut i = 0usize;
        b.iter(|| {
            state.push_chunk_to_thinking(think_id, "x");
            state.reveal_entry_line(i % REVEAL_ENTRIES, 0);
            i += 1;
        });
    });
    g.finish();
}

criterion_group!(
    benches,
    bench_single_frame,
    bench_full_scroll,
    bench_windowed_scroll,
    bench_reveal_skip,
    bench_reveal_rebuild
);
criterion_main!(benches);
