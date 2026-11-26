//! Diff hunk construction for the EchoAgent Build TUI.
//!
//! Turns edit-tool output (structured `SearchReplaceEditDetail` records, ACP
//! `ToolCall` payloads, or plain before/after text) into line-tagged
//! [`DiffHunk`]s that the pager renders, and back into unified-diff text.

use echo_agent_tools::types::output::SearchReplaceEditDetail;
use similar::{ChangeTag, TextDiff};

#[derive(Debug, Clone, PartialEq)]
pub struct DiffLine {
    pub text: String,
    pub lo: usize,
    pub ln: usize,
    pub tag: ChangeTag,
}

pub type DiffHunk = Vec<DiffLine>;

pub fn build_diff_hunks(details: &[SearchReplaceEditDetail]) -> Vec<DiffHunk> {
    const MAX_CONTEXT: usize = 3;
    let mut hunks: Vec<DiffHunk> = Vec::new();

    for edit in details {
        let mut diff_lines: DiffHunk = Vec::new();
        let before_lines: Vec<String> = if edit.context_before.is_empty() {
            vec![]
        } else {
            edit.context_before
                .split_inclusive('\n')
                .map(|s| s.to_string())
                .collect()
        };
        let n_before = before_lines.len();
        for (i, line_text) in before_lines.into_iter().enumerate() {
            // +1 because context_before ends just before edit.old_line/new_line
            let from_end = n_before.saturating_sub(i + 1);
            let lo = edit.old_line.saturating_sub(from_end + 1);
            let ln = edit.new_line.saturating_sub(from_end + 1);
            diff_lines.push(DiffLine {
                text: line_text,
                lo,
                ln,
                tag: ChangeTag::Equal,
            });
        }
        let (mut lo, mut ln) = (edit.old_line, edit.new_line);
        let empty_to_empty = edit.old_string.is_empty() && edit.new_string.is_empty();
        let mid_file = !edit.context_before.is_empty() || !edit.context_after.is_empty();
        let new_text: &str = if empty_to_empty && mid_file {
            // Blank-line insertion: both sides empty but a line was inserted.
            // Represent the blank line so TextDiff can see the insertion.
            // Context-free empty-to-empty (an empty file write) must NOT
            // fabricate a +1 insertion — it produces no diff lines at all.
            "\n"
        } else {
            &edit.new_string
        };
        // When old_string/new_string start mid-line (after indentation),
        // the diff lines are missing the leading whitespace that context lines
        // have. Prepend `line_prefix` to each change on the first file line.
        // Once a change contains a newline, subsequent lines are full file
        // lines and don't need the prefix.
        let prefix = &edit.line_prefix;
        let has_prefix = !prefix.is_empty();
        let mut prefix_applied_delete = false;
        let mut prefix_applied_insert = false;
        let diff = TextDiff::from_lines(edit.old_string.as_str(), new_text);
        for change in diff.iter_all_changes() {
            let tag = change.tag();
            let mut text = change.value().to_owned();
            if has_prefix {
                let needs_prefix = match tag {
                    ChangeTag::Delete => !prefix_applied_delete,
                    ChangeTag::Insert => !prefix_applied_insert,
                    ChangeTag::Equal => !prefix_applied_delete && !prefix_applied_insert,
                };
                if needs_prefix {
                    text.insert_str(0, prefix);
                }
                match tag {
                    ChangeTag::Delete | ChangeTag::Equal => prefix_applied_delete = true,
                    ChangeTag::Insert => prefix_applied_insert = true,
                }
            }
            diff_lines.push(DiffLine { text, lo, ln, tag });
            match tag {
                ChangeTag::Equal => {
                    lo = lo.saturating_add(1);
                    ln = ln.saturating_add(1);
                }
                ChangeTag::Delete => {
                    lo = lo.saturating_add(1);
                }
                ChangeTag::Insert => {
                    ln = ln.saturating_add(1);
                }
            }
        }

        if !edit.context_after.is_empty() {
            for line in edit.context_after.split_inclusive('\n') {
                diff_lines.push(DiffLine {
                    text: line.to_owned(),
                    lo,
                    ln,
                    tag: ChangeTag::Equal,
                });
                lo = lo.saturating_add(1);
                ln = ln.saturating_add(1);
            }
        }

        let total_len = diff_lines.len();
        let mut start;
        let mut end = total_len;
        if diff_lines.iter().all(|entry| entry.tag == ChangeTag::Equal) {
            start = end; // empty slice
        } else {
            let equal_before = diff_lines
                .iter()
                .take_while(|entry| entry.tag == ChangeTag::Equal)
                .count();
            let equal_after = diff_lines
                .iter()
                .rev()
                .take_while(|entry| entry.tag == ChangeTag::Equal)
                .count();
            start = equal_before.saturating_sub(MAX_CONTEXT);
            end = total_len.saturating_sub(equal_after.saturating_sub(MAX_CONTEXT));
        }

        while start < end {
            let entry = &diff_lines[start];
            if entry.tag == ChangeTag::Equal && entry.text.trim_ascii().is_empty() {
                start += 1;
            } else {
                break;
            }
        }
        while start < end {
            let entry = &diff_lines[end - 1];
            if entry.tag == ChangeTag::Equal && entry.text.trim_ascii().is_empty() {
                end -= 1;
            } else {
                break;
            }
        }

        if start < end {
            hunks.push(diff_lines[start..end].to_vec());
        }
    }
    hunks
}

/// Build diff hunks from full old/new text strings.
///
/// Simpler alternative to `build_diff_hunks` when you don't have structured
/// `SearchReplaceEditDetail` data — just the full before/after text.
/// Used for ACP `ToolCallContent::Diff` fallback (pre-execution previews).
pub fn diff_hunks_from_strings(old_text: &str, new_text: &str, start_line: usize) -> Vec<DiffHunk> {
    let detail = SearchReplaceEditDetail {
        old_string: old_text.to_owned(),
        old_line: start_line,
        new_string: new_text.to_owned(),
        new_line: start_line,
        context_before: String::new(),
        context_after: String::new(),
        line_prefix: String::new(),
    };
    build_diff_hunks(&[detail])
}

/// Stitch overlapping/adjacent hunks from coalesced same-file edits into
/// unified hunks.
///
/// Consecutive edits to nearby lines each carry ±context from their own file
/// snapshot, so a merged block's concatenated hunks repeat context lines and
/// re-show intermediate file states. Folding each hunk into the accumulated
/// previous one in `ln` (post-state) coordinates:
///
/// - a later edit of a shown context line swaps that Equal row for its
///   `-`/`+` pair;
/// - a line edited twice collapses to `-original +final` (no intermediate);
/// - repeated context is dropped; new trailing rows extend the hunk.
///
/// Anything the shared `ln` coordinates cannot describe truthfully bails to
/// the separate-hunk fallback (today's gap-marker rendering): non-monotonic
/// or non-adjacent pairs, text disagreement at a shared `ln` (line-count
/// drift between snapshots makes coordinates lie), and line-count-changing
/// shapes inside the covered range (pure deletes, unpaired inserts,
/// multi-line replacement runs). Never render wrong content.
///
/// Kept rows retain the `lo` of their own snapshot — the same convention the
/// unmerged per-hunk display already uses for its old-file column.
pub fn stitch_overlapping_hunks(hunks: Vec<DiffHunk>) -> Vec<DiffHunk> {
    let mut out: Vec<DiffHunk> = Vec::with_capacity(hunks.len());
    for hunk in hunks {
        if let Some(last) = out.last_mut()
            && let Some(stitched) = stitch_hunk_pair(last, &hunk)
        {
            *last = stitched;
            continue;
        }
        out.push(hunk);
    }
    out
}

/// Post-state (`ln`) coverage of a hunk's rendered rows (Equal/Insert).
fn render_range(hunk: &DiffHunk) -> Option<(usize, usize)> {
    let mut range: Option<(usize, usize)> = None;
    for line in hunk {
        if line.tag == ChangeTag::Delete {
            continue;
        }
        range = Some(match range {
            None => (line.ln, line.ln),
            Some((min, max)) => (min.min(line.ln), max.max(line.ln)),
        });
    }
    range
}

/// Row index in `hunk` rendering post-state line `ln` (Equal or Insert).
fn render_pos(hunk: &DiffHunk, ln: usize) -> Option<usize> {
    hunk.iter()
        .position(|l| l.tag != ChangeTag::Delete && l.ln == ln)
}

fn trimmed(text: &str) -> &str {
    text.trim_end_matches(['\r', '\n'])
}

/// Fold `b` into `a` when both describe one contiguous post-state region;
/// `None` keeps the pair as separate hunks.
fn stitch_hunk_pair(a: &DiffHunk, b: &DiffHunk) -> Option<DiffHunk> {
    let (a_min, a_max) = render_range(a)?;
    let (b_min, _) = render_range(b)?;
    if b_min < a_min || b_min > a_max + 1 {
        return None;
    }

    let mut out = a.clone();
    let mut max_ln = a_max;
    let mut i = 0;
    while i < b.len() {
        let row = &b[i];
        if row.ln > max_ln {
            // Past the stitched coverage: `b` is the sole source for this
            // tail, so splice its remaining rows in verbatim (rendered rows
            // must stay contiguous).
            for rest in &b[i..] {
                if rest.tag != ChangeTag::Delete {
                    if rest.ln != max_ln + 1 {
                        return None;
                    }
                    max_ln = rest.ln;
                }
                out.push(rest.clone());
            }
            break;
        }
        match row.tag {
            ChangeTag::Equal => {
                let pos = render_pos(&out, row.ln)?;
                if trimmed(&out[pos].text) != trimmed(&row.text) {
                    return None;
                }
                i += 1;
            }
            ChangeTag::Delete => {
                // Only single-line replacement pairs keep line counts (and
                // therefore every later `ln`) truthful.
                let next = b.get(i + 1)?;
                if next.tag != ChangeTag::Insert || next.ln != row.ln {
                    return None;
                }
                let pos = render_pos(&out, row.ln)?;
                if trimmed(&out[pos].text) != trimmed(&row.text) {
                    return None;
                }
                match out[pos].tag {
                    // A context line the later call edited: show its -/+ pair.
                    ChangeTag::Equal => {
                        out[pos] = row.clone();
                        out.insert(pos + 1, next.clone());
                    }
                    // Same line edited twice: keep the earlier delete (if
                    // any), drop the intermediate text, keep the final insert.
                    ChangeTag::Insert => {
                        out[pos] = next.clone();
                    }
                    ChangeTag::Delete => unreachable!("render_pos skips deletes"),
                }
                i += 2;
            }
            // Unpaired insert inside the covered range: line-count growth.
            ChangeTag::Insert => return None,
        }
    }
    Some(out)
}

/// Extract diff hunks from an ACP ToolCall's raw_output or content.
///
/// Tries three strategies in order:
/// 1. Parse `raw_output` as `SearchReplaceOutput::EditsApplied` for structured
///    per-edit hunks with context lines and accurate line numbers.
/// 2. Parse `Diff.meta` as `SearchReplaceEditContextInformation` for structured
///    edit details embedded in the Diff content block (set by acp_conversion).
/// 3. Fall back to `ToolCallContent::Diff` old_text/new_text for full-text diff,
///    using line numbers from `meta` when available (pre-execution previews).
///
/// Returns `(hunks, edit_count)`.
pub fn extract_edit_hunks(tc: &agent_client_protocol::ToolCall) -> (Vec<DiffHunk>, usize) {
    use echo_agent_tools::types::output::{
        SearchReplaceEditContextInformation, SearchReplaceOutput, ToolOutput,
    };

    // Strategy 1: structured edit details from raw_output (via ToolOutput wrapper)
    if let Some(raw) = &tc.raw_output {
        match serde_json::from_value::<ToolOutput>(raw.clone()) {
            Ok(ToolOutput::SearchReplace(SearchReplaceOutput::EditsApplied(edits))) => {
                let hunks = build_diff_hunks(&edits.edits.details);
                let count = hunks.len().max(1);
                return (hunks, count);
            }
            Err(e) => {
                tracing::warn!(
                    tool_call_id = %tc.tool_call_id.0,
                    error_kind = ?e.classify(),
                    "extract_edit_hunks: raw_output failed to deserialize as ToolOutput, \
                     falling back to Diff.meta"
                );
            }
            _ => {
                // raw_output is a different ToolOutput variant (not SearchReplace::EditsApplied)
            }
        }
    }

    // Strategy 2 & 3: ACP Diff content
    for content in &tc.content {
        if let agent_client_protocol::ToolCallContent::Diff(diff) = content {
            // Strategy 2: structured edit details from Diff.meta
            // acp_conversion embeds SearchReplaceEditContextInformation here.
            if let Some(meta) = &diff.meta
                && let Ok(edits) = serde_json::from_value::<SearchReplaceEditContextInformation>(
                    serde_json::Value::Object(meta.clone()),
                )
                && !edits.details.is_empty()
            {
                let hunks = build_diff_hunks(&edits.details);
                let count = hunks.len().max(1);
                return (hunks, count);
            }

            // Strategy 3: full-text diff from old_text / new_text.
            // Use line numbers from meta (pre-execution preview) when available,
            // otherwise default to 1.
            let start_line = diff
                .meta
                .as_ref()
                .and_then(|m| m.get("new_line"))
                .and_then(|v| v.as_u64())
                .map(|l| l as usize)
                .unwrap_or(1);
            let old = diff.old_text.as_deref().unwrap_or_default();
            let hunks = diff_hunks_from_strings(old, &diff.new_text, start_line);
            let count = hunks.len().max(1);
            return (hunks, count);
        }
    }

    (vec![], 1)
}

/// Generate a unified diff patch string from diff hunks.
///
/// Produces output suitable for `git apply` or clipboard sharing:
/// ```text
/// --- a/path/to/file
/// +++ b/path/to/file
/// @@ -old_start,old_count +new_start,new_count @@
///  context line
/// +added line
/// -removed line
/// ```
pub fn diff_hunks_to_patch(path: &str, hunks: &[DiffHunk]) -> String {
    if hunks.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    out.push_str(&format!("--- a/{path}\n"));
    out.push_str(&format!("+++ b/{path}\n"));

    for hunk in hunks {
        if hunk.is_empty() {
            continue;
        }

        // Compute hunk header: @@ -old_start,old_count +new_start,new_count @@
        let old_start = hunk
            .iter()
            .filter(|l| l.tag != ChangeTag::Insert)
            .map(|l| l.lo)
            .next()
            .unwrap_or(1);
        let new_start = hunk
            .iter()
            .filter(|l| l.tag != ChangeTag::Delete)
            .map(|l| l.ln)
            .next()
            .unwrap_or(1);
        let old_count = hunk.iter().filter(|l| l.tag != ChangeTag::Insert).count();
        let new_count = hunk.iter().filter(|l| l.tag != ChangeTag::Delete).count();

        out.push_str(&format!(
            "@@ -{old_start},{old_count} +{new_start},{new_count} @@\n"
        ));

        for line in hunk {
            let prefix = match line.tag {
                ChangeTag::Equal => ' ',
                ChangeTag::Insert => '+',
                ChangeTag::Delete => '-',
            };
            let text = line.text.trim_end_matches(['\r', '\n']);
            out.push(prefix);
            out.push_str(text);
            out.push('\n');
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use similar::ChangeTag;

    #[test]
    fn simple_replacement() {
        let details = vec![SearchReplaceEditDetail {
            old_string: "let x = 1;".to_string(),
            new_string: "let x = 2;".to_string(),
            old_line: 5,
            new_line: 5,
            context_before: "fn main() {\n".to_string(),
            context_after: "}".to_string(),
            line_prefix: String::new(),
        }];

        let hunks = build_diff_hunks(&details);
        assert_eq!(hunks.len(), 1);

        let hunk = &hunks[0];
        // Should have: context_before + delete + insert + context_after
        assert!(hunk.len() >= 3, "got {} lines", hunk.len());

        // Find the delete and insert lines
        let deletes: Vec<_> = hunk.iter().filter(|l| l.tag == ChangeTag::Delete).collect();
        let inserts: Vec<_> = hunk.iter().filter(|l| l.tag == ChangeTag::Insert).collect();
        assert_eq!(deletes.len(), 1);
        assert_eq!(inserts.len(), 1);
        assert!(deletes[0].text.contains("let x = 1;"));
        assert!(inserts[0].text.contains("let x = 2;"));
    }

    #[test]
    fn multiple_edits_produce_multiple_hunks() {
        let details = vec![
            SearchReplaceEditDetail {
                old_string: "foo".to_string(),
                new_string: "bar".to_string(),
                old_line: 1,
                new_line: 1,
                context_before: String::new(),
                context_after: String::new(),
                line_prefix: String::new(),
            },
            SearchReplaceEditDetail {
                old_string: "baz".to_string(),
                new_string: "qux".to_string(),
                old_line: 10,
                new_line: 10,
                context_before: String::new(),
                context_after: String::new(),
                line_prefix: String::new(),
            },
        ];

        let hunks = build_diff_hunks(&details);
        assert_eq!(hunks.len(), 2);
    }

    #[test]
    fn no_change_produces_no_hunks() {
        let details = vec![SearchReplaceEditDetail {
            old_string: "same".to_string(),
            new_string: "same".to_string(),
            old_line: 1,
            new_line: 1,
            context_before: String::new(),
            context_after: String::new(),
            line_prefix: String::new(),
        }];

        let hunks = build_diff_hunks(&details);
        assert_eq!(hunks.len(), 0, "identical text should produce no hunks");
    }

    #[test]
    fn context_lines_are_included() {
        let details = vec![SearchReplaceEditDetail {
            old_string: "old".to_string(),
            new_string: "new".to_string(),
            old_line: 5,
            new_line: 5,
            context_before: "line3\nline4\n".to_string(),
            context_after: "line6\nline7\n".to_string(),
            line_prefix: String::new(),
        }];

        let hunks = build_diff_hunks(&details);
        assert_eq!(hunks.len(), 1);

        let hunk = &hunks[0];
        let equal_lines: Vec<_> = hunk.iter().filter(|l| l.tag == ChangeTag::Equal).collect();
        // Should have context lines (before + after)
        assert!(
            equal_lines.len() >= 2,
            "expected context lines, got {}",
            equal_lines.len()
        );
    }

    #[test]
    fn line_numbers_are_correct() {
        let details = vec![SearchReplaceEditDetail {
            old_string: "old".to_string(),
            new_string: "new".to_string(),
            old_line: 10,
            new_line: 10,
            context_before: String::new(),
            context_after: String::new(),
            line_prefix: String::new(),
        }];

        let hunks = build_diff_hunks(&details);
        let hunk = &hunks[0];
        let delete = hunk.iter().find(|l| l.tag == ChangeTag::Delete).unwrap();
        assert_eq!(delete.lo, 10);
        let insert = hunk.iter().find(|l| l.tag == ChangeTag::Insert).unwrap();
        assert_eq!(insert.ln, 10);
    }

    #[test]
    fn context_before_line_numbers_precede_edit() {
        // Context lines should have line numbers *before* old_line, not at old_line.
        let details = vec![SearchReplaceEditDetail {
            old_string: "old".to_string(),
            new_string: "new".to_string(),
            old_line: 5,
            new_line: 5,
            context_before: "ctx1\nctx2".to_string(),
            context_after: String::new(),
            line_prefix: String::new(),
        }];

        let hunks = build_diff_hunks(&details);
        let hunk = &hunks[0];
        let ctx: Vec<_> = hunk.iter().filter(|l| l.tag == ChangeTag::Equal).collect();
        assert_eq!(ctx.len(), 2);
        assert_eq!(ctx[0].lo, 3); // old_line - 2
        assert_eq!(ctx[1].lo, 4); // old_line - 1
    }

    #[test]
    fn insert_after_no_duplicate_line_numbers() {
        // Simulates insert_after: old_string is empty, new_string has content.
        // old_line is set to the line after the anchor (where the insertion goes).
        let details = vec![SearchReplaceEditDetail {
            old_string: String::new(),
            new_string: "inserted line".to_string(),
            old_line: 5,
            new_line: 5,
            context_before: "anchor line".to_string(),
            context_after: "next line\n".to_string(),
            line_prefix: String::new(),
        }];

        let hunks = build_diff_hunks(&details);
        assert_eq!(hunks.len(), 1);
        let hunk = &hunks[0];

        // Collect all `ln` values that appear in the "new" column.
        let new_column: Vec<usize> = hunk
            .iter()
            .filter(|l| l.tag != ChangeTag::Delete)
            .map(|l| l.ln)
            .collect();

        // No new-column line number should repeat.
        for i in 1..new_column.len() {
            assert_ne!(
                new_column[i - 1],
                new_column[i],
                "duplicate new-line number {} at positions {} and {}",
                new_column[i],
                i - 1,
                i,
            );
        }

        // Context_before "anchor line" should be at ln = 4 (old_line - 1).
        let ctx: Vec<_> = hunk.iter().filter(|l| l.tag == ChangeTag::Equal).collect();
        assert_eq!(ctx[0].ln, 4, "context_before should be at new_line - 1");

        // Insert should be at ln = 5 (new_line).
        let ins: Vec<_> = hunk.iter().filter(|l| l.tag == ChangeTag::Insert).collect();
        assert_eq!(ins.len(), 1);
        assert_eq!(ins[0].ln, 5);
    }

    #[test]
    fn replace_in_place_line_numbers() {
        // Replace one line with one line (no net addition/removal).
        // old_line == new_line because the line count doesn't change.
        let details = vec![SearchReplaceEditDetail {
            old_string: "    let x = 1;".to_string(),
            new_string: "    let x = 42;".to_string(),
            old_line: 5,
            new_line: 5,
            context_before: "fn main() {\n    // setup".to_string(),
            context_after: "    let y = x + 1;\n}\n".to_string(),
            line_prefix: String::new(),
        }];

        let hunks = build_diff_hunks(&details);
        assert_eq!(hunks.len(), 1);
        let hunk = &hunks[0];

        // Expected layout:
        //   3 3  fn main() {       (context_before)
        //   4 4      // setup      (context_before)
        //   5        let x = 1;    (delete)
        //     5      let x = 42;   (insert)
        //   6 6      let y = x + 1; (context_after)
        //   7 7  }                 (context_after)

        // Context before: lines 3, 4 (old_line - 2, old_line - 1)
        let ctx_before: Vec<_> = hunk
            .iter()
            .take_while(|l| l.tag == ChangeTag::Equal)
            .collect();
        assert_eq!(ctx_before.len(), 2);
        assert_eq!(ctx_before[0].lo, 3);
        assert_eq!(ctx_before[0].ln, 3);
        assert_eq!(ctx_before[1].lo, 4);
        assert_eq!(ctx_before[1].ln, 4);

        // Delete: old line 5
        let del: Vec<_> = hunk.iter().filter(|l| l.tag == ChangeTag::Delete).collect();
        assert_eq!(del.len(), 1);
        assert_eq!(del[0].lo, 5);
        assert!(del[0].text.contains("let x = 1;"));

        // Insert: new line 5
        let ins: Vec<_> = hunk.iter().filter(|l| l.tag == ChangeTag::Insert).collect();
        assert_eq!(ins.len(), 1);
        assert_eq!(ins[0].ln, 5);
        assert!(ins[0].text.contains("let x = 42;"));

        // Context after: lines 6/6, 7/7
        let ctx_after: Vec<_> = hunk
            .iter()
            .rev()
            .take_while(|l| l.tag == ChangeTag::Equal)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>();
        assert_eq!(ctx_after.len(), 2);
        assert_eq!(ctx_after[0].lo, 6);
        assert_eq!(ctx_after[0].ln, 6);
        assert_eq!(ctx_after[1].lo, 7);
        assert_eq!(ctx_after[1].ln, 7);

        // Old column should be monotonically increasing (no duplicates).
        let old_column: Vec<usize> = hunk
            .iter()
            .filter(|l| l.tag != ChangeTag::Insert)
            .map(|l| l.lo)
            .collect();
        for i in 1..old_column.len() {
            assert!(
                old_column[i] > old_column[i - 1],
                "old column not monotonic: {:?}",
                old_column,
            );
        }

        // New column should be monotonically increasing (no duplicates).
        let new_column: Vec<usize> = hunk
            .iter()
            .filter(|l| l.tag != ChangeTag::Delete)
            .map(|l| l.ln)
            .collect();
        for i in 1..new_column.len() {
            assert!(
                new_column[i] > new_column[i - 1],
                "new column not monotonic: {:?}",
                new_column,
            );
        }
    }

    #[test]
    fn diff_hunks_from_strings_simple() {
        let hunks = diff_hunks_from_strings("hello\nworld\n", "hello\nearth\n", 1);
        assert_eq!(hunks.len(), 1);

        let hunk = &hunks[0];
        let deletes: Vec<_> = hunk.iter().filter(|l| l.tag == ChangeTag::Delete).collect();
        let inserts: Vec<_> = hunk.iter().filter(|l| l.tag == ChangeTag::Insert).collect();
        assert_eq!(deletes.len(), 1);
        assert_eq!(inserts.len(), 1);
        assert!(deletes[0].text.contains("world"));
        assert!(inserts[0].text.contains("earth"));
    }

    #[test]
    fn diff_hunks_from_strings_identical() {
        let hunks = diff_hunks_from_strings("same\n", "same\n", 1);
        assert_eq!(hunks.len(), 0);
    }

    #[test]
    fn diff_hunks_from_strings_empty_old() {
        // New file creation
        let hunks = diff_hunks_from_strings("", "new content\n", 1);
        assert_eq!(hunks.len(), 1);
        let inserts: Vec<_> = hunks[0]
            .iter()
            .filter(|l| l.tag == ChangeTag::Insert)
            .collect();
        assert!(!inserts.is_empty());
    }

    #[test]
    fn blank_line_insert_produces_visible_hunk() {
        // Simulates hashline insert_after with content: "" — both old and new are empty.
        let details = vec![SearchReplaceEditDetail {
            old_string: String::new(),
            new_string: String::new(),
            old_line: 4,
            new_line: 4,
            context_before: "    let y = 2;\n    let z = x + y;\n".to_string(),
            context_after: "    if z > 2 {\n        println!(\"big\");\n".to_string(),
            line_prefix: String::new(),
        }];

        let hunks = build_diff_hunks(&details);
        assert_eq!(
            hunks.len(),
            1,
            "blank line insert should produce a visible hunk"
        );

        let inserts: Vec<_> = hunks[0]
            .iter()
            .filter(|l| l.tag == ChangeTag::Insert)
            .collect();
        assert_eq!(
            inserts.len(),
            1,
            "should have exactly one inserted blank line"
        );
    }

    #[test]
    fn empty_file_write_produces_no_hunks() {
        // An empty new file (write with empty content, ACP Diff old/new both
        // empty, no context) must not ride the blank-line heuristic into a
        // fabricated +1 insertion — now prominent as the collapsed header's
        // diffstat.
        let hunks = diff_hunks_from_strings("", "", 1);
        assert!(hunks.is_empty(), "empty-to-empty must diff to nothing");
    }

    // ── Overlap stitching (coalesced same-file edits) ──────────────────

    /// An edit detail with the ±context the real search_replace tool emits
    /// from its own file snapshot.
    fn edit_detail(
        old: &str,
        new: &str,
        line: usize,
        before: &str,
        after: &str,
    ) -> SearchReplaceEditDetail {
        SearchReplaceEditDetail {
            old_string: old.to_string(),
            new_string: new.to_string(),
            old_line: line,
            new_line: line,
            context_before: before.to_string(),
            context_after: after.to_string(),
            line_prefix: String::new(),
        }
    }

    fn stitch_rows(hunk: &DiffHunk) -> Vec<(ChangeTag, usize, usize, &str)> {
        hunk.iter()
            .map(|l| (l.tag, l.lo, l.ln, l.text.trim_end()))
            .collect()
    }

    /// Session 019f646d repro: five sequential 1:1 edits on a 5-line file,
    /// each hunk carrying overlapping ±3 context from its own snapshot. The
    /// merged block must render ONE unified hunk of five -/+ pairs — no
    /// repeated context, no intermediate file states, no separators.
    #[test]
    fn stitch_five_sequential_full_line_edits_into_one_hunk() {
        let edits = [
            (
                "line one",
                "LINE ONE",
                1,
                "",
                "line two\nline three\nline four\n",
            ),
            (
                "line two",
                "LINE TWO",
                2,
                "LINE ONE\n",
                "line three\nline four\nline five\n",
            ),
            (
                "line three",
                "LINE THREE",
                3,
                "LINE ONE\nLINE TWO\n",
                "line four\nline five\n",
            ),
            (
                "line four",
                "LINE FOUR",
                4,
                "LINE ONE\nLINE TWO\nLINE THREE\n",
                "line five\n",
            ),
            (
                "line five",
                "LINE FIVE",
                5,
                "LINE TWO\nLINE THREE\nLINE FOUR\n",
                "",
            ),
        ];
        let hunks: Vec<DiffHunk> = edits
            .into_iter()
            .flat_map(|(old, new, line, before, after)| {
                build_diff_hunks(&[edit_detail(old, new, line, before, after)])
            })
            .collect();
        assert_eq!(hunks.len(), 5, "one overlapping hunk per edit");

        let stitched = stitch_overlapping_hunks(hunks);
        assert_eq!(stitched.len(), 1, "overlapping hunks stitch into one");
        assert_eq!(
            stitch_rows(&stitched[0]),
            vec![
                (ChangeTag::Delete, 1, 1, "line one"),
                (ChangeTag::Insert, 2, 1, "LINE ONE"),
                (ChangeTag::Delete, 2, 2, "line two"),
                (ChangeTag::Insert, 3, 2, "LINE TWO"),
                (ChangeTag::Delete, 3, 3, "line three"),
                (ChangeTag::Insert, 4, 3, "LINE THREE"),
                (ChangeTag::Delete, 4, 4, "line four"),
                (ChangeTag::Insert, 5, 4, "LINE FOUR"),
                (ChangeTag::Delete, 5, 5, "line five"),
                (ChangeTag::Insert, 6, 5, "LINE FIVE"),
            ]
        );
    }

    #[test]
    fn stitch_collapses_double_edit_to_original_and_final() {
        // a→b then b→c on the same line: the merged hunk shows -a +c only.
        let first = build_diff_hunks(&[edit_detail("a", "b", 1, "", "x\n")]);
        let second = build_diff_hunks(&[edit_detail("b", "c", 1, "", "x\n")]);

        let stitched = stitch_overlapping_hunks(vec![first[0].clone(), second[0].clone()]);
        assert_eq!(stitched.len(), 1);
        let rows: Vec<(ChangeTag, usize, &str)> = stitched[0]
            .iter()
            .map(|l| (l.tag, l.ln, l.text.trim_end()))
            .collect();
        assert_eq!(
            rows,
            vec![
                (ChangeTag::Delete, 1, "a"),
                (ChangeTag::Insert, 1, "c"),
                (ChangeTag::Equal, 2, "x"),
            ],
            "no intermediate state: the chained edit renders -original +final"
        );
    }

    #[test]
    fn stitch_bails_to_separate_hunks_on_content_disagreement() {
        let mk = |text: &str, lo: usize, ln: usize, tag: ChangeTag| DiffLine {
            text: text.to_string(),
            lo,
            ln,
            tag,
        };
        let a = vec![
            mk("alpha", 1, 1, ChangeTag::Equal),
            mk("beta", 2, 2, ChangeTag::Delete),
            mk("BETA", 3, 2, ChangeTag::Insert),
        ];
        // Overlapping `ln` range but conflicting text at ln 1: line-count
        // drift between snapshots — coordinates lie, so keep both hunks.
        let b = vec![
            mk("omega", 1, 1, ChangeTag::Equal),
            mk("gamma", 3, 3, ChangeTag::Delete),
            mk("GAMMA", 4, 3, ChangeTag::Insert),
        ];

        let stitched = stitch_overlapping_hunks(vec![a.clone(), b.clone()]);
        assert_eq!(stitched.len(), 2, "disagreement keeps hunks separate");
        assert_eq!(stitch_rows(&stitched[0]), stitch_rows(&a));
        assert_eq!(stitch_rows(&stitched[1]), stitch_rows(&b));
    }

    #[test]
    fn stitch_bails_to_separate_hunks_on_insert_only_overlap() {
        // "beta"→"BETA" at line 2, then an insert-only edit (the insert_after
        // shape: empty old_string) between BETA and gamma. The insertion
        // grows the line count, so every later `ln` in the first hunk would
        // lie — the unpaired-Insert arm must keep both hunks unmodified.
        let a = build_diff_hunks(&[edit_detail("beta", "BETA", 2, "alpha\n", "gamma\n")]);
        let b = build_diff_hunks(&[edit_detail("", "inserted", 3, "BETA\n", "gamma\n")]);
        let (a, b) = (a[0].clone(), b[0].clone());

        let stitched = stitch_overlapping_hunks(vec![a.clone(), b.clone()]);
        assert_eq!(stitched, vec![a, b], "insert-only overlap keeps both hunks");
    }

    #[test]
    fn stitch_bails_to_separate_hunks_on_delete_run_overlap() {
        // Delete-only edit inside the previous hunk's coverage: the pair rule
        // (Delete immediately followed by its same-`ln` Insert) declines, so
        // both hunks survive unmodified.
        let a = build_diff_hunks(&[edit_detail("alpha", "ALPHA", 1, "", "beta\ngamma\n")]);
        let b = build_diff_hunks(&[edit_detail("beta", "", 2, "ALPHA\n", "gamma\n")]);
        let (a, b) = (a[0].clone(), b[0].clone());

        let stitched = stitch_overlapping_hunks(vec![a.clone(), b.clone()]);
        assert_eq!(stitched, vec![a, b], "delete-only overlap keeps both hunks");

        // Same for a multi-line D,D,I,I replacement run (line-count neutral,
        // but not the single-line pair shape the stitcher trusts).
        let base = edit_detail("alpha", "ALPHA", 1, "", "beta\ngamma\ndelta\n");
        let multi = edit_detail("beta\ngamma", "BETA\nGAMMA", 2, "ALPHA\n", "delta\n");
        let a = build_diff_hunks(&[base])[0].clone();
        let b = build_diff_hunks(&[multi])[0].clone();

        let stitched = stitch_overlapping_hunks(vec![a.clone(), b.clone()]);
        assert_eq!(stitched, vec![a, b], "replacement run keeps both hunks");
    }

    #[test]
    fn stitch_keeps_disjoint_and_non_monotonic_hunks_separate() {
        let far = |line: usize| {
            diff_hunks_from_strings(&format!("old_{line}\n"), &format!("new_{line}\n"), line)
                .remove(0)
        };

        // Disjoint hunks keep today's gap-marker rendering.
        assert_eq!(stitch_overlapping_hunks(vec![far(5), far(40)]).len(), 2);
        // Later edit above the earlier one: non-monotonic, keep separate.
        assert_eq!(stitch_overlapping_hunks(vec![far(20), far(4)]).len(), 2);
    }

    #[test]
    fn context_lines_appear_in_hunks() {
        // Simulates hashline replace with ±3 context from to_search_replace.
        let details = vec![SearchReplaceEditDetail {
            old_string: "    let x = 1;".to_string(),
            new_string: "    let x = 42;".to_string(),
            old_line: 2,
            new_line: 2,
            context_before: "fn main() {\n".to_string(),
            context_after: "    let y = 2;\n    let z = x + y;\n".to_string(),
            line_prefix: String::new(),
        }];

        let hunks = build_diff_hunks(&details);
        assert_eq!(hunks.len(), 1);

        let hunk = &hunks[0];
        let equal_lines: Vec<_> = hunk.iter().filter(|l| l.tag == ChangeTag::Equal).collect();
        assert!(
            equal_lines.len() >= 2,
            "should have context lines, got {}",
            equal_lines.len()
        );
        assert!(
            equal_lines.iter().any(|l| l.text.contains("fn main")),
            "context_before should appear in hunk"
        );
        assert!(
            equal_lines.iter().any(|l| l.text.contains("let y = 2")),
            "context_after should appear in hunk"
        );
    }

    #[test]
    fn extract_edit_hunks_from_raw_output() {
        use agent_client_protocol as acp;
        use echo_agent_tools::types::output::{
            SearchReplaceEditContextInformation, SearchReplaceEditsApplied, SearchReplaceOutput,
            ToolOutput,
        };
        use std::sync::Arc;

        let edits_applied = SearchReplaceEditsApplied {
            old_string: "let x = 1;".to_string(),
            new_string: "let x = 2;".to_string(),
            tool_output_for_prompt: String::new(),
            tool_output_for_prompt_concise: None,
            absolute_path: "/tmp/test.rs".into(),
            edits: SearchReplaceEditContextInformation {
                details: vec![SearchReplaceEditDetail {
                    old_string: "let x = 1;".to_string(),
                    new_string: "let x = 2;".to_string(),
                    old_line: 5,
                    new_line: 5,
                    context_before: "fn main() {".to_string(),
                    context_after: "}".to_string(),
                    line_prefix: String::new(),
                }],
            },
            patch: None,
            unicode_normalized: false,
        };

        // Wrap in ToolOutput::SearchReplace — matches production rawOutput format
        let raw_output = serde_json::to_value(ToolOutput::SearchReplace(
            SearchReplaceOutput::EditsApplied(edits_applied),
        ))
        .unwrap();

        let tc = acp::ToolCall::new(
            acp::ToolCallId::new(Arc::from("tc1")),
            "Edit test.rs".to_string(),
        )
        .kind(acp::ToolKind::Edit)
        .status(acp::ToolCallStatus::Completed)
        .content(vec![])
        .raw_output(Some(raw_output))
        .locations(vec![]);

        let (hunks, count) = extract_edit_hunks(&tc);
        assert_eq!(hunks.len(), 1, "should have 1 hunk from raw_output");
        assert_eq!(count, 1);

        // Verify the hunk has correct content
        let deletes: Vec<_> = hunks[0]
            .iter()
            .filter(|l| l.tag == ChangeTag::Delete)
            .collect();
        assert_eq!(deletes.len(), 1);
        assert!(deletes[0].text.contains("let x = 1;"));
    }

    #[test]
    fn extract_edit_hunks_fallback_to_content_diff() {
        use agent_client_protocol as acp;
        use std::sync::Arc;

        let tc = acp::ToolCall::new(
            acp::ToolCallId::new(Arc::from("tc1")),
            "Edit test.rs".to_string(),
        )
        .kind(acp::ToolKind::Edit)
        .status(acp::ToolCallStatus::Completed)
        .content(vec![acp::ToolCallContent::Diff(
            acp::Diff::new("test.rs", "hello\nearth\n".to_string())
                .old_text(Some("hello\nworld\n".to_string())),
        )])
        .locations(vec![]);

        let (hunks, count) = extract_edit_hunks(&tc);
        assert_eq!(hunks.len(), 1, "should have 1 hunk from content diff");
        assert_eq!(count, 1);
    }

    #[test]
    fn extract_edit_hunks_empty_when_no_data() {
        use agent_client_protocol as acp;
        use std::sync::Arc;

        let tc = acp::ToolCall::new(
            acp::ToolCallId::new(Arc::from("tc1")),
            "Edit test.rs".to_string(),
        )
        .kind(acp::ToolKind::Edit)
        .status(acp::ToolCallStatus::Completed)
        .content(vec![])
        .locations(vec![]);

        let (hunks, count) = extract_edit_hunks(&tc);
        assert!(hunks.is_empty());
        assert_eq!(count, 1);
    }

    #[test]
    fn extract_edit_hunks_from_diff_meta_structured() {
        // Strategy 2: structured edit details from Diff.meta
        // (acp_conversion embeds SearchReplaceEditContextInformation).
        use agent_client_protocol as acp;
        use echo_agent_tools::types::output::SearchReplaceEditContextInformation;
        use std::sync::Arc;

        let edits = SearchReplaceEditContextInformation {
            details: vec![SearchReplaceEditDetail {
                old_string: "let x = 1;".to_string(),
                new_string: "let x = 2;".to_string(),
                old_line: 42,
                new_line: 42,
                context_before: "fn main() {".to_string(),
                context_after: "}".to_string(),
                line_prefix: String::new(),
            }],
        };

        // No raw_output — Strategy 1 skipped
        let tc = acp::ToolCall::new(
            acp::ToolCallId::new(Arc::from("tc1")),
            "Edit test.rs".to_string(),
        )
        .kind(acp::ToolKind::Edit)
        .status(acp::ToolCallStatus::Completed)
        .content(vec![acp::ToolCallContent::Diff(
            acp::Diff::new("test.rs", "let x = 2;".to_string())
                .old_text(Some("let x = 1;".to_string()))
                .meta(
                    serde_json::to_value(&edits)
                        .ok()
                        .and_then(|v| v.as_object().cloned()),
                ),
        )]);

        let (hunks, count) = extract_edit_hunks(&tc);
        assert_eq!(hunks.len(), 1);
        assert_eq!(count, 1);

        // Line numbers should be absolute (42), not relative (1).
        let del = hunks[0]
            .iter()
            .find(|l| l.tag == ChangeTag::Delete)
            .unwrap();
        assert_eq!(del.lo, 42, "old_line should be absolute");
        let ins = hunks[0]
            .iter()
            .find(|l| l.tag == ChangeTag::Insert)
            .unwrap();
        assert_eq!(ins.ln, 42, "new_line should be absolute");
    }

    #[test]
    fn extract_edit_hunks_from_diff_meta_start_line() {
        // Strategy 3: pre-execution preview with simple {old_line, new_line} meta.
        use agent_client_protocol as acp;
        use std::sync::Arc;

        let tc = acp::ToolCall::new(
            acp::ToolCallId::new(Arc::from("tc1")),
            "Edit test.rs".to_string(),
        )
        .kind(acp::ToolKind::Edit)
        .status(acp::ToolCallStatus::InProgress)
        .content(vec![acp::ToolCallContent::Diff(
            acp::Diff::new("test.rs", "new_val".to_string())
                .old_text(Some("old_val".to_string()))
                .meta(
                    serde_json::json!({"old_line": 50, "new_line": 50})
                        .as_object()
                        .cloned(),
                ),
        )])
        .locations(vec![]);

        let (hunks, count) = extract_edit_hunks(&tc);
        assert_eq!(hunks.len(), 1);
        assert_eq!(count, 1);

        // Line numbers should use start_line=50 from meta, not 1.
        let del = hunks[0]
            .iter()
            .find(|l| l.tag == ChangeTag::Delete)
            .unwrap();
        assert_eq!(del.lo, 50, "old_line should come from meta");
        let ins = hunks[0]
            .iter()
            .find(|l| l.tag == ChangeTag::Insert)
            .unwrap();
        assert_eq!(ins.ln, 50, "new_line should come from meta");
    }

    #[test]
    fn context_before_trailing_newline_no_phantom_line() {
        // When context_before ends with '\n', with_nl should not double it,
        // otherwise a phantom empty context line appears in the rendered diff.
        let details = vec![SearchReplaceEditDetail {
            old_string: String::new(),
            new_string: "    new_field: None,".to_string(),
            old_line: 5,
            new_line: 5,
            context_before: "    field_a: None,\n    field_b: Vec::new(),\n".to_string(),
            context_after: "}".to_string(),
            line_prefix: String::new(),
        }];

        let hunks = build_diff_hunks(&details);
        assert_eq!(hunks.len(), 1);
        let hunk = &hunks[0];

        // Context before should have exactly 2 lines, not 3 (no phantom blank).
        let ctx: Vec<_> = hunk
            .iter()
            .take_while(|l| l.tag == ChangeTag::Equal)
            .collect();
        assert_eq!(
            ctx.len(),
            2,
            "trailing newline in context_before should not create a phantom blank line, got: {:?}",
            ctx.iter().map(|l| &l.text).collect::<Vec<_>>()
        );

        // Verify no blank-only equal lines exist before the insert.
        let blank_ctx: Vec<_> = hunk
            .iter()
            .filter(|l| l.tag == ChangeTag::Equal && l.text.trim().is_empty())
            .collect();
        assert!(
            blank_ctx.is_empty(),
            "should have no blank context lines, got {} phantom lines",
            blank_ctx.len()
        );
    }

    #[test]
    fn line_prefix_prepended_to_changed_lines() {
        // Simulates a mid-line match: the file line is "            .filter(|t| old)"
        // but old_string is just ".filter(|t| old)" — the 12-space indent is the prefix.
        let details = vec![SearchReplaceEditDetail {
            old_string: ".filter(|t| old)".to_string(),
            new_string: ".filter(|t| new)".to_string(),
            old_line: 5,
            new_line: 5,
            context_before: "            .values()\n".to_string(),
            context_after: "            .count()\n".to_string(),
            line_prefix: "            ".to_string(), // 12 spaces
        }];

        let hunks = build_diff_hunks(&details);
        assert_eq!(hunks.len(), 1);
        let hunk = &hunks[0];

        let del: Vec<_> = hunk.iter().filter(|l| l.tag == ChangeTag::Delete).collect();
        let ins: Vec<_> = hunk.iter().filter(|l| l.tag == ChangeTag::Insert).collect();
        assert_eq!(del.len(), 1);
        assert_eq!(ins.len(), 1);

        // Both changed lines must start with the 12-space prefix.
        assert!(
            del[0].text.starts_with("            .filter"),
            "delete line should have leading indent, got: {:?}",
            del[0].text,
        );
        assert!(
            ins[0].text.starts_with("            .filter"),
            "insert line should have leading indent, got: {:?}",
            ins[0].text,
        );

        // Context lines already have their own indent (from the file).
        let ctx: Vec<_> = hunk.iter().filter(|l| l.tag == ChangeTag::Equal).collect();
        assert!(ctx.iter().all(|l| l.text.starts_with("            .")));
    }

    #[test]
    fn line_prefix_only_on_first_line_of_multiline_match() {
        // Multi-line old_string: only the first line should get the prefix.
        let details = vec![SearchReplaceEditDetail {
            old_string: "call_a()\n    call_b()\n".to_string(),
            new_string: "call_x()\n    call_y()\n".to_string(),
            old_line: 3,
            new_line: 3,
            context_before: "fn example() {\n".to_string(),
            context_after: "}\n".to_string(),
            line_prefix: "    ".to_string(), // 4 spaces
        }];

        let hunks = build_diff_hunks(&details);
        assert_eq!(hunks.len(), 1);
        let hunk = &hunks[0];

        let dels: Vec<_> = hunk.iter().filter(|l| l.tag == ChangeTag::Delete).collect();
        let inss: Vec<_> = hunk.iter().filter(|l| l.tag == ChangeTag::Insert).collect();
        assert_eq!(dels.len(), 2);
        assert_eq!(inss.len(), 2);

        // First delete/insert line: prefix applied.
        assert!(
            dels[0].text.starts_with("    call_a"),
            "first delete should have prefix, got: {:?}",
            dels[0].text,
        );
        assert!(
            inss[0].text.starts_with("    call_x"),
            "first insert should have prefix, got: {:?}",
            inss[0].text,
        );

        // Second delete/insert line: NO extra prefix (already a full file line).
        assert!(
            dels[1].text.starts_with("    call_b"),
            "second delete should keep original indent, got: {:?}",
            dels[1].text,
        );
        assert!(
            inss[1].text.starts_with("    call_y"),
            "second insert should keep original indent, got: {:?}",
            inss[1].text,
        );
    }

    #[test]
    fn empty_line_prefix_changes_nothing() {
        // When line_prefix is empty, behavior is unchanged from before.
        let details = vec![SearchReplaceEditDetail {
            old_string: "old_val".to_string(),
            new_string: "new_val".to_string(),
            old_line: 1,
            new_line: 1,
            context_before: String::new(),
            context_after: String::new(),
            line_prefix: String::new(),
        }];

        let hunks = build_diff_hunks(&details);
        assert_eq!(hunks.len(), 1);

        let del = hunks[0]
            .iter()
            .find(|l| l.tag == ChangeTag::Delete)
            .unwrap();
        let ins = hunks[0]
            .iter()
            .find(|l| l.tag == ChangeTag::Insert)
            .unwrap();
        assert!(del.text.starts_with("old_val"));
        assert!(ins.text.starts_with("new_val"));
    }
}
