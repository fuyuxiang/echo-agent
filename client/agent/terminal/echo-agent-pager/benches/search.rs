//! Criterion benchmarks for scrollback search.
//!
//! - `scan` measures the raw regex scan over a large corpus — the work that ran
//!   synchronously on the input thread on every keystroke before the background
//!   daemon, and now runs off-thread.
//! - `query_steady` / `query_cold` measure the UI-thread cost of `update_query`
//!   after the daemon change: a steady keystroke only compiles the matcher and
//!   enqueues the query (the scan is off-thread), while the cold path also
//!   rebuilds and ships the corpus on a content change.

use std::hint::black_box;
use std::time::Duration;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};

use echo_agent_pager::scrollback::{
    RenderBlock, ScrollbackSearchIndex, ScrollbackSearchState, ScrollbackState,
};
use echo_agent_pager::search::{QueryKind, TextMatcher};

/// Roughly the entry count of a long working session.
const CORPUS_ENTRIES: usize = 30_000;

/// Each measured iteration scans (or rebuilds) the whole corpus, so cap the
/// sample count — criterion's default 100 would run for minutes.
const SAMPLE_SIZE: usize = 10;

/// One paragraph of body text per entry, so the whole corpus is on the order of
/// a long session's searchable text (tens of MB; the exact size is logged).
fn entry_body(i: usize) -> String {
    let lorem = "lorem ipsum dolor sit amet consectetur adipiscing elit sed do \
                 eiusmod tempor incididunt ut labore et dolore magna aliqua ut \
                 enim ad minim veniam quis nostrud exercitation ullamco laboris ";
    format!(
        "Entry {i}: the quick brown fox jumps over the lazy dog. {lorem}{lorem}{lorem} \
         function foo_{i} calls bar_{i} and returns baz_{i} after work."
    )
}

/// Build a large scrollback approximating a long session.
fn build_large_scrollback(entries: usize) -> ScrollbackState {
    let mut state = ScrollbackState::new();
    let mut total_bytes = 0usize;
    for i in 0..entries {
        let body = entry_body(i);
        total_bytes += body.len();
        state.push_block(RenderBlock::user_prompt(body));
    }
    eprintln!(
        "scrollback search corpus: {entries} entries, ~{:.1} MB searchable text",
        total_bytes as f64 / (1024.0 * 1024.0)
    );
    state
}

/// The regex scan itself — the work the daemon now runs off the input thread
/// (previously this ran synchronously per keystroke). `fox` appears in every
/// entry, the worst case for match collection.
fn bench_scan(c: &mut Criterion) {
    let state = build_large_scrollback(CORPUS_ENTRIES);
    let mut index = ScrollbackSearchIndex::new();
    index.sync(&state);
    let matcher = TextMatcher::new("fox", QueryKind::Regex);

    let mut g = c.benchmark_group("search");
    g.sample_size(SAMPLE_SIZE)
        .warm_up_time(Duration::from_secs(1));
    g.bench_function("scan", |b| {
        b.iter(|| black_box(index.find(black_box(&matcher))));
    });
    g.finish();
}

/// Steady keystroke after the daemon: the corpus is already shipped, so
/// `update_query` just compiles the matcher and enqueues the query, and `poll`
/// picks up the async result — no scan on the UI thread.
fn bench_query_steady(c: &mut Criterion) {
    let state = build_large_scrollback(CORPUS_ENTRIES);
    let mut search = ScrollbackSearchState::open();
    // Ship the corpus once so the measured calls only enqueue (content unchanged).
    search.update_query("warmup", &state);

    let queries = ["fox", "baz", "lorem", "function"];
    let mut g = c.benchmark_group("search");
    g.sample_size(SAMPLE_SIZE)
        .warm_up_time(Duration::from_secs(1));
    g.bench_function("query_steady", |b| {
        let mut n = 0usize;
        b.iter(|| {
            let q = queries[n % queries.len()];
            n += 1;
            search.update_query(q, &state);
            search.poll();
        });
    });
    g.finish();
}

/// Cold path: a fresh session, so `update_query` also rebuilds and ships the
/// corpus (the per-entry searchable-text cache) before enqueueing the query.
fn bench_query_cold(c: &mut Criterion) {
    let state = build_large_scrollback(CORPUS_ENTRIES);
    let mut g = c.benchmark_group("search");
    g.sample_size(SAMPLE_SIZE)
        .warm_up_time(Duration::from_secs(1));
    g.bench_function("query_cold", |b| {
        b.iter_batched(
            ScrollbackSearchState::open,
            |mut search| {
                search.update_query("fox", &state);
            },
            BatchSize::SmallInput,
        );
    });
    g.finish();
}

criterion_group!(benches, bench_scan, bench_query_steady, bench_query_cold);
criterion_main!(benches);
