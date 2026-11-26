//! Streaming-render syntect caches for fenced code blocks in the unfrozen
//! tail. Two complementary strategies behind one entry point
//! ([`OpenCodeHighlighter::highlight_block`]):
//!
//! - **Still-open trailing block** (closing ``` not arrived): persists
//!   syntect's *resumable* per-line state ([`ParseState`]/[`HighlightState`])
//!   across `rerender_tail` calls so each committed line is highlighted
//!   exactly once. Without it, every push re-ran syntect over the whole
//!   growing block — O(N²) over the stream (~35 ms/push near the end of a
//!   ~1000-line block).
//! - **Closed blocks trapped in the tail** (e.g. inside an open list, which
//!   can never checkpoint): memoizes the batch highlight per
//!   `(fence_info, body)` so syntect runs once per distinct fence body
//!   instead of once per streamed chunk (~50–100 ms per re-run,
//!   recorded 4.5 s UI freeze).
//!
//! Both paths are byte-identical to a one-shot batch render. Invalidation is
//! wholesale: the streaming renderer drops this struct on any
//! theme/style/width reset.
//!
//! # Invariants relied upon (open-block path)
//!
//! - **Append-only:** while a block is open the source only grows by appending,
//!   and (because nothing freezes) the block's start offset within the tail is
//!   stable. Both are guarded defensively here; on any mismatch the persisted
//!   state is discarded and rebuilt from scratch.
//! - **One `Event::Text` per pass:** `TextMergeWithOffset` coalesces the block
//!   body into a single `Event::Text`, so this is invoked once per block per
//!   render pass and only needs to persist *across* passes.

use std::collections::HashMap;

use syntect::highlighting::{
    HighlightIterator, HighlightState, Highlighter, Style as SyntectStyle,
};
use syntect::parsing::{ParseState, ScopeStack};
use syntect::util::LinesWithEndings;

use crate::syntax::{Syntect, syntax_highlight_raw};

/// Per-line highlight output: styled `(style, text)` segments for one line.
type HlLine = Vec<(SyntectStyle, String)>;

/// Byte budget for memoized closed-fence bodies; cleared wholesale on
/// overflow. Sized in body bytes (not entries) because pulldown can split a
/// list-indented fence into per-line `Event::Text` fragments — an entry
/// count would overflow on one large fence. If live bodies ever exceed the
/// budget the memo degrades to recomputing each pass (the pre-memo batch
/// behavior), never to unbounded memory or wrong output.
const CLOSED_MEMO_CAP_BYTES: usize = 256 * 1024;

/// Streaming syntect caches for fenced code blocks in the unfrozen tail:
/// incremental state for the single still-open trailing block, plus a memo
/// for closed blocks the tail re-parses every pass (see module docs).
///
/// Owns all the low-level syntect state so the parser/renderer don't have to.
pub(crate) struct OpenCodeHighlighter {
    /// Language/info token of the block currently cached. A change means a
    /// different syntax (and colors), so the cache must be rebuilt.
    fence_info: String,
    /// Block start offset within the tail. A change means we are looking at a
    /// different block, so the cache must be rebuilt.
    start_in_tail: usize,
    /// Bytes highlighted up to and including the last committed `\n`.
    committed_len: usize,
    /// Highlighted, newline-terminated lines (one entry per committed line).
    committed_lines: Vec<HlLine>,
    /// syntect parse state AFTER the last committed (newline-terminated) line.
    parse_state: ParseState,
    /// syntect highlight state AFTER the last committed line.
    highlight_state: HighlightState,
    /// Memo for **closed** fences still in the unfrozen tail
    /// (`fence_info -> body -> highlighted lines`). Nested maps keep the hot
    /// lookup allocation-free; invalidation is inherited from `self` (the
    /// streaming renderer drops this struct on any theme/style/width reset).
    closed_memo: HashMap<String, HashMap<String, Vec<HlLine>>>,
    /// Total body bytes currently memoized, for the `CLOSED_MEMO_CAP_BYTES`
    /// budget check.
    closed_memo_bytes: usize,
}

impl OpenCodeHighlighter {
    /// Create an empty cache. The `parse_state`/`highlight_state` are seeded
    /// with the plain-text syntax purely as placeholders: the empty
    /// `fence_info` sentinel guarantees the first real
    /// [`highlight`](Self::highlight) call takes the rebuild branch and
    /// discards them in favour of the correct syntax.
    pub(crate) fn new(syn: &Syntect) -> Self {
        let highlighter = Highlighter::new(&syn.theme);
        Self {
            fence_info: String::new(),
            start_in_tail: 0,
            committed_len: 0,
            committed_lines: Vec::new(),
            // Seeded invalid; rebuilt on first highlight (see doc above).
            parse_state: ParseState::new(syn.syntax_set.find_syntax_plain_text()),
            highlight_state: HighlightState::new(&highlighter, ScopeStack::new()),
            closed_memo: HashMap::new(),
            closed_memo_bytes: 0,
        }
    }

    /// Highlight a fenced block body from the streaming tail, routing to the
    /// right cache: the incremental open-block path when the body reaches the
    /// EOF of the tail (block still streaming), the closed-fence memo
    /// otherwise. Single entry point so the parser carries no cache policy.
    pub(crate) fn highlight_block(
        &mut self,
        syn: &Syntect,
        fence_info: &str,
        start_in_tail: usize,
        body_reaches_eof: bool,
        text: &str,
    ) -> Option<Vec<HlLine>> {
        if body_reaches_eof {
            self.highlight(syn, fence_info, start_in_tail, text)
        } else {
            self.highlight_closed(syn, fence_info, text)
        }
    }

    /// Batch-highlight a **closed** fence body, memoized on
    /// `(fence_info, body)`.
    ///
    /// Closed fences trapped in an unfreezable tail (e.g. inside an open
    /// list) are re-parsed by every `rerender_tail` pass; the memo makes
    /// syntect run once per distinct body. The compute path *is*
    /// [`syntax_highlight_raw`], so output is byte-identical by construction.
    /// Theme stability follows [`highlight`](Self::highlight): the streaming
    /// renderer drops this struct on any style change.
    fn highlight_closed(
        &mut self,
        syn: &Syntect,
        fence_info: &str,
        text: &str,
    ) -> Option<Vec<HlLine>> {
        if let Some(hit) = self.closed_memo.get(fence_info).and_then(|m| m.get(text)) {
            // Hit clone is the same accepted O(lines)/pass residual as the
            // open-block return below (see TODO on `highlight`).
            return Some(hit.clone());
        }
        let lines = syntax_highlight_raw(Some(syn), fence_info, text)?;
        if self.closed_memo_bytes.saturating_add(text.len()) > CLOSED_MEMO_CAP_BYTES {
            self.closed_memo.clear();
            self.closed_memo_bytes = 0;
        }
        let prev = self
            .closed_memo
            .entry(fence_info.to_owned())
            .or_default()
            .insert(text.to_owned(), lines.clone());
        debug_assert!(prev.is_none(), "miss-checked key cannot already exist");
        self.closed_memo_bytes += text.len();
        Some(lines)
    }

    /// Test-only view of memoized closed-fence body bytes.
    #[cfg(test)]
    pub(crate) fn closed_memo_bytes(&self) -> usize {
        self.closed_memo_bytes
    }

    /// Highlight the open block body `text` (the full body so far, append-only),
    /// reusing persisted syntect state where possible.
    ///
    /// Returns one styled line per source line (including the trailing partial
    /// line if the body does not end in `\n`), matching what a batch
    /// `HighlightLines` run would produce. Returns `None` if the fence has no
    /// known syntax or a line fails to parse, so the caller can fall back to the
    /// plain/untagged code path exactly like [`syntax_highlight_raw`].
    ///
    /// # Theme stability invariant
    ///
    /// The persisted state and the already-highlighted `committed_lines` bake
    /// in the colors of the `syn.theme` seen so far, so the caller MUST pass a
    /// [`Syntect`] whose `theme` is stable for the lifetime of a given open
    /// block. A theme swap must go through a cache reset (the streaming renderer
    /// does this in `set_style`); otherwise committed lines keep their old
    /// colors while newly-committed lines use the new theme. The batch path has
    /// no such constraint because it re-highlights from scratch every call.
    fn highlight(
        &mut self,
        syn: &Syntect,
        fence_info: &str,
        start_in_tail: usize,
        text: &str,
    ) -> Option<Vec<HlLine>> {
        // Rebuild from scratch when anything that would change the output from
        // the very first line changes: the language (different syntax/colors),
        // the block position (a different block), or a non-append-only edit to
        // the body (the committed prefix no longer matches `text`).
        let needs_rebuild = fence_info != self.fence_info
            || start_in_tail != self.start_in_tail
            || !self.committed_prefix_matches(text);
        if needs_rebuild {
            // `syntax` is only needed to (re)seed the parser, so it is resolved
            // here rather than on the warm path where it would be dead work.
            let syntax = syn.find_syntax_for_fence_info(fence_info)?;
            let highlighter = Highlighter::new(&syn.theme);
            fence_info.clone_into(&mut self.fence_info);
            self.start_in_tail = start_in_tail;
            self.committed_len = 0;
            self.committed_lines.clear();
            self.parse_state = ParseState::new(syntax);
            self.highlight_state = HighlightState::new(&highlighter, ScopeStack::new());
        }

        // Nothing new since the last committed `\n`: return the cached lines
        // without constructing a `Highlighter` at all.
        if self.committed_len == text.len() {
            return Some(self.committed_lines.clone());
        }

        // Walk only the not-yet-committed remainder.
        let highlighter = Highlighter::new(&syn.theme);
        let mut tentative: Option<HlLine> = None;
        for line in LinesWithEndings::from(&text[self.committed_len..]) {
            if line.ends_with('\n') {
                // A newline-terminated line is final: highlight once and
                // permanently advance the persisted state. On a (practically
                // unreachable) parse error, invalidate the cache so the next
                // pass rebuilds from scratch instead of resuming from a
                // now-inconsistent `parse_state` — matching the stateless
                // batch fallback.
                let ops = match self.parse_state.parse_line(line, &syn.syntax_set) {
                    Ok(ops) => ops,
                    Err(_) => {
                        self.fence_info.clear();
                        return None;
                    }
                };
                let highlighted =
                    HighlightIterator::new(&mut self.highlight_state, &ops, line, &highlighter)
                        .map(|(s, t)| (s, t.to_string()))
                        .collect();
                self.committed_lines.push(highlighted);
                self.committed_len += line.len();
            } else {
                // The trailing line has no `\n` yet — it is still streaming and
                // may be extended by the next push. Highlight it on CLONES so
                // the committed state stays anchored at the last `\n`.
                let mut parse_state = self.parse_state.clone();
                let mut highlight_state = self.highlight_state.clone();
                let ops = match parse_state.parse_line(line, &syn.syntax_set) {
                    Ok(ops) => ops,
                    Err(_) => {
                        self.fence_info.clear();
                        return None;
                    }
                };
                tentative = Some(
                    HighlightIterator::new(&mut highlight_state, &ops, line, &highlighter)
                        .map(|(s, t)| (s, t.to_string()))
                        .collect(),
                );
            }
        }

        // TODO: this clone keeps the open-block RETURN at O(lines)/pass
        // = O(lines^2)/stream. It only copies precomputed style spans (the
        // expensive syntect parse/highlight CPU is already O(N) total), and the
        // surrounding tail render + url_scan are likewise O(N)/pass, so this is
        // tracked as an accepted residual — not a regression. Removing it needs
        // a borrowed return threaded through `Replace` + the render pipeline.
        let mut out = self.committed_lines.clone();
        if let Some(last) = tentative {
            out.push(last);
        }
        Some(out)
    }

    /// Whether the committed prefix is still a prefix of `text` (append-only
    /// safety check). Allocation-free: walks the stored styled segments, whose
    /// texts concatenate back to the original first `committed_len` bytes.
    fn committed_prefix_matches(&self, text: &str) -> bool {
        if self.committed_len > text.len() {
            return false;
        }
        let bytes = text.as_bytes();
        let mut pos = 0;
        for line in &self.committed_lines {
            for (_, piece) in line {
                let end = pos + piece.len();
                if bytes.get(pos..end) != Some(piece.as_bytes()) {
                    return false;
                }
                pos = end;
            }
        }
        pos == self.committed_len
    }
}

#[cfg(test)]
mod tests {
    use syntect::util::LinesWithEndings;

    use super::*;
    use crate::syntax::test_syntect;

    /// Batch reference highlight, mirroring `parse::syntax_highlight_raw`.
    fn batch(syn: &Syntect, fence: &str, text: &str) -> Vec<HlLine> {
        let mut hl = syn
            .highlight_lines_for_fence_info(fence)
            .expect("syntax for fence");
        LinesWithEndings::from(text)
            .map(|line| {
                hl.highlight_line(line, &syn.syntax_set)
                    .expect("highlight line")
                    .into_iter()
                    .map(|(s, t)| (s, t.to_string()))
                    .collect()
            })
            .collect()
    }

    #[test]
    fn append_only_growth_matches_fresh_full_highlight() {
        let syn = test_syntect();
        let full = "foo: 1\nbar:\n  - a\n  - b\nbaz: true\n";
        let mut cache = OpenCodeHighlighter::new(syn);
        // Grow one byte at a time; every prefix must equal a one-shot batch
        // highlight of that same prefix (incremental == batch, byte-for-byte).
        for end in 1..=full.len() {
            if !full.is_char_boundary(end) {
                continue;
            }
            let got = cache.highlight(syn, "yaml", 0, &full[..end]).expect("hl");
            assert_eq!(got, batch(syn, "yaml", &full[..end]), "prefix len {end}");
        }
    }

    #[test]
    fn fence_change_invalidates() {
        let syn = test_syntect();
        let text = "let x = 1;\nfn main() {}\n";
        let mut cache = OpenCodeHighlighter::new(syn);
        // Prime with yaml, then re-key to rust: output must match a fresh rust
        // batch, proving the persisted yaml state was discarded.
        let _ = cache.highlight(syn, "yaml", 0, text).expect("hl yaml");
        let got = cache.highlight(syn, "rust", 0, text).expect("hl rust");
        assert_eq!(got, batch(syn, "rust", text));
    }

    #[test]
    fn start_offset_change_invalidates() {
        let syn = test_syntect();
        let mut cache = OpenCodeHighlighter::new(syn);
        let a = "alpha: 1\nbeta: 2\n";
        let _ = cache.highlight(syn, "yaml", 0, a).expect("hl a");
        // Same language, different block position and body: the new body must
        // be highlighted fresh (no stale committed lines from the old block).
        let b = "gamma: 3\ndelta: 4\n";
        let got = cache.highlight(syn, "yaml", 42, b).expect("hl b");
        assert_eq!(got, batch(syn, "yaml", b));
    }

    #[test]
    fn unknown_fence_returns_none() {
        let syn = test_syntect();
        let mut cache = OpenCodeHighlighter::new(syn);
        assert!(
            cache
                .highlight(syn, "definitely-not-a-language-xyz", 0, "data\n")
                .is_none(),
        );
    }

    // ── highlight_closed (closed-fence memo) ─────────────────────────

    #[test]
    fn closed_memo_matches_batch_and_is_idempotent() {
        let syn = test_syntect();
        let mut cache = OpenCodeHighlighter::new(syn);
        let body = "fn answer(x: u64) -> u64 {\n    x.wrapping_mul(42)\n}\n";

        // First call computes; must equal the batch reference exactly.
        let first = cache.highlight_closed(syn, "rust", body).expect("hl");
        assert_eq!(first, batch(syn, "rust", body));
        assert_eq!(cache.closed_memo_bytes(), body.len());

        // Second call is a memo hit: identical output, no new entry.
        let second = cache.highlight_closed(syn, "rust", body).expect("hl");
        assert_eq!(second, first);
        assert_eq!(cache.closed_memo_bytes(), body.len());
    }

    #[test]
    fn closed_memo_distinguishes_fence_info_and_body() {
        let syn = test_syntect();
        let mut cache = OpenCodeHighlighter::new(syn);
        let body_a = "key: value\n";
        let body_b = "other: thing\n";

        let yaml_a = cache.highlight_closed(syn, "yaml", body_a).expect("hl");
        let yaml_b = cache.highlight_closed(syn, "yaml", body_b).expect("hl");
        let rust_a = cache.highlight_closed(syn, "rust", body_a).expect("hl");

        assert_eq!(yaml_a, batch(syn, "yaml", body_a));
        assert_eq!(yaml_b, batch(syn, "yaml", body_b));
        assert_eq!(rust_a, batch(syn, "rust", body_a));
    }

    #[test]
    fn closed_memo_does_not_disturb_open_block_state() {
        let syn = test_syntect();
        let mut cache = OpenCodeHighlighter::new(syn);
        // Interleave closed-memo calls with open-block incremental growth
        // (a tail with one closed fence above an open one); open-block
        // output must stay batch-identical throughout.
        let closed = "name: pinned\n";
        let full = "a = 1\nb = 2\nc = 3\n";
        for end in 1..=full.len() {
            if !full.is_char_boundary(end) {
                continue;
            }
            let _ = cache.highlight_closed(syn, "yaml", closed).expect("memo");
            let got = cache.highlight(syn, "python", 7, &full[..end]).expect("hl");
            assert_eq!(got, batch(syn, "python", &full[..end]), "prefix len {end}");
        }
        assert_eq!(cache.closed_memo_bytes(), closed.len());
    }

    #[test]
    fn closed_memo_cap_overflow_keeps_output_correct() {
        let syn = test_syntect();
        let mut cache = OpenCodeHighlighter::new(syn);
        // Bodies sized so a handful of distinct ones cross the byte budget
        // and trigger the wholesale clear; output must stay batch-identical
        // before, at, and after the eviction.
        let filler = "x".repeat(CLOSED_MEMO_CAP_BYTES / 4);
        for i in 0..6 {
            let body = format!("key_{i}: \"{filler}\"\n");
            let got = cache.highlight_closed(syn, "yaml", &body).expect("hl");
            assert_eq!(got, batch(syn, "yaml", &body), "iteration {i}");
        }
        assert!(cache.closed_memo_bytes() <= CLOSED_MEMO_CAP_BYTES);
    }
}
