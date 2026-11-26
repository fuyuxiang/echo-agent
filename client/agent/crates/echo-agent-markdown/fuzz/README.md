# Fuzzing echo-agent-markdown

Coverage-guided fuzzing for the markdown renderer using [cargo-fuzz](https://rust-fuzz.github.io/book/cargo-fuzz.html) (libFuzzer).

## Prerequisites

```bash
cargo install cargo-fuzz   # if not already installed
rustup toolchain install nightly
```

## Targets

| Target | What it fuzzes |
|---|---|
| `render_all` | All 8 combos: `pretty × syntect × {full, streaming}` for every input |

Each iteration runs:
- `render_markdown_ratatui_full()` — 4 combos (pretty/non-pretty × syntect/no-syntect)
- `StreamingMarkdownRenderer` char-by-char — same 4 combos

## Running

From `crates/echo-agent-markdown`:

```bash
# Run indefinitely (Ctrl-C to stop):
cargo +nightly fuzz run render_all fuzz/corpus/render_all fuzz/seeds/render_all -- -max_len=16384

# Run for 5 minutes:
cargo +nightly fuzz run render_all fuzz/corpus/render_all fuzz/seeds/render_all -- -max_len=16384 -max_total_time=300
```

- `corpus/` — auto-generated inputs (gitignored)
- `seeds/` — hand-written seed inputs (checked in)

## Reproducing a crash

When a crash is found, the input is saved to `artifacts/render_all/crash-<hash>`. Reproduce it with:

```bash
cargo +nightly fuzz run render_all fuzz/artifacts/render_all/crash-<hash>
```

## Adding seed inputs

Drop `.txt` or `.md` files into `seeds/render_all/`. Good seeds cover distinct markdown features (tables, code blocks, emoji, nested lists, etc.) and help the fuzzer reach new code paths faster.
