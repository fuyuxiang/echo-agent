//! Criterion benchmarks for the terminal-RESIZE path.
//!
//! Dragging a terminal edge sends a stream of `Event::Resize`, and the
//! reported symptom is that the drag gets laggier the longer a session runs.
//!
//! Regressions these guard against, each of which was measured on a real
//! session and removed:
//! - re-deriving an entry's source text per width instead of reusing its
//!   cached line-width profile (makes the estimate pass O(conversation bytes)),
//! - building or cloning an `AppearanceConfig` per entry,
//! - running `warm_measure_pages_above` on every resize instead of once the
//!   width settles.

use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};

use echo_agent_pager::scrollback::{RenderBlock, ScrollbackState};

/// Roughly a VS Code editor pane maximized on a laptop screen.
const VIEWPORT_WIDTH: u16 = 120;
const VIEWPORT_HEIGHT: u16 = 50;

/// ~3,200 entries / ~5 MB of text — the scale of a multi-hour session.
const TURNS: usize = 400;

fn agent_markdown(i: usize) -> String {
    format!(
        "Here is what I found for step {i}.\n\n\
         The `ScrollbackState` keeps a layout cache keyed by width, so a resize \
         invalidates every entry. That matters because the estimate pass has to \
         re-derive each block's source text before it can compute a height.\n\n\
         - first observation about entry {i}\n\
         - second observation, slightly longer, about how the wrap cache is keyed \
           on `(width, generation, theme)` and therefore misses after a drag\n\
         - third observation\n\n\
         ```rust\n\
         fn rebuild_layout_cache(&mut self, width: u16) {{\n\
         \x20   for entry in self.entries.values() {{\n\
         \x20       let renderer = EntryRenderer::new(entry, &theme)\n\
         \x20           .with_appearance(self.appearance.clone());\n\
         \x20       let height = renderer.estimate_height(width);\n\
         \x20   }}\n\
         }}\n\
         ```\n\n\
         In short: the {i}th response re-wraps on every width change, and the \
         syntax highlighting of the fence above is recomputed with it. \
         {}\n",
        "Additional prose so the message spans several wrapped rows. ".repeat(6)
    )
}

fn thinking_text(i: usize) -> String {
    format!(
        "Considering approach {i}. {}",
        "The user asked about resize latency, so I should look at the layout cache. ".repeat(8)
    )
}

fn edit_texts(i: usize) -> (String, String) {
    let old = format!(
        "fn handler_{i}(req: Request) -> Response {{\n\
         \x20   let body = req.body();\n\
         \x20   let parsed = serde_json::from_slice(body)?;\n\
         \x20   Response::ok(parsed)\n\
         }}\n"
    );
    let new = format!(
        "fn handler_{i}(req: Request) -> Response {{\n\
         \x20   let body = req.body();\n\
         \x20   let parsed: Payload = serde_json::from_slice(body)\n\
         \x20       .map_err(|e| Error::BadRequest(e.to_string()))?;\n\
         \x20   Response::ok(parsed)\n\
         }}\n"
    );
    (old, new)
}

fn bash_output(i: usize) -> String {
    (0..40)
        .map(|l| format!("terminal/echo-agent-pager/src/file_{i}_{l}.rs:{l}: match found"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Block mix and proportions follow what a real coding session produces.
fn build_session() -> (ScrollbackState, usize) {
    let mut state = ScrollbackState::new();
    let mut bytes = 0usize;
    let mut push = |state: &mut ScrollbackState, block: RenderBlock, n: usize| {
        bytes += n;
        state.push_block(block);
    };
    for i in 0..TURNS {
        let p = format!("please investigate issue {i} and report back with a plan");
        push(&mut state, RenderBlock::user_prompt(p.clone()), p.len());
        let t = thinking_text(i);
        push(&mut state, RenderBlock::thinking(t.clone()), t.len());
        let out = bash_output(i);
        push(
            &mut state,
            RenderBlock::execute_with_output(
                format!("rg -n 'pattern{i}' crates/"),
                out.clone(),
                None::<String>,
            ),
            out.len(),
        );
        push(
            &mut state,
            RenderBlock::read(
                format!("terminal/echo-agent-pager/src/mod_{i}.rs"),
                None,
            ),
            64,
        );
        let (old, new) = edit_texts(i);
        push(
            &mut state,
            RenderBlock::edit_with_hunks(
                format!("terminal/echo-agent-pager/src/mod_{i}.rs"),
                echo_agent_pager_diff::diff_hunks_from_strings(&old, &new, 1),
            ),
            old.len() + new.len(),
        );
        push(
            &mut state,
            RenderBlock::search(format!("fn handler_{i}"), 12, Vec::new()),
            48,
        );
        let md = agent_markdown(i);
        push(&mut state, RenderBlock::agent_message(md.clone()), md.len());
        let t2 = thinking_text(i + 1);
        push(&mut state, RenderBlock::thinking(t2.clone()), t2.len());
    }
    state.prepare_layout(VIEWPORT_WIDTH, VIEWPORT_HEIGHT);
    (state, bytes)
}

fn bench_resize_step(c: &mut Criterion) {
    let (mut state, bytes) = build_session();
    eprintln!(
        "resize corpus: {} entries, ~{:.1} MB text",
        state.len(),
        bytes as f64 / (1024.0 * 1024.0)
    );
    let mut g = c.benchmark_group("resize");
    g.sample_size(20).warm_up_time(Duration::from_millis(500));
    g.bench_function("width_step", |b| {
        let mut w = VIEWPORT_WIDTH;
        b.iter(|| {
            w = if w == VIEWPORT_WIDTH {
                VIEWPORT_WIDTH - 1
            } else {
                VIEWPORT_WIDTH
            };
            state.prepare_layout(w, VIEWPORT_HEIGHT);
        });
    });
    g.finish();
}

fn bench_resize_drag(c: &mut Criterion) {
    let (mut state, _) = build_session();
    let mut g = c.benchmark_group("resize");
    g.sample_size(10).warm_up_time(Duration::from_millis(500));
    g.bench_function("drag_20_steps", |b| {
        b.iter(|| {
            for step in 0..20u16 {
                state.prepare_layout(VIEWPORT_WIDTH - step, VIEWPORT_HEIGHT);
            }
            state.prepare_layout(VIEWPORT_WIDTH, VIEWPORT_HEIGHT);
        });
    });
    g.finish();
}

fn bench_resize_noop(c: &mut Criterion) {
    let (mut state, _) = build_session();
    let mut g = c.benchmark_group("resize");
    g.bench_function("same_width_noop", |b| {
        b.iter(|| {
            state.prepare_layout(VIEWPORT_WIDTH, VIEWPORT_HEIGHT);
        });
    });
    g.finish();
}

criterion_group!(
    benches,
    bench_resize_step,
    bench_resize_drag,
    bench_resize_noop
);
criterion_main!(benches);
