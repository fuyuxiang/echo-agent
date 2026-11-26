//! Indentation-mode reader — exact port of codex `indentation::*`.
//!
//! Loads all file lines, computes effective indents (blank lines inherit
//! from previous non-blank), then expands bidirectionally from an anchor
//! line using the codex interleaved single-loop algorithm with inline
//! sibling filtering and inline header-comment handling.

use std::collections::VecDeque;

use super::text_utils::format_display;

/// Tab width used for indent measurement (spaces per tab).
const TAB_WIDTH: usize = 4;

/// Comment prefixes recognized for `include_header`.
const COMMENT_PREFIXES: &[&str] = &["#", "//", "--"];

/// Configuration for indentation-mode reading.
///
/// Mirrors codex `IndentationModeOptions`.
#[derive(Debug, Clone)]
pub(crate) struct IndentationOptions {
    pub anchor_line: Option<usize>,
    pub max_levels: usize,
    pub include_siblings: bool,
    pub include_header: bool,
    pub max_lines: Option<usize>,
}

// ─── LineRecord ──────────────────────────────────────────────────────

/// Per-line record: number, raw (untruncated), display (truncated), indent.
///
/// Matches codex `LineRecord { number, raw, display, indent }`.
/// `raw` is used for `trimmed()` / `is_blank()` / `is_comment()`;
/// `display` is used for output formatting.
#[derive(Debug)]
struct LineRecord {
    /// 1-indexed line number.
    number: usize,
    /// Raw untruncated line content (UTF-8 lossy).
    raw: String,
    /// Display string (UTF-8 lossy, truncated at MAX_LINE_LENGTH).
    display: String,
    /// Raw indent level (number of leading spaces, tabs counted as TAB_WIDTH).
    indent: usize,
}

impl LineRecord {
    /// Leading-whitespace-stripped raw content. Codex uses `raw.trim_start()`.
    fn trimmed(&self) -> &str {
        self.raw.trim_start()
    }

    /// Whether the line is blank (only whitespace).
    fn is_blank(&self) -> bool {
        self.trimmed().is_empty()
    }

    /// Whether the line is a comment (starts with a known prefix).
    /// Codex uses `self.raw.trim().starts_with(prefix)`.
    fn is_comment(&self) -> bool {
        let t = self.raw.trim();
        COMMENT_PREFIXES.iter().any(|p| t.starts_with(p))
    }
}

// ─── Core functions ──────────────────────────────────────────────────

/// Collect line records from raw file bytes.
fn collect_lines(bytes: &[u8]) -> Vec<LineRecord> {
    if bytes.is_empty() {
        return vec![];
    }

    let mut records = Vec::new();
    let mut line_num = 0usize;
    let mut start = 0;

    for i in 0..bytes.len() {
        if bytes[i] == b'\n' {
            line_num += 1;
            let mut end = i;
            if end > start && bytes[end - 1] == b'\r' {
                end -= 1;
            }
            let raw_bytes = &bytes[start..end];
            let raw = String::from_utf8_lossy(raw_bytes).into_owned();
            let display = format_display(raw_bytes);
            let indent = measure_indent(&raw);
            records.push(LineRecord {
                number: line_num,
                raw,
                display,
                indent,
            });
            start = i + 1;
        }
    }

    // Handle remaining content after last \n (or file without trailing \n).
    if start < bytes.len() {
        line_num += 1;
        let mut end = bytes.len();
        if end > start && bytes[end - 1] == b'\r' {
            end -= 1;
        }
        let raw_bytes = &bytes[start..end];
        let raw = String::from_utf8_lossy(raw_bytes).into_owned();
        let display = format_display(raw_bytes);
        let indent = measure_indent(&raw);
        records.push(LineRecord {
            number: line_num,
            raw,
            display,
            indent,
        });
    }

    records
}

/// Measure indent: count leading spaces (tabs = TAB_WIDTH spaces).
fn measure_indent(line: &str) -> usize {
    let mut indent = 0;
    for ch in line.chars() {
        match ch {
            ' ' => indent += 1,
            '\t' => indent += TAB_WIDTH,
            _ => break,
        }
    }
    indent
}

/// Compute effective indents: blank lines inherit the indent of the
/// previous non-blank line. Returns a vec parallel to `records`.
fn compute_effective_indents(records: &[LineRecord]) -> Vec<usize> {
    let mut effective = Vec::with_capacity(records.len());
    let mut last_non_blank_indent = 0usize;

    for rec in records {
        if rec.is_blank() {
            effective.push(last_non_blank_indent);
        } else {
            last_non_blank_indent = rec.indent;
            effective.push(rec.indent);
        }
    }

    effective
}

/// Read a block of lines using indentation-based expansion from an anchor.
///
/// This is the main entry point for indentation mode.
///
/// Ported from codex `indentation::read_block` — uses the codex interleaved
/// single-loop algorithm with two cursors (i going up, j going down) that
/// alternate. Sibling filtering and header-comment inclusion are handled
/// **inline** during expansion, not as post-processing passes.
pub(crate) fn read_block(
    bytes: &[u8],
    offset: usize,
    limit: usize,
    options: IndentationOptions,
) -> Result<Vec<String>, String> {
    let collected = collect_lines(bytes);

    if collected.is_empty() {
        return Ok(vec![]);
    }

    let anchor = options.anchor_line.unwrap_or(offset);

    if anchor == 0 || anchor > collected.len() {
        return Err("anchor_line exceeds file length".to_string());
    }

    let effective = compute_effective_indents(&collected);

    // guard_limit = max_lines.unwrap_or(limit). Codex validates this > 0.
    let guard_limit = options.max_lines.unwrap_or(limit);
    if guard_limit == 0 {
        return Err("max_lines must be greater than zero".to_string());
    }

    // final_limit = min(limit, guard_limit, collected.len())
    let final_limit = limit.min(guard_limit).min(collected.len());

    let anchor_idx = anchor - 1; // 0-indexed
    let anchor_indent = effective[anchor_idx];

    // Compute min_indent threshold.
    let min_indent = if options.max_levels == 0 {
        0
    } else {
        anchor_indent.saturating_sub(options.max_levels * TAB_WIDTH)
    };

    // Early return: final_limit == 1 → just the anchor line.
    if final_limit == 1 {
        let rec = &collected[anchor_idx];
        return Ok(vec![format!("L{}: {}", rec.number, rec.display)]);
    }

    // ── Interleaved bidirectional expansion ──────────────────────
    //
    // Codex algorithm (lines 293–357): single `while out.len() < final_limit`
    // loop. BOTH cursors are tried every iteration (up first, then down).
    // A `progressed` counter tracks whether either direction added a line;
    // if 0, both are exhausted and we break.
    //
    // `i` starts at anchor_idx - 1 going down to 0 (or -1 = exhausted).
    // `j` starts at anchor_idx + 1 going up to collected.len() (= exhausted).

    let mut out: VecDeque<usize> = VecDeque::new();
    out.push_back(anchor_idx);

    // Use isize for i so we can represent -1 as "exhausted"
    let mut i: isize = anchor_idx as isize - 1;
    let mut j: usize = anchor_idx + 1;
    let n = collected.len();

    // Counters: track boundary-level lines accepted in each direction.
    let mut i_counter_min_indent: usize = 0;
    let mut j_counter_min_indent: usize = 0;

    while out.len() < final_limit {
        let mut progressed = 0usize;

        // ── ALWAYS try upward cursor (if available) ─────────────
        if i >= 0 {
            let added = expand_up(
                &collected,
                &effective,
                &mut out,
                &mut i,
                min_indent,
                options.include_siblings,
                options.include_header,
                &mut i_counter_min_indent,
            );
            if added {
                progressed += 1;
            }
            // Short-cut: codex breaks after up if limit reached.
            if out.len() >= final_limit {
                break;
            }
        }

        // ── ALWAYS try downward cursor (if available) ───────────
        if j < n {
            let added = expand_down(
                &effective,
                &mut out,
                &mut j,
                n,
                min_indent,
                options.include_siblings,
                &mut j_counter_min_indent,
            );
            if added {
                progressed += 1;
            }
        }

        if progressed == 0 {
            break;
        }
    }

    // Trim leading/trailing blank lines.
    trim_empty_lines(&collected, &mut out);

    // Format output.
    let lines: Vec<String> = out
        .iter()
        .map(|&idx| {
            let rec = &collected[idx];
            format!("L{}: {}", rec.number, rec.display)
        })
        .collect();

    Ok(lines)
}

/// Expand the upward cursor by one step. Returns true if a line was
/// added to `out` (net gain — not reverted).
///
/// Codex logic (lines 296–320):
/// 1. If `eff >= min_indent`: push_front (line 300).
/// 2. If `eff == min_indent && !include_siblings`:
///    - `can_take_line = allow_header_comment || counter == 0`
///    - If can_take_line: increment counter (line is kept).
///    - If !can_take_line: pop_front (revert THIS just-pushed line), stop cursor.
/// 3. If `eff < min_indent`: stop cursor, return false.
#[allow(clippy::too_many_arguments)]
fn expand_up(
    collected: &[LineRecord],
    effective: &[usize],
    out: &mut VecDeque<usize>,
    i: &mut isize,
    min_indent: usize,
    include_siblings: bool,
    include_header: bool,
    counter: &mut usize,
) -> bool {
    if *i < 0 {
        return false;
    }

    let iu = *i as usize;
    let eff = effective[iu];

    if eff < min_indent {
        // Below threshold — stop cursor.
        *i = -1;
        return false;
    }

    // eff >= min_indent — push first (codex line 300), then filter.
    out.push_front(iu);
    *i -= 1;

    // Sibling filter: only applies when eff == min_indent && !include_siblings.
    if eff == min_indent && !include_siblings {
        let allow_header_comment = include_header && collected[iu].is_comment();
        let can_take_line = allow_header_comment || *counter == 0;
        if can_take_line {
            *counter += 1; // line is kept, increment counter
        } else {
            // Revert THIS just-pushed line and stop cursor.
            out.pop_front();
            *i = -1;
            return false; // net: no line added
        }
    }

    true
}

/// Expand the downward cursor by one step. Returns true if a line was
/// added to `out` (net gain — not reverted).
///
/// Codex logic (lines 332–348):
/// 1. If `eff >= min_indent`: push_back (line 334).
/// 2. If `eff == min_indent && !include_siblings`:
///    - If `counter > 0`: pop_back (revert THIS just-pushed line), stop cursor.
///    - Always increment counter (line 346).
/// 3. If `eff < min_indent`: stop cursor, return false.
fn expand_down(
    effective: &[usize],
    out: &mut VecDeque<usize>,
    j: &mut usize,
    n: usize,
    min_indent: usize,
    include_siblings: bool,
    counter: &mut usize,
) -> bool {
    if *j >= n {
        return false;
    }

    let ju = *j;
    let eff = effective[ju];

    if eff < min_indent {
        // Below threshold — stop cursor.
        *j = n;
        return false;
    }

    // eff >= min_indent — push first (codex line 334), then filter.
    out.push_back(ju);
    *j += 1;

    // Sibling filter: only applies when eff == min_indent && !include_siblings.
    if eff == min_indent && !include_siblings {
        if *counter > 0 {
            // Second+ boundary-level line — revert THIS just-pushed line.
            out.pop_back();
            *j = n; // stop cursor
            // Still increment counter (codex line 346: always increments).
            *counter += 1;
            return false; // net: no line added
        }
        *counter += 1; // always increment (codex line 346)
    }

    true
}

/// Trim leading and trailing blank lines from the result deque.
fn trim_empty_lines(records: &[LineRecord], deque: &mut VecDeque<usize>) {
    while let Some(&idx) = deque.front() {
        if records[idx].is_blank() {
            deque.pop_front();
        } else {
            break;
        }
    }
    while let Some(&idx) = deque.back() {
        if records[idx].is_blank() {
            deque.pop_back();
        } else {
            break;
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_opts(
        anchor_line: Option<usize>,
        max_levels: usize,
        include_siblings: bool,
        include_header: bool,
        max_lines: Option<usize>,
    ) -> IndentationOptions {
        IndentationOptions {
            anchor_line,
            max_levels,
            include_siblings,
            include_header,
            max_lines,
        }
    }

    // ── Exact-output tests ───────────────────────────────────────

    #[test]
    fn captures_function_block_with_limit() {
        // anchor=2 (x=1, indent 4), max_levels=1, min_indent = 4-4 = 0.
        // With min_indent=0, the entire file is reachable (no indent is below 0).
        // Sibling filter: going up, def foo is first boundary (counter=1, accepted).
        // Going down: y, return, blank (effective=4 > 0), def bar (effective=0 == min,
        // counter=1, accepted), pass (effective=4 > 0, accepted). No second boundary hit,
        // so downward includes everything.
        let content =
            b"def foo():\n    x = 1\n    y = 2\n    return x + y\n\ndef bar():\n    pass\n";

        // Without limit: entire file (minus blank trim)
        let opts_full = make_opts(Some(2), 1, false, true, None);
        let result_full = read_block(content, 1, 2000, opts_full).unwrap();
        assert!(result_full.iter().any(|l| l.contains("def foo():")));
        assert!(result_full.iter().any(|l| l.contains("x = 1")));
        assert!(result_full.iter().any(|l| l.contains("return x + y")));

        // With max_lines=4: capped to 4 lines
        let opts_limited = make_opts(Some(2), 1, false, true, Some(4));
        let result = read_block(content, 1, 4, opts_limited).unwrap();
        assert_eq!(
            result,
            vec![
                "L1: def foo():",
                "L2:     x = 1",
                "L3:     y = 2",
                "L4:     return x + y",
            ]
        );
    }

    #[test]
    fn expands_to_parent_class() {
        // L1: class MyClass:   (indent 0)
        // L2:     def method(self):  (indent 4)
        // L3:         x = 1    (indent 8)  ← ANCHOR
        // L4:         y = 2    (indent 8)
        // L5:         return x + y  (indent 8)
        // L6: (blank, effective=8)
        // L7:     def other(self):  (indent 4)
        // L8:         pass     (indent 8)
        // anchor=3, max_levels=2, min_indent = 8-8 = 0.
        // Both directions try every iteration. Up first: class MyClass (indent 0,
        // boundary counter=1, kept). Down: y (eff=8>0, kept). Up: exhausted (i=-1).
        // Down: return, blank, def other (boundary counter=1, kept since counter was 0),
        // pass. All accepted because min_indent=0.
        let content = b"class MyClass:\n    def method(self):\n        x = 1\n        y = 2\n        return x + y\n\n    def other(self):\n        pass\n";
        let opts = make_opts(Some(3), 2, false, true, None);
        let result = read_block(content, 1, 2000, opts).unwrap();

        assert_eq!(result[0], "L1: class MyClass:");
        assert_eq!(result[1], "L2:     def method(self):");
        assert_eq!(result[2], "L3:         x = 1");
        assert_eq!(result[3], "L4:         y = 2");
        assert_eq!(result[4], "L5:         return x + y");
    }

    #[test]
    fn sibling_filter_at_nonzero_min_indent() {
        // Layout:
        //   L1:  class C:               (indent 0)
        //   L2:      def a(self):       (indent 4, boundary)
        //   L3:          pass            (indent 8)
        //   L4:      def b(self):       (indent 4, boundary)
        //   L5:          pass            (indent 8)
        //   L6:      def anchor(self):  (indent 4, boundary)
        //   L7:          x = 1          (indent 8) ← ANCHOR
        //   L8:      def d(self):       (indent 4, boundary)
        //   L9:          pass            (indent 8)
        //   L10:     def e(self):       (indent 4, boundary)
        //   L11:         pass            (indent 8)
        //
        // anchor=7, max_levels=1, min_indent = 8-4 = 4.
        let content = b"\
class C:
    def a(self):
        pass
    def b(self):
        pass
    def anchor(self):
        x = 1
    def d(self):
        pass
    def e(self):
        pass
";
        // Without siblings: up hits def anchor (boundary, counter 0→1, kept),
        // then def b (boundary, counter==1, can_take_line=false → REVERT anchor, stop).
        // Wait: up goes from anchor_idx=6 upward. i starts at 5 (def anchor line).
        // L6 (idx 5) = "    def anchor(self):" → eff=4 == min=4. counter==0, can_take=true.
        // Push. counter=1. i=4.
        // L5 (idx 4) = "        pass" → eff=8 > 4. Push. i=3.
        // L4 (idx 3) = "    def b(self):" → eff=4 == min=4. counter==1, can_take=false.
        // REVERT (pop front = L4 just pushed). Stop. i=-1.
        // Wait that's not right. Let me retrace...
        // Actually: push L4 first, THEN check. can_take_line = false (counter==1, not comment).
        // Revert = pop front = L4 (the just-pushed one). i=-1.
        //
        // Down: j starts at 7 (def d).
        // L8 (idx 7) = "    def d(self):" → eff=4 == min=4. counter==0 → kept. counter=1.
        // L9 (idx 8) = "        pass" → eff=8>4 → kept.
        // L10 (idx 9) = "    def e(self):" → eff=4 == min=4. counter>0 → REVERT L10, stop.
        //
        // Result (before trim): [L5:pass, L6:def anchor, L7:x=1, L8:def d, L9:pass]
        // After blank trim (no blanks): same.
        let opts_no_sibs = make_opts(Some(7), 1, false, true, None);
        let result_no_sibs = read_block(content, 1, 2000, opts_no_sibs).unwrap();

        assert_eq!(
            result_no_sibs,
            vec![
                "L5:         pass",
                "L6:     def anchor(self):",
                "L7:         x = 1",
                "L8:     def d(self):",
                "L9:         pass",
            ]
        );

        // With siblings: all methods at indent 4 should be included
        let opts_sibs = make_opts(Some(7), 1, true, true, None);
        let result_sibs = read_block(content, 1, 2000, opts_sibs).unwrap();

        assert!(result_sibs.iter().any(|l| l.contains("def a(")));
        assert!(result_sibs.iter().any(|l| l.contains("def anchor")));
        assert!(result_sibs.iter().any(|l| l.contains("def e(")));
        assert!(result_sibs.len() > result_no_sibs.len());
    }

    #[test]
    fn include_header_adds_comments() {
        // L1: # Helper function   (indent 0, comment)
        // L2: # for computation   (indent 0, comment)
        // L3: def compute(x):     (indent 0)
        // L4:     return x * 2    (indent 4) ← ANCHOR
        //
        // anchor=4, max_levels=1, min_indent = 4-4 = 0.
        // With include_header=true: comments at indent 0 pass via allow_header_comment.
        let content = b"# Helper function\n# for computation\ndef compute(x):\n    return x * 2\n";

        let opts_header = make_opts(Some(4), 1, false, true, None);
        let result = read_block(content, 1, 2000, opts_header).unwrap();
        assert_eq!(
            result,
            vec![
                "L1: # Helper function",
                "L2: # for computation",
                "L3: def compute(x):",
                "L4:     return x * 2",
            ]
        );

        // Without header: comments at boundary are rejected by sibling filter
        let opts_no_header = make_opts(Some(4), 1, false, false, None);
        let result_no = read_block(content, 1, 2000, opts_no_header).unwrap();
        assert_eq!(
            result_no,
            vec!["L3: def compute(x):", "L4:     return x * 2",]
        );
    }

    #[test]
    fn limit_caps_output_size() {
        // anchor=3 (b=2), max_levels=0, limit=3.
        // Codex: both up+down each iteration. final_limit = min(3, 3, 6) = 3.
        // Iter 1: up: push a=1 → [a,b,c...wait]
        // out starts as [b]. Iter 1: up push foo → [foo, b]. down push c → [foo, b, c].
        // out.len()=3 → done.
        let content = b"def foo():\n    a = 1\n    b = 2\n    c = 3\n    d = 4\n    e = 5\n";
        let opts = make_opts(Some(3), 0, false, true, Some(3));
        let result = read_block(content, 1, 3, opts).unwrap();
        assert_eq!(result.len(), 3);
        assert_eq!(
            result,
            vec!["L2:     a = 1", "L3:     b = 2", "L4:     c = 3",]
        );
    }

    #[test]
    fn final_limit_1_returns_anchor_only() {
        let content = b"line1\nline2\nline3\n";
        let opts = make_opts(Some(2), 0, false, true, Some(1));
        let result = read_block(content, 1, 2000, opts).unwrap();
        assert_eq!(result, vec!["L2: line2"]);
    }

    // ── Edge cases ───────────────────────────────────────────────

    #[test]
    fn anchor_exceeds_file_length_error() {
        let content = b"one\ntwo\n";
        let opts = make_opts(Some(100), 0, false, true, None);
        let result = read_block(content, 1, 2000, opts);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "anchor_line exceeds file length");
    }

    #[test]
    fn empty_file_returns_empty() {
        let content = b"";
        let opts = make_opts(None, 0, false, true, None);
        let result = read_block(content, 1, 2000, opts).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn guard_limit_zero_returns_error() {
        let content = b"line1\nline2\n";
        let opts = make_opts(Some(1), 0, false, true, Some(0));
        let result = read_block(content, 1, 2000, opts);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "max_lines must be greater than zero");
    }

    #[test]
    fn trims_leading_trailing_blank_lines() {
        // Blank lines at the edges of the expansion should be trimmed.
        let content = b"\ndef foo():\n    x = 1\n\n";
        let opts = make_opts(Some(3), 1, false, true, None);
        let result = read_block(content, 1, 2000, opts).unwrap();
        // First and last lines of result should not be blank
        assert!(!result.first().unwrap().ends_with(": "));
        assert!(!result.last().unwrap().ends_with(": "));
    }

    #[test]
    fn trimmed_uses_trim_start() {
        // Verify that trimmed() strips only leading whitespace.
        // A line like "  hello  " should have trimmed() = "hello  "
        let rec = LineRecord {
            number: 1,
            raw: "  hello  ".to_string(),
            display: "  hello  ".to_string(),
            indent: 2,
        };
        assert_eq!(rec.trimmed(), "hello  ");
        assert!(!rec.is_blank());
    }

    #[test]
    fn is_comment_uses_raw_trim() {
        // Verify is_comment uses raw.trim() (both sides), not trim_start().
        let rec = LineRecord {
            number: 1,
            raw: "  // comment  ".to_string(),
            display: "  // comment  ".to_string(),
            indent: 2,
        };
        assert!(rec.is_comment());
    }

    #[test]
    fn cpp_switch_shallow_expansion() {
        let content = b"#include <iostream>\n\nint main() {\n    switch (x) {\n        case 1:\n            std::cout << \"one\";\n            break;\n        case 2:\n            std::cout << \"two\";\n            break;\n    }\n    return 0;\n}\n";
        // anchor=6 (std::cout << "one", indent 12), max_levels=1, min_indent=12-4=8.
        let opts = make_opts(Some(6), 1, false, true, None);
        let result = read_block(content, 1, 2000, opts).unwrap();
        // Should include case 1: and its body
        assert!(result.iter().any(|l| l.contains("case 1:")));
        assert!(result.iter().any(|l| l.contains("\"one\"")));
    }

    #[test]
    fn cpp_switch_deeper_expansion() {
        let content = b"// Main entry point\n#include <iostream>\n\nint main() {\n    switch (x) {\n        case 1:\n            std::cout << \"one\";\n            break;\n        case 2:\n            std::cout << \"two\";\n            break;\n    }\n    return 0;\n}\n";
        // anchor=7 (std::cout << "one", indent 12), max_levels=2, min_indent=12-8=4.
        let opts = make_opts(Some(7), 2, false, true, None);
        let result = read_block(content, 1, 2000, opts).unwrap();
        assert!(result.iter().any(|l| l.contains("switch (x)")));
    }
}
