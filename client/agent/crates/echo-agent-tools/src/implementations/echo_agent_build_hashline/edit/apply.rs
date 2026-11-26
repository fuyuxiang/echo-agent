//! Core edit-application logic for `hashline_edit`.
//!
//! Validates anchors against the pre-edit file snapshot, detects overlapping
//! edits, sorts operations bottom-up, and applies them. Returns a fresh-anchor
//! snippet of the edited region.

use std::path::Path;

use super::range_policy;
use super::types::*;
use crate::implementations::echo_agent_build_hashline::anchor::split_lines;
use crate::implementations::echo_agent_build_hashline::read_file::format_hashline_content;
use crate::implementations::echo_agent_build_hashline::scheme::{
    Anchor, AnchorScheme, DEFAULT_SEARCH_RADIUS, ParsedAnchor, ShiftResult, ValidationResult,
};

const SNIPPET_CONTEXT: usize = 3;

/// Generate a scheme-appropriate format label and example anchor for error messages.
///
/// Probes the scheme with a single-line sample to determine whether it uses
/// a context hash (e.g. `"22:abc:rst"`) or only a local hash (e.g. `"22:abc"`).
fn anchor_format_hint(scheme: &dyn AnchorScheme) -> (&'static str, String) {
    let len = scheme.hash_len().clamp(1, 4);
    let hash = &"abcd"[..len];
    let has_context = scheme
        .generate_anchors(&["x"])
        .first()
        .is_some_and(|a| a.context.is_some());
    if has_context {
        let ctx = &"rstu"[..len];
        ("LINE:HASH1:HASH2", format!("22:{hash}:{ctx}"))
    } else {
        ("LINE:HASH", format!("22:{hash}"))
    }
}

/// Format an anchor's local+context as `"local:ctx"` or `"local"`.
pub(crate) fn anchor_suffix(a: &Anchor) -> String {
    match &a.context {
        Some(ctx) => format!("{}:{ctx}", a.local),
        None => a.local.clone(),
    }
}

/// Check whether any line in `content` starts with an anchor prefix
/// (e.g. `"22:abc:rst\u{2192}..."` or `"axy:edj->..."`).
/// Returns the first offending line (1-based) if found.
fn detect_anchor_prefix_in_content(content: &str) -> Option<usize> {
    for (idx, line) in content.lines().enumerate() {
        let s = line.trim_start();
        if let Some((before, _)) = s.split_once('\u{2192}')
            && before.len() <= 25
            && before.contains(':')
            && !before.contains(' ')
        {
            return Some(idx + 1);
        }
        if let Some((before, _)) = s.split_once("->")
            && before.len() <= 25
            && before.contains(':')
            && !before.contains(' ')
        {
            return Some(idx + 1);
        }
    }
    None
}

fn anchor_content_error(op_label: &str, content: &str, line_num: usize) -> HashlineEditError {
    let offending_line = content.lines().nth(line_num - 1).unwrap_or("").to_owned();

    // Build a small context snippet (up to 3 lines around the offending line).
    let lines: Vec<&str> = content.lines().collect();
    let ctx_start = line_num.saturating_sub(1).saturating_sub(1); // 1 line before (0-based)
    let ctx_end = (line_num + 1).min(lines.len()); // 1 line after
    let context: String = (ctx_start..ctx_end)
        .map(|i| {
            let marker = if i + 1 == line_num { ">>>" } else { "   " };
            format!("{marker} line {}: {}", i + 1, lines[i])
        })
        .collect::<Vec<_>>()
        .join("\n");

    HashlineEditError {
        error: HashlineEditErrorKind::InvalidInput,
        message: format!(
            "{op_label} content contains anchor prefixes (e.g. \"22:abc:rst\u{2192}\") \
             copied from hashline_read output. The first offending line is line {line_num}. \
             Strip the anchor prefixes and the \u{2192} separator from every line, \
             keeping only the actual file content, then retry."
        ),
        requested_anchor: None,
        current: Some(offending_line),
        context: Some(context),
        context_start_line: Some(ctx_start + 1),
        shifted_to: None,
        shifted_anchor: None,
        ambiguous_candidates: vec![],
    }
}

/// Format `"LINE:SUFFIX→CONTENT"`.
fn render_anchored_line(a: &Anchor, content: &str) -> String {
    format!("{}:{}→{content}", a.line, anchor_suffix(a))
}

/// A validated, resolved edit operation ready for application.
/// All line indices are 0-based.
#[derive(Debug)]
struct ResolvedOp {
    /// Original index in the input batch (for stable ordering).
    original_idx: usize,
    /// Start line (0-based, inclusive).
    start: usize,
    /// End line (0-based, exclusive). For insert_after, start == end (insertion point).
    end: usize,
    /// Replacement lines (empty = delete).
    new_lines: Vec<String>,
}

/// Result of `apply_edits`: the output to return to the caller, plus the new
/// file content on success (to be written to disk by the tool layer).
pub(crate) struct ApplyResult {
    /// The structured output (success or error).
    pub output: HashlineEditOutput,
    /// The new file content string. `Some` only when `output` is
    /// `EditsApplied`; `None` on error.
    pub new_content: Option<String>,
    /// Per-edit region details for diff metadata. Empty on error or whole-file write.
    pub edit_details: Vec<EditRegionDetail>,
}

/// Per-edit old/new content and line numbers for diff metadata.
pub(crate) struct EditRegionDetail {
    /// 1-based line in the old file.
    pub old_line: usize,
    pub old_text: String,
    /// 1-based line in the new file (accounts for prior insertions/deletions).
    pub new_line: usize,
    pub new_text: String,
}
/// Apply a batch of hashline edit operations to file content.
///
/// Validates all anchors against `content` before applying any edits.
/// Returns both the structured output and the new file content (if
/// successful), so the caller can write to disk without re-deriving the
/// content through a separate code path.
pub(crate) fn apply_edits(
    content: &str,
    ops: &[HashlineOp],
    file_path: &Path,
    scheme: &dyn AnchorScheme,
) -> ApplyResult {
    let lines = split_lines(content);

    if ops.len() == 1
        && let HashlineOp::Write {
            content: new_content,
        } = &ops[0]
    {
        if let Some(line_num) = detect_anchor_prefix_in_content(new_content) {
            return ApplyResult {
                output: HashlineEditOutput::Error(anchor_content_error(
                    "write",
                    new_content,
                    line_num,
                )),
                new_content: None,
                edit_details: vec![],
            };
        }
        return ApplyResult {
            output: build_write_result(new_content, file_path, scheme),
            new_content: Some(new_content.clone()),
            edit_details: vec![],
        };
    }

    let mut resolved: Vec<ResolvedOp> = Vec::with_capacity(ops.len());

    for (idx, op) in ops.iter().enumerate() {
        match resolve_op(op, idx, &lines, scheme) {
            Ok(r) => resolved.push(r),
            Err(mut e) => {
                if ops.len() > 1 {
                    let op_label = match op {
                        HashlineOp::Replace { .. } => "replace",
                        HashlineOp::InsertAfter { .. } => "insert_after",
                        HashlineOp::Write { .. } => "write",
                    };
                    e.message = format!(
                        "Edit {}/{} ({op_label}): {}\n\n\
                         This batch contained {} edits. \
                         Because this anchor failed validation, \
                         none of the edits were applied. \
                         Retry all {} edits with fresh anchors, \
                         not just the failed one.",
                        idx + 1,
                        ops.len(),
                        e.message,
                        ops.len(),
                        ops.len(),
                    );
                }
                return ApplyResult {
                    output: HashlineEditOutput::Error(e),
                    new_content: None,
                    edit_details: vec![],
                };
            }
        }
    }

    if let Some(mut err) = check_overlaps(&resolved) {
        if ops.len() > 1 {
            err.message = format!(
                "{}\n\nThis batch contained {} edits. \
                 Because of the overlap, none were applied. \
                 Fix the overlapping ranges and retry all edits.",
                err.message,
                ops.len(),
            );
        }
        return ApplyResult {
            output: HashlineEditOutput::Error(err),
            new_content: None,
            edit_details: vec![],
        };
    }

    let mut warnings: Vec<String> = Vec::new();
    for op in &resolved {
        if let Some(w) = range_policy::range_warning(op.start, op.end) {
            warnings.push(w);
        }
    }

    // Bottom-up + reverse-original-idx: preserves request order for same-position ops.
    resolved.sort_by(|a, b| {
        b.start
            .cmp(&a.start)
            .then(b.original_idx.cmp(&a.original_idx))
    });

    let mut result_lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();

    // Collect each edit's affected region (0-based, pre-splice coordinates).
    // We record post-splice positions by tracking cumulative line-count shifts.
    let mut edit_regions: Vec<(usize, usize)> = Vec::with_capacity(resolved.len());
    let mut edit_details: Vec<EditRegionDetail> = Vec::with_capacity(resolved.len());
    let mut cumulative_shift: isize = 0;

    // Ops are sorted bottom-up for splicing — iterate in reverse to get top-down
    // order for region tracking.
    for op in resolved.iter().rev() {
        let shifted_start = (op.start as isize + cumulative_shift) as usize;
        let replaced = op.end - op.start;
        let inserted = op.new_lines.len();
        let shifted_end = shifted_start + inserted;
        edit_regions.push((shifted_start, shifted_end));

        let old_text = if op.start < op.end {
            lines[op.start..op.end].join("\n")
        } else {
            String::new()
        };
        let new_text = op.new_lines.join("\n");
        edit_details.push(EditRegionDetail {
            old_line: op.start + 1,
            old_text,
            new_line: shifted_start + 1,
            new_text,
        });

        cumulative_shift += inserted as isize - replaced as isize;
    }

    for op in &resolved {
        result_lines.splice(op.start..op.end, op.new_lines.iter().cloned());
    }

    let new_content = result_lines.join("\n");
    let total_new_lines = split_lines(&new_content).len();

    // Sort edit regions top-down and merge nearby ones.
    edit_regions.sort_by_key(|r| r.0);
    let snippet = build_snippet(&new_content, &edit_regions, total_new_lines, scheme);
    let snippet_start_line = edit_regions
        .first()
        .map(|r| r.0.saturating_sub(SNIPPET_CONTEXT) + 1)
        .unwrap_or(1);

    ApplyResult {
        output: HashlineEditOutput::EditsApplied(HashlineEditsApplied {
            applied: ops.len(),
            scheme: scheme.name().to_owned(),
            snippet_start_line,
            snippet,
            absolute_path: file_path.to_path_buf(),
            warnings,
        }),
        new_content: Some(new_content),
        edit_details,
    }
}

/// Maximum total snippet lines before switching to per-region snippets.
/// When the contiguous range from first to last edit exceeds this, we show
/// individual ±SNIPPET_CONTEXT windows separated by `... N lines not shown ...`.
const MAX_CONTIGUOUS_SNIPPET: usize = 80;

/// Build the snippet output for a batch of edits.
///
/// If all edits fall within `MAX_CONTIGUOUS_SNIPPET` lines of each other,
/// returns a single contiguous snippet. Otherwise, returns per-edit-region
/// snippets separated by gap markers.
fn build_snippet(
    new_content: &str,
    edit_regions: &[(usize, usize)],
    total_new_lines: usize,
    scheme: &dyn AnchorScheme,
) -> String {
    if edit_regions.is_empty() {
        return String::new();
    }

    let global_start = edit_regions
        .first()
        .unwrap()
        .0
        .saturating_sub(SNIPPET_CONTEXT);
    let global_end = edit_regions
        .last()
        .map(|r| r.1 + SNIPPET_CONTEXT)
        .unwrap()
        .min(total_new_lines);

    // If the span is small enough, emit one contiguous snippet (original behavior).
    if global_end - global_start <= MAX_CONTIGUOUS_SNIPPET {
        let (snippet, _raw) = format_hashline_content(
            new_content,
            Some(global_start + 1),
            Some(global_end - global_start),
            scheme,
        );
        return snippet;
    }

    // Merge overlapping/adjacent regions (with context).
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for &(start, end) in edit_regions {
        let ctx_start = start.saturating_sub(SNIPPET_CONTEXT);
        let ctx_end = (end + SNIPPET_CONTEXT).min(total_new_lines);
        if let Some(last) = merged.last_mut()
            && ctx_start <= last.1
        {
            last.1 = last.1.max(ctx_end);
            continue;
        }
        merged.push((ctx_start, ctx_end));
    }

    // Build per-region snippets separated by gap markers.
    let mut parts: Vec<String> = Vec::new();
    let mut prev_end: usize = 0;

    for (i, &(start, end)) in merged.iter().enumerate() {
        if i > 0 {
            let gap = start.saturating_sub(prev_end);
            parts.push(format!("... {gap} lines not shown ..."));
        } else if start > 0 {
            parts.push(format!("... {start} lines not shown ..."));
        }

        let (region_snippet, _raw) =
            format_hashline_content(new_content, Some(start + 1), Some(end - start), scheme);
        parts.push(region_snippet);
        prev_end = end;
    }

    if prev_end < total_new_lines {
        let remaining = total_new_lines - prev_end;
        parts.push(format!("... {remaining} lines not shown ..."));
    }

    parts.join("\n")
}
/// Resolve a single `HashlineOp` into a `ResolvedOp`, validating anchors.
fn resolve_op(
    op: &HashlineOp,
    original_idx: usize,
    lines: &[&str],
    scheme: &dyn AnchorScheme,
) -> Result<ResolvedOp, HashlineEditError> {
    match op {
        HashlineOp::Replace {
            anchor,
            end_anchor,
            content,
        } => {
            let start = validate_anchor(anchor, lines, scheme)?;
            let end = match end_anchor {
                Some(ea) => {
                    let e = validate_anchor(ea, lines, scheme)?;
                    if e < start {
                        return Err(HashlineEditError {
                            error: HashlineEditErrorKind::InvalidInput,
                            message: format!(
                                "end_anchor line {} is before start anchor line {}.",
                                e + 1,
                                start + 1
                            ),
                            requested_anchor: Some(ea.clone()),
                            current: None,
                            context: None,
                            context_start_line: None,
                            shifted_to: None,
                            shifted_anchor: None,
                            ambiguous_candidates: vec![],
                        });
                    }
                    e + 1 // exclusive end
                }
                None => start + 1, // single line
            };

            if let Some(line_num) = detect_anchor_prefix_in_content(content) {
                return Err(anchor_content_error("replace", content, line_num));
            }
            let new_lines: Vec<String> = if content.is_empty() {
                vec![] // delete
            } else {
                content.lines().map(|l| l.to_owned()).collect()
            };

            Ok(ResolvedOp {
                original_idx,
                start,
                end,
                new_lines,
            })
        }

        HashlineOp::InsertAfter { anchor, content } => {
            let insert_at = if anchor == "0:" {
                0
            } else if anchor == "EOF" {
                // Insert at the actual end of file content. If the file ends
                // with '\n', split_lines produces a synthetic trailing empty
                // line — insert before it rather than after it.
                let len = lines.len();
                if len > 1 && lines[len - 1].is_empty() {
                    len - 1
                } else {
                    len
                }
            } else {
                let line = validate_anchor(anchor, lines, scheme)?;
                line + 1
            };

            if let Some(line_num) = detect_anchor_prefix_in_content(content) {
                return Err(anchor_content_error("insert_after", content, line_num));
            }
            let new_lines: Vec<String> = if content.is_empty() {
                vec![String::new()] // blank line
            } else {
                content.lines().map(|l| l.to_owned()).collect()
            };

            Ok(ResolvedOp {
                original_idx,
                start: insert_at,
                end: insert_at, // insertion: start == end
                new_lines,
            })
        }

        HashlineOp::Write { .. } => {
            // Write ops should be handled before reaching here.
            Err(HashlineEditError {
                error: HashlineEditErrorKind::InvalidInput,
                message: "Write op must be the only operation in a batch. \
                         Either use write alone (to replace the entire file) or use \
                         replace/insert_after ops without write."
                    .to_owned(),
                requested_anchor: None,
                current: None,
                context: None,
                context_start_line: None,
                shifted_to: None,
                shifted_anchor: None,
                ambiguous_candidates: vec![],
            })
        }
    }
}

/// Try to recover a `ParsedAnchor` from a hash-only string like `"ab:cd"` (no line number).
/// Generates anchors for the file and returns `Some` only if exactly one line's
/// suffix matches, avoiding ambiguity.
fn recover_anchor_by_suffix(
    suffix: &str,
    lines: &[&str],
    scheme: &dyn AnchorScheme,
) -> Option<ParsedAnchor> {
    let anchors = scheme.generate_anchors(lines);
    let matches: Vec<_> = anchors
        .iter()
        .filter(|a| match (&a.context, suffix.split_once(':')) {
            (Some(ctx), Some((local, sfx_ctx))) => a.local == local && ctx.as_str() == sfx_ctx,
            (None, None) => a.local == suffix,
            _ => false,
        })
        .collect();
    if matches.len() == 1 {
        let a = matches[0];
        Some(ParsedAnchor {
            line: a.line,
            local: a.local.clone(),
            context: a.context.clone(),
        })
    } else {
        None
    }
}

/// Validate an anchor string against file content.
/// Returns the 0-based line index on success, or a structured error.
fn validate_anchor(
    anchor_str: &str,
    lines: &[&str],
    scheme: &dyn AnchorScheme,
) -> Result<usize, HashlineEditError> {
    // Strip trailing arrow + content that the model copies from hashline_read
    // output (e.g. `22:abc:rst→code` or `22:abc:rst->code`).
    let anchor_str = anchor_str
        .split_once('\u{2192}')
        .or_else(|| anchor_str.split_once("->"))
        .map_or(anchor_str, |(pre, _)| pre);

    let parsed = match ParsedAnchor::parse(anchor_str) {
        Some(p) => p,
        None => {
            // Recovery: the model sometimes drops the line number, sending
            // just "ab:cd" instead of "22:ab:cd". Try matching the hash suffix
            // against generated anchors — accept if exactly one line matches.
            if let Some(recovered) = recover_anchor_by_suffix(anchor_str, lines, scheme) {
                tracing::debug!(
                    anchor = anchor_str,
                    recovered_line = recovered.line,
                    "recovered malformed anchor by suffix match"
                );
                recovered
            } else {
                let (fmt, ex) = anchor_format_hint(scheme);
                return Err(HashlineEditError {
                    error: HashlineEditErrorKind::InvalidInput,
                    message: format!(
                        "Malformed anchor: \"{anchor_str}\". Expected format: \"{fmt}\" (e.g. \"{ex}\")."
                    ),
                    requested_anchor: Some(anchor_str.to_owned()),
                    current: None,
                    context: None,
                    context_start_line: None,
                    shifted_to: None,
                    shifted_anchor: None,
                    ambiguous_candidates: vec![],
                });
            }
        }
    };

    let result = scheme.validate(&parsed, lines);

    match result {
        ValidationResult::Valid => Ok(parsed.line - 1), // 0-based

        ValidationResult::OutOfRange => Err(HashlineEditError {
            error: HashlineEditErrorKind::AnchorNotFound,
            message: format!(
                "Line {} is out of range (file has {} lines).",
                parsed.line,
                lines.len()
            ),
            requested_anchor: Some(anchor_str.to_owned()),
            current: None,
            context: None,
            context_start_line: None,
            shifted_to: None,
            shifted_anchor: None,
            ambiguous_candidates: vec![],
        }),

        ValidationResult::Stale => {
            let shift = scheme.find_shifted(&parsed, lines, DEFAULT_SEARCH_RADIUS);
            let anchors = scheme.generate_anchors(lines);

            // Wider context for recovery (±5 lines).
            let recovery_ctx = 5;
            let ctx_start = parsed.line.saturating_sub(1).saturating_sub(recovery_ctx);
            let ctx_end = (parsed.line + recovery_ctx).min(lines.len());

            let context: String = (ctx_start..ctx_end)
                .map(|i| render_anchored_line(&anchors[i], lines[i]))
                .collect::<Vec<_>>()
                .join("\n");

            let idx = parsed.line.saturating_sub(1);
            let current =
                (idx < lines.len()).then(|| render_anchored_line(&anchors[idx], lines[idx]));

            let (shifted_to, shifted_anchor, ambiguous_candidates, error_kind, message) =
                match shift {
                    ShiftResult::Found { new_line } => {
                        let fresh =
                            format!("{}:{}", new_line, anchor_suffix(&anchors[new_line - 1]));
                        let msg = format!(
                            "Anchor stale at line {}. Content appears to have shifted to line {new_line}. \
                             Retry with anchor \"{fresh}\".",
                            parsed.line
                        );
                        (
                            Some(new_line),
                            Some(fresh),
                            vec![],
                            HashlineEditErrorKind::AnchorStale,
                            msg,
                        )
                    }
                    ShiftResult::Ambiguous { candidates } => {
                        let msg = format!(
                            "Anchor stale at line {}. Multiple candidates at lines {:?}. \
                             Use the fresh anchors from the context below to retry your edit.",
                            parsed.line, candidates,
                        );
                        (
                            None,
                            None,
                            candidates,
                            HashlineEditErrorKind::AmbiguousAnchor,
                            msg,
                        )
                    }
                    ShiftResult::NotFound => {
                        let msg = format!(
                            "Anchor stale at line {}. Use the fresh anchors from the context below to retry your edit.",
                            parsed.line,
                        );
                        (None, None, vec![], HashlineEditErrorKind::AnchorStale, msg)
                    }
                };

            Err(HashlineEditError {
                error: error_kind,
                message,
                requested_anchor: Some(anchor_str.to_owned()),
                current,
                context: Some(context),
                context_start_line: Some(ctx_start + 1),
                shifted_to,
                shifted_anchor,
                ambiguous_candidates,
            })
        }
    }
}

fn check_overlaps(ops: &[ResolvedOp]) -> Option<HashlineEditError> {
    let mut ranges: Vec<(usize, usize, usize)> = ops
        .iter()
        .filter(|op| op.start != op.end)
        .map(|op| (op.start, op.end, op.original_idx))
        .collect();
    ranges.sort_by_key(|r| r.0);

    // Replacement vs replacement overlap.
    for window in ranges.windows(2) {
        if window[0].1 > window[1].0 {
            return Some(overlap_error(
                window[0].0,
                window[0].1,
                window[0].2,
                window[1].0,
                window[1].1,
                window[1].2,
            ));
        }
    }

    // Insertion vs replacement overlap: reject if the insertion point falls
    // strictly inside a replacement span (start <= insert_at < end).
    for op in ops {
        if op.start != op.end {
            continue; // not an insertion
        }
        let insert_at = op.start;
        for &(rs, re, r_idx) in &ranges {
            if rs <= insert_at && insert_at < re {
                return Some(overlap_error(
                    rs,
                    re,
                    r_idx,
                    insert_at,
                    insert_at,
                    op.original_idx,
                ));
            }
        }
    }

    None
}

fn overlap_error(
    a_start: usize,
    a_end: usize,
    a_idx: usize,
    b_start: usize,
    b_end: usize,
    b_idx: usize,
) -> HashlineEditError {
    let a_desc = if a_start == a_end {
        format!("edit #{} (insertion at line {})", a_idx + 1, a_start + 1)
    } else {
        format!("edit #{} (lines {}-{})", a_idx + 1, a_start + 1, a_end)
    };
    let b_desc = if b_start == b_end {
        format!("edit #{} (insertion at line {})", b_idx + 1, b_start + 1)
    } else {
        format!("edit #{} (lines {}-{})", b_idx + 1, b_start + 1, b_end)
    };
    HashlineEditError {
        error: HashlineEditErrorKind::OverlappingEdits,
        message: format!("Overlapping edits: {a_desc} and {b_desc}."),
        requested_anchor: None,
        current: None,
        context: None,
        context_start_line: None,
        shifted_to: None,
        shifted_anchor: None,
        ambiguous_candidates: vec![],
    }
}

fn build_write_result(
    new_content: &str,
    file_path: &Path,
    scheme: &dyn AnchorScheme,
) -> HashlineEditOutput {
    let total = split_lines(new_content).len();
    let snippet_end = (SNIPPET_CONTEXT * 2).min(total);
    let (snippet, _raw) = format_hashline_content(new_content, Some(1), Some(snippet_end), scheme);

    HashlineEditOutput::EditsApplied(HashlineEditsApplied {
        applied: 1,
        scheme: scheme.name().to_owned(),
        snippet_start_line: 1,
        snippet,
        absolute_path: file_path.to_path_buf(),
        warnings: vec![],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_path() -> PathBuf {
        PathBuf::from("/tmp/test.rs")
    }

    use crate::implementations::echo_agent_build_hashline::config::HashlineSchemeParams;
    use crate::implementations::echo_agent_build_hashline::scheme::{
        ChunkFingerprint, ContentOnly,
    };

    const SAMPLE: &str =
        "fn main() {\n    let x = 1;\n    let y = 2;\n    println!(\"{x} {y}\");\n}\n";

    fn test_scheme() -> Box<dyn AnchorScheme> {
        HashlineSchemeParams::default().build_scheme().unwrap()
    }

    fn anchors_for(content: &str) -> Vec<String> {
        let scheme = test_scheme();
        let lines = split_lines(content);
        scheme
            .generate_anchors(&lines)
            .iter()
            .map(|a| format!("{}:{}", a.line, anchor_suffix(a)))
            .collect()
    }

    #[test]
    fn point_replace() {
        let anchors = anchors_for(SAMPLE);
        let ops = vec![HashlineOp::Replace {
            anchor: anchors[1].clone(), // "    let x = 1;"
            end_anchor: None,
            content: "    let x = 999;".to_owned(),
        }];

        match apply_edits(SAMPLE, &ops, &test_path(), &*test_scheme()).output {
            HashlineEditOutput::EditsApplied(result) => {
                assert_eq!(result.applied, 1);
                assert!(result.snippet.contains("999"));
                assert!(!result.snippet.contains("let x = 1"));
            }
            HashlineEditOutput::Error(e) => panic!("Expected success, got error: {}", e.message),
        }
    }

    #[test]
    fn delete_via_empty_content() {
        let anchors = anchors_for(SAMPLE);
        let ops = vec![HashlineOp::Replace {
            anchor: anchors[1].clone(),
            end_anchor: None,
            content: String::new(), // delete
        }];

        match apply_edits(SAMPLE, &ops, &test_path(), &*test_scheme()).output {
            HashlineEditOutput::EditsApplied(result) => {
                assert_eq!(result.applied, 1);
                // Deleted line should not appear in snippet.
                assert!(!result.snippet.contains("let x = 1"));
            }
            HashlineEditOutput::Error(e) => panic!("Expected success, got error: {}", e.message),
        }
    }

    #[test]
    fn range_replace() {
        let anchors = anchors_for(SAMPLE);
        let ops = vec![HashlineOp::Replace {
            anchor: anchors[1].clone(),           // "    let x = 1;"
            end_anchor: Some(anchors[2].clone()), // "    let y = 2;"
            content: "    let z = 42;".to_owned(),
        }];

        match apply_edits(SAMPLE, &ops, &test_path(), &*test_scheme()).output {
            HashlineEditOutput::EditsApplied(result) => {
                assert_eq!(result.applied, 1);
                assert!(result.snippet.contains("42"));
            }
            HashlineEditOutput::Error(e) => panic!("Expected success, got error: {}", e.message),
        }
    }

    #[test]
    fn insert_after_bof() {
        let ops = vec![HashlineOp::InsertAfter {
            anchor: "0:".to_owned(),
            content: "// header comment".to_owned(),
        }];

        match apply_edits(SAMPLE, &ops, &test_path(), &*test_scheme()).output {
            HashlineEditOutput::EditsApplied(result) => {
                assert_eq!(result.applied, 1);
                assert!(result.snippet.contains("header comment"));
            }
            HashlineEditOutput::Error(e) => panic!("Expected success, got error: {}", e.message),
        }
    }

    #[test]
    fn insert_after_eof() {
        let ops = vec![HashlineOp::InsertAfter {
            anchor: "EOF".to_owned(),
            content: "// footer".to_owned(),
        }];

        match apply_edits(SAMPLE, &ops, &test_path(), &*test_scheme()).output {
            HashlineEditOutput::EditsApplied(result) => {
                assert_eq!(result.applied, 1);
                assert!(result.snippet.contains("footer"));
            }
            HashlineEditOutput::Error(e) => panic!("Expected success, got error: {}", e.message),
        }
    }

    #[test]
    fn insert_after_anchor() {
        let anchors = anchors_for(SAMPLE);
        let ops = vec![HashlineOp::InsertAfter {
            anchor: anchors[1].clone(),
            content: "    let z = 3;".to_owned(),
        }];

        match apply_edits(SAMPLE, &ops, &test_path(), &*test_scheme()).output {
            HashlineEditOutput::EditsApplied(result) => {
                assert_eq!(result.applied, 1);
                assert!(result.snippet.contains("let z = 3"));
            }
            HashlineEditOutput::Error(e) => panic!("Expected success, got error: {}", e.message),
        }
    }

    #[test]
    fn write_replaces_entire_file() {
        let ops = vec![HashlineOp::Write {
            content: "new content\n".to_owned(),
        }];

        match apply_edits(SAMPLE, &ops, &test_path(), &*test_scheme()).output {
            HashlineEditOutput::EditsApplied(result) => {
                assert_eq!(result.applied, 1);
                assert!(result.snippet.contains("new content"));
            }
            HashlineEditOutput::Error(e) => panic!("Expected success, got error: {}", e.message),
        }
    }

    #[test]
    fn batch_ordering_bottom_up() {
        let anchors = anchors_for(SAMPLE);
        // Two non-overlapping replacements at lines 2 and 4.
        let ops = vec![
            HashlineOp::Replace {
                anchor: anchors[1].clone(), // line 2
                end_anchor: None,
                content: "    let x = 100;".to_owned(),
            },
            HashlineOp::Replace {
                anchor: anchors[3].clone(), // line 4
                end_anchor: None,
                content: "    println!(\"changed\");".to_owned(),
            },
        ];

        match apply_edits(SAMPLE, &ops, &test_path(), &*test_scheme()).output {
            HashlineEditOutput::EditsApplied(result) => {
                assert_eq!(result.applied, 2);
                assert!(result.snippet.contains("100"));
                assert!(result.snippet.contains("changed"));
            }
            HashlineEditOutput::Error(e) => panic!("Expected success, got error: {}", e.message),
        }
    }

    #[test]
    fn scattered_batch_produces_per_region_snippets() {
        // Build a file large enough that edits at opposite ends exceed MAX_CONTIGUOUS_SNIPPET.
        let line_count = 200;
        let mut file_lines: Vec<String> = (0..line_count).map(|i| format!("line_{i}")).collect();
        file_lines.push(String::new()); // trailing newline
        let content = file_lines.join("\n");

        let anchors = anchors_for(&content);

        // Edit lines near the top (line 5) and near the bottom (line 195).
        let ops = vec![
            HashlineOp::Replace {
                anchor: anchors[4].clone(),
                end_anchor: None,
                content: "REPLACED_TOP".to_owned(),
            },
            HashlineOp::Replace {
                anchor: anchors[194].clone(),
                end_anchor: None,
                content: "REPLACED_BOTTOM".to_owned(),
            },
        ];

        match apply_edits(&content, &ops, &test_path(), &*test_scheme()).output {
            HashlineEditOutput::EditsApplied(result) => {
                assert!(
                    result.snippet.contains("REPLACED_TOP"),
                    "Snippet should contain the top edit"
                );
                assert!(
                    result.snippet.contains("REPLACED_BOTTOM"),
                    "Snippet should contain the bottom edit"
                );
                assert!(
                    result.snippet.contains("lines not shown"),
                    "Snippet should have gap markers between distant edits, got:\n{}",
                    &result.snippet[..result.snippet.len().min(500)]
                );
                // The snippet should be MUCH smaller than the full span.
                let snippet_lines = result.snippet.lines().count();
                assert!(
                    snippet_lines < 30,
                    "Snippet should be compact (~14 lines of context + gaps), got {snippet_lines} lines"
                );
            }
            HashlineEditOutput::Error(e) => panic!("Expected success: {}", e.message),
        }
    }

    #[test]
    fn nearby_batch_produces_contiguous_snippet() {
        // Edits close together should still produce a single contiguous snippet.
        let anchors = anchors_for(SAMPLE);
        let ops = vec![
            HashlineOp::Replace {
                anchor: anchors[1].clone(), // line 2
                end_anchor: None,
                content: "    let x = 99;".to_owned(),
            },
            HashlineOp::Replace {
                anchor: anchors[3].clone(), // line 4
                end_anchor: None,
                content: "    println!(\"hi\");".to_owned(),
            },
        ];

        match apply_edits(SAMPLE, &ops, &test_path(), &*test_scheme()).output {
            HashlineEditOutput::EditsApplied(result) => {
                assert!(
                    !result.snippet.contains("lines not shown"),
                    "Nearby edits should produce a contiguous snippet without gaps"
                );
                assert!(result.snippet.contains("99"));
                assert!(result.snippet.contains("hi"));
            }
            HashlineEditOutput::Error(e) => panic!("Expected success: {}", e.message),
        }
    }

    #[test]
    fn scattered_mixed_ops_delete_insert_replace() {
        // Verify snippet generation handles all edit types when scattered.
        let line_count = 200;
        let mut file_lines: Vec<String> = (0..line_count).map(|i| format!("line_{i}")).collect();
        file_lines.push(String::new());
        let content = file_lines.join("\n");
        let anchors = anchors_for(&content);

        let ops = vec![
            // Delete near the top.
            HashlineOp::Replace {
                anchor: anchors[5].clone(),
                end_anchor: None,
                content: String::new(),
            },
            // Insert in the middle.
            HashlineOp::InsertAfter {
                anchor: anchors[100].clone(),
                content: "INSERTED_A\nINSERTED_B".to_owned(),
            },
            // Replace near the bottom.
            HashlineOp::Replace {
                anchor: anchors[190].clone(),
                end_anchor: None,
                content: "REPLACED_BOTTOM".to_owned(),
            },
        ];

        match apply_edits(&content, &ops, &test_path(), &*test_scheme()).output {
            HashlineEditOutput::EditsApplied(result) => {
                // All three edit regions should appear in the snippet.
                assert!(
                    result.snippet.contains("line_4") || result.snippet.contains("line_6"),
                    "Snippet should show context around the deletion"
                );
                assert!(
                    result.snippet.contains("INSERTED_A"),
                    "Snippet should show inserted content"
                );
                assert!(
                    result.snippet.contains("REPLACED_BOTTOM"),
                    "Snippet should show replaced content"
                );
                assert!(
                    result.snippet.contains("lines not shown"),
                    "Scattered mixed ops should have gap markers"
                );
                let snippet_lines = result.snippet.lines().count();
                assert!(
                    snippet_lines < 40,
                    "Snippet should be compact, got {snippet_lines} lines"
                );
            }
            HashlineEditOutput::Error(e) => panic!("Expected success: {}", e.message),
        }
    }

    #[test]
    fn stale_anchor_error() {
        // Use an anchor from a different file content.
        let ops = vec![HashlineOp::Replace {
            anchor: "2:zzz:zzz".to_owned(),
            end_anchor: None,
            content: "replaced".to_owned(),
        }];

        match apply_edits(SAMPLE, &ops, &test_path(), &*test_scheme()).output {
            HashlineEditOutput::Error(e) => {
                assert_eq!(e.error, HashlineEditErrorKind::AnchorStale);
                assert!(e.context.is_some());
                assert!(e.current.is_some());
            }
            HashlineEditOutput::EditsApplied(_) => panic!("Expected error"),
        }
    }

    #[test]
    fn batch_stale_anchor_rejects_all_and_identifies_failing_edit() {
        let anchors = anchors_for(SAMPLE);
        // First op is valid, second op uses a stale anchor.
        let ops = vec![
            HashlineOp::Replace {
                anchor: anchors[0].clone(),
                end_anchor: None,
                content: "valid edit".to_owned(),
            },
            HashlineOp::Replace {
                anchor: "3:zzz:zzz".to_owned(),
                end_anchor: None,
                content: "stale edit".to_owned(),
            },
        ];

        match apply_edits(SAMPLE, &ops, &test_path(), &*test_scheme()).output {
            HashlineEditOutput::Error(e) => {
                assert!(
                    e.message.contains("Edit 2/2"),
                    "Should identify failing edit as 2/2, got: {}",
                    e.message
                );
                assert!(
                    e.message.contains("none of the edits were applied"),
                    "Should state none were applied, got: {}",
                    e.message
                );
                assert!(
                    e.message.contains("Retry all 2 edits"),
                    "Should tell to retry all, got: {}",
                    e.message
                );
            }
            HashlineEditOutput::EditsApplied(_) => {
                panic!("Batch should fail when any anchor is stale")
            }
        }
    }

    #[test]
    fn batch_overlap_identifies_conflicting_edits() {
        let anchors = anchors_for(SAMPLE);
        let ops = vec![
            HashlineOp::Replace {
                anchor: anchors[1].clone(),
                end_anchor: Some(anchors[3].clone()), // lines 2-4
                content: "a".to_owned(),
            },
            HashlineOp::Replace {
                anchor: anchors[2].clone(), // line 3 — overlaps
                end_anchor: None,
                content: "b".to_owned(),
            },
        ];

        match apply_edits(SAMPLE, &ops, &test_path(), &*test_scheme()).output {
            HashlineEditOutput::Error(e) => {
                assert_eq!(e.error, HashlineEditErrorKind::OverlappingEdits);
                assert!(
                    e.message.contains("edit #1") && e.message.contains("edit #2"),
                    "Should identify both conflicting edits, got: {}",
                    e.message
                );
                assert!(
                    e.message.contains("none were applied"),
                    "Should state none were applied, got: {}",
                    e.message
                );
            }
            HashlineEditOutput::EditsApplied(_) => panic!("Overlapping batch should fail"),
        }
    }

    #[test]
    fn malformed_anchor_error() {
        let ops = vec![HashlineOp::Replace {
            anchor: "not-an-anchor".to_owned(),
            end_anchor: None,
            content: "replaced".to_owned(),
        }];

        match apply_edits(SAMPLE, &ops, &test_path(), &*test_scheme()).output {
            HashlineEditOutput::Error(e) => {
                assert_eq!(e.error, HashlineEditErrorKind::InvalidInput);
            }
            HashlineEditOutput::EditsApplied(_) => panic!("Expected error"),
        }
    }

    #[test]
    fn out_of_range_anchor_error() {
        let ops = vec![HashlineOp::Replace {
            anchor: "100:abc:def".to_owned(),
            end_anchor: None,
            content: "replaced".to_owned(),
        }];

        match apply_edits(SAMPLE, &ops, &test_path(), &*test_scheme()).output {
            HashlineEditOutput::Error(e) => {
                assert_eq!(e.error, HashlineEditErrorKind::AnchorNotFound);
            }
            HashlineEditOutput::EditsApplied(_) => panic!("Expected error"),
        }
    }

    #[test]
    fn overlapping_edits_error() {
        let anchors = anchors_for(SAMPLE);
        let ops = vec![
            HashlineOp::Replace {
                anchor: anchors[1].clone(),
                end_anchor: Some(anchors[3].clone()), // lines 2-4
                content: "a".to_owned(),
            },
            HashlineOp::Replace {
                anchor: anchors[2].clone(), // line 3 — overlaps
                end_anchor: None,
                content: "b".to_owned(),
            },
        ];

        match apply_edits(SAMPLE, &ops, &test_path(), &*test_scheme()).output {
            HashlineEditOutput::Error(e) => {
                assert_eq!(e.error, HashlineEditErrorKind::OverlappingEdits);
            }
            HashlineEditOutput::EditsApplied(_) => panic!("Expected error"),
        }
    }

    #[test]
    fn end_before_start_error() {
        let anchors = anchors_for(SAMPLE);
        let ops = vec![HashlineOp::Replace {
            anchor: anchors[3].clone(),           // line 4
            end_anchor: Some(anchors[1].clone()), // line 2 — before start
            content: "x".to_owned(),
        }];

        match apply_edits(SAMPLE, &ops, &test_path(), &*test_scheme()).output {
            HashlineEditOutput::Error(e) => {
                assert_eq!(e.error, HashlineEditErrorKind::InvalidInput);
            }
            HashlineEditOutput::EditsApplied(_) => panic!("Expected error"),
        }
    }

    #[test]
    fn success_includes_scheme_name() {
        let anchors = anchors_for(SAMPLE);
        let ops = vec![HashlineOp::Replace {
            anchor: anchors[1].clone(),
            end_anchor: None,
            content: "    let x = 42;".to_owned(),
        }];

        match apply_edits(SAMPLE, &ops, &test_path(), &*test_scheme()).output {
            HashlineEditOutput::EditsApplied(result) => {
                assert_eq!(result.scheme, "chunk_v1");
            }
            HashlineEditOutput::Error(e) => panic!("Expected success, got error: {}", e.message),
        }
    }

    #[test]
    fn snippet_has_fresh_anchors() {
        let anchors = anchors_for(SAMPLE);
        let ops = vec![HashlineOp::Replace {
            anchor: anchors[1].clone(),
            end_anchor: None,
            content: "    let x = 42;".to_owned(),
        }];

        match apply_edits(SAMPLE, &ops, &test_path(), &*test_scheme()).output {
            HashlineEditOutput::EditsApplied(result) => {
                // Snippet should have the hashline format with anchors.
                assert!(result.snippet.contains('→'));
                assert!(result.snippet.contains(':'));
            }
            HashlineEditOutput::Error(e) => panic!("Expected success, got error: {}", e.message),
        }
    }

    #[test]
    fn eof_insert_no_extra_blank_line_trailing_newline() {
        // File ending with '\n' — EOF should not introduce an extra blank line.
        let content = "line1\nline2\n";
        let ops = vec![HashlineOp::InsertAfter {
            anchor: "EOF".to_owned(),
            content: "line3".to_owned(),
        }];

        match apply_edits(content, &ops, &test_path(), &*test_scheme()).output {
            HashlineEditOutput::EditsApplied(result) => {
                // "line3" should appear right after "line2" in the snippet,
                // without an intervening blank line.
                assert!(result.snippet.contains("line3"));
                // Count content lines in snippet (excluding "lines not shown").
                let content_lines: Vec<&str> = result
                    .snippet
                    .lines()
                    .filter(|l| !l.starts_with("..."))
                    .collect();
                // Should not have a blank-only line between line2 and line3.
                let texts: Vec<&str> = content_lines
                    .iter()
                    .filter_map(|l| l.split('→').nth(1))
                    .collect();
                if let Some(pos) = texts.iter().position(|t| *t == "line3")
                    && pos > 0
                {
                    assert_ne!(
                        texts[pos - 1],
                        "",
                        "should not have blank line before EOF insert"
                    );
                }
            }
            HashlineEditOutput::Error(e) => panic!("Expected success, got error: {}", e.message),
        }
    }

    #[test]
    fn eof_insert_no_trailing_newline() {
        // File NOT ending with '\n'.
        let content = "line1\nline2";
        let ops = vec![HashlineOp::InsertAfter {
            anchor: "EOF".to_owned(),
            content: "line3".to_owned(),
        }];

        match apply_edits(content, &ops, &test_path(), &*test_scheme()).output {
            HashlineEditOutput::EditsApplied(result) => {
                assert!(result.snippet.contains("line3"));
            }
            HashlineEditOutput::Error(e) => panic!("Expected success, got error: {}", e.message),
        }
    }

    #[test]
    fn same_anchor_inserts_preserve_request_order() {
        let anchors = anchors_for(SAMPLE);
        let ops = vec![
            HashlineOp::InsertAfter {
                anchor: anchors[1].clone(), // after line 2
                content: "    // first".to_owned(),
            },
            HashlineOp::InsertAfter {
                anchor: anchors[1].clone(), // same anchor
                content: "    // second".to_owned(),
            },
        ];

        match apply_edits(SAMPLE, &ops, &test_path(), &*test_scheme()).output {
            HashlineEditOutput::EditsApplied(result) => {
                // "first" should appear before "second" in the output.
                let first_pos = result.snippet.find("// first");
                let second_pos = result.snippet.find("// second");
                assert!(
                    first_pos.is_some() && second_pos.is_some(),
                    "both inserts should appear in snippet"
                );
                assert!(
                    first_pos.unwrap() < second_pos.unwrap(),
                    "request order should be preserved: first before second"
                );
            }
            HashlineEditOutput::Error(e) => panic!("Expected success, got error: {}", e.message),
        }
    }

    #[test]
    fn insert_inside_replace_range_rejected() {
        let anchors = anchors_for(SAMPLE);
        let ops = vec![
            HashlineOp::Replace {
                anchor: anchors[1].clone(),
                end_anchor: Some(anchors[3].clone()), // lines 2-4
                content: "replaced".to_owned(),
            },
            HashlineOp::InsertAfter {
                anchor: anchors[2].clone(), // line 3 — inside replaced span
                content: "inserted".to_owned(),
            },
        ];

        match apply_edits(SAMPLE, &ops, &test_path(), &*test_scheme()).output {
            HashlineEditOutput::Error(e) => {
                assert_eq!(e.error, HashlineEditErrorKind::OverlappingEdits);
            }
            HashlineEditOutput::EditsApplied(_) => {
                panic!("Expected overlap error for insert inside replace range")
            }
        }
    }

    #[test]
    fn insert_before_replace_range_allowed() {
        let anchors = anchors_for(SAMPLE);
        let ops = vec![
            HashlineOp::Replace {
                anchor: anchors[1].clone(),
                end_anchor: Some(anchors[3].clone()), // 0-based [1..4)
                content: "replaced".to_owned(),
            },
            HashlineOp::InsertAfter {
                anchor: "0:".to_owned(), // inserts at idx 0, before range
                content: "header".to_owned(),
            },
        ];

        match apply_edits(SAMPLE, &ops, &test_path(), &*test_scheme()).output {
            HashlineEditOutput::EditsApplied(_) => {}
            HashlineEditOutput::Error(e) => {
                panic!("Insert before range should not overlap: {}", e.message)
            }
        }
    }

    #[test]
    fn insert_at_start_of_replace_range_rejected() {
        let anchors = anchors_for(SAMPLE);
        // Replace 0-based [1..4). Insert after line 1 → insert_at=2, but
        // insert_after anchor[0] (line 1) → insert_at=1, which is range.start.
        let ops = vec![
            HashlineOp::Replace {
                anchor: anchors[1].clone(),
                end_anchor: Some(anchors[3].clone()), // 0-based [1..4)
                content: "replaced".to_owned(),
            },
            HashlineOp::InsertAfter {
                anchor: anchors[0].clone(), // after line 1 → insert_at=1 = range.start
                content: "at_range_start".to_owned(),
            },
        ];

        match apply_edits(SAMPLE, &ops, &test_path(), &*test_scheme()).output {
            HashlineEditOutput::Error(e) => {
                assert_eq!(e.error, HashlineEditErrorKind::OverlappingEdits);
            }
            HashlineEditOutput::EditsApplied(_) => {
                panic!("Insert at start of replace range should be rejected")
            }
        }
    }

    #[test]
    fn insert_at_exclusive_end_of_replace_range_allowed() {
        let anchors = anchors_for(SAMPLE);
        // Replace lines 2-3 (0-based: [1..3))
        let ops = vec![
            HashlineOp::Replace {
                anchor: anchors[1].clone(),
                end_anchor: Some(anchors[2].clone()), // lines 2-3
                content: "replaced".to_owned(),
            },
            HashlineOp::InsertAfter {
                anchor: anchors[2].clone(), // insert after line 3 — at idx 3, which is exclusive end
                content: "after_range".to_owned(),
            },
        ];

        // insert_at=3 is NOT inside [1..3), so this should succeed.
        match apply_edits(SAMPLE, &ops, &test_path(), &*test_scheme()).output {
            HashlineEditOutput::EditsApplied(result) => {
                assert!(result.snippet.contains("after_range"));
            }
            HashlineEditOutput::Error(e) => {
                panic!("Insert at exclusive end should not overlap: {}", e.message)
            }
        }
    }

    // -- Stateless range policy tests ----------------------------------------

    #[test]
    fn large_range_produces_warning() {
        let mut big_lines: Vec<String> = (0..30).map(|i| format!("line {i}")).collect();
        big_lines.push(String::new());
        let big_content = big_lines.join("\n");

        let anchors = anchors_for(&big_content);
        let ops = vec![HashlineOp::Replace {
            anchor: anchors[0].clone(),
            end_anchor: Some(anchors[24].clone()),
            content: "replaced".to_owned(),
        }];

        match apply_edits(&big_content, &ops, &test_path(), &*test_scheme()).output {
            HashlineEditOutput::EditsApplied(a) => {
                assert!(
                    a.warnings.iter().any(|w| w.contains("large range")),
                    "Large range edit should produce warning, got: {:?}",
                    a.warnings
                );
            }
            HashlineEditOutput::Error(e) => panic!("Expected success: {}", e.message),
        }
    }

    #[test]
    fn medium_range_warns() {
        let mut lines: Vec<String> = (0..15).map(|i| format!("line{i}")).collect();
        lines.push(String::new());
        let content = lines.join("\n");

        let anchors = anchors_for(&content);
        let ops = vec![HashlineOp::Replace {
            anchor: anchors[0].clone(),
            end_anchor: Some(anchors[9].clone()),
            content: "replaced".to_owned(),
        }];

        match apply_edits(&content, &ops, &test_path(), &*test_scheme()).output {
            HashlineEditOutput::EditsApplied(a) => {
                assert!(
                    a.warnings.iter().any(|w| w.contains("medium range")),
                    "Medium range should warn, got: {:?}",
                    a.warnings
                );
            }
            HashlineEditOutput::Error(e) => panic!("Expected success: {}", e.message),
        }
    }

    #[test]
    fn small_range_no_warning() {
        let anchors = anchors_for(SAMPLE);
        let ops = vec![HashlineOp::Replace {
            anchor: anchors[1].clone(),
            end_anchor: Some(anchors[2].clone()), // 2-line range
            content: "replaced".to_owned(),
        }];

        match apply_edits(SAMPLE, &ops, &test_path(), &*test_scheme()).output {
            HashlineEditOutput::EditsApplied(a) => {
                assert!(a.warnings.is_empty(), "Small range should not warn");
            }
            HashlineEditOutput::Error(e) => panic!("Expected success: {}", e.message),
        }
    }

    // -- Recovery tests -----------------------------------------------------

    #[test]
    fn shifted_recovery_after_insert_above() {
        let anchors = anchors_for(SAMPLE);
        let anchor_line2 = anchors[1].clone(); // "    let x = 1;"

        // Insert 2 lines at the top → line 2 shifts to line 4.
        let mut shifted_lines: Vec<&str> = vec!["// new1", "// new2"];
        let orig: Vec<&str> = SAMPLE.lines().collect();
        shifted_lines.extend_from_slice(&orig);
        let shifted_content = shifted_lines.join("\n");

        let ops = vec![HashlineOp::Replace {
            anchor: anchor_line2,
            end_anchor: None,
            content: "    let x = 999;".to_owned(),
        }];

        match apply_edits(&shifted_content, &ops, &test_path(), &*test_scheme()).output {
            HashlineEditOutput::Error(e) => {
                // With chunk-based scheme, insertion changes chunk boundaries,
                // so recovery may or may not find the shifted line depending
                // on whether the chunk context still matches. Both AnchorStale
                // (with or without shifted_to) are acceptable outcomes.
                assert!(
                    e.error == HashlineEditErrorKind::AnchorStale
                        || e.error == HashlineEditErrorKind::AmbiguousAnchor,
                    "expected stale or ambiguous, got {:?}",
                    e.error
                );
                // If shifted recovery succeeded, verify the fields.
                if let Some(new_line) = e.shifted_to {
                    assert!(new_line > 2, "shifted line should be after insertion");
                    assert!(e.shifted_anchor.is_some());
                    assert!(e.message.contains("shifted"));
                    assert!(e.message.contains("Retry"));
                }
            }
            HashlineEditOutput::EditsApplied(_) => {
                panic!("Expected stale error after insert above")
            }
        }
    }

    #[test]
    fn shifted_recovery_after_delete_above() {
        let anchors = anchors_for(SAMPLE);
        let anchor_line4 = anchors[3].clone(); // "    println!(...)"

        // Delete line 1 → line 4 shifts to line 3.
        let mut lines: Vec<&str> = SAMPLE.lines().collect();
        lines.remove(0);
        let modified = lines.join("\n");

        let ops = vec![HashlineOp::Replace {
            anchor: anchor_line4,
            end_anchor: None,
            content: "    println!(\"changed\");".to_owned(),
        }];

        match apply_edits(&modified, &ops, &test_path(), &*test_scheme()).output {
            HashlineEditOutput::Error(e) => {
                assert_eq!(e.error, HashlineEditErrorKind::AnchorStale);
                // Recovery should find the content at line 3.
                if let Some(new_line) = e.shifted_to {
                    assert_eq!(new_line, 3);
                    assert!(e.shifted_anchor.is_some());
                }
            }
            HashlineEditOutput::EditsApplied(_) => {
                panic!("Expected stale error for shifted content")
            }
        }
    }

    #[test]
    fn no_recovery_when_content_changed() {
        let anchors = anchors_for(SAMPLE);
        let anchor_line2 = anchors[1].clone();

        // Replace line 2's content entirely.
        let mut lines: Vec<&str> = SAMPLE.lines().collect();
        lines[1] = "    let completely_different = true;";
        let modified = lines.join("\n");

        let ops = vec![HashlineOp::Replace {
            anchor: anchor_line2,
            end_anchor: None,
            content: "    let x = 999;".to_owned(),
        }];

        match apply_edits(&modified, &ops, &test_path(), &*test_scheme()).output {
            HashlineEditOutput::Error(e) => {
                assert_eq!(e.error, HashlineEditErrorKind::AnchorStale);
                assert!(
                    e.shifted_to.is_none(),
                    "should not find shifted target when content changed"
                );
                assert!(e.message.contains("fresh anchors"));
            }
            HashlineEditOutput::EditsApplied(_) => panic!("Expected stale error"),
        }
    }

    #[test]
    fn ambiguous_recovery_with_repetitive_content() {
        // File with many identical lines.
        let mut lines: Vec<String> = vec!["unique_header".to_owned()];
        for _ in 0..10 {
            lines.push("    repeated_line();".to_owned());
        }
        lines.push(String::new());
        let content = lines.join("\n");

        let anchors = anchors_for(&content);
        let anchor_line5 = anchors[4].clone(); // one of the repeated lines

        // Insert a line at top → all repeated lines shift.
        let mut shifted = vec!["// inserted".to_owned()];
        shifted.extend(lines);
        let shifted_content = shifted.join("\n");

        let ops = vec![HashlineOp::Replace {
            anchor: anchor_line5,
            end_anchor: None,
            content: "    changed();".to_owned(),
        }];

        match apply_edits(&shifted_content, &ops, &test_path(), &*test_scheme()).output {
            HashlineEditOutput::Error(e) => {
                // Content-only local hash with chunk context: may be ambiguous
                // or may find a shifted match depending on chunk boundaries.
                // The key invariant: it should NOT silently succeed.
                assert!(
                    e.error == HashlineEditErrorKind::AnchorStale
                        || e.error == HashlineEditErrorKind::AmbiguousAnchor,
                    "Expected stale or ambiguous, got {:?}",
                    e.error
                );
                if e.error == HashlineEditErrorKind::AmbiguousAnchor {
                    assert!(
                        !e.ambiguous_candidates.is_empty(),
                        "ambiguous error should list candidates"
                    );
                    assert!(e.message.contains("Multiple candidates"));
                }
            }
            HashlineEditOutput::EditsApplied(_) => {
                panic!("Expected error for shifted repetitive content")
            }
        }
    }

    #[test]
    fn stale_error_has_context_snippet() {
        let ops = vec![HashlineOp::Replace {
            anchor: "2:zzz:zzz".to_owned(),
            end_anchor: None,
            content: "replaced".to_owned(),
        }];

        match apply_edits(SAMPLE, &ops, &test_path(), &*test_scheme()).output {
            HashlineEditOutput::Error(e) => {
                assert!(e.context.is_some(), "stale error should have context");
                let ctx = e.context.unwrap();
                assert!(ctx.contains('→'), "context should have anchored lines");
                assert!(
                    e.context_start_line.is_some(),
                    "context should have start line"
                );
            }
            HashlineEditOutput::EditsApplied(_) => panic!("Expected stale error"),
        }
    }

    #[test]
    fn shifted_anchor_is_usable() {
        let anchors = anchors_for(SAMPLE);
        let anchor_line2 = anchors[1].clone();

        // Insert 1 line at top → line 2 shifts to line 3.
        let mut shifted_lines: Vec<&str> = vec!["// new"];
        let orig: Vec<&str> = SAMPLE.lines().collect();
        shifted_lines.extend_from_slice(&orig);
        let shifted_content = shifted_lines.join("\n");

        let ops = vec![HashlineOp::Replace {
            anchor: anchor_line2,
            end_anchor: None,
            content: "    let x = 999;".to_owned(),
        }];

        let err = match apply_edits(&shifted_content, &ops, &test_path(), &*test_scheme()).output {
            HashlineEditOutput::Error(e) => e,
            HashlineEditOutput::EditsApplied(_) => panic!("Expected stale error"),
        };

        if let Some(fresh) = err.shifted_anchor {
            let retry_ops = vec![HashlineOp::Replace {
                anchor: fresh,
                end_anchor: None,
                content: "    let x = 999;".to_owned(),
            }];
            match apply_edits(&shifted_content, &retry_ops, &test_path(), &*test_scheme()).output {
                HashlineEditOutput::EditsApplied(a) => {
                    assert!(a.snippet.contains("999"));
                }
                HashlineEditOutput::Error(e) => {
                    panic!("Retry with shifted_anchor should succeed: {}", e.message)
                }
            }
        }
    }

    /// Deterministic test proving shifted recovery works with a real full
    /// chunk-context anchor — the same shape `hashline_read` emits.
    ///
    /// Scenario: insert exactly `chunk_size` (8) lines at position 0.
    /// Every original line shifts by +8. A line originally at position `p`
    /// moves to `p+8`, which is in the next chunk — but that chunk now
    /// contains the same lines as the original chunk at `p`. So the chunk
    /// fingerprint matches, and `find_shifted` recovers deterministically.
    #[test]
    fn deterministic_shifted_recovery_with_full_anchor() {
        // 16 unique lines → chunks [0,8) and [8,16).
        let lines: Vec<String> = (0..16).map(|i| format!("unique_line_{i}")).collect();
        let original = lines.join("\n");

        // Get the FULL anchor (with chunk context) for line 5.
        let full_anchors = anchors_for(&original);
        let full_anchor = full_anchors[4].clone(); // line 5, has :local:context

        // Insert exactly 8 new lines at the top.
        // Line 5 → position 13. Chunk at [8,16) in the shifted file =
        // original lines [0,8) = same chunk content → same fingerprint.
        let inserted: Vec<String> = (0..8).map(|i| format!("inserted_{i}")).collect();
        let mut shifted_lines = inserted;
        shifted_lines.extend(lines);
        let shifted_content = shifted_lines.join("\n");

        let ops = vec![HashlineOp::Replace {
            anchor: full_anchor,
            end_anchor: None,
            content: "REPLACED".to_owned(),
        }];

        // Step 1: edit fails (anchor at line 5 now has different content).
        let err = match apply_edits(&shifted_content, &ops, &test_path(), &*test_scheme()).output {
            HashlineEditOutput::Error(e) => e,
            HashlineEditOutput::EditsApplied(_) => {
                panic!("Expected stale error for shifted full anchor")
            }
        };

        // Step 2: recovery MUST find the shifted line (chunk alignment preserved).
        assert!(
            err.shifted_to.is_some(),
            "Recovery must find shifted line with full chunk anchor. Error: {}",
            err.message
        );
        assert_eq!(err.shifted_to.unwrap(), 13); // line 5 + 8 = line 13
        let fresh = err.shifted_anchor.expect("shifted_anchor must be present");
        assert!(err.message.contains("Retry"));

        // Step 3: retry with the shifted anchor MUST succeed.
        let retry_ops = vec![HashlineOp::Replace {
            anchor: fresh,
            end_anchor: None,
            content: "REPLACED".to_owned(),
        }];
        match apply_edits(&shifted_content, &retry_ops, &test_path(), &*test_scheme()).output {
            HashlineEditOutput::EditsApplied(a) => {
                assert!(a.snippet.contains("REPLACED"));
            }
            HashlineEditOutput::Error(e) => {
                panic!("Retry with shifted_anchor must succeed: {}", e.message)
            }
        }
    }

    #[test]
    fn edit_details_for_single_replace() {
        let anchors = anchors_for(SAMPLE);
        let ops = vec![HashlineOp::Replace {
            anchor: anchors[1].clone(),
            end_anchor: None,
            content: "    let x = 42;".to_owned(),
        }];

        let result = apply_edits(SAMPLE, &ops, &test_path(), &*test_scheme());
        assert_eq!(result.edit_details.len(), 1);
        assert_eq!(result.edit_details[0].old_text, "    let x = 1;");
        assert_eq!(result.edit_details[0].new_text, "    let x = 42;");
        assert_eq!(result.edit_details[0].old_line, 2);
        assert_eq!(result.edit_details[0].new_line, 2);
    }

    #[test]
    fn edit_details_for_insert_has_empty_old_text() {
        let anchors = anchors_for(SAMPLE);
        let ops = vec![HashlineOp::InsertAfter {
            anchor: anchors[1].clone(),
            content: "    let z = 3;".to_owned(),
        }];

        let result = apply_edits(SAMPLE, &ops, &test_path(), &*test_scheme());
        assert_eq!(result.edit_details.len(), 1);
        assert_eq!(result.edit_details[0].old_text, "");
        assert_eq!(result.edit_details[0].new_text, "    let z = 3;");
    }

    #[test]
    fn insert_after_empty_content_inserts_blank_line() {
        let content = "line1\nline2\nline3\n";
        let anchors = anchors_for(content);

        let ops = vec![HashlineOp::InsertAfter {
            anchor: anchors[0].clone(),
            content: String::new(),
        }];

        let result = apply_edits(content, &ops, &test_path(), &*test_scheme());
        let new = result.new_content.expect("should succeed");

        // A blank line should appear between line1 and line2.
        assert!(
            new.contains("line1\n\nline2"),
            "empty content should insert a blank line, got: {new}"
        );

        // The detail should reflect the blank line insertion.
        assert_eq!(result.edit_details.len(), 1);
        assert_eq!(result.edit_details[0].old_text, "");
        assert_eq!(result.edit_details[0].new_text, "");

        // The snippet should include the blank line with a fresh anchor.
        match result.output {
            HashlineEditOutput::EditsApplied(applied) => {
                assert!(
                    applied.snippet.contains('\u{2192}'),
                    "snippet should contain anchored lines"
                );
            }
            HashlineEditOutput::Error(e) => panic!("expected success: {}", e.message),
        }
    }

    #[test]
    fn edit_details_for_delete_has_empty_new_text() {
        let anchors = anchors_for(SAMPLE);
        let ops = vec![HashlineOp::Replace {
            anchor: anchors[1].clone(),
            end_anchor: None,
            content: String::new(),
        }];

        let result = apply_edits(SAMPLE, &ops, &test_path(), &*test_scheme());
        assert_eq!(result.edit_details.len(), 1);
        assert_eq!(result.edit_details[0].old_text, "    let x = 1;");
        assert_eq!(result.edit_details[0].new_text, "");
    }

    #[test]
    fn edit_details_for_range_replace_captures_full_range() {
        let content = "line1\nline2\nline3\nline4\nline5\n";
        let anchors = anchors_for(content);

        let ops = vec![HashlineOp::Replace {
            anchor: anchors[1].clone(),           // line2
            end_anchor: Some(anchors[3].clone()), // line4
            content: "replaced_range".to_owned(),
        }];

        let result = apply_edits(content, &ops, &test_path(), &*test_scheme());
        assert_eq!(result.edit_details.len(), 1);
        assert_eq!(result.edit_details[0].old_text, "line2\nline3\nline4");
        assert_eq!(result.edit_details[0].new_text, "replaced_range");
        assert_eq!(result.edit_details[0].old_line, 2);
    }

    #[test]
    fn edit_details_for_multi_edit_with_shift() {
        let anchors = anchors_for(SAMPLE);
        let ops = vec![
            HashlineOp::InsertAfter {
                anchor: anchors[0].clone(), // after "fn main() {"
                content: "    // comment".to_owned(),
            },
            HashlineOp::Replace {
                anchor: anchors[3].clone(), // println line
                end_anchor: None,
                content: "    println!(\"changed\");".to_owned(),
            },
        ];

        let result = apply_edits(SAMPLE, &ops, &test_path(), &*test_scheme());
        assert_eq!(result.edit_details.len(), 2);
        // Insert: no old content
        assert_eq!(result.edit_details[0].old_text, "");
        assert_eq!(result.edit_details[0].new_text, "    // comment");
        // Replace: old content is the println line
        assert_eq!(
            result.edit_details[1].old_text,
            "    println!(\"{x} {y}\");"
        );
        assert_eq!(
            result.edit_details[1].new_text,
            "    println!(\"changed\");"
        );
        // New line should account for the insertion shift
        assert_eq!(result.edit_details[1].old_line, 4);
        assert_eq!(result.edit_details[1].new_line, 5); // shifted by 1
    }

    #[test]
    fn edit_details_empty_for_write_op() {
        let ops = vec![HashlineOp::Write {
            content: "new content\n".to_owned(),
        }];

        let result = apply_edits(SAMPLE, &ops, &test_path(), &*test_scheme());
        assert!(result.edit_details.is_empty());
    }

    #[test]
    fn edit_details_empty_on_error() {
        let ops = vec![HashlineOp::Replace {
            anchor: "2:zzz:zzz".to_owned(),
            end_anchor: None,
            content: "replaced".to_owned(),
        }];

        let result = apply_edits(SAMPLE, &ops, &test_path(), &*test_scheme());
        assert!(result.edit_details.is_empty());
    }

    #[test]
    fn edit_details_for_scattered_edits_are_compact() {
        let line_count = 200;
        let mut file_lines: Vec<String> = (0..line_count).map(|i| format!("line_{i}")).collect();
        file_lines.push(String::new());
        let content = file_lines.join("\n");
        let anchors = anchors_for(&content);

        let ops = vec![
            HashlineOp::InsertAfter {
                anchor: anchors[4].clone(),
                content: "INSERTED".to_owned(),
            },
            HashlineOp::Replace {
                anchor: anchors[194].clone(),
                end_anchor: None,
                content: "REPLACED".to_owned(),
            },
        ];

        let result = apply_edits(&content, &ops, &test_path(), &*test_scheme());
        assert_eq!(result.edit_details.len(), 2);

        // Each detail should contain only the affected content
        assert_eq!(result.edit_details[0].old_text, "");
        assert_eq!(result.edit_details[0].new_text, "INSERTED");
        assert_eq!(result.edit_details[1].old_text, "line_194");
        assert_eq!(result.edit_details[1].new_text, "REPLACED");

        // Total size should be tiny — NOT the entire file
        let total: usize = result
            .edit_details
            .iter()
            .map(|d| d.old_text.len() + d.new_text.len())
            .sum();
        assert!(total < 100, "Details should be compact, got {total} bytes");
    }

    #[test]
    fn format_hint_chunk_scheme_shows_two_hashes() {
        let scheme = ChunkFingerprint::with_params(3, 8);
        let (label, example) = anchor_format_hint(&scheme);
        assert_eq!(label, "LINE:HASH1:HASH2");
        assert_eq!(example, "22:abc:rst");
    }

    #[test]
    fn format_hint_content_only_scheme_shows_single_hash() {
        let scheme = ContentOnly::with_hash_len(3);
        let (label, example) = anchor_format_hint(&scheme);
        assert_eq!(label, "LINE:HASH");
        assert_eq!(example, "22:abc");
    }

    #[test]
    fn format_hint_respects_hash_len() {
        let short = ChunkFingerprint::with_params(2, 8);
        let (_, ex_short) = anchor_format_hint(&short);
        assert_eq!(ex_short, "22:ab:rs");

        let long = ContentOnly::with_hash_len(4);
        let (_, ex_long) = anchor_format_hint(&long);
        assert_eq!(ex_long, "22:abcd");
    }

    #[test]
    fn format_hint_used_in_malformed_anchor_error() {
        let content = "line1\nline2\n";
        let lines: Vec<&str> = split_lines(content);
        let scheme = ChunkFingerprint::with_params(3, 8);

        let err = validate_anchor("not-an-anchor", &lines, &scheme).unwrap_err();
        assert!(
            err.message.contains("LINE:HASH1:HASH2"),
            "msg: {}",
            err.message
        );
        assert!(err.message.contains("22:abc:rst"), "msg: {}", err.message);

        let scheme_co = ContentOnly::with_hash_len(3);
        let err_co = validate_anchor("!!!", &lines, &scheme_co).unwrap_err();
        assert!(
            err_co.message.contains("LINE:HASH"),
            "msg: {}",
            err_co.message
        );
        assert!(
            !err_co.message.contains("HASH1:HASH2"),
            "should not show chunk format: {}",
            err_co.message
        );
        assert!(err_co.message.contains("22:abc"), "msg: {}", err_co.message);
    }

    #[test]
    fn arrow_content_stripped_from_anchor() {
        let content = "first\nsecond\nthird\nfourth\nfifth\nsixth\nseventh\neighth\nninth\n";
        let lines: Vec<&str> = split_lines(content);
        let scheme = ChunkFingerprint::with_params(3, 8);
        let anchors = scheme.generate_anchors(&lines);
        let real_anchor = anchors[0].render();

        assert!(validate_anchor(&real_anchor, &lines, &scheme).is_ok());

        // Unicode → stripped.
        let with_arrow = format!("{real_anchor}\u{2192}first");
        assert!(validate_anchor(&with_arrow, &lines, &scheme).is_ok());

        // ASCII -> also stripped (model occasionally normalizes the Unicode arrow).
        let with_ascii = format!("{real_anchor}->first");
        assert!(validate_anchor(&with_ascii, &lines, &scheme).is_ok());

        // Leading spaces still rejected — stripping only handles arrow suffixes.
        let padded = format!("   {real_anchor}\u{2192}    first");
        assert!(validate_anchor(&padded, &lines, &scheme).is_err());

        // Stale anchor with arrow reports the stripped anchor in error metadata.
        let stale = format!("2:zzz:zzz\u{2192}content");
        let err = validate_anchor(&stale, &lines, &scheme).unwrap_err();
        assert_eq!(err.requested_anchor.as_deref(), Some("2:zzz:zzz"));

        let stale_ascii = format!("2:zzz:zzz->content");
        let err = validate_anchor(&stale_ascii, &lines, &scheme).unwrap_err();
        assert_eq!(err.requested_anchor.as_deref(), Some("2:zzz:zzz"));
    }

    #[test]
    fn hash_only_anchor_recovered_when_unique() {
        let content = "alpha\nbeta\ngamma\ndelta\nepsilon\nzeta\neta\ntheta\niota\n";
        let lines: Vec<&str> = split_lines(content);
        let scheme = ChunkFingerprint::with_params(3, 8);
        let anchors = scheme.generate_anchors(&lines);

        // Get the suffix (hash portion without line number) for line 3.
        let suffix = anchor_suffix(&anchors[2]);
        assert!(!suffix.is_empty());

        // Full anchor works.
        let full = anchors[2].render();
        assert!(validate_anchor(&full, &lines, &scheme).is_ok());

        // Hash-only (no line number) should recover if unique.
        let result = validate_anchor(&suffix, &lines, &scheme);
        assert!(
            result.is_ok(),
            "unique hash suffix should recover: {suffix}"
        );
        assert_eq!(result.unwrap(), 2); // 0-based line index
    }

    #[test]
    fn hash_only_anchor_rejected_when_ambiguous() {
        // File with repeated lines → same hash suffix on multiple lines.
        let content = "same\nsame\nsame\nsame\nsame\nsame\nsame\nsame\n";
        let lines: Vec<&str> = split_lines(content);
        let scheme = ContentOnly::with_hash_len(3);
        let anchors = scheme.generate_anchors(&lines);

        let suffix = anchor_suffix(&anchors[0]);
        let result = validate_anchor(&suffix, &lines, &scheme);
        assert!(result.is_err(), "ambiguous hash suffix should not recover");
    }

    #[test]
    fn hash_only_anchor_rejected_when_no_match() {
        let content = "alpha\nbeta\ngamma\n";
        let lines: Vec<&str> = split_lines(content);
        let scheme = ContentOnly::with_hash_len(3);
        let result = validate_anchor("zzz", &lines, &scheme);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().error,
            HashlineEditErrorKind::InvalidInput
        );
    }

    #[test]
    fn detect_anchor_prefix_in_content_works() {
        // Unicode arrow detected
        assert_eq!(
            detect_anchor_prefix_in_content("axy:edj\u{2192}    # comment"),
            Some(1)
        );
        // With line number prefix
        assert_eq!(
            detect_anchor_prefix_in_content("   56:axy:edj\u{2192}    let x = 1;"),
            Some(1)
        );
        // ASCII arrow detected
        assert_eq!(
            detect_anchor_prefix_in_content("22:abc:rst->code here"),
            Some(1)
        );
        // No prefix — not detected
        assert_eq!(detect_anchor_prefix_in_content("    normal code"), None);
        // Empty after arrow — still detected
        assert_eq!(detect_anchor_prefix_in_content("ab:cd\u{2192}"), Some(1));
        // Multi-line: detected on first line
        let multi = "22:xx:yy\u{2192}line1\nline2";
        assert_eq!(detect_anchor_prefix_in_content(multi), Some(1));
        // Multi-line: detected on second line
        let multi2 = "normal line\n22:xx:yy\u{2192}line2";
        assert_eq!(detect_anchor_prefix_in_content(multi2), Some(2));
        // No anchors in multi-line
        assert_eq!(detect_anchor_prefix_in_content("line1\nline2\nline3"), None);
    }

    #[test]
    fn replace_rejects_content_with_anchor_prefix() {
        let anchors = anchors_for(SAMPLE);
        let anchor = anchors[1].clone();
        let content_with_anchor = "22:abc:rst\u{2192}let x = 1;".to_owned();
        let ops = vec![HashlineOp::Replace {
            anchor,
            end_anchor: None,
            content: content_with_anchor,
        }];
        match apply_edits(SAMPLE, &ops, &test_path(), &*test_scheme()).output {
            HashlineEditOutput::Error(e) => {
                assert_eq!(e.error, HashlineEditErrorKind::InvalidInput);
                assert!(e.message.contains("anchor prefixes"), "msg: {}", e.message);
            }
            other => panic!("Expected error, got: {other:?}"),
        }
    }

    #[test]
    fn insert_after_rejects_content_with_anchor_prefix() {
        let anchors = anchors_for(SAMPLE);
        let anchor = anchors[1].clone();
        let content_with_anchor = "axy:edj\u{2192}    # comment".to_owned();
        let ops = vec![HashlineOp::InsertAfter {
            anchor,
            content: content_with_anchor,
        }];
        match apply_edits(SAMPLE, &ops, &test_path(), &*test_scheme()).output {
            HashlineEditOutput::Error(e) => {
                assert_eq!(e.error, HashlineEditErrorKind::InvalidInput);
                assert!(e.message.contains("anchor prefixes"), "msg: {}", e.message);
            }
            other => panic!("Expected error, got: {other:?}"),
        }
    }

    #[test]
    fn write_rejects_content_with_anchor_prefix() {
        let content_with_anchor = "normal line\n22:abc:rst\u{2192}let x = 1;\n".to_owned();
        let ops = vec![HashlineOp::Write {
            content: content_with_anchor,
        }];
        match apply_edits(SAMPLE, &ops, &test_path(), &*test_scheme()).output {
            HashlineEditOutput::Error(e) => {
                assert_eq!(e.error, HashlineEditErrorKind::InvalidInput);
                assert!(e.message.contains("anchor prefixes"), "msg: {}", e.message);
            }
            other => panic!("Expected error, got: {other:?}"),
        }
    }
}
