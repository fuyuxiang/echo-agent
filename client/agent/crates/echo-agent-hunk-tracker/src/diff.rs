//! Diff computation using the `similar` crate.

use similar::{ChangeTag, TextDiff};
use std::fmt::Write;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, warn};

use crate::types::{Hunk, HunkId, HunkLineInfo, HunkSource};

/// Number of context lines to include around changes (like git diff).
const CONTEXT_LINES: usize = 3;

/// Maximum time allowed for a single diff computation.
const DIFF_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum file size (in bytes) to attempt diffing.
/// Files larger than this will be skipped to avoid pathological diff behavior.
const MAX_DIFF_FILE_SIZE: usize = 1024 * 1024; // 1 MB

/// Generate a unified diff patch string from baseline and current content.
/// This produces a patch that can be parsed by Pierre's `getSingularPatch`.
///
/// Returns None if:
/// - Content is identical
/// - Either file exceeds MAX_DIFF_FILE_SIZE
/// - Diff computation times out
pub fn generate_unified_patch(path: &Path, baseline: &str, current: &str) -> Option<String> {
    // If content is identical, no patch needed
    if baseline == current {
        return None;
    }

    // Check file size limits
    if baseline.len() > MAX_DIFF_FILE_SIZE || current.len() > MAX_DIFF_FILE_SIZE {
        warn!(
            path = %path.display(),
            baseline_size = baseline.len(),
            current_size = current.len(),
            max_size = MAX_DIFF_FILE_SIZE,
            "Skipping unified patch for file exceeding size limit"
        );
        return None;
    }

    let start_time = Instant::now();

    let diff = TextDiff::configure()
        .timeout(DIFF_TIMEOUT)
        .diff_lines(baseline, current);

    let elapsed = start_time.elapsed();
    if elapsed >= DIFF_TIMEOUT {
        warn!(
            path = %path.display(),
            elapsed_ms = elapsed.as_millis(),
            "Unified patch diff timed out"
        );
        return None;
    }

    // Generate unified diff with file headers
    let path_str = path.display().to_string();
    let unified = diff
        .unified_diff()
        .context_radius(CONTEXT_LINES)
        .header(&format!("a/{}", path_str), &format!("b/{}", path_str))
        .to_string();

    if unified.is_empty() {
        None
    } else {
        Some(unified)
    }
}

/// Generate a patch fragment for a single hunk with context lines.
/// Returns just the hunk portion (no file headers), e.g.:
/// "@@ -10,5 +10,7 @@\n context\n-old\n+new\n context\n"
pub fn generate_hunk_patch(baseline: &str, current: &str, hunk: &Hunk) -> String {
    let old_lines: Vec<&str> = baseline.lines().collect();
    let new_lines: Vec<&str> = current.lines().collect();

    let mut output = String::new();

    // Calculate context bounds (0-indexed)
    let old_start_idx = hunk.line_info.old_start.saturating_sub(1);
    let new_start_idx = hunk.line_info.new_start.saturating_sub(1);

    // Context before the change
    let context_before_start = old_start_idx.saturating_sub(CONTEXT_LINES);
    let context_before_end = old_start_idx;

    // Context after the change (in new file coordinates)
    let changes_end_new = new_start_idx + hunk.line_info.new_count;
    let context_after_start = changes_end_new;
    let context_after_end = (changes_end_new + CONTEXT_LINES).min(new_lines.len());

    // For old file, context after
    let changes_end_old = old_start_idx + hunk.line_info.old_count;
    let context_after_start_old = changes_end_old;
    let context_after_end_old = (changes_end_old + CONTEXT_LINES).min(old_lines.len());

    // Calculate total lines for header
    let total_old_lines = (context_before_end - context_before_start)
        + hunk.line_info.old_count
        + (context_after_end_old - context_after_start_old);
    let total_new_lines = (context_before_end - context_before_start)
        + hunk.line_info.new_count
        + (context_after_end - context_after_start);

    // Hunk header (1-indexed)
    let header_old_start = context_before_start + 1;
    let header_new_start = context_before_start + 1; // Context is same in both

    let _ = writeln!(
        output,
        "@@ -{},{} +{},{} @@",
        header_old_start, total_old_lines, header_new_start, total_new_lines
    );

    // Context lines before
    for i in context_before_start..context_before_end {
        if let Some(line) = old_lines.get(i) {
            let _ = writeln!(output, " {}", line);
        }
    }

    // Deleted lines
    if let Some(old_text) = &hunk.old_text {
        for line in old_text.lines() {
            let _ = writeln!(output, "-{}", line);
        }
    }

    // Added lines
    for line in hunk.new_text.lines() {
        let _ = writeln!(output, "+{}", line);
    }

    // Context lines after (from new file since changes may have shifted things)
    for i in context_after_start..context_after_end {
        if let Some(line) = new_lines.get(i) {
            let _ = writeln!(output, " {}", line);
        }
    }

    output
}

/// Compute hunks by diffing baseline against current content.
/// Uses the `similar` crate for line-based diff.
///
/// Returns an empty vector if:
/// - Content is identical (no changes)
/// - Either file exceeds MAX_DIFF_FILE_SIZE
/// - Diff computation times out
pub fn compute_hunks(path: &Path, baseline: &str, current: &str, source: HunkSource) -> Vec<Hunk> {
    // If content is identical, no hunks
    if baseline == current {
        return vec![];
    }

    // Check file size limits to avoid pathological diff behavior
    let baseline_size = baseline.len();
    let current_size = current.len();
    if baseline_size > MAX_DIFF_FILE_SIZE || current_size > MAX_DIFF_FILE_SIZE {
        warn!(
            path = %path.display(),
            baseline_size,
            current_size,
            max_size = MAX_DIFF_FILE_SIZE,
            "Skipping diff for file exceeding size limit"
        );
        return vec![];
    }

    let start_time = Instant::now();

    debug!(
        path = %path.display(),
        baseline_lines = baseline.lines().count(),
        current_lines = current.lines().count(),
        "Starting diff computation"
    );

    let diff = TextDiff::configure()
        .timeout(DIFF_TIMEOUT)
        .diff_lines(baseline, current);

    let elapsed = start_time.elapsed();

    // Check if we hit the timeout (similar crate returns partial results on timeout)
    if elapsed >= DIFF_TIMEOUT {
        warn!(
            path = %path.display(),
            elapsed_ms = elapsed.as_millis(),
            timeout_ms = DIFF_TIMEOUT.as_millis(),
            "Diff computation timed out, returning empty hunks"
        );
        return vec![];
    }

    debug!(
        path = %path.display(),
        elapsed_ms = elapsed.as_millis(),
        "Diff computation completed"
    );

    let mut hunks = Vec::new();

    // Track current position in old and new files (1-indexed for display)
    let mut old_line = 1usize;
    let mut new_line = 1usize;

    // Accumulator for current hunk being built
    let mut current_hunk: Option<HunkBuilder> = None;

    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Equal => {
                // Equal line - finalize any in-progress hunk
                if let Some(builder) = current_hunk.take() {
                    hunks.push(builder.build(path, source));
                }
                old_line += 1;
                new_line += 1;
            }
            ChangeTag::Delete => {
                // Line exists in old, not in new
                let hunk = current_hunk.get_or_insert_with(|| HunkBuilder::new(old_line, new_line));
                hunk.add_old_line(change.value());
                old_line += 1;
            }
            ChangeTag::Insert => {
                // Line exists in new, not in old
                let hunk = current_hunk.get_or_insert_with(|| HunkBuilder::new(old_line, new_line));
                hunk.add_new_line(change.value());
                new_line += 1;
            }
        }
    }

    // Finalize last hunk if any
    if let Some(builder) = current_hunk.take() {
        hunks.push(builder.build(path, source));
    }

    hunks
}

/// Helper to accumulate lines while building a hunk.
struct HunkBuilder {
    old_start: usize,
    new_start: usize,
    old_lines: Vec<String>,
    new_lines: Vec<String>,
}

impl HunkBuilder {
    fn new(old_start: usize, new_start: usize) -> Self {
        Self {
            old_start,
            new_start,
            old_lines: Vec::new(),
            new_lines: Vec::new(),
        }
    }

    fn add_old_line(&mut self, line: &str) {
        self.old_lines.push(line.to_string());
    }

    fn add_new_line(&mut self, line: &str) {
        self.new_lines.push(line.to_string());
    }

    fn build(self, path: &Path, source: HunkSource) -> Hunk {
        let old_text = if self.old_lines.is_empty() {
            None
        } else {
            Some(self.old_lines.join(""))
        };

        let new_text = self.new_lines.join("");

        Hunk {
            id: HunkId::new(),
            path: path.to_path_buf(),
            line_info: HunkLineInfo {
                old_start: self.old_start,
                old_count: self.old_lines.len(),
                new_start: self.new_start,
                new_count: self.new_lines.len(),
            },
            source,
            old_text,
            new_text,
            patch: None, // Patch is generated later when requested
            created_at: chrono::Utc::now(),
            selected: false,
        }
    }
}

/// Generate a unified diff string for display.
pub fn format_unified_diff(hunk: &Hunk) -> String {
    let mut output = String::new();

    // Header
    let _ = writeln!(output, "--- a/{}", hunk.path.display());
    let _ = writeln!(output, "+++ b/{}", hunk.path.display());

    // Hunk header
    let _ = writeln!(output, "{}", hunk.line_info);

    // Content
    if let Some(old_text) = &hunk.old_text {
        for line in old_text.lines() {
            let _ = writeln!(output, "-{}", line);
        }
    }
    for line in hunk.new_text.lines() {
        let _ = writeln!(output, "+{}", line);
    }

    output
}

/// Replace lines in content starting at `start_line` (1-indexed),
/// removing `remove_count` lines and inserting `insert_text`.
///
/// # Arguments
/// * `content` - The full file content to patch
/// * `start_line` - 1-indexed line number where patch begins
/// * `remove_count` - Number of lines to remove (can be 0 for pure insert)
/// * `insert_text` - Text to insert (can be empty for pure delete)
///
/// # Returns
/// The patched content
pub fn patch_lines(
    content: &str,
    start_line: usize,
    remove_count: usize,
    insert_text: &str,
) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let start_idx = start_line.saturating_sub(1); // Convert to 0-indexed

    let mut result = Vec::new();

    // Lines before the patch point
    result.extend(lines[..start_idx.min(lines.len())].iter().copied());

    // Insert new lines (if any)
    if !insert_text.is_empty() {
        for line in insert_text.lines() {
            result.push(line);
        }
    }

    // Lines after the removed section
    let end_idx = (start_idx + remove_count).min(lines.len());
    result.extend(lines[end_idx..].iter().copied());

    // Reconstruct with proper trailing newline handling
    let mut output = result.join("\n");
    if content.ends_with('\n') && !output.is_empty() {
        output.push('\n');
    }
    output
}

/// Compare two hunks to see if they represent the same logical change
/// (content match, possibly at different positions).
pub fn hunks_match_content(a: &Hunk, b: &Hunk) -> bool {
    a.path == b.path && a.old_text == b.old_text && a.new_text == b.new_text
}

/// Check if a hunk has moved (same content, different position).
pub fn hunk_moved(old: &Hunk, new: &Hunk) -> bool {
    hunks_match_content(old, new) && old.line_info != new.line_info
}

/// Check if two hunks overlap by line range in the baseline (old) file.
/// Uses old_start/old_count for stable overlap detection even when file shifts.
/// Used for determining when hunks should be merged or matched.
pub fn hunks_overlap(a: &Hunk, b: &Hunk) -> bool {
    if a.path != b.path {
        return false;
    }

    // Use old_start/old_count (baseline-relative) for stable overlap detection
    let a_start = a.line_info.old_start;
    let a_end = a.line_info.old_start.saturating_add(a.line_info.old_count);
    let b_start = b.line_info.old_start;
    let b_end = b.line_info.old_start.saturating_add(b.line_info.old_count);

    // For pure insertions (old_count=0), consider adjacent positions as overlapping
    if a.line_info.old_count == 0 && b.line_info.old_count == 0 {
        // Two insertions at the same baseline position overlap
        return a_start == b_start;
    }

    // Handle insertions overlapping with regular hunks:
    // An insertion at position X overlaps with a hunk spanning [start, end) if start <= X <= end
    if a.line_info.old_count == 0 {
        // a is an insertion at a_start
        return a_start >= b_start && a_start <= b_end;
    }
    if b.line_info.old_count == 0 {
        // b is an insertion at b_start
        return b_start >= a_start && b_start <= a_end;
    }

    // Overlaps if NOT (a ends before b starts OR b ends before a starts)
    // Include adjacent (touching) hunks as overlapping
    !(a_end < b_start || b_end < a_start)
}

/// Find the best matching old hunk for a new hunk.
/// Priority: 1) exact content + position match, 2) content match closest by line, 3) maximum overlap size
pub fn find_matching_old_hunk<'a>(
    new_hunk: &Hunk,
    old_hunks: &'a [Arc<Hunk>],
) -> Option<&'a Arc<Hunk>> {
    // Collect all content matches
    let content_matches: Vec<_> = old_hunks
        .iter()
        .filter(|o| hunks_match_content(o, new_hunk))
        .collect();

    if !content_matches.is_empty() {
        // If we have content matches, pick the one closest by line position
        // This handles the case of identical changes at multiple locations (e.g., variable rename)
        return content_matches
            .into_iter()
            .min_by_key(|o| o.line_info.new_start.abs_diff(new_hunk.line_info.new_start));
    }

    // Fall back to BEST overlapping hunk (max overlap size)
    old_hunks
        .iter()
        .filter(|o| hunks_overlap(o, new_hunk))
        .max_by_key(|o| calculate_overlap_size(&o.line_info, &new_hunk.line_info))
}

/// Calculate the overlap size between two hunks (in baseline lines)
fn calculate_overlap_size(a: &HunkLineInfo, b: &HunkLineInfo) -> usize {
    let a_start = a.old_start;
    let a_end = a.old_start + a.old_count;
    let b_start = b.old_start;
    let b_end = b.old_start + b.old_count;

    let overlap_start = a_start.max(b_start);
    let overlap_end = a_end.min(b_end);

    overlap_end.saturating_sub(overlap_start)
}

/// Find all old hunks that overlap with a new hunk (for merging).
pub fn find_overlapping_hunks<'a>(
    new_hunk: &Hunk,
    old_hunks: &'a [Arc<Hunk>],
) -> Vec<&'a Arc<Hunk>> {
    old_hunks
        .iter()
        .filter(|o| hunks_overlap(o, new_hunk))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent_source() -> HunkSource {
        HunkSource::AgentEdit { prompt_index: 0 }
    }

    #[test]
    fn test_no_changes() {
        let content = "line 1\nline 2\nline 3\n";
        let hunks = compute_hunks(Path::new("test.rs"), content, content, agent_source());
        assert!(hunks.is_empty());
    }

    #[test]
    fn test_single_line_modification() {
        let baseline = "line 1\nline 2\nline 3\n";
        let current = "line 1\nmodified\nline 3\n";
        let hunks = compute_hunks(Path::new("test.rs"), baseline, current, agent_source());

        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].old_text, Some("line 2\n".to_string()));
        assert_eq!(hunks[0].new_text, "modified\n");
        assert_eq!(hunks[0].line_info.old_start, 2);
        assert_eq!(hunks[0].line_info.new_start, 2);
    }

    #[test]
    fn test_insertion() {
        let baseline = "line 1\nline 2\n";
        let current = "line 1\ninserted\nline 2\n";
        let hunks = compute_hunks(Path::new("test.rs"), baseline, current, agent_source());

        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].old_text, None);
        assert_eq!(hunks[0].new_text, "inserted\n");
        assert_eq!(hunks[0].line_info.old_count, 0);
        assert_eq!(hunks[0].line_info.new_count, 1);
    }

    #[test]
    fn test_deletion() {
        let baseline = "line 1\nline 2\nline 3\n";
        let current = "line 1\nline 3\n";
        let hunks = compute_hunks(Path::new("test.rs"), baseline, current, agent_source());

        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].old_text, Some("line 2\n".to_string()));
        assert_eq!(hunks[0].new_text, "");
        assert_eq!(hunks[0].line_info.old_count, 1);
        assert_eq!(hunks[0].line_info.new_count, 0);
    }

    #[test]
    fn test_multiple_hunks() {
        let baseline = "line 1\nline 2\nline 3\nline 4\nline 5\n";
        let current = "modified 1\nline 2\nline 3\nline 4\nmodified 5\n";
        let hunks = compute_hunks(Path::new("test.rs"), baseline, current, agent_source());

        assert_eq!(hunks.len(), 2);
        assert_eq!(hunks[0].line_info.old_start, 1);
        assert_eq!(hunks[1].line_info.old_start, 5);
    }

    #[test]
    fn test_format_unified_diff() {
        let hunk = Hunk {
            id: HunkId::new(),
            path: "test.rs".into(),
            line_info: HunkLineInfo {
                old_start: 2,
                old_count: 1,
                new_start: 2,
                new_count: 1,
            },
            source: agent_source(),
            old_text: Some("old line\n".to_string()),
            new_text: "new line\n".to_string(),
            patch: None,
            created_at: chrono::Utc::now(),
            selected: false,
        };

        let diff = format_unified_diff(&hunk);
        assert!(diff.contains("--- a/test.rs"));
        assert!(diff.contains("+++ b/test.rs"));
        assert!(diff.contains("-old line"));
        assert!(diff.contains("+new line"));
    }

    #[test]
    fn test_find_matching_hunk_with_identical_content_at_different_positions() {
        // Simulate a variable rename that appears at multiple locations
        // Old hunks at lines 10 and 100 with identical content
        let old_hunk_at_10 = Arc::new(Hunk {
            id: HunkId::from_string("hunk-10".to_string()),
            path: "test.rs".into(),
            line_info: HunkLineInfo {
                old_start: 10,
                old_count: 1,
                new_start: 10,
                new_count: 1,
            },
            source: agent_source(),
            old_text: Some("let a = b;\n".to_string()),
            new_text: "let c = b;\n".to_string(),
            patch: None,
            created_at: chrono::Utc::now(),
            selected: false,
        });

        let old_hunk_at_100 = Arc::new(Hunk {
            id: HunkId::from_string("hunk-100".to_string()),
            path: "test.rs".into(),
            line_info: HunkLineInfo {
                old_start: 100,
                old_count: 1,
                new_start: 100,
                new_count: 1,
            },
            source: agent_source(),
            old_text: Some("let a = b;\n".to_string()),
            new_text: "let c = b;\n".to_string(),
            patch: None,
            created_at: chrono::Utc::now(),
            selected: false,
        });

        let old_hunks = vec![old_hunk_at_10.clone(), old_hunk_at_100.clone()];

        // New hunk at line 10 should match old hunk at line 10
        let new_hunk_near_10 = Hunk {
            id: HunkId::new(),
            path: "test.rs".into(),
            line_info: HunkLineInfo {
                old_start: 10,
                old_count: 1,
                new_start: 12, // slightly shifted
                new_count: 1,
            },
            source: agent_source(),
            old_text: Some("let a = b;\n".to_string()),
            new_text: "let c = b;\n".to_string(),
            patch: None,
            created_at: chrono::Utc::now(),
            selected: false,
        };

        let matched = find_matching_old_hunk(&new_hunk_near_10, &old_hunks);
        assert!(matched.is_some());
        assert_eq!(matched.unwrap().id.as_str(), "hunk-10");

        // New hunk at line 100 should match old hunk at line 100
        let new_hunk_near_100 = Hunk {
            id: HunkId::new(),
            path: "test.rs".into(),
            line_info: HunkLineInfo {
                old_start: 100,
                old_count: 1,
                new_start: 102, // slightly shifted
                new_count: 1,
            },
            source: agent_source(),
            old_text: Some("let a = b;\n".to_string()),
            new_text: "let c = b;\n".to_string(),
            patch: None,
            created_at: chrono::Utc::now(),
            selected: false,
        };

        let matched = find_matching_old_hunk(&new_hunk_near_100, &old_hunks);
        assert!(matched.is_some());
        assert_eq!(matched.unwrap().id.as_str(), "hunk-100");
    }

    #[test]
    fn test_find_matching_hunk_fallback_best_overlap() {
        // This test figures out the edge case mentioned: when a new hunk overlaps
        // *multiple* old hunks (and no content match, so fallback), the current
        // .find() picks the *first* overlapping one -- order-dependent, can preserve
        // wrong hunk ID/source.
        //
        // We use different overlap sizes so "best" (max overlap) is unambiguous.
        // With current code, this test FAILS (picks "small" because it's first).
        // After fix to use max overlap, it should PASS (picks "large").

        // Old hunks with no content match to new_hunk, ordered small-first
        let old_hunk_small = Arc::new(Hunk {
            id: HunkId::from_string("hunk-small".to_string()),
            path: "test.rs".into(),
            line_info: HunkLineInfo {
                old_start: 1,
                old_count: 1,
                new_start: 1,
                new_count: 2, // covers new lines 1-2
            },
            source: agent_source(),
            old_text: Some("old-small\n".to_string()),
            new_text: "new-small\n".to_string(),
            patch: None,
            created_at: chrono::Utc::now(),
            selected: false,
        });

        let old_hunk_large = Arc::new(Hunk {
            id: HunkId::from_string("hunk-large".to_string()),
            path: "test.rs".into(),
            line_info: HunkLineInfo {
                old_start: 3,
                old_count: 1,
                new_start: 3,
                new_count: 4, // covers new lines 3-6
            },
            source: agent_source(),
            old_text: Some("old-large\n".to_string()),
            new_text: "new-large\n".to_string(),
            patch: None,
            created_at: chrono::Utc::now(),
            selected: false,
        });

        let old_hunks = vec![old_hunk_small.clone(), old_hunk_large.clone()]; // small first!

        // New hunk overlaps both, but more with large:
        // new lines 2-5 (end=6)
        // - small: overlap lines 2 (size=1)
        // - large: overlap lines 3-5 (size=3)
        // Content differs -> no content match -> fallback to overlap
        let new_hunk = Hunk {
            id: HunkId::new(),
            path: "test.rs".into(),
            line_info: HunkLineInfo {
                old_start: 2,
                old_count: 4,
                new_start: 2,
                new_count: 4, // lines 2-5
            },
            source: agent_source(),
            old_text: Some("different-old\n".to_string()),
            new_text: "different-new\n".to_string(),
            patch: None,
            created_at: chrono::Utc::now(),
            selected: false,
        };

        let matched = find_matching_old_hunk(&new_hunk, &old_hunks);
        assert!(matched.is_some(), "Should find an overlapping hunk");

        // EXPECTS BEST MATCH: large overlap, NOT the first one
        // (this currently FAILS with .find(), proving the bug)
        assert_eq!(
            matched.unwrap().id.as_str(),
            "hunk-large",
            "Should pick hunk with largest overlap size, not first in list"
        );
    }

    #[test]
    fn test_patch_lines_basic() {
        let content = "line 1\nline 2\nline 3\nline 4\nline 5\n";

        // Replace line 2 with "CHANGED"
        let patched = super::patch_lines(content, 2, 1, "CHANGED\n");
        assert_eq!(patched, "line 1\nCHANGED\nline 3\nline 4\nline 5\n");
    }

    #[test]
    fn test_patch_lines_no_trailing_newline_in_insert() {
        let content = "line 1\nline 2\nline 3\n";

        // Replace line 2 with "CHANGED" (no trailing newline in insert text)
        let patched = super::patch_lines(content, 2, 1, "CHANGED");
        assert_eq!(patched, "line 1\nCHANGED\nline 3\n");
    }

    #[test]
    fn test_patch_lines_pure_insert() {
        let content = "line 1\nline 2\nline 3\n";

        // Insert at line 2 without removing anything
        let patched = super::patch_lines(content, 2, 0, "INSERTED\n");
        assert_eq!(patched, "line 1\nINSERTED\nline 2\nline 3\n");
    }

    #[test]
    fn test_patch_lines_pure_delete() {
        let content = "line 1\nline 2\nline 3\n";

        // Delete line 2 without inserting anything
        let patched = super::patch_lines(content, 2, 1, "");
        assert_eq!(patched, "line 1\nline 3\n");
    }

    #[test]
    fn test_patch_lines_multiple_lines() {
        let content = "line 1\nline 2\nline 3\nline 4\nline 5\n";

        // Replace lines 2-3 with 2 new lines
        let patched = super::patch_lines(content, 2, 2, "NEW A\nNEW B\n");
        assert_eq!(patched, "line 1\nNEW A\nNEW B\nline 4\nline 5\n");
    }

    #[test]
    fn test_generate_hunk_patch_includes_context_and_headers() {
        let baseline = "line 1\nline 2\nline 3\nline 4\nline 5\n";
        let current = "line 1\nline 2\nchanged line 3\nline 4\nline 5\n";

        let hunk = Hunk {
            id: HunkId::new(),
            path: "test.rs".into(),
            line_info: HunkLineInfo {
                old_start: 3,
                old_count: 1,
                new_start: 3,
                new_count: 1,
            },
            source: agent_source(),
            old_text: Some("line 3\n".to_string()),
            new_text: "changed line 3\n".to_string(),
            patch: None,
            created_at: chrono::Utc::now(),
            selected: false,
        };

        let patch = generate_hunk_patch(baseline, current, &hunk);

        assert!(patch.starts_with("@@ -1,5 +1,5 @@"));
        assert!(patch.contains(" line 1"));
        assert!(patch.contains(" line 2"));
        assert!(patch.contains("-line 3"));
        assert!(patch.contains("+changed line 3"));
        assert!(patch.contains(" line 4"));
        assert!(patch.contains(" line 5"));
    }

    #[test]
    fn test_generate_hunk_patch_multiple_hunks_with_add_and_delete() {
        let baseline = "line 1\nline 2\nline 3\nline 4\nline 5\nline 6\nline 7\n";
        let current = "line 1\nline 2\nadded line 2a\nline 3\nline 5\nline 6\nline 7\n";

        let hunks = compute_hunks(Path::new("test.rs"), baseline, current, agent_source());
        assert_eq!(hunks.len(), 2, "Should have one add and one delete hunk");

        let add_hunk = hunks
            .iter()
            .find(|h| h.old_text.is_none())
            .expect("Add hunk should exist");
        let delete_hunk = hunks
            .iter()
            .find(|h| h.new_text.is_empty())
            .expect("Delete hunk should exist");

        let add_patch = generate_hunk_patch(baseline, current, add_hunk);
        assert!(add_patch.contains("+added line 2a"));
        assert!(!add_patch.contains("-line 2"));

        let delete_patch = generate_hunk_patch(baseline, current, delete_hunk);
        assert!(delete_patch.contains("-line 4"));
        assert!(!delete_patch.contains("+line 4"));
    }

    #[test]
    fn test_compute_hunks_after_accept_simulation() {
        // Simulate what happens after accepting one hunk and diffing
        // This simulates the scenario in test_sequential_accepts_preserve_remaining_hunks

        // Patched baseline (after accepting HUNK_A at line 2)
        let patched_baseline = "line 1\nHUNK_A\nline 3\nline 4\nline 5\nline 6\nline 7\nline 8\nline 9\nline 10\nline 11\nline 12\n";

        // Current content (has all 3 changes)
        let current = "line 1\nHUNK_A\nline 3\nline 4\nline 5\nline 6\nHUNK_B\nline 8\nline 9\nline 10\nHUNK_C\nline 12\n";

        let hunks = compute_hunks(
            Path::new("test.rs"),
            patched_baseline,
            current,
            agent_source(),
        );

        // Should produce 2 hunks: one at line 7, one at line 11
        assert_eq!(
            hunks.len(),
            2,
            "Should produce 2 hunks after accepting first one"
        );

        // Verify the hunks are at the expected positions
        assert_eq!(
            hunks[0].line_info.old_start, 7,
            "First hunk should be at line 7"
        );
        assert_eq!(hunks[0].new_text, "HUNK_B\n", "First hunk should be HUNK_B");

        assert_eq!(
            hunks[1].line_info.old_start, 11,
            "Second hunk should be at line 11"
        );
        assert_eq!(
            hunks[1].new_text, "HUNK_C\n",
            "Second hunk should be HUNK_C"
        );
    }
}
