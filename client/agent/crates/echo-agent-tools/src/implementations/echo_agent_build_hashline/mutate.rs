//! Synthetic mutation generation for the hashline benchmark harness.
//!
//! Mutations simulate the kinds of edits that happen during real agent
//! workflows: line insertions/deletions, formatter-style whitespace changes,
//! local token edits, boilerplate insertion, and range rewrites.
//!
//! Each [`Mutation`] variant describes *what* to do; [`apply_mutation`]
//! applies it to a `Vec<String>` of file lines (owned, mutable).

/// A synthetic mutation to apply to a file's lines.
///
/// All line indices are 0-based.
#[derive(Debug, Clone)]
pub enum Mutation {
    /// Insert `count` new lines before `before_idx`.
    InsertLines {
        before_idx: usize,
        lines: Vec<String>,
    },

    /// Delete lines in range `start_idx..start_idx + count`.
    DeleteLines { start_idx: usize, count: usize },

    /// Replace the content of a single line (local token edit).
    EditLine {
        line_idx: usize,
        new_content: String,
    },

    /// Change indentation of a single line (formatter-style).
    ReindentLine { line_idx: usize, new_indent: String },

    /// Replace a range of lines with new content (range rewrite).
    RangeRewrite {
        start_idx: usize,
        end_idx: usize,
        new_lines: Vec<String>,
    },
}

/// Classification of what happened to an original line after a mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineOutcome {
    /// Line was not modified and remains at the same position.
    Unchanged,
    /// Line was not modified but shifted to a new 0-based position.
    Shifted { new_idx: usize },
    /// Line's content was directly modified (edit, range rewrite).
    Modified,
    /// Line's indentation changed but non-whitespace content is preserved.
    /// Whitespace-normalized anchors should survive this change.
    Reindented,
    /// Line was deleted.
    Deleted,
}

/// Result of applying a mutation — carries metadata for benchmark analysis.
#[derive(Debug, Clone)]
pub struct MutationResult {
    /// Per-original-line outcome, indexed by original 0-based line index.
    /// Length equals the original line count before the mutation.
    pub outcomes: Vec<LineOutcome>,

    /// Net line-count change (positive = lines added, negative = lines removed).
    pub line_delta: isize,
}

/// Apply a mutation to a mutable vec of owned lines.
///
/// Returns metadata about what was affected, for benchmark analysis.
///
/// # Panics
///
/// Panics if indices are out of range for the current `lines` vec.
pub fn apply_mutation(lines: &mut Vec<String>, mutation: &Mutation) -> MutationResult {
    let orig_len = lines.len();

    match mutation {
        Mutation::InsertLines {
            before_idx,
            lines: new,
        } => {
            let idx = (*before_idx).min(lines.len());
            let count = new.len();
            for (i, line) in new.iter().enumerate() {
                lines.insert(idx + i, line.clone());
            }

            // Lines before idx: unchanged. Lines at idx and above: shifted.
            let outcomes = (0..orig_len)
                .map(|i| {
                    if i < idx {
                        LineOutcome::Unchanged
                    } else {
                        LineOutcome::Shifted { new_idx: i + count }
                    }
                })
                .collect();

            MutationResult {
                outcomes,
                line_delta: count as isize,
            }
        }

        Mutation::DeleteLines { start_idx, count } => {
            let start = *start_idx;
            let end = (start + count).min(lines.len());
            let actual_count = end.saturating_sub(start);
            lines.drain(start..end);

            let outcomes = (0..orig_len)
                .map(|i| {
                    if i < start {
                        LineOutcome::Unchanged
                    } else if i < end {
                        LineOutcome::Deleted
                    } else {
                        LineOutcome::Shifted {
                            new_idx: i - actual_count,
                        }
                    }
                })
                .collect();

            MutationResult {
                outcomes,
                line_delta: -(actual_count as isize),
            }
        }

        Mutation::EditLine {
            line_idx,
            new_content,
        } => {
            lines[*line_idx] = new_content.clone();

            let outcomes = (0..orig_len)
                .map(|i| {
                    if i == *line_idx {
                        LineOutcome::Modified
                    } else {
                        LineOutcome::Unchanged
                    }
                })
                .collect();

            MutationResult {
                outcomes,
                line_delta: 0,
            }
        }

        Mutation::ReindentLine {
            line_idx,
            new_indent,
        } => {
            let trimmed = lines[*line_idx].trim_start().to_owned();
            lines[*line_idx] = format!("{new_indent}{trimmed}");

            // Reindent changes only leading whitespace — anchors using
            // whitespace-normalized hashing should survive this.
            let outcomes = (0..orig_len)
                .map(|i| {
                    if i == *line_idx {
                        LineOutcome::Reindented
                    } else {
                        LineOutcome::Unchanged
                    }
                })
                .collect();

            MutationResult {
                outcomes,
                line_delta: 0,
            }
        }

        Mutation::RangeRewrite {
            start_idx,
            end_idx,
            new_lines,
        } => {
            let start = *start_idx;
            let end = (*end_idx).min(lines.len());
            let removed_count = end.saturating_sub(start);
            lines.splice(start..end, new_lines.iter().cloned());

            let size_delta = new_lines.len() as isize - removed_count as isize;

            let outcomes = (0..orig_len)
                .map(|i| {
                    if i < start {
                        LineOutcome::Unchanged
                    } else if i < end {
                        LineOutcome::Deleted
                    } else {
                        LineOutcome::Shifted {
                            new_idx: (i as isize + size_delta) as usize,
                        }
                    }
                })
                .collect();

            MutationResult {
                outcomes,
                line_delta: size_delta,
            }
        }
    }
}

/// Generate an "insert lines above" mutation: insert `count` boilerplate
/// lines before `before_idx`.
pub fn gen_insert_above(before_idx: usize, count: usize) -> Mutation {
    let lines: Vec<String> = (0..count)
        .map(|i| format!("// inserted line {i}"))
        .collect();
    Mutation::InsertLines { before_idx, lines }
}

/// Generate a "delete lines" mutation.
pub fn gen_delete(start_idx: usize, count: usize) -> Mutation {
    Mutation::DeleteLines { start_idx, count }
}

/// Generate a local token edit on a single line.
pub fn gen_token_edit(line_idx: usize, new_content: &str) -> Mutation {
    Mutation::EditLine {
        line_idx,
        new_content: new_content.to_owned(),
    }
}

/// Generate a formatter-style re-indentation.
pub fn gen_reindent(line_idx: usize, new_indent: &str) -> Mutation {
    Mutation::ReindentLine {
        line_idx,
        new_indent: new_indent.to_owned(),
    }
}

/// Generate a range rewrite replacing `start_idx..end_idx` with new content.
pub fn gen_range_rewrite(start_idx: usize, end_idx: usize, new_lines: &[&str]) -> Mutation {
    Mutation::RangeRewrite {
        start_idx,
        end_idx,
        new_lines: new_lines.iter().map(|s| s.to_string()).collect(),
    }
}

/// Generate a boilerplate-insertion mutation: insert repeated identical lines.
pub fn gen_boilerplate_insert(before_idx: usize, line: &str, count: usize) -> Mutation {
    Mutation::InsertLines {
        before_idx,
        lines: vec![line.to_owned(); count],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_lines() -> Vec<String> {
        vec![
            "fn main() {".to_owned(),
            "    let x = 1;".to_owned(),
            "    let y = 2;".to_owned(),
            "    println!(\"{x} {y}\");".to_owned(),
            "}".to_owned(),
        ]
    }

    #[test]
    fn insert_lines_above() {
        let mut lines = sample_lines();
        let m = gen_insert_above(1, 2);
        let result = apply_mutation(&mut lines, &m);
        assert_eq!(lines.len(), 7);
        assert!(lines[1].contains("inserted line 0"));
        assert!(lines[2].contains("inserted line 1"));
        assert_eq!(lines[3], "    let x = 1;");
        assert_eq!(result.line_delta, 2);
        // Line 0 unchanged, lines 1-4 shifted by +2.
        assert_eq!(result.outcomes[0], LineOutcome::Unchanged);
        assert_eq!(result.outcomes[1], LineOutcome::Shifted { new_idx: 3 });
        assert_eq!(result.outcomes[4], LineOutcome::Shifted { new_idx: 6 });
    }

    #[test]
    fn insert_at_end() {
        let mut lines = sample_lines();
        let m = gen_insert_above(100, 1); // past end → clamped
        let result = apply_mutation(&mut lines, &m);
        assert_eq!(lines.len(), 6);
        assert!(lines[5].contains("inserted line 0"));
        assert_eq!(result.line_delta, 1);
        // All original lines unchanged (insert was at end).
        assert!(result.outcomes.iter().all(|o| *o == LineOutcome::Unchanged));
    }

    #[test]
    fn delete_lines() {
        let mut lines = sample_lines();
        let m = gen_delete(1, 2);
        let result = apply_mutation(&mut lines, &m);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "fn main() {");
        assert_eq!(lines[1], "    println!(\"{x} {y}\");");
        assert_eq!(result.line_delta, -2);
        assert_eq!(result.outcomes[0], LineOutcome::Unchanged);
        assert_eq!(result.outcomes[1], LineOutcome::Deleted);
        assert_eq!(result.outcomes[2], LineOutcome::Deleted);
        assert_eq!(result.outcomes[3], LineOutcome::Shifted { new_idx: 1 });
    }

    #[test]
    fn delete_past_end_clamped() {
        let mut lines = sample_lines();
        let m = gen_delete(3, 100); // tries to delete 100 from idx 3
        let result = apply_mutation(&mut lines, &m);
        assert_eq!(lines.len(), 3); // only deleted 2 (indices 3,4)
        assert_eq!(result.line_delta, -2);
        assert_eq!(result.outcomes[3], LineOutcome::Deleted);
        assert_eq!(result.outcomes[4], LineOutcome::Deleted);
    }

    #[test]
    fn edit_line() {
        let mut lines = sample_lines();
        let m = gen_token_edit(1, "    let x = 999;");
        let result = apply_mutation(&mut lines, &m);
        assert_eq!(lines[1], "    let x = 999;");
        assert_eq!(result.line_delta, 0);
        assert_eq!(result.outcomes[1], LineOutcome::Modified);
        assert_eq!(result.outcomes[0], LineOutcome::Unchanged);
    }

    #[test]
    fn reindent_line() {
        let mut lines = sample_lines();
        let m = gen_reindent(1, "        "); // double indent
        let result = apply_mutation(&mut lines, &m);
        assert_eq!(lines[1], "        let x = 1;");
        assert_eq!(result.line_delta, 0);
        assert_eq!(result.outcomes[1], LineOutcome::Reindented);
    }

    #[test]
    fn range_rewrite() {
        let mut lines = sample_lines();
        let m = gen_range_rewrite(1, 3, &["    let z = 42;"]);
        let result = apply_mutation(&mut lines, &m);
        assert_eq!(lines.len(), 4); // 5 - 2 removed + 1 added
        assert_eq!(lines[1], "    let z = 42;");
        assert_eq!(lines[2], "    println!(\"{x} {y}\");");
        assert_eq!(result.line_delta, -1);
        assert_eq!(result.outcomes[1], LineOutcome::Deleted);
        assert_eq!(result.outcomes[2], LineOutcome::Deleted);
        assert_eq!(result.outcomes[3], LineOutcome::Shifted { new_idx: 2 });
    }

    #[test]
    fn boilerplate_insert() {
        let mut lines = sample_lines();
        let m = gen_boilerplate_insert(0, "// boilerplate", 3);
        let result = apply_mutation(&mut lines, &m);
        assert_eq!(lines.len(), 8);
        assert_eq!(lines[0], "// boilerplate");
        assert_eq!(lines[1], "// boilerplate");
        assert_eq!(lines[2], "// boilerplate");
        assert_eq!(lines[3], "fn main() {");
        assert_eq!(result.line_delta, 3);
        // All original lines shifted by +3.
        assert_eq!(result.outcomes[0], LineOutcome::Shifted { new_idx: 3 });
        assert_eq!(result.outcomes[4], LineOutcome::Shifted { new_idx: 7 });
    }

    #[test]
    fn range_rewrite_expand() {
        let mut lines = sample_lines();
        let m = gen_range_rewrite(
            1,
            2,
            &["    let a = 1;", "    let b = 2;", "    let c = 3;"],
        );
        let result = apply_mutation(&mut lines, &m);
        assert_eq!(lines.len(), 7); // 5 - 1 removed + 3 added
        assert_eq!(result.line_delta, 2);
        assert_eq!(result.outcomes[1], LineOutcome::Deleted);
        assert_eq!(result.outcomes[2], LineOutcome::Shifted { new_idx: 4 });
    }
}
