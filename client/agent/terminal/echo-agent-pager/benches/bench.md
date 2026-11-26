# 🚀 Architecture Overview — `echo-agent-pager` Rendering Engine

The **echo-agent-pager** rendering engine is built on a layered pipeline that transforms raw markdown into terminal-ready cells. This document covers every major subsystem — from markdown parsing through syntax highlighting, word wrapping, block layout, viewport clipping, and final buffer composition. Understanding these layers is critical for anyone profiling or optimising the renderer.

---

## 📐 The Rendering Pipeline

Every frame follows the same sequence of stages. Content flows **downward** through transforms, each adding structure:

1. **Markdown parsing** — `StreamingMarkdownRenderer` converts source text into a tree of styled `Line<'static>` spans. Code fences trigger **syntect** highlighting.
2. **Word wrapping** — `word_wrap_lines_with_joiners()` breaks logical lines into physical rows that fit the viewport width, tracking *joiners* (continuation markers like `↳`) for copy/paste fidelity.
3. **Block output** — `BlockContent::output()` packages wrapped lines into a `BlockOutput` with per-line metadata: background colour, joiner strings, and optional decorations.
4. **Entry rendering** — `EntryRenderer` composes the accent column (`┃`), left/right padding, and block content into a horizontal strip. Vertical padding (vpad) adds breathing room above and below.
5. **Viewport clipping** — `render_scrolled_entries_with_scratch()` walks the entry list, skips off-screen entries, and uses a `ScratchBuffer` to render partially-visible entries into a temp buffer before copying the visible slice.
6. **Buffer diff** — ratatui's `Terminal::flush()` diffs the old and new `Buffer` and emits only changed cells as escape sequences. This is **O(changed cells)**, not O(total cells).

> **💡 Key insight**: steps 1–3 are **cached** across frames. Only step 4–5 run every frame. Profiling should focus there.

### Performance characteristics

| Stage | Complexity | Cached? | Hot path? |
|---|---|---|---|
| Markdown parse | `O(n)` in source length | ✅ Yes, per-generation | ❌ No |
| Syntax highlight | `O(n)` with syntect DFA | ✅ Yes, per-generation | ❌ No |
| Word wrap | `O(lines × width)` | ✅ Yes, `(width, gen)` key | ❌ No |
| `BlockContent::output()` | `O(wrapped_lines)` | ✅ Via `WrapCache` | ⚠️ First call only |
| `EntryRenderer::render()` | `O(height × width)` cell writes | ❌ No | ✅ **Yes** |
| Scratch buffer copy | `O(visible_rows × width)` clones | ❌ No | ✅ **Yes** |
| Buffer diff + flush | `O(changed_cells)` | N/A | ✅ **Yes** |

---

## 🧱 Block Types and Their Render Cost

Each `RenderBlock` variant has different rendering characteristics. Here's a breakdown of the major block types with their typical content patterns and associated costs:

### `AgentMessageBlock` — the heaviest hitter 🔥

Agent messages contain **arbitrary markdown**: paragraphs, code blocks, tables, lists, inline formatting. A single agent response can easily exceed 200 wrapped lines. The `MarkdownContent` subsystem does the heavy lifting:

- `StreamingMarkdownRenderer::push_and_render()` incrementally parses and highlights
- `word_wrap_lines_with_joiners()` handles Unicode-aware line breaking with `unicode-width`
- Wide characters (CJK, emoji) consume 2 columns: `'🦀'.width() == 2`, `'λ'.width() == 1`

```rust
/// The core markdown-to-lines pipeline.
///
/// This function is called on every content mutation (push_chunk, finish)
/// and produces the canonical `Vec<Line<'static>>` that gets cached.
pub fn render_markdown(source: &str, pretty: bool) -> Vec<Line<'static>> {
    let mut renderer = StreamingMarkdownRenderer::new(MD_STYLE, pretty);
    renderer.push(source);
    renderer.render(Some(get_syntect()));
    renderer.view().lines.to_vec()
}

/// Word-wrap with joiner tracking for copy fidelity.
///
/// Each output line knows whether it's a continuation of the previous
/// logical line (joiner = Some("↳")) or a fresh line (joiner = None).
/// This matters for selection/copy: we strip joiners when copying.
pub fn word_wrap_lines_with_joiners(
    lines: Vec<Line<'static>>,
    max_width: usize,
) -> (Vec<Line<'static>>, Vec<Option<String>>) {
    let mut wrapped = Vec::with_capacity(lines.len() * 2);
    let mut joiners = Vec::with_capacity(lines.len() * 2);
    for line in lines {
        let line_width = line.width();
        if line_width <= max_width {
            wrapped.push(line);
            joiners.push(None);
        } else {
            // Split at grapheme cluster boundaries respecting unicode width.
            // This is the expensive path — O(spans × chars) per line.
            let parts = split_line_at_width(&line, max_width);
            for (i, part) in parts.into_iter().enumerate() {
                wrapped.push(part);
                joiners.push(if i > 0 { Some("↳".into()) } else { None });
            }
        }
    }
    (wrapped, joiners)
}
```

### `ThinkingBlock` — truncated by default

Thinking blocks render identically to agent messages but default to `DisplayMode::Truncated` (3 visible lines + `⋯ N more lines`). When expanded, they're as expensive as agent messages. The truncation logic runs *after* wrapping, so the full wrap cost is paid even when collapsed — a potential optimisation target.

### `ToolCallBlock` variants

| Variant | Collapsed height | Expanded cost | Notes |
|---|---|---|---|
| `Execute` | 1 line (command summary) | `O(output_lines)` | Bash output can be huge |
| `Read` | 1 line (path + line count) | `O(file_lines)` | Syntax-highlighted file content |
| `Edit` | 1 line (path + edit count) | `O(diff_lines)` | Diff hunks with `+`/`-` colouring |
| `ListDir` | 1 line (path) | `O(entries)` | Directory tree listing |
| `Search` | 1 line (pattern + count) | `O(matches)` | Grep results with context |
| `Other` | 1 line (tool name) | `O(output)` | Generic tool output |

### `UserPromptBlock` — lightweight ✨

User prompts are short (1–5 lines typically), render with a `┃` accent in `accent_user` colour, and are **never foldable**. They're the cheapest block to render.

---

## 🎨 The Accent Column and Colour Blending

The leftmost column of every entry shows a vertical accent bar `┃`. This serves as a visual type indicator:

- **User prompts**: `accent_user` (Tokyo Night blue, `#7aa2f7`)
- **Tool calls**: `accent_tool` / `accent_success` / `accent_error`
- **Thinking**: `accent_thinking` (purple, `#bb9af7`)
- **Running blocks**: animated wave effect 🌊

The animation uses `blend_color(bg, fg, brightness)` per-row per-frame:

```rust
/// Compute wave brightness for a single row at a given tick.
///
/// Returns a value in [0.2, 1.0] — never fully invisible.
/// The wave travels downward at WAVE_SPEED radians per tick.
pub fn wave_brightness(tick: u64, row: u16, wave_rows: u16, speed: f32) -> f32 {
    let phase = (tick as f32 * speed) - (row as f32 * std::f32::consts::PI / wave_rows as f32);
    let raw = (phase.sin() + 1.0) / 2.0; // normalize to [0, 1]
    0.2 + raw * 0.8 // scale to [0.2, 1.0]
}

/// Linearly blend two RGB colours.
///
/// `opacity = 0.0` → pure `base`; `opacity = 1.0` → pure `color`.
/// Returns `None` if either colour isn't RGB (indexed colours can't blend).
pub fn blend_color(base: Color, color: Color, opacity: f32) -> Option<Color> {
    match (base, color) {
        (Color::Rgb(br, bg, bb), Color::Rgb(cr, cg, cb)) => {
            let r = br as f32 + (cr as f32 - br as f32) * opacity;
            let g = bg as f32 + (cg as f32 - bg as f32) * opacity;
            let b = bb as f32 + (cb as f32 - bb as f32) * opacity;
            Some(Color::Rgb(r as u8, g as u8, b as u8))
        }
        _ => None,
    }
}
```

---

## 📦 The `ScratchBuffer` and Partial Rendering

When an entry is **partially visible** (clipped at top or bottom of the viewport), we can't render directly into the output buffer — we'd write cells outside the visible area. Instead:

1. Resize a reusable `ScratchBuffer` to the entry's full height
2. Render the complete entry into scratch
3. Copy only the visible rows (`skip_rows..skip_rows + visible_height`) into the output

This is the **cell-by-cell copy loop** — one of the hottest paths:

```rust
for dy in 0..visible_rows {
    let src_y = skip_rows + dy;
    let dst_y = dest_area.y + dy;
    for dx in 0..dest_area.width {
        if let Some(src_cell) = temp_buf.cell((dx, src_y))
            && let Some(dst_cell) = buf.cell_mut((dest_area.x + dx, dst_y))
        {
            dst_cell.clone_from(src_cell);
        }
    }
}
```

> **🔬 Optimisation opportunity**: `Cell::clone_from` copies `symbol: String` (24 bytes on stack + possible heap), `fg`, `bg`, `underline_color`, `modifier`, `skip`. A `memcpy`-based bulk row copy could be significantly faster for wide terminals. At `width=200`, that's 200 `clone_from` calls per visible row per frame — potentially 6000 calls for a 30-row viewport with top+bottom clipping.

---

## 🔤 Unicode Width Challenges

Terminal rendering must account for **variable-width characters**. The `unicode-width` crate provides `UnicodeWidthChar::width()` and `UnicodeWidthStr::width()`:

| Character | Example | `width()` | Notes |
|---|---|---|---|
| ASCII | `A`, `z`, `!` | 1 | Basic Latin |
| CJK Unified | `漢`, `字`, `中` | 2 | Chinese/Japanese/Korean ideographs |
| Fullwidth forms | `Ａ`, `Ｂ`, `１` | 2 | Fullwidth ASCII variants |
| Emoji | `🦀`, `🚀`, `🎨` | 2 | Most emoji are wide |
| Combining marks | `é` (e + ◌́) | 1 | Combining char has width 0 |
| Zero-width | ZWJ, ZWNJ | 0 | Used in emoji sequences like 👨‍👩‍👧‍👦 |
| Tab | `\t` | — | Not handled by unicode-width; we expand to spaces |

The word wrapper must **never split a wide character** across the column boundary. If a 2-cell-wide char would start at column `width - 1`, we must wrap it to the next line and pad the current line with a space.

Here's a stress test: `漢字テスト🦀🚀🎨` contains 5 double-width CJK chars (10 columns) plus 3 double-width emoji (6 columns) = 16 columns total. At `width = 10`, this wraps to 2 lines. At `width = 7`, it wraps to 3 lines with padding cells.

---

## 📊 Inline Code and Syntax Highlighting Deep Dive

Inline code uses backtick syntax: `HashMap<String, Vec<u8>>`, `Option<&'a mut T>`, `impl Fn(usize) -> bool`. Each inline code span gets a distinct background colour (`bg_code`) to visually separate it from prose. The renderer must:

1. Parse the backtick delimiter (single `` ` `` or double ``` `` ```)
2. Extract the code content
3. Apply `Style::default().bg(theme.bg_code).fg(theme.fg_code)`
4. Handle **nested formatting** — e.g., `**bold `code` bold**` where code is inside bold

Fenced code blocks trigger full **syntect** highlighting. The highlighting pipeline:

1. Look up the `SyntaxReference` by language identifier (`rust`, `python`, `typescript`, etc.)
2. Create a `HighlightLines` with the Tokyo Night theme
3. Iterate source lines, calling `highlight_line()` to get `Vec<(syntect::Style, &str)>`
4. Convert syntect styles to ratatui `Span` styles (mapping RGB colours)
5. Each line gets `Style::default().bg(theme.bg_dark)` as a block background

The syntect state machine is **line-stateful** — each line's highlighting depends on the parse state at the end of the previous line. This means we can't parallelise highlighting within a single code block, but we *can* cache the result.

---

## 🧪 Testing Patterns

The scrollback rendering has comprehensive snapshot tests using `insta`. Here's the typical pattern:

```python
# This is a Python code block to exercise a different syntax highlighter.
# The renderer must detect the language and switch syntect grammars.

import asyncio
from dataclasses import dataclass, field
from typing import Optional, Dict, List, Tuple

@dataclass
class TrainingConfig:
    """Configuration for a distributed training run. 🔧"""
    model_name: str
    batch_size: int = 32
    learning_rate: float = 3e-4
    max_epochs: int = 100
    gradient_accumulation_steps: int = 1
    warmup_ratio: float = 0.1
    weight_decay: float = 0.01
    devices: List[str] = field(default_factory=lambda: ["cuda:0"])
    mixed_precision: bool = True
    compile_model: bool = False  # torch.compile — can 2× throughput
    checkpoint_dir: Optional[str] = None

    @property
    def effective_batch_size(self) -> int:
        return self.batch_size * self.gradient_accumulation_steps * len(self.devices)

    def validate(self) -> None:
        assert self.batch_size > 0, f"batch_size must be positive, got {self.batch_size}"
        assert 0 < self.learning_rate < 1, f"learning_rate out of range: {self.learning_rate}"
        assert self.max_epochs > 0, f"max_epochs must be positive, got {self.max_epochs}"
        for device in self.devices:
            assert device.startswith(("cuda", "cpu")), f"unknown device: {device}"


async def train_epoch(
    model,
    dataloader,
    optimizer,
    scheduler,
    config: TrainingConfig,
    epoch: int,
) -> Dict[str, float]:
    """Run a single training epoch. Returns metrics dict. 📈"""
    model.train()
    total_loss = 0.0
    num_batches = 0

    for batch_idx, batch in enumerate(dataloader):
        # Forward pass — compute loss on this micro-batch
        outputs = model(**batch)
        loss = outputs.loss / config.gradient_accumulation_steps
        loss.backward()

        if (batch_idx + 1) % config.gradient_accumulation_steps == 0:
            optimizer.step()
            scheduler.step()
            optimizer.zero_grad()

        total_loss += loss.item() * config.gradient_accumulation_steps
        num_batches += 1

    avg_loss = total_loss / max(num_batches, 1)
    return {"epoch": epoch, "avg_loss": avg_loss, "num_batches": num_batches}
```

---

## ⚡ Benchmarking Strategy

To measure render performance, we need to isolate the **per-frame** cost from one-time setup:

- **Setup** (not measured): Parse markdown, create `ScrollbackEntry`, compute initial wrap cache
- **Measured**: For each scroll offset `0..total_height`, render into a `Buffer` of size `width × viewport_height`

This simulates a user holding down `j` (scroll down) and measures the **worst case** — every frame re-renders the viewport at a new scroll position, exercising:

- `EntryRenderer::render()` — accent, padding, content layout
- `BlockRenderer::render()` — vpad, content lines, background fills
- Partial rendering via `ScratchBuffer` — for clipped entries at top/bottom
- Cell-by-cell copy — the innermost hot loop

### Expected results

On a modern machine (M2 Pro), we expect:

- **~50–200 µs/frame** for a 120×30 viewport with a 200-line markdown document
- **~80% of time** in `EntryRenderer::render()` + scratch buffer copy
- **~15% of time** in `BlockContent::output()` (cache hit path — just iterating cached lines)
- **~5% of time** in layout computation (`HorizontalLayout`, `EntryLayout`, gap math)

If the benchmark shows >500 µs/frame, there's likely an unexpected cache miss or allocation in the hot path. Use `cargo bench -- --profile-time 10` with `flamegraph` to identify the culprit.

---

## 🌐 Miscellaneous Wide Characters and Edge Cases

Here are some strings that exercise interesting rendering edge cases:

- **Emoji sequences**: 👨‍👩‍👧‍👦 (family ZWJ sequence, should be width 2 but terminal support varies)
- **Flags**: 🇺🇸 🇯🇵 🇩🇪 (regional indicator pairs)
- **Fullwidth**: `ＡＢＣＤＥ` (each char is 2 columns wide)
- **Combining**: `naïve` vs `naïve` (precomposed U+00EF vs combining U+0308)
- **Box drawing**: `┌─────────┐│ content │└─────────┘` (all width 1)
- **Mathematical**: `∀x ∈ ℝ : x² ≥ 0`, `∑_{i=0}^{n} aᵢ = S`, `∫₀^∞ e^{-x} dx = 1`
- **CJK mixed**: `これはテストです — this is a test — 這是測試 — 이것은 시험이다`
- **RTL markers**: `Hello ‮dlrow‬!` (contains RLO/PDF override characters)

The renderer must handle all of these without panicking or producing garbled output. The word wrapper is the critical component — it must correctly account for each character's display width when deciding where to break lines.

> **⚠️ Warning**: Some terminals render emoji sequences incorrectly (showing them as 1-wide or as multiple glyphs). Our renderer uses `unicode-width` which reports the **Unicode standard** width, not the terminal's actual rendering width. This is a known source of misalignment — there is no perfect solution without querying the terminal.

---

*Generated for benchmarking purposes. Total: ~230 lines of rich markdown content with multiple code blocks, tables, inline code, emoji, wide Unicode characters, and varied formatting.*
