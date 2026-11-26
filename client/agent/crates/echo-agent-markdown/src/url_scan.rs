//! Plain-URL detection over rendered display ratatui Lines.

use linkify::{LinkFinder, LinkKind};
use ratatui::text::Line;

use crate::buffers::unicode_display_width;
use crate::output::HyperlinkTarget;

/// Scan `lines` for plain URLs and return new `HyperlinkTarget` entries
/// that don't overlap any existing target in `existing`.
///
/// `next_id` is the first id to assign; the returned `u32` is the
/// post-scan counter, suitable for stuffing back into
/// `FrozenState::next_link_id`.
pub(crate) fn detect_plain_urls(
    lines: &[Line<'_>],
    existing: &[HyperlinkTarget],
    next_id: u32,
) -> (Vec<HyperlinkTarget>, u32) {
    detect_plain_urls_with_offset(lines, 0, existing, next_id)
}

/// Like [`detect_plain_urls`] but scans `lines` whose first element
/// represents document line `line_index_offset` (caller passes a tail
/// slice of `self.output.lines` and the index of its first element).
///
/// Lines fully inside `0..line_index_offset` are assumed to be in
/// `existing` already and are not re-scanned.  The dedup overlap check
/// still works correctly because emitted targets use document-absolute
/// `line_index = line_index_offset + i`, matching the indices already
/// present in `existing`.
pub(crate) fn detect_plain_urls_with_offset(
    lines: &[Line<'_>],
    line_index_offset: usize,
    existing: &[HyperlinkTarget],
    next_id: u32,
) -> (Vec<HyperlinkTarget>, u32) {
    let mut result = Vec::new();
    let mut current_id = next_id;
    let mut finder = LinkFinder::new();
    finder.kinds(&[LinkKind::Url, LinkKind::Email]);

    for (i, line) in lines.iter().enumerate() {
        let line_index = line_index_offset + i;
        let mut display_col: usize = 0;

        for span in &line.spans {
            let span_text: &str = span.content.as_ref();

            for link in finder.links(span_text) {
                let start = link.start();
                let end = link.end();
                if start > end
                    || end > span_text.len()
                    || !span_text.is_char_boundary(start)
                    || !span_text.is_char_boundary(end)
                {
                    continue;
                }
                let before = &span_text[..start];
                let matched = &span_text[start..end];

                let col_start = display_col + unicode_display_width(before);
                let col_end = col_start + unicode_display_width(matched);
                let url = match link.kind() {
                    LinkKind::Email => {
                        // `git@github.com:org/repo` is an scp remote, not mail.
                        if matches!(span_text.as_bytes().get(end), Some(b':' | b'/')) {
                            continue;
                        }
                        format!("mailto:{}", link.as_str())
                    }
                    _ => link.as_str().to_string(),
                };

                // Dedup: skip if any existing or already-added target overlaps
                // on the same line. Overlap: cand.start < ex.end && ex.start < cand.end.
                let overlaps = existing.iter().chain(result.iter()).any(|h| {
                    h.line_index == line_index
                        && col_start < h.column_range.end
                        && h.column_range.start < col_end
                });

                if !overlaps {
                    result.push(HyperlinkTarget {
                        line_index,
                        column_range: col_start..col_end,
                        url,
                        id: current_id,
                    });
                    current_id += 1;
                }
            }

            display_col += unicode_display_width(span_text);
        }
    }

    (result, current_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StreamingMarkdownRenderer;
    use crate::style::test_style;

    /// Helper: render markdown via StreamingMarkdownRenderer::finish() and
    /// return the hyperlinks from the finalized output.
    fn finish_and_get_hyperlinks(text: &str) -> Vec<HyperlinkTarget> {
        let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, true);
        renderer.push_and_render(text, None);
        let view = renderer.finish(None);
        view.hyperlinks.to_vec()
    }

    fn line_to_string(line: &Line<'static>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn plain_url_in_prose_produces_target() {
        let text = "See https://example.com for details.\n";
        let hyperlinks = finish_and_get_hyperlinks(text);

        assert_eq!(hyperlinks.len(), 1, "exactly one hyperlink expected");
        let h = &hyperlinks[0];
        assert_eq!(h.url, "https://example.com");

        // Verify column range covers only the URL
        let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, true);
        renderer.push_and_render(text, None);
        let view = renderer.finish(None);
        let rendered = line_to_string(&view.lines[h.line_index]);
        let slice: String = rendered
            .chars()
            .skip(h.column_range.start)
            .take(h.column_range.len())
            .collect();
        assert_eq!(slice, "https://example.com");
    }

    #[test]
    fn multiple_urls_one_line_distinct_ids() {
        let text = "See https://a.example and https://b.example.\n";
        let hyperlinks = finish_and_get_hyperlinks(text);

        assert_eq!(hyperlinks.len(), 2, "two hyperlinks expected");
        assert_ne!(hyperlinks[0].id, hyperlinks[1].id, "ids must differ");
        assert_eq!(hyperlinks[0].url, "https://a.example");
        assert_eq!(hyperlinks[1].url, "https://b.example");
        // Column ranges must be disjoint
        assert!(
            hyperlinks[0].column_range.end <= hyperlinks[1].column_range.start,
            "column ranges must be disjoint, got {:?} vs {:?}",
            hyperlinks[0].column_range,
            hyperlinks[1].column_range,
        );
    }

    #[test]
    fn markdown_link_with_url_text_does_not_double_link() {
        let text = "Visit [https://example.com](https://example.com).\n";
        let hyperlinks = finish_and_get_hyperlinks(text);

        // NOTE: one might expect 1 target, but in pretty mode
        // `[url](url)` renders as `url (url)`, showing the URL in two
        // distinct positions: once as the link text (covered by the parser's
        // HyperlinkTarget) and once in the `(url)` suffix at a disjoint
        // column range. Dedup prevents a *third* entry at the same column
        // range as the parser-produced target; the second entry (at the
        // suffix position) is correctly detected as a separate target.
        assert_eq!(
            hyperlinks.len(),
            2,
            "expected 2 hyperlinks (parser link text + URL in pretty-mode suffix), got {}",
            hyperlinks.len()
        );
        // Both should reference the same URL.
        assert!(hyperlinks.iter().all(|h| h.url == "https://example.com"));
        // Column ranges must be disjoint (dedup working correctly).
        assert!(
            hyperlinks[0].column_range.end <= hyperlinks[1].column_range.start
                || hyperlinks[1].column_range.end <= hyperlinks[0].column_range.start,
            "column ranges must be disjoint, got {:?} and {:?}",
            hyperlinks[0].column_range,
            hyperlinks[1].column_range,
        );
    }

    #[test]
    fn autolink_does_not_double_link() {
        let text = "Visit <https://example.com>.\n";
        let hyperlinks = finish_and_get_hyperlinks(text);

        // All entries should share the same URL — autolinks may produce
        // multiple HyperlinkTarget fragments sharing the same id.
        let autolink_count = hyperlinks
            .iter()
            .filter(|h| h.url == "https://example.com")
            .count();
        assert!(autolink_count >= 1, "expected at least one autolink target");
        // The total count should match the autolink fragments only — no
        // extra plain-URL duplicates.
        assert_eq!(
            hyperlinks.len(),
            autolink_count,
            "plain-URL scan should not add duplicates on top of autolink targets"
        );
    }

    #[test]
    fn plain_email_in_prose_produces_mailto_target() {
        let text = "Email foo@bar.com please.\n";
        let hyperlinks = finish_and_get_hyperlinks(text);

        assert_eq!(hyperlinks.len(), 1, "exactly one hyperlink expected");
        assert_eq!(hyperlinks[0].url, "mailto:foo@bar.com");

        let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, true);
        renderer.push_and_render(text, None);
        let view = renderer.finish(None);
        let rendered = line_to_string(&view.lines[hyperlinks[0].line_index]);
        let slice: String = rendered
            .chars()
            .skip(hyperlinks[0].column_range.start)
            .take(hyperlinks[0].column_range.len())
            .collect();
        assert_eq!(slice, "foo@bar.com");
    }

    #[test]
    fn email_after_multibyte_prefix_is_mailto() {
        let hyperlinks = finish_and_get_hyperlinks("連絡先: foo@bar.com です\n");
        assert_eq!(hyperlinks.len(), 1);
        assert_eq!(hyperlinks[0].url, "mailto:foo@bar.com");
    }

    #[test]
    fn scp_git_remote_is_not_mailto() {
        let hyperlinks = finish_and_get_hyperlinks("clone git@github.com:org/repo.git\n");
        assert!(
            hyperlinks.iter().all(|h| !h.url.starts_with("mailto:")),
            "scp-style git remotes must not become mailto links: {hyperlinks:?}"
        );
    }

    #[test]
    fn trailing_period_excluded_from_url() {
        let text = "See https://example.com.\n";
        let hyperlinks = finish_and_get_hyperlinks(text);

        assert_eq!(hyperlinks.len(), 1);
        assert_eq!(
            hyperlinks[0].url, "https://example.com",
            "trailing dot should be excluded by linkify"
        );
    }

    #[test]
    fn cjk_neighbors_preserve_correct_columns() {
        use crate::buffers::unicode_display_width;

        let text = "日本語 https://example.com 日本語\n";
        let hyperlinks = finish_and_get_hyperlinks(text);

        assert_eq!(hyperlinks.len(), 1);
        let h = &hyperlinks[0];
        assert_eq!(h.url, "https://example.com");

        // "日本語 " has 3 CJK chars (2 cells each) + 1 space = 7 display cells
        let prefix = "日本語 ";
        let expected_start = unicode_display_width(prefix);
        assert_eq!(expected_start, 7, "prefix should be 7 display cells");
        assert_eq!(h.column_range.start, expected_start);

        let url_width = unicode_display_width("https://example.com");
        assert_eq!(h.column_range.end, expected_start + url_width);
    }

    #[test]
    fn empty_document_returns_empty() {
        let hyperlinks = finish_and_get_hyperlinks("");
        assert!(
            hyperlinks.is_empty(),
            "empty document should produce no hyperlinks"
        );
    }

    /// Behavior pin: URL inside inline code.
    ///
    /// This test pins the *current* behavior — if linkify matches inside
    /// the code-styled span, the URL becomes a HyperlinkTarget. If we
    /// later decide to skip code-styled spans, this test will fail loudly
    /// and force the change to be intentional.
    #[test]
    fn url_inside_inline_code_documented_behavior() {
        let text = "Use `https://example.com` carefully.\n";
        let hyperlinks = finish_and_get_hyperlinks(text);

        // Pin observed behavior: linkify finds the URL inside the
        // code-styled span, producing a HyperlinkTarget.
        assert!(
            !hyperlinks.is_empty(),
            "behavior pin: URL inside inline code currently produces a HyperlinkTarget"
        );
        assert_eq!(hyperlinks[0].url, "https://example.com");
    }

    /// Behavior pin: URL inside a fenced code block.
    ///
    /// Same rationale as `url_inside_inline_code_documented_behavior` —
    /// this pins current behavior, not product spec.
    #[test]
    fn url_inside_code_fence_documented_behavior() {
        let text = "```\nsee https://example.com\n```\n";
        let hyperlinks = finish_and_get_hyperlinks(text);

        // Pin observed behavior: linkify finds the URL in the code block's
        // rendered text. Whether this is desirable is a product decision;
        // this test ensures any change is intentional.
        let has_url = hyperlinks.iter().any(|h| h.url == "https://example.com");
        assert!(
            has_url,
            "behavior pin: URL inside fenced code block currently produces a HyperlinkTarget"
        );
    }

    /// URL detection must run from `render()` too, not only `finish()`.
    /// Otherwise width changes or other state resets (e.g. via
    /// `set_max_table_width`) drop the URL hyperlinks that pretty-mode
    /// rendering adds for the `(url)` suffix of markdown links.
    ///
    /// Also pins the OSC 8 grouping invariant: the link-text and URL
    /// hyperlinks must have DISTINCT ids and DISJOINT column ranges, so
    /// terminals group them as two separate hyperlinks (not one merged
    /// underline across the brackets).
    #[test]
    fn render_detects_pretty_mode_url_suffix() {
        let text = "[link](https://example.com/some/long/path)\n";
        let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, true);
        renderer.push_and_render(text, None);
        // No finish() call: after render() alone, both the parser-produced
        // (link text) and the url_scan-produced (URL in `(url)` suffix)
        // hyperlinks must be present.
        let view = renderer.view();
        assert_eq!(
            view.hyperlinks.len(),
            2,
            "render() must produce both the link-text and URL-suffix hyperlinks; \
             got {:?}",
            view.hyperlinks,
        );
        assert!(
            view.hyperlinks
                .iter()
                .all(|h| h.url == "https://example.com/some/long/path")
        );
        assert_ne!(
            view.hyperlinks[0].id, view.hyperlinks[1].id,
            "link-text and URL-suffix hyperlinks must have distinct OSC 8 ids",
        );
        let (a, b) = (&view.hyperlinks[0], &view.hyperlinks[1]);
        assert!(
            a.column_range.end <= b.column_range.start
                || b.column_range.end <= a.column_range.start,
            "column ranges must be disjoint, got {:?} and {:?}",
            a.column_range,
            b.column_range,
        );
    }

    /// Snapshot helper used by survival tests below.
    fn snapshot(view: &crate::output::MarkdownRenderView<'_>) -> Vec<HyperlinkTarget> {
        let mut snap: Vec<HyperlinkTarget> = view.hyperlinks.to_vec();
        snap.sort_by_key(|h| (h.line_index, h.column_range.start));
        snap
    }

    fn assert_url_suffix_preserved(
        before: &[HyperlinkTarget],
        after: &[HyperlinkTarget],
        url: &str,
    ) {
        let before_suffix = before
            .iter()
            .find(|h| h.url == url && h.column_range.start > 5)
            .expect("URL-suffix hyperlink must be present BEFORE reset");
        let after_suffix = after
            .iter()
            .find(|h| h.url == url && h.column_range.start > 5)
            .expect("URL-suffix hyperlink must be present AFTER reset");
        assert_eq!(
            before_suffix.column_range, after_suffix.column_range,
            "URL-suffix column range must be stable across the reset",
        );
        assert_eq!(
            before_suffix.line_index, after_suffix.line_index,
            "URL-suffix line index must be stable across the reset",
        );
    }

    /// After `finish()`, re-rendering (e.g. triggered by a width change
    /// via `set_max_table_width`) must NOT drop the URL hyperlinks that
    /// pretty-mode adds for the `(url)` suffix.
    ///
    /// Snapshots the full hyperlink list before/after the reset and
    /// asserts that the URL-suffix entry survives with its column range
    /// intact (the OSC 8 id may be re-assigned by the post-reset
    /// re-render — that's expected — but the location must not move).
    #[test]
    fn url_hyperlinks_survive_re_render_after_finish() {
        let url = "https://example.com/some/long/path";
        let text = format!("[link]({url})\n");
        let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, true);
        renderer.push_and_render(&text, None);
        renderer.finish(None);
        let before = snapshot(&renderer.view());

        // Simulate a width change which resets renderer state.
        renderer.set_max_table_width(Some(40));
        renderer.render(None);
        let after = snapshot(&renderer.view());

        assert_eq!(
            before.len(),
            after.len(),
            "hyperlink count must be stable across the reset; before={before:?} after={after:?}",
        );
        assert_url_suffix_preserved(&before, &after, url);
    }

    /// Identical contract to `url_hyperlinks_survive_re_render_after_finish`
    /// but exercising the `set_pretty` reset path (production:
    /// `MarkdownContent::set_raw_mode` toggle).
    #[test]
    fn url_hyperlinks_survive_re_render_after_set_pretty_toggle() {
        let url = "https://example.com/some/long/path";
        let text = format!("[link]({url})\n");
        let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, true);
        renderer.push_and_render(&text, None);
        renderer.finish(None);
        let before = snapshot(&renderer.view());

        // Toggle pretty off then back on — both transitions reset state.
        renderer.set_pretty(false);
        renderer.set_pretty(true);
        renderer.render(None);
        let after = snapshot(&renderer.view());

        assert_eq!(
            before.len(),
            after.len(),
            "hyperlink count must be stable across the set_pretty toggle",
        );
        assert_url_suffix_preserved(&before, &after, url);
    }

    /// Identical contract to `url_hyperlinks_survive_re_render_after_finish`
    /// but exercising the `set_style` reset path (production: theme change
    /// via `MarkdownContent::ensure_wrapped` when theme cache kind shifts).
    #[test]
    fn url_hyperlinks_survive_re_render_after_set_style() {
        let url = "https://example.com/some/long/path";
        let text = format!("[link]({url})\n");
        let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, true);
        renderer.push_and_render(&text, None);
        renderer.finish(None);
        let before = snapshot(&renderer.view());

        // `set_style` unconditionally resets state, even with same style.
        renderer.set_style(test_style::STYLE);
        renderer.render(None);
        let after = snapshot(&renderer.view());

        assert_eq!(
            before.len(),
            after.len(),
            "hyperlink count must be stable across the set_style reset",
        );
        assert_url_suffix_preserved(&before, &after, url);
    }
}
