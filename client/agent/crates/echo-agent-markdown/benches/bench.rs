//! Benchmarks for markdown rendering.

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};

use echo_agent_markdown::{
    MarkdownStyle, StreamingMarkdownRenderer, Syntect, render_markdown_ratatui_full,
};

/// Default style for benchmarking.
fn default_style() -> MarkdownStyle {
    MarkdownStyle::default()
}

/// Create syntect highlighter for benchmarking.
fn create_syntect() -> Syntect {
    Syntect::new(include_bytes!("../assets/tokyo-night.tmTheme"))
}

fn bench_render_markdown(c: &mut Criterion) {
    let syntect = create_syntect();

    c.bench_function("render_markdown", |b| {
        let content = black_box(
            r#"# Heading 1
## Heading 2, can `contain` *formatted* **text**
### Heading 3
#### Heading 4
##### Heading 5
###### Heading 6

This also
=========
Works
-----

Some `inline`, **bold**, ~~strikethrough~~, *italic*, $math$.

- [ ] Task (not done)
    * [x] Subtask (done)
- *Numbered* lists as well
    1. One
    2) Two

---

- [Link](https://example.com)
- [*Link*](https://foo.com "Title A") or [**Link**](https://bar.com 'Title B')
- ![alt](https://image.link) or ![alt](https://image.link "Image title")
- <https://www.markdownguide.org> or <fake@example.com>

***

```javascript
function hello() { // some code
  console.log("Hello, world!");
}
```

> Multi-line
>
> Block-quote
> > and a *nested* one.

```
Plain fenced code block
```

$$
g(t) = \int_a^b K(t,s) f(s) ds
$$

This is *some <a href="https://google.com">inline html</a> block*.

<html>
<foo a="b">HTML block.</foo>
</html>

> [!NOTE]
> note quote

| H 1  | H 2  |
| ---- | ---- |
| C 1  | C 2  |
"#,
        );
        b.iter(|| {
            render_markdown_ratatui_full(content, black_box(default_style()), true, Some(&syntect))
                .0
                .lines
                .len()
        })
    });
}

/// Generate a markdown document with multiple blocks for streaming simulation.
fn generate_streaming_content(num_blocks: usize) -> String {
    let mut content = String::new();
    for i in 0..num_blocks {
        match i % 5 {
            0 => {
                content.push_str(&format!("# Heading {}\n\n", i));
            }
            1 => {
                content.push_str(&format!(
                    "This is paragraph {} with some **bold** and *italic* text.\n\n",
                    i
                ));
            }
            2 => {
                content.push_str(&format!(
                    "```rust\nfn block_{}() {{\n    // code\n}}\n```\n\n",
                    i
                ));
            }
            3 => {
                content.push_str(&format!("> Quote block {}\n> More quoted text.\n\n", i));
            }
            4 => {
                content.push_str(&format!(
                    "- Item {}.1\n- Item {}.2\n- Item {}.3\n\n",
                    i, i, i
                ));
            }
            _ => unreachable!(),
        }
    }
    content
}

/// Benchmark streaming with full re-render on each token (O(N²) baseline).
fn bench_streaming_full_rerender(c: &mut Criterion) {
    let mut group = c.benchmark_group("streaming");
    let syntect = create_syntect();

    for num_blocks in [10, 50, 100] {
        let content = generate_streaming_content(num_blocks);
        let tokens: Vec<&str> = content.split_inclusive(char::is_whitespace).collect();

        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}/full", num_blocks)),
            &tokens,
            |b, tokens| {
                b.iter(|| {
                    let mut text = String::new();
                    let mut total_lines = 0;
                    for token in tokens.iter() {
                        text.push_str(token);
                        let (output, _) = render_markdown_ratatui_full(
                            &text,
                            default_style(),
                            true,
                            Some(&syntect),
                        );
                        total_lines = output.lines.len();
                    }
                    black_box(total_lines)
                });
            },
        );
    }

    group.finish();
}

/// Generate a hyperlink-heavy markdown document.
///
/// Each block contains 4 inline links and 1 autolink so the renderer's
/// link-translation path is exercised on most rendered lines.  Designed
/// to surface O(lines * link_targets) costs in `translate_link_targets`.
fn generate_hyperlink_content(num_blocks: usize) -> String {
    let mut content = String::new();
    for i in 0..num_blocks {
        content.push_str(&format!("# Section {}\n\n", i));
        content.push_str(&format!(
            "See [docs {i}](https://example.com/docs/{i}), [api {i}](https://example.com/api/{i}), [src {i}](https://github.com/x/r/blob/main/src{i}.rs), and [issue {i}](https://github.com/x/r/issues/{i}).\n\n",
            i = i,
        ));
        content.push_str(&format!(
            "Reference link: <https://reference.example.com/path/segment-{}/page>.\n\n",
            i
        ));
        content.push_str(&format!(
            "Mixed prose with [click](https://a{i}.com) and [click](https://b{i}.com) repeating the same text twice on one line for collision testing.\n\n",
            i = i,
        ));
    }
    content
}

/// Benchmark a single full render of a hyperlink-heavy document.
///
/// Surfaces the cost of the parse-time `link_targets` collection plus the
/// post-render translation step.  Pair with `bench_render_markdown` to see
/// the link-translation overhead in isolation.
fn bench_render_markdown_hyperlinks(c: &mut Criterion) {
    let syntect = create_syntect();
    let mut group = c.benchmark_group("render_markdown_hyperlinks");

    for num_blocks in [10, 50, 200] {
        let content = generate_hyperlink_content(num_blocks);
        group.bench_with_input(
            BenchmarkId::from_parameter(num_blocks),
            &content,
            |b, content| {
                b.iter(|| {
                    let (out, _) = render_markdown_ratatui_full(
                        content,
                        black_box(default_style()),
                        true,
                        Some(&syntect),
                    );
                    (out.lines.len(), out.hyperlinks.len())
                });
            },
        );
    }
    group.finish();
}

/// Benchmark incremental streaming of a hyperlink-heavy document.
///
/// Exercises `rerender_tail` repeatedly, which calls the link-translation
/// path on the unfrozen tail every push.
fn bench_streaming_hyperlinks_incremental(c: &mut Criterion) {
    let mut group = c.benchmark_group("streaming_hyperlinks");
    let syntect = create_syntect();

    for num_blocks in [10, 50] {
        let content = generate_hyperlink_content(num_blocks);
        let tokens: Vec<&str> = content.split_inclusive(char::is_whitespace).collect();

        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}/incremental", num_blocks)),
            &tokens,
            |b, tokens| {
                b.iter(|| {
                    let mut renderer = StreamingMarkdownRenderer::new(default_style(), true);
                    let mut total_links = 0;
                    for token in tokens.iter() {
                        renderer.push_and_render(token, Some(&syntect));
                        total_links = renderer.view().hyperlinks.len();
                    }
                    black_box(total_links)
                });
            },
        );
    }

    group.finish();
}

/// Benchmark streaming with incremental renderer (O(N) target).
fn bench_streaming_incremental(c: &mut Criterion) {
    let mut group = c.benchmark_group("streaming");
    let syntect = create_syntect();

    for num_blocks in [10, 50, 100] {
        let content = generate_streaming_content(num_blocks);
        let tokens: Vec<&str> = content.split_inclusive(char::is_whitespace).collect();

        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}/incremental", num_blocks)),
            &tokens,
            |b, tokens| {
                b.iter(|| {
                    let mut renderer = StreamingMarkdownRenderer::new(default_style(), true);
                    let mut total_lines = 0;
                    for token in tokens.iter() {
                        renderer.push_and_render(token, Some(&syntect));
                        total_lines = renderer.view().lines.len();
                    }
                    black_box(total_lines)
                });
            },
        );
    }

    group.finish();
}

/// Generate a math-heavy markdown document.
///
/// Each block exercises all four delimiter forms (`$...$`, `$$...$$`,
/// `\(...\)`, `\[...\]`) plus the expensive converter paths: scripts,
/// fractions, roots, symbol lookups, alphabets, and multi-row environments
/// (aligned / pmatrix / cases) that go through the MathBox 2D layout.
fn generate_math_content(num_blocks: usize) -> String {
    let mut content = String::new();
    for i in 0..num_blocks {
        content.push_str(&format!("## Block {i}: norm \\(\\|x\\|_{{{i}}}\\)\n\n"));
        content.push_str(&format!(
            "Inline $e^{{i\\pi}} + {i} = \\frac{{\\alpha_{i}}}{{\\beta^2}}$ and \
             \\(\\sqrt[3]{{x_{i}}} \\le \\mathbb{{R}}^n\\) mid-prose, then \
             $\\sum_{{k=0}}^{{{i}}} \\binom{{n}}{{k}} \\approx 2^n$.\n\n",
        ));
        content.push_str(&format!(
            "$$\n\\int_0^{i} \\hat{{f}}(t) \\, dt = \\lim_{{n \\to \\infty}} \
             \\frac{{{i}}}{{n+1}}\n$$\n\n",
        ));
        content.push_str(&format!(
            "\\[\n\\begin{{aligned}}\nf_{i}(x) &= x^{i} + \\gamma \\\\\n\
             g_{i}(x) &= \\nabla f_{i} \\cdot \\vec{{v}}\n\\end{{aligned}}\n\\]\n\n",
        ));
        content.push_str(
            "\\[\nA = \\begin{pmatrix} 1 & 2 \\\\ 3 & 4 \\end{pmatrix}, \\quad \
             |x| = \\begin{cases} x & x \\ge 0 \\\\ -x & x < 0 \\end{cases}\n\\]\n\n",
        );
    }
    content
}

/// Benchmark a single full render of a math-heavy document.
///
/// Surfaces the cost of the LaTeX → Unicode converter plus the parse-time
/// `\(...\)` / `\[...\]` source scans and block replacements.
fn bench_render_markdown_math(c: &mut Criterion) {
    let syntect = create_syntect();
    let mut group = c.benchmark_group("render_markdown_math");

    for num_blocks in [10, 50, 200] {
        let content = generate_math_content(num_blocks);
        group.bench_with_input(
            BenchmarkId::from_parameter(num_blocks),
            &content,
            |b, content| {
                b.iter(|| {
                    let (out, _) = render_markdown_ratatui_full(
                        content,
                        black_box(default_style()),
                        true,
                        Some(&syntect),
                    );
                    out.lines.len()
                });
            },
        );
    }
    group.finish();
}

/// Benchmark incremental streaming of a math-heavy document.
///
/// Exercises the streaming hot path the `MAX_MATH_SOURCE_LEN` guard
/// protects: every push re-renders the unfrozen tail, re-running the math
/// scans and conversions on it.
fn bench_streaming_math_incremental(c: &mut Criterion) {
    let mut group = c.benchmark_group("streaming_math");
    let syntect = create_syntect();

    for num_blocks in [10, 50] {
        let content = generate_math_content(num_blocks);
        let tokens: Vec<&str> = content.split_inclusive(char::is_whitespace).collect();

        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}/incremental", num_blocks)),
            &tokens,
            |b, tokens| {
                b.iter(|| {
                    let mut renderer = StreamingMarkdownRenderer::new(default_style(), true);
                    let mut total_lines = 0;
                    for token in tokens.iter() {
                        renderer.push_and_render(token, Some(&syntect));
                        total_lines = renderer.view().lines.len();
                    }
                    black_box(total_lines)
                });
            },
        );
    }

    group.finish();
}

/// Generate a document with plain URLs in prose (no markdown link syntax).
fn generate_plain_url_content(num_blocks: usize) -> String {
    let mut content = String::new();
    for i in 0..num_blocks {
        content.push_str(&format!(
            "See https://example.com/page/{i} for details about topic {i}.\n\n",
        ));
    }
    content
}

/// Benchmark streaming + finish() of a plain-URL-heavy document.
///
/// Exercises the `detect_plain_urls` scan, which after the multi-line-URL
/// fix runs inside both `rerender_tail` (every `push_and_render`) and
/// `finish()`.  Bench numbers from this point forward are not comparable
/// to historical runs that measured the prior `finish()`-only path.
fn bench_streaming_plain_urls_incremental(c: &mut Criterion) {
    let mut group = c.benchmark_group("streaming_plain_urls");
    let syntect = create_syntect();

    for num_blocks in [10, 50] {
        let content = generate_plain_url_content(num_blocks);
        let tokens: Vec<&str> = content.split_inclusive(char::is_whitespace).collect();

        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}/incremental", num_blocks)),
            &tokens,
            |b, tokens| {
                b.iter(|| {
                    let mut renderer = StreamingMarkdownRenderer::new(default_style(), true);
                    for token in tokens.iter() {
                        renderer.push_and_render(token, Some(&syntect));
                    }
                    let view = renderer.finish(Some(&syntect));
                    black_box(view.hyperlinks.len())
                });
            },
        );
    }
    group.finish();
}

/// Generate a realistic nested YAML document of roughly `num_lines` lines.
///
/// Produces nested keys, lists, and scalars (the kind of config an LLM streams
/// into a single fenced block) so the syntect highlighter does real work per
/// line rather than trivial whitespace.
fn generate_yaml_lines(num_lines: usize) -> Vec<String> {
    let mut lines = Vec::with_capacity(num_lines);
    let mut i = 0usize;
    while lines.len() < num_lines {
        lines.push(format!("service_{i}:"));
        lines.push(format!("  name: \"service-{i}\""));
        lines.push("  enabled: true".to_string());
        lines.push(format!("  replicas: {}", i % 7 + 1));
        lines.push("  resources:".to_string());
        lines.push(format!("    cpu: \"{}m\"", (i % 4 + 1) * 250));
        lines.push(format!("    memory: \"{}Mi\"", (i % 8 + 1) * 128));
        lines.push("  env:".to_string());
        lines.push("    - name: LOG_LEVEL".to_string());
        lines.push(format!(
            "      value: \"{}\"",
            if i.is_multiple_of(2) { "info" } else { "debug" }
        ));
        lines.push("    - name: REGION".to_string());
        lines.push(format!("      value: us-east-{}", i % 3 + 1));
        lines.push("  ports:".to_string());
        lines.push(format!("    - {}", 8000 + i));
        lines.push("  tags:".to_string());
        lines.push(format!("    team: team-{}", i % 5));
        i += 1;
    }
    lines.truncate(num_lines);
    lines
}

/// Benchmark streaming a SINGLE open ```yaml fenced block line-by-line WITHOUT
/// ever closing the fence.
///
/// This reproduces the UI-freeze pathology: while the fence is open the block
/// never checkpoints, so every `push_and_render` re-highlights the whole tail.
/// With the incremental open-code cache, per-line cost should stay roughly flat
/// in block size instead of growing linearly (overall O(N) instead of O(N²)).
fn bench_streaming_open_yaml_incremental(c: &mut Criterion) {
    let mut group = c.benchmark_group("streaming_open_yaml");
    let syntect = create_syntect();

    for num_lines in [100, 500, 1081] {
        let lines = generate_yaml_lines(num_lines);

        // Only the cache-on path ships; the parameter is just the line count
        // (no A/B baseline here — the no-cache baseline was measured ad hoc and
        // is not committed).
        group.bench_with_input(
            BenchmarkId::from_parameter(num_lines),
            &lines,
            |b, lines| {
                b.iter(|| {
                    let mut renderer = StreamingMarkdownRenderer::new(default_style(), true);
                    // Open the fence but never close it.
                    renderer.push_and_render("```yaml\n", Some(&syntect));
                    let mut total_lines = 0;
                    for line in lines.iter() {
                        renderer.push_and_render(line, Some(&syntect));
                        renderer.push_and_render("\n", Some(&syntect));
                        total_lines = renderer.view().lines.len();
                    }
                    black_box(total_lines)
                });
            },
        );
    }

    group.finish();
}

/// Closed scala fences inside bullet list items, then `trailing_words` more
/// streamed content in the same never-closing list. Lists can't checkpoint,
/// so the fences stay in the re-rendered tail for the whole stream
/// (~108 ms/token, ~4.5 s UI stall).
fn generate_fence_in_list_content(trailing_words: usize) -> String {
    let mut s = String::new();
    s.push_str("Here is where you're stuck:\n\n");
    // Two list items embedding closed scala fences (the pathological shape).
    for i in 0..2 {
        s.push_str(&format!(
            "- **[File{i}.scala:{}](https://example.com/f{i})** (domain)\n  \
             ```scala\n  \
             final case class SampleRecord{i}(\n      \
             baseId: RecordId,\n      \
             payload: PersistedPayload,\n      \
             creationTimestamp: Instant,\n  \
             ) extends RecordContent {{\n    \
             override lazy val raw: Growable[Raw] = ??? // TODO: implement\n  \
             }}\n  \
             ```\n",
            100 + i,
        ));
    }
    // Continued streaming within the same (never-closing) list context.
    for w in 0..trailing_words {
        if w % 12 == 0 {
            s.push_str("\n- item: ");
        }
        s.push_str(&format!("word{w} "));
    }
    s.push('\n');
    s
}

/// Control: same fences and trailing content at top level with blank lines,
/// so checkpoints advance past the fences.
fn generate_fence_top_level_content(trailing_words: usize) -> String {
    let mut s = String::new();
    s.push_str("Here is where you're stuck:\n\n");
    for i in 0..2 {
        s.push_str(&format!(
            "```scala\nfinal case class SampleRecord{i}(\n    \
             baseId: RecordId,\n    \
             payload: PersistedPayload,\n    \
             creationTimestamp: Instant,\n) extends RecordContent {{\n  \
             override lazy val raw: Growable[Raw] = ??? // TODO: implement\n}}\n```\n\n",
        ));
    }
    for w in 0..trailing_words {
        s.push_str(&format!("word{w} "));
        if w % 12 == 11 {
            s.push_str("\n\n");
        }
    }
    s.push('\n');
    s
}

/// Stream closed-fences-in-open-list token-by-token (`in_list`) against the
/// checkpoint-friendly `top_level` control. `in_list` must stay within a
/// small constant of `top_level`; unbounded growth in `trailing_words` is
/// the regression.
fn bench_streaming_fence_in_list(c: &mut Criterion) {
    let mut group = c.benchmark_group("streaming_fence_in_list");
    group.sample_size(10);
    let syntect = create_syntect();

    for trailing_words in [200, 400, 800] {
        for (variant, content) in [
            ("in_list", generate_fence_in_list_content(trailing_words)),
            (
                "top_level",
                generate_fence_top_level_content(trailing_words),
            ),
        ] {
            let tokens: Vec<&str> = content.split_inclusive(char::is_whitespace).collect();
            group.bench_with_input(
                BenchmarkId::from_parameter(format!("{trailing_words}/{variant}")),
                &tokens,
                |b, tokens| {
                    b.iter(|| {
                        let mut renderer = StreamingMarkdownRenderer::new(default_style(), true);
                        let mut total_lines = 0;
                        for token in tokens.iter() {
                            renderer.push_and_render(token, Some(&syntect));
                            total_lines = renderer.view().lines.len();
                        }
                        black_box(total_lines)
                    });
                },
            );
        }
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_render_markdown,
    bench_render_markdown_hyperlinks,
    bench_render_markdown_math,
    bench_streaming_full_rerender,
    bench_streaming_incremental,
    bench_streaming_hyperlinks_incremental,
    bench_streaming_math_incremental,
    bench_streaming_plain_urls_incremental,
    bench_streaming_open_yaml_incremental,
    bench_streaming_fence_in_list,
);
criterion_main!(benches);
