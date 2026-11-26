//! Headless markdown analysis sharing EchoAgent Build's exact `pulldown-cmark` config.
//!
//! This crate is intentionally lean -- it depends only on `pulldown-cmark` -- so it
//! can be used without pulling in the terminal-rendering stack (syntect, ratatui,
//! two-face). [`parser_options`] is the single source of truth for the parser
//! feature set, shared with `echo-agent-markdown` so analysis matches what EchoAgent
//! Build actually renders 1:1.
//!
//! After parsing, EchoAgent applies [`offset_events`]: only `~~…~~` is strikethrough.
//! Single-tilde pairs (`~text~`) are demoted to literal `~` text so LLM output
//! like `~**10%**` is not struck (pulldown treats those pairs as strike; we do not).

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use std::ops::Range;

/// The exact `pulldown-cmark` option set EchoAgent Build uses to render markdown.
///
/// With `ENABLE_STRIKETHROUGH`, pulldown treats both `~~…~~` and single-`~` pairs as
/// strike. Callers must consume events via [`offset_events`] so only double-tilde
/// strikethrough is retained (LLM-friendly post-policy).
pub fn parser_options() -> Options {
    Options::ENABLE_GFM
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_MATH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_TABLES
}

/// Offset event stream from EchoAgent's parser, with single-tilde strikethrough demoted.
///
/// Prefer this over `Parser::new_ext(...).into_offset_iter()` so analysis and
/// rendering agree on what counts as strikethrough.
pub fn offset_events(text: &str) -> impl Iterator<Item = (Event<'_>, Range<usize>)> + '_ {
    DoubleTildeOnlyStrike {
        text,
        events: Parser::new_ext(text, parser_options()).into_offset_iter(),
    }
}

/// Stackless filter: Start and End share the same byte span in pulldown, so both
/// are classified by whether that span opens with `~~`. Single-tilde frames emit
/// delimiter `Text` instead of strike tags (delimiters are not separate events).
struct DoubleTildeOnlyStrike<'a, I> {
    text: &'a str,
    events: I,
}

/// True when the strike span at `range.start` is the double-tilde form.
fn is_double_tilde_strike(text: &str, range: &Range<usize>) -> bool {
    text.get(range.start..).is_some_and(|s| s.starts_with("~~"))
}

/// Opening or closing delimiter byte as `Text`, with the matching source range.
fn strike_delim_text<'a>(
    text: &'a str,
    range: &Range<usize>,
    opening: bool,
) -> (Event<'a>, Range<usize>) {
    let delim = if opening {
        let end = range.start + 1;
        debug_assert!(text.is_char_boundary(end) && end <= text.len());
        (range.start..end, &text[range.start..end])
    } else {
        let start = range.end - 1;
        debug_assert!(text.is_char_boundary(start) && start < text.len());
        (start..range.end, &text[start..range.end])
    };
    (Event::Text(delim.1.into()), delim.0)
}

impl<'a, I> Iterator for DoubleTildeOnlyStrike<'a, I>
where
    I: Iterator<Item = (Event<'a>, Range<usize>)>,
{
    type Item = (Event<'a>, Range<usize>);

    fn next(&mut self) -> Option<Self::Item> {
        let (event, range) = self.events.next()?;
        match &event {
            Event::Start(Tag::Strikethrough) if !is_double_tilde_strike(self.text, &range) => {
                Some(strike_delim_text(self.text, &range, true))
            }
            Event::End(TagEnd::Strikethrough) if !is_double_tilde_strike(self.text, &range) => {
                Some(strike_delim_text(self.text, &range, false))
            }
            _ => Some((event, range)),
        }
    }
}

/// Counts of markdown elements found in a document.
///
/// Counting mirrors the `pulldown-cmark` event stream the renderer walks, so a few
/// overlaps are intentional and documented per-field below.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct MarkdownStats {
    pub h1: u32,
    pub h2: u32,
    pub h3: u32,
    pub h4: u32,
    pub h5: u32,
    pub h6: u32,
    pub tables: u32,
    pub fenced_code: u32,
    pub indented_code: u32,
    pub inline_code: u32,
    pub strong: u32,
    pub emphasis: u32,
    pub strikethrough: u32,
    /// All GFM link types: inline, reference, collapsed, shortcut-when-defined,
    /// angle-bracket autolink, and email autolink.
    pub links: u32,
    /// Markup inside an image's alt text is still counted (e.g. `![**x**](u)` bumps `strong`).
    pub images: u32,
    pub blockquotes: u32,
    pub thematic_breaks: u32,
    pub inline_math: u32,
    pub display_math: u32,
    /// Subset of `list_items`: a task-list item is also a list item.
    pub task_list_items: u32,
    /// Container lists are not counted (no `lists` field); nested list items are included.
    pub list_items: u32,
}

impl MarkdownStats {
    /// Total heading count across all levels, derived from `h1..=h6`.
    pub fn headings(&self) -> u32 {
        self.h1 + self.h2 + self.h3 + self.h4 + self.h5 + self.h6
    }

    /// Single source of truth for name->value mapping; the exhaustive destructure makes adding a field a compile error here, so downstream consumers cannot drift from the struct.
    pub fn as_pairs(&self) -> [(&'static str, u32); 22] {
        // Exhaustive (no `..`): adding a field to MarkdownStats fails to compile until it is mapped below.
        let Self {
            h1,
            h2,
            h3,
            h4,
            h5,
            h6,
            tables,
            fenced_code,
            indented_code,
            inline_code,
            strong,
            emphasis,
            strikethrough,
            links,
            images,
            blockquotes,
            thematic_breaks,
            inline_math,
            display_math,
            task_list_items,
            list_items,
        } = *self;
        [
            ("headings", self.headings()),
            ("h1", h1),
            ("h2", h2),
            ("h3", h3),
            ("h4", h4),
            ("h5", h5),
            ("h6", h6),
            ("tables", tables),
            ("fenced_code", fenced_code),
            ("indented_code", indented_code),
            ("inline_code", inline_code),
            ("strong", strong),
            ("emphasis", emphasis),
            ("strikethrough", strikethrough),
            ("links", links),
            ("images", images),
            ("blockquotes", blockquotes),
            ("thematic_breaks", thematic_breaks),
            ("inline_math", inline_math),
            ("display_math", display_math),
            ("task_list_items", task_list_items),
            ("list_items", list_items),
        ]
    }
}

/// A render-fidelity failure: the model emitted markdown that does not render as
/// the structure it clearly intended.
///
/// Distinct from [`MarkdownStats`] counts: a count answers "how many tables", an
/// issue answers "did a construct silently degrade". `pulldown-cmark` never errors
/// (CommonMark is total), so each issue is detected by comparing intent (the raw
/// syntax) against what actually parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuralIssue {
    /// A GFM table delimiter row (`|---|---|`) sits under a header line, but the
    /// table did not parse (e.g. the delimiter's column count != the header's), so
    /// the lines render as a paragraph -- the "made a table but it didn't show" bug.
    MalformedTable,
    /// A fenced code block runs to EOF without a closing fence, swallowing the rest of the message.
    UnterminatedCodeBlock,
}

impl StructuralIssue {
    /// Stable snake_case name for this issue (for logs, metrics, or FFI bindings).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MalformedTable => "malformed_table",
            Self::UnterminatedCodeBlock => "unterminated_code_block",
        }
    }
}

/// Element counts plus any structural issues from a single parse pass.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MarkdownAnalysis {
    pub stats: MarkdownStats,
    pub issues: Vec<StructuralIssue>,
}

/// Strip the container markers pulldown keeps on each source line (`>` for blockquotes, indent for lists).
fn strip_block_prefix(line: &str) -> &str {
    line.trim_start_matches(['>', ' ', '\t'])
}

/// Unterminated iff no line after the opener is a closing fence matching the opener's char and length.
///
/// Works off raw block source (the only thing carrying closure info), so a verbatim fence line in
/// content could mask a real EOF -- a rare, safe-direction (under-penalizing) miss we accept for simplicity.
fn fenced_block_is_unterminated(block_src: &str) -> bool {
    let mut lines = block_src.lines();
    let Some(open) = lines.next().map(strip_block_prefix) else {
        return false;
    };
    let Some(fence_char) = open.chars().next().filter(|c| matches!(c, '`' | '~')) else {
        return false;
    };
    let open_len = open.chars().take_while(|c| *c == fence_char).count();
    !lines.any(|line| {
        let close = strip_block_prefix(line).trim_end();
        // `open_len >= 3`, so `close.len() >= open_len` already implies non-empty.
        close.len() >= open_len && close.chars().all(|c| c == fence_char)
    })
}

/// A GFM table delimiter row: only `|`, `-`, `:`, and whitespace, with at least one
/// pipe and one dash. The pipe requirement rejects a bare `---` thematic break or a
/// setext `-----` underline; the dash requirement rejects a `|||`-only row.
fn is_table_delimiter_line(line: &str) -> bool {
    let line = line.trim();
    line.contains('|')
        && line.contains('-')
        && line
            .chars()
            .all(|c| matches!(c, '|' | '-' | ':' | ' ' | '\t'))
}

/// A line that could be a table header: non-empty, containing a column pipe, and
/// not itself delimiter-shaped (a `|---|` row arming the next line would chain one
/// broken table into a duplicate flag per extra delimiter row).
fn line_looks_like_header(line: &str) -> bool {
    let line = line.trim();
    !line.is_empty() && line.contains('|') && !is_table_delimiter_line(line)
}

/// Flag delimiter rows the model intended as a table but that `pulldown-cmark` did
/// not parse as one (so they render as a paragraph -- the broken-table bug).
///
/// `parsed_spans` are the byte ranges of real tables and code blocks: a delimiter
/// line starting inside one is either part of a valid table or literal code text, so
/// it never signals a malformed table. Everything else is fair game -- a delimiter
/// row directly under a pipe-bearing header line is an intended-but-unparsed table.
fn detect_malformed_tables(
    text: &str,
    parsed_spans: &[Range<usize>],
    issues: &mut Vec<StructuralIssue>,
) {
    let in_parsed_span = |offset: usize| parsed_spans.iter().any(|span| span.contains(&offset));

    let mut prev_is_header = false;
    let mut offset = 0;
    // `split_inclusive` keeps the trailing `\n`, so summing lengths tracks byte offsets exactly.
    for line in text.split_inclusive('\n') {
        let start = offset;
        offset += line.len();

        // A `|---|` line inside a parsed table/code block is not an intended-but-broken table.
        let excluded = in_parsed_span(start);
        let content = strip_block_prefix(line.trim_end_matches(['\n', '\r']));

        if !excluded && prev_is_header && is_table_delimiter_line(content) {
            issues.push(StructuralIssue::MalformedTable);
        }
        // The delimiter's header must be the immediately-preceding, non-excluded pipe line.
        prev_is_header = !excluded && line_looks_like_header(content);
    }
}

/// Parse `text` with EchoAgent Build's options; count elements and flag structural issues.
pub fn analyze(text: &str) -> MarkdownAnalysis {
    let mut stats = MarkdownStats::default();
    let mut issues = Vec::new();
    // Byte ranges of constructs where a `|---|`-shaped line is legitimately not an
    // intended table (real tables and code blocks); consumed by `detect_malformed_tables`.
    let mut parsed_spans: Vec<Range<usize>> = Vec::new();

    // u32 element counters can't overflow: model output is token-bounded, far below `u32::MAX`.
    // `offset_events` attaches byte ranges and demotes single-tilde strike so counts match render.
    for (event, range) in offset_events(text) {
        // Structural-issue bookkeeping, tracked alongside the element counting below.
        match &event {
            // A parsed table's span covers its delimiter row -- exclude it from malformed-table scanning.
            Event::Start(Tag::Table(_)) => parsed_spans.push(range.clone()),
            // The range spans the opening fence through the close (or to EOF when unterminated).
            Event::Start(Tag::CodeBlock(kind)) => {
                if matches!(kind, CodeBlockKind::Fenced(_))
                    && fenced_block_is_unterminated(&text[range.clone()])
                {
                    issues.push(StructuralIssue::UnterminatedCodeBlock);
                }
                parsed_spans.push(range.clone());
            }
            _ => {}
        }

        match event {
            Event::Start(Tag::Heading { level, .. }) => match level {
                HeadingLevel::H1 => stats.h1 += 1,
                HeadingLevel::H2 => stats.h2 += 1,
                HeadingLevel::H3 => stats.h3 += 1,
                HeadingLevel::H4 => stats.h4 += 1,
                HeadingLevel::H5 => stats.h5 += 1,
                HeadingLevel::H6 => stats.h6 += 1,
            },
            Event::Start(Tag::Table(_)) => stats.tables += 1,
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(_))) => stats.fenced_code += 1,
            Event::Start(Tag::CodeBlock(CodeBlockKind::Indented)) => stats.indented_code += 1,
            Event::Start(Tag::Strong) => stats.strong += 1,
            Event::Start(Tag::Emphasis) => stats.emphasis += 1,
            Event::Start(Tag::Strikethrough) => stats.strikethrough += 1,
            Event::Start(Tag::Link { .. }) => stats.links += 1,
            Event::Start(Tag::Image { .. }) => stats.images += 1,
            Event::Start(Tag::BlockQuote(_)) => stats.blockquotes += 1,
            Event::Start(Tag::Item) => stats.list_items += 1,
            Event::Code(_) => stats.inline_code += 1,
            Event::InlineMath(_) => stats.inline_math += 1,
            Event::DisplayMath(_) => stats.display_math += 1,
            Event::Rule => stats.thematic_breaks += 1,
            Event::TaskListMarker(_) => stats.task_list_items += 1,
            _ => {}
        }
    }

    // Second pass: with every real-table/code-block span known, flag delimiter rows
    // the model intended as tables but that did not parse as one.
    detect_malformed_tables(text, &parsed_spans, &mut issues);

    MarkdownAnalysis { stats, issues }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipeless_single_dash_divider_is_a_table() {
        assert_eq!(analyze("a | b\n- | -\nc | d\n").stats.tables, 1);
    }

    #[test]
    fn outer_pipe_single_dash_divider_is_a_table() {
        assert_eq!(analyze("| a | b |\n| - | - |\n| c | d |\n").stats.tables, 1);
    }

    #[test]
    fn two_tables_in_one_doc() {
        let doc = "| a | b |\n| - | - |\n| c | d |\n\n| e | f |\n| - | - |\n| g | h |\n";
        assert_eq!(analyze(doc).stats.tables, 2);
    }

    #[test]
    fn setext_heading_is_h1() {
        let stats = analyze("Title\n=====\n\nbody\n").stats;
        assert_eq!(stats.headings(), 1);
        assert_eq!(stats.h1, 1);
    }

    #[test]
    fn atx_heading_levels_counted() {
        let stats = analyze("## h2\n\n### h3\n\n#### h4\n\n##### h5\n\n###### h6\n").stats;
        assert_eq!(stats.h2, 1);
        assert_eq!(stats.h3, 1);
        assert_eq!(stats.h4, 1);
        assert_eq!(stats.h5, 1);
        assert_eq!(stats.h6, 1);
        assert_eq!(stats.headings(), 5);
    }

    #[test]
    fn inline_math_is_detected() {
        assert_eq!(analyze("mass is $E=mc^2$ ok\n").stats.inline_math, 1);
    }

    #[test]
    fn display_math_is_detected() {
        assert_eq!(analyze("$$\\int x dx$$\n").stats.display_math, 1);
    }

    #[test]
    fn indented_backticks_stay_inside_fenced_block() {
        assert_eq!(analyze("```\ncode\n    ```\nafter\n").stats.fenced_code, 1);
    }

    #[test]
    fn fenced_code_with_info_string() {
        assert_eq!(analyze("```rust\nx\n```\n").stats.fenced_code, 1);
    }

    #[test]
    fn four_space_indent_is_indented_code() {
        let stats = analyze("    code\n").stats;
        assert_eq!(stats.indented_code, 1);
        assert_eq!(stats.fenced_code, 0);
    }

    #[test]
    fn thematic_break_counted() {
        assert_eq!(analyze("---\n").stats.thematic_breaks, 1);
    }

    #[test]
    fn image_is_not_counted_as_link() {
        let stats = analyze("![a](x.png)\n").stats;
        assert_eq!(stats.images, 1);
        assert_eq!(stats.links, 0);
    }

    #[test]
    fn task_list_items_are_subset_of_list_items() {
        let stats = analyze("- [ ] a\n- [x] b\n- c\n").stats;
        assert_eq!(stats.list_items, 3);
        assert_eq!(stats.task_list_items, 2);
    }

    #[test]
    fn autolink_and_reference_links_counted() {
        assert_eq!(analyze("<https://x.com>\n").stats.links, 1);
        assert_eq!(analyze("[a][b]\n\n[b]: https://x.com\n").stats.links, 1);
    }

    #[test]
    fn empty_input_is_default() {
        assert_eq!(analyze("").stats, MarkdownStats::default());
    }

    #[test]
    fn as_pairs_pins_every_pair() {
        // One golden doc exercising several element types pins every label, value, AND order
        // at once -- a mislabel like ("inline_math", display_math) would fail here.
        let doc = "# Title\n## Sub\n\nSome **bold**, *italic*, ~~strike~~, `code`, and a [link](https://x.com).\n\n| a | b |\n| - | - |\n| c | d |\n\n- [ ] todo\n- [x] done\n- plain\n";
        assert_eq!(
            analyze(doc).stats.as_pairs(),
            [
                ("headings", 2),
                ("h1", 1),
                ("h2", 1),
                ("h3", 0),
                ("h4", 0),
                ("h5", 0),
                ("h6", 0),
                ("tables", 1),
                ("fenced_code", 0),
                ("indented_code", 0),
                ("inline_code", 1),
                ("strong", 1),
                ("emphasis", 1),
                ("strikethrough", 1),
                ("links", 1),
                ("images", 0),
                ("blockquotes", 0),
                ("thematic_breaks", 0),
                ("inline_math", 0),
                ("display_math", 0),
                ("task_list_items", 2),
                ("list_items", 3),
            ]
        );
    }

    #[test]
    fn mixed_document_locks_counts() {
        let doc = "# Title\n\nSome **bold** and *italic* and ~~strike~~ and `code`.\n\n- one\n- two\n\n> quote\n\n[link](https://example.com)\n";
        let stats = analyze(doc).stats;
        assert_eq!(stats.headings(), 1);
        assert_eq!(stats.h1, 1);
        assert_eq!(stats.list_items, 2);
        assert_eq!(stats.strong, 1);
        assert_eq!(stats.emphasis, 1);
        assert_eq!(stats.strikethrough, 1);
        assert_eq!(stats.inline_code, 1);
        assert_eq!(stats.links, 1);
        assert_eq!(stats.blockquotes, 1);
    }

    fn strike_start_end_counts(text: &str) -> (usize, usize) {
        let mut starts = 0;
        let mut ends = 0;
        for (e, _) in offset_events(text) {
            match e {
                Event::Start(Tag::Strikethrough) => starts += 1,
                Event::End(TagEnd::Strikethrough) => ends += 1,
                _ => {}
            }
        }
        (starts, ends)
    }

    #[test]
    fn bill_single_tilde_percent_is_not_strikethrough() {
        // Trigger case: approx percentages must not strike; nested strong still applies.
        let doc = "- `n=1` only: ~**10%** (~**300**)";
        let stats = analyze(doc).stats;
        assert_eq!(stats.strikethrough, 0);
        assert_eq!(stats.strong, 2);
        assert_eq!(strike_start_end_counts(doc), (0, 0));
    }

    #[test]
    fn double_tilde_deleted_is_strikethrough() {
        assert_eq!(analyze("~~deleted~~").stats.strikethrough, 1);
        assert_eq!(strike_start_end_counts("~~deleted~~"), (1, 1));
    }

    #[test]
    fn single_tilde_pair_is_literal_not_strike() {
        let doc = "~single~";
        assert_eq!(analyze(doc).stats.strikethrough, 0);
        assert_eq!(strike_start_end_counts(doc), (0, 0));
        let texts: Vec<_> = offset_events(doc)
            .filter_map(|(e, _)| match e {
                Event::Text(t) => Some(t.into_string()),
                _ => None,
            })
            .collect();
        assert!(texts.iter().any(|t| t == "~"));
    }

    #[test]
    fn lone_tilde_percent_is_not_strike() {
        assert_eq!(analyze("lone ~10% is fine").stats.strikethrough, 0);
        assert_eq!(strike_start_end_counts("lone ~10% is fine"), (0, 0));
    }

    #[test]
    fn mixed_double_and_single_tilde_counts_one_strike() {
        let doc = "keep ~~this~~ but not ~that~";
        assert_eq!(analyze(doc).stats.strikethrough, 1);
        assert_eq!(strike_start_end_counts(doc), (1, 1));
    }

    #[test]
    fn nested_double_inside_single_tilde_is_balanced() {
        // Outer single-`~` demoted; inner `~~…~~` kept — Start/End must stay paired.
        let doc = "~start ~~double~~ end~";
        assert_eq!(analyze(doc).stats.strikethrough, 1);
        assert_eq!(strike_start_end_counts(doc), (1, 1));
        let texts: Vec<_> = offset_events(doc)
            .filter_map(|(e, _)| match e {
                Event::Text(t) => Some(t.into_string()),
                _ => None,
            })
            .collect();
        // Outer delimiters visible as literal text (not only via strike styling).
        assert!(texts.iter().filter(|t| t.as_str() == "~").count() >= 2);
    }

    #[test]
    fn well_formed_doc_has_no_issues() {
        // Heading, paragraph, then a table with a real body row.
        let doc = "# Title\n\nIntro paragraph.\n\n| a | b |\n| - | - |\n| c | d |\n";
        assert_eq!(analyze(doc).issues, Vec::<StructuralIssue>::new());
    }

    #[test]
    fn well_formed_table_is_not_malformed() {
        // A leading, body-bearing table parses cleanly: a count, never a render-fidelity failure.
        let analysis = analyze("| a | b |\n| - | - |\n| c | d |\n");
        assert_eq!(analysis.stats.tables, 1);
        assert!(!analysis.issues.contains(&StructuralIssue::MalformedTable));
    }

    #[test]
    fn chained_delimiter_rows_flag_one_malformed_table() {
        // One broken table with stacked delimiter-shaped rows must flag exactly once:
        // a delimiter row never doubles as the next row's "header". Column counts
        // differ on every adjacent pair so pulldown parses no table at all (two
        // equal-width delimiter rows would parse as a table themselves).
        let doc = "| a | b | c |\n|---|---|---|----|\n|---|---|---|---|----|\n| 1 | 2 | 3 |\n";
        let analysis = analyze(doc);
        assert_eq!(analysis.stats.tables, 0);
        assert_eq!(
            analysis
                .issues
                .iter()
                .filter(|i| **i == StructuralIssue::MalformedTable)
                .count(),
            1
        );
    }

    #[test]
    fn header_only_table_is_not_malformed() {
        // A header + delimiter (no body) still parses as a table, so it is not a malformed table.
        let analysis = analyze("| a | b |\n| - | - |\n");
        assert_eq!(analysis.stats.tables, 1);
        assert!(!analysis.issues.contains(&StructuralIssue::MalformedTable));
    }

    #[test]
    fn delimiter_column_mismatch_flags_malformed_table() {
        // Header has 2 columns, delimiter has 3: pulldown-cmark abandons the table.
        let analysis = analyze("| a | b |\n| - | - | - |\n| c | d | e |\n");
        assert_eq!(analysis.stats.tables, 0);
        assert!(analysis.issues.contains(&StructuralIssue::MalformedTable));
    }

    #[test]
    fn broken_table_extra_delimiter_column_flags_malformed_table() {
        // Wide synthetic table whose delimiter row has 12 columns but the header
        // has 11 (an extra `|---|`), so pulldown-cmark renders the lines as a
        // paragraph instead of a table.
        let doc = "\
| ColA | ColB | ColC | ColD | ColE | ColF | ColG | ColH | ColI | ColJ | ColK |
|---|---|---|---|---|---|---|---|---|---|---|------------------------------------|
| A001 | 2026-01-01 12:00 (REF-100001) | ITEM-2026-01-01-00-00-00-ABCDEF01 | 2026-01-01 12:00:00 | 1.00 | 0.0 | 20.0 | 10.0 | 50.00 | left | 1 |
| A002 | 2026-01-02 12:00 (REF-100002) | N/A (sample ends) | N/A | N/A | N/A | N/A | N/A | N/A | N/A | N/A |
";
        let analysis = analyze(doc);
        assert_eq!(
            analysis.stats.tables, 0,
            "pulldown-cmark must not parse the broken table"
        );
        assert!(
            analysis.issues.contains(&StructuralIssue::MalformedTable),
            "the intended-but-unparsed table must be flagged"
        );
    }

    #[test]
    fn pipe_prose_is_not_a_malformed_table() {
        // A paragraph that merely contains pipes (no delimiter row) must not flag.
        let analysis = analyze("use `a | b` in the shell\nand `c | d` too\n");
        assert!(!analysis.issues.contains(&StructuralIssue::MalformedTable));
    }

    #[test]
    fn delimiter_inside_code_fence_is_not_a_malformed_table() {
        // A `|---|` line inside a fenced code block is literal content, not a table.
        let analysis = analyze("```\n| a | b |\n|---|---|---|\n```\n");
        assert!(!analysis.issues.contains(&StructuralIssue::MalformedTable));
    }

    #[test]
    fn setext_heading_dashes_are_not_a_malformed_table() {
        // `Title\n-----` is a setext H2: the underline has no pipe, so it is not a delimiter row.
        let analysis = analyze("Title\n-----\n");
        assert!(!analysis.issues.contains(&StructuralIssue::MalformedTable));
    }

    #[test]
    fn unterminated_fenced_block_flags_issue() {
        let analysis = analyze("```\ncode\n");
        assert!(
            analysis
                .issues
                .contains(&StructuralIssue::UnterminatedCodeBlock)
        );
    }

    #[test]
    fn closed_fenced_block_has_no_unterminated_issue() {
        let analysis = analyze("```\ncode\n```\n");
        assert!(
            !analysis
                .issues
                .contains(&StructuralIssue::UnterminatedCodeBlock)
        );
    }

    #[test]
    fn closed_fenced_block_with_lang_has_no_unterminated_issue() {
        let analysis = analyze("```rust\nx\n```\n");
        assert!(
            !analysis
                .issues
                .contains(&StructuralIssue::UnterminatedCodeBlock)
        );
    }

    #[test]
    fn tilde_fenced_block_closed_has_no_unterminated_issue() {
        let analysis = analyze("~~~\nx\n~~~\n");
        assert!(
            !analysis
                .issues
                .contains(&StructuralIssue::UnterminatedCodeBlock)
        );
    }

    #[test]
    fn blockquoted_closed_fence_has_no_unterminated_issue() {
        // The block range keeps the `>` prefixes; a properly-closed quoted fence must be clean.
        let analysis = analyze("> ```\n> code\n> ```\n");
        assert!(
            !analysis
                .issues
                .contains(&StructuralIssue::UnterminatedCodeBlock)
        );
    }

    #[test]
    fn blockquoted_unterminated_fence_flags_issue() {
        let analysis = analyze("> ```\n> code\n");
        assert!(
            analysis
                .issues
                .contains(&StructuralIssue::UnterminatedCodeBlock)
        );
    }

    #[test]
    fn longer_opener_not_closed_by_shorter_fence() {
        // A 5-backtick opener is not closed by a 3-backtick line (close must be >= opener length).
        let analysis = analyze("`````\ncode\n```\n");
        assert!(
            analysis
                .issues
                .contains(&StructuralIssue::UnterminatedCodeBlock)
        );
    }

    // (name, doc, parsed_tables) corpus of intended-but-broken tables, shared by the
    // detection test and the flagged-implies-not-parsed invariant below.
    const MALFORMED_TABLE_TRUE_POSITIVES: &[(&str, &str, u32)] = &[
        (
            "delimiter_wider_than_header",
            "| a | b |\n|---|---|---|\n| c | d | e |\n",
            0,
        ),
        (
            "delimiter_narrower_than_header",
            "| a | b | c |\n|---|---|\n| d | e | f |\n",
            0,
        ),
        (
            "broken_table_in_blockquote",
            "> | a | b |\n> |---|---|---|\n",
            0,
        ),
        (
            "broken_table_at_eof_no_newline",
            "| a | b |\n|---|---|---|",
            0,
        ),
    ];

    #[test]
    fn malformed_table_true_positives() {
        for (name, doc, _) in MALFORMED_TABLE_TRUE_POSITIVES {
            assert!(
                analyze(doc)
                    .issues
                    .contains(&StructuralIssue::MalformedTable),
                "missed malformed table in `{name}`: {doc:?}"
            );
        }
    }

    #[test]
    fn flagged_implies_table_not_parsed() {
        // Invariant: a flagged doc's broken table must not also be counted as parsed.
        for (name, doc, parsed_tables) in MALFORMED_TABLE_TRUE_POSITIVES {
            assert_eq!(
                analyze(doc).stats.tables,
                *parsed_tables,
                "broken table in `{name}` must not count as parsed: {doc:?}"
            );
        }
    }

    #[test]
    fn malformed_table_no_false_positives() {
        // (name, doc) docs that either parse as real tables or are legitimately not tables.
        let cases: &[(&str, &str)] = &[
            (
                "aligned_colon_delimiter",
                "| a | b | c |\n|:---|---:|:--:|\n| 1 | 2 | 3 |\n",
            ),
            (
                "table_in_blockquote",
                "> | a | b |\n> | - | - |\n> | c | d |\n",
            ),
            (
                "table_in_list_item",
                "- item\n\n  | a | b |\n  | - | - |\n  | c | d |\n",
            ),
            (
                "mismatched_delimiter_in_tilde_fence",
                "~~~\n| a | b |\n|---|---|---|\n~~~\n",
            ),
            ("crlf_table", "| a | b |\r\n| - | - |\r\n| c | d |\r\n"),
            (
                "multi_byte_unicode_header",
                "| ünïcødé | b |\n| - | - |\n| c | d |\n",
            ),
            ("table_at_eof_no_newline", "| a | b |\n| - | - |\n| c | d |"),
            (
                "two_valid_tables_with_prose_between",
                "| a | b |\n| - | - |\n| c | d |\n\nprose here\n\n| e | f |\n| - | - |\n| g | h |\n",
            ),
            // The `---` has no pipe, so it is a setext underline / break, not a delimiter row.
            ("pipe_prose_then_plain_dashes", "uses a | b pipe\n---\n"),
            (
                "mismatched_delimiter_in_indented_code_block",
                "    | a |\n    |---|---|\n",
            ),
        ];
        for (name, doc) in cases {
            assert!(
                !analyze(doc)
                    .issues
                    .contains(&StructuralIssue::MalformedTable),
                "false positive on `{name}`: {doc:?}"
            );
        }
    }

    #[test]
    fn legacy_mdx_rule_divergence() {
        // Legacy markdown-validator MDX_* rule -> our verdict. We only flag
        // render-fidelity failures (intended structure that pulldown did not parse),
        // never style opinions:
        //   FENCE_UNBALANCED          -> UnterminatedCodeBlock (fence swallows the rest of the doc).
        //   TABLE_COLUMN_MISMATCH     -> MalformedTable (header/delimiter arity mismatch un-parses it).
        //   TABLE_DIVIDER_INVALID     -> NOT flagged: pulldown rejects the table, but a divider with
        //                                non-`|-: ` chars also fails our delimiter predicate. Accepted
        //                                safe-direction miss (under-penalize, never over-penalize).
        //   TABLE_START               -> not flagged: a doc-leading table renders fine (style opinion).
        //   TABLE_MIN_ROWS            -> not flagged: GFM parses header+delimiter as a body-less table.
        //   TABLE_EMPTY_HEADER        -> not flagged: empty header cells still parse as a table.
        //   TABLE_MISSING_BLANK_AFTER -> not flagged: GFM swallows the trailing prose line as a row.
        //   TABLE_CELL_NEWLINE        -> not flagged: each physical line is its own row; still a table.
        //   TABLE_FENCE_IN_CELL       -> not flagged: backticks in a cell are inline content.
        //   TABLE_IN_CODEBLOCK        -> not flagged: a table inside a fence is literal code by design.
        let cases: &[(&str, &str, u32, bool, bool)] = &[
            // (legacy rule, scenario doc, parsed tables, malformed_table?, unterminated_code_block?)
            ("FENCE_UNBALANCED", "```\ncode\n", 0, false, true),
            (
                "TABLE_COLUMN_MISMATCH",
                "| a | b |\n|---|---|---|\n| c | d | e |\n",
                0,
                true,
                false,
            ),
            (
                "TABLE_DIVIDER_INVALID",
                "| a | b |\n| -- | xx |\n| c | d |\n",
                0,
                false,
                false,
            ),
            (
                "TABLE_START",
                "| a | b |\n| - | - |\n| c | d |\n\nprose\n",
                1,
                false,
                false,
            ),
            ("TABLE_MIN_ROWS", "| a | b |\n| - | - |\n", 1, false, false),
            (
                "TABLE_EMPTY_HEADER",
                "| | |\n|---|---|\n| c | d |\n",
                1,
                false,
                false,
            ),
            (
                "TABLE_MISSING_BLANK_AFTER",
                "| a | b |\n| - | - |\n| c | d |\nprose\n",
                1,
                false,
                false,
            ),
            (
                "TABLE_CELL_NEWLINE",
                "| a | b |\n| - | - |\n| c | d\ne |\n",
                1,
                false,
                false,
            ),
            (
                "TABLE_FENCE_IN_CELL",
                "| a | ``` |\n| - | - |\n| c | d |\n",
                1,
                false,
                false,
            ),
            (
                "TABLE_IN_CODEBLOCK",
                "```\n| a | b |\n| - | - |\n| c | d |\n```\n",
                0,
                false,
                false,
            ),
        ];
        for (rule, doc, tables, malformed, unterminated) in cases {
            let analysis = analyze(doc);
            assert_eq!(
                analysis.stats.tables, *tables,
                "MDX_{rule}: parsed tables in {doc:?}"
            );
            assert_eq!(
                analysis.issues.contains(&StructuralIssue::MalformedTable),
                *malformed,
                "MDX_{rule}: malformed_table in {doc:?}"
            );
            assert_eq!(
                analysis
                    .issues
                    .contains(&StructuralIssue::UnterminatedCodeBlock),
                *unterminated,
                "MDX_{rule}: unterminated_code_block in {doc:?}"
            );
        }
    }

    #[test]
    fn gfm_spec_derived_table_cases() {
        // Minimal docs re-derived from the GFM spec's table-recognition rules
        // (section 4.10 "Tables (extension)"); each comment cites the behavior.
        let cases: &[(&str, &str, u32, bool)] = &[
            // (name, doc, parsed tables, malformed_table?)
            // GFM: a header row + matching delimiter row form a table (ex. 198).
            (
                "arity_match_is_table",
                "| foo | bar |\n| --- | --- |\n| baz | bim |\n",
                1,
                false,
            ),
            // GFM: delimiter cells may carry alignment colons and skip outer pipes (ex. 199).
            (
                "alignment_colons_no_outer_pipes",
                "| abc | defghi |\n:-: | -----------:\n| bar | baz |\n",
                1,
                false,
            ),
            // GFM: `\|` escapes a pipe inside a cell instead of splitting it (ex. 200).
            (
                "escaped_pipe_in_cell",
                "| f\\|oo | bar |\n| --- | --- |\n| b\\|az | bim |\n",
                1,
                false,
            ),
            // GFM: header/delimiter cell-count mismatch -> no table is recognized (ex. 203).
            (
                "arity_mismatch_not_recognized",
                "| abc | def |\n| --- |\n| bar |\n",
                0,
                true,
            ),
            // GFM: body rows may have more/fewer cells; padded/truncated, still a table (ex. 204).
            (
                "ragged_body_rows_still_table",
                "| abc | def |\n| --- | --- |\n| bar |\n| bar | baz | boo |\n",
                1,
                false,
            ),
            // GFM: the table is broken at the first empty line (ex. 205); the
            // pipe line after the blank is plain prose, not a second table.
            (
                "blank_line_ends_table",
                "| abc | def |\n| --- | --- |\n\n| bar | baz |\n",
                1,
                false,
            ),
            // A blank line between header and delimiter prevents recognition, and the
            // delimiter no longer sits under a header line, so we do not flag either.
            (
                "blank_between_header_and_delimiter",
                "| a | b |\n\n| - | - |\n",
                0,
                false,
            ),
        ];
        for (name, doc, tables, malformed) in cases {
            let analysis = analyze(doc);
            assert_eq!(
                analysis.stats.tables, *tables,
                "gfm `{name}`: parsed tables in {doc:?}"
            );
            assert_eq!(
                analysis.issues.contains(&StructuralIssue::MalformedTable),
                *malformed,
                "gfm `{name}`: malformed_table in {doc:?}"
            );
        }
    }

    #[test]
    fn valid_table_mutations_flag() {
        // Corrupting ONLY the delimiter row of a valid table must both un-parse the
        // table and raise MalformedTable -- the detector tracks pulldown exactly.
        let bases = [
            "| a | b |\n| - | - |\n| c | d |\n",
            "| a | b | c |\n| - | - | - |\n| d | e | f |\n",
            "| a | b | c | d | e |\n| - | - | - | - | - |\n| f | g | h | i | j |\n",
            "> | a | b |\n> | - | - |\n> | c | d |\n",
        ];
        // Rebuild the doc with line 1 (the delimiter row) rewritten by `mutate`.
        fn with_delimiter(base: &str, mutate: impl Fn(&str) -> String) -> String {
            let lines: Vec<String> = base
                .lines()
                .enumerate()
                .map(|(i, line)| {
                    if i == 1 {
                        mutate(line)
                    } else {
                        line.to_string()
                    }
                })
                .collect();
            lines.join("\n") + "\n"
        }

        for base in bases {
            let clean = analyze(base);
            assert!(
                !clean.issues.contains(&StructuralIssue::MalformedTable),
                "base must be clean: {base:?}"
            );
            assert_eq!(clean.stats.tables, 1, "base parses as one table: {base:?}");

            let mutants = [
                // Append one extra `---|` cell to the delimiter row (wider than header).
                (
                    "append_delimiter_cell",
                    with_delimiter(base, |line| format!("{line}---|")),
                ),
                // Delete one ` - |` cell from the delimiter row (narrower than header).
                (
                    "drop_delimiter_cell",
                    with_delimiter(base, |line| {
                        let at = line.rfind(" - |").expect("delimiter has a ` - |` cell");
                        format!("{}{}", &line[..at], &line[at + 4..])
                    }),
                ),
            ];
            for (mutation, mutant) in mutants {
                let analysis = analyze(&mutant);
                assert!(
                    analysis.issues.contains(&StructuralIssue::MalformedTable),
                    "{mutation} must flag MalformedTable: {mutant:?}"
                );
                assert_eq!(
                    analysis.stats.tables,
                    clean.stats.tables - 1,
                    "{mutation} must un-parse the table: {mutant:?}"
                );
            }
        }
    }
}
